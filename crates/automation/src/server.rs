//! The MCP server. Tool names are snake_case.

use std::sync::{Arc, Mutex, PoisonError};

use base64::Engine as _;
use filmcraft_engine::Session;
use filmcraft_time::{FrameRate, Tick};
use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{CallToolResult, ContentBlock as Content};
use rmcp::{ErrorData as McpError, ServerHandler, ServiceExt, tool, tool_handler, tool_router};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::sample::{SampleLayout, SampledFrame, format_timestamp, stitch};
use crate::{AutomationError, BridgeClient, png_rgba};

pub enum Backend {
    Headless(Arc<Mutex<Session>>),
    Bridge(Arc<BridgeClient>),
}

/// A panic payload as text.
fn panic_message(p: Box<dyn std::any::Any + Send>) -> String {
    p.downcast_ref::<&str>().map(|s| s.to_string()).or_else(|| p.downcast_ref::<String>().cloned()).unwrap_or_else(|| "panic".into())
}

/// A failed blocking task: a panic is reported as an internal error and the session stays usable.
fn join_error(e: tokio::task::JoinError) -> AutomationError {
    match e.try_into_panic() {
        Ok(p) => AutomationError::Other(format!("internal error: {}", panic_message(p))),
        Err(e) => AutomationError::Other(e.to_string()),
    }
}

