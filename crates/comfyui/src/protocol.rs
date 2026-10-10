//! The ComfyUI server's JSON.
//!
//! | call | what FilmCraft reads |
//! |---|---|
//! | `POST /prompt {prompt, client_id}` | `prompt_id`, or `error` + `node_errors` when the workflow is refused |
//! | `GET /history/<id>` | `{<id>: {outputs: {<node>: {<key>: [..]}}, status: {status_str, completed, messages}}}`; `{}` until it ran |
//! | `GET /queue` | `queue_running` / `queue_pending`: `[[number, prompt_id, …], …]` |
//! | `GET /view?filename&subfolder&type` | the bytes of an output file |
//! | `POST /upload/image` (multipart) | `{name, subfolder, type}` of an uploaded input file |
//!
//! Output nodes report their results under keys that vary by node (`images`, `gifs`, `audio`,
//! `video`, `text`…): every array element that names a file (`{filename, subfolder, type}`) is a
//! file output, every string a text output. Flags such as `"animated": [true]` are ignored. File
//! outputs are classified by extension ([`OutputKind`]).

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{ComfyError, Result};

/// What an output is, by file extension.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum OutputKind {
    Video,
    Image,
    Audio,
    Text,
    /// A file FilmCraft can't use as media (latents, 3D models…).
    Other,
}

impl OutputKind {
    pub fn of_file(name: &str) -> Self {
        let ext = name.rsplit_once('.').map(|(_, e)| e.to_ascii_lowercase()).unwrap_or_default();
        match ext.as_str() {
            "mp4" | "m4v" | "mov" | "mkv" | "webm" | "avi" | "mxf" | "mpg" | "mpeg" | "ts" | "y4m" => Self::Video,
            "png" | "jpg" | "jpeg" | "webp" | "bmp" | "tif" | "tiff" | "gif" => Self::Image,
            "wav" | "wave" | "flac" | "mp3" | "ogg" | "opus" | "m4a" | "aac" | "aif" | "aiff" => Self::Audio,
            "txt" | "srt" | "vtt" | "json" | "md" => Self::Text,
            _ => Self::Other,
        }
    }
    /// Media a clip can show (video, image or audio).
    pub fn is_media(self) -> bool {
        matches!(self, Self::Video | Self::Image | Self::Audio)
    }
}

/// A file in one of the server's folders.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileRef {
    pub filename: String,
    #[serde(default)]
    pub subfolder: String,
    /// `output`, `temp` (previews) or `input`.
    #[serde(rename = "type", default = "output_folder")]
    pub folder: String,
}

fn output_folder() -> String {
    "output".into()
}

impl FileRef {
    /// The `/view` path that serves this file.
    pub fn view_path(&self) -> String {
        format!("/view?filename={}&subfolder={}&type={}", encode(&self.filename), encode(&self.subfolder), encode(&self.folder))
    }
    /// The value a loader node's input takes for this file (`subfolder/name`).
    pub fn input_value(&self) -> String {
        if self.subfolder.is_empty() { self.filename.clone() } else { format!("{}/{}", self.subfolder, self.filename) }
    }
}

/// One result of a finished prompt.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Output {
    /// The output node's id.
    pub node: String,
    /// The key the node reported it under (`images`, `audio`, `text`…).
    pub key: String,
    pub kind: OutputKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file: Option<FileRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
}

/// Where a prompt is.
#[derive(Clone, Debug, PartialEq)]
pub enum State {
    /// Waiting in the queue behind `ahead` prompts.
    Pending {
        ahead: usize,
    },
    Running,
    /// Not in the queue and not in the history (yet): just submitted, or the server forgot it.
    Unknown,
}

/// A prompt's history entry.
#[derive(Clone, Debug, PartialEq)]
pub enum Finished {
    Success(Vec<Output>),
    Error(String),
}

