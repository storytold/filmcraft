//! Where the voice actually is: the "Pause Cut brain".
//!
//! Speech recognition gives the words; this module finds the voice in the waveform, so pauses come
//! from the audio and not from word timestamps (recognisers let the word after a pause swallow the
//! pause, and their bounds are only good to a few tens of milliseconds).
//!
//! [`voice_map`] measures 10 ms windows every 5 ms (RMS, peak and zero-crossing rate) and finds the
//! voiced spans:
//!
//! - **Levels.** Frames at or below −100 dBFS are digital silence (a closed OBS noise gate) and are
//!   left out. The noise floor is the 10th percentile of the other frames, the speech level the
//!   90th. In a gated recording what is left at the bottom is gate fades and quiet speech, so there
//!   the floor is taken as at most 40 dB below the speech.
//! - **Gate.** A voiced *core* is at least two frames whose RMS reaches the gate: 22 dB below the
//!   speech level, and at least 12 dB above the floor, so a stationary noise floor is never voice
//!   whatever its absolute level.
//! - **Hysteresis.** Each core grows outwards (up to the next core) over frames above a lower
//!   threshold, 14 dB under the gate and at least 6 dB above the floor, by RMS or by a peak 12 dB
//!   above it (a final /t/ burst), stepping over dips of up to 25 ms. That keeps soft onsets (a quiet
//!   /sh/ or /f/ before the loud part of a word), decaying vowels and final fricatives.
//! - **Breaths.** Growth longer than 100 ms that is not hiss (mean zero-crossing rate under 0.36:
//!   a breath, an inhale, a noise floor right next to the voice) keeps only the decay into that
//!   plateau, down to the dip between the two when there is one (or the rise out of it, before a
//!   core).
//! - **Edges and bridging.** Span edges sit 7.5 ms outside the outermost active windows' centres.
//!   Gaps shorter than 40 ms (stop closures, flicker) are bridged; longer gaps stay, so pauses of
//!   60–80 ms and up are visible to the caller.
//! - **Clicks.** A burst shorter than 60 ms with no other voice within 250 ms (or shorter than 30 ms
//!   with none within 100 ms) is a click or a key, unless it is the main sound under a recognised
//!   word of plausible length (a word that swallowed a pause does not protect a click in it).
//! - **Quiet words.** A recognised word of plausible length (1.5 s at most) under which nothing
//!   passed the gate keeps its loudest stretch that is clearly above the floor and is not a breath.
//!
//! [`snap_words`] then moves word bounds onto that voice. Both are total: empty, silent, NaN or
//! absurd input gives an empty map or leaves words alone, never a panic.

use filmcraft_project::Word;
use filmcraft_time::{Tick, TimeRange};

use crate::{SAMPLE_RATE, TICKS_PER_SAMPLE};

/// Analysis hop: 5 ms.
const HOP: usize = (SAMPLE_RATE / 200) as usize;
/// Analysis window: 10 ms.
const WIN: usize = 2 * HOP;
/// Samples per millisecond.
const MS: usize = (SAMPLE_RATE / 1000) as usize;

/// Frames at or below this are digital silence.
const DIGITAL_SILENCE_DB: f32 = -100.0;
/// Share of digital-silence frames from which a recording counts as noise-gated.
const GATED_SHARE: f32 = 0.05;
/// In a gated recording the floor is at most this far below the speech level.
const GATED_FLOOR_BELOW_SPEECH: f32 = 40.0;
/// The gate sits this far below the speech level...
const GATE_BELOW_SPEECH: f32 = 22.0;
/// ...but at least this far above the floor.
const GATE_ABOVE_FLOOR: f32 = 12.0;
/// The growth threshold sits this far below the gate...
const LOW_BELOW_GATE: f32 = 14.0;
/// ...but at least this far above the floor.
const LOW_ABOVE_FLOOR: f32 = 6.0;
/// A frame whose peak is this far above the growth threshold counts as active (bursts).
const PEAK_CREST: f32 = 12.0;

/// A core is at least this many frames at or above the gate.
const MIN_CORE_FRAMES: usize = 2;
/// Fewer frames than this above digital silence: no voice at all.
const MIN_LIVE_FRAMES: usize = 2;
/// Growth before a core (soft onsets), in frames.
const ONSET_GROW_FRAMES: usize = 30;
/// Growth after a core (decays, final fricatives and bursts), in frames.
const TAIL_GROW_FRAMES: usize = 40;
/// Growth longer than this (frames) that is not hiss is a breath or noise plateau...
const LONG_GROW_FRAMES: usize = 20;
/// ...unless its mean zero-crossing rate is at least this (a long final /s/).
const SIBILANT_ZCR: f32 = 0.36;
/// Below this mean zero-crossing rate a stretch is voiced, not breath.
const VOICED_ZCR: f32 = 0.12;
/// Long growth keeps the frames more than this above the plateau's median level.
const KNEE_DB: f32 = 3.0;
/// A dip at least this far below the plateau, within VALLEY_FRAMES of where the level reaches it,
/// is where the voice ends.
const VALLEY_DB: f32 = 3.0;
const VALLEY_FRAMES: usize = 12;
/// Growth steps over dips of at most this many frames.
const GROW_DIP_FRAMES: usize = 5;
/// Pre-roll before a span, in samples.
const ONSET_PAD: usize = 5 * MS;
/// Hangover after a span, in samples.
const TAIL_PAD: usize = 5 * MS;
/// Gaps shorter than this are bridged (samples).
const BRIDGE: usize = 40 * MS;
/// Bursts shorter than this...
const CLICK_MAX: usize = 60 * MS;
/// ...with no other voice this close are clicks (samples)...
const CLICK_ISOLATION: usize = 250 * MS;
/// ...and transients shorter than this...
const TRANSIENT_MAX: usize = 30 * MS;
/// ...with no other voice this close.
const TRANSIENT_ISOLATION: usize = 100 * MS;
/// Recognised words longer than this are not trusted to protect sound (they swallowed a pause).
const PLAUSIBLE_WORD: usize = 1500 * MS;
/// A quiet word's loudest stretch must reach this far above the floor...
const QUIET_WORD_ABOVE_FLOOR: f32 = 15.0;
/// ...and this far above the growth threshold to be kept.
const QUIET_WORD_ABOVE_LOW: f32 = 6.0;
/// Spans shorter than this are dropped (samples).
const MIN_SPAN: usize = 5 * MS;

