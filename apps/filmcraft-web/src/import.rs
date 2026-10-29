//! Getting files in and out: pickers (File System Access API where available, else
//! `<input type=file>`), drag-and-drop, imports, project open/save.
//!
//! An import registers each `File` in [`crate::fs`] (no copy), prefetches the container index
//! (an MP4's `moov`, a small Matroska file whole, the head and tail of a large one), then runs
//! the engine's `file.import`. A read that has to wait for the browser makes the import retry
//! once the bytes are there, so imports never block the UI thread.

use std::cell::RefCell;
use std::rc::Rc;

use filmcraft_ui_egui::{FilmcraftApp, RelinkHint};
use serde_json::{Value, json};
use wasm_bindgen::JsCast;
use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::JsFuture;

use crate::fs;

/// Run `f` on the UI thread (next frame) and wait for its result.
pub async fn on_ui<T: 'static>(f: impl FnOnce(&mut crate::WebApp, &egui::Context) -> T + 'static) -> T {
    let slot: Rc<RefCell<Option<T>>> = Rc::new(RefCell::new(None));
    let s = slot.clone();
    crate::post(move |w, c| *s.borrow_mut() = Some(f(w, c)));
    loop {
        if let Some(v) = slot.borrow_mut().take() {
            return v;
        }
        fs::sleep(5).await;
    }
}

/// Largest `moov` a prewarm fetches; a bigger one is left to the demuxer's own reads.
const MOOV_MAX: u64 = 128 << 20;

/// Load what opening the file will read first: the container index. An MP4/MOV is not scanned:
/// each top-level atom header is read from the cache (the head chunk, then one chunk per hop) and
/// the atom skipped by its size, so only the headers and the `moov` itself are fetched, wherever
/// the `moov` sits in the file.
pub(crate) async fn prewarm(path: &str, size: u64) {
    let head_len = size.min(64 << 10);
    fs::prefetch(path, 0, head_len).await;
    let Some(head) = fs::cached_range(path, 0, head_len as usize) else { return };
    if filmcraft_codecs::mp4::sniff(&head) {
        let mut off = 0u64;
        for _ in 0..crate::atomwalk::MAX_HOPS {
            fs::prefetch(path, off, off.saturating_add(crate::atomwalk::HEADER_LEN)).await;
            let Some(h) = fs::cached_range(path, off, crate::atomwalk::HEADER_LEN as usize) else { break };
            match crate::atomwalk::step(&h, off, size) {
                crate::atomwalk::Step::Moov { off, len } => {
                    if len <= MOOV_MAX {
                        fs::prefetch(path, off, off + len).await;
                    }
                    break;
                }
                crate::atomwalk::Step::Next(n) => off = n,
                crate::atomwalk::Step::Stop => break,
            }
        }
    } else if head.starts_with(&[0x1A, 0x45, 0xDF, 0xA3]) {
        // Matroska: opening scans every cluster header
        if size <= 192 << 20 {
            fs::prefetch(path, 0, size).await;
        } else {
            fs::prefetch(path, 0, 8 << 20).await;
            fs::prefetch(path, size.saturating_sub(8 << 20), size).await;
        }
    } else if size <= 256 << 20 {
        // stills, WAV, compressed audio, captions: decoded/parsed whole
        fs::prefetch(path, 0, size).await;
    }
}

/// `file.import` one path, retried while its bytes are loading.
async fn import_path(path: String, bin: u64) -> Result<Value, String> {
    for _ in 0..2000 {
        let p = path.clone();
        let (r, pending) = on_ui(move |w, _| {
            let _ = filmcraft_media::pending::take();
            let r = w.app.session.execute("file.import", json!({"paths": [p], "bin": bin})).map_err(|e| e.to_string());
            (r, filmcraft_media::pending::take())
        })
        .await;
        let failed = match &r {
            Ok(v) => v.get("errors").and_then(Value::as_array).is_some_and(|e| !e.is_empty()),
            Err(_) => true,
        };
        if failed && pending {
            fs::idle().await;
            continue;
        }
        return r;
    }
    Err(format!("{path}: timed out loading the file"))
}