/// The `prompt_id` of a `POST /prompt` response; a refusal becomes [`ComfyError::Rejected`] with
/// the server's message and every node error.
pub fn parse_queued(status: u16, body: &[u8]) -> Result<String> {
    let v: Value = serde_json::from_slice(body).map_err(|e| ComfyError::Server(format!("POST /prompt: HTTP {status}, not JSON: {e}")))?;
    if let Some(id) = v.get("prompt_id").and_then(Value::as_str).filter(|_| (200..300).contains(&status)) {
        return Ok(id.to_string());
    }
    let mut parts = Vec::new();
    if let Some(e) = v.get("error") {
        let m = e.get("message").and_then(Value::as_str).or(e.as_str()).unwrap_or("error");
        let d = e.get("details").and_then(Value::as_str).filter(|d| !d.is_empty());
        parts.push(match d {
            Some(d) => format!("{m}: {d}"),
            None => m.to_string(),
        });
    }
    if let Some(ne) = v.get("node_errors").and_then(Value::as_object) {
        for (node, ev) in ne.iter().take(16) {
            let class = ev.get("class_type").and_then(Value::as_str).unwrap_or("node");
            for e in ev.get("errors").and_then(Value::as_array).into_iter().flatten().take(4) {
                let m = e.get("message").and_then(Value::as_str).unwrap_or("error");
                let d = e.get("details").and_then(Value::as_str).filter(|d| !d.is_empty());
                parts.push(match d {
                    Some(d) => format!("node {node} ({class}): {m}: {d}"),
                    None => format!("node {node} ({class}): {m}"),
                });
            }
        }
    }
    if parts.is_empty() {
        parts.push(format!("HTTP {status}"));
    }
    Err(ComfyError::Rejected(parts.join("; ")))
}

/// Longest node id, output key, file or folder name taken from a history entry.
const MAX_NAME: usize = 1024;

/// `t` cut to at most `max` bytes, at a character boundary.
fn cut(t: &str, max: usize) -> &str {
    let mut end = t.len().min(max);
    while !t.is_char_boundary(end) {
        end -= 1;
    }
    t.get(..end).unwrap_or_default()
}

/// The outputs listed by a history entry's `outputs` object, in node id order: at most
/// [`crate::MAX_OUTPUTS`], texts cut to [`crate::MAX_TEXT`] bytes, entries with absurdly long
/// names left out.
pub fn parse_outputs(outputs: &Value) -> Vec<Output> {
    let Some(map) = outputs.as_object() else { return Vec::new() };
    let mut nodes: Vec<(&String, &Value)> = map.iter().filter(|(n, _)| n.len() <= MAX_NAME).collect();
    nodes.sort_by(|a, b| match (a.0.parse::<u64>(), b.0.parse::<u64>()) {
        (Ok(x), Ok(y)) => x.cmp(&y),
        _ => a.0.cmp(b.0),
    });
    let mut out = Vec::new();
    for (node, v) in nodes {
        let Some(keys) = v.as_object() else { continue };
        for (key, list) in keys.iter().filter(|(k, _)| k.len() <= MAX_NAME) {
            for e in list.as_array().into_iter().flatten() {
                if out.len() >= crate::MAX_OUTPUTS {
                    return out;
                }
                if let Some(t) = e.as_str() {
                    let text = cut(t, crate::MAX_TEXT).to_string();
                    out.push(Output { node: node.clone(), key: key.clone(), kind: OutputKind::Text, file: None, text: Some(text) });
                } else if let Ok(f) = serde_json::from_value::<FileRef>(e.clone())
                    && !f.filename.is_empty()
                    && [&f.filename, &f.subfolder, &f.folder].iter().all(|x| x.len() <= MAX_NAME)
                {
                    let kind = OutputKind::of_file(&f.filename);
                    out.push(Output { node: node.clone(), key: key.clone(), kind, file: Some(f), text: None });
                }
            }
        }
    }
    out
}

