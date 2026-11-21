//! `media.analyze` / `media.index`: what is in a piece of footage, before it is on a timeline.
//!
//! An agent can't watch hours of raw footage, and footage without speech has no transcript to
//! read. `media.analyze` runs a background job (`jobs.list`, `jobs.cancel`) that decodes each
//! media item at a reduced size and rate and measures it; `media.index` returns the result: the
//! shots (the scene detector's cuts), per-shot picture measures and flags, a quality score to rank
//! takes against each other, the words spoken in each shot when the item has a transcript, and the
//! audio level over time. The index lives in the session (it is not saved with the project, and
//! analysing changes nothing in the project).
//!
//! Picture measures per sampled frame, on the reduced picture: mean luma (brightness), the share
//! of crushed (luma ≤ 4) and blown (≥ 251) pixels, sharpness (mean absolute Laplacian of luma)
//! and motion (the scene signature's difference to the previous sample). They are measurements,
//! not judgements: camera shake, faces, composition and aesthetics are not measured.

use std::sync::{Arc, Mutex};

use filmcraft_project::ItemId;
use filmcraft_render::scene::{SceneDetector, SceneOptions};
use filmcraft_time::Tick;
use serde_json::{Value, json};

use crate::commands::{CommandSpec, NamedKeys, always, bad, bool_p, f64_p, u64_p};
use crate::{EngineError, Result, Session};

/// Longest side of the analysed pictures.
const ANALYSIS_WIDTH: f64 = 256.0;
/// Footage analysed per item at most (4 h).
const MAX_SECONDS: f64 = 4.0 * 3600.0;
/// Audio level window.
const LEVEL_WINDOW: f64 = 0.5;
/// Mean motion below this: a static shot (a still, or a locked-off shot where nothing moves).
const STATIC_MOTION: f32 = 0.002;
/// Sample-to-sample motion below this: the same picture repeated (a freeze), not a slow pan.
const FROZEN_MOTION: f32 = 0.0003;

pub(crate) fn commands() -> Vec<CommandSpec> {
    vec![
        CommandSpec {
            id: "media.analyze",
            label: "Analyze Footage",
            menu: &[],
            shortcut: None,
            params: r#"{"items":[id]?,"fps":f=6,"wait":bool=false}"#,
            enabled: always,
            run: analyze,
            journal: false,
            named: NamedKeys::NONE,
        },
        CommandSpec {
            id: "media.index",
            label: "Footage Index",
            menu: &[],
            shortcut: None,
            params: r#"{"item":id,"shotsLimit":n=200}"#,
            enabled: always,
            run: index,
            journal: false,
            named: NamedKeys::NONE,
        },
    ]
}

/// Measures of one sampled frame.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct FrameMeasure {
    /// Mean luma, 0..1.
    pub brightness: f32,
    /// Share of pixels with luma ≤ 4.
    pub crushed: f32,
    /// Share of pixels with luma ≥ 251.
    pub blown: f32,
    /// Mean absolute Laplacian of luma, 0..1 (higher = more fine detail).
    pub sharpness: f32,
    /// Difference to the previous sample, 0..1 (0 for the first).
    pub motion: f32,
}