#[derive(Clone)]
pub struct FilmcraftMcp {
    backend: Arc<Backend>,
    /// Bridge `render_frame` moves the app's one playhead and screenshots the one Program monitor, so
    /// calls (which MCP clients may send in parallel) take turns instead of seeing each other's frame.
    render_lock: Arc<tokio::sync::Mutex<()>>,
    tool_router: ToolRouter<Self>,
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct ListParams {
    /// Only commands whose id or label contains this text (case-insensitive).
    #[serde(default)]
    pub filter: Option<String>,
    /// Only commands that can run right now.
    #[serde(default)]
    pub enabled_only: Option<bool>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct RunParams {
    /// Command id, e.g. `sequence.addEdit`, `timeline.trim`, `effects.apply`, `file.import`.
    pub id: String,
    /// Parameters object (see `params` in `command_list`).
    #[serde(default)]
    pub params: Option<Value>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct BatchStep {
    /// Command id.
    pub id: String,
    /// Parameters object.
    #[serde(default)]
    pub params: Option<Value>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct BatchParams {
    /// Commands to run in order.
    pub steps: Vec<BatchStep>,
    /// Stop at the first failing step (default true).
    #[serde(default)]
    pub stop_on_error: Option<bool>,
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct RenderParams {
    /// Timeline time in seconds (default: the playhead).
    #[serde(default)]
    pub seconds: Option<f64>,
    /// Longest side of the PNG (default 960).
    #[serde(default)]
    pub max_side: Option<u32>,
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct ShotParams {
    /// Panel to crop to (e.g. `Timeline`, `Program`); omit for the whole window.
    #[serde(default)]
    pub panel: Option<String>,
    /// Longest side of the PNG (default 1600).
    #[serde(default)]
    pub max_side: Option<u32>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ClickParams {
    /// Element id from `ui_elements` (e.g. `timeline.track.V1.lock`, `tools.Razor`, `project.item.12`).
    #[serde(default)]
    pub id: Option<String>,
    /// Or screen coordinates in points.
    #[serde(default)]
    pub x: Option<f64>,
    #[serde(default)]
    pub y: Option<f64>,
    /// left | right | middle
    #[serde(default)]
    pub button: Option<String>,
    /// Number of clicks (0–10; defaults to 1). Use 2 for double-click. Larger values are rejected.
    #[serde(default)]
    pub count: Option<u32>,
    /// {"shift":bool,"alt":bool,"command":bool,"ctrl":bool}
    #[serde(default)]
    pub modifiers: Option<Value>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct DragParams {
    /// Start: {"id":..} or {"x":..,"y":..} (optionally "fx"/"fy" fractions inside the element).
    pub from: Value,
    /// End point, same forms.
    pub to: Value,
    /// Movement steps (0–1024; defaults to 12; 0 or 1 uses 2). Larger values are rejected.
    #[serde(default)]
    pub steps: Option<u32>,
    #[serde(default)]
    pub modifiers: Option<Value>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct KeyParams {
    /// Key with optional modifiers, e.g. `Space`, `Cmd+K`, `Shift+Delete`, `I`, `L`.
    pub key: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct TypeParams {
    pub text: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct MediaImportParams {
    /// Absolute paths to media files (MP4/MOV, WAV/MP3/FLAC/AIFF/Ogg, PNG/JPEG/…).
    pub paths: Vec<String>,
    /// Bin ID to import into (optional).
    #[serde(default)]
    pub bin: Option<u64>,
    /// Import numbered stills as an image sequence (optional, default false).
    #[serde(default)]
    pub image_sequence: Option<bool>,
}

/// `file.import` params for a `media_import` call.
fn media_import_params(p: MediaImportParams) -> Value {
    let mut params = json!({"paths": p.paths});
    if let Some(bin) = p.bin {
        params["bin"] = json!(bin);
    }
    if let Some(seq) = p.image_sequence {
        params["imageSequence"] = json!(seq);
    }
    params
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct ElementsParams {
    /// Only element ids starting with this prefix (e.g. `timeline.clip.`).
    #[serde(default)]
    pub prefix: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ControlParams {
    /// Any control-channel method (e.g. `ui.set`, `ui.scroll`, `ui.timeline.hit`, `ui.playback`).
    pub method: String,
    #[serde(default)]
    pub params: Option<Value>,
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct TimelineSampleParams {
    /// Start and end time in seconds (e.g. [0.0, 10.0]). Default: full sequence range [0.0, duration].
    #[serde(default)]
    pub range: Option<[f64; 2]>,
    /// Number of evenly spaced samples across the timeline or range (2 to 16, default 6).
    #[serde(default)]
    pub count: Option<u32>,
    /// Layout format: "grid" (default) or "strip".
    #[serde(default)]
    pub layout: Option<SampleLayout>,
    /// Longest side of the composite PNG (default 1200).
    #[serde(default)]
    pub max_side: Option<u32>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct CutInspectParams {
    /// Cut boundary timestamp in seconds.
    pub seconds: f64,
    /// Number of frames preceding the cut to sample (1 to 8, default 2).
    #[serde(default)]
    pub frames_before: Option<u32>,
    /// Number of frames following the cut to sample (1 to 8, default 2).
    #[serde(default)]
    pub frames_after: Option<u32>,
    /// Longest side of the composite PNG (default 1200).
    #[serde(default)]
    pub max_side: Option<u32>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ClipCutParams {
    /// Timestamp in seconds where the cut should be made.
    pub seconds: f64,
    /// Specific clip ID to cut (optional; if omitted, cuts across targeted tracks or all tracks).
    #[serde(default)]
    pub clip: Option<u64>,
    /// Specific track to cut, e.g. "V1" (optional).
    #[serde(default)]
    pub track: Option<String>,
    /// When true, automatically attaches visual cut inspection frames to the response.
    #[serde(default)]
    pub verify: Option<bool>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ClipTrimParams {
    /// ID of the clip to trim.
    pub clip: u64,
    /// Which edge to trim: "in" (head) or "out" (tail).
    pub edge: String,
    /// Trim adjustment in seconds (positive trims forward/right, negative trims backward/left).
    #[serde(default)]
    pub delta_seconds: Option<f64>,
    /// Or trim adjustment in integer frames.
    #[serde(default)]
    pub delta_frames: Option<i64>,
    /// Whether to ripple trim (shifting downstream clips) or regular trim (leaving gaps/overwriting). Default true.
    #[serde(default)]
    pub ripple: Option<bool>,
    /// When true, attaches visual inspection of the trimmed edit point.
    #[serde(default)]
    pub verify: Option<bool>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ClipDeleteParams {
    /// ID of the clip to delete.
    #[serde(default)]
    pub clip: Option<u64>,
    /// Or timestamp in seconds within a gap to delete.
    #[serde(default)]
    pub gap_at: Option<f64>,
    /// Track for gap deletion, e.g. "V1" (defaults to "V1").
    #[serde(default)]
    pub track: Option<String>,
    /// When true, closes the gap by shifting downstream clips left (ripple delete).
    /// When false, leaves empty space (lift / clear). Default true.
    #[serde(default)]
    pub ripple: Option<bool>,
    /// When true, attaches visual verification around the deletion point.
    #[serde(default)]
    pub verify: Option<bool>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ClipArrangeParams {
    /// ID of the clip to move.
    pub clip: u64,
    /// Destination start time on the timeline in seconds.
    pub to_seconds: f64,
    /// Destination track, e.g. "V1", "V2", "A1", or track ID. Default: stay on same track.
    #[serde(default)]
    pub to_track: Option<String>,
    /// When true, inserts the clip by splitting and shifting existing clips (ripple move). Default false (overwrite).
    #[serde(default)]
    pub insert: Option<bool>,
}

fn ok_json(v: &Value) -> CallToolResult {
    CallToolResult::success(vec![Content::text(serde_json::to_string_pretty(v).unwrap_or_default())])
}
fn fail(e: impl std::fmt::Display) -> CallToolResult {
    CallToolResult::error(vec![Content::text(e.to_string())])
}
fn png_result(png: &[u8], note: &str) -> CallToolResult {
    CallToolResult::success(vec![Content::image(base64::engine::general_purpose::STANDARD.encode(png), "image/png"), Content::text(note.to_string())])
}
fn image_with_json(png: &[u8], metadata: &Value) -> CallToolResult {
    CallToolResult::success(vec![
        Content::image(base64::engine::general_purpose::STANDARD.encode(png), "image/png"),
        Content::text(serde_json::to_string_pretty(metadata).unwrap_or_default()),
    ])
}
fn active_clips_at(seq_info: &Value, t: Tick) -> Vec<Value> {
    let mut clips = Vec::new();
    let mut scan_tracks = |tracks: Option<&Vec<Value>>, default_kind: &str| {
        if let Some(tracks) = tracks {
            for track in tracks {
                let track_name = track.get("name").and_then(Value::as_str).unwrap_or(default_kind);
                if let Some(items) = track.get("items").and_then(Value::as_array) {
                    for item in items {
                        let start = item.get("start").and_then(Value::as_i64).unwrap_or(0);
                        let duration = item.get("duration").and_then(Value::as_i64).unwrap_or(0);
                        let end = item.get("end").and_then(Value::as_i64).unwrap_or_else(|| start.saturating_add(duration));
                        if start <= t.0 && t.0 < end {
                            clips.push(json!({
                                "clip": item.get("clip"),
                                "name": item.get("name"),
                                "track": track_name,
                                "start": item.get("start"),
                                "duration": item.get("duration"),
                                "start_seconds": Tick(start).as_seconds_f64(),
                                "duration_seconds": Tick(duration).as_seconds_f64(),
                            }));
                        }
                    }
                }
            }
        }
    };
    scan_tracks(seq_info.get("video").and_then(Value::as_array), "video");
    scan_tracks(seq_info.get("audio").and_then(Value::as_array), "audio");
    clips
}
fn find_clip_in_seq(seq_info: &Value, clip_id: u64) -> Option<Value> {
    for kind in ["video", "audio"] {
        if let Some(tracks) = seq_info.get(kind).and_then(Value::as_array) {
            for track in tracks {
                let track_name = track.get("name").and_then(Value::as_str).unwrap_or(kind);
                let track_id = track.get("id").and_then(Value::as_u64).unwrap_or(0);
                if let Some(items) = track.get("items").and_then(Value::as_array) {
                    for item in items {
                        if item.get("clip").and_then(Value::as_u64) == Some(clip_id) {
                            let mut res = item.clone();
                            res["track_name"] = json!(track_name);
                            res["track_id"] = json!(track_id);
                            return Some(res);
                        }
                    }
                }
            }
        }
    }
    None
}
fn wrap(r: Result<Value, AutomationError>) -> Result<CallToolResult, McpError> {
    Ok(match r {
        Ok(v) => ok_json(&v),
        Err(e) => fail(e),
    })
}

/// Parse-error replies queued for the writer before the reader waits for it to catch up.
const MAX_QUEUED_PARSE_ERRORS: usize = 64;

impl FilmcraftMcp {
    pub fn headless(session: Session) -> Self {
        Self { backend: Arc::new(Backend::Headless(Arc::new(Mutex::new(session)))), render_lock: Arc::default(), tool_router: Self::tool_router() }
    }
    pub fn bridge(addr: &str) -> Result<Self, AutomationError> {
        Ok(Self { backend: Arc::new(Backend::Bridge(Arc::new(BridgeClient::new(addr)?))), render_lock: Arc::default(), tool_router: Self::tool_router() })
    }

    pub async fn serve_stdio(self) -> Result<(), AutomationError> {
        self.serve_io(tokio::io::stdin(), tokio::io::stdout()).await
    }

    /// Serve MCP over any line stream. A line that isn't JSON gets a JSON-RPC parse error
    /// (`-32700`, id null) instead of being dropped silently; the rest go to the rmcp service. One
    /// writer task owns `output`, so replies never interleave. Parse errors wait for room in a
    /// small queue before more input is read: a client that sends junk and never reads its output
    /// stalls its own input instead of growing the server's memory.
    pub async fn serve_io(
        self,
        input: impl tokio::io::AsyncRead + Unpin + Send + 'static,
        output: impl tokio::io::AsyncWrite + Unpin + Send + 'static,
    ) -> Result<(), AutomationError> {
        use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
        let (mut to_service, service_in) = tokio::io::duplex(1 << 20);
        let (service_out, from_service) = tokio::io::duplex(1 << 20);
        let (err_tx, mut err_rx) = tokio::sync::mpsc::channel::<String>(MAX_QUEUED_PARSE_ERRORS);
        let reader = tokio::spawn(async move {
            let mut lines = BufReader::new(input).lines();
            // `err_tx.closed()`: the writer is gone (output failed), so stop forwarding requests whose replies cannot be delivered.
            loop {
                let line = tokio::select! {
                    l = lines.next_line() => match l { Ok(Some(l)) => l, _ => break },
                    () = err_tx.closed() => break,
                };
                if line.trim().is_empty() {
                    continue;
                }
                match serde_json::from_str::<Value>(&line) {
                    Ok(_) => {
                        if to_service.write_all(format!("{line}\n").as_bytes()).await.is_err() {
                            break;
                        }
                    }
                    Err(e) => {
                        let reply = json!({"jsonrpc": "2.0", "id": null, "error": {"code": -32700, "message": format!("parse error: {e}")}});
                        // the writer is gone (output failed): stop reading
                        if err_tx.send(reply.to_string()).await.is_err() {
                            break;
                        }
                    }
                }
            }
            // EOF: closing the pipe ends the service
            let _ = to_service.shutdown().await;
        });
        let writer = tokio::spawn(async move {
            let mut output = output;
            let mut replies = BufReader::new(from_service).lines();
            loop {
                let line = tokio::select! {
                    l = replies.next_line() => match l { Ok(Some(l)) => l, _ => break },
                    Some(e) = err_rx.recv() => e,
                };
                if output.write_all(format!("{line}\n").as_bytes()).await.is_err() || output.flush().await.is_err() {
                    break;
                }
            }
        });
        let running = self.serve((service_in, service_out)).await.map_err(|e| AutomationError::Other(format!("MCP init: {e}")))?;
        let r = running.waiting().await.map_err(|e| AutomationError::Other(e.to_string()));
        reader.abort();
        let _ = writer.await;
        r.map(|_| ())
    }

    /// Run an engine command on whichever backend.
    pub async fn run(&self, id: &str, params: Value) -> Result<Value, AutomationError> {
        match &*self.backend {
            Backend::Headless(s) => {
                let s = s.clone();
                let id = id.to_string();
                tokio::task::spawn_blocking(move || {
                    // A command that panicked earlier poisoned the lock; the session is still usable.
                    let mut g = s.lock().unwrap_or_else(PoisonError::into_inner);
                    g.execute(&id, params).map_err(AutomationError::from)
                })
                .await
                .map_err(join_error)?
            }
            Backend::Bridge(b) => b.execute(id, params).await,
        }
    }

    /// The project tree plus the active sequence (null when there is none).
    ///
    /// `project.inspect` and `sequence.inspect` are two separate calls (two lock scopes in
    /// headless mode, two round trips to the app in bridge mode), so the active sequence can
    /// change between them if another client, or the app's own UI, switches sequence tabs in
    /// that window (#1477). Pinning `sequence.inspect` to the `activeSequence` id this
    /// `project.inspect` just reported keeps the pair consistent: the returned `sequence`, when
    /// not null, always describes the sequence the returned `project.activeSequence` names.
    async fn document(&self) -> Result<Value, AutomationError> {
        let project = self.run("project.inspect", json!({})).await?;
        // `ItemId` is a u64 (`item_p` parses the `item` param with `u64_p`): read it as one too,
        // not `as_i64`, so an id above `i64::MAX` is still looked up instead of silently going null.
        let active = project.get("activeSequence").and_then(Value::as_u64);
        let sequence = match active {
            Some(id) => self.run("sequence.inspect", json!({"item": id})).await.ok(),
            None => None,
        };
        Ok(json!({"project": project, "sequence": sequence}))
    }

    fn bridge_client(&self) -> Option<Arc<BridgeClient>> {
        match &*self.backend {
            Backend::Bridge(b) => Some(b.clone()),
            Backend::Headless(_) => None,
        }
    }

    async fn ui(&self, method: &str, params: Value) -> Result<CallToolResult, McpError> {
        match self.bridge_client() {
            Some(b) => wrap(b.call(method, params).await),
            None => Ok(fail(
                "this tool drives the live app: run `filmcraft-cli mcp --bridge 127.0.0.1:<port>` with the app started as `filmcraft --control <port>`",
            )),
        }
    }
}

#[tool_router]
impl FilmcraftMcp {
    #[tool(
        title = "List commands",
        description = "List every command (id, label, menu path, shortcut, parameter doc, enabled now). Menus, shortcuts, panels and timeline gestures all map to these ids.",
        annotations(read_only_hint = true, destructive_hint = false, idempotent_hint = true, open_world_hint = false)
    )]
    async fn command_list(&self, Parameters(p): Parameters<ListParams>) -> Result<CallToolResult, McpError> {
        let r = self.run("command.list", json!({})).await.map(|v| {
            let f = p.filter.unwrap_or_default().to_ascii_lowercase();
            let only = p.enabled_only.unwrap_or(false);
            Value::Array(
                v.as_array()
                    .cloned()
                    .unwrap_or_default()
                    .into_iter()
                    .filter(|c| {
                        let id = c["id"].as_str().unwrap_or("").to_ascii_lowercase();
                        let label = c["label"].as_str().unwrap_or("").to_ascii_lowercase();
                        (f.is_empty() || id.contains(&f) || label.contains(&f)) && (!only || c["enabled"].as_bool() == Some(true))
                    })
                    .collect(),
            )
        });
        wrap(r)
    }

    #[tool(
        title = "Run a command",
        description = "Run a command by id with JSON params, e.g. {\"id\":\"timeline.razor\",\"params\":{\"seconds\":3.5}} or {\"id\":\"effects.apply\",\"params\":{\"effect\":\"Gaussian Blur\"}}. Undoable edits land in History.",
        annotations(read_only_hint = false, destructive_hint = true, idempotent_hint = false, open_world_hint = false)
    )]
    async fn command_run(&self, Parameters(p): Parameters<RunParams>) -> Result<CallToolResult, McpError> {
        wrap(self.run(&p.id, p.params.unwrap_or(json!({}))).await)
    }

    #[tool(
        title = "Run several commands",
        description = "Run several commands in order: {steps: [{id, params?}], stop_on_error?: true}. Returns {completed, failed, results: [{ok, result | error}]}; each edit is its own undo step.",
        annotations(read_only_hint = false, destructive_hint = true, idempotent_hint = false, open_world_hint = false)
    )]
    async fn command_batch(&self, Parameters(p): Parameters<BatchParams>) -> Result<CallToolResult, McpError> {
        let stop = p.stop_on_error.unwrap_or(true);
        let (mut completed, mut failed, mut results) = (0u32, 0u32, Vec::new());
        for st in p.steps {
            match self.run(&st.id, st.params.unwrap_or(json!({}))).await {
                Ok(v) => {
                    completed += 1;
                    results.push(json!({"ok": true, "result": v}));
                }
                Err(e) => {
                    failed += 1;
                    results.push(json!({"ok": false, "error": e.to_string()}));
                    if stop {
                        break;
                    }
                }
            }
        }
        let v = json!({"completed": completed, "failed": failed, "results": results});
        Ok(if failed > 0 { CallToolResult::error(vec![Content::text(v.to_string())]) } else { ok_json(&v) })
    }

    #[tool(
        title = "Inspect the document",
        description = "The project as JSON: the tree (as project_inspect) plus the active sequence (as sequence_inspect; null when there is none).",
        annotations(read_only_hint = true, destructive_hint = false, idempotent_hint = true, open_world_hint = false)
    )]
    async fn doc_inspect(&self) -> Result<CallToolResult, McpError> {
        wrap(self.document().await)
    }

    #[tool(
        title = "Render a preview",
        description = "The active sequence's frame at `seconds` (default: the playhead) as PNG, same as render_frame (bridge mode: Program monitor screenshot).",
        annotations(read_only_hint = true, destructive_hint = false, idempotent_hint = true, open_world_hint = false)
    )]
    async fn render_preview(&self, p: Parameters<RenderParams>) -> Result<CallToolResult, McpError> {
        self.render_frame(p).await
    }

    #[tool(
        title = "Inspect the project tree",
        description = "Project tree (bins and items with ids, types, durations), active sequence, source clip.",
        annotations(read_only_hint = true, destructive_hint = false, idempotent_hint = true, open_world_hint = false)
    )]
    async fn project_inspect(&self) -> Result<CallToolResult, McpError> {
        wrap(self.run("project.inspect", json!({})).await)
    }

    #[tool(
        title = "Inspect the active sequence",
        description = "The active sequence as JSON: settings, tracks, clips (ids, start/duration in ticks and frames, effects), transitions, markers, in/out, playhead, selection. 254016000000 ticks = 1 second.",
        annotations(read_only_hint = true, destructive_hint = false, idempotent_hint = true, open_world_hint = false)
    )]
    async fn sequence_inspect(&self) -> Result<CallToolResult, McpError> {
        wrap(self.run("sequence.inspect", json!({})).await)
    }

    #[tool(
        title = "Import media",
        description = "Import media files by absolute path (MP4/MOV, WAV/MP3/FLAC/AIFF/Ogg, PNG/JPEG/…).",
        annotations(read_only_hint = false, destructive_hint = false, idempotent_hint = false, open_world_hint = false)
    )]
    async fn media_import(&self, Parameters(p): Parameters<MediaImportParams>) -> Result<CallToolResult, McpError> {
        wrap(self.run("file.import", media_import_params(p)).await)
    }

    #[tool(
        title = "Render a frame",
        description = "Render the active sequence's frame (at `seconds` or the playhead) and return it as PNG.",
        annotations(read_only_hint = true, destructive_hint = false, idempotent_hint = true, open_world_hint = false)
    )]
    async fn render_frame(&self, Parameters(p): Parameters<RenderParams>) -> Result<CallToolResult, McpError> {
        let max = p.max_side.unwrap_or(960);
        match &*self.backend {
            Backend::Headless(s) => {
                let s = s.clone();
                let r = tokio::task::spawn_blocking(move || -> Result<Vec<u8>, AutomationError> {
                    let g = s.lock().unwrap_or_else(PoisonError::into_inner);
                    // read-only: render at `seconds` without moving the playhead (#114)
                    let t = p.seconds.map_or_else(|| g.playhead(), filmcraft_time::Tick::from_seconds_f64);
                    let (w, h) =
                        g.active_sequence().map(|q| (q.settings.width, q.settings.height)).ok_or_else(|| AutomationError::Other("no sequence".into()))?;
                    let scale = render_scale(max, w, h);
                    let img = g.try_render_program_at(scale, t).map_err(|e| AutomationError::Other(e.to_string()))?;
                    png_rgba(img.w as u32, img.h as u32, img.over_black_rgba8(), max)
                })
                .await
                .unwrap_or_else(|e| Err(join_error(e)));
                Ok(match r {
                    Ok(png) => png_result(&png, "rendered frame"),
                    Err(e) => fail(e),
                })
            }
            Backend::Bridge(b) => {
                // held until the playhead is back, also for the no-`seconds` screenshot, which must not catch another call's seek
                let _turn = self.render_lock.lock().await;
                let Some(sec) = p.seconds else {
                    return self.screenshot(&b.clone(), Some("Program".into()), max).await;
                };
                // The Program monitor shows the playhead's frame, so the live playhead has to move
                // for the screenshot; put it (and the selection, which may follow it) back after,
                // so the tool stays read-only (#114).
                let before = match self.run("sequence.inspect", json!({})).await {
                    Ok(v) => v,
                    Err(e) => return Ok(fail(e)),
                };
                if let Err(e) = self.run("playhead.set", json!({"seconds": sec})).await {
                    return Ok(fail(e));
                }
                let shot = self.screenshot(&b.clone(), Some("Program".into()), max).await;
                if let Some(t) = before.get("playhead").and_then(Value::as_i64) {
                    let _ = self.run("playhead.set", json!({"time": t})).await;
                }
                // only when it changed: selecting also leaves trim mode
                let now = self.run("sequence.inspect", json!({})).await.ok();
                if let Some(sel) = before.get("selection").filter(|v| v.is_array())
                    && now.as_ref().and_then(|n| n.get("selection")) != Some(sel)
                {
                    let _ = self.run("timeline.select", json!({"clips": sel})).await;
                }
                shot
            }
        }
    }

    #[tool(
        title = "Live app: UI state",
        description = "Live app: UI state (tool, workspace, panels, timeline zoom, playback, selection, fps).",
        annotations(read_only_hint = true, destructive_hint = false, idempotent_hint = true, open_world_hint = false)
    )]
    async fn ui_inspect(&self) -> Result<CallToolResult, McpError> {
        self.ui("ui.inspect", json!({})).await
    }

    #[tool(
        title = "Live app: on-screen elements",
        description = "Live app: every on-screen interactive element with id, label and rect (points). Use ids with ui_click/ui_drag.",
        annotations(read_only_hint = true, destructive_hint = false, idempotent_hint = true, open_world_hint = false)
    )]
    async fn ui_elements(&self, Parameters(p): Parameters<ElementsParams>) -> Result<CallToolResult, McpError> {
        self.ui("ui.elements", json!({"prefix": p.prefix.unwrap_or_default()})).await
    }

    /// Forward a click to the live editor; an omitted count is sent as null and uses its default.
    #[tool(
        title = "Live app: click",
        description = "Live app: click an element by id or at x,y (points). Supports right/middle button, double-click (count=2) and modifiers.",
        annotations(read_only_hint = false, destructive_hint = true, idempotent_hint = false, open_world_hint = false)
    )]
    async fn ui_click(&self, Parameters(p): Parameters<ClickParams>) -> Result<CallToolResult, McpError> {
        let mut v = json!({"button": p.button, "count": p.count, "modifiers": p.modifiers});
        if let Some(id) = p.id {
            v["id"] = json!(id);
        } else {
            v["x"] = json!(p.x);
            v["y"] = json!(p.y);
        }
        self.ui("ui.click", v).await
    }

    /// Forward a drag to the live editor, which applies the step default and validates its bounds.
    #[tool(
        title = "Live app: drag",
        description = "Live app: press-drag-release from one point/element to another (move clips, trim edges, scrub, resize panels, drop project items onto tracks).",
        annotations(read_only_hint = false, destructive_hint = true, idempotent_hint = false, open_world_hint = false)
    )]
    async fn ui_drag(&self, Parameters(p): Parameters<DragParams>) -> Result<CallToolResult, McpError> {
        self.ui("ui.drag", json!({"from": p.from, "to": p.to, "steps": p.steps, "modifiers": p.modifiers})).await
    }

    #[tool(
        title = "Live app: press a key",
        description = "Live app: press a key or shortcut (e.g. `Space`, `L`, `Cmd+K`, `Shift+Delete`, `Cmd+Z`).",
        annotations(read_only_hint = false, destructive_hint = true, idempotent_hint = false, open_world_hint = false)
    )]
    async fn ui_key(&self, Parameters(p): Parameters<KeyParams>) -> Result<CallToolResult, McpError> {
        self.ui("ui.key", json!({"key": p.key})).await
    }

    #[tool(
        title = "Live app: type text",
        description = "Live app: type text into the focused field.",
        annotations(read_only_hint = false, destructive_hint = true, idempotent_hint = false, open_world_hint = false)
    )]
    async fn ui_type(&self, Parameters(p): Parameters<TypeParams>) -> Result<CallToolResult, McpError> {
        self.ui("ui.type", json!({"text": p.text})).await
    }

    #[tool(
        title = "Live app: screenshot",
        description = "Live app: screenshot of the window or one panel (PNG).",
        annotations(read_only_hint = true, destructive_hint = false, idempotent_hint = true, open_world_hint = false)
    )]
    async fn ui_screenshot(&self, Parameters(p): Parameters<ShotParams>) -> Result<CallToolResult, McpError> {
        match self.bridge_client() {
            Some(b) => self.screenshot(&b, p.panel, p.max_side.unwrap_or(1600)).await,
            None => Ok(fail("ui_screenshot needs bridge mode")),
        }
    }

    #[tool(
        title = "Live app: control-channel call",
        description = "Live app: call any control-channel method directly (ui.set, ui.scroll, ui.timeline.hit, ui.timeline.locate, ui.playback, ui.panel.show, ui.menu.list, …).",
        annotations(read_only_hint = false, destructive_hint = true, idempotent_hint = false, open_world_hint = false)
    )]
    async fn ui_control(&self, Parameters(p): Parameters<ControlParams>) -> Result<CallToolResult, McpError> {
        self.ui(&p.method, p.params.unwrap_or(json!({}))).await
    }

    #[tool(
        title = "Sample timeline frames",
        description = "Sample N frames evenly spaced across the sequence (or a range) and return a composite contact sheet (grid or strip) with timestamp overlays and active clip metadata.",
        annotations(read_only_hint = true, destructive_hint = false, idempotent_hint = true, open_world_hint = false)
    )]
    async fn timeline_sample(&self, Parameters(p): Parameters<TimelineSampleParams>) -> Result<CallToolResult, McpError> {
        let seq_info = match self.run("sequence.inspect", json!({})).await {
            Ok(v) => v,
            Err(e) => return Ok(fail(e)),
        };
        let duration_ticks = seq_info.get("duration").and_then(Value::as_i64).unwrap_or(0);
        let duration = Tick(duration_ticks);
        if duration <= Tick::ZERO {
            return Ok(fail("sequence is empty or has zero duration"));
        }

        let (start_sec, end_sec) = if let Some([s, e]) = p.range {
            if !s.is_finite() || !e.is_finite() || s < 0.0 || e <= s {
                return Ok(fail("invalid range: start must be non-negative and end must be greater than start"));
            }
            let e = e.min(duration.as_seconds_f64());
            if s >= e {
                return Ok(fail("invalid range: start must be before the end of the sequence"));
            }
            (s, e)
        } else {
            (0.0, duration.as_seconds_f64())
        };

        let count = p.count.unwrap_or(6).clamp(2, 16) as usize;
        let step = (end_sec - start_sec) / (count as f64);
        let rate = seq_info
            .get("settings")
            .and_then(|s| s.get("frame_rate"))
            .and_then(|r| serde_json::from_value::<FrameRate>(r.clone()).ok())
            .unwrap_or(FrameRate::FPS_23_976);

        let mut points = Vec::with_capacity(count);
        for i in 0..count {
            let sec = start_sec + (i as f64 + 0.5) * step;
            let tick = rate.snap_nearest(Tick::from_seconds_f64(sec));
            let label = format_timestamp(tick.as_seconds_f64());
            points.push((tick, Some(label)));
        }

        let max = p.max_side.unwrap_or(1200);
        let frames = match self.sample_frames(points, max).await {
            Ok(f) => f,
            Err(e) => return Ok(fail(e)),
        };
        let layout = p.layout.unwrap_or(SampleLayout::Grid);
        let png = match stitch(&frames, layout, max) {
            Ok(b) => b,
            Err(e) => return Ok(fail(e)),
        };

        let mut samples_meta = Vec::with_capacity(frames.len());
        for (i, f) in frames.iter().enumerate() {
            let t = Tick::from_seconds_f64(f.seconds);
            let active = active_clips_at(&seq_info, t);
            samples_meta.push(json!({
                "index": i,
                "seconds": f.seconds,
                "tick": t.0,
                "frame": rate.frame_at(t),
                "timestamp": format_timestamp(f.seconds),
                "active_clips": active,
            }));
        }

        let meta = json!({
            "sequence_duration": duration.as_seconds_f64(),
            "sequence_duration_ticks": duration.0,
            "frame_rate": rate.as_f64(),
            "count": frames.len(),
            "layout": if layout == SampleLayout::Strip { "strip" } else { "grid" },
            "samples": samples_meta,
        });

        Ok(image_with_json(&png, &meta))
    }

    #[tool(
        title = "Inspect cut continuity",
        description = "Sample frames immediately preceding and following a cut point (side-by-side strip) with A/B outgoing/incoming clip labels and gap/transition metadata.",
        annotations(read_only_hint = true, destructive_hint = false, idempotent_hint = true, open_world_hint = false)
    )]
    async fn cut_inspect(&self, Parameters(p): Parameters<CutInspectParams>) -> Result<CallToolResult, McpError> {
        if !p.seconds.is_finite() || p.seconds < 0.0 {
            return Ok(fail("cut seconds must be a non-negative finite number"));
        }
        match self.inspect_cut_internal(p.seconds, p.frames_before, p.frames_after, p.max_side).await {
            Ok((png, meta)) => Ok(image_with_json(&png, &meta)),
            Err(e) => Ok(fail(e)),
        }
    }

    #[tool(
        title = "Cut clip at timestamp",
        description = "Split a clip (or targeted tracks) at `seconds`. Returns the left and right split clip IDs and boundaries. When `verify: true`, attaches visual cut continuity inspection.",
        annotations(read_only_hint = false, destructive_hint = true, idempotent_hint = false, open_world_hint = false)
    )]
    async fn clip_cut(&self, Parameters(p): Parameters<ClipCutParams>) -> Result<CallToolResult, McpError> {
        if !p.seconds.is_finite() || p.seconds < 0.0 {
            return Ok(fail("cut time must be a non-negative finite number of seconds"));
        }
        let cut_res = if let Some(clip) = p.clip {
            self.run("timeline.razor", json!({"clip": clip, "seconds": p.seconds})).await
        } else if let Some(ref tr) = p.track {
            self.run("timeline.razor", json!({"track": tr, "seconds": p.seconds})).await
        } else {
            self.run("sequence.addEditAllTracks", json!({"seconds": p.seconds})).await
        };
        let cut_val = match cut_res {
            Ok(v) => v,
            Err(e) => return Ok(fail(e)),
        };

        let seq_info = self.run("sequence.inspect", json!({})).await.unwrap_or_default();
        let rate = seq_info
            .get("settings")
            .and_then(|s| s.get("frame_rate"))
            .and_then(|r| serde_json::from_value::<FrameRate>(r.clone()).ok())
            .unwrap_or(FrameRate::FPS_23_976);
        let cut_tick = rate.snap(Tick::from_seconds_f64(p.seconds));

        let outgoing = active_clips_at(&seq_info, rate.tick_of((rate.frame_at(cut_tick) - 1).max(0)));
        let incoming = active_clips_at(&seq_info, cut_tick);

        let mut meta = json!({
            "cut_seconds": p.seconds,
            "cut_tick": cut_tick.0,
            "cuts_made": cut_val.get("cuts").and_then(Value::as_u64).unwrap_or(0),
            "left_clip": outgoing.first(),
            "right_clip": incoming.first(),
        });

        if p.verify.unwrap_or(false) {
            match self.inspect_cut_internal(p.seconds, Some(2), Some(2), Some(800)).await {
                Ok((png, vmeta)) => {
                    meta["verification"] = vmeta;
                    return Ok(image_with_json(&png, &meta));
                }
                Err(e) => {
                    meta["verification_error"] = json!(e.to_string());
                }
            }
        }
        Ok(ok_json(&meta))
    }

    #[tool(
        title = "Trim clip edge",
        description = "Adjust the In (head) or Out (tail) point of a clip by seconds or frames. When `ripple: true` (default), shifts downstream clips to close or open gaps. When `verify: true`, attaches visual inspection.",
        annotations(read_only_hint = false, destructive_hint = true, idempotent_hint = false, open_world_hint = false)
    )]
    async fn clip_trim(&self, Parameters(p): Parameters<ClipTrimParams>) -> Result<CallToolResult, McpError> {
        let edge = p.edge.to_ascii_lowercase();
        if edge != "in" && edge != "out" {
            return Ok(fail("edge must be 'in' or 'out'"));
        }
        let mode = if p.ripple.unwrap_or(true) { "ripple" } else { "regular" };
        let mut trim_params = json!({
            "clip": p.clip,
            "edge": edge,
            "mode": mode,
        });
        if let Some(df) = p.delta_frames {
            trim_params["deltaFrames"] = json!(df);
        } else if let Some(ds) = p.delta_seconds {
            if !ds.is_finite() {
                return Ok(fail("delta_seconds must be a finite number"));
            }
            trim_params["delta"] = json!(Tick::from_seconds_f64(ds).0);
        } else {
            return Ok(fail("need either delta_seconds or delta_frames"));
        }

        let res = match self.run("timeline.trim", trim_params).await {
            Ok(v) => v,
            Err(e) => return Ok(fail(e)),
        };

        let seq_info = self.run("sequence.inspect", json!({})).await.unwrap_or_default();
        let updated_clip = find_clip_in_seq(&seq_info, p.clip);

        let mut meta = json!({
            "clip": p.clip,
            "edge": edge,
            "ripple": p.ripple.unwrap_or(true),
            "trim_result": res,
            "updated_clip": updated_clip,
        });

        if p.verify.unwrap_or(false)
            && let Some(ref c) = updated_clip
        {
            let start = c.get("start").and_then(Value::as_i64).unwrap_or(0);
            let dur = c.get("duration").and_then(Value::as_i64).unwrap_or(0);
            let inspect_sec = if edge == "in" {
                Tick(start).as_seconds_f64()
            } else {
                Tick(start.saturating_add(dur)).as_seconds_f64()
            };
            match self.inspect_cut_internal(inspect_sec, Some(2), Some(2), Some(800)).await {
                Ok((png, vmeta)) => {
                    meta["verification"] = vmeta;
                    return Ok(image_with_json(&png, &meta));
                }
                Err(e) => {
                    meta["verification_error"] = json!(e.to_string());
                }
            }
        }
        Ok(ok_json(&meta))
    }

    #[tool(
        title = "Delete clip or gap",
        description = "Remove a clip or gap. When `ripple: true` (default), shifts downstream clips to close the gap. When `verify: true`, attaches visual inspection.",
        annotations(read_only_hint = false, destructive_hint = true, idempotent_hint = false, open_world_hint = false)
    )]
    async fn clip_delete(&self, Parameters(p): Parameters<ClipDeleteParams>) -> Result<CallToolResult, McpError> {
        let ripple = p.ripple.unwrap_or(true);
        let seq_before = self.run("sequence.inspect", json!({})).await.unwrap_or_default();

        let (inspect_sec, del_res) = if let Some(clip) = p.clip {
            let clip_info = find_clip_in_seq(&seq_before, clip);
            let start_sec = clip_info.as_ref()
                .and_then(|c| c.get("start").and_then(Value::as_i64))
                .map(|t| Tick(t).as_seconds_f64())
                .unwrap_or(0.0);
            let cmd = if ripple { "edit.rippleDelete" } else { "edit.clear" };
            let r = self.run(cmd, json!({"clip": clip})).await;
            (start_sec, r)
        } else if let Some(gap_at) = p.gap_at {
            if !gap_at.is_finite() || gap_at < 0.0 {
                return Ok(fail("gap_at must be a non-negative finite number of seconds"));
            }
            let track = p.track.unwrap_or_else(|| "V1".into());
            if let Err(e) = self.run("timeline.selectGap", json!({"seconds": gap_at, "track": track})).await {
                return Ok(fail(e));
            }
            let cmd = if ripple { "edit.rippleDelete" } else { "edit.clear" };
            let r = self.run(cmd, json!({})).await;
            (gap_at, r)
        } else {
            return Ok(fail("need either `clip` or `gap_at`"));
        };

        let del_val = match del_res {
            Ok(v) => v,
            Err(e) => return Ok(fail(e)),
        };

        let mut meta = json!({
            "deleted_clip": p.clip,
            "deleted_gap_at": p.gap_at,
            "ripple": ripple,
            "result": del_val,
        });

        if p.verify.unwrap_or(false) {
            match self.inspect_cut_internal(inspect_sec, Some(2), Some(2), Some(800)).await {
                Ok((png, vmeta)) => {
                    meta["verification"] = vmeta;
                    return Ok(image_with_json(&png, &meta));
                }
                Err(e) => {
                    meta["verification_error"] = json!(e.to_string());
                }
            }
        }
        Ok(ok_json(&meta))
    }

    #[tool(
        title = "Move or arrange clip",
        description = "Move a clip to `to_seconds` and optionally to a different track (e.g. \"V1\", \"V2\"). When `insert: true`, ripples downstream clips instead of overwriting.",
        annotations(read_only_hint = false, destructive_hint = true, idempotent_hint = false, open_world_hint = false)
    )]
    async fn clip_arrange(&self, Parameters(p): Parameters<ClipArrangeParams>) -> Result<CallToolResult, McpError> {
        if !p.to_seconds.is_finite() || p.to_seconds < 0.0 {
            return Ok(fail("destination time must be a non-negative finite number of seconds"));
        }
        let seq_before = self.run("sequence.inspect", json!({})).await.unwrap_or_default();
        let clip_info = match find_clip_in_seq(&seq_before, p.clip) {
            Some(c) => c,
            None => return Ok(fail(format!("clip {} not found in sequence", p.clip))),
        };
        let track_val = match p.to_track {
            Some(ref tr) => json!(tr),
            None => clip_info.get("track_id").cloned().unwrap_or_else(|| json!("V1")),
        };
        let to_tick = Tick::from_seconds_f64(p.to_seconds);
        let move_params = json!({
            "moves": [{
                "clip": p.clip,
                "track": track_val,
                "time": to_tick.0,
            }],
            "insert": p.insert.unwrap_or(false),
        });

        match self.run("timeline.move", move_params).await {
            Ok(res) => {
                let seq_after = self.run("sequence.inspect", json!({})).await.unwrap_or_default();
                let updated = find_clip_in_seq(&seq_after, p.clip);
                let meta = json!({
                    "clip": p.clip,
                    "to_seconds": p.to_seconds,
                    "to_tick": to_tick.0,
                    "result": res,
                    "updated_clip": updated,
                });
                Ok(ok_json(&meta))
            }
            Err(e) => Ok(fail(e)),
        }
    }
}

