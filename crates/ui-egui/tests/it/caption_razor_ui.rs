//! The Razor tool (C) on a caption in the Timeline cuts it, as it cuts clips. The real
//! `FilmcraftApp` under `egui_kittest` with the demo project.

use std::sync::mpsc::{Sender, channel};

use egui::{Modifiers, Pos2, Rect, pos2};
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

    /// (start, end, text) of every caption on C1.
    fn captions(&mut self) -> Vec<(i64, i64, String)> {
        let r = self.exec("captions.list", json!({}));
        r["tracks"][0]["captions"]
            .as_array()
            .expect("captions")
            .iter()
            .map(|c| (c["start"].as_i64().expect("start"), c["end"].as_i64().expect("end"), c["text"].as_str().expect("text").to_string()))
            .collect()
    }

    /// Press at `from`, move to `to` over `steps` frames with `mods` held, release.
    fn drag_with(&mut self, from: Pos2, to: Pos2, steps: usize, mods: Modifiers) {
        let push = |d: &mut Self, e: egui::Event| d.harness.input_mut().events.push(e);
        push(self, egui::Event::ModifiersChanged(mods));
        push(self, egui::Event::PointerMoved(from));
        self.frames(1);
        push(self, egui::Event::PointerButton { pos: from, button: egui::PointerButton::Primary, pressed: true, modifiers: mods });
        self.frames(1);
        for i in 1..=steps {
            push(self, egui::Event::PointerMoved(from + (to - from) * (i as f32 / steps as f32)));
            self.frames(1);
        }
        push(self, egui::Event::PointerButton { pos: to, button: egui::PointerButton::Primary, pressed: false, modifiers: mods });
        self.frames(2);
        push(self, egui::Event::ModifiersChanged(Modifiers::NONE));
        self.frames(1);
    }

    fn click(&mut self, at: Pos2) {
        self.drag_with(at, at, 0, Modifiers::NONE);
    }
}

/// With the Razor tool (C), a click on a caption in the Timeline cuts it in two there. The
/// caption blocks took the click for selecting and moving whatever the tool, so it did nothing.
#[test]
fn razor_tool_splits_a_caption() {
    let mut d = Driver::demo();
    let id = d.exec("captions.add", json!({"seconds": 2.0, "durationSeconds": 4.0, "text": "cut me"}))["caption"].as_u64().expect("id");
    d.ok("ui.set", json!({"tool": "razor"}));
    let r = d.rect(&format!("timeline.caption.{id}")).expect("caption block");
    d.click(r.center());
    let caps = d.captions();
    assert_eq!(caps.len(), 2, "the caption was cut: {caps:?}");
    assert_eq!(caps[0].1, caps[1].0, "the halves meet at the cut");
    assert!(caps.iter().all(|c| c.2 == "cut me"));
}
