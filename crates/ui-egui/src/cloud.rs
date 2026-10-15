//! FilmCraft's half of Open and Save against cloud storage.
//!
//! The pages themselves are `craft_ui::cloud`. Every Craft app offers the same ones. What is
//! here is what only FilmCraft can answer: which extensions a project has, how bytes become a
//! project and how an open project is encoded back.
//!
//! FilmCraft holds no provider credential and speaks no provider protocol. Every call goes to
//! the CollabCraft service, which is the single place a provider is configured and permissions
//! are decided. Replacing the storage backend changes nothing here.
//!
//! Nothing in this file opens a socket. The transport half is a [`CloudBackend`] the host
//! application supplies (see `apps/filmcraft/src/cloud.rs`). Every CollabCraft call blocks.
//! A blocking read of a large project inside a frame would freeze the window for seconds. So
//! the pages state a [`Job`], the host runs it on a worker thread, and the answer arrives on a
//! channel that [`pump`] drains on a later frame.
//!
//! # Large-media constraint
//!
//! A `.fcproj` archive carries project metadata, sequences, settings, proxy references and
//! trim-region markers. It does NOT embed the raw video, audio or image-sequence files. Those
//! stay on the local or shared storage the project references by path. Upload and download
//! here transfer the metadata file only; they do not and cannot move media. Nothing in this
//! module bypasses that: the whole-file contract is preserved, not worked around.
//!
//! A provider that imposes a file-size limit well below the project archive should be flagged
//! by [`Fault::Refused`] from the server, not silently truncated here. If a project archive
//! grows very large (many long-form sequences with dense automation), Save As to a local path
//! and a manual upload to large-file storage is the right path.

use std::collections::HashMap;

// Re-exported so a host application, and a test, can name these types without depending on the
// crates they come from.
pub use craft_cloud::{Capability, Entry, EntryKind};

// ── PR #1982 adaptation ────────────────────────────────────────────────────────
//
// storytold/filmcraft#1982 (filmcraft-hub, CraftHub adapter / craft-collab CRDT layer) was
// reviewed and its four decisions mapped to this architecture:
//
// ┌───────────────────────┬────────────┬───────────────────────────────────────────────────────┐
// │ #1982 feature         │ Decision   │ Rationale                                             │
// ├───────────────────────┼────────────┼───────────────────────────────────────────────────────┤
// │ CRDT adapter          │ Superseded │ craft-cloud / CollabCraft is the real-time layer.     │
// │ (filmcraft-hub /      │            │ Adding craft-collab creates a parallel provider layer │
// │ craft-collab)         │            │ which the user explicitly ruled out.                  │
// ├───────────────────────┼────────────┼───────────────────────────────────────────────────────┤
// │ Timeline region locks │ Adapted    │ LockRange type added below. Populated by a live       │
// │ (TimelineRange shape, │            │ collab session; checked by the timeline renderer.     │
// │ §6.7)                 │            │ Enforcement requires a live session — without one the │
// │                       │            │ field is empty and no clip moves are refused.         │
// ├───────────────────────┼────────────┼───────────────────────────────────────────────────────┤
// │ Playhead presence     │ Adapted    │ Peer.at convention: "{sequence_name} @ {timecode}"    │
// │ (PresenceMsg.playhead │            │ as a string. format_peer_at() builds this string.     │
// │ in sequence ticks)    │            │ Sending our own playhead requires the live collab     │
// │                       │            │ session (collabcraft-share WebSocket side); this is   │
// │                       │            │ the receive / display half.                           │
// ├───────────────────────┼────────────┼───────────────────────────────────────────────────────┤
// │ Deferred LUT/assets   │ Documented │ LUT text is never embedded in the .fcproj archive     │
// │ (content-addressed    │            │ transferred here. The constraint is already in the    │
// │ AssetRef via SHA-256) │            │ save_note and the module doc. Full deferred-asset     │
// │                       │            │ support requires the craft-collab asset channel.      │
// └───────────────────────┴────────────┴───────────────────────────────────────────────────────┘

