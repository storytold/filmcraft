//! Control endpoints of the desktop app. Two listeners feed one request queue.
//!
//! The Craft Link socket (`filmcraft-<pid>` under the runtime dir, token handshake in craft-ipc) is
//! always on; [`start_with`] publishes it. `--control <port>` adds the localhost JSON-lines TCP
//! server below (one request per line, one reply per line). Loopback only.
//!
//! The server only ever reads requests: a line that is not a JSON object with a string `method`
//! (an HTTP request line from a browser's cross-origin `fetch`, a stray `nc`, binary junk) gets
//! one error reply and the connection is closed, so nothing after it is executed. Lines are
//! bounded ([`MAX_LINE`]) and so is the number of open connections ([`MAX_CONNECTIONS`]); junk
//! never reaches the UI thread. Every request must carry the session credential from
//! `FILMCRAFT_CONTROL_TOKEN`; without a configured credential the listener stays closed.
//! See `docs/control-protocol.md`.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use craft_ipc::LinkError;
use craft_ipc::server::{ControlSink, FrameOpen, Frames, Server, ServerConfig};
use filmcraft_ui_egui::ControlRequest;
use serde_json::{Value, json};
use subtle::ConstantTimeEq;

/// Longest accepted request line (bytes, without the newline). Requests are small JSON objects;
/// anything longer is refused and the connection closed.
pub const MAX_LINE: usize = 4 * 1024 * 1024;
/// Connections served at once; further ones get an error line and are closed.
pub const MAX_CONNECTIONS: usize = 16;
/// A peer that stops sending or reading must eventually give back its connection slot.
const CONNECTION_TIMEOUT: Duration = Duration::from_secs(120);

/// Why the TCP control server is not serving although `--control` was given (#3436).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DisabledWhy {
    /// `FILMCRAFT_CONTROL_TOKEN` is missing or is not 64 hex characters.
    Token,
    /// The port could not be bound. Showing this in the app is left to #3318, which reworks
    /// port-collision reporting; until then the reason is only logged.
    Bind { port: u16, error: String },
}

/// `--control-input-only` was given but the control server is not serving: the session would be
/// unreachable, so startup fails with this error and `main` exits directly, with no dialog an
/// unattended launch could not dismiss (#3436).
#[derive(Debug)]
pub struct ControlServerRequired(pub String);

impl std::fmt::Display for ControlServerRequired {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "--control-input-only requires a running control server: {}", self.0)
    }
}

impl std::error::Error for ControlServerRequired {}

impl DisabledWhy {
    /// The reason as one line, for `filmcraft.log` and the status bar.
    pub fn reason(&self) -> String {
        match self {
            Self::Token => "FILMCRAFT_CONTROL_TOKEN must contain 64 random hex characters (see docs/control-protocol.md)".into(),
            Self::Bind { port, error } => format!("could not bind 127.0.0.1:{port}: {error}"),
        }
    }
}

/// Queue a push event on the running link server. Quiet when none runs.
pub fn emit(event: &str, data: Value) {
    if let Some(server) = server_lock().as_ref() {
        let _ = server.emit(event, data);
    }
}

/// The link server has to outlive `start_with`, or the socket closes when the function returns.
static SERVER: Mutex<Option<Server>> = Mutex::new(None);

/// What [`start_with`] hands to the app.
pub struct Started {
    /// Requests from both listeners.
    pub requests: Receiver<ControlRequest>,
    /// The app sets this once it drains `requests`. Until then both listeners reject a request
    /// with a retryable error instead of queueing it (#3436).
    pub ready: Arc<AtomicBool>,
    /// Why the TCP server is not serving, when `--control` was given and it is not.
    pub tcp_disabled: Option<DisabledWhy>,
}

