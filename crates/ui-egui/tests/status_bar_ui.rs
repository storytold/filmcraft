//! Regression test for #2295: status-bar text must be rendered as a selectable egui Label,
//! not painted text, so the user can click-drag to select and Ctrl+C to copy it.

use std::sync::mpsc::channel;

use egui_kittest::Harness;
use filmcraft_engine::Session;
use filmcraft_ui_egui::FilmcraftApp;
use filmcraft_ui_egui::control::ControlRequest;
use serde_json::{Value, json};

struct Driver {
    harness: Harness<'static, FilmcraftApp>,
    tx: std::sync::mpsc::Sender<ControlRequest>,
}

impl Driver {
    fn new() -> Self {
        filmcraft_ui_egui::crash::install(None);
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
}

/// The status bar must register a `status.text` automation element, proving it is a Label
/// widget (with selectable=true) rather than raw painted text (#2295).
#[test]
fn status_bar_text_is_selectable() {
    let mut d = Driver::new();
    // Post a status message so the element has non-empty content.
    d.harness.state_mut().ui.status = "Ready".into();
    d.frames(2);
    let els = d.ok("ui.elements", json!({"prefix": "status."}));
    let ids: Vec<String> = els.as_array().expect("ui.elements returned an array").iter().map(|v| v["id"].as_str().unwrap_or("").to_owned()).collect();
    assert!(ids.iter().any(|id| id == "status.text"), "status.text automation element not found; got: {ids:?}");
}
