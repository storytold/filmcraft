//! Headless UI test of deleting caption tracks: the caption track header's right-click menu
//! (Delete Caption Track / Delete Empty Caption Tracks) and the Caption Tracks section of
//! Sequence ▸ Delete Tracks….

use crate::support::{Drive, Launch, driver};
use serde_json::json;

driver!(Driver);

impl Driver {
    /// Caption track names top first, with their caption counts.
    fn caption_tracks(&mut self) -> Vec<(String, usize)> {
        let r = self.exec("captions.list", json!({}));
        r["tracks"].as_array().unwrap().iter().map(|t| (t["name"].as_str().unwrap().to_string(), t["captions"].as_array().unwrap().len())).collect()
    }

    fn right_click_menu(&mut self, track: &str, item: &str) {
        self.ok("ui.click", json!({"id": format!("timeline.captionTrack.{track}"), "button": "right"}));
        self.frames(3);
        self.ok("ui.click", json!({"id": format!("timeline.captionTrack.{track}.menu.{item}")}));
        self.frames(3);
    }
}

fn setup() -> Driver {
    let mut d = Driver::demo();
    // start from no caption tracks, then add four (each goes on top): Keep (with a caption), E1–E3 empty
    while !d.caption_tracks().is_empty() {
        d.exec("captions.deleteTrack", json!({"track": "C1"}));
    }
    for name in ["E3", "E2", "Keep", "E1"] {
        d.exec("captions.newTrack", json!({"format": "Subtitle", "name": name}));
    }
    d.exec("captions.add", json!({"track": "C2", "text": "Hello", "seconds": 1.0}));
    d.frames(3);
    assert_eq!(d.caption_tracks(), [("E1".into(), 0), ("Keep".into(), 1), ("E2".into(), 0), ("E3".into(), 0)]);
    d
}

#[test]
fn header_menu_deletes_one_or_all_empty_caption_tracks() {
    let mut d = setup();
    d.right_click_menu("C1", "delete");
    assert_eq!(d.caption_tracks(), [("Keep".into(), 1), ("E2".into(), 0), ("E3".into(), 0)]);
    // from the non-empty track's menu: only the empty ones go
    d.right_click_menu("C1", "deleteEmpty");
    assert_eq!(d.caption_tracks(), [("Keep".into(), 1)]);
    // the last caption track can be deleted too, captions and all; undo brings it back
    d.right_click_menu("C1", "delete");
    assert!(d.caption_tracks().is_empty());
    assert!(d.ids("timeline.captionTrack.").is_empty(), "no caption rows left");
    d.exec("edit.undo", json!({}));
    d.frames(3);
    assert_eq!(d.caption_tracks(), [("Keep".into(), 1)]);
}

#[test]
fn delete_tracks_dialog_has_a_caption_section() {
    let mut d = setup();
    d.ok("ui.menu.invoke", json!({"command": "sequence.deleteTracks", "params": {}}));
    d.frames(3);
    d.ok("ui.click", json!({"id": "deleteTracks.captions"}));
    d.frames(2);
    // one specific track: C3 (E2)
    d.ok("ui.click", json!({"id": "deleteTracks.captions.target"}));
    d.frames(3);
    d.ok("ui.click", json!({"id": "deleteTracks.captions.option.C3"}));
    d.frames(2);
    d.ok("ui.click", json!({"id": "deleteTracks.ok"}));
    d.frames(3);
    assert_eq!(d.caption_tracks(), [("E1".into(), 0), ("Keep".into(), 1), ("E3".into(), 0)]);
    // All Empty Tracks (the default choice)
    d.ok("ui.menu.invoke", json!({"command": "sequence.deleteTracks", "params": {}}));
    d.frames(3);
    d.ok("ui.click", json!({"id": "deleteTracks.captions"}));
    d.frames(2);
    d.ok("ui.click", json!({"id": "deleteTracks.ok"}));
    d.frames(3);
    assert_eq!(d.caption_tracks(), [("Keep".into(), 1)]);
}