/// Brightness, clipping and sharpness of an RGBA8 picture (`motion` is left at 0).
pub fn measure_frame(rgba: &[u8], w: usize, h: usize) -> FrameMeasure {
    if w == 0 || h == 0 || rgba.len() < w * h * 4 {
        return FrameMeasure::default();
    }
    let luma: Vec<f32> =
        rgba.as_chunks::<4>().0.iter().take(w * h).map(|p| 0.2126 * f32::from(p[0]) + 0.7152 * f32::from(p[1]) + 0.0722 * f32::from(p[2])).collect();
    let n = luma.len() as f32;
    let (mut sum, mut crushed, mut blown) = (0f32, 0usize, 0usize);
    for &y in &luma {
        sum += y;
        crushed += usize::from(y <= 4.0);
        blown += usize::from(y >= 251.0);
    }
    let mut lap = 0f32;
    let mut m = 0usize;
    for yy in 1..h.saturating_sub(1) {
        for xx in 1..w.saturating_sub(1) {
            let c = luma[yy * w + xx];
            let v = 4.0 * c - luma[yy * w + xx - 1] - luma[yy * w + xx + 1] - luma[(yy - 1) * w + xx] - luma[(yy + 1) * w + xx];
            lap += v.abs();
            m += 1;
        }
    }
    FrameMeasure {
        brightness: sum / n / 255.0,
        crushed: crushed as f32 / n,
        blown: blown as f32 / n,
        sharpness: if m > 0 { (lap / m as f32 / 255.0).min(1.0) } else { 0.0 },
        motion: 0.0,
    }
}

/// One shot of an analysed item, in media time.
#[derive(Clone, Debug, PartialEq)]
pub struct Shot {
    pub start: Tick,
    pub end: Tick,
    pub brightness: f32,
    pub crushed: f32,
    pub blown: f32,
    pub sharpness: f32,
    pub motion: f32,
    /// Longest run of repeated pictures (no change between samples), in seconds.
    pub still_seconds: f64,
}

/// What `media.analyze` found in one item.
#[derive(Clone, Debug)]
pub struct MediaIndex {
    pub item: ItemId,
    pub name: String,
    pub duration: Tick,
    pub fps: f64,
    pub frames: usize,
    pub shots: Vec<Shot>,
    /// (rms dBFS, peak dBFS) per [`LEVEL_WINDOW`] from the item's start.
    pub levels: Vec<(f64, f64)>,
    /// The tail past [`MAX_SECONDS`] was not analysed.
    pub truncated: bool,
    /// `Session::document_epoch` of the analysis: a replaced project's ids mean other things.
    pub epoch: u64,
    /// The media analysed. An undo can give another item the same id, so a lookup checks it.
    pub media: Option<filmcraft_project::MediaRef>,
}

