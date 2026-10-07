//! FilmCraft headless CLI: every engine command is one shell call away, for scripts and AI agents.
//!
//! Run `filmcraft-cli help` for the full reference (also in docs/agents.md).

#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable))]

mod args;
mod probe;

use args::{Args, parse_value};
use filmcraft_automation::BridgeClient;
use filmcraft_engine::Session;
use serde_json::{Value, json};

const HELP: &str = "\
filmcraft-cli: drive FilmCraft from the shell (headless engine, or the live app with --bridge)

USAGE
  filmcraft-cli [OPTIONS] <SUBCOMMAND> [ARGS]

SUBCOMMANDS
  exec <id> [key=value ...]     run one command; prints its result as JSON
  exec <id> '<json params>'     same, params as one JSON object
  run <script.jsonl | ->        run commands, one {\"id\",\"params\"} per line (# comments ok)
  commands [filter] [--json]    list commands (id, label, shortcut, params)
  describe <id>                 one command as JSON (menu, shortcut, params, enabled now)
  inspect [project|sequence]    project tree or active sequence as JSON (default: both)
  import <file>...              import media into the project
  export <out> [--preset name] [--format f] [--range r] [--start s --end s] [--settings json]
                                export the active sequence and wait for it to finish: with an
                                export preset (`export --list-presets`; built-in or the user's), or
                                a format (h264|prores|dnxhr|mjpeg|mxf-op1a|mxf-opatom|png|tiff|bmp|gif|wav|aiff, guessed
                                from the extension); --range entire|inOut|workArea, or a custom
                                range in seconds; --settings is ExportSettings JSON merged over the
                                preset; --queue adds to the export queue and runs it instead
  export --list-presets [query] list export presets (name, category, format) as JSON
  render --seconds S --out f.png [--scale 0.5]   render one Program frame to PNG
  probe <media> [--image-sequence]
                                media info as JSON, with MXF / Ogg / BWF details; with
                                --image-sequence <media> is the first numbered still of a sequence
  bench-decode <media> [--frames N]
  mcp                           MCP server on stdio (headless, or --bridge to the live app)
  help                          this text
  --version                     print the version

OPTIONS
  --project <p.fcproj>          open this project first (headless)
  --demo                        open the built-in demo project (headless)
  --save                        save the project back to --project when done
  --save-as <p.fcproj>          save the project to this path when done
  --bridge <127.0.0.1:PORT>     send commands to the running app (`filmcraft --control PORT`)
  --data-dir <dir>              FilmCraft data directory for user export presets (headless;
                                default: the app's data directory)
  --keep-going                  `run`: report failing lines and continue
  --compact                     one-line JSON output

VALUES
  key=value parses value as JSON when it can (3.5, true, [1,2], {\"a\":1}), else as a string:
  `exec timeline.razor seconds=3.5`, `exec file.newBin name=Selects`.
  Times: 254016000000 ticks per second; most commands also take seconds=, frame= or timecode=.

EXIT STATUS
  0 success · 1 a command failed · 2 usage error
";

fn usage(msg: impl std::fmt::Display) -> ! {
    eprintln!("filmcraft-cli: {msg}\nRun `filmcraft-cli help` for usage.");
    std::process::exit(2)
}

fn fail(msg: impl std::fmt::Display) -> ! {
    eprintln!("filmcraft-cli: {msg}");
    std::process::exit(1)
}

/// Where commands go: an in-process session, or the desktop app's control channel.
enum Backend {
    Local(Box<Session>),
    Bridge(BridgeClient),
}

impl Backend {
    fn open(a: &Args) -> Self {
        if let Some(addr) = a.opt("--bridge") {
            if a.opt("--project").is_some() || a.flag("--demo") {
                usage("--bridge drives the running app; open projects there (exec file.open path=…)");
            }
            return Backend::Bridge(BridgeClient::new(addr).unwrap_or_else(|e| usage(e)));
        }
        let mut s = Session::default();
        // user export presets (and other per-user libraries) from the data directory
        if let Some(dir) = a.opt("--data-dir").map(std::path::PathBuf::from).or_else(filmcraft_engine::autosave::default_data_dir) {
            s.export_presets.set_dir(&dir);
        }
        if let Some(p) = a.opt("--project") {
            if let Err(e) = s.execute("file.open", json!({"path": p})) {
                fail(e);
            }
        } else if a.flag("--demo") {
            let _ = s.execute("file.openDemoProject", json!({}));
        }
        Backend::Local(Box::new(s))
    }

    async fn exec(&mut self, id: &str, params: Value) -> Result<Value, String> {
        match self {
            Backend::Local(s) => s.execute(id, params).map_err(|e| e.to_string()),
            Backend::Bridge(b) => b.call("engine.execute", json!({"command": id, "params": params})).await.map_err(|e| e.to_string()),
        }
    }

    /// Save to the --project path (`--save`) or to `--save-as`. Bridge mode saves in the app.
    async fn finish(&mut self, a: &Args) {
        let r = if let Some(p) = a.opt("--save-as") {
            Some(self.exec("file.saveAs", json!({"path": p})).await)
        } else if a.flag("--save") {
            match a.opt("--project") {
                Some(p) => Some(self.exec("file.save", json!({"path": p})).await),
                None => Some(self.exec("file.save", json!({})).await),
            }
        } else {
            None
        };
        if let Some(Err(e)) = r {
            fail(format!("saving: {e}"));
        }
    }
}

fn print(a: &Args, v: &Value) {
    let s = if a.flag("--compact") { serde_json::to_string(v) } else { serde_json::to_string_pretty(v) };
    println!("{}", s.unwrap_or_default());
}

/// Export format from the output file's extension.
fn format_for(path: &str) -> Option<&'static str> {
    let ext = path.rsplit_once('.')?.1.to_ascii_lowercase();
    Some(match ext.as_str() {
        "mp4" | "m4v" => "h264",
        "mov" => "prores",
        "mxf" => "mxf-op1a",
        "png" => "png",
        "gif" => "gif",
        "wav" => "wav",
        "tif" | "tiff" => "tiff",
        "bmp" => "bmp",
        "aif" | "aiff" => "aiff",
        _ => return None,
    })
}

/// OS hardware video decoders (VideoToolbox on macOS, Media Foundation on Windows) in front of our
/// own, as in the desktop app. Also what `mcp` and the headless commands decode with.
fn register_hardware_decoders() -> filmcraft_platform::Availability {
    filmcraft_platform::register()
}

#[tokio::main]
async fn main() {
    if matches!(std::env::args().nth(1).as_deref(), Some("--version" | "-V")) {
        println!("filmcraft-cli {}", env!("CARGO_PKG_VERSION"));
        return;
    }
    register_hardware_decoders();
    let a = Args::parse(std::env::args().skip(1));
    let Some(cmd) = a.pos(0) else { usage("missing subcommand") };
    match cmd {
        "help" | "--help" | "-h" => print!("{HELP}"),
        "version" => println!("filmcraft-cli {}", env!("CARGO_PKG_VERSION")),
        "probe" => {
            let path = a.pos(1).unwrap_or_else(|| usage("probe <media> [--image-sequence]"));
            match probe::probe(path, a.flag("--image-sequence")) {
                Ok(v) => print(&a, &v),
                Err(e) => fail(e),
            }
        }
        "bench-decode" => {
            let path = a.pos(1).unwrap_or_else(|| usage("bench-decode <media>"));
            let n: i64 = a.opt("--frames").and_then(|v| v.parse().ok()).unwrap_or(120);
            let src = filmcraft_codecs::open_bytes(path, std::fs::read(path).unwrap_or_else(|e| fail(e)).into()).unwrap_or_else(|e| fail(e));
            let rate = src.info().frame_rate();
            let t0 = std::time::Instant::now();
            for f in 0..n {
                src.video_frame(filmcraft_media::FrameRequest::full(rate.tick_of(f))).unwrap_or_else(|e| fail(e));
            }
            let dt = t0.elapsed().as_secs_f64();
            println!("{n} frames in {dt:.2}s → {:.1} fps (sequential, via Mp4Source)", n as f64 / dt);
            let t1 = std::time::Instant::now();
            let f = src.video_frame(filmcraft_media::FrameRequest::full(rate.tick_of(n / 2))).unwrap_or_else(|e| fail(e));
            let (w, h, _) = f.to_linear_f32_decimated(2);
            println!("½-res linear conversion {w}x{h}: {:.1} ms", t1.elapsed().as_secs_f64() * 1000.0);
        }
        "commands" | "describe" => {
            let mut b = Backend::open(&a);
            let list = b.exec("command.list", json!({})).await.unwrap_or_else(|e| fail(e));
            let list = list.as_array().cloned().unwrap_or_default();
            if cmd == "describe" {
                let id = a.pos(1).unwrap_or_else(|| usage("describe <id>"));
                match list.iter().find(|c| c["id"] == id) {
                    Some(c) => print(&a, c),
                    None => fail(format!("unknown command `{id}` (try `filmcraft-cli commands {id}`)")),
                }
                return;
            }
            let f = a.pos(1).map(str::to_ascii_lowercase).unwrap_or_default();
            let hit = |c: &Value| {
                let id = c["id"].as_str().unwrap_or("").to_ascii_lowercase();
                let label = c["label"].as_str().unwrap_or("").to_ascii_lowercase();
                f.is_empty() || id.contains(&f) || label.contains(&f)
            };
            let list: Vec<Value> = list.into_iter().filter(hit).collect();
            if a.flag("--json") {
                print(&a, &Value::Array(list));
            } else {
                for c in &list {
                    let s = |k: &str| c[k].as_str().unwrap_or("").to_string();
                    println!("{:<32} {:<34} {:<14} {}", s("id"), s("label"), s("shortcut"), s("params"));
                }
            }
        }
        "exec" => {
            let id = a.pos(1).unwrap_or_else(|| usage("exec <id> [key=value ...]"));
            let params = a.params_from(2).unwrap_or_else(|e| usage(e));
            let mut b = Backend::open(&a);
            match b.exec(id, params).await {
                Ok(v) => print(&a, &v),
                Err(e) => fail(format!("{id}: {e}")),
            }
            b.finish(&a).await;
        }
        "run" => {
            let script = a.pos(1).unwrap_or_else(|| usage("run <script.jsonl | ->"));
            let text = if script == "-" {
                std::io::read_to_string(std::io::stdin()).unwrap_or_else(|e| fail(e))
            } else {
                std::fs::read_to_string(script).unwrap_or_else(|e| fail(format!("{script}: {e}")))
            };
            let mut b = Backend::open(&a);
            let mut failed = 0;
            for (n, line) in text.lines().enumerate().filter(|(_, l)| !l.trim().is_empty() && !l.trim_start().starts_with('#')) {
                let v: Value = serde_json::from_str(line).unwrap_or_else(|e| usage(format!("line {}: {e}", n + 1)));
                let id = v["id"].as_str().unwrap_or_else(|| usage(format!("line {}: missing \"id\"", n + 1)));
                match b.exec(id, v.get("params").cloned().unwrap_or(json!({}))).await {
                    Ok(r) => println!("{}", json!({"line": n + 1, "id": id, "ok": true, "result": r})),
                    Err(e) => {
                        println!("{}", json!({"line": n + 1, "id": id, "ok": false, "error": e}));
                        failed += 1;
                        if !a.flag("--keep-going") {
                            std::process::exit(1);
                        }
                    }
                }
            }
            b.finish(&a).await;
            if failed > 0 {
                std::process::exit(1);
            }
        }
        "inspect" => {
            let mut b = Backend::open(&a);
            let what = a.pos(1).unwrap_or("all");
            let mut get = async |id: &str| b.exec(id, json!({})).await.unwrap_or_else(|e| fail(e));
            let v = match what {
                "project" => get("project.inspect").await,
                "sequence" => get("sequence.inspect").await,
                "all" => json!({"project": get("project.inspect").await, "sequence": get("sequence.inspect").await}),
                other => usage(format!("inspect: unknown `{other}` (project | sequence)")),
            };
            print(&a, &v);
        }
        "import" => {
            let paths: Vec<String> =
                a.positionals[1..].iter().map(|p| std::fs::canonicalize(p).map(|c| c.to_string_lossy().into_owned()).unwrap_or_else(|_| p.clone())).collect();
            if paths.is_empty() {
                usage("import <file>...");
            }
            let mut b = Backend::open(&a);
            match b.exec("file.import", json!({"paths": paths})).await {
                Ok(v) => print(&a, &v),
                Err(e) => fail(format!("import: {e}")),
            }
            b.finish(&a).await;
        }
        "export" => {
            if a.flag("--list-presets") {
                let mut b = Backend::open(&a);
                let q = a.pos(1).map(|q| json!({"query": q})).unwrap_or(json!({}));
                match b.exec("export.presets.list", q).await {
                    Ok(v) => print(&a, &v["presets"]),
                    Err(e) => fail(format!("export: {e}")),
                }
                return;
            }
            let out = a.pos(1).unwrap_or_else(|| usage("export <out> [--preset name | --format f]"));
            let mut p = json!({"path": out, "wait": true});
            match (a.opt("--preset"), a.opt("--format")) {
                (Some(preset), f) => {
                    p["preset"] = json!(preset);
                    if let Some(f) = f {
                        p["format"] = json!(f);
                    }
                }
                (None, f) => {
                    let format = f.or_else(|| format_for(out)).unwrap_or_else(|| usage("export: give --preset or --format (unknown extension)"));
                    p["format"] = json!(format);
                }
            }
            for (k, key) in [("--scale", "scale"), ("--quality", "quality")] {
                if let Some(v) = a.opt(k) {
                    p[key] = parse_value(v);
                }
            }
            if let Some(r) = a.opt("--range") {
                p["range"] = json!(r);
            }
            if let (Some(s0), Some(s1)) = (a.opt("--start"), a.opt("--end")) {
                p["range"] = json!("custom");
                p["startSeconds"] = parse_value(s0);
                p["endSeconds"] = parse_value(s1);
            }
            if let Some(js) = a.opt("--settings") {
                p["settings"] = serde_json::from_str(js).unwrap_or_else(|e| usage(format!("--settings: {e}")));
            }
            if a.flag("--no-audio") {
                p["audio"] = json!(false);
            }
            let mut b = Backend::open(&a);
            let t0 = std::time::Instant::now();
            let r = if a.flag("--queue") {
                let mut q = p.clone();
                q["start"] = json!(true);
                b.exec("export.queue.add", q).await.and_then(|v| {
                    let failed: Vec<&Value> = v["items"].as_array().map(|i| i.iter().filter(|x| x["status"] != "done").collect()).unwrap_or_default();
                    if failed.is_empty() { Ok(v) } else { Err(format!("queue: {}", Value::Array(failed.into_iter().cloned().collect()))) }
                })
            } else {
                b.exec("file.exportMedia", p).await
            };
            match r {
                Ok(v) => {
                    print(&a, &v);
                    eprintln!("exported {out} in {:.1}s", t0.elapsed().as_secs_f64());
                }
                Err(e) => fail(format!("export: {e}")),
            }
        }
        "render" => {
            if a.opt("--bridge").is_some() {
                usage("render is headless; with --bridge use the MCP `render_frame` tool or `exec` ui commands");
            }
            let Backend::Local(mut s) = Backend::open(&a) else { usage("render is headless") };
            let secs: f64 = a.opt("--seconds").and_then(|v| v.parse().ok()).unwrap_or(0.0);
            let scale: f32 = a.opt("--scale").and_then(|v| v.parse().ok()).unwrap_or(1.0);
            let out = a.opt("--out").unwrap_or("frame.png");
            s.set_playhead(filmcraft_time::Tick::from_seconds_f64(secs));
            let t0 = std::time::Instant::now();
            let Some(img) = s.render_program(scale) else { fail("no sequence") };
            let dt = t0.elapsed();
            let png = filmcraft_automation::png_rgba(img.w as u32, img.h as u32, img.over_black_rgba8(), 0).unwrap_or_else(|e| fail(e));
            std::fs::write(out, png).unwrap_or_else(|e| fail(format!("{out}: {e}")));
            eprintln!("rendered {}x{} in {:.1} ms → {out}", img.w, img.h, dt.as_secs_f64() * 1000.0);
        }
        "mcp" => {
            let server = match a.opt("--bridge") {
                Some(addr) => filmcraft_automation::FilmcraftMcp::bridge(addr).unwrap_or_else(|e| fail(e)),
                None => match Backend::open(&a) {
                    Backend::Local(s) => filmcraft_automation::FilmcraftMcp::headless(*s),
                    Backend::Bridge(_) => usage("mcp: use --bridge ADDR to drive a running app"),
                },
            };
            if let Err(e) = server.serve_stdio().await {
                fail(format!("mcp: {e}"));
            }
        }
        other => usage(format!("unknown subcommand `{other}`")),
    }
}

#[cfg(test)]
mod format_tests {
    /// The CLI (and MCP / headless runs, which share its entry point) registers the hardware
    /// decoders at start-up.
    #[test]
    fn startup_registers_the_hardware_decoders() {
        let hardware = super::register_hardware_decoders();
        assert_eq!(filmcraft_platform::registered(), cfg!(any(target_os = "macos", target_os = "windows")), "{hardware:?}");
    }

    #[test]
    fn export_format_from_extension() {
        assert_eq!(super::format_for("out/clip.MXF"), Some("mxf-op1a"));
        assert_eq!(super::format_for("a.mov"), Some("prores"));
        assert_eq!(super::format_for("noext"), None);
    }
}
