//! `window.filmcraft`: the control channel / agent API for the page.
//!
//! `request`, `execute`, `commands`, `uiInspect` and `screenshot` come from `craft-web-api`
//! (storytold/craft-libs#413), the same in every Craft app; FilmCraft adds its own functions on the
//! same object. The same JSON methods as the desktop control channel (`docs/control-protocol.md`)
//! and the same engine commands as MCP, as promises:
//!
//! ```js
//! await filmcraft.execute("sequence.razor", {seconds: 2})   // engine command → its result
//! await filmcraft.request("ui.inspect")                    // control method → its result
//! await filmcraft.commands()                               // command list
//! const {data} = await filmcraft.screenshot()            // the canvas as PNG (base64; also `pngBase64`)
//! await filmcraft.importUrl("/clip.mp4")                   // fetch + import (tests)
//! await filmcraft.importFiles(input.files)                 // File / FileList / File[]
//! await filmcraft.openProject(file)                        // .fcproj File or URL
//! filmcraft.files(); await filmcraft.readFile("/exports/a.mp4"); filmcraft.info()
//! ```
//!
//! Results resolve with the method's `result`; errors reject with the error message.

use std::cell::RefCell;
use std::future::Future;
use std::pin::Pin;
use std::rc::Rc;
use std::sync::mpsc::{Receiver, Sender, TryRecvError};

use craft_web_api::methods::Methods;
use craft_web_api::page::{PageApi, Request};
use filmcraft_ui_egui::ControlRequest;
use serde_json::{Value, json};
use wasm_bindgen::JsCast;
use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::{JsFuture, future_to_promise};

thread_local! {
    static TX: RefCell<Option<Sender<ControlRequest>>> = const { RefCell::new(None) };
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

/// How often a waiting call looks for the app's reply, in milliseconds (about a frame).
const POLL_MS: i32 = 16;

/// Queue a control request for the app and wait for its reply, as the protocol's `{ok, result}` /
/// `{ok: false, error}` object. FilmCraft's control requests cannot be withdrawn, so a call the page
/// gave up on still runs when the app reaches it, as it does for a desktop client that timed out.
fn send(method: &str, params: Value) -> Pin<Box<dyn Future<Output = Value>>> {
    let (req, rx) = ControlRequest::new(method, params);
    let sent = TX.with(|t| t.borrow().as_ref().map(|tx| tx.send(req).is_ok()).unwrap_or(false));
    if !sent {
        return Box::pin(async { json!({"ok": false, "error": "FilmCraft is not running yet"}) });
    }
    crate::repaint();
    Box::pin(wait(rx))
}

/// The app's reply, polled about once a frame.
async fn wait(reply: Receiver<Value>) -> Value {
    loop {
        match reply.try_recv() {
            Ok(reply) => return reply,
            Err(TryRecvError::Empty) => sleep(POLL_MS).await,
            Err(TryRecvError::Disconnected) => return json!({"ok": false, "error": "request dropped"}),
        }
    }
}

async fn sleep(ms: i32) {
    let promise = js_sys::Promise::new(&mut |resolve, _| {
        if let Some(window) = web_sys::window() {
            let _ = window.set_timeout_with_callback_and_timeout_and_arguments_0(&resolve, ms);
        }
    });
    let _ = JsFuture::from(promise).await;
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

/// Install `window.filmcraft`: the crate's functions, then FilmCraft's own. FilmCraft has no
/// `document.inspect`, so `docInspect` is not installed; `inspect` is its `ui.inspect`.
pub fn install() -> Result<(), JsValue> {
    let request: Request = Rc::new(|method, params| send(&method, params));
    let methods = Methods { doc_inspect: None, ..Methods::default() };
    Rc::new(PageApi::new("filmcraft", methods, request)).install()?;
    let window = web_sys::window().ok_or("no window")?;
    let obj: js_sys::Object = js_sys::Reflect::get(&window, &"filmcraft".into())?.unchecked_into();
    set_fn(
        &obj,
        "inspect",
        Closure::<dyn Fn() -> js_sys::Promise>::new(|| {
            future_to_promise(async {
                let reply = send("ui.inspect", json!({})).await;
                if reply.get("ok").and_then(Value::as_bool) == Some(true) {
                    Ok(to_js(reply.get("result").unwrap_or(&Value::Null)))
                } else {
                    Err(js_sys::Error::new(reply.get("error").and_then(Value::as_str).unwrap_or("error")).into())
                }
            })
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
                let file = match src.as_string() {
                    Some(url) => fetch_file(&url, None).await?,
                    None => files_of(&src).into_iter().next().ok_or("need a File or URL")?,
                };
                crate::import::open_project_file(file).await.map(|v| to_js(&v)).map_err(|e| JsValue::from(js_sys::Error::new(&e)))
            }))
        })
        .into_js_value(),
    )?;
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
