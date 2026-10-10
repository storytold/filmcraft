//! Selecting a gap between clips and closing it with Delete / Backspace / Ripple Delete (#648, #668):
//! `timeline.selectGap` selects the empty time on one track, `edit.clear` and `edit.rippleDelete`
//! close it as one "Ripple Delete" undo step.

use filmcraft_project::ClipId;
use filmcraft_time::Tick;
use serde_json::json;

use crate::{GapSelection, Session};

fn range(s: &Session, c: ClipId) -> (Tick, Tick) {
    s.active_sequence().unwrap().find_item(c).map(|(_, i)| (i.start, i.end())).unwrap()
}

/// Two 48-frame clips (pictures on V1, linked sounds on A1) placed at `frames`.
fn two_clips(s: &mut Session, frames: [i64; 2]) -> [(ClipId, ClipId); 2] {
    s.execute("file.openDemoProject", json!({})).unwrap();
    s.execute("file.newSequence", json!({"name": "Gaps", "video": 1, "audio": 1})).unwrap();
    let rate = s.sequence_rate();
    let item = s.project.items.values().find(|i| i.has_video() && i.has_audio() && i.as_media().is_some()).map(|i| i.id).unwrap();
    for f in frames {
        s.execute("timeline.place", json!({"item": item.0, "frame": f, "sourceIn": rate.tick_of(48).0, "duration": rate.tick_of(48).0})).unwrap();
    }
    s.execute("edit.deselectAll", json!({})).unwrap();
    let q = s.active_sequence().unwrap();
    [0, 1].map(|n| (q.video_tracks[0].items[n].id, q.audio_tracks[0].items[n].id))
}

#[test]
fn clicking_between_two_clips_selects_the_gap_and_delete_closes_it() {
    let mut s = Session::default();
    let [(picture1, _), (picture2, sound2)] = two_clips(&mut s, [0, 96]);
    let rate = s.sequence_rate();
    let f = |n: i64| rate.tick_of(n);
    let (f48, f60, f96) = (f(48), f(60), f(96));
    s.execute("timeline.select", json!({"clips": [picture1.0]})).unwrap();
    let before = s.project.clone();

    let r = s.execute("timeline.selectGap", json!({"track": "V1", "time": f60.0})).unwrap();
    assert_eq!(r, json!({"track": s.active_sequence().unwrap().video_tracks[0].id.0, "start": f48.0, "end": f96.0}));
    assert!(s.state.selection.is_empty(), "the gap replaces the clip selection");
    assert!(s.is_enabled("edit.clear") && s.is_enabled("edit.rippleDelete"), "Delete and Ripple Delete apply to the gap");

    // Delete / Backspace
    s.execute("edit.clear", json!({})).unwrap();
    assert_eq!(range(&s, picture2), (f48, f96), "the next clip moves up into the gap");
    assert_eq!(range(&s, sound2), (f48, f96), "and its sound with it");
    assert_eq!(range(&s, picture1).0, Tick::ZERO, "the clip before stays");
    assert!(s.state.gap_selection.is_none(), "the gap is gone");
    s.active_sequence().unwrap().check().unwrap();

    assert_eq!(s.undo().as_deref(), Some("Ripple Delete"), "one undo step");
    assert_eq!(*s.project, *before);
}

#[test]
fn ripple_delete_closes_a_selected_gap_too() {
    let mut s = Session::default();
    let [_, (picture2, _)] = two_clips(&mut s, [0, 96]);
    let rate = s.sequence_rate();
    let f = |n: i64| rate.tick_of(n);
    s.execute("timeline.selectGap", json!({"track": "A1", "frame": 50})).unwrap();
    s.execute("edit.rippleDelete", json!({})).unwrap();
    assert_eq!(range(&s, picture2).0, f(48));
}

#[test]
fn the_empty_time_before_the_first_clip_is_a_gap() {
    let mut s = Session::default();
    let [(picture1, _), (picture2, _)] = two_clips(&mut s, [24, 72]);
    let rate = s.sequence_rate();
    let f = |n: i64| rate.tick_of(n);
    let r = s.execute("timeline.selectGap", json!({"track": "V1", "frame": 5})).unwrap();
    assert_eq!((r["start"].clone(), r["end"].clone()), (json!(0), json!(f(24).0)));
    s.execute("edit.clear", json!({})).unwrap();
    assert_eq!((range(&s, picture1).0, range(&s, picture2).0), (Tick::ZERO, f(48)));
}

#[test]
fn empty_time_after_the_last_clip_is_not_a_gap() {
    let mut s = Session::default();
    two_clips(&mut s, [0, 96]);
    assert!(s.execute("timeline.selectGap", json!({"track": "V1", "frame": 400})).is_err());
    assert!(s.execute("timeline.selectGap", json!({"track": "V1", "frame": 10})).is_err(), "inside a clip");
    assert!(s.state.gap_selection.is_none());
    assert!(!s.is_enabled("edit.clear"), "nothing to delete");
}

