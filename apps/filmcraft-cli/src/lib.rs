//! FilmCraft headless CLI: every engine command is one shell call away, for scripts and AI agents.
//!
//! Follows RFC-0012 (command-line conventions): the command line is the clap derive types in
//! [`cli`], errors are the thiserror enums in [`error`], and `main` only wires the process to
//! [`run`]. Run `filmcraft-cli --help` for the reference (also in docs/agents.md).

#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable))]

mod backend;
pub mod cli;
mod commands;
pub mod error;
mod io;
mod probe;

use std::ffi::OsString;
use std::io::Write;

use error::{CliError, Failure};
use io::Io;

/// The GPU export frame renderer (filmcraft-gpu's off-screen compositor behind filmcraft-export's
/// frame-renderer hook): exports with GPU rendering Auto composite on the GPU and fall back to the
/// CPU reference renderer wherever it cannot.
struct GpuFrameRenderer(filmcraft_gpu::ExportRenderer);

impl filmcraft_export::FrameRenderer for GpuFrameRenderer {
    fn render(
        &mut self,
        project: &filmcraft_project::Project,
        seq: filmcraft_project::ItemId,
        t: filmcraft_time::Tick,
        opts: filmcraft_render::RenderOptions,
        sources: &dyn filmcraft_render::SourceProvider,
    ) -> Option<filmcraft_render::Image> {
        self.0.render(project, seq, t, opts, sources)
    }
}

fn register_gpu_frame_renderer() {
    filmcraft_export::register_frame_renderer(|| {
        filmcraft_gpu::ExportRenderer::new().map(|r| Box::new(GpuFrameRenderer(r)) as Box<dyn filmcraft_export::FrameRenderer>)
    });
}

/// OS hardware video decoders (VideoToolbox on macOS, Media Foundation on Windows) in front of our
/// own, as in the desktop app. Also what `mcp` and the headless commands decode with.
fn register_hardware_decoders() -> filmcraft_platform::Availability {
    filmcraft_platform::register()
}

/// Run the CLI with `args` (the program name first): results go to `out`, errors and progress to
/// `err`. Returns the exit status: 0 success, 1 a command failed, 2 usage error.
pub fn run<I, T>(args: I, out: &mut dyn Write, err: &mut dyn Write) -> u8
where
    I: IntoIterator<Item = T>,
    T: Into<OsString>,
{
    let mut io = Io::new(out, err);
    let args: Vec<OsString> = args.into_iter().map(Into::into).collect();
    let result = match cli::parse(&args) {
        Ok(cli) => match tokio::runtime::Builder::new_multi_thread().enable_all().build() {
            Ok(rt) => rt.block_on(commands::dispatch(cli, &mut io)),
            Err(e) => Err(Failure::Runtime(e).into()),
        },
        // help and version go to stdout with status 0; usage errors to stderr with status 2
        Err(e) if !e.use_stderr() => {
            io.text(&e.render().to_string());
            Ok(())
        }
        Err(e) => {
            io.diag_text(&e.render().to_string());
            return u8::try_from(e.exit_code()).unwrap_or(2);
        }
    };
    let mut code = 0;
    if let Err(e) = result {
        code = e.exit_code();
        report(&mut io, &e);
    }
    if let Some(e) = io.take_stdout_error() {
        report(&mut io, &CliError::from(e));
        code = 1;
    }
    code
}

