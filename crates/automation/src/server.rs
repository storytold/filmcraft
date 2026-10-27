//! The MCP server. Tool names are snake_case.

use std::sync::{Arc, Mutex, PoisonError};

use base64::Engine as _;
use filmcraft_engine::Session;
use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{CallToolResult, ContentBlock as Content};
use rmcp::{ErrorData as McpError, ServerHandler, ServiceExt, tool, tool_handler, tool_router};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{Value, json};

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

fn ok_json(v: &Value) -> CallToolResult {
    CallToolResult::success(vec![Content::text(serde_json::to_string_pretty(v).unwrap_or_default())])
}
fn fail(e: impl std::fmt::Display) -> CallToolResult {
    CallToolResult::error(vec![Content::text(e.to_string())])
}
fn png_result(png: &[u8], note: &str) -> CallToolResult {
    CallToolResult::success(vec![Content::image(base64::engine::general_purpose::STANDARD.encode(png), "image/png"), Content::text(note.to_string())])
}
fn wrap(r: Result<Value, AutomationError>) -> Result<CallToolResult, McpError> {
    Ok(match r {
        Ok(v) => ok_json(&v),
        Err(e) => fail(e),
    })
}

impl FilmcraftMcp {
    pub fn headless(session: Session) -> Self {
        Self { backend: Arc::new(Backend::Headless(Arc::new(Mutex::new(session)))), tool_router: Self::tool_router() }
    }
    pub fn bridge(addr: &str) -> Result<Self, AutomationError> {
        Ok(Self { backend: Arc::new(Backend::Bridge(Arc::new(BridgeClient::new(addr)?))), tool_router: Self::tool_router() })
    }

    pub async fn serve_stdio(self) -> Result<(), AutomationError> {
        self.serve_io(tokio::io::stdin(), tokio::io::stdout()).await
    }

    /// Serve MCP over any line stream. A line that isn't JSON gets a JSON-RPC parse error
    /// (`-32700`, id null) instead of being dropped silently; the rest go to the rmcp service. One
    /// writer task owns `output`, so replies never interleave.
    pub async fn serve_io(
        self,
        input: impl tokio::io::AsyncRead + Unpin + Send + 'static,
        output: impl tokio::io::AsyncWrite + Unpin + Send + 'static,
    ) -> Result<(), AutomationError> {
        use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
        let (mut to_service, service_in) = tokio::io::duplex(1 << 20);
        let (service_out, from_service) = tokio::io::duplex(1 << 20);
        let (err_tx, mut err_rx) = tokio::sync::mpsc::unbounded_channel::<String>();
        let reader = tokio::spawn(async move {
            let mut lines = BufReader::new(input).lines();
            while let Ok(Some(line)) = lines.next_line().await {
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
                        let _ = err_tx.send(reply.to_string());
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

const INSTRUCTIONS: &str = "FilmCraft video editor (Premiere Pro-class). Every edit is an engine command: `command_list` to discover ids/params, `command_run` to execute (undoable; `command_batch` runs several). `doc_inspect` (or `project_inspect`/`sequence_inspect`) returns ids you can pass to commands; `render_preview` shows the result. In bridge mode the `ui_*` tools drive the live app: `ui_elements` lists clickable ids, `ui_click`/`ui_drag`/`ui_key` operate it, `ui_screenshot` shows it. Time is in ticks: 254016000000 per second (commands also accept `seconds`, `frame` or `timecode`).";

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
