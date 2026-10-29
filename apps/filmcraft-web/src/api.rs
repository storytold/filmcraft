//! `window.filmcraft`: the control channel / agent API for the page.
//!
//! The same JSON methods as the desktop control channel (`docs/control-protocol.md`) and the
//! same engine commands as MCP, as promises:
//!
//! ```js
//! await filmcraft.execute("sequence.razor", {seconds: 2})   // engine command → its result
//! await filmcraft.request("ui.inspect")                    // control method → its result
//! await filmcraft.commands()                               // command list
//! const {pngBase64} = await filmcraft.screenshot()         // the canvas as PNG (base64)
//! await filmcraft.importUrl("/clip.mp4")                   // fetch + import (tests)
//! await filmcraft.importFiles(input.files)                 // File / FileList / File[]
//! await filmcraft.openProject(file)                        // .fcproj File, URL, or a path on a remote mount
//! filmcraft.files(); await filmcraft.readFile("/exports/a.mp4"); filmcraft.info()
//! ```
//!
//! Results resolve with the method's `result`; errors reject with the error message.

use std::cell::RefCell;
use std::sync::mpsc::{Receiver, Sender, TryRecvError};

use filmcraft_ui_egui::ControlRequest;
use serde_json::{Value, json};
use wasm_bindgen::JsCast;
use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::{JsFuture, future_to_promise};

type Waiting = (Receiver<Value>, js_sys::Function, js_sys::Function);

thread_local! {
    static TX: RefCell<Option<Sender<ControlRequest>>> = const { RefCell::new(None) };
    static WAITING: RefCell<Vec<Waiting>> = const { RefCell::new(Vec::new()) };
}

pub fn set_sender(tx: Sender<ControlRequest>) {
    TX.with(|t| *t.borrow_mut() = Some(tx));
}

pub fn to_js(v: &Value) -> JsValue {
    js_sys::JSON::parse(&v.to_string()).unwrap_or(JsValue::NULL)
}

pub fn from_js(v: &JsValue) -> Value {
    if v.is_undefined() || v.is_null() {
        return Value::Null;
    }
    js_sys::JSON::stringify(v).ok().and_then(|s| s.as_string()).and_then(|s| serde_json::from_str(&s).ok()).unwrap_or(Value::Null)
}

/// Resolve the promises of answered control requests (called every frame).
pub fn poll_replies() {
    WAITING.with(|w| {
        w.borrow_mut().retain(|(rx, resolve, reject)| match rx.try_recv() {
            Ok(v) => {
                if v.get("ok").and_then(Value::as_bool) == Some(true) {
                    let _ = resolve.call1(&JsValue::NULL, &to_js(v.get("result").unwrap_or(&Value::Null)));
                } else {
                    let msg = v.get("error").and_then(Value::as_str).unwrap_or("error").to_string();
                    let _ = reject.call1(&JsValue::NULL, &js_sys::Error::new(&msg));
                }
                false
            }
            Err(TryRecvError::Empty) => true,
            Err(TryRecvError::Disconnected) => {
                let _ = reject.call1(&JsValue::NULL, &js_sys::Error::new("request dropped"));
                false
            }
        });
    });
}

/// Send a control request; the promise settles when the UI thread answers it.
pub fn request(method: &str, params: Value) -> js_sys::Promise {
    let method = method.to_string();
    let p = js_sys::Promise::new(&mut |resolve, reject| {
        let (req, rx) = ControlRequest::new(method.clone(), params.clone());
        let sent = TX.with(|t| t.borrow().as_ref().map(|tx| tx.send(req).is_ok()).unwrap_or(false));
        if sent {
            WAITING.with(|w| w.borrow_mut().push((rx, resolve, reject)));
        } else {
            let _ = reject.call1(&JsValue::NULL, &js_sys::Error::new("FilmCraft is not running yet"));
        }
    });
    crate::repaint();
    p
}

fn set_fn(obj: &js_sys::Object, name: &str, f: JsValue) -> Result<(), JsValue> {
    js_sys::Reflect::set(obj, &name.into(), &f).map(|_| ())
}

fn files_of(v: &JsValue) -> Vec<web_sys::File> {
    if let Some(f) = v.dyn_ref::<web_sys::File>() {
        return vec![f.clone()];
    }
    if let Some(list) = v.dyn_ref::<web_sys::FileList>() {
        return (0..list.length()).filter_map(|i| list.get(i)).collect();
    }
    if js_sys::Array::is_array(v) {
        return js_sys::Array::from(v).iter().filter_map(|x| x.dyn_into::<web_sys::File>().ok()).collect();
    }
    Vec::new()
}

/// Fetch a URL into a `File` named after it (or `name`).
pub async fn fetch_file(url: &str, name: Option<String>) -> Result<web_sys::File, JsValue> {
    let w = web_sys::window().ok_or("no window")?;
    let resp: web_sys::Response = JsFuture::from(w.fetch_with_str(url)).await?.dyn_into()?;
    if !resp.ok() {
        return Err(format!("{url}: HTTP {}", resp.status()).into());
    }
    let blob: web_sys::Blob = JsFuture::from(resp.blob()?).await?.dyn_into()?;
    let name = name.unwrap_or_else(|| url.split(['?', '#']).next().unwrap_or(url).rsplit('/').next().unwrap_or("media").to_string());
    let parts = js_sys::Array::of1(&blob);
    web_sys::File::new_with_blob_sequence(&parts, &name)
}