impl FilmcraftMcp {
    async fn screenshot(&self, b: &BridgeClient, panel: Option<String>, max: u32) -> Result<CallToolResult, McpError> {
        // The desktop bridge needs a pathname, so use a freshly-created,
        // owner-only directory rather than exposing image bytes at a
        // predictable shared-temp filename.
        let dir = match tempfile::tempdir() {
            Ok(dir) => dir,
            Err(e) => return Ok(fail(format!("screenshot: {e}"))),
        };
        let path = dir.path().join("screenshot.png");
        if let Err(e) = b.call("ui.screenshot", json!({"path": path.to_string_lossy(), "panel": panel})).await {
            return Ok(fail(e));
        }
        let bytes = match tokio::fs::read(&path).await {
            Ok(b) => b,
            Err(e) => return Ok(fail(format!("screenshot: {e}"))),
        };
        let bytes = match image::load_from_memory(&bytes) {
            Ok(img) => {
                let rgba = img.to_rgba8();
                let (w, h) = rgba.dimensions();
                png_rgba(w, h, rgba.into_raw(), max).unwrap_or(bytes)
            }
            Err(_) => bytes,
        };
        Ok(png_result(&bytes, "screenshot of the live FilmCraft window"))
    }

    /// Sample a list of frames at specific tick positions and labels.
    async fn sample_frames(&self, points: Vec<(Tick, Option<String>)>, max_side: u32) -> Result<Vec<SampledFrame>, AutomationError> {
        match &*self.backend {
            Backend::Headless(s) => {
                let s = s.clone();
                tokio::task::spawn_blocking(move || -> Result<Vec<SampledFrame>, AutomationError> {
                    let g = s.lock().unwrap_or_else(PoisonError::into_inner);
                    let (w, h) = g.active_sequence()
                        .map(|q| (q.settings.width, q.settings.height))
                        .ok_or_else(|| AutomationError::Other("no sequence".into()))?;
                    let scale = render_scale(max_side, w, h);
                    let mut frames = Vec::with_capacity(points.len());
                    for (tick, label) in points {
                        let img = g.try_render_program_at(scale, tick)
                            .map_err(|e| AutomationError::Other(e.to_string()))?;
                        let width = img.w as u32;
                        let height = img.h as u32;
                        let rgba = img.over_black_rgba8();
                        frames.push(SampledFrame {
                            seconds: tick.as_seconds_f64(),
                            label,
                            width,
                            height,
                            rgba,
                        });
                    }
                    Ok(frames)
                })
                .await
                .map_err(join_error)?
            }
            Backend::Bridge(b) => {
                let before = self.run("sequence.inspect", json!({})).await?;
                let mut frames = Vec::with_capacity(points.len());
                for (tick, label) in points {
                    if let Err(e) = self.run("playhead.set", json!({"time": tick.0})).await {
                        return Err(e);
                    }
                    let dir = tempfile::tempdir().map_err(|e| AutomationError::Other(format!("screenshot: {e}")))?;
                    let path = dir.path().join("screenshot.png");
                    if let Err(e) = b.call("ui.screenshot", json!({"path": path.to_string_lossy(), "panel": "Program"})).await {
                        return Err(AutomationError::Other(format!("screenshot: {e}")));
                    }
                    let bytes = tokio::fs::read(&path).await.map_err(|e| AutomationError::Other(format!("screenshot: {e}")))?;
                    let img = image::load_from_memory(&bytes).map_err(|e| AutomationError::Other(format!("screenshot: {e}")))?;
                    let rgba = img.to_rgba8();
                    let (w, h) = rgba.dimensions();
                    frames.push(SampledFrame {
                        seconds: tick.as_seconds_f64(),
                        label,
                        width,
                        height,
                        rgba: rgba.into_raw(),
                    });
                }
                if let Some(t) = before.get("playhead").and_then(Value::as_i64) {
                    let _ = self.run("playhead.set", json!({"time": t})).await;
                }
                let now = self.run("sequence.inspect", json!({})).await.ok();
                if let Some(sel) = before.get("selection").filter(|v| v.is_array())
                    && now.as_ref().and_then(|n| n.get("selection")) != Some(sel)
                {
                    let _ = self.run("timeline.select", json!({"clips": sel})).await;
                }
                Ok(frames)
            }
        }
    }

