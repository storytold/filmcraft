//! Running a workflow on a ComfyUI server.
//!
//! [`Client::run`] uploads the recipe's input files, queues the workflow (`POST /prompt`), polls
//! `GET /history/<id>` (and `GET /queue` for the position) until it finished, and streams the
//! output files (`GET /view`) into a [`Sink`]. Progress goes to a callback that can stop the run;
//! a stopped run removes the prompt from the queue, or interrupts it when it is the one running.
//!
//! The [`Transport`] is all the networking: blocking calls. The HTTP one is in [`crate::http`];
//! tests and headless sessions use [`crate::fake::FakeComfy`].

use std::io::Write;
use std::sync::Arc;
use std::time::Duration;

use serde_json::{Value, json};

use crate::protocol::{self, FileRef, Finished, Output, State};
use crate::{Binding, CLIENT_ID, ComfyError, Recipe, Result, Workflow};

/// An HTTP response.
#[derive(Clone, Debug, PartialEq)]
pub struct Response {
    pub status: u16,
    pub body: Vec<u8>,
}

impl Response {
    pub fn ok(&self) -> bool {
        (200..300).contains(&self.status)
    }
    fn json(&self, what: &str) -> Result<Value> {
        if !self.ok() {
            let text = String::from_utf8_lossy(self.body.get(..self.body.len().min(300)).unwrap_or_default()).into_owned();
            return Err(ComfyError::Server(format!("{what}: HTTP {}: {text}", self.status)));
        }
        serde_json::from_slice(&self.body).map_err(|e| ComfyError::Server(format!("{what}: not JSON: {e}")))
    }
}

/// The connection to one ComfyUI server. Paths start with `/` and include the query.
pub trait Transport: Send + Sync {
    fn get(&self, path: &str) -> Result<Response>;
    fn post(&self, path: &str, content_type: &str, body: &[u8]) -> Result<Response>;
    /// Stream the body of `GET path` (a file) into `out`; more than `limit` bytes is
    /// [`ComfyError::TooLarge`], a failed write [`ComfyError::Storage`]. Returns the size. The
    /// default reads the whole answer with [`Transport::get`] (in-process servers); the HTTP
    /// transport streams it.
    fn download(&self, path: &str, out: &mut dyn Write, limit: u64) -> Result<u64> {
        let r = self.get(path)?;
        if !r.ok() {
            return Err(ComfyError::Server(format!("{path}: HTTP {}", r.status)));
        }
        let n = r.body.len() as u64;
        if n > limit {
            return Err(too_large(path, limit));
        }
        out.write_all(&r.body).map_err(|e| ComfyError::Storage(e.to_string()))?;
        Ok(n)
    }
    /// Wait between two polls.
    fn pause(&self, d: Duration) {
        std::thread::sleep(d);
    }
}

/// The error for a download over `limit` bytes.
pub(crate) fn too_large(what: &str, limit: u64) -> ComfyError {
    ComfyError::TooLarge(format!("{what}: larger than {} MiB, the most FilmCraft downloads", limit >> 20))
}

/// Where a run's output files go. The engine streams them to disk ([`MemorySink`] keeps them in
/// memory, for tests and hosts without a file system).
pub trait Sink {
    /// A writer for the file of `output` (a wanted media output, in output order).
    fn create(&mut self, output: &Output) -> std::result::Result<Box<dyn Write + '_>, String>;
    /// The download into the last created writer ended: complete with `size` bytes, or `None`
    /// (failed or stopped: drop what was written).
    fn close(&mut self, output: &Output, size: Option<u64>) -> std::result::Result<(), String>;
}

/// A [`Sink`] that keeps the files in memory: (output, bytes), complete files only.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct MemorySink {
    pub files: Vec<(Output, Vec<u8>)>,
    current: Vec<u8>,
}

impl Sink for MemorySink {
    fn create(&mut self, _: &Output) -> std::result::Result<Box<dyn Write + '_>, String> {
        self.current.clear();
        Ok(Box::new(&mut self.current))
    }
    fn close(&mut self, output: &Output, size: Option<u64>) -> std::result::Result<(), String> {
        let bytes = std::mem::take(&mut self.current);
        if size.is_some() {
            self.files.push((output.clone(), bytes));
        }
        Ok(())
    }
}

