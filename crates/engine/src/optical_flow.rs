//! Generate an optical-flow visualization or motion-vector overlay track above selected video clips
//! (`clip.opticalFlowOverlay` and its menu shortcuts under `Clip ▸ Video Options ▸ Optical Flow Track`).
//!
//! The command checks its parameters and plans every clip at once, then renders in a background job
//! (with progress and Cancel in the status bar; `wait: true` runs it in place, as on hosts without
//! threads). Each selected video clip is evaluated frame by frame at the sequence's frame rate through
//! [`filmcraft_render::flow::FlowEstimator`] (NVIDIA's hardware NVOFA engine when
//! `filmcraft-platform` registered it, otherwise the pure-Rust CPU Lucas–Kanade pyramid), drawn
//! and colour-encoded through [`filmcraft_render::flow::FlowVizEncoder`] (a WGSL compute pass when
//! the app registered `filmcraft-gpu`'s, otherwise the CPU) and streamed into a QuickTime ProRes
//! `.mov` on disk: ProRes 422 for the visual looks, ProRes 4444
//! (no chroma subsampling) for the Motion Vectors data pass. Memory use does not grow with the
//! clip's length.
//!
//! When the job finishes, [`poll`] imports the files and places each one on the first free unlocked
//! video track **above** its source clip (adding a new top video track when needed), with the same
//! start and duration, as **one** undo step. It is all or nothing: if any clip cannot be placed
//! (a source clip was moved, trimmed or deleted meanwhile, a file cannot be imported) the project is
//! left as it was and the generated files are deleted. A cancelled or failed job changes nothing and
//! deletes what it wrote.

use std::collections::{BTreeSet, HashSet};
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex, PoisonError};

use filmcraft_project::{ClipId, ItemId, ParamValue, Project, TrackId, TrackKind};
use filmcraft_render::flow::{CPU_FLOW_BACKEND, FlowEstimator, FlowVizEncoder, FlowVizMode};
use filmcraft_render::{Blend, Image, RenderOptions, render_sequence_track};
use filmcraft_time::{FrameRate, Tick, TimeRange};
use serde_json::{Value, json};

use crate::commands::{CommandSpec, bad, bool_p, clips_p, f64_p, has_seq, str_p};
use crate::{EngineError, Result, Session};

/// Most frames one job renders over all its clips (an hour at 60 fps). A project can store any
/// clip duration, so the schedule is capped up front: a longer selection is an error, never a
/// silently shorter overlay.
pub const MAX_OVERLAY_FRAMES: i64 = 216_000;

/// The undo step and job label.
const LABEL: &str = "Generate Optical Flow Track";

fn spec(
    id: &'static str,
    label: &'static str,
    menu: &'static [&'static str],
    params: &'static str,
    run: fn(&mut Session, &Value) -> Result<Value>,
) -> CommandSpec {
    CommandSpec { id, label, menu, shortcut: None, params, enabled: can_optical_flow_overlay, run, journal: true, named: crate::commands::NamedKeys::NONE }
}

