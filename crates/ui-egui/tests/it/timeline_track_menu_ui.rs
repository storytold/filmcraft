//! The track header's context menu (#1117): right-clicking a track header offers Add Track, Add
//! Tracks…, Delete Track and Delete Tracks…, next to the Sequence menu's dialogs.

use crate::support::{Drive, Launch, driver};
use serde_json::json;

driver! {
    Driver;
    fn after_ok(&mut self) {
        self.frames(2);
    }
}

fn counts(d: &Driver) -> (usize, usize) {
    let q = d.harness.state().session.active_sequence().unwrap();
    (q.video_tracks.len(), q.audio_tracks.len())
}

/// A point on track `label`'s header, clear of its buttons: just left of the first one.
fn header_point(d: &mut Driver, label: &str) -> (f64, f64) {
    let patch = d.expect_rect(&format!("timeline.track.{label}.sourcePatch"));
    (patch[0] - 4.0, patch[1] + patch[3] / 2.0)
}

#[test]
fn a_track_header_menu_adds_and_deletes_tracks() {
    let mut d = Driver::demo();
    let (v0, a0) = counts(&d);
    let (x, y) = header_point(&mut d, "V1");
    d.ok("ui.click", json!({"x": x, "y": y, "button": "right"}));
    for item in ["addTrack", "addTracks", "deleteTrack", "deleteTracks"] {
        assert!(d.has(&format!("timeline.track.V1.menu.{item}")), "{item} is offered");
    }
    // Add Track puts one video track after V1
    let (v1, v2) = {
        let q = d.harness.state().session.active_sequence().unwrap();
        (q.video_tracks[0].id, q.video_tracks[1].id)
    };
    d.ok("ui.click", json!({"id": "timeline.track.V1.menu.addTrack"}));
    d.frames(2);
    assert_eq!(counts(&d), (v0 + 1, a0));
    let q = d.harness.state().session.active_sequence().unwrap();
    assert_eq!(q.video_tracks[0].id, v1, "V1 stays first");
    assert_eq!(q.video_tracks[2].id, v2, "the old V2 is pushed to the third place, the new track sits between");
    // Delete Track on an audio header removes that track
    let a2 = d.harness.state().session.active_sequence().unwrap().audio_tracks[1].id;
    let (x, y) = header_point(&mut d, "A2");
    d.ok("ui.click", json!({"x": x, "y": y, "button": "right"}));
    d.ok("ui.click", json!({"id": "timeline.track.A2.menu.deleteTrack"}));
    d.frames(2);
    assert_eq!(counts(&d), (v0 + 1, a0 - 1));
    assert!(d.harness.state().session.active_sequence().unwrap().track(a2).is_none(), "A2 itself is gone");
    // the strip at the right of a tall header opens it too
    let lock = d.expect_rect("timeline.track.V1.locked");
    let tracks = d.expect_rect("timeline.tracks");
    d.ok("ui.click", json!({"x": tracks[0] - 5.0, "y": lock[1] + lock[3] / 2.0, "button": "right"}));
    assert!(d.has("timeline.track.V1.menu.deleteTracks"));
    d.ok("ui.key", json!({"key": "Escape"}));
    d.frames(2);
    // the dialogs open from the same menu
    let (x, y) = header_point(&mut d, "V1");
    d.ok("ui.click", json!({"x": x, "y": y, "button": "right"}));
    d.ok("ui.click", json!({"id": "timeline.track.V1.menu.addTracks"}));
    d.frames(3);
    assert!(d.harness.state().dialog.is_some(), "Add Tracks… opens its dialog");
}

#[test]
fn the_empty_part_of_the_header_column_offers_the_dialogs() {
    let mut d = Driver::demo();
    // short tracks leave room above the top video track: the header column has no track there
    d.ok("ui.set", json!({"timeline": {"videoTrackHeight": 30.0}}));
    d.frames(4);
    let n = d.harness.state().session.active_sequence().unwrap().video_tracks.len();
    let top = d.expect_rect(&format!("timeline.track.V{n}.sourcePatch"));
    let (x, y) = (top[0] + 30.0, top[1] - 25.0);
    d.ok("ui.click", json!({"x": x, "y": y, "button": "right"}));
    assert!(d.has("timeline.tracks.menu.addTracks"), "Add Tracks… is offered");
    assert!(d.has("timeline.tracks.menu.deleteTracks"));
    assert!(!d.has(&format!("timeline.track.V{n}.menu.addTrack")), "not a track's menu");
    d.ok("ui.click", json!({"id": "timeline.tracks.menu.addTracks"}));
    d.frames(3);
    assert!(d.harness.state().dialog.is_some(), "the dialog opens");
}

/// A right-click on one of the header's buttons opens the same menu, and the button keeps its state.
#[test]
fn a_right_click_on_a_header_button_opens_the_menu() {
    let mut d = Driver::demo();
    let before = serde_json::to_value(d.harness.state().session.active_sequence().unwrap().video_tracks[0].clone()).unwrap();
    for (label, button) in [("V1", "sourcePatch"), ("V1", "locked"), ("V1", "target"), ("V1", "syncLock"), ("V1", "enabled"), ("A1", "muted"), ("A1", "solo")] {
        let r = d.expect_rect(&format!("timeline.track.{label}.{button}"));
        d.ok("ui.click", json!({"x": r[0] + r[2] / 2.0, "y": r[1] + r[3] / 2.0, "button": "right"}));
        assert!(d.has(&format!("timeline.track.{label}.menu.addTrack")), "a right-click on {button} opens the menu");
        d.ok("ui.key", json!({"key": "Escape"}));
        d.frames(2);
        assert!(!d.has(&format!("timeline.track.{label}.menu.addTrack")), "Escape closes it");
    }
    let q = d.harness.state().session.active_sequence().unwrap();
    assert_eq!(serde_json::to_value(q.video_tracks[0].clone()).unwrap(), before, "no button toggled");
    assert!(!q.audio_tracks[0].muted && !q.audio_tracks[0].solo);
}