    async fn inspect_cut_internal(
        &self,
        seconds: f64,
        frames_before: Option<u32>,
        frames_after: Option<u32>,
        max_side: Option<u32>,
    ) -> Result<(Vec<u8>, Value), AutomationError> {
        let seq_info = self.run("sequence.inspect", json!({})).await?;
        let rate = seq_info
            .get("settings")
            .and_then(|s| s.get("frame_rate"))
            .and_then(|r| serde_json::from_value::<FrameRate>(r.clone()).ok())
            .unwrap_or(FrameRate::FPS_23_976);

        let cut_tick = rate.snap(Tick::from_seconds_f64(seconds));
        let cut_frame = rate.frame_at(cut_tick);

        let before_cnt = frames_before.unwrap_or(2).clamp(1, 8) as i64;
        let after_cnt = frames_after.unwrap_or(2).clamp(1, 8) as i64;

        let mut points: Vec<(Tick, Option<String>)> = Vec::new();
        // Outgoing frames before the cut
        for i in (1..=before_cnt).rev() {
            let f = (cut_frame - i).max(0);
            let t = rate.tick_of(f);
            points.push((t, Some(format!("T-{i}"))));
        }
        // Incoming frames at and after the cut
        for j in 0..after_cnt {
            let f = cut_frame + j;
            let t = rate.tick_of(f);
            let label = if j == 0 { "CUT".to_string() } else { format!("T+{j}") };
            points.push((t, Some(label)));
        }

        let max = max_side.unwrap_or(1200);
        let frames = self.sample_frames(points, max).await?;
        let png = stitch(&frames, SampleLayout::Strip, max)?;

        let outgoing = active_clips_at(&seq_info, rate.tick_of((cut_frame - 1).max(0)));
        let incoming = active_clips_at(&seq_info, rate.tick_of(cut_frame));

        let same_clip = outgoing.first().and_then(|o| o.get("clip")) == incoming.first().and_then(|i| i.get("clip"))
            && outgoing.first().is_some();
        let is_gap = outgoing.is_empty() || incoming.is_empty();

        let mut is_transition = false;
        if let Some(tracks) = seq_info.get("video").and_then(Value::as_array) {
            for track in tracks {
                if let Some(transitions) = track.get("transitions").and_then(Value::as_array) {
                    for tr in transitions {
                        let start = tr.get("start").and_then(Value::as_i64).unwrap_or(0);
                        let duration = tr.get("duration").and_then(Value::as_i64).unwrap_or(0);
                        if start <= cut_tick.0 && cut_tick.0 <= start.saturating_add(duration) {
                            is_transition = true;
                            break;
                        }
                    }
                }
            }
        }

        let meta = json!({
            "cut_seconds": cut_tick.as_seconds_f64(),
            "cut_tick": cut_tick.0,
            "cut_frame": cut_frame,
            "outgoing_clips": outgoing,
            "incoming_clips": incoming,
            "same_clip": same_clip,
            "is_gap": is_gap,
            "is_transition": is_transition,
            "frames_count": frames.len(),
        });

        Ok((png, meta))
    }
}

