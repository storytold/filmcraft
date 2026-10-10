//! Clip-level audio: clip gain → clip effects → Volume / Channel Volume / Panner, audio transitions
//! and speed changes (varispeed resampling), summed per track. The track/submix/Mix graph that
//! consumes it is [`crate::mixer`].
//!
//! Stereo, mono and adaptive tracks carry two channels. 5.1 tracks carry six (L, R, C, LFE, Ls,
//! Rs): their clips play the source's first six channels (or the six `source_channels` picked with
//! Modify ▸ Audio Channels); mono and stereo sources on a 5.1 track are upmixed onto the front
//! speakers ([`filmcraft_audio_dsp::channels`]). The clip Panner (stereo balance) does not apply to
//! 5.1 clips, and Channel Volume's left/right levels apply to L and R.

use std::collections::HashMap;

use filmcraft_audio_dsp::channels::{self as chans, Layout, Mixdown};
use filmcraft_frame::AudioBuffer;
use filmcraft_project::{AudioChannels, Project, Sequence, Track, TrackId, TrackItem};
use filmcraft_time::{Tick, TimeRange};

use crate::mixer::Override;
use crate::{SourceProvider, transitions};

/// Live Clip Mixer lanes ([`crate::mixer::LiveMix`] overrides keyed by the track id): the clip
/// Volume level (dB) and Panner balance of the clip under the playhead while a Clip Mixer control
/// is held.
pub const CLIP_LANE_VOLUME: &str = "clip.volume";
pub const CLIP_LANE_PAN: &str = "clip.pan";

pub fn db_to_gain(db: f64) -> f32 {
    if db <= -96.0 { 0.0 } else { 10f64.powf(db / 20.0) as f32 }
}

/// Mix `frames` stereo samples of sequence audio starting at sample `start` (sequence rate), through
/// the full mixer graph ([`crate::mixer`]). A 5.1 Mix is folded to stereo with the ITU-R BS.775
/// downmix. Export and playback use [`mix_sequence_layout`] for other channel counts.
pub fn mix_sequence(project: &Project, seq: &Sequence, start: i64, frames: usize, sources: &dyn SourceProvider) -> AudioBuffer {
    to_layout(crate::mixer::mix_graph(project, seq, start, frames, sources, None), Layout::Stereo, Mixdown::FrontRear)
}

/// The sequence mix in `layout` (mono, stereo or 5.1): a 5.1 Mix is downmixed with `mixdown`
/// (BS.775 for [`Mixdown::FrontRear`]), a stereo Mix is upmixed onto the front speakers.
pub fn mix_sequence_layout(
    project: &Project,
    seq: &Sequence,
    start: i64,
    frames: usize,
    sources: &dyn SourceProvider,
    layout: Layout,
    mixdown: Mixdown,
) -> AudioBuffer {
    to_layout(crate::mixer::mix_graph(project, seq, start, frames, sources, None), layout, mixdown)
}

/// Convert a mixed buffer (2 or 6 channels) to `layout`.
pub fn to_layout(b: AudioBuffer, layout: Layout, mixdown: Mixdown) -> AudioBuffer {
    if b.channels.len() == layout.channels() {
        return b;
    }
    AudioBuffer { sample_rate: b.sample_rate, channels: chans::convert_owned(b.channels, layout, mixdown) }
}

/// The layout of a sequence's Mix (`SequenceSettings::audio_master`; mono and adaptive Mixes play
/// as stereo).
pub fn master_layout(seq: &Sequence) -> Layout {
    if seq.settings.audio_master == AudioChannels::Surround51 { Layout::Surround51 } else { Layout::Stereo }
}

/// The summed clip audio of one track (clip gain, clip effects, Volume / Channel Volume / Panner,
/// audio transitions) for sequence samples `[start, start + frames)`: the track's mixer input
/// (2 channels, or 6 for a 5.1 track).
pub fn track_input(project: &Project, track: &Track, start: i64, frames: usize, sr: u32, sources: &dyn SourceProvider) -> AudioBuffer {
    track_input_live(project, track, start, frames, sr, sources, &HashMap::new())
}

