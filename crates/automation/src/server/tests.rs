use super::*;

#[test]
fn render_scale_zero_means_no_limit() {
    assert_eq!(render_scale(0, 1920, 1080), 1.0);
    assert_eq!(render_scale(960, 1920, 1080), 0.5);
    assert_eq!(render_scale(960, 640, 360), 1.0);
    assert_eq!(render_scale(960, 0, 0), 1.0);
}

#[tokio::test]
async fn headless_commands_and_render() {
    let mut s = Session::default();
    s.execute("file.openDemoProject", json!({})).unwrap();
    let m = FilmcraftMcp::headless(s);
    let v = m.run("sequence.addEditAllTracks", json!({"seconds": 2.0})).await.unwrap();
    assert!(v["cuts"].as_u64().unwrap() >= 2);
    let r = m.render_frame(Parameters(RenderParams { seconds: Some(1.0), max_side: Some(320) })).await.unwrap();
    assert_ne!(r.is_error, Some(true));
}

/// `render_frame` / `render_preview` are annotated read-only (#114): rendering at `seconds`
/// leaves the playhead, and the selection that follows it, where they were, and still renders
/// the frame at `seconds`.
#[tokio::test]
async fn rendering_at_a_time_does_not_move_the_playhead() {
    let m = FilmcraftMcp::headless(demo());
    m.run("sequence.selectionFollowsPlayhead", json!({"on": true})).await.unwrap();
    m.run("playhead.set", json!({"seconds": 1.0})).await.unwrap();
    let before = m.run("sequence.inspect", json!({})).await.unwrap();
    let png = |r: CallToolResult| serde_json::to_string(&r.content).unwrap();
    let at_playhead = png(m.render_frame(Parameters(RenderParams { seconds: None, max_side: Some(64) })).await.unwrap());
    for seconds in [5.0, 8.0] {
        let r = m.render_frame(Parameters(RenderParams { seconds: Some(seconds), max_side: Some(64) })).await.unwrap();
        assert_ne!(png(r), at_playhead, "renders the frame at {seconds} s");
        m.render_preview(Parameters(RenderParams { seconds: Some(seconds), max_side: Some(64) })).await.unwrap();
    }
    let after = m.run("sequence.inspect", json!({})).await.unwrap();
    assert_eq!(after["playhead"], before["playhead"]);
    assert_eq!(after["selection"], before["selection"]);
}

/// One request/response exchange over `serve_io`, as an MCP client sees it.
struct Client {
    tx: tokio::io::DuplexStream,
    rx: tokio::io::Lines<tokio::io::BufReader<tokio::io::DuplexStream>>,
}

impl Client {
    fn start(s: Session) -> Self {
        use tokio::io::AsyncBufReadExt;
        let (tx, server_in) = tokio::io::duplex(1 << 22);
        let (server_out, rx) = tokio::io::duplex(1 << 22);
        tokio::spawn(FilmcraftMcp::headless(s).serve_io(server_in, server_out));
        Self { tx, rx: tokio::io::BufReader::new(rx).lines() }
    }
    async fn send(&mut self, line: &str) {
        use tokio::io::AsyncWriteExt;
        self.tx.write_all(format!("{line}\n").as_bytes()).await.unwrap();
    }
    async fn next(&mut self) -> Value {
        serde_json::from_str(&self.rx.next_line().await.unwrap().unwrap()).unwrap()
    }
    async fn ask(&mut self, msg: Value) -> Value {
        self.send(&msg.to_string()).await;
        self.next().await
    }
    async fn init(&mut self) -> Value {
        let r = self.ask(json!({"jsonrpc":"2.0","id":0,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"t","version":"1"}}})).await;
        self.send(&json!({"jsonrpc":"2.0","method":"notifications/initialized"}).to_string()).await;
        r
    }
    async fn call(&mut self, id: u64, name: &str, arguments: Value) -> Value {
        self.ask(json!({"jsonrpc":"2.0","id":id,"method":"tools/call","params":{"name":name,"arguments":arguments}})).await
    }
}

