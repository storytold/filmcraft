//! FilmCraft's half of Open and Save against cloud storage.
//!
//! The contract types here ([`CloudBackend`], [`Job`], [`Post`], [`Event`], [`Fault`],
//! [`Account`]) are kept in the same shape as `crates/ui/src/cloud/backend.rs` on
//! wdunn001/craft-libs, which is where storytold/craft-libs#160 is adding the same contract to
//! `craft-ui` itself. They are not pulled from that crate directly: craft-ui's own `cloud`
//! feature exists today only on that fork, pinned to an egui release older than the one this
//! workspace already builds against (`craft-update`, merged on main since this branch diverged,
//! needs the newer one). Once #160 merges, craft-ui carries this contract at the egui release
//! this workspace uses, and this module is replaced by `pub use craft_ui::cloud::{...}` in one
//! commit alongside the `craft-ui` / `craft-cloud` pin bump in the workspace `Cargo.toml`.
//!
//! FilmCraft holds no provider credential and speaks no provider protocol. A [`CloudBackend`] is
//! the one thing a host application supplies (see `apps/filmcraft` / `apps/filmcraft-web`); this
//! crate never learns what it reaches. Nothing here opens a socket, so the pages stay buildable
//! for the web and testable with no service running.
//!
//! With no backend connected, [`available`] is `false` and the cloud menu items, the Open/Save
//! pages and the account badge are not drawn anywhere in this crate (see `menus.rs::cloud_menu`
//! and `header.rs`).
//!
//! # Projects vs media
//!
//! Only the project archive (`.fcproj`) round-trips through the cloud here. FilmCraft projects
//! are whole-file archives that carry metadata and settings but not the raw media; uploading one
//! transfers the metadata only, never raw media bytes.

use std::sync::Arc;
use std::sync::mpsc::{Receiver, Sender};

pub use craft_cloud::{Capability, Entry, EntryKind};

use crate::FilmcraftApp;

/// Work for a host's worker. Each one answers with one [`Event`].
pub enum Job {
    /// Who is signed in, and what the provider behind the service can do.
    Account,
    /// One folder's contents. An empty path is the user's root.
    List(String),
    /// A document's bytes.
    Read { entry: Entry },
    /// Save under a new name in `folder`.
    Create { folder: String, name: String, bytes: Vec<u8> },
}

/// What a worker reports back.
pub enum Event {
    Account(Result<Account, Fault>),
    Listed { path: String, entries: Result<Vec<Entry>, Fault> },
    Read { entry: Entry, bytes: Result<Vec<u8>, Fault> },
    Created(Result<Entry, Fault>),
}

/// Where a worker's answers go. Handed to [`CloudBackend::submit`] with the job it belongs to.
pub struct Post {
    tx: Sender<Event>,
}

impl Post {
    fn new(tx: Sender<Event>) -> Post {
        Post { tx }
    }

    pub fn send(&self, event: Event) {
        let _ = self.tx.send(event);
    }
}

/// Somewhere a [`Job`] can run without blocking a frame.
///
/// `submit` must return immediately. The answer arrives through `post` whenever it is ready, or
/// never; the pages survive either.
pub trait CloudBackend: Send + Sync {
    fn submit(&self, job: Job, post: Post);
}

/// What the service said about the signed-in user.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Account {
    pub display: String,
    pub capabilities: Vec<Capability>,
}

/// A call that did not succeed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Fault {
    Unreachable(String),
    Refused(String),
}

impl Fault {
    /// One sentence for a person to read.
    pub fn text(&self) -> String {
        match self {
            Fault::Unreachable(detail) => format!("The cloud service could not be reached. {detail}"),
            Fault::Refused(detail) => detail.clone(),
        }
    }
}

/// One folder's contents, as last fetched.
#[derive(Clone, Default)]
pub struct Folder {
    pub path: String,
    pub entries: Vec<Entry>,
    pub loading: bool,
    pub loaded: bool,
    pub error: Option<Fault>,
}

/// Open and Save against cloud storage. Inert until the host connects a backend.
pub struct CloudState {
    backend: Option<Arc<dyn CloudBackend>>,
    tx: Sender<Event>,
    rx: Receiver<Event>,
    pub account: Option<Result<Account, Fault>>,
    pub mine: Folder,
    /// The name typed into the Save As page.
    pub save_name: String,
}

