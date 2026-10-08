//! The HTTP transport against a minimal ComfyUI look-alike on a local socket: upload, queue,
//! poll, download, and a refused workflow's 400 with its node errors.

#![cfg(feature = "http")]

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::sync::{Arc, Mutex};

use filmcraft_comfyui::http::HttpTransport;
use filmcraft_comfyui::{Binding, Client, ComfyError, OutputKind, Recipe, RunOptions};
use serde_json::{Value, json};

#[derive(Default)]
struct Seen {
    uploads: Vec<Vec<u8>>,
    prompts: Vec<Value>,
}

/// One request: (method, path, body).
fn read_request(stream: &mut std::net::TcpStream) -> Option<(String, String, Vec<u8>)> {
    let mut r = BufReader::new(stream.try_clone().ok()?);
    let mut line = String::new();
    r.read_line(&mut line).ok()?;
    let mut parts = line.split_whitespace();
    let (method, path) = (parts.next()?.to_string(), parts.next()?.to_string());
    let mut len = 0usize;
    loop {
        let mut h = String::new();
        r.read_line(&mut h).ok()?;
        if h.trim().is_empty() {
            break;
        }
        if let Some((k, v)) = h.split_once(':')
            && k.eq_ignore_ascii_case("content-length")
        {
            len = v.trim().parse().ok()?;
        }
    }
    let mut body = vec![0; len];
    r.read_exact(&mut body).ok()?;
    Some((method, path, body))
}

fn respond(stream: &mut std::net::TcpStream, status: u16, body: &[u8]) {
    let head = format!("HTTP/1.1 {status} X\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len());
    let _ = stream.write_all(head.as_bytes());
    let _ = stream.write_all(body);
}

/// Serve a fake ComfyUI on 127.0.0.1; returns its base URL.
fn serve(seen: Arc<Mutex<Seen>>, refuse: bool) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut s) = stream else { continue };
            let Some((method, path, body)) = read_request(&mut s) else { continue };
            let mut g = seen.lock().unwrap();
            match (method.as_str(), path.split('?').next().unwrap()) {
                ("POST", "/upload/image") => {
                    g.uploads.push(body);
                    respond(&mut s, 200, br#"{"name":"up.png","subfolder":"","type":"input"}"#);
                }
                ("POST", "/prompt") if refuse => {
                    let e = json!({"error": {"message": "Prompt outputs failed validation", "details": ""},
                        "node_errors": {"4": {"class_type": "CheckpointLoaderSimple", "errors": [{"message": "Value not in list", "details": "ckpt_name"}]}}});
                    respond(&mut s, 400, e.to_string().as_bytes());
                }
                ("POST", "/prompt") => {
                    let v: Value = serde_json::from_slice(&body).unwrap();
                    g.prompts.push(v["prompt"].clone());
                    respond(&mut s, 200, br#"{"prompt_id":"p1","number":1,"node_errors":{}}"#);
                }
                ("GET", "/queue") => respond(&mut s, 200, br#"{"queue_running":[],"queue_pending":[]}"#),
                ("GET", "/history/p1") => {
                    let h = json!({"p1": {"outputs": {"9": {"images": [{"filename": "out 1.png", "subfolder": "", "type": "output"}]}}, "status": {"status_str": "success", "completed": true}}});
                    respond(&mut s, 200, h.to_string().as_bytes());
                }
                ("GET", "/view") if path.contains("filename=out%201.png") => respond(&mut s, 200, b"PNGDATA"),
                ("GET", "/system_stats") => respond(&mut s, 200, br#"{"system":{"comfyui_version":"0.3.0"}}"#),
                _ => respond(&mut s, 404, b"not found"),
            }
        }
    });
    base
}

fn recipe() -> Recipe {
    let mut r = Recipe::new(json!({
        "4": {"class_type": "CheckpointLoaderSimple", "inputs": {"ckpt_name": "x"}},
        "6": {"class_type": "LoadImage", "inputs": {"image": "example.png"}},
        "9": {"class_type": "SaveImage", "inputs": {"images": ["6", 0]}}
    }))
    .unwrap();
    r.bind([Binding::file("6", "image", "/clips/frame.png")]);
    r
}

#[test]
fn runs_a_workflow_over_http() {
    let seen = Arc::new(Mutex::new(Seen::default()));
    let base = serve(seen.clone(), false);
    let client = Client::new(Arc::new(HttpTransport::new(&base).unwrap()));
    assert_eq!(client.system_stats().unwrap()["system"]["comfyui_version"], "0.3.0");
    let r = client.run(&recipe(), &mut |_| Ok(b"FRAME".to_vec()), &RunOptions::default(), &mut |_| true).unwrap();
    assert_eq!(r.files.len(), 1);
    assert_eq!(r.files[0].output.kind, OutputKind::Image);
    assert_eq!(r.files[0].bytes, b"PNGDATA");
    let g = seen.lock().unwrap();
    assert!(g.uploads[0].windows(5).any(|w| w == b"FRAME"), "multipart body carries the file");
    assert_eq!(g.prompts[0]["6"]["inputs"]["image"], "up.png");
}

#[test]
fn a_refused_workflow_explains_why() {
    let base = serve(Arc::default(), true);
    let client = Client::new(Arc::new(HttpTransport::new(&base).unwrap()));
    let e = client.run(&recipe(), &mut |_| Ok(Vec::new()), &RunOptions::default(), &mut |_| true).unwrap_err();
    match e {
        ComfyError::Rejected(m) => assert!(m.contains("node 4 (CheckpointLoaderSimple): Value not in list"), "{m}"),
        e => panic!("{e:?}"),
    }
}
