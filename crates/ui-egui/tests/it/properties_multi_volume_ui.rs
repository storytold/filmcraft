//! #3710: with several audio clips selected, dragging the Properties panel's Level changes every
//! one of them by the same amount (each keeps its own level), and the drag is one undo step.

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
    fn exec(&mut self, command: &str, params: Value) -> Value {
        self.ok("engine.execute", json!({"command": command, "params": params}))
    }
    /// The first audio track's clips: (id, start, end, Volume level).
    fn audio(&mut self) -> Vec<(u64, i64, i64, f64)> {
        let seq = self.exec("sequence.inspect", json!({}));
        let items = seq["audio"][0]["items"].as_array().cloned().unwrap_or_default();
        items
            .iter()
            .map(|i| {
                let level = i["effects"]
                    .as_array()
                    .and_then(|e| e.iter().find(|e| e["effect"] == "volume"))
                    .and_then(|e| e["params"]["level"]["value"].as_str())
                    .and_then(|v| v.trim_start_matches("Float(").trim_end_matches(')').parse::<f64>().ok())
                    .expect("a Volume level");
                (i["clip"].as_u64().unwrap(), i["start"].as_i64().unwrap(), i["end"].as_i64().unwrap(), level)
            })
            .collect()
    }
    fn element(&mut self, id: &str) -> Option<Value> {
        self.frames(2);
        let v = self.ok("ui.elements", json!({"prefix": id}));
        v.as_array()?.iter().find(|e| e["id"] == id).cloned()
    }
}

#[test]
fn properties_level_changes_every_selected_audio_clip() {
    let mut session = Session::default();
    session.execute("file.openDemoProject", json!({})).expect("demo project");
    let (tx, rx) = channel();
    let app = FilmcraftApp::new(session).with_control(rx);
    let harness = Harness::builder().with_size(egui::vec2(1600.0, 900.0)).with_max_steps(10_000).build_eframe(move |_cc| app);
    let mut d = Driver { harness, tx };
    d.frames(4);
    // three audio clips with different levels
    let (first, start, end, _) = d.audio()[0];
    let third = (end - start) / 3;
    d.exec("timeline.razor", json!({"clip": first, "time": start + third}));
    let second = d.audio()[1].0;
    d.exec("timeline.razor", json!({"clip": second, "time": start + 2 * third}));
    let ids: Vec<u64> = d.audio().iter().take(3).map(|c| c.0).collect();
    for (id, level) in ids.iter().zip([-6.0, 0.0, 3.0]) {
        d.exec("effects.setParam", json!({"clip": id, "effect": "volume", "param": "level", "value": level}));
    }
    let before: Vec<f64> = d.audio().iter().take(3).map(|c| c.3).collect();
    d.exec("timeline.select", json!({"clips": ids}));
    d.ok("ui.panel.show", json!({"panel": "Properties"}));
    d.frames(3);
    let group = d.element("properties.audioGroup").expect("the panel says the Level changes every selected audio clip");
    assert_eq!(group["label"], "3", "{group}");

    // the Audio section sits below the fold: scroll it into view, then drag the Level down
    d.ok("ui.scroll", json!({"id": "panel.Properties", "dy": -2000.0}));
    d.frames(3);
    let level = d.element("properties.volume").expect("the Level is laid out");
    let y = level["rect"][1].as_f64().unwrap();
    assert!(y > 0.0 && y < 880.0, "the Level is on screen: {level}");
    d.ok("ui.drag", json!({"from": {"id": "properties.volume"}, "to": {"id": "properties.volume", "fx": -1.5}, "steps": 12}));
    d.frames(3);
    let after: Vec<f64> = d.audio().iter().take(3).map(|c| c.3).collect();
    let deltas: Vec<f64> = after.iter().zip(&before).map(|(a, b)| a - b).collect();
    assert!(deltas[0] < -0.05, "the drag lowered the Level: {before:?} -> {after:?}");
    for d in &deltas {
        assert!((d - deltas[0]).abs() < 1e-9, "every clip moved by the same amount: {before:?} -> {after:?}");
    }

    // one undo step brings all three back
    d.exec("edit.undo", json!({}));
    let undone: Vec<f64> = d.audio().iter().take(3).map(|c| c.3).collect();
    assert_eq!(undone, before, "the drag is one undo step");
}
