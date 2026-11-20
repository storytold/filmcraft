//! Embedded MCP over Streamable HTTP. The editor is the only owner of project state.

use std::convert::Infallible;
use std::fmt::Write as _;
use std::io::{self, BufRead, BufReader, Read, Write as _};
use std::net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream, UdpSocket};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, mpsc::SyncSender};
use std::time::Duration;

use http_body_util::{BodyExt, Full, Limited, combinators::BoxBody};
use hyper::body::{Bytes, Incoming};
use hyper::header::{AUTHORIZATION, CONTENT_TYPE, WWW_AUTHENTICATE};
use hyper::{Request, Response, StatusCode, server::conn::http1, service::service_fn};
use hyper_util::rt::{TokioIo, TokioTimer};
use rmcp::handler::server::{router::tool::ToolRouter, wrapper::Parameters};
use rmcp::model::{CallToolResult, Implementation, InitializeRequestParams, InitializeResult, ServerCapabilities, ServerConfig};
use rmcp::service::{RequestContext, RoleServer};
use rmcp::transport::streamable_http_server::{StreamableHttpServerConfig, StreamableHttpService, session::local::LocalSessionManager};
use rmcp::{ErrorData, ServerHandler, tool, tool_handler, tool_router};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{Value, json};
use subtle::ConstantTimeEq;
use tokio::sync::oneshot;

const MAX_REQUEST: usize = 64 * 1024;
const MAX_CONNECTIONS: usize = 8;
const DEADLINE: Duration = Duration::from_secs(5);
type HttpResponse = Response<BoxBody<Bytes, Infallible>>;
type McpService = StreamableHttpService<EditorMcp, LocalSessionManager>;

pub(super) struct EditorRequest {
    pub command: EditorCommand,
    pub reply: oneshot::Sender<Result<Value, String>>,
}

pub(super) enum EditorCommand {
    Inspect,
    ReviewStatus,
    Propose(Value),
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Client {
    Antigravity,
    Codex,
    ClaudeCode,
}

impl Client {
    pub const ALL: [Self; 3] = [Self::Antigravity, Self::Codex, Self::ClaudeCode];

    pub fn label(self) -> &'static str {
        match self {
            Self::Antigravity => "Antigravity",
            Self::Codex => "Codex",
            Self::ClaudeCode => "Claude Code",
        }
    }

    pub fn location(self) -> &'static str {
        match self {
            Self::Antigravity => "Antigravity → MCP Servers → Manage MCP Servers → View raw config",
            Self::Codex => "Codex → config.toml",
            Self::ClaudeCode => "Claude Code → project .mcp.json",
        }
    }

    pub fn uses_json(self) -> bool {
        self != Self::Codex
    }

    fn config(self, url: &str, token: &str, entry_only: bool) -> String {
        let authorization = format!("Bearer {token}");
        if self == Self::Codex {
            return format!("[mcp_servers.filmcraft]\nurl = \"{url}\"\nhttp_headers = {{ Authorization = \"{authorization}\" }}\n");
        }
        let mut entry = json!({"headers": {"Authorization": authorization}});
        if self == Self::Antigravity {
            entry["serverUrl"] = json!(url);
        } else {
            entry["type"] = json!("http");
            entry["url"] = json!(url);
        }
        if entry_only {
            return format!("\"filmcraft\": {}", serde_json::to_string_pretty(&entry).unwrap_or_default());
        }
        serde_json::to_string_pretty(&json!({"mcpServers": {"filmcraft": entry}})).unwrap_or_default()
    }
}

pub(super) fn private_address() -> Option<Ipv4Addr> {
    // Connecting a UDP socket chooses a route without sending a packet.
    let socket = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0)).ok()?;
    socket.connect((Ipv4Addr::new(192, 0, 2, 1), 9)).ok()?;
    match socket.local_addr().ok()? {
        SocketAddr::V4(address) if address.ip().is_private() => Some(*address.ip()),
        _ => None,
    }
}

