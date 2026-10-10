//! An in-process ComfyUI stand-in ([`FakeComfy`]) for tests and headless sessions.
//!
//! It speaks the same protocol as a real server through the [`Transport`] trait: `/prompt`
//! queues the workflow and asks a responder what the run produces, `/history` reports it after a
//! configurable number of polls, `/view` serves the files, `/upload/image` stores input files.
//! Everything it was asked is recorded, so tests can check what was queued and uploaded.

use std::collections::BTreeMap;
use std::sync::Mutex;
use std::time::Duration;

use serde_json::{Value, json};

use crate::client::{Response, Transport};
use crate::{ComfyError, Result};

/// One output of a fake run.
#[derive(Clone, Debug, PartialEq)]
pub enum FakeOutput {
    /// A file reported under `key` (`images`, `gifs`, `audio`…) by `node`.
    File { node: String, key: String, name: String, bytes: Vec<u8> },
    /// A text reported under `text` by `node`.
    Text { node: String, text: String },
}

impl FakeOutput {
    pub fn file(node: &str, key: &str, name: &str, bytes: Vec<u8>) -> Self {
        Self::File { node: node.into(), key: key.into(), name: name.into(), bytes }
    }
    pub fn text(node: &str, text: &str) -> Self {
        Self::Text { node: node.into(), text: text.into() }
    }
}

/// What a run produces for a queued workflow (`Err`: the run fails with that message).
pub type Responder = Box<dyn Fn(&Value) -> std::result::Result<Vec<FakeOutput>, String> + Send + Sync>;

#[derive(Default)]
struct Inner {
    next: u64,
    /// prompt id -> (workflow, history polls left before it finishes, finished entry)
    prompts: BTreeMap<String, (Value, u32, Value)>,
    /// (folder, subfolder, name) -> bytes
    files: BTreeMap<(String, String, String), Vec<u8>>,
    uploads: Vec<(String, Vec<u8>)>,
    cancelled: Vec<String>,
}

/// A fake ComfyUI server.
pub struct FakeComfy {
    respond: Responder,
    /// History polls that report "not finished" before a prompt completes.
    pub polls: u32,
    inner: Mutex<Inner>,
}

fn lock(m: &Mutex<Inner>) -> std::sync::MutexGuard<'_, Inner> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

fn json_response(status: u16, v: Value) -> Result<Response> {
    Ok(Response { status, body: v.to_string().into_bytes() })
}

