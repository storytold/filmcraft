//! The command line as clap derive types (RFC-0012): `--help` is these doc comments, and the
//! values reach the commands already typed.

use std::path::PathBuf;

use std::ffi::OsString;

use clap::builder::PathBufValueParser;
use clap::{Args, CommandFactory, FromArgMatches, Parser, Subcommand, ValueEnum};
use filmcraft_automation::BridgeClient;
use serde_json::{Map, Value};
use thiserror::Error;

use crate::error::{ParamsError, UsageError};

/// Keep nested CLI params safely within the depth also accepted by JSON serialization.
const MAX_PARAM_NESTING: usize = 64;

/// Drive FilmCraft from the shell: the headless engine, or the running app with --bridge.
///
/// Every engine command is one `exec` away, for scripts and AI agents. Values of `key=value`
/// parameters are read as JSON when they parse (3.5, true, [1,2], {"a":1}) and as strings
/// otherwise: `exec timeline.razor seconds=3.5`, `exec file.newBin name=Selects`. Times are
/// 254016000000 ticks per second; most commands also take seconds=, frame= or timecode=.
#[derive(Parser, Debug)]
#[command(name = "filmcraft-cli", version, propagate_version = true, after_help = EXIT_STATUS)]
pub struct Cli {
    #[command(flatten)]
    pub global: Global,
    #[command(subcommand)]
    pub command: Command,
}

const EXIT_STATUS: &str = "\
Exit status:
  0  success
  1  a command failed, or export/render drew offline media without --allow-offline
  2  usage error";

/// Parse the command line (program name first). A bare `--help` or `-h` is help even after a
/// mistake on the same line (`export o.wav --quality 101 --help`): clap would report the mistake.
/// Options that exclude each other on both sides of the subcommand are checked on the parsed
/// values, where clap does not see them, and reported as clap's own errors.
/// The global options that take a value in the next word.
const VALUE_GLOBALS: [&str; 4] = ["--project", "--save-as", "--bridge", "--data-dir"];

pub fn parse(args: &[OsString]) -> Result<Cli, clap::Error> {
    let mut cmd = Cli::command();
    let matches = match cmd.try_get_matches_from_mut(args) {
        Err(e) if args.iter().skip(1).take_while(|a| *a != "--").any(|a| a == "--help" || a == "-h") => {
            // the first word that names a subcommand, not counting the value of a global option
            let mut words = args.iter().skip(1);
            let mut sub = None;
            while let Some(a) = words.next() {
                if VALUE_GLOBALS.iter().any(|o| a == o) {
                    words.next();
                } else if a.to_str().is_some_and(|n| cmd.find_subcommand(n).is_some()) {
                    sub = Some(a);
                    break;
                }
            }
            let argv0 = args.first().cloned().unwrap_or_default();
            let help: Vec<OsString> = [Some(argv0), sub.cloned(), Some(OsString::from("--help"))].into_iter().flatten().collect();
            return Err(cmd.try_get_matches_from_mut(help).err().unwrap_or(e));
        }
        matches => matches?,
    };
    let cli = Cli::from_arg_matches(&matches)?;
    cli.global.validate().map_err(|e| e.into_clap(&mut cmd))?;
    Ok(cli)
}

/// Options every subcommand takes; they may come before or after the subcommand.
#[derive(Args, Debug, Clone)]
pub struct Global {
    /// Open this project first (headless).
    #[arg(long, global = true, value_name = "P.FCPROJ", value_parser = PathBufValueParser::new(), conflicts_with_all = ["demo", "bridge"])]
    pub project: Option<PathBuf>,
    /// Open the built-in demo project (headless).
    #[arg(long, global = true, conflicts_with = "bridge")]
    pub demo: bool,
    /// Save the project back to --project when done.
    #[arg(long, global = true, conflicts_with = "save_as")]
    pub save: bool,
    /// Save the project to this path when done.
    #[arg(long, global = true, value_name = "P.FCPROJ", value_parser = PathBufValueParser::new())]
    pub save_as: Option<PathBuf>,
    /// Send commands to the running app (`filmcraft --control PORT`) instead of a headless engine; a
    /// bare port means 127.0.0.1:PORT.
    #[arg(long, global = true, value_name = "127.0.0.1:PORT", value_parser = parse_bridge)]
    pub bridge: Option<BridgeAddr>,
    /// FilmCraft data directory for user export presets (headless; default: the app's data directory).
    #[arg(long, global = true, value_name = "DIR", value_parser = PathBufValueParser::new())]
    pub data_dir: Option<PathBuf>,
    /// Print JSON on one line.
    #[arg(long, global = true)]
    pub compact: bool,
}