/// Import browser files (no copies): returns `{items, errors, paths}`.
pub async fn import_files(files: Vec<web_sys::File>) -> Value {
    // Capture once per batch: navigation while prewarming must not redirect pending imports.
    let bin = on_ui(|w, _| w.app.import_bin().0).await;
    let mut items = Vec::new();
    let mut errors = Vec::new();
    let mut paths = Vec::new();
    for f in files {
        let name = f.name();
        let size = f.size() as u64;
        let path = fs::register_blob(&name, f.clone().into());
        prewarm(&path, size).await;
        match import_path(path.clone(), bin).await {
            Ok(v) => {
                items.extend(v.get("items").and_then(Value::as_array).cloned().unwrap_or_default());
                errors.extend(v.get("errors").and_then(Value::as_array).cloned().unwrap_or_default());
                crate::recovery::keep_media(&path, &f);
            }
            Err(e) => errors.push(json!(e)),
        }
        paths.push(json!(path));
    }
    if !errors.is_empty() {
        let msg = errors.iter().filter_map(Value::as_str).collect::<Vec<_>>().join("; ");
        crate::post(move |w, _| w.app.status(msg));
    }
    json!({"items": items, "errors": errors, "paths": paths})
}

/// Open a `.fcproj` file picked or dropped by the user.
pub async fn open_project_file(file: web_sys::File) -> Result<Value, String> {
    let buf = JsFuture::from(file.array_buffer()).await.map_err(|e| format!("{e:?}"))?;
    let path = format!("/projects/{}", file.name());
    fs::put(&path, js_sys::Uint8Array::new(&buf).to_vec());
    on_ui(move |w, _| w.app.session.execute("file.open", json!({"path": path})).map_err(|e| e.to_string())).await
}

/// Handle files from a picker or a drop: projects open, everything else is imported.
pub fn take_files(files: Vec<web_sys::File>) {
    wasm_bindgen_futures::spawn_local(async move {
        let (projects, media): (Vec<_>, Vec<_>) = files.into_iter().partition(|f| f.name().to_ascii_lowercase().ends_with(".fcproj"));
        for p in projects {
            if let Err(e) = open_project_file(p).await {
                crate::post(move |w, _| w.app.status(e));
            }
        }
        if !media.is_empty() {
            import_files(media).await;
        }
    });
}

/// Files a picker returned: imported, or with a relink hint, the first one is registered under
/// `/files/<name>` and the hint's command runs on it (#111).
fn picked(files: Vec<web_sys::File>, relink: Option<RelinkHint>) {
    let Some(hint) = relink else { return take_files(files) };
    let Some(f) = files.into_iter().next() else { return };
    wasm_bindgen_futures::spawn_local(async move {
        let path = fs::register_blob(&f.name(), f.clone().into());
        crate::recovery::keep_media(&path, &f);
        let mut params = hint.params;
        if !params.is_object() {
            params = json!({});
        }
        params["path"] = json!(path);
        let command = hint.command;
        let outcome = on_ui(move |w, _| w.app.session.execute(&command, params).map_err(|e| e.to_string())).await;
        crate::post(move |w, _| match outcome {
            Ok(v) => {
                if let Some(n) = v.get("relinked").and_then(Value::as_array).map(Vec::len) {
                    w.app.status(format!("Linked {n} clip(s)"));
                }
            }
            Err(e) => w.app.status(e),
        });
    });
}

fn has_fsa_picker() -> bool {
    web_sys::window().is_some_and(|w| js_sys::Reflect::has(&w, &"showOpenFilePicker".into()).unwrap_or(false))
}

/// Let the user pick files: `showOpenFilePicker` when available, else a file input.
pub fn pick(exts: &[&str], multiple: bool) {
    pick_then(exts, multiple, None);
}

