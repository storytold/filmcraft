//! Host modes: a page that runs FilmCraft (as an `<iframe>`, or in its own document) opens
//! projects in it and stores what it saves instead of the browser downloading. The messages and
//! the two transports are `craft-web-host`'s (storytold/craft-libs), the same contract as the
//! other Craft apps' bridges, with the `filmcraft:` prefix. This module is FilmCraft's part of it.
//!
//! - `?host=parent` (an `<iframe>`, such as a Nextcloud app) talks to the same-origin parent over
//!   `postMessage`; `data-host="page"` on the canvas (the page's own document) uses DOM
//!   `CustomEvent`s on the canvas instead. `?host=parent` wins when both are present. With no
//!   parent, or a parent on another origin, there is no bridge: the normal app with downloads.
//! - `filmcraft:ready` once the app is running and listening; send a file now. FilmCraft reads
//!   bytes, so it says `file: false` and a host sends `bytes`.
//! - `filmcraft:open` (from the host): `{ name, bytes }`. A `.fcproj` opens like a project picked
//!   with File › Open or dropped on the page; any other file imports like a dropped one. Over
//!   [`MAX_OPEN_BYTES`] (512 MiB), or a project over [`PROJECT_OPEN_MAX`] (256 MiB, the whole-file
//!   read), refused before any byte is copied; anything malformed is a message in the app's
//!   status bar, never a crash.
//! - `filmcraft:save` (to the host): `{ name, saveAs, bytes }` for every file FilmCraft would
//!   otherwise download — see the `saveAs` rule below.
//! - `filmcraft:dirty` whenever unsaved changes appear or go away (the session's own
//!   unsaved-changes state), `filmcraft:failed` when the app couldn't start.
//!
//! ## The `saveAs` rule
//!
//! Every file the app writes for the user — saved projects, video/frame/audio exports,
//! captions, interchange documents (AAF, OMF, EDL, markers), extracted audio, the schema
//! backup kept beside an upgraded project — is queued from the write hook
//! ([`Bridge::install`]) and handed to the host at the next frame ([`Bridge::sync`]), when the
//! frame's commands have settled and the session's state is final. Nothing reaches the
//! browser's download in host mode. `saveAs` says whether the file becomes the document's file
//! — the project the next Save writes ([`is_save_as`], pure and unit-tested):
//!
//! - A write the session adopts as its project path (Save As; also the first Save of an
//!   untitled project, which the UI runs as Save As under a picked name) that the host does
//!   not already have as the document: `true`. From then on, Saves of that document carry its
//!   name with `false`.
//! - A Save of the document under its own name — including one the host opened, and a Save As
//!   back to the file the host already has as the document: `false`.
//! - Everything else — Save a Copy, exports, captions, interchange, extracted audio, the
//!   schema backup: `false`. They are copies; the document's file stays the same.
//!
//! "The document the host has" is what the host can know: the project it opened over this
//! bridge, and every file it received with `saveAs: true`. A project opened inside the app
//! (File › Open) is new to the host, so its first Save goes out with `true`.
//!
//! In host mode the host sends the document to edit, so no demo project opens and nothing from
//! an earlier visit is recovered (no OPFS snapshot, no restored media), saves are handed to the
//! host rather than downloaded, and the host guards leaving the page while changes are
//! unsaved. FilmCraft has no user names that work could be signed with, so there is no
//! `?author=`.

use std::cell::RefCell;
use std::rc::Rc;

use craft_web_host::web::{FileData, Host, Message};
use filmcraft_engine::Session;

use crate::hostmode::{MAX_OPEN_BYTES, PROJECT_OPEN_MAX, is_project, is_save_as, open_name, open_size_ok, project_open_ok};

/// The app id in the host messages (`filmcraft:ready`).
const APP: &str = "filmcraft";

/// The host bridge in host mode. Cloned into the write hook and the open listener; the state
/// they share is behind `Rc`.
#[derive(Clone)]
pub struct Bridge {
    host: Rc<Host>,
    /// Files written since the last frame: their paths (the bytes stay in the file table,
    /// [`crate::fs::mem_bytes`], so a large export is never copied a second time).
    written: Rc<RefCell<Vec<String>>>,
    /// The document file the host knows: the project it opened here, and every file it was
    /// sent with `saveAs: true`.
    host_document: Rc<RefCell<Option<String>>>,
}

impl Bridge {
    /// The bridge for this page, or `None` when FilmCraft isn't hosted: no `?host=parent` and no
    /// `data-host="page"` on the canvas, an unframed `?host=parent` page (it stays the normal app
    /// with downloads), or a parent on another origin.
    pub fn connect(canvas: &web_sys::HtmlCanvasElement) -> Option<Self> {
        Some(Self { host: Rc::new(Host::connect(APP, canvas)?), written: Rc::default(), host_document: Rc::default() })
    }

    /// Install the bridge: hear the host's files, and hand the app's writes to it instead of
    /// the browser's download.
    pub fn install(&self) {
        self.listen();
        let bridge = self.clone();
        crate::fs::set_on_write(move |path: &str, _bytes: &[u8]| bridge.wrote(path));
    }

    /// After each frame's commands (the web app's per-frame logic pass): files written this
    /// frame go to the host, with the `saveAs` decision from the session's state (the module
    /// doc has the rule), and the host hears when unsaved changes appear or go away (the crate
    /// does not repeat a state it already reported).
    pub fn sync(&self, session: &Session) {
        self.flush_saves(session);
        self.host.post_dirty(session.is_dirty());
    }