impl Global {
    /// The conflicts clap declares between global options, checked on the fully parsed values:
    /// clap only sees them when both options are on the same side of the subcommand.
    pub fn validate(&self) -> Result<(), UsageError> {
        let given = [
            ("--bridge", self.bridge.is_some()),
            ("--project", self.project.is_some()),
            ("--demo", self.demo),
            ("--save", self.save),
            ("--save-as", self.save_as.is_some()),
        ];
        let conflicts = [("--bridge", "--project"), ("--bridge", "--demo"), ("--project", "--demo"), ("--save", "--save-as")];
        let is_given = |name: &str| given.iter().any(|(n, on)| *n == name && *on);
        match conflicts.into_iter().find(|(a, b)| is_given(a) && is_given(b)) {
            Some((a, b)) => Err(UsageError::Conflict(a, b)),
            None => Ok(()),
        }
    }
}

/// A `--bridge` address the bridge client accepts.
#[derive(Debug, Clone)]
pub struct BridgeAddr(pub String);

/// `PORT` alone is the app on this machine: `127.0.0.1:PORT`.
fn parse_bridge(s: &str) -> Result<BridgeAddr, BridgeError> {
    let addr = match s.parse::<u16>() {
        Ok(port) => format!("127.0.0.1:{port}"),
        Err(_) => s.to_string(),
    };
    // port 0 asks the system for any port: there is no app to reach there
    if addr.rsplit_once(':').is_some_and(|(_, port)| port.parse::<u16>() == Ok(0)) {
        return Err(BridgeError::PortZero);
    }
    BridgeClient::new(addr.as_str()).map(|_| BridgeAddr(addr)).map_err(BridgeError::Client)
}

#[derive(Subcommand, Debug)]
pub enum Command {
    /// Run one command and print its result as JSON.
    Exec(ExecArgs),
    /// Run commands from a script, one {"id","params"} JSON object per line (# comments ok), in order.
    ///
    /// file.exportMedia waits for its file before the next line (and before exit) unless its
    /// params say "wait": false.
    Run(RunArgs),
    /// List commands (id, label, shortcut, params).
    Commands(CommandsArgs),
    /// Print one command as JSON (menu, shortcut, params, enabled now).
    Describe(DescribeArgs),
    /// Print the project tree or the active sequence as JSON (default: both).
    Inspect(InspectArgs),
    /// Import media files into the project.
    Import(ImportArgs),
    /// Export the active sequence and wait for it to finish.
    ///
    /// Give an export preset (`export --list-presets`; built-in or the user's) or a format
    /// (guessed from the extension). Offline media (missing, made offline, unreadable) renders as
    /// placeholder frames: the export then lists it in `offlineMedia`, warns on stderr and exits
    /// 1, unless --allow-offline accepts it.
    Export(ExportArgs),
    /// Render one Program frame to PNG (offline media: as for export).
    Render(RenderArgs),
    /// Print media info as JSON, with MXF / Ogg / BWF / MPEG details.
    Probe(ProbeArgs),
    /// Time sequential decoding of a media file.
    BenchDecode(BenchDecodeArgs),
    /// Serve MCP on stdio (headless, or --bridge to the live app).
    Mcp,
}

#[derive(Args, Debug)]
pub struct ExecArgs {
    /// Command id (see `commands`).
    #[arg(value_parser = text)]
    pub id: String,
    /// Parameters: `key=value` pairs (dotted keys nest: `color.r=1`), or one JSON object.
    #[arg(value_parser = ParamArg::parse, value_name = "KEY=VALUE")]
    pub params: Vec<ParamArg>,
}

