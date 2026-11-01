//! Synchronising clips: Clip ▸ Synchronize (timeline clips), Merge Clips and Create
//! Multi-Camera Source Sequence (project items).
//!
//! Every method reduces a clip to an **anchor**: the media time of the clip that lines up with
//! one common instant (common time = media time − anchor).
//!
//! | method | anchor |
//! |---|---|
//! | `in` | the In point (project items: the source In mark, else the start; timeline clips: their source In) |
//! | `out` | the Out point (exclusive end) |
//! | `timecode` | −(start timecode), optionally ignoring hours |
//! | `marker` | the first clip marker (or the first one named `marker`) |
//! | `audio` | from the waveform cross-correlation against the reference clip ([`filmcraft_audio_dsp::sync`]) |
//!
//! `offset` (frames) shifts every clip but the reference by that much (a known delay to correct).

use filmcraft_audio_dsp::sync::{SyncOptions, SyncResult, find_offset, mixdown};
use filmcraft_project::{ItemId, Project, TrackItem};
use filmcraft_time::{FrameRate, TICKS_PER_SECOND, Tick, TimeRange};
use serde_json::{Value, json};

use crate::{EngineError, Result, Session};

/// Sample rate audio is analysed at.
pub const ANALYSIS_RATE: u32 = 48_000;
/// Seconds of audio decoded per read while analysing (bounds the buffer, whatever the channel count).
const ANALYSIS_CHUNK_SECONDS: u64 = 60;
/// Longest stretch of a clip analysed for audio sync (from its start).
pub const MAX_ANALYSIS: Tick = Tick(30 * 60 * TICKS_PER_SECOND);

#[derive(Clone, Debug, PartialEq)]
pub enum Method {
    In,
    Out,
    Timecode { ignore_hours: bool },
    Marker { name: Option<String> },
    Audio,
}

impl Method {
    pub fn from_params(p: &Value) -> std::result::Result<Method, String> {
        let m = p.get("method").and_then(Value::as_str).unwrap_or("in").to_ascii_lowercase();
        Ok(match m.as_str() {
            "in" | "inpoint" | "inpoints" | "start" | "clipstart" => Method::In,
            "out" | "outpoint" | "outpoints" | "end" | "clipend" => Method::Out,
            "timecode" | "tc" => Method::Timecode { ignore_hours: p.get("ignoreHours").and_then(Value::as_bool).unwrap_or(false) },
            "marker" | "clipmarker" | "markers" => Method::Marker { name: p.get("marker").and_then(Value::as_str).map(str::to_string) },
            "audio" | "sound" | "waveform" => Method::Audio,
            o => return Err(format!("unknown sync method `{o}` (in, out, timecode, marker, audio)")),
        })
    }
    pub fn name(&self) -> &'static str {
        match self {
            Method::In => "in",
            Method::Out => "out",
            Method::Timecode { .. } => "timecode",
            Method::Marker { .. } => "marker",
            Method::Audio => "audio",
        }
    }
}

/// What syncing needs to know about one clip.
#[derive(Clone, Debug)]
pub struct SyncClip {
    pub item: ItemId,
    pub name: String,
    /// Media range the clip uses.
    pub range: TimeRange,
    /// In / Out points (media time, exclusive Out).
    pub in_point: Tick,
    pub out_point: Tick,
    /// Start timecode in ticks (0 when the media has none).
    pub timecode: Tick,
    /// Clip markers: (media time, name).
    pub markers: Vec<(Tick, String)>,
}

/// The media item behind an item (subclips resolve to their parent) and its subclip range.
fn media_of(p: &Project, item: ItemId) -> Option<(ItemId, &filmcraft_project::MediaClip, Option<TimeRange>)> {
    p.resolve_media(item)
}

fn start_timecode(m: &filmcraft_project::MediaClip) -> Tick {
    let rate = m.frame_rate();
    m.info.start_timecode.map(|f| rate.tick_of(f)).unwrap_or(Tick::ZERO)
}

/// A project item as a sync clip (the whole media, or a subclip's range; In/Out marks).
pub fn project_clip(p: &Project, item: ItemId) -> Option<SyncClip> {
    let it = p.item(item)?;
    let (_, m, sub) = media_of(p, item)?;
    let range = sub.unwrap_or(TimeRange::new(Tick::ZERO, m.duration()));
    let rate = m.frame_rate();
    let in_point = m.mark_in.filter(|_| sub.is_none()).unwrap_or(range.start);
    let out_point = m.mark_out.filter(|_| sub.is_none()).map(|o| o + rate.frame_duration()).unwrap_or(range.end());
    Some(SyncClip {
        item,
        name: it.name.clone(),
        range,
        in_point,
        out_point,
        timecode: start_timecode(m),
        markers: m.markers.iter().map(|k| (k.start, k.name.clone())).collect(),
    })
}

