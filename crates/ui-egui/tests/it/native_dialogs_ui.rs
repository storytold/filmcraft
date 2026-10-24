//! File dialogs that don't stop the frame loop (#260), headless with a stand-in host runner
//! (`HostHooks::run_dialog`) the test answers by hand: while a dialog is open the app keeps
//! answering requests and drawing, and the chosen path is used when the answer arrives (a menu
//! command, a panel's field), cancelled or lost dialogs do nothing, one dialog is open at a time,
//! and a path for a dialog the user has closed meanwhile goes nowhere. Also: with the synchronous
//! pickers (no runner) a panel's Browse… still fills its field.

use std::path::PathBuf;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex};

use egui_kittest::Harness;
use filmcraft_engine::Session;
use filmcraft_ui_egui::FilmcraftApp;
use filmcraft_ui_egui::control::ControlRequest;
use filmcraft_ui_egui::native_dialogs::FileDialog;
use serde_json::{Value, json};

/// Dialogs the app asked for, with the sender that answers each.
type Asked = Arc<Mutex<Vec<(FileDialog, Sender<Vec<String>>)>>>;

struct Driver {
    harness: Harness<'static, FilmcraftApp>,
    tx: Sender<ControlRequest>,
    asked: Asked,
}

impl Driver {
    /// The demo project; `runner`: dialogs go to the stand-in host runner (else synchronous hooks).
    fn new(runner: bool) -> Self {
        let mut session = Session::default();
        session.execute("file.openDemoProject", json!({})).expect("demo project");
        let (tx, rx) = channel();
        let mut app = FilmcraftApp::new(session).with_control(rx);
        let asked: Asked = Arc::default();
        if runner {
            let a = asked.clone();
            app.hooks.run_dialog = Some(Box::new(move |dialog: FileDialog| -> Receiver<Vec<String>> {
                let (tx, rx) = channel();
                a.lock().unwrap().push((dialog, tx));
                rx
            }));
        }
        let harness = Harness::builder().with_size(egui::vec2(1600.0, 980.0)).with_max_steps(10_000).build_eframe(move |_cc| app);
        let mut d = Driver { harness, tx, asked };
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
        let r = self.call(method, params.clone());
        assert_eq!(r["ok"], true, "{method} {params}: {r}");
        r["result"].clone()
    }

    fn click(&mut self, id: &str) {
        self.ok("ui.click", json!({"id": id}));
        self.frames(3);
    }

    fn app(&mut self) -> &mut FilmcraftApp {
        self.harness.state_mut()
    }

    /// The one dialog asked for so far (taken), with its answering sender.
    fn asked(&mut self) -> (FileDialog, Sender<Vec<String>>) {
        let mut a = self.asked.lock().unwrap();
        assert_eq!(a.len(), 1, "one dialog asked for: {:?}", a.iter().map(|x| &x.0).collect::<Vec<_>>());
        a.pop().unwrap()
    }

    fn items(&mut self) -> usize {
        self.app().session.project.items.len()
    }
}

/// A one-second 8 kHz mono WAV in Cargo's temporary folder for integration tests.
fn wav() -> PathBuf {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("native-dialogs");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("tone.wav");
    let (rate, n) = (8000u32, 8000u32);
    let mut b = Vec::new();
    b.extend_from_slice(b"RIFF");
    b.extend_from_slice(&(36 + n * 2).to_le_bytes());
    b.extend_from_slice(b"WAVEfmt ");
    b.extend_from_slice(&16u32.to_le_bytes());
    b.extend_from_slice(&1u16.to_le_bytes());
    b.extend_from_slice(&1u16.to_le_bytes());
    b.extend_from_slice(&rate.to_le_bytes());
    b.extend_from_slice(&(rate * 2).to_le_bytes());
    b.extend_from_slice(&2u16.to_le_bytes());
    b.extend_from_slice(&16u16.to_le_bytes());
    b.extend_from_slice(b"data");
    b.extend_from_slice(&(n * 2).to_le_bytes());
    for i in 0..n {
        b.extend_from_slice(&(((i as f32 * 0.2).sin() * 8000.0) as i16).to_le_bytes());
    }
    std::fs::write(&path, b).unwrap();
    path
}

