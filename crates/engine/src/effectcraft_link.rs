//! Replace With EffectCraft Composition: FilmCraft's take on Premiere's Replace With After Effects
//! Composition (Dynamic Link), built on EffectCraft's headless CLI instead of an in-process link.
//!
//! `effectcraft.replaceClip` takes a clip, writes an `.ecproj` whose composition matches the
//! clip's footage (size, frame rate, duration) with that footage as its only layer, renders the
//! composition to a ProRes 4444 movie next to it, points the clip (and its linked partners) at the
//! render, and opens the project in EffectCraft. Saving a change in EffectCraft bumps the project
//! file's modification time; [`Session::start_effectcraft_sync`] sees that, renders again to a
//! temporary file and renames it over the movie, and Settings ▸ Media ▸ Automatically refresh
//! growing files ([`crate::media_watch`]) reads the new movie. `effectcraft.edit` opens the project
//! of a linked clip again.
//!
//! The two apps meet only through files and processes (`effectcraft-cli`, `effectcraft`), so
//! FilmCraft carries no dependency on EffectCraft, which already depends on FilmCraft's codecs.
//! The tools are found from the `cli` / `app` parameters, then `EFFECTCRAFT_CLI` / `EFFECTCRAFT_APP`,
//! then the `PATH`. Links live for the session; a reopened project keeps the rendered movie as
//! ordinary footage, and `effectcraft.link` links it to its project again.

use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex, PoisonError};

use filmcraft_export::{Progress, Report};
use filmcraft_project::{ClipId, ItemId, Label, MediaRef};
use filmcraft_time::Tick;
use serde_json::{Value, json};

use crate::commands::{CommandSpec, bad, bool_p, has_seq, str_p, u64_p};
use crate::media_watch::Stamp;
use crate::{EngineError, Result, Session};

/// A clip's footage replaced by the render of an EffectCraft composition.
#[derive(Clone, Debug, PartialEq)]
pub struct EcLink {
    /// The `.ecproj` file.
    pub project: String,
    /// The composition rendered, by name.
    pub comp: String,
    /// The movie the clips use.
    pub render: String,
    /// The project file's size and modification time when the movie was last rendered.
    pub stamp: Option<Stamp>,
    /// The tools used for this link (`None` = find them as usual).
    pub cli: Option<String>,
    pub app: Option<String>,
}

/// Renders finished on a worker thread: (project, stamp rendered from, the movie written or why not).
pub type EcSync = Arc<Mutex<Option<Vec<(String, Option<Stamp>, std::result::Result<String, String>)>>>>;

/// Characters a file name can't hold (or that make a command line awkward) become `_`.
fn safe_stem(name: &str) -> String {
    let s: String = name.chars().map(|c| if c.is_alphanumeric() || matches!(c, ' ' | '-' | '_' | '.') { c } else { '_' }).collect();
    let s = s.trim().trim_matches('.').to_string();
    if s.is_empty() { "Clip".into() } else { s.chars().take(80).collect() }
}

/// The tool to run: the parameter, then the environment variable, then a copy beside the running
/// program or in an `EffectCraft/target/release` folder next to one of its parent folders (a
/// side-by-side checkout), then the bare name for the `PATH`.
fn find_tool(param: Option<&str>, env: &str, default: &str) -> String {
    if let Some(p) = param {
        return p.to_string();
    }
    if let Some(v) = std::env::var(env).ok().filter(|v| !v.trim().is_empty()) {
        return v;
    }
    let file = format!("{default}{}", std::env::consts::EXE_SUFFIX);
    let exe_dir = std::env::current_exe().ok().and_then(|e| e.parent().map(Path::to_path_buf));
    let beside = exe_dir.iter().map(|d| d.join(&file));
    let checkouts = exe_dir
        .iter()
        .flat_map(|d| d.ancestors().skip(1).take(4).map(|a| a.join("EffectCraft").join("target").join("release").join(&file)).collect::<Vec<_>>());
    beside.chain(checkouts).find(|p| p.is_file()).map_or_else(|| default.to_string(), |p| p.to_string_lossy().into_owned())
}

/// Run `effectcraft-cli` with `args`; its stderr (or stdout) is the error when it fails.
#[cfg(not(target_arch = "wasm32"))]
fn run_cli(cli: &str, args: &[String]) -> std::result::Result<(), String> {
    cli_output(cli, args).map(|_| ())
}

/// Run `effectcraft-cli` and return what it printed on stdout.
#[cfg(not(target_arch = "wasm32"))]
fn cli_output(cli: &str, args: &[String]) -> std::result::Result<String, String> {
    let out = cli_command(cli, args).output().map_err(|e| format!("can't run {cli}: {e} (set EFFECTCRAFT_CLI or the `cli` parameter to effectcraft-cli)"))?;
    if out.status.success() {
        return Ok(String::from_utf8_lossy(&out.stdout).into_owned());
    }
    let text = String::from_utf8_lossy(if out.stderr.is_empty() { &out.stdout } else { &out.stderr }).trim().to_string();
    Err(format!("effectcraft-cli failed ({}): {}", out.status, text.chars().take(600).collect::<String>()))
}

#[cfg(target_arch = "wasm32")]
fn cli_output(_: &str, _: &[String]) -> std::result::Result<String, String> {
    Err("EffectCraft links need the desktop app".into())
}

/// The command for `effectcraft-cli` with `args`, with no console window.
#[cfg(not(target_arch = "wasm32"))]
fn cli_command(cli: &str, args: &[String]) -> std::process::Command {
    let mut cmd = std::process::Command::new(cli);
    cmd.args(args).stdin(std::process::Stdio::null());
    // A console program started by a window app would open a console window of its own, one that
    // looks stuck (the CLI prints nothing while it renders) and stops the render when it is closed.
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    cmd
}

