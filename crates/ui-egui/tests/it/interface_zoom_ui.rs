//! Headless UI tests of the interface zoom (#2253).

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
        let harness = Harness::builder().with_size(egui::vec2(1600.0, 980.0)).with_step_dt(1.0 / 60.0).with_max_steps(10_000).build_eframe(move |_cc| app);
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
            if let Ok(v) = reply.try_recv() {
                assert_eq!(v["ok"], json!(true), "{method} {params} failed: {v}");
                return v["result"].clone();
            }
        }
        panic!("no reply to {method} {params}");
    }

    fn click(&mut self, id: &str) {
        self.ok("ui.click", json!({"id": id}));
        self.frames(2);
    }

    fn app(&mut self) -> &mut FilmcraftApp {
        self.harness.state_mut()
    }

    /// One key press with `modifiers`, as the window reports it.
    fn press(&mut self, key: egui::Key, modifiers: egui::Modifiers) {
        let events = &mut self.harness.input_mut().events;
        events.push(egui::Event::ModifiersChanged(modifiers));
        for pressed in [true, false] {
            events.push(egui::Event::Key { key, physical_key: None, pressed, repeat: false, modifiers });
        }
        events.push(egui::Event::ModifiersChanged(egui::Modifiers::NONE));
        self.frames(3);
    }
}

/// #2253: with the Timeline focused, Ctrl+= and Ctrl+- are its track-height keys, but Ctrl+Plus
/// (the `+` key, or Shift+=) fell through to egui and scaled the whole interface, which the taken
/// Ctrl+- and Ctrl+0 could not undo. `+` now goes where `=` goes, and View ▸ Reset Interface Zoom
/// gives the configured scale back.
#[test]
fn ctrl_plus_does_what_ctrl_equals_does_and_the_interface_zoom_can_be_reset() {
    let mut d = Driver::demo();
    let clip = d.app().session.active_sequence().unwrap().video_tracks[0].items[0].id.0;
    d.click(&format!("timeline.clip.{clip}"));
    assert_eq!(d.app().ui.focused, filmcraft_ui_egui::dock::PanelKind::Timeline);
    let zoom = d.harness.ctx.zoom_factor();
    let height = d.app().ui.timeline.video_track_h;
    d.press(egui::Key::Plus, egui::Modifiers::COMMAND);
    assert_eq!(d.harness.ctx.zoom_factor(), zoom, "the interface keeps its scale");
    assert!(d.app().ui.timeline.video_track_h > height, "Ctrl+Plus makes the video tracks taller, as Ctrl+= does");

    // however it got scaled, View ▸ Reset Interface Zoom brings it back
    d.harness.ctx.set_zoom_factor(1.75);
    d.frames(2);
    assert_eq!(d.harness.ctx.zoom_factor(), 1.75);
    let r = d.ok("ui.menu.invoke", json!({"id": "view.resetInterfaceZoom"}));
    assert_eq!(r["zoom"], json!(1.0), "the reply says the zoom it set: {r}");
    d.frames(2);
    assert_eq!(d.harness.ctx.zoom_factor(), 1.0);
}