    pub fn post_ready(&self) {
        let _ = self.host.post_ready(env!("CARGO_PKG_VERSION"), false);
    }

    /// The app couldn't start (no WebGPU or WebGL2, for example): let the host say so.
    pub fn post_failed(&self, error: &str) {
        let _ = self.host.post_failed(error, None);
    }

    /// The host's project opened: the host has this file as the document, so a Save of it goes
    /// out under its own name (`saveAs: false`).
    fn opened_document(&self, path: &str) {
        *self.host_document.borrow_mut() = Some(path.to_string());
    }

    /// A file the app wrote (saved project, export, …): queued for [`Bridge::sync`], and a
    /// repaint in case the write happened outside a frame.
    fn wrote(&self, path: &str) {
        self.written.borrow_mut().push(path.to_string());
        crate::repaint();
    }

    /// Hand this frame's written files to the host. The `saveAs` decision needs the session's
    /// state *after* the frame's commands (a Save As adopts its path only once the write has
    /// happened), so it is made here, not in the write hook, one file at a time against the
    /// host's document as the earlier hand-overs left it.
    fn flush_saves(&self, session: &Session) {
        let written = std::mem::take(&mut *self.written.borrow_mut());
        if written.is_empty() {
            return;
        }
        let current = session.path.clone();
        for path in written {
            let Some(bytes) = crate::fs::mem_bytes(&path) else {
                log::warn!("host save {path}: the written bytes are gone");
                continue;
            };
            let name = file_name(&path);
            // Each save is classified against the host's document as it is now, after the
            // previous hand-overs (a failed one changed nothing), and the borrow is dropped before
            // sending: in page mode a host listener reacts to a save synchronously, and may open
            // a file right back.
            let before = self.host_document.borrow().clone();
            let save_as = is_save_as(&path, before.as_deref(), current.as_deref());
            match self.host.post_save(&name, &bytes, None, save_as) {
                // The host has this file as the document now, unless it opened another one while
                // handling the save; a failed hand-over changes nothing, so a retry is still a Save As.
                Ok(()) if save_as => {
                    let mut document = self.host_document.borrow_mut();
                    if *document == before {
                        *document = Some(path);
                    }
                }
                Ok(()) => {}
                Err(e) => log::error!("couldn't hand {name} to the host: {e}"),
            }
        }
    }

    /// Hear `filmcraft:open` and open or import the file like one the user picked or dropped.
    fn listen(&self) {
        let bridge = self.clone();
        self.host.listen(move |message| {
            if let Message::Open { name, data } = message {
                deliver_open(&bridge, &name, &data);
            }
        });
    }
}

/// Handle a `filmcraft:open` payload: checked, then opened or imported like a file the user
/// picked or dropped.
fn deliver_open(bridge: &Bridge, name: &str, data: &FileData) {
    let FileData::Bytes(array) = data else {
        return rejected_open("FilmCraft reads bytes, not a File".to_string());
    };
    // The data's own length, as a number: a 4 GiB view must not wrap to 0 and pass the check.
    let len = data.size() as u64;
    if !open_size_ok(len) {
        return rejected_open(format!("{len} bytes is over the {} MiB host open limit", MAX_OPEN_BYTES >> 20));
    }
    let name = open_name(name);
    if is_project(&name) {
        // A project is read whole, and that read stops at 256 MiB. Refuse a larger one before
        // copying it into the file table, where the open would then fail.
        if !project_open_ok(len) {
            return rejected_open(format!("{len} bytes is over the {} MiB limit for reading a project whole", PROJECT_OPEN_MAX >> 20));
        }
        let bytes = array.to_vec();
        let path = format!("/projects/{name}");
        let opened = bridge.clone();
        wasm_bindgen_futures::spawn_local(async move {
            match crate::import::open_project_bytes(path.clone(), bytes).await {
                // The host has this file as the document now: a Save of it goes out under its
                // own name (`saveAs: false`).
                Ok(_) => opened.opened_document(&path),
                Err(e) => crate::post(move |w, _| w.app.status(e)),
            }
        });
    } else {
        // Media registers like a dropped file: a `Blob` over the bytes, never copied whole.
        let Some(blob) = blob_from_bytes(array) else { return rejected_open("couldn't wrap the bytes in a Blob".to_string()) };
        wasm_bindgen_futures::spawn_local(async move { crate::import::import_blob(name, blob).await });
    }
}

/// An open request the app can't act on: said in the app's status bar, never a panic. Takes
/// the reason owned: it reaches the status bar a frame later.
fn rejected_open(why: String) {
    crate::post(move |w, _| w.app.status(format!("the host's file was not opened: {why}")));
}

/// The payload as a `Blob` (media registers like a dropped file): a view over the bytes, no
/// copy on the wasm side — the `Blob` constructor copies into its own store.
fn blob_from_bytes(view: &js_sys::Uint8Array) -> Option<web_sys::Blob> {
    let parts = js_sys::Array::new();
    parts.push(view);
    let opts = web_sys::BlobPropertyBag::new();
    opts.set_type("application/octet-stream");
    web_sys::Blob::new_with_u8_array_sequence_and_options(&parts, &opts).ok()
}

fn file_name(path: &str) -> String {
    std::path::Path::new(path).file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_else(|| path.to_string())
}