/// Run `effectcraft-cli` and hand its `progress <done> <total> <elapsed> <left>` lines (see
/// `render --progress`) to `on_progress` as they come: (frames done, frames total, seconds left).
#[cfg(not(target_arch = "wasm32"))]
fn run_cli_progress(cli: &str, args: &[String], mut on_progress: impl FnMut(u64, u64, Option<f64>)) -> std::result::Result<(), String> {
    use std::io::{BufRead, BufReader, Read};
    let mut child = cli_command(cli, args)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| format!("can't run {cli}: {e} (set EFFECTCRAFT_CLI or the `cli` parameter to effectcraft-cli)"))?;
    // Read stdout too, so a full pipe can't stall the program.
    let stdout = child.stdout.take();
    let drain = std::thread::spawn(move || {
        if let Some(mut o) = stdout {
            let _ = o.read_to_end(&mut Vec::new());
        }
    });
    let mut tail = String::new();
    if let Some(err) = child.stderr.take() {
        for line in BufReader::new(err).lines().map_while(std::result::Result::ok) {
            if let Some((done, total, left)) = parse_progress(&line) {
                on_progress(done, total, left);
            } else {
                tail.push_str(&line);
                tail.push('\n');
            }
        }
    }
    let status = child.wait().map_err(|e| format!("effectcraft-cli: {e}"))?;
    let _ = drain.join();
    if status.success() {
        return Ok(());
    }
    let from = tail.char_indices().rev().nth(600).map_or(0, |(i, _)| i);
    Err(format!("effectcraft-cli failed ({status}): {}", tail[from..].trim()))
}

#[cfg(target_arch = "wasm32")]
fn run_cli_progress(_: &str, _: &[String], _: impl FnMut(u64, u64, Option<f64>)) -> std::result::Result<(), String> {
    Err("EffectCraft links need the desktop app".into())
}

/// `progress <done> <total> <elapsed> <left or ->` (what `effectcraft-cli render --progress`
/// prints) as (frames done, frames total, seconds left).
#[cfg(not(target_arch = "wasm32"))]
fn parse_progress(line: &str) -> Option<(u64, u64, Option<f64>)> {
    let mut f = line.strip_prefix("progress ")?.split_whitespace();
    let done = f.next()?.parse().ok()?;
    let total = f.next()?.parse().ok()?;
    let left = f.nth(1).and_then(|v| v.parse::<f64>().ok());
    Some((done, total, left))
}

/// "42 s", "3 min 05 s".
fn duration_text(secs: f64) -> String {
    let n = secs.max(0.0).round() as u64;
    if n < 60 { format!("{n} s") } else { format!("{} min {:02} s", n / 60, n % 60) }
}

fn set_status(p: &Progress, text: impl Into<String>) {
    *p.status.lock().unwrap_or_else(PoisonError::into_inner) = text.into();
}

/// What the render reports: frames done of total, and the time left when the CLI can tell.
fn report_progress(p: &Progress, done: u64, total: u64, left: Option<f64>) {
    let total = total.max(1);
    let done = done.min(total);
    p.total.store(total, Ordering::Relaxed);
    p.done.store(done, Ordering::Relaxed);
    let pct = done.saturating_mul(100) / total;
    let left = left.map(|l| format!(", about {} left", duration_text(l))).unwrap_or_default();
    set_status(p, format!("Rendering {pct}% (frame {done} of {total}{left})"));
}

/// The Jobs list's entry for a background render: it shows in the Events and Progress panels, and
/// in the status line while it runs.
type Outcome = Arc<Mutex<Option<std::result::Result<Report, String>>>>;
const JOB_PREFIX: &str = "EffectCraft: ";

fn register_job(s: &mut Session, name: &str, status: &str) -> (Arc<Progress>, Outcome) {
    let id = s.jobs.iter().map(|j| j.id).max().unwrap_or(0) + 1;
    let job = crate::Job { id, label: format!("{JOB_PREFIX}{name}"), progress: Default::default(), result: Default::default() };
    // Until the CLI reports frames, a bar that has not started.
    job.progress.total.store(1000, Ordering::Relaxed);
    set_status(&job.progress, status);
    let handles = (job.progress.clone(), job.result.clone());
    s.jobs.push(job);
    handles
}

/// Close a job entry with what happened (`Ok` = the movie written).
fn close_job(p: &Progress, result: &Outcome, outcome: &std::result::Result<String, String>, seconds: f64) {
    let r = match outcome {
        Ok(path) => {
            let total = p.total.load(Ordering::Relaxed);
            p.done.store(total, Ordering::Relaxed);
            set_status(p, format!("Done in {}", duration_text(seconds)));
            let bytes = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
            Ok(Report { path: path.clone(), frames: total, seconds, bytes, render_fps: 0.0, extra_files: Vec::new() })
        }
        Err(e) => {
            *p.error.lock().unwrap_or_else(PoisonError::into_inner) = Some(e.clone());
            set_status(p, e.clone());
            Err(e.clone())
        }
    };
    p.finished.store(true, Ordering::Relaxed);
    *result.lock().unwrap_or_else(PoisonError::into_inner) = Some(r);
}

/// Run `work`, turning a panic into an error so a job never stays "running" for good.
fn guarded<T>(work: impl FnOnce() -> std::result::Result<T, String>) -> std::result::Result<T, String> {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(work)).unwrap_or_else(|_| Err("internal error (see the crash log); the render was stopped".into()))
}

#[cfg(target_arch = "wasm32")]
fn run_cli(_: &str, _: &[String]) -> std::result::Result<(), String> {
    Err("EffectCraft links need the desktop app".into())
}

/// `out` with " v2", " v3"... before its extension: the first name nothing uses yet.
fn variant_of(out: &str) -> String {
    let path = Path::new(out);
    let stem = path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    // "Clip EffectCraft v3" -> "Clip EffectCraft": the number counts up from the original name.
    let base = match stem.rsplit_once(" v") {
        Some((head, tail)) if !tail.is_empty() && tail.bytes().all(|b| b.is_ascii_digit()) => head.to_string(),
        _ => stem,
    };
    let ext = path.extension().map(|e| format!(".{}", e.to_string_lossy())).unwrap_or_default();
    let dir = path.parent().map(Path::to_path_buf).unwrap_or_default();
    (2..10_000)
        .map(|n| dir.join(format!("{base} v{n}{ext}")))
        .find(|p| !p.exists())
        .unwrap_or_else(|| dir.join(format!("{base} v{}{ext}", std::process::id())))
        .to_string_lossy()
        .into_owned()
}

