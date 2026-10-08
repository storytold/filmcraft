//! Workflows in ComfyUI's API format.
//!
//! An API workflow is a JSON object of nodes keyed by id:
//!
//! ```json
//! {"6": {"class_type": "CLIPTextEncode", "inputs": {"text": "a cat", "clip": ["4", 1]}, "_meta": {"title": "Prompt"}}}
//! ```
//!
//! An input is either a literal (string, number, bool…) or a link `[node id, output index]` to
//! another node's output. Literals are what a clip can override ([`crate::Binding`]); links are
//! the graph and stay as they are. The workflow files ComfyUI saves for its editor (with `nodes`
//! and `links` arrays) are a different format and are refused with a hint to export the API one.

use serde::Serialize;
use serde_json::{Map, Value};

use crate::{Binding, ComfyError, Result};

/// The most nodes a workflow may have (a guard against hostile files, far above real graphs).
pub const MAX_NODES: usize = 10_000;

/// What kind of literal an input holds.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum InputKind {
    Text,
    Int,
    Float,
    Bool,
    /// A file name in the server's input folder (Load Image / Load Audio / Load Video…): bind a
    /// local file to upload it.
    File,
    /// Anything else (lists, objects, null).
    Other,
}

/// One literal input of a node.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InputInfo {
    pub input: String,
    pub value: Value,
    pub kind: InputKind,
    /// A random seed (`seed`, `noise_seed`, `*_seed`): regenerating with new seeds changes it.
    pub seed: bool,
}

/// A node and its literal inputs.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NodeInfo {
    pub node: String,
    pub class_type: String,
    /// The node's title in ComfyUI (`_meta.title`), else its class.
    pub title: String,
    pub inputs: Vec<InputInfo>,
}

/// A validated API-format workflow.
#[derive(Clone, Debug, PartialEq)]
pub struct Workflow {
    nodes: Map<String, Value>,
}

/// Whether `v` is a link to another node's output (`["4", 1]`).
pub fn is_link(v: &Value) -> bool {
    matches!(v.as_array().map(Vec::as_slice), Some([Value::String(_), n]) if n.is_u64())
}

/// Whether an input name is a sampler seed.
pub fn is_seed(input: &str) -> bool {
    input == "seed" || input == "noise_seed" || input.ends_with("_seed")
}

/// Inputs that name a file in the server's input folder, on loader nodes.
fn is_file_input(class_type: &str, input: &str, value: &Value) -> bool {
    const NAMES: [&str; 6] = ["image", "audio", "video", "file", "upload", "mask"];
    value.is_string() && class_type.starts_with("Load") && NAMES.iter().any(|n| input == *n || input.starts_with(&format!("{n}_")))
}

fn kind_of(class_type: &str, input: &str, v: &Value) -> InputKind {
    if is_file_input(class_type, input, v) {
        return InputKind::File;
    }
    match v {
        Value::String(_) => InputKind::Text,
        Value::Bool(_) => InputKind::Bool,
        Value::Number(n) if n.is_i64() || n.is_u64() => InputKind::Int,
        Value::Number(_) => InputKind::Float,
        _ => InputKind::Other,
    }
}

/// Node ids in a stable, human order: numbers numerically (`"2"` before `"10"`), then the rest.
fn order(a: &str, b: &str) -> std::cmp::Ordering {
    match (a.parse::<u64>(), b.parse::<u64>()) {
        (Ok(x), Ok(y)) => x.cmp(&y),
        (Ok(_), Err(_)) => std::cmp::Ordering::Less,
        (Err(_), Ok(_)) => std::cmp::Ordering::Greater,
        _ => a.cmp(b),
    }
}

