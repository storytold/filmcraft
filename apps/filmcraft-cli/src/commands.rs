//! The subcommands.

use std::path::{Path, PathBuf};

use filmcraft_engine::Session;
use serde_json::{Value, json};

use crate::backend::{Backend, path_param};
use crate::cli::{
    BenchDecodeArgs, Cli, CommandsArgs, DescribeArgs, ExecArgs, ExportArgs, Global, ImportArgs, InspectArgs, InspectTarget, ProbeArgs, RenderArgs, RunArgs,
    params_object,
};
use crate::error::{CliError, Failure, ScriptLineError, UsageError};
use crate::io::Io;

/// Read a file off the runtime thread: the handlers are async because MCP, bridge calls and
/// control commands await, so a blocking `std::fs` read would stall every other task on that thread.
async fn read_file(path: PathBuf) -> Result<Vec<u8>, Failure> {
    let p = path.clone();
    let r = tokio::task::spawn_blocking(move || std::fs::read(p)).await.map_err(std::io::Error::other).and_then(|r| r);
    r.map_err(|source| Failure::Io { path, source })
}

/// `read_file` for UTF-8 text.
async fn read_text(path: PathBuf) -> Result<String, Failure> {
    let p = path.clone();
    let r = tokio::task::spawn_blocking(move || std::fs::read_to_string(p)).await.map_err(std::io::Error::other).and_then(|r| r);
    r.map_err(|source| Failure::Io { path, source })
}

/// Write a file off the runtime thread; see `read_file`.
async fn write_file(path: PathBuf, data: Vec<u8>) -> Result<(), Failure> {
    let p = path.clone();
    let r = tokio::task::spawn_blocking(move || std::fs::write(p, data)).await.map_err(std::io::Error::other).and_then(|r| r);
    r.map_err(|source| Failure::Io { path, source })
}

/// `path` made absolute against the working directory, as the project stores it. Unlike
/// `canonicalize`, `std::path::absolute` keeps symlinks and adds no Windows verbatim `\\?\`
/// prefix (#850). A path it cannot resolve is kept as given.
pub fn absolute(path: &Path) -> PathBuf {
    std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf())
}

/// Export format from the output file's extension.
pub fn format_for(path: &Path) -> Option<&'static str> {
    let ext = path.extension()?.to_str()?.to_ascii_lowercase();
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
        "m4a" | "aac" => "aac",
        _ => return None,
    })
}

pub async fn probe(g: &Global, a: ProbeArgs, io: &mut Io<'_>) -> Result<(), CliError> {
    let v = crate::probe::probe(&a.media, a.image_sequence).map_err(Failure::from)?;
    io.json(g.compact, &v);
    Ok(())
}

pub async fn bench_decode(a: BenchDecodeArgs, io: &mut Io<'_>) -> Result<(), CliError> {
    let name = a.media.to_string_lossy().into_owned();
    let bytes = read_file(a.media.clone()).await?;
    let src = filmcraft_codecs::open_bytes(&name, bytes.into()).map_err(|e| Failure::Media(e.into()))?;
    let rate = src.info().frame_rate();
    let t0 = std::time::Instant::now();
    for f in 0..a.frames {
        src.video_frame(filmcraft_media::FrameRequest::full(rate.tick_of(f))).map_err(|e| Failure::Media(e.into()))?;
    }
    let dt = t0.elapsed().as_secs_f64();
    let n = a.frames;
    io.line(format_args!("{n} frames in {dt:.2}s → {:.1} fps (sequential, via Mp4Source)", n as f64 / dt));
    let t1 = std::time::Instant::now();
    let f = src.video_frame(filmcraft_media::FrameRequest::full(rate.tick_of(n / 2))).map_err(|e| Failure::Media(e.into()))?;
    let (w, h, _) = f.to_linear_f32_decimated(2);
    io.line(format_args!("½-res linear conversion {w}x{h}: {:.1} ms", t1.elapsed().as_secs_f64() * 1000.0));
    Ok(())
}

async fn command_list(b: &mut Backend) -> Result<Vec<Value>, Failure> {
    let list = b.exec("command.list", json!({})).await?;
    Ok(list.as_array().cloned().unwrap_or_default())
}

