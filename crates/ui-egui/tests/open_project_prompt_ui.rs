//! Headless UI tests of File ▸ Open Project with unsaved changes: once the project to open is
//! chosen, the Save Project prompt asks Cancel / Don't Save / Save before it opens.

use std::path::PathBuf;
use std::sync::mpsc::{Sender, channel};
use std::sync::{Arc, Mutex};

use egui_kittest::Harness;
use filmcraft_engine::Session;
use filmcraft_engine::project::{ClipId, Label};
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

    fn menu(&mut self, id: &str) -> Value {
        let r = self.ok("ui.menu.invoke", json!({"id": id}));
        self.frames(3);
        r
    }

    fn click(&mut self, id: &str) {
        self.ok("ui.click", json!({"id": id}));
        self.frames(3);
    }

    fn has(&mut self, id: &str) -> bool {
        let v = self.ok("ui.elements", json!({"prefix": id}));
        v.as_array().unwrap().iter().any(|e| e["id"] == json!(id))
    }

    fn app(&mut self) -> &mut FilmcraftApp {
        self.harness.state_mut()
    }

    fn path(&mut self) -> Option<String> {
        self.app().session.path.clone()
    }

    /// Make an unsaved change: the first clip turns yellow. Returns the clip.
    fn edit(&mut self) -> ClipId {
        let c = self.app().session.active_sequence().unwrap().video_tracks[0].items[0].id;
        self.exec("timeline.select", json!({"clips": [c.0]}));
        self.menu("edit.label.yellow");
        assert!(self.app().session.is_dirty());
        c
    }

    /// The label of `clip` in the open project.
    fn label(&mut self, clip: ClipId) -> Label {
        let p = &self.app().session.project;
        p.sequences().filter_map(|i| p.sequence(i.id)).find_map(|q| q.find_item(clip).map(|(_, it)| it.label)).unwrap()
    }

    /// File ▸ Open Project… with the Open dialog answering `path`.
    fn open_project(&mut self, path: &str) -> Value {
        let path = path.to_string();
        self.app().hooks.pick_open_project = Some(Box::new(move || Some(path.clone())));
        self.menu("file.open")
    }
}

