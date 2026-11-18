//! FilmCraft's half of Open and Save against cloud storage.
//!
//! The pages themselves are `craft_ui::cloud`. Every Craft app offers the same ones. What is
//! here is what only FilmCraft can answer: which extensions a project has, how bytes become a
//! project and how an open project is encoded back.
//!
//! FilmCraft holds no provider credential and speaks no provider protocol. A [`CloudBackend`]
//! is the one thing a host application supplies; this crate never learns what it reaches.
//! Nothing in this file opens a socket, so Open and Save stay buildable for the web and
//! testable with no service running.
//!
//! No backend is connected anywhere in this app yet. With none connected, [`available`] is
//! `false` and the cloud menu items, the Open/Save pages, the account badge and the share
//! dialog are not drawn (see `menus.rs::cloud_menu` and `header.rs::draw_cloud_badge`).
//!
//! Only the project archive (`.fcproj`) round-trips through Open and Save here. FilmCraft
//! projects are whole-file archives that carry metadata and settings but not the raw media;
//! saving one to the cloud transfers the metadata only, never raw media bytes.

use std::collections::HashMap;

// Re-exported so a host application, and a test, can name these types without depending on the
// crates they come from.
pub use craft_cloud::{Capability, Entry, EntryKind};
pub use craft_ui::cloud::{
    Account, Busy, CloudApp, CloudBackend, CloudFiles, Conflict, CopyFormat, Destination, DocumentKind, Event, Fault, Folder, Job, Link, Origin, Post,
    Source, UNAVAILABLE, available, browse, can_converse, cancel_save_as, conflict_modal, destination_name, has_target, link, load_shared, open_entry,
    open_page, origin, parent_of, prefers_cloud, progress, pump, reload_active, save, save_as, save_page, share_button, shares, sign_in, sign_out, start,
    who, work_together,
};
use egui::{Color32, Painter, Rect, Ui};
use serde_json::Value as Json;

use crate::FilmcraftApp;
use crate::icons::{self, Icon};

/// Project archive extensions for File ▸ Open from cloud… / Save as to cloud….
const PROJECT_EXTS: [&str; 1] = ["fcproj"];
const PROJECT_MIMES: [&str; 2] = ["application/x-filmcraft-project", "application/octet-stream"];

/// What a FilmCraft project archive is in the cloud file list.
pub const PROJECT: DocumentKind = DocumentKind { extensions: &PROJECT_EXTS, mimes: &PROJECT_MIMES, noun: "project", default_extension: "fcproj" };

/// The Save a Copy formats available in the Share menu.
///
/// FilmCraft projects are not losslessly round-trippable to other formats at the project level,
/// so "Save a Copy" here offers the interchange formats the engine can export to: Final Cut
/// XML and AAF/OMF. These are interop exports, not full fidelity.
const COPIES: [(&str, &str); 2] = [("fcpxml", "Save a Copy as Final Cut Pro XML (.fcpxml)\u{2026}"), ("aaf", "Export as AAF\u{2026}")];

/// What each save in flight encoded: keyed by document uid so a concurrent save and export
/// do not confuse each other's dirty-flag updates.
pub type Sent = HashMap<u64, u64>;

/// Whether FilmCraft can open this cloud entry as a project.
pub fn opens(entry: &Entry) -> bool {
    craft_ui::cloud::opens(entry, PROJECT)
}

/// A typed name as a project file name. Folder separators are dropped; a missing or unknown
/// extension becomes `.fcproj`.
pub fn project_name(typed: &str) -> String {
    craft_ui::cloud::document_name(typed, PROJECT)
}

/// The whole of the cloud state, for the control channel and for tests.
pub fn status(app: &FilmcraftApp) -> Json {
    craft_ui::cloud::report(app)
}

/// The Share menu's export half, as the control it is drawn in wants it.
pub fn copies() -> Vec<CopyFormat> {
    COPIES.iter().map(|(id, label)| CopyFormat::new(*id, *label)).collect()
}