/// One parameter argument of `exec`.
#[derive(Debug, Clone)]
pub enum ParamArg {
    /// `{"a": 1}`: all the params at once.
    Json(Value),
    Pair(String, Value),
}

impl ParamArg {
    fn parse(s: &str) -> Result<Self, ParamsError> {
        if s.trim_start().starts_with('{') {
            return Ok(ParamArg::Json(serde_json::from_str(s)?));
        }
        let (k, v) = s.split_once('=').ok_or_else(|| ParamsError::ExpectedKeyValue(s.to_string()))?;
        if k.bytes().filter(|&b| b == b'.').count() >= MAX_PARAM_NESTING {
            return Err(ParamsError::TooDeep(MAX_PARAM_NESTING));
        }
        Ok(ParamArg::Pair(k.to_string(), parse_value(v)))
    }
}

/// The params object of `exec` from its arguments.
pub fn params_object(args: &[ParamArg]) -> Result<Value, ParamsError> {
    let mut map = Map::new();
    for arg in args {
        match (arg, args.len()) {
            (ParamArg::Json(v), 1) => return Ok(v.clone()),
            (ParamArg::Json(v), _) => return Err(ParamsError::JsonWithOthers(v.to_string())),
            (ParamArg::Pair(k, v), _) => insert_dotted(&mut map, k, v.clone()),
        }
    }
    Ok(Value::Object(map))
}

fn insert_dotted(m: &mut Map<String, Value>, key: &str, v: Value) {
    match key.split_once('.') {
        None => {
            m.insert(key.to_string(), v);
        }
        Some((head, tail)) => {
            let e = m.entry(head.to_string()).or_insert_with(|| Value::Object(Map::new()));
            if !e.is_object() {
                *e = Value::Object(Map::new());
            }
            if let Value::Object(child) = e {
                insert_dotted(child, tail, v);
            }
        }
    }
}

/// JSON when it parses (numbers, bools, null, arrays, objects, quoted strings), else a string.
pub fn parse_value(s: &str) -> Value {
    serde_json::from_str(s).unwrap_or_else(|_| Value::String(s.to_string()))
}

#[derive(Args, Debug)]
pub struct RunArgs {
    /// The script file, or `-` for standard input.
    #[arg(value_name = "SCRIPT.JSONL", value_parser = PathBufValueParser::new())]
    pub script: PathBuf,
    /// Report failing lines and continue.
    #[arg(long)]
    pub keep_going: bool,
}

#[derive(Args, Debug)]
pub struct CommandsArgs {
    /// Only commands whose id or label contains this text.
    pub filter: Option<String>,
    /// Print JSON instead of a table.
    #[arg(long)]
    pub json: bool,
}

#[derive(Args, Debug)]
pub struct DescribeArgs {
    /// Command id.
    #[arg(value_parser = text)]
    pub id: String,
}

#[derive(Args, Debug)]
pub struct InspectArgs {
    /// What to print.
    #[arg(value_enum)]
    pub what: Option<InspectTarget>,
}

#[derive(ValueEnum, Debug, Clone, Copy)]
pub enum InspectTarget {
    /// The project tree and the active sequence (the default).
    All,
    Project,
    Sequence,
}

#[derive(Args, Debug)]
pub struct ImportArgs {
    /// Media files; paths are stored absolute.
    #[arg(required = true, value_name = "FILE", value_parser = PathBufValueParser::new())]
    pub files: Vec<PathBuf>,
}

