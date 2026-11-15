//! #4603: with several clips selected in the Timeline (a box selection), dragging Position in the
//! Properties panel moves every selected video clip by the same amount, as one undo step, not only
//! the clip the panel shows.

use std::sync::mpsc::{Sender, channel};

use egui_kittest::Harness;
use filmcraft_engine::Session;
use filmcraft_project::{ClipId, ParamValue};
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
                self.frames(2);
                return r["result"].clone();
            }
        }
        panic!("no reply to {method} {params}");
    }
    fn rect(&mut self, id: &str) -> [f32; 4] {
        let v = self.ok("ui.elements", json!({"prefix": id}));
        let e = v.as_array().expect("elements").iter().find(|e| e["id"] == id).unwrap_or_else(|| panic!("no element {id}")).clone();
        let r: Vec<f32> = e["rect"].as_array().expect("rect").iter().map(|x| x.as_f64().expect("number") as f32).collect();
        [r[0], r[1], r[2], r[3]]
    }
    fn send(&mut self, e: egui::Event) {
        self.harness.input_mut().events.push(e);
        self.frames(1);
    }
    fn drag(&mut self, from: egui::Pos2, to: egui::Pos2) {
        let button = |pos, pressed| egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed, modifiers: Default::default() };
        self.send(egui::Event::PointerMoved(from));
        self.send(button(from, true));
        for k in 1..=12 {
            self.send(egui::Event::PointerMoved(from.lerp(to, k as f32 / 12.0)));
        }
        self.send(button(to, false));
        self.frames(4);
    }
    /// The Motion Position of each clip.
    fn positions(&self, clips: &[u64]) -> Vec<(f64, f64)> {
        let s = &self.harness.state().session;
        let seq = s.active_sequence().expect("sequence");
        clips
            .iter()
            .map(|c| {
                let (_, it) = seq.find_item(ClipId(*c)).expect("clip");
                let p = it.effect("motion").and_then(|e| e.param("position")).expect("position");
                assert!(p.keyframes.is_empty(), "the demo's Position is static");
                match p.value {
                    ParamValue::Vec2(v) => (v.x, v.y),
                    ref other => panic!("Position is a point, got {other:?}"),
                }
            })
            .collect()
    }
}

#[test]
fn a_position_drag_in_properties_moves_every_selected_clip() {
    let mut session = Session::default();
    session.execute("file.openDemoProject", json!({})).expect("demo project");
    let (tx, rx) = channel();
    let app = FilmcraftApp::new(session).with_control(rx);
    let harness = Harness::builder().with_size(egui::vec2(1600.0, 980.0)).with_max_steps(10_000).build_eframe(move |_cc| app);
    let mut d = Driver { harness, tx };
    d.frames(4);
    let seq = d.ok("engine.execute", json!({"command": "sequence.inspect", "params": {}}));
    let clips: Vec<u64> = seq["video"]
        .as_array()
        .expect("video tracks")
        .iter()
        .flat_map(|t| t["items"].as_array().into_iter().flatten())
        .filter_map(|i| i["clip"].as_u64())
        .take(3)
        .collect();
    assert!(clips.len() >= 2, "the demo has several video clips: {clips:?}");
    // what a box selection over them leaves selected
    d.ok("engine.execute", json!({"command": "timeline.select", "params": {"clips": clips}}));
    d.ok("ui.panel.show", json!({"panel": "Properties"}));
    d.frames(4);
    let before = d.positions(&clips);
    let undo = d.harness.state().session.history.undo.len();
    // Position X: the first value in the Position row, which starts 150 points into the row; the
    // row's keyframe button gives its height
    let nav = d.rect("properties.motion.position.addKeyframe");
    let panel = d.rect("panel.Properties");
    let from = egui::pos2(panel[0] + 14.0 + 150.0 + 12.0, nav[1] + nav[3] / 2.0);
    d.drag(from, from + egui::vec2(40.0, 0.0));
    let after = d.positions(&clips);
    let dx = after[0].0 - before[0].0;
    assert!(dx.abs() > 1.0, "the drag moved the shown clip: {before:?} -> {after:?}");
    for (i, (b, a)) in before.iter().zip(&after).enumerate() {
        assert!((a.0 - b.0 - dx).abs() < 1e-6, "clip {} moved by the same X: {before:?} -> {after:?}", clips[i]);
        assert!((a.1 - b.1).abs() < 1e-6, "clip {} kept its Y: {before:?} -> {after:?}", clips[i]);
    }
    assert_eq!(d.harness.state().session.history.undo.len(), undo + 1, "the drag is one undo step");
    d.ok("engine.execute", json!({"command": "edit.undo", "params": {}}));
    assert_eq!(d.positions(&clips), before, "one undo puts every clip back");
}
