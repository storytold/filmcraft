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
    for junk in
        [&b"[1,2]\n{\"method\":\"a\"}\n"[..], b"{\"id\":1}\n{\"method\":\"a\"}\n", b"{\"method\":5}\n{\"method\":\"a\"}\n", b"\xff\xfe\n{\"method\":\"a\"}\n"]
    {
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
    std::thread::spawn(move || accept_loop(listener, tx, egui::Context::default(), TOKEN.into(), Arc::new(AtomicBool::new(true))));
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
            serve_with_timeout(stream, &tx, &egui::Context::default(), TOKEN, Duration::from_millis(100), Arc::new(AtomicBool::new(true))).unwrap();
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
        serve_with_timeout(stream, &tx, &egui::Context::default(), TOKEN, Duration::from_millis(100), Arc::new(AtomicBool::new(true))).unwrap();
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
        serve_with_timeout(stream, &tx, &egui::Context::default(), TOKEN, Duration::from_secs(5), Arc::new(AtomicBool::new(true))).unwrap();
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
        let (rx, _ready, why) = start_with_token(port, egui::Context::default(), token);
        assert!(matches!(rx.try_recv(), Err(std::sync::mpsc::TryRecvError::Disconnected)));
        // The reason is returned for the status bar (#3436) as well as logged.
        let Some(w) = &why else { panic!("a bad token must report why") };
        assert!(matches!(w, DisabledWhy::Token));
        assert!(w.reason().contains("FILMCRAFT_CONTROL_TOKEN"), "{}", w.reason());
        let _still_available = TcpListener::bind(("127.0.0.1", port)).unwrap();
    }
}

#[test]
fn a_refused_bind_is_reported_and_leaves_the_listener_closed() {
    // The port is held: the server cannot start, and the reason comes back for the log.
    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let port = listener.local_addr().unwrap().port();
    let (rx, _ready, why) = start_with_token(port, egui::Context::default(), Some(TOKEN.into()));
    assert!(matches!(rx.try_recv(), Err(std::sync::mpsc::TryRecvError::Disconnected)));
    let Some(w) = &why else { panic!("a refused bind must report why") };
    assert!(matches!(w, DisabledWhy::Bind { .. }));
    let reason = w.reason();
    assert!(reason.contains("could not bind") && reason.contains(&port.to_string()), "{reason}");
}

#[test]
fn a_started_server_returns_no_notice() {
    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let port = listener.local_addr().unwrap().port();
    drop(listener);
    let (_rx, _ready, why) = start_with_token(port, egui::Context::default(), Some(TOKEN.into()));
    assert!(why.is_none());
}

#[test]
fn requests_before_ready_are_rejected_not_queued() {
    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let port = listener.local_addr().unwrap().port();
    drop(listener);
    let (rx, ready, why) = start_with_token(port, egui::Context::default(), Some(TOKEN.into()));
    assert!(why.is_none());
    let mut c = TcpStream::connect(("127.0.0.1", port)).unwrap();
    c.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    // The app is still starting: the request is answered without reaching the UI queue, so a
    // client retry cannot apply the same mutation twice (#3436).
    writeln!(c, "{}", json!({"id": 1, "token": TOKEN, "method": "ui.inspect"})).unwrap();
    let mut line = String::new();
    BufReader::new(&mut c).read_line(&mut line).unwrap();
    assert!(line.contains("still starting"), "{line}");
    assert!(matches!(rx.try_recv(), Err(std::sync::mpsc::TryRecvError::Empty)), "{line}");
    // Once the app is ready the same connection is served normally.
    ready.store(true, Ordering::Relaxed);
    writeln!(c, "{}", json!({"id": 2, "token": TOKEN, "method": "ui.inspect"})).unwrap();
    let request = rx.recv_timeout(Duration::from_secs(5)).unwrap();
    assert_eq!(request.method, "ui.inspect");
    request.reply.send(json!({"ok": true})).unwrap();
    let mut line = String::new();
    BufReader::new(&mut c).read_line(&mut line).unwrap();
    assert!(line.contains("\"id\":2"), "{line}");
}

#[test]
fn disable_reasons_name_their_cause() {
    assert!(DisabledWhy::Token.reason().contains("FILMCRAFT_CONTROL_TOKEN"));
    let bind = DisabledWhy::Bind { port: 9876, error: "address in use".into() };
    let reason = bind.reason();
    assert!(reason.contains("9876") && reason.contains("address in use"), "{reason}");
}

