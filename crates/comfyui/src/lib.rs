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
//! overrides and the server. Running the same recipe again regenerates the clip.
//!
//! Nothing here touches the file system: input files arrive as bytes and outputs leave as bytes.
//! See `docs/comfyui.md` for the user-facing behaviour.

#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable))]

pub mod client;
pub mod fake;
#[cfg(feature = "http")]
pub mod http;
pub mod protocol;
pub mod workflow;

use serde::{Deserialize, Serialize};
use serde_json::Value;

pub use client::{Client, Fetched, Progress, Response, RunOptions, RunResult, Transport};
pub use protocol::{FileRef, Output, OutputKind};
pub use workflow::{InputInfo, InputKind, NodeInfo, Workflow};

/// The server a new clip talks to when nothing else is set (ComfyUI's default listen address).
pub const DEFAULT_SERVER: &str = "http://127.0.0.1:8188";

/// The `client_id` FilmCraft queues prompts under.
pub const CLIENT_ID: &str = "filmcraft";

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
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Recipe {
    /// Server base URL; empty = the preferences' server.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub server: String,
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

    /// The server to use: this recipe's, else `default`.
    pub fn server_or<'a>(&'a self, default: &'a str) -> &'a str {
        if self.server.trim().is_empty() { default } else { self.server.trim() }
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
    fn server_falls_back_to_default() {
        let mut r = Recipe::default();
        assert_eq!(r.server_or(DEFAULT_SERVER), DEFAULT_SERVER);
        r.server = " http://gpu:8188 ".into();
        assert_eq!(r.server_or(DEFAULT_SERVER), "http://gpu:8188");
    }
}
