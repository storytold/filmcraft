//! #1420: the monitor resolution renders the Program monitor's frame at that fraction of the
//! sequence's frame size, whatever the size of the monitor on screen (the monitor scales the
//! picture to fit). The frame used to be capped at the monitor's on-screen size, so on a small
//! monitor Full, 1/2 and often 1/4 all rendered the same frame.

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
            if let Ok(v) = reply.try_recv() {
                assert_eq!(v["ok"], json!(true), "{method} {params}: {v}");
                return v["result"].clone();
            }
        }
        panic!("no reply to {method}");
    }

    /// The size in pixels of the frame the Program monitor shows once it settles on `scale`
    /// (scale x 1000, as in the frame key).
    fn shown_size(&mut self, scale: u32) -> (usize, usize) {
        for _ in 0..2000 {
            self.frames(1);
            let app = self.harness.state();
            if let Some(k) = app.program_picture()
                && k.size == scale
                && let Some(f) = app.frames.get(&k)
            {
                return (f.w, f.h);
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        let shown = self.harness.state().program_picture();
        panic!("the Program monitor never showed a frame at scale {scale}: it shows {shown:?}");
    }
}

#[test]
fn paused_resolution_sets_the_rendered_frame_size() {
    let mut d = Driver::demo();
    let (w, h) = {
        let s = d.harness.state().session.active_sequence().unwrap();
        (s.settings.width as usize, s.settings.height as usize)
    };
    // the monitor is far narrower than the sequence: Full is not what fits on screen
    let monitor = d.ok("ui.elements", json!({"prefix": "program.picture"}));
    let shown_w =
        monitor.as_array().unwrap().iter().find(|e| e["id"] == "program.picture").map(|e| e["rect"][2].as_f64().unwrap() - e["rect"][0].as_f64().unwrap());
    assert!(shown_w.is_some_and(|sw| sw < w as f64 / 2.0), "the monitor shows the picture smaller than 1/2: {shown_w:?} for {w} px");

    d.ok("ui.menu.invoke", json!({"id": "view.pausedRes.full"}));
    assert_eq!(d.shown_size(1000), (w, h), "Full renders the sequence's frame size");
    d.ok("ui.menu.invoke", json!({"id": "view.pausedRes.half"}));
    assert_eq!(d.shown_size(500), (w / 2, h / 2), "1/2 renders half the width and height");
    d.ok("ui.menu.invoke", json!({"id": "view.pausedRes.quarter"}));
    assert_eq!(d.shown_size(250), (w / 4, h / 4), "1/4 renders a quarter of the width and height");
}
