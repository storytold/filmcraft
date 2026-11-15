//! Localhost JSON-lines control server (one request per line, one reply per line). Loopback only.
//!
//! The server only ever reads requests: a line that is not a JSON object with a string `method`
//! (an HTTP request line from a browser's cross-origin `fetch`, a stray `nc`, binary junk) gets
//! one error reply and the connection is closed, so nothing after it is executed. Lines are
//! bounded ([`MAX_LINE`]) and so is the number of open connections ([`MAX_CONNECTIONS`]); junk
//! never reaches the UI thread. Every request must carry the session credential from
//! `FILMCRAFT_CONTROL_TOKEN`; without a configured credential the listener stays closed.
//! See `docs/control-protocol.md`.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::time::Duration;

#[cfg(test)]
use craft_control_protocol::read_bounded_line;
use filmcraft_ui_egui::ControlRequest;
use serde_json::{Value, json};

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
        }
    }
}

/// Start the server for `--control`. Returns the request queue, a flag the app sets once it can
/// answer requests (until then they are rejected with a retryable error), and — when the server
/// is *not* running — why (#3436), for the log and (for a bad token) the status bar.
pub fn start(port: u16, ctx: egui::Context) -> (Receiver<ControlRequest>, Arc<AtomicBool>, Option<DisabledWhy>) {
    start_with_token(port, ctx, std::env::var("FILMCRAFT_CONTROL_TOKEN").ok())
}

fn start_with_token(port: u16, ctx: egui::Context, token: Option<String>) -> (Receiver<ControlRequest>, Arc<AtomicBool>, Option<DisabledWhy>) {
    let (tx, rx) = channel::<ControlRequest>();
    // An inherited environment variable shares the credential with the owner's bridge process
    // without exposing it in command-line arguments, logs, or a shared discovery file.
    let Some(token) = token.filter(|t| t.len() == 64 && t.bytes().all(|b| b.is_ascii_hexdigit())) else {
        // The reason goes to the log (a desktop launch has no stderr to read; #3436) and back to
        // the app, which shows it in the status bar.
        log::warn!("control server disabled: {}", DisabledWhy::Token.reason());
        return (rx, Arc::new(AtomicBool::new(true)), Some(DisabledWhy::Token));
    };
    match TcpListener::bind(("127.0.0.1", port)) {
        Ok(listener) => {
            log::info!("control server listening on 127.0.0.1:{port}");
            // Requests that arrive before the app can answer them are rejected with a retryable
            // error instead of queued: a queued request outlives its 60 s reply wait, and a
            // client retry would then apply the same mutation twice (#3436).
            let ready = Arc::new(AtomicBool::new(false));
            let loop_ready = Arc::clone(&ready);
            std::thread::spawn(move || accept_loop(listener, tx, ctx, token, loop_ready));
            (rx, ready, None)
        }
        Err(error) => {
            let why = DisabledWhy::Bind { port, error: error.to_string() };
            log::warn!("control server disabled: {}", why.reason());
            (rx, Arc::new(AtomicBool::new(true)), Some(why))
        }
    }
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

/// Serve the shared wire protocol with FilmCraft's request size limit.
fn serve_lines<R: Read, W: Write>(read: R, out: W, token: &str, handle: impl FnMut(String, Value) -> Option<Value>) {
    craft_control_protocol::serve_lines(read, out, token, MAX_LINE, handle);
}

#[cfg(test)]
mod tests;
