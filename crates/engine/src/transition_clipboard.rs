//! Copy and paste a transition across edit points without moving clips.

use std::collections::HashSet;

use filmcraft_project::{ClipId, Sequence, Track, TrackId, TrackItem, TrackKind, Transition, TransitionId};
use filmcraft_time::Tick;
use serde_json::{Value, json};

use crate::commands::bad;
use crate::{EngineError, Result, Session};

const MAX_DESTINATIONS: usize = 256;
const MAX_FRAMES: i64 = 10_000_000;

/// Frame-based timing makes the copied transition independent of the destination frame rate.
#[derive(Clone, Debug)]
pub struct TransitionClipboard {
    transition: Transition,
    kind: TrackKind,
    duration_frames: i64,
    start_offset_frames: i64,
}

fn item_end(item: &TrackItem) -> Option<Tick> {
    item.start.0.checked_add(item.duration.0).map(Tick)
}

fn attached_cut(track: &Track, transition: &Transition) -> Option<Tick> {
    let cut = match (transition.from, transition.to) {
        (Some(from), Some(to)) => {
            let outgoing = track.item(from)?;
            let incoming = track.item(to)?;
            let cut = item_end(outgoing)?;
            (cut == incoming.start).then_some(cut)
        }
        (None, Some(to)) => track.item(to).map(|item| item.start),
        (Some(from), None) => track.item(from).and_then(item_end),
        (None, None) => None,
    }?;
    let end = transition.start.0.checked_add(transition.duration.0)?;
    (transition.duration.0 > 0 && transition.start <= cut && end >= cut.0).then_some(cut)
}

pub(crate) fn copy(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "sequence.copyTransition";
    let id = p.get("transition").and_then(Value::as_u64).ok_or_else(|| bad(CMD, "need `transition`"))?;
    let q = s.active_sequence().ok_or(EngineError::NoSequence)?;
    let (track, transition) = q
        .all_tracks()
        .find_map(|track| track.transitions.iter().find(|tr| tr.id.0 == id).map(|tr| (track, tr)))
        .ok_or_else(|| bad(CMD, "transition not found"))?;
    let cut = attached_cut(track, transition).ok_or_else(|| bad(CMD, "transition is detached from its cut"))?;
    let rate = q.settings.frame_rate.sane();
    if rate.frame_duration().0 <= 0 {
        return Err(bad(CMD, "sequence frame rate is out of range"));
    }
    let duration_frames = rate.frame_at(transition.duration);
    let offset = transition.start.0.checked_sub(cut.0).ok_or_else(|| bad(CMD, "transition offset is out of range"))?;
    let start_offset_frames = rate.frame_at(Tick(offset));
    if !(1..=MAX_FRAMES).contains(&duration_frames) || !(-MAX_FRAMES..=MAX_FRAMES).contains(&start_offset_frames) {
        return Err(bad(CMD, "transition duration or offset is out of range"));
    }
    s.state.clipboard_transition = Some(TransitionClipboard { transition: transition.clone(), kind: track.kind, duration_frames, start_offset_frames });
    s.state.clipboard.clear();
    s.state.clipboard_markers.clear();
    Ok(json!({"copied": id}))
}

/// Resolve either side of an edit to the same pair of clips and cut tick.
fn destination(q: &Sequence, clip: ClipId, out: bool) -> Result<(TrackId, TrackKind, Option<ClipId>, Option<ClipId>, Tick)> {
    const CMD: &str = "sequence.pasteTransition";
    let (track_id, item) = q.find_item(clip).ok_or_else(|| bad(CMD, "destination clip no longer exists"))?;
    let track = q.track(track_id).ok_or(EngineError::NoSequence)?;
    let cut = if out { item_end(item).ok_or_else(|| bad(CMD, "destination clip end is out of range"))? } else { item.start };
    let other = if out { track.items.iter().find(|it| it.start == cut) } else { track.items.iter().find(|it| item_end(it) == Some(cut)) };
    let (from, to) = if out { (Some(clip), other.map(|it| it.id)) } else { (other.map(|it| it.id), Some(clip)) };
    Ok((track_id, track.kind, from, to, cut))
}

