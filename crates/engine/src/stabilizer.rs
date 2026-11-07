//! Warp Stabilizer analysis (`effects.stabilize`, `effects.stopStabilize`).
//!
//! Applying Warp Stabilizer does **not** analyse the clip: the effect renders unchanged until the
//! user presses its Stabilize button. That command sets the effect's `stabilize` flag and runs the
//! frame analysis in a background job (`jobs.list` shows it, `jobs.cancel` stops it). Rendering
//! then reads the cached camera path; it never starts an analysis on its own.
//!
//! Changing any other parameter of the effect clears `stabilize` and stops the running analysis
//! ([`cancel`]), so the button has to be pressed again — as in Premiere. `effects.stopStabilize`
//! stops the job and clears the flag in one step.

use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

use filmcraft_project::effect::StabilizerAnalysis;
use filmcraft_project::{ClipId, ItemId, ParamValue, TrackItem, TrackMethod};
use serde_json::{Value, json};

use crate::commands::{CommandSpec, bad, bool_p, clip_p, has_seq};
use crate::{EngineError, Result, Session};

/// A running Warp Stabilizer analysis: which clip's effect it belongs to.
pub struct PendingStabilizer {
    pub job: u64,
    pub clip: ClipId,
    pub effect: usize,
    /// The sequence the clip lives in (the analysis uses that sequence's frame rate).
    pub seq: ItemId,
    /// Cooperative cancellation for the analysis threads (the job's own flag as well, so
    /// `jobs.cancel` reaches both).
    stop: Arc<AtomicBool>,
    /// The finished analysis, attached by the worker and saved into the effect by [`poll`].
    analysis: Arc<Mutex<Option<StabilizerAnalysis>>>,
}

pub(crate) fn commands() -> Vec<CommandSpec> {
    vec![
        CommandSpec {
            id: "effects.stabilize",
            label: "Stabilize",
            menu: &[],
            shortcut: None,
            params: r#"{"clip":id,"effect":index|id?="warp_stabilizer","wait":bool=false}"#,
            enabled: has_seq,
            run: stabilize,
            journal: true,
        },
        CommandSpec {
            id: "effects.stopStabilize",
            label: "Stop Stabilizing",
            menu: &[],
            shortcut: None,
            params: r#"{"clip":id,"effect":index|id?="warp_stabilizer"}"#,
            enabled: has_seq,
            run: stop,
            journal: true,
        },
    ]
}

/// The effect an `effect` parameter (index or id) names on the clip.
fn effect_index(s: &Session, clip: ClipId, p: &Value) -> Result<usize> {
    let seq = s.active_sequence().ok_or(EngineError::NoSequence)?;
    let (_, it) = seq.find_item(clip).ok_or_else(|| bad("effects.stabilize", "no such clip"))?;
    match p.get("effect") {
        None | Some(Value::Null) => it.effects.iter().position(|e| e.effect == "warp_stabilizer"),
        Some(Value::Number(n)) => n.as_u64().map(|n| n as usize).filter(|i| *i < it.effects.len()),
        Some(Value::String(id)) => it.effects.iter().position(|e| &e.effect == id),
        Some(_) => None,
    }
    .ok_or_else(|| bad("effects.stabilize", "no Warp Stabilizer effect on that clip"))
}

fn check(s: &Session, clip: ClipId, idx: usize) -> Result<TrackItem> {
    let seq = s.active_sequence().ok_or(EngineError::NoSequence)?;
    let (_, it) = seq.find_item(clip).ok_or_else(|| bad("effects.stabilize", "no such clip"))?;
    let e = it.effects.get(idx).ok_or_else(|| bad("effects.stabilize", "no such effect on that clip"))?;
    if e.effect != "warp_stabilizer" {
        return Err(bad("effects.stabilize", format!("effect {} is not Warp Stabilizer", e.effect)));
    }
    Ok(it.clone())
}