/// [`track_input`] with the live mixer overrides: held Clip Mixer controls ([`CLIP_LANE_VOLUME`],
/// [`CLIP_LANE_PAN`] on this track) replace the clip's Volume / Panner values while held.
pub fn track_input_live(
    project: &Project,
    track: &Track,
    start: i64,
    frames: usize,
    sr: u32,
    sources: &dyn SourceProvider,
    ovs: &HashMap<(TrackId, String), Override>,
) -> AudioBuffer {
    track_input_at(project, track, start, frames, sr, sources, ovs, 0)
}

/// [`track_input_live`] for a track `depth` nested sequences down from the one being mixed.
#[allow(clippy::too_many_arguments)]
pub(crate) fn track_input_at(
    project: &Project,
    track: &Track,
    start: i64,
    frames: usize,
    sr: u32,
    sources: &dyn SourceProvider,
    ovs: &HashMap<(TrackId, String), Override>,
    depth: u32,
) -> AudioBuffer {
    let w = crate::mixer::width_of(track.channels);
    let live =
        ClipLive { vol: ovs.get(&(track.id, CLIP_LANE_VOLUME.to_string())).copied(), pan: ovs.get(&(track.id, CLIP_LANE_PAN.to_string())).copied(), depth };
    let range = TimeRange::from_bounds(Tick::from_units(start, sr as i64), Tick::from_units(start + frames as i64, sr as i64));
    let mut tbuf = AudioBuffer::silence(sr, w, frames);
    for item in track.items.iter().filter(|i| i.enabled && i.range().overlaps(&range)) {
        mix_item(project, item, start, frames, sr, sources, &mut tbuf, 1.0, &live);
    }
    // audio transitions: attenuate the covered region of each side
    for tr in &track.transitions {
        if !tr.range().overlaps(&range) {
            continue;
        }
        // Re-mix: recompute the covered samples with crossfade gains.
        let from = tr.from.and_then(|id| track.item(id));
        let to = tr.to.and_then(|id| track.item(id));
        let s0 = tr.start.to_units_floor(sr as i64).max(start);
        let s1 = tr.end().to_units_floor(sr as i64).min(start + frames as i64);
        if s1 <= s0 {
            continue;
        }
        let n = (s1 - s0) as usize;
        let off = (s0 - start) as usize;
        for ch in tbuf.channels.iter_mut() {
            ch[off..off + n].fill(0.0);
        }
        let mut a = AudioBuffer::silence(sr, w, n);
        let mut b = AudioBuffer::silence(sr, w, n);
        if let Some(f) = from {
            mix_item(project, f, s0, n, sr, sources, &mut a, 1.0, &live);
        }
        if let Some(tt) = to {
            mix_item(project, tt, s0, n, sr, sources, &mut b, 1.0, &live);
        }
        let dur = (tr.end() - tr.start).to_units_floor(sr as i64).max(1) as f32;
        let base = (s0 - tr.start.to_units_floor(sr as i64)) as f32;
        for i in 0..n {
            let p = (base + i as f32) / dur;
            let (ga, gb) = transitions::audio_gains(&tr.effect.effect, p);
            for c in 0..w {
                tbuf.channels[c][off + i] += a.channels[c][i] * ga + b.channels[c][i] * gb;
            }
        }
    }
    tbuf
}

/// Held Clip Mixer controls of one track.
#[derive(Default)]
struct ClipLive {
    vol: Option<Override>,
    pan: Option<Override>,
    /// How many nested sequences deep this track is ([`crate::MAX_NEST_DEPTH`] ends the descent).
    depth: u32,
}