/// The Craft Link socket for `runtime` (the real runtime dir for `None`), plus the token-checked
/// TCP server when `tcp_port` is set. Both feed one receiver. A failed link start is logged and
/// nothing arrives from that listener.
pub fn start_with(runtime: Option<craft_ipc::RuntimeDir>, tcp_port: Option<u16>, ctx: egui::Context) -> Started {
    let (tx, requests) = channel::<ControlRequest>();
    let ready = Arc::new(AtomicBool::new(false));
    start_link(runtime, tx.clone(), ctx.clone(), Arc::clone(&ready));
    let tcp_disabled = tcp_port.and_then(|port| start_tcp(port, ctx, std::env::var("FILMCRAFT_CONTROL_TOKEN").ok(), tx, Arc::clone(&ready)));
    Started { requests, ready, tcp_disabled }
}

/// The TCP server alone, with the credential given. The receiver disconnects when it did not start.
#[cfg(test)]
fn start_with_token(port: u16, ctx: egui::Context, token: Option<String>) -> (Receiver<ControlRequest>, Arc<AtomicBool>, Option<DisabledWhy>) {
    let (tx, rx) = channel::<ControlRequest>();
    let ready = Arc::new(AtomicBool::new(false));
    let why = start_tcp(port, ctx, token, tx, Arc::clone(&ready));
    (rx, ready, why)
}

fn start_link(runtime: Option<craft_ipc::RuntimeDir>, tx: Sender<ControlRequest>, ctx: egui::Context, ready: Arc<AtomicBool>) {
    let sink = Arc::new(UiSink { tx, ctx: ctx.clone(), ready });
    let wake_ctx = ctx.clone();
    let mut window = filmcraft_link::host::Host::window(ctx);
    window.runtime = runtime.clone();
    let host = filmcraft_link::host::install(window);
    // Live links: find EffectCraft once a second, attach the project's links, pull frames from it.
    let runtime_host = Arc::clone(&host);
    filmcraft_link::manager::install(filmcraft_link::manager::ManagerConfig {
        runtime: Arc::new(move || runtime_host.runtime_dir()),
        ui: Arc::clone(&sink) as Arc<dyn ControlSink>,
        wake: Arc::new(move || wake_ctx.request_repaint()),
        scan_every: filmcraft_link::manager::SCAN_EVERY,
        fetch: craft_ipc::fetch::FetchOptions::default(),
    });
    // The socket file is `filmcraft-<pid>`. Drop the previous listener before the new bind.
    let mut slot = server_lock();
    *slot = None;
    match Server::start(ServerConfig {
        app: "filmcraft".into(),
        app_version: env!("CARGO_PKG_VERSION").into(),
        instance: None,
        runtime,
        tcp_port: None,
        sink,
        activate: host.activate_handler(),
        frames: Arc::new(NoFrames),
    }) {
        Ok(server) => *slot = Some(server),
        Err(err) => log::warn!("control server failed to start: {err}"),
    }
}

/// Start the token-checked TCP server. Returns why it did not start.
fn start_tcp(port: u16, ctx: egui::Context, token: Option<String>, tx: Sender<ControlRequest>, ready: Arc<AtomicBool>) -> Option<DisabledWhy> {
    // An inherited environment variable shares the credential with the owner's bridge process
    // without exposing it in command-line arguments, logs, or a shared discovery file.
    let Some(token) = token.filter(|t| t.len() == 64 && t.bytes().all(|b| b.is_ascii_hexdigit())) else {
        // The reason goes to the log (a desktop launch has no stderr to read; #3436) and back to
        // the app, which shows it in the status bar.
        log::warn!("control server disabled: {}", DisabledWhy::Token.reason());
        return Some(DisabledWhy::Token);
    };
    match TcpListener::bind(("127.0.0.1", port)) {
        Ok(listener) => {
            log::info!("control server listening on 127.0.0.1:{port}");
            std::thread::spawn(move || accept_loop(listener, tx, ctx, token, ready));
            None
        }
        Err(error) => {
            let why = DisabledWhy::Bind { port, error: error.to_string() };
            log::warn!("control server disabled: {}", why.reason());
            Some(why)
        }
    }
}

fn server_lock() -> MutexGuard<'static, Option<Server>> {
    SERVER.lock().unwrap_or_else(PoisonError::into_inner)
}

#[cfg(test)]
fn take_server() -> Option<Server> {
    server_lock().take()
}

