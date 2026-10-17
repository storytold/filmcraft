//! The native extension window, run in a second instance of the FilmCraft executable.
//!
//! A separate process gives WebKit/WebView2 its own main-thread event loop without changing
//! eframe's event loop. It is a failure boundary, not an OS security sandbox. The only editor
//! connection is bounded JSON lines over the parent's private pipes. Browser file pickers still
//! require a user gesture and explicit selection; no filesystem or shell API is exposed here.

use std::borrow::Cow;
use std::io::{BufRead, BufReader, Read, Write};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, SyncSender};

use serde_json::Value;
use tao::event::{Event, WindowEvent};
use tao::event_loop::{ControlFlow, EventLoopBuilder, EventLoopProxy};
use tao::platform::run_return::EventLoopExtRunReturn;
use tao::window::WindowBuilder;
use wry::http::{HeaderValue, Request, Response, StatusCode, Uri, header};
use wry::{NewWindowResponse, PageLoadEvent, PermissionResponse, WebContext, WebViewBuilder};

use super::package::{Package, asset_mime_type, validate_asset_path, validate_network_origin};

const SCHEME: &str = "filmcraft-extension";
const MAX_FRAME_BYTES: usize = 256 * 1024;
const QUEUE_CAPACITY: usize = 32;
const SDK: &str = include_str!("sdk.js");

#[derive(Debug)]
enum WindowMessage {
    Incoming,
    Closed,
    Failed(String),
}

#[derive(Clone)]
struct FailureSignal {
    proxy: EventLoopProxy<WindowMessage>,
    failed: Arc<AtomicBool>,
}

impl FailureSignal {
    fn fail(&self, message: impl Into<String>) {
        // A hostile page cannot grow the native event queue by repeatedly overflowing IPC.
        if !self.failed.swap(true, Ordering::Relaxed) {
            let _ = self.proxy.send_event(WindowMessage::Failed(message.into()));
        }
    }
}

/// Called only from the executable's internal helper mode, before starting eframe.
pub fn run(package: Package, data_dir: PathBuf) -> Result<(), String> {
    catch_unwind(AssertUnwindSafe(|| run_inner(package, data_dir)))
        .map_err(|_| "the extension window failed unexpectedly; the editor is still running".to_owned())?
}

fn run_inner(package: Package, data_dir: PathBuf) -> Result<(), String> {
    let csp = content_security_policy(&package.manifest.network_origins)?;
    let entry_url = format!("{SCHEME}://localhost/{}", package.manifest.entry);
    let navigation_entry = package.manifest.entry.clone();
    let mut event_loop = EventLoopBuilder::<WindowMessage>::with_user_event().build();
    #[cfg(target_os = "macos")]
    {
        use tao::platform::macos::{ActivationPolicy, EventLoopExtMacOS};
        event_loop.set_activation_policy(ActivationPolicy::Accessory);
        event_loop.set_dock_visibility(false);
    }
    let window = WindowBuilder::new()
        .with_title(format!("{} — FilmCraft", package.manifest.name))
        .with_inner_size(tao::dpi::LogicalSize::new(560.0, 720.0))
        .with_min_inner_size(tao::dpi::LogicalSize::new(320.0, 240.0))
        .build(&event_loop)
        .map_err(|error| format!("could not create the extension window: {error}"))?;
    let failure = FailureSignal { proxy: event_loop.create_proxy(), failed: Arc::new(AtomicBool::new(false)) };
    let (outgoing, outgoing_rx) = mpsc::sync_channel(QUEUE_CAPACITY);
    let (incoming, incoming_rx) = mpsc::sync_channel(QUEUE_CAPACITY);
    start_writer(outgoing_rx, failure.clone())?;
    start_reader(incoming, failure.clone())?;

    let ipc_output = outgoing.clone();
    let ipc_failure = failure.clone();
    let ready_failure = failure.clone();
    // WebView2 still needs a writable environment folder in private mode. Its default is
    // beside the executable, which is unwritable for an installation in Program Files.
    // The parent supplies and owns a separate runtime directory for this extension.
    let mut web_context = WebContext::new(Some(data_dir));
    let webview = WebViewBuilder::new_with_web_context(&mut web_context)
        .with_custom_protocol(SCHEME.into(), move |_, request| asset_response(&package, &csp, request))
        .with_initialization_script_for_main_only(SDK, true)
        .with_ipc_handler(move |request| {
            if !is_package_uri(request.uri()) {
                ipc_failure.fail("the extension sent a request from an untrusted page");
                return;
            }
            match normalize_frame(request.body()) {
                Ok(frame) => queue_output(&ipc_output, frame, &ipc_failure),
                Err(error) => ipc_failure.fail(error),
            }
        })
        .with_navigation_handler(move |url| package_navigation(&url, &navigation_entry))
        .with_new_window_req_handler(|_, _| NewWindowResponse::Deny)
        .with_download_started_handler(|_, _| false)
        .with_drag_drop_handler(|_| true)
        .with_permission_handler(|_| PermissionResponse::Deny)
        .with_clipboard(false)
        .with_devtools(false)
        .with_incognito(true)
        .with_on_page_load_handler(move |event, _| {
            if matches!(event, PageLoadEvent::Finished) {
                queue_output(&outgoing, "{\"event\":\"ready\"}".into(), &ready_failure);
            }
        })
        .with_url(entry_url)
        .build(&window)
        .map_err(|error| format!("could not start the system webview: {error}"))?;

    let mut failure_message = None;
    let exit_code = event_loop.run_return(|event, _, control_flow| {
        *control_flow = ControlFlow::Wait;
        match event {
            Event::UserEvent(WindowMessage::Incoming) => {
                // One wake corresponds to one queued frame. Draining many frames for each wake
                // would let stale wake events accumulate while the reader refills the queue.
                if let Ok(message) = incoming_rx.try_recv() {
                    let result = delivery_script(&message)
                        .and_then(|script| webview.evaluate_script(&script).map_err(|error| format!("could not deliver an editor reply: {error}")));
                    if let Err(error) = result {
                        failure_message = Some(error);
                        *control_flow = ControlFlow::Exit;
                    }
                }
            }
            Event::UserEvent(WindowMessage::Failed(error)) => {
                failure_message = Some(error);
                *control_flow = ControlFlow::Exit;
            }
            Event::UserEvent(WindowMessage::Closed) | Event::WindowEvent { event: WindowEvent::CloseRequested, .. } => {
                *control_flow = ControlFlow::Exit;
            }
            _ => {}
        }
    });
    // run_return lets native objects drop before main returns. The pipe reader may be blocked
    // when a user closes the window; main then exits this helper process, without joining it.
    drop(webview);
    if let Some(error) = failure_message {
        return Err(error);
    }
    if exit_code != 0 {
        return Err(format!("the extension event loop exited with status {exit_code}"));
    }
    Ok(())
}