/// The finished state of `prompt_id` in a `GET /history/<id>` response (None: not finished).
pub fn parse_history(v: &Value, prompt_id: &str) -> Option<Finished> {
    let entry = v.get(prompt_id)?;
    let status = entry.get("status");
    let status_str = status.and_then(|s| s.get("status_str")).and_then(Value::as_str);
    let completed = status.and_then(|s| s.get("completed")).and_then(Value::as_bool);
    if status_str == Some("error") {
        let msg = status
            .and_then(|s| s.get("messages"))
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter(|m| m.get(0).and_then(Value::as_str) == Some("execution_error"))
            .filter_map(|m| m.get(1))
            .map(|d| {
                let e = d.get("exception_message").and_then(Value::as_str).unwrap_or("execution error").trim();
                match (d.get("node_id").and_then(Value::as_str), d.get("node_type").and_then(Value::as_str)) {
                    (Some(n), Some(t)) => format!("node {n} ({t}): {e}"),
                    _ => e.to_string(),
                }
            })
            .next()
            .unwrap_or_else(|| "execution error".into());
        return Some(Finished::Error(msg));
    }
    // older servers have no `status`: an entry with outputs is a finished run
    if completed == Some(false) && status_str != Some("success") {
        return None;
    }
    Some(Finished::Success(entry.get("outputs").map(parse_outputs).unwrap_or_default()))
}

/// Where `prompt_id` is in a `GET /queue` response.
pub fn parse_queue(v: &Value, prompt_id: &str) -> State {
    let ids = |k: &str| -> Vec<String> {
        v.get(k).and_then(Value::as_array).into_iter().flatten().filter_map(|e| e.get(1).and_then(Value::as_str)).map(str::to_string).collect()
    };
    if ids("queue_running").iter().any(|i| i == prompt_id) {
        return State::Running;
    }
    // pending entries are [number, id, …]; lower numbers run first
    let mut pending: Vec<(i64, String)> = v
        .get("queue_pending")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|e| Some((e.get(0)?.as_i64().unwrap_or(0), e.get(1)?.as_str()?.to_string())))
        .collect();
    pending.sort();
    match pending.iter().position(|(_, i)| i == prompt_id) {
        Some(k) => State::Pending { ahead: k + ids("queue_running").len() },
        None => State::Unknown,
    }
}

/// The uploaded file of a `POST /upload/image` response.
pub fn parse_uploaded(status: u16, body: &[u8]) -> Result<FileRef> {
    if !(200..300).contains(&status) {
        let text = String::from_utf8_lossy(body.get(..body.len().min(300)).unwrap_or_default()).into_owned();
        return Err(ComfyError::Server(format!("upload: HTTP {status}: {text}")));
    }
    let v: Value = serde_json::from_slice(body).map_err(|e| ComfyError::Server(format!("upload: not JSON: {e}")))?;
    let name = v.get("name").and_then(Value::as_str).filter(|n| !n.is_empty()).ok_or_else(|| ComfyError::Server("upload: no `name` in the response".into()))?;
    Ok(FileRef {
        filename: name.to_string(),
        subfolder: v.get("subfolder").and_then(Value::as_str).unwrap_or_default().to_string(),
        folder: v.get("type").and_then(Value::as_str).unwrap_or("input").to_string(),
    })
}