/// #499: `media_import` takes `paths` (a required array), and forwards `bin` / `image_sequence` to `file.import`.
#[test]
fn media_import_schema_and_params() {
    let m = FilmcraftMcp::headless(Session::default());
    let def = m.tool_router.get("media_import").unwrap();
    let schema = Value::Object((*def.input_schema).clone());
    assert_eq!(schema["properties"]["paths"]["type"], "array", "{schema}");
    assert!(schema["required"].as_array().unwrap().iter().any(|r| r == "paths"), "{schema}");
    let p: MediaImportParams = serde_json::from_value(json!({"paths": ["/a.png"], "bin": 7, "image_sequence": true})).unwrap();
    assert_eq!(media_import_params(p), json!({"paths": ["/a.png"], "bin": 7, "imageSequence": true}));
    let p: MediaImportParams = serde_json::from_value(json!({"paths": ["/a.wav"]})).unwrap();
    assert_eq!(media_import_params(p), json!({"paths": ["/a.wav"]}));
    assert!(serde_json::from_value::<MediaImportParams>(json!({"text": "/a.wav"})).is_err());
}

fn demo() -> Session {
    let mut s = Session::default();
    s.execute("file.openDemoProject", json!({})).unwrap();
    s
}

/// Every listed tool has a title and the four hints, and the core tools are listed.
#[tokio::test(flavor = "multi_thread")]
async fn tools_are_titled_and_annotated() {
    let mut c = Client::start(demo());
    let init = c.init().await;
    assert_eq!(init["result"]["serverInfo"]["name"], "filmcraft");
    let tools = c.ask(json!({"jsonrpc":"2.0","id":1,"method":"tools/list"})).await["result"]["tools"].as_array().cloned().unwrap();
    for t in &tools {
        assert!(t["title"].is_string(), "{} has no title", t["name"]);
        for h in ["readOnlyHint", "destructiveHint", "idempotentHint", "openWorldHint"] {
            assert!(t["annotations"][h].is_boolean(), "{} lacks {h}", t["name"]);
        }
    }
    let names: Vec<&str> = tools.iter().filter_map(|t| t["name"].as_str()).collect();
    for want in ["command_list", "command_run", "command_batch", "doc_inspect", "render_preview", "project_inspect", "render_frame", "ui_click"] {
        assert!(names.contains(&want), "{want} not listed");
    }
    let ro = |n: &str| tools.iter().find(|t| t["name"] == n).map(|t| t["annotations"]["readOnlyHint"].clone());
    assert_eq!((ro("command_list"), ro("command_run")), (Some(json!(true)), Some(json!(false))));
}

/// Strict arguments, malformed JSON, batch, doc_inspect and render_preview over the raw line
/// protocol.
#[tokio::test(flavor = "multi_thread")]
async fn strict_arguments_and_parse_errors() {
    let mut c = Client::start(demo());
    c.init().await;
    let r = c.call(1, "command_list", json!({"filter": "razor", "enabled_only": true})).await;
    assert!(r["result"]["content"][0]["text"].as_str().unwrap().contains("timeline.razor"), "{r}");
    // an unknown argument names itself and the accepted ones
    let r = c.call(2, "command_list", json!({"filtr": "x"})).await;
    assert_eq!(r["error"]["code"], -32602, "{r}");
    let m = r["error"]["message"].as_str().unwrap();
    assert!(m.contains("\"filtr\"") && m.contains("filter") && m.contains("enabled_only"), "{m}");
    let r = c.call(3, "doc_inspect", json!({"verbose": true})).await;
    assert_eq!(r["error"]["code"], -32602, "{r}");
    // command params stay free-form
    let r = c.call(4, "command_run", json!({"id": "playhead.set", "params": {"seconds": 0.5}})).await;
    assert_eq!(r["result"]["isError"], false, "{r}");
    // malformed JSON: a parse error, and the server keeps serving
    c.send("{\"jsonrpc\": \"2.0\", \"id\": 5, BROKEN").await;
    let r = c.next().await;
    assert_eq!((r["error"]["code"].as_i64(), r["id"].is_null()), (Some(-32700), true), "{r}");
    let r = c.ask(json!({"jsonrpc":"2.0","id":6,"method":"ping"})).await;
    assert!(r["result"].is_object(), "{r}");
    // batch: stops at the first failure and reports both
    let r = c.call(7, "command_batch", json!({"steps": [{"id": "playhead.set", "params": {"seconds": 0.5}}, {"id": "no.such"}, {"id": "playhead.set"}]})).await;
    assert_eq!(r["result"]["isError"], true, "{r}");
    let v: Value = serde_json::from_str(r["result"]["content"][0]["text"].as_str().unwrap()).unwrap();
    assert_eq!((v["completed"].as_u64(), v["failed"].as_u64(), v["results"].as_array().map(Vec::len)), (Some(1), Some(1), Some(2)), "{v}");
    let r = c.call(8, "doc_inspect", json!({})).await;
    let v: Value = serde_json::from_str(r["result"]["content"][0]["text"].as_str().unwrap()).unwrap();
    assert!(v["project"].is_object() && v["sequence"].is_object(), "{v}");
    let r = c.call(9, "render_preview", json!({"max_side": 64})).await;
    assert_eq!(r["result"]["content"][0]["type"], "image", "{r}");
}

