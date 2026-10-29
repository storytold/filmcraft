//! Browser HTTP primitives of remote storage: one `fetch` with the page's credentials and an
//! abort timer, response header and capped body reads.

use wasm_bindgen::JsCast;
use wasm_bindgen_futures::JsFuture;

use crate::remote_dav;

/// Text of a JavaScript error value.
pub(crate) fn js_err(e: wasm_bindgen::JsValue) -> String {
    let aborted = js_sys::Reflect::get(&e, &"name".into()).ok().and_then(|n| n.as_string()).is_some_and(|n| n == "AbortError");
    if aborted { "the server did not answer in time".to_string() } else { format!("{e:?}") }
}

/// Send one request with the page's credentials and a timeout; the response of any status, or the failure text.
pub(crate) async fn fetch(method: &str, url: &str, headers: &[(&str, &str)], body: Option<&[u8]>) -> Result<web_sys::Response, String> {
    fetch_within(method, url, headers, body, None).await
}

/// [`fetch`] whose timeout is also cut to what is left until `deadline`, so the request is
/// aborted (not just no longer awaited) when the caller's time budget ends.
pub(crate) async fn fetch_within(
    method: &str,
    url: &str,
    headers: &[(&str, &str)],
    body: Option<&[u8]>,
    deadline: Option<web_time::Instant>,
) -> Result<web_sys::Response, String> {
    let init = web_sys::RequestInit::new();
    init.set_method(method);
    // Same origin only: a redirect elsewhere fails instead of leaking the request.
    init.set_mode(web_sys::RequestMode::SameOrigin);
    init.set_cache(web_sys::RequestCache::NoCache);
    let h = web_sys::Headers::new().map_err(js_err)?;
    for (k, v) in headers {
        h.set(k, v).map_err(js_err)?;
    }
    init.set_headers(&h);
    // Every request is aborted when it takes too long (the timer also covers reading the body; a
    // request that already finished ignores it), so nothing waits on a server that went quiet.
    let ctl = web_sys::AbortController::new().map_err(js_err)?;
    init.set_signal(Some(&ctl.signal()));
    let win = web_sys::window().ok_or("no window")?;
    let left_ms = deadline.map(|d| u64::try_from(d.saturating_duration_since(web_time::Instant::now()).as_millis()).unwrap_or(u64::MAX));
    let timer = wasm_bindgen::closure::Closure::once_into_js({
        let ctl = ctl.clone();
        move || ctl.abort()
    });
    win.set_timeout_with_callback_and_timeout_and_arguments_0(timer.unchecked_ref(), remote_dav::capped_timeout_ms(remote_dav::timeout_ms(method), left_ms))
        .map_err(js_err)?;
    if let Some(b) = body {
        init.set_body(&js_sys::Uint8Array::from(b));
    }
    let req = web_sys::Request::new_with_str_and_init(url, &init).map_err(js_err)?;
    let resp = JsFuture::from(win.fetch_with_request(&req)).await.map_err(js_err)?;
    resp.dyn_into::<web_sys::Response>().map_err(|_| "fetch did not return a Response".to_string())
}

/// A response header's value, `None` when absent.
pub(crate) fn header(r: &web_sys::Response, name: &str) -> Option<String> {
    r.headers().get(name).ok().flatten()
}

/// The body, refused before reading when `Content-Length` exceeds `cap` and while reading as soon
/// as the bytes received exceed it, so a listing without (or with a false) length cannot fill memory.
pub(crate) async fn read_body(r: &web_sys::Response, cap: usize) -> Result<Vec<u8>, String> {
    if header(r, "content-length").and_then(|h| remote_dav::parse_content_length(&h)).is_some_and(|n| n > cap as u64) {
        return Err("response too large".into());
    }
    // no body (e.g. 204): empty
    let Some(stream) = r.body() else { return Ok(Vec::new()) };
    let reader: web_sys::ReadableStreamDefaultReader = stream.get_reader().unchecked_into();
    let mut out = Vec::new();
    loop {
        let chunk = JsFuture::from(reader.read()).await.map_err(js_err)?;
        if js_sys::Reflect::get(&chunk, &"done".into()).ok().and_then(|d| d.as_bool()).unwrap_or(true) {
            return Ok(out);
        }
        let value = js_sys::Reflect::get(&chunk, &"value".into()).map_err(js_err)?;
        let part = js_sys::Uint8Array::new(&value);
        if out.len().saturating_add(part.length() as usize) > cap {
            // stop the transfer: nothing more is read
            let _ = reader.cancel();
            return Err("response too large".into());
        }
        out.extend_from_slice(&part.to_vec());
    }
}
