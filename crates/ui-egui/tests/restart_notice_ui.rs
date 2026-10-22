//! The language-change restart notice: choosing another interface language keeps the running
//! interface as it is and offers Restart (closes the window; the host relaunches the app on exit)
//! and Later (the change still applies at the next start). Unsaved changes ask to save first, as
//! with Quit.

use std::cell::Cell;
use std::rc::Rc;
use std::sync::mpsc::{Sender, channel};

use egui_kittest::Harness;
use filmcraft_engine::Session;
use filmcraft_ui_egui::FilmcraftApp;
use filmcraft_ui_egui::control::ControlRequest;
use filmcraft_ui_egui::i18n::Language;
use serde_json::{Value, json};

struct Driver {
    harness: Harness<'static, FilmcraftApp>,
    tx: Sender<ControlRequest>,
}

impl Driver {
    fn new() -> Self {
        Self::with_session(|_| {})
    }
    /// `setup` adjusts the session before the app is built (e.g. open the demo project).
    fn with_session(setup: impl FnOnce(&mut Session)) -> Self {
        let mut session = Session::default();
        setup(&mut session);
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
    fn ids(&mut self) -> Vec<String> {
        let v = self.ok("ui.elements", json!({"prefix": "language.restart"}));
        v.as_array().unwrap().iter().map(|e| e["id"].as_str().unwrap().to_string()).collect()
    }
    fn label(&mut self, id: &str) -> String {
        let v = self.ok("ui.elements", json!({"prefix": id}));
        v.as_array().unwrap().iter().find(|e| e["id"] == id).map(|e| e["label"].as_str().unwrap().to_string()).unwrap_or_default()
    }
    fn click(&mut self, id: &str) {
        self.ok("ui.click", json!({"id": id}));
        self.frames(3);
    }
    fn app(&mut self) -> &mut FilmcraftApp {
        self.harness.state_mut()
    }
    /// The app sent `cmd` to the window in the last frame.
    fn sent(&self, cmd: &egui::ViewportCommand) -> bool {
        self.harness.output().viewport_output.get(&egui::ViewportId::ROOT).is_some_and(|v| v.commands.contains(cmd))
    }
}

#[test]
fn a_language_change_keeps_the_interface_and_asks_to_restart() {
    let mut d = Driver::new();
    assert!(d.ids().is_empty(), "no change, no notice");
    d.ok("ui.menu.invoke", json!({"id": "app.language.spanish"}));
    d.frames(3);
    assert_eq!(d.app().ui.language, Language::En, "the running interface stays as it is");
    assert_eq!(d.app().session.prefs.general.interface_language, "es");
    assert_eq!(d.app().restart_notice.as_ref().map(|n| n.language), Some(Language::Es));
    for id in ["language.restart", "language.restart.restart", "language.restart.later"] {
        assert!(d.ids().iter().any(|i| i == id), "{id} in {:?}", d.ids());
    }
    assert_eq!(d.label("language.restart"), "Restart FilmCraft to use Español");
    filmcraft_ui_egui::i18n::set_current(Language::En);
}

#[test]
fn later_dismisses_the_notice_and_switching_back_clears_it() {
    let mut d = Driver::new();
    d.ok("ui.menu.invoke", json!({"id": "app.language.german"}));
    d.frames(3);
    d.click("language.restart.later");
    assert!(d.ids().is_empty(), "Later closes the notice");
    assert!(d.app().restart_notice.as_ref().is_some_and(|n| n.closed));
    // another change asks again
    d.exec("prefs.set", json!({"key": "general.interfaceLanguage", "value": "fr"}));
    d.frames(3);
    assert!(d.ids().iter().any(|i| i == "language.restart.restart"));
    // choosing the running language back makes a restart pointless
    d.exec("prefs.set", json!({"key": "general.interfaceLanguage", "value": "en"}));
    d.frames(3);
    assert!(d.app().restart_notice.is_none());
    assert!(d.ids().is_empty());
}

#[test]
fn restart_closes_the_window_and_the_host_relaunches_on_exit() {
    let mut d = Driver::new();
    let relaunches = Rc::new(Cell::new(0));
    let count = Rc::clone(&relaunches);
    d.app().hooks.restart = Some(Box::new(move || {
        count.set(count.get() + 1);
        Ok(())
    }));
    d.ok("ui.menu.invoke", json!({"id": "app.language.spanish"}));
    d.frames(3);
    let ctx = d.harness.ctx.clone();
    d.app().begin_restart(&ctx);
    d.frames(1);
    assert!(d.sent(&egui::ViewportCommand::Close), "Restart asks the window to close");
    assert!(d.app().restart_after_quit);
    eframe::App::on_exit(d.app());
    assert_eq!(relaunches.get(), 1, "the host relaunches the app once it has exited");
}

#[test]
fn a_relaunch_that_fails_says_why() {
    let mut d = Driver::new();
    d.app().hooks.restart = Some(Box::new(|| Err("can't restart FilmCraft: no binary".into())));
    d.ok("ui.menu.invoke", json!({"id": "app.language.spanish"}));
    d.frames(3);
    d.ok("ui.click", json!({"id": "language.restart.restart"}));
    eframe::App::on_exit(d.app());
    assert!(d.app().ui.status.contains("no binary"), "{}", d.app().ui.status);
}

#[test]
fn restarting_with_unsaved_changes_asks_to_save_and_cancel_forgets_the_restart() {
    let mut d = Driver::with_session(|s| {
        s.execute("file.openDemoProject", json!({})).expect("demo project");
    });
    let clip = d.app().session.active_sequence().unwrap().video_tracks[0].items[0].id.0;
    d.exec("timeline.select", json!({"clips": [clip]}));
    d.exec("edit.label.yellow", json!({}));
    assert!(d.app().session.is_dirty());
    d.ok("ui.menu.invoke", json!({"id": "app.language.spanish"}));
    d.frames(3);
    d.ok("ui.click", json!({"id": "language.restart.restart"}));
    assert!(d.app().restart_after_quit);
    // the window reports the close like an OS request: with unsaved changes it is cancelled and
    // the quit prompt asks to save first, as for Quit
    d.harness.input_mut().viewports.entry(egui::ViewportId::ROOT).or_default().events.push(egui::ViewportEvent::Close);
    d.frames(1);
    assert!(d.sent(&egui::ViewportCommand::CancelClose), "the window must stay open");
    assert!(d.app().ui.clip_dialog.as_ref().is_some_and(|c| c.command == "app.quit"));
    d.ok("ui.key", json!({"key": "Escape"}));
    d.frames(2);
    assert!(!d.app().restart_after_quit, "cancelling the prompt cancels the restart too");
}