impl Default for CloudState {
    fn default() -> CloudState {
        let (tx, rx) = std::sync::mpsc::channel();
        CloudState { backend: None, tx, rx, account: None, mine: Folder::default(), save_name: String::new() }
    }
}

impl CloudState {
    pub fn connect(&mut self, backend: impl CloudBackend + 'static) {
        self.backend = Some(Arc::new(backend));
    }
}

pub const UNAVAILABLE: &str = "No cloud service is configured.";

/// Whether a backend is connected at all.
pub fn available(app: &FilmcraftApp) -> bool {
    app.cloud.backend.is_some()
}

fn submit(app: &FilmcraftApp, job: Job) {
    if let Some(backend) = &app.cloud.backend {
        backend.submit(job, Post::new(app.cloud.tx.clone()));
    }
}

/// Which cloud page the File menu opened, if any.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CloudPage {
    /// File ▸ Open from cloud… — `.fcproj` projects.
    Open,
    /// File ▸ Save as to cloud… — write the project archive.
    SaveAs,
}

/// Ensures the account and the current folder are loaded.
pub fn start(app: &mut FilmcraftApp) {
    if !available(app) {
        return;
    }
    if app.cloud.account.is_none() {
        submit(app, Job::Account);
    }
    if !app.cloud.mine.loaded && !app.cloud.mine.loading {
        let path = app.cloud.mine.path.clone();
        browse(app, &path);
    }
}

/// List a folder.
pub fn browse(app: &mut FilmcraftApp, path: &str) {
    if !available(app) {
        return;
    }
    app.cloud.mine.path = path.to_owned();
    app.cloud.mine.loading = true;
    app.cloud.mine.loaded = false;
    app.cloud.mine.error = None;
    submit(app, Job::List(path.to_owned()));
}

/// Open or browse into a cloud entry, from the Open page.
fn open_entry(app: &mut FilmcraftApp, entry: Entry) {
    if entry.kind == EntryKind::Folder {
        let path = entry.path.clone();
        browse(app, &path);
        return;
    }
    submit(app, Job::Read { entry });
}

/// Save the current project under `name` in `folder`.
pub fn save_as(app: &mut FilmcraftApp, folder: &str, name: &str) -> Result<(), String> {
    if !available(app) {
        return Err(UNAVAILABLE.to_owned());
    }
    let bytes = filmcraft_engine::commands::project_bytes(&app.session);
    let name = project_name(name);
    submit(app, Job::Create { folder: folder.to_owned(), name, bytes });
    Ok(())
}

/// A typed name as a project file name: folder separators dropped, `.fcproj` appended if missing.
fn project_name(typed: &str) -> String {
    let leaf = typed.rsplit(['/', '\\']).next().unwrap_or(typed).trim();
    let leaf = if leaf.is_empty() { "project" } else { leaf };
    if leaf.to_ascii_lowercase().ends_with(".fcproj") { leaf.to_owned() } else { format!("{leaf}.fcproj") }
}

/// Open project bytes fetched from the cloud, through the same staging path local recovery uses.
fn open_bytes(app: &mut FilmcraftApp, name: &str, bytes: Vec<u8>) -> Result<(), String> {
    let stamp = web_time::SystemTime::now().duration_since(web_time::UNIX_EPOCH).map(|d| d.as_millis()).unwrap_or(0);
    let leaf = std::path::Path::new(name).file_name().and_then(|n| n.to_str()).filter(|n| !n.is_empty()).unwrap_or("cloud.fcproj");
    let stage = if cfg!(target_arch = "wasm32") {
        format!("/opfs/cloud-open/{leaf}")
    } else {
        filmcraft_engine::temp_dir().join(format!("filmcraft-cloud-open-{stamp}-{leaf}")).to_string_lossy().into_owned()
    };
    app.session.services.write_file(&stage, &bytes).map_err(|e| format!("could not stage the project: {e}"))?;
    app.session.execute("file.open", serde_json::json!({"path": stage})).map_err(|e| e.to_string())?;
    Ok(())
}