/// Voice activity of a mono 16 kHz recording (media time, from sample 0).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct VoiceMap {
    /// Voiced spans in time order, non-overlapping, each at least a few ms; gaps between them are silence/noise floor.
    pub spans: Vec<TimeRange>,
    /// Estimated noise floor (dBFS).
    pub floor_db: f32,
    /// Typical speech level (dBFS).
    pub speech_db: f32,
    /// The gate used (dBFS).
    pub gate_db: f32,
}

/// Find the voice in `audio` (see the module docs). `words` (media time, as recognised) may be used
/// to protect quiet speech the gate alone would drop.
pub fn voice_map(audio: &[f32], words: &[Word]) -> VoiceMap {
    let frames = Frames::analyse(audio);
    let Some(lv) = Levels::estimate(&frames.rms) else {
        return VoiceMap { spans: Vec::new(), floor_db: DIGITAL_SILENCE_DB, speech_db: DIGITAL_SILENCE_DB, gate_db: DIGITAL_SILENCE_DB };
    };
    let len = audio.len();
    let active: Vec<bool> = frames.rms.iter().zip(&frames.peak).map(|(r, p)| *r >= lv.low || *p >= lv.low + PEAK_CREST).collect();

    // cores: runs of at least MIN_CORE_FRAMES frames at or above the gate
    let mut cores: Vec<(usize, usize)> = Vec::new();
    let n = frames.rms.len();
    let mut i = 0;
    while i < n {
        if frames.rms.get(i).is_none_or(|r| *r < lv.gate) {
            i += 1;
            continue;
        }
        let mut j = i;
        while frames.rms.get(j).is_some_and(|r| *r >= lv.gate) {
            j += 1;
        }
        if j - i >= MIN_CORE_FRAMES {
            cores.push((i, j));
        }
        i = j;
    }
    // each core grows outwards, up to its neighbours
    let mut spans: Vec<(usize, usize)> = Vec::with_capacity(cores.len());
    for (c, &(i, j)) in cores.iter().enumerate() {
        let left_limit = c.checked_sub(1).and_then(|p| cores.get(p)).map_or(0, |p| p.1);
        let right_limit = cores.get(c + 1).map_or(n, |q| q.0);
        let a = knee_left(&frames, grow_left(&active, i, left_limit, ONSET_GROW_FRAMES), i);
        let b = knee_right(&frames, j, grow_right(&active, j, right_limit, TAIL_GROW_FRAMES));
        spans.push(frame_span(a, b, len));
    }
    let spans = merge(spans, 0);
    let word_spans = plausible_words(words, len);
    // quiet words: nothing passed the gate inside a recognised word, but something there is clearly
    // above the floor and is not a breath
    let mut quiet: Vec<(usize, usize)> = Vec::new();
    for &(ws, we) in &word_spans {
        if overlapping(&spans, ws, we).next().is_some() {
            continue;
        }
        if let Some((a, b)) = loudest_stretch(&frames, &active, ws, we, (lv.floor + QUIET_WORD_ABOVE_FLOOR).max(lv.low + QUIET_WORD_ABOVE_LOW))
            && !breath_like(&frames, a, b)
        {
            quiet.push(frame_span(a, b, len));
        }
    }
    let spans = merge(spans.into_iter().chain(quiet).collect(), BRIDGE);
    let spans = drop_clicks(spans, &word_spans);
    VoiceMap {
        spans: spans
            .into_iter()
            .filter(|(a, b)| b.saturating_sub(*a) >= MIN_SPAN)
            .map(|(a, b)| TimeRange::from_bounds(sample_tick(a), sample_tick(b)))
            .collect(),
        floor_db: lv.floor,
        speech_db: lv.speech,
        gate_db: lv.gate,
    }
}

/// Per-frame levels: 10 ms windows every 5 ms (frame `i` starts at sample `i * HOP`).
struct Frames {
    rms: Vec<f32>,
    peak: Vec<f32>,
    /// Zero-crossing rate (0..1): low for voicing, high for /s/-like hiss, in between for breath.
    zcr: Vec<f32>,
}

impl Frames {
    fn analyse(audio: &[f32]) -> Frames {
        let n = audio.len().div_ceil(HOP);
        let mut rms = Vec::with_capacity(n);
        let mut peak = Vec::with_capacity(n);
        let mut zcr = Vec::with_capacity(n);
        for i in 0..n {
            let a = i.saturating_mul(HOP);
            let b = a.saturating_add(WIN).min(audio.len());
            let w = audio.get(a..b).unwrap_or(&[]);
            let (mut e, mut p, mut z, mut prev) = (0f64, 0f32, 0u32, None);
            for &x in w {
                // NaN/inf (corrupt decode) counts as silence; absurd values are bounded
                let x = if x.is_finite() { x.clamp(-16.0, 16.0) } else { 0.0 };
                e += f64::from(x) * f64::from(x);
                p = p.max(x.abs());
                if prev.is_some_and(|neg| neg != (x < 0.0)) {
                    z += 1;
                }
                prev = Some(x < 0.0);
            }
            let ms = e / w.len().max(1) as f64;
            rms.push((10.0 * (ms + 1e-20).log10()) as f32);
            peak.push(20.0 * (p + 1e-10).log10());
            zcr.push(z as f32 / w.len().max(1) as f32);
        }
        Frames { rms, peak, zcr }
    }
}

/// Recording levels and thresholds (dBFS).
struct Levels {
    floor: f32,
    speech: f32,
    gate: f32,
    low: f32,
}

impl Levels {
    fn estimate(rms: &[f32]) -> Option<Levels> {
        let mut live: Vec<f32> = rms.iter().copied().filter(|v| *v > DIGITAL_SILENCE_DB).collect();
        if live.len() < MIN_LIVE_FRAMES {
            return None;
        }
        live.sort_by(f32::total_cmp);
        let speech = percentile(&live, 0.90)?;
        let mut floor = percentile(&live, 0.10)?;
        let gated = 1.0 - live.len() as f32 / rms.len().max(1) as f32 >= GATED_SHARE;
        if gated {
            floor = floor.min(speech - GATED_FLOOR_BELOW_SPEECH);
        }
        let gate = (speech - GATE_BELOW_SPEECH).max(floor + GATE_ABOVE_FLOOR);
        let low = (gate - LOW_BELOW_GATE).max(floor + LOW_ABOVE_FLOOR).min(gate);
        Some(Levels { floor, speech, gate, low })
    }
}