/// Move the finished render `tmp` to `out`. A movie FilmCraft is playing can't be replaced on
/// Windows ("access denied"), so unless `replace` is set (and works) the render goes to a numbered
/// name next to it. Returns the path written.
fn place_render(tmp: &Path, out: &str, replace: bool) -> std::result::Result<String, String> {
    if !Path::new(out).exists() || replace {
        match std::fs::rename(tmp, out) {
            Ok(()) => return Ok(out.to_string()),
            // Open in a player: keep the old movie and write the new one beside it.
            Err(_) if Path::new(out).exists() => {}
            Err(e) => return Err(format!("can't move the render to {out}: {e}")),
        }
    }
    let alt = variant_of(out);
    std::fs::rename(tmp, &alt).map(|()| alt.clone()).map_err(|e| format!("can't move the render to {alt}: {e}"))
}

pub(crate) fn render_comp(cli: &str, project: &str, comp: &str, out: &str, replace: bool, progress: Option<&Progress>) -> std::result::Result<String, String> {
    let tmp = Path::new(out).with_extension("partial.mov");
    let tmp_s = tmp.to_string_lossy().into_owned();
    let args: Vec<String> =
        ["render", project, "--comp", comp, "--out", &tmp_s, "--format", "prores", "--prores", "4444", "--channels", "rgba", "--audio", "auto"]
            .iter()
            .map(|a| a.to_string())
            .collect();
    let rendered = match progress {
        Some(p) => {
            let mut with = args.clone();
            with.push("--progress".into());
            // An effectcraft-cli from before `--progress` still renders, without the bar.
            run_cli_progress(cli, &with, |d, t, l| report_progress(p, d, t, l))
                .or_else(|e| if e.contains("unknown option") { run_cli(cli, &args) } else { Err(e) })
        }
        None => run_cli(cli, &args),
    };
    let r = rendered.and_then(|()| place_render(&tmp, out, replace));
    if r.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    r
}

/// Open `project` in the EffectCraft app (detached; a failure is reported, not fatal).
#[cfg(not(target_arch = "wasm32"))]
fn open_app(app: &str, project: &str) -> std::result::Result<(), String> {
    std::process::Command::new(app)
        .arg(project)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map(|_| ())
        .map_err(|e| format!("can't open {app}: {e} (set EFFECTCRAFT_APP or the `app` parameter to the effectcraft program)"))
}

#[cfg(target_arch = "wasm32")]
fn open_app(_: &str, _: &str) -> std::result::Result<(), String> {
    Err("EffectCraft links need the desktop app".into())
}

fn stamp_of(path: &str) -> Option<Stamp> {
    crate::media_watch::stamp(path)
}

/// The clip `p` names, else the only selected clip.
fn target_clip(s: &Session, p: &Value, cmd: &str) -> Result<ClipId> {
    if let Some(c) = u64_p(p, "clip") {
        return Ok(ClipId(c));
    }
    match s.state.selection.as_slice() {
        [one] => Ok(*one),
        [] => Err(bad(cmd, "select a clip (or pass `clip`)")),
        _ => Err(bad(cmd, "select one clip (or pass `clip`)")),
    }
}

/// The footage item of `clip`, its file and its name in the active sequence.
fn clip_source(s: &Session, clip: ClipId, cmd: &str) -> Result<(ItemId, String, String)> {
    let q = s.active_sequence().ok_or(EngineError::NoSequence)?;
    let (_, it) = q.find_item(clip).ok_or_else(|| bad(cmd, format!("no clip {}", clip.0)))?;
    let m = s
        .project
        .item(it.item)
        .and_then(|i| i.as_media())
        .ok_or_else(|| bad(cmd, "the clip is not footage (generators, titles and nests can't be replaced)"))?;
    match &m.media {
        MediaRef::File { path } => Ok((it.item, path.clone(), it.name.clone())),
        _ => Err(bad(cmd, "the clip's media is not a file")),
    }
}

/// Where the project and render go: `dir`, else next to the saved FilmCraft project, else the
/// system temporary folder.
fn link_dir(s: &Session, p: &Value) -> PathBuf {
    if let Some(d) = str_p(p, "dir") {
        return PathBuf::from(d);
    }
    match s.path.as_deref().and_then(|x| Path::new(x).parent()) {
        Some(parent) => parent.join("EffectCraft Linked Media"),
        None => std::env::temp_dir().join("FilmCraft EffectCraft Linked Media"),
    }
}

pub struct EcJob {
    clip: ClipId,
    sequence: ItemId,
    old_item: ItemId,
    /// Where in the footage the composition starts: the render is the part of the footage the
    /// clip uses, so the clip's media times move back by this.
    offset: Tick,
    project: String,
    comp: String,
    render: String,
    /// The `cli` / `app` parameters as given (kept in the link), and the app found.
    cli_param: Option<String>,
    app_param: Option<String>,
    app: String,
    open: bool,
    /// Show the new files in the Media Browser when the clip is replaced.
    reveal: bool,
    /// The render's progress while it runs in the background (the timeline's clip overlay).
    progress: Option<Arc<Progress>>,
    /// Set by the worker thread when the project is written and rendered.
    done: Arc<Mutex<Option<std::result::Result<String, String>>>>,
}

/// The project to write and the movie to render from it (what `effectcraft.replaceClip` works out
/// before the slow part, so a worker thread can run [`build_and_render`] on its own).
struct BuildPlan {
    /// `comp.new` and `file.import` / `layer.addItem` parameters (JSON).
    new_comp: String,
    import: String,
    add_layer: String,
    project: String,
    comp: String,
    render: String,
    /// Replace an existing movie instead of writing the next numbered one ([`place_render`]).
    replace: bool,
}

fn build_and_render(cli: &str, plan: &BuildPlan, progress: Option<&Progress>) -> std::result::Result<String, String> {
    if let Some(p) = progress {
        set_status(p, "Writing the EffectCraft project...");
    }
    let args: Vec<String> =
        ["run", "--empty", "--save-as", &plan.project, "comp.new", &plan.new_comp, "file.import", &plan.import, "layer.addItem", &plan.add_layer]
            .iter()
            .map(|a| a.to_string())
            .collect();
    run_cli(cli, &args)?;
    if let Some(p) = progress {
        set_status(p, "Rendering...");
    }
    render_comp(cli, &plan.project, &plan.comp, &plan.render, plan.replace, progress)
}

/// The first `{base}-{n}` (n from 2) with neither a project nor a movie yet.
fn free_stem(dir: &Path, base: &str) -> String {
    (2..10_000)
        .map(|n| format!("{base}-{n}"))
        .find(|s| !dir.join(format!("{s}.ecproj")).exists() && !dir.join(format!("{s}.mov")).exists())
        .unwrap_or_else(|| format!("{base}-{}", std::process::id()))
}

