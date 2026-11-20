//! Audio-based silence removal, independent of transcription.
//! Detection is read-only; applying cuts is a single undoable timeline edit.

use crate::commands::{CommandSpec, bad, has_seq};
use crate::{EngineError, Result, Session};
use filmcraft_audio_dsp::silence::detect_silence;
use filmcraft_time::{Tick, TimeRange};
use serde_json::{Value, json};

fn setting(p: &Value, name: &str, default: f64) -> Result<f64> {
    let value = match p.get(name) {
        None => default,
        Some(v) => v.as_f64().ok_or_else(|| bad("audio.removeSilence", format!("{name} must be a number")))?,
    };
    if !value.is_finite() {
        return Err(bad("audio.removeSilence", format!("{name} must be finite")));
    }
    Ok(value)
}

fn run(s: &mut Session, p: &Value) -> Result<Value> {
    let threshold = setting(p, "thresholdDb", -40.0)?;
    let minimum = setting(p, "minSeconds", 0.7)?;
    let padding = setting(p, "paddingSeconds", 0.12)?;
    if !(-120.0..=0.0).contains(&threshold) || !(0.0..=60.0).contains(&minimum) || !(0.0..=10.0).contains(&padding) {
        return Err(bad("audio.removeSilence", "thresholdDb must be -120..0, minSeconds 0..60, paddingSeconds 0..10"));
    }
    let preview = p.get("preview").and_then(Value::as_bool).unwrap_or(true);
    let rate = s.sequence_rate();
    let q = s.active_sequence().ok_or_else(|| EngineError::Other("no active sequence".into()))?;
    if q.all_tracks().any(|t| t.locked) {
        return Err(EngineError::Other("unlock all timeline tracks before ripple-deleting silence".into()));
    }
    if q.audio_tracks.iter().filter(|t| t.items.iter().any(|c| c.enabled)).count() > 1 {
        return Err(EngineError::Other("silence detection currently supports one active audio track; mute or remove other audio tracks before editing".into()));
    }
    if q.audio_tracks.iter().flat_map(|t| t.items.iter()).any(|c| c.enabled && (c.speed != 1.0 || c.reverse)) {
        return Err(EngineError::Other("speed-changed or reversed audio clips are not supported for silence detection".into()));
    }
    let clips: Vec<_> = q.audio_tracks.iter().flat_map(|t| t.items.iter()).filter(|c| c.enabled && c.speed == 1.0 && !c.reverse).cloned().collect();
    if clips.is_empty() {
        return Err(EngineError::Other("no eligible audio clips in the sequence".into()));
    }
    let sr = 16_000_u32;
    let mut ranges = Vec::new();
    for clip in &clips {
        let (media, _, _) = s.project.resolve_media(clip.item).ok_or_else(|| EngineError::Other("audio clip source cannot be resolved".into()))?;
        let source = s.source(media).ok_or_else(|| EngineError::Other("audio source cannot be opened".into()))?;
        let frames = clip.duration.to_units_floor(i64::from(sr)).max(0) as usize;
        let offset = clip.source_in.to_units_floor(i64::from(sr));
        // Chunk reads to keep decoding memory bounded for long recordings.
        // Each chunk overlaps the next; collect silence runs across boundaries.
        let mut quiet_start: Option<usize> = None;
        let mut frame = 0_usize;
        let limit = (minimum * f64::from(sr)).ceil() as usize;
        let pad = (padding * f64::from(sr)).ceil() as usize;
        while frame < frames {
            let count = (frames - frame).min(sr as usize * 10);
            let start = offset.saturating_add(i64::try_from(frame).unwrap_or(i64::MAX));
            let audio = source.audio_stream(clip.audio_stream, start, count, sr).map_err(|e| EngineError::Other(format!("cannot decode audio: {e}")))?;
            if audio.channels.is_empty() || audio.channels.iter().any(|ch| ch.len() < count) {
                return Err(EngineError::Other("decoded audio has missing samples".into()));
            }
            // Preserve all channels when evaluating whether a frame is quiet.
            let channel_count = audio.channels.len();
            let mut interleaved = Vec::with_capacity(count * channel_count);
            for i in 0..count {
                for channel in &audio.channels {
                    interleaved.push(channel[i]);
                }
            }
            // Compute each chunk's active/silent transitions with the same threshold.
            let segments = detect_silence(&interleaved, channel_count, sr, threshold as f32, 0.0, 0.0);
            let mut cursor = 0;
            for segment in segments {
                let begin = frame + segment.start;
                let end = frame + segment.end;
                if segment.start > cursor
                    && let Some(a) = quiet_start.take()
                {
                    push_cut(&mut ranges, clip.start, a, frame + cursor, limit, pad, sr, rate);
                }
                if quiet_start.is_none() {
                    quiet_start = Some(begin);
                }
                cursor = segment.end;
                if cursor < count
                    && let Some(a) = quiet_start.take()
                {
                    push_cut(&mut ranges, clip.start, a, end, limit, pad, sr, rate);
                }
            }
            if cursor < count
                && let Some(a) = quiet_start.take()
            {
                push_cut(&mut ranges, clip.start, a, frame + cursor, limit, pad, sr, rate);
            }
            frame += count;
        }
        if let Some(a) = quiet_start {
            push_cut(&mut ranges, clip.start, a, frames, limit, pad, sr, rate);
        }
    }
    ranges.sort_by_key(|r| r.start);
    // Multiple audio tracks may describe the same silent interval. Merge overlaps.
    let mut merged: Vec<TimeRange> = Vec::new();
    for r in ranges {
        if let Some(last) = merged.last_mut()
            && r.start <= last.end()
        {
            *last = TimeRange::from_bounds(last.start, last.end().max(r.end()));
        } else {
            merged.push(r);
        }
    }
    let count = merged.len();
    let seconds: f64 = merged.iter().map(|r| r.duration.seconds()).sum();
    if preview {
        return Ok(json!({"preview": true, "removed": count, "seconds": seconds,
            "ranges": merged.iter().map(|r| json!({"start": r.start.0, "duration": r.duration.0})).collect::<Vec<_>>()}));
    }
    if count == 0 {
        return Err(EngineError::Other(
            "no silence ranges matched these settings; try a higher threshold (for example -30 dBFS) or a shorter minimum pause".into(),
        ));
    }
    let ticks = s.edit_sequence("Remove Silence", |q, ctx, _| Ok(filmcraft_edit::transcript::ripple_delete_ranges(q, merged, ctx)))?;
    Ok(json!({"preview": false, "removed": count, "seconds": ticks.seconds()}))
}