/// Drain the backend's answers and apply them. Called once a frame.
pub fn pump(app: &mut FilmcraftApp) {
    let events: Vec<Event> = app.cloud.rx.try_iter().collect();
    for event in events {
        match event {
            Event::Account(answer) => app.cloud.account = Some(answer),
            Event::Listed { path, entries } => {
                if path != app.cloud.mine.path {
                    continue;
                }
                app.cloud.mine.loading = false;
                match entries {
                    Ok(entries) => {
                        app.cloud.mine.entries = entries;
                        app.cloud.mine.loaded = true;
                    }
                    Err(fault) => app.cloud.mine.error = Some(fault),
                }
            }
            Event::Read { entry, bytes } => match bytes {
                Ok(bytes) => {
                    if let Err(e) = open_bytes(app, &entry.name, bytes) {
                        app.ui.status = format!("could not open \"{}\" from the cloud: {e}", entry.name);
                    } else {
                        app.cloud_page = None;
                    }
                }
                Err(fault) => app.ui.status = format!("could not read \"{}\" from the cloud: {}", entry.name, fault.text()),
            },
            Event::Created(answer) => match answer {
                Ok(entry) => {
                    app.ui.status = format!("Saved \"{}\" to the cloud", entry.name);
                    app.cloud_page = None;
                }
                Err(fault) => app.ui.status = format!("could not save to the cloud: {}", fault.text()),
            },
        }
    }
}

/// The account badge: display name once known, a spinner while the first account check is in
/// flight. Hidden entirely by the caller (`header.rs`) when [`available`] is `false`.
pub fn who(app: &mut FilmcraftApp, ui: &mut egui::Ui) {
    start(app);
    match &app.cloud.account {
        None => {
            ui.add(egui::Spinner::new().size(14.0));
        }
        Some(Ok(account)) => {
            ui.label(egui::RichText::new(&account.display).size(12.5));
        }
        Some(Err(fault)) => {
            ui.label(egui::RichText::new(fault.text()).size(11.0));
        }
    }
}

