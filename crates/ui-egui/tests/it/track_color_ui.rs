//! Headless UI test of track colour labels (#1186): the strip at the left edge of a track header
//! opens the colour menu; the label tints the header and the lane and is an undoable track setting.
//!
//! Set `FILMCRAFT_UI_SNAPSHOT_DIR=<dir>` to also render offscreen with wgpu and write `track-color-*.png`.

#![allow(dead_code)]

use crate::support::{Drive, Launch, builder, demo_session, driver, with_snapshots};
use serde_json::{Value, json};

driver!(Driver);

impl Driver {
    fn demo() -> Self {
        Self::open(demo_session(), with_snapshots(builder()))
    }
}

fn label(d: &mut Driver, kind: &str, index: usize) -> Value {
    d.exec("sequence.inspect", json!({}))[kind][index]["label"].clone()
}

#[test]
fn the_track_header_strip_sets_a_colour_label() {
    let mut d = Driver::demo();
    assert_eq!(label(&mut d, "video", 0), json!(null));
    let ids = d.ids("timeline.track.");
    for id in ["timeline.track.V1.color", "timeline.track.A1.color"] {
        assert!(ids.iter().any(|i| i == id), "{id} missing from {ids:?}");
    }
    // the strip sits left of the source patch, inside the header
    let strip = d.element("timeline.track.V1.color").unwrap();
    let patch = d.element("timeline.track.V1.sourcePatch").unwrap();
    assert!(strip[0] + strip[2] <= patch[0], "strip {strip:?} overlaps the source patch {patch:?}");
    d.click("timeline.track.V1.color");
    let menu = d.ids("timeline.track.V1.color.");
    assert!(menu.iter().any(|i| i == "timeline.track.V1.color.Forest") && menu.iter().any(|i| i == "timeline.track.V1.color.None"), "{menu:?}");
    d.click("timeline.track.V1.color.Forest");
    assert_eq!(label(&mut d, "video", 0), json!("Forest"));
    assert!(!d.ids("timeline.track.V1.color.").iter().any(|i| i == "timeline.track.V1.color.Forest"), "the menu closes");
    // audio tracks too
    d.click("timeline.track.A1.color");
    d.click("timeline.track.A1.color.Mango");
    assert_eq!(label(&mut d, "audio", 0), json!("Mango"));
    d.snapshot("track-color-labels", None);
    // None clears; undo brings it back
    d.click("timeline.track.V1.color");
    d.click("timeline.track.V1.color.None");
    assert_eq!(label(&mut d, "video", 0), json!(null));
    d.exec("edit.undo", json!({}));
    assert_eq!(label(&mut d, "video", 0), json!("Forest"));
}
