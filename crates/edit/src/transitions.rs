//! Applied transitions: where one may sit over its cut, its alignment, and moving / resizing /
//! removing one (Effect Controls' Duration and Alignment, dragging its edges or its middle in the
//! Timeline, Delete).
//!
//! Premiere (Align transitions; Change transition duration): a transition between two clips stays
//! over their cut, "Center at Cut", "Start at Cut", "End at Cut" or, once dragged off-centre,
//! "Custom Start". Changing its duration moves both ends equally for Center at Cut / Custom Start,
//! only the end for Start at Cut and only the beginning for End at Cut. A one-sided transition (a
//! fade from or to nothing) starts or ends at its clip's edge.
//!
//! Premiere limits a transition by the media its clips have beyond the cut (handles). FilmCraft
//! does not check handles when applying one, so here a transition may cover at most the clips it
//! joins: from the outgoing clip's start to the incoming clip's end.

use filmcraft_project::{Sequence, Track, Transition, TransitionAlign, TransitionId};
use filmcraft_time::Tick;

use crate::{Edge, EditError, Result};

/// How a transition sits on its cut, as Effect Controls' Alignment menu shows it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Alignment {
    Center,
    Start,
    End,
    /// Premiere's "Custom Start": dragged to a place over the cut that is none of the others.
    Custom,
}

impl Alignment {
    pub const MENU: [Alignment; 3] = [Alignment::Center, Alignment::Start, Alignment::End];

    pub fn id(self) -> &'static str {
        match self {
            Alignment::Center => "center",
            Alignment::Start => "start",
            Alignment::End => "end",
            Alignment::Custom => "custom",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Alignment::Center => "Center at Cut",
            Alignment::Start => "Start at Cut",
            Alignment::End => "End at Cut",
            Alignment::Custom => "Custom Start",
        }
    }

    /// `center`, `start` or `end` (Custom Start is reached by moving a transition, not chosen).
    pub fn parse(s: &str) -> Option<Alignment> {
        Alignment::MENU.into_iter().find(|a| a.id().eq_ignore_ascii_case(s) || a.label().eq_ignore_ascii_case(s))
    }
}

/// Where a transition may be: `(earliest start, cut, latest end)`. Between two clips that is the
/// outgoing clip's start, the cut and the incoming clip's end; for a fade to or from nothing the cut
/// is its clip's edge. `None` when its clips are gone.
pub fn bounds(track: &Track, tr: &Transition) -> Option<(Tick, Tick, Tick)> {
    let from = tr.from.and_then(|c| track.item(c));
    let to = tr.to.and_then(|c| track.item(c));
    match (from, to) {
        (Some(f), Some(t)) => Some((f.start, t.start, t.end())),
        (None, Some(t)) => Some((t.start, t.start, t.end())),
        (Some(f), None) => Some((f.start, f.end(), f.end())),
        (None, None) => None,
    }
}

/// Can `edge` of `tr` be dragged to resize it? A one-sided transition pins the end at its clip's
/// edge, Start at Cut pins its start on the cut and End at Cut pins its end, so those ends do not
/// move ([`drag_span`] leaves them where they are). The two timelines show a trim handle only on
/// draggable ends.
pub fn draggable_end(tr: &Transition, cut: Tick, frame: Tick, edge: Edge) -> bool {
    let align = alignment(tr, cut, frame);
    !(edge == Edge::In && align == Alignment::Start) && !(edge == Edge::Out && align == Alignment::End)
}

/// A transition's alignment on `cut`. Centred means within half a `frame` of the cut, as
/// centring an odd number of frames lands on a frame boundary.
pub fn alignment(tr: &Transition, cut: Tick, frame: Tick) -> Alignment {
    match (tr.from, tr.to) {
        (None, Some(_)) => return Alignment::Start,
        (Some(_), None) => return Alignment::End,
        _ => {}
    }
    if tr.start == cut {
        Alignment::Start
    } else if tr.end() == cut {
        Alignment::End
    } else if (tr.start.0.saturating_mul(2).saturating_add(tr.duration.0) - cut.0.saturating_mul(2)).abs() <= frame.0.max(0) {
        Alignment::Center
    } else {
        Alignment::Custom
    }
}