pub(crate) fn commands() -> Vec<CommandSpec> {
    vec![
        spec(
            "clip.opticalFlowOverlay",
            "Generate Optical Flow Track…",
            // parameterised (any mode): for agents and the timeline menu's enabled check; the menus
            // show the three modes below
            &[],
            r#"{"clips":[id]?,"mode":"hsvWheel|motionVectors|kineticGlow"?,"maxMotion":f64=16,"opacity":f64?,"blendMode":str?,"dir":str?,"wait":bool=false}"#,
            |s, p| run_overlay(s, p, None),
        ),
        spec(
            "clip.opticalFlowOverlay.hsvWheel",
            "HSV Neon Flow Wheel",
            &["Clip", "Video Options", "Optical Flow Track"],
            r#"{"clips":[id]?,"maxMotion":f64=16,"opacity":f64?,"blendMode":str?,"dir":str?,"wait":bool=false}"#,
            |s, p| run_overlay(s, p, Some(FlowVizMode::HsvWheel)),
        ),
        spec(
            "clip.opticalFlowOverlay.motionVectors",
            "Motion Vectors (UV Pass)",
            &["Clip", "Video Options", "Optical Flow Track"],
            r#"{"clips":[id]?,"maxMotion":f64=16,"opacity":f64?,"blendMode":str?,"dir":str?,"wait":bool=false}"#,
            |s, p| run_overlay(s, p, Some(FlowVizMode::MotionVectors)),
        ),
        spec(
            "clip.opticalFlowOverlay.kineticGlow",
            "Kinetic Motion Glow",
            &["Clip", "Video Options", "Optical Flow Track"],
            r#"{"clips":[id]?,"maxMotion":f64=16,"opacity":f64?,"blendMode":str?,"dir":str?,"wait":bool=false}"#,
            |s, p| run_overlay(s, p, Some(FlowVizMode::KineticGlow)),
        ),
    ]
}

fn video_clips(s: &Session, p: &Value) -> Vec<ClipId> {
    let Some(q) = s.active_sequence() else { return Vec::new() };
    let mut seen = BTreeSet::new();
    clips_p(s, p).into_iter().filter(|c| seen.insert(*c) && q.video_tracks.iter().any(|t| t.item(*c).is_some())).collect()
}

pub(crate) fn can_optical_flow_overlay(s: &Session) -> std::result::Result<(), String> {
    has_seq(s)?;
    let q = s.active_sequence().ok_or("no sequence is open")?;
    if s.state.selection.iter().any(|c| q.video_tracks.iter().any(|t| t.item(*c).is_some())) { Ok(()) } else { Err("select a video clip".into()) }
}

/// `<dir>/<base> <n>.mov` with the first `n` used by no project item, no existing file and no other
/// clip of this job (`taken`).
fn unique_mov_path(s: &Session, dir: &str, base: &str, taken: &mut HashSet<String>) -> Result<String> {
    let names: HashSet<&str> = s.project.items.values().map(|i| i.name.as_str()).collect();
    for k in 1..=99_999u32 {
        let file = format!("{base} {k}.mov");
        let path = std::path::Path::new(dir).join(&file).to_string_lossy().into_owned();
        if !names.contains(file.as_str()) && !taken.contains(&path) && s.services.file_size(&path).is_err() {
            taken.insert(path.clone());
            return Ok(path);
        }
    }
    Err(EngineError::Other(format!("{dir}: no free file name for \"{base} <n>.mov\"")))
}

fn find_free_video_track_above(p: &Project, seq_id: ItemId, above_track_idx: usize, range: TimeRange) -> Option<TrackId> {
    let seq = p.sequence(seq_id)?;
    seq.video_tracks
        .iter()
        .enumerate()
        .skip(above_track_idx.saturating_add(1))
        .find(|(_, t)| !t.locked && !t.items.iter().any(|it| it.start < range.end() && range.start < it.end()))
        .map(|(_, t)| t.id)
}

fn parse_blend_index(cmd: &str, name: Option<&str>, mode: FlowVizMode) -> Result<u32> {
    match name {
        Some(n) => filmcraft_project::effect::BLEND_MODES
            .iter()
            .position(|b| b.eq_ignore_ascii_case(n))
            .and_then(|i| u32::try_from(i).ok())
            .ok_or_else(|| bad(cmd, format!("unknown blendMode `{n}`"))),
        None => Ok(match mode {
            FlowVizMode::HsvWheel | FlowVizMode::KineticGlow => Blend::Screen.index(),
            FlowVizMode::MotionVectors => Blend::Normal.index(),
        }),
    }
}

/// How the overlay clips are rendered and composited.
#[derive(Clone, Copy, Debug)]
struct Look {
    mode: FlowVizMode,
    max_motion: f32,
    opacity: f32,
    blend: u32,
}