/// Cloud Open / Save As as a modal over the editor.
pub fn modal(app: &mut FilmcraftApp, ctx: &egui::Context) {
    let Some(page) = app.cloud_page else { return };
    start(app);
    let title = match page {
        CloudPage::Open => tl!("Open from cloud"),
        CloudPage::SaveAs => tl!("Save as to cloud"),
    };
    let mut close = false;
    egui::Modal::new(egui::Id::unique("filmcraft_cloud_page")).show(ctx, |ui| {
        ui.set_min_width(360.0);
        ui.set_max_width(520.0);
        ui.label(egui::RichText::new(title).strong().size(16.0));
        ui.add_space(10.0);
        let folder = app.cloud.mine.clone();
        let here = if folder.path.is_empty() { tl!("Cloud storage").to_owned() } else { folder.path.clone() };
        ui.label(egui::RichText::new(here).size(12.0));
        ui.add_space(6.0);
        egui::ScrollArea::vertical().max_height(320.0).show(ui, |ui| {
            if folder.loading {
                ui.horizontal(|ui| {
                    ui.add(egui::Spinner::new().size(14.0));
                    ui.label(tl!("Loading…"));
                });
                return;
            }
            if let Some(fault) = &folder.error {
                ui.label(fault.text());
                return;
            }
            let mut chosen: Option<Entry> = None;
            for entry in &folder.entries {
                let label = if entry.kind == EntryKind::Folder { format!("\u{1f4c1} {}", entry.name) } else { entry.name.clone() };
                if ui.selectable_label(false, label).clicked() {
                    chosen = Some(entry.clone());
                }
            }
            if let Some(entry) = chosen {
                match page {
                    CloudPage::Open => open_entry(app, entry),
                    CloudPage::SaveAs if entry.kind == EntryKind::Folder => browse(app, &entry.path),
                    CloudPage::SaveAs => {}
                }
            }
        });
        if page == CloudPage::SaveAs {
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                ui.label(tl!("Name:"));
                ui.text_edit_singleline(&mut app.cloud.save_name);
                if ui.button(tl!("Save")).clicked() {
                    let folder = app.cloud.mine.path.clone();
                    let name = app.cloud.save_name.clone();
                    if let Err(e) = save_as(app, &folder, &name) {
                        app.ui.status = e;
                    }
                }
            });
        }
        ui.add_space(12.0);
        ui.horizontal(|ui| {
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.button(tl!("Close")).clicked() || ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                    close = true;
                }
            });
        });
    });
    if close {
        app.cloud_page = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Noop;
    impl CloudBackend for Noop {
        fn submit(&self, _job: Job, _post: Post) {}
    }

    #[test]
    fn unavailable_without_a_connected_backend() {
        let app = FilmcraftApp::new(filmcraft_engine::Session::default());
        assert!(!available(&app));
    }

    #[test]
    fn available_once_connected() {
        let mut app = FilmcraftApp::new(filmcraft_engine::Session::default());
        app.cloud.connect(Noop);
        assert!(available(&app));
    }

    #[test]
    fn save_as_requires_a_connected_backend() {
        let mut app = FilmcraftApp::new(filmcraft_engine::Session::default());
        assert_eq!(save_as(&mut app, "", "demo"), Err(UNAVAILABLE.to_owned()));
    }

    #[test]
    fn project_name_appends_the_extension_and_drops_folders() {
        assert_eq!(project_name("demo"), "demo.fcproj");
        assert_eq!(project_name("demo.fcproj"), "demo.fcproj");
        assert_eq!(project_name("folder/demo"), "demo.fcproj");
        assert_eq!(project_name(""), "project.fcproj");
    }

    /// An in-process mock [`CloudBackend`]: answers `Job::List` with a fixed entry list, so the
    /// Open page's listing and selection logic is exercised with no cloud service running.
    struct MockBackend {
        entries: Vec<Entry>,
    }

    impl CloudBackend for MockBackend {
        fn submit(&self, job: Job, post: Post) {
            match job {
                Job::Account => post.send(Event::Account(Ok(Account { display: "mock@example.test".into(), capabilities: vec![] }))),
                Job::List(path) => post.send(Event::Listed { path, entries: Ok(self.entries.clone()) }),
                Job::Read { entry } => post.send(Event::Read { entry, bytes: Ok(b"not a real project".to_vec()) }),
                Job::Create { name, .. } => {
                    post.send(Event::Created(Ok(Entry {
                        id: "mock-id".into(),
                        room: "mock-room".into(),
                        name,
                        path: String::new(),
                        kind: EntryKind::File,
                        size: Some(0),
                        modified: None,
                        mime: None,
                        etag: Some("1".into()),
                        writable: true,
                        shared_by: None,
                    })))
                }
            }
        }
    }

    fn mock_entry(name: &str) -> Entry {
        Entry {
            id: name.to_owned(),
            room: name.to_owned(),
            name: name.to_owned(),
            path: String::new(),
            kind: EntryKind::File,
            size: Some(0),
            modified: None,
            mime: None,
            etag: None,
            writable: true,
            shared_by: None,
        }
    }

    #[test]
    fn mock_backend_lists_and_updates_the_folder() {
        let mut app = FilmcraftApp::new(filmcraft_engine::Session::default());
        app.cloud.connect(MockBackend { entries: vec![mock_entry("demo.fcproj")] });
        start(&mut app);
        pump(&mut app);
        assert!(app.cloud.mine.loaded, "the folder should be loaded after one pump");
        assert_eq!(app.cloud.mine.entries.len(), 1);
        assert_eq!(app.cloud.mine.entries[0].name, "demo.fcproj");
        assert!(matches!(&app.cloud.account, Some(Ok(a)) if a.display == "mock@example.test"));
    }

    #[test]
    fn mock_backend_save_as_reports_success_and_closes_the_page() {
        let mut app = FilmcraftApp::new(filmcraft_engine::Session::default());
        app.cloud.connect(MockBackend { entries: vec![] });
        app.cloud_page = Some(CloudPage::SaveAs);
        save_as(&mut app, "", "demo").expect("a connected backend accepts the save");
        pump(&mut app);
        assert!(app.cloud_page.is_none(), "a successful save closes the page");
        assert!(app.ui.status.contains("demo.fcproj"));
    }
}