fn find(seq: &Sequence, id: TransitionId) -> Option<(&Track, &Transition)> {
    seq.all_tracks().find_map(|t| t.transitions.iter().find(|x| x.id == id).map(|x| (t, x)))
}

/// Where a transition would start for `duration` and `align`, keeping its cut (and, for Center
/// at Cut / Custom Start, its centre; `snap` puts a centred start on a frame).
pub fn start_for(seq: &Sequence, id: TransitionId, duration: Tick, align: Alignment, frame: Tick, snap: impl Fn(Tick) -> Tick) -> Option<Tick> {
    let (track, tr) = find(seq, id)?;
    let (_, cut, _) = bounds(track, tr)?;
    let one_sided = alignment(tr, cut, frame);
    let align = match one_sided {
        Alignment::Start | Alignment::End if tr.from.is_none() || tr.to.is_none() => one_sided,
        _ => align,
    };
    Some(match align {
        Alignment::Start => cut,
        Alignment::End => cut - duration,
        Alignment::Center => snap(cut - duration.mul_ratio(1, 2)),
        // Custom Start: the ends move equally, around where it is now
        Alignment::Custom => snap(tr.start + tr.duration.mul_ratio(1, 2) - duration.mul_ratio(1, 2)),
    })
}

/// Move / resize a transition to cover `start .. start + duration`. Refused (leaving the sequence
/// as it was) when its track is locked, it would be shorter than `min`, it would leave its clips,
/// or it would no longer touch its cut (a one-sided one must keep its edge).
pub fn set_span(seq: &mut Sequence, id: TransitionId, start: Tick, duration: Tick, min: Tick, frame: Tick) -> Result<()> {
    let (track, tr) = find(seq, id).ok_or_else(|| EditError::Other(format!("no transition {}", id.0)))?;
    if track.locked {
        return Err(EditError::Locked);
    }
    let (lo, cut, hi) = bounds(track, tr).ok_or_else(|| EditError::Other("the transition's clips are gone".into()))?;
    if duration < min.max(Tick(1)) {
        return Err(EditError::Other("a transition lasts at least one frame".into()));
    }
    let end = start.0.checked_add(duration.0).map(Tick).ok_or_else(|| EditError::Other("transition out of range".into()))?;
    if start < lo || end > hi {
        return Err(EditError::Other("a transition can't extend past the clips it joins".into()));
    }
    let on_cut = match (tr.from, tr.to) {
        (None, Some(_)) => start == cut,
        (Some(_), None) => end == cut,
        _ => start <= cut && cut <= end,
    };
    if !on_cut {
        return Err(EditError::Other(if tr.from.is_some() && tr.to.is_some() {
            "a transition between two clips must stay over their cut".into()
        } else {
            "a one-sided transition stays on its clip's edge".into()
        }));
    }
    let tid = track.id;
    let Some(x) = seq.track_mut(tid).and_then(|t| t.transitions.iter_mut().find(|x| x.id == id)) else {
        return Err(EditError::Other(format!("no transition {}", id.0)));
    };
    x.start = start;
    x.duration = duration;
    // the stored alignment (interchange): Custom Start has no variant, its span says it all
    x.align = match alignment(x, cut, frame) {
        Alignment::Start => TransitionAlign::StartAtCut,
        Alignment::End => TransitionAlign::EndAtCut,
        Alignment::Center | Alignment::Custom => TransitionAlign::CenterAtCut,
    };
    Ok(())
}