struct UiSink {
    tx: Sender<ControlRequest>,
    ctx: egui::Context,
    ready: Arc<AtomicBool>,
}

impl ControlSink for UiSink {
    fn submit(&self, method: String, params: Value) -> Option<Receiver<Value>> {
        // Same rule as the TCP server: before the app drains the queue a request would outlive
        // its reply wait and still run after startup.
        if !self.ready.load(Ordering::Relaxed) {
            return None;
        }
        let (req, rx) = ControlRequest::new(method, params);
        if self.tx.send(req).is_err() {
            return None;
        }
        self.ctx.request_repaint();
        Some(rx)
    }
}

struct NoFrames;

impl Frames for NoFrames {
    fn open(&self, _params: &serde_json::Map<String, Value>) -> Result<FrameOpen, LinkError> {
        Err(LinkError::NotReady("frames are not available".into()))
    }

    fn render(&self, _ticket: &str, _header: &Value) -> Result<(Value, Vec<u8>), LinkError> {
        Err(LinkError::NotReady("frames are not available".into()))
    }

    fn close_ticket(&self, _ticket: &str) {}
}

fn accept_loop(listener: TcpListener, tx: Sender<ControlRequest>, ctx: egui::Context, token: String, ready: Arc<AtomicBool>) {
    let open = Arc::new(AtomicUsize::new(0));
    for mut stream in listener.incoming().flatten() {
        if open.fetch_add(1, Ordering::SeqCst) >= MAX_CONNECTIONS {
            open.fetch_sub(1, Ordering::SeqCst);
            let _ = writeln!(stream, "{}", json!({"ok": false, "error": "too many control connections"}));
            continue;
        }
        let tx = tx.clone();
        let ctx = ctx.clone();
        let token = token.clone();
        let ready = Arc::clone(&ready);
        // The slot is given back when the guard drops: after `serve` returns, if it panics, or if
        // the thread can't be started (the closure, and with it the guard, is dropped).
        let slot = Slot(Arc::clone(&open));
        let spawned = std::thread::Builder::new().name("control-conn".into()).spawn(move || {
            let _slot = slot;
            serve(stream, &tx, &ctx, &token, ready);
        });
        if let Err(e) = spawned {
            log::warn!("control server could not start a connection thread: {e}");
        }
    }
}

/// One counted connection; decrements the open-connection count when dropped.
struct Slot(Arc<AtomicUsize>);