#[test]
fn unauthenticated_save_as_closes_before_any_command_runs() {
    for token in [Value::Null, json!(false), json!(42), json!([]), json!({}), json!(""), json!("a".repeat(64)), json!(&TOKEN[..63]), json!(format!("{TOKEN}0"))]
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
    let input =
        format!("{}\n{}\n{}\n", json!({"token": TOKEN, "method": "ui.inspect"}), json!({"method": "app.quit"}), json!({"token": TOKEN, "method": "app.quit"}));
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
        serve(s, &tx, &egui::Context::default(), TOKEN, Arc::new(AtomicBool::new(true)));
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
        serve(s, &tx, &egui::Context::default(), TOKEN, Arc::new(AtomicBool::new(true)));
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

/// A scratch data folder, removed when the guard drops (also when the test panics).
fn scratch_dir() -> tempfile::TempDir {
    tempfile::tempdir().unwrap()
}

fn free_port() -> u16 {
    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    listener.local_addr().unwrap().port()
}

/// Send one request with `token` to a server on `port` and return whether it reached the UI queue.
fn reaches_the_queue(port: u16, rx: &Receiver<ControlRequest>, ready: &AtomicBool, token: &str) -> bool {
    ready.store(true, Ordering::Relaxed);
    let mut c = TcpStream::connect(("127.0.0.1", port)).unwrap();
    c.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    writeln!(c, "{}", json!({"id": 1, "token": token, "method": "ui.inspect"})).unwrap();
    match rx.recv_timeout(Duration::from_secs(2)) {
        Ok(request) => {
            let _ = request.reply.send(json!({"ok": true}));
            true
        }
        Err(_) => false,
    }
}

fn token_path(dir: &Path) -> std::path::PathBuf {
    dir.join(filmcraft_engine::autosave::CONTROL_TOKEN_FILE)
}

#[test]
fn a_settings_start_writes_a_fresh_owner_only_token_that_the_server_accepts() {
    let tmp = scratch_dir();
    // A data folder that does not exist yet is created.
    let dir = tmp.path().join("nested").join("FilmCraft");
    let port = free_port();
    let (rx, ready, why) = start_with(port, egui::Context::default(), None, Some(&dir));
    assert_eq!(why, None);
    let token = filmcraft_engine::autosave::load_control_token(&dir).unwrap().unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(std::fs::metadata(token_path(&dir)).unwrap().permissions().mode() & 0o777, 0o600);
        assert_eq!(std::fs::metadata(&dir).unwrap().permissions().mode() & 0o777, 0o700);
    }
    // No temporary file is left behind.
    assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 1);
    assert!(reaches_the_queue(port, &rx, &ready, &token));
    assert!(!reaches_the_queue(port, &rx, &ready, TOKEN), "only the new token is accepted");
}

#[test]
fn every_settings_start_replaces_the_token() {
    let tmp = scratch_dir();
    let first = write_token_file(tmp.path()).unwrap();
    let second = write_token_file(tmp.path()).unwrap();
    assert_ne!(first, second);
    assert_eq!(filmcraft_engine::autosave::load_control_token(tmp.path()).unwrap().as_deref(), Some(second.as_str()));
}

#[test]
fn the_environment_token_wins_and_no_file_is_written() {
    let tmp = scratch_dir();
    let port = free_port();
    let (rx, ready, why) = start_with(port, egui::Context::default(), Some(TOKEN.into()), Some(tmp.path()));
    assert_eq!(why, None);
    assert!(!token_path(tmp.path()).exists());
    assert!(reaches_the_queue(port, &rx, &ready, TOKEN));
}

#[test]
fn control_without_the_environment_token_stays_closed_and_ignores_the_file() {
    // `--control` passes no token folder: a token file, even a valid one, is not used, as before.
    let tmp = scratch_dir();
    write_token_file(tmp.path()).unwrap();
    let port = free_port();
    let (rx, _ready, why) = start_with(port, egui::Context::default(), None, None);
    assert_eq!(why, Some(DisabledWhy::Token));
    assert!(matches!(rx.try_recv(), Err(std::sync::mpsc::TryRecvError::Disconnected)));
    let _still_available = TcpListener::bind(("127.0.0.1", port)).unwrap();
}

#[test]
fn a_busy_port_leaves_the_running_servers_token_alone() {
    let tmp = scratch_dir();
    let running = write_token_file(tmp.path()).unwrap();
    let held = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let port = held.local_addr().unwrap().port();
    let (_rx, _ready, why) = start_with(port, egui::Context::default(), None, Some(tmp.path()));
    assert!(matches!(why, Some(DisabledWhy::Bind { .. })), "{why:?}");
    assert_eq!(filmcraft_engine::autosave::load_control_token(tmp.path()).unwrap().as_deref(), Some(running.as_str()));
}

