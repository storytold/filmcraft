//! Clip ▸ Scene Edit Detection… (M3.11).
//!
//! `clip.sceneEditDetection {clips?, sensitivity=50, minShotFrames=6, applyCuts=true,
//! createSubclips=false, generateMarkers=false, wait=false}` analyses the selected timeline video
//! clips in a background job (`jobs.list` shows the progress, `jobs.cancel` stops it without
//! changing anything). Every sequence frame of each clip is decoded at reduced size and compared
//! with the previous one ([`filmcraft_render::scene`]). When the job finishes, the results are
//! applied in one undo step ("Scene Edit Detection"):
//!
//! - **Apply a cut at each detected cut point**: the clip (and its linked audio) is cut at the
//!   first frame of each new shot;
//! - **Create a subclip for each cut**: one subclip per shot, in a new bin "<clip> Scenes";
//! - **Generate clip markers**: a marker on the clip's master clip (media time) at each cut.

use std::sync::{Arc, Mutex};

use filmcraft_project::{ClipId, ItemId, ItemKind, Label, Marker, MarkerId, MarkerKind, TrackKind};
use filmcraft_render::scene::{SceneDetector, SceneOptions};
use filmcraft_time::{Tick, TimeRange};
use serde_json::{Value, json};

use crate::commands::{CommandSpec, bad, bool_p, clips_p, f64_p, has_seq, u64_p};
use crate::{EngineError, Result, Session};

/// Longest side of the frames analysed (speed; the measures don't need detail).
const ANALYSIS_WIDTH: f64 = 256.0;

pub(crate) fn commands() -> Vec<CommandSpec> {
    vec![CommandSpec {
        id: "clip.sceneEditDetection",
        label: "Scene Edit Detection…",
        menu: &["Clip"],
        shortcut: None,
        params: r#"{"clips":[id]?,"sensitivity":0..100=50,"minShotFrames":n=6,"applyCuts":bool=true,"createSubclips":bool=false,"generateMarkers":bool=false,"wait":bool=false}"#,
        enabled: has_video_selection,
        run: detect,
        journal: true,
    }]
}

fn has_video_selection(s: &Session) -> std::result::Result<(), String> {
    has_seq(s)?;
    let q = s.active_sequence().ok_or("no sequence is open")?;
    let any = s.state.selection.iter().any(|c| q.find_item(*c).is_some_and(|(t, _)| q.track(t).is_some_and(|t| t.kind == TrackKind::Video)));
    if any { Ok(()) } else { Err("select a video clip in the timeline".into()) }
}

/// What to do with the cuts found.
#[derive(Clone, Copy, Debug, Default)]
pub struct SceneApply {
    pub cuts: bool,
    pub subclips: bool,
    pub markers: bool,
}

/// One analysed clip.
#[derive(Clone, Debug)]
pub struct ClipScenes {
    pub clip: ClipId,
    pub name: String,
    /// The clip's media item (a subclip's parent).
    pub media_item: ItemId,
    /// Media range the clip shows.
    pub media_range: TimeRange,
    /// (timeline time, media time) of the first frame of each new shot.
    pub cuts: Vec<(Tick, Tick)>,
}