/// `effectcraft.replaceClip {clip?, dir?, cli?, app?, open?: bool = true, background?: bool = false,
/// existing?: "ask" | "overwrite" | "keepOld" = "ask"}`.
///
/// When the project or movie for this clip already exist, `ask` returns
/// `{needsDecision: "exists", paths: [...]}` and does nothing; the interface asks the user and
/// runs the command again with `overwrite` (reuse the names) or `keepOld` (write `... EffectCraft-2`,
/// `-3`... beside them).
/// With `background` the command returns at once (`{started: true}`) and the clip is swapped when
/// the render is done ([`Session::poll_effectcraft_jobs`]); the interface does that so a long clip
/// doesn't freeze the window.
fn replace_clip(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "effectcraft.replaceClip";
    if cfg!(target_arch = "wasm32") {
        return Err(bad(CMD, "EffectCraft links need the desktop app"));
    }
    let clip = target_clip(s, p, CMD)?;
    let sequence = s.state.active_sequence.ok_or(EngineError::NoSequence)?;
    let (old_item, source, clip_name) = clip_source(s, clip, CMD)?;
    let info = s.project.item(old_item).and_then(|i| i.as_media()).map(|m| m.info.clone()).ok_or_else(|| bad(CMD, "no footage"))?;
    let v = info.video.as_ref().ok_or_else(|| bad(CMD, "the clip has no picture"))?;
    if v.width == 0 || v.height == 0 {
        return Err(bad(CMD, "the footage has no usable size"));
    }
    // Only the part of the footage the clip shows goes to EffectCraft (no handles), as in
    // Premiere's Replace With After Effects Composition.
    let (offset, length) = used_range(s, sequence, clip, info.duration, v.frame_rate.frame_duration())
        .ok_or_else(|| bad(CMD, "the clip shows no footage (is it a frame hold?)"))?;
    let seconds = length.seconds();
    if !(seconds.is_finite() && seconds > 0.0) {
        return Err(bad(CMD, "the clip has no usable length"));
    }
    let dir = link_dir(s, p);
    std::fs::create_dir_all(&dir).map_err(|e| EngineError::Other(format!("can't create {}: {e}", dir.display())))?;
    let base = format!("{} EffectCraft", safe_stem(&clip_name));
    let mut stem = base.clone();
    let found: Vec<String> =
        ["ecproj", "mov"].iter().map(|e| dir.join(format!("{stem}.{e}"))).filter(|f| f.exists()).map(|f| f.to_string_lossy().into_owned()).collect();
    let overwrite = if found.is_empty() {
        false
    } else {
        match str_p(p, "existing").unwrap_or("ask") {
            "overwrite" => true,
            "keepOld" => {
                stem = free_stem(&dir, &base);
                false
            }
            "ask" => return Ok(json!({"needsDecision": "exists", "paths": found, "clip": clip.0})),
            other => return Err(bad(CMD, format!("existing: ask|overwrite|keepOld, not `{other}`"))),
        }
    };
    let project = dir.join(format!("{stem}.ecproj")).to_string_lossy().into_owned();
    let render = dir.join(format!("{stem}.mov")).to_string_lossy().into_owned();
    // Not the footage's own name: EffectCraft finds the footage layer by that name.
    let comp = format!("{} Comp", safe_stem(&clip_name));
    let (cli, app) = (find_tool(str_p(p, "cli"), "EFFECTCRAFT_CLI", "effectcraft-cli"), find_tool(str_p(p, "app"), "EFFECTCRAFT_APP", "effectcraft"));
    let new_comp = json!({"name": comp, "width": v.width, "height": v.height, "frameRate": v.frame_rate.as_f64(), "duration": seconds}).to_string();
    let import = json!({"paths": [&source], "addToComp": false}).to_string();
    // The footage's layer starts before the composition does, so the composition opens `offset`
    // into the file.
    let footage_name = Path::new(&source).file_name().map_or_else(|| source.clone(), |n| n.to_string_lossy().into_owned());
    let add_layer = json!({"comp": comp, "item": footage_name, "time": -offset.seconds()}).to_string();
    let plan = BuildPlan { new_comp, import, add_layer, project: project.clone(), comp: comp.clone(), render: render.clone(), replace: overwrite };
    let mut job = EcJob {
        clip,
        sequence,
        old_item,
        offset,
        project: project.clone(),
        comp: comp.clone(),
        render: render.clone(),
        cli_param: str_p(p, "cli").map(str::to_string),
        app_param: str_p(p, "app").map(str::to_string),
        app,
        open: bool_p(p, "open").unwrap_or(true),
        reveal: bool_p(p, "reveal").unwrap_or(true),
        progress: None,
        done: Arc::default(),
    };
    if bool_p(p, "background").unwrap_or(false) {
        if s.ec_jobs.iter().any(|j| j.render == render) {
            return Err(bad(CMD, "this clip is already being rendered"));
        }
        let done = job.done.clone();
        let (prog, outcome) = register_job(s, &clip_name, "Starting...");
        job.progress = Some(prog.clone());
        let spawned = std::thread::Builder::new().name("effectcraft-replace".into()).spawn(move || {
            let t0 = web_time::Instant::now();
            let r = guarded(|| build_and_render(&cli, &plan, Some(&prog)));
            close_job(&prog, &outcome, &r, t0.elapsed().as_secs_f64());
            *done.lock().unwrap_or_else(PoisonError::into_inner) = Some(r);
        });
        spawned.map_err(|e| EngineError::Other(format!("can't start the render: {e}")))?;
        let out = json!({"started": true, "clip": clip.0, "project": job.project, "render": job.render});
        s.ec_jobs.push(job);
        return Ok(out);
    }
    let written = build_and_render(&cli, &plan, None).map_err(EngineError::Other)?;
    finish(s, &job, &written)
}

