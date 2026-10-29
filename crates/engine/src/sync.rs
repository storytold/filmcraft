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

use filmcraft_audio_dsp::sync::{SyncOptions, SyncResult, find_offset};
use filmcraft_project::{ItemId, Project, TrackItem};
use filmcraft_time::{FrameRate, TICKS_PER_SECOND, Tick, TimeRange};
use serde_json::{Value, json};

use crate::{EngineError, Result, Session};

/// Sample rate audio is analysed at.
pub const ANALYSIS_RATE: u32 = 48_000;
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

/// Mono audio of a clip's media range at [`ANALYSIS_RATE`] (None: no audio).
pub fn clip_audio(s: &Session, c: &SyncClip) -> Option<Vec<f32>> {
    let src = s.source(c.item)?;
    if !src.info().has_audio() {
        return None;
    }
    let sr = ANALYSIS_RATE as i64;
    let start = c.range.start.to_units_floor(sr);
    let total = c.range.duration.min(MAX_ANALYSIS).to_units_floor(sr).max(0) as usize;
    // #3554: one `audio()` call for a long clip exceeds the decoder's buffer cap
    // (512 MB: ~22 min of stereo) and fails the whole sync with "no audio". Read in
    // bounded windows instead; peak allocation stays flat however long the clip is.
    const WINDOW: usize = ANALYSIS_RATE as usize * 10; // 10 s
    // Mix each window down before retaining it: holding every channel of a 30-min
    // stereo clip reserves ~692 MB plus ~346 MB more for the final mixdown.
    let mut mono: Vec<f32> = Vec::with_capacity(total);
    let mut nch = 0usize;
    let mut got = 0;
    while got < total {
        let n = (total - got).min(WINDOW);
        let buf = src.audio(start + got as i64, n, ANALYSIS_RATE).ok()?;
        if nch == 0 {
            nch = buf.channels.len();
        }
        if buf.channels.len() != nch || buf.channels.iter().any(|b| b.len() != n) {
            return None;
        }
        let k = buf.channels.len().max(1) as f32;
        for i in 0..n {
            mono.push(buf.channels.iter().map(|b| b[i]).sum::<f32>() / k);
        }
        got += n;
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