#[derive(Args, Debug)]
pub struct ExportArgs {
    /// The output file; with --list-presets, a filter for the preset names.
    #[arg(required_unless_present = "list_presets", value_name = "OUT", value_parser = PathBufValueParser::new())]
    pub out: Option<PathBuf>,
    /// List export presets (name, category, format) as JSON.
    #[arg(long, conflicts_with_all = ["preset", "format", "range", "start", "end", "settings", "gpu_rendering", "scale", "quality", "no_audio", "queue", "allow_offline"])]
    pub list_presets: bool,
    /// Export preset name (built-in or the user's).
    #[arg(long, value_parser = text)]
    pub preset: Option<String>,
    /// Format: h264, hevc, prores, dnxhr, apv, mjpeg, mxf-op1a, mxf-opatom, png, tiff, bmp, gif, wav, aiff or aac (default: guessed from the extension).
    #[arg(long, value_parser = text)]
    pub format: Option<String>,
    /// Range to export.
    #[arg(long, value_enum, conflicts_with_all = ["start", "end"])]
    pub range: Option<RangeArg>,
    /// Start of a custom range, in seconds.
    #[arg(long, value_name = "SECONDS", value_parser = finite::<f64>, allow_negative_numbers = true)]
    pub start: Option<f64>,
    /// End of a custom range, in seconds.
    #[arg(long, value_name = "SECONDS", value_parser = finite::<f64>, allow_negative_numbers = true)]
    pub end: Option<f64>,
    /// ExportSettings JSON, merged over the preset.
    #[arg(long, value_name = "JSON", value_parser = parse_json)]
    pub settings: Option<Value>,
    /// GPU rendering.
    #[arg(long, value_enum)]
    pub gpu_rendering: Option<GpuRendering>,
    /// Render at a fraction of the frame size (0.5 = half).
    #[arg(long, value_parser = finite::<f64>, allow_negative_numbers = true)]
    pub scale: Option<f64>,
    /// Quality, for the formats that take one.
    #[arg(long, value_parser = clap::value_parser!(u8).range(0..=100))]
    pub quality: Option<u8>,
    /// Leave the sound out.
    #[arg(long)]
    pub no_audio: bool,
    /// Add to the export queue and run it instead.
    #[arg(long)]
    pub queue: bool,
    /// Accept placeholder frames for offline media.
    #[arg(long)]
    pub allow_offline: bool,
}

#[derive(ValueEnum, Debug, Clone, Copy)]
pub enum RangeArg {
    #[value(name = "entire")]
    Entire,
    #[value(name = "inOut")]
    InOut,
    #[value(name = "workArea")]
    WorkArea,
}

impl RangeArg {
    pub fn id(self) -> &'static str {
        match self {
            RangeArg::Entire => "entire",
            RangeArg::InOut => "inOut",
            RangeArg::WorkArea => "workArea",
        }
    }
}

#[derive(ValueEnum, Debug, Clone, Copy)]
pub enum GpuRendering {
    Auto,
    Off,
}

impl GpuRendering {
    pub fn id(self) -> &'static str {
        match self {
            GpuRendering::Auto => "auto",
            GpuRendering::Off => "off",
        }
    }
}

#[derive(Args, Debug)]
pub struct RenderArgs {
    /// Timeline position, in seconds.
    #[arg(long, default_value_t = 0.0, value_name = "SECONDS", value_parser = finite::<f64>, allow_negative_numbers = true)]
    pub seconds: f64,
    /// Output PNG.
    #[arg(long, default_value = "frame.png", value_name = "FILE.PNG", value_parser = PathBufValueParser::new())]
    pub out: PathBuf,
    /// Render at a fraction of the frame size (0.5 = half).
    #[arg(long, default_value_t = 1.0, value_parser = finite::<f32>, allow_negative_numbers = true)]
    pub scale: f32,
    /// Accept placeholder frames for offline media.
    #[arg(long)]
    pub allow_offline: bool,
}

#[derive(Args, Debug)]
pub struct ProbeArgs {
    /// Media file.
    #[arg(value_parser = PathBufValueParser::new())]
    pub media: PathBuf,
    /// `media` is the first numbered still of an image sequence.
    #[arg(long)]
    pub image_sequence: bool,
}

#[derive(Args, Debug)]
pub struct BenchDecodeArgs {
    /// Media file.
    #[arg(value_parser = PathBufValueParser::new())]
    pub media: PathBuf,
    /// Frames to decode.
    #[arg(long, default_value_t = 120, value_parser = clap::value_parser!(i64).range(1..))]
    pub frames: i64,
}