pub async fn commands(g: &Global, a: CommandsArgs, io: &mut Io<'_>) -> Result<(), CliError> {
    let mut b = Backend::open(g)?;
    let list = command_list(&mut b).await?;
    let f = a.filter.as_deref().map(str::to_ascii_lowercase).unwrap_or_default();
    let hit = |c: &Value| {
        let id = c["id"].as_str().unwrap_or("").to_ascii_lowercase();
        let label = c["label"].as_str().unwrap_or("").to_ascii_lowercase();
        f.is_empty() || id.contains(&f) || label.contains(&f)
    };
    let list: Vec<Value> = list.into_iter().filter(hit).collect();
    if a.json {
        io.json(g.compact, &Value::Array(list));
    } else {
        for c in &list {
            let s = |k: &str| c[k].as_str().unwrap_or("").to_string();
            io.line(format_args!("{:<32} {:<34} {:<14} {}", s("id"), s("label"), s("shortcut"), s("params")));
        }
    }
    Ok(())
}

pub async fn describe(g: &Global, a: DescribeArgs, io: &mut Io<'_>) -> Result<(), CliError> {
    let mut b = Backend::open(g)?;
    let list = command_list(&mut b).await?;
    match list.iter().find(|c| c["id"] == a.id.as_str()) {
        Some(c) => io.json(g.compact, c),
        None => return Err(Failure::UnknownCommand(a.id).into()),
    }
    Ok(())
}

pub async fn exec(g: &Global, a: ExecArgs, io: &mut Io<'_>) -> Result<(), CliError> {
    let params = params_object(&a.params)?;
    let mut b = Backend::open(g)?;
    match b.exec(&a.id, params).await {
        Ok(v) => io.json(g.compact, &v),
        Err(source) => return Err(Failure::Exec { id: a.id, source }.into()),
    }
    b.finish(g).await?;
    Ok(())
}

pub async fn run(g: &Global, a: RunArgs, io: &mut Io<'_>) -> Result<(), CliError> {
    let text =
        if a.script == Path::new("-") { std::io::read_to_string(std::io::stdin()).map_err(Failure::Stdin)? } else { read_text(a.script.clone()).await? };
    let mut b = Backend::open(g)?;
    let mut failed = 0;
    for (n, line) in text.lines().enumerate().filter(|(_, l)| !l.trim().is_empty() && !l.trim_start().starts_with('#')) {
        let line_no = n + 1;
        let script_line = |reason: ScriptLineError| UsageError::ScriptLine { line: line_no, reason };
        let v: Value = serde_json::from_str(line).map_err(|e| script_line(e.into()))?;
        let id = v["id"].as_str().ok_or_else(|| script_line(ScriptLineError::MissingId))?;
        match b.exec(id, v.get("params").cloned().unwrap_or(json!({}))).await {
            Ok(r) => io.line(format_args!("{}", json!({"line": line_no, "id": id, "ok": true, "result": r}))),
            Err(e) => {
                io.line(format_args!("{}", json!({"line": line_no, "id": id, "ok": false, "error": e.to_string()})));
                failed += 1;
                if !a.keep_going {
                    return Err(Failure::Script { line: line_no, id: id.to_string(), source: e }.into());
                }
            }
        }
    }
    b.finish(g).await?;
    if failed > 0 {
        return Err(Failure::ScriptFailed { count: failed }.into());
    }
    Ok(())
}

pub async fn inspect(g: &Global, a: InspectArgs, io: &mut Io<'_>) -> Result<(), CliError> {
    let mut b = Backend::open(g)?;
    let v = match a.what {
        Some(InspectTarget::Project) => b.exec("project.inspect", json!({})).await.map_err(Failure::from)?,
        Some(InspectTarget::Sequence) => b.exec("sequence.inspect", json!({})).await.map_err(Failure::from)?,
        Some(InspectTarget::All) | None => {
            let project = b.exec("project.inspect", json!({})).await.map_err(Failure::from)?;
            let sequence = b.exec("sequence.inspect", json!({})).await.map_err(Failure::from)?;
            json!({"project": project, "sequence": sequence})
        }
    };
    io.json(g.compact, &v);
    Ok(())
}

pub async fn import(g: &Global, a: ImportArgs, io: &mut Io<'_>) -> Result<(), CliError> {
    let paths: Vec<Value> = a.files.iter().map(|p| path_param(&absolute(p))).collect();
    let mut b = Backend::open(g)?;
    match b.exec("file.import", json!({"paths": paths})).await {
        Ok(v) => io.json(g.compact, &v),
        Err(e) => return Err(Failure::Import(e).into()),
    }
    b.finish(g).await?;
    Ok(())
}

