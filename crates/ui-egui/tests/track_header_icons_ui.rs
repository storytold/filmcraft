//! Headless UI tests of the Timeline's track header buttons: their icons are large enough to read
//! and still fit inside the header at the smallest track height.

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
        let harness = Harness::builder().with_size(egui::vec2(1600.0, 1400.0)).with_max_steps(10_000).build_eframe(move |_cc| app);
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

    fn rect(&mut self, id: &str) -> egui::Rect {
        let v = self.ok("ui.elements", json!({"prefix": id}));
        let e = v.as_array().unwrap().iter().find(|e| e["id"] == id).unwrap_or_else(|| panic!("no element {id}")).clone();
        let r: Vec<f32> = e["rect"].as_array().unwrap().iter().map(|x| x.as_f64().unwrap() as f32).collect();
        egui::Rect::from_min_size(egui::pos2(r[0], r[1]), egui::vec2(r[2], r[3]))
    }
}

/// The header buttons of a track, left to right, and the smallest side each must have.
fn buttons(kind: char) -> Vec<(&'static str, f32)> {
    if kind == 'V' {
        vec![("locked", 22.0), ("syncLock", 22.0), ("enabled", 22.0)]
    } else {
        vec![("locked", 22.0), ("syncLock", 22.0), ("muted", 16.0), ("solo", 16.0), ("voiceover", 22.0), ("keyframes", 22.0)]
    }
}

#[test]
fn header_buttons_are_large_and_fit_the_smallest_track_height() {
    let mut d = Driver::demo();
    for height in [60.0, 22.0] {
        d.ok("ui.set", json!({"timeline": {"videoTrackHeight": height, "audioTrackHeight": height}}));
        d.frames(3);
        for track in ["V1", "A1"] {
            let header = d.rect(&format!("timeline.track.{track}.target")).expand2(egui::vec2(0.0, 2.0));
            let mut right = f32::MIN;
            for (name, side) in buttons(track.chars().next().unwrap()) {
                let r = d.rect(&format!("timeline.track.{track}.{name}"));
                assert!(r.width() >= side && r.height() >= side, "{track} {name} is {r:?} at height {height}");
                assert!(r.min.y >= header.min.y - 0.5 && r.max.y <= header.max.y + 0.5, "{track} {name} {r:?} leaves its header {header:?} at height {height}");
                assert!(name == "locked" || r.min.x >= right - 0.5, "{track} {name} overlaps the button before it");
                right = r.max.x;
            }
        }
    }
}