#[allow(clippy::too_many_arguments)]
fn push_cut(ranges: &mut Vec<TimeRange>, clip_start: Tick, begin: usize, end: usize, minimum: usize, padding: usize, sr: u32, rate: filmcraft_time::FrameRate) {
    if end.saturating_sub(begin) < minimum {
        return;
    }
    let a = begin.saturating_add(padding);
    let b = end.saturating_sub(padding);
    if a >= b {
        return;
    }
    let mut start = rate.snap(clip_start + Tick::from_units(a as i64, i64::from(sr)));
    let finish = rate.snap(clip_start + Tick::from_units(b as i64, i64::from(sr)));
    let exact = clip_start + Tick::from_units(a as i64, i64::from(sr));
    if start < exact {
        start += rate.frame_duration();
    }
    if finish > start {
        ranges.push(TimeRange::from_bounds(start, finish));
    }
}

pub fn commands() -> Vec<CommandSpec> {
    vec![CommandSpec {
        id: "audio.removeSilence",
        label: "Remove Silence…",
        menu: &["Sequence", "Audio"],
        shortcut: None,
        params: r#"{"thresholdDb":-40?,"minSeconds":0.7?,"paddingSeconds":0.12?,"preview":true?}"#,
        enabled: has_seq,
        run,
        journal: true,
    }]
}
