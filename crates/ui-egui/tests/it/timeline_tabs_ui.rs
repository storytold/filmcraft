//! Headless UI tests of the Timeline's sequence tabs: every sequence keeps its own zoom, scroll
//! and track heights; a right-click on a tab opens its menu; tabs are reordered by dragging; tabs
//! that do not fit are in a list; a project reopens with its tabs. Premiere's behaviour was
//! observed in Premiere Pro 26.5.2.

use crate::support::{Drive, Launch, builder, demo_session, driver, save_snapshot, with_snapshots};
use filmcraft_engine::project::ItemId;
use serde_json::json;

driver! {
    Driver;
    const CLICK_SETTLE: usize = 3;
}

impl Driver {
    fn new() -> Self {
        <Self as Launch>::open(demo_session(), with_snapshots(builder()))
    }
}

impl Driver {
    fn snapshot(&mut self, name: &str) {
        save_snapshot(self, 3, &format!("{name}.png"));
    }

    fn rect(&mut self, id: &str) -> Option<[f64; 4]> {
        self.element_rect(id)
    }

    fn has(&mut self, id: &str) -> bool {
        self.rect(id).is_some()
    }

    fn new_sequence(&mut self, name: &str) -> u64 {
        let id = self.exec("file.newSequence", json!({"name": name}))["sequence"].as_u64().unwrap();
        self.frames(3);
        id
    }

    fn open(&mut self) -> Vec<u64> {
        self.app().session.state.open_sequences.iter().map(|i| i.0).collect()
    }

    fn active(&mut self) -> u64 {
        self.app().session.state.active_sequence.unwrap().0
    }

    /// (zoom, scroll, video track height) the Timeline panel is showing.
    fn view(&mut self) -> (f64, f64, f32) {
        let v = &self.app().ui.timeline;
        (v.target_pps, v.target_scroll, v.video_track_h)
    }

    fn show(&mut self, seq: u64) {
        self.exec("sequence.open", json!({"item": seq}));
        self.frames(3);
    }
}

#[test]
fn each_sequence_tab_keeps_its_own_zoom_scroll_and_track_heights() {
    let mut d = Driver::new();
    let main = d.active();
    d.ok("ui.set", json!({"timeline": {"pps": 333.0, "scroll": 4.5, "videoTrackHeight": 96.0}}));
    d.frames(3);
    assert_eq!(d.view(), (333.0, 4.5, 96.0));
    // a sequence shown for the first time is fitted, at the default track height
    let other = d.new_sequence("Other");
    let fitted = d.view();
    assert!(fitted.0 != 333.0 && fitted.1 == 0.0 && fitted.2 == 60.0, "{fitted:?}");
    d.ok("ui.set", json!({"timeline": {"pps": 20.0, "scroll": 1.0, "videoTrackHeight": 30.0}}));
    d.frames(3);
    // back and forth by clicking the tabs: each comes back as it was left
    d.click(&format!("timeline.tab.{main}"));
    assert_eq!(d.view(), (333.0, 4.5, 96.0));
    d.click(&format!("timeline.tab.{other}"));
    assert_eq!(d.view(), (20.0, 1.0, 30.0));
    // the session holds both (it is what a saved project keeps)
    let views = d.app().session.state.timeline_views.clone();
    assert_eq!((views[&ItemId(main)].pps, views[&ItemId(other)].pps), (333.0, 20.0));
    // closing the shown tab shows the other one with its view
    d.click(&format!("timeline.tab.{other}.close"));
    assert_eq!((d.active(), d.view()), (main, (333.0, 4.5, 96.0)));
    // a view changed in the session (a command, the control channel) is taken over by the panel
    d.app().session.state.timeline_views.get_mut(&ItemId(main)).unwrap().pps = 55.0;
    d.frames(2);
    assert_eq!(d.view().0, 55.0);
}

#[test]
fn a_right_click_on_a_tab_shows_it_and_opens_its_menu() {
    let mut d = Driver::new();
    let main = d.active();
    let (b, c) = (d.new_sequence("B"), d.new_sequence("C"));
    assert_eq!((d.open(), d.active()), (vec![main, b, c], c));
    // Premiere: a right-click on a tab that is not shown shows it and opens the panel menu
    d.ok("ui.click", json!({"id": format!("timeline.tab.{b}"), "button": "right"}));
    d.frames(3);
    assert_eq!(d.active(), b);
    assert!(d.has("panel.menu.Timeline.close") && d.has("panel.menu.Timeline.closeOthers") && d.has("panel.menu.Timeline.revealSequence"));
    d.snapshot("timeline-tab-menu");
    d.click("panel.menu.Timeline.closeOthers");
    assert_eq!((d.open(), d.active()), (vec![b], b));
    assert!(!d.has("panel.menu.Timeline.closeOthers"), "the menu closes");
    // the menu goes away on a click elsewhere, and on Escape
    d.show(main);
    d.ok("ui.click", json!({"id": format!("timeline.tab.{b}"), "button": "right"}));
    d.frames(3);
    assert!(d.has("panel.menu.Timeline.close"));
    d.click("panel.Program");
    assert!(!d.has("panel.menu.Timeline.close"), "a click elsewhere closes the menu");
    d.ok("ui.click", json!({"id": format!("timeline.tab.{main}"), "button": "right"}));
    d.frames(3);
    assert!(d.has("panel.menu.Timeline.close"));
    d.ok("ui.key", json!({"key": "Escape"}));
    d.frames(3);
    assert!(!d.has("panel.menu.Timeline.close"), "Escape closes the menu");
    // a right-click on another tab while the menu is open moves the menu to that tab
    d.ok("ui.click", json!({"id": format!("timeline.tab.{b}"), "button": "right"}));
    d.frames(3);
    d.ok("ui.click", json!({"id": format!("timeline.tab.{main}"), "button": "right"}));
    d.frames(3);
    assert!(d.has("panel.menu.Timeline.close") && d.active() == main);
    d.click("panel.Program");
    // Close Panel from the menu closes the tab it was opened on
    d.show(main);
    d.ok("ui.click", json!({"id": format!("timeline.tab.{b}"), "button": "right"}));
    d.frames(3);
    d.click("panel.menu.Timeline.close");
    assert_eq!((d.open(), d.active()), (vec![main], main));
}