/// One clip to render, planned on the session's thread.
#[derive(Clone, Debug)]
struct Work {
    clip: ClipId,
    track_index: usize,
    start: Tick,
    dur: Tick,
    frames: i64,
    path: String,
}

/// One rendered overlay file waiting to be placed.
#[derive(Clone, Debug)]
struct Rendered {
    clip: ClipId,
    start: Tick,
    dur: Tick,
    path: String,
    /// The file's bytes when the host keeps files in memory (web); otherwise it is read from disk.
    bytes: Option<Arc<[u8]>>,
}

/// What a finished job hands to [`poll`].
#[derive(Debug, Default)]
struct Outcome {
    clips: Vec<Rendered>,
    hardware_frames: u64,
    cpu_frames: u64,
    /// Why the hardware engine was not (or not always) used.
    fallback: Option<String>,
    /// Overlay frames drawn on the GPU and on the CPU, the GPU's name, and why it was not (or not
    /// always) used.
    viz_gpu_frames: u64,
    viz_cpu_frames: u64,
    viz_gpu_name: Option<String>,
    viz_fallback: Option<String>,
}

impl Outcome {
    /// Which backend computed the frames: the hardware's name, the CPU, or both when the hardware
    /// failed part way.
    fn backend(&self, hw_name: &str) -> String {
        match (self.hardware_frames, self.cpu_frames) {
            (0, _) => CPU_FLOW_BACKEND.to_string(),
            (_, 0) => hw_name.to_string(),
            (h, c) => format!("{hw_name} ({h} frames), then {CPU_FLOW_BACKEND} ({c} frames)"),
        }
    }

    /// Where the overlay frames were drawn, in the same form.
    fn viz_backend(&self) -> String {
        let gpu = self.viz_gpu_name.as_deref().unwrap_or("GPU");
        match (self.viz_gpu_frames, self.viz_cpu_frames) {
            (0, _) => "CPU".to_string(),
            (_, 0) => gpu.to_string(),
            (g, c) => format!("{gpu} ({g} frames), then CPU ({c} frames)"),
        }
    }
}

