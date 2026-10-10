//! Workspace automation: `cargo xtask <layers|wasm|ci>`.
//!
//! - `layers`: enforces the dependency layering of `docs/architecture.md` §1 (downward-only edges,
//!   listed same-layer edges, L0 codec crates depend on `bitstream` only, no UI/OS crates below L5).
//! - `wasm`: `cargo check --target wasm32-unknown-unknown` for every crate in L0–L4, the egui UI
//!   and the web app.
//! - `web [--dev] [--serve PORT]`: build the web app (`apps/filmcraft-web`) into
//!   `<target>/web/dist` with `wasm-bindgen` (docs/web.md); `--serve` serves it on localhost.
//! - `assets`: every asset file (image, icon, font, LUT, audio, video…) has a complete
//!   `<file>.attribution` sidecar and an entry in `ATTRIBUTION.md` (AGENTS.md §1).
//! - `fixtures [crate…]`: pre-generate the ffmpeg fixture matrix of the oracle tests (runs each
//!   crate's ignored `generate_fixtures` test, i.e. the same generators the tests use) and print
//!   what was made, reused or skipped.
//! - `ico <out.ico> <in.png>…`: pack PNGs into a Windows `.ico` (used by `packaging/icons.sh`).
//! - `ci`: fmt check, clippy -D warnings, tests, layers, assets, wasm.

mod ico;
mod version;

use std::process::{Command, ExitCode};

use serde_json::Value;

/// (crate name without the `filmcraft-` prefix, layer).
const LAYERS: &[(&str, u8)] = &[
    ("testkit", 0),
    ("time", 0),
    ("geom", 0),
    ("color", 0),
    ("bitstream", 0),
    ("isobmff", 0),
    ("matroska", 0),
    ("mxf", 0),
    ("cfb", 0),
    ("mpegts", 0),
    ("mpeg2v", 0),
    ("ac3", 0),
    ("ogg", 0),
    ("riff", 0),
    ("h264", 0),
    ("h264enc", 0),
    ("hevc", 0),
    ("vp9", 0),
    ("prores", 0),
    ("mjpeg", 0),
    ("dnx", 0),
    ("apv", 0),
    ("av1", 0),
    ("aac", 0),
    ("opus", 0),
    ("frame", 1),
    ("media", 1),
    ("project", 1),
    ("audio-dsp", 1),
    ("text", 1),
    ("edit", 2),
    ("codecs", 2),
    ("keyframe", 2),
    ("effects", 2),
    ("audio", 2),
    ("captions", 2),
    ("speech", 2),
    ("tts", 2),
    ("tts-text", 2),
    ("comfyui", 2),
    ("interchange", 2),
    ("render", 3),
    ("gpu", 3),
    ("golden", 3),
    ("scopes", 3),
    ("playback", 3),
    ("export", 3),
    ("format", 3),
    ("engine", 4),
    ("platform", 5),
    ("ui-egui", 5),
    ("automation", 5),
    ("filmcraft", 6),
    ("cli", 6),
    ("web", 6),
];

/// L0 crates that are not codecs/containers (no `bitstream`-only restriction).
const L0_FOUNDATION: &[&str] = &["time", "geom", "color", "bitstream", "testkit"];

/// Allowed same-layer edges (from, to).
const SAME_LAYER: &[(&str, &str)] = &[
    ("media", "frame"),
    ("project", "media"),
    ("project", "frame"),
    ("effects", "keyframe"),
    ("audio", "keyframe"),
    ("playback", "render"),
    ("playback", "gpu"),
    ("export", "render"),
    ("scopes", "gpu"),
    ("gpu", "render"),
    ("cli", "filmcraft"),
    ("tts", "tts-text"),
];

/// Crates that must not appear below L5 (UI toolkits, windowing, OS audio/menus).
const UI_ONLY: &[&str] = &["egui", "eframe", "egui-wgpu", "winit", "rfd", "cpal", "muda"];

