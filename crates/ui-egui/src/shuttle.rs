//! Pitch-preserving shuttle playback: waveform-similarity overlap-add (WSOLA) time-scale
//! modification. The program mix is read at the device rate and re-assembled from overlapping
//! grains taken `speed` times as far apart as they are laid down; each grain is slid to where
//! it best continues the previous one, so periodic sound (voice, tones) joins without a click or
//! a flutter and keeps its pitch.

use filmcraft_engine::Session;
use filmcraft_project::ItemId;

type Mixer = Box<dyn FnMut(i64, &mut [f32], usize) + Send>;
const MAX_CHANNELS: usize = 64;
/// Above this device rate (no real device) the pitch is not kept: plain resampling.
const MAX_DEVICE_RATE: u32 = 768_000;
/// Source positions stay this far inside the `i64` range, so grain arithmetic cannot overflow.
const FRAME_MARGIN: i128 = 1 << 20;

pub(crate) fn mix(session: &Session, seq_id: ItemId, device_rate: u32, speed: f64) -> Option<(u32, Mixer)> {
    let rate = super::shuttle_mix_rate(device_rate, speed)?;
    if !session.prefs.audio.maintain_pitch_shuttling || rate == device_rate || device_rate > MAX_DEVICE_RATE {
        return Some((rate, Box::new(super::playback_mix(session, seq_id, rate))));
    }
    Some((rate, Box::new(corrected(super::playback_mix(session, seq_id, device_rate), device_rate, rate))))
}

/// `mix` (rendered at `from`, the device rate) played back at `from / to` times the speed with the
/// pitch kept. `cursor` counts output frames in units of `1 / to` of timeline time, as the plain
/// path's does.
fn corrected(mix: impl FnMut(i64, &mut [f32], usize) + Send + 'static, from: u32, to: u32) -> impl FnMut(i64, &mut [f32], usize) + Send {
    let mut stream = Wsola::new(Source::new(Box::new(mix)), from, to);
    move |cursor, buf: &mut [f32], ch| {
        if ch == 0 || ch > MAX_CHANNELS || !buf.len().is_multiple_of(ch) {
            buf.fill(0.0);
            return;
        }
        let n = buf.len() / ch;
        if n == 0 {
            return;
        }
        // reject extremes before any source position is computed from them
        let source = |frame: i128| (frame * i128::from(from)).div_euclid(i128::from(to.max(1)));
        let (first, last) = (source(i128::from(cursor)), source(i128::from(cursor) + n as i128));
        if first < i128::from(i64::MIN) + FRAME_MARGIN || last > i128::from(i64::MAX) - FRAME_MARGIN || i128::from(cursor) + n as i128 > i128::from(i64::MAX) {
            buf.fill(0.0);
            stream.expect = None;
            return;
        }
        stream.render(cursor, buf, ch);
    }
}

/// Frames asked of the mixer per call.
const READ_BLOCK: usize = 4096;
/// A request this far ahead of what was read is a jump: the gap is skipped, not decoded.
const JUMP: i64 = 1 << 17;

/// The program mix read strictly forward in contiguous blocks, so its running effect state
/// (reverb tails, denoise history) continues from call to call instead of being rebuilt, with
/// the frames still wanted kept for the overlapping grains.
struct Source {
    mix: Mixer,
    /// Interleaved frames from source frame `base` to `end`.
    buf: Vec<f32>,
    channels: usize,
    base: i64,
    end: i64,
}

impl Source {
    fn new(mix: Mixer) -> Self {
        Source { mix, buf: Vec::new(), channels: 0, base: 0, end: 0 }
    }