/// A running optical-flow overlay job; its clips are placed by [`poll`] when it finishes.
pub struct PendingFlow {
    pub job: u64,
    seq: ItemId,
    look: Look,
    /// `Session::document_epoch` when the job started: ids mean something else in another project.
    epoch: u64,
    results: Arc<Mutex<Option<Outcome>>>,
    /// The hardware backend's name, for messages.
    hw_name: Arc<Mutex<String>>,
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Frames needed to cover `dur` at `fd` per frame (the last one may be partial).
fn frames_for(dur: Tick, fd: Tick) -> Option<i64> {
    if dur.0 <= 0 || fd.0 <= 0 {
        return None;
    }
    let whole = dur.0 / fd.0;
    Some(if dur.0 % fd.0 == 0 { whole } else { whole.saturating_add(1) })
}

fn run_overlay(s: &mut Session, p: &Value, fixed_mode: Option<FlowVizMode>) -> Result<Value> {
    let cmd = "clip.opticalFlowOverlay";
    let seq_id = s.state.active_sequence.ok_or(EngineError::NoSequence)?;
    let clips = video_clips(s, p);
    if clips.is_empty() {
        return Err(bad(cmd, "select a video clip"));
    }

    let mode = match (fixed_mode, str_p(p, "mode")) {
        (Some(m), None) => m,
        (_, Some(m)) => FlowVizMode::from_name(m).ok_or_else(|| bad(cmd, format!("unknown mode `{m}` (expected hsvWheel, motionVectors, kineticGlow)")))?,
        (None, None) => FlowVizMode::HsvWheel,
    };

    let max_motion = f64_p(p, "maxMotion").unwrap_or(16.0);
    if !max_motion.is_finite() || !(0.5..=512.0).contains(&max_motion) {
        return Err(bad(cmd, "`maxMotion` must be between 0.5 and 512.0"));
    }
    let default_opacity = match mode {
        FlowVizMode::HsvWheel | FlowVizMode::KineticGlow => 85.0,
        FlowVizMode::MotionVectors => 100.0,
    };
    let opacity = f64_p(p, "opacity").unwrap_or(default_opacity);
    if !opacity.is_finite() || !(0.0..=100.0).contains(&opacity) {
        return Err(bad(cmd, "`opacity` must be between 0 and 100"));
    }
    let blend = parse_blend_index(cmd, str_p(p, "blendMode"), mode)?;
    let look = Look { mode, max_motion: max_motion as f32, opacity: opacity as f32, blend };

    if s.flow_jobs.iter().any(|j| j.seq == seq_id) {
        return Err(EngineError::Other("an optical-flow track is already being generated for this sequence".into()));
    }

    let (w, h, rate) = {
        let seq = s.project.sequence(seq_id).ok_or(EngineError::NoSequence)?;
        let (w, h) = filmcraft_render::output_size(seq, 1.0);
        (w, h, seq.settings.frame_rate)
    };
    let side_ok = |v: usize| u32::try_from(v).is_ok_and(|v| (1..=filmcraft_export::PRORES_MOV_MAX_SIDE).contains(&v));
    if !side_ok(w) || !side_ok(h) {
        return Err(bad(cmd, format!("a {w}x{h} sequence is larger than an optical-flow track can be")));
    }
    let fd = rate.frame_duration();
    if fd.0 <= 0 {
        return Err(bad(cmd, "the sequence has no valid frame rate"));
    }

    let dir = crate::voiceover::media_dir(s, p, "Optical Flow")?;
    let mut taken = HashSet::new();
    let mut work = Vec::new();
    let mut total = 0i64;
    {
        let seq = s.project.sequence(seq_id).ok_or(EngineError::NoSequence)?;
        for clip in &clips {
            let Some((track_index, it)) = seq.video_tracks.iter().enumerate().find_map(|(ti, tr)| tr.item(*clip).map(|it| (ti, it))) else {
                continue;
            };
            let frames = frames_for(it.duration, fd).ok_or_else(|| bad(cmd, format!("clip {} has no duration", clip.0)))?;
            total = total.saturating_add(frames);
            if total > MAX_OVERLAY_FRAMES {
                return Err(bad(
                    cmd,
                    format!("the selection is longer than {MAX_OVERLAY_FRAMES} frames; generate the optical-flow track for fewer or shorter clips"),
                ));
            }
            let item_name = s.project.item(it.item).map(|i| i.name.as_str()).unwrap_or("Clip");
            let stem = std::path::Path::new(item_name).file_stem().and_then(|s| s.to_str()).filter(|s| !s.is_empty()).unwrap_or("Clip");
            work.push((*clip, track_index, it.start, it.duration, frames, format!("{stem} ({})", mode.label())));
        }
    }
    let work: Vec<Work> = work
        .into_iter()
        .map(|(clip, track_index, start, dur, frames, base)| {
            unique_mov_path(s, &dir, &base, &mut taken).map(|path| Work { clip, track_index, start, dur, frames, path })
        })
        .collect::<Result<_>>()?;
    if work.is_empty() {
        return Err(bad(cmd, "select a video clip"));
    }
    let in_memory = s.services.export_in_memory();
    if !in_memory && !cfg!(target_arch = "wasm32") {
        std::fs::create_dir_all(&dir).map_err(|e| EngineError::Other(format!("{dir}: {e}")))?;
    }

    let id = s.jobs.iter().map(|j| j.id).max().unwrap_or(0) + 1;
    let job = crate::Job { id, label: format!("{LABEL} ({})", mode.label()), progress: Default::default(), result: Default::default() };
    job.progress.total.store(u64::try_from(total).unwrap_or(0), Ordering::Relaxed);
    let results: Arc<Mutex<Option<Outcome>>> = Arc::default();
    let hw_name: Arc<Mutex<String>> = Arc::new(Mutex::new("hardware".to_string()));
    let ctx = JobCtx {
        project: s.project.clone(),
        seq: seq_id,
        size: (w, h),
        rate,
        look,
        provider: s.media.full_res_provider(s.project.clone(), s.services.clone()).own_sources(),
        services: s.services.clone(),
        in_memory,
    };
    let (prog, res, out, name) = (job.progress.clone(), job.result.clone(), results.clone(), hw_name.clone());
    let run = move || {
        let t0 = web_time::Instant::now();
        let r = ctx.render_all(&work, &prog, &name);
        let secs = t0.elapsed().as_secs_f64();
        let done = prog.done.load(Ordering::Relaxed);
        let r = match r {
            Ok(o) => {
                *lock(&prog.status) = format!("Rendered {done} frame(s): flow via {}, drawn on {} ({secs:.1}s)", o.backend(&lock(&name)), o.viz_backend());
                *lock(&out) = Some(o);
                Ok(filmcraft_export::Report { frames: done, seconds: secs, render_fps: done as f64 / secs.max(1e-6), ..Default::default() })
            }
            Err(e) => {
                *lock(&prog.status) = e.clone();
                *lock(&prog.error) = Some(e.clone());
                Err(e)
            }
        };
        *lock(&res) = Some(r);
        prog.finished.store(true, Ordering::Relaxed);
    };
    let run = crate::export_tools::guard_job(job.progress.clone(), job.result.clone(), run);
    s.jobs.push(job);
    s.flow_jobs.push(PendingFlow { job: id, seq: seq_id, look, epoch: s.document_epoch, results, hw_name });

    let wait = bool_p(p, "wait").unwrap_or(false);
    if wait || cfg!(target_arch = "wasm32") {
        run();
        let failed = s.jobs.iter().find(|j| j.id == id).and_then(|j| lock(&j.result).clone()).and_then(|r| r.err());
        let pos = s.flow_jobs.iter().position(|j| j.job == id);
        let pending = pos.map(|i| s.flow_jobs.remove(i));
        if let Some(e) = failed {
            return Err(EngineError::Other(e));
        }
        let pending = pending.ok_or_else(|| EngineError::Other("the optical-flow job vanished".into()))?;
        let mut v = apply(s, pending)?;
        v["job"] = json!(id);
        return Ok(v);
    }
    std::thread::Builder::new().name("filmcraft-optical-flow".into()).spawn(run).map_err(|e| EngineError::Other(e.to_string()))?;
    Ok(json!({"job": id, "frames": total, "mode": mode.id()}))
}

/// Everything the worker needs, owned (the project is the snapshot the job started from).
struct JobCtx {
    project: Arc<Project>,
    seq: ItemId,
    size: (usize, usize),
    rate: FrameRate,
    look: Look,
    provider: crate::media_pool::PoolProvider,
    services: Arc<dyn crate::Services>,
    in_memory: bool,
}

impl JobCtx {
    fn render(&self, track_index: usize, t: Tick) -> Image {
        render_sequence_track(&self.project, self.seq, track_index, t, RenderOptions::default(), &self.provider)
    }