/// Balance for stereo signals (clip Panner, stereo track pan): centre = unity on both sides, turning
/// one way attenuates the other side along the −3 dB constant-power curve (normalised so the
/// centre is 0 dB); hard left/right silences the opposite side.
pub fn pan_gains(pan: f32) -> (f32, f32) {
    let a = (pan + 1.0) * std::f32::consts::FRAC_PI_4;
    let k = std::f32::consts::SQRT_2;
    ((a.cos() * k).min(1.0), (a.sin() * k).min(1.0))
}

#[allow(clippy::too_many_arguments)]
fn mix_item(
    project: &Project,
    item: &TrackItem,
    start: i64,
    frames: usize,
    sr: u32,
    sources: &dyn SourceProvider,
    out: &mut AudioBuffer,
    extra: f32,
    live: &ClipLive,
) {
    let item_s0 = item.start.to_units_floor(sr as i64);
    let item_s1 = item.end().to_units_floor(sr as i64);
    let a0 = start.max(item_s0);
    let a1 = (start + frames as i64).min(item_s1);
    if a1 <= a0 {
        return;
    }
    if item.essential.as_ref().is_some_and(|e| e.mute) {
        return;
    }
    let w = out.channels.len();
    let n = (a1 - a0) as usize;
    let buf = if let Some(nested) = project.sequence(item.item) {
        // Past the depth limit a nest is silent, like its picture is empty, so a sequence that
        // contains itself (a damaged project; the editor refuses to make one) cannot recurse
        // without end.
        if live.depth >= crate::MAX_NEST_DEPTH {
            return;
        }
        // A multi-camera clip plays the audio of its source's audio setting: camera 1, all
        // cameras, or (when switching audio) the angle the clip selects.
        let q = if nested.multicam.is_some() {
            std::borrow::Cow::Owned(nested.with_angle_audio(item.multicam_angle(nested)))
        } else {
            std::borrow::Cow::Borrowed(nested)
        };
        // The nested sequence is the clip's source: speed, reverse and clip effects apply to its
        // mix exactly as they do to a media clip's sound.
        let read = |m0: i64, len: usize| Some(nested_mix(project, &q, m0, len, sr, sources, w, live.depth));
        effected(item, &read, a0, n, sr, w)
    } else {
        let Some(src) = sources.source(item.item) else { return };
        if !src.info().has_audio() {
            return;
        }
        effected(item, &|m0, len| src.audio_stream(item.audio_stream, m0, len, sr).ok(), a0, n, sr, w)
    };
    // gains: clip gain × Volume (keyframed, per 64-sample block) × channel volume × panner
    let clip_gain = db_to_gain(item.gain_db);
    let vol = item.effect("volume").filter(|e| e.enabled && !e.param("bypass").and_then(|p| p.value.as_bool()).unwrap_or(false));
    let chv = item.effect("channel_volume").filter(|e| e.enabled && !e.param("bypass").and_then(|p| p.value.as_bool()).unwrap_or(false));
    let pan = item.effect("panner").filter(|e| e.enabled);
    let off = (a0 - start) as usize;
    // Gains change on an absolute 64-sample grid, so the output does not depend on how callers cut
    // the timeline into requests.
    let mut blk = 0usize;
    while blk < n {
        let t_tl = Tick::from_units((a0 + blk as i64).div_euclid(64) * 64, sr as i64);
        let mt = item.source_time_at(t_tl);
        let level = |auto: f64| live.vol.map(|o| o.at(t_tl, auto)).unwrap_or(auto);
        let v = match vol {
            Some(e) => db_to_gain(level(e.f64_at("level", mt))),
            None if live.vol.is_some() => db_to_gain(level(0.0)),
            None => 1.0,
        };
        let (cl, cr) = chv.map(|e| (db_to_gain(e.f64_at("left", mt)), db_to_gain(e.f64_at("right", mt)))).unwrap_or((1.0, 1.0));
        let balance = |auto: f64| live.pan.map(|o| o.at(t_tl, auto)).unwrap_or(auto);
        let (pl, pr) = match pan {
            Some(e) => pan_gains((balance(e.f64_at("balance", mt)) / 100.0) as f32),
            None if live.pan.is_some() => pan_gains((balance(0.0) / 100.0) as f32),
            None => (1.0, 1.0),
        };
        let g = clip_gain * v * extra;
        let end = (((a0 + blk as i64).div_euclid(64) + 1) * 64 - a0).min(n as i64) as usize;
        if w == 2 {
            for i in blk..end {
                let (l, r) = (buf[0][i], buf[1][i]);
                out.channels[0][off + i] += l * g * cl * pl;
                out.channels[1][off + i] += r * g * cr * pr;
            }
        } else {
            for (c, (dst, src)) in out.channels.iter_mut().zip(&buf).enumerate() {
                let gc = g * match c {
                    chans::L => cl,
                    chans::R => cr,
                    _ => 1.0,
                };
                for i in blk..end {
                    dst[off + i] += src[i] * gc;
                }
            }
        }
        blk = end;
    }
}