impl Workflow {
    /// Validate an API-format workflow.
    pub fn parse(v: &Value) -> Result<Self> {
        let bad = |m: String| ComfyError::Workflow(m);
        let Some(obj) = v.as_object() else { return Err(bad("expected a JSON object of nodes (ComfyUI ▸ Workflow ▸ Export (API))".into())) };
        if obj.get("nodes").is_some_and(Value::is_array) || obj.get("links").is_some_and(Value::is_array) {
            return Err(bad("this is an editor workflow; in ComfyUI use Workflow ▸ Export (API) and load that file".into()));
        }
        if obj.is_empty() {
            return Err(bad("the workflow has no nodes".into()));
        }
        if obj.len() > MAX_NODES {
            return Err(bad(format!("the workflow has {} nodes (at most {MAX_NODES})", obj.len())));
        }
        for (id, n) in obj {
            if !n.get("class_type").is_some_and(Value::is_string) {
                return Err(bad(format!("node {id} has no `class_type`")));
            }
            let Some(inputs) = n.get("inputs").and_then(Value::as_object) else { return Err(bad(format!("node {id} has no `inputs` object"))) };
            for (name, val) in inputs {
                if is_link(val)
                    && let Some(target) = val.get(0).and_then(Value::as_str)
                    && !obj.contains_key(target)
                {
                    return Err(bad(format!("node {id} input `{name}` links to missing node {target}")));
                }
            }
        }
        Ok(Self { nodes: obj.clone() })
    }

    pub fn len(&self) -> usize {
        self.nodes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }

    /// Every node with its literal (overridable) inputs, in node id order.
    pub fn nodes(&self) -> Vec<NodeInfo> {
        let mut ids: Vec<&String> = self.nodes.keys().collect();
        ids.sort_by(|a, b| order(a, b));
        ids.into_iter()
            .filter_map(|id| {
                let n = self.nodes.get(id)?;
                let class_type = n.get("class_type").and_then(Value::as_str).unwrap_or_default().to_string();
                let title = n.pointer("/_meta/title").and_then(Value::as_str).filter(|t| !t.trim().is_empty()).unwrap_or(&class_type).to_string();
                let inputs = n
                    .get("inputs")
                    .and_then(Value::as_object)
                    .map(|m| {
                        m.iter()
                            .filter(|(_, v)| !is_link(v))
                            .map(|(k, v)| InputInfo { input: k.clone(), value: v.clone(), kind: kind_of(&class_type, k, v), seed: is_seed(k) && v.is_number() })
                            .collect()
                    })
                    .unwrap_or_default();
                Some(NodeInfo { node: id.clone(), class_type, title, inputs })
            })
            .collect()
    }

    /// The current literal value of `node`'s `input` (None: no such node/input, or it is a link).
    pub fn input(&self, node: &str, input: &str) -> Option<&Value> {
        self.nodes.get(node)?.get("inputs")?.get(input).filter(|v| !is_link(v))
    }

    /// The seed inputs (`(node, input)`), for regenerating with new seeds.
    pub fn seeds(&self) -> Vec<(String, String)> {
        self.nodes().into_iter().flat_map(|n| n.inputs.into_iter().filter(|i| i.seed).map(move |i| (n.node.clone(), i.input))).collect()
    }

    /// Check a binding against the workflow: the node must exist, and a link (part of the graph)
    /// can't be overridden.
    pub fn check_binding(&self, b: &Binding) -> Result<()> {
        let n = self.nodes.get(&b.node).ok_or_else(|| ComfyError::Workflow(format!("no node {}", b.node)))?;
        if n.get("inputs").and_then(|i| i.get(&b.input)).is_some_and(is_link) {
            return Err(ComfyError::Workflow(format!("node {} input `{}` is a link, not a value", b.node, b.input)));
        }
        if b.input.is_empty() {
            return Err(ComfyError::Workflow(format!("node {}: empty input name", b.node)));
        }
        Ok(())
    }

