//! The `postMessage` bridge to a same-origin parent page (`?host=parent`), for pages that embed
//! FilmCraft in an `<iframe>` (such as a Nextcloud app). Messages are plain objects with a `type`:
//!
//! - `filmcraft:ready` (to the parent): `{ version }`, the app is listening; send a file now.
//! - `filmcraft:open` (from the parent): `{ name, path, blob }` (or `bytes`: an `ArrayBuffer` or
//!   `Uint8Array`). `path` is where the file lives on the parent's side ("/Videos/clip.mp4"); it
//!   is kept in the virtual file table under [`PREFIX`]. A project (`.fcproj`) opens; any other
//!   file is imported into a new project named after it, with a sequence made from it.
//! - `filmcraft:need` (to the parent): `{ paths }`, files an opened project refers to that are on
//!   the parent's side and not here yet (its media).
//! - `filmcraft:file` (from the parent): `{ path, blob }` one of them, or `{ path, error }` when it
//!   can't be had (that media stays offline; File ▸ Link Media… reconnects it).
//! - `filmcraft:save` (to the parent): `{ name, bytes }` (a transferred `ArrayBuffer`) for every
//!   file FilmCraft writes (saved projects, exports): `name` is the parent's path for a file
//!   under [`PREFIX`], else just the file name (a new project, an export), for the parent to ask
//!   where it goes. The parent stores it and reports failures itself.
//! - `filmcraft:dirty` (to the parent): `{ dirty }` whenever unsaved changes appear or go away.
//! - `filmcraft:failed` (to the parent): `{ error }` when the app couldn't start.
//!
//! Only the parent window on the page's own origin is heard or answered. In host mode the page
//! opens neither the demo project nor the recovery snapshot, and the parent guards leaving it.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;

use serde_json::json;
use wasm_bindgen::JsCast as _;
use wasm_bindgen::JsValue;
use wasm_bindgen::closure::Closure;
use wasm_bindgen_futures::JsFuture;

use crate::{fs, import};

/// Where the parent's files live in the virtual file table: "/Videos/clip.mp4" there is
/// "/host/Videos/clip.mp4" here.
pub const PREFIX: &str = "/host";

/// How long to wait for the media of a project to arrive before opening it anyway.
const MEDIA_WAIT_MS: i32 = 30 * 60 * 1000;

/// What the parent answered for a requested file.
enum Answer {
    Pending,
    Here,
    Missing,
}

thread_local! {
    static BRIDGE: RefCell<Option<Bridge>> = const { RefCell::new(None) };
    /// Files asked of the parent (by path here), and what came of each.
    static NEEDED: RefCell<HashMap<String, Answer>> = RefCell::new(HashMap::new());
    /// The unsaved-changes state last reported to the parent.
    static REPORTED: Cell<Option<bool>> = const { Cell::new(None) };
}

#[derive(Clone)]
pub struct Bridge {
    window: web_sys::Window,
    parent: web_sys::Window,
    origin: String,
}

/// The query parameter `key`, decoded.
fn query_param(key: &str) -> Option<String> {
    let search = web_sys::window()?.location().search().ok()?;
    web_sys::UrlSearchParams::new_with_str(&search).ok()?.get(key)
}

/// Whether the page runs embedded (`?host=parent` inside a frame).
pub fn embedded() -> bool {
    query_param("host").as_deref() == Some("parent") && Bridge::new().is_some()
}

impl Bridge {
    /// `None` when the page isn't framed (there is no parent to talk to).
    fn new() -> Option<Self> {
        let window = web_sys::window()?;
        let parent = window.parent().ok().flatten()?;
        if js_sys::Object::is(&parent, &window) {
            return None;
        }
        let origin = window.location().origin().ok()?;
        Some(Self { window, parent, origin })
    }

    fn post(&self, fields: &[(&str, JsValue)]) -> bool {
        message(fields).is_some_and(|m| self.parent.post_message(&m, &self.origin).is_ok())
    }
}

/// Embedded: route written files to the parent and hear files from it, then say the app is ready.
/// Call once the app runs (after [`fs::set_on_write`] would be installed).
pub fn start() -> bool {
    if query_param("host").as_deref() != Some("parent") {
        return false;
    }
    let Some(bridge) = Bridge::new() else { return false };
    fs::set_on_write(|path, data| {
        if let Err(e) = post_save(path, data) {
            log::warn!("couldn't hand {path} to the page: {e}");
        }
    });
    listen(&bridge);
    bridge.post(&[("type", "filmcraft:ready".into()), ("version", env!("CARGO_PKG_VERSION").into())]);
    BRIDGE.with(|b| *b.borrow_mut() = Some(bridge));
    true
}

