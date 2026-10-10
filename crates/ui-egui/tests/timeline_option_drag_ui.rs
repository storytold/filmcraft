//! Option-drag of a timeline clip copies it (Premiere): the original stays, a copy lands where
//! the pointer lets go and is selected; a plain drag still moves.

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
                assert_eq!(v["ok"], json!(true), "{method} {params} failed: {v}");
                return v["result"].clone();
            }
        }
        panic!("no reply to {method} {params}");
    }
    fn exec(&mut self, command: &str, params: Value) -> Value {
        self.ok("engine.execute", json!({"command": command, "params": params}))
    }
    fn rect(&mut self, id: &str) -> [f64; 4] {
        let v = self.ok("ui.elements", json!({"prefix": id}));
        let e = v.as_array().unwrap().iter().find(|e| e["id"] == id).unwrap_or_else(|| panic!("no element {id}")).clone();
        let r: Vec<f64> = e["rect"].as_array().unwrap().iter().map(|x| x.as_f64().unwrap()).collect();
        [r[0], r[1], r[2], r[3]]
    }
}

fn clip_count(d: &mut Driver) -> usize {
    d.harness.state().session.active_sequence().map(|q| q.all_tracks().map(|t| t.items.len()).sum()).unwrap_or(0)
}

#[test]
fn option_drag_copies_a_clip_and_a_plain_drag_moves_it() {
    let mut d = Driver::demo();
    // the clip on V2 (no linked sound), and empty space to its right
    let q = d.harness.state().session.active_sequence().unwrap().clone();
    let v2 = q.video_tracks[1].items[0].clone();
    let n = clip_count(&mut d);
    let r = d.rect(&format!("timeline.clip.{}", v2.id.0));
    let (x, y) = (r[0] + 10.0, r[1] + r[3] / 2.0);
    d.ok("ui.drag", json!({"from": {"x": x, "y": y}, "to": {"x": x + 240.0, "y": y}, "steps": 12, "modifiers": {"alt": true}}));
    d.frames(3);
    let q2 = d.harness.state().session.active_sequence().unwrap().clone();
    assert_eq!(clip_count(&mut d), n + 1, "one copy was added");
    assert_eq!(q2.find_item(v2.id).map(|(_, i)| i.start), Some(v2.start), "the original stays");
    let sel = d.harness.state().session.state.selection.clone();
    assert_eq!(sel.len(), 1);
    let (track, copy) = q2.find_item(sel[0]).unwrap();
    assert_ne!(copy.id, v2.id);
    assert_eq!((track, copy.item, copy.duration), (q.video_tracks[1].id, v2.item, v2.duration));
    assert!(copy.start > v2.start, "the copy is where the pointer let go");
    // one undo removes the copy
    d.exec("edit.undo", json!({}));
    assert_eq!(clip_count(&mut d), n);
    // without Option the same drag moves the clip
    d.ok("ui.drag", json!({"from": {"x": x, "y": y}, "to": {"x": x + 240.0, "y": y}, "steps": 12}));
    d.frames(3);
    assert_eq!(clip_count(&mut d), n);
    assert!(d.harness.state().session.active_sequence().unwrap().find_item(v2.id).unwrap().1.start > v2.start, "moved");
}