fn selected_transition_cut(q: &Sequence, id: TransitionId) -> Result<(ClipId, bool)> {
    const CMD: &str = "sequence.pasteTransition";
    let (track, transition) = q
        .all_tracks()
        .find_map(|track| track.transitions.iter().find(|tr| tr.id == id).map(|transition| (track, transition)))
        .ok_or_else(|| bad(CMD, format!("transition {} no longer exists", id.0)))?;
    attached_cut(track, transition).ok_or_else(|| bad(CMD, "selected transition is detached from its cut"))?;
    match (transition.from, transition.to) {
        (_, Some(to)) => Ok((to, false)),
        (Some(from), None) => Ok((from, true)),
        _ => Err(bad(CMD, "selected transition has no cut")),
    }
}

/// Calculate the sample with wide intermediate values: project times can be malformed, and
/// `TrackItem::source_time_at` uses `Tick` arithmetic that can overflow for such inputs.
fn checked_source_sample(item: &TrackItem, at: Tick) -> Result<i128> {
    const CMD: &str = "sequence.pasteTransition";
    if let Some(held) = item.frame_hold {
        return Ok(i128::from(held.0));
    }
    let rel = i128::from(at.0) - i128::from(item.start.0);
    let scaled = (rel as f64 * item.speed.abs()).round();
    let duration = (item.duration.0 as f64 * item.speed.abs()).round();
    let limit = i64::MAX as f64;
    if !scaled.is_finite() || scaled.abs() >= limit || !duration.is_finite() || duration.abs() >= limit {
        return Err(bad(CMD, format!("clip {} source time is out of range", item.id.0)));
    }
    let scaled = scaled as i128;
    let source = if item.reverse { i128::from(item.source_in.0) + duration as i128 - scaled - 1 } else { i128::from(item.source_in.0) + scaled };
    Ok(source)
}

/// A transition samples both clips throughout its span, including source frames outside their
/// visible timeline ranges. Check the first and last sampled ticks before changing the sequence.
fn validate_source_handles(s: &Session, track: &Track, from: Option<ClipId>, to: Option<ClipId>, start: Tick, end: Tick) -> Result<()> {
    const CMD: &str = "sequence.pasteTransition";
    let last = Tick(end.0.checked_sub(1).ok_or_else(|| bad(CMD, "transition time is out of range"))?);
    for id in [from, to].into_iter().flatten() {
        let item = track.item(id).ok_or_else(|| bad(CMD, "destination clip no longer exists"))?;
        if !item.speed.is_finite() || item.speed == 0.0 {
            return Err(bad(CMD, format!("clip {} has an invalid playback speed", id.0)));
        }
        if s.project.item(item.item).is_none() {
            return Err(bad(CMD, format!("clip {} has no source item", id.0)));
        }
        let Some(limit) = crate::media_duration(&s.project, &s.media, item.item) else { continue };
        let first_source = checked_source_sample(item, start)?;
        let last_source = checked_source_sample(item, last)?;
        let media_start = crate::media_start(&s.project, item.item);
        if limit <= media_start || first_source.min(last_source) < i128::from(media_start.0) || first_source.max(last_source) >= i128::from(limit.0) {
            return Err(bad(CMD, format!("clip {} lacks source media handles; shorten the transition or move its In/Out point; no changes made", id.0)));
        }
    }
    Ok(())
}