/// Embedded and FilmCraft couldn't start: tell the parent.
pub fn failed(error: &str) {
    if query_param("host").as_deref() == Some("parent")
        && let Some(bridge) = Bridge::new()
    {
        bridge.post(&[("type", "filmcraft:failed".into()), ("error", error.into())]);
    }
}

/// After each frame: tell the parent when unsaved changes appear or go away.
pub fn report_dirty(dirty: bool) {
    if REPORTED.with(Cell::get) == Some(dirty) {
        return;
    }
    let sent = BRIDGE.with(|b| b.borrow().as_ref().is_some_and(|b| b.post(&[("type", "filmcraft:dirty".into()), ("dirty", dirty.into())])));
    if sent {
        REPORTED.with(|r| r.set(Some(dirty)));
    }
}

/// The name the parent gets for a file written at `path` here.
fn parent_name(path: &str) -> String {
    match path.strip_prefix(PREFIX).filter(|p| p.starts_with('/')) {
        Some(theirs) => theirs.to_string(),
        None => path.rsplit('/').next().unwrap_or(path).to_string(),
    }
}

fn post_save(path: &str, data: &[u8]) -> Result<(), String> {
    let bridge = BRIDGE.with(|b| b.borrow().clone()).ok_or("the page that embeds FilmCraft is gone")?;
    let buffer = js_sys::Uint8Array::from(data).buffer();
    let message = message(&[("type", "filmcraft:save".into()), ("name", parent_name(path).into()), ("bytes", buffer.clone().into())])
        .ok_or("couldn't build the message")?;
    bridge.parent.post_message_with_transfer(&message, &bridge.origin, &js_sys::Array::of1(&buffer)).map_err(|e| format!("{e:?}"))
}

fn listen(bridge: &Bridge) {
    let parent = bridge.parent.clone();
    let origin = bridge.origin.clone();
    let on_message = Closure::<dyn FnMut(web_sys::MessageEvent)>::new(move |event: web_sys::MessageEvent| {
        if event.origin() != origin || !event.source().is_some_and(|s| js_sys::Object::is(&s, &parent)) {
            return;
        }
        let data = event.data();
        match string(&data, "type").as_deref() {
            Some("filmcraft:open") => {
                let Some(blob) = blob(&data) else { return };
                let name = string(&data, "name").filter(|n| !n.trim().is_empty()).unwrap_or_else(|| "Media".to_string());
                let theirs = string(&data, "path").filter(|p| p.starts_with('/')).unwrap_or_else(|| format!("/{name}"));
                wasm_bindgen_futures::spawn_local(open(name, theirs, blob));
            }
            Some("filmcraft:file") => {
                let Some(theirs) = string(&data, "path").filter(|p| p.starts_with('/')) else { return };
                let here = format!("{PREFIX}{theirs}");
                let answer = match blob(&data) {
                    Some(blob) => {
                        fs::put_blob(&here, blob);
                        Answer::Here
                    }
                    None => Answer::Missing,
                };
                NEEDED.with(|n| {
                    if let Some(slot) = n.borrow_mut().get_mut(&here) {
                        *slot = answer;
                    }
                });
                crate::repaint();
            }
            _ => {}
        }
    });
    if bridge.window.add_event_listener_with_callback("message", on_message.as_ref().unchecked_ref()).is_ok() {
        // The listener lives as long as the page.
        on_message.forget();
    }
}

/// Open a file the parent sent: a project with its media, or a clip in a new project.
async fn open(name: String, theirs: String, blob: web_sys::Blob) {
    let here = format!("{PREFIX}{theirs}");
    let outcome = if name.to_ascii_lowercase().ends_with(".fcproj") { open_project(here, blob).await } else { open_media(name, here, blob).await };
    if let Err(e) = outcome {
        crate::post(move |w, _| w.app.status(e));
    }
}

