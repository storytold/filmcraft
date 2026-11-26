//! Headless UI tests of the Timeline's sequence tabs, the clip context menu and the Nest… name
//! dialog.

use crate::support::{Drive, Launch, builder, demo_session, driver};
use filmcraft_engine::project::ItemKind;
use serde_json::json;

driver! {
    Driver;
    const CLICK_SETTLE: usize = 3;
}

impl Driver {
    fn new() -> Self {
        Self::open(demo_session(), builder().with_step_dt(1.0 / 60.0))
    }
    fn v1(&mut self, i: usize) -> filmcraft_engine::project::TrackItem {
        self.app().session.active_sequence().unwrap().video_tracks[0].items[i].clone()
    }
}

impl Driver {
    fn sequences(&mut self) -> Vec<(u64, String)> {
        self.app().session.project.items.values().filter(|i| matches!(i.kind, ItemKind::Sequence(_))).map(|i| (i.id.0, i.name.clone())).collect()
    }

    fn has_timeline(&mut self) -> bool {
        self.app().ui.dock.contains(&filmcraft_ui_egui::dock::PanelKind::Timeline)
    }
}

#[test]
fn nest_asks_for_a_name_and_numbers_the_default() {
    let mut d = Driver::new();
    let main = d.app().session.state.active_sequence.unwrap();
    let a = d.v1(0);
    d.exec("timeline.select", json!({"clips": [a.id.0]}));
    assert_eq!(d.menu("clip.nest")["dialog"], "nest");
    assert_eq!(d.app().ui.clip_dialog.as_ref().unwrap().params["name"], "Nested Sequence 01");
    assert!(d.has("nest.name") && d.has("nest.cancel"));
    d.click("nest.ok");
    assert!(d.app().ui.clip_dialog.is_none(), "closed on OK");
    assert!(d.sequences().iter().any(|s| s.1 == "Nested Sequence 01"));
    // like Premiere, nesting stays in the sequence being edited
    assert_eq!(d.app().session.state.active_sequence, Some(main));
    // the next one is numbered on; Cancel nests nothing
    let b = d.v1(1);
    d.exec("timeline.select", json!({"clips": [b.id.0]}));
    d.menu("clip.nest");
    assert_eq!(d.app().ui.clip_dialog.as_ref().unwrap().params["name"], "Nested Sequence 02");
    d.click("nest.cancel");
    assert!(!d.sequences().iter().any(|s| s.1 == "Nested Sequence 02"));
    // without a name the command still picks an unused one
    d.exec("clip.nest", json!({"name": ""}));
    assert!(d.sequences().iter().any(|s| s.1 == "Nested Sequence 02"));
}

#[test]
fn clip_context_menu_offers_nest_and_make_subsequence() {
    let mut d = Driver::new();
    let a = d.v1(0);
    d.ok("ui.click", json!({"id": format!("timeline.clip.{}", a.id.0), "button": "right"}));
    d.frames(3);
    assert!(d.app().session.state.selection.contains(&a.id), "right-click selects the clip");
    for id in ["edit.cut", "edit.pasteAttributes", "clip.enable", "clip.group", "clip.nest", "sequence.makeSubsequence", "clip.frameHoldOptions"] {
        assert!(d.has(&format!("timeline.clipMenu.{id}")), "{id}");
    }
    d.click("timeline.clipMenu.clip.nest");
    assert_eq!(d.app().ui.clip_dialog.as_ref().map(|c| c.command.clone()).as_deref(), Some("clip.nest"), "Nest… asks for the name");
    d.click("nest.cancel");
    let before = d.sequences().len();
    d.ok("ui.click", json!({"id": format!("timeline.clip.{}", a.id.0), "button": "right"}));
    d.frames(3);
    d.click("timeline.clipMenu.sequence.makeSubsequence");
    assert_eq!(d.sequences().len(), before + 1);
}

#[test]
fn timeline_has_a_tab_per_open_sequence() {
    let mut d = Driver::new();
    let main = d.app().session.state.active_sequence.unwrap().0;
    let a = d.v1(0);
    d.exec("timeline.select", json!({"clips": [a.id.0]}));
    let nested = d.exec("clip.nest", json!({"name": "Inner"}))["sequence"].as_u64().unwrap();
    assert!(d.has(&format!("timeline.tab.{main}")));
    assert!(!d.has(&format!("timeline.tab.{nested}")), "not opened until asked");
    // double-clicking the nested clip opens it in a second tab
    let clip = {
        let q = d.app().session.active_sequence().unwrap();
        q.video_tracks[0].items.iter().find(|i| i.item.0 == nested).unwrap().id.0
    };
    d.ok("ui.click", json!({"id": format!("timeline.clip.{clip}"), "count": 2}));
    d.frames(4);
    assert_eq!(d.app().session.state.active_sequence.map(|i| i.0), Some(nested));
    assert!(d.has(&format!("timeline.tab.{main}")) && d.has(&format!("timeline.tab.{nested}")));
    // tabs switch sequences; × closes one and the panel stays
    d.click(&format!("timeline.tab.{main}"));
    assert_eq!(d.app().session.state.active_sequence.map(|i| i.0), Some(main));
    d.click(&format!("timeline.tab.{main}.close"));
    assert_eq!(d.app().session.state.active_sequence.map(|i| i.0), Some(nested));
    assert!(!d.has(&format!("timeline.tab.{main}")));
    d.click(&format!("timeline.tab.{nested}.close"));
    assert_eq!(d.app().session.state.active_sequence, None);
    assert!(d.has_timeline(), "closing the last sequence keeps the Timeline panel");
    assert!(d.has("panel.tab.Timeline"));
    // reopening from the project brings it back, even if the panel itself was closed
    d.app().ui.dock.apply(craft_behavior::dock::Action::close(filmcraft_ui_egui::dock::PanelKind::Timeline)).unwrap();
    d.frames(2);
    assert!(!d.has_timeline());
    d.exec("sequence.open", json!({"item": main}));
    d.frames(3);
    assert!(d.has_timeline());
    assert!(d.has(&format!("timeline.tab.{main}")));
}
