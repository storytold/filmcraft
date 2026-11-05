//! The page config: an optional `./filmcraft-config.json` next to `index.html`, fetched once at
//! start-up. The keys are parsed by [`crate::web_config`]; this module is the browser side.

use std::cell::Cell;

use wasm_bindgen::{JsCast, closure::Closure};
use wasm_bindgen_futures::JsFuture;

use crate::web_config::{self, Startup};

/// Largest config file read (bytes).
const MAX_BYTES: usize = 64 * 1024;

/// Longest the request and the body read may take (ms); a stalled server must not hold up start-up.
const TIMEOUT_MS: i32 = 5_000;

thread_local! {
    static STARTUP: Cell<Option<Startup>> = const { Cell::new(None) };
}

/// The `startup` key (`None`: absent, invalid or no config file).
pub fn startup() -> Option<Startup> {
    STARTUP.with(Cell::get)
}

/// Fetch and apply the config file; a missing or invalid file changes nothing.
pub async fn load() {
    match fetch().await {
        Ok(bytes) => apply(&bytes),
        Err(e) => log::info!("no page config: {e}"),
    }
}

/// Apply the keys of a config file's bytes.
pub fn apply(bytes: &[u8]) {
    match web_config::parse_startup(bytes) {
        Ok(v) => STARTUP.with(|s| s.set(v)),
        Err(e) => log::warn!("filmcraft-config.json: {e}; ignored"),
    }
}

/// The config file's bytes. The request is aborted after [`TIMEOUT_MS`] (the timer covers the body
/// too) and always revalidated with the server, so a stale cached copy never outlives an update.
async fn fetch() -> Result<Vec<u8>, String> {
    let win = web_sys::window().ok_or("no window")?;
    let ctl = web_sys::AbortController::new().map_err(js_err)?;
    let init = web_sys::RequestInit::new();
    init.set_cache(web_sys::RequestCache::NoCache);
    init.set_signal(Some(&ctl.signal()));
    let on_timeout = Closure::once(move || ctl.abort());
    let timer = win.set_timeout_with_callback_and_timeout_and_arguments_0(on_timeout.as_ref().unchecked_ref(), TIMEOUT_MS).map_err(js_err)?;
    let out = read(&win, &init).await;
    win.clear_timeout_with_handle(timer);
    out
}

/// Fetch the file and read its body chunk by chunk, stopping as soon as it exceeds [`MAX_BYTES`].
async fn read(win: &web_sys::Window, init: &web_sys::RequestInit) -> Result<Vec<u8>, String> {
    let resp = JsFuture::from(win.fetch_with_str_and_init("filmcraft-config.json", init)).await.map_err(js_err)?;
    let resp: web_sys::Response = resp.dyn_into().map_err(|_| "not a Response".to_string())?;
    if resp.status() != 200 {
        return Err(format!("status {}", resp.status()));
    }
    let body = resp.body().ok_or("no body")?;
    let reader: web_sys::ReadableStreamDefaultReader = body.get_reader().unchecked_into();
    let mut out = Vec::new();
    loop {
        let chunk = JsFuture::from(reader.read()).await.map_err(js_err)?;
        if js_sys::Reflect::get(&chunk, &"done".into()).map_err(js_err)?.as_bool() == Some(true) {
            return Ok(out);
        }
        let value = js_sys::Reflect::get(&chunk, &"value".into()).map_err(js_err)?;
        let bytes: js_sys::Uint8Array = value.dyn_into().map_err(|_| "chunk is not bytes".to_string())?;
        if out.len().saturating_add(bytes.length() as usize) > MAX_BYTES {
            // Stop the download; the limit is enforced while reading, not after the whole body arrived.
            let _ = reader.cancel();
            return Err("config too large".into());
        }
        out.extend_from_slice(&bytes.to_vec());
    }
}

/// A JS error value as text.
fn js_err(e: wasm_bindgen::JsValue) -> String {
    format!("{e:?}")
}
