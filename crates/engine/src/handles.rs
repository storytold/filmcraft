//! Media handles: the media a clip has before its In point and after its Out point.
//!
//! A trim, roll or extend stops where a clip's media ends, and a transition plays the clips it
//! joins past the cut (with no media there, the picture repeats a frame and the sound is
//! silent). The results of those commands list as `{clip, edge, shortByFrames}` each clip edge
//! whose media stopped an edit, or that has no media under part of a transition, so a script or
//! agent can tell an edit the media stopped from one that happened, and which frames of a
//! transition repeat.

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

/// `{clip, edge, shortByFrames}` for `missing` sequence ticks of media, in whole sequence frames
/// (part of a frame counts). `None` when nothing is missing.
fn entry(s: &Session, clip: ClipId, edge: Edge, missing: Tick) -> Option<Value> {
    if missing <= Tick::ZERO {
        return None;
    }
    let rate = s.active_sequence()?.settings.frame_rate;
    // counted on the sequence's frame boundaries: at a rate whose frame is not a whole number of
    // ticks (37.516 fps), `frame_duration` rounds down and dividing by it counts a frame too many
    let frames = rate.frame_at(Tick(missing.0 - 1)).saturating_add(1);
    let edge = if edge == Edge::In { "in" } else { "out" };
    Some(json!({"clip": clip.0, "edge": edge, "shortByFrames": frames}))
}

/// How much media `clip` lacks to show `need` (sequence ticks) beyond `edge`. `None` when it has
/// enough.
pub(crate) fn short(s: &Session, clip: ClipId, edge: Edge, need: Tick) -> Option<Value> {
    let q = s.active_sequence()?;
    let (_, it) = q.find_item(clip)?;
    entry(s, clip, edge, need - handle(s, it, edge)?)
}

/// The clip edge that moving a cut extends, and the media the clip has beyond it, taken before the
/// edit. Moving the cut later extends the clip that ends there past its Out point; moving it
/// earlier extends the clip that starts there before its In point.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Reach {
    clip: ClipId,
    edge: Edge,
    media: Tick,
}

impl Reach {
    /// The edge of `left` or `right` (the clips ending and starting at a cut; a trim has only one)
    /// that moving the cut by `asked` extends. `None` when the move extends no clip, or nothing
    /// limits that clip's media.
    pub(crate) fn of_cut(s: &Session, left: Option<ClipId>, right: Option<ClipId>, asked: Tick) -> Option<Reach> {
        let (clip, edge) = match asked.0.signum() {
            1 => (left?, Edge::Out),
            -1 => (right?, Edge::In),
            _ => return None,
        };
        let q = s.active_sequence()?;
        let (_, it) = q.find_item(clip)?;
        Some(Reach { clip, edge, media: handle(s, it, edge)? })
    }

    /// What the media lacked when it is what stopped the move: the cut was `asked` to move and
    /// moved `applied`, which used all the media there. `None` when it went all the way, or when
    /// something else stopped it first (a neighbour, the frame a clip keeps, a linked clip with
    /// less media, a transition on the cut).
    pub(crate) fn short(&self, s: &Session, asked: Tick, applied: Tick) -> Option<Value> {
        // distances along this edge, so a cut moved earlier counts as positive too
        let (asked, moved) = if self.edge == Edge::Out { (asked, applied) } else { (-asked, -applied) };
        if moved >= asked || moved.max(Tick::ZERO) < self.media {
            return None;
        }
        entry(s, self.clip, self.edge, asked - self.media)
    }
}

/// [`Reach::of_cut`] for a trim of `edge` of each of `clips` by `asked` (a clip on a locked track
/// is not trimmed, so it never stops one).
pub(crate) fn trim_reach(s: &Session, clips: &[ClipId], edge: Edge, asked: Tick) -> Vec<Reach> {
    let Some(q) = s.active_sequence() else { return Vec::new() };
    let unlocked = |c: &&ClipId| q.find_item(**c).is_some_and(|(t, _)| q.track(t).is_some_and(|tr| !tr.locked));
    clips
        .iter()
        .filter(unlocked)
        .filter_map(|c| match edge {
            Edge::In => Reach::of_cut(s, None, Some(*c), asked),
            Edge::Out => Reach::of_cut(s, Some(*c), None, asked),
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