/// A timeline clip as a sync clip (its source range; markers of the clip and of its media).
pub fn timeline_clip(p: &Project, ti: &TrackItem) -> SyncClip {
    let media = media_of(p, ti.item);
    let range = TimeRange::from_bounds(ti.source_in, ti.source_out());
    let mut markers: Vec<(Tick, String)> = ti.markers.iter().map(|k| (k.start, k.name.clone())).collect();
    if let Some((_, m, _)) = media {
        markers.extend(m.markers.iter().map(|k| (k.start, k.name.clone())));
    }
    SyncClip {
        item: ti.item,
        name: ti.name.clone(),
        range,
        in_point: range.start,
        out_point: range.end(),
        timecode: media.map(|(_, m, _)| start_timecode(m)).unwrap_or(Tick::ZERO),
        markers,
    }
}

/// Frames per analysis read: [`ANALYSIS_CHUNK_SECONDS`], shortened so the decoder's buffer for the
/// stream at its own sample rate and channel count (plus the frames it reads past the window)
/// stays under the audio buffer limit, e.g. for a multichannel high-rate stream.
fn chunk_frames(stream: Option<&filmcraft_media::AudioStreamInfo>) -> usize {
    let wanted = (ANALYSIS_CHUNK_SECONDS * ANALYSIS_RATE as u64) as usize;
    let Some(a) = stream else { return wanted };
    let per_frame = (a.channels.max(1) as usize).saturating_mul(std::mem::size_of::<f32>());
    // source frames one buffer can hold, less a margin for the decoder's extra frames
    let room = (filmcraft_media::MAX_AUDIO_BUFFER_BYTES / per_frame).saturating_sub(64);
    let fits = (room as u128 * ANALYSIS_RATE as u128 / a.sample_rate.max(1) as u128) as usize;
    wanted.min(fits).max(ANALYSIS_RATE as usize / 100)
}

/// Mono audio of a clip's media range at [`ANALYSIS_RATE`] (None: no audio).
pub fn clip_audio(s: &Session, c: &SyncClip) -> Option<Vec<f32>> {
    let src = s.source(c.item)?;
    if !src.info().has_audio() {
        return None;
    }
    let sr = ANALYSIS_RATE as i64;
    let start = c.range.start.to_units_floor(sr);
    let len = c.range.duration.min(MAX_ANALYSIS).to_units_floor(sr).max(0) as usize;
    // Read in short chunks and mix each down to mono: one read of the whole range is a planar
    // buffer of channels x length, which for stereo over ~23 minutes exceeds the audio buffer
    // limit and failed as "no audio" (#3554).
    let chunk = chunk_frames(src.info().audio());
    let mut mono: Vec<f32> = Vec::with_capacity(len);
    let mut done = 0usize;
    while done < len {
        let n = chunk.min(len - done);
        let buf = src.audio(start.checked_add(done as i64)?, n, ANALYSIS_RATE).ok()?;
        let chans: Vec<&[f32]> = buf.channels.iter().map(|c| c.as_slice()).collect();
        let part = mixdown(&chans);
        let short = part.len() < n;
        mono.extend(part);
        if short {
            // the media ended inside this chunk: keep later samples from shifting earlier
            break;
        }
        done += n;
    }
    Some(mono)
}

/// The outcome for one clip: its anchor, and for audio the correlation figures.
#[derive(Clone, Debug)]
pub struct Anchor {
    pub anchor: Tick,
    pub audio: Option<SyncResult>,
}

