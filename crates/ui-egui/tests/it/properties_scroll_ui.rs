//! The Properties panel scrolls (#1322): with a short panel the Audio section and the speed button
//! sit below the fold, and the mouse wheel has to bring them into view.

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
    fn top(&mut self, id: &str) -> Option<f64> {
        self.frames(2);
        let v = self.ok("ui.elements", json!({"prefix": id}));
        let e = v.as_array()?.iter().find(|e| e["id"] == id)?;
        e["rect"].as_array()?.get(1)?.as_f64()
    }
}

#[test]
fn properties_panel_scrolls_to_its_lower_sections() {
    let mut session = Session::default();
    session.execute("file.openDemoProject", json!({})).expect("demo project");
    let (tx, rx) = channel();
    let app = FilmcraftApp::new(session).with_control(rx);
    let harness = Harness::builder().with_size(egui::vec2(1600.0, 560.0)).with_max_steps(10_000).build_eframe(move |_cc| app);
    let mut d = Driver { harness, tx };
    d.frames(4);
    let seq = d.ok("engine.execute", json!({"command": "sequence.inspect", "params": {}}));
    let clip = seq["video"][0]["items"][0]["clip"].as_u64().expect("first video clip");
    d.ok("engine.execute", json!({"command": "timeline.select", "params": {"clips": [clip]}}));
    d.ok("ui.panel.show", json!({"panel": "Properties"}));
    d.frames(3);
    let before = d.top("properties.speed").expect("the speed button is laid out");
    d.ok("ui.scroll", json!({"id": "panel.Properties", "dy": -2000.0}));
    d.frames(3);
    let after = d.top("properties.speed").expect("the speed button is still laid out");
    assert!(after < before - 20.0, "the wheel scrolls the panel: {before} -> {after}");
}