pub(super) struct Server {
    pub address: SocketAddr,
    token: String,
    stop: oneshot::Sender<()>,
    status: Arc<Status>,
}

#[derive(Default)]
struct Status {
    initialized: AtomicBool,
    rejected_auth: AtomicU64,
}

pub(super) fn generate_token() -> io::Result<String> {
    let mut random = [0; 32];
    getrandom::fill(&mut random).map_err(|err| io::Error::other(err.to_string()))?;
    let mut token = String::with_capacity(64);
    for byte in random {
        let _ = write!(token, "{byte:02x}");
    }
    Ok(token)
}

impl Server {
    pub fn start(ip: Ipv4Addr, saved: Option<String>, editor: SyncSender<EditorRequest>, wake_editor: impl Fn() + Send + Sync + 'static) -> io::Result<Self> {
        if ip != Ipv4Addr::LOCALHOST && !ip.is_private() {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "Choose loopback or a private IPv4 address on this device."));
        }
        if saved.is_some() && ip != Ipv4Addr::LOCALHOST {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "Saved pairing requires loopback."));
        }
        let port = if saved.is_some() { crate::pairing::LOCAL_PORT } else { 0 };
        let listener = TcpListener::bind((ip, port))?;
        listener.set_nonblocking(true)?;
        let address = listener.local_addr()?;
        let token = match saved {
            Some(token) => token,
            None => generate_token()?,
        };
        let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
        let (stop, stopped) = oneshot::channel();
        let status = Arc::new(Status::default());
        let worker_status = status.clone();
        let auth = format!("Bearer {token}");
        let server = EditorMcp::new(editor, Arc::new(wake_editor), status.clone());
        std::thread::Builder::new().name("filmcraft-mcp".into()).spawn(move || {
            let result =
                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| runtime.block_on(listen(listener, address, &auth, server, worker_status, stopped))));
            match result {
                Ok(Ok(())) => {}
                Ok(Err(err)) => log::warn!("MCP server stopped: {err}"),
                Err(_) => log::warn!("MCP server stopped unexpectedly"),
            }
        })?;
        Ok(Self { address, token, stop, status })
    }

    pub fn running(&self) -> bool {
        !self.stop.is_closed()
    }

    pub fn contacted(&self) -> bool {
        self.status.initialized.load(Ordering::Relaxed)
    }

    pub fn rejected_auth(&self) -> u64 {
        self.status.rejected_auth.load(Ordering::Relaxed)
    }

    /// Tests this app's listener and credential without creating a client MCP session.
    pub fn connection_probe(&self) -> impl FnOnce() -> io::Result<()> + use<> {
        let address = self.address;
        let token = self.token.clone();
        move || {
            let timeout = Duration::from_secs(2);
            let mut stream = TcpStream::connect_timeout(&address, timeout)?;
            stream.set_read_timeout(Some(timeout))?;
            stream.set_write_timeout(Some(timeout))?;
            write!(stream, "GET /health HTTP/1.1\r\nHost: {address}\r\nAuthorization: Bearer {token}\r\nConnection: close\r\n\r\n")?;
            let mut status = String::new();
            BufReader::new(stream.take(256)).read_line(&mut status)?;
            match status.split_whitespace().nth(1) {
                Some("204") => Ok(()),
                Some("401") => Err(io::Error::other("The listener rejected FilmCraft’s token.")),
                _ => Err(io::Error::other("The listener did not return a FilmCraft health response.")),
            }
        }
    }

    pub fn url(&self) -> String {
        format!("http://{}/mcp", self.address)
    }

    pub fn config(&self, client: Client, entry_only: bool, show_token: bool) -> String {
        let token = if show_token { &self.token } else { "TOKEN_HIDDEN" };
        client.config(&self.url(), token, entry_only)
    }
}