#[test]
fn the_app_keeps_running_while_the_import_dialog_is_open_and_imports_when_it_answers() {
    let mut d = Driver::new(true);
    let before = d.items();
    // File ▸ Import answers at once: the dialog is open, nothing is imported yet
    assert_eq!(d.ok("ui.menu.invoke", json!({"id": "file.import"})), json!({"dialog": "open"}));
    assert!(d.app().native_dialog_open());
    let (dialog, answer) = d.asked();
    assert!(matches!(&dialog, FileDialog::OpenFiles { exts } if exts.iter().any(|e| e == "wav")), "{dialog:?}");
    // the frame loop runs and requests are answered while the dialog is open
    for _ in 0..5 {
        d.ok("ui.inspect", json!({}));
    }
    d.frames(20);
    assert_eq!(d.items(), before);
    // the user chooses a file: it is imported on the next frame
    answer.send(vec![wav().to_string_lossy().into_owned()]).unwrap();
    d.frames(2);
    assert!(!d.app().native_dialog_open());
    assert_eq!(d.items(), before + 1);
    assert!(d.app().session.project.items.values().any(|i| i.name == "tone.wav"));
}

#[test]
fn cancelled_or_lost_dialogs_do_nothing_and_one_dialog_is_open_at_a_time() {
    let mut d = Driver::new(true);
    let before = d.items();
    d.ok("ui.menu.invoke", json!({"id": "file.import"}));
    // a second dialog while one is open is refused
    let r = d.call("ui.menu.invoke", json!({"id": "file.open"}));
    assert_eq!(r["ok"], false, "{r}");
    assert!(r.to_string().contains("already open"), "{r}");
    let (_, answer) = d.asked();
    // cancelled: no path
    answer.send(vec![]).unwrap();
    d.frames(2);
    assert!(!d.app().native_dialog_open());
    assert_eq!(d.items(), before);
    // the host's dialog went away without an answer (its thread failed): as if cancelled
    d.ok("ui.menu.invoke", json!({"id": "file.import"}));
    let (_, answer) = d.asked();
    drop(answer);
    d.frames(2);
    assert!(!d.app().native_dialog_open());
    assert_eq!(d.items(), before);
    // and the next dialog opens normally
    assert_eq!(d.ok("ui.menu.invoke", json!({"id": "file.open"})), json!({"dialog": "open"}));
    assert!(matches!(d.asked().0, FileDialog::OpenProject));
}

#[test]
fn a_panel_folder_arrives_in_its_open_dialog_and_nowhere_once_it_is_closed() {
    let mut d = Driver::new(true);
    // Settings ▸ Media Cache ▸ Location ▸ Browse…
    d.ok("ui.menu.invoke", json!({"id": "app.settings.mediaCache"}));
    d.frames(3);
    d.click("settings.mediaCache.location.browse");
    let (dialog, answer) = d.asked();
    assert!(matches!(dialog, FileDialog::Folder { .. }), "{dialog:?}");
    d.frames(10);
    answer.send(vec!["/tmp/filmcraft-cache-test".into()]).unwrap();
    d.frames(3);
    let loc = d.app().ui.settings.as_ref().map(|s| s.values.pointer("/mediaCache/location").cloned());
    assert_eq!(loc, Some(Some(json!("/tmp/filmcraft-cache-test"))));
    // Browse… again, then Cancel the Settings dialog before the folder comes back: nothing to write into
    d.click("settings.mediaCache.location.browse");
    let (_, answer) = d.asked();
    d.click("settings.cancel");
    assert!(d.app().ui.settings.is_none());
    answer.send(vec!["/tmp/elsewhere".into()]).unwrap();
    d.frames(3);
    assert!(d.app().ui.settings.is_none());
    assert!(!d.app().native_dialog_open());
    assert_ne!(d.app().session.prefs.to_value().pointer("/mediaCache/location"), Some(&json!("/tmp/elsewhere")));
}

#[test]
fn export_location_browse_works_with_the_runner_and_with_synchronous_pickers() {
    // runner: the folder arrives later
    let mut d = Driver::new(true);
    d.ok("ui.set", json!({"mode": "export"}));
    d.frames(3);
    d.click("export.location.browse");
    let (_, answer) = d.asked();
    answer.send(vec!["/tmp/exports-a".into()]).unwrap();
    d.frames(2);
    assert_eq!(d.app().ui.export.location, "/tmp/exports-a");

    // synchronous picker (no runner, as on macOS / Windows / other hosts): still filled in
    let mut d = Driver::new(false);
    d.app().hooks.pick_folder = Some(Box::new(|| Some("/tmp/exports-b".into())));
    d.ok("ui.set", json!({"mode": "export"}));
    d.frames(3);
    d.click("export.location.browse");
    assert_eq!(d.app().ui.export.location, "/tmp/exports-b");
    assert!(!d.app().native_dialog_open());
}