/// Whether an analysis is running for the clip's effect (the Stabilize button shows Stop).
pub fn running(s: &Session, clip: ClipId, effect: usize) -> bool {
    let Some(pj) = s.stabilizer_jobs.iter().find(|j| j.clip == clip && j.effect == effect) else { return false };
    s.jobs.iter().find(|j| j.id == pj.job).is_some_and(|j| !j.progress.finished.load(Ordering::Relaxed))
}

/// Stop the analysis of the clip's effect (if any): the worker gives up at its next frame check.
pub fn cancel(s: &mut Session, clip: ClipId, effect: usize) {
    if let Some(pj) = s.stabilizer_jobs.iter().find(|j| j.clip == clip && j.effect == effect)
        && let Some(j) = s.jobs.iter().find(|j| j.id == pj.job)
    {
        j.progress.cancel.store(true, Ordering::Relaxed);
        pj.stop.store(true, Ordering::Relaxed);
    }
}

/// One undo step for pressing Stabilize and the analysis finishing: the flag edit and the saved
/// analysis merge while nothing else was edited in between.
fn merge_key(clip: ClipId, effect: usize) -> String {
    format!("stabilize:{}:{effect}", clip.0)
}

/// Set the effect's `stabilize` parameter; turning it off also drops any stored analysis.
fn set_flag(s: &mut Session, clip: ClipId, effect: usize, on: bool) -> Result<()> {
    let key = merge_key(clip, effect);
    s.edit_sequence_as(if on { "Stabilize Clip" } else { "Stop Stabilizing" }, Some(&key), |q, _, _| {
        let (_, it) = q.find_item_mut(clip).ok_or(filmcraft_edit::EditError::NoItem(clip))?;
        let e = it.effects.get_mut(effect).ok_or_else(|| bad("effects.stabilize", "the effect is gone"))?;
        if let Some(p) = e.param_mut("stabilize") {
            p.value = ParamValue::Bool(on);
            p.keyframes.clear();
        }
        if !on {
            e.analysis = None;
        }
        Ok(())
    })
}

fn method_of(it: &TrackItem, idx: usize) -> (TrackMethod, bool) {
    let Some(e) = it.effects.get(idx) else { return (TrackMethod::PositionScaleRotation, false) };
    let method = match e.params.get("method").map(|p| &p.value) {
        Some(ParamValue::Choice(0)) => TrackMethod::Position,
        _ => TrackMethod::PositionScaleRotation,
    };
    let detailed = matches!(e.params.get("detailed").map(|p| &p.value), Some(ParamValue::Bool(true)));
    (method, detailed)
}