fn start_writer(receiver: Receiver<String>, failure: FailureSignal) -> Result<(), String> {
    std::thread::Builder::new()
        .name("extension-stdout".into())
        .spawn(move || {
            let result = catch_unwind(AssertUnwindSafe(|| -> Result<(), String> {
                let stdout = std::io::stdout();
                let mut output = stdout.lock();
                while let Ok(frame) = receiver.recv() {
                    output
                        .write_all(frame.as_bytes())
                        .and_then(|()| output.write_all(b"\n"))
                        .and_then(|()| output.flush())
                        .map_err(|error| format!("the editor connection closed: {error}"))?;
                }
                Ok(())
            }));
            match result {
                Ok(Ok(())) => {}
                Ok(Err(error)) => failure.fail(error),
                Err(_) => failure.fail("the extension output worker failed"),
            }
        })
        .map(|_| ())
        .map_err(|error| format!("could not start the extension output worker: {error}"))
}

fn start_reader(sender: SyncSender<String>, failure: FailureSignal) -> Result<(), String> {
    std::thread::Builder::new()
        .name("extension-stdin".into())
        .spawn(move || {
            let result = catch_unwind(AssertUnwindSafe(|| -> Result<(), String> {
                let stdin = std::io::stdin();
                let mut input = BufReader::new(stdin.lock());
                while let Some(frame) = read_frame(&mut input)? {
                    let frame = normalize_frame(&frame)?;
                    // Only this worker may wait on backpressure; the native UI never does.
                    if sender.send(frame).is_err() || failure.proxy.send_event(WindowMessage::Incoming).is_err() {
                        return Ok(());
                    }
                }
                let _ = failure.proxy.send_event(WindowMessage::Closed);
                Ok(())
            }));
            match result {
                Ok(Ok(())) => {}
                Ok(Err(error)) => failure.fail(error),
                Err(_) => failure.fail("the extension input worker failed"),
            }
        })
        .map(|_| ())
        .map_err(|error| format!("could not start the extension input worker: {error}"))
}

fn queue_output(sender: &SyncSender<String>, frame: String, failure: &FailureSignal) {
    if sender.try_send(frame).is_err() {
        failure.fail("the extension sent requests faster than the editor could receive them");
    }
}