pub(crate) fn paste(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "sequence.pasteTransition";
    let copied = s.state.clipboard_transition.clone().ok_or_else(|| bad(CMD, "copy a transition first"))?;
    let q = s.active_sequence().ok_or(EngineError::NoSequence)?;
    let rate = q.settings.frame_rate.sane();
    if rate.frame_duration().0 <= 0 {
        return Err(bad(CMD, "sequence frame rate is out of range"));
    }
    let mut requested = Vec::new();
    if let Some(cuts) = p.get("cuts") {
        let cuts = cuts.as_array().ok_or_else(|| bad(CMD, "`cuts` must be an array"))?;
        if cuts.len() > MAX_DESTINATIONS {
            return Err(bad(CMD, "too many destination cuts"));
        }
        for cut in cuts {
            let id = cut.get("clip").and_then(Value::as_u64).ok_or_else(|| bad(CMD, "cut needs a clip id"))?;
            let out = match cut.get("edge").and_then(Value::as_str) {
                Some("out") => true,
                Some("in") => false,
                _ => return Err(bad(CMD, "edge must be `in` or `out`")),
            };
            requested.push((ClipId(id), out));
        }
    } else if let Some(id) = p.get("transition") {
        let id = id.as_u64().ok_or_else(|| bad(CMD, "`transition` must be an id"))?;
        requested.push(selected_transition_cut(q, TransitionId(id))?);
    } else if !s.state.transition_selection.is_empty() {
        if s.state.transition_selection.len() > MAX_DESTINATIONS {
            return Err(bad(CMD, "too many selected transitions"));
        }
        for id in &s.state.transition_selection {
            requested.push(selected_transition_cut(q, *id)?);
        }
    } else if !s.state.edit_points.is_empty() {
        if s.state.edit_points.len() > MAX_DESTINATIONS {
            return Err(bad(CMD, "too many selected edit points"));
        }
        requested.extend(s.state.edit_points.iter().map(|point| (point.clip, point.out)));
    } else {
        // Keyboard paste without a selected cut uses only exact playhead edits on targeted tracks.
        let playhead = s.playhead();
        let targeted = s.targeting().targeted;
        for track in q.all_tracks().filter(|track| track.kind == copied.kind && targeted.contains(&track.id)) {
            for item in &track.items {
                if item.start == playhead {
                    requested.push((item.id, false));
                    if requested.len() > MAX_DESTINATIONS {
                        return Err(bad(CMD, "too many destination cuts"));
                    }
                }
            }
        }
    }
    if requested.is_empty() {
        return Err(bad(CMD, "select an edit point or position the playhead exactly at a cut on a targeted track"));
    }

    let duration = rate.tick_of(copied.duration_frames);
    let offset = rate.tick_of(copied.start_offset_frames);
    if duration < rate.frame_duration() || rate.frame_at(duration) != copied.duration_frames {
        return Err(bad(CMD, "copied transition duration does not fit this sequence"));
    }
    let mut unique = HashSet::new();
    let mut placement = Vec::new();
    for (id, out) in requested {
        let (track_id, kind, from, to, cut) = destination(q, id, out)?;
        if kind != copied.kind {
            return Err(bad(CMD, "cannot paste a video transition onto an audio cut or vice versa"));
        }
        let track = q.track(track_id).ok_or(EngineError::NoSequence)?;
        if track.locked {
            return Err(bad(CMD, "destination track is locked"));
        }
        if !unique.insert((track_id, from, to)) {
            continue;
        }
        let start = cut.0.checked_add(offset.0).ok_or_else(|| bad(CMD, "transition time is out of range"))?;
        let end = start.checked_add(duration.0).ok_or_else(|| bad(CMD, "transition time is out of range"))?;
        if start < 0 || start > cut.0 || end < cut.0 {
            return Err(bad(CMD, "copied transition cannot fit at this edit point"));
        }
        if from.is_none() && start != cut.0 || to.is_none() && end != cut.0 {
            return Err(bad(CMD, "a one-sided transition must stay on its clip edge"));
        }
        if from.is_some_and(|from| track.item(from).is_none_or(|item| start < item.start.0))
            || to.is_some_and(|to| track.item(to).and_then(item_end).is_none_or(|item_end| end > item_end.0))
        {
            return Err(bad(CMD, "not enough clip duration at a destination cut; no changes made"));
        }
        validate_source_handles(s, track, from, to, Tick(start), Tick(end))?;
        let mut transition = copied.transition.clone();
        transition.id = TransitionId(0);
        transition.from = from;
        transition.to = to;
        transition.start = Tick(start);
        transition.duration = duration;
        placement.push((track_id, transition));
    }
    if placement.is_empty() {
        return Err(bad(CMD, "no destination cuts"));
    }

    // Validation finishes before the one undoable edit. Preserve overlapping transitions
    // attached to other cuts; replace only the one on this exact pair.
    let ids = s.edit_sequence("Paste Transition", |sequence, context, _| {
        let mut ids = Vec::with_capacity(placement.len());
        for (track_id, mut transition) in placement {
            let track = sequence.track_mut(track_id).ok_or(EngineError::NoSequence)?;
            track.transitions.retain(|existing| existing.from != transition.from || existing.to != transition.to);
            transition.id = TransitionId(context.alloc());
            ids.push(transition.id.0);
            track.transitions.push(transition);
            track.sort();
        }
        Ok(ids)
    })?;
    Ok(json!({"pasted": ids.len(), "transitions": ids}))
}