async fn listen(
    listener: TcpListener,
    address: SocketAddr,
    auth: &str,
    editor: EditorMcp,
    status: Arc<Status>,
    mut stopped: oneshot::Receiver<()>,
) -> io::Result<()> {
    let listener = tokio::net::TcpListener::from_std(listener)?;
    let config = StreamableHttpServerConfig::default()
        .with_legacy_session_mode(false)
        .with_json_response(true)
        .with_allowed_hosts([address.to_string()])
        .with_allowed_origins([format!("http://{address}")]);
    let wake_editor = editor.wake_editor.clone();
    let mcp = McpService::new(move || Ok(editor.clone()), Arc::new(LocalSessionManager::default()), config);
    let auth: Arc<[u8]> = Arc::from(auth.as_bytes());
    let mut connections = tokio::task::JoinSet::new();
    loop {
        tokio::select! {
            _ = &mut stopped => return Ok(()),
            _ = connections.join_next(), if !connections.is_empty() => {},
            accepted = listener.accept(), if connections.len() < MAX_CONNECTIONS => {
                let (stream, peer) = accepted?;
                if !peer.ip().is_loopback() && !matches!(peer.ip(), std::net::IpAddr::V4(ip) if ip.is_private()) {
                    continue;
                }
                let auth = auth.clone();
                let mcp = mcp.clone();
                let status = status.clone();
                let wake_editor = wake_editor.clone();
                connections.spawn(async move {
                    let service = service_fn(|request| async {
                        let response = respond(request, &auth, &mcp, address).await;
                        if response.status() == StatusCode::UNAUTHORIZED {
                            status.rejected_auth.fetch_add(1, Ordering::Relaxed);
                            wake_editor();
                        }
                        Ok::<_, Infallible>(response)
                    });
                    let connection = http1::Builder::new().timer(TokioTimer::new()).header_read_timeout(DEADLINE)
                        .keep_alive(false).serve_connection(TokioIo::new(stream), service);
                    let _ = tokio::time::timeout(DEADLINE * 3, connection).await;
                });
            }
        }
    }
}

async fn respond(request: Request<Incoming>, auth: &[u8], mcp: &McpService, address: SocketAddr) -> HttpResponse {
    let probe = request.uri().path() == "/health" && request.method() == hyper::Method::GET;
    if !probe && request.uri().path() != "/mcp" {
        return http_error(StatusCode::NOT_FOUND, "Use the /mcp endpoint.");
    }
    let headers = request.headers();
    let mut authorizations = headers.get_all(AUTHORIZATION).iter();
    if !authorizations.next().is_some_and(|header| bool::from(header.as_bytes().ct_eq(auth))) || authorizations.next().is_some() {
        let mut response = http_error(StatusCode::UNAUTHORIZED, "Set the Authorization header from FilmCraft’s connection settings.");
        response.headers_mut().insert(WWW_AUTHENTICATE, hyper::header::HeaderValue::from_static("Bearer realm=\"FilmCraft\""));
        return response;
    }
    if probe {
        let host = headers.get(hyper::header::HOST).and_then(|host| host.to_str().ok());
        if host != Some(address.to_string().as_str()) || headers.contains_key(hyper::header::ORIGIN) {
            return http_error(StatusCode::FORBIDDEN, "Use FilmCraft’s connection address.");
        }
        return http_error(StatusCode::NO_CONTENT, "");
    }
    let (parts, body) = request.into_parts();
    let body = match tokio::time::timeout(DEADLINE, Limited::new(body, MAX_REQUEST).collect()).await {
        Ok(Ok(body)) => body.to_bytes(),
        Ok(Err(err)) if err.is::<http_body_util::LengthLimitError>() => {
            return http_error(StatusCode::PAYLOAD_TOO_LARGE, "The MCP request is too large.");
        }
        Ok(Err(_)) => return http_error(StatusCode::BAD_REQUEST, "Could not read MCP request."),
        Err(_) => return http_error(StatusCode::REQUEST_TIMEOUT, "The MCP request timed out."),
    };
    mcp.handle(Request::from_parts(parts, Full::new(body))).await
}

fn http_error(status: StatusCode, message: &'static str) -> HttpResponse {
    let mut response = Response::new(Full::new(Bytes::from_static(message.as_bytes())).boxed());
    *response.status_mut() = status;
    response.headers_mut().insert(CONTENT_TYPE, hyper::header::HeaderValue::from_static("text/plain; charset=utf-8"));
    response
}

