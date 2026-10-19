//! The ComfyUI bridge: run any ComfyUI workflow and bring its outputs (video, audio, images,
//! text) back as media.
//!
//! - [`workflow`]: workflows in ComfyUI's **API format** (Workflow ▸ Export (API) in ComfyUI):
//!   validation, the list of editable inputs, and [`Binding`]s that override inputs before a run.
//! - [`protocol`]: the server's JSON (`/prompt`, `/history`, `/queue`) and the [`Output`]s a
//!   finished prompt lists, classified by file extension into [`OutputKind`]s.
//! - [`client`]: the [`Transport`] trait (two blocking calls: GET and POST) and the [`Client`]
//!   that queues a workflow, waits for it, uploads input files and downloads the outputs.
//! - [`http`] (feature `http`): the HTTP(S) transport (ureq, pure-Rust TLS).
//! - [`fake`]: an in-process ComfyUI stand-in for tests and headless sessions.
//!
//! A [`Recipe`] is what a ComfyUI clip stores in the project: the workflow as exported, the input
//! overrides and the output nodes. Running the same recipe again regenerates the clip. A recipe
//! never names a server: a project file can be shared, so the server is the user's setting.
//!
//! Nothing here touches the file system: input files arrive as bytes and output files are
//! streamed into a [`Sink`] (the engine writes them to disk). Everything a server sends is
//! capped ([`MAX_FILE`], [`MAX_FILES`], [`MAX_RUN_BYTES`], [`MAX_OUTPUTS`], [`MAX_TEXT`]). See
//! `docs/comfyui.md` for the user-facing behaviour.

#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable))]

pub mod client;
pub mod fake;
#[cfg(feature = "http")]
pub mod http;
pub mod protocol;
pub mod workflow;

use serde::{Deserialize, Serialize};
use serde_json::Value;

pub use client::{Client, Fetched, MemorySink, Progress, Response, RunOptions, RunResult, Sink, Transport};
pub use protocol::{FileRef, Output, OutputKind};
pub use workflow::{InputInfo, InputKind, NodeInfo, Workflow};

/// The server a new clip talks to when nothing else is set (ComfyUI's default listen address).
pub const DEFAULT_SERVER: &str = "http://127.0.0.1:8188";

/// The `client_id` FilmCraft queues prompts under.
pub const CLIENT_ID: &str = "filmcraft";

/// Largest output file downloaded (streamed to disk, never held in memory whole).
pub const MAX_FILE: u64 = 2 << 30;
/// Most output files downloaded per run (further outputs are listed, not downloaded).
pub const MAX_FILES: usize = 64;
/// Most bytes downloaded per run, all files together.
pub const MAX_RUN_BYTES: u64 = 8 << 30;
/// Most outputs read from a history entry.
pub const MAX_OUTPUTS: usize = 1024;
/// Longest text output kept (bytes; longer texts are cut at a character boundary).
pub const MAX_TEXT: usize = 64 << 10;
/// Largest input file uploaded (it is read into memory to be sent).
pub const MAX_UPLOAD: u64 = 1 << 30;

#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum ComfyError {
    #[error("stopped")]
    Cancelled,
    #[error("ComfyUI is not available in this build ({0})")]
    Unavailable(String),
    #[error("workflow: {0}")]
    Workflow(String),
    #[error("ComfyUI refused the workflow: {0}")]
    Rejected(String),
    #[error("ComfyUI failed to run the workflow: {0}")]
    Execution(String),
    #[error("timed out after {0} s waiting for ComfyUI")]
    Timeout(u64),
    #[error("ComfyUI server: {0}")]
    Server(String),
    #[error("connection: {0}")]
    Connection(String),
    /// Something the server sent is over a limit (a file, the run's total).
    #[error("{0}")]
    TooLarge(String),
    /// Saving an output file failed.
    #[error("saving {0}")]
    Storage(String),
}

pub type Result<T> = std::result::Result<T, ComfyError>;

/// One input override: `node`'s `input` takes `value`, or the server-side name of `file` once it
/// has been uploaded (Load Image / Load Audio / Load Video inputs).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Binding {
    /// Node id in the API workflow (`"6"`).
    pub node: String,
    /// Input name on that node (`"text"`, `"seed"`, `"image"`).
    pub input: String,
    /// A literal value (string, number, bool).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<Value>,
    /// A local file uploaded to the server's input folder before the run.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file: Option<String>,
}

impl Binding {
    pub fn value(node: impl Into<String>, input: impl Into<String>, value: Value) -> Self {
        Self { node: node.into(), input: input.into(), value: Some(value), file: None }
    }
    pub fn file(node: impl Into<String>, input: impl Into<String>, path: impl Into<String>) -> Self {
        Self { node: node.into(), input: input.into(), value: None, file: Some(path.into()) }
    }
    /// Whether this binding targets the same input as `other`.
    pub fn same_input(&self, other: &Binding) -> bool {
        self.node == other.node && self.input == other.input
    }
}