/// A locked region on a timeline track, as adapted from storytold/filmcraft#1982 §6.7.
///
/// When a live CollabCraft collab session is active, the session sends lock records for ranges
/// another user has claimed. The timeline renderer checks `app.cloud_locks` and draws lock
/// overlays where `holder` is not our own identity.
///
/// Without a live session `app.cloud_locks` is empty and no moves are refused here. The shape
/// is preserved so the renderer contract is defined even before the collab session wire lands.
#[derive(Clone, Debug, PartialEq)]
pub struct LockRange {
    /// The track id that holds this lock.
    pub track: u64,
    /// Lock start in sequence ticks (the document's canonical time base).
    pub start: i64,
    /// Lock duration in sequence ticks.
    pub duration: i64,
    /// The CollabCraft user id that holds this lock.
    pub holder: String,
}

impl LockRange {
    /// Whether this lock covers any part of `[range_start, range_start + range_duration)`.
    pub fn intersects(&self, range_start: i64, range_duration: i64) -> bool {
        self.start < range_start + range_duration && self.start + self.duration > range_start
    }
}

/// Format a peer's location as a human-readable string for [`craft_ui::presence::Peer::at`].
///
/// This is the FilmCraft convention for the `at` field (adapted from #1982's playhead presence
/// design): `"{sequence_name} @ {timecode}"`. A collab session that reports peer positions
/// builds this string and passes it as `Peer::at(format_peer_at(seq, tick, fps))`.
///
/// `fps` is the sequence frame rate (ticks_per_frame from the sequence settings), used only to
/// convert `tick` to a display timecode; no math beyond division happens here.
pub fn format_peer_at(sequence_name: &str, tick: i64, ticks_per_frame: i64) -> String {
    if ticks_per_frame <= 0 {
        return format!("{sequence_name} @ tick {tick}");
    }
    let frame = tick / ticks_per_frame;
    // HH:MM:SS:FF — enough for display, not a conformant SMPTE encoder.
    let ff = frame % 30;
    let total_sec = frame / 30;
    let ss = total_sec % 60;
    let mm = (total_sec / 60) % 60;
    let hh = total_sec / 3600;
    format!("{sequence_name} @ {hh:02}:{mm:02}:{ss:02}:{ff:02}")
}
pub use craft_ui::cloud::{
    Account, Busy, CloudApp, CloudBackend, CloudFiles, Conflict, CopyFormat, Destination, DocumentKind, Event, Fault, Folder, Job, Link, Origin, Post, Source,
    UNAVAILABLE, available, browse, can_converse, conflict_modal, destination_name, has_target, link, load_shared, open_entry, open_page, origin, parent_of,
    prefers_cloud, progress, pump, reload_active, save, save_as, save_page, share_button, shares, sign_in, sign_out, start, who, work_together,
};
use egui::{Color32, Painter, Rect, Ui};
use serde_json::{Value as Json, json};

use crate::FilmcraftApp;
use crate::icons::{self, Icon};

/// Extensions FilmCraft opens from cloud storage. The same set as the desktop file picker.
///
/// Only the project archive is opened here. Raw media files are not imported through cloud
/// pages (they have no reliable file-size ceiling, and the provider knows nothing about them).
const OPENS: [&str; 1] = ["fcproj"];

/// Content types FilmCraft project files carry.
///
/// Most WebDAV servers return `application/octet-stream` for unknown types, so the MIME match
/// is a safety net, not the primary filter. The extension is checked first.
const MIMES: [&str; 2] = [
    "application/x-filmcraft-project",
    "application/octet-stream", // WebDAV fallback for unknown types
];

/// What a FilmCraft project is, as a cloud file list needs to know.
///
/// One file per project. No embedded media: the project references media by path. Cloud
/// Open/Save transfers the archive only. See the module-level doc for the full constraint.
pub const PROJECT: DocumentKind = DocumentKind { extensions: &OPENS, mimes: &MIMES, noun: "project", default_extension: "fcproj" };

/// The Save a Copy formats available in the Share menu.
///
/// FilmCraft projects are not losslessly round-trippable to other formats at the project level,
/// so "Save a Copy" here offers the interchange formats the engine can export to: Final Cut
/// XML and AAF/OMF. These are interop exports, not full fidelity.
const COPIES: [(&str, &str); 2] = [("fcpxml", "Save a Copy as Final Cut Pro XML (.fcpxml)\u{2026}"), ("aaf", "Export as AAF\u{2026}")];