/// How long to wait, how often to look, and how much to download.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RunOptions {
    pub poll: Duration,
    /// Give up (and cancel the prompt) after this long.
    pub timeout: Duration,
    /// Largest output file (bytes).
    pub max_file: u64,
    /// Most output files downloaded; further outputs are listed but not downloaded.
    pub max_files: usize,
    /// Most bytes downloaded, all files together.
    pub max_total: u64,
    /// Largest input file uploaded (bytes).
    pub max_upload: u64,
}

impl Default for RunOptions {
    fn default() -> Self {
        Self {
            poll: Duration::from_millis(500),
            timeout: Duration::from_secs(3600),
            max_file: crate::MAX_FILE,
            max_files: crate::MAX_FILES,
            max_total: crate::MAX_RUN_BYTES,
            max_upload: crate::MAX_UPLOAD,
        }
    }
}

/// What a run is doing (for the job's status line).
#[derive(Clone, Debug, PartialEq)]
pub enum Progress {
    Uploading { file: String },
    Queued { ahead: usize },
    Running { seconds: u64 },
    Downloading { done: usize, total: usize, file: String },
}

impl std::fmt::Display for Progress {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Progress::Uploading { file } => write!(f, "Uploading {file}"),
            Progress::Queued { ahead: 0 } => write!(f, "Queued"),
            Progress::Queued { ahead } => write!(f, "Queued ({ahead} ahead)"),
            Progress::Running { seconds } => write!(f, "Running ({seconds} s)"),
            Progress::Downloading { done, total, file } => write!(f, "Downloading {file} ({}/{total})", done + 1),
        }
    }
}

/// A downloaded output file (its bytes went to the [`Sink`]).
#[derive(Clone, Debug, PartialEq)]
pub struct Fetched {
    pub output: Output,
    pub size: u64,
}

/// What a finished run produced.
#[derive(Clone, Debug, PartialEq)]
pub struct RunResult {
    pub prompt_id: String,
    /// Every output the server listed (also those not downloaded).
    pub outputs: Vec<Output>,
    /// The downloaded files of the wanted media outputs, in output order.
    pub files: Vec<Fetched>,
}

impl RunResult {
    /// The text outputs.
    pub fn texts(&self) -> Vec<&str> {
        self.outputs.iter().filter_map(|o| o.text.as_deref()).collect()
    }
}

/// A client for one server.
#[derive(Clone)]
pub struct Client {
    t: Arc<dyn Transport>,
}