/// The host part of the service address, for the account badge's first line.
///
/// Read from the environment and compiled in nowhere, because the service is
/// deployment-specific. Somebody with accounts on two deployments needs to know which one a
/// project is about to land in.
fn service_host() -> String {
    ["FILMCRAFT_CLOUD_URL", "CRAFT_CLOUD_URL"]
        .into_iter()
        .filter_map(|key| std::env::var(key).ok())
        .map(|value| value.trim().trim_end_matches('/').to_owned())
        .find(|value| !value.is_empty())
        .map(|value| value.trim_start_matches("https://").trim_start_matches("http://").to_owned())
        .unwrap_or_default()
}

/// Determine the write format from the destination.
///
/// A cloud round-trip to a `.fcproj` always stays `.fcproj`. Interop exports (FCPXML, AAF)
/// are only produced through explicit "Save a Copy" actions, never as a side-effect of Save.
/// A destination whose extension this application does not write is refused with a clear error.
fn format_for(into: Destination<'_>) -> Result<&'static str, String> {
    let ext = into.extension();
    match ext.as_str() {
        "fcproj" | "" => Ok("fcproj"),
        "fcpxml" => Ok("fcpxml"),
        "aaf" => Ok("aaf"),
        other => Err(format!("FilmCraft cannot write `.{other}`. Use File \u{203a} Export for other formats, or type a name ending in `.fcproj`.")),
    }
}

impl CloudApp for FilmcraftApp {
    fn cloud(&self) -> &CloudFiles {
        &self.cloud
    }

    fn cloud_mut(&mut self) -> &mut CloudFiles {
        &mut self.cloud
    }

    fn kind(&self) -> DocumentKind {
        PROJECT
    }

    fn active(&self) -> Option<u64> {
        // One project per window. Addressable for cloud Save / Save As whether the project lives
        // on disk or was opened from the cloud (open_bytes leaves session.path at None, so the
        // title shows the cloud name instead of a local one).
        Some(1u64)
    }

    fn title(&self, _uid: u64) -> String {
        self.session.project.name.clone()
    }

    fn open_bytes(&mut self, _name: &str, bytes: Vec<u8>) -> Result<Option<u64>, String> {
        // Decoded directly from memory, with no disk path: a cloud Open must not adopt a local
        // temp path as the project's path, and must not add one to Recent Projects.
        filmcraft_engine::commands::open_project_bytes(&mut self.session, &bytes).map_err(|e| e.to_string())?;
        self.cloud_page = None;
        Ok(Some(1u64))
    }

    fn encode(&mut self, uid: u64, into: Destination<'_>) -> Result<Vec<u8>, String> {
        let fmt = format_for(into)?;
        if fmt == "fcproj" {
            let revision = self.session.revision;
            self.cloud_sent.insert(uid, revision);
            return Ok(filmcraft_engine::commands::project_bytes(&self.session));
        }
        // FCPXML / AAF: export through the host VFS (desktop disk or wasm in-memory), never std::fs.
        let stamp = web_time::SystemTime::now().duration_since(web_time::UNIX_EPOCH).map(|d| d.as_millis()).unwrap_or(0);
        let path = filmcraft_engine::temp_dir().join(format!("filmcraft-cloud-save-{stamp}.{fmt}")).to_string_lossy().to_string();
        let cmd = match fmt {
            "fcpxml" => "file.exportFcpxml",
            "aaf" => "file.exportAaf",
            _ => "file.saveCopy",
        };
        self.session.execute(cmd, serde_json::json!({"path": path})).map_err(|e| e.to_string())?;
        self.session.services.read_file(&path).map_err(|e| format!("could not read the encoded project: {e}"))
    }

    fn close(&mut self, _uid: u64) {
        // FilmCraft holds one project per window. Closing a cloud-opened project that a reload
        // replaced means the window already shows the new one; nothing to do here.
    }

    fn mark_saved(&mut self, uid: u64) {
        let Some(revision) = self.cloud_sent.remove(&uid) else { return };
        if self.session.revision == revision {
            self.session.saved_revision = revision;
        }
    }

    fn drop_pending(&mut self, uid: u64) {
        self.cloud_sent.remove(&uid);
    }

    fn sharing(&mut self) -> &mut craft_ui::share::ShareDialog {
        &mut self.share
    }

    fn presence(&mut self) -> &mut craft_ui::presence::PresenceBar {
        &mut self.presence
    }

    fn paint_icon(&self, painter: &Painter, rect: Rect, kind: EntryKind, colour: Color32) {
        let icon = if kind == EntryKind::Folder { Icon::Folder } else { Icon::Film };
        icons::paint(painter, rect, icon, colour);
    }

    fn open_url(&self, url: &str) {
        // `open_url` takes `&self`: `hooks.open_url` is an `FnMut` and needs `&mut self` to call,
        // so this goes through the egui context instead, kept for exactly this (see `app.rs`,
        // where `ui_ctx` is refreshed every frame). `ctx.open_url` only does something on the
        // web; native hosts that want a real browser reach it from `links::open` instead, which
        // this path (conversation / share links opened from inert, disconnected UI) never does.
        self.ui_ctx.open_url(egui::OpenUrl::new_tab(url));
    }

    fn say(&mut self, text: String) {
        self.ui.status = text;
    }

    fn toast(&mut self, text: String) {
        self.session.toast(text);
    }

    fn go_to_save_as(&mut self) {
        self.cloud_page = Some(CloudPage::SaveAs);
    }

    fn app_name(&self) -> &str {
        "FilmCraft"
    }

    fn tr(&self, text: &str) -> String {
        crate::i18n::t(text).to_owned()
    }

    fn service(&self) -> String {
        service_host()
    }

    fn save_note(&self) -> Option<String> {
        Some(self.tr("The project is saved as a FilmCraft project archive (.fcproj). \
             Media files are not uploaded; they must remain accessible by path."))
    }

    fn style(&self, _ui: &Ui) -> craft_ui::cloud::CloudStyle {
        let t = &self.tokens;
        craft_ui::cloud::CloudStyle {
            surface: t.header_bg,
            border: Color32::BLACK,
            text: t.text,
            dim: t.text_dim,
            hover: t.hover,
            soft: t.field_bg,
            family: egui::FontFamily::Proportional,
            bold: egui::FontFamily::Name("semibold".into()),
        }
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

/// Drive the Share dialog every frame (capability poll + modal) and finish saves it asked for.
///
/// `ShareDialog::show` must run every frame regardless of whether the dialog is open: it is
/// where the dialog's own backend answers are collected. Skipping it while closed would leave
/// those answers queued forever. Call after [`pump`].
pub fn pump_share(app: &mut FilmcraftApp, ctx: &egui::Context) {
    // A share-initiated Create that just finished: hand the new cloud id back to the dialog.
    if app.cloud.sharing_save && app.cloud.busy.is_none() {
        let prior = app.share_save_prior_etag.take();
        // Entry.etag is Option; flatten so we compare String to String.
        let now = origin(app).and_then(|o| o.entry.etag.clone());
        let changed = match (&prior, &now) {
            (Some(None), Some(_)) => true,              // first cloud save
            (Some(Some(a)), Some(b)) if a != b => true, // new version
            _ => false,
        };
        if changed {
            if let Some(o) = origin(app).cloned() {
                app.share.saved(o.entry.id, o.entry.name);
            }
        } else {
            app.share.save_failed(app.tr("The project was not saved to the cloud."));
        }
        app.cloud.sharing_save = false;
    }
    app.share.show(ctx);
    if let Some(req) = app.share.take_save_request() {
        // The share dialog's destination id is "cloud"; write into the current cloud folder
        // (root if empty).
        let folder = app.cloud.mine.path.clone();
        app.share_save_prior_etag = Some(origin(app).and_then(|o| o.entry.etag.clone()));
        app.cloud.sharing_save = true;
        if let Err(e) = save_as(app, &folder, &req.name) {
            app.cloud.sharing_save = false;
            app.share_save_prior_etag = None;
            app.share.save_failed(e);
        }
    }
}

/// Cloud Open / Save As as a modal over the editor.
pub fn modal(app: &mut FilmcraftApp, ctx: &egui::Context) {
    let Some(page) = app.cloud_page else { return };
    let title = match page {
        CloudPage::Open => tl!("Open from cloud"),
        CloudPage::SaveAs => tl!("Save as to cloud"),
    };
    let mut close = false;
    egui::Modal::new(egui::Id::unique("filmcraft_cloud_page")).show(ctx, |ui| {
        let margin = egui::Frame::popup(ui.style()).total_margin().sum();
        let room = (ctx.content_rect().size() - margin - egui::vec2(16.0, 16.0)).max(egui::Vec2::splat(1.0));
        let width = 680.0_f32.min(room.x);
        ui.set_min_width(360.0_f32.min(width));
        ui.set_max_width(width);
        ui.label(egui::RichText::new(title).strong().size(16.0));
        ui.add_space(10.0);
        let body = (room.y - ui.min_size().y - 48.0).max(1.0);
        egui::ScrollArea::vertical().id_salt(("filmcraft_cloud_page", page as u8)).max_height(body).show(ui, |ui| match page {
            CloudPage::Open => open_page(app, ui),
            CloudPage::SaveAs => {
                close = save_page(app, ui);
            }
        });
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
        // Leaving the page without having started a save (the ordinary Close/Escape path; a
        // save that did start already cleared `app.cloud_page` above and arrives here with
        // `close` unset) discards any conflict-copy target and suggested name a dismissed
        // conflict modal may have left behind.
        if page == CloudPage::SaveAs {
            cancel_save_as(app);
        }
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
    fn go_to_save_as_opens_the_cloud_save_page() {
        let mut app = FilmcraftApp::new(filmcraft_engine::Session::default());
        app.cloud.connect(Noop);
        CloudApp::go_to_save_as(&mut app);
        assert_eq!(app.cloud_page, Some(CloudPage::SaveAs));
    }

    #[test]
    fn active_is_addressable_without_a_disk_path() {
        let app = FilmcraftApp::new(filmcraft_engine::Session::default());
        assert!(app.session.path.is_none());
        assert_eq!(CloudApp::active(&app), Some(1));
    }

    #[test]
    fn unavailable_without_a_connected_backend() {
        let app = FilmcraftApp::new(filmcraft_engine::Session::default());
        assert!(!available(&app));
    }

    /// An in-process mock [`CloudBackend`]: answers `Job::List` and `Job::Account` so the Open
    /// page's listing logic runs with no cloud service running.
    struct MockBackend {
        entries: Vec<Entry>,
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

    impl CloudBackend for MockBackend {
        fn submit(&self, job: Job, post: Post) {
            match job {
                Job::Account => post.send(Event::Account(Ok(Account { display: "mock@example.test".into(), ..Default::default() }))),
                Job::List(path) => post.send(Event::Listed { path, entries: Ok(self.entries.clone()) }),
                Job::Read { entry, into } => post.send(Event::Read { entry, into, bytes: Err(Fault::Refused("mock has no project bytes".into())) }),
                _ => {}
            }
        }
    }

    #[test]
    fn mock_backend_lists_the_root_folder() {
        let mut app = FilmcraftApp::new(filmcraft_engine::Session::default());
        app.cloud.connect(MockBackend { entries: vec![mock_entry("demo.fcproj")] });
        start(&mut app);
        pump(&mut app, &egui::Context::default());
        assert!(app.cloud.mine.loaded, "the folder should be loaded after one pump");
        assert_eq!(app.cloud.mine.entries.len(), 1);
        assert_eq!(app.cloud.mine.entries[0].name, "demo.fcproj");
        assert!(matches!(&app.cloud.account, Some(a) if a.display == "mock@example.test"));
    }
}
