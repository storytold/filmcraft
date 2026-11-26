//! The settings menus (the Timeline's and the monitors' wrench) stay open while options are
//! toggled, and close on a choice, an action or a click outside.

use crate::support::{Drive, Launch, driver};
use egui_kittest::kittest::Queryable;
use filmcraft_ui_egui::state::PlaybackRes;
use serde_json::json;

driver! {
    Driver;
    fn after_ok(&mut self) {
        self.frames(2);
    }
}

#[test]
fn the_timeline_settings_menu_stays_open_while_options_are_toggled() {
    let mut d = Driver::demo();
    d.ok("ui.click", json!({"id": "timeline.toggle.settings"}));
    d.frames(2);
    assert!(d.has("timeline.settings.showThumbnails"), "the menu opened");
    let before = d.harness.state().ui.timeline.show_thumbnails;
    d.ok("ui.click", json!({"id": "timeline.settings.showThumbnails"}));
    d.frames(2);
    assert_ne!(d.harness.state().ui.timeline.show_thumbnails, before, "the toggle took");
    assert!(d.has("timeline.settings.showThroughEdits"), "and the menu is still open");
    d.ok("ui.click", json!({"id": "timeline.settings.showThroughEdits"}));
    d.frames(2);
    assert!(d.has("timeline.settings.showThumbnails"), "still open after a second toggle");
    // a click outside closes it
    d.ok("ui.click", json!({"id": "timeline.ruler", "fx": 0.9, "fy": 0.5}));
    d.frames(3);
    assert!(!d.has("timeline.settings.showThumbnails"), "a click away closes it");
}

#[test]
fn the_track_height_actions_set_both_heights_and_close_the_timeline_menu() {
    let mut d = Driver::demo();
    for (action, height) in [("Expand All Tracks", 64.0), ("Minimize All Tracks", 26.0)] {
        d.ok("ui.click", json!({"id": "timeline.toggle.settings"}));
        d.frames(2);
        assert!(d.has("timeline.settings.showThumbnails"), "the menu opened for {action}");
        d.harness.get_by_label(action).click();
        d.frames(3);
        let timeline = &d.harness.state().ui.timeline;
        assert_eq!((timeline.video_track_h, timeline.audio_track_h), (height, height), "{action}");
        assert!(!d.has("timeline.settings.showThumbnails"), "{action} closes the menu");
    }
}

#[test]
fn the_monitor_wrench_stays_open_for_toggles_and_closes_on_a_display_choice() {
    let mut d = Driver::demo();
    d.ok("ui.click", json!({"id": "program.settings"}));
    d.frames(2);
    assert!(d.has("program.settings.showRulers"), "the menu opened");
    d.ok("ui.click", json!({"id": "program.settings.showRulers"}));
    d.frames(2);
    assert!(d.has("program.settings.showGuides"), "a toggle leaves it open");
    d.ok("ui.click", json!({"id": "program.settings.display.composite"}));
    d.frames(3);
    assert!(!d.has("program.settings.showGuides"), "a display mode is a choice and closes it");
}

#[test]
fn a_resolution_choice_and_clear_guides_close_each_monitors_wrench() {
    for monitor in ["program", "source"] {
        let mut d = Driver::demo();
        d.ok("ui.menu.invoke", json!({"id": "source.open", "params": {"item": 5}}));
        let view = |d: &Driver| {
            let ui = &d.harness.state().ui;
            if monitor == "program" { ui.program.clone() } else { ui.source.clone() }
        };
        for (submenu, key, name) in [("Playback Resolution", "playbackRes.quarter", "playback"), ("Paused Resolution", "pausedRes.eighth", "paused")] {
            d.ok("ui.click", json!({"id": format!("{monitor}.settings")}));
            d.frames(2);
            d.harness.get_by_label(submenu).click();
            d.frames(3);
            d.ok("ui.click", json!({"id": format!("{monitor}.settings.{key}")}));
            d.frames(3);
            let v = view(&d);
            let (got, want) = if name == "playback" { (v.res, PlaybackRes::Quarter) } else { (v.paused_res, PlaybackRes::Eighth) };
            assert_eq!(got, want, "{monitor} {name} resolution");
            assert!(!d.has(&format!("{monitor}.settings.showGuides")), "{monitor}: a {name} resolution closes the menu");
        }
        // Clear Guides is only offered while guides exist
        d.ok("ui.menu.invoke", json!({"id": "view.addGuide", "params": {"monitor": monitor, "position": 0.5}}));
        d.ok("ui.click", json!({"id": format!("{monitor}.settings")}));
        d.frames(2);
        assert!(d.has(&format!("{monitor}.settings.clearGuides")), "{monitor}: Clear Guides is offered with a guide");
        d.ok("ui.click", json!({"id": format!("{monitor}.settings.clearGuides")}));
        d.frames(3);
        assert!(view(&d).guides.is_empty(), "{monitor}: the guides are gone");
        assert!(!d.has(&format!("{monitor}.settings.showGuides")), "{monitor}: Clear Guides closes the menu");
    }
}