#[test]
fn a_token_file_that_cannot_be_written_is_its_own_reason() {
    let tmp = scratch_dir();
    // The "data folder" is a file: nothing can be created in it.
    let not_a_dir = tmp.path().join("file");
    std::fs::write(&not_a_dir, b"x").unwrap();
    let port = free_port();
    let (rx, _ready, why) = start_with(port, egui::Context::default(), None, Some(&not_a_dir));
    let Some(DisabledWhy::TokenFile { error }) = &why else { panic!("{why:?}") };
    assert!(why.as_ref().unwrap().reason().contains("token file"), "{error}");
    assert!(matches!(rx.try_recv(), Err(std::sync::mpsc::TryRecvError::Disconnected)));
    let _still_available = TcpListener::bind(("127.0.0.1", port)).unwrap();
}

#[cfg(unix)]
#[test]
fn a_loose_or_linked_token_file_is_replaced_not_written_through() {
    use std::os::unix::fs::PermissionsExt;
    let tmp = scratch_dir();
    // A world-readable file with a valid token: refused when read, replaced on the next start.
    std::fs::write(token_path(tmp.path()), format!("{TOKEN}\n")).unwrap();
    std::fs::set_permissions(token_path(tmp.path()), std::fs::Permissions::from_mode(0o644)).unwrap();
    assert!(filmcraft_engine::autosave::load_control_token(tmp.path()).is_err());
    let token = write_token_file(tmp.path()).unwrap();
    assert_eq!(std::fs::metadata(token_path(tmp.path())).unwrap().permissions().mode() & 0o777, 0o600);
    assert_eq!(filmcraft_engine::autosave::load_control_token(tmp.path()).unwrap().as_deref(), Some(token.as_str()));
    // A link planted in place of the file: the link is replaced, its target is not touched.
    let target = tmp.path().join("elsewhere");
    std::fs::write(&target, b"keep").unwrap();
    std::fs::remove_file(token_path(tmp.path())).unwrap();
    std::os::unix::fs::symlink(&target, token_path(tmp.path())).unwrap();
    assert!(filmcraft_engine::autosave::load_control_token(tmp.path()).is_err(), "a link is never read");
    let token = write_token_file(tmp.path()).unwrap();
    assert_eq!(std::fs::read(&target).unwrap(), b"keep");
    assert!(std::fs::symlink_metadata(token_path(tmp.path())).unwrap().file_type().is_file());
    assert_eq!(filmcraft_engine::autosave::load_control_token(tmp.path()).unwrap().as_deref(), Some(token.as_str()));
}

#[test]
fn the_settings_port_is_read_from_the_preferences_file() {
    let tmp = scratch_dir();
    let dir = tmp.path();
    let prefs_path = dir.join(filmcraft_engine::autosave::PREFS_FILE);
    // No preferences file, or the setting off: no server.
    assert_eq!(port_from_settings(dir), None);
    let mut prefs = filmcraft_engine::autosave::Preferences::default();
    prefs.control_server.port = 9911;
    prefs.save(&prefs_path).unwrap();
    assert_eq!(port_from_settings(dir), None);
    prefs.control_server.start_at_launch = true;
    prefs.save(&prefs_path).unwrap();
    assert_eq!(port_from_settings(dir), Some(9911));
    // Hand-edited files: out-of-range ports are clamped by the schema on load; wrong types,
    // missing fields and broken JSON fall back to the defaults (off, or port 9876) without a panic.
    for (text, want) in [
        (r#"{"version":2,"controlServer":{"startAtLaunch":true,"port":0}}"#, Some(1024)),
        (r#"{"version":2,"controlServer":{"startAtLaunch":true,"port":1023}}"#, Some(1024)),
        (r#"{"version":2,"controlServer":{"startAtLaunch":true,"port":1024}}"#, Some(1024)),
        (r#"{"version":2,"controlServer":{"startAtLaunch":true,"port":65535}}"#, Some(65_535)),
        (r#"{"version":2,"controlServer":{"startAtLaunch":true,"port":65536}}"#, Some(65_535)),
        (r#"{"version":2,"controlServer":{"startAtLaunch":true,"port":99999999}}"#, Some(65_535)),
        (r#"{"version":2,"controlServer":{"startAtLaunch":true,"port":-5}}"#, Some(1024)),
        (r#"{"version":2,"controlServer":{"startAtLaunch":true,"port":"9000"}}"#, Some(9876)),
        (r#"{"version":2,"controlServer":{"startAtLaunch":true}}"#, Some(9876)),
        (r#"{"version":2,"controlServer":{"startAtLaunch":"yes","port":9000}}"#, None),
        (r#"{"version":2,"controlServer":5}"#, None),
        (r#"{"version":2,"controlServer":{"startAtLaunch":tr"#, None),
    ] {
        std::fs::write(&prefs_path, text).unwrap();
        assert_eq!(port_from_settings(dir), want, "{text}");
    }
}