fn finish(s: &mut Session, job: &EcJob, render: &str) -> Result<Value> {
    const CMD: &str = "effectcraft.replaceClip";
    let still_there = s.project.sequence(job.sequence).is_some_and(|q| q.find_item(job.clip).is_some_and(|(_, it)| it.item == job.old_item));
    if !still_there {
        return Err(bad(CMD, "the clip was changed or removed while its composition was rendering; the render is next to the project"));
    }
    let new_item = crate::commands::import_streamed(s, render, None)?;
    // The clip and the new project item take the Dynamic Link label (Settings > Labels), so a
    // clip made in EffectCraft stands out on the timeline.
    let label = s.prefs.labels.defaults.dynamic_link;
    let repointed = repoint(s, job.sequence, job.clip, job.old_item, new_item, job.offset, label)?;
    s.ec_links.retain(|l| l.render != render && l.project != job.project);
    s.ec_links.push(EcLink {
        project: job.project.clone(),
        comp: job.comp.clone(),
        render: render.to_string(),
        stamp: stamp_of(&job.project),
        cli: job.cli_param.clone(),
        app: job.app_param.clone(),
    });
    if job.reveal {
        reveal_in_browser(s, &[render.to_string(), job.project.clone()]);
    }
    let opened = if job.open { open_app(&job.app, &job.project).err() } else { None };
    Ok(json!({"clip": job.clip.0, "item": new_item.0, "replaced": repointed, "project": job.project, "render": render, "comp": job.comp, "openError": opened}))
}

/// Show `paths` (the new movie and its project) in the Media Browser: its folder, with them selected.
fn reveal_in_browser(s: &mut Session, paths: &[String]) {
    let Some(dir) = paths.first().and_then(|p| Path::new(p).parent()) else { return };
    let dir = dir.to_string_lossy().into_owned();
    let _ = s.execute("mediaBrowser.navigate", json!({"path": dir}));
    let _ = s.execute("mediaBrowser.select", json!({"paths": paths}));
}

/// Where in the footage `clip` starts and how much of it the clip shows: the lowest media time
/// it reaches and the span to the highest, kept inside the footage. `None` for a frame hold or a
/// clip that is gone.
fn used_range(s: &Session, seq: ItemId, clip: ClipId, footage: Tick, frame: Tick) -> Option<(Tick, Tick)> {
    let it = s.project.sequence(seq)?.find_item(clip)?.1;
    if it.frame_hold.is_some() {
        return None;
    }
    let (a, b) = (it.moving_source_time_at(it.start), it.moving_source_time_at(it.end()));
    let lo = a.min(b).max(Tick::ZERO);
    let hi = a.max(b).min(footage.max(frame));
    // At least a frame, even for a clip the footage barely covers.
    let len = if hi > lo { hi - lo } else { frame };
    Some((lo, len.max(frame)))
}

fn repoint(s: &mut Session, seq: ItemId, clip: ClipId, old: ItemId, new: ItemId, offset: Tick, label: Label) -> Result<usize> {
    s.edit("Replace With EffectCraft Composition", |proj, _| {
        if let Some(i) = proj.item_mut(new) {
            i.label = label;
        }
        let q = proj.sequence_mut(seq).ok_or(EngineError::NoSequence)?;
        let link = q.find_item(clip).and_then(|(_, it)| it.link);
        let mut n = 0;
        for t in q.all_tracks_mut() {
            for it in t.items.iter_mut() {
                if it.item == old && (it.id == clip || (link.is_some() && it.link == link)) {
                    it.item = new;
                    it.label = label;
                    it.source_in = Tick(it.source_in.0.saturating_sub(offset.0).max(0));
                    if let Some(h) = it.frame_hold.as_mut() {
                        *h = Tick(h.0.saturating_sub(offset.0).max(0));
                    }
                    n += 1;
                }
            }
        }
        Ok(n)
    })
}

/// `effectcraft.link {clip?, project, comp?, cli?, app?}`: link a clip whose footage is the
/// render of an existing EffectCraft project (after a reopen, or a render made by hand).
fn link_clip(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "effectcraft.link";
    let clip = target_clip(s, p, CMD)?;
    let (_, render, name) = clip_source(s, clip, CMD)?;
    let project = str_p(p, "project").ok_or_else(|| bad(CMD, "need `project` (the .ecproj)"))?.to_string();
    if stamp_of(&project).is_none() {
        return Err(bad(CMD, format!("{project} does not exist")));
    }
    let comp = str_p(p, "comp").map(str::to_string).unwrap_or_else(|| safe_stem(&name));
    s.ec_links.retain(|l| l.render != render);
    s.ec_links.push(EcLink {
        project: project.clone(),
        comp,
        render: render.clone(),
        stamp: stamp_of(&project),
        cli: str_p(p, "cli").map(str::to_string),
        app: str_p(p, "app").map(str::to_string),
    });
    Ok(json!({"project": project, "render": render}))
}

fn linked(s: &Session, clip: ClipId, cmd: &str) -> Result<EcLink> {
    let (_, render, _) = clip_source(s, clip, cmd)?;
    s.ec_links
        .iter()
        .find(|l| l.render == render)
        .cloned()
        .ok_or_else(|| bad(cmd, "the clip is not linked to an EffectCraft composition (effectcraft.replaceClip or effectcraft.link)"))
}

/// `effectcraft.openProject {path, app?}`: open an EffectCraft project (the Media Browser's double
/// click on a `.ecproj`).
fn open_project(_: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "effectcraft.openProject";
    let path = str_p(p, "path").ok_or_else(|| bad(CMD, "need `path`"))?;
    if stamp_of(path).is_none() {
        return Err(bad(CMD, format!("{path} does not exist")));
    }
    let app = find_tool(str_p(p, "app"), "EFFECTCRAFT_APP", "effectcraft");
    open_app(&app, path).map_err(EngineError::Other)?;
    Ok(json!({"project": path}))
}

/// `effectcraft.edit {clip?, app?}`: open the linked composition in EffectCraft.
fn edit_clip(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "effectcraft.edit";
    let clip = target_clip(s, p, CMD)?;
    let l = linked(s, clip, CMD)?;
    let app = find_tool(str_p(p, "app").or(l.app.as_deref()), "EFFECTCRAFT_APP", "effectcraft");
    open_app(&app, &l.project).map_err(EngineError::Other)?;
    Ok(json!({"project": l.project}))
}