    /// Render every clip; on failure or Cancel, delete what was written.
    fn render_all(&self, work: &[Work], prog: &filmcraft_export::Progress, hw_name: &Mutex<String>) -> std::result::Result<Outcome, String> {
        let mut out = Outcome::default();
        for w in work {
            match self.render_clip(w, prog, hw_name, &mut out) {
                Ok(r) => out.clips.push(r),
                Err(e) => {
                    remove_files(self.in_memory, out.clips.iter().map(|r| r.path.as_str()).chain([w.path.as_str()]));
                    return Err(if prog.cancel.load(Ordering::Relaxed) { "Stopped: nothing was changed".to_string() } else { e });
                }
            }
        }
        Ok(out)
    }

    fn render_clip(&self, w: &Work, prog: &filmcraft_export::Progress, hw_name: &Mutex<String>, out: &mut Outcome) -> std::result::Result<Rendered, String> {
        let (fw, fh) = self.size;
        let fd = self.rate.frame_duration();
        let mut est = FlowEstimator::new(fw, fh);
        if est.is_hardware() {
            *lock(hw_name) = est.backend().to_string();
        }
        // the overlay is drawn and colour-encoded on the GPU when the app registered one
        let mut viz = FlowVizEncoder::new();
        if viz.is_gpu() {
            out.viz_gpu_name = Some(viz.backend());
        }
        let (mut viz_gpu, mut viz_cpu) = (0u64, 0u64);
        let look = self.look;
        let name = std::path::Path::new(&w.path).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let (mut prev, mut ahead): (Option<Image>, Option<Image>) = (None, None);
        let (mut hw, mut cpu) = (0u64, 0u64);
        let frames = (0..w.frames).map(|fi| -> filmcraft_export::Result<Vec<f32>> {
            if prog.cancel.load(Ordering::Relaxed) {
                return Err(filmcraft_export::ExportError::Cancelled);
            }
            let t = w.start + Tick(fi.saturating_mul(fd.0));
            let curr = match ahead.take() {
                Some(img) => img,
                None => self.render(w.track_index, t),
            };
            // Flow into each frame from the one before; the first frame has none before it, so it
            // takes the flow towards the next one (rendered once and reused as frame 1).
            let field = match prev.as_ref() {
                Some(p) => est.estimate(p, &curr),
                None if w.frames > 1 => {
                    let next = self.render(w.track_index, (w.start + fd).min(w.start + w.dur - Tick(1)));
                    let f = est.estimate(&curr, &next);
                    ahead = Some(next);
                    f
                }
                None => est.estimate(&curr, &curr),
            };
            if field.hardware {
                hw += 1;
            } else {
                cpu += 1;
            }
            let (encoded, on_gpu) = viz.encode(&curr, &field, look.mode, look.max_motion);
            if on_gpu {
                viz_gpu += 1;
            } else {
                viz_cpu += 1;
            }
            prev = Some(curr);
            prog.done.fetch_add(1, Ordering::Relaxed);
            *lock(&prog.status) = format!("{name}: frame {} of {}", fi + 1, w.frames);
            Ok(encoded)
        });
        // ProRes 4444 keeps the UV pass's channels apart (no chroma subsampling); 422 is plenty for
        // the visual looks.
        let profile = filmcraft_export::prores_profile(if look.mode == FlowVizMode::MotionVectors { "4444" } else { "standard" });
        let (w32, h32) = (u32::try_from(fw).map_err(|e| e.to_string())?, u32::try_from(fh).map_err(|e| e.to_string())?);
        let bytes = if self.in_memory || cfg!(target_arch = "wasm32") {
            let (cur, _) =
                filmcraft_export::encode_prores_mov(std::io::Cursor::new(Vec::new()), w32, h32, self.rate, profile, frames).map_err(|e| e.to_string())?;
            let data = cur.into_inner();
            self.services.write_file(&w.path, &data).map_err(|e| format!("{}: {e}", w.path))?;
            Some(Arc::<[u8]>::from(data))
        } else {
            // a new file only: the name was checked to be free, and nothing else may be overwritten
            let file = std::fs::OpenOptions::new().write(true).create_new(true).open(&w.path).map_err(|e| format!("{}: {e}", w.path))?;
            let (buf, _) =
                filmcraft_export::encode_prores_mov(std::io::BufWriter::new(file), w32, h32, self.rate, profile, frames).map_err(|e| e.to_string())?;
            buf.into_inner().map_err(|e| format!("{}: {}", w.path, e.error()))?.sync_all().map_err(|e| format!("{}: {e}", w.path))?;
            None
        };
        out.hardware_frames += hw;
        out.cpu_frames += cpu;
        if let Some(why) = est.fallback_reason()
            && out.fallback.is_none()
        {
            out.fallback = Some(why.to_string());
        }
        out.viz_gpu_frames += viz_gpu;
        out.viz_cpu_frames += viz_cpu;
        if let Some(why) = viz.fallback_reason()
            && out.viz_fallback.is_none()
        {
            out.viz_fallback = Some(why.to_string());
        }
        Ok(Rendered { clip: w.clip, start: w.start, dur: w.dur, path: w.path.clone(), bytes })
    }
}

/// Delete generated files (best effort; hosts that keep files in memory have nothing to delete).
fn remove_files<'a>(in_memory: bool, paths: impl IntoIterator<Item = &'a str>) {
    if in_memory || cfg!(target_arch = "wasm32") {
        return;
    }
    for p in paths {
        let _ = std::fs::remove_file(p);
    }
}

