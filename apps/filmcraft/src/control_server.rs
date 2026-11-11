//! Localhost JSON-lines control server (one request per line, one reply per line). Loopback only.
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
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::time::Duration;

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

/// Why the control server is not serving although `--control` was given (#3436).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DisabledWhy {
    /// `FILMCRAFT_CONTROL_TOKEN` is missing or is not 64 hex characters.
    Token,
    /// The port could not be bound. Showing this in the app is left to #3318, which reworks
    /// port-collision reporting; until then the reason is only logged.
    Bind { port: u16, error: String },
    /// Settings ▸ Control Server started the server, but its token file could not be written.
    TokenFile { error: String },
}

/// `--control-input-only` was given but the control server is not serving: the session would be
/// unreachable, so startup fails with this error and `main` exits directly — no dialog an
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
            Self::TokenFile { error } => format!("could not write the control token file: {error}"),
        }
    }
}

/// Start the server for `--control` / `FILMCRAFT_CONTROL_PORT` (`token_dir` is `None`) or for
/// Settings ▸ Control Server (`token_dir` is the data folder). Returns the request queue, a flag
/// the app sets once it can answer requests (until then they are rejected with a retryable error),
/// and — when the server is *not* running — why (#3436), for the log and the status bar.
///
/// The credential is `FILMCRAFT_CONTROL_TOKEN` whenever it is set. Without it, `--control` stays
/// closed as before, while a Settings start writes a fresh random token to
/// `<token_dir>/control-token` (see [`write_token_file`]) once the port is bound, for the bridge
/// to read; every Settings start replaces the previous token.
pub fn start(port: u16, ctx: egui::Context, token_dir: Option<&Path>) -> (Receiver<ControlRequest>, Arc<AtomicBool>, Option<DisabledWhy>) {
    start_with(port, ctx, std::env::var("FILMCRAFT_CONTROL_TOKEN").ok().filter(|t| !t.is_empty()), token_dir)
}

/// [`start`] with the value of `FILMCRAFT_CONTROL_TOKEN` passed in, so tests don't touch the
/// process environment.
fn start_with(
    port: u16,
    ctx: egui::Context,
    env_token: Option<String>,
    token_dir: Option<&Path>,
) -> (Receiver<ControlRequest>, Arc<AtomicBool>, Option<DisabledWhy>) {
    let dir = match (env_token, token_dir) {
        (Some(token), _) => {
            log::info!("control server credential: FILMCRAFT_CONTROL_TOKEN");
            return start_with_token(port, ctx, Some(token));
        }
        (None, None) => return start_with_token(port, ctx, None),
        (None, Some(dir)) => dir,
    };
    // Bind before writing: a second FilmCraft on the same port must not replace the token the
    // running one serves with.
    let listener = match bind(port) {
        Ok(l) => l,
        Err(why) => return disabled(why),
    };
    match write_token_file(dir) {
        Ok(token) => {
            log::info!("control server credential: {} (new for this launch)", dir.join(filmcraft_engine::autosave::CONTROL_TOKEN_FILE).display());
            serve_on(listener, port, ctx, token)
        }
        Err(e) => disabled(DisabledWhy::TokenFile { error: format!("{}: {e}", dir.display()) }),
    }
}

/// The control port from Settings ▸ Control Server, when "Start the control server when FilmCraft
/// starts" is on. Read straight from the preferences file: the server starts before the session.
pub fn port_from_settings(data_dir: &Path) -> Option<u16> {
    filmcraft_engine::autosave::Preferences::load(&data_dir.join(filmcraft_engine::autosave::PREFS_FILE)).control_server.launch_port()
}

/// Write a new random credential to `<dir>/control-token` and return it.
///
/// The token goes to a new temporary file in the same folder (created exclusively, so never through
/// an existing file or link; owner-only on Unix), is synced, and then renamed over
/// `control-token`. The rename replaces whatever was there (an old token, a link, a file with
/// loose permissions) without following it, and a crash leaves either the old file or the new
/// one, never a partial token. The folder is created owner-only on Unix when it does not exist.
fn write_token_file(dir: &Path) -> std::io::Result<String> {
    use std::fs::{DirBuilder, OpenOptions};
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes).map_err(|e| std::io::Error::other(e.to_string()))?;
    let token: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
    let mut builder = DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder.create(dir)?;
    let mut suffix = [0u8; 8];
    getrandom::fill(&mut suffix).map_err(|e| std::io::Error::other(e.to_string()))?;
    let suffix: String = suffix.iter().map(|b| format!("{b:02x}")).collect();
    let tmp = dir.join(format!(".{}.{suffix}.tmp", filmcraft_engine::autosave::CONTROL_TOKEN_FILE));
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let written = (|| {
        let mut file = options.open(&tmp)?;
        file.write_all(token.as_bytes())?;
        file.write_all(b"\n")?;
        file.sync_all()?;
        std::fs::rename(&tmp, dir.join(filmcraft_engine::autosave::CONTROL_TOKEN_FILE))
    })();
    if let Err(e) = written {
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }
    // Make the rename itself durable (Unix; a folder can't be opened as a file on Windows).
    #[cfg(unix)]
    if let Ok(d) = std::fs::File::open(dir) {
        let _ = d.sync_all();
    }
    Ok(token)
}

/// The server is not running: log why (a desktop launch has no stderr to read; #3436) and hand the
/// reason back for the status bar.
fn disabled(why: DisabledWhy) -> (Receiver<ControlRequest>, Arc<AtomicBool>, Option<DisabledWhy>) {
    log::warn!("control server disabled: {}", why.reason());
    let (_tx, rx) = channel::<ControlRequest>();
    (rx, Arc::new(AtomicBool::new(true)), Some(why))
}

fn bind(port: u16) -> Result<TcpListener, DisabledWhy> {
    TcpListener::bind(("127.0.0.1", port)).map_err(|error| DisabledWhy::Bind { port, error: error.to_string() })
}

fn start_with_token(port: u16, ctx: egui::Context, token: Option<String>) -> (Receiver<ControlRequest>, Arc<AtomicBool>, Option<DisabledWhy>) {
    // An inherited environment variable (or, for a Settings start, the owner-only token file)
    // shares the credential with the owner's bridge process without exposing it in command-line
    // arguments or logs.
    let Some(token) = token.filter(|t| filmcraft_engine::autosave::is_control_token(t)) else {
        return disabled(DisabledWhy::Token);
    };
    match bind(port) {
        Ok(listener) => serve_on(listener, port, ctx, token),
        Err(why) => disabled(why),
    }
}

/// Serve `listener` with `token` on a background thread.
fn serve_on(listener: TcpListener, port: u16, ctx: egui::Context, token: String) -> (Receiver<ControlRequest>, Arc<AtomicBool>, Option<DisabledWhy>) {
    let (tx, rx) = channel::<ControlRequest>();
    log::info!("control server listening on 127.0.0.1:{port}");
    // Requests that arrive before the app can answer them are rejected with a retryable
    // error instead of queued: a queued request outlives its 60 s reply wait, and a
    // client retry would then apply the same mutation twice (#3436).
    let ready = Arc::new(AtomicBool::new(false));
    let loop_ready = Arc::clone(&ready);
    std::thread::spawn(move || accept_loop(listener, tx, ctx, token, loop_ready));
    (rx, ready, None)
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
