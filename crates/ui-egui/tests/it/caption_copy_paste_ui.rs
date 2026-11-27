//! Cmd+C / Cmd+V with a caption selected in the Timeline pastes a copy at the playhead, as with
//! clips. The real `FilmcraftApp` under `egui_kittest` with the demo project.

use super::caption_option_drag_ui::Driver;
use crate::support::{Drive, Launch};
use serde_json::json;

/// Cmd+C / Cmd+V with a caption selected in the Timeline pastes a copy at the playhead.
#[test]
fn copy_paste_duplicates_a_caption() {
    let mut d = Driver::demo_snap_off();
    let id = d.exec("captions.add", json!({"seconds": 1.0, "durationSeconds": 1.0, "text": "again"}))["caption"].as_u64().expect("id");
    d.ok("ui.click", json!({"id": format!("timeline.caption.{id}")}));
    d.ok("ui.key", json!({"key": "Cmd+C"}));
    d.exec("playhead.set", json!({"seconds": 5.0}));
    d.ok("ui.key", json!({"key": "Cmd+V"}));
    let caps = d.captions();
    assert_eq!(caps.len(), 2, "pasted: {caps:?}");
    assert_eq!(caps[1].2, "again");
    assert!(caps[1].0 > caps[0].1);
}