/// [`pick`], handing the files to [`picked`]: a cancelled picker leaves nothing pending.
fn pick_then(exts: &[&str], multiple: bool, relink: Option<RelinkHint>) {
    let accept: Vec<String> = exts.iter().map(|e| format!(".{e}")).collect();
    if has_fsa_picker() {
        let opts = js_sys::Object::new();
        let _ = js_sys::Reflect::set(&opts, &"multiple".into(), &multiple.into());
        wasm_bindgen_futures::spawn_local(async move {
            let Some(w) = web_sys::window() else { return };
            let Ok(f) = js_sys::Reflect::get(&w, &"showOpenFilePicker".into()).and_then(|f| f.dyn_into::<js_sys::Function>()) else { return };
            let Ok(p) = f.call1(&w, &opts).and_then(|p| p.dyn_into::<js_sys::Promise>()) else { return };
            // rejected when the user cancels
            let Ok(handles) = JsFuture::from(p).await else { return };
            let mut files = Vec::new();
            for h in js_sys::Array::from(&handles).iter() {
                if let Ok(h) = h.dyn_into::<web_sys::FileSystemFileHandle>()
                    && let Ok(f) = JsFuture::from(h.get_file()).await
                    && let Ok(f) = f.dyn_into::<web_sys::File>()
                {
                    files.push(f);
                }
            }
            picked(files, relink);
        });
        return;
    }
    let Some(doc) = web_sys::window().and_then(|w| w.document()) else { return };
    let Ok(input) = doc.create_element("input").and_then(|e| e.dyn_into::<web_sys::HtmlInputElement>().map_err(Into::into)) else { return };
    input.set_type("file");
    input.set_multiple(multiple);
    input.set_accept(&accept.join(","));
    let inp = input.clone();
    let on_change = Closure::<dyn FnMut()>::new(move || {
        if let Some(list) = inp.files() {
            picked((0..list.length()).filter_map(|i| list.get(i)).collect(), relink.clone());
        }
    });
    input.set_onchange(Some(on_change.as_ref().unchecked_ref()));
    on_change.forget();
    input.click();
}

/// Drop files anywhere on the page to import them (or open a project).
pub fn install_drop_handlers() -> Result<(), JsValue> {
    let w = web_sys::window().ok_or("no window")?;
    let over = Closure::<dyn FnMut(web_sys::DragEvent)>::new(|e: web_sys::DragEvent| e.prevent_default());
    w.add_event_listener_with_callback_and_bool("dragover", over.as_ref().unchecked_ref(), true)?;
    over.forget();
    let drop = Closure::<dyn FnMut(web_sys::DragEvent)>::new(|e: web_sys::DragEvent| {
        e.prevent_default();
        e.stop_propagation();
        if let Some(list) = e.data_transfer().and_then(|d| d.files()) {
            take_files((0..list.length()).filter_map(|i| list.get(i)).collect());
        }
    });
    w.add_event_listener_with_callback_and_bool("drop", drop.as_ref().unchecked_ref(), true)?;
    drop.forget();
    Ok(())
}

/// Wire the UI's file dialogs to the browser.
pub fn install_hooks(app: &mut FilmcraftApp) {
    // Pickers are asynchronous: the dialog hook starts the picker and returns nothing; the files
    // are imported when the user has chosen them.
    app.hooks.pick_files = Some(Box::new(|exts: &[&str]| {
        pick(exts, true);
        Vec::new()
    }));
    app.hooks.pick_open_project = Some(Box::new(|| {
        pick(&["fcproj"], false);
        None
    }));
    // Saving writes the file in memory and offers it as a download (`fs::WebServices`).
    app.hooks.pick_save = Some(Box::new(|name: &str| Some(format!("/projects/{name}"))));
    app.hooks.pick_save_as = Some(Box::new(|_filter: &str, _exts: &[&str], name: &str| {
        let file = std::path::Path::new(name).file_name().map_or(name.into(), std::ffi::OsStr::to_string_lossy);
        Some(format!("/exports/{file}"))
    }));
    // Link Media ▸ Locate…, Attach Proxies, Reconnect Full Resolution: the picker is async, so the
    // hint's command runs once the user has chosen (#111).
    app.hooks.pick_file_for_relink = Some(Box::new(|exts: &[&str], hint: Option<RelinkHint>| {
        if hint.is_some() {
            pick_then(exts, false, hint);
        }
        None
    }));
}
