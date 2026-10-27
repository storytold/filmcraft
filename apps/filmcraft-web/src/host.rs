//! Host modes: a page that runs FilmCraft (as an `<iframe>`, or in its own document) opens
//! projects in it and stores what it saves instead of the browser downloading. The same
//! messages as the other Craft apps' bridges (WordCraft, GridCraft, PdfCraft), with the
//! `filmcraft:` prefix.
//!
//! ## `?host=parent` — an `<iframe>` (such as a Nextcloud app)
//!
//! The framing page and FilmCraft talk over `postMessage` between same-origin windows. Messages
//! are plain objects with a `type`:
//!
//! - `filmcraft:ready` (to the parent): `{ version }`, sent once the app is running and
//!   listening; send a file now.
//! - `filmcraft:open` (from the parent): `{ name, bytes }`, bytes an `ArrayBuffer` or
//!   `Uint8Array`. A `.fcproj` opens like a project picked with File › Open or dropped on the
//!   page; any other file imports like a dropped one. Over [`MAX_OPEN_BYTES`] (512 MiB, refused
//!   before any byte is copied) or malformed: a message in the app's status bar, never a crash.
//! - `filmcraft:save` (to the parent): `{ name, saveAs, bytes }` (a transferred `ArrayBuffer`)
//!   for every file FilmCraft would otherwise download — see the `saveAs` rule below. The
//!   parent stores it and reports failures itself.
//! - `filmcraft:dirty` (to the parent): `{ dirty }` whenever unsaved changes appear or go away
//!   (the session's own unsaved-changes state, reported only when it changes).
//! - `filmcraft:failed` (to the parent): `{ error }` when the app couldn't start.
//!
//! Only the parent window on the page's own origin is heard or answered. With no parent, or a
//! parent on another origin (messages to it are dropped silently, so saves would vanish while
//! the document looked saved), there is no bridge: the normal app with downloads.
//!
//! ## `data-host="page"` — the page's own document
//!
//! A page that loads the module into its own document (its own `<canvas id="filmcraft_canvas"
//! data-host="page">`, no iframe) gets the same behaviour over DOM `CustomEvent`s on the canvas
//! instead of `postMessage`: the same four events outward (`ready`, `save`, `dirty`, `failed`,
//! with the message fields as the event's `detail`), and the page dispatches `filmcraft:open`
//! on the canvas with `detail: { name, bytes }` to open a file. Events don't bubble, so two
//! editors on one page can't hear each other, and the canvas is the only target FilmCraft
//! listens on.
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

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use filmcraft_engine::Session;
use wasm_bindgen::JsCast as _;
use wasm_bindgen::JsValue;
use wasm_bindgen::closure::Closure;

use crate::hostmode::{MAX_OPEN_BYTES, is_project, is_save_as, open_name, open_size_ok};

/// How a host page is reached. The modes behave the same; they differ only in how an event goes
/// out ([`Transport::send`]) and how an open request comes in ([`Bridge::install`]).
#[derive(Clone)]
enum Transport {
    /// `?host=parent`: `postMessage` between same-origin windows.
    Parent { window: web_sys::Window, parent: web_sys::Window, origin: String },
    /// `data-host="page"`: DOM `CustomEvent`s on the editor's own canvas.
    Page { canvas: web_sys::HtmlCanvasElement },
}

impl Transport {
    /// Hand an event to the host. In parent mode this is a `postMessage` whose object carries a
    /// `type` field; in page mode a `CustomEvent` of that name on the canvas, whose `detail`
    /// holds the fields and which never bubbles (two editors on one page must not hear each
    /// other). `transfer` hands an `ArrayBuffer` to the parent instead of copying it (parent
    /// mode only; the page's document already shares the buffer's bytes).
    fn send(&self, event: &str, fields: &[(&str, JsValue)], transfer: Option<&js_sys::ArrayBuffer>) -> Result<(), String> {
        match self {
            Transport::Parent { parent, origin, .. } => {
                let mut message_fields = Vec::with_capacity(fields.len() + 1);
                message_fields.push(("type", JsValue::from_str(event)));
                message_fields.extend_from_slice(fields);
                let message = message(&message_fields).ok_or_else(|| "couldn't build the message".to_string())?;
                let sent = match transfer {
                    Some(buffer) => parent.post_message_with_transfer(&message, origin, &js_sys::Array::of1(buffer)),
                    None => parent.post_message(&message, origin),
                };
                sent.map_err(|e| js_error(&e, "couldn't post the message"))
            }
            Transport::Page { canvas } => {
                let detail = message(fields).ok_or_else(|| "couldn't build the event detail".to_string())?;
                let init = web_sys::CustomEventInit::new();
                // The builder methods (`bubbles(..)`, `detail(..)`) are deprecated in web-sys
                // 0.3.106 and fail clippy -D warnings; use the setters.
                init.set_bubbles(false);
                init.set_detail(&detail);
                let custom = web_sys::CustomEvent::new_with_event_init_dict(event, &init).map_err(|e| js_error(&e, "couldn't create the event"))?;
                // The bool says whether a listener called `preventDefault`; not a send failure.
                canvas.dispatch_event(&custom).map_err(|e| js_error(&e, "couldn't dispatch the event"))?;
                Ok(())
            }
        }
    }
}