impl FilmcraftMcp {
    /// The first argument `tool` doesn't take, as a message naming the ones it does.
    fn unknown_arg(&self, tool: &str, args: Option<&rmcp::model::JsonObject>) -> Option<String> {
        let def = self.tool_router.get(tool)?;
        let props = def.input_schema.get("properties").and_then(Value::as_object).cloned().unwrap_or_default();
        let bad = args?.keys().find(|k| !props.contains_key(*k))?;
        let accepted: Vec<&str> = props.keys().map(String::as_str).collect();
        let accepted = if accepted.is_empty() { "no arguments".to_string() } else { accepted.join(", ") };
        Some(format!("unknown argument \"{bad}\" for {tool}; expected: {accepted}"))
    }
}

const INSTRUCTIONS: &str = "FilmCraft video editor (Premiere Pro-class). Every edit is an engine command: `command_list` to discover ids/params, `command_run` to execute (undoable; `command_batch` runs several). `doc_inspect` (or `project_inspect`/`sequence_inspect`) returns ids you can pass to commands; `render_preview` shows the result. For autonomous editing loops: `timeline_sample` renders multi-frame contact sheets, `cut_inspect` verifies A/B cut continuity, and `clip_cut`, `clip_trim`, `clip_delete`, and `clip_arrange` perform timeline edits with optional instant visual verification (`verify: true`). In bridge mode the `ui_*` tools drive the live app: `ui_elements` lists clickable ids, `ui_click`/`ui_drag`/`ui_key` operate it, `ui_screenshot` shows it. Time is in ticks: 254016000000 per second (commands also accept `seconds`, `frame` or `timecode`).";

