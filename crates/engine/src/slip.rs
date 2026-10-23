//! The Slip Monitor (Premiere's four-up slip view): while the Slip tool drags a clip, the Program
//! monitor shows the previous clip's last frame, the slipped clip's new first and last frames and
//! the next clip's first frame, with the new source timecodes and the slip offset.
//!
//! [`preview`] works the slip out on a copy of the sequence with the edit `timeline.slip` applies
//! ([`crate::commands::slip_linked`]: one delta for the clip and its linked partners, clamped by
//! the most constrained one's media, #529), so the drag shows exactly what the release commits.
//! Nothing in the project changes and no undo step is made. `timeline.slipPreview` returns it.

use serde_json::{Value, json};

use filmcraft_edit as edit;
use filmcraft_project::{ClipId, ItemId, Sequence, TrackKind};
use filmcraft_time::{Tick, TimeDisplay, format_time};

use crate::commands::{slip_linked, with_links};
use crate::trim::side_json;
use crate::{EngineError, Result, Session};

/// What slipping `clip` by `delta` (media ticks, as `timeline.slip` takes) would show.
///
/// `in` / `out` are the slipped clip's first and last frames after the slip, `previous` / `next`
/// the last frame of the clip that ends where it starts and the first frame of the clip that
/// starts where it ends, on its track (null at a gap or the end of the track). When the dragged
/// clip is sound with linked picture, the picture is what's shown (`shown`). Each side carries
/// `sourceTimecode`, the media timecode of its frame. `delta` is the slip actually applied (the
/// request clamped at the media's ends; `atLimit` when that held it back) and `offsetFrames` that
/// in sequence frames. `clips` lists each slipped clip's new source range.
pub fn preview(s: &Session, clip: ClipId, delta: Tick) -> Result<Value> {
    let seq = s.active_sequence().ok_or(EngineError::NoSequence)?;
    let (_, before) = seq.find_item(clip).ok_or_else(|| EngineError::Other(format!("no clip {}", clip.0)))?;
    let clips = with_links(s, &[clip]);
    let mut q = seq.clone();
    let durations = |id: ItemId| crate::media_duration(&s.project, &s.media, id);
    let starts = |id: ItemId| crate::media_start(&s.project, id);
    let mut next_id = s.project.next_id;
    let rate = seq.settings.frame_rate;
    let mut ctx = edit::EditCtx { next_id: &mut next_id, media_duration: &durations, media_start: &starts, min_duration: rate.frame_duration() };
    slip_linked(&mut q, &mut ctx, &clips, delta)?;

    let applied = q.find_item(clip).map(|(_, it)| it.source_in - before.source_in).unwrap_or_default();
    let on_video = |c: &ClipId| seq.find_item(*c).and_then(|(tid, _)| seq.track(tid)).is_some_and(|t| t.kind == TrackKind::Video);
    let shown = if on_video(&clip) { clip } else { clips.iter().copied().find(on_video).unwrap_or(clip) };
    let (prev, next) = neighbours(&q, shown);
    let side = |c: Option<ClipId>, outgoing: bool| c.and_then(|c| side_json(s, &q, c, outgoing)).map(|v| with_timecode(s, v)).unwrap_or(Value::Null);
    let slipped: Vec<Value> = clips
        .iter()
        .filter_map(|c| {
            let (_, was) = seq.find_item(*c)?;
            let (_, it) = q.find_item(*c)?;
            Some(json!({"clip": c.0, "sourceIn": it.source_in.0, "sourceOut": it.source_out().0, "delta": (it.source_in - was.source_in).0}))
        })
        .collect();
    Ok(json!({
        "clip": clip.0,
        "shown": shown.0,
        "video": on_video(&shown),
        "requested": delta.0,
        "delta": applied.0,
        "offsetFrames": rate.frame_at(rate.snap_nearest(applied)),
        "atLimit": applied != delta,
        "previous": side(prev, true),
        "in": side(Some(shown), false),
        "out": side(Some(shown), true),
        "next": side(next, false),
        "clips": slipped,
    }))
}

/// The clips that touch `clip` on its track: the one ending where it starts and the one starting
/// where it ends. A slip moves neither.
fn neighbours(seq: &Sequence, clip: ClipId) -> (Option<ClipId>, Option<ClipId>) {
    let Some((tid, it)) = seq.find_item(clip) else { return (None, None) };
    let Some(track) = seq.track(tid) else { return (None, None) };
    let prev = track.items.iter().find(|o| o.id != clip && o.end() == it.start).map(|o| o.id);
    let next = track.items.iter().find(|o| o.id != clip && o.start == it.end()).map(|o| o.id);
    (prev, next)
}

/// Add `sourceTimecode` to a side: its media time as the media's own timecode (its start
/// timecode plus the media time, at the media's rate), as the Timecode panel shows it.
fn with_timecode(s: &Session, mut side: Value) -> Value {
    let item = side["item"].as_u64().map(ItemId);
    let media_time = Tick(side["mediaTime"].as_i64().unwrap_or(0));
    if let Some(it) = item.and_then(|i| s.project.item(i)) {
        let rate = it.frame_rate();
        let start = it.as_media().and_then(|m| m.info.start_timecode).map(|f| rate.tick_of(f)).unwrap_or_default();
        side["sourceTimecode"] = json!(format_time(start + media_time, rate, false, TimeDisplay::Timecode, 48000));
    }
    side
}