/// The `p` quantile (0..1) of sorted `v`.
fn percentile(v: &[f32], p: f32) -> Option<f32> {
    let i = ((v.len().saturating_sub(1)) as f32 * p.clamp(0.0, 1.0)).round() as usize;
    v.get(i).copied()
}

/// First frame of the growth to the left of a core starting at `a`, not before frame `limit`.
fn grow_left(active: &[bool], a: usize, limit: usize, max: usize) -> usize {
    let (mut s, mut dip) = (a, 0);
    let mut k = a;
    while k > limit && a - k < max {
        k -= 1;
        if active.get(k).copied().unwrap_or(false) {
            s = k;
            dip = 0;
        } else {
            dip += 1;
            if dip > GROW_DIP_FRAMES {
                break;
            }
        }
    }
    s
}

/// End (exclusive) of the growth to the right of a core ending at `b` (exclusive), not after
/// frame `limit`.
fn grow_right(active: &[bool], b: usize, limit: usize, max: usize) -> usize {
    let (mut e, mut dip) = (b, 0);
    let mut k = b;
    while k < limit.min(active.len()) && k - b < max {
        if active.get(k).copied().unwrap_or(false) {
            e = k + 1;
            dip = 0;
        } else {
            dip += 1;
            if dip > GROW_DIP_FRAMES {
                break;
            }
        }
        k += 1;
    }
    e
}

/// Whether growth over frames `a..b` is breath or noise next to the voice rather than the voice's
/// own soft edge: long, and not /s/-like hiss.
fn plateau(frames: &Frames, a: usize, b: usize) -> Option<f32> {
    if b.saturating_sub(a) <= LONG_GROW_FRAMES {
        return None;
    }
    let zcr = frames.zcr.get(a..b)?;
    if zcr.iter().sum::<f32>() / zcr.len().max(1) as f32 >= SIBILANT_ZCR {
        return None;
    }
    let mut lv = frames.rms.get(a..b)?.to_vec();
    lv.sort_by(f32::total_cmp);
    percentile(&lv, 0.5)
}

/// Long growth before a core starting at frame `core` keeps only the rise out of the plateau
/// (an inhale or a noise floor right before a word stays out): from the deepest point of a clear
/// dip between plateau and voice, or else from where the level leaves the plateau.
fn knee_left(frames: &Frames, s: usize, core: usize) -> usize {
    let Some(level) = plateau(frames, s, core) else { return s };
    let rms = |k: usize| frames.rms.get(k).copied().unwrap_or(f32::MIN);
    let mut knee = core;
    while knee > s && rms(knee - 1) > level + KNEE_DB {
        knee -= 1;
    }
    // the dip: the quietest frame within VALLEY_FRAMES before the knee
    let lo = knee.saturating_sub(VALLEY_FRAMES).max(s);
    match (lo..knee).rev().min_by(|a, b| rms(*a).total_cmp(&rms(*b))) {
        Some(v) if rms(v) <= level - VALLEY_DB => v + 1,
        _ => knee,
    }
}

/// Long growth after a core ending at frame `core` (exclusive) keeps only the decay into the
/// plateau (a breath or a noise floor right after a word stays out): down to the deepest point of
/// a clear dip between voice and plateau, or else to where the level reaches the plateau.
fn knee_right(frames: &Frames, core: usize, e: usize) -> usize {
    let Some(level) = plateau(frames, core, e) else { return e };
    let rms = |k: usize| frames.rms.get(k).copied().unwrap_or(f32::MIN);
    let mut knee = core;
    while knee < e && rms(knee) > level + KNEE_DB {
        knee += 1;
    }
    let hi = knee.saturating_add(VALLEY_FRAMES).min(e);
    match (knee..hi).min_by(|a, b| rms(*a).total_cmp(&rms(*b))) {
        Some(v) if rms(v) <= level - VALLEY_DB => v,
        _ => knee,
    }
}

/// Sample range of frames `a..b`, with pre-roll and hangover, within `0..len`.
fn frame_span(a: usize, b: usize, len: usize) -> (usize, usize) {
    // a frame is active when its window holds enough sound; the edge sits near the window's middle
    let s = a.saturating_mul(HOP).saturating_add(HOP / 2).saturating_sub(ONSET_PAD);
    let e = b.saturating_mul(HOP).saturating_add(HOP / 2).saturating_add(TAIL_PAD);
    (s.min(len), e.min(len))
}

/// Recognised words of plausible length, as sample ranges within `0..len`.
fn plausible_words(words: &[Word], len: usize) -> Vec<(usize, usize)> {
    words
        .iter()
        .filter_map(|w| {
            let s = tick_sample(w.start).min(len);
            let e = tick_sample(w.end).min(len);
            (e > s && e - s <= PLAUSIBLE_WORD).then_some((s, e))
        })
        .collect()
}

/// The loudest run of active frames inside samples `s..e` whose RMS reaches `min_db`.
fn loudest_stretch(frames: &Frames, active: &[bool], s: usize, e: usize, min_db: f32) -> Option<(usize, usize)> {
    let (fa, fb) = (s / HOP, e.div_ceil(HOP).min(active.len()));
    let mut best: Option<(usize, usize, f32)> = None;
    let mut i = fa;
    while i < fb {
        if !active.get(i).copied().unwrap_or(false) {
            i += 1;
            continue;
        }
        let mut j = i;
        let mut top = f32::MIN;
        while j < fb && active.get(j).copied().unwrap_or(false) {
            top = top.max(frames.rms.get(j).copied().unwrap_or(f32::MIN));
            j += 1;
        }
        if j - i >= MIN_CORE_FRAMES && top >= min_db && best.is_none_or(|b| top > b.2) {
            best = Some((i, j, top));
        }
        i = j;
    }
    best.map(|(a, b, _)| (a, b))
}

/// Sort and merge ranges whose gap is shorter than `bridge`.
fn merge(mut v: Vec<(usize, usize)>, bridge: usize) -> Vec<(usize, usize)> {
    v.retain(|(a, b)| b > a);
    v.sort_unstable();
    let mut out: Vec<(usize, usize)> = Vec::with_capacity(v.len());
    for (a, b) in v {
        match out.last_mut() {
            Some(last) if a < last.1.saturating_add(bridge) => last.1 = last.1.max(b),
            _ => out.push((a, b)),
        }
    }
    out
}