/// Decode a percent-encoded query value.
fn decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while let Some(&c) = b.get(i) {
        let hex = b.get(i + 1..i + 3).and_then(|h| std::str::from_utf8(h).ok()).and_then(|h| u8::from_str_radix(h, 16).ok());
        match (c, hex) {
            (b'%', Some(v)) => {
                out.push(v);
                i += 3;
            }
            (b'+', _) => {
                out.push(b' ');
                i += 1;
            }
            _ => {
                out.push(c);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn query(path: &str) -> BTreeMap<String, String> {
    path.split_once('?').map(|(_, q)| q).unwrap_or_default().split('&').filter_map(|kv| kv.split_once('=')).map(|(k, v)| (decode(k), decode(v))).collect()
}

/// The uploaded file of a multipart body: (file name, bytes).
fn multipart_file(content_type: &str, body: &[u8]) -> Option<(String, Vec<u8>)> {
    let boundary = content_type.split("boundary=").nth(1)?.trim();
    let head_start = find(body, b"filename=\"")? + 10;
    let name_end = head_start + find(body.get(head_start..)?, b"\"")?;
    let name = String::from_utf8_lossy(body.get(head_start..name_end)?).into_owned();
    let data_start = name_end + find(body.get(name_end..)?, b"\r\n\r\n")? + 4;
    let end_marker = format!("\r\n--{boundary}");
    let data_end = data_start + find(body.get(data_start..)?, end_marker.as_bytes())?;
    Some((name, body.get(data_start..data_end)?.to_vec()))
}

fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len().max(1)).position(|w| w == needle)
}

impl FakeComfy {
    pub fn new(respond: Responder) -> Self {
        Self { respond, polls: 1, inner: Mutex::default() }
    }

    /// A server whose every run produces `outputs`.
    pub fn with_outputs(outputs: Vec<FakeOutput>) -> Self {
        Self::new(Box::new(move |_| Ok(outputs.clone())))
    }

    /// The workflows queued so far, in order.
    pub fn queued(&self) -> Vec<Value> {
        let g = lock(&self.inner);
        let mut v: Vec<(u64, Value)> = g.prompts.iter().map(|(id, p)| (id.trim_start_matches("fake-").parse().unwrap_or(0), p.0.clone())).collect();
        v.sort_by_key(|x| x.0);
        v.into_iter().map(|x| x.1).collect()
    }

    /// Files uploaded so far: (stored name, bytes).
    pub fn uploads(&self) -> Vec<(String, Vec<u8>)> {
        lock(&self.inner).uploads.clone()
    }

    /// Prompts that were cancelled.
    pub fn cancelled(&self) -> Vec<String> {
        lock(&self.inner).cancelled.clone()
    }

    fn queue_prompt(&self, body: &[u8]) -> Result<Response> {
        let v: Value = serde_json::from_slice(body).map_err(|e| ComfyError::Server(e.to_string()))?;
        let Some(wf) = v.get("prompt").filter(|p| p.is_object()).cloned() else {
            return json_response(400, json!({"error": {"type": "invalid_prompt", "message": "no prompt", "details": ""}, "node_errors": {}}));
        };
        let produced = (self.respond)(&wf);
        let mut g = lock(&self.inner);
        g.next += 1;
        let id = format!("fake-{}", g.next);
        let entry = match produced {
            Ok(outs) => {
                let mut outputs = serde_json::Map::new();
                for o in outs {
                    let (node, key, item) = match o {
                        FakeOutput::File { node, key, name, bytes } => {
                            g.files.insert(("output".into(), String::new(), name.clone()), bytes);
                            (node, key, json!({"filename": name, "subfolder": "", "type": "output"}))
                        }
                        FakeOutput::Text { node, text } => (node, "text".to_string(), json!(text)),
                    };
                    let slot = outputs.entry(node).or_insert_with(|| json!({}));
                    if let Some(m) = slot.as_object_mut() {
                        let list = m.entry(key).or_insert_with(|| json!([]));
                        if let Some(a) = list.as_array_mut() {
                            a.push(item);
                        }
                    }
                }
                json!({"outputs": outputs, "status": {"status_str": "success", "completed": true, "messages": []}})
            }
            Err(e) => json!({"outputs": {}, "status": {"status_str": "error", "completed": false, "messages": [
                ["execution_error", {"node_id": "1", "node_type": "Fake", "exception_message": e}]
            ]}}),
        };
        g.prompts.insert(id.clone(), (wf, self.polls, entry));
        json_response(200, json!({"prompt_id": id, "number": g.next, "node_errors": {}}))
    }
}

impl Transport for FakeComfy {
    fn get(&self, path: &str) -> Result<Response> {
        let route = path.split('?').next().unwrap_or(path);
        let mut g = lock(&self.inner);
        if route == "/system_stats" {
            return json_response(200, json!({"system": {"os": "fake", "comfyui_version": "fake"}, "devices": []}));
        }
        if route == "/queue" {
            let running: Vec<Value> = g.prompts.iter().filter(|(_, p)| p.1 > 0).map(|(id, _)| json!([0, id, {}])).collect();
            return json_response(200, json!({"queue_running": running, "queue_pending": []}));
        }
        if let Some(id) = route.strip_prefix("/history/") {
            let id = decode(id);
            let Some(p) = g.prompts.get_mut(&id) else { return json_response(200, json!({})) };
            if p.1 > 0 {
                p.1 -= 1;
                return json_response(200, json!({}));
            }
            return json_response(200, json!({ id: p.2.clone() }));
        }
        if route == "/view" {
            let q = query(path);
            let key =
                (q.get("type").cloned().unwrap_or_default(), q.get("subfolder").cloned().unwrap_or_default(), q.get("filename").cloned().unwrap_or_default());
            return Ok(match g.files.get(&key) {
                Some(b) => Response { status: 200, body: b.clone() },
                None => Response { status: 404, body: b"not found".to_vec() },
            });
        }
        Ok(Response { status: 404, body: b"not found".to_vec() })
    }

    fn post(&self, path: &str, content_type: &str, body: &[u8]) -> Result<Response> {
        match path {
            "/prompt" => self.queue_prompt(body),
            "/upload/image" => {
                let Some((name, bytes)) = multipart_file(content_type, body) else { return json_response(400, json!({"error": "bad multipart"})) };
                let mut g = lock(&self.inner);
                g.files.insert(("input".into(), String::new(), name.clone()), bytes.clone());
                g.uploads.push((name.clone(), bytes));
                json_response(200, json!({"name": name, "subfolder": "", "type": "input"}))
            }
            "/interrupt" | "/queue" => {
                let v: Value = serde_json::from_slice(body).unwrap_or_default();
                let ids: Vec<String> = match v.get("delete").and_then(Value::as_array) {
                    Some(a) => a.iter().filter_map(Value::as_str).map(str::to_string).collect(),
                    None => v.get("prompt_id").and_then(Value::as_str).map(str::to_string).into_iter().collect(),
                };
                let mut g = lock(&self.inner);
                for id in ids {
                    if !g.cancelled.contains(&id) {
                        g.cancelled.push(id.clone());
                    }
                    g.prompts.remove(&id);
                }
                json_response(200, json!({}))
            }
            _ => Ok(Response { status: 404, body: b"not found".to_vec() }),
        }
    }

    fn pause(&self, _: Duration) {}
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::client::{MemorySink, Progress, RunOptions, Sink};
    use crate::{Binding, Client, Output, OutputKind, Recipe};

    fn recipe() -> Recipe {
        let mut r = Recipe::new(json!({
            "1": {"class_type": "LoadImage", "inputs": {"image": "x.png"}},
            "6": {"class_type": "CLIPTextEncode", "inputs": {"text": "a cat"}},
            "9": {"class_type": "SaveImage", "inputs": {"images": ["1", 0]}}
        }))
        .unwrap();
        r.bind([Binding::value("6", "text", json!("a dog")), Binding::file("1", "image", "/clips/first frame.png")]);
        r
    }

    fn read(path: &str) -> std::result::Result<Vec<u8>, String> {
        Ok(format!("bytes of {path}").into_bytes())
    }

    #[test]
    fn full_run_uploads_queues_and_downloads() {
        let fake = Arc::new(FakeComfy::new(Box::new(|wf| {
            let prompt = wf["6"]["inputs"]["text"].as_str().unwrap_or_default().to_string();
            Ok(vec![
                FakeOutput::file("9", "images", "out_00001_.png", vec![1, 2, 3]),
                FakeOutput::file("12", "audio", "voice.wav", vec![4, 5]),
                FakeOutput::text("30", &format!("next: {prompt}")),
                FakeOutput::file("40", "latents", "x.latent", vec![9]),
            ])
        })));
        let client = Client::new(fake.clone());
        let mut lines = Vec::new();
        let mut sink = MemorySink::default();
        let r = client.run(
            &recipe(),
            &mut read,
            &RunOptions::default(),
            &mut |p| {
                lines.push(p.to_string());
                true
            },
            &mut sink,
        );
        let r = r.unwrap();
        let queued = fake.queued();
        assert_eq!(queued.len(), 1);
        assert_eq!(queued[0]["6"]["inputs"]["text"], "a dog");
        let uploaded = queued[0]["1"]["inputs"]["image"].as_str().unwrap().to_string();
        assert!(uploaded.starts_with("filmcraft_") && uploaded.ends_with("_first_frame.png"), "{uploaded}");
        assert_eq!(fake.uploads(), vec![(uploaded, b"bytes of /clips/first frame.png".to_vec())]);
        assert_eq!(
            sink.files.iter().map(|(o, b)| (o.kind, b.clone())).collect::<Vec<_>>(),
            [(OutputKind::Image, vec![1, 2, 3]), (OutputKind::Audio, vec![4, 5])]
        );
        assert_eq!(r.files.iter().map(|f| f.size).collect::<Vec<_>>(), [3, 2]);
        assert_eq!(r.texts(), ["next: a dog"]);
        assert_eq!(r.outputs.len(), 4);
        assert!(lines.iter().any(|l| l.starts_with("Uploading first_frame.png")), "{lines:?}");
        assert!(lines.iter().any(|l| l.starts_with("Running")), "{lines:?}");
    }

    #[test]
    fn output_selection() {
        let fake =
            Arc::new(FakeComfy::with_outputs(vec![FakeOutput::file("9", "images", "a.png", vec![1]), FakeOutput::file("12", "audio", "b.wav", vec![2])]));
        let mut r = recipe();
        r.outputs = vec!["12".into()];
        let res = Client::new(fake).run(&r, &mut read, &RunOptions::default(), &mut |_| true, &mut MemorySink::default()).unwrap();
        assert_eq!(res.files.len(), 1);
        assert_eq!(res.files[0].output.node, "12");
    }

    #[test]
    fn execution_errors_and_missing_files() {
        let fake = Arc::new(FakeComfy::new(Box::new(|_| Err("CUDA out of memory".into()))));
        let e = Client::new(fake).run(&recipe(), &mut read, &RunOptions::default(), &mut |_| true, &mut MemorySink::default()).unwrap_err();
        assert_eq!(e, ComfyError::Execution("node 1 (Fake): CUDA out of memory".into()));
        let fake = Arc::new(FakeComfy::with_outputs(vec![]));
        let e = Client::new(fake)
            .run(&recipe(), &mut |_| Err("No such file".into()), &RunOptions::default(), &mut |_| true, &mut MemorySink::default())
            .unwrap_err();
        assert!(matches!(e, ComfyError::Workflow(ref m) if m.contains("No such file")), "{e}");
    }

    #[test]
    fn stopping_cancels_the_prompt() {
        let mut fake = FakeComfy::with_outputs(vec![FakeOutput::file("9", "images", "a.png", vec![1])]);
        fake.polls = 100;
        let fake = Arc::new(fake);
        let e = Client::new(fake.clone())
            .run(&recipe(), &mut read, &RunOptions::default(), &mut |p| !matches!(p, Progress::Running { .. }), &mut MemorySink::default())
            .unwrap_err();
        assert_eq!(e, ComfyError::Cancelled);
        assert_eq!(fake.cancelled(), ["fake-1"]);
    }

    #[test]
    fn times_out() {
        let mut fake = FakeComfy::with_outputs(vec![]);
        fake.polls = 1000;
        let fake = Arc::new(fake);
        let opts = RunOptions { poll: Duration::from_millis(500), timeout: Duration::from_secs(2), ..Default::default() };
        let e = Client::new(fake.clone()).run(&recipe(), &mut read, &opts, &mut |_| true, &mut MemorySink::default()).unwrap_err();
        assert_eq!(e, ComfyError::Timeout(2));
        assert_eq!(fake.cancelled().len(), 1);
    }

    #[test]
    fn hostile_server_answers_are_errors() {
        struct Junk;
        impl Transport for Junk {
            fn get(&self, _: &str) -> Result<Response> {
                Ok(Response { status: 200, body: b"\xff\xfe not json".to_vec() })
            }
            fn post(&self, _: &str, _: &str, _: &[u8]) -> Result<Response> {
                Ok(Response { status: 200, body: br#"{"prompt_id": 7}"#.to_vec() })
            }
            fn pause(&self, _: Duration) {}
        }
        let r = Recipe::new(json!({"1": {"class_type": "X", "inputs": {}}})).unwrap();
        let e = Client::new(Arc::new(Junk)).run(&r, &mut read, &RunOptions::default(), &mut |_| true, &mut MemorySink::default()).unwrap_err();
        assert!(matches!(e, ComfyError::Rejected(_)), "{e}");
        assert!(Client::new(Arc::new(Junk)).system_stats().is_err());
    }

    fn three_files() -> Arc<FakeComfy> {
        Arc::new(FakeComfy::with_outputs(vec![
            FakeOutput::file("9", "images", "a.png", vec![1; 10]),
            FakeOutput::file("10", "images", "b.png", vec![2; 10]),
            FakeOutput::file("11", "images", "c.png", vec![3; 10]),
        ]))
    }

    #[test]
    fn downloads_are_capped() {
        let run = |opts: RunOptions, sink: &mut MemorySink| Client::new(three_files()).run(&recipe(), &mut read, &opts, &mut |_| true, sink);
        // a file over the limit fails the run, and nothing of it is kept
        let mut sink = MemorySink::default();
        let e = run(RunOptions { max_file: 9, ..Default::default() }, &mut sink).unwrap_err();
        assert!(matches!(e, ComfyError::TooLarge(ref m) if m.contains("a.png")), "{e}");
        assert!(sink.files.is_empty());
        // only the first `max_files` outputs are downloaded; all are listed
        let mut sink = MemorySink::default();
        let r = run(RunOptions { max_files: 2, ..Default::default() }, &mut sink).unwrap();
        assert_eq!(sink.files.iter().map(|f| f.0.node.as_str()).collect::<Vec<_>>(), ["9", "10"]);
        assert_eq!(r.outputs.len(), 3);
        // the run's total: the third file no longer fits
        let mut sink = MemorySink::default();
        let e = run(RunOptions { max_total: 25, ..Default::default() }, &mut sink).unwrap_err();
        assert!(matches!(e, ComfyError::TooLarge(ref m) if m.contains("in all")), "{e}");
        assert_eq!(sink.files.len(), 2);
    }

    #[test]
    fn a_failed_write_drops_the_file_and_a_stop_while_downloading_is_a_stop() {
        /// Fails every write; counts what it was asked to close.
        #[derive(Default)]
        struct Full {
            closed: Vec<Option<u64>>,
        }
        impl Sink for Full {
            fn create(&mut self, _: &Output) -> std::result::Result<Box<dyn std::io::Write + '_>, String> {
                Ok(Box::new(FullWriter))
            }
            fn close(&mut self, _: &Output, size: Option<u64>) -> std::result::Result<(), String> {
                self.closed.push(size);
                Ok(())
            }
        }
        struct FullWriter;
        impl std::io::Write for FullWriter {
            fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
                Err(std::io::Error::other("disk full"))
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        let mut sink = Full::default();
        let e = Client::new(three_files()).run(&recipe(), &mut read, &RunOptions::default(), &mut |_| true, &mut sink).unwrap_err();
        assert!(matches!(e, ComfyError::Storage(ref m) if m.contains("disk full")), "{e}");
        assert_eq!(sink.closed, [None]);
        // the same failure after the user pressed Stop is a stop
        let mut downloads = 0;
        let mut sink = Full::default();
        let e = Client::new(three_files())
            .run(
                &recipe(),
                &mut read,
                &RunOptions::default(),
                &mut |p| {
                    downloads += usize::from(matches!(p, Progress::Downloading { .. }));
                    downloads < 2
                },
                &mut sink,
            )
            .unwrap_err();
        assert_eq!(e, ComfyError::Cancelled);
        assert_eq!(sink.closed, [None]);
    }

    #[test]
    fn huge_uploads_are_refused() {
        let fake = Arc::new(FakeComfy::with_outputs(vec![]));
        let opts = RunOptions { max_upload: 8, ..Default::default() };
        let e = Client::new(fake.clone()).run(&recipe(), &mut read, &opts, &mut |_| true, &mut MemorySink::default()).unwrap_err();
        assert!(matches!(e, ComfyError::TooLarge(ref m) if m.contains("first frame.png")), "{e}");
        assert!(fake.uploads().is_empty() && fake.queued().is_empty());
    }

    #[test]
    fn multipart_and_query_helpers() {
        assert_eq!(decode("a%20b%2Fc+d%zz"), "a b/c d%zz");
        let q = query("/view?filename=a%20b.png&type=output&subfolder=");
        assert_eq!(q["filename"], "a b.png");
        assert_eq!(
            multipart_file("multipart/form-data; boundary=XX", b"--XX\r\nContent-Disposition: form-data; filename=\"f.png\"\r\n\r\nDATA\r\n--XX--"),
            Some(("f.png".into(), b"DATA".to_vec()))
        );
        assert_eq!(multipart_file("text/plain", b"x"), None);
    }
}
