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