#[test]
fn a_tab_dragged_past_its_neighbours_changes_places_with_them() {
    let mut d = Driver::new();
    let main = d.active();
    let (b, c) = (d.new_sequence("B"), d.new_sequence("C"));
    // the last tab to the left end
    d.ok("ui.drag", json!({"from": {"id": format!("timeline.tab.{c}")}, "to": {"id": format!("timeline.tab.{main}"), "fx": 0.1}, "steps": 12}));
    d.frames(4);
    assert_eq!(d.open(), [c, main, b]);
    assert_eq!(d.active(), c, "the dragged tab is the one shown");
    // and one place to the right again
    d.ok("ui.drag", json!({"from": {"id": format!("timeline.tab.{c}")}, "to": {"id": format!("timeline.tab.{main}"), "fx": 0.9}, "steps": 12}));
    d.frames(4);
    assert_eq!(d.open(), [main, c, b]);
    // a drag that does not reach the middle of a neighbour changes nothing; neither does a click
    let r = d.rect(&format!("timeline.tab.{c}")).unwrap();
    d.ok("ui.drag", json!({"from": {"x": r[0] + 20.0, "y": r[1] + r[3] / 2.0}, "to": {"x": r[0] + 30.0, "y": r[1] + r[3] / 2.0}, "steps": 4}));
    d.frames(4);
    d.click(&format!("timeline.tab.{b}"));
    assert_eq!((d.open(), d.active()), (vec![main, c, b], b));
}

#[test]
fn tabs_that_do_not_fit_are_reached_through_a_list() {
    let mut d = Driver::new();
    let main = d.active();
    assert!(!d.has("timeline.tabs.more"), "no list while every tab fits");
    let mut last = main;
    for i in 0..24 {
        last = d.new_sequence(&format!("A sequence with a long name {i:02}"));
    }
    // the shown tab is always in the strip; the first one no longer fits
    assert!(d.has(&format!("timeline.tab.{last}")) && !d.has(&format!("timeline.tab.{main}")));
    let strip = d.rect("panel.Timeline").unwrap();
    let tab = d.rect(&format!("timeline.tab.{last}")).unwrap();
    assert!(tab[0] >= strip[0] && tab[0] + tab[2] <= strip[0] + strip[2], "the shown tab is inside the panel: {tab:?} in {strip:?}");
    // the list has every open sequence; picking one shows it, and its tab
    d.click("timeline.tabs.more");
    assert!(d.has(&format!("timeline.tabs.list.{last}")));
    d.snapshot("timeline-tab-list");
    d.click(&format!("timeline.tabs.list.{main}"));
    assert_eq!(d.active(), main);
    assert!(d.has(&format!("timeline.tab.{main}")) && !d.has(&format!("timeline.tab.{last}")));
    assert!(!d.has(&format!("timeline.tabs.list.{main}")), "the list closes");
    // it also closes on a click elsewhere
    d.click("timeline.tabs.more");
    assert!(d.has(&format!("timeline.tabs.list.{last}")));
    d.click("panel.Program");
    assert!(!d.has(&format!("timeline.tabs.list.{last}")));
}

#[test]
fn a_reopened_project_shows_its_tabs_as_they_were_left() {
    let dir = std::env::temp_dir().join(format!("filmcraft-tabs-ui-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("tabs.fcproj").to_string_lossy().to_string();

    let mut d = Driver::new();
    let main = d.active();
    d.ok("ui.set", json!({"timeline": {"pps": 250.0, "scroll": 2.0, "videoTrackHeight": 80.0}}));
    d.frames(3);
    let other = d.new_sequence("Other");
    d.ok("ui.set", json!({"timeline": {"pps": 15.0, "scroll": 0.5}}));
    d.frames(3);
    d.exec("sequence.moveTab", json!({"item": other, "index": 0}));
    d.exec("file.saveAs", json!({"path": path}));

    // another window of the app opens the file
    let mut e = Driver::new();
    e.ok("ui.set", json!({"timeline": {"pps": 77.0, "scroll": 9.0}}));
    e.frames(3);
    e.exec("file.open", json!({"path": path}));
    e.frames(3);
    assert_eq!((e.open(), e.active()), (vec![other, main], other));
    assert_eq!(e.view(), (15.0, 0.5, 60.0));
    e.click(&format!("timeline.tab.{main}"));
    assert_eq!(e.view(), (250.0, 2.0, 80.0));
    let _ = std::fs::remove_dir_all(&dir);
}