/// Where dragging a transition by `delta` in the Timeline puts it, as `(start, duration)`. Its
/// middle (`edge` None) slides it over the cut, the one move that turns a transition into
/// Custom Start. An end changes the duration without breaking the alignment: the grabbed end
/// follows the pointer, the other end moves as the alignment says. Center at Cut keeps its
/// midpoint on the cut, so dragging the right end out also pulls the left end out by the same
/// amount; Start at Cut keeps its start on the cut and End at Cut its end, the cut-side end
/// pinned where it is (dragging it does nothing); Custom Start trims just the grabbed end. (A
/// typed duration, in Effect Controls or Set Transition Duration, follows the alignment too: see
/// [`start_for`].) A fade to or from nothing keeps its clip-edge end, and does not slide. Always
/// clamped to where it may be ([`bounds`], over the cut, at least one `frame`). `None` when its
/// clips are gone.
pub fn drag_span(track: &Track, tr: &Transition, edge: Option<Edge>, delta: Tick, frame: Tick) -> Option<(Tick, Tick)> {
    let (lo, cut, hi) = bounds(track, tr)?;
    let frame = frame.max(Tick(1));
    let (s0, e0, d0) = (tr.start, tr.end(), tr.duration);
    let one_sided = tr.from.is_none() || tr.to.is_none();
    let unchanged = Some((s0, d0));
    match edge {
        None if one_sided => unchanged,
        None => {
            let (min_s, max_s) = (lo.max(cut - d0), (hi - d0).min(cut));
            if min_s > max_s { unchanged } else { Some(((s0 + delta).clamp(min_s, max_s), d0)) }
        }
        Some(Edge::In) if tr.from.is_none() => unchanged,
        Some(Edge::Out) if tr.to.is_none() => unchanged,
        Some(e) if !draggable_end(tr, cut, frame, e) => unchanged, // pinned: see [`draggable_end`]
        Some(e) => match (alignment(tr, cut, frame), e) {
            // Center at Cut: the grabbed end tracks the pointer, the other moves the same way,
            // so the midpoint (on the cut) holds; it stops when a side of its clips runs out
            (Alignment::Center, Edge::In) => {
                let shrink = d0.0.saturating_sub(frame.0) / 2;
                let grab = delta.0.clamp((lo.0 - s0.0).max(e0.0 - hi.0), shrink);
                let (st, en) = (Tick(s0.0 + grab), Tick(e0.0 - grab));
                Some((st, en - st))
            }
            (Alignment::Center, Edge::Out) => {
                let shrink = d0.0.saturating_sub(frame.0) / 2;
                let grab = delta.0.clamp(-shrink, (s0.0 - lo.0).min(hi.0 - e0.0));
                let (st, en) = (Tick(s0.0 - grab), Tick(e0.0 + grab));
                Some((st, en - st))
            }
            // a Custom Start's ends, and the free end of a Start / End at Cut transition, trim
            // alone: that end follows the pointer, the other stays (Premiere shows the Trim-In /
            // Trim-Out icon there)
            (_, Edge::In) => {
                let max_s = cut.min(e0 - frame).max(lo);
                let st = (s0 + delta).clamp(lo, max_s);
                Some((st, e0 - st))
            }
            (_, Edge::Out) => {
                let min_e = cut.max(s0 + frame).min(hi);
                let en = (e0 + delta).clamp(min_e, hi);
                Some((s0, en - s0))
            }
        },
    }
}