/// `effectcraft.sync`: render every link whose project changed since its last render, now, and
/// switch the clips to the new movies.
fn sync_now(s: &mut Session, _: &Value) -> Result<Value> {
    let mut done = Vec::new();
    let mut errors = Vec::new();
    let todo: Vec<(String, EcLink, Option<Stamp>)> =
        s.ec_links.iter().map(|l| (l.project.clone(), l.clone(), stamp_of(&l.project))).filter(|(_, l, now)| now.is_some() && *now != l.stamp).collect();
    for (project, l, now) in todo {
        let cli = find_tool(l.cli.as_deref(), "EFFECTCRAFT_CLI", "effectcraft-cli");
        match render_comp(&cli, &l.project, &l.comp, &l.render, false, None).and_then(|w| s.switch_linked_render(&project, now, &w).map(|()| w)) {
            Ok(w) => done.push(w),
            Err(e) => errors.push(format!("{project}: {e}")),
        }
    }
    let refreshed = crate::media_watch::refresh_changed(s, &Value::Null)?;
    Ok(json!({"rendered": done, "errors": errors, "refreshed": refreshed["refreshed"]}))
}

impl Session {
    #[cfg(not(target_arch = "wasm32"))]
    pub fn start_effectcraft_sync(&mut self) {
        if self.ec_sync.is_some() || self.ec_links.is_empty() {
            return;
        }
        let changed: Vec<(EcLink, Option<Stamp>)> = self
            .ec_links
            .iter()
            .map(|l| (l.clone(), stamp_of(&l.project)))
            .filter(|(l, now)| now.is_some() && *now != l.stamp && !self.ec_imports.iter().any(|j| j.project == l.project))
            .collect();
        if changed.is_empty() {
            return;
        }
        let todo: Vec<(EcLink, Option<Stamp>, Arc<Progress>, Outcome)> = changed
            .into_iter()
            .map(|(l, now)| {
                let name = Path::new(&l.project).file_stem().map_or_else(|| l.comp.clone(), |n| n.to_string_lossy().into_owned());
                let (prog, outcome) = register_job(self, &name, "Waiting...");
                (l, now, prog, outcome)
            })
            .collect();
        let slot: EcSync = Arc::default();
        let out = slot.clone();
        let running: Vec<(String, Arc<Progress>)> = todo.iter().map(|(l, _, prog, _)| (l.project.clone(), prog.clone())).collect();
        let spawned = std::thread::Builder::new().name("effectcraft-sync".into()).spawn(move || {
            let mut results = Vec::new();
            for (l, now, prog, outcome) in todo {
                let t0 = web_time::Instant::now();
                set_status(&prog, "Rendering...");
                let cli = find_tool(l.cli.as_deref(), "EFFECTCRAFT_CLI", "effectcraft-cli");
                // Never over the movie in use: it goes to the next numbered name and the clips switch.
                let r = guarded(|| render_comp(&cli, &l.project, &l.comp, &l.render, false, Some(&prog)));
                close_job(&prog, &outcome, &r, t0.elapsed().as_secs_f64());
                results.push((l.project.clone(), now, r));
            }
            *out.lock().unwrap_or_else(PoisonError::into_inner) = Some(results);
        });
        if spawned.is_ok() {
            self.ec_sync = Some(slot);
            self.ec_progress = running;
        }
    }

    #[cfg(target_arch = "wasm32")]
    pub fn start_effectcraft_sync(&mut self) {}

    pub fn effectcraft_progress(&self) -> Option<String> {
        let mut running = self.jobs.iter().filter(|j| j.label.starts_with(JOB_PREFIX) && !j.progress.finished.load(Ordering::Relaxed));
        let first = running.next()?;
        let status = first.progress.status.lock().unwrap_or_else(PoisonError::into_inner).clone();
        let more = running.count();
        let more = if more > 0 { format!(" (+{more} more)") } else { String::new() };
        Some(format!("{}: {status}{more}; the clip updates when it is done", first.label.trim_end()))
    }

    /// What to paint over `clip` (whose footage is `item`) in the timeline while EffectCraft renders
    /// it: how far along it is (0 to 1) and whether frames are coming yet (else it is still
    /// preparing). `None` when nothing renders for the clip.
    pub fn effectcraft_clip_overlay(&self, clip: ClipId, item: ItemId) -> Option<(f32, bool)> {
        if self.ec_jobs.is_empty() && self.ec_progress.is_empty() && self.ec_imports.is_empty() {
            return None;
        }
        // A replacement in progress is the clip's own; a re-render belongs to every clip that plays
        // the link's movie.
        let prog = self
            .ec_imports
            .iter()
            .find(|j| j.item == item)
            .map(|j| j.progress.clone())
            .or_else(|| self.ec_jobs.iter().find(|j| j.clip == clip).and_then(|j| j.progress.clone()))
            .or_else(|| {
                let MediaRef::File { path } = &self.project.item(item)?.as_media()?.media else { return None };
                let link = self.ec_links.iter().find(|l| l.render == *path)?;
                self.ec_progress.iter().find(|(project, _)| *project == link.project).map(|(_, p)| p.clone())
            })?;
        if prog.finished.load(Ordering::Relaxed) {
            return None;
        }
        let fraction = prog.fraction().clamp(0.0, 1.0);
        Some((fraction, prog.done.load(Ordering::Relaxed) > 0))
    }

    /// Finish the background replacements whose render is done: one result per job, the swap's
    /// outcome (`{clip, item, replaced, ...}`) or why it failed.
    pub fn poll_effectcraft_jobs(&mut self) -> Vec<std::result::Result<Value, String>> {
        let mut out = Vec::new();
        let mut i = 0;
        while i < self.ec_jobs.len() {
            let finished = self.ec_jobs.get(i).and_then(|j| j.done.lock().unwrap_or_else(PoisonError::into_inner).take());
            let Some(result) = finished else {
                i += 1;
                continue;
            };
            let job = self.ec_jobs.remove(i);
            out.push(match result {
                Ok(written) => finish(self, &job, &written).map_err(|e| e.to_string()),
                Err(e) => Err(e),
            });
        }
        let mut i = 0;
        while i < self.ec_imports.len() {
            let finished = self.ec_imports.get(i).and_then(|j| j.done.lock().unwrap_or_else(PoisonError::into_inner).take());
            let Some(result) = finished else {
                i += 1;
                continue;
            };
            let job = self.ec_imports.remove(i);
            self.ec_pending.remove(&job.render);
            out.push(match result {
                Ok(written) => bind_import(self, &job, &written).map_err(|e| e.to_string()),
                Err(e) => {
                    self.ec_links.retain(|l| l.project != job.project);
                    crate::relink::refresh(self);
                    Err(e)
                }
            });
        }
        out
    }

