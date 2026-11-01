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
    tx: Sender<ControlRequest>,
}

impl Driver {
    /// The demo project, never saved; `control_input_only`: the agent-only mode (`--control-input-only`).
    fn new(control_input_only: bool) -> Self {
        let mut s = Session::default();
        s.execute("file.openDemoProject", json!({})).unwrap();
        let (tx, rx) = channel();
        let app = FilmcraftApp::new(s);
        let mut app = if control_input_only { app.with_control_input_only(rx) } else { app.with_control(rx) };
        let os = Rc::new(RefCell::new(Vec::new()));
        let queue = Rc::clone(&os);
        app.hooks.os_events = Some(Box::new(move || std::mem::take(&mut *queue.borrow_mut())));
        let harness = Harness::builder().with_size(egui::vec2(1600.0, 980.0)).build_eframe(move |_| app);
        let mut d = Driver { harness, os, tx };
        d.frames(4);
        d
    }

    fn frames(&mut self, n: usize) {
        for _ in 0..n {
            // the control channel's clicks enter through the input hook
            let ctx = self.harness.ctx.clone();
            let mut raw = std::mem::take(self.harness.input_mut());
            eframe::App::raw_input_hook(self.harness.state_mut(), &ctx, &mut raw);
            *self.harness.input_mut() = raw;
            self.harness.step();
        }
    }

    /// Click the widget with automation id `id` through the control channel.
    fn click(&mut self, id: &str) {
        let (req, reply) = ControlRequest::new("ui.click", json!({"id": id}));
        self.tx.send(req).unwrap();
        for _ in 0..600 {
            self.frames(1);
            if let Ok(v) = reply.try_recv() {
                assert_eq!(v["ok"], json!(true), "click {id}: {v}");
                self.frames(3);
                return;
            }
        }
        panic!("no reply to the click on {id}");
    }

    /// The command of the dialog shown, if any.
    fn dialog(&mut self) -> Option<String> {
        self.app().ui.clip_dialog.as_ref().map(|d| d.command.clone())
    }

    /// Make an unsaved change.
    fn edit(&mut self) {
        self.app().session.execute("file.newSequence", json!({"width": 16, "height": 16})).unwrap();
        assert!(self.app().session.is_dirty());
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

/// Opening a project replaces the open one: with unsaved changes, Save / Don't Save / Cancel is
/// asked first, as Close Project asks, instead of losing them. Don't Save opens it.
#[test]
fn a_project_opened_from_the_os_asks_before_discarding_unsaved_changes() {
    let dir = tempfile::tempdir().unwrap();
    let path = saved_project(dir.path());
    let mut d = Driver::new(false);
    d.edit();
    d.os_event(OsEvent::Open { project: Some(path.clone()), media: vec![] });
    assert!(d.sent(&egui::ViewportCommand::Focus), "the window comes forward to ask");
    assert_eq!(d.dialog().as_deref(), Some("file.closeProject"));
    assert_eq!(d.app().session.path, None, "nothing is opened before the answer");
    assert!(d.app().session.is_dirty());
    d.frames(2);
    d.click("closeProject.dontSave");
    assert_eq!(d.dialog(), None);
    assert_eq!(d.app().session.path.as_deref(), Some(path.as_str()));
    assert!(!d.app().session.is_dirty());
}

#[test]
fn cancel_keeps_the_unsaved_project_and_save_saves_it_before_opening() {
    let dir = tempfile::tempdir().unwrap();
    let other = saved_project(dir.path());
    let mine = dir.path().join("Mine.fcproj").to_string_lossy().into_owned();
    let mut d = Driver::new(false);
    d.app().session.execute("file.saveAs", json!({"path": mine})).unwrap();
    d.edit();
    let wav = dir.path().join("Room Tone.wav");
    std::fs::write(&wav, filmcraft_media::wav::write_wav16(&[0.0; 4800], 1, 48_000)).unwrap();
    let media = vec![wav.to_string_lossy().into_owned()];
    d.os_event(OsEvent::Open { project: Some(other.clone()), media: media.clone() });
    d.frames(2);
    d.click("closeProject.cancel");
    assert_eq!(d.dialog(), None);
    assert_eq!(d.app().session.path.as_deref(), Some(mine.as_str()));
    assert!(d.app().session.is_dirty(), "the changes are still there");
    let before = std::fs::read(&mine).unwrap();
    d.os_event(OsEvent::Open { project: Some(other.clone()), media });
    d.frames(2);
    d.click("closeProject.save");
    assert_ne!(std::fs::read(&mine).unwrap(), before, "the changes were saved first");
    assert_eq!(d.app().session.path.as_deref(), Some(other.as_str()));
    assert!(d.app().session.project.items.values().any(|i| i.name.starts_with("Room Tone")), "the media came along");
}

/// A project that can't be opened leaves the open project as it was: the media sent with it is
/// not imported into the wrong project.
#[test]
fn media_sent_with_a_project_that_fails_to_open_is_not_imported() {
    let dir = tempfile::tempdir().unwrap();
    let wav = dir.path().join("Room Tone.wav");
    std::fs::write(&wav, filmcraft_media::wav::write_wav16(&[0.0; 4800], 1, 48_000)).unwrap();
    let missing = dir.path().join("Gone.fcproj").to_string_lossy().into_owned();
    let mut d = Driver::new(false);
    let items = d.app().session.project.items.len();
    d.os_event(OsEvent::Open { project: Some(missing.clone()), media: vec![wav.to_string_lossy().into_owned()] });
    assert_eq!(d.app().session.project.items.len(), items);
    assert!(d.app().ui.status.contains(&missing), "status: {}", d.app().ui.status);
}

/// The Dock's Quit (and logging out) closes the window like Cmd+Q: with unsaved changes the close
/// is cancelled and Save / Don't Save / Cancel is asked first.
#[test]
fn os_quit_with_unsaved_changes_asks_to_save() {
    let mut d = Driver::new(false);
    d.edit();
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