/// Remove transitions (on unlocked tracks). Returns how many went.
pub fn remove(seq: &mut Sequence, ids: &[TransitionId]) -> Result<usize> {
    if seq.all_tracks().any(|t| t.locked && t.transitions.iter().any(|x| ids.contains(&x.id))) {
        return Err(EditError::Locked);
    }
    let mut n = 0;
    for t in seq.all_tracks_mut() {
        let before = t.transitions.len();
        t.transitions.retain(|x| !ids.contains(&x.id));
        n += before - t.transitions.len();
    }
    if n == 0 { Err(EditError::Nothing) } else { Ok(n) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use filmcraft_project::{ClipId, ItemId, Label, Project, SequenceSettings, TrackItem, find_effect};
    use filmcraft_time::FrameRate;

    const R: FrameRate = FrameRate::FPS_24;

    fn f(n: i64) -> Tick {
        R.tick_of(n)
    }

    fn clip(id: u64, start: i64, dur: i64) -> TrackItem {
        TrackItem {
            id: ClipId(id),
            item: ItemId(1),
            name: format!("c{id}"),
            label: Label::Iris,
            start: f(start),
            duration: f(dur),
            source_in: Tick::ZERO,
            speed: 1.0,
            reverse: false,
            enabled: true,
            link: None,
            group: None,
            effects: vec![],
            markers: vec![],
            gain_db: 0.0,
            frame_hold: None,
            scale_to_frame: false,
            essential: None,
            multicam: None,
            time_interpolation: Default::default(),
            hold_filters: false,
            field_options: None,
            source_channels: Vec::new(),
            audio_stream: 0,
            graphic: None,
        }
    }

    fn transition(id: u64, start: i64, dur: i64, from: Option<u64>, to: Option<u64>) -> Transition {
        // any effect: these tests are about where the transition sits
        let effect = find_effect("opacity").unwrap().instance();
        Transition {
            id: TransitionId(id),
            effect,
            start: f(start),
            duration: f(dur),
            from: from.map(ClipId),
            to: to.map(ClipId),
            align: TransitionAlign::CenterAtCut,
            reverse: false,
        }
    }

    /// V1: c1 0..48, c2 48..96, c3 96..144; a centred 24-frame transition on 48 (id 1), a fade in
    /// at 0 (id 2) and a fade out at 144 (id 3).
    fn seq() -> Sequence {
        let mut p = Project::new("t");
        let s = p.new_sequence("s", SequenceSettings { frame_rate: R, ..Default::default() }, 1, 0, None);
        let mut q = p.sequence(s).unwrap().clone();
        let t = &mut q.video_tracks[0];
        t.items = vec![clip(1, 0, 48), clip(2, 48, 48), clip(3, 96, 48)];
        t.transitions = vec![transition(1, 36, 24, Some(1), Some(2)), transition(2, 0, 12, None, Some(1)), transition(3, 132, 12, Some(3), None)];
        q
    }

    fn span(q: &Sequence, id: u64) -> (Tick, Tick) {
        let x = q.video_tracks[0].transitions.iter().find(|x| x.id.0 == id).unwrap();
        (x.start, x.duration)
    }

    fn align(q: &Sequence, id: u64) -> Alignment {
        let (t, x) = find(q, TransitionId(id)).unwrap();
        let (_, cut, _) = bounds(t, x).unwrap();
        alignment(x, cut, f(1))
    }

    #[test]
    fn alignment_reads_the_span() {
        let mut q = seq();
        assert_eq!((align(&q, 1), align(&q, 2), align(&q, 3)), (Alignment::Center, Alignment::Start, Alignment::End));
        set_span(&mut q, TransitionId(1), f(48), f(24), f(1), f(1)).unwrap();
        assert_eq!(align(&q, 1), Alignment::Start);
        set_span(&mut q, TransitionId(1), f(24), f(24), f(1), f(1)).unwrap();
        assert_eq!(align(&q, 1), Alignment::End);
        set_span(&mut q, TransitionId(1), f(40), f(24), f(1), f(1)).unwrap();
        assert_eq!(align(&q, 1), Alignment::Custom);
        // an odd centred duration sits half a frame off the cut and is still Center at Cut
        set_span(&mut q, TransitionId(1), f(42), f(13), f(1), f(1)).unwrap();
        assert_eq!(align(&q, 1), Alignment::Center);
    }

    #[test]
    fn duration_changes_follow_the_alignment() {
        let q = seq();
        let snap = |t: Tick| R.snap(t);
        // Center at Cut: both ends move
        assert_eq!(start_for(&q, TransitionId(1), f(36), Alignment::Center, f(1), snap), Some(f(30)));
        // Start at Cut: only the end moves; End at Cut: only the beginning
        assert_eq!(start_for(&q, TransitionId(1), f(36), Alignment::Start, f(1), snap), Some(f(48)));
        assert_eq!(start_for(&q, TransitionId(1), f(36), Alignment::End, f(1), snap), Some(f(12)));
        // a one-sided transition keeps its edge whatever is asked
        assert_eq!(start_for(&q, TransitionId(2), f(20), Alignment::Center, f(1), snap), Some(f(0)));
        assert_eq!(start_for(&q, TransitionId(3), f(20), Alignment::Center, f(1), snap), Some(f(124)));
    }

    #[test]
    fn custom_start_resizes_around_its_middle() {
        let mut q = seq();
        set_span(&mut q, TransitionId(1), f(40), f(20), f(1), f(1)).unwrap();
        assert_eq!(start_for(&q, TransitionId(1), f(10), Alignment::Custom, f(1), |t| R.snap(t)), Some(f(45)));
    }

    #[test]
    fn spans_stay_on_the_cut_and_inside_the_clips() {
        let mut q = seq();
        let before = q.clone();
        for (id, start, dur) in [
            (1, -1, 24),  // before the outgoing clip
            (1, 40, 60),  // past the incoming clip's end
            (1, 10, 20),  // entirely before the cut
            (1, 50, 10),  // entirely after it
            (1, 40, 0),   // empty
            (2, 1, 12),   // a fade in must start at its clip's start
            (3, 120, 12), // a fade out must end at its clip's end
            (9, 0, 12),   // no such transition
        ] {
            assert!(set_span(&mut q, TransitionId(id), f(start), f(dur), f(1), f(1)).is_err(), "{id} {start} {dur}");
            assert_eq!(q, before, "refused changes leave the sequence as it was");
        }
        // the whole of both clips is allowed
        set_span(&mut q, TransitionId(1), f(0), f(96), f(1), f(1)).unwrap();
        assert_eq!(span(&q, 1), (f(0), f(96)));
        // Tick::MAX never overflows
        assert!(set_span(&mut q, TransitionId(1), Tick(i64::MAX - 1), Tick(i64::MAX), f(1), f(1)).is_err());
    }

    fn drag(q: &Sequence, id: u64, edge: Option<Edge>, frames: i64) -> (i64, i64) {
        let (t, x) = find(q, TransitionId(id)).unwrap();
        let (st, d) = drag_span(t, x, edge, f(frames), f(1)).unwrap();
        (R.frame_at(st), R.frame_at(d))
    }

    #[test]
    fn dragging_a_transition() {
        let mut q = seq();
        // centred (36..60 over the cut at 48): an end drags that end and mirrors the other, so
        // the midpoint stays on the cut and the alignment never becomes Custom Start
        assert_eq!(drag(&q, 1, Some(Edge::In), -4), (32, 32));
        assert_eq!(drag(&q, 1, Some(Edge::Out), -10), (46, 4));
        assert_eq!(drag(&q, 1, Some(Edge::Out), -100), (47, 1), "shrinking stops at a one-frame centre (47.5..48.5)");
        assert_eq!(drag(&q, 1, Some(Edge::Out), 100), (0, 96), "and at the ends of both clips");
        assert_eq!(drag(&q, 1, Some(Edge::In), -100), (0, 96), "the start edge the same way");
        // an odd centred duration (half a frame off the cut) drags centred too
        set_span(&mut q, TransitionId(1), f(42), f(13), f(1), f(1)).unwrap();
        assert_eq!(align(&q, 1), Alignment::Center);
        assert_eq!(drag(&q, 1, Some(Edge::Out), 2), (40, 17), "the midpoint (half a frame right of the cut) holds");
        set_span(&mut q, TransitionId(1), f(36), f(24), f(1), f(1)).unwrap();
        // the middle slides it, staying over the cut and inside the clips
        assert_eq!(drag(&q, 1, None, 5), (41, 24));
        assert_eq!(drag(&q, 1, None, 100), (48, 24));
        assert_eq!(drag(&q, 1, None, -100), (24, 24));
        // off-centre, Custom Start: an end trims that end only, the other stays
        set_span(&mut q, TransitionId(1), f(40), f(20), f(1), f(1)).unwrap();
        assert_eq!(drag(&q, 1, Some(Edge::In), -4), (36, 24));
        assert_eq!(drag(&q, 1, Some(Edge::In), 100), (48, 12), "the start stops at the cut");
        assert_eq!(drag(&q, 1, Some(Edge::Out), 6), (40, 26));
        assert_eq!(drag(&q, 1, Some(Edge::Out), -100), (40, 8), "the end stops at the cut");
        // Start at Cut keeps its start on the cut (dragging it does nothing), End at Cut its end
        set_span(&mut q, TransitionId(1), f(48), f(20), f(1), f(1)).unwrap();
        assert_eq!(align(&q, 1), Alignment::Start);
        assert_eq!(drag(&q, 1, Some(Edge::In), -5), (48, 20), "the start can't leave the cut");
        assert_eq!(drag(&q, 1, Some(Edge::In), 5), (48, 20), "either way");
        assert_eq!(drag(&q, 1, Some(Edge::Out), 4), (48, 24), "its duration grows from the end");
        assert_eq!(drag(&q, 1, Some(Edge::Out), -100), (48, 1), "and shrinks at least a frame");
        set_span(&mut q, TransitionId(1), f(28), f(20), f(1), f(1)).unwrap();
        assert_eq!(align(&q, 1), Alignment::End);
        assert_eq!(drag(&q, 1, Some(Edge::Out), 5), (28, 20), "the end can't leave the cut");
        assert_eq!(drag(&q, 1, Some(Edge::Out), -5), (28, 20), "either way");
        assert_eq!(drag(&q, 1, Some(Edge::In), -4), (24, 24), "its duration grows from the start");
        // a fade in keeps its start, a fade out its end; neither slides
        assert_eq!(drag(&q, 2, Some(Edge::In), 3), (0, 12));
        assert_eq!(drag(&q, 2, Some(Edge::Out), 3), (0, 15));
        assert_eq!(drag(&q, 2, None, 3), (0, 12));
        assert_eq!(drag(&q, 3, Some(Edge::Out), 3), (132, 12));
        assert_eq!(drag(&q, 3, Some(Edge::In), -3), (129, 15));
        // whatever comes out is a span set_span accepts
        for (id, edge, n) in [(1, Some(Edge::In), -70), (1, None, 33), (2, Some(Edge::Out), 99), (3, Some(Edge::In), -99)] {
            let (t, x) = find(&q, TransitionId(id)).unwrap();
            let (st, d) = drag_span(t, x, edge, f(n), f(1)).unwrap();
            let mut c = q.clone();
            set_span(&mut c, TransitionId(id), st, d, f(1), f(1)).unwrap();
        }
    }

    #[test]
    fn pinned_ends_are_not_draggable() {
        let q = seq();
        let (t, x) = find(&q, TransitionId(1)).unwrap();
        let (_, cut, _) = bounds(t, x).unwrap();
        // Center at Cut: both ends are trim handles
        assert!(draggable_end(x, cut, f(1), Edge::In) && draggable_end(x, cut, f(1), Edge::Out));
        // Start at Cut pins its start, End at Cut its end
        for (start, in_draggable, out_draggable) in [(48, false, true), (28, true, false)] {
            let mut y = x.clone();
            y.start = f(start);
            y.duration = f(20);
            assert_eq!(draggable_end(&y, cut, f(1), Edge::In), in_draggable, "start {start}");
            assert_eq!(draggable_end(&y, cut, f(1), Edge::Out), out_draggable, "start {start}");
        }
        // a one-sided transition pins its clip edge (a fade in the start, a fade out the end)
        for (id, in_draggable, out_draggable) in [(2, false, true), (3, true, false)] {
            let (t, z) = find(&q, TransitionId(id)).unwrap();
            let (_, cut, _) = bounds(t, z).unwrap();
            assert_eq!(draggable_end(z, cut, f(1), Edge::In), in_draggable, "transition {id}");
            assert_eq!(draggable_end(z, cut, f(1), Edge::Out), out_draggable, "transition {id}");
        }
    }

    #[test]
    fn locked_tracks_refuse() {
        let mut q = seq();
        q.video_tracks[0].locked = true;
        assert_eq!(set_span(&mut q, TransitionId(1), f(40), f(20), f(1), f(1)), Err(EditError::Locked));
        assert_eq!(remove(&mut q, &[TransitionId(1)]), Err(EditError::Locked));
    }

    #[test]
    fn remove_takes_only_the_named_transitions() {
        let mut q = seq();
        assert_eq!(remove(&mut q, &[TransitionId(1), TransitionId(3)]), Ok(2));
        let left: Vec<u64> = q.video_tracks[0].transitions.iter().map(|x| x.id.0).collect();
        assert_eq!(left, vec![2]);
        assert_eq!(q.video_tracks[0].items.len(), 3, "the clips stay");
        assert_eq!(remove(&mut q, &[TransitionId(1)]), Err(EditError::Nothing));
    }
}