    /// `len` interleaved frames from source frame `start`; silence where the mixer gives none.
    fn region(&mut self, start: i64, len: usize, ch: usize) -> Vec<f32> {
        if start < self.base || start > self.end.saturating_add(JUMP) || self.channels != ch {
            // (also when the channel count changed: the kept frames have another stride)
            self.channels = ch;
            self.base = start;
            self.end = start;
            self.buf.clear();
        }
        let stop = start.saturating_add(len as i64);
        // frames before `start` are not needed again (grains only move forward)
        let skip = usize::try_from(start - self.base).unwrap_or(0).min(self.buf.len() / ch);
        self.buf.drain(..skip * ch);
        self.base += skip as i64;
        if self.base < start {
            self.base = start;
            self.end = start;
        }
        while self.end < stop {
            let n = usize::try_from(stop - self.end).unwrap_or(0).min(READ_BLOCK);
            let at = self.buf.len();
            self.buf.resize(at + n * ch, 0.0);
            if let Some(block) = self.buf.get_mut(at..) {
                (self.mix)(self.end, block, ch);
                block.iter_mut().for_each(|x| *x = if x.is_finite() { *x } else { 0.0 });
            }
            self.end += n as i64;
        }
        let mut region = vec![0.0f32; len * ch];
        let from = usize::try_from(start - self.base).unwrap_or(0) * ch;
        for (out, x) in region.iter_mut().zip(self.buf.iter().skip(from)) {
            *out = *x;
        }
        region
    }
}

/// Channels whose planes are kept for the alignment search, and how many of them (the most
/// energetic) it correlates; the shift is shared by all channels.
const GUIDE_CHANNELS: usize = 8;
const SEARCH_CHANNELS: usize = 2;

/// The previous grain: where it was taken from and the source region it was chosen in (one
/// plane per guide channel).
struct Prev {
    start: i64,
    base: i64,
    planes: Vec<Vec<f32>>,
}

struct Wsola {
    source: Source,
    from: u32,
    to: u32,
    /// Grain length; grains overlap by half and are laid `hop` apart.
    len: usize,
    hop: usize,
    /// How far a grain may slide either way from its nominal position.
    slide: usize,
    /// Samples averaged into one for the alignment search.
    decim: usize,
    /// The most multiply-adds one alignment search has needed (for the cost test).
    search_ops: std::cell::Cell<usize>,
    window: Vec<f32>,
    channels: usize,
    next_grain: i64,
    /// The cursor the next call must start at to continue the stream.
    expect: Option<i64>,
    prev: Option<Prev>,
    /// Interleaved overlap-add of the grains so far; `acc[0]` is output frame `acc_base`.
    acc: Vec<f32>,
    acc_base: i64,
}

impl Wsola {
    fn new(source: Source, from: u32, to: u32) -> Self {
        // ~40 ms grains: long enough to hold a few pitch periods of a low voice
        let len = ((u64::from(from) * 40 / 1000) as usize).clamp(256, 8192);
        // the alignment search runs on the signal averaged down to at most 48 kHz, so its cost
        // does not grow with the device rate; grain and slide are whole multiples of that factor
        let decim = (from as usize).div_ceil(48_000).max(1);
        // at least one averaged sample per half grain, at any rate
        let len = len.max(2 * decim);
        let len = len - len % (2 * decim);
        let hop = len / 2;
        let window = (0..len).map(|i| 0.5 - 0.5 * (std::f32::consts::TAU * i as f32 / len as f32).cos()).collect();
        Wsola {
            source,
            from,
            to,
            len,
            hop,
            slide: hop,
            decim,
            search_ops: std::cell::Cell::new(0),
            window,
            channels: 0,
            next_grain: 0,
            expect: None,
            prev: None,
            acc: Vec::new(),
            acc_base: 0,
        }
    }