/// The ranges of sorted, non-overlapping `spans` that overlap `a..b`.
fn overlapping(spans: &[(usize, usize)], a: usize, b: usize) -> impl Iterator<Item = &(usize, usize)> {
    let i = spans.partition_point(|s| s.1 <= a);
    spans.get(i..).unwrap_or(&[]).iter().take_while(move |s| s.0 < b)
}

/// Drop short bursts away from other voice (mouse clicks, keys), unless a burst is the main sound
/// under a recognised word (a word that swallowed a pause doesn't protect a click in it).
fn drop_clicks(spans: Vec<(usize, usize)>, words: &[(usize, usize)]) -> Vec<(usize, usize)> {
    let mut by_start = words.to_vec();
    by_start.sort_unstable();
    let keep: Vec<bool> = (0..spans.len())
        .map(|i| {
            let Some(&(a, b)) = spans.get(i) else { return false };
            let d = b.saturating_sub(a);
            let isolation = if d < TRANSIENT_MAX {
                TRANSIENT_ISOLATION
            } else if d < CLICK_MAX {
                CLICK_ISOLATION
            } else {
                return true;
            };
            let prev_far = i.checked_sub(1).and_then(|p| spans.get(p)).is_none_or(|p| a.saturating_sub(p.1) >= isolation);
            let next_far = spans.get(i + 1).is_none_or(|n| n.0.saturating_sub(b) >= isolation);
            if !(prev_far && next_far) {
                return true;
            }
            // words overlapping the burst start after a - PLAUSIBLE_WORD
            let from = by_start.partition_point(|w| w.0.saturating_add(PLAUSIBLE_WORD) <= a);
            let to = by_start.partition_point(|w| w.0 < b);
            let inside = |s: &(usize, usize), ws: usize, we: usize| s.1.min(we).saturating_sub(s.0.max(ws));
            by_start.get(from..to).unwrap_or(&[]).iter().any(|&(ws, we)| {
                let own = inside(&(a, b), ws, we);
                we > a && overlapping(&spans, ws, we).all(|s| inside(s, ws, we) <= own)
            })
        })
        .collect();
    spans.into_iter().zip(keep).filter_map(|(s, k)| k.then_some(s)).collect()
}

/// Whether frames `a..b` look like a breath: long, noisy but not hiss.
fn breath_like(frames: &Frames, a: usize, b: usize) -> bool {
    if b.saturating_sub(a) <= LONG_GROW_FRAMES {
        return false;
    }
    let Some(z) = frames.zcr.get(a..b) else { return false };
    let mean = z.iter().sum::<f32>() / z.len().max(1) as f32;
    (VOICED_ZCR..SIBILANT_ZCR).contains(&mean)
}

fn sample_tick(n: usize) -> Tick {
    Tick(i64::try_from(n).unwrap_or(i64::MAX).saturating_mul(TICKS_PER_SAMPLE))
}

/// Sample index of `t` (negative times are sample 0).
fn tick_sample(t: Tick) -> usize {
    usize::try_from(t.0.div_euclid(TICKS_PER_SAMPLE)).unwrap_or(0)
}

// ---------------------------------------------------------------------------------------------
// Word snapping

/// Shortest word after snapping: 60 ms.
const MIN_WORD: Tick = Tick(60 * (TICKS_PER_SAMPLE * MS as i64));
/// A silence at least this long inside a word separates parts that may belong to other words.
const SPLIT_GAP: Tick = Tick(80 * (TICKS_PER_SAMPLE * MS as i64));
/// A silence at least this long inside a word is a swallowed pause, never a stop closure.
const LONG_SILENCE: Tick = Tick(250 * (TICKS_PER_SAMPLE * MS as i64));
/// A part of a word that continues a neighbour's voice and is at most this long is the neighbour's.
const NEIGHBOUR_PART: Tick = Tick(200 * (TICKS_PER_SAMPLE * MS as i64));
/// How far a word in silence, or a word's bound, may move to reach voice.
const REACH: Tick = Tick(300 * (TICKS_PER_SAMPLE * MS as i64));

fn tadd(a: Tick, b: Tick) -> Tick {
    Tick(a.0.saturating_add(b.0))
}
fn tsub(a: Tick, b: Tick) -> Tick {
    Tick(a.0.saturating_sub(b.0))
}
fn tend(r: &TimeRange) -> Tick {
    tadd(r.start, r.duration)
}

/// A voiced part of a word: `start..end` (clipped to the word) and whether its span runs on past
/// the word's start / end.
#[derive(Clone, Copy, Debug)]
struct Part {
    start: Tick,
    end: Tick,
    open_start: bool,
    open_end: bool,
}

/// Snap word bounds to the voice: a word's start/end move to where its voice actually starts/ends;
/// a word spanning a long silence keeps the side that has its speech (recognisers make the word
/// after a pause swallow the pause); a word sitting entirely in silence moves next to the nearest
/// voice within ~0.3 s; every word keeps >= 60 ms (a word the recogniser made shorter grows to
/// 60 ms where there is room); words stay in order and never overlap.
pub fn snap_words(words: &mut [Word], voice: &VoiceMap) {
    let spans = &voice.spans;
    if spans.is_empty() || words.is_empty() {
        return;
    }
    // 1. each word on its own voice
    let mut target: Vec<(Tick, Tick, bool)> = words.iter().map(|w| snap_one(w.start, w.end.max(w.start), spans)).collect();
    // 2. voice right next to a word that no other word claims is the word's own
    for i in 0..target.len() {
        let Some(&(s, e, voiced)) = target.get(i) else { continue };
        if !voiced {
            continue;
        }
        let prev_end = i.checked_sub(1).and_then(|p| target.get(p)).map_or(Tick::MIN, |t| t.1);
        let next_start = target.get(i + 1).map_or(Tick::MAX, |t| t.0);
        let mut ns = s;
        if let Some(sp) = span_at(spans, s)
            && sp.start < s
            && prev_end <= sp.start
            && tsub(s, sp.start) <= REACH
        {
            ns = sp.start;
        }
        let mut ne = e;
        if let Some(sp) = span_at(spans, tsub(e, Tick(1)))
            && tend(sp) > e
            && next_start >= tend(sp)
            && tsub(tend(sp), e) <= REACH
        {
            ne = tend(sp);
        }
        if let Some(t) = target.get_mut(i) {
            t.0 = ns;
            t.1 = ne;
        }
    }
    // 3. words in silence go next to the nearest voice
    for i in 0..target.len() {
        let Some(&(s, e, voiced)) = target.get(i) else { continue };
        if voiced {
            continue;
        }
        if let Some((ns, ne)) = beside_voice(s, e, spans)
            && let Some(t) = target.get_mut(i)
        {
            t.0 = ns;
            t.1 = ne;
        }
    }
    // 4. order, no overlaps, minimum length
    let may_push: Vec<bool> = words.iter().map(|w| tsub(w.end, w.start) >= MIN_WORD).collect();
    fit(&mut target, &may_push);
    for (w, (s, e, _)) in words.iter_mut().zip(target) {
        w.start = s;
        w.end = e;
    }
}