fn stabilize(s: &mut Session, p: &Value) -> Result<Value> {
    let clip = clip_p(p, "clip").ok_or_else(|| bad("effects.stabilize", "need `clip`"))?;
    let idx = effect_index(s, clip, p)?;
    let it = check(s, clip, idx)?;
    let seq_id = s.state.active_sequence.ok_or(EngineError::NoSequence)?;
    if running(s, clip, idx) {
        return Err(bad("effects.stabilize", "this clip is already being stabilized"));
    }
    // the flag is on while the analysis runs and stays on afterwards, so the renderer uses the
    // cached path; touching another parameter clears it (and stops the job)
    set_flag(s, clip, idx, true)?;
    let (method, detailed) = method_of(&it, idx);
    let project = s.project.clone();
    let provider = s.media.provider(project.clone(), s.services.clone());
    let Some(seq) = project.sequence(seq_id).cloned() else { return Err(EngineError::NoSequence) };
    let Some(item) = seq.find_item(clip).map(|(_, i)| i.clone()) else {
        return Err(bad("effects.stabilize", "the clip is gone"));
    };
    if filmcraft_render::source_size(&project, item.item).is_none() {
        return Err(bad("effects.stabilize", "the clip has no picture to analyse"));
    }
    let id = s.jobs.iter().map(|j| j.id).max().unwrap_or(0) + 1;
    let frames = (item.duration.seconds() * seq.settings.frame_rate.as_f64()).round().max(1.0) as u64;
    let job = crate::Job { id, label: format!("Stabilizing {}", item.name), progress: Default::default(), result: Default::default() };
    job.progress.total.store(frames, Ordering::Relaxed);
    let (prog, res) = (job.progress.clone(), job.result.clone());
    let (p_for_frames, status) = (prog.clone(), prog.clone());
    let stop = Arc::new(AtomicBool::new(false));
    let stop_for_run = stop.clone();
    let analysis_slot: Arc<Mutex<Option<StabilizerAnalysis>>> = Arc::new(Mutex::new(None));
    let analysis_for_run = analysis_slot.clone();
    let start = web_time::Instant::now();
    let run = move || {
        let on_frame = move |k: usize| {
            let done = p_for_frames.done.fetch_add(1, Ordering::Relaxed) + 1;
            *status.status.lock().unwrap_or_else(|e| e.into_inner()) = format!("Analysing frame {k} ({done} of {frames})");
        };
        let analysis = filmcraft_media::cancel::with_cancel(&stop_for_run, || {
            filmcraft_render::vfx::analyze_stabilizer(&project, &seq, &item, &provider, method, detailed, Some(&on_frame))
        });
        let stopped = prog.cancel.load(Ordering::Relaxed) || stop_for_run.load(Ordering::Relaxed);
        let seconds = start.elapsed().as_secs_f64();
        let r = match analysis {
            Some(path) if !stopped => {
                // hand the analysis to `poll`, which saves it into the effect's project data
                *analysis_for_run.lock().unwrap_or_else(|e| e.into_inner()) = Some(path.to_analysis(filmcraft_render::vfx::clip_key(&item), method, detailed));
                *prog.status.lock().unwrap_or_else(|e| e.into_inner()) = format!("Stabilized {frames} frame(s) in {seconds:.1}s");
                Ok(filmcraft_export::Report {
                    path: String::new(),
                    frames,
                    seconds,
                    bytes: 0,
                    render_fps: frames as f64 / seconds.max(1e-6),
                    extra_files: Vec::new(),
                })
            }
            _ if stopped => {
                *prog.status.lock().unwrap_or_else(|e| e.into_inner()) = "Stopped".to_string();
                Err("stopped".to_string())
            }
            _ => {
                let msg = "Warp Stabilizer could not analyse this clip".to_string();
                *prog.status.lock().unwrap_or_else(|e| e.into_inner()) = msg.clone();
                Err(msg)
            }
        };
        *res.lock().unwrap_or_else(|e| e.into_inner()) = Some(r);
        prog.finished.store(true, Ordering::Relaxed);
    };
    let run = crate::export_tools::guard_job(job.progress.clone(), job.result.clone(), run);
    s.jobs.push(job);
    s.stabilizer_jobs.push(PendingStabilizer { job: id, clip, effect: idx, seq: seq_id, stop, analysis: analysis_slot });
    let wait = bool_p(p, "wait").unwrap_or(false) || cfg!(target_arch = "wasm32");
    if wait {
        run();
        poll(s);
        let failed = s.jobs.iter().find(|j| j.id == id).and_then(|j| j.result.lock().unwrap_or_else(|e| e.into_inner()).clone());
        if let Some(Err(e)) = failed {
            // the analysis failed: leave the effect as it was before the button (flag off)
            set_flag(s, clip, idx, false)?;
            return Err(EngineError::Other(e));
        }
    } else if let Err(e) = std::thread::Builder::new().name("filmcraft-stabilizer".into()).spawn(run) {
        // the job never started: forget it and clear the flag so the button can try again
        s.jobs.retain(|j| j.id != id);
        s.stabilizer_jobs.retain(|pj| pj.job != id);
        set_flag(s, clip, idx, false)?;
        return Err(EngineError::Other(e.to_string()));
    }
    let mut out = json!({"job": id, "clip": clip.0, "effect": idx, "frames": frames});
    if !wait {
        out["poll"] = json!("jobs.list");
        out["cancel"] = json!("jobs.cancel");
    }
    Ok(out)
}