async fn open_media(name: String, here: String, blob: web_sys::Blob) -> Result<(), String> {
    let size = blob.size() as u64;
    fs::put_blob(&here, blob);
    let stem = name.rsplit_once('.').map_or(name.as_str(), |(stem, _)| stem).to_string();
    import::on_ui(move |w, _| w.app.session.execute("file.newProject", json!({ "name": stem })).map(|_| ()).map_err(|e| e.to_string())).await?;
    let r = import::import_registered(here, size).await?;
    if let Some(errors) = r.get("errors").and_then(|e| e.as_array()).filter(|e| !e.is_empty()) {
        let msg = errors.iter().filter_map(|e| e.as_str()).collect::<Vec<_>>().join("; ");
        return Err(msg);
    }
    // A sequence made from the clip, as dropping it on the empty timeline does, and nothing to
    // save yet: the new project holds only what the user opened.
    let item = r.get("items").and_then(|i| i.as_array()).and_then(|i| i.first()).and_then(|i| i.as_u64());
    import::on_ui(move |w, _| {
        if let Some(item) = item
            && let Err(e) = w.app.session.execute("file.newSequence", json!({ "fromItem": item }))
        {
            w.app.status(e.to_string());
        }
        w.app.session.saved_revision = w.app.session.revision;
    })
    .await;
    Ok(())
}

async fn open_project(here: String, blob: web_sys::Blob) -> Result<(), String> {
    let buffer = JsFuture::from(blob.array_buffer()).await.map_err(|e| format!("{e:?}"))?;
    let bytes = js_sys::Uint8Array::new(&buffer).to_vec();
    // The media it refers to that are on the parent's side and not here yet.
    let loaded = filmcraft_format::decode(&bytes).map_err(|e| format!("{here}: {e}"))?;
    let wanted: Vec<String> = filmcraft_engine::relink::scan(&loaded.project, &fs::WebServices)
        .into_iter()
        .filter(|m| m.status == "missing" && m.path.starts_with(&format!("{PREFIX}/")))
        .map(|m| m.path)
        .collect();
    fs::put(&here, bytes);
    if !wanted.is_empty() {
        fetch_media(wanted).await;
    }
    import::on_ui(move |w, _| {
        let opened = w.app.session.execute("file.open", json!({ "path": here.clone() })).map(|_| ()).map_err(|e| e.to_string());
        if opened.is_ok() {
            let name = here.rsplit('/').next().unwrap_or(&here).to_string();
            w.app.status(format!("Opened {name}"));
        }
        opened
    })
    .await
}

/// Ask the parent for `wanted` (paths here) and wait until each has arrived or can't be had.
async fn fetch_media(wanted: Vec<String>) {
    NEEDED.with(|n| {
        let mut n = n.borrow_mut();
        for path in &wanted {
            n.insert(path.clone(), Answer::Pending);
        }
    });
    let count = wanted.len();
    crate::post(move |w, _| w.app.status(format!("Loading {count} media file(s) from the server…")));
    let theirs: Vec<JsValue> = wanted.iter().map(|p| JsValue::from_str(&parent_name(p))).collect();
    let paths = js_sys::Array::from_iter(theirs);
    let asked = BRIDGE.with(|b| b.borrow().as_ref().is_some_and(|b| b.post(&[("type", "filmcraft:need".into()), ("paths", paths.into())])));
    let mut waited = 0;
    while asked && waited < MEDIA_WAIT_MS && NEEDED.with(|n| wanted.iter().any(|p| matches!(n.borrow().get(p), Some(Answer::Pending)))) {
        fs::sleep(100).await;
        waited += 100;
    }
    NEEDED.with(|n| {
        let mut n = n.borrow_mut();
        for path in &wanted {
            n.remove(path);
        }
    });
}

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

/// The file in a message: `blob` (a `Blob`), or `bytes` (an `ArrayBuffer` or `Uint8Array`).
fn blob(data: &JsValue) -> Option<web_sys::Blob> {
    if let Ok(b) = js_sys::Reflect::get(data, &JsValue::from_str("blob"))
        && let Ok(blob) = b.dyn_into::<web_sys::Blob>()
    {
        return Some(blob);
    }
    let bytes = js_sys::Reflect::get(data, &JsValue::from_str("bytes")).ok()?;
    let array = if let Some(a) = bytes.dyn_ref::<js_sys::Uint8Array>() {
        a.clone()
    } else if bytes.is_instance_of::<js_sys::ArrayBuffer>() {
        js_sys::Uint8Array::new(&bytes)
    } else {
        return None;
    };
    web_sys::Blob::new_with_u8_array_sequence(&js_sys::Array::of1(&array)).ok()
}