/// The span containing `t`.
fn span_at(spans: &[TimeRange], t: Tick) -> Option<&TimeRange> {
    let i = spans.partition_point(|r| tend(r) <= t);
    spans.get(i).filter(|r| r.start <= t)
}

/// Bounds of one word on its voice, and whether it has any voice.
fn snap_one(s: Tick, e: Tick, spans: &[TimeRange]) -> (Tick, Tick, bool) {
    if e <= s {
        // zero-length: voiced if it sits in a span
        return (s, e, span_at(spans, s).is_some());
    }
    let first = spans.partition_point(|r| tend(r) <= s);
    let parts: Vec<Part> = spans
        .get(first..)
        .unwrap_or(&[])
        .iter()
        .take_while(|r| r.start < e)
        .map(|r| Part { start: r.start.max(s), end: tend(r).min(e), open_start: r.start < s, open_end: tend(r) > e })
        .filter(|p| p.end > p.start)
        .collect();
    if parts.is_empty() {
        return (s, e, false);
    }
    // group parts separated by silences of SPLIT_GAP or more
    let mut groups: Vec<Part> = Vec::new();
    for p in parts {
        match groups.last_mut() {
            Some(g) if tsub(p.start, g.end) < SPLIT_GAP => {
                g.end = p.end;
                g.open_end = p.open_end;
            }
            _ => groups.push(p),
        }
    }
    let gap = |k: usize| match (groups.get(k), groups.get(k + 1)) {
        (Some(a), Some(b)) => tsub(b.start, a.end),
        _ => Tick::ZERO,
    };
    let tail_of_prev = |g: &Part| g.open_start && tsub(g.end, g.start) <= NEIGHBOUR_PART;
    let onset_of_next = |g: &Part| g.open_end && tsub(g.end, g.start) <= NEIGHBOUR_PART;
    let (mut lo, mut hi) = (0usize, groups.len().saturating_sub(1));
    // A long silence is never inside a word. Recognisers let the word after a pause swallow the
    // pause, so the word's own voice is after it, unless all there is after it is the next word's
    // onset and the voice before it starts inside the word (so it is not the previous word's).
    while let Some(k) = (lo..hi).rev().find(|&k| gap(k) >= LONG_SILENCE) {
        let right_is_next = k + 1 == hi && groups.get(hi).is_some_and(onset_of_next);
        let left_is_own = groups.get(lo).is_some_and(|g| !g.open_start);
        if right_is_next && left_is_own {
            hi = k;
        } else {
            lo = k + 1;
        }
    }
    // around shorter silences, a short part that continues a neighbour's voice is the neighbour's
    if hi > lo && groups.get(lo).is_some_and(tail_of_prev) {
        lo += 1;
    }
    if hi > lo && groups.get(hi).is_some_and(onset_of_next) {
        hi -= 1;
    }
    match (groups.get(lo), groups.get(hi)) {
        (Some(a), Some(b)) => (a.start, b.end, true),
        _ => (s, e, false),
    }
}

/// A word in silence at `s..e` moved next to the nearest voice within [`REACH`]: its first
/// [`MIN_WORD`] when the voice follows, its last when the voice precedes.
fn beside_voice(s: Tick, e: Tick, spans: &[TimeRange]) -> Option<(Tick, Tick)> {
    let i = spans.partition_point(|r| tend(r) <= s);
    let before = i.checked_sub(1).and_then(|p| spans.get(p)).map(|r| (tsub(s, tend(r)), tend(r)));
    let after = spans.get(i).filter(|r| r.start >= e).map(|r| (tsub(r.start, e), r.start));
    match (before, after) {
        (Some((db, tb)), Some((da, _))) if db < da => (db <= REACH).then_some((tsub(tb, MIN_WORD), tb)),
        (_, Some((da, ta))) => (da <= REACH).then_some((ta, tadd(ta, MIN_WORD))),
        (Some((db, tb)), None) => (db <= REACH).then_some((tsub(tb, MIN_WORD), tb)),
        (None, None) => None,
    }
}

