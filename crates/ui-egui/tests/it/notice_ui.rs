//! The Nightly session-health notice: posted inside the window (never a modal), listed for
//! automation, and dismissed by Close.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Sender, channel};

use egui_kittest::Harness;
use filmcraft_engine::Session;
use filmcraft_ui_egui::FilmcraftApp;
use filmcraft_ui_egui::control::ControlRequest;
use filmcraft_ui_egui::notices::Notice;
use serde_json::{Value, json};

struct Driver {
    harness: Harness<'static, FilmcraftApp>,
    tx: Sender<ControlRequest>,
}

impl Driver {
    fn new(notice: Option<Notice>) -> Self {
        let (tx, rx) = channel();
        let mut app = FilmcraftApp::new(Session::default()).with_control(rx);
        app.notice = notice;
        // The window hosts the craft runtime now: a harness that drives frames without installing it
        // headless panics ("the runtime plugin's hooks did not run for the previous pass").
        let harness = Harness::builder().with_size(egui::vec2(1600.0, 980.0)).with_max_steps(10_000).build_eframe(move |cc| {
            filmcraft_ui_egui::framework::install_headless(&cc.egui_ctx);
            app
        });
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
        let v = self.call("ui.elements", json!({"prefix": "notice"}));
        assert_eq!(v["ok"], json!(true), "{v}");
        v["result"].as_array().unwrap().iter().map(|e| e["id"].as_str().unwrap().to_string()).collect()
    }
    fn click(&mut self, id: &str) {
        let v = self.call("ui.click", json!({"id": id}));
        assert_eq!(v["ok"], json!(true), "{id}: {v}");
        self.frames(3);
    }
}

#[test]
fn the_notice_acknowledges_its_record_only_once_it_is_drawn() {
    // The Nightly session-health record is cleared by this flag, so a window that closes before
    // the notice reached a kept pass keeps the record for the next launch.
    let drawn = Arc::new(AtomicBool::new(false));
    let mut notice = Notice::new("FilmCraft Nightly", vec!["The previous FilmCraft Nightly session did not finish normally.".into()]);
    notice.shown = Some(drawn.clone());
    let mut d = Driver::new(Some(notice));
    assert!(d.ids().iter().any(|id| id == "notice"));
    assert!(drawn.load(Ordering::Relaxed), "a drawn notice acknowledges the interruption it reports");

    let dismissed = Arc::new(AtomicBool::new(false));
    let mut notice = Notice::new("FilmCraft Nightly", vec!["The previous FilmCraft Nightly session did not finish normally.".into()]);
    notice.closed = true;
    notice.shown = Some(dismissed.clone());
    let _d = Driver::new(Some(notice));
    assert!(!dismissed.load(Ordering::Relaxed), "a dismissed notice reports nothing and clears nothing");
}

#[test]
fn no_notice_without_a_session_health_message() {
    let mut d = Driver::new(None);
    assert!(d.ids().is_empty());
}

#[test]
fn the_notice_is_shown_and_close_dismisses_it() {
    let mut d = Driver::new(Some(Notice::new("FilmCraft Nightly", vec!["The previous FilmCraft Nightly session did not finish normally.".into()])));
    let ids = d.ids();
    assert!(ids.iter().any(|id| id == "notice"), "{ids:?}");
    assert!(ids.iter().any(|id| id == "notice.close"), "{ids:?}");
    d.click("notice.close");
    assert!(d.ids().is_empty(), "Close dismisses the notice for this session");
}
