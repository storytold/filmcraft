//! A transition on a compact track fills the track's height: below the clip's name strip on a
//! tall track, from its top on a short one, so it can be read and grabbed.

use crate::support::{Drive, Launch, driver};
use serde_json::json;

driver! {
    Driver;
    fn after_ok(&mut self) {
        self.frames(2);
    }
}

#[test]
fn a_transition_fills_a_compact_track_and_sits_below_the_name_on_a_tall_one() {
    let mut d = Driver::demo();
    let id = d.harness.state().session.active_sequence().unwrap().video_tracks[0].transitions[0].id.0;
    let (name, row) = (format!("timeline.transition.{id}"), "timeline.track.V1.sourcePatch");
    // tall: a strip for the clip's name above it
    d.ok("ui.set", json!({"timeline": {"videoTrackHeight": 96.0}}));
    d.frames(4);
    let (t, r) = (d.expect_rect(&name), d.expect_rect(row));
    assert!(t[1] - r[1] >= 15.0, "tall: the transition starts below the name strip ({t:?} in {r:?})");
    // compact: it fills the track
    d.ok("ui.set", json!({"timeline": {"videoTrackHeight": 30.0}}));
    d.frames(4);
    let (t, r) = (d.expect_rect(&name), d.expect_rect(row));
    assert!(t[1] - r[1] <= 2.0, "compact: it starts at the top of the track ({t:?} in {r:?})");
    assert!(t[3] >= r[3] - 3.0, "and is as tall as the track: {} of {}", t[3], r[3]);
    // and a press on its upper part grabs the transition, not the clip under it
    let hit = d.ok("ui.timeline.hit", json!({"x": t[0] + t[2] / 2.0, "y": t[1] + 4.0}));
    assert_eq!(hit["hit"], "transition", "{hit}");
}