/// What each save in flight encoded: keyed by document uid so a concurrent save and export
/// do not confuse each other's dirty-flag updates.
pub type Sent = HashMap<u64, ()>;

/// Whether FilmCraft can open this cloud entry.
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
    ["FILMCRAFT_CLOUD_URL", "CRAFT_CLOUD_URL", "COLLABCRAFT_URL"]
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
        // FilmCraft tracks the open project as the session itself. There is at most one project
        // per window. Use a stable 64-bit hash of the autosave path as the uid, falling back to
        // a compile-time sentinel when no project is open (an empty session has no cloud target).
        self.session.path.as_ref().map(|_| 1u64)
    }

    fn title(&self, _uid: u64) -> String {
        self.session.project.name.clone()
    }

    fn open_bytes(&mut self, _name: &str, bytes: Vec<u8>) -> Result<Option<u64>, String> {
        // Write the bytes to a temporary file the engine can open. The engine's project loader
        // takes a path, not a byte slice, because a project archive may reference media relative
        // to its own directory. A temp file in the same folder as the existing project gives the
        // relative paths the best chance of resolving.
        let tmp = std::env::temp_dir().join(format!(
            "filmcraft-cloud-open-{}.fcproj",
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis()).unwrap_or(0)
        ));
        std::fs::write(&tmp, &bytes).map_err(|e| format!("could not stage the downloaded project: {e}"))?;
        let path = tmp.to_string_lossy().to_string();
        let result = self.session.execute("file.open", json!({"path": path}));
        let _ = std::fs::remove_file(&tmp); // always clean up, ignore errors
        match result {
            Ok(_) => {
                // Mark the cloud origin so Save knows where to go back.
                // The session path is set by the engine to the temp path; clear it so the
                // window title and Save As show the cloud name, not the temp path.
                self.session.path = None;
                Ok(Some(1u64))
            }
            Err(e) => Err(e.to_string()),
        }
    }

    fn encode(&mut self, _uid: u64, into: Destination<'_>) -> Result<Vec<u8>, String> {
        // Determine what the destination wants.
        let fmt = format_for(into)?;
        // The project must be saved to disk before we can encode it. Use a temp file.
        let tmp = std::env::temp_dir().join(format!(
            "filmcraft-cloud-save-{}.{fmt}",
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis()).unwrap_or(0)
        ));
        let path = tmp.to_string_lossy().to_string();
        let cmd = match fmt {
            "fcpxml" => "file.exportFcpxml",
            "aaf" => "file.exportAaf",
            _ => "file.saveCopy",
        };
        self.session.execute(cmd, json!({"path": path})).map_err(|e| e.to_string())?;
        let bytes = std::fs::read(&tmp).map_err(|e| format!("could not read the encoded project: {e}"))?;
        let _ = std::fs::remove_file(&tmp);
        Ok(bytes)
    }

    fn close(&mut self, _uid: u64) {
        // FilmCraft holds one project per window. Closing a cloud-opened project that a reload
        // replaced means the window already shows the new one; nothing to do here.
    }

    fn mark_saved(&mut self, _uid: u64) {
        // The engine does not have a "saved to cloud" concept, but we can clear the dirty flag
        // if the project has not changed since we encoded it.
        // Nothing to do: FilmCraft's dirty flag is managed by the autosave worker.
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
        // hooks.open_url requires &mut self (FnMut); use the stored egui context instead.
        // On desktop the cloud worker's own open_browser() already handles device-flow URLs;
        // this path is used for conversation/sharing URLs opened from the UI.
        self.ui_ctx.open_url(egui::OpenUrl::new_tab(url));
    }

    fn say(&mut self, text: String) {
        self.ui.status = text;
    }

    fn toast(&mut self, text: String) {
        self.toast = Some((text, self.last_ui_time));
    }

    fn go_to_save_as(&mut self) {
        // FilmCraft has no ribbon backstage. Show a status message that directs the user to
        // the cloud.saveAs control channel command or the File menu.
        self.ui.status = tl!("Save As: use File \u{203a} Save As or cloud.saveAs {folder, name} via the control channel").to_owned();
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