/// Resources: the project (as `doc_inspect`) and the command catalog (as `command_list`).
const DOCUMENT_URI: &str = "filmcraft://document";
const COMMANDS_URI: &str = "filmcraft://commands";

/// MCP 2026-07-28 clients (current Claude Code) require `ttlMs` and `cacheScope` on list and read
/// results; rmcp fills `resultType` and leaves the cache hints to the server, as the generated
/// `tools/list` does. The resource list never changes; the document does, so reads aren't cached.
fn cache_hints(context: &rmcp::service::RequestContext<rmcp::RoleServer>, ttl_ms: u64) -> Option<(u64, rmcp::model::CacheScope)> {
    context.protocol_version().is_some_and(|v| v >= rmcp::model::ProtocolVersion::V_2026_07_28).then_some((ttl_ms, rmcp::model::CacheScope::Private))
}

#[tool_handler(router = self.tool_router)]
impl ServerHandler for FilmcraftMcp {
    fn get_info(&self) -> rmcp::model::ServerConfig {
        rmcp::model::ServerConfig::new(rmcp::model::ServerCapabilities::builder().enable_tools().enable_resources().build())
            .with_server_info(rmcp::model::Implementation::new("filmcraft", env!("CARGO_PKG_VERSION")))
            .with_instructions(INSTRUCTIONS.to_string())
    }

