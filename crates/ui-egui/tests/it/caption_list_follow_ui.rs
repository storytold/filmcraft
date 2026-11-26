//! The Text panel's Captions list keeps the caption under the playhead in view as the playhead
//! moves, so it stays lined up with the Timeline. The real `FilmcraftApp` under `egui_kittest`
//! with the demo project.

use super::caption_option_drag_ui::Driver;
use crate::support::{Drive, Launch};
use serde_json::json;

/// The Captions list keeps the caption under the playhead in view as the playhead moves on.
#[test]
fn captions_list_follows_the_playhead() {
    let mut d = Driver::demo_snap_off();
    let mut last = 0;
    for i in 0..30 {
        last = d.exec("captions.add", json!({"seconds": i as f64, "durationSeconds": 0.5, "text": format!("word {i}")}))["caption"].as_u64().expect("id");
    }
    d.ok("ui.panel.show", json!({"panel": "Text"}));
    d.ok("ui.click", json!({"id": "text.tab.Captions"}));
    d.exec("playhead.set", json!({"seconds": 0.0}));
    let top = d.rect("text.captions.search").expect("search").max.y;
    let bottom = d.rect("text.captions.style.font").expect("style strip").min.y;
    let row = format!("text.captions.{last}.row");
    let before = d.rect(&row).expect("last row");
    assert!(before.center().y > bottom, "the last caption starts out of view: {before:?} below {bottom}");
    d.exec("playhead.set", json!({"seconds": 29.2}));
    d.frames(30);
    let after = d.rect(&row).expect("last row");
    assert!(after.center().y > top && after.center().y < bottom, "the playhead's caption is in view: {after:?} in {top}..{bottom}");
}