/// Place finished jobs' clips (one undo step each) and drop finished or cancelled jobs. Called once
/// per UI frame from [`Session::poll_persistence`].
pub fn poll(s: &mut Session) {
    let mut i = 0;
    while i < s.flow_jobs.len() {
        let job = s.jobs.iter().find(|j| j.id == s.flow_jobs[i].job);
        let finished = job.is_none_or(|j| j.progress.finished.load(Ordering::Relaxed));
        if !finished {
            i += 1;
            continue;
        }
        let pj = s.flow_jobs.remove(i);
        if let Err(e) = apply(s, pj) {
            s.error_toast("clip.opticalFlowOverlay", format!("Optical Flow Track: {e}"));
        }
    }
}

/// Place a finished job's clips, all or nothing.
fn apply(s: &mut Session, pj: PendingFlow) -> Result<Value> {
    let Some(out) = lock(&pj.results).take() else {
        // failed or stopped: the worker already reported it and deleted its files
        return Ok(json!({"clips": []}));
    };
    let in_memory = s.services.export_in_memory();
    let cancelled = s.jobs.iter().find(|j| j.id == pj.job).is_some_and(|j| j.progress.cancel.load(Ordering::Relaxed));
    let discard = |out: &Outcome| remove_files(in_memory, out.clips.iter().map(|r| r.path.as_str()));
    if cancelled {
        discard(&out);
        return Ok(json!({"clips": []}));
    }
    if pj.epoch != s.document_epoch {
        discard(&out);
        return Err(EngineError::Other("the project was replaced while the track was rendering; nothing was changed".into()));
    }
    let (before, undo, redo) = (s.project.clone(), s.history.undo.clone(), s.history.redo.clone());
    let n0 = s.history.undo.len();
    match place_all(s, pj.seq, pj.look, &out) {
        Ok(created) => {
            crate::clip_ops::collapse_history(s, n0, LABEL);
            let backend = out.backend(&lock(&pj.hw_name));
            if let Some(why) = &out.fallback {
                log::warn!("optical flow fell back to {CPU_FLOW_BACKEND}: {why}");
            }
            if let Some(why) = &out.viz_fallback {
                log::warn!("optical-flow overlay drawn on the CPU: {why}");
            }
            if s.state.active_sequence == Some(pj.seq) {
                s.state.selection = created.iter().map(|(c, _)| *c).collect();
            }
            s.toast(format!("Generated {} track via {backend}", pj.look.mode.label()));
            Ok(json!({
                "mode": pj.look.mode.id(),
                "backend": backend,
                "hardware": out.hardware_frames > 0,
                "hardwareFrames": out.hardware_frames,
                "cpuFrames": out.cpu_frames,
                "fallbackReason": out.fallback,
                "overlayBackend": out.viz_backend(),
                "overlayGpuFrames": out.viz_gpu_frames,
                "overlayCpuFrames": out.viz_cpu_frames,
                "overlayFallbackReason": out.viz_fallback,
                "clips": created.iter().map(|(c, _)| c.0).collect::<Vec<_>>(),
                "items": created.iter().map(|(_, i)| i.0).collect::<Vec<_>>(),
                "paths": out.clips.iter().map(|r| r.path.clone()).collect::<Vec<_>>(),
            }))
        }
        Err(e) => {
            // take back every step made above, whatever the history limit dropped meanwhile
            s.project = before;
            s.history.undo = undo;
            s.history.redo = redo;
            s.history.merge_key = None;
            s.fix_state();
            s.bump();
            discard(&out);
            Err(e)
        }
    }
}