#[derive(Clone)]
struct EditorMcp {
    editor: SyncSender<EditorRequest>,
    wake_editor: Arc<dyn Fn() + Send + Sync>,
    tools: ToolRouter<Self>,
    status: Arc<Status>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct Revision {
    document: u64,
    sequence: u64,
    revision: u64,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct Proposal {
    revision: Revision,
    #[schemars(length(min = 1, max = 12))]
    edits: Vec<Edit>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
enum Edit {
    Split {
        clip: u64,
        at_seconds: f64,
    },
    Trim {
        clip: u64,
        edge: Edge,
        frames: i64,
        #[serde(default)]
        mode: TrimMode,
    },
    Move {
        clip: u64,
        at_seconds: f64,
        track: String,
    },
    Delete {
        clip: u64,
    },
    RippleDelete {
        clip: u64,
    },
    Title {
        at_seconds: f64,
        text: String,
        duration_seconds: Option<f64>,
    },
    Caption {
        at_seconds: f64,
        text: String,
        duration_seconds: Option<f64>,
    },
    Effect {
        clip: u64,
        effect: String,
    },
    Transition {
        clip: u64,
        effect: String,
        #[serde(default)]
        edge: Edge,
        frames: Option<u64>,
    },
    Gain {
        clip: u64,
        db: f64,
    },
}

#[derive(Debug, Default, Deserialize, JsonSchema, serde::Serialize)]
#[serde(rename_all = "snake_case")]
enum Edge {
    #[default]
    In,
    Out,
}

#[derive(Debug, Default, Deserialize, JsonSchema, serde::Serialize)]
#[serde(rename_all = "snake_case")]
enum TrimMode {
    #[default]
    Regular,
    Ripple,
}

impl Edit {
    fn engine_step(self) -> Result<Value, ErrorData> {
        let time = |seconds: f64| {
            if !(0.0..=86_400.0).contains(&seconds) {
                return Err(ErrorData::invalid_params("Timeline time must be between 0 and 86400 seconds.", None));
            }
            Ok((seconds * filmcraft_time::TICKS_PER_SECOND as f64).round() as i64)
        };
        let (id, params) = match self {
            Self::Split { clip, at_seconds } => ("timeline.razor", json!({"clip":clip, "time":time(at_seconds)?})),
            Self::Trim { clip, edge, frames, mode } => ("timeline.trim", json!({"clip":clip, "edge":edge, "deltaFrames":frames, "mode":mode})),
            Self::Move { clip, at_seconds, track } => ("timeline.move", json!({"clip":clip, "time":time(at_seconds)?, "track":track})),
            Self::Delete { clip } => ("timeline.delete", json!({"clip":clip, "ripple":false})),
            Self::RippleDelete { clip } => ("timeline.delete", json!({"clip":clip, "ripple":true})),
            Self::Title { at_seconds, text, duration_seconds } => {
                ("graphics.newText", json!({"time":time(at_seconds)?, "text":text, "durationSeconds":duration_seconds.unwrap_or(3.0)}))
            }
            Self::Caption { at_seconds, text, duration_seconds } => {
                ("captions.add", json!({"time":time(at_seconds)?, "text":text, "durationSeconds":duration_seconds.unwrap_or(3.0)}))
            }
            Self::Effect { clip, effect } => ("effects.apply", json!({"clip":clip, "effect":effect})),
            Self::Transition { clip, effect, edge, frames } => {
                ("sequence.applyVideoTransition", json!({"clip":clip, "effect":effect, "edge":edge, "frames":frames.unwrap_or(12)}))
            }
            Self::Gain { clip, db } => ("clip.audioGain", json!({"clip":clip, "db":db})),
        };
        Ok(json!({"id":id, "params":params}))
    }
}

#[tool_router(router = tools)]
impl EditorMcp {
    fn new(editor: SyncSender<EditorRequest>, wake_editor: Arc<dyn Fn() + Send + Sync>, status: Arc<Status>) -> Self {
        Self { editor, wake_editor, tools: Self::tools(), status }
    }

    fn mark_client_ready(&self) {
        if !self.status.initialized.swap(true, Ordering::Relaxed) {
            (self.wake_editor)();
        }
    }

    async fn call(&self, command: EditorCommand) -> Result<CallToolResult, ErrorData> {
        self.mark_client_ready();
        let (reply, response) = oneshot::channel();
        self.editor.try_send(EditorRequest { command, reply }).map_err(|_| ErrorData::internal_error("The editor is busy or disconnected.", None))?;
        (self.wake_editor)();
        match tokio::time::timeout(DEADLINE, response).await {
            Ok(Ok(Ok(result))) => Ok(CallToolResult::structured(result)),
            Ok(Ok(Err(err))) => Ok(CallToolResult::error(vec![rmcp::model::ContentBlock::text(err)])),
            _ => Err(ErrorData::internal_error("FilmCraft did not respond. Keep the editor open.", None)),
        }
    }

    #[tool(description = "Inspect the open project, active timeline, clip IDs and revision. Start here before proposing edits.")]
    async fn filmcraft_inspect(&self) -> Result<CallToolResult, ErrorData> {
        self.call(EditorCommand::Inspect).await
    }

    #[tool(description = "Get the status of edits submitted for review in FilmCraft.")]
    async fn filmcraft_review_status(&self) -> Result<CallToolResult, ErrorData> {
        self.call(EditorCommand::ReviewStatus).await
    }

    #[tool(
        description = "Propose 1–12 edits for approval in FilmCraft. Copy the revision object from filmcraft_inspect and use timeline seconds. Delete and ripple_delete require clip IDs. Delete leaves a gap. Ripple delete closes the gap where possible. Effects: gaussian_blur, light_leaks, sharpen, tint. Transitions: cross_dissolve, dip_to_black, dip_to_white."
    )]
    async fn filmcraft_propose(&self, Parameters(proposal): Parameters<Proposal>) -> Result<CallToolResult, ErrorData> {
        if !(1..=12).contains(&proposal.edits.len()) {
            return Err(ErrorData::invalid_params("Provide 1 to 12 edits.", None));
        }
        let steps = proposal.edits.into_iter().map(Edit::engine_step).collect::<Result<Vec<_>, _>>()?;
        let Revision { document, sequence, revision } = proposal.revision;
        self.call(EditorCommand::Propose(json!({"document":document, "sequence":sequence, "revision":revision, "steps":steps}))).await
    }
}

#[tool_handler(router = self.tools)]
impl ServerHandler for EditorMcp {
    async fn initialize(&self, request: InitializeRequestParams, context: RequestContext<RoleServer>) -> Result<InitializeResult, ErrorData> {
        context.peer.set_peer_info(request.clone());
        let result = self.negotiate_initialize(&request)?;
        // Streamable HTTP in stateless mode consumes the later initialized notification.
        self.mark_client_ready();
        Ok(result)
    }

    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new("filmcraft", env!("CARGO_PKG_VERSION")))
            .with_instructions("Inspect the current timeline, then propose edits. FilmCraft applies proposals only after the user approves them.")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn delete_actions_send_only_an_explicit_clip_and_ripple_mode() {
        for (action, ripple) in [("delete", false), ("ripple_delete", true)] {
            let edit: Edit = serde_json::from_value(json!({"action": action, "clip": 7})).unwrap();
            assert_eq!(edit.engine_step().unwrap(), json!({"id": "timeline.delete", "params": {"clip": 7, "ripple": ripple}}));
            for invalid in [
                json!({"action": action}),
                json!({"action": action, "clip": null}),
                json!({"action": action, "clip": -7}),
                json!({"action": action, "clip": 7, "clips": [8]}),
                json!({"action": action, "clip": 7, "ripple": !ripple}),
            ] {
                assert!(serde_json::from_value::<Edit>(invalid).is_err());
            }
        }
    }
}