/// Keep words in order, non-overlapping and at least [`MIN_WORD`] long, moving them as little as
/// possible: a short word grows into free room after it (or into the next word while that keeps
/// [`MIN_WORD`]), then before it. Only a word that was at least [`MIN_WORD`] long to begin with may
/// push the words after it; a recogniser's pile of zero-length words stays a pile instead of
/// shifting everything after it.
fn fit(t: &mut [(Tick, Tick, bool)], may_push: &[bool]) {
    let mut prev_end = Tick::MIN;
    for i in 0..t.len() {
        let room_end = t.get(i + 1).map_or(Tick::MAX, |n| n.0.max(tsub(n.1, MIN_WORD)));
        let push = may_push.get(i).copied().unwrap_or(false);
        let Some(w) = t.get_mut(i) else { continue };
        let s0 = w.0.max(prev_end);
        let (mut s, mut e) = (s0, w.1.max(s0));
        if tsub(e, s) < MIN_WORD {
            e = tadd(s, MIN_WORD).min(e.max(room_end));
            s = s.min(tsub(e, MIN_WORD).max(prev_end));
            if push {
                e = e.max(tadd(s, MIN_WORD));
            }
        }
        w.0 = s;
        w.1 = e;
        prev_end = e;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::seconds_tick;

    const SR: f32 = SAMPLE_RATE as f32;

    /// Deterministic uniform noise in [-1, 1).
    struct Rng(u64);
    impl Rng {
        fn next(&mut self) -> f32 {
            self.0 = self.0.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1_442_695_040_888_963_407);
            (self.0 >> 40) as f32 / (1u64 << 23) as f32 - 1.0
        }
    }

    fn amp(db: f32) -> f32 {
        10f32.powf(db / 20.0)
    }

    fn idx(t: f32) -> usize {
        (t * SR).round() as usize
    }

    /// `secs` of white noise at `floor_db` RMS, or digital silence.
    fn floor(secs: f32, floor_db: Option<f32>, rng: &mut Rng) -> Vec<f32> {
        let n = idx(secs);
        match floor_db {
            Some(db) => (0..n).map(|_| rng.next() * amp(db) * 3f32.sqrt()).collect(),
            None => vec![0.0; n],
        }
    }

    /// A voice-like harmonic tone at `db` RMS from `a` to `b` seconds (5 ms ramps).
    fn tone(audio: &mut [f32], a: f32, b: f32, db: f32) {
        let h = [1.0f32, 0.6, 0.4, 0.25];
        let norm = (h.iter().map(|x| x * x / 2.0).sum::<f32>()).sqrt();
        for i in idx(a)..idx(b).min(audio.len()) {
            let t = i as f32 / SR;
            let ramp = ((t - a) / 0.005).min((b - t) / 0.005).clamp(0.0, 1.0);
            let v: f32 = h.iter().enumerate().map(|(k, g)| g * (std::f32::consts::TAU * 140.0 * (k + 1) as f32 * t).sin()).sum();
            audio[i] += v / norm * amp(db) * ramp;
        }
    }

    /// Noise at `db` RMS from `a` to `b`: white (`smooth` 0, /s/-like) or low-passed (breath-like).
    fn noise(audio: &mut [f32], a: f32, b: f32, db: f32, smooth: f32, rng: &mut Rng) {
        let (i0, i1) = (idx(a), idx(b).min(audio.len()));
        let mut y = 0.0;
        let raw: Vec<f32> = (i0..i1)
            .map(|_| {
                y = smooth * y + (1.0 - smooth) * rng.next();
                y
            })
            .collect();
        let rms = (raw.iter().map(|v| v * v).sum::<f32>() / raw.len().max(1) as f32).sqrt().max(1e-12);
        for (k, v) in raw.iter().enumerate() {
            audio[i0 + k] += v / rms * amp(db);
        }
    }

    fn secs(vm: &VoiceMap) -> Vec<(f64, f64)> {
        vm.spans.iter().map(|r| (r.start.seconds(), tend(r).seconds())).collect()
    }

    fn near(v: f64, want: f64, tol: f64) -> bool {
        (v - want).abs() <= tol
    }

    fn assert_spans(vm: &VoiceMap, want: &[(f64, f64)], tol: f64) {
        let got = secs(vm);
        assert_eq!(got.len(), want.len(), "spans {got:?}, want {want:?}");
        for (g, w) in got.iter().zip(want) {
            assert!(near(g.0, w.0, tol) && near(g.1, w.1, tol), "span {g:?}, want {w:?} (all: {got:?})");
        }
    }

    fn assert_well_formed(vm: &VoiceMap, len: usize) {
        let end = sample_tick(len);
        for w in vm.spans.windows(2) {
            assert!(tend(&w[0]) <= w[1].start, "overlap or disorder: {:?}", vm.spans);
        }
        for r in &vm.spans {
            assert!(r.duration >= sample_tick(MIN_SPAN) && r.start >= Tick::ZERO && tend(r) <= end, "bad span {r:?}");
        }
    }

    #[test]
    fn tones_and_gaps_on_different_floors() {
        // gaps: 30 ms (bridged), 70 ms, 100 ms and 500 ms (all visible)
        for floor_db in [Some(-79.0), Some(-62.0), Some(-45.0), None] {
            let mut rng = Rng(7);
            let mut a = floor(3.5, floor_db, &mut rng);
            for (s, e) in [(0.50, 0.90), (0.93, 1.30), (1.37, 1.70), (1.80, 2.20), (2.70, 3.10)] {
                tone(&mut a, s, e, -15.0);
            }
            let vm = voice_map(&a, &[]);
            assert_well_formed(&vm, a.len());
            assert_spans(&vm, &[(0.50, 1.30), (1.37, 1.70), (1.80, 2.20), (2.70, 3.10)], 0.015);
            assert!(vm.gate_db > vm.floor_db && vm.gate_db < vm.speech_db, "{floor_db:?}: {vm:?}");
        }
    }

    #[test]
    fn levels_follow_the_recording() {
        for floor_db in [-62.0, -79.0] {
            let mut rng = Rng(3);
            let mut a = floor(3.0, Some(floor_db), &mut rng);
            for (s, e) in [(0.3, 0.9), (1.4, 2.0)] {
                tone(&mut a, s, e, -18.0);
            }
            let vm = voice_map(&a, &[]);
            assert!(near(f64::from(vm.floor_db), floor_db.into(), 3.0), "{vm:?}");
            assert!(near(f64::from(vm.speech_db), -18.0, 3.0), "{vm:?}");
        }
    }

    #[test]
    fn stationary_noise_is_not_voice() {
        // a loud floor above any absolute -40 dB gate, and a quiet one; no speech anywhere
        for db in [-35.0, -60.0] {
            let mut rng = Rng(11);
            let a = floor(2.0, Some(db), &mut rng);
            assert!(voice_map(&a, &[]).spans.is_empty(), "noise at {db} dBFS");
        }
        // speech over a -35 dBFS noise floor: only the speech
        let mut rng = Rng(12);
        let mut a = floor(2.0, Some(-35.0), &mut rng);
        tone(&mut a, 0.8, 1.2, -8.0);
        let vm = voice_map(&a, &[]);
        assert_spans(&vm, &[(0.8, 1.2)], 0.02);
    }

    #[test]
    fn soft_fricative_onset_and_final_hiss_are_kept() {
        let mut rng = Rng(5);
        let mut a = floor(2.0, Some(-70.0), &mut rng);
        noise(&mut a, 0.92, 1.0, -44.0, 0.0, &mut rng); // quiet /sh/ before the word
        tone(&mut a, 1.0, 1.3, -15.0);
        noise(&mut a, 1.3, 1.42, -42.0, 0.0, &mut rng); // final /s/
        let vm = voice_map(&a, &[]);
        assert_spans(&vm, &[(0.92, 1.42)], 0.015);
    }

    #[test]
    fn decaying_vowel_tail_is_kept() {
        let mut rng = Rng(9);
        let mut a = floor(2.0, Some(-75.0), &mut rng);
        tone(&mut a, 0.5, 0.8, -15.0);
        // decay: 8.7 dB per 25 ms after 0.8 s, so 36 dB down at about 0.9 s
        let mut decay = vec![0.0; a.len()];
        tone(&mut decay, 0.8, 1.2, -15.0);
        for (i, v) in decay.iter().enumerate().skip(idx(0.8)) {
            a[i] += v * (-((i - idx(0.8)) as f32 / SR) / 0.025).exp();
        }
        let vm = voice_map(&a, &[]);
        let s = secs(&vm);
        assert_eq!(s.len(), 1, "{s:?}");
        assert!(s[0].1 >= 0.89 && s[0].1 <= 1.0, "tail cut or overlong: {s:?}");
    }

    #[test]
    fn breath_next_to_a_word_stays_out() {
        let mut rng = Rng(21);
        let mut a = floor(3.0, Some(-68.0), &mut rng);
        tone(&mut a, 0.5, 0.9, -15.0);
        noise(&mut a, 0.9, 1.4, -45.0, 0.7, &mut rng); // breath right after the word, no dip
        noise(&mut a, 1.7, 2.1, -46.0, 0.7, &mut rng); // inhale right before the next word
        tone(&mut a, 2.1, 2.5, -15.0);
        let vm = voice_map(&a, &[]);
        assert_spans(&vm, &[(0.5, 0.9), (2.1, 2.5)], 0.03);
    }

    #[test]
    fn clicks_far_from_speech_are_dropped() {
        let mut rng = Rng(4);
        let mut a = floor(4.0, Some(-70.0), &mut rng);
        tone(&mut a, 0.5, 0.9, -15.0);
        tone(&mut a, 3.0, 3.4, -15.0);
        // a mouse click in the middle of the pause and a key 150 ms after the word
        for c in [idx(2.0), idx(1.05)] {
            for k in 0..40 {
                a[c + k] += if k % 2 == 0 { 0.4 } else { -0.4 };
            }
        }
        let vm = voice_map(&a, &[]);
        assert_spans(&vm, &[(0.5, 0.9), (3.0, 3.4)], 0.015);
        // the main sound under a recognised word is kept
        let w = [Word::new("a", seconds_tick(1.97), seconds_tick(2.05))];
        let vm = voice_map(&a, &w);
        assert_eq!(vm.spans.len(), 3, "{:?}", secs(&vm));
        // a word that has voice of its own does not protect a click
        let w = [Word::new("so", seconds_tick(0.5), seconds_tick(1.1))];
        assert_eq!(voice_map(&a, &w).spans.len(), 2);
    }

    #[test]
    fn quiet_word_under_a_recognised_word_is_kept() {
        let mut rng = Rng(8);
        let mut a = floor(3.0, Some(-75.0), &mut rng);
        tone(&mut a, 0.5, 0.9, -12.0);
        tone(&mut a, 1.5, 1.7, -40.0); // a whispered aside, under the gate
        tone(&mut a, 2.3, 2.7, -12.0);
        assert_eq!(voice_map(&a, &[]).spans.len(), 2);
        let w = [Word::new("aside", seconds_tick(1.45), seconds_tick(1.75))];
        let vm = voice_map(&a, &w);
        assert_spans(&vm, &[(0.5, 0.9), (1.5, 1.7), (2.3, 2.7)], 0.02);
    }

    #[test]
    fn hostile_audio_never_panics() {
        let mut rng = Rng(1);
        let noisy: Vec<f32> = (0..20_000).map(|i| if i % 97 == 0 { f32::NAN } else { rng.next() }).collect();
        let words = [
            Word::new("x", Tick::MIN, Tick::MAX),
            Word::new("y", Tick(i64::MAX), Tick(i64::MIN)),
            Word::new("z", Tick(-5), Tick(-1)),
            Word::new("w", seconds_tick(0.5), seconds_tick(0.5)),
        ];
        let inputs: Vec<Vec<f32>> = vec![
            vec![],
            vec![0.5],
            vec![0.0; 16_000],
            vec![f32::NAN; 4000],
            vec![f32::INFINITY; 4000],
            (0..8000).map(|i| if i % 2 == 0 { 1e30 } else { -1e30 }).collect(),
            (0..8000).map(|i| if i % 3 == 0 { f32::NEG_INFINITY } else { 0.1 }).collect(),
            noisy,
        ];
        for a in &inputs {
            for ws in [&words[..], &[]] {
                let vm = voice_map(a, ws);
                assert_well_formed(&vm, a.len());
                assert!(vm.floor_db.is_finite() && vm.speech_db.is_finite() && vm.gate_db.is_finite());
                let mut w = words.to_vec();
                snap_words(&mut w, &vm);
            }
        }
        assert_eq!(voice_map(&[], &[]), VoiceMap { spans: vec![], floor_db: -100.0, speech_db: -100.0, gate_db: -100.0 });
        // hostile spans (unsorted, overlapping, negative) in a hand-made map
        let vm = VoiceMap {
            spans: vec![
                TimeRange::new(Tick(i64::MAX - 5), Tick(i64::MAX)),
                TimeRange::new(Tick(100), Tick(-50)),
                TimeRange::new(Tick::MIN, Tick::MAX),
                TimeRange::new(Tick(0), Tick(10)),
            ],
            ..Default::default()
        };
        let mut w = words.to_vec();
        snap_words(&mut w, &vm);
    }

    // --- snap_words --------------------------------------------------------------------------

    fn map(spans: &[(f64, f64)]) -> VoiceMap {
        VoiceMap { spans: spans.iter().map(|&(a, b)| TimeRange::from_bounds(seconds_tick(a), seconds_tick(b))).collect(), ..Default::default() }
    }

    fn words(w: &[(&str, f64, f64)]) -> Vec<Word> {
        w.iter().map(|&(t, a, b)| Word::new(t, seconds_tick(a), seconds_tick(b))).collect()
    }

    fn bounds(w: &[Word]) -> Vec<(f64, f64)> {
        w.iter().map(|w| ((w.start.seconds() * 1000.0).round() / 1000.0, (w.end.seconds() * 1000.0).round() / 1000.0)).collect()
    }

    fn assert_snap_invariants(w: &[Word]) {
        for x in w {
            assert!(x.end - x.start >= MIN_WORD, "short word {x:?}");
        }
        for p in w.windows(2) {
            assert!(p[0].end <= p[1].start, "overlap {:?} / {:?}", p[0], p[1]);
        }
    }

    #[test]
    fn word_after_a_pause_keeps_its_voice() {
        let vm = map(&[(0.2, 0.6), (1.5, 1.9)]);
        let mut w = words(&[("one", 0.2, 0.6), ("two", 0.6, 1.9)]);
        snap_words(&mut w, &vm);
        assert_eq!(bounds(&w), [(0.2, 0.6), (1.5, 1.9)]);
        // the swallowing word also took the previous word's tail
        let mut w = words(&[("one", 0.2, 0.55), ("two", 0.55, 1.9)]);
        snap_words(&mut w, &vm);
        assert_eq!(bounds(&w), [(0.2, 0.6), (1.5, 1.9)]);
        // the word was placed early: its voice is before the pause, the next word's onset after
        let mut w = words(&[("two", 0.2, 1.6), ("three", 1.6, 1.9)]);
        snap_words(&mut w, &vm);
        assert_eq!(bounds(&w), [(0.2, 0.6), (1.5, 1.9)]);
    }

    #[test]
    fn silence_is_trimmed_and_closures_kept() {
        let vm = map(&[(1.0, 1.2), (1.28, 1.5), (2.0, 2.3)]);
        let mut w = words(&[("decision", 0.9, 1.6), ("now", 1.6, 2.45)]);
        snap_words(&mut w, &vm);
        assert_eq!(bounds(&w), [(1.0, 1.5), (2.0, 2.3)]);
    }

    #[test]
    fn word_in_silence_moves_next_to_the_voice() {
        let vm = map(&[(1.0, 1.5)]);
        let mut w = words(&[("a", 0.8, 0.9), ("word", 1.0, 1.5), ("far", 3.0, 3.2)]);
        snap_words(&mut w, &vm);
        assert_eq!(bounds(&w), [(1.0, 1.06), (1.06, 1.5), (3.0, 3.2)]);
        assert_snap_invariants(&w);
    }

    #[test]
    fn unclaimed_voice_next_to_a_word_is_its_own() {
        let vm = map(&[(1.0, 1.6), (2.0, 2.4)]);
        let mut w = words(&[("tail", 1.1, 1.45), ("next", 1.45, 2.4)]);
        snap_words(&mut w, &vm);
        assert_eq!(bounds(&w), [(1.0, 1.6), (2.0, 2.4)]);
    }

    #[test]
    fn words_stay_ordered_long_enough_and_apart() {
        let vm = map(&[(0.0, 0.3), (0.31, 0.4), (1.0, 1.02), (2.0, 5.0)]);
        let mut w = words(&[
            ("a", 0.1, 0.1),
            ("b", 0.1, 0.1),
            ("c", 0.05, 0.02),
            ("d", 0.5, 0.52),
            ("e", 1.0, 1.0),
            ("f", 0.9, 4.0),
            ("g", 4.0, 4.01),
            ("h", 9.0, 8.0),
        ]);
        let long: Vec<bool> = w.iter().map(|x| x.end - x.start >= MIN_WORD).collect();
        snap_words(&mut w, &vm);
        for p in w.windows(2) {
            assert!(p[0].start <= p[0].end && p[0].end <= p[1].start, "overlap {:?} / {:?}", p[0], p[1]);
        }
        for (x, l) in w.iter().zip(long) {
            assert!(!l || x.end - x.start >= MIN_WORD, "short word {x:?}");
        }
        // with room, every word gets 60 ms
        let mut w = words(&[("so", 44.7, 44.78), ("I", 44.78, 44.78), ("guess", 44.78, 44.94), ("a", 45.5, 45.52)]);
        snap_words(&mut w, &map(&[(44.7, 45.0), (45.45, 45.6)]));
        assert_snap_invariants(&w);
        assert_eq!(bounds(&w), [(44.7, 44.78), (44.78, 44.84), (44.84, 45.0), (45.45, 45.6)]);
    }

    #[test]
    fn a_pile_of_zero_length_words_does_not_shift_the_rest() {
        let vm = map(&[(1.0, 1.5), (2.0, 2.5)]);
        let mut w: Vec<Word> = (0..100).map(|i| Word::new(format!("w{i}"), seconds_tick(1.0), seconds_tick(1.0))).collect();
        w.push(Word::new("next", seconds_tick(1.0), seconds_tick(1.5)));
        w.push(Word::new("after", seconds_tick(2.0), seconds_tick(2.5)));
        snap_words(&mut w, &vm);
        let b = bounds(&w);
        assert_eq!(b[100].1, 1.5);
        assert_eq!(b[101], (2.0, 2.5));
        for p in w.windows(2) {
            assert!(p[0].end <= p[1].start);
        }
    }

    #[test]
    fn no_voice_leaves_words_alone() {
        let mut w = words(&[("a", 0.1, 0.12), ("b", 0.5, 0.9)]);
        let before = w.clone();
        snap_words(&mut w, &VoiceMap::default());
        assert_eq!(w, before);
    }

    #[test]
    fn end_to_end_pause_swallowing_word() {
        let mut rng = Rng(17);
        let mut a = floor(3.0, Some(-70.0), &mut rng);
        tone(&mut a, 0.4, 0.8, -15.0);
        tone(&mut a, 1.9, 2.3, -15.0);
        let mut w = words(&[("first", 0.4, 0.8), ("second", 0.8, 2.3)]);
        let vm = voice_map(&a, &w);
        snap_words(&mut w, &vm);
        let b = bounds(&w);
        assert!(near(b[0].0, 0.4, 0.015) && near(b[0].1, 0.8, 0.015), "{b:?}");
        assert!(near(b[1].0, 1.9, 0.015) && near(b[1].1, 2.3, 0.015), "{b:?}");
    }
}