fn short(name: &str) -> &str {
    name.strip_prefix("filmcraft-").unwrap_or(name)
}

fn layer_of(name: &str) -> Option<u8> {
    LAYERS.iter().find(|(n, _)| *n == short(name)).map(|(_, l)| *l)
}

fn metadata() -> Result<Value, String> {
    let out = Command::new(env!("CARGO")).args(["metadata", "--format-version", "1", "--no-deps"]).output().map_err(|e| e.to_string())?;
    if !out.status.success() {
        return Err(String::from_utf8_lossy(&out.stderr).into());
    }
    serde_json::from_slice(&out.stdout).map_err(|e| e.to_string())
}

fn workspace_crates(md: &Value) -> Vec<(String, Vec<String>)> {
    md["packages"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|p| p["name"] != "xtask")
        .map(|p| {
            let deps = p["dependencies"]
                .as_array()
                .into_iter()
                .flatten()
                .filter(|d| d["kind"].is_null() || d["kind"] == "build")
                .filter_map(|d| d["name"].as_str().map(str::to_string))
                .collect();
            (p["name"].as_str().unwrap_or_default().to_string(), deps)
        })
        .collect()
}

fn layers() -> Result<(), String> {
    let md = metadata()?;
    let mut errors = Vec::new();
    for (name, deps) in workspace_crates(&md) {
        let Some(l) = layer_of(&name) else {
            errors.push(format!("{name}: not assigned a layer (add it to xtask LAYERS and docs/architecture.md §1)"));
            continue;
        };
        for d in &deps {
            if l < 5 && UI_ONLY.contains(&d.as_str()) {
                errors.push(format!("{name} (L{l}) depends on UI/OS crate `{d}`"));
            }
            if !d.starts_with("filmcraft-") {
                continue;
            }
            let Some(dl) = layer_of(d) else { continue };
            let (a, b) = (short(&name), short(d));
            if l == 0 && !L0_FOUNDATION.contains(&a) && b != "bitstream" {
                errors.push(format!("{a} (L0 codec/container) may depend only on bitstream, found {b}"));
            } else if dl > l {
                errors.push(format!("{a} (L{l}) depends upward on {b} (L{dl})"));
            } else if dl == l && l > 0 && !SAME_LAYER.contains(&(a, b)) {
                errors.push(format!("{a} → {b}: same-layer edge (L{l}) not in the allowed list"));
            }
        }
    }
    if errors.is_empty() {
        println!("layers: ok");
        Ok(())
    } else {
        Err(errors.join("\n"))
    }
}

/// File extensions that count as assets (AGENTS.md §1).
const ASSET_EXT: &[&str] = &[
    "png", "jpg", "jpeg", "gif", "webp", "svg", "ico", "icns", "bmp", "tif", "tiff", "heic", "avif", "exr", "ttf", "otf", "ttc", "woff", "woff2", "cube",
    "3dl", "lut", "look", "wav", "mp3", "aac", "flac", "ogg", "opus", "m4a", "aif", "aiff", "mp4", "mov", "m4v", "mkv", "webm", "avi", "mxf", "psd", "ai",
    "eps", "pdf", "prproj", "ffx", "prfpset", "mogrt", "aep",
];

/// Sidecar fields that must be present and non-empty.
const REQUIRED_FIELDS: &[&str] = &["asset", "title", "author", "source", "license", "added"];

fn repo_files() -> Result<Vec<String>, String> {
    let out = Command::new("git").args(["ls-files", "--cached", "--others", "--exclude-standard"]).output().map_err(|e| e.to_string())?;
    if !out.status.success() {
        return Err(String::from_utf8_lossy(&out.stderr).into());
    }
    Ok(String::from_utf8_lossy(&out.stdout).lines().map(str::to_string).filter(|f| std::path::Path::new(f).exists()).collect())
}

fn is_asset(path: &str) -> bool {
    std::path::Path::new(path).extension().and_then(|e| e.to_str()).is_some_and(|e| ASSET_EXT.contains(&e.to_ascii_lowercase().as_str()))
}

