//! Media handles: the media a clip has before its In point and after its Out point.
//!
//! A trim, roll or extend stops where a clip's media ends, and a transition plays the clips it
//! joins past the cut (with no media there, the picture repeats a frame and the sound is
//! silent). The results of those commands list each clip edge that needed more media than it has
//! as `{clip, edge, shortByFrames}`, so a script or agent can tell an edit the media stopped from
//! one that happened, and which frames of a transition repeat.

use filmcraft_edit::Edge;
use filmcraft_project::{ClipId, TransitionId};
use filmcraft_time::Tick;
use serde_json::{Value, json};

use crate::Session;

/// Media `it` has beyond `edge`, in sequence ticks at its speed, within the limits trims stop
/// at. `None` when nothing limits it (a still, a graphic, an adjustment layer or a held frame).
fn handle(s: &Session, it: &filmcraft_project::TrackItem, edge: Edge) -> Option<Tick> {
    if it.frame_hold.is_some() {
        return None;
    }
    let end = crate::media_duration(&s.project, &s.media, it.item)?;
    let before = it.source_in - crate::media_start(&s.project, it.item);
    let after = end - it.source_out();
    // a reversed clip plays its media backwards: its In edge shows the media after its source Out
    let media = match (edge, it.reverse) {
        (Edge::In, false) | (Edge::Out, true) => before,
        (Edge::In, true) | (Edge::Out, false) => after,
    };
    Some(Tick((media.0.max(0) as f64 / it.speed.abs().max(1e-9)).floor() as i64))
}

/// How much media `clip` lacks to show `need` (sequence ticks) beyond `edge`, as `{clip, edge,
/// shortByFrames}` in whole sequence frames (part of a frame counts). `None` when it has enough.
pub(crate) fn short(s: &Session, clip: ClipId, edge: Edge, need: Tick) -> Option<Value> {
    if need <= Tick::ZERO {
        return None;
    }
    let q = s.active_sequence()?;
    let (_, it) = q.find_item(clip)?;
    let missing = (need - handle(s, it, edge)?).0;
    if missing <= 0 {
        return None;
    }
    let frame = q.settings.frame_rate.frame_duration().0.max(1);
    let edge = if edge == Edge::In { "in" } else { "out" };
    Some(json!({"clip": clip.0, "edge": edge, "shortByFrames": missing.saturating_add(frame - 1) / frame}))
}

/// What moving the cut between `left` (ending there) and `right` (starting there) by `delta`
/// needs and the media does not have: later extends `left` past its Out point, earlier extends
/// `right` before its In point. A trim is a cut with one side.
pub(crate) fn cut_short(s: &Session, left: Option<ClipId>, right: Option<ClipId>, delta: Tick) -> Option<Value> {
    if delta > Tick::ZERO { short(s, left?, Edge::Out, delta) } else { short(s, right?, Edge::In, -delta) }
}

/// [`cut_short`] for a trim of `edge` of each of `clips` by `delta` (a clip on a locked track
/// is not trimmed, so it is never short).
pub(crate) fn trim_short(s: &Session, clips: &[ClipId], edge: Edge, delta: Tick) -> Vec<Value> {
    let Some(q) = s.active_sequence() else { return Vec::new() };
    let unlocked = |c: &&ClipId| q.find_item(**c).is_some_and(|(t, _)| q.track(t).is_some_and(|tr| !tr.locked));
    clips
        .iter()
        .filter(unlocked)
        .filter_map(|c| match edge {
            Edge::In => cut_short(s, None, Some(*c), delta),
            Edge::Out => cut_short(s, Some(*c), None, delta),
        })
        .collect()
}

/// What the clips transition `id` joins lack under it: the outgoing clip plays on past its Out
/// point to the transition's end, the incoming one from the transition's start up to its In point.
pub(crate) fn transition_short(s: &Session, id: TransitionId) -> Vec<Value> {
    let Some(q) = s.active_sequence() else { return Vec::new() };
    let Some((track, tr)) = q.all_tracks().find_map(|t| t.transitions.iter().find(|x| x.id == id).map(|x| (t, x))) else {
        return Vec::new();
    };
    let from = tr.from.and_then(|c| track.item(c)).and_then(|a| short(s, a.id, Edge::Out, tr.end() - a.end()));
    let to = tr.to.and_then(|c| track.item(c)).and_then(|b| short(s, b.id, Edge::In, b.start - tr.start));
    from.into_iter().chain(to).collect()
}
