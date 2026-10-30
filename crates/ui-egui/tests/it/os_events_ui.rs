//! The operating system's requests (`HostHooks::os_events`): files opened from outside while
//! FilmCraft runs (macOS: a Finder double-click, Open With, a drop on the Dock icon) and the Dock's
//! Quit. AppKit used to refuse the files ("FilmCraft cannot open files in the “FilmCraft Project”
//! format") because nothing handled its open-documents event.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::mpsc::{Sender, channel};

use egui_kittest::Harness;
use filmcraft_engine::Session;
use filmcraft_ui_egui::control::ControlRequest;
use filmcraft_ui_egui::{FilmcraftApp, OsEvent};
use serde_json::json;

struct Driver {
    harness: Harness<'static, FilmcraftApp>,
    os: Rc<RefCell<Vec<OsEvent>>>,
    _control: Option<Sender<ControlRequest>>,
}

impl Driver {
    /// The demo project, never saved; `control_input_only`: the agent-only mode (`--control-input-only`).
    fn new(control_input_only: bool) -> Self {
        let mut s = Session::default();
        s.execute("file.openDemoProject", json!({})).unwrap();
        let mut app = FilmcraftApp::new(s);
        let mut control = None;
        if control_input_only {
            let (tx, rx) = channel();
            app = app.with_control_input_only(rx);
            control = Some(tx);
        }
        let os = Rc::new(RefCell::new(Vec::new()));
        let queue = Rc::clone(&os);
        app.hooks.os_events = Some(Box::new(move || std::mem::take(&mut *queue.borrow_mut())));
        let harness = Harness::builder().with_size(egui::vec2(1600.0, 980.0)).build_eframe(move |_| app);
        let mut d = Driver { harness, os, _control: control };
        d.frames(4);
        d
    }

    fn frames(&mut self, n: usize) {
        for _ in 0..n {
            self.harness.step();
        }
    }

    fn app(&mut self) -> &mut FilmcraftApp {
        self.harness.state_mut()
    }

    /// The OS sends `event`; the next frame handles it.
    fn os_event(&mut self, event: OsEvent) {
        self.os.borrow_mut().push(event);
        self.frames(1);
    }

    /// The app sent `cmd` to the window in the last frame.
    fn sent(&self, cmd: &egui::ViewportCommand) -> bool {
        self.harness.output().viewport_output.get(&egui::ViewportId::ROOT).is_some_and(|v| v.commands.contains(cmd))
    }

    /// The window is asked to close for one frame.
    fn request_close(&mut self) {
        self.harness.input_mut().viewports.entry(egui::ViewportId::ROOT).or_default().events.push(egui::ViewportEvent::Close);
        self.frames(1);
    }
}

/// A saved project at `<dir>/Harbour Cut.fcproj`.
fn saved_project(dir: &std::path::Path) -> String {
    let path = dir.join("Harbour Cut.fcproj").to_string_lossy().into_owned();
    let mut s = Session::default();
    s.execute("file.openDemoProject", json!({})).unwrap();
    s.execute("file.saveAs", json!({"path": path})).unwrap();
    path
}

#[test]
fn a_project_opened_from_the_os_opens_and_brings_the_window_forward() {
    let dir = tempfile::tempdir().unwrap();
    let path = saved_project(dir.path());
    let mut d = Driver::new(false);
    assert_eq!(d.app().session.path, None);
    d.os_event(OsEvent::Open { project: Some(path.clone()), media: vec![] });
    assert!(d.sent(&egui::ViewportCommand::Focus), "the window comes forward");
    assert_eq!(d.app().session.path.as_deref(), Some(path.as_str()));
    assert_eq!(d.app().session.project.name, "Harbour Cut");
    assert!(d.app().session.prefs.general.recent_projects.contains(&path), "listed in Open Recent");
}

#[test]
fn media_opened_from_the_os_is_imported_into_the_open_project() {
    let dir = tempfile::tempdir().unwrap();
    let wav = dir.path().join("Room Tone.wav");
    std::fs::write(&wav, filmcraft_media::wav::write_wav16(&[0.0; 4800], 1, 48_000)).unwrap();
    let mut d = Driver::new(false);
    let items = d.app().session.project.items.len();
    d.os_event(OsEvent::Open { project: None, media: vec![wav.to_string_lossy().into_owned()] });
    assert!(d.sent(&egui::ViewportCommand::Focus));
    assert_eq!(d.app().session.project.items.len(), items + 1);
    assert!(d.app().session.project.items.values().any(|i| i.name.starts_with("Room Tone")));
    assert_eq!(d.app().session.path, None, "the open project stays open");
}

#[test]
fn a_missing_project_is_reported_and_the_open_project_stays() {
    let dir = tempfile::tempdir().unwrap();
    let missing = dir.path().join("Gone.fcproj").to_string_lossy().into_owned();
    let mut d = Driver::new(false);
    let name = d.app().session.project.name.clone();
    d.os_event(OsEvent::Open { project: Some(missing.clone()), media: vec![] });
    assert_eq!(d.app().session.project.name, name);
    assert!(d.app().ui.status.contains(&missing), "status: {}", d.app().ui.status);
}

/// The Dock's Quit (and logging out) closes the window like Cmd+Q: with unsaved changes the close
/// is cancelled and Save / Don't Save / Cancel is asked first.
#[test]
fn os_quit_with_unsaved_changes_asks_to_save() {
    let mut d = Driver::new(false);
    d.app().session.execute("file.newSequence", json!({"width": 16, "height": 16})).unwrap();
    assert!(d.app().session.is_dirty());
    d.os_event(OsEvent::Quit);
    assert!(d.sent(&egui::ViewportCommand::Close), "Quit asks the window to close");
    assert!(!d.app().quit_confirmed, "nothing is confirmed yet");
    d.request_close();
    assert!(d.sent(&egui::ViewportCommand::CancelClose), "the window must stay open");
    assert_eq!(d.app().ui.clip_dialog.as_ref().map(|c| c.command.as_str()), Some("app.quit"));
}

/// An agent-only session (`--control-input-only`) takes no files from outside, as it takes no
/// native menu commands (#1943), but Quit still works.
#[test]
fn control_only_sessions_ignore_os_files_but_still_quit() {
    let dir = tempfile::tempdir().unwrap();
    let path = saved_project(dir.path());
    let mut d = Driver::new(true);
    d.os_event(OsEvent::Open { project: Some(path), media: vec![] });
    assert!(!d.sent(&egui::ViewportCommand::Focus));
    d.frames(1);
    assert_eq!(d.app().session.path, None);
    d.os_event(OsEvent::Quit);
    assert!(d.sent(&egui::ViewportCommand::Close));
}