fn assets() -> Result<(), String> {
    let files = repo_files()?;
    let index = std::fs::read_to_string("ATTRIBUTION.md").map_err(|e| format!("ATTRIBUTION.md: {e}"))?;
    let mut errors = Vec::new();
    let mut n = 0;
    for f in files.iter().filter(|f| is_asset(f)) {
        n += 1;
        let lower = f.to_ascii_lowercase();
        if lower.contains("adobe") || lower.contains("premiere") {
            errors.push(format!("{f}: asset paths must not reference Adobe/Premiere (AGENTS.md §1)"));
        }
        let side = format!("{f}.attribution");
        match std::fs::read_to_string(&side) {
            Err(_) => errors.push(format!("{f}: missing attribution sidecar {side}")),
            Ok(text) => {
                for field in REQUIRED_FIELDS {
                    let ok = text.lines().any(|l| l.split_once(':').is_some_and(|(k, v)| k.trim() == *field && !v.trim().is_empty()));
                    if !ok {
                        errors.push(format!("{side}: field `{field}` missing or empty"));
                    }
                }
                let lic = text.lines().find_map(|l| l.split_once(':').filter(|(k, _)| k.trim() == "license").map(|(_, v)| v.trim().to_ascii_lowercase()));
                if lic.is_some_and(|l| l.contains("-nc") || l.contains("-nd") || l.contains("adobe") || l.contains("proprietary")) {
                    errors.push(format!("{side}: licence not allowed (no NC/ND, Adobe or proprietary licences)"));
                }
            }
        }
        if !index.contains(&format!("`{f}`")) {
            errors.push(format!("{f}: not listed in ATTRIBUTION.md"));
        }
    }
    for f in files.iter().filter(|f| f.ends_with(".attribution")) {
        let asset = f.trim_end_matches(".attribution");
        if !std::path::Path::new(asset).exists() {
            errors.push(format!("{f}: sidecar for a file that does not exist"));
        }
    }
    if errors.is_empty() {
        println!("assets: ok ({n} assets attributed)");
        Ok(())
    } else {
        Err(errors.join("\n"))
    }
}

fn run(cmd: &mut Command) -> Result<(), String> {
    eprintln!("$ {cmd:?}");
    let st = cmd.status().map_err(|e| e.to_string())?;
    if st.success() { Ok(()) } else { Err(format!("failed: {cmd:?}")) }
}

/// Crates above L4 that must also build for the web.
const WEB_CRATES: &[&str] = &["filmcraft-ui-egui", "filmcraft-web"];

fn wasm() -> Result<(), String> {
    let md = metadata()?;
    let mut cmd = Command::new(env!("CARGO"));
    cmd.args(["check", "--target", "wasm32-unknown-unknown"]);
    let mut n = 0;
    for (name, _) in workspace_crates(&md) {
        if layer_of(&name).is_some_and(|l| l <= 4) || WEB_CRATES.contains(&name.as_str()) {
            cmd.args(["-p", &name]);
            n += 1;
        }
    }
    if n == 0 {
        return Ok(());
    }
    run(&mut cmd)?;
    println!("wasm: ok ({n} crates)");
    Ok(())
}

/// The wasm-bindgen CLI must match the `wasm-bindgen` crate version exactly.
const WASM_BINDGEN: &str = "0.2.129";

fn target_dir() -> std::path::PathBuf {
    std::env::var_os("CARGO_TARGET_DIR").map(std::path::PathBuf::from).unwrap_or_else(|| std::path::PathBuf::from("target"))
}