    /// Point every clip that uses a link's movie at `new`, the movie just rendered, and remember
    /// the project's `stamp`. The old movie is deleted when nothing uses it any more.
    fn switch_linked_render(&mut self, project: &str, stamp: Option<Stamp>, new: &str) -> std::result::Result<(), String> {
        let Some(old) = self.ec_links.iter().find(|l| l.project == project).map(|l| l.render.clone()) else { return Ok(()) };
        if old != new {
            let items: Vec<ItemId> = self
                .project
                .items
                .iter()
                .filter(|(_, it)| matches!(it.as_media().map(|m| &m.media), Some(MediaRef::File { path }) if *path == old))
                .map(|(id, _)| *id)
                .collect();
            for item in items {
                self.execute("media.replaceFootage", json!({"item": item.0, "path": new})).map_err(|e| e.to_string())?;
            }
        }
        if let Some(l) = self.ec_links.iter_mut().find(|l| l.project == project) {
            l.render = new.to_string();
            l.stamp = stamp;
        }
        if old != new {
            // Best effort: a movie still open somewhere stays until it can be removed.
            let _ = std::fs::remove_file(&old);
        }
        Ok(())
    }

    /// Take the result of a finished pass: `Some(Ok(n))` re-rendered `n` movies (the clips now use
    /// them), `Some(Err(text))` the first render that failed. `None` while there is nothing new.
    pub fn poll_effectcraft_sync(&mut self) -> Option<std::result::Result<usize, String>> {
        let results = self.ec_sync.as_ref()?.lock().unwrap_or_else(PoisonError::into_inner).take()?;
        self.ec_sync = None;
        self.ec_progress.clear();
        let mut n = 0;
        let mut first_error = None;
        for (project, stamp, result) in results {
            // A failed render keeps its stamp too: it is tried again when the project is saved again.
            match result {
                Ok(new) => match self.switch_linked_render(&project, stamp, &new) {
                    Ok(()) => n += 1,
                    Err(e) => {
                        first_error.get_or_insert(e);
                    }
                },
                Err(e) => {
                    if let Some(l) = self.ec_links.iter_mut().find(|l| l.project == project) {
                        l.stamp = stamp;
                    }
                    first_error.get_or_insert(e);
                }
            }
        }
        Some(match first_error {
            Some(e) => Err(e),
            None => Ok(n),
        })
    }
}

// ------------------------------------------------------------------ importing .ecproj files

/// An EffectCraft project being rendered for an item the Project panel already shows.
pub struct EcImport {
    item: ItemId,
    project: String,
    comp: String,
    render: String,
    progress: Arc<Progress>,
    /// Set by the worker thread: the movie written, or why not.
    done: Arc<Mutex<Option<std::result::Result<String, String>>>>,
}

/// Is `path` an EffectCraft project?
pub(crate) fn is_project(path: &str) -> bool {
    Path::new(path).extension().is_some_and(|e| e.eq_ignore_ascii_case("ecproj"))
}

/// The composition of a project that becomes the clip: its name, size, frame rate and length.
struct CompInfo {
    name: String,
    width: u32,
    height: u32,
    fps: f64,
    seconds: f64,
}

/// Read the composition to use from `effectcraft-cli info`: the one open when the project was saved,
/// else the first. EffectCraft projects hold any number of compositions.
fn project_comp(cli: &str, project: &str) -> std::result::Result<CompInfo, String> {
    let text = cli_output(cli, &["info".to_string(), project.to_string(), "--json".to_string()])?;
    let v: Value = serde_json::from_str(&text).map_err(|e| format!("effectcraft-cli info: {e}"))?;
    let items = v["project"]["items"].as_array().cloned().unwrap_or_default();
    let comps: Vec<&Value> = items.iter().filter(|i| i["type"] == json!("Composition")).collect();
    let active = v["activeComp"]["name"].as_str();
    let c = comps.iter().find(|c| c["name"].as_str() == active).or(comps.first()).ok_or("the project has no composition")?;
    let size = c["size"].as_array();
    let dim = |i: usize| size.and_then(|s| s.get(i)).and_then(Value::as_u64).and_then(|d| u32::try_from(d).ok()).filter(|d| (1..=16_384).contains(d));
    let (Some(width), Some(height)) = (dim(0), dim(1)) else { return Err("the composition has no usable size".into()) };
    let fps = c["frameRate"].as_f64().filter(|f| f.is_finite() && (0.001..=1000.0).contains(f)).ok_or("the composition has no usable frame rate")?;
    let seconds = c["duration"].as_f64().filter(|d| d.is_finite() && *d > 0.0 && *d < 86_400.0).ok_or("the composition has no usable length")?;
    Ok(CompInfo { name: c["name"].as_str().unwrap_or("Comp").to_string(), width, height, fps, seconds })
}

/// What a render of `c` will look like, for the item that stands in for it until it is done.
fn placeholder_info(name: &str, c: &CompInfo) -> filmcraft_media::MediaInfo {
    let rate = filmcraft_time::FrameRate::from_f64(c.fps);
    filmcraft_media::MediaInfo {
        name: name.to_string(),
        kind: filmcraft_media::MediaKind::Movie,
        duration: rate.snap_nearest(Tick::from_seconds_f64(c.seconds)).max(rate.frame_duration()),
        video: Some(filmcraft_media::VideoStreamInfo {
            width: c.width,
            height: c.height,
            frame_rate: rate,
            par: (1, 1),
            codec: "ProRes 4444".into(),
            pixel_format: "yuva444p12".into(),
            color: Default::default(),
            has_alpha: true,
            bitrate: None,
            hdr: None,
        }),
        audio_streams: Vec::new(),
        container: "mov".into(),
        start_timecode: None,
        file_size: None,
    }
}