/// Why a `--bridge` address is refused.
#[derive(Debug, Error)]
pub enum BridgeError {
    #[error("port 0 is not an address to connect to")]
    PortZero,
    #[error(transparent)]
    Client(filmcraft_automation::AutomationError),
}

#[derive(Debug, Error)]
pub enum ValueError {
    #[error("a value is required")]
    Empty,
    #[error("expected a finite number, got `{0}`")]
    NotFinite(String),
}

/// A finite number (`NaN` and `inf` parse as floats, but mean nothing here).
fn finite<T: std::str::FromStr + Copy + Into<f64>>(s: &str) -> Result<T, ValueError> {
    match s.parse::<T>() {
        Ok(n) if n.into().is_finite() => Ok(n),
        _ => Err(ValueError::NotFinite(s.to_string())),
    }
}

/// A value that is not empty (`--project ""` names nothing).
fn text(s: &str) -> Result<String, ValueError> {
    if s.is_empty() { Err(ValueError::Empty) } else { Ok(s.to_string()) }
}

fn parse_json(s: &str) -> Result<Value, serde_json::Error> {
    serde_json::from_str(s)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn parse(args: &str) -> Result<Cli, clap::Error> {
        Cli::try_parse_from(std::iter::once("filmcraft-cli").chain(args.split_whitespace()))
    }

    fn params(args: &str) -> Value {
        let Ok(Cli { command: Command::Exec(e), .. }) = parse(args) else { panic!("{args}") };
        params_object(&e.params).unwrap()
    }

    #[test]
    fn the_definition_is_consistent() {
        use clap::CommandFactory;
        Cli::command().debug_assert();
    }

    #[test]
    fn options_anywhere() {
        let Ok(c) = parse("exec --demo timeline.razor seconds=3.5 --save --compact") else { panic!() };
        assert!(c.global.demo && c.global.save && c.global.compact);
        let Ok(c) = parse("exec timeline.razor --project p.fcproj") else { panic!() };
        assert_eq!(c.global.project.as_deref(), Some(std::path::Path::new("p.fcproj")));
    }

    #[test]
    fn key_value_params() {
        assert_eq!(
            params("exec x seconds=3.5 name=Selects on=true ids=[1,2] color.r=1 color.g=0.5 s=\"7\""),
            json!({"seconds": 3.5, "name": "Selects", "on": true, "ids": [1, 2], "color": {"r": 1, "g": 0.5}, "s": "7"})
        );
        assert!(parse("exec x novalue").is_err());
        assert_eq!(params("exec x"), json!({}));
    }

    #[test]
    fn json_params_are_alone() {
        let Ok(Cli { command: Command::Exec(e), .. }) = Cli::try_parse_from(["filmcraft-cli", "exec", "x", r#"{"a": {"b": [1]}}"#]) else { panic!() };
        assert_eq!(params_object(&e.params).unwrap(), json!({"a": {"b": [1]}}));
        let Ok(Cli { command: Command::Exec(e), .. }) = Cli::try_parse_from(["filmcraft-cli", "exec", "x", r#"{"a": 1}"#, "b=2"]) else { panic!() };
        assert!(matches!(params_object(&e.params), Err(ParamsError::JsonWithOthers(_))));
        assert!(Cli::try_parse_from(["filmcraft-cli", "exec", "x", "{oops"]).is_err());
    }

    #[test]
    fn dotted_key_depth_is_bounded() {
        let allowed = format!("{}leaf=1", "a.".repeat(MAX_PARAM_NESTING - 1));
        assert!(Cli::try_parse_from(["filmcraft-cli", "exec", "x", &allowed]).is_ok());
        let too_deep = format!("{}leaf=1", "a.".repeat(MAX_PARAM_NESTING));
        assert!(Cli::try_parse_from(["filmcraft-cli", "exec", "x", &too_deep]).is_err());
    }

    #[test]
    fn version_after_global_options() {
        for s in ["--version", "-V", "--demo --version", "--compact -V", "--data-dir d --version", "export --version"] {
            let e = parse(s).unwrap_err();
            assert_eq!(e.kind(), clap::error::ErrorKind::DisplayVersion, "{s}");
        }
    }

    #[test]
    fn typos_repeats_and_conflicts_are_usage_errors() {
        for s in [
            "exec x --fromat json",
            "export out.wav --format wav --format aiff",
            "--project a.fcproj --demo exec x",
            "--bridge 127.0.0.1:1 --demo exec x",
            "--save --save-as p exec x",
            "export out.wav --range inOut --start 1",
            "export",
            "export --list-presets --queue",
            "export o.wav --quality 101",
            "export o.wav --scale NaN",
            "export o.wav --settings {oops",
            "mcp --automation-read-root X",
            "render --seconds inf",
            "bench-decode a.mov --frames 0",
            "import",
            "frobnicate",
            "version",
            "--project= exec x",
            "export out.wav --preset=",
        ] {
            let e = parse(s).unwrap_err();
            assert_eq!(e.exit_code(), 2, "{s}: {e}");
        }
    }

    #[test]
    fn the_gpu_rendering_value_follows_a_space() {
        let Ok(Cli { command: Command::Export(e), .. }) = parse("export --gpu-rendering off out.wav --start 0 --end 0.1") else { panic!() };
        assert_eq!(e.out.as_deref(), Some(std::path::Path::new("out.wav")));
        assert!(matches!(e.gpu_rendering, Some(GpuRendering::Off)));
        assert_eq!((e.start, e.end), (Some(0.0), Some(0.1)));
    }

    #[test]
    fn conflicting_globals_on_both_sides_of_the_subcommand_are_rejected() {
        for s in [
            "--bridge 127.0.0.1:1 exec x --demo",
            "--demo exec x --project p.fcproj",
            "--project p.fcproj inspect --bridge 127.0.0.1:1",
            "--save exec x --save-as p.fcproj",
        ] {
            let Ok(c) = parse(s) else { panic!("{s} parses: clap cannot see the conflict") };
            assert!(matches!(c.global.validate(), Err(UsageError::Conflict(..))), "{s}");
        }
        let Ok(c) = parse("--demo exec x --save --compact") else { panic!() };
        assert!(c.global.validate().is_ok());
    }

    #[test]
    fn a_bare_bridge_port_means_the_local_app() {
        let Ok(c) = parse("--bridge 7878 exec x") else { panic!() };
        assert_eq!(c.global.bridge.map(|b| b.0).as_deref(), Some("127.0.0.1:7878"));
        let Ok(c) = parse("--bridge=127.0.0.1:9876 exec x") else { panic!() };
        assert_eq!(c.global.bridge.map(|b| b.0).as_deref(), Some("127.0.0.1:9876"));
        assert!(parse("--bridge 0.0.0.0:9876 exec x").is_err(), "not a loopback address");
        assert!(parse("--bridge 70000 exec x").is_err());
        for zero in ["0", "127.0.0.1:0", "00"] {
            let e = parse(&format!("--bridge {zero} exec x")).unwrap_err();
            assert_eq!(e.exit_code(), 2, "{zero}");
            assert!(e.to_string().contains("port 0"), "{zero}: {e}");
        }
    }

    #[test]
    fn a_value_flag_never_takes_the_next_flag() {
        assert!(parse("export --preset --queue out.wav").is_err());
        assert!(parse("--project --demo exec x").is_err());
    }

    #[test]
    fn double_dash_ends_options() {
        let Ok(Cli { command: Command::Import(i), .. }) = parse("import -- --weird-name.mov a.mov") else { panic!() };
        assert_eq!(i.files.len(), 2);
    }

    #[test]
    fn help_is_the_doc_comments() {
        for s in ["--help", "-h", "help", "export --help", "help export"] {
            let e = parse(s).unwrap_err();
            assert_eq!(e.exit_code(), 0, "{s}");
        }
    }
}