    fn render(&mut self, cursor: i64, buf: &mut [f32], ch: usize) {
        let n = buf.len() / ch;
        let hop = self.hop as i64;
        if self.expect != Some(cursor) || self.channels != ch {
            // a seek (or first call): restart from the two grains that cover `cursor`
            self.channels = ch;
            self.next_grain = cursor.div_euclid(hop) - 1;
            self.acc_base = self.next_grain.saturating_mul(hop);
            self.acc.clear();
            self.prev = None;
        }
        let end = cursor.saturating_add(n as i64);
        let last = (end - 1).div_euclid(hop);
        while self.next_grain <= last {
            self.add_grain(self.next_grain);
            self.next_grain += 1;
        }
        let from = (cursor.saturating_sub(self.acc_base).max(0) as usize).saturating_mul(ch);
        for (i, out) in buf.iter_mut().enumerate() {
            *out = self.acc.get(from + i).copied().filter(|x| x.is_finite()).unwrap_or(0.0);
        }
        let done = ((end.saturating_sub(self.acc_base).max(0) as usize) * ch).min(self.acc.len());
        self.acc.drain(..done);
        self.acc_base = end;
        self.expect = Some(end);
    }

    fn add_grain(&mut self, j: i64) {
        let (ch, len, hop, slide) = (self.channels, self.len, self.hop, self.slide);
        // the grain's centre sits at output frame `j * hop + hop`, which plays source frame c
        let centre = (i128::from(j) * hop as i128 + hop as i128) * i128::from(self.from);
        let centre = (centre + i128::from(self.to) / 2).div_euclid(i128::from(self.to.max(1)));
        let Ok(nominal) = i64::try_from(centre - hop as i128) else { return };
        let region_start = nominal.saturating_sub(slide as i64);
        let region_len = len + 2 * slide;
        let region = self.source.region(region_start, region_len, ch);
        // Correlate per channel and add the scores: summing the waveforms first would cancel
        // out-of-phase stereo and leave nothing to align on.
        let frames: Vec<&[f32]> = region.chunks_exact(ch).collect();
        let planes: Vec<Vec<f32>> = (0..ch.min(GUIDE_CHANNELS))
            .map(|c| frames.chunks(self.decim).map(|g| g.iter().map(|f| f.get(c).copied().unwrap_or(0.0)).sum::<f32>() / g.len() as f32).collect())
            .collect();
        let shift = self.best_shift(&planes).map_or(slide, |k| k * self.decim);
        let offset = j.saturating_mul(hop as i64).saturating_sub(self.acc_base).max(0) as usize;
        if self.acc.len() < (offset + len) * ch {
            self.acc.resize((offset + len) * ch, 0.0);
        }
        for (i, w) in self.window.iter().enumerate() {
            let src = (shift + i) * ch;
            let dst = (offset + i) * ch;
            if let (Some(from), Some(to)) = (region.get(src..src + ch), self.acc.get_mut(dst..dst + ch)) {
                for (o, x) in to.iter_mut().zip(from) {
                    *o += x * w;
                }
            }
        }
        self.prev = Some(Prev { start: region_start + shift as i64, base: region_start, planes });
    }