/// Import an EffectCraft project as an item: it appears in the Project panel at once (and can be
/// dropped on the timeline) while the composition renders; the movie is bound to it when done
/// ([`Session::poll_effectcraft_jobs`]). With `background` false the render finishes first.
/// Importing a project that is already in the project gives back its item.
pub(crate) fn import_project(s: &mut Session, project: &str, bin: Option<filmcraft_project::BinId>, background: bool) -> Result<ItemId> {
    const CMD: &str = "file.import";
    if cfg!(target_arch = "wasm32") {
        return Err(bad(CMD, "EffectCraft projects need the desktop app"));
    }
    if stamp_of(project).is_none() {
        return Err(bad(CMD, format!("{project} does not exist")));
    }
    // already imported: the same item
    if let Some(l) = s.ec_links.iter().find(|l| l.project == project)
        && let Some(id) = s.project.items.iter().find_map(|(id, it)| match it.as_media().map(|m| &m.media) {
            Some(MediaRef::File { path }) if *path == l.render => Some(*id),
            _ => None,
        })
    {
        return Ok(id);
    }
    let cli = find_tool(None, "EFFECTCRAFT_CLI", "effectcraft-cli");
    let comp = project_comp(&cli, project).map_err(EngineError::Other)?;
    let stem = Path::new(project).file_stem().map_or_else(|| "EffectCraft".to_string(), |n| n.to_string_lossy().into_owned());
    let dir = link_dir(s, &json!({}));
    std::fs::create_dir_all(&dir).map_err(|e| EngineError::Other(format!("can't create {}: {e}", dir.display())))?;
    let wanted = dir.join(format!("{} Render.mov", safe_stem(&stem))).to_string_lossy().into_owned();
    let render = if Path::new(&wanted).exists() { variant_of(&wanted) } else { wanted };
    let label = s.prefs.labels.defaults.dynamic_link;
    let info = placeholder_info(&stem, &comp);
    let path = render.clone();
    let item = s.edit("Import EffectCraft Project", |p, _| {
        Ok(p.add_item(
            &stem,
            label,
            filmcraft_project::ItemKind::Media(Box::new(filmcraft_project::MediaClip {
                media: MediaRef::File { path },
                info,
                interpret: Default::default(),
                mark_in: None,
                mark_out: None,
                markers: vec![],
                offline: false,
                proxy: None,
                identity: None,
            })),
            bin,
        ))
    })?;
    s.ec_links.retain(|l| l.project != project);
    s.ec_links.push(EcLink { project: project.to_string(), comp: comp.name.clone(), render: render.clone(), stamp: stamp_of(project), cli: None, app: None });
    // The movie is not there yet: not an offline-media problem.
    s.ec_pending.insert(render.clone());
    let (prog, outcome) = register_job(s, &stem, "Starting...");
    let job = EcImport { item, project: project.to_string(), comp: comp.name.clone(), render: render.clone(), progress: prog.clone(), done: Arc::default() };
    let (done, project_s, comp_s, render_s) = (job.done.clone(), project.to_string(), comp.name, render);
    let work = move || {
        let t0 = web_time::Instant::now();
        let r = guarded(|| render_comp(&cli, &project_s, &comp_s, &render_s, false, Some(&prog)));
        close_job(&prog, &outcome, &r, t0.elapsed().as_secs_f64());
        *done.lock().unwrap_or_else(PoisonError::into_inner) = Some(r);
    };
    if background {
        std::thread::Builder::new().name("effectcraft-import".into()).spawn(work).map_err(|e| EngineError::Other(format!("can't start the render: {e}")))?;
        s.ec_imports.push(job);
    } else {
        work();
        let written = job.done.lock().unwrap_or_else(PoisonError::into_inner).take().unwrap_or_else(|| Err("the render did not finish".into()));
        s.ec_pending.remove(&job.render);
        match written {
            Ok(w) => {
                bind_import(s, &job, &w)?;
            }
            Err(e) => {
                s.ec_links.retain(|l| l.project != job.project);
                return Err(EngineError::Other(e));
            }
        }
    }
    Ok(item)
}

/// The render is done: the item takes the movie's real properties (audio too) and plays it.
fn bind_import(s: &mut Session, job: &EcImport, written: &str) -> Result<Value> {
    if s.project.item(job.item).and_then(|i| i.as_media()).is_none() {
        return Err(bad("file.import", "the imported composition was removed from the project while it rendered"));
    }
    let src = s.media.open_file(written, &*s.services)?;
    let info = src.info().clone();
    let identity = crate::relink::identity_of(&*s.services, written).ok();
    // Not an edit: the file the item names was written (like a changed file the media refresh reads
    // again), so this is no undo step and does not dirty the project.
    if let Some(m) = Arc::make_mut(&mut s.project).item_mut(job.item).and_then(|i| i.as_media_mut()) {
        m.media = MediaRef::File { path: written.to_string() };
        m.info = info;
        m.identity = identity;
    }
    s.media.insert_file(job.item, written, src);
    if written != job.render
        && let Some(l) = s.ec_links.iter_mut().find(|l| l.project == job.project)
    {
        l.render = written.to_string();
    }
    crate::relink::refresh(s);
    s.bump_view();
    Ok(json!({"imported": job.item.0, "project": job.project, "render": written, "comp": job.comp}))
}

pub fn commands() -> Vec<CommandSpec> {
    vec![
        CommandSpec {
            id: "effectcraft.replaceClip",
            label: "Replace With EffectCraft Composition",
            menu: &["Clip"],
            shortcut: None,
            params: r#"{"clip":clipId?,"dir":str?,"cli":str?,"app":str?,"open":bool=true,"reveal":bool=true,"background":bool=false,"existing":"ask"|"overwrite"|"keepOld"="ask"}"#,
            enabled: has_seq,
            run: replace_clip,
            journal: true,
        },
        CommandSpec {
            id: "effectcraft.edit",
            label: "Edit in EffectCraft",
            menu: &["Clip"],
            shortcut: None,
            params: r#"{"clip":clipId?,"app":str?}"#,
            enabled: has_seq,
            run: edit_clip,
            journal: false,
        },
        CommandSpec {
            id: "effectcraft.openProject",
            label: "Open in EffectCraft",
            menu: &[],
            shortcut: None,
            params: r#"{"path":str,"app":str?}"#,
            enabled: crate::commands::always,
            run: open_project,
            journal: false,
        },
        CommandSpec {
            id: "effectcraft.link",
            label: "Link to EffectCraft Project…",
            menu: &[],
            shortcut: None,
            params: r#"{"clip":clipId?,"project":str,"comp":str?,"cli":str?,"app":str?}"#,
            enabled: has_seq,
            run: link_clip,
            journal: false,
        },
        CommandSpec {
            id: "effectcraft.sync",
            label: "Render Changed EffectCraft Compositions",
            menu: &[],
            shortcut: None,
            params: "{}",
            enabled: crate::commands::always,
            run: sync_now,
            journal: true,
        },
    ]
}

#[cfg(test)]
#[path = "effectcraft_link_tests.rs"]
mod tests;