#[cfg(test)]
mod tests {
    use super::checked_source_sample;
    use filmcraft_time::Tick;
    use serde_json::json;

    use crate::Session;

    fn demo() -> Session {
        let mut session = Session::default();
        session.execute("file.openDemoProject", json!({})).unwrap();
        session
    }

    #[test]
    fn copies_to_two_cuts_replaces_only_their_transitions_and_undoes_as_one_edit() {
        let mut session = demo();
        let clips = session.active_sequence().unwrap().video_tracks[0].items.clone();
        let rate = session.sequence_rate();
        let source = session
            .execute("sequence.applyVideoTransition", json!({"clip": clips[2].id.0, "edge": "in", "effect": "wipe", "frames": 12}))
            .unwrap()["transition"]
            .as_u64()
            .unwrap();
        let source_start = (clips[2].start - rate.tick_of(4)).0;
        session.execute("sequence.setTransition", json!({"transition": source, "start": source_start, "reverse": true})).unwrap();
        let before = session.active_sequence().unwrap().clone();
        let copied = before.video_tracks[0].transitions.iter().find(|tr| tr.id.0 == source).unwrap().clone();
        session.execute("timeline.select", json!({"transitions": [source]})).unwrap();
        session.execute("edit.copy", json!({})).unwrap();
        assert!(session.state.clipboard_transition.is_some());
        let result = session
            .execute("sequence.pasteTransition", json!({"cuts": [{"clip": clips[3].id.0, "edge": "in"}, {"clip": clips[4].id.0, "edge": "in"}]}))
            .unwrap();
        assert_eq!(result["pasted"], 2);
        let after = session.active_sequence().unwrap().clone();
        assert_eq!(after.video_tracks[0].items, before.video_tracks[0].items);
        let pasted_ids: Vec<u64> = result["transitions"].as_array().unwrap().iter().map(|id| id.as_u64().unwrap()).collect();
        for (clip, id) in [clips[3].clone(), clips[4].clone()].iter().zip(pasted_ids) {
            let transition = after.video_tracks[0].transitions.iter().find(|tr| tr.id.0 == id).unwrap();
            assert_eq!(transition.to, Some(clip.id));
            assert_eq!(transition.start, clip.start - rate.tick_of(4));
            assert_eq!(transition.effect, copied.effect);
            assert!(transition.reverse);
        }
        for original in before.video_tracks[0].transitions.iter().filter(|tr| tr.to != Some(clips[3].id) && tr.to != Some(clips[4].id)) {
            assert!(after.video_tracks[0].transitions.contains(original));
        }
        session.execute("edit.undo", json!({})).unwrap();
        assert_eq!(session.active_sequence().unwrap().video_tracks[0].transitions, before.video_tracks[0].transitions);
        session.execute("edit.redo", json!({})).unwrap();
        assert_eq!(session.active_sequence().unwrap().video_tracks[0].transitions, after.video_tracks[0].transitions);
    }