    /// The offset into `planes` (the region read for this grain) at which a grain best continues
    /// the previous one: maximum normalised correlation of its first half with the stretch that
    /// followed the previous grain's own first half, over all guide channels. `None` without a
    /// usable previous grain or a signal (the nominal position then stands).
    fn best_shift(&self, planes: &[Vec<f32>]) -> Option<usize> {
        let (hop, slide) = (self.hop / self.decim, self.slide / self.decim);
        let prev = self.prev.as_ref()?;
        let natural_at = usize::try_from(prev.start.checked_add(self.hop as i64)?.checked_sub(prev.base)?).ok()? / self.decim;
        let naturals: Vec<&[f32]> = prev.planes.iter().map(|p| p.get(natural_at..natural_at + hop)).collect::<Option<_>>()?;
        if naturals.iter().flat_map(|n| n.iter()).map(|x| x * x).sum::<f32>() < 1e-9 {
            return None;
        }
        // correlate on the most energetic channels only; the shift is shared by all
        let mut order: Vec<usize> = (0..naturals.len()).collect();
        let level = |c: usize| naturals.get(c).map_or(0.0, |n| n.iter().map(|x| x * x).sum::<f32>());
        order.sort_by(|a, b| level(*b).total_cmp(&level(*a)));
        order.truncate(SEARCH_CHANNELS);
        let guide: Vec<(&Vec<f32>, &[f32])> = order.iter().filter_map(|c| Some((planes.get(*c)?, *naturals.get(*c)?))).collect();
        let mut best: Option<(f32, usize)> = None;
        let mut ops = 0;
        'lags: for k in 0..=2 * slide {
            let (mut dot, mut energy) = (0.0f32, 0.0f32);
            for (plane, natural) in &guide {
                let Some(candidate) = plane.get(k..k + hop) else { continue 'lags };
                for (a, b) in candidate.iter().zip(*natural).step_by(2) {
                    dot += a * b;
                    energy += a * a;
                    ops += 1;
                }
            }
            let score = if energy > 1e-9 { dot / energy.sqrt() } else { f32::MIN };
            let closer = |old: usize| k.abs_diff(slide) < old.abs_diff(slide);
            if best.is_none_or(|(s, at)| score > s || (score == s && closer(at))) {
                best = Some((score, k));
            }
        }
        self.search_ops.set(self.search_ops.get().max(ops));
        best.map(|(_, k)| k)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tone_mix(rate: u32, tones: Vec<f64>, burst: bool) -> impl FnMut(i64, &mut [f32], usize) + Send {
        move |start, buf: &mut [f32], ch| {
            for (i, frame) in buf.chunks_exact_mut(ch).enumerate() {
                let time = (start + i as i64) as f64 / f64::from(rate);
                for (sample, hz) in frame.iter_mut().zip(&tones) {
                    let on = !burst || (0.5..0.6).contains(&time);
                    *sample = if on { (0.3 * (std::f64::consts::TAU * hz * time).sin()) as f32 } else { 0.0 };
                }
            }
        }
    }

    fn amplitude(samples: &[f32], hz: f64, sr: u32) -> f64 {
        let (mut re, mut im) = (0.0, 0.0);
        for (i, x) in samples.iter().enumerate() {
            let phase = std::f64::consts::TAU * hz * i as f64 / f64::from(sr);
            re += f64::from(*x) * phase.cos();
            im += f64::from(*x) * phase.sin();
        }
        2.0 * re.hypot(im) / samples.len() as f64
    }

    fn rates(sr: u32, speed: f64) -> (u32, u32) {
        (sr, super::super::shuttle_mix_rate(sr, speed).unwrap())
    }

    #[test]
    fn original_tones_survive_shuttling_including_high_frequencies() {
        // each tone in its own run, the same on both channels of a stereo pair
        for sr in [44_100, 48_000, 96_000] {
            for speed in [0.5, 0.75, 1.5, 2.0, 4.0] {
                let (from, to) = rates(sr, speed);
                for hz in [440.0, 660.0, 1000.0, 6300.0, 9000.0, 18_000.0] {
                    let mut mix = corrected(tone_mix(from, vec![hz, hz], false), from, to);
                    let mut buf = vec![0.0; 16_000 * 2];
                    mix(i64::from(to), &mut buf, 2);
                    let samples: Vec<f32> = buf.iter().step_by(2).copied().collect();
                    let level = amplitude(&samples, hz, sr);
                    assert!(level > 0.1, "{sr} Hz, {speed}x: {hz} Hz level={level}");
                    if hz * speed < f64::from(sr) / 2.0 {
                        assert!(amplitude(&samples, hz * speed, sr) < level * 0.15, "uncorrected pitch at {speed}x: {hz} Hz");
                    }
                }
            }
        }
    }

    #[test]
    fn block_boundaries_and_seeks_do_not_change_the_output() {
        let (sr, speed) = (48_000, 4.0);
        let (from, to) = rates(sr, speed);
        let make = || corrected(tone_mix(from, vec![440.0, 660.0], false), from, to);
        let mut whole_mix = make();
        let mut whole = vec![0.0; 24_000];
        whole_mix(i64::from(to), &mut whole, 2);
        let mut cut_mix = make();
        let mut cut = Vec::new();
        let mut n = 0;
        for size in [1, 7, 333, 1024].into_iter().cycle() {
            let frames = size.min(12_000 - n);
            let mut block = vec![0.0; frames * 2];
            cut_mix(i64::from(to) + n as i64, &mut block, 2);
            cut.extend(block);
            n += frames;
            if n == 12_000 {
                break;
            }
        }
        assert_eq!(cut, whole);
        let mut sought = vec![0.0; 4096];
        cut_mix(0, &mut sought, 2);
        let mut fresh = vec![0.0; 4096];
        make()(0, &mut fresh, 2);
        assert_eq!(sought, fresh, "a backward seek must reset the pitch history");
    }

    #[test]
    fn pitch_latency_is_compensated_against_the_timeline() {
        for speed in [0.5, 2.0, 4.0] {
            let sr = 48_000;
            let (from, to) = rates(sr, speed);
            let mut mix = corrected(tone_mix(from, vec![440.0], true), from, to);
            let mut buf = vec![0.0; (f64::from(sr) * 1.4) as usize];
            mix(0, &mut buf, 1);
            let energy: f64 = buf.iter().map(|x| f64::from(*x).powi(2)).sum();
            let center: f64 = buf.iter().enumerate().map(|(i, x)| i as f64 / f64::from(sr) * f64::from(*x).powi(2)).sum::<f64>() / energy;
            let expected = 0.55 / speed;
            assert!(energy > 1.0);
            assert!((center - expected).abs() < 0.025 / speed, "{speed}x: burst at {center}s, expected {expected}s");
            assert!(buf.iter().take((0.4 / speed * f64::from(sr)) as usize).all(|x| x.abs() < 1e-6), "audio must not arrive early");
        }
    }

    #[test]
    fn hostile_coordinates_and_channel_counts_produce_silence() {
        let (from, to) = rates(48_000, 4.0);
        let mut mix = corrected(tone_mix(from, vec![440.0, 660.0], false), from, to);
        for (cursor, ch, len) in [(i64::MIN, 2, 2048), (i64::MAX, 2, 2048), (0, 0, 2048), (0, 65, 65), (0, 2, 3)] {
            let mut buf = vec![1.0; len];
            mix(cursor, &mut buf, ch);
            assert!(buf.iter().all(|x| *x == 0.0));
        }
        mix(0, &mut [], 2);
        let mut buf = vec![0.0; 2048];
        mix(i64::from(to), &mut buf, 2);
        assert!(buf.iter().all(|x| x.is_finite()));
        assert!(buf.iter().any(|x| x.abs() > 0.1));
    }

    /// A stationary voiced-speech stand-in: harmonics of `f0` falling off as 1/h.
    fn voice_mix(rate: u32, f0: f64) -> impl FnMut(i64, &mut [f32], usize) + Send {
        move |start, buf: &mut [f32], ch| {
            for (i, frame) in buf.chunks_exact_mut(ch).enumerate() {
                let t = (start + i as i64) as f64 / f64::from(rate);
                let v: f64 = (1..=30).map(|h| (std::f64::consts::TAU * f0 * h as f64 * t).sin() / h as f64).sum();
                frame.fill((0.2 * v) as f32);
            }
        }
    }

    /// Share of the (Hann-windowed) spectrum power lying within ±8 Hz of a harmonic of `f0`.
    fn harmonic_share(x: &[f32], f0: f64, sr: u32) -> f64 {
        let n = 8192;
        let (mut near, mut all) = (0.0, 0.0);
        for frame in x.chunks_exact(n).take(2) {
            let win: Vec<f64> =
                frame.iter().enumerate().map(|(i, v)| f64::from(*v) * (0.5 - 0.5 * (std::f64::consts::TAU * i as f64 / n as f64).cos())).collect();
            let mut hz = 20.0;
            while hz < 6_500.0 {
                let (mut re, mut im) = (0.0, 0.0);
                for (i, w) in win.iter().enumerate() {
                    let ph = std::f64::consts::TAU * hz * i as f64 / f64::from(sr);
                    re += w * ph.cos();
                    im += w * ph.sin();
                }
                let p = re * re + im * im;
                let off = (hz / f0 - (hz / f0).round()).abs() * f0;
                all += p;
                if off < 8.0 {
                    near += p;
                }
                hz += 4.0;
            }
        }
        near / all
    }

    /// #2110: a low voice shuttled with Maintain Pitch stayed a clean harmonic series (the old
    /// phase-vocoder path smeared it to noise at 2x and 4x).
    /// Opposite-polarity stereo (R = -L) cancels in a mono sum; the shuttle still has to keep its pitch.
    #[test]
    fn out_of_phase_stereo_keeps_its_pitch() {
        let sr = 48_000;
        let (from, to) = rates(sr, 2.0);
        let mut tone = tone_mix(from, vec![440.0], false);
        let out_of_phase = move |start: i64, buf: &mut [f32], ch: usize| {
            tone(start, &mut buf[..], ch);
            for frame in buf.chunks_exact_mut(ch) {
                frame[1] = -frame[0];
            }
        };
        let mut mix = corrected(out_of_phase, from, to);
        let mut buf = vec![0.0; 2 * 24_000];
        mix(i64::from(to), &mut buf, 2);
        let left: Vec<f32> = buf.iter().step_by(2).skip(2_000).copied().collect();
        let wanted = amplitude(&left, 440.0, sr);
        assert!(wanted > 0.2, "{wanted}");
        for hz in [400.0, 430.0, 460.0, 480.0] {
            assert!(amplitude(&left, hz, sr) < wanted * 0.15, "{hz} Hz leaked");
        }
    }

    /// The alignment search of one grain costs the same at any device rate, and stays inside what
    /// a mixing thread can do in real time at 96 kHz and 192 kHz with six channels.
    #[test]
    fn the_alignment_search_cost_does_not_grow_with_the_sample_rate() {
        let mut worst = Vec::new();
        for (rate, ch) in [(48_000u32, 6usize), (96_000, 6), (192_000, 6), (192_000, 8)] {
            let mut stream = Wsola::new(Source::new(Box::new(tone_mix_n(rate, ch))), rate, rate / 2);
            let mut buf = vec![0.0; 4096 * ch];
            for block in 0..6 {
                stream.render(i64::from(rate / 2) + block * 4096, &mut buf, ch);
            }
            worst.push(stream.search_ops.get());
        }
        // at most 2 channels x 1921 lags x 240 samples per 20 ms grain, at any rate
        assert!(worst[0] > 100_000, "{worst:?}");
        assert!(worst.iter().all(|w| *w <= 2_000_000), "{worst:?}");
    }

    fn tone_mix_n(rate: u32, ch: usize) -> impl FnMut(i64, &mut [f32], usize) + Send {
        let mut inner = tone_mix(rate, (0..ch).map(|c| 300.0 + 170.0 * c as f64).collect(), false);
        move |start, buf: &mut [f32], channels| {
            debug_assert_eq!(channels, ch);
            inner(start, buf, channels);
        }
    }

    /// An absurd device rate must not give an empty grain (which divided by zero).
    #[test]
    fn an_absurd_sample_rate_still_has_a_grain() {
        for rate in [1u32, 8_000, 768_000, 200_000_000, u32::MAX] {
            let mut stream = Wsola::new(Source::new(Box::new(tone_mix_n(48_000, 1))), rate, (rate / 2).max(1));
            assert!(stream.len >= 2 && stream.hop >= 1 && stream.len.is_multiple_of(2), "{rate}: len {} hop {}", stream.len, stream.hop);
            let mut buf = vec![0.0; 64];
            stream.render(0, &mut buf, 1);
            assert!(buf.iter().all(|x| x.is_finite()), "{rate}");
        }
    }

    /// Frames kept from a stereo stream must not be read with a mono stride after the device
    /// changes its channel count.
    #[test]
    fn a_change_of_channel_count_starts_the_source_afresh() {
        let (from, to) = rates(48_000, 2.0);
        let two_channels = |start: i64, buf: &mut [f32], ch: usize| {
            // channel 0 is a 440 Hz tone, channel 1 a 660 Hz one; mono is the first only
            for (i, frame) in buf.chunks_exact_mut(ch).enumerate() {
                let t = (start + i as i64) as f64 / 48_000.0;
                for (c, x) in frame.iter_mut().enumerate() {
                    *x = (0.3 * (std::f64::consts::TAU * (440.0 + 220.0 * c as f64) * t).sin()) as f32;
                }
            }
        };
        let mut switched = corrected(two_channels, from, to);
        let mut stereo = vec![0.0; 4096 * 2];
        switched(i64::from(to), &mut stereo, 2);
        let mut mono_after = vec![0.0; 4096];
        switched(i64::from(to) + 4096, &mut mono_after, 1);
        let mut fresh = corrected(two_channels, from, to);
        let mut mono_fresh = vec![0.0; 4096];
        fresh(i64::from(to) + 4096, &mut mono_fresh, 1);
        assert!(mono_fresh.iter().any(|x| x.abs() > 0.1), "the reference is silent");
        assert_eq!(mono_after, mono_fresh);
    }

    /// The source's kept frames have the stride of the channel count they were read with.
    #[test]
    fn the_source_drops_its_frames_when_the_channel_count_changes() {
        let frame_value = |start: i64, buf: &mut [f32], ch: usize| {
            for (i, frame) in buf.chunks_exact_mut(ch).enumerate() {
                for (c, x) in frame.iter_mut().enumerate() {
                    *x = (start + i as i64) as f32 + 0.5 * c as f32;
                }
            }
        };
        let mut source = Source::new(Box::new(frame_value));
        source.region(0, 3 * READ_BLOCK, 2);
        // continuing inside what was read, now with one channel
        let mono = source.region(READ_BLOCK as i64, 100, 1);
        let want: Vec<f32> = (0..100).map(|i| (READ_BLOCK + i) as f32).collect();
        assert_eq!(mono, want);
    }

    /// The mixer keeps effect state between calls that continue where the last one ended, so
    /// the shuttle must read it as one forward stream, never grain by grain.
    #[test]
    fn the_mixer_is_read_as_one_contiguous_stream() {
        use std::sync::{Arc, Mutex};
        for speed in [0.5, 0.75, 1.5, 2.0, 4.0] {
            let calls = Arc::new(Mutex::new(Vec::<(i64, usize)>::new()));
            let log = calls.clone();
            let mut inner = tone_mix(48_000, vec![440.0], false);
            let (from, to) = rates(48_000, speed);
            let mut mix = corrected(
                move |start: i64, buf: &mut [f32], ch: usize| {
                    if let Ok(mut c) = log.lock() {
                        c.push((start, buf.len() / ch));
                    }
                    inner(start, buf, ch);
                },
                from,
                to,
            );
            let mut buf = vec![0.0; 1000];
            for block in 0..60 {
                mix(i64::from(to) + block * 1000, &mut buf, 1);
            }
            let calls = calls.lock().map(|c| c.clone()).unwrap_or_default();
            assert!(calls.len() > 10, "{speed}x: {calls:?}");
            for pair in calls.windows(2) {
                assert_eq!(pair[1].0, pair[0].0 + pair[0].1 as i64, "{speed}x: a read that does not continue the last: {pair:?}");
            }
        }
    }

    #[test]
    fn voice_stays_harmonic_when_shuttled() {
        let sr = 48_000;
        for f0 in [90.0, 120.0, 200.0] {
            for speed in [1.5, 2.0, 4.0] {
                let (from, to) = rates(sr, speed);
                let mut mix = corrected(voice_mix(from, f0), from, to);
                let mut buf = vec![0.0; 32_000];
                mix(i64::from(to) * 2, &mut buf, 1);
                let share = harmonic_share(&buf[4_000..], f0, sr);
                assert!(share > 0.9, "{f0} Hz voice at {speed}x: {share:.3} of the energy on its harmonics");
            }
        }
    }
}