/// A running Scene Edit Detection job; applied by [`poll`] when it finishes.
pub struct PendingScene {
    pub job: u64,
    pub seq: ItemId,
    pub apply: SceneApply,
    pub results: Arc<Mutex<Option<Vec<ClipScenes>>>>,
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// The media item a clip shows (a subclip resolves to its parent: subclip clips use the parent's
/// media time).
fn media_root(p: &filmcraft_project::Project, item: ItemId) -> Option<ItemId> {
    p.resolve_media(item).map(|(root, _, _)| root)
}

struct Work {
    clip: ClipId,
    name: String,
    media_item: ItemId,
    src: filmcraft_media::SharedSource,
    scale: f32,
    /// (timeline time, media time) of every sequence frame of the clip.
    frames: Vec<(Tick, Tick)>,
}

fn detect(s: &mut Session, p: &Value) -> Result<Value> {
    let seq_id = s.state.active_sequence.ok_or(EngineError::NoSequence)?;
    let q = s.active_sequence().ok_or(EngineError::NoSequence)?;
    let fd = q.settings.frame_rate.frame_duration();
    let opts = SceneOptions {
        sensitivity: f64_p(p, "sensitivity").unwrap_or(50.0).clamp(0.0, 100.0) as f32,
        min_shot_frames: u64_p(p, "minShotFrames").unwrap_or(6).clamp(1, 10_000) as usize,
    };
    let apply = SceneApply {
        cuts: bool_p(p, "applyCuts").unwrap_or(true),
        subclips: bool_p(p, "createSubclips").unwrap_or(false),
        markers: bool_p(p, "generateMarkers").unwrap_or(false),
    };
    if !(apply.cuts || apply.subclips || apply.markers) {
        return Err(bad("clip.sceneEditDetection", "choose at least one of applyCuts, createSubclips, generateMarkers"));
    }
    if s.scene_jobs.iter().any(|j| j.seq == seq_id) {
        return Err(EngineError::Other("Scene Edit Detection is already running on this sequence".into()));
    }
    let mut work = Vec::new();
    for c in clips_p(s, p) {
        let Some((tid, it)) = q.find_item(c) else { continue };
        if q.track(tid).is_none_or(|t| t.kind != TrackKind::Video) {
            continue;
        }
        let Some(media_item) = media_root(&s.project, it.item) else { continue };
        let Some(src) = s.source(it.item) else { continue };
        let Some(v) = src.info().video.clone() else { continue };
        let n = (it.duration.0 / fd.0.max(1)).max(1);
        let frames: Vec<(Tick, Tick)> = (0..n)
            .map(|j| {
                let t = it.start + Tick(fd.0 * j);
                (t, it.source_time_at(t))
            })
            .collect();
        work.push(Work { clip: c, name: it.name.clone(), media_item, src, scale: (ANALYSIS_WIDTH / v.width.max(1) as f64).min(1.0) as f32, frames });
    }
    if work.is_empty() {
        return Err(EngineError::Other("none of the selected clips is a video clip with media to analyse".into()));
    }
    let total: usize = work.iter().map(|w| w.frames.len()).sum();
    let id = s.jobs.iter().map(|j| j.id).max().unwrap_or(0) + 1;
    let label = format!("Scene Edit Detection ({} clip{})", work.len(), if work.len() == 1 { "" } else { "s" });
    let job = crate::Job { id, label, progress: Default::default(), result: Default::default() };
    job.progress.total.store(total as u64, std::sync::atomic::Ordering::Relaxed);
    let results: Arc<Mutex<Option<Vec<ClipScenes>>>> = Arc::default();
    let (prog, res, out) = (job.progress.clone(), job.result.clone(), results.clone());
    let run = move || {
        use std::sync::atomic::Ordering;
        let t0 = web_time::Instant::now();
        let mut done = 0u64;
        let mut found = Vec::new();
        let mut err = None;
        'clips: for w in &work {
            let mut det = SceneDetector::new();
            for (k, (_, mt)) in w.frames.iter().enumerate() {
                if prog.cancel.load(Ordering::Relaxed) {
                    err = Some("stopped".to_string());
                    break 'clips;
                }
                match w
                    .src
                    .video_frame(filmcraft_media::FrameRequest { time: *mt, scale: w.scale })
                    .and_then(|f| f.to_rgba8().map(|px| (f, px)).map_err(filmcraft_media::MediaError::Decode))
                {
                    Ok((f, px)) => {
                        det.push_rgba(&px, f.width as usize, f.height as usize);
                    }
                    Err(e) => {
                        err = Some(format!("{}: can't decode frame {k}: {e}", w.name));
                        break 'clips;
                    }
                }
                done += 1;
                prog.done.store(done, Ordering::Relaxed);
                *lock(&prog.status) = format!("{}: frame {} of {}", w.name, k + 1, w.frames.len());
            }
            let cuts: Vec<(Tick, Tick)> = det.cuts(&opts).into_iter().map(|i| w.frames[i]).collect();
            let (a, b) = (w.frames.first().map(|f| f.1).unwrap_or_default(), w.frames.last().map(|f| f.1).unwrap_or_default());
            let fdm = w.frames.get(1).map(|f| (f.1 - w.frames[0].1).abs()).unwrap_or(Tick(1)).max(Tick(1));
            found.push(ClipScenes {
                clip: w.clip,
                name: w.name.clone(),
                media_item: w.media_item,
                media_range: TimeRange::from_bounds(a.min(b), a.max(b) + fdm),
                cuts,
            });
        }
        let secs = t0.elapsed().as_secs_f64();
        let n_cuts: usize = found.iter().map(|c| c.cuts.len()).sum();
        *lock(&prog.status) = match &err {
            Some(e) if e == "stopped" => "Stopped: nothing was changed".into(),
            Some(e) => e.clone(),
            None => format!("Found {n_cuts} cut point(s) in {done} frame(s) ({secs:.1}s)"),
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
                    extra_files: Vec::new(),
                })
            }
        };
        *lock(&res) = Some(r);
        prog.finished.store(true, Ordering::Relaxed);
    };
    let run = crate::export_tools::guard_job(job.progress.clone(), job.result.clone(), run);
    s.jobs.push(job);
    s.scene_jobs.push(PendingScene { job: id, seq: seq_id, apply, results: results.clone() });
    let wait = bool_p(p, "wait").unwrap_or(false);
    let mut out = json!({"job": id, "frames": total});
    if wait || cfg!(target_arch = "wasm32") {
        run();
        if let Some(r) = lock(&results).as_ref() {
            out["clips"] = json!(
                r.iter()
                    .map(|c| json!({"clip": c.clip.0, "cuts": c.cuts.iter().map(|x| x.0.0).collect::<Vec<_>>(), "mediaCuts": c.cuts.iter().map(|x| x.1.0).collect::<Vec<_>>()}))
                    .collect::<Vec<_>>()
            );
        }
        poll(s);
    } else {
        std::thread::Builder::new().name("filmcraft-scene-detect".into()).spawn(run).map_err(|e| EngineError::Other(e.to_string()))?;
    }
    if let Some(Err(e)) = s.jobs.iter().find(|j| j.id == id).and_then(|j| lock(&j.result).clone()) {
        return Err(EngineError::Other(e));
    }
    Ok(out)
}