pub async fn export(g: &Global, a: ExportArgs, io: &mut Io<'_>) -> Result<(), CliError> {
    if a.list_presets {
        let mut b = Backend::open(g)?;
        let q = a.out.as_ref().map(|q| json!({"query": q.to_string_lossy()})).unwrap_or(json!({}));
        let v = b.exec("export.presets.list", q).await.map_err(Failure::Export)?;
        io.json(g.compact, &v["presets"]);
        return Ok(());
    }
    // clap requires `out` unless --list-presets
    let Some(out) = a.out.clone() else { return Ok(()) };
    let mut p = json!({"path": path_param(&out), "wait": true});
    match (&a.preset, &a.format) {
        (Some(preset), f) => {
            p["preset"] = json!(preset);
            if let Some(f) = f {
                p["format"] = json!(f);
            }
        }
        (None, f) => {
            let format = f.as_deref().or_else(|| format_for(&out)).ok_or(UsageError::UnknownExportFormat)?;
            p["format"] = json!(format);
        }
    }
    if let Some(v) = a.scale {
        p["scale"] = json!(v);
    }
    if let Some(v) = a.quality {
        p["quality"] = json!(v);
    }
    if let Some(r) = a.range {
        p["range"] = json!(r.id());
    }
    set_custom_bounds(&mut p, a.start, a.end);
    if let Some(settings) = &a.settings {
        p["settings"] = settings.clone();
    }
    if a.no_audio {
        p["audio"] = json!(false);
    }
    if let Some(v) = a.gpu_rendering {
        p["gpuRendering"] = json!(v.id());
    }
    let mut b = Backend::open(g)?;
    let t0 = std::time::Instant::now();
    let r = if a.queue {
        let mut q = p.clone();
        q["start"] = json!(true);
        match b.exec("export.queue.add", q).await {
            Ok(v) => {
                let failed: Vec<Value> = v["items"].as_array().map(|i| i.iter().filter(|x| x["status"] != "done").cloned().collect()).unwrap_or_default();
                if failed.is_empty() { Ok(v) } else { Err(Failure::Queue(Value::Array(failed).to_string())) }
            }
            Err(e) => Err(Failure::Export(e)),
        }
    } else {
        b.exec("file.exportMedia", p).await.map_err(Failure::Export)
    };
    let v = r?;
    io.json(g.compact, &v);
    let written = written_paths(&v, &out.to_string_lossy()).join(", ");
    for warning in v.get("warnings").and_then(Value::as_array).into_iter().flatten().filter_map(Value::as_str) {
        io.warning(format_args!("{warning} (CLI: --range entire)"));
    }
    check_offline(a.allow_offline, &offline_media(&v), &format!("export: {written}"), io)?;
    io.diag(format_args!("exported {written} in {:.1}s", t0.elapsed().as_secs_f64()));
    Ok(())
}

pub async fn render(g: &Global, a: RenderArgs, io: &mut Io<'_>) -> Result<(), CliError> {
    if g.bridge.is_some() {
        return Err(UsageError::HeadlessOnly("render").into());
    }
    let Backend::Local(mut s) = Backend::open(g)? else { return Err(UsageError::HeadlessOnly("render").into()) };
    s.set_playhead(filmcraft_time::Tick::from_seconds_f64(a.seconds));
    let t0 = std::time::Instant::now();
    let (img, offline) = render_at_playhead(&s, a.scale).map_err(|e| Failure::Render(e.into()))?;
    let dt = t0.elapsed();
    let png = filmcraft_automation::png_rgba(img.w as u32, img.h as u32, img.over_black_rgba8(), 0)
        .map_err(|source| Failure::Png { path: a.out.display().to_string(), source })?;
    write_file(a.out.clone(), png).await?;
    let offline: Vec<Value> = offline.iter().filter_map(|o| serde_json::to_value(o).ok()).collect();
    let out = a.out.display().to_string();
    check_offline(a.allow_offline, &offline, &format!("render: {out}"), io)?;
    io.diag(format_args!("rendered {}x{} in {:.1} ms → {out}", img.w, img.h, dt.as_secs_f64() * 1000.0));
    Ok(())
}

pub async fn mcp(g: &Global) -> Result<(), CliError> {
    let server = match &g.bridge {
        Some(addr) => filmcraft_automation::FilmcraftMcp::bridge(&addr.0).map_err(UsageError::Bridge)?,
        None => match Backend::open(g)? {
            Backend::Local(s) => filmcraft_automation::FilmcraftMcp::headless(*s),
            Backend::Bridge(_) => return Err(UsageError::McpNeedsBridge.into()),
        },
    };
    server.serve_stdio().await.map_err(Failure::Mcp)?;
    Ok(())
}

