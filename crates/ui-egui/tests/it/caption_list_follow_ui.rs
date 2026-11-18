//! The Text panel's Captions list keeps the caption under the playhead in view as the playhead
//! moves, so it stays lined up with the Timeline. The real `FilmcraftApp` under `egui_kittest`
//! with the demo project.

use std::sync::mpsc::{Sender, channel};

use egui::{Rect, pos2};
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
    /// The demo project with snapping off, so drags land where the pointer goes; frames 1/60 s
    /// apart, as on a display.
    fn demo() -> Self {
        let mut session = Session::default();
        session.execute("file.openDemoProject", json!({})).expect("demo project");
        session.execute("sequence.snap", json!({"on": false})).expect("snapping off");
        let (tx, rx) = channel();
        let app = FilmcraftApp::new(session).with_control(rx);
        let harness = Harness::builder().with_size(egui::vec2(1600.0, 980.0)).with_step_dt(1.0 / 60.0).with_max_steps(10_000).build_eframe(move |_cc| app);
        let mut d = Driver { harness, tx };
        d.frames(4);
        d.ok("ui.elements", json!({"prefix": "timeline.clip."}));
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
            if let Ok(v) = reply.try_recv() {
                assert_eq!(v["ok"], json!(true), "{method} {params} failed: {v}");
                return v["result"].clone();
            }
        }
        panic!("no reply to {method} {params}");
    }

    fn exec(&mut self, command: &str, params: Value) -> Value {
        self.ok("engine.execute", json!({"command": command, "params": params}))
    }

    fn rect(&mut self, id: &str) -> Option<Rect> {
        self.frames(2);
        let els = self.ok("ui.elements", json!({"prefix": id}));
        let el = els.as_array()?.iter().find(|e| e["id"] == id)?.clone();
        let r: Vec<f32> = el["rect"].as_array()?.iter().filter_map(|v| v.as_f64().map(|v| v as f32)).collect();
        Some(Rect::from_min_size(pos2(*r.first()?, *r.get(1)?), egui::vec2(*r.get(2)?, *r.get(3)?)))
    }
}

/// The Captions list keeps the caption under the playhead in view as the playhead moves on.
#[test]
fn captions_list_follows_the_playhead() {
    let mut d = Driver::demo();
    let mut last = 0;
    for i in 0..30 {
        last = d.exec("captions.add", json!({"seconds": i as f64, "durationSeconds": 0.5, "text": format!("word {i}")}))["caption"].as_u64().expect("id");
    }
    d.ok("ui.panel.show", json!({"panel": "Text"}));
    d.ok("ui.click", json!({"id": "text.tab.Captions"}));
    d.exec("playhead.set", json!({"seconds": 0.0}));
    let top = d.rect("text.captions.search").expect("search").max.y;
    let bottom = d.rect("text.captions.style.font").expect("style strip").min.y;
    let row = format!("text.captions.{last}.row");
    let before = d.rect(&row).expect("last row");
    assert!(before.center().y > bottom, "the last caption starts out of view: {before:?} below {bottom}");
    d.exec("playhead.set", json!({"seconds": 29.2}));
    d.frames(30);
    let after = d.rect(&row).expect("last row");
    assert!(after.center().y > top && after.center().y < bottom, "the playhead's caption is in view: {after:?} in {top}..{bottom}");
}
