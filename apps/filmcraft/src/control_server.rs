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
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
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

pub fn start(port: u16, ctx: egui::Context) -> Receiver<ControlRequest> {
    start_with_token(port, ctx, std::env::var("FILMCRAFT_CONTROL_TOKEN").ok())
}

fn start_with_token(port: u16, ctx: egui::Context, token: Option<String>) -> Receiver<ControlRequest> {
    let (tx, rx) = channel::<ControlRequest>();
    // An inherited environment variable shares the credential with the owner's bridge process
    // without exposing it in command-line arguments, logs, or a shared discovery file.
    let Some(token) = token.filter(|t| t.len() == 64 && t.bytes().all(|b| b.is_ascii_hexdigit())) else {
        eprintln!("filmcraft: control server disabled: FILMCRAFT_CONTROL_TOKEN must contain 64 random hex characters (see docs/control-protocol.md)");
        return rx;
    };
    let listener = match TcpListener::bind(("127.0.0.1", port)) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("filmcraft: control server failed to bind 127.0.0.1:{port}: {e}");
            return rx;
        }
    };
    eprintln!("filmcraft: control server listening on 127.0.0.1:{port}");
    std::thread::spawn(move || accept_loop(listener, tx, ctx, token));
    rx
}

fn accept_loop(listener: TcpListener, tx: Sender<ControlRequest>, ctx: egui::Context, token: String) {
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
        // The slot is given back when the guard drops: after `serve` returns, if it panics, or if
        // the thread can't be started (the closure, and with it the guard, is dropped).
        let slot = Slot(Arc::clone(&open));
        let spawned = std::thread::Builder::new().name("control-conn".into()).spawn(move || {
            let _slot = slot;
            serve(stream, &tx, &ctx, &token);
        });
        if let Err(e) = spawned {
            eprintln!("filmcraft: control server could not start a connection thread: {e}");
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

fn serve(stream: TcpStream, tx: &Sender<ControlRequest>, ctx: &egui::Context, token: &str) {
    if let Err(e) = serve_with_timeout(stream, tx, ctx, token, CONNECTION_TIMEOUT) {
        eprintln!("filmcraft: could not configure control connection: {e}");
    }
}

fn serve_with_timeout(stream: TcpStream, tx: &Sender<ControlRequest>, ctx: &egui::Context, token: &str, timeout: Duration) -> std::io::Result<()> {
    stream.set_read_timeout(Some(timeout))?;
    stream.set_write_timeout(Some(timeout))?;
    let read = stream.try_clone()?;
    serve_lines(read, stream, token, |method, params| {
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
mod tests {
    use super::*;

    const TOKEN: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

    fn run(input: &[u8]) -> (Vec<Value>, Vec<String>) {
        let mut out = Vec::new();
        let mut calls = Vec::new();
        serve_lines(input, &mut out, TOKEN, |m, _p| {
            calls.push(m);
            Some(json!({"ok": true, "result": null}))
        });
        let replies = String::from_utf8(out).unwrap().lines().map(|l| serde_json::from_str::<Value>(l).unwrap()).collect();
        (replies, calls)
    }

    #[test]
    fn requests_are_answered_in_order_with_ids() {
        let input = format!(
            "{{\"token\":\"{TOKEN}\",\"id\":1,\"method\":\"a\"}}\n\n{{\"token\":\"{TOKEN}\",\"id\":2,\"method\":\"b\",\"params\":{{}}}}\r\n{{\"token\":\"{TOKEN}\",\"method\":\"c\"}}"
        );
        let (r, calls) = run(input.as_bytes());
        assert_eq!(calls, ["a", "b", "c"]);
        assert_eq!(r.len(), 3);
        assert_eq!(r[0]["id"], 1);
        assert_eq!(r[1]["id"], 2);
        assert_eq!(r[2]["id"], Value::Null);
    }

    #[test]
    fn http_request_closes_before_the_body_runs() {
        // A browser's cross-origin `fetch` POST (issue #119): the request line is rejected and the
        // connection closed, so the JSON body is never executed.
        let http = b"POST / HTTP/1.1\r\nHost: 127.0.0.1:9876\r\nOrigin: https://attacker.example\r\nContent-Type: text/plain\r\n\r\n\
            {\"id\":7,\"method\":\"engine.execute\",\"params\":{\"command\":\"file.openDemoProject\",\"params\":{}}}\n";
        let (r, calls) = run(http);
        assert!(calls.is_empty(), "nothing may run: {calls:?}");
        assert_eq!(r.len(), 1);
        assert_eq!(r[0]["ok"], false);
        assert!(r[0]["error"].as_str().unwrap().contains("closing the connection"));
    }

    #[test]
    fn non_request_json_closes_the_connection() {
        for junk in [
            &b"[1,2]\n{\"method\":\"a\"}\n"[..],
            b"{\"id\":1}\n{\"method\":\"a\"}\n",
            b"{\"method\":5}\n{\"method\":\"a\"}\n",
            b"\xff\xfe\n{\"method\":\"a\"}\n",
        ] {
            let (r, calls) = run(junk);
            assert!(calls.is_empty(), "{calls:?}");
            assert_eq!(r.len(), 1);
            assert_eq!(r[0]["ok"], false);
        }
    }

    #[test]
    fn a_request_before_junk_still_runs() {
        let input = format!("{{\"token\":\"{TOKEN}\",\"method\":\"a\"}}\nGET / HTTP/1.1\n{{\"token\":\"{TOKEN}\",\"method\":\"b\"}}\n");
        let (r, calls) = run(input.as_bytes());
        assert_eq!(calls, ["a"]);
        assert_eq!(r.len(), 2);
        assert_eq!(r[1]["ok"], false);
    }

    #[test]
    fn overlong_line_is_refused() {
        // One byte over the limit, then a valid request: neither runs.
        let mut input = vec![b'x'; MAX_LINE + 1];
        input.extend_from_slice(b"\n{\"method\":\"a\"}\n");
        let (r, calls) = run(&input);
        assert!(calls.is_empty(), "{calls:?}");
        assert_eq!(r.len(), 1);
        assert!(r[0]["error"].as_str().unwrap().contains("longer than"));
    }

    #[test]
    fn line_at_the_limit_is_accepted() {
        let mut buf = Vec::new();
        let line = vec![b'a'; 10];
        let mut input = line.clone();
        input.push(b'\n');
        assert_eq!(read_bounded_line(&mut &input[..], &mut buf, 10), Ok(true));
        assert_eq!(buf, line);
        assert!(read_bounded_line(&mut &[b'a'; 11][..], &mut buf, 10).is_err());
    }

    #[test]
    fn tcp_connection_is_closed_after_junk() {
        // End to end over a real loopback socket (the transport the app uses).
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let addr = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let (s, _) = listener.accept().unwrap();
            let read = s.try_clone().unwrap();
            serve_lines(read, s, TOKEN, |_, _| Some(json!({"ok": true})));
        });
        let mut c = TcpStream::connect(addr).unwrap();
        c.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        c.write_all(format!("{{\"token\":\"{TOKEN}\",\"id\":7,\"method\":\"x\"}}\nhello\n{{\"token\":\"{TOKEN}\",\"method\":\"y\"}}\n").as_bytes()).unwrap();
        let mut text = String::new();
        c.read_to_string(&mut text).unwrap(); // returns at EOF: the server closed the connection
        server.join().unwrap();
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 2, "{text}");
        assert!(lines[0].contains("\"id\":7"));
        assert!(lines[1].contains("closing the connection"));
    }

    #[test]
    fn extra_connections_are_refused() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let addr = listener.local_addr().unwrap();
        let (tx, _rx) = channel::<ControlRequest>();
        std::thread::spawn(move || accept_loop(listener, tx, egui::Context::default(), TOKEN.into()));
        // Idle connections are accepted in order and held open.
        let held: Vec<TcpStream> = (0..16).map(|_| TcpStream::connect(addr).unwrap()).collect();
        let mut extra = TcpStream::connect(addr).unwrap();
        extra.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        let mut line = String::new();
        BufReader::new(&mut extra).read_line(&mut line).unwrap();
        assert!(line.contains("too many control connections"), "{line}");
        drop(held);
    }

    /// Idle and unterminated requests must release a counted connection without client EOF.
    fn timed_out_connection_releases_slot(partial: &[u8]) {
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let addr = listener.local_addr().unwrap();
        let open = Arc::new(AtomicUsize::new(1));
        let counted = Arc::clone(&open);
        let (tx, rx) = channel::<ControlRequest>();
        let (done, finished) = channel();
        let server = std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            {
                let _slot = Slot(counted);
                serve_with_timeout(stream, &tx, &egui::Context::default(), TOKEN, Duration::from_millis(100)).unwrap();
            }
            done.send(()).unwrap();
        });
        let mut client = TcpStream::connect(addr).unwrap();
        client.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        client.write_all(partial).unwrap();
        let completion = finished.recv_timeout(Duration::from_secs(5));
        let mut unsolicited = String::new();
        if completion.is_ok() {
            client.read_to_string(&mut unsolicited).unwrap();
        }
        // Always unblock and join the test worker before asserting, even on the unfixed server.
        let unexpected_command = rx.try_recv().is_ok();
        drop(rx);
        drop(client);
        server.join().unwrap();
        assert!(completion.is_ok(), "an idle peer held its connection slot until the client closed");
        assert!(unsolicited.is_empty(), "expiration must not leave a stale reply for a pooled client: {unsolicited}");
        assert_eq!(open.load(Ordering::SeqCst), 0);
        assert!(!unexpected_command, "an incomplete request must not reach the UI queue");
    }

    #[test]
    /// A silent unauthenticated peer must not retain control-server capacity.
    fn idle_connection_releases_its_slot() {
        timed_out_connection_releases_slot(b"");
    }

    #[test]
    /// Sending a partial JSON line must not bypass idle expiration.
    fn unterminated_request_releases_its_slot() {
        timed_out_connection_releases_slot(b"{\"method\":");
    }

    #[test]
    /// A timeout is not EOF: valid JSON without its newline must not be dispatched.
    fn valid_unterminated_request_is_not_dispatched() {
        let request = json!({"id": 1, "token": TOKEN, "method": "ui.inspect"}).to_string();
        timed_out_connection_releases_slot(request.as_bytes());
    }

    #[test]
    /// Waiting for the UI is separate from socket idleness; buffered requests remain answerable.
    fn idle_timeout_does_not_cancel_a_pending_ui_reply() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let addr = listener.local_addr().unwrap();
        let (tx, rx) = channel::<ControlRequest>();
        let server = std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            serve_with_timeout(stream, &tx, &egui::Context::default(), TOKEN, Duration::from_millis(100)).unwrap();
        });
        let mut client = TcpStream::connect(addr).unwrap();
        client.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        let mut read = BufReader::new(client.try_clone().unwrap());
        // Buffer complete requests, as the bridge does. Queue both before waiting for replies:
        // client scheduling after reply 1 must not create a genuine idle gap in this test.
        let requests: String = [1, 2].into_iter().map(|id| format!("{}\n", json!({"token": TOKEN, "id": id, "method": "ui.inspect"}))).collect();
        client.write_all(requests.as_bytes()).unwrap();
        for id in [1, 2] {
            let request = rx.recv_timeout(Duration::from_secs(5)).unwrap_or_else(|e| panic!("request {id} did not reach the UI: {e}"));
            // Deliberately exceed the read timeout while the server is awaiting a UI reply.
            std::thread::sleep(Duration::from_millis(250));
            request.reply.send(json!({"ok": true})).unwrap();
            let mut line = String::new();
            read.read_line(&mut line).unwrap();
            assert_eq!(serde_json::from_str::<Value>(&line).unwrap(), json!({"id": id, "ok": true}));
        }
        client.shutdown(std::net::Shutdown::Write).unwrap();
        server.join().unwrap();
    }

    #[test]
    /// An active client can send another request after consuming its previous reply.
    fn active_connection_can_be_reused() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let addr = listener.local_addr().unwrap();
        let (tx, rx) = channel::<ControlRequest>();
        let server = std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            serve_with_timeout(stream, &tx, &egui::Context::default(), TOKEN, Duration::from_secs(5)).unwrap();
        });
        let mut client = TcpStream::connect(addr).unwrap();
        client.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        let mut read = BufReader::new(client.try_clone().unwrap());
        for id in [1, 2] {
            let line = format!("{}\n", json!({"token": TOKEN, "id": id, "method": "ui.inspect"}));
            client.write_all(line.as_bytes()).unwrap();
            rx.recv_timeout(Duration::from_secs(5)).unwrap().reply.send(json!({"ok": true})).unwrap();
            let mut reply = String::new();
            read.read_line(&mut reply).unwrap();
            assert_eq!(serde_json::from_str::<Value>(&reply).unwrap(), json!({"id": id, "ok": true}));
        }
        client.shutdown(std::net::Shutdown::Write).unwrap();
        server.join().unwrap();
    }

    #[test]
    fn missing_or_invalid_token_leaves_the_listener_closed() {
        for token in [None, Some(String::new()), Some("a".repeat(63)), Some("a".repeat(65)), Some("z".repeat(64))] {
            let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
            let port = listener.local_addr().unwrap().port();
            drop(listener);
            let rx = start_with_token(port, egui::Context::default(), token);
            assert!(matches!(rx.try_recv(), Err(std::sync::mpsc::TryRecvError::Disconnected)));
            let _still_available = TcpListener::bind(("127.0.0.1", port)).unwrap();
        }
    }

    #[test]
    fn unauthenticated_save_as_closes_before_any_command_runs() {
        for token in
            [Value::Null, json!(false), json!(42), json!([]), json!({}), json!(""), json!("a".repeat(64)), json!(&TOKEN[..63]), json!(format!("{TOKEN}0"))]
        {
            let mut request = json!({"id": 1, "method": "engine.execute", "params": {"command": "file.saveAs", "params": {"path": "/owner/project.fcproj"}}});
            if !token.is_null() {
                request["token"] = token;
            }
            // A valid request after a rejected one must never execute, either.
            let input = format!("{request}\n{}\n", json!({"token": TOKEN, "method": "ui.inspect"}));
            let (replies, calls) = run(input.as_bytes());
            assert!(calls.is_empty());
            assert_eq!(replies, [json!({"ok": false, "error": "control authentication failed; closing the connection"})]);
        }
    }

    #[test]
    fn authentication_is_required_on_every_request() {
        let input = format!(
            "{}\n{}\n{}\n",
            json!({"token": TOKEN, "method": "ui.inspect"}),
            json!({"method": "app.quit"}),
            json!({"token": TOKEN, "method": "app.quit"})
        );
        let (replies, calls) = run(input.as_bytes());
        assert_eq!(calls, ["ui.inspect"]);
        assert_eq!(replies.len(), 2);
        assert_eq!(replies[1]["ok"], false);
    }

    #[test]
    fn tcp_authentication_happens_before_enqueueing() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let addr = listener.local_addr().unwrap();
        let (tx, rx) = channel::<ControlRequest>();
        let server = std::thread::spawn(move || {
            let (s, _) = listener.accept().unwrap();
            serve(s, &tx, &egui::Context::default(), TOKEN);
        });
        let mut c = TcpStream::connect(addr).unwrap();
        c.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        c.write_all(b"{\"method\":\"engine.execute\",\"params\":{\"command\":\"file.saveAs\",\"params\":{\"path\":\"/owner/project.fcproj\"}}}\n").unwrap();
        c.shutdown(std::net::Shutdown::Write).unwrap();
        let mut reply = String::new();
        c.read_to_string(&mut reply).unwrap();
        server.join().unwrap();
        assert!(reply.contains("control authentication failed"));
        assert!(matches!(rx.try_recv(), Err(std::sync::mpsc::TryRecvError::Disconnected)));
    }

    #[test]
    fn authenticated_tcp_request_reaches_the_ui_queue() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let addr = listener.local_addr().unwrap();
        let (tx, rx) = channel::<ControlRequest>();
        let server = std::thread::spawn(move || {
            let (s, _) = listener.accept().unwrap();
            serve(s, &tx, &egui::Context::default(), TOKEN);
        });
        let params = json!({"command": "file.saveAs", "params": {"path": "/owner/project.fcproj"}});
        let mut c = TcpStream::connect(addr).unwrap();
        c.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        writeln!(c, "{}", json!({"id": 7, "token": TOKEN, "method": "engine.execute", "params": params})).unwrap();
        c.shutdown(std::net::Shutdown::Write).unwrap();
        let request = rx.recv_timeout(Duration::from_secs(5)).unwrap();
        assert_eq!(request.method, "engine.execute");
        assert_eq!(request.params, params);
        request.reply.send(json!({"ok": true})).unwrap();
        let mut reply = String::new();
        c.read_to_string(&mut reply).unwrap();
        server.join().unwrap();
        assert_eq!(serde_json::from_str::<Value>(&reply).unwrap(), json!({"id": 7, "ok": true}));
    }
}