/// How to make a ComfyUI clip (stored with the clip's project item).
///
/// There is no server here on purpose: a project file can come from anyone, so the server a
/// workflow runs on (and receives the input files) is always the user's own setting. A `server`
/// key in an older or hand-written recipe is ignored.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Recipe {
    /// The workflow in API format, as exported (the bindings are applied when it is queued).
    pub workflow: Value,
    /// Input overrides, applied in order.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub inputs: Vec<Binding>,
    /// Node ids whose outputs the clip uses (empty = every output node).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub outputs: Vec<String>,
}

impl Recipe {
    /// A recipe for `workflow` (validated).
    pub fn new(workflow: Value) -> Result<Self> {
        Workflow::parse(&workflow)?;
        Ok(Self { workflow, ..Default::default() })
    }

    /// Set or replace bindings (same node + input replaces; a binding with neither `value` nor
    /// `file` removes the override).
    pub fn bind(&mut self, bindings: impl IntoIterator<Item = Binding>) {
        for b in bindings {
            self.inputs.retain(|x| !x.same_input(&b));
            if b.value.is_some() || b.file.is_some() {
                self.inputs.push(b);
            }
        }
    }

    /// Whether `output` is one the clip uses.
    pub fn wants(&self, output: &Output) -> bool {
        self.outputs.is_empty() || self.outputs.contains(&output.node)
    }

    /// The local files the recipe uploads: (node, input, path).
    pub fn files(&self) -> impl Iterator<Item = (&str, &str, &str)> {
        self.inputs.iter().filter_map(|b| b.file.as_deref().map(|f| (b.node.as_str(), b.input.as_str(), f)))
    }
}

/// Check and normalise a server address: `http(s)://host[:port][/prefix]`, without a trailing
/// `/`, a query, a fragment, whitespace or control characters.
pub fn normalize_server(url: &str) -> Result<String> {
    let u = url.trim().trim_end_matches('/');
    let rest = u.strip_prefix("http://").or_else(|| u.strip_prefix("https://"));
    match rest {
        Some(r) if !r.is_empty() && !r.starts_with('/') && !r.contains(|c: char| c.is_whitespace() || c.is_control() || c == '?' || c == '#') => {
            Ok(u.to_string())
        }
        _ => Err(ComfyError::Connection(format!("`{url}` is not an http:// or https:// server address"))),
    }
}

/// A 64-bit FNV-1a hash (upload names, seeds): stable across runs and platforms.
pub fn fnv1a(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in bytes {
        h ^= u64::from(*b);
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    h
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn bind_replaces_and_removes() {
        let mut r = Recipe::default();
        r.bind([Binding::value("6", "text", json!("a cat")), Binding::value("3", "seed", json!(1))]);
        r.bind([Binding::value("6", "text", json!("a dog"))]);
        assert_eq!(r.inputs.len(), 2);
        assert_eq!(r.inputs[1].value, Some(json!("a dog")));
        r.bind([Binding { node: "3".into(), input: "seed".into(), ..Default::default() }]);
        assert_eq!(r.inputs.len(), 1);
    }

    #[test]
    fn recipe_round_trips_with_compact_bindings() {
        let mut r = Recipe::new(json!({"1": {"class_type": "LoadImage", "inputs": {"image": "x.png"}}})).unwrap();
        r.bind([Binding::file("1", "image", "/tmp/in.png")]);
        let v = serde_json::to_value(&r).unwrap();
        assert_eq!(v["inputs"][0], json!({"node": "1", "input": "image", "file": "/tmp/in.png"}));
        assert!(v.get("server").is_none());
        let back: Recipe = serde_json::from_value(v).unwrap();
        assert_eq!(back, r);
    }

    #[test]
    fn a_recipe_never_carries_a_server() {
        // a project file naming a server of its own: the server is dropped, the rest is kept
        let v = json!({"server": "http://attacker.example:8188", "workflow": {"9": {"class_type": "SaveImage", "inputs": {}}}, "outputs": ["9"]});
        let r: Recipe = serde_json::from_value(v).unwrap();
        assert_eq!(r.outputs, ["9"]);
        let back = serde_json::to_value(&r).unwrap();
        assert!(back.get("server").is_none(), "{back}");
        assert!(!back.to_string().contains("attacker"), "{back}");
    }

    #[test]
    fn recipe_lists_its_files() {
        let mut r = Recipe::new(json!({"1": {"class_type": "LoadImage", "inputs": {"image": "x.png"}}})).unwrap();
        r.bind([Binding::file("1", "image", "/in.png"), Binding::value("1", "upload", json!("image"))]);
        assert_eq!(r.files().collect::<Vec<_>>(), [("1", "image", "/in.png")]);
    }

    #[test]
    fn server_addresses() {
        assert_eq!(normalize_server(" http://127.0.0.1:8188/ ").unwrap(), "http://127.0.0.1:8188");
        assert_eq!(normalize_server("https://gpu.example/comfy").unwrap(), "https://gpu.example/comfy");
        for bad in ["", "127.0.0.1:8188", "ftp://x", "http://", "http:///x", "http://a b", "http://a?x=1", "http://a#f", "http://a\nb", "http://a\u{7f}"] {
            assert!(normalize_server(bad).is_err(), "{bad:?}");
        }
    }
}
