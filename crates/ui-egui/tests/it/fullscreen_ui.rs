//! Full screen (#4933): the header button and `window.toggleFullScreen` must alternate. The
//! windowing system only reports the state from its last configure, which on Wayland can lag
//! or never arrive for a state the app itself requested, so the toggle cannot derive the next
//! state from that report alone: two toggles in a row must send enter, then leave.

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
        self.tx.send(req).expect("control channel");
        for _ in 0..600 {
            self.frames(1);
            if let Ok(r) = reply.try_recv() {
                assert_eq!(r["ok"], true, "{method} {params}: {r}");
                return r["result"].clone();
            }
        }
        panic!("no reply to {method} {params}");
    }
    /// The app sent `cmd` to the window in the last frame.
    fn sent(&self, cmd: &egui::ViewportCommand) -> bool {
        self.harness.output().viewport_output.get(&egui::ViewportId::ROOT).is_some_and(|v| v.commands.contains(cmd))
    }
    fn rect(&mut self, id: &str) -> egui::Rect {
        let v = self.ok("ui.elements", json!({"prefix": id}));
        let e = v.as_array().unwrap().iter().find(|e| e["id"] == id).unwrap_or_else(|| panic!("no element {id}")).clone();
        let r: Vec<f32> = e["rect"].as_array().unwrap().iter().map(|x| x.as_f64().unwrap() as f32).collect();
        egui::Rect::from_min_size(egui::pos2(r[0], r[1]), egui::vec2(r[2], r[3]))
    }
    fn send(&mut self, e: egui::Event) {
        self.harness.input_mut().events.push(e);
        self.frames(1);
    }
    /// A real pointer click on the element, so the frame that handles the release is the
    /// frame whose output `sent` reads.
    fn click(&mut self, id: &str) {
        let at = self.rect(id).center();
        let button = |pressed| egui::Event::PointerButton { pos: at, button: egui::PointerButton::Primary, pressed, modifiers: Default::default() };
        self.send(egui::Event::PointerMoved(at));
        self.send(button(true));
        self.send(button(false));
    }
}

#[test]
fn the_header_button_enters_then_leaves_full_screen() {
    let mut d = Driver::demo();
    d.click("header.fullscreen");
    assert!(d.sent(&egui::ViewportCommand::Fullscreen(true)), "the first click enters full screen");
    d.click("header.fullscreen");
    assert!(d.sent(&egui::ViewportCommand::Fullscreen(false)), "the second click leaves full screen");
}

#[test]
fn the_toggle_command_enters_then_leaves_full_screen() {
    let mut d = Driver::demo();
    let r = d.ok("ui.menu.invoke", json!({"id": "window.toggleFullScreen"}));
    assert_eq!(r["fullScreen"], true, "the first toggle enters");
    let r = d.ok("ui.menu.invoke", json!({"id": "window.toggleFullScreen"}));
    assert_eq!(r["fullScreen"], false, "the second toggle leaves");
}