/// Keep the base name of `path` with only safe characters (upload names).
fn safe_name(path: &str) -> String {
    let base = path.rsplit(['/', '\\']).next().unwrap_or(path);
    let s: String = base.chars().take(80).map(|c| if c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_') { c } else { '_' }).collect();
    if s.trim_matches(['.', '_']).is_empty() { "input".into() } else { s }
}

impl Client {
    pub fn new(t: Arc<dyn Transport>) -> Self {
        Self { t }
    }

    /// `GET /system_stats`: the server's versions and devices (a connection check).
    pub fn system_stats(&self) -> Result<Value> {
        self.t.get("/system_stats")?.json("system_stats")
    }

    /// Upload `bytes` to the server's input folder; returns where it landed. The name carries a
    /// content hash, so the same file is stored once and different files never collide.
    pub fn upload(&self, path: &str, bytes: &[u8]) -> Result<FileRef> {
        let hash = crate::fnv1a(bytes);
        let name = format!("filmcraft_{hash:016x}_{}", safe_name(path));
        let boundary = format!("filmcraft-{:016x}", hash ^ 0x5bd1_e995);
        let mut body = Vec::with_capacity(bytes.len() + 512);
        body.extend_from_slice(
            format!("--{boundary}\r\nContent-Disposition: form-data; name=\"image\"; filename=\"{name}\"\r\nContent-Type: application/octet-stream\r\n\r\n")
                .as_bytes(),
        );
        body.extend_from_slice(bytes);
        body.extend_from_slice(format!("\r\n--{boundary}\r\nContent-Disposition: form-data; name=\"overwrite\"\r\n\r\ntrue\r\n--{boundary}--\r\n").as_bytes());
        let r = self.t.post("/upload/image", &format!("multipart/form-data; boundary={boundary}"), &body)?;
        protocol::parse_uploaded(r.status, &r.body)
    }

    /// Queue `workflow`; returns the prompt id.
    pub fn queue(&self, workflow: &Value) -> Result<String> {
        let body = json!({"prompt": workflow, "client_id": CLIENT_ID}).to_string();
        let r = self.t.post("/prompt", "application/json", body.as_bytes())?;
        protocol::parse_queued(r.status, &r.body)
    }

    /// The finished state of a prompt (None: still queued or running).
    pub fn history(&self, prompt_id: &str) -> Result<Option<Finished>> {
        let v = self.t.get(&format!("/history/{}", protocol::encode(prompt_id)))?.json("history")?;
        Ok(protocol::parse_history(&v, prompt_id))
    }

    /// Where a prompt is in the queue.
    pub fn state(&self, prompt_id: &str) -> Result<State> {
        let v = self.t.get("/queue")?.json("queue")?;
        Ok(protocol::parse_queue(&v, prompt_id))
    }

    /// Stream an output file into `out` (at most `limit` bytes); its size.
    pub fn download(&self, f: &FileRef, out: &mut dyn Write, limit: u64) -> Result<u64> {
        self.t.download(&f.view_path(), out, limit).map_err(|e| match e {
            ComfyError::TooLarge(_) => too_large(&f.filename, limit),
            ComfyError::Server(m) => ComfyError::Server(format!("{}: {m}", f.filename)),
            e => e,
        })
    }

    /// Take a prompt off the queue, or interrupt it when it is running (best effort).
    pub fn cancel(&self, prompt_id: &str) {
        if matches!(self.state(prompt_id), Ok(State::Running)) {
            let _ = self.t.post("/interrupt", "application/json", json!({"prompt_id": prompt_id}).to_string().as_bytes());
        }
        let _ = self.t.post("/queue", "application/json", json!({"delete": [prompt_id]}).to_string().as_bytes());
    }

    /// The workflow to queue for `recipe`: its bindings checked and applied, files uploaded
    /// (`read` reads a local file; files over `max_upload` bytes are refused).
    pub fn prepare(
        &self,
        recipe: &Recipe,
        read: &mut dyn FnMut(&str) -> std::result::Result<Vec<u8>, String>,
        max_upload: u64,
        progress: &mut dyn FnMut(&Progress) -> bool,
    ) -> Result<Value> {
        let wf = Workflow::parse(&recipe.workflow)?;
        let mut values = Vec::new();
        for b in &recipe.inputs {
            wf.check_binding(b)?;
            let Binding { node, input, value, file } = b;
            let v = match (file, value) {
                (Some(path), _) => {
                    if !progress(&Progress::Uploading { file: safe_name(path) }) {
                        return Err(ComfyError::Cancelled);
                    }
                    let bytes = read(path).map_err(|e| ComfyError::Workflow(format!("node {node} `{input}`: {path}: {e}")))?;
                    if bytes.len() as u64 > max_upload {
                        return Err(ComfyError::TooLarge(format!(
                            "node {node} `{input}`: {path}: larger than {} MiB, the most FilmCraft uploads",
                            max_upload >> 20
                        )));
                    }
                    Value::String(self.upload(path, &bytes)?.input_value())
                }
                (None, Some(v)) => v.clone(),
                (None, None) => continue,
            };
            values.push((node.clone(), input.clone(), v));
        }
        wf.apply(&values)
    }

    /// Run `recipe`: prepare, queue, wait and stream the wanted media outputs into `sink`
    /// (at most `opts.max_files` files of `opts.max_file` bytes, `opts.max_total` in all). A file
    /// over a limit fails the run; files already in `sink` are the caller's to drop.
    pub fn run(
        &self,
        recipe: &Recipe,
        read: &mut dyn FnMut(&str) -> std::result::Result<Vec<u8>, String>,
        opts: &RunOptions,
        progress: &mut dyn FnMut(&Progress) -> bool,
        sink: &mut dyn Sink,
    ) -> Result<RunResult> {
        let workflow = self.prepare(recipe, read, opts.max_upload, progress)?;
        let prompt_id = self.queue(&workflow)?;
        let outputs = self.wait(&prompt_id, opts, progress)?;
        let wanted: Vec<&Output> = outputs.iter().filter(|o| o.kind.is_media() && o.file.is_some() && recipe.wants(o)).take(opts.max_files).collect();
        let mut files = Vec::with_capacity(wanted.len());
        let mut total = 0u64;
        for (k, o) in wanted.iter().enumerate() {
            let Some(f) = &o.file else { continue };
            if !progress(&Progress::Downloading { done: k, total: wanted.len(), file: f.filename.clone() }) {
                return Err(ComfyError::Cancelled);
            }
            let left = opts.max_total.saturating_sub(total);
            let limit = opts.max_file.min(left);
            let mut w = sink.create(o).map_err(ComfyError::Storage)?;
            let r = self.download(f, &mut *w, limit).and_then(|n| w.flush().map(|_| n).map_err(|e| ComfyError::Storage(e.to_string())));
            drop(w);
            sink.close(o, r.as_ref().ok().copied()).map_err(ComfyError::Storage)?;
            let n = match r {
                Ok(n) => n,
                // a stop shows as a failed write: say it was a stop
                Err(_) if !progress(&Progress::Downloading { done: k, total: wanted.len(), file: f.filename.clone() }) => return Err(ComfyError::Cancelled),
                Err(ComfyError::TooLarge(_)) if limit < opts.max_file => {
                    return Err(ComfyError::TooLarge(format!("{}: the run's files are larger than {} MiB in all", f.filename, opts.max_total >> 20)));
                }
                Err(e) => return Err(e),
            };
            total = total.saturating_add(n);
            files.push(Fetched { output: (*o).clone(), size: n });
        }
        Ok(RunResult { prompt_id, outputs, files })
    }

    /// Poll until `prompt_id` finished; its outputs.
    pub fn wait(&self, prompt_id: &str, opts: &RunOptions, progress: &mut dyn FnMut(&Progress) -> bool) -> Result<Vec<Output>> {
        let poll = opts.poll.max(Duration::from_millis(10));
        let mut waited = Duration::ZERO;
        let mut running = Duration::ZERO;
        loop {
            match self.history(prompt_id)? {
                Some(Finished::Success(o)) => return Ok(o),
                Some(Finished::Error(e)) => return Err(ComfyError::Execution(e)),
                None => {}
            }
            let p = match self.state(prompt_id)? {
                State::Pending { ahead } => Progress::Queued { ahead },
                State::Running | State::Unknown => {
                    running += poll;
                    Progress::Running { seconds: running.as_secs() }
                }
            };
            if !progress(&p) {
                self.cancel(prompt_id);
                return Err(ComfyError::Cancelled);
            }
            if waited >= opts.timeout {
                self.cancel(prompt_id);
                return Err(ComfyError::Timeout(opts.timeout.as_secs()));
            }
            self.t.pause(poll);
            waited += poll;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn upload_names_are_safe() {
        assert_eq!(safe_name("/home/me/My Shot (1).png"), "My_Shot__1_.png");
        assert_eq!(safe_name("C:\\clips\\a.wav"), "a.wav");
        assert_eq!(safe_name("../.."), "input");
        assert_eq!(safe_name(""), "input");
    }

    #[test]
    fn progress_lines() {
        assert_eq!(Progress::Queued { ahead: 2 }.to_string(), "Queued (2 ahead)");
        assert_eq!(Progress::Downloading { done: 0, total: 2, file: "a.mp4".into() }.to_string(), "Downloading a.mp4 (1/2)");
    }
}