fn read_frame(reader: &mut impl BufRead) -> Result<Option<String>, String> {
    let mut bytes = Vec::new();
    let count =
        reader.take((MAX_FRAME_BYTES + 1) as u64).read_until(b'\n', &mut bytes).map_err(|error| format!("could not read the editor connection: {error}"))?;
    if count == 0 {
        return Ok(None);
    }
    if count > MAX_FRAME_BYTES {
        return Err("the extension IPC frame exceeds 256 KiB".into());
    }
    if bytes.last() != Some(&b'\n') {
        return Err("the editor connection ended in an incomplete IPC frame".into());
    }
    bytes.pop();
    if bytes.last() == Some(&b'\r') {
        bytes.pop();
    }
    String::from_utf8(bytes).map(Some).map_err(|_| "extension IPC must be UTF-8".into())
}

fn normalize_frame(frame: &str) -> Result<String, String> {
    if frame.len() >= MAX_FRAME_BYTES {
        return Err("the extension IPC frame exceeds 256 KiB".into());
    }
    let value: Value = serde_json::from_str(frame).map_err(|error| format!("invalid extension IPC JSON: {error}"))?;
    if !value.is_object() {
        return Err("extension IPC must be a JSON object".into());
    }
    let encoded = serde_json::to_string(&value).map_err(|error| format!("could not encode extension IPC: {error}"))?;
    if encoded.len() >= MAX_FRAME_BYTES {
        return Err("the extension IPC frame exceeds 256 KiB".into());
    }
    Ok(encoded)
}

fn delivery_script(frame: &str) -> Result<String, String> {
    // Parse and serialize again rather than inserting pipe text as executable JavaScript.
    let json = normalize_frame(frame)?;
    Ok(format!("window.__filmcraftReceive({json});"))
}

fn is_package_uri(uri: &Uri) -> bool {
    matches!(
        (uri.scheme_str(), uri.authority().map(|authority| authority.as_str())),
        (Some(SCHEME), Some("localhost")) | (Some("http"), Some("filmcraft-extension.localhost"))
    )
}

fn package_navigation(url: &str, entry: &str) -> bool {
    let without_fragment = url.split_once('#').map_or(url, |(head, _)| head);
    without_fragment.parse::<Uri>().is_ok_and(|uri| is_package_uri(&uri) && uri.query().is_none() && asset_path(&uri) == Some(entry))
}

fn asset_path(uri: &Uri) -> Option<&str> {
    let path = uri.path().strip_prefix('/')?;
    validate_asset_path(path).ok()?;
    Some(path)
}

fn content_security_policy(origins: &[String]) -> Result<HeaderValue, String> {
    for origin in origins {
        validate_network_origin(origin)?;
    }
    // `fetch("data.json")` and module loaders must be able to read packaged assets.
    // `self` is this window's custom protocol origin, not an arbitrary network origin.
    let connections = std::iter::once("'self'").chain(origins.iter().map(String::as_str)).collect::<Vec<_>>().join(" ");
    let policy = format!(
        "default-src 'none'; script-src 'self'; style-src 'self' 'unsafe-inline'; \
         img-src 'self' data:; font-src 'self'; media-src 'self' blob:; connect-src {connections}; \
         frame-src 'none'; child-src 'none'; worker-src 'none'; object-src 'none'; \
         base-uri 'none'; form-action 'none'; frame-ancestors 'none'"
    );
    HeaderValue::from_str(&policy).map_err(|error| format!("invalid extension content security policy: {error}"))
}