/// A running `media.analyze` job; its results are stored by [`poll`] when it finishes.
pub struct PendingIndex {
    pub job: u64,
    pub results: Arc<Mutex<Option<Vec<MediaIndex>>>>,
    pub epoch: u64,
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// Group sampled frames into shots at `cuts` (indices of frames that start a new shot).
pub(crate) fn group_shots(times: &[Tick], measures: &[FrameMeasure], cuts: &[usize], end: Tick, step: f64) -> Vec<Shot> {
    let mut bounds: Vec<usize> = vec![0];
    bounds.extend(cuts.iter().copied().filter(|&c| c > 0 && c < times.len()));
    bounds.push(times.len());
    bounds.dedup();
    bounds
        .windows(2)
        .filter(|w| w[1] > w[0])
        .map(|w| {
            let ms = &measures[w[0]..w[1]];
            let n = ms.len() as f32;
            let mut sharp: Vec<f32> = ms.iter().map(|m| m.sharpness).collect();
            sharp.sort_by(f32::total_cmp);
            // the first frame's motion is the change *into* the shot: leave it out
            let inner = if ms.len() > 1 { &ms[1..] } else { ms };
            let (mut run, mut best) = (0usize, 0usize);
            for m in inner {
                run = if m.motion < FROZEN_MOTION { run + 1 } else { 0 };
                best = best.max(run);
            }
            Shot {
                start: times[w[0]],
                end: times.get(w[1]).copied().unwrap_or(end),
                brightness: ms.iter().map(|m| m.brightness).sum::<f32>() / n,
                crushed: ms.iter().map(|m| m.crushed).fold(0.0, f32::max),
                blown: ms.iter().map(|m| m.blown).fold(0.0, f32::max),
                sharpness: sharp[sharp.len() / 2],
                motion: inner.iter().map(|m| m.motion).sum::<f32>() / inner.len().max(1) as f32,
                still_seconds: best as f64 * step,
            }
        })
        .collect()
}

struct Work {
    item: ItemId,
    name: String,
    src: filmcraft_media::SharedSource,
    /// The project's media info says the item has a picture (an audio-only interpretation of a
    /// file with video has none).
    video: bool,
    media: Option<filmcraft_project::MediaRef>,
}

fn analyse(w: &Work, fps: f64, epoch: u64, prog: &filmcraft_export::Progress, done: &mut u64) -> std::result::Result<MediaIndex, String> {
    use std::sync::atomic::Ordering;
    let info = w.src.info().clone();
    let truncated = info.duration.seconds() > MAX_SECONDS;
    let duration = if truncated { Tick::from_seconds_f64(MAX_SECONDS) } else { info.duration };
    let step = 1.0 / fps;
    let mut times = Vec::new();
    let mut measures = Vec::new();
    let mut cuts = Vec::new();
    if let Some(v) = info.video.as_ref().filter(|_| w.video) {
        let scale = (ANALYSIS_WIDTH / f64::from(v.width.max(1))).min(1.0) as f32;
        let n = (duration.seconds() * fps).floor().max(1.0) as usize;
        let mut det = SceneDetector::new();
        for k in 0..n {
            if prog.cancel.load(Ordering::Relaxed) {
                return Err("stopped".into());
            }
            let t = Tick::from_seconds_f64(k as f64 * step);
            let f = w
                .src
                .video_frame(filmcraft_media::FrameRequest { time: t, scale })
                .map_err(|e| format!("{}: can't decode the frame at {:.2} s: {e}", w.name, t.seconds()))?;
            let rgba = f.to_rgba8();
            let (fw, fh) = (f.width as usize, f.height as usize);
            let mut m = measure_frame(&rgba, fw, fh);
            let d = det.push_rgba(&rgba, fw, fh);
            m.motion = if k == 0 { 0.0 } else { d };
            times.push(t);
            measures.push(m);
            *done += 1;
            prog.done.store(*done, Ordering::Relaxed);
            *lock(&prog.status) = format!("{}: {:.0} of {:.0} s", w.name, t.seconds(), duration.seconds());
        }
        // a shot at least a quarter of a second long at the analysis rate
        let opts = SceneOptions { sensitivity: 50.0, min_shot_frames: ((fps * 0.25).round() as usize).max(1) };
        cuts = det.cuts(&opts);
    }
    let mut levels = Vec::new();
    if let Some(a) = info.audio_streams.first() {
        let sr = a.sample_rate.max(1);
        let per = ((LEVEL_WINDOW * f64::from(sr)) as usize).max(1);
        let total = duration.to_units_floor(i64::from(sr)).max(0) as usize;
        let mut pos = 0usize;
        while pos < total {
            if prog.cancel.load(Ordering::Relaxed) {
                return Err("stopped".into());
            }
            let n = (total - pos).min(per);
            let b = w.src.audio(pos as i64, n, sr).map_err(|e| format!("{}: can't decode the audio at {:.2} s: {e}", w.name, pos as f64 / f64::from(sr)))?;
            levels.push(crate::audio_levels::measure(&b.channels));
            pos += n;
        }
    }
    let shots = if times.is_empty() { Vec::new() } else { group_shots(&times, &measures, &cuts, duration, step) };
    Ok(MediaIndex { item: w.item, name: w.name.clone(), duration, fps, frames: times.len(), shots, levels, truncated, epoch, media: w.media.clone() })
}

/// The items to analyse: `items`, else the Project selection, else the media of the selected clips.
fn target_items(s: &Session, p: &Value) -> Vec<ItemId> {
    if let Some(a) = p.get("items").and_then(Value::as_array) {
        return a.iter().filter_map(|v| v.as_u64().map(ItemId)).collect();
    }
    if !s.state.project_selection.is_empty() {
        return s.state.project_selection.clone();
    }
    let Some(q) = s.active_sequence() else { return Vec::new() };
    let mut out = Vec::new();
    for c in &s.state.selection {
        if let Some((_, it)) = q.find_item(*c)
            && !out.contains(&it.item)
        {
            out.push(it.item);
        }
    }
    out
}

fn analyze(s: &mut Session, p: &Value) -> Result<Value> {
    let fps = f64_p(p, "fps").unwrap_or(6.0);
    if !fps.is_finite() || !(0.5..=60.0).contains(&fps) {
        return Err(bad("media.analyze", "fps must be between 0.5 and 60"));
    }
    let mut work = Vec::new();
    let mut skipped = Vec::new();
    for item in target_items(s, p) {
        let name = s.project.item(item).map(|i| i.name.clone()).unwrap_or_default();
        let project_video = s.project.item(item).and_then(|i| i.as_media()).map(|m| m.info.video.is_some());
        match s.source(item) {
            Some(src) if src.info().video.is_some() || !src.info().audio_streams.is_empty() => {
                let video = project_video.unwrap_or(src.info().video.is_some()) && src.info().video.is_some();
                let media = media_ref(s, item);
                work.push(Work { item, name, src, video, media });
            }
            Some(_) => skipped.push(json!({"item": item.0, "reason": "no picture or sound"})),
            None => skipped.push(json!({"item": item.0, "reason": "the media can't be opened (offline, a sequence or a graphic)"})),
        }
    }
    if work.is_empty() {
        return Err(bad("media.analyze", "nothing to analyse: pass items (media item ids) or select footage in the Project panel"));
    }
    let count = work.len();
    let total: u64 = work.iter().map(|w| (w.src.info().duration.seconds().min(MAX_SECONDS) * fps).max(1.0) as u64).sum();
    let id = s.jobs.iter().map(|j| j.id).max().unwrap_or(0) + 1;
    let label = format!("Analyze Footage ({} item{})", work.len(), if work.len() == 1 { "" } else { "s" });
    let job = crate::Job { id, label, progress: Default::default(), result: Default::default() };
    job.progress.total.store(total, std::sync::atomic::Ordering::Relaxed);
    let results: Arc<Mutex<Option<Vec<MediaIndex>>>> = Arc::default();
    let (prog, res, out) = (job.progress.clone(), job.result.clone(), results.clone());
    let epoch = s.document_epoch;
    let run = move || {
        use std::sync::atomic::Ordering;
        let t0 = web_time::Instant::now();
        let mut done = 0u64;
        let mut found = Vec::new();
        let mut err = None;
        for w in &work {
            match analyse(w, fps, epoch, &prog, &mut done) {
                Ok(ix) => found.push(ix),
                Err(e) => {
                    err = Some(e);
                    break;
                }
            }
        }
        let secs = t0.elapsed().as_secs_f64();
        *lock(&prog.status) = match &err {
            Some(e) if e == "stopped" => "Stopped: nothing was stored".into(),
            Some(e) => e.clone(),
            None => format!("Analysed {} item(s), {done} frame(s) ({secs:.1}s)", found.len()),
        };
        let r = match err {
            Some(e) => Err(e),
            None => {
                *lock(&out) = Some(found);
                Ok(filmcraft_export::Report {
                    path: String::new(),
                    frames: done,
                    seconds: secs,
                    bytes: 0,
                    render_fps: done as f64 / secs.max(1e-6),
                    ..Default::default()
                })
            }
        };
        *lock(&res) = Some(r);
        prog.finished.store(true, Ordering::Relaxed);
    };
    let run = crate::export_tools::guard_job(job.progress.clone(), job.result.clone(), run);
    s.jobs.push(job);
    s.index_jobs.push(PendingIndex { job: id, results, epoch });
    let mut reply = json!({"job": id, "items": count, "frames": total, "skipped": skipped, "poll": "jobs.list", "then": "media.index"});
    if bool_p(p, "wait").unwrap_or(false) || cfg!(target_arch = "wasm32") {
        run();
        poll(s);
        if let Some(Err(e)) = s.jobs.iter().find(|j| j.id == id).and_then(|j| lock(&j.result).clone()) {
            return Err(EngineError::Other(e));
        }
        reply["done"] = json!(true);
    } else {
        std::thread::Builder::new().name("filmcraft-media-index".into()).spawn(run).map_err(|e| EngineError::Other(e.to_string()))?;
    }
    Ok(reply)
}

/// Store the results of finished jobs. Called once per UI frame and after synchronous runs.
pub fn poll(s: &mut Session) {
    use std::sync::atomic::Ordering;
    let mut i = 0;
    while i < s.index_jobs.len() {
        let job = s.jobs.iter().find(|j| j.id == s.index_jobs[i].job);
        let finished = job.is_none_or(|j| j.progress.finished.load(Ordering::Relaxed));
        let cancelled = job.is_some_and(|j| j.progress.cancel.load(Ordering::Relaxed));
        if !finished {
            i += 1;
            continue;
        }
        let pj = s.index_jobs.remove(i);
        let Some(results) = lock(&pj.results).take().filter(|_| !cancelled) else { continue };
        if pj.epoch != s.document_epoch {
            continue;
        }
        for ix in results {
            // the item was relinked or replaced while it was analysed: this result describes media
            // it no longer shows, and must not replace a newer analysis of what it shows now
            if ix.media != media_ref(s, ix.item) {
                continue;
            }
            s.media_index.insert(ix.item, Arc::new(ix));
        }
    }
}

fn round3(x: f32) -> f64 {
    (f64::from(x) * 1000.0).round() / 1000.0
}

/// Flags and a 0–100 score for a shot, against the item's median sharpness.
fn assess(sh: &Shot, median_sharpness: f32) -> (Vec<&'static str>, f64) {
    let mut flags = Vec::new();
    // black: nearly every pixel at the floor (a dark night shot has lights and isn't black)
    let black = sh.crushed > 0.95;
    if black {
        flags.push("black");
    } else if sh.brightness < 0.2 {
        flags.push("dark");
    }
    if sh.blown > 0.05 {
        flags.push("overexposed");
    }
    if sh.crushed > 0.25 && !black {
        flags.push("crushedShadows");
    }
    if median_sharpness > 0.0 && sh.sharpness < 0.5 * median_sharpness && !black {
        flags.push("soft");
    }
    if sh.motion < STATIC_MOTION {
        flags.push("static");
    } else if sh.still_seconds >= 1.0 {
        // the picture stops for a second or more inside an otherwise moving shot
        flags.push("frozen");
    }
    let sharp = if median_sharpness > 0.0 { (sh.sharpness / median_sharpness).min(1.5) / 1.5 } else { 1.0 };
    let exposure = 1.0 - (sh.blown * 5.0 + (0.2 - sh.brightness).max(0.0) * 4.0).clamp(0.0, 1.0);
    let alive = if black { 0.0 } else { 1.0 };
    let score = 100.0 * (0.5 * f64::from(sharp) + 0.35 * f64::from(exposure) + 0.15 * alive);
    (flags, (score * 10.0).round() / 10.0)
}

/// The media behind an item (a subclip's parent media).
fn media_ref(s: &Session, item: ItemId) -> Option<filmcraft_project::MediaRef> {
    s.project.resolve_media(item).map(|(_, m, _)| m.media.clone())
}

fn index(s: &mut Session, p: &Value) -> Result<Value> {
    // results of a job that finished since the last poll (headless sessions only poll here)
    poll(s);
    let item = ItemId(u64_p(p, "item").ok_or_else(|| bad("media.index", "item (a media item id) is required"))?);
    let limit = u64_p(p, "shotsLimit").unwrap_or(200).clamp(1, 10_000) as usize;
    let ix = match s.media_index.get(&item) {
        Some(ix) if ix.epoch == s.document_epoch && ix.media == media_ref(s, item) => ix.clone(),
        Some(ix) if ix.epoch == s.document_epoch => {
            return Err(bad("media.index", "the item's media changed since it was analysed (an undo, a relink or a replace): run media.analyze again"));
        }
        _ => {
            let running = s.index_jobs.iter().any(|j| s.jobs.iter().any(|x| x.id == j.job));
            return Err(bad(
                "media.index",
                if running { "the analysis is still running: check jobs.list" } else { "not analysed yet: run media.analyze {items:[id]} first" },
            ));
        }
    };
    let words = s.project.transcripts.get(&item).cloned();
    let mut sharp: Vec<f32> = ix.shots.iter().map(|x| x.sharpness).collect();
    sharp.sort_by(f32::total_cmp);
    let median = sharp.get(sharp.len() / 2).copied().unwrap_or(0.0);
    let shots: Vec<Value> = ix
        .shots
        .iter()
        .take(limit)
        .map(|sh| {
            let (flags, score) = assess(sh, median);
            let text = words.as_ref().map(|tr| {
                let mid = |w: &filmcraft_project::Word| Tick(w.start.0 + (w.end.0 - w.start.0) / 2);
                let w: Vec<&str> = tr.words.iter().filter(|w| mid(w) >= sh.start && mid(w) < sh.end).map(|w| w.text.as_str()).collect();
                w.join(" ")
            });
            json!({
                "start": sh.start.0, "end": sh.end.0,
                "startSeconds": sh.start.seconds(), "seconds": (sh.end - sh.start).seconds(),
                "brightness": round3(sh.brightness), "blown": round3(sh.blown), "crushed": round3(sh.crushed),
                "sharpness": round3(sh.sharpness), "motion": round3(sh.motion), "stillSeconds": sh.still_seconds,
                "flags": flags, "score": score, "text": text,
            })
        })
        .collect();
    let rms: Vec<f64> = ix.levels.iter().map(|l| (l.0 * 10.0).round() / 10.0).collect();
    let quiet: Vec<Value> = crate::audio_levels::quiet_runs(&rms, -45.0, 2)
        .into_iter()
        .map(|(a, b)| json!({"startSeconds": a as f64 * LEVEL_WINDOW, "endSeconds": (b as f64 * LEVEL_WINDOW).min(ix.duration.seconds())}))
        .collect();
    let mut best: Vec<(usize, f64)> = ix.shots.iter().enumerate().map(|(k, sh)| (k, assess(sh, median).1)).collect();
    best.sort_by(|a, b| b.1.total_cmp(&a.1));
    Ok(json!({
        "item": item.0,
        "name": ix.name,
        "durationSeconds": ix.duration.seconds(),
        "analysisFps": ix.fps,
        "frames": ix.frames,
        "truncated": ix.truncated,
        "shotCount": ix.shots.len(),
        "shots": shots,
        "shotsTruncated": ix.shots.len() > limit,
        "bestShots": best.iter().take(5).map(|(k, sc)| json!({"shot": k, "startSeconds": ix.shots[*k].start.seconds(), "score": sc})).collect::<Vec<_>>(),
        "audio": if ix.levels.is_empty() { Value::Null } else { json!({"windowSeconds": LEVEL_WINDOW, "rmsDb": rms, "quiet": quiet}) },
        "hasTranscript": words.is_some(),
        "notes": "Media time. Shots come from frame differences at the analysis rate (fast motion can split a shot; raise fps to be finer). brightness/blown/crushed/sharpness/motion are measured on a reduced picture; score (0–100) ranks takes of this item against each other, it is not an absolute grade. Shake, faces, composition and aesthetics are not measured.",
    }))
}

#[cfg(test)]
mod tests;