impl Drop for Slot {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

fn serve(stream: TcpStream, tx: &Sender<ControlRequest>, ctx: &egui::Context, token: &str, ready: Arc<AtomicBool>) {
    if let Err(e) = serve_with_timeout(stream, tx, ctx, token, CONNECTION_TIMEOUT, ready) {
        log::warn!("could not configure control connection: {e}");
    }
}

fn serve_with_timeout(
    stream: TcpStream,
    tx: &Sender<ControlRequest>,
    ctx: &egui::Context,
    token: &str,
    timeout: Duration,
    ready: Arc<AtomicBool>,
) -> std::io::Result<()> {
    stream.set_read_timeout(Some(timeout))?;
    stream.set_write_timeout(Some(timeout))?;
    let read = stream.try_clone()?;
    serve_lines(read, stream, token, |method, params| {
        // Until the app can drain the queue, a request would outlive its 60 s reply wait and
        // still run after startup: reject it instead, so a client retry is the only copy (#3436).
        if !ready.load(Ordering::Relaxed) {
            return Some(json!({"ok": false, "error": "FilmCraft is still starting; send the request again"}));
        }
        let (req, rrx) = ControlRequest::new(method, params);
        tx.send(req).ok()?;
        ctx.request_repaint();
        Some(rrx.recv_timeout(Duration::from_secs(60)).unwrap_or_else(|_| json!({"ok": false, "error": "timeout"})))
    });
    Ok(())
}

/// One parsed line.
enum Line {
    Request {
        id: Value,
        method: String,
        params: Value,
    },
    Blank,
    /// Not a request: reply with this error and close the connection.
    Reject(String),
}

fn parse_line(bytes: &[u8], token: &str) -> Line {
    let Ok(text) = std::str::from_utf8(bytes) else {
        return Line::Reject("not a control request (invalid UTF-8); closing the connection".into());
    };
    let text = text.trim();
    if text.is_empty() {
        return Line::Blank;
    }
    let msg = match serde_json::from_str::<Value>(text) {
        Ok(v) => v,
        Err(e) => return Line::Reject(format!("bad JSON ({e}); expected one JSON request object per line; closing the connection")),
    };
    let Some(obj) = msg.as_object() else {
        return Line::Reject("not a control request (expected a JSON object); closing the connection".into());
    };
    let Some(method) = obj.get("method").and_then(Value::as_str) else {
        return Line::Reject("not a control request (missing string `method`); closing the connection".into());
    };
    // Authenticate every request before constructing or enqueueing a ControlRequest. A valid
    // earlier request must not authorize a later one with a missing or incorrect credential.
    let authenticated = obj.get("token").and_then(Value::as_str).is_some_and(|supplied| bool::from(supplied.as_bytes().ct_eq(token.as_bytes())));
    if !authenticated {
        return Line::Reject("control authentication failed; closing the connection".into());
    }
    Line::Request { id: obj.get("id").cloned().unwrap_or(Value::Null), method: method.to_string(), params: obj.get("params").cloned().unwrap_or(json!({})) }
}

#[derive(Debug, PartialEq, Eq)]
enum ReadFailure {
    Timeout,
    Rejected(String),
}

/// Read one `\n`-terminated line of at most `max` bytes into `buf` (newline stripped).
/// `Ok(false)` at end of stream; `Err` when the line is too long or the read fails.
fn read_bounded_line(reader: &mut impl BufRead, buf: &mut Vec<u8>, max: usize) -> Result<bool, ReadFailure> {
    buf.clear();
    // `take` bounds the bytes buffered for one line: a peer can't make us allocate more than `max`.
    let limit = u64::try_from(max).unwrap_or(u64::MAX).saturating_add(1);
    let n = reader.take(limit).read_until(b'\n', buf).map_err(|e| match e.kind() {
        std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock => ReadFailure::Timeout,
        _ => ReadFailure::Rejected(e.to_string()),
    })?;
    if n == 0 {
        return Ok(false);
    }
    if buf.last() == Some(&b'\n') {
        buf.pop();
        if buf.last() == Some(&b'\r') {
            buf.pop();
        }
        return Ok(true);
    }
    if buf.len() > max {
        return Err(ReadFailure::Rejected(format!("request line longer than {max} bytes; closing the connection")));
    }
    // Last line without a newline, then end of stream.
    Ok(true)
}

/// Serve requests from `read`, answering on `out`, until the peer closes, sends something that is
/// not a request, or `handle` returns `None` (app gone). `handle` gets `(method, params)` and
/// returns the reply object (the request's `id` is added here).
fn serve_lines<R: Read, W: Write>(read: R, mut out: W, token: &str, mut handle: impl FnMut(String, Value) -> Option<Value>) {
    let mut reader = BufReader::new(read);
    let mut buf = Vec::new();
    loop {
        let reject = match read_bounded_line(&mut reader, &mut buf, MAX_LINE) {
            Ok(false) => return,
            // Expiration is not a request: a pooled client must see EOF and reconnect, rather
            // than read a stale error reply. A partial line must not be parsed on timeout.
            Err(ReadFailure::Timeout) => return,
            Err(ReadFailure::Rejected(e)) => e,
            Ok(true) => match parse_line(&buf, token) {
                Line::Blank => continue,
                Line::Reject(e) => e,
                Line::Request { id, method, params } => {
                    let Some(mut r) = handle(method, params) else { return };
                    if let Some(o) = r.as_object_mut() {
                        o.insert("id".into(), id);
                    }
                    if writeln!(out, "{r}").and_then(|()| out.flush()).is_err() {
                        return;
                    }
                    continue;
                }
            },
        };
        let _ = writeln!(out, "{}", json!({"ok": false, "error": reject})).and_then(|()| out.flush());
        return;
    }
}

#[cfg(test)]
mod tests;
