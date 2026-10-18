//! Editable sequence markers from the existing clean-room Remix beat detector (#1816).
//! Analysis is bounded to ten source minutes, at 12 kHz, and never changes source media.
use filmcraft_audio_dsp::remix;
use filmcraft_project::{ClipId, ItemKind, Label, Marker, MarkerId, MarkerKind, TrackKind};
use filmcraft_time::Tick;
use serde_json::{Value, json};

use crate::commands::{CommandSpec, bad};
use crate::{EngineError, Result, Session};

const ID: &str = "markers.detectBeats";
const RATE: u32 = 12_000;
const MAX_SAMPLES: usize = RATE as usize * 600;

fn selected(s: &Session) -> Option<ClipId> {
    let q = s.active_sequence()?;
    s.state.selection.iter().copied().find(|id| q.find_item(*id).and_then(|(t, _)| q.track(t)).is_some_and(|t| t.kind == TrackKind::Audio))
}

pub fn commands() -> Vec<CommandSpec> {
    vec![CommandSpec {
        id: ID,
        label: "Detect Audio Beats",
        menu: &["Markers"],
        shortcut: None,
        params: r#"{"clip":id?} — selected audio clip by default; adds editable sequence markers, one undo step"#,
        enabled: |s| selected(s).map(|_| ()).ok_or_else(|| "select an audio clip".into()),
        run,
        journal: true,
    }]
}

fn run(s: &mut Session, p: &Value) -> Result<Value> {
    let fields = p.as_object().ok_or_else(|| bad(ID, "parameters must be an object"))?;
    if fields.keys().any(|k| k != "clip") {
        return Err(bad(ID, "only `clip` is supported"));
    }
    let clip = match fields.get("clip") {
        Some(v) => ClipId(v.as_u64().filter(|v| *v != 0).ok_or_else(|| bad(ID, "clip must be a positive id"))?),
        None => selected(s).ok_or_else(|| bad(ID, "select an audio clip"))?,
    };
    let q = s.active_sequence().ok_or(EngineError::NoSequence)?;
    let (track, it) = q.find_item(clip).ok_or_else(|| bad(ID, "no such clip"))?;
    if q.track(track).is_none_or(|t| t.kind != TrackKind::Audio) {
        return Err(bad(ID, "select an audio clip"));
    }
    let item = s.project.item(it.item).ok_or_else(|| bad(ID, "the clip has no media"))?;
    if !matches!(&item.kind, ItemKind::Media(m) if m.info.has_audio() && !m.offline) {
        return Err(bad(ID, "the clip needs online audio media"));
    }
    if filmcraft_render::remix::is_remixed(it)
        || it.frame_hold.is_some()
        || it.effect("time_remap").is_some_and(|e| e.enabled && e.params.values().any(|p| !p.keyframes.is_empty()))
    {
        return Err(bad(ID, "render or revert Remix, frame hold or animated time remapping before detecting beats"));
    }
    // Convert the stored constant speed once; all subsequent edit mapping uses exact Tick ratios.
    if !it.speed.is_finite()
        || it.speed.abs() < 0.000_001
        || it.speed.abs() > 1000.0
        || it.duration <= Tick::ZERO
        || it.source_in < Tick::ZERO
        || it.start < Tick::ZERO
    {
        return Err(bad(ID, "the clip has an invalid speed or time range"));
    }
    let speed = (it.speed.abs() * 1_000_000.0).round() as i64;
    let source_duration = it.duration.mul_ratio(speed, 1_000_000);
    let count = source_duration.to_units_floor(i64::from(RATE));
    let count = usize::try_from(count)
        .ok()
        .filter(|n| *n > 0 && *n <= MAX_SAMPLES)
        .ok_or_else(|| bad(ID, "select an audio range between one sample and ten source minutes"))?;
    let channels = match &item.kind {
        ItemKind::Media(media) => media.info.audio_streams.get(it.audio_stream),
        _ => None,
    }
    .ok_or_else(|| bad(ID, "the clip's selected audio stream is unavailable"))?
    .channels;
    // Bound the decoded planar buffer as well as duration: hostile channel counts and
    // long multichannel ranges must fail before asking a decoder to allocate them.
    if channels == 0 || count.checked_mul(channels as usize).is_none_or(|samples| samples > 16_000_000) {
        return Err(bad(ID, "audio analysis exceeds the memory budget; select a shorter audio range"));
    }
    let source = s.media.full_res_source(&s.project, it.item, &*s.services).ok_or_else(|| bad(ID, "audio media is unavailable"))?;
    let audio = source
        .audio_stream(it.audio_stream, it.source_in.to_units_floor(i64::from(RATE)), count, RATE)
        .map_err(|e| bad(ID, format!("cannot read audio: {e}")))?;
    let channels: Vec<&[f32]> = audio.channels.iter().map(Vec::as_slice).collect();
    let analysis = remix::analyze(&channels, RATE).map_err(|e| bad(ID, format!("no reliable beat grid: {e}; select a longer rhythmic music clip")))?;
    let mut times: Vec<Tick> = analysis
        .beats
        .iter()
        .filter_map(|sample| {
            // Remix cut points precede attacks by half a crossfade; markers belong on the attacks.
            let source_time = Tick::from_units(sample.saturating_add(remix::beat_lead(RATE)), i64::from(RATE));
            let relative = source_time.mul_ratio(1_000_000, speed);
            let relative = if it.reverse { it.duration - relative - Tick(1) } else { relative };
            (relative >= Tick::ZERO && relative < it.duration).then_some(it.start + relative)
        })
        .collect();
    times.sort();
    times.dedup();
    // Re-running detection preserves edited names and avoids stacking duplicate markers.
    times.retain(|time| !q.markers.iter().any(|m| m.start == *time));
    let count = times.len();
    let markers = if count == 0 {
        Vec::new()
    } else {
        s.edit_sequence("Detect Audio Beats", |q, ids, _| {
            let mut added = Vec::with_capacity(count);
            for (i, start) in times.into_iter().enumerate() {
                let id = MarkerId(ids.alloc());
                q.markers.push(Marker {
                    id,
                    start,
                    duration: Tick::ZERO,
                    name: format!("Beat {}", i + 1),
                    comment: String::new(),
                    kind: MarkerKind::Comment,
                    color: Label::Green,
                });
                added.push(id.0);
            }
            q.markers.sort_by_key(|m| m.start);
            Ok(added)
        })?
    };
    Ok(json!({"clip": clip.0, "markers": markers, "count": count, "sourceBpm": analysis.bpm, "bpm": analysis.bpm * speed as f64 / 1_000_000.0}))
}
