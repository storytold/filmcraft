//! Dragging a transition snaps as clip trims do (#4328): an end dragged, or the whole transition
//! moved, lands on a clip edge or the playhead near it when snapping is on, and on the frame under
//! the pointer when it is off.
//!
//! The sequence of [`harness`]`(false)`: clips at 0..48f and 48..96f with the default 30-frame
//! cross dissolve centred on their cut (33..63f), at 2.5 px a frame.

use egui::pos2;
use filmcraft_engine::Session;
use serde_json::json;

use super::timeline_transition_move_ui::{harness, pointer, rect, step};

/// Drags the cut transition by `dx` px from the middle of `part` (its automation id: the body,
/// `.in` or `.out`) with the playhead at `playhead` frames and snapping `on` or off, and returns
/// the committed transition's start and end in frames and its alignment.
fn drag_transition(part: &str, dx: f32, playhead: i64, on: bool) -> (i64, i64, String) {
    drag_transition_after(part, dx, playhead, on, |_, _, _| {})
}

/// [`drag_transition`] once `setup` has changed the session, given the transition's id and the
/// ticks in a frame.
fn drag_transition_after(part: &str, dx: f32, playhead: i64, on: bool, setup: impl FnOnce(&mut Session, u64, i64)) -> (i64, i64, String) {
    let mut h = harness(false);
    let rate = h.state().session.sequence_rate();
    let frame = rate.tick_of(1).0;
    h.state_mut().session.set_playhead(rate.tick_of(playhead));
    h.state_mut().session.state.snapping = on;
    step(&mut h);
    let tr0 = h.state().session.active_sequence().unwrap().video_tracks[0].transitions[0].clone();
    assert_eq!((tr0.start.0 / frame, tr0.end().0 / frame), (33, 63), "the default 30-frame dissolve centred on the cut at 48f");
    setup(&mut h.state_mut().session, tr0.id.0, frame);
    step(&mut h);
    let p0 = rect(&h, &format!("timeline.transition.{}{part}", tr0.id.0)).center();
    pointer(&mut h, p0, Some(true));
    for i in 1..=5 {
        pointer(&mut h, pos2(p0.x + dx * i as f32 / 5.0, p0.y), None);
    }
    pointer(&mut h, pos2(p0.x + dx, p0.y), Some(false));
    let q = h.state_mut().session.execute("sequence.inspect", json!({})).unwrap();
    let tr1 = q["video"][0]["transitions"][0].clone();
    assert_eq!(tr1["id"], json!(tr0.id.0));
    let (start, duration) = (tr1["start"].as_i64().unwrap(), tr1["duration"].as_i64().unwrap());
    (start / frame, (start + duration) / frame, tr1["align"].as_str().unwrap_or_default().to_owned())
}

fn span(part: &str, dx: f32, playhead: i64, on: bool) -> (i64, i64) {
    let (s, e, _) = drag_transition(part, dx, playhead, on);
    (s, e)
}

/// An end, or the whole transition, dragged to one frame from the playhead lands on it. 15.5 px
/// is 6 frames.
#[test]
fn dragging_a_transition_snaps_its_edges_to_the_playhead() {
    // the end, dragged out to 69f with the playhead at 70f
    assert_eq!(span(".out", 15.5, 70, true), (33, 70), "the end snaps to the playhead");
    assert_eq!(span(".out", 15.5, 70, false), (33, 69), "snapping off: the raw frame");
    // the start, dragged out to 27f with the playhead at 26f
    assert_eq!(span(".in", -15.5, 26, true), (26, 63), "the start snaps to the playhead");
    assert_eq!(span(".in", -15.5, 26, false), (27, 63), "snapping off: the raw frame");
    // the whole transition, moved right to 39..69f, with the playhead at 40f by its start
    assert_eq!(span("", 15.5, 40, true), (40, 70), "the moved start snaps to the playhead");
    assert_eq!(span("", 15.5, 40, false), (39, 69), "snapping off: the raw frame");
}

/// A transition moved to a frame before the cut, with the playhead far away, snaps onto the cut:
/// Start at Cut. 35.5 px is 14 frames: 47..77f.
#[test]
fn a_moved_transition_snaps_onto_the_cut() {
    assert_eq!(drag_transition("", 35.5, 0, true), (48, 78, "start".to_owned()), "snapped onto the cut: Start at Cut");
    assert_eq!(span("", 35.5, 0, false), (47, 77), "snapping off: the raw frame");
}

/// With both ends of a moved transition near a target, the closer one wins: moved to 46..76f
/// (32.7 px, 13 frames), the start is two frames from the cut and the end one from the playhead.
#[test]
fn a_moved_transition_snaps_the_end_nearer_its_target() {
    assert_eq!(span("", 32.7, 77, true), (47, 77), "the end snaps to the playhead, not the start to the cut (48..78f)");
    assert_eq!(span("", 32.7, 77, false), (46, 76), "snapping off: the raw frame");
}

/// An end dragged near the end of its clip snaps there, not to the playhead just past it (and
/// further away): to 94f (77.7 px, 31 frames) with the clip ending at 96f and the playhead at 97f.
#[test]
fn a_transition_end_snaps_to_its_clips_end_before_the_playhead_past_it() {
    assert_eq!(span(".out", 77.7, 97, true), (33, 96), "stops at the clip's end");
    assert_eq!(span(".out", 77.7, 97, false), (33, 94), "snapping off: the raw frame");
}

/// The end of a Start at Cut transition (48..60f) dragged to 50f (-25.3 px, 10 frames) passes
/// over the nearest target it can't reach for the next one: the cut at 48f, two frames off, would
/// leave none of it, so it lands on the marker at 53f, three frames off.
#[test]
fn a_transition_end_passes_over_a_target_it_cannot_reach_for_the_next() {
    let setup = |s: &mut Session, id: u64, frame: i64| {
        s.execute("sequence.setTransition", json!({"transition": id, "start": 48 * frame, "duration": 12 * frame})).unwrap();
        s.execute("markers.add", json!({"time": 53 * frame})).unwrap();
    };
    assert_eq!(drag_transition_after(".out", -25.3, 0, true, setup), (48, 53, "start".to_owned()), "on the marker");
    assert_eq!(drag_transition_after(".out", -25.3, 0, false, setup), (48, 50, "start".to_owned()), "snapping off: the raw frame");
}