    /// Strict arguments and long exports in front of the tool router.
    async fn call_tool(
        &self,
        request: rmcp::model::CallToolRequestParams,
        context: rmcp::service::RequestContext<rmcp::RoleServer>,
    ) -> Result<rmcp::model::CallToolResponse, McpError> {
        if let Some(m) = self.unknown_arg(&request.name, request.arguments.as_ref()) {
            return Err(McpError::invalid_params(m, None));
        }
        // A blocking export reports progress and can be cancelled (docs/agents.md § Long exports).
        // Bridge mode too: the app runs it as a job, so it stays responsive (#91, #92).
        if request.name == "command_run"
            && let Some(a) = &request.arguments
            && let Some(id) = a.get("id").and_then(Value::as_str)
        {
            let params = a.get("params").cloned().filter(Value::is_object).unwrap_or_else(|| json!({}));
            if crate::long_job::is_long(id, &params) {
                return Ok(self.run_long(id, params, &context).await.into());
            }
        }
        let tcc = rmcp::handler::server::tool::ToolCallContext::new(self, request, context);
        self.tool_router.call(tcc).await
    }

    async fn list_resources(
        &self,
        _request: Option<rmcp::model::PaginatedRequestParams>,
        context: rmcp::service::RequestContext<rmcp::RoleServer>,
    ) -> Result<rmcp::model::ListResourcesResult, McpError> {
        use rmcp::model::Resource;
        let mut r = rmcp::model::ListResourcesResult::with_all_items(vec![
            Resource::new(DOCUMENT_URI, "document")
                .with_title("Project")
                .with_description("The project tree and the active sequence (same as doc_inspect).")
                .with_mime_type("application/json"),
            Resource::new(COMMANDS_URI, "commands")
                .with_title("Command catalog")
                .with_description("Every command with id, label, menu path, shortcut, params and enabled state (same as command_list).")
                .with_mime_type("application/json"),
        ]);
        if let Some((ttl, scope)) = cache_hints(&context, 600_000) {
            r = r.with_ttl_ms(ttl).with_cache_scope(scope);
        }
        Ok(r)
    }