/// Percent-encode a query value (RFC 3986 unreserved characters pass through).
pub fn encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
            out.push(char::from(b));
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn queued_and_refused() {
        assert_eq!(parse_queued(200, br#"{"prompt_id":"abc","number":3,"node_errors":{}}"#).unwrap(), "abc");
        let body = json!({
            "error": {"type": "prompt_outputs_failed_validation", "message": "Prompt outputs failed validation", "details": ""},
            "node_errors": {"4": {"class_type": "CheckpointLoaderSimple", "errors": [{"message": "Value not in list", "details": "ckpt_name: 'x' not in []"}]}}
        });
        let e = parse_queued(400, body.to_string().as_bytes()).unwrap_err().to_string();
        assert!(e.contains("Prompt outputs failed validation"), "{e}");
        assert!(e.contains("node 4 (CheckpointLoaderSimple): Value not in list: ckpt_name"), "{e}");
        assert!(matches!(parse_queued(500, b"<html>"), Err(ComfyError::Server(_))));
        assert!(matches!(parse_queued(400, b"{}"), Err(ComfyError::Rejected(_))));
    }

    #[test]
    fn history_outputs_of_every_kind() {
        let h = json!({"p1": {
            "outputs": {
                "12": {"audio": [{"filename": "a_00001_.flac", "subfolder": "audio", "type": "output"}]},
                "9": {"images": [{"filename": "img_00001_.png", "subfolder": "", "type": "output"}], "animated": [false]},
                "20": {"gifs": [{"filename": "vid_00001.mp4", "subfolder": "", "type": "output", "format": "video/h264-mp4"}]},
                "30": {"text": ["a prompt for the next shot"]}
            },
            "status": {"status_str": "success", "completed": true, "messages": []}
        }});
        let Some(Finished::Success(o)) = parse_history(&h, "p1") else { panic!() };
        let kinds: Vec<(&str, OutputKind)> = o.iter().map(|x| (x.node.as_str(), x.kind)).collect();
        assert_eq!(kinds, [("9", OutputKind::Image), ("12", OutputKind::Audio), ("20", OutputKind::Video), ("30", OutputKind::Text)]);
        assert_eq!(o[1].file.as_ref().unwrap().view_path(), "/view?filename=a_00001_.flac&subfolder=audio&type=output");
        assert_eq!(o[3].text.as_deref(), Some("a prompt for the next shot"));
        assert_eq!(parse_history(&json!({}), "p1"), None);
    }

    #[test]
    fn outputs_are_capped() {
        // a server listing a million files and a huge text: bounded lists, cut text
        let files: Vec<Value> = (0..5000).map(|i| json!({"filename": format!("f{i}.png")})).collect();
        let long = "é".repeat(crate::MAX_TEXT);
        let o = parse_outputs(&json!({"1": {"text": [long]}, "2": {"images": files}, "3": {"images": [{"filename": "x".repeat(5000) + ".png"}]}}));
        assert_eq!(o.len(), crate::MAX_OUTPUTS);
        let t = o[0].text.as_deref().unwrap();
        assert!(t.len() <= crate::MAX_TEXT && t.chars().all(|c| c == 'é'), "{}", t.len());
        assert!(o.iter().all(|x| x.node != "3"));
    }

    #[test]
    fn history_errors_and_running() {
        let h = json!({"p": {"outputs": {}, "status": {"status_str": "error", "completed": false, "messages": [
            ["execution_start", {}],
            ["execution_error", {"node_id": "3", "node_type": "KSampler", "exception_message": "CUDA out of memory\n"}]
        ]}}});
        assert_eq!(parse_history(&h, "p"), Some(Finished::Error("node 3 (KSampler): CUDA out of memory".into())));
        let running = json!({"p": {"outputs": {}, "status": {"status_str": null, "completed": false}}});
        assert_eq!(parse_history(&running, "p"), None);
        let legacy = json!({"p": {"outputs": {"9": {"images": [{"filename": "x.png"}]}}}});
        let Some(Finished::Success(o)) = parse_history(&legacy, "p") else { panic!() };
        assert_eq!(o[0].file.as_ref().unwrap().folder, "output");
    }

    #[test]
    fn queue_positions() {
        let q = json!({"queue_running": [[5, "r", {}]], "queue_pending": [[9, "b", {}], [7, "a", {}]]});
        assert_eq!(parse_queue(&q, "r"), State::Running);
        assert_eq!(parse_queue(&q, "a"), State::Pending { ahead: 1 });
        assert_eq!(parse_queue(&q, "b"), State::Pending { ahead: 2 });
        assert_eq!(parse_queue(&q, "z"), State::Unknown);
        assert_eq!(parse_queue(&json!(null), "z"), State::Unknown);
    }

    #[test]
    fn encodes_query_values() {
        assert_eq!(encode("a b/c&d.png"), "a%20b%2Fc%26d.png");
        assert_eq!(encode("é"), "%C3%A9");
        let f = FileRef { filename: "x.png".into(), subfolder: "in".into(), folder: "input".into() };
        assert_eq!(f.input_value(), "in/x.png");
    }

    #[test]
    fn upload_response() {
        let f = parse_uploaded(200, br#"{"name":"clip.png","subfolder":"","type":"input"}"#).unwrap();
        assert_eq!(f.input_value(), "clip.png");
        assert!(parse_uploaded(413, b"too big").is_err());
        assert!(parse_uploaded(200, b"{}").is_err());
    }
}