/// Build the web app into `<target>/web/dist` (see docs/web.md).
fn web(args: &[String]) -> Result<(), String> {
    let dev = args.iter().any(|a| a == "--dev");
    let serve = args.iter().position(|a| a == "--serve").map(|i| args.get(i + 1).and_then(|p| p.parse::<u16>().ok()).unwrap_or(8765));
    let profile = if dev { "dev" } else { "release" };
    let mut build = Command::new(env!("CARGO"));
    build.args(["build", "--target", "wasm32-unknown-unknown", "-p", "filmcraft-web", "--profile", profile]);
    run(&mut build)?;
    let out = Command::new("wasm-bindgen")
        .arg("--version")
        .output()
        .map_err(|_| format!("wasm-bindgen CLI not found: cargo install wasm-bindgen-cli --version {WASM_BINDGEN} --locked"))?;
    let v = String::from_utf8_lossy(&out.stdout);
    if !v.contains(WASM_BINDGEN) {
        return Err(format!(
            "wasm-bindgen CLI is {} but the crate is {WASM_BINDGEN}: cargo install wasm-bindgen-cli --version {WASM_BINDGEN} --locked",
            v.trim()
        ));
    }
    let dir = if dev { "debug" } else { "release" };
    let wasm = target_dir().join("wasm32-unknown-unknown").join(dir).join("filmcraft_web.wasm");
    let dist = target_dir().join("web").join("dist");
    let _ = std::fs::remove_dir_all(&dist);
    std::fs::create_dir_all(&dist).map_err(|e| e.to_string())?;
    run(Command::new("wasm-bindgen").args(["--target", "web", "--no-typescript", "--out-dir"]).arg(&dist).arg(&wasm))?;
    let bg = dist.join("filmcraft_web_bg.wasm");
    if !dev && Command::new("wasm-opt").arg("--version").output().is_ok() {
        // optional: smaller and faster (binaryen); skipped when not installed
        let opt = dist.join("filmcraft_web_opt.wasm");
        run(Command::new("wasm-opt")
            .args(["-O2", "--enable-bulk-memory", "--enable-nontrapping-float-to-int", "--enable-sign-ext", "--enable-mutable-globals"])
            .arg(&bg)
            .arg("-o")
            .arg(&opt))?;
        std::fs::rename(&opt, &bg).map_err(|e| e.to_string())?;
    }
    // index.html loads the glue and the wasm with `?v=<hash of this build>`: hosts cache them as
    // immutable (packaging/web/_headers), so a new build must change their URLs.
    let build = {
        let mut h: u64 = 0xcbf2_9ce4_8422_2325; // FNV-1a
        for f in ["filmcraft_web.js", "filmcraft_web_bg.wasm"] {
            for b in std::fs::read(dist.join(f)).map_err(|e| format!("{f}: {e}"))? {
                h = (h ^ b as u64).wrapping_mul(0x100_0000_01b3);
            }
        }
        format!("{h:016x}")
    };
    let web = std::path::Path::new("apps/filmcraft-web/web");
    for e in std::fs::read_dir(web).map_err(|e| e.to_string())?.flatten() {
        let name = e.file_name().to_string_lossy().to_string();
        if name == "index.html" {
            let html = std::fs::read_to_string(e.path()).map_err(|e| format!("{name}: {e}"))?;
            std::fs::write(dist.join(&name), html.replace("__FC_BUILD__", &build)).map_err(|e| format!("{name}: {e}"))?;
        } else if !name.ends_with(".attribution") {
            std::fs::copy(e.path(), dist.join(&name)).map_err(|e| format!("{name}: {e}"))?;
        }
    }
    let size = |p: &std::path::Path| std::fs::metadata(p).map(|m| m.len()).unwrap_or(0);
    let total: u64 = std::fs::read_dir(&dist).map_err(|e| e.to_string())?.flatten().map(|e| size(&e.path())).sum();
    println!("web: {} ({:.1} MB wasm, {:.1} MB total)", dist.display(), size(&bg) as f64 / 1e6, total as f64 / 1e6);
    if let Some(port) = serve {
        serve_dir(&dist, port)?;
    }
    Ok(())
}