/// Compute the anchors of `clips` (index `reference` is the reference for audio and `offset`).
/// `offset` (ticks) moves every other clip later by that much.
pub fn anchors(s: &Session, clips: &[SyncClip], reference: usize, method: &Method, offset: Tick) -> Result<Vec<Anchor>> {
    if clips.is_empty() {
        return Err(EngineError::Other("nothing to synchronize".into()));
    }
    let hour = Tick(3600 * TICKS_PER_SECOND);
    let mut out: Vec<Anchor> = Vec::with_capacity(clips.len());
    let ref_audio = if *method == Method::Audio {
        Some(clip_audio(s, &clips[reference]).ok_or_else(|| EngineError::Other(format!("{} has no audio to synchronize by", clips[reference].name)))?)
    } else {
        None
    };
    for (i, c) in clips.iter().enumerate() {
        let a = match method {
            Method::In => Anchor { anchor: c.in_point, audio: None },
            Method::Out => Anchor { anchor: c.out_point, audio: None },
            Method::Timecode { ignore_hours } => {
                let tc = if *ignore_hours { Tick(c.timecode.0.rem_euclid(hour.0)) } else { c.timecode };
                Anchor { anchor: Tick::ZERO - tc, audio: None }
            }
            Method::Marker { name } => {
                let m =
                    c.markers.iter().filter(|(_, n)| name.as_deref().is_none_or(|w| n.eq_ignore_ascii_case(w))).map(|(t, _)| *t).min().ok_or_else(|| {
                        EngineError::Other(format!("{} has no clip marker{}", c.name, name.as_ref().map(|n| format!(" named “{n}”")).unwrap_or_default()))
                    })?;
                Anchor { anchor: m, audio: None }
            }
            Method::Audio => {
                if i == reference {
                    Anchor { anchor: c.range.start, audio: None }
                } else {
                    let a = ref_audio.as_deref().unwrap_or_default();
                    let b = clip_audio(s, c).ok_or_else(|| EngineError::Other(format!("{} has no audio to synchronize by", c.name)))?;
                    let r = find_offset(a, &b, ANALYSIS_RATE, &SyncOptions::default())
                        .ok_or_else(|| EngineError::Other(format!("{} and {} have no common audio", clips[reference].name, c.name)))?;
                    // b[n] (media r0_i + n) lines up with a[n + L] (media r0_ref + n + L); with the
                    // reference anchored at r0_ref, equal common times give anchor_i = r0_i − L
                    let lag = Tick::from_units(r.lag, ANALYSIS_RATE as i64);
                    Anchor { anchor: c.range.start - lag, audio: Some(r) }
                }
            }
        };
        out.push(a);
    }
    if offset != Tick::ZERO {
        for (i, a) in out.iter_mut().enumerate() {
            if i != reference {
                // later on the common timeline = smaller anchor
                a.anchor -= offset;
            }
        }
    }
    Ok(out)
}

/// Where each clip's range starts on a common timeline that begins at 0 (the earliest clip), at
/// the given frame rate (snapped to frames when `snap`).
pub fn placements(clips: &[SyncClip], anchors: &[Anchor], rate: FrameRate, snap: bool) -> Vec<Tick> {
    let raw: Vec<Tick> = clips.iter().zip(anchors).map(|(c, a)| c.range.start - a.anchor).collect();
    let min = raw.iter().copied().min().unwrap_or(Tick::ZERO);
    raw.into_iter().map(|t| if snap { rate.snap_nearest(t - min) } else { t - min }).collect()
}

/// JSON report of one sync for agents.
pub fn report(clips: &[SyncClip], anchors: &[Anchor], starts: &[Tick], rate: FrameRate) -> Value {
    Value::Array(
        clips
            .iter()
            .zip(anchors)
            .zip(starts)
            .map(|((c, a), st)| {
                json!({
                    "item": c.item.0,
                    "name": c.name,
                    "start": st.0,
                    "startFrame": rate.frame_at(*st),
                    "anchor": a.anchor.0,
                    "audio": a.audio.map(|r| json!({"lagSamples": r.lag, "lagFine": r.lag_fine, "confidence": r.confidence, "distinct": r.distinct, "reliable": r.reliable(), "inverted": r.inverted})),
                })
            })
            .collect(),
    )
}

#[cfg(test)]
mod chunk_tests {
    use super::*;
    use filmcraft_media::{AudioStreamInfo, MAX_AUDIO_BUFFER_BYTES};

    fn stream(sample_rate: u32, channels: u32) -> AudioStreamInfo {
        AudioStreamInfo { sample_rate, channels, codec: String::new(), bits_per_sample: None }
    }

    /// The decoder's buffer for one chunk never exceeds the audio buffer limit, from stereo to the
    /// widest, fastest stream the container readers accept (#3554).
    #[test]
    fn a_chunk_fits_the_audio_buffer_limit_for_any_stream() {
        let full = (ANALYSIS_CHUNK_SECONDS * ANALYSIS_RATE as u64) as usize;
        assert_eq!(chunk_frames(None), full);
        assert_eq!(chunk_frames(Some(&stream(48_000, 2))), full);
        for (rate, ch) in [(384_000u32, 6u32), (384_000, 64), (192_000, 32), (96_000, 8), (8_000, 1)] {
            let n = chunk_frames(Some(&stream(rate, ch)));
            let source_frames = (n as u128 * rate as u128).div_ceil(ANALYSIS_RATE as u128) as usize + 2;
            assert!(source_frames * ch as usize * 4 <= MAX_AUDIO_BUFFER_BYTES, "{rate} Hz x {ch}: {n} frames");
            assert!(n > 0);
        }
        assert!(chunk_frames(Some(&stream(384_000, 6))) < full, "a 60 s read of 6 x 384 kHz would not fit");
    }
}
