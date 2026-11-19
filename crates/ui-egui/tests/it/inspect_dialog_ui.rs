//! `ui.inspect` reports a dialog an embedding app draws itself (Craft Link's prompt) in `dialog`.

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
    fn new(label: Option<&'static str>) -> Self {
        let (tx, rx) = channel();
        let mut app = FilmcraftApp::new(Session::default()).with_control(rx);
        app.hooks.dialog_label = label.map(|text| Box::new(move || Some(text.to_string())) as Box<dyn Fn() -> Option<String>>);
        let harness = Harness::builder().with_size(egui::vec2(1600.0, 980.0)).with_max_steps(10_000).build_eframe(move |_cc| app);
        let mut driver = Driver { harness, tx };
        driver.frames(4);
        driver
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

    fn call(&mut self, method: &str, params: Value) -> Value {
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
}

#[test]
fn inspect_names_the_dialog_the_embedding_app_shows() {
    let mut driver = Driver::new(Some("LinkNotInstalled"));
    let inspected = driver.call("ui.inspect", json!({}));
    assert_eq!(inspected["dialog"], json!("LinkNotInstalled"), "{inspected}");
}

#[test]
fn inspect_reports_no_dialog_when_none_is_shown() {
    let mut driver = Driver::new(None);
    let inspected = driver.call("ui.inspect", json!({}));
    assert_eq!(inspected["dialog"], Value::Null, "{inspected}");
}