/// Apply finished jobs (one undo step each) and drop finished / cancelled ones. Called once per UI
/// frame from [`Session::poll_persistence`] and after synchronous runs.
pub fn poll(s: &mut Session) {
    use std::sync::atomic::Ordering;
    let mut i = 0;
    while i < s.scene_jobs.len() {
        let job = s.jobs.iter().find(|j| j.id == s.scene_jobs[i].job);
        let finished = job.is_none_or(|j| j.progress.finished.load(Ordering::Relaxed));
        // cancelled before the results were applied: nothing changes
        let cancelled = job.is_some_and(|j| j.progress.cancel.load(Ordering::Relaxed));
        if !finished {
            i += 1;
            continue;
        }
        let pj = s.scene_jobs.remove(i);
        let Some(results) = lock(&pj.results).take().filter(|_| !cancelled) else { continue };
        if let Err(e) = apply_results(s, pj.seq, pj.apply, results) {
            s.error_toast("clip.sceneEditDetection", format!("Scene Edit Detection: {e}"));
        }
    }
}

fn apply_results(s: &mut Session, seq_id: ItemId, apply: SceneApply, results: Vec<ClipScenes>) -> Result<Value> {
    let n_cuts: usize = results.iter().map(|r| r.cuts.len()).sum();
    if n_cuts == 0 {
        s.toast("Scene Edit Detection found no cut points");
        return Ok(json!({"cuts": 0}));
    }
    let media = s.media.clone();
    s.edit("Scene Edit Detection", move |pr, _| {
        let snapshot = pr.clone();
        let durations = move |id: ItemId| crate::media_duration(&snapshot, &media, id);
        for r in &results {
            if apply.cuts {
                let min = pr.sequence(seq_id).map(|q| q.settings.frame_rate.frame_duration()).unwrap_or(Tick(1));
                let mut next = pr.next_id;
                let q = pr.sequence_mut(seq_id).ok_or(EngineError::NoSequence)?;
                let mut ctx = filmcraft_edit::EditCtx { next_id: &mut next, media_duration: &durations, media_start: &|_| Tick::ZERO, min_duration: min };
                // latest cut first: the left piece keeps the clip's id for the earlier cuts
                for (t, _) in r.cuts.iter().rev() {
                    let link = q.find_item(r.clip).and_then(|(_, it)| it.link);
                    let mut items = vec![r.clip];
                    if let Some(l) = link {
                        items.extend(q.all_tracks().flat_map(|tr| tr.items.iter()).filter(|it| it.link == Some(l) && it.id != r.clip).map(|it| it.id));
                    }
                    filmcraft_edit::razor_items(q, &items, *t, &mut ctx);
                }
                pr.next_id = next;
                pr.sequence(seq_id).ok_or(EngineError::NoSequence)?.check().map_err(EngineError::Other)?;
            }
            if apply.markers {
                let ids: Vec<u64> = r.cuts.iter().map(|_| pr.alloc_id()).collect();
                if let Some(m) = pr.item_mut(r.media_item).and_then(|i| i.as_media_mut()) {
                    for ((_, mt), id) in r.cuts.iter().zip(ids) {
                        if m.markers.iter().any(|x| x.start == *mt) {
                            continue;
                        }
                        m.markers.push(Marker {
                            id: MarkerId(id),
                            start: *mt,
                            duration: Tick::ZERO,
                            name: "Scene Edit".into(),
                            comment: String::new(),
                            kind: MarkerKind::Comment,
                            color: Label::Green,
                        });
                    }
                    m.markers.sort_by_key(|x| x.start);
                }
            }
            if apply.subclips {
                let base = pr.item(r.media_item).map(|i| i.name.clone()).unwrap_or_else(|| r.name.clone());
                let label = pr.item(r.media_item).map(|i| i.label).unwrap_or(Label::Iris);
                let bin = pr.add_bin(&format!("{base} Scenes"), None);
                let mut bounds: Vec<Tick> = vec![r.media_range.start];
                bounds.extend(r.cuts.iter().map(|c| c.1).filter(|t| *t > r.media_range.start && *t < r.media_range.end()));
                bounds.push(r.media_range.end());
                bounds.sort();
                bounds.dedup();
                for (k, w) in bounds.windows(2).enumerate() {
                    let kind = ItemKind::Subclip { parent: r.media_item, range: TimeRange::from_bounds(w[0], w[1]), restrict_trims: false };
                    pr.add_item(&format!("{base} Scene {:02}", k + 1), label, kind, Some(bin));
                }
            }
        }
        Ok(())
    })?;
    s.toast(format!("Scene Edit Detection: {n_cuts} cut point(s)"));
    Ok(json!({"cuts": n_cuts}))
}

#[cfg(test)]
#[path = "scene_detect_tests.rs"]
mod tests;
