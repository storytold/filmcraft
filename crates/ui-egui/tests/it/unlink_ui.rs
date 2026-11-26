//! Headless UI test of Unlink from the timeline clip menu (#220): after unlinking, the clip that
//! was right-clicked is the only one selected, so a drag moves it away from its former partner.

use crate::support::{Drive, Launch, driver};
use filmcraft_engine::project::{ClipId, TrackItem};
use serde_json::json;

driver!(Driver);

impl Driver {
    fn item(&mut self, c: ClipId) -> TrackItem {
        self.app().session.active_sequence().unwrap().find_item(c).unwrap().1.clone()
    }

    fn selection(&mut self) -> Vec<ClipId> {
        let mut v = self.app().session.state.selection.clone();
        v.sort_by_key(|c| c.0);
        v
    }

    /// Right-click `clip` on the timeline and choose Link / Unlink from its menu.
    fn menu_link(&mut self, clip: ClipId) {
        self.ok("ui.click", json!({"id": format!("timeline.clip.{}", clip.0), "button": "right"}));
        self.frames(3);
        self.ok("ui.click", json!({"id": "timeline.clipMenu.clip.link"}));
        self.frames(3);
    }

    /// A linked picture on V1 and its sound.
    fn linked_pair(&mut self) -> (ClipId, ClipId) {
        let seq = self.app().session.active_sequence().unwrap().clone();
        let v = seq.video_tracks[0].items.iter().find(|i| i.link.is_some()).expect("a linked clip");
        let a = seq.audio_tracks.iter().flat_map(|t| &t.items).find(|i| i.link == v.link).expect("its sound");
        (v.id, a.id)
    }
}

#[test]
fn unlinked_picture_drags_away_from_its_sound() {
    let mut d = Driver::demo();
    let (v, a) = d.linked_pair();
    let (v0, a0) = (d.item(v).start, d.item(a).start);
    d.menu_link(v);
    assert_eq!((d.item(v).link, d.item(a).link), (None, None), "unlinked");
    assert_eq!(d.selection(), [v], "only the right-clicked clip stays selected");
    // dragging the picture straight away leaves the sound where it was
    let id = format!("timeline.clip.{}", v.0);
    d.ok("ui.drag", json!({"from": {"id": id, "fx": 0.3}, "to": {"id": id, "fx": 0.6}, "steps": 12}));
    d.frames(4);
    assert_ne!(d.item(v).start, v0, "the picture moved");
    assert_eq!(d.item(a).start, a0, "its sound stayed");
}

#[test]
fn unlinking_from_the_sound_keeps_the_sound_selected_and_link_keeps_both() {
    let mut d = Driver::demo();
    let (v, a) = d.linked_pair();
    d.menu_link(a);
    assert_eq!(d.selection(), [a]);
    // Link again: select both, link from the menu; the selection is left alone
    d.ok("engine.execute", json!({"command": "timeline.select", "params": {"clips": [v.0, a.0]}}));
    d.menu_link(v);
    assert!(d.item(v).link.is_some() && d.item(v).link == d.item(a).link, "linked again");
    let mut both = vec![v, a];
    both.sort_by_key(|c| c.0);
    assert_eq!(d.selection(), both);
}
