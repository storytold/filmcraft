//! The corner-roundness handle of rectangle / polygon masks on the Program monitor, driven
//! headless through the control channel: dragging it rounds the corners in one undo step; masks
//! without sharp corners show no handle.
//!
//! With `FILMCRAFT_UI_SHOTS=<dir>` the test also renders the UI with wgpu and writes PNGs there.

use super::masks_ui::Driver;
use crate::support::{Drive, Launch, builder, demo_session, with_shots};
use serde_json::{Value, json};

impl Driver {
    /// The demo project, frames at the harness's default step.
    fn demo_default_step() -> Self {
        Self::open(demo_session(), with_shots(builder()))
    }
}

fn vertex_count(m: &Value) -> usize {
    m["path"]["vertices"].as_array().map_or(0, Vec::len)
}

#[test]
fn roundness_handle_rounds_mask_corners_in_one_undo_step() {
    let mut d = Driver::demo_default_step();
    d.ok("ui.set", json!({"workspace": "Effects"}));
    let seq = d.exec("sequence.inspect", json!({}));
    let clip = &seq["video"][0]["items"][0];
    let (id, start, dur) = (clip["clip"].as_u64().unwrap(), clip["start"].as_i64().unwrap(), clip["duration"].as_i64().unwrap());
    d.exec("playhead.set", json!({"time": start + dur / 2}));
    d.exec("timeline.select", json!({"clips": [id]}));

    // an ellipse has no sharp corner: no roundness handle
    d.exec("masks.add", json!({"clip": id, "effect": "opacity", "shape": "ellipse"}));
    d.exec("masks.select", json!({"clip": id, "effect": "opacity", "mask": 0}));
    d.frames(3);
    assert!(d.has("program.mask.vertex.0"), "the mask overlay is on the monitor");
    assert!(!d.has("program.mask.roundness"), "no roundness handle on an ellipse");

    // a rectangle has one, just inside its top-left corner
    d.exec("masks.add", json!({"clip": id, "effect": "opacity", "shape": "rectangle", "center": [960, 540], "size": [960, 540]}));
    d.exec("masks.select", json!({"clip": id, "effect": "opacity", "mask": 1}));
    d.frames(3);
    assert!(d.has("program.mask.roundness"));
    let corner = d.center("program.mask.vertex.0");
    let handle = d.center("program.mask.roundness");
    assert!(handle.0 > corner.0 && handle.1 > corner.1, "handle {handle:?} inside the top-left corner {corner:?}");
    assert_eq!(vertex_count(&d.masks()[1]), 4);
    d.shot("mask-roundness-handle");

    // a click without a drag changes nothing
    let undo0 = d.exec("history.list", json!({}))["undo"].as_array().unwrap().len();
    d.ok("ui.click", json!({"id": "program.mask.roundness"}));
    d.frames(2);
    assert_eq!(vertex_count(&d.masks()[1]), 4);
    assert_eq!(d.exec("history.list", json!({}))["undo"].as_array().unwrap().len(), undo0);

    // dragging inward rounds all four corners: eight vertices, one undo step
    d.drag("program.mask.roundness", 30.0, 30.0);
    let m = d.masks();
    assert_eq!(vertex_count(&m[1]), 8, "{}", m[1]["path"]);
    let undo1 = d.exec("history.list", json!({}))["undo"].as_array().unwrap().len();
    assert_eq!(undo1, undo0 + 1, "one undo step per drag");
    // the first fillet starts below the old top-left corner (960 − 480, 540 − 270)
    let p = &m[1]["path"]["vertices"][0]["p"];
    let (x, y) = (p[0].as_f64().unwrap(), p[1].as_f64().unwrap());
    assert!((x - 480.0).abs() < 1e-6 && y > 270.0 + 10.0, "fillet start ({x}, {y})");
    d.shot("mask-rounded-corners");
    // the rounded corners are no longer sharp: the handle is gone
    d.frames(2);
    assert!(!d.has("program.mask.roundness"));

    d.exec("edit.undo", json!({}));
    d.frames(2);
    assert_eq!(vertex_count(&d.masks()[1]), 4);
    assert!(d.has("program.mask.roundness"));
}