/// `filmcraft://document` and `filmcraft://commands` are listed and readable as JSON.
#[tokio::test(flavor = "multi_thread")]
async fn resources_document_and_commands() {
    let mut c = Client::start(demo());
    let init = c.init().await;
    assert!(init["result"]["capabilities"]["resources"].is_object(), "{init}");
    assert!(init["result"]["instructions"].as_str().unwrap().contains("doc_inspect"), "{init}");
    let r = c.ask(json!({"jsonrpc":"2.0","id":1,"method":"resources/list"})).await;
    let uris: Vec<&str> = r["result"]["resources"].as_array().unwrap().iter().filter_map(|x| x["uri"].as_str()).collect();
    assert_eq!(uris, [DOCUMENT_URI, COMMANDS_URI], "{r}");
    let r = c.ask(json!({"jsonrpc":"2.0","id":2,"method":"resources/read","params":{"uri":DOCUMENT_URI}})).await;
    assert_eq!(r["result"]["contents"][0]["mimeType"], "application/json", "{r}");
    let v: Value = serde_json::from_str(r["result"]["contents"][0]["text"].as_str().unwrap()).unwrap();
    assert!(v["project"].is_object() && v["sequence"].is_object(), "{v}");
    let r = c.ask(json!({"jsonrpc":"2.0","id":3,"method":"resources/read","params":{"uri":COMMANDS_URI}})).await;
    assert!(r["result"]["contents"][0]["text"].as_str().unwrap().contains("timeline.razor"), "{r}");
    let r = c.ask(json!({"jsonrpc":"2.0","id":4,"method":"resources/read","params":{"uri":"filmcraft://nope"}})).await;
    assert!(r["error"]["code"].is_i64(), "{r}");
}

/// MCP 2026-07-28 clients (current Claude Code) negotiate per request and reject list and read
/// results without `resultType`, `ttlMs` and `cacheScope`; sessions that negotiated an older
/// revision through `initialize` get the old shape.
#[tokio::test(flavor = "multi_thread")]
async fn modern_clients_get_result_type_and_cache_hints() {
    let mut c = Client::start(Session::default());
    let meta = json!({
        "io.modelcontextprotocol/protocolVersion": "2026-07-28",
        "io.modelcontextprotocol/clientInfo": {"name": "t", "version": "1"},
        "io.modelcontextprotocol/clientCapabilities": {},
    });
    let r = c.ask(json!({"jsonrpc":"2.0","id":1,"method":"server/discover","params":{"_meta":meta}})).await;
    assert!(r["result"]["supportedVersions"].as_array().unwrap().contains(&json!("2026-07-28")), "{r}");
    for (id, method, params) in [
        (2, "tools/list", json!({"_meta": meta})),
        (3, "resources/list", json!({"_meta": meta})),
        (4, "resources/read", json!({"_meta": meta, "uri": DOCUMENT_URI})),
    ] {
        let r = c.ask(json!({"jsonrpc":"2.0","id":id,"method":method,"params":params})).await;
        assert_eq!(r["result"]["resultType"], "complete", "{method}: {r}");
        assert!(r["result"]["ttlMs"].is_u64(), "{method}: {r}");
        assert!(matches!(r["result"]["cacheScope"].as_str(), Some("public" | "private")), "{method}: {r}");
    }
    let mut c = Client::start(Session::default());
    c.init().await;
    for (id, method, params) in [(2, "tools/list", json!({})), (3, "resources/list", json!({})), (4, "resources/read", json!({"uri": DOCUMENT_URI}))] {
        let r = c.ask(json!({"jsonrpc":"2.0","id":id,"method":method,"params":params})).await;
        assert!(r["result"].is_object() && r["result"].get("resultType").is_none() && r["result"].get("ttlMs").is_none(), "{method}: {r}");
    }
}