/// A folder for one test holding the demo project saved as `current.fcproj` and `other.fcproj`.
/// The session is left on `current.fcproj` with nothing unsaved.
fn projects(d: &mut Driver, test: &str) -> (PathBuf, String, String) {
    let dir = std::env::temp_dir().join(format!("fc-open-project-prompt-{}-{test}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let current = dir.join("current.fcproj").to_string_lossy().into_owned();
    let other = dir.join("other.fcproj").to_string_lossy().into_owned();
    d.exec("file.saveAs", json!({"path": other}));
    d.exec("file.saveAs", json!({"path": current}));
    assert!(!d.app().session.is_dirty());
    (dir, current, other)
}

#[test]
fn open_project_with_unsaved_changes_asks_first_and_cancel_keeps_the_project() {
    let mut d = Driver::new();
    let (dir, current, other) = projects(&mut d, "cancel");
    let clip = d.edit();
    let r = d.open_project(&other);
    assert_eq!(d.path(), Some(current.clone()), "nothing opens before an answer");
    assert_eq!(r["dialog"], "openProject");
    assert!(d.has("openProject.save") && d.has("openProject.dontSave") && d.has("openProject.cancel"));
    d.click("openProject.cancel");
    assert!(d.app().ui.clip_dialog.is_none());
    assert_eq!(d.path(), Some(current));
    assert!(d.app().session.is_dirty());
    assert_eq!(d.label(clip), Label::Yellow, "the change is kept");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn open_project_save_saves_the_changes_then_opens_the_chosen_project() {
    let mut d = Driver::new();
    let (dir, current, other) = projects(&mut d, "save");
    let clip = d.edit();
    d.open_project(&other);
    d.click("openProject.save");
    assert!(d.app().ui.clip_dialog.is_none());
    assert_eq!(d.path(), Some(other));
    assert!(!d.app().session.is_dirty());
    // the change was written to the project it was made in
    d.exec("file.open", json!({"path": current}));
    assert_eq!(d.label(clip), Label::Yellow);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn open_project_dont_save_opens_the_chosen_project_without_saving() {
    let mut d = Driver::new();
    let (dir, current, other) = projects(&mut d, "dontSave");
    let first = d.app().session.active_sequence().unwrap().video_tracks[0].items[0].clone();
    assert_ne!(first.label, Label::Yellow);
    let clip = d.edit();
    // a project that can't be opened leaves the current one open with its changes
    let missing = dir.join("missing.fcproj").to_string_lossy().into_owned();
    d.open_project(&missing);
    d.click("openProject.dontSave");
    assert!(d.app().ui.clip_dialog.is_none());
    assert!(d.app().ui.status.contains("missing.fcproj"), "{}", d.app().ui.status);
    assert_eq!(d.path(), Some(current.clone()));
    assert!(d.app().session.is_dirty());
    assert_eq!(d.label(clip), Label::Yellow);
    d.open_project(&other);
    d.click("openProject.dontSave");
    assert!(d.app().ui.clip_dialog.is_none());
    assert_eq!(d.path(), Some(other));
    assert!(!d.app().session.is_dirty());
    // the project the change was made in is as it was saved
    d.exec("file.open", json!({"path": current}));
    assert_eq!(d.label(clip), first.label);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn open_project_asks_only_with_unsaved_changes_and_a_scripted_open_does_not_ask() {
    let mut d = Driver::new();
    let (dir, current, other) = projects(&mut d, "clean");
    let r = d.open_project(&other);
    assert!(d.app().ui.clip_dialog.is_none(), "nothing unsaved: no prompt");
    assert_eq!(r["path"], json!(other));
    assert_eq!(d.path(), Some(other.clone()));
    d.edit();
    assert_eq!(d.open_project(&current)["dialog"], "openProject");
    assert_eq!(d.path(), Some(other));
    d.click("openProject.cancel");
    // `file.open` with a path (agents, MCP) opens without asking
    d.exec("file.open", json!({"path": current}));
    assert!(d.app().ui.clip_dialog.is_none());
    assert_eq!(d.path(), Some(current));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn open_project_save_on_a_never_saved_project_uses_the_save_dialog() {
    let mut d = Driver::new();
    let (dir, _, other) = projects(&mut d, "neverSaved");
    d.app().session.path = None;
    let clip = d.edit();
    let saved = dir.join("saved.fcproj").to_string_lossy().into_owned();
    let mut answers = vec![None, Some(saved.clone())];
    d.app().hooks.pick_save = Some(Box::new(move |_| if answers.is_empty() { None } else { answers.remove(0) }));
    d.open_project(&other);
    // the Save dialog is canceled: back to the prompt, nothing opens
    d.click("openProject.save");
    assert!(d.app().ui.clip_dialog.is_some());
    assert_eq!(d.path(), None);
    assert!(d.app().session.is_dirty());
    // saved to the chosen file, then the chosen project opens
    d.click("openProject.save");
    assert!(d.app().ui.clip_dialog.is_none());
    assert_eq!(d.path(), Some(other));
    d.exec("file.open", json!({"path": saved}));
    assert_eq!(d.label(clip), Label::Yellow);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn open_project_asks_when_the_open_dialog_answers_in_a_later_frame() {
    let mut d = Driver::new();
    let (dir, current, other) = projects(&mut d, "runner");
    d.edit();
    // a host that shows file dialogs without blocking (the desktop app on Linux)
    let answer: Arc<Mutex<Option<Sender<Vec<String>>>>> = Arc::default();
    let a = answer.clone();
    d.app().hooks.run_dialog = Some(Box::new(move |_| {
        let (tx, rx) = channel();
        *a.lock().unwrap() = Some(tx);
        rx
    }));
    assert_eq!(d.menu("file.open"), json!({"dialog": "open"}));
    answer.lock().unwrap().take().unwrap().send(vec![other.clone()]).unwrap();
    d.frames(3);
    assert!(d.has("openProject.dontSave"));
    assert_eq!(d.path(), Some(current));
    d.click("openProject.dontSave");
    assert_eq!(d.path(), Some(other));
    let _ = std::fs::remove_dir_all(&dir);
}