/// A tiny static file server for the web build (localhost only). Sends the cross-origin
/// isolation headers so `crossOriginIsolated` is true, like a production deployment would.
fn serve_dir(dir: &std::path::Path, port: u16) -> Result<(), String> {
    use std::io::{BufRead, BufReader, Write};
    let listener = std::net::TcpListener::bind(("127.0.0.1", port)).map_err(|e| format!("127.0.0.1:{port}: {e}"))?;
    println!("serving {} on http://127.0.0.1:{port}/", dir.display());
    for stream in listener.incoming().flatten() {
        let dir = dir.to_path_buf();
        std::thread::spawn(move || {
            let mut stream = stream;
            let mut line = String::new();
            let mut reader = BufReader::new(match stream.try_clone() {
                Ok(s) => s,
                Err(_) => return,
            });
            if reader.read_line(&mut line).is_err() {
                return;
            }
            loop {
                let mut h = String::new();
                if reader.read_line(&mut h).map(|n| n == 0).unwrap_or(true) || h.trim().is_empty() {
                    break;
                }
            }
            let path = line.split_whitespace().nth(1).unwrap_or("/").split(['?', '#']).next().unwrap_or("/").to_string();
            let rel = if path == "/" { "index.html".to_string() } else { path.trim_start_matches('/').replace("..", "") };
            let file = dir.join(&rel);
            let (status, body) = match std::fs::read(&file) {
                Ok(b) => ("200 OK", b),
                Err(_) => ("404 Not Found", b"not found".to_vec()),
            };
            let mime = match file.extension().and_then(|e| e.to_str()).unwrap_or("") {
                "html" => "text/html; charset=utf-8",
                "js" => "text/javascript",
                "wasm" => "application/wasm",
                "svg" => "image/svg+xml",
                "png" => "image/png",
                "json" => "application/json",
                "mp4" => "video/mp4",
                "webm" => "video/webm",
                _ => "application/octet-stream",
            };
            let head = format!(
                "HTTP/1.1 {status}\r\nContent-Type: {mime}\r\nContent-Length: {}\r\nCross-Origin-Opener-Policy: same-origin\r\nCross-Origin-Embedder-Policy: require-corp\r\nCache-Control: no-cache\r\nConnection: close\r\n\r\n",
                body.len()
            );
            let _ = stream.write_all(head.as_bytes()).and_then(|_| stream.write_all(&body));
        });
    }
    Ok(())
}

/// (crate, test target) whose ignored `generate_fixtures` test builds that crate's fixture matrix.
const FIXTURE_GENERATORS: &[(&str, &str)] = &[
    ("h264", "conformance"),
    ("hevc", "conformance"),
    ("isobmff", "oracle_demux"),
    ("matroska", "oracle"),
    ("prores", "oracle_decode"),
    ("dnx", "oracle_decode"),
    ("codecs", "mxf_oracle"),
    ("codecs", "mpeg_oracle"),
    ("mpeg2v", "oracle"),
    ("ac3", "oracle"),
    ("codecs", "ogg_oracle"),
];