/// `error: <message>` on stderr; a usage error also points at `--help`, as clap's own do.
fn report(io: &mut Io<'_>, e: &CliError) {
    io.diag(format_args!("error: {e}"));
    if matches!(e, CliError::Usage(_)) {
        io.diag(format_args!("\nFor more information, try '--help'."));
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::commands::{absolute, format_for, offline_media, offline_warning, render_at_playhead, set_custom_bounds, written_paths};

    fn run_args(args: &[&str]) -> (u8, String, String) {
        let (mut out, mut err) = (Vec::new(), Vec::new());
        let code = run(std::iter::once("filmcraft-cli").chain(args.iter().copied()), &mut out, &mut err);
        (code, String::from_utf8_lossy(&out).into_owned(), String::from_utf8_lossy(&err).into_owned())
    }

    #[test]
    fn help_goes_to_stdout_and_usage_errors_to_stderr() {
        let (code, out, err) = run_args(&["--help"]);
        assert_eq!((code, err.as_str()), (0, ""));
        assert!(out.contains("exec") && out.contains("Exit status"), "{out}");
        let (code, out, err) = run_args(&["frobnicate"]);
        assert_eq!((code, out.as_str()), (2, ""));
        assert!(err.starts_with("error:"), "{err}");
        let (code, _, err) = run_args(&["exec", "file.newBin", "notkv"]);
        assert_eq!(code, 2, "{err}");
    }

    #[test]
    fn help_and_version_report_a_failing_stdout() {
        struct Full;
        impl Write for Full {
            fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
                Err(std::io::ErrorKind::StorageFull.into())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        for args in [["filmcraft-cli", "--help"], ["filmcraft-cli", "--version"]] {
            let mut err = Vec::new();
            assert_eq!(run(args, &mut Full, &mut err), 1, "{args:?}");
            assert!(String::from_utf8_lossy(&err).starts_with("error: can't write to stdout"), "{args:?}");
        }
    }

    #[test]
    fn conflicting_options_either_side_of_the_subcommand_exit_2() {
        for args in [["--demo", "exec", "no.such", "--project=p.fcproj"], ["--bridge=127.0.0.1:1", "exec", "no.such", "--demo"]] {
            let (code, out, err) = run_args(&args);
            assert_eq!((code, out.as_str()), (2, ""), "{args:?}: {err}");
            assert!(err.contains("cannot be used with"), "{err}");
        }
    }

    #[test]
    fn inspect_all_is_the_same_as_bare_inspect() {
        let (code, bare, err) = run_args(&["--demo", "--compact", "inspect"]);
        assert_eq!(code, 0, "{err}");
        let (code, all, err) = run_args(&["--demo", "--compact", "inspect", "all"]);
        assert_eq!(code, 0, "{err}");
        assert_eq!(all, bare);
        assert!(bare.contains("\"project\"") && bare.contains("\"sequence\""));
    }

    /// PhotoCraft's decision (#9194): a bare --help/-h is help even after a mistake on the line.
    #[test]
    fn help_is_help_even_after_a_mistake() {
        for args in [
            &["export", "o.wav", "--quality", "101", "--help"][..],
            &["--demo", "export", "--fromat", "-h"],
            &["export", "--preset", "--help"],
            &["--demo", "--project", "p.fcproj", "export", "--help"],
        ] {
            let (code, out, err) = run_args(args);
            assert_eq!((code, err.as_str()), (0, ""), "{args:?}");
            assert!(out.contains("--allow-offline"), "{args:?}: {out}");
        }
        // after `--` it is a file name, not help
        let (code, ..) = run_args(&["import", "--", "--help"]);
        assert_ne!(code, 0);
        // no subcommand named: the top-level help
        let (code, out, _) = run_args(&["--nope", "--help"]);
        assert_eq!(code, 0);
        assert!(out.contains("Exit status"), "{out}");
    }

    /// An option value that is also a subcommand's name is not the subcommand help is about.
    #[test]
    fn help_after_a_mistake_names_the_subcommand_not_an_option_value() {
        let (code, out, _) = run_args(&["--data-dir", "export", "render", "--scale", "NaN", "--help"]);
        assert_eq!(code, 0);
        assert!(out.contains("--seconds") && !out.contains("--list-presets"), "{out}");
    }

    #[cfg(unix)]
    #[test]
    fn a_path_that_is_not_unicode_reaches_the_parser_and_an_empty_one_does_not() {
        use std::os::unix::ffi::OsStringExt;
        let odd = OsString::from_vec(vec![b'x', 0xff, b'.', b'm', b'p', b'4']);
        let (mut out, mut err) = (Vec::new(), Vec::new());
        let args = [OsString::from("filmcraft-cli"), OsString::from("probe"), odd];
        // the file does not exist: a failure (1) after parsing, not a usage error (2)
        assert_eq!(run(args, &mut out, &mut err), 1, "{}", String::from_utf8_lossy(&err));
        let (code, ..) = run_args(&["probe", ""]);
        assert_eq!(code, 2);
    }

    #[test]
    fn a_usage_error_is_found_before_any_work() {
        let (code, out, err) = run_args(&["--demo", "exec", "file.newBin", "--nme", "x"]);
        assert_eq!((code, out.as_str()), (2, ""));
        assert!(err.contains("--nme"), "{err}");
    }

    #[test]
    fn exec_runs_a_command_and_a_failure_is_exit_1() {
        let (code, out, err) = run_args(&["--demo", "--compact", "exec", "file.newBin", "name=Via run"]);
        assert_eq!(code, 0, "{err}");
        assert!(serde_json::from_str::<serde_json::Value>(&out).is_ok(), "{out}");
        let (code, out, err) = run_args(&["--demo", "exec", "no.such.command"]);
        assert_eq!((code, out.as_str()), (1, ""));
        assert!(err.starts_with("error: no.such.command:"), "{err}");
    }

    #[test]
    fn render_reports_the_real_reason() {
        let mut s = filmcraft_engine::Session::default();
        assert!(render_at_playhead(&s, 0.5).unwrap_err().to_string().contains("sequence"));
        s.execute("file.newSequence", json!({"width": 16, "height": 16})).unwrap();
        for scale in [0.0, -1.0, f32::NAN] {
            let e = render_at_playhead(&s, scale).unwrap_err().to_string();
            assert!(e.contains("finite and positive"), "{e}");
        }
        assert_eq!(render_at_playhead(&s, 0.5).unwrap().0.w, 8);
    }

    /// The CLI (and MCP / headless runs, which share its entry point) registers the hardware
    /// decoders at start-up.
    #[test]
    fn startup_registers_the_hardware_decoders() {
        let hardware = register_hardware_decoders();
        // always on macOS and Windows; on Linux when a VA-API driver or NVIDIA's driver (NVDEC) is there
        let expected = cfg!(any(target_os = "macos", target_os = "windows"))
            || (cfg!(target_os = "linux") && matches!(hardware, filmcraft_platform::Availability::Available(_)));
        assert_eq!(filmcraft_platform::registered(), expected, "{hardware:?}");
    }

    #[test]
    fn a_failing_diagnostic_stream_does_not_panic() {
        struct Closed;
        impl Write for Closed {
            fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
                Err(std::io::ErrorKind::BrokenPipe.into())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Err(std::io::ErrorKind::BrokenPipe.into())
            }
        }
        let mut out = Vec::new();
        let mut err = Closed;
        let mut io = Io::new(&mut out, &mut err);
        io.diag(format_args!("exported {}", "a.wav"));
        let mut err = Vec::new();
        let mut io = Io::new(&mut out, &mut err);
        io.diag(format_args!("hi"));
        assert_eq!(err, b"hi\n");
    }

    #[test]
    fn a_closed_stdout_is_not_an_error_but_other_write_errors_are() {
        struct Failing(std::io::ErrorKind);
        impl Write for Failing {
            fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
                Err(self.0.into())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        let mut err = Vec::new();
        let mut closed = Failing(std::io::ErrorKind::BrokenPipe);
        let mut io = Io::new(&mut closed, &mut err);
        io.line(format_args!("a"));
        io.line(format_args!("b"));
        assert!(io.take_stdout_error().is_none());
        let mut full = Failing(std::io::ErrorKind::StorageFull);
        let mut io = Io::new(&mut full, &mut err);
        io.line(format_args!("a"));
        assert!(io.take_stdout_error().is_some());
    }

    #[test]
    fn export_message_names_the_written_files() {
        let direct = json!({"job": 1, "path": "out-apv.mp4.mov", "result": {"path": "out-apv.mp4.mov"}});
        assert_eq!(written_paths(&direct, "out-apv.mp4"), ["out-apv.mp4.mov"]);
        let queued = json!({"added": [1, 2], "items": [{"path": "a.mp4.mov"}, {"path": "b.wav"}]});
        assert_eq!(written_paths(&queued, "a.mp4"), ["a.mp4.mov", "b.wav"]);
        assert_eq!(written_paths(&json!({}), "x.mov"), ["x.mov"]);
        assert_eq!(written_paths(&json!({"items": []}), "x.mov"), ["x.mov"]);
    }

    #[test]
    fn offline_media_from_direct_and_queued_results() {
        let a = json!({"item": 8, "name": "a.mp4", "path": "/m/a.mp4", "status": "missing", "error": ""});
        let b = json!({"item": 9, "name": "b.mov", "path": "/m/b.mov", "status": "offline", "error": ""});
        assert_eq!(offline_media(&json!({"path": "x.mp4", "offlineMedia": [a]})), std::slice::from_ref(&a));
        let queued = json!({"items": [{"offlineMedia": [a, b]}, {"offlineMedia": [a]}, {"offlineMedia": []}]});
        assert_eq!(offline_media(&queued), [a.clone(), b.clone()]);
        assert!(offline_media(&json!({"path": "x.mp4"})).is_empty());
        let w = offline_warning(&[a, b]);
        assert!(w.contains("2 offline media item(s)") && w.contains("item 8 a.mp4 (missing): /m/a.mp4") && w.contains("item 9 b.mov (offline)"), "{w}");
    }

    #[test]
    fn export_lone_bound_is_forwarded_as_custom_range() {
        let mut p = json!({});
        set_custom_bounds(&mut p, Some(0.25), None);
        assert_eq!(p["range"], "custom");
        assert_eq!(p["startSeconds"], 0.25);
        assert!(p.get("endSeconds").is_none());
        let mut p = json!({});
        set_custom_bounds(&mut p, None, Some(0.25));
        assert_eq!(p["range"], "custom");
        assert_eq!(p["endSeconds"], 0.25);
        let mut p = json!({});
        set_custom_bounds(&mut p, None, None);
        assert!(p.get("range").is_none());
    }

    /// `import` used `canonicalize`, which resolves symlinks and on Windows stores `\\?\C:\…`
    /// verbatim paths in the project (#850).
    #[test]
    fn imported_paths_are_absolute_without_resolving_links() {
        let cwd = std::env::current_dir().unwrap();
        assert_eq!(absolute(std::path::Path::new("clip.mov")), cwd.join("clip.mov"));
        assert!(!absolute(std::path::Path::new("clip.mov")).to_string_lossy().starts_with(r"\\?\"));
        #[cfg(unix)]
        {
            let dir = std::env::temp_dir().join(format!("filmcraft-cli-abs-{}", std::process::id()));
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join("real.mov"), b"x").unwrap();
            let link = dir.join("link.mov");
            let _ = std::fs::remove_file(&link);
            std::os::unix::fs::symlink(dir.join("real.mov"), &link).unwrap();
            assert_eq!(absolute(&link), link);
            std::fs::remove_dir_all(&dir).unwrap();
        }
    }

    #[test]
    fn export_format_from_extension() {
        let f = |p: &str| format_for(std::path::Path::new(p));
        assert_eq!(f("out/clip.MXF"), Some("mxf-op1a"));
        assert_eq!(f("a.mov"), Some("prores"));
        assert_eq!(f("noext"), None);
    }
}