fn asset_response(package: &Package, csp: &HeaderValue, request: Request<Vec<u8>>) -> Response<Cow<'static, [u8]>> {
    let found = (request.method() == "GET" && is_package_uri(request.uri()))
        .then(|| asset_path(request.uri()))
        .flatten()
        .and_then(|path| package.files.get(path).zip(asset_mime_type(path)));
    let (status, body, mime) = match found {
        Some((bytes, mime)) => (StatusCode::OK, Cow::Owned(bytes.clone()), mime),
        None => (StatusCode::NOT_FOUND, Cow::Borrowed(b"Package asset not found".as_slice()), "text/plain; charset=utf-8"),
    };
    let mut response = Response::new(body);
    *response.status_mut() = status;
    let headers = response.headers_mut();
    headers.insert(header::CONTENT_TYPE, HeaderValue::from_static(mime));
    headers.insert(header::CONTENT_SECURITY_POLICY, csp.clone());
    headers.insert(header::X_CONTENT_TYPE_OPTIONS, HeaderValue::from_static("nosniff"));
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    headers.insert(header::REFERRER_POLICY, HeaderValue::from_static("no-referrer"));
    response
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_package_navigation_is_allowed() {
        for url in [
            "filmcraft-extension://localhost/index.html",
            "filmcraft-extension://localhost/index.html#marker",
            "http://filmcraft-extension.localhost/index.html",
        ] {
            assert!(package_navigation(url, "index.html"), "{url}");
        }
        for url in [
            "https://example.com/index.html",
            "file:///etc/passwd",
            "javascript:alert(1)",
            "filmcraft-extension://evil/index.html",
            "filmcraft-extension://localhost@evil/index.html",
            "filmcraft-extension://localhost:80/index.html",
            "filmcraft-extension://localhost/../manifest.json",
            "filmcraft-extension://localhost/%2e%2e/manifest.json",
            "filmcraft-extension://localhost//index.html",
            "filmcraft-extension://localhost/other.html",
            "filmcraft-extension://localhost/index.html?state=2",
        ] {
            assert!(!package_navigation(url, "index.html"), "{url}");
        }
    }

    #[test]
    fn ipc_is_bounded_and_requires_complete_utf8_frames() {
        assert_eq!(read_frame(&mut &b"{\"id\":1}\r\n"[..]).unwrap(), Some("{\"id\":1}".into()));
        assert!(read_frame(&mut &b"{}"[..]).is_err());
        assert!(read_frame(&mut &b"\xff\n"[..]).is_err());
        let oversized = vec![b'x'; MAX_FRAME_BYTES + 10];
        let mut input = oversized.as_slice();
        assert!(read_frame(&mut input).is_err());
        assert_eq!(input.len(), 9);
    }

    #[test]
    fn ipc_cannot_insert_executable_script() {
        assert!(delivery_script("{}); alert(1); ({").is_err());
        assert!(normalize_frame("[]").is_err());
        assert!(normalize_frame(&format!("{{\"x\":\"{}\"}}", "x".repeat(MAX_FRAME_BYTES))).is_err());
        let message = serde_json::json!({"id":1,"result":"\"); alert(1); //\n"}).to_string();
        let script = delivery_script(&message).unwrap();
        assert_eq!(script, format!("window.__filmcraftReceive({message});"));
        assert!(!script.contains('\n'));
    }

    #[test]
    fn packaged_json_fetches_use_the_same_origin_and_policy_as_the_entry() {
        let package = Package {
            manifest: super::super::package::Manifest {
                id: "org.example.test".into(),
                name: "Test".into(),
                version: "1.0.0".into(),
                api_version: 1,
                entry: "index.html".into(),
                permissions: vec![],
                network_origins: vec![],
            },
            files: [("data.json".into(), br#"{"value":42}"#.to_vec())].into(),
        };
        let csp = content_security_policy(&[]).unwrap();
        for uri in ["filmcraft-extension://localhost/data.json", "http://filmcraft-extension.localhost/data.json"] {
            let request = Request::builder().uri(uri).body(Vec::new()).unwrap();
            let response = asset_response(&package, &csp, request);
            assert_eq!(response.status(), StatusCode::OK);
            assert_eq!(response.body().as_ref(), br#"{"value":42}"#);
            assert_eq!(response.headers()[header::CONTENT_TYPE], "application/json; charset=utf-8");
            assert_eq!(response.headers()[header::CONTENT_SECURITY_POLICY], csp);
            assert_eq!(response.headers()[header::X_CONTENT_TYPE_OPTIONS], "nosniff");
        }
        for uri in [
            "https://example.com/data.json",
            "http://filmcraft-extension.localhost.evil/data.json",
            "filmcraft-extension://localhost/%2e%2e/data.json",
            "filmcraft-extension://localhost/missing.json",
        ] {
            let request = Request::builder().uri(uri).body(Vec::new()).unwrap();
            assert_eq!(asset_response(&package, &csp, request).status(), StatusCode::NOT_FOUND);
        }
        let post = Request::builder().method("POST").uri("filmcraft-extension://localhost/data.json").body(Vec::new()).unwrap();
        assert_eq!(asset_response(&package, &csp, post).status(), StatusCode::NOT_FOUND);
    }

    #[test]
    fn csp_allows_packaged_fetch_but_denies_undeclared_network_and_remote_code() {
        let policy = content_security_policy(&[]).unwrap();
        let policy = policy.to_str().unwrap();
        assert!(policy.contains("connect-src 'self';"));
        assert!(policy.contains("script-src 'self';"));
        assert!(policy.contains("form-action 'none'"));
        assert!(!policy.contains("unsafe-eval"));
        assert!(!policy.contains("https:"));
        assert!(!policy.contains("*"));
        let origins = vec!["https://api.example.com".to_owned()];
        assert!(content_security_policy(&origins).unwrap().to_str().unwrap().contains("connect-src 'self' https://api.example.com;"));
        for origin in ["https://api.example.com; script-src *", "https://*.example.com", "http://api.example.com", "'self'"] {
            assert!(content_security_policy(&[origin.into()]).is_err(), "{origin}");
        }
    }
}
