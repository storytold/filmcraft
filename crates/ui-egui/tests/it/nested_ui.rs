//! Headless UI tests of nested sequence clips in the Timeline.

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
    fn new() -> Self {
        let mut s = Session::default();
        s.execute("file.openDemoProject", json!({})).unwrap();
        let (tx, rx) = channel();
        let app = FilmcraftApp::new(s).with_control(rx);
        let harness = Harness::builder().with_size(egui::vec2(1600.0, 980.0)).with_max_steps(10_000).build_eframe(move |_cc| app);
        let mut d = Driver { harness, tx };
        d.frames(4);
        d
    }

    fn frames(&mut self, n: usize) {
        for _ in 0..n {
            // the control channel's clicks and keys enter through the input hook
            let ctx = self.harness.ctx.clone();
            let mut raw = std::mem::take(self.harness.input_mut());
            eframe::App::raw_input_hook(self.harness.state_mut(), &ctx, &mut raw);
            *self.harness.input_mut() = raw;
            self.harness.step();
        }
    }

    fn call(&mut self, method: &str, params: Value) -> Value {
        let (req, reply) = ControlRequest::new(method, params.clone());
        self.tx.send(req).unwrap();
        for _ in 0..600 {
            self.frames(1);
            if let Ok(v) = reply.try_recv() {
                return v;
            }
        }
        panic!("no reply to {method} {params}");
    }

    fn ok(&mut self, method: &str, params: Value) -> Value {
        let v = self.call(method, params.clone());
        assert_eq!(v["ok"], json!(true), "{method} {params} failed: {v}");
        v["result"].clone()
    }

    fn exec(&mut self, command: &str, params: Value) -> Value {
        self.ok("engine.execute", json!({"command": command, "params": params}))
    }

    fn app(&mut self) -> &mut FilmcraftApp {
        self.harness.state_mut()
    }
}

impl Driver {
    /// (x, y, width, height) of an element, if it is on screen.
    fn rect(&mut self, id: &str) -> Option<[f64; 4]> {
        let v = self.ok("ui.elements", json!({"prefix": id}));
        let e = v.as_array().unwrap().iter().find(|e| e["id"] == json!(id))?.clone();
        let r = &e["rect"];
        Some([r[0].as_f64().unwrap(), r[1].as_f64().unwrap(), r[2].as_f64().unwrap(), r[3].as_f64().unwrap()])
    }
}

/// As in Premiere, a nest keeps its length when its sequence gets shorter, and the
/// Timeline hatches the part that is now past the contents.
#[test]
fn the_empty_end_of_a_nest_is_marked_in_the_timeline() {
    let mut d = Driver::new();
    let outer = d.app().session.state.active_sequence.unwrap().0;
    let (a, b) = {
        let v1 = &d.app().session.active_sequence().unwrap().video_tracks[0];
        (v1.items[3].clone(), v1.items[4].clone())
    };
    d.exec("timeline.select", json!({"clips": [a.id.0, b.id.0]}));
    let r = d.exec("clip.nest", json!({"name": "Inner"}));
    let (nested, nest) = (r["sequence"].as_u64().unwrap(), r["clips"][0].as_u64().unwrap());
    d.frames(3);
    let clip = d.rect(&format!("timeline.clip.{nest}")).expect("the nest clip is on screen");
    assert!(d.rect(&format!("timeline.clip.{nest}.empty")).is_none(), "a full nest has no empty part");
    // remove the second clip inside the nest
    d.exec("sequence.open", json!({"item": nested}));
    d.exec("timeline.select", json!({"clips": [b.id.0]}));
    d.exec("edit.clear", json!({}));
    d.exec("sequence.open", json!({"item": outer}));
    d.frames(3);
    let clip2 = d.rect(&format!("timeline.clip.{nest}")).expect("the nest clip is on screen");
    assert!((clip2[2] - clip[2]).abs() < 1.0, "the nest is as long as before");
    let empty = d.rect(&format!("timeline.clip.{nest}.empty")).expect("the empty part is marked");
    // it covers the removed clip's share of the nest, at its right end
    let share = b.duration.0 as f64 / (a.duration.0 + b.duration.0) as f64;
    assert!((empty[2] - clip2[2] * share).abs() < 2.0, "{empty:?} of {clip2:?}, share {share}");
    assert!((empty[0] + empty[2] - (clip2[0] + clip2[2])).abs() < 1.0, "it ends where the clip ends");
}