/// `len` samples of a nested sequence's mix starting at sample `m0` of its own time, counted at
/// `sr` (the rate of the sequence the nest is in), as `w` channels. The nested sequence is mixed at
/// its own sample rate and converted when the two differ; time before its start is silent.
/// `depth` is the nesting depth of the sequence the nest is in.
#[allow(clippy::too_many_arguments)]
fn nested_mix(project: &Project, nested: &Sequence, m0: i64, len: usize, sr: u32, sources: &dyn SourceProvider, w: usize, depth: u32) -> AudioBuffer {
    // a damaged project can claim any rate: keep the buffer below sized by a real one
    let own = nested.settings.sample_rate.clamp(1_000, 768_000);
    let sr = sr.max(1);
    // the nested sequence's samples that cover [m0, m0 + len) at `sr`
    let at = |i: i64| i as f64 * own as f64 / sr as f64;
    let first = if own == sr { m0 } else { at(m0).floor() as i64 };
    let count = if own == sr { len } else { (at(m0.saturating_add(len as i64)).ceil() as i64).saturating_sub(first).max(0) as usize + 2 };
    // nothing plays before the sequence starts
    let lead = first.min(0).unsigned_abs().min(count as u64) as usize;
    let mut mix = AudioBuffer::silence(own, w, count);
    if count > lead {
        let b = crate::mixer::mix_graph_at(project, nested, first + lead as i64, count - lead, sources, None, depth + 1);
        let b = to_layout(b, Layout::from_channels(w), Mixdown::FrontRear);
        for (dst, src) in mix.channels.iter_mut().zip(&b.channels) {
            let m = src.len().min(count - lead);
            dst[lead..lead + m].copy_from_slice(&src[..m]);
        }
    }
    if own == sr {
        mix.sample_rate = sr;
        return mix;
    }
    let mut out = AudioBuffer::silence(sr, w, len);
    for (dst, src) in out.channels.iter_mut().zip(&mix.channels) {
        for (i, d) in dst.iter_mut().enumerate() {
            let pos = (at(m0 + i as i64) - first as f64).max(0.0);
            let i0 = pos.floor() as usize;
            let fr = (pos - i0 as f64) as f32;
            let a = src.get(i0).copied().unwrap_or(0.0);
            let b = src.get(i0 + 1).copied().unwrap_or(a);
            *d = a + (b - a) * fr;
        }
    }
    out
}

/// A clip's source sound: `len` samples from sample `m0` of the source's own time, at the
/// sequence's sample rate (a media source's audio, or a nested sequence's mix).
type SourceAudio<'a> = &'a dyn Fn(i64, usize) -> Option<AudioBuffer>;