    async fn read_resource(
        &self,
        request: rmcp::model::ReadResourceRequestParams,
        context: rmcp::service::RequestContext<rmcp::RoleServer>,
    ) -> Result<rmcp::model::ReadResourceResponse, McpError> {
        let v = match request.uri.as_str() {
            DOCUMENT_URI => self.document().await,
            COMMANDS_URI => self.run("command.list", json!({})).await,
            other => return Err(McpError::resource_not_found(format!("resource not found: {other}"), None)),
        }
        .map_err(|e| McpError::internal_error(e.to_string(), None))?;
        let text = serde_json::to_string_pretty(&v).map_err(|e| McpError::internal_error(e.to_string(), None))?;
        let mut r = rmcp::model::ReadResourceResult::new(vec![rmcp::model::ResourceContents::text(text, request.uri).with_mime_type("application/json")]);
        if let Some((ttl, scope)) = cache_hints(&context, 0) {
            r = r.with_ttl_ms(ttl).with_cache_scope(scope);
        }
        Ok(r.into())
    }
}

/// Render scale that fits the longest side of a `w` x `h` sequence into `max_side` (never upscales; 0 = no limit, as in `png_rgba`).
fn render_scale(max_side: u32, w: u32, h: u32) -> f32 {
    if max_side == 0 {
        return 1.0;
    }
    (max_side as f32 / w.max(h).max(1) as f32).min(1.0)
}

#[cfg(test)]
mod tests;