/// The host bridge in host mode. Cloned into the write hook and the open listener; the state
/// they share is behind `Rc`.
#[derive(Clone)]
pub struct Bridge {
    transport: Transport,
    /// Files written since the last frame: their paths (the bytes stay in the file table,
    /// [`crate::fs::mem_bytes`], so a large export is never copied a second time).
    written: Rc<RefCell<Vec<String>>>,
    /// The document file the host knows: the project it opened here, and every file it was
    /// sent with `saveAs: true`.
    host_document: Rc<RefCell<Option<String>>>,
    /// The unsaved-changes state last reported to the host.
    reported: Rc<Cell<Option<bool>>>,
}

impl Bridge {
    /// The parent bridge for `?host=parent`. `None` when the page isn't framed (there is no
    /// parent to talk to), so opening that URL directly stays the normal app with downloads, or
    /// when the parent is on another origin: messages only go to this origin, so the browser
    /// would drop every save without an error while the document showed as saved.
    pub fn new() -> Option<Self> {
        let window = web_sys::window()?;
        let parent = window.parent().ok().flatten()?;
        if js_sys::Object::is(&parent, &window) {
            return None;
        }
        let origin = window.location().origin().ok()?;
        // An opaque origin (a `file://` page in some browsers) can't be a `postMessage` target.
        if origin == "null" {
            return None;
        }
        // Reading another origin's location throws, which is `None` too.
        if parent.location().origin().ok()? != origin {
            return None;
        }
        Some(Self { transport: Transport::Parent { window, parent, origin }, written: Rc::default(), host_document: Rc::default(), reported: Rc::default() })
    }

