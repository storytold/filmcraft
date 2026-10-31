//! Errors of the CLI (RFC-0012): usage errors exit 2, failures exit 1.

use std::path::PathBuf;

use filmcraft_automation::AutomationError;
use filmcraft_engine::EngineError;
use thiserror::Error;

/// A command rejected by the engine or the app it is bridged to.
#[derive(Debug, Error)]
pub enum ExecError {
    #[error(transparent)]
    Engine(#[from] EngineError),
    #[error(transparent)]
    Bridge(#[from] AutomationError),
}

/// What is wrong with the `key=value` / JSON parameters of `exec`.
#[derive(Debug, Error)]
pub enum ParamsError {
    #[error("expected key=value, got `{0}`")]
    ExpectedKeyValue(String),
    #[error("params JSON: {0}")]
    Json(#[from] serde_json::Error),
    #[error("params JSON must be the only parameter argument, got `{0}` as well")]
    JsonWithOthers(String),
    #[error("parameter key exceeds {0} nesting levels")]
    TooDeep(usize),
}

/// A line of a `run` script that is not a command.
#[derive(Debug, Error)]
pub enum ScriptLineError {
    #[error("{0}")]
    Json(#[from] serde_json::Error),
    #[error("missing \"id\"")]
    MissingId,
}

/// The command line was wrong in a way the parser could not see: exit status 2.
#[derive(Debug, Error)]
pub enum UsageError {
    #[error(transparent)]
    Params(#[from] ParamsError),
    #[error("line {line}: {reason}")]
    ScriptLine { line: usize, reason: ScriptLineError },
    #[error("{0} is headless; with --bridge use the MCP `render_frame` tool or `exec` ui commands")]
    HeadlessOnly(&'static str),
    #[error("export: give --preset or --format (unknown extension)")]
    UnknownExportFormat,
    #[error("mcp: use --bridge ADDR to drive a running app")]
    McpNeedsBridge,
    /// Global options that exclude each other, given on both sides of the subcommand (where the
    /// parser's own conflict check does not see them).
    #[error("the argument '{0}' cannot be used with '{1}'")]
    Conflict(&'static str, &'static str),
    #[error("--bridge: {0}")]
    Bridge(#[source] AutomationError),
}

impl UsageError {
    /// This error as one of clap's own, so it prints and exits like them (RFC-0012).
    pub fn into_clap(self, cmd: &mut clap::Command) -> clap::error::Error {
        let kind = match self {
            UsageError::Conflict(..) => clap::error::ErrorKind::ArgumentConflict,
            _ => clap::error::ErrorKind::ValueValidation,
        };
        cmd.error(kind, self)
    }
}

/// A command that ran and failed: exit status 1.
#[derive(Debug, Error)]
pub enum Failure {
    #[error("{id}: {source}")]
    Exec { id: String, source: ExecError },
    #[error("{0}")]
    Backend(#[from] ExecError),
    #[error("saving: {0}")]
    Save(#[source] ExecError),
    #[error("{}: {source}", path.display())]
    Io { path: PathBuf, source: std::io::Error },
    #[error("can't read the script from stdin: {0}")]
    Stdin(#[source] std::io::Error),
    #[error("unknown command `{0}` (try `filmcraft-cli commands {0}`)")]
    UnknownCommand(String),
    #[error("import: {0}")]
    Import(#[source] ExecError),
    #[error("export: {0}")]
    Export(#[source] ExecError),
    #[error("export queue: {0}")]
    Queue(String),
    #[error("{0}")]
    Probe(#[from] crate::probe::ProbeError),
    #[error("render: {0}")]
    Render(#[source] Box<dyn std::error::Error + Send + Sync>),
    #[error("{0}")]
    Media(#[source] Box<dyn std::error::Error + Send + Sync>),
    #[error("can't encode {path}: {source}")]
    Png { path: String, source: AutomationError },
    #[error("{what} has placeholder frames for offline media; relink it (exec media.relink …) or pass --allow-offline")]
    Offline { what: String },
    #[error("line {line}: {id}: {source}")]
    Script { line: usize, id: String, source: ExecError },
    #[error("{count} line(s) of the script failed")]
    ScriptFailed { count: usize },
    #[error("mcp: {0}")]
    Mcp(#[source] AutomationError),
    #[error("can't write to stdout: {0}")]
    Stdout(#[source] std::io::Error),
    #[error("can't start the async runtime: {0}")]
    Runtime(#[source] std::io::Error),
}

/// Everything a subcommand can end in besides success.
#[derive(Debug, Error)]
pub enum CliError {
    #[error(transparent)]
    Usage(#[from] UsageError),
    #[error(transparent)]
    Failure(#[from] Failure),
}

impl CliError {
    pub fn exit_code(&self) -> u8 {
        match self {
            CliError::Usage(_) => 2,
            CliError::Failure(_) => 1,
        }
    }
}

impl From<ParamsError> for CliError {
    fn from(e: ParamsError) -> Self {
        CliError::Usage(e.into())
    }
}
