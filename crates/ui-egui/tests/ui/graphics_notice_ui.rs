//! The fallback graphics notice (#820): shown while the desktop app runs in a fallback graphics
//! mode, its button saves the default mode for the next start, and it can be closed.

use std::cell::Cell;
use std::rc::Rc;
use std::sync::mpsc::{Sender, channel};

use egui_kittest::Harness;
use filmcraft_engine::Session;
use filmcraft_ui_egui::FilmcraftApp;
use filmcraft_ui_egui::control::ControlRequest;
use filmcraft_ui_egui::graphics_notice::GraphicsNotice;
use serde_json::{Value, json};

struct Driver {
    harness: Harness<'static, FilmcraftApp>,
    tx: Sender<ControlRequest>,
}

impl Driver {
    fn new(notice: Option<GraphicsNotice>) -> Self {
        let (tx, rx) = channel();
        let mut app = FilmcraftApp::new(Session::default()).with_control(rx);
        app.graphics_notice = notice;
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
    fn ids(&mut self) -> Vec<String> {
        let v = self.call("ui.elements", json!({"prefix": "graphics.notice"}));
        assert_eq!(v["ok"], json!(true), "{v}");
        v["result"].as_array().unwrap().iter().map(|e| e["id"].as_str().unwrap().to_string()).collect()
    }
    fn label(&mut self, id: &str) -> String {
        let v = self.call("ui.elements", json!({"prefix": id}));
        v["result"].as_array().unwrap().iter().find(|e| e["id"] == id).map(|e| e["label"].as_str().unwrap().to_string()).unwrap_or_default()
    }
    fn click(&mut self, id: &str) {
        let v = self.call("ui.click", json!({"id": id}));
        assert_eq!(v["ok"], json!(true), "{id}: {v}");
        self.frames(3);
    }
    fn app(&mut self) -> &mut FilmcraftApp {
        self.harness.state_mut()
    }
}

#[test]
fn no_notice_in_the_default_graphics_mode() {
    let mut d = Driver::new(None);
    assert!(d.ids().is_empty());
}

#[test]
fn the_reset_button_saves_the_default_mode_once_and_close_hides_the_notice() {
    let resets = Rc::new(Cell::new(0));
    let counter = Rc::clone(&resets);
    let notice = GraphicsNotice::new(
        "software rendering",
        Box::new(move || {
            counter.set(counter.get() + 1);
            Ok(())
        }),
    );
    let mut d = Driver::new(Some(notice));
    let ids = d.ids();
    for id in ["graphics.notice", "graphics.notice.reset", "graphics.notice.close"] {
        assert!(ids.iter().any(|i| i == id), "{id} in {ids:?}");
    }
    assert!(d.label("graphics.notice").contains("software rendering"));
    d.click("graphics.notice.reset");
    assert_eq!(resets.get(), 1);
    assert!(d.app().graphics_notice.as_ref().is_some_and(|n| n.reset_done));
    // done: the button is gone, the notice says when it takes effect
    let ids = d.ids();
    assert!(!ids.iter().any(|i| i == "graphics.notice.reset"), "{ids:?}");
    assert!(ids.iter().any(|i| i == "graphics.notice"));
    d.click("graphics.notice.close");
    assert!(d.ids().is_empty());
    assert_eq!(resets.get(), 1);
}

#[test]
fn a_reset_that_cannot_be_saved_says_why_and_keeps_the_button() {
    let notice = GraphicsNotice::new("DirectX 12 only", Box::new(|| Err("can't write graphics-start.json: read-only".into())));
    let mut d = Driver::new(Some(notice));
    d.click("graphics.notice.reset");
    assert!(d.app().ui.status.contains("read-only"), "{}", d.app().ui.status);
    assert!(d.app().graphics_notice.as_ref().is_some_and(|n| !n.reset_done));
    assert!(d.ids().iter().any(|i| i == "graphics.notice.reset"));
}

#[test]
fn the_notice_follows_the_interface_language() {
    let notice = GraphicsNotice::new("software rendering", Box::new(|| Ok(())));
    let mut d = Driver::new(Some(notice));
    d.app().ui.language = filmcraft_ui_egui::i18n::Language::Es;
    d.frames(3);
    assert_eq!(d.label("graphics.notice"), "Modo gráfico seguro: procesamiento por software");
    assert_eq!(d.label("graphics.notice.reset"), "Usar gráficos predeterminados");
    filmcraft_ui_egui::i18n::set_current(filmcraft_ui_egui::i18n::Language::En);
}