/// docs/agents.md § Long exports: a blocking export reports `notifications/progress` for its
/// token (none without one), answers `ping` meanwhile, and `notifications/cancelled` stops
/// it, deletes the partial file and sends no response.
#[tokio::test(flavor = "multi_thread")]
async fn export_progress_and_cancel() {
    let dir = std::env::temp_dir().join(format!("filmcraft-mcp-progress-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let mut c = Client::start(demo());
    c.init().await;
    let export = |id: u64, name: &str, token: Option<Value>, seconds: f64| {
        let path = dir.join(name).to_string_lossy().to_string();
        let params = json!({"path": path, "format": "h264", "width": 640, "height": 360, "audio": false, "range": "custom", "startSeconds": 0, "endSeconds": seconds, "wait": true});
        let mut call =
            json!({"jsonrpc":"2.0","id":id,"method":"tools/call","params":{"name":"command_run","arguments":{"id":"file.exportMedia","params":params}}});
        if let Some(t) = token {
            call["params"]["_meta"] = json!({"progressToken": t});
        }
        call.to_string()
    };
    // no token: no notifications, the same response as before
    c.send(&export(1, "quiet.mp4", None, 0.5)).await;
    let m = c.next().await;
    assert_eq!((m["id"].as_u64(), &m["result"]["isError"]), (Some(1), &json!(false)), "{m}");
    // progress, strictly increasing, with a total; ping answered while the export runs. The
    // job is polled every 100 ms, so a fast machine finishes a short export within one poll:
    // a longer one is tried until the export outlasts a few polls.
    let (mut notes, mut pong) = (0, false);
    for (attempt, seconds) in [1.0, 4.0, 16.0].into_iter().enumerate() {
        let (id, ping, name) = (20 + attempt as u64, 30 + attempt as u64, format!("a{attempt}.mp4"));
        c.send(&export(id, &name, Some(json!("tok")), seconds)).await;
        let (mut last, mut pinged) = (-1.0, false);
        (notes, pong) = (0, false);
        let done = loop {
            let m = c.next().await;
            if m["method"] == "notifications/progress" {
                assert_eq!(m["params"]["progressToken"], "tok", "{m}");
                let p = m["params"]["progress"].as_f64().unwrap();
                assert!(p > last && m["params"]["total"].as_f64().unwrap() >= p, "{m}");
                last = p;
                notes += 1;
                if !pinged {
                    pinged = true;
                    c.send(&json!({"jsonrpc": "2.0", "id": ping, "method": "ping"}).to_string()).await;
                }
            } else if m["id"] == ping {
                pong = true;
            } else if m["id"] == id {
                break m;
            }
        };
        assert_eq!(done["result"]["isError"], false, "{done}");
        assert!(notes >= 1, "a finished export reports its progress at least once");
        assert!(dir.join(&name).exists());
        if notes >= 2 && pong {
            break;
        }
        // the ping's answer is still on its way: read it so it cannot be taken for a later reply
        while pinged && !pong {
            pong = c.next().await["id"] == ping;
        }
        pong = false;
    }
    assert!(notes >= 2 && pong, "{notes} notifications, ping answered during the export: {pong}");
    // cancel after the first notification: no response, no partial file
    c.send(&export(4, "b.mp4", Some(json!(7)), 20.0)).await;
    loop {
        let m = c.next().await;
        assert_ne!(m["id"], 4, "finished before the cancel: {m}");
        if m["method"] == "notifications/progress" {
            break;
        }
    }
    c.send(r#"{"jsonrpc":"2.0","method":"notifications/cancelled","params":{"requestId":4}}"#).await;
    // The export stops at its next frame, which a slow machine (software rendering in a CI
    // container) can be well into: wait until the job has finished, not for a fixed time.
    let (t0, mut id) = (std::time::Instant::now(), 100u64);
    let last = loop {
        id += 1;
        c.send(&json!({"jsonrpc":"2.0","id":id,"method":"tools/call","params":{"name":"command_run","arguments":{"id":"jobs.list"}}}).to_string()).await;
        let m = loop {
            let m = c.next().await;
            assert_ne!(m["id"], 4, "no response for a cancelled request: {m}");
            if m["id"] == id {
                break m;
            }
        };
        let jobs: Value = serde_json::from_str(m["result"]["content"][0]["text"].as_str().unwrap()).unwrap();
        let last = jobs.as_array().and_then(|a| a.last()).cloned().unwrap_or_default();
        if last["finished"] == true {
            break last;
        }
        assert!(t0.elapsed().as_secs() < 30, "the cancelled export is still running: {jobs}");
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    };
    assert_eq!(last["result"]["error"], "cancelled", "{last}");
    while dir.join("b.mp4").exists() {
        assert!(t0.elapsed().as_secs() < 30, "partial file still there");
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    // and still no response for the cancelled request
    c.send(r#"{"jsonrpc":"2.0","id":5,"method":"ping"}"#).await;
    loop {
        let m = c.next().await;
        assert_ne!(m["id"], 4, "no response for a cancelled request: {m}");
        if m["id"] == 5 {
            break;
        }
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// A command that panicked used to poison the session lock, and every later call failed with
/// "session lock poisoned" until the server was restarted.
#[tokio::test(flavor = "multi_thread")]
async fn a_poisoned_session_lock_keeps_serving() {
    let m = FilmcraftMcp::headless(demo());
    let Backend::Headless(s) = &*m.backend else { panic!("headless backend") };
    let s = s.clone();
    let _ = std::thread::spawn(move || {
        let _g = s.lock().unwrap();
        panic!("a command panicked");
    })
    .join();
    let v = m.run("sequence.inspect", json!({})).await.unwrap();
    assert!(v.is_object(), "{v}");
    let r = m.render_frame(Parameters(RenderParams { seconds: Some(0.5), max_side: Some(64) })).await.unwrap();
    assert_ne!(r.is_error, Some(true));
}

/// `doc_inspect` composes `project.inspect` and `sequence.inspect` as two separate round
/// trips to the app. If the active sequence changes between them (another client, or the
/// app's own UI, switching tabs in that window), an unscoped `sequence.inspect` answers for
/// whichever sequence is active *now*, not the one `project.inspect` just reported, so the
/// pair comes back inconsistent (#1477, reproduced with a real bridge client and a flipping
/// `sequence.open` in `race_test.py`: ~300/300 calls mismatched). Pinning the second call to
/// the id the first one reported keeps `sequence`, when not null, always describing the
/// sequence `project.activeSequence` names.
#[tokio::test]
async fn doc_inspect_pins_the_sequence_to_the_reported_active_id() {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    tokio::spawn(async move {
        let (s, _) = listener.accept().await.unwrap();
        let (r, mut w) = s.into_split();
        let mut lines = tokio::io::BufReader::new(r).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            let req: Value = serde_json::from_str(&line).unwrap();
            let result = match req["params"]["command"].as_str().unwrap_or_default() {
                "project.inspect" => json!({"activeSequence": 21}),
                // The app's own active sequence has since moved on to 54; only an explicit
                // `item` still gets 21's data (an unscoped call would wrongly answer for 54).
                "sequence.inspect" => json!({"id": req["params"]["params"]["item"].as_i64().unwrap_or(54)}),
                _ => Value::Null,
            };
            let line = format!("{}\n", json!({"id": req["id"], "ok": true, "result": result}));
            w.write_all(line.as_bytes()).await.unwrap();
        }
    });
    let m = FilmcraftMcp::bridge(&addr).unwrap();
    let v = m.document().await.unwrap();
    assert_eq!(v["project"]["activeSequence"], json!(21), "{v}");
    assert_eq!(v["sequence"]["id"], json!(21), "sequence must describe the reported active id, not whatever is active now: {v}");
}

/// `activeSequence` is a `u64` (`ItemId`'s representation; `item_p` parses the `item` param
/// with `u64_p`), so an id above `i64::MAX` must still round-trip through `document()`:
/// reading it with `as_i64` would silently return `None` and make `sequence` go null instead
/// of inspecting the (valid) id project.inspect reported.
#[tokio::test]
async fn doc_inspect_handles_an_active_sequence_id_above_i64_max() {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt};
    const HUGE: u64 = i64::MAX as u64 + 1;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    tokio::spawn(async move {
        let (s, _) = listener.accept().await.unwrap();
        let (r, mut w) = s.into_split();
        let mut lines = tokio::io::BufReader::new(r).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            let req: Value = serde_json::from_str(&line).unwrap();
            let result = match req["params"]["command"].as_str().unwrap_or_default() {
                "project.inspect" => json!({"activeSequence": HUGE}),
                // Echoes back whatever `item` it was actually asked for, so the test can
                // check that `document()` asked for `HUGE` rather than nothing at all.
                "sequence.inspect" => json!({"id": req["params"]["params"]["item"]}),
                _ => Value::Null,
            };
            let line = format!("{}\n", json!({"id": req["id"], "ok": true, "result": result}));
            w.write_all(line.as_bytes()).await.unwrap();
        }
    });
    let m = FilmcraftMcp::bridge(&addr).unwrap();
    let v = m.document().await.unwrap();
    assert_eq!(v["project"]["activeSequence"].as_u64(), Some(HUGE), "{v}");
    assert_eq!(v["sequence"]["id"].as_u64(), Some(HUGE), "an id above i64::MAX must still be inspected, not dropped to a null sequence: {v}");
}