/// The clip's audio after speed/reverse and clip effects (before clip gain, Volume and Panner) for
/// timeline samples `[a0, a0 + n)`, `w` channels.
fn effected(item: &TrackItem, src: SourceAudio, a0: i64, n: usize, sr: u32, w: usize) -> Vec<Vec<f32>> {
    let read = |x0: i64, len: usize| raw_channels(item, src, x0, len, sr, w);
    if crate::audio_fx::has_effects(item) { crate::audio_fx::process(item, a0, n, sr, &read) } else { read(a0, n) }
}

/// One clip's own signal for timeline samples `[start, start + frames)` (sequence rate): clip gain
/// and clip effects, without Volume / Channel Volume / Panner, transitions or Mute (silence outside
/// the clip). Loudness Auto-Match and ducking analyse this. `None` when the clip has no audio
/// source (nested sequences are not analysed).
pub fn clip_signal(item: &TrackItem, start: i64, frames: usize, sr: u32, sources: &dyn SourceProvider) -> Option<[Vec<f32>; 2]> {
    let src = sources.source(item.item)?;
    if !src.info().has_audio() {
        return None;
    }
    let mut out = [vec![0.0f32; frames], vec![0.0f32; frames]];
    let a0 = start.max(item.start.to_units_floor(sr as i64));
    let a1 = (start + frames as i64).min(item.end().to_units_floor(sr as i64));
    if a1 > a0 {
        let n = (a1 - a0) as usize;
        let off = (a0 - start) as usize;
        let g = db_to_gain(item.gain_db);
        let buf = effected(item, &|m0, len| src.audio_stream(item.audio_stream, m0, len, sr).ok(), a0, n, sr, 2);
        for (dst, b) in out.iter_mut().zip(&buf) {
            for (d, x) in dst[off..off + n].iter_mut().zip(b) {
                *d = x * g;
            }
        }
    }
    Some(out)
}

/// Whether a clip plays one source channel on both sides: a mono source, or one picked channel
/// (Modify ▸ Audio Channels, Breakout to Mono). Loudness Auto-Match measures such a clip as the one
/// channel it is, as BS.1770 (and ffmpeg's `ebur128`) measure a mono programme, not as the
/// dual-mono pair [`clip_signal`] plays, which reads 3 LU louder. `None` when the clip has no
/// audio source.
pub fn clip_is_mono(item: &TrackItem, sources: &dyn SourceProvider) -> Option<bool> {
    let src = sources.source(item.item)?;
    let available = src.info().audio_streams.get(item.audio_stream)?.channels as usize;
    let (left, right) = source_pair(item, available);
    Some(left == right)
}