    #[test]
    fn hostile_destinations_fail_without_changing_the_sequence() {
        let mut session = demo();
        let clips = session.active_sequence().unwrap().video_tracks[0].items.clone();
        let source = session.execute("sequence.applyVideoTransition", json!({"clip": clips[2].id.0, "edge": "in", "frames": 12})).unwrap()["transition"]
            .as_u64()
            .unwrap();
        session.execute("sequence.copyTransition", json!({"transition": source})).unwrap();
        let before = session.active_sequence().unwrap().clone();
        let too_many: Vec<_> = (0..257).map(|_| json!({"clip": clips[3].id.0, "edge": "in"})).collect();
        for params in [
            json!({"cuts": "bad"}),
            json!({"cuts": too_many}),
            json!({"cuts": [{"clip": clips[3].id.0, "edge": "side"}]}),
            json!({"cuts": [{"clip": u64::MAX, "edge": "in"}]}),
            json!({"cuts": [{"clip": clips[3].id.0, "edge": "in"}, {"clip": clips[0].id.0, "edge": "in"}]}),
        ] {
            assert!(session.execute("sequence.pasteTransition", params).is_err());
            assert_eq!(session.active_sequence().unwrap().video_tracks[0].transitions, before.video_tracks[0].transitions);
        }
        session.execute("timeline.setTrack", json!({"track": "V1", "locked": true})).unwrap();
        assert!(session.execute("sequence.pasteTransition", json!({"cuts": [{"clip": clips[3].id.0, "edge": "in"}]})).is_err());
        assert_eq!(session.active_sequence().unwrap().video_tracks[0].transitions, before.video_tracks[0].transitions);
    }

    #[test]
    fn finite_source_without_a_head_handle_rejects_bulk_paste_atomically() {
        let mut session = demo();
        let clips = session.active_sequence().unwrap().video_tracks[0].items.clone();
        let rate = session.sequence_rate();
        let source = session.execute("sequence.applyVideoTransition", json!({"clip": clips[2].id.0, "edge": "in", "frames": 12})).unwrap()["transition"]
            .as_u64()
            .unwrap();
        session.execute("sequence.copyTransition", json!({"transition": source})).unwrap();

        // Give the last destination a finite source with enough frames for its visible clip but
        // none before its In point. The earlier destination must not be changed on failure.
        let media = std::sync::Arc::make_mut(&mut session.project).item_mut(clips[4].item).unwrap().as_media_mut().unwrap();
        media.info.kind = filmcraft_media::MediaKind::Movie;
        media.info.duration = clips[4].duration + rate.tick_of(12);
        let sequence = session.state.active_sequence.unwrap();
        std::sync::Arc::make_mut(&mut session.project).sequence_mut(sequence).unwrap().find_item_mut(clips[4].id).unwrap().1.source_in = Tick::ZERO;
        let before = session.active_sequence().unwrap().video_tracks[0].transitions.clone();
        let cuts = json!({"cuts": [{"clip": clips[3].id.0, "edge": "in"}, {"clip": clips[4].id.0, "edge": "in"}]});
        let error = session.execute("sequence.pasteTransition", cuts.clone()).unwrap_err().to_string();
        assert!(error.contains("source media handles"), "{error}");
        assert_eq!(session.active_sequence().unwrap().video_tracks[0].transitions, before);

        // Six source frames before the In point make the same paste valid.
        std::sync::Arc::make_mut(&mut session.project).sequence_mut(sequence).unwrap().find_item_mut(clips[4].id).unwrap().1.source_in = rate.tick_of(6);
        let pasted = session.execute("sequence.pasteTransition", cuts).unwrap();
        assert_eq!(pasted["pasted"], 2);

        // The outgoing clip also needs frames after its visible Out point. A finite source that
        // ends there cannot supply them, even when the incoming clip has enough head handle.
        let outgoing = std::sync::Arc::make_mut(&mut session.project).item_mut(clips[3].item).unwrap().as_media_mut().unwrap();
        outgoing.info.kind = filmcraft_media::MediaKind::Movie;
        outgoing.info.duration = clips[3].source_out();
        let before = session.active_sequence().unwrap().video_tracks[0].transitions.clone();
        let error = session.execute("sequence.pasteTransition", json!({"cuts": [{"clip": clips[4].id.0, "edge": "in"}]})).unwrap_err().to_string();
        assert!(error.contains(&format!("clip {} lacks source media handles", clips[3].id.0)), "{error}");
        assert_eq!(session.active_sequence().unwrap().video_tracks[0].transitions, before);
    }

    #[test]
    fn malformed_source_times_return_an_error_without_panicking() {
        let session = demo();
        let mut item = session.active_sequence().unwrap().video_tracks[0].items[0].clone();
        item.start = Tick(i64::MIN);
        item.speed = f64::MAX;
        let result = std::panic::catch_unwind(|| checked_source_sample(&item, Tick(i64::MAX)));
        assert!(result.is_ok());
        assert!(result.unwrap().is_err());
    }
}
