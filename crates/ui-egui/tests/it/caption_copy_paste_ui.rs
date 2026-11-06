//! Cmd+C / Cmd+V with a caption selected in the Timeline pastes a copy at the playhead, as with
//! clips. The real `FilmcraftApp` under `egui_kittest` with the demo project.

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
}

/// Cmd+C / Cmd+V with a caption selected in the Timeline pastes a copy at the playhead.
#[test]
fn copy_paste_duplicates_a_caption() {
    let mut d = Driver::demo();
    let id = d.exec("captions.add", json!({"seconds": 1.0, "durationSeconds": 1.0, "text": "again"}))["caption"].as_u64().expect("id");
    d.ok("ui.click", json!({"id": format!("timeline.caption.{id}")}));
    d.ok("ui.key", json!({"key": "Cmd+C"}));
    d.exec("playhead.set", json!({"seconds": 5.0}));
    d.ok("ui.key", json!({"key": "Cmd+V"}));
    let caps = d.captions();
    assert_eq!(caps.len(), 2, "pasted: {caps:?}");
    assert_eq!(caps[1].2, "again");
    assert!(caps[1].0 > caps[0].1);
}