fn fixtures(only: &[String]) -> Result<(), String> {
    let unknown: Vec<&String> = only.iter().filter(|o| !FIXTURE_GENERATORS.iter().any(|(c, _)| *c == short(o))).collect();
    if !unknown.is_empty() {
        let known: Vec<&str> = FIXTURE_GENERATORS.iter().map(|(c, _)| *c).collect();
        return Err(format!("no fixture generator for {unknown:?} (known: {})", known.join(", ")));
    }
    let Some(ff) = filmcraft_testkit::ffmpeg() else {
        return Err("ffmpeg not found: set FILMCRAFT_FFMPEG=/path/to/ffmpeg or put ffmpeg on PATH".into());
    };
    println!("ffmpeg:   {}", ff.display());
    println!("fixtures: {}", filmcraft_testkit::fixtures::fixtures_root().display());
    let (mut made, mut cached, mut skipped) = (0, 0, 0);
    let mut failed = Vec::new();
    for (krate, target) in FIXTURE_GENERATORS.iter().filter(|(c, _)| only.is_empty() || only.iter().any(|o| short(o) == *c)) {
        let pkg = format!("filmcraft-{krate}");
        println!("\n== {pkg} ({target})");
        let mut cmd = Command::new(env!("CARGO"));
        cmd.args(["test", "--release", "-p", &pkg, "--test", target, "--", "--ignored", "--exact", "generate_fixtures", "--nocapture"]);
        cmd.stderr(std::process::Stdio::inherit());
        let out = cmd.output().map_err(|e| e.to_string())?;
        for line in String::from_utf8_lossy(&out.stdout).lines() {
            let Some(rest) = line.strip_prefix("FIXTURE ") else { continue };
            match rest.split_whitespace().next() {
                Some("made") => made += 1,
                Some("cached") => cached += 1,
                _ => skipped += 1,
            }
            println!("  {rest}");
        }
        if !out.status.success() {
            failed.push(pkg);
        }
    }
    println!("\nfixtures: {made} made, {cached} already present, {skipped} skipped");
    if failed.is_empty() { Ok(()) } else { Err(format!("fixture generation failed in {}", failed.join(", "))) }
}

fn ci() -> Result<(), String> {
    let cargo = env!("CARGO");
    run(Command::new(cargo).args(["fmt", "--check"]))?;
    run(Command::new(cargo).args(["clippy", "--workspace", "--all-targets", "--release", "--", "-D", "warnings"]))?;
    run(Command::new(cargo).args(["test", "--workspace", "--release"]))?;
    layers()?;
    assets()?;
    wasm()
}

/// The headless playback benchmark (`crates/ui-egui/examples/bench_playback.rs`); extra
/// arguments go to the benchmark (`cargo xtask bench-playback --scenario stack3 --res full`).
fn bench_playback() -> Result<(), String> {
    let extra: Vec<String> = std::env::args().skip(2).collect();
    run(Command::new(env!("CARGO")).args(["run", "--release", "-p", "filmcraft-ui-egui", "--example", "bench_playback", "--"]).args(&extra))
}

/// The benchmark suite (`crates/ui-egui/examples/bench/`): decode, playback, scrubbing, timeline
/// UI, export, project save/open and peak memory, written to `target/bench/bench-<label>.{json,md}`
/// (`cargo xtask bench --sections decode,scrub --repeat 3 --label after`). See docs/performance.md.
fn bench() -> Result<(), String> {
    let extra: Vec<String> = std::env::args().skip(2).collect();
    run(Command::new(env!("CARGO")).args(["run", "--release", "-p", "filmcraft-ui-egui", "--example", "bench", "--"]).args(&extra))
}

/// The workspace root (the parent of `xtask/`).
fn root() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).parent().expect("xtask lives in the workspace").to_path_buf()
}

fn main() -> ExitCode {
    let task = std::env::args().nth(1).unwrap_or_default();
    let r = match task.as_str() {
        "layers" => layers(),
        "wasm" => wasm(),
        "assets" => assets(),
        "fixtures" => fixtures(&std::env::args().skip(2).collect::<Vec<_>>()),
        "ci" => ci(),
        "bench" => bench(),
        "bench-playback" => bench_playback(),
        "web" => web(&std::env::args().skip(2).collect::<Vec<_>>()),
        "version" => {
            let rest: Vec<String> = std::env::args().skip(2).collect();
            version::run(&root(), &rest.iter().map(String::as_str).collect::<Vec<_>>())
        }
        "ico" => {
            let rest: Vec<String> = std::env::args().skip(2).collect();
            ico::run(&rest.iter().map(String::as_str).collect::<Vec<_>>())
        }
        _ => Err(
            "usage: cargo xtask <layers|assets|wasm|web [--dev] [--serve PORT]|fixtures [crate…]|ico OUT IN…|version [set X.Y.Z]|ci|bench [args]|bench-playback [args]>"
                .into(),
        ),
    };
    match r {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("{e}");
            ExitCode::FAILURE
        }
    }
}