#[test]
fn selecting_a_clip_or_deselecting_all_drops_the_gap() {
    let mut s = Session::default();
    let [(picture1, _), _] = two_clips(&mut s, [0, 96]);
    s.execute("timeline.selectGap", json!({"track": "V1", "frame": 60})).unwrap();
    s.execute("timeline.select", json!({"clips": [picture1.0]})).unwrap();
    assert!(s.state.gap_selection.is_none());
    s.execute("timeline.selectGap", json!({"track": "V1", "frame": 60})).unwrap();
    s.execute("edit.deselectAll", json!({})).unwrap();
    assert!(s.state.gap_selection.is_none());
}

/// An edit that fills the gap leaves no gap selected; Delete then removes nothing it should not.
#[test]
fn an_edit_that_changes_the_gap_drops_it() {
    let mut s = Session::default();
    let [(picture1, _), (picture2, _)] = two_clips(&mut s, [0, 96]);
    let rate = s.sequence_rate();
    let f = |n: i64| rate.tick_of(n);
    s.execute("timeline.selectGap", json!({"track": "V1", "frame": 60})).unwrap();
    s.execute("timeline.trim", json!({"clip": picture1.0, "edge": "out", "mode": "regular", "deltaFrames": 12})).unwrap();
    assert!(s.state.gap_selection.is_none(), "the gap changed");
    assert!(s.execute("edit.clear", json!({})).is_err());
    assert_eq!(range(&s, picture2).0, f(96), "nothing moved");
}

/// A stale selection (set from outside, as the control channel can) is refused, not acted on.
#[test]
fn a_stale_gap_is_refused() {
    let mut s = Session::default();
    let [_, (picture2, _)] = two_clips(&mut s, [0, 96]);
    let rate = s.sequence_rate();
    let f = |n: i64| rate.tick_of(n);
    let track = s.active_sequence().unwrap().video_tracks[0].id;
    s.state.gap_selection = Some(GapSelection { track, range: filmcraft_time::TimeRange::from_bounds(f(10), f(20)) });
    assert!(s.execute("edit.clear", json!({})).is_err());
    assert!(s.state.gap_selection.is_none());
    assert_eq!(range(&s, picture2).0, f(96));
}

#[test]
fn a_gap_on_a_locked_track_cannot_be_selected_or_closed() {
    let mut s = Session::default();
    let [_, (picture2, _)] = two_clips(&mut s, [0, 96]);
    let rate = s.sequence_rate();
    let f = |n: i64| rate.tick_of(n);
    s.execute("timeline.setTrack", json!({"track": "V1", "locked": true})).unwrap();
    assert!(s.execute("timeline.selectGap", json!({"track": "V1", "frame": 60})).is_err());
    // Sequence ▸ Close Gap moved the other tracks while the locked one stayed put
    assert!(s.execute("sequence.closeGap", json!({"track": "V1", "frame": 60})).is_err());
    assert_eq!(range(&s, picture2).0, f(96));
    let sound2 = s.active_sequence().unwrap().audio_tracks[0].items[1].id;
    assert_eq!(range(&s, sound2).0, f(96), "the unlocked sound stays in sync");
}

#[test]
fn hostile_select_gap_parameters_are_errors() {
    let mut s = Session::default();
    assert!(s.execute("timeline.selectGap", json!({"track": "V1", "frame": 1})).is_err(), "no sequence");
    two_clips(&mut s, [0, 96]);
    for p in [
        json!({}),
        json!({"track": "V1"}),
        json!({"frame": 60}),
        json!({"track": "V99", "frame": 60}),
        json!({"track": 999_999, "frame": 60}),
        json!({"track": "V1", "time": i64::MAX}),
        json!({"track": "V1", "time": i64::MIN}),
        json!({"track": "V1", "frame": i64::MAX}),
        json!({"track": "V1", "seconds": f64::MAX}),
        json!({"track": [], "frame": "x"}),
    ] {
        let _ = s.execute("timeline.selectGap", p);
    }
    assert!(s.state.gap_selection.is_none());
}

#[test]
fn the_gap_selection_is_part_of_the_ui_state() {
    let mut s = Session::default();
    two_clips(&mut s, [0, 96]);
    s.execute("timeline.selectGap", json!({"track": "V1", "frame": 60})).unwrap();
    let v = serde_json::to_value(&s.state).unwrap();
    let back: crate::EditorState = serde_json::from_value(v).unwrap();
    assert_eq!(back.gap_selection, s.state.gap_selection);
}
