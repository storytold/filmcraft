//! Files the operating system asks the *running* app to open (macOS open events, #1229, through
//! [`HostHooks::os_open_files`]): a project opens like File ▸ Open, anything else imports the way
//! the same file on the command line does, and a file that cannot be opened is reported instead of
//! silently ignored.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use egui_kittest::Harness;
use filmcraft_engine::Session;
use filmcraft_ui_egui::FilmcraftApp;
use serde_json::json;

struct Driver {
    harness: Harness<'static, FilmcraftApp>,
    /// What the "operating system" hands over: paths per open event, oldest first.
    opened: Arc<Mutex<Vec<Vec<String>>>>,
}

impl Driver {
    /// A running app whose OS open events come from `opened` instead of Finder.
    fn new() -> Self {
        let mut s = Session::default();
        s.execute("file.openDemoProject", json!({})).unwrap();
        let opened = Arc::new(Mutex::new(Vec::new()));
        let queue = Arc::clone(&opened);
        let mut app = FilmcraftApp::new(s);
        app.hooks.os_open_files = Some(Box::new(move || {
            let mut events = queue.lock().unwrap_or_else(|e| e.into_inner());
            events.drain(..).flatten().collect()
        }));
        let harness = Harness::builder().with_size(egui::vec2(1600.0, 980.0)).with_max_steps(10_000).build_eframe(move |_cc| app);
        let mut d = Driver { harness, opened };
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

    /// One open event, as the platform module delivers it (a batch of paths), then the frames that
    /// drain it.
    fn open_event(&mut self, paths: &[&str]) {
        self.opened.lock().unwrap_or_else(|e| e.into_inner()).push(paths.iter().map(|p| (*p).to_string()).collect());
        self.frames(2);
    }

    fn open_path(&self) -> Option<String> {
        self.harness.state().session.path.clone()
    }
}

/// A directory of this test run, for the projects the events open.
fn work_dir() -> PathBuf {
    let dir = std::env::temp_dir().join(format!("fc-ui-open-documents-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// A project saved by an app of its own: `open` it in the driver to prove the event opened a file
/// rather than leaving the startup project alone.
fn saved_project(dir: &Path, name: &str) -> String {
    let path = dir.join(format!("{name}.fcproj"));
    let mut s = Session::default();
    s.execute("file.openDemoProject", json!({})).unwrap();
    s.execute("file.saveAs", json!({"path": path.to_string_lossy()})).unwrap();
    path.to_string_lossy().to_string()
}

/// The regression: the app must open the project the OS hands a running instance (Finder, `open`,
/// the Dock). Before, the event was dropped and the app kept showing whatever it had.
#[test]
fn an_open_event_opens_the_project_it_hands_over() {
    let dir = work_dir();
    let first = saved_project(&dir, "an-open-event-opens-the-project");
    let second = saved_project(&dir, "an-open-event-opens-the-second-project");
    let mut d = Driver::new();
    let startup = d.open_path();
    assert_ne!(startup.as_deref(), Some(first.as_str()), "the event must be what changes the project");

    d.open_event(&[&first]);
    assert_eq!(d.open_path().as_deref(), Some(first.as_str()), "the handed-over project is the open one");

    // a running app: a second event opens the other project
    d.open_event(&[&second]);
    assert_eq!(d.open_path().as_deref(), Some(second.as_str()));

    // a media file imports, the project stays open (the rule the launch arguments use)
    d.open_event(&[&dir.join("clip.mp4").to_string_lossy()]);
    assert_eq!(d.open_path().as_deref(), Some(second.as_str()), "importing does not change the project");
}

/// A project the OS hands over together with media opens the project and imports the rest.
#[test]
fn a_project_and_media_in_one_event_open_and_import() {
    let dir = work_dir();
    let project = saved_project(&dir, "a-project-and-media-in-one-event");
    let missing = dir.join("not-there.mp4");
    let _ = std::fs::remove_file(&missing);
    let mut d = Driver::new();

    d.open_event(&[&project, &missing.to_string_lossy()]);
    assert_eq!(d.open_path().as_deref(), Some(project.as_str()), "the project opens");
    // the media is not there: the app says so in the status bar (never silent, AGENTS.md §0)
    let status = d.harness.state().ui.status.clone();
    assert!(!status.trim().is_empty(), "a file that cannot be imported is reported");
}

/// A path the OS hands over that is not a readable project leaves the session alone and reports why.
#[test]
fn an_unreadable_project_is_reported_and_leaves_the_project_open() {
    let dir = work_dir();
    let good = saved_project(&dir, "an-unreadable-project-is-reported");
    let broken = dir.join("broken.fcproj");
    std::fs::write(&broken, b"{ not a project }").unwrap();
    let mut d = Driver::new();
    d.open_event(&[&good]);
    assert_eq!(d.open_path().as_deref(), Some(good.as_str()));

    d.open_event(&[&broken.to_string_lossy()]);
    assert_eq!(d.open_path().as_deref(), Some(good.as_str()), "a broken file does not close the open project");
    let status = d.harness.state().ui.status.clone();
    assert!(status.contains("broken.fcproj"), "the status bar names the file that failed: {status:?}");
}

/// Without the hook — the platforms that pass the files as arguments, and any host that never
/// installs one — nothing happens at all.
#[test]
fn without_the_hook_no_open_event_opens_anything() {
    let dir = work_dir();
    let project = saved_project(&dir, "without-the-hook-no-open-event");
    let mut s = Session::default();
    s.execute("file.openDemoProject", json!({})).unwrap();
    let mut app = FilmcraftApp::new(s);
    app.hooks.os_open_files = None;
    let mut h = Harness::builder().with_size(egui::vec2(1600.0, 980.0)).build_eframe(move |_cc| app);
    for _ in 0..4 {
        let ctx = h.ctx.clone();
        let mut raw = std::mem::take(h.input_mut());
        eframe::App::raw_input_hook(h.state_mut(), &ctx, &mut raw);
        *h.input_mut() = raw;
        h.step();
    }
    assert_ne!(h.state().session.path.as_deref(), Some(project.as_str()));
}