/// Import and place every rendered clip; the caller rolls back on error.
fn place_all(s: &mut Session, seq_id: ItemId, look: Look, out: &Outcome) -> Result<Vec<(ClipId, ItemId)>> {
    let mut created = Vec::new();
    for r in &out.clips {
        let track_idx = {
            let seq = s.project.sequence(seq_id).ok_or(EngineError::NoSequence)?;
            seq.video_tracks
                .iter()
                .enumerate()
                .find_map(|(ti, tr)| tr.item(r.clip).filter(|it| it.start == r.start && it.duration == r.dur).map(|_| ti))
                .ok_or_else(|| {
                    EngineError::Other(format!("clip {} was moved, trimmed or deleted while its track was rendering; nothing was changed", r.clip.0))
                })?
        };
        let media_item = match &r.bytes {
            Some(b) => crate::commands::import_bytes(s, &r.path, b.clone(), None)?,
            None => crate::commands::import_streamed(s, &r.path, None)?,
        };
        let rate = s.project.sequence(seq_id).ok_or(EngineError::NoSequence)?.settings.frame_rate;
        let range = TimeRange::new(r.start, r.dur);
        let free = find_free_video_track_above(&s.project, seq_id, track_idx, range);
        let (start, dur) = (r.start, r.dur);
        let clip = s.edit(LABEL, |pr, _st| {
            let mut it = pr
                .make_track_item(media_item, TrackKind::Video, start, TimeRange::new(Tick::ZERO, dur), rate)
                .ok_or_else(|| EngineError::Other("the generated optical-flow file could not be read back".into()))?;
            it.duration = dur;
            if let Some(op) = it.effects.iter_mut().find(|e| e.effect == "opacity") {
                if let Some(p_op) = op.params.get_mut("opacity") {
                    p_op.value = ParamValue::Float(f64::from(look.opacity));
                }
                if let Some(p_bl) = op.params.get_mut("blend") {
                    p_bl.value = ParamValue::Choice(look.blend);
                }
            }
            let id = it.id;
            let new_track = if free.is_none() { Some(TrackId(pr.alloc_id())) } else { None };
            let seq = pr.sequence_mut(seq_id).ok_or(EngineError::NoSequence)?;
            let target = match (free, new_track) {
                (Some(t), _) => t,
                (None, Some(t)) => {
                    seq.video_tracks.push(filmcraft_project::Track::new(t, TrackKind::Video, "Video 0".to_string()));
                    crate::sequence_tools::renumber(&mut seq.video_tracks, "Video");
                    t
                }
                (None, None) => return Err(EngineError::Other("no video track to place the optical-flow clip on".into())),
            };
            let tr = seq.video_tracks.iter_mut().find(|t| t.id == target).ok_or_else(|| EngineError::Other("destination video track is missing".into()))?;
            let pos = tr.items.partition_point(|x| x.start <= start);
            tr.items.insert(pos, it);
            seq.check().map_err(EngineError::Other)?;
            Ok(id)
        })?;
        created.push((clip, media_item));
    }
    Ok(created)
}

#[cfg(test)]
mod tests;