/// The clip's audio (after speed/reverse) for timeline samples `[x0, x0 + len)` as `w` channels
/// (2: stereo, mono sources on both sides; 6: 5.1), with silence outside the clip.
fn raw_channels(item: &TrackItem, src: SourceAudio, x0: i64, len: usize, sr: u32, w: usize) -> Vec<Vec<f32>> {
    let mut out = vec![vec![0.0f32; len]; w];
    let item_s0 = item.start.to_units_floor(sr as i64);
    let item_s1 = item.end().to_units_floor(sr as i64);
    let a0 = x0.max(item_s0);
    let a1 = (x0 + len as i64).min(item_s1);
    if a1 <= a0 {
        return out;
    }
    let n = (a1 - a0) as usize;
    let off = (a0 - x0) as usize;
    let speed = item.speed.abs().max(1e-6);
    let src_start_ticks = item.source_time_at(Tick::from_units(a0, sr as i64));
    let remixed = crate::remix::read(item, &|m0, len| src(m0, len), a0 - item_s0, n, sr);
    let buf = if remixed.is_some() {
        remixed
    } else if (speed - 1.0).abs() < 1e-9 && !item.reverse {
        src(src_start_ticks.to_units_floor(sr as i64), n)
    } else {
        // varispeed: read a longer span and resample linearly
        let span = ((n as f64) * speed).ceil() as usize + 2;
        let s0 = if item.reverse { src_start_ticks.to_units_floor(sr as i64) - span as i64 } else { src_start_ticks.to_units_floor(sr as i64) };
        src(s0, span).map(|raw| {
            let mut b = AudioBuffer::silence(sr, raw.channel_count(), n);
            for (c, ch) in raw.channels.iter().enumerate() {
                for i in 0..n {
                    let pos = if item.reverse { span as f64 - 1.0 - i as f64 * speed } else { i as f64 * speed };
                    let i0 = pos.floor().max(0.0) as usize;
                    let fr = (pos - i0 as f64) as f32;
                    let a = ch.get(i0).copied().unwrap_or(0.0);
                    let bb = ch.get(i0 + 1).copied().unwrap_or(a);
                    b.channels[c][i] = a + (bb - a) * fr;
                }
            }
            b
        })
    };
    let Some(buf) = buf else { return out };
    if buf.channels.is_empty() {
        return out;
    }
    if w == 2 {
        let (left, right) = source_pair(item, buf.channel_count());
        for (c, dst) in out.iter_mut().enumerate() {
            let ch = &buf.channels[if c == 0 { left } else { right }];
            let m = n.min(ch.len());
            dst[off..off + m].copy_from_slice(&ch[..m]);
        }
        return out;
    }
    // 5.1 track: pick or upmix the source channels
    let picked: Vec<&[f32]> = source_51(item, buf.channel_count()).iter().map(|&c| buf.channels[c].as_slice()).collect();
    let layout = Layout::from_channels(picked.len());
    let conv =
        if picked.len() == 6 { picked.iter().map(|c| c.to_vec()).collect() } else { chans::convert(&picked, layout, Layout::Surround51, Mixdown::FrontRear) };
    for (dst, ch) in out.iter_mut().zip(&conv) {
        let m = n.min(ch.len());
        dst[off..off + m].copy_from_slice(&ch[..m]);
    }
    out
}

/// The source channels a clip on a 5.1 track plays: six picked channels (Modify ▸ Audio Channels),
/// else the source's first six; a mono pick or source gives one channel (upmixed to C), a stereo
/// pick or 2–5 channel source gives two (upmixed to L/R).
pub fn source_51(item: &TrackItem, available: usize) -> Vec<usize> {
    let ok = |c: u16| if (c as usize) < available { c as usize } else { 0 };
    match item.source_channels.len() {
        0 => match available {
            0 | 1 => vec![0],
            2..=5 => vec![0, 1],
            _ => (0..6).collect(),
        },
        1 => vec![ok(item.source_channels[0])],
        2..=5 => vec![ok(item.source_channels[0]), ok(item.source_channels[1])],
        _ => item.source_channels.iter().take(6).map(|&c| ok(c)).collect(),
    }
}

/// The source channels (left, right) an audio clip plays: its `source_channels` (one = mono on
/// both sides; Modify ▸ Audio Channels, Breakout to Mono), else the first two (mono sources on
/// both sides). Channels the source doesn't have fall back to the first.
pub fn source_pair(item: &TrackItem, available: usize) -> (usize, usize) {
    let ok = |c: u16| if (c as usize) < available { c as usize } else { 0 };
    match item.source_channels.as_slice() {
        [] => (0, if available >= 2 { 1 } else { 0 }),
        [m] => (ok(*m), ok(*m)),
        [l, r, ..] => (ok(*l), ok(*r)),
    }
}

/// Peak (min, max) pairs per bucket of `bucket` samples for channel `ch` — waveform display.
pub fn peaks(buf: &AudioBuffer, ch: usize, bucket: usize) -> Vec<(f32, f32)> {
    let Some(c) = buf.channels.get(ch) else { return Vec::new() };
    c.chunks(bucket.max(1)).map(|b| b.iter().fold((0f32, 0f32), |(lo, hi), s| (lo.min(*s), hi.max(*s)))).collect()
}