fn stop(s: &mut Session, p: &Value) -> Result<Value> {
    let clip = clip_p(p, "clip").ok_or_else(|| bad("effects.stopStabilize", "need `clip`"))?;
    let idx = effect_index(s, clip, p)?;
    check(s, clip, idx)?;
    cancel(s, clip, idx);
    set_flag(s, clip, idx, false)?;
    Ok(json!({"stopped": true, "clip": clip.0, "effect": idx}))
}

/// Remove finished analyses from the pending list (the job itself stays in `jobs` for the Events
/// panel / progress UI), save a successful analysis into the effect's project data and report a
/// failure that was not a user stop.
pub fn poll(s: &mut Session) {
    let mut i = 0;
    while i < s.stabilizer_jobs.len() {
        let (job_id, seq, clip, effect) = {
            let pj = &s.stabilizer_jobs[i];
            (pj.job, pj.seq, pj.clip, pj.effect)
        };
        let Some(job) = s.jobs.iter().find(|j| j.id == job_id) else {
            s.stabilizer_jobs.remove(i);
            continue;
        };
        if !job.progress.finished.load(Ordering::Relaxed) {
            i += 1;
            continue;
        }
        let stopped = job.progress.cancel.load(Ordering::Relaxed);
        let error = job.progress.error.lock().unwrap_or_else(|e| e.into_inner()).clone();
        let failed = job.result.lock().unwrap_or_else(|e| e.into_inner()).as_ref().is_some_and(|r| r.is_err());
        let analysis = s.stabilizer_jobs[i].analysis.lock().unwrap_or_else(|e| e.into_inner()).take();
        s.stabilizer_jobs.remove(i);
        if !stopped
            && !failed
            && error.is_none()
            && let Some(a) = analysis
        {
            store_analysis(s, seq, clip, effect, a);
        }
        if s.state.active_sequence != Some(seq) {
            continue;
        }
        if !stopped && (failed || error.is_some()) {
            let msg = error.unwrap_or_else(|| "Warp Stabilizer analysis failed".into());
            s.error_toast("effects.stabilize", format!("Warp Stabilizer: {msg}"));
        }
    }
}

/// Save a finished analysis into the effect, unless the clip changed while it ran (its key no
/// longer matches) or the effect is gone or no longer stabilizing.
fn store_analysis(s: &mut Session, seq: ItemId, clip: ClipId, effect: usize, a: StabilizerAnalysis) {
    let Some(current) = s.project.sequence(seq).and_then(|q| q.find_item(clip)).map(|(_, it)| it.clone()) else { return };
    if filmcraft_render::vfx::clip_key(&current) != a.clip {
        return;
    }
    let Some(e) = current.effects.get(effect) else { return };
    if e.effect != "warp_stabilizer" || !matches!(e.params.get("stabilize").map(|p| &p.value), Some(ParamValue::Bool(true))) {
        return;
    }
    let key = merge_key(clip, effect);
    if let Err(err) = s.edit_merged("Stabilize Clip", &key, |p, _| {
        let Some(q) = p.sequence_mut(seq) else { return Ok(()) };
        let Some((_, it)) = q.find_item_mut(clip) else { return Ok(()) };
        if let Some(e) = it.effects.get_mut(effect)
            && e.effect == "warp_stabilizer"
        {
            e.analysis = Some(a);
        }
        Ok(())
    }) {
        s.error_toast("effects.stabilize", format!("Warp Stabilizer: could not save the analysis: {err}"));
    }
}

/// `jobs.cancel` also stops an analysis (the parameter flag is left as it is; the next analysis
/// replaces the cache anyway). Kept here so the command and the button share one path.
pub fn cancel_job(s: &mut Session, id: u64) {
    if let Some(pj) = s.stabilizer_jobs.iter().find(|j| j.job == id) {
        let (clip, effect) = (pj.clip, pj.effect);
        cancel(s, clip, effect);
    }
}