    /// The workflow to queue: a copy with `values` (`(node, input, value)`) set.
    pub fn apply(&self, values: &[(String, String, Value)]) -> Result<Value> {
        let mut nodes = self.nodes.clone();
        for (node, input, value) in values {
            let inputs = nodes
                .get_mut(node)
                .and_then(|n| n.get_mut("inputs"))
                .and_then(Value::as_object_mut)
                .ok_or_else(|| ComfyError::Workflow(format!("no node {node}")))?;
            if inputs.get(input).is_some_and(is_link) {
                return Err(ComfyError::Workflow(format!("node {node} input `{input}` is a link, not a value")));
            }
            inputs.insert(input.clone(), value.clone());
        }
        Ok(Value::Object(nodes))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn txt2img() -> Value {
        json!({
            "3": {"class_type": "KSampler", "inputs": {"seed": 42, "steps": 20, "cfg": 7.5, "model": ["4", 0], "positive": ["6", 0]}},
            "4": {"class_type": "CheckpointLoaderSimple", "inputs": {"ckpt_name": "sd15.safetensors"}},
            "6": {"class_type": "CLIPTextEncode", "inputs": {"text": "a cat", "clip": ["4", 1]}, "_meta": {"title": "Positive"}},
            "10": {"class_type": "LoadImage", "inputs": {"image": "example.png"}},
            "9": {"class_type": "SaveImage", "inputs": {"images": ["3", 0], "filename_prefix": "FilmCraft"}}
        })
    }

    #[test]
    fn lists_literal_inputs_in_node_order() {
        let w = Workflow::parse(&txt2img()).unwrap();
        let nodes = w.nodes();
        assert_eq!(nodes.iter().map(|n| n.node.as_str()).collect::<Vec<_>>(), ["3", "4", "6", "9", "10"]);
        let ks = &nodes[0];
        assert_eq!(ks.inputs.iter().map(|i| i.input.as_str()).collect::<Vec<_>>(), ["cfg", "seed", "steps"]);
        assert_eq!(ks.inputs[0].kind, InputKind::Float);
        assert!(ks.inputs[1].seed);
        assert_eq!(nodes[2].title, "Positive");
        assert_eq!(nodes[4].inputs[0].kind, InputKind::File);
        assert_eq!(w.seeds(), vec![("3".to_string(), "seed".to_string())]);
    }

    #[test]
    fn apply_sets_values_and_keeps_links() {
        let w = Workflow::parse(&txt2img()).unwrap();
        let out = w.apply(&[("6".into(), "text".into(), json!("a dog"))]).unwrap();
        assert_eq!(out["6"]["inputs"]["text"], "a dog");
        assert_eq!(out["6"]["inputs"]["clip"], json!(["4", 1]));
        assert_eq!(out["6"]["_meta"]["title"], "Positive");
        assert!(w.apply(&[("6".into(), "clip".into(), json!("x"))]).is_err());
        assert!(w.apply(&[("99".into(), "text".into(), json!("x"))]).is_err());
    }

    #[test]
    fn refuses_editor_workflows_and_junk() {
        let e = Workflow::parse(&json!({"nodes": [], "links": []})).unwrap_err();
        assert!(e.to_string().contains("Export (API)"), "{e}");
        assert!(Workflow::parse(&json!([])).is_err());
        assert!(Workflow::parse(&json!({})).is_err());
        assert!(Workflow::parse(&json!({"1": {"inputs": {}}})).is_err());
        assert!(Workflow::parse(&json!({"1": {"class_type": "X"}})).is_err());
        assert!(Workflow::parse(&json!({"1": {"class_type": "X", "inputs": {"a": ["7", 0]}}})).is_err());
    }

    #[test]
    fn check_binding() {
        let w = Workflow::parse(&txt2img()).unwrap();
        assert!(w.check_binding(&Binding::value("6", "text", json!("x"))).is_ok());
        assert!(w.check_binding(&Binding::value("6", "clip", json!("x"))).is_err());
        assert!(w.check_binding(&Binding::value("77", "text", json!("x"))).is_err());
    }
}
