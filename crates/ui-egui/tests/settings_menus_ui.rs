//! The settings menus (the Timeline's and the monitors' wrench) stay open while options are
//! toggled, and close on a choice, an action or a click outside.

use std::sync::mpsc::{Sender, channel};

use egui_kittest::Harness;
use filmcraft_engine::Session;
use filmcraft_ui_egui::FilmcraftApp;
use filmcraft_ui_egui::control::ControlRequest;
use serde_json::{Value, json};

struct Driver {
    harness: Harness<'static, FilmcraftApp>,
    tx: Sender<ControlRequest>,
}

impl Driver {
    fn demo() -> Self {
        let mut session = Session::default();
        session.execute("file.openDemoProject", json!({})).expect("demo project");
        let (tx, rx) = channel();
        let app = FilmcraftApp::new(session).with_control(rx);
        let harness = Harness::builder().with_size(egui::vec2(1600.0, 980.0)).with_max_steps(10_000).build_eframe(move |_cc| app);
        let mut d = Driver { harness, tx };
        d.frames(4);
        d
    }
    fn frames(&mut self, n: usize) {
        for _ in 0..n {
            let ctx = self.harness.ctx.clone();
            let mut raw = std::mem::take(self.harness.input_mut());
            eframe::App::raw_input_hook(self.harness.state_mut(), &ctx, &mut raw);
            *self.harness.input_mut() = raw;
            self.harness.step();
        }
    }
    fn ok(&mut self, method: &str, params: Value) -> Value {
        let (req, reply) = ControlRequest::new(method, params.clone());
        self.tx.send(req).unwrap();
        for _ in 0..600 {
            self.frames(1);
            if let Ok(r) = reply.try_recv() {
                assert_eq!(r["ok"], true, "{method} {params}: {r}");
                self.frames(2);
                return r["result"].clone();
            }
        }
        panic!("no reply to {method} {params}");
    }
}

fn has(d: &mut Driver, id: &str) -> bool {
    let v = d.ok("ui.elements", json!({"prefix": id}));
    v.as_array().unwrap().iter().any(|e| e["id"] == id)
}

#[test]
fn the_timeline_settings_menu_stays_open_while_options_are_toggled() {
    let mut d = Driver::demo();
    d.ok("ui.click", json!({"id": "timeline.toggle.settings"}));
    d.frames(2);
    assert!(has(&mut d, "timeline.settings.showThumbnails"), "the menu opened");
    let before = d.harness.state().ui.timeline.show_thumbnails;
    d.ok("ui.click", json!({"id": "timeline.settings.showThumbnails"}));
    d.frames(2);
    assert_ne!(d.harness.state().ui.timeline.show_thumbnails, before, "the toggle took");
    assert!(has(&mut d, "timeline.settings.showThroughEdits"), "and the menu is still open");
    d.ok("ui.click", json!({"id": "timeline.settings.showThroughEdits"}));
    d.frames(2);
    assert!(has(&mut d, "timeline.settings.showThumbnails"), "still open after a second toggle");
    // a click outside closes it
    d.ok("ui.click", json!({"id": "timeline.ruler", "fx": 0.9, "fy": 0.5}));
    d.frames(3);
    assert!(!has(&mut d, "timeline.settings.showThumbnails"), "a click away closes it");
}

#[test]
fn the_monitor_wrench_stays_open_for_toggles_and_closes_on_a_display_choice() {
    let mut d = Driver::demo();
    d.ok("ui.click", json!({"id": "program.settings"}));
    d.frames(2);
    assert!(has(&mut d, "program.settings.showRulers"), "the menu opened");
    d.ok("ui.click", json!({"id": "program.settings.showRulers"}));
    d.frames(2);
    assert!(has(&mut d, "program.settings.showGuides"), "a toggle leaves it open");
    d.ok("ui.click", json!({"id": "program.settings.display.composite"}));
    d.frames(3);
    assert!(!has(&mut d, "program.settings.showGuides"), "a display mode is a choice and closes it");
}