/// Install `window.filmcraft`.
pub fn install() -> Result<(), JsValue> {
    let obj = js_sys::Object::new();
    set_fn(
        &obj,
        "request",
        Closure::<dyn Fn(String, JsValue) -> js_sys::Promise>::new(|method: String, params: JsValue| {
            let p = from_js(&params);
            request(&method, if p.is_null() { json!({}) } else { p })
        })
        .into_js_value(),
    )?;
    set_fn(
        &obj,
        "execute",
        Closure::<dyn Fn(String, JsValue) -> js_sys::Promise>::new(|command: String, params: JsValue| {
            let p = from_js(&params);
            request("engine.execute", json!({"command": command, "params": if p.is_null() { json!({}) } else { p }}))
        })
        .into_js_value(),
    )?;
    set_fn(&obj, "commands", Closure::<dyn Fn() -> js_sys::Promise>::new(|| request("engine.commands", json!({}))).into_js_value())?;
    set_fn(&obj, "inspect", Closure::<dyn Fn() -> js_sys::Promise>::new(|| request("ui.inspect", json!({}))).into_js_value())?;
    set_fn(
        &obj,
        "screenshot",
        Closure::<dyn Fn(JsValue) -> js_sys::Promise>::new(|params: JsValue| {
            let p = from_js(&params);
            request("ui.screenshot", if p.is_null() { json!({}) } else { p })
        })
        .into_js_value(),
    )?;
    set_fn(
        &obj,
        "info",
        Closure::<dyn Fn() -> JsValue>::new(|| {
            let mut v = crate::info();
            v["webcodecsStats"] = crate::webcodecs::stats();
            v["fetchesInFlight"] = json!(crate::fs::inflight());
            to_js(&v)
        })
        .into_js_value(),
    )?;
    set_fn(
        &obj,
        "files",
        Closure::<dyn Fn() -> JsValue>::new(|| {
            to_js(&Value::Array(crate::fs::list().into_iter().map(|(p, n, k)| json!({"path": p, "size": n, "kind": k})).collect()))
        })
        .into_js_value(),
    )?;
    set_fn(
        &obj,
        "readFile",
        Closure::<dyn Fn(String) -> js_sys::Promise>::new(|path: String| {
            future_to_promise(caught(async move {
                let blob = crate::fs::blob(&path).ok_or_else(|| JsValue::from_str(&format!("{path}: not found")))?;
                let buf = JsFuture::from(blob.array_buffer()).await?;
                Ok(js_sys::Uint8Array::new(&buf).into())
            }))
        })
        .into_js_value(),
    )?;
    set_fn(
        &obj,
        "importFiles",
        Closure::<dyn Fn(JsValue) -> js_sys::Promise>::new(|files: JsValue| {
            let files = files_of(&files);
            future_to_promise(async move { Ok(to_js(&crate::import::import_files(files).await)) })
        })
        .into_js_value(),
    )?;
    set_fn(
        &obj,
        "importUrl",
        Closure::<dyn Fn(String, JsValue) -> js_sys::Promise>::new(|url: String, name: JsValue| {
            future_to_promise(caught(async move {
                let f = fetch_file(&url, name.as_string()).await?;
                Ok(to_js(&crate::import::import_files(vec![f]).await))
            }))
        })
        .into_js_value(),
    )?;
    set_fn(
        &obj,
        "openProject",
        Closure::<dyn Fn(JsValue) -> js_sys::Promise>::new(|src: JsValue| {
            future_to_promise(caught(async move {
                // a path on a remote mount opens from the server (with the sizes of its media)
                if let Some(path) = src.as_string().filter(|p| crate::remote::is_remote(p)) {
                    return crate::remote::open_project(&path).await.map(|v| to_js(&v)).map_err(|e| JsValue::from(js_sys::Error::new(&e)));
                }
                let file = match src.as_string() {
                    Some(url) => fetch_file(&url, None).await?,
                    None => files_of(&src).into_iter().next().ok_or("need a File or URL")?,
                };
                crate::import::open_project_file(file).await.map(|v| to_js(&v)).map_err(|e| JsValue::from(js_sys::Error::new(&e)))
            }))
        })
        .into_js_value(),
    )?;
    let w = web_sys::window().ok_or("no window")?;
    js_sys::Reflect::set(&w, &"filmcraft".into(), &obj)?;
    Ok(())
}

/// `f`, rejecting with an [`as_error`] `Error`.
async fn caught(f: impl std::future::Future<Output = Result<JsValue, JsValue>>) -> Result<JsValue, JsValue> {
    f.await.map_err(as_error)
}

/// Every member rejects with an `Error` (#90), as `execute` and `request` do, so `e.message` and
/// `e instanceof Error` work: a plain string (a fetch status, "not found") becomes one, and an
/// `Error` passes through unchanged.
fn as_error(e: JsValue) -> JsValue {
    if e.is_instance_of::<js_sys::Error>() {
        return e;
    }
    let msg = e.as_string().unwrap_or_else(|| format!("{e:?}"));
    js_sys::Error::new(&msg).into()
}
