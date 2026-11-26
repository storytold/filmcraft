//! The Razor tool (C) on a caption in the Timeline cuts it, as it cuts clips. The real
//! `FilmcraftApp` under `egui_kittest` with the demo project.

use super::caption_option_drag_ui::Driver;
use crate::support::{Drive, Launch};
use egui::{Modifiers, Pos2};
use serde_json::json;

impl Driver {
    fn click_at(&mut self, at: Pos2) {
        self.drag_with(at, at, 0, Modifiers::NONE);
    }
}

/// With the Razor tool (C), a click on a caption in the Timeline cuts it in two there. The
/// caption blocks took the click for selecting and moving whatever the tool, so it did nothing.
#[test]
fn razor_tool_splits_a_caption() {
    let mut d = Driver::demo_snap_off();
    let id = d.exec("captions.add", json!({"seconds": 2.0, "durationSeconds": 4.0, "text": "cut me"}))["caption"].as_u64().expect("id");
    d.ok("ui.set", json!({"tool": "razor"}));
    let r = d.rect(&format!("timeline.caption.{id}")).expect("caption block");
    d.click_at(r.center());
    let caps = d.captions();
    assert_eq!(caps.len(), 2, "the caption was cut: {caps:?}");
    assert_eq!(caps[0].1, caps[1].0, "the halves meet at the cut");
    assert!(caps.iter().all(|c| c.2 == "cut me"));
}