    /// The page bridge for `data-host="page"`: the host page runs FilmCraft in its own document
    /// and hears the editor's DOM `CustomEvent`s on this canvas.
    pub fn for_page(canvas: &web_sys::HtmlCanvasElement) -> Self {
        Self { transport: Transport::Page { canvas: canvas.clone() }, written: Rc::default(), host_document: Rc::default(), reported: Rc::default() }
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
    /// doc has the rule), and the host hears when unsaved changes appear or go away.
    pub fn sync(&self, session: &Session) {
        self.flush_saves(session);
        let dirty = session.is_dirty();
        if self.reported.get() != Some(dirty) && self.transport.send("filmcraft:dirty", &[("dirty", dirty.into())], None).is_ok() {
            self.reported.set(Some(dirty));
        }
    }

    pub fn post_ready(&self) {
        let _ = self.transport.send("filmcraft:ready", &[("version", env!("CARGO_PKG_VERSION").into())], None);
    }

    /// The app couldn't start (no WebGPU or WebGL2, for example): let the host say so.
    pub fn post_failed(&self, error: &str) {
        let _ = self.transport.send("filmcraft:failed", &[("error", error.into())], None);
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
            match self.post_save(&name, &bytes, save_as) {
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

    fn post_save(&self, name: &str, bytes: &[u8], save_as: bool) -> Result<(), String> {
        // `filmcraft:save` carries the bytes as an `ArrayBuffer`; the parent bridge transfers it.
        let buffer = js_sys::Uint8Array::from(bytes).buffer();
        let fields: [(&str, JsValue); 3] = [("name", name.into()), ("saveAs", save_as.into()), ("bytes", buffer.clone().into())];
        self.transport.send("filmcraft:save", &fields, Some(&buffer))
    }

    /// Hear `filmcraft:open` — from the same-origin parent (a `message` on the window) or from
    /// the host document (a `filmcraft:open` event on the canvas) — and open or import the file
    /// like one the user picked or dropped.
    fn listen(&self) {
        let bridge = self.clone();
        match &self.transport {
            Transport::Parent { window, parent, origin } => {
                let parent = parent.clone();
                let origin = origin.clone();
                let on_message = Closure::<dyn FnMut(web_sys::MessageEvent)>::new(move |event: web_sys::MessageEvent| {
                    if event.origin() != origin || !event.source().is_some_and(|s| js_sys::Object::is(&s, &parent)) {
                        return;
                    }
                    let data = event.data();
                    if string(&data, "type").as_deref() != Some("filmcraft:open") {
                        return;
                    }
                    deliver_open(&bridge, &data);
                });
                if window.add_event_listener_with_callback("message", on_message.as_ref().unchecked_ref()).is_ok() {
                    // The listener lives as long as the page.
                    on_message.forget();
                }
            }
            Transport::Page { canvas } => {
                let on_open = Closure::<dyn FnMut(web_sys::Event)>::new(move |event: web_sys::Event| {
                    // A plain `Event` of the same name carries no detail; only a `CustomEvent`.
                    let Some(open) = event.dyn_ref::<web_sys::CustomEvent>() else { return };
                    deliver_open(&bridge, &open.detail());
                });
                if canvas.add_event_listener_with_callback("filmcraft:open", on_open.as_ref().unchecked_ref()).is_ok() {
                    // The listener lives as long as the page.
                    on_open.forget();
                }
            }
        }
    }
}

/// Handle a `filmcraft:open` payload: `{ name, bytes }`, bytes an `ArrayBuffer` or
/// `Uint8Array` — checked, then opened or imported like a file the user picked or dropped.
fn deliver_open(bridge: &Bridge, data: &JsValue) {
    let Ok(raw) = js_sys::Reflect::get(data, &JsValue::from_str("bytes")) else { return };
    if !raw.is_instance_of::<js_sys::ArrayBuffer>() && raw.dyn_ref::<js_sys::Uint8Array>().is_none() {
        return rejected_open("the bytes are neither an ArrayBuffer nor a Uint8Array".to_string());
    }
    let Some(len) = byte_length(&raw) else { return rejected_open("the bytes have no readable length".to_string()) };
    if !open_size_ok(len) {
        return rejected_open(format!("{len} bytes is over the {} MiB host open limit", MAX_OPEN_BYTES >> 20));
    }
    let name = open_name(string(data, "name").as_deref());
    if is_project(&name) {
        let Some(bytes) = bytes_from(&raw) else { return rejected_open("couldn't read the bytes".to_string()) };
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
        let Some(blob) = blob_from_bytes(&raw) else { return rejected_open("couldn't wrap the bytes in a Blob".to_string()) };
        wasm_bindgen_futures::spawn_local(async move { crate::import::import_blob(name, blob).await });
    }
}

/// An open request the app can't act on: said in the app's status bar, never a panic. Takes
/// the reason owned: it reaches the status bar a frame later.
fn rejected_open(why: String) {
    crate::post(move |w, _| w.app.status(format!("the host's file was not opened: {why}")));
}

/// The payload's `byteLength`, read as a number: the bindings' `u32` would wrap a 4 GiB view
/// to 0 and let it past the size check.
fn byte_length(v: &JsValue) -> Option<u64> {
    let n = js_sys::Reflect::get(v, &JsValue::from_str("byteLength")).ok()?.as_f64()?;
    if !n.is_finite() || n < 0.0 {
        return None;
    }
    Some(n as u64)
}

/// The payload as Rust bytes (a project opens from them): one copy, from an `ArrayBuffer` or a
/// `Uint8Array`.
fn bytes_from(v: &JsValue) -> Option<Vec<u8>> {
    if let Some(array) = v.dyn_ref::<js_sys::Uint8Array>() {
        Some(array.to_vec())
    } else if v.is_instance_of::<js_sys::ArrayBuffer>() {
        Some(js_sys::Uint8Array::new(v).to_vec())
    } else {
        None
    }
}

/// The payload as a `Blob` (media registers like a dropped file): a view over the bytes, no
/// copy on the wasm side — the `Blob` constructor copies into its own store.
fn blob_from_bytes(v: &JsValue) -> Option<web_sys::Blob> {
    let parts = js_sys::Array::new();
    if let Some(view) = v.dyn_ref::<js_sys::Uint8Array>() {
        parts.push(view);
    } else if v.is_instance_of::<js_sys::ArrayBuffer>() {
        parts.push(&js_sys::Uint8Array::new(v));
    } else {
        return None;
    }
    let opts = web_sys::BlobPropertyBag::new();
    opts.set_type("application/octet-stream");
    web_sys::Blob::new_with_u8_array_sequence_and_options(&parts, &opts).ok()
}

fn file_name(path: &str) -> String {
    std::path::Path::new(path).file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_else(|| path.to_string())
}

/// A message object or `CustomEvent` detail with the given fields, or `None` if one couldn't be
/// built (a field that isn't a valid property name, say).
fn message(fields: &[(&str, JsValue)]) -> Option<js_sys::Object> {
    let m = js_sys::Object::new();
    for (key, value) in fields {
        js_sys::Reflect::set(&m, &JsValue::from_str(key), value).ok()?;
    }
    Some(m)
}

fn string(data: &JsValue, key: &str) -> Option<String> {
    js_sys::Reflect::get(data, &JsValue::from_str(key)).ok()?.as_string()
}

/// A `JsValue` error as a string, after `what` went wrong.
fn js_error(e: &JsValue, what: &str) -> String {
    format!("{what}: {}", e.as_string().unwrap_or_else(|| format!("{e:?}")))
}