/// The files an export wrote, from its result: `path` of a direct export, or each queued item's
/// `path` (the format may have changed the extension: `out.mp4` exported as APV is `out.mp4.mov`).
/// `requested` when the result names none.
pub fn written_paths(result: &Value, requested: &str) -> Vec<String> {
    let paths: Vec<String> = match result.get("path").and_then(Value::as_str) {
        Some(p) => vec![p.to_string()],
        None => result
            .get("items")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|i| i.get("path").and_then(Value::as_str))
            .map(str::to_string)
            .collect(),
    };
    if paths.is_empty() { vec![requested.to_string()] } else { paths }
}

/// The offline items an export result reports (`offlineMedia` of a direct export, or of each
/// queued item), once each.
pub fn offline_media(result: &Value) -> Vec<Value> {
    let lists = std::iter::once(result).chain(result.get("items").and_then(Value::as_array).into_iter().flatten());
    let mut out: Vec<Value> = Vec::new();
    for o in lists.filter_map(|r| r.get("offlineMedia").and_then(Value::as_array)).flatten() {
        if !out.iter().any(|x| x.get("item") == o.get("item")) {
            out.push(o.clone());
        }
    }
    out
}

/// One stderr line per offline item that rendered as placeholder frames (#1054).
pub fn offline_warning(offline: &[Value]) -> String {
    let s = |o: &Value, k: &str| o.get(k).and_then(Value::as_str).unwrap_or("").to_string();
    let mut msg = format!("{} offline media item(s) rendered as placeholder frames:", offline.len());
    for o in offline {
        let id = o.get("item").and_then(Value::as_u64).map(|i| i.to_string()).unwrap_or_default();
        msg.push_str(&format!("\n  item {id} {} ({}): {}", s(o, "name"), s(o, "status"), s(o, "path")));
    }
    msg
}

/// Warn about offline media on stderr; without `--allow-offline` that fails the run (exit 1), after
/// the output (`what`) was written, so scripts don't take placeholder frames for the real picture.
fn check_offline(allow: bool, offline: &[Value], what: &str, io: &mut Io<'_>) -> Result<(), Failure> {
    if offline.is_empty() {
        return Ok(());
    }
    io.warning(format_args!("{}", offline_warning(offline)));
    if allow { Ok(()) } else { Err(Failure::Offline { what: what.to_string() }) }
}

/// The active sequence at the playhead with the offline items it drew, or the reason it cannot be
/// rendered (no sequence, bad scale).
pub fn render_at_playhead(
    s: &Session,
    scale: f32,
) -> Result<(filmcraft_engine::render::Image, Vec<filmcraft_engine::media_pool::OfflineMedia>), filmcraft_engine::EngineError> {
    s.try_render_program_with_offline_at(scale, s.playhead())
}

/// `--start` / `--end` ask for a custom range. A lone bound is forwarded too, so the engine reports the missing one
/// instead of the export silently running over the whole sequence.
pub fn set_custom_bounds(p: &mut Value, start: Option<f64>, end: Option<f64>) {
    if start.is_none() && end.is_none() {
        return;
    }
    p["range"] = json!("custom");
    if let Some(s0) = start {
        p["startSeconds"] = json!(s0);
    }
    if let Some(s1) = end {
        p["endSeconds"] = json!(s1);
    }
}

/// Run `cli`'s subcommand.
pub async fn dispatch(cli: Cli, io: &mut Io<'_>) -> Result<(), CliError> {
    use crate::cli::Command;
    let Cli { global: g, command } = cli;
    crate::register_hardware_decoders();
    crate::register_gpu_frame_renderer();
    match command {
        Command::Probe(a) => probe(&g, a, io).await,
        Command::BenchDecode(a) => bench_decode(a, io).await,
        Command::Commands(a) => commands(&g, a, io).await,
        Command::Describe(a) => describe(&g, a, io).await,
        Command::Exec(a) => exec(&g, a, io).await,
        Command::Run(a) => run(&g, a, io).await,
        Command::Inspect(a) => inspect(&g, a, io).await,
        Command::Import(a) => import(&g, a, io).await,
        Command::Export(a) => export(&g, a, io).await,
        Command::Render(a) => render(&g, a, io).await,
        Command::Mcp => mcp(&g).await,
    }
}
