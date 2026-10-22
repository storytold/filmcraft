//! Effects that look at other frames: Posterize Time, Echo, Auto Reframe and Warp Stabilizer.
//!
//! Warp Stabilizer is a 2-D (similarity / translation) stabilizer. Analysis tracks Shi–Tomasi
//! features with pyramidal Lucas–Kanade and RANSAC. Cached plans optimize the accumulated camera
//! path with a second-difference penalty and precompute crop/zoom coverage. Method 3 additionally
//! retains persistent trajectories, factors their rank-nine subspace and solves a content-preserving
//! mesh per frame. Rendering applies one global transform and, for Method 3, one mesh inverse;
//! optional temporal synthesis uses motion-aligned neighbouring frames.

use parking_lot::Mutex as PlanMutex;
use std::collections::HashMap;
use std::sync::{
    Arc, LazyLock, Mutex, OnceLock,
    atomic::{AtomicUsize, Ordering},
};

use filmcraft_project::{EffectInstance, TrackMethod};
use rayon::prelude::*;

use super::*;
use crate::track::Gray;
mod analysis;
mod epipolar;
mod mesh;
mod numerics;
mod planning;
mod smoothing;
mod subspace;
mod synthesis;
mod trajectories;
mod trajectory_data;
mod trajectory_index;

use analysis::{Analysis, analysed_path};
pub use analysis::{StabilizerProgress, StabilizerStage};
use planning::{PlanSettings, StabilizationPlan, SubspacePlanError, build_plan, build_subspace_plan};

#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
pub(in crate::vfx) struct PlanKey {
    path: usize,
    smooth: u64,
    crop: u64,
    safe: u64,
    maximum: u64,
    flags: u8,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct PlanSelection {
    pub(super) key: PlanKey,
    pub(super) generation: u64,
}

/// Posterize Time: the clip shows a new frame only at the effect's frame rate.
pub fn posterize_time(img: &mut Image, e: &EffectInstance, cx: &FxCtx) {
    let Some(env) = cx.env else { return };
    let rate = fv(e, "rate", cx).max(0.01) as f64;
    let t = cx.seconds;
    let q = (t * rate + 1e-6).floor() / rate;
    let dt = q - t;
    if dt.abs() < 1e-6 {
        return;
    }
    if let Some(f) = env.frame(dt, e) {
        *img = f;
    }
}

/// Echo: composites copies of the clip from earlier (negative Echo Time) or later frames.
pub fn echo(img: &mut Image, e: &EffectInstance, cx: &FxCtx) {
    let Some(env) = cx.env else { return };
    let dt = fv(e, "time", cx) as f64;
    let n = fv(e, "count", cx).clamp(0.0, 30.0) as usize;
    if n == 0 || dt.abs() < 1e-6 {
        return;
    }
    let start = fv(e, "start", cx).clamp(0.0, 1.0);
    let decay = fv(e, "decay", cx).max(0.0);
    let op = chv(e, "operator");
    let mut layers: Vec<(Image, f32)> = vec![(img.clone(), start)];
    for k in 1..=n {
        if let Some(f) = env.frame(dt * k as f64, e) {
            layers.push((f, start * decay.powi(k as i32)));
        }
    }
    *img = combine_echoes(&layers, op);
}

/// Combine echo layers (index 0 = the current frame) with an Echo Operator.
pub(crate) fn combine_echoes(layers: &[(Image, f32)], op: u32) -> Image {
    let (w, h) = (layers[0].0.w, layers[0].0.h);
    let mut out = Image::new(w, h);
    let wsum: f32 = layers.iter().map(|l| l.1).sum::<f32>().max(1e-6);
    out.px.par_chunks_mut(4).enumerate().for_each(|(i, o)| {
        let px = |l: &(Image, f32)| -> [f32; 4] {
            if l.0.w != w || l.0.h != h {
                return [0.0; 4];
            }
            let p = &l.0.px[i * 4..i * 4 + 4];
            [p[0] * l.1, p[1] * l.1, p[2] * l.1, p[3] * l.1]
        };
        let mut acc = match op {
            2 => [f32::INFINITY; 4],
            3 => [1.0; 4],
            _ => [0.0; 4],
        };
        match op {
            4 => {
                // composite in back: the current frame in front, older echoes behind
                for l in layers.iter().rev() {
                    over(&mut acc, px(l));
                }
            }
            5 => {
                // composite in front: echoes over the current frame
                for l in layers {
                    over(&mut acc, px(l));
                }
            }
            _ => {
                for l in layers {
                    let p = px(l);
                    for k in 0..4 {
                        acc[k] = match op {
                            1 => acc[k].max(p[k]),
                            2 => acc[k].min(p[k]),
                            3 => acc[k] * (1.0 - p[k].min(1.0)),
                            6 => acc[k] + p[k] / wsum,
                            _ => acc[k] + p[k],
                        };
                    }
                }
                if op == 3 {
                    acc = acc.map(|v| 1.0 - v);
                }
            }
        }
        if op == 0 {
            acc[3] = acc[3].min(1.0);
        }
        o.copy_from_slice(&acc.map(|v| if v.is_finite() { v.max(0.0) } else { 0.0 }));
    });
    out
}

// ------------------------------------------------------------------ analysis helpers

fn gray_of(img: &Image) -> Gray {
    let px = img.px.par_chunks(4).map(|p| filmcraft_color::linear_to_srgb(luma([p[0], p[1], p[2]]).clamp(0.0, 1.0))).collect();
    Gray::new(img.w, img.h, px)
}

/// Saliency centroid (0..1 of the frame): gradient energy weighted, mildly centre-biased.
pub(crate) fn saliency_centroid(img: &Image) -> (f32, f32) {
    let g = gray_of(img);
    let (w, h) = (g.w, g.h);
    let (mut sx, mut sy, mut sw) = (0.0f64, 0.0f64, 0.0f64);
    for y in 1..h.saturating_sub(1) {
        for x in 1..w.saturating_sub(1) {
            let gx = g.px[y * w + x + 1] - g.px[y * w + x - 1];
            let gy = g.px[(y + 1) * w + x] - g.px[(y - 1) * w + x];
            let (u, v) = (x as f32 / w as f32 - 0.5, y as f32 / h as f32 - 0.5);
            let bias = 1.0 - (u * u + v * v) * 0.8;
            let e = ((gx * gx + gy * gy) * bias.max(0.1)) as f64;
            let e = e * e;
            sx += e * x as f64;
            sy += e * y as f64;
            sw += e;
        }
    }
    if sw <= 1e-12 {
        return (0.5, 0.5);
    }
    (((sx / sw + 0.5) / w as f64) as f32, ((sy / sw + 0.5) / h as f64) as f32)
}

type CentroidCache = Mutex<HashMap<(u64, i64), (f32, f32)>>;
fn centroid_cache() -> &'static CentroidCache {
    static C: OnceLock<CentroidCache> = OnceLock::new();
    C.get_or_init(Default::default)
}

/// Auto Reframe: keeps the subject (a saliency centroid averaged over a time window set by the
/// Motion Preset) in frame while zooming the clip to fill the sequence (or a chosen aspect).
pub fn auto_reframe(img: &mut Image, e: &EffectInstance, cx: &FxCtx) {
    let (lw, lh) = (img.w as f64, img.h as f64);
    // the frame the layer must fill, in layer pixels (Motion at its defaults places the layer
    // centred in the sequence 1:1)
    let target = match (chv(e, "aspect"), cx.env) {
        (0, Some(env)) => {
            let (sw, sh) = env.sequence_size();
            let (src_w, _) = env.source_size();
            let k = lw / src_w.max(1) as f64;
            (sw as f64 * k, sh as f64 * k)
        }
        (0, None) => return,
        (a, _) => {
            let r = [16.0 / 9.0, 9.0 / 16.0, 1.0, 4.0 / 5.0, 16.0 / 9.0][a.min(4) as usize];
            if lw / lh > r { (lh * r, lh) } else { (lw, lw / r) }
        }
    };
    let zoom_user = (fv(e, "zoom", cx) as f64 / 100.0).max(1.0);
    let z = (target.0 / lw).max(target.1 / lh).max(1.0) * zoom_user;
    // window of the source that stays visible
    let (vw, vh) = (target.0.min(lw * z) / z, target.1.min(lh * z) / z);
    let (half, step) = [(1.0, 0.25), (0.5, 0.125), (0.15, 0.05)][chv(e, "preset").min(2) as usize];
    let subject = match cx.env {
        Some(env) => {
            let key = env.clip_key();
            let fps = env.frame_rate().max(1.0);
            let mut acc = (0.0f64, 0.0f64, 0.0f64);
            let mut k = -half;
            while k <= half + 1e-9 {
                let t = cx.seconds + k;
                if t >= -1e-9 && t <= env.clip_seconds() + 1e-9 {
                    let id = (t * fps).round() as i64;
                    let cached = centroid_cache().lock().ok().and_then(|c| c.get(&(key, id)).copied());
                    let c = cached.or_else(|| {
                        let f = env.source_frame(id as f64 / fps - cx.seconds, (160.0 / env.source_size().0.max(1) as f32).min(1.0))?;
                        let c = saliency_centroid(&f);
                        if let Ok(mut m) = centroid_cache().lock() {
                            if m.len() > 100_000 {
                                m.clear();
                            }
                            m.insert((key, id), c);
                        }
                        Some(c)
                    });
                    if let Some((u, v)) = c {
                        let wgt = 1.0 - (k / (half + step)).abs();
                        acc = (acc.0 + u as f64 * wgt, acc.1 + v as f64 * wgt, acc.2 + wgt);
                    }
                }
                k += step;
            }
            if acc.2 > 0.0 { (acc.0 / acc.2, acc.1 / acc.2) } else { (0.5, 0.5) }
        }
        None => {
            let (u, v) = saliency_centroid(&img.downsample2());
            (u as f64, v as f64)
        }
    };
    let off = offv(e, "offset", cx);
    let sx = (subject.0 * lw + off.x).clamp(vw / 2.0, lw - vw / 2.0);
    let sy = (subject.1 * lh + off.y).clamp(vh / 2.0, lh - vh / 2.0);
    // output(p) = input(subject + (p − centre) / z)
    let m = Affine::translate(lw / 2.0, lh / 2.0).then_apply(&Affine::scale(z, z)).then_apply(&Affine::translate(-sx, -sy));
    if (z - 1.0).abs() < 1e-9 && (sx - lw / 2.0).abs() < 1e-6 && (sy - lh / 2.0).abs() < 1e-6 {
        return;
    }
    affine_warp(img, &m);
}

// ------------------------------------------------------------------ Warp Stabilizer

/// A clip's analysed camera motion: per-frame steps (frame k → k+1) in analysis pixels.
#[derive(Clone, Debug)]
pub struct StabilizerPath {
    pub w: usize,
    pub h: usize,
    pub steps: Vec<Affine>,
    pub fps: f64,
}

/// Forget analysis and settings-dependent plans after media changes.
pub fn clear_stabilizer_cache() {
    analysis::clear();
    let mut cache = plan_cache().lock();
    cache.entries.clear();
    cache.retained_bytes = 0;
}

/// Analyse (or fetch the cached analysis of) the clip `env` describes.
/// `now` is the clip-local timeline offset, not source/media time.
pub fn stabilizer_path(env: &dyn FxEnv, now: f64, method: TrackMethod, detailed: bool, fast: bool) -> Option<Arc<StabilizerPath>> {
    analysed_path(env, now, method, detailed, fast, false).map(|a| a.path.clone())
}

/// Snapshot the exact clip/settings analysis without waiting for a renderer or decoding media.
pub fn stabilizer_progress(
    project: &filmcraft_project::Project,
    sequence: &filmcraft_project::Sequence,
    item: &filmcraft_project::TrackItem,
    effect: &EffectInstance,
) -> Option<StabilizerProgress> {
    analysis::progress(project, sequence, item, effect)
}

/// Request explicit cancellation of the matching Warp Stabilizer analysis job (tracking,
/// factorization and settings planning). Returns `false` when nothing is in flight; a cancelled
/// job stays cancelled until [`restart_stabilizer_analysis`].
pub fn cancel_stabilizer_analysis(
    project: &filmcraft_project::Project,
    sequence: &filmcraft_project::Sequence,
    item: &filmcraft_project::TrackItem,
    effect: &EffectInstance,
) -> bool {
    analysis::cancel(project, sequence, item, effect)
}

/// Queue analysis after cancellation or failure. Returns `false` when a matching job is already
/// active or complete; frame requests perform the work without blocking the UI caller.
pub fn restart_stabilizer_analysis(
    project: &filmcraft_project::Project,
    sequence: &filmcraft_project::Sequence,
    item: &filmcraft_project::TrackItem,
    effect: &EffectInstance,
) -> bool {
    analysis::restart(project, sequence, item, effect)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct PlanProgressSnapshot {
    stage: StabilizerStage,
    completed_frames: usize,
    subspace_frames: Option<usize>,
    fallback_reason: Option<&'static str>,
}

const PLAN_CACHE_BUDGET: usize = 64 * 1024 * 1024;
const PLAN_CACHE_MAX_ENTRIES: usize = 8;

// A panicking plan owner must not leave waiters spinning or reclaim the empty entry.
const PLAN_BUILD_FAILED: usize = 2;

#[cfg(test)]
struct PlanEntryHook {
    reached: std::sync::Barrier,
    release: std::sync::Barrier,
    armed: std::sync::atomic::AtomicBool,
}

struct PlanEntry {
    // Pin the analysis so its address cannot be reused by a different path.
    _analysis: Arc<Analysis>,
    generation: u64,
    /// A completed plan is published only after the whole build succeeds.
    plan: PlanMutex<Option<Arc<StabilizationPlan>>>,
    /// Only the owner of `building` performs expensive planning.  The flag is never held while
    /// taking the cache or progress mutexes.  `PLAN_BUILD_FAILED` is a terminal marker for
    /// waiters that already hold this entry after an owner panic.
    building: AtomicUsize,
    snapshot: PlanMutex<PlanProgressSnapshot>,
    retained_bytes: AtomicUsize,
    #[cfg(test)]
    claim_hook: PlanMutex<Option<Arc<PlanEntryHook>>>,
    #[cfg(test)]
    waiter_hook: PlanMutex<Option<Arc<PlanEntryHook>>>,
    #[cfg(test)]
    build_count: AtomicUsize,
}
#[derive(Default)]
struct PlanCacheState {
    entries: HashMap<PlanKey, Arc<PlanEntry>>,
    retained_bytes: usize,
}

type PlanCache = PlanMutex<PlanCacheState>;
fn plan_cache() -> &'static PlanCache {
    static C: LazyLock<PlanCache> = LazyLock::new(Default::default);
    &C
}

fn default_snapshot(analysis: &Analysis) -> PlanProgressSnapshot {
    PlanProgressSnapshot {
        stage: StabilizerStage::Planning,
        completed_frames: 0,
        subspace_frames: analysis.subspace_model.as_ref().map(|_| 0),
        fallback_reason: None,
    }
}

fn cache_entry(analysis: &Arc<Analysis>, key: PlanKey) -> Arc<PlanEntry> {
    let mut cache = plan_cache().lock();
    if let Some(entry) = cache.entries.get(&key) {
        return entry.clone();
    }
    let entry = Arc::new(PlanEntry {
        _analysis: analysis.clone(),
        generation: analysis.next_plan_generation(),
        plan: PlanMutex::new(None),
        building: AtomicUsize::new(0),
        snapshot: PlanMutex::new(default_snapshot(analysis)),
        retained_bytes: AtomicUsize::new(0),
        #[cfg(test)]
        claim_hook: PlanMutex::new(None),
        #[cfg(test)]
        waiter_hook: PlanMutex::new(None),
        #[cfg(test)]
        build_count: AtomicUsize::new(0),
    });
    cache.entries.insert(key, entry.clone());
    enforce_plan_cache_budget(&mut cache, key);
    entry
}

fn update_plan_snapshot(analysis: &Analysis, selection: PlanSelection, entry: &PlanEntry, snapshot: PlanProgressSnapshot) {
    *entry.snapshot.lock() = snapshot;
    analysis.publish_plan_snapshot(selection, snapshot);
}

fn plan_retained_bytes(plan: &StabilizationPlan) -> usize {
    let mut bytes = std::mem::size_of::<StabilizationPlan>();
    bytes = bytes.saturating_add(plan.corrections.capacity().saturating_mul(std::mem::size_of::<Affine>()));
    bytes = bytes.saturating_add(plan.cameras.capacity().saturating_mul(std::mem::size_of::<Affine>()));
    bytes = bytes.saturating_add(plan.zooms.capacity().saturating_mul(std::mem::size_of::<f64>()));
    bytes = bytes.saturating_add(plan.meshes.capacity().saturating_mul(std::mem::size_of::<Option<mesh::MeshFrame>>()));
    for mesh in plan.meshes.iter().flatten() {
        bytes = bytes.saturating_add(mesh.retained_bytes());
    }
    bytes
}

fn remove_entry_if_same(key: PlanKey, entry: &Arc<PlanEntry>) {
    let mut cache = plan_cache().lock();
    if cache.entries.get(&key).is_some_and(|candidate| Arc::ptr_eq(candidate, entry)) {
        cache.retained_bytes = cache.retained_bytes.saturating_sub(entry.retained_bytes.load(Ordering::Acquire));
        cache.entries.remove(&key);
    }
}

fn enforce_plan_cache_budget(cache: &mut PlanCacheState, keep: PlanKey) {
    while cache.retained_bytes > PLAN_CACHE_BUDGET || cache.entries.len() > PLAN_CACHE_MAX_ENTRIES {
        let victim = cache.entries.iter().find_map(|(key, entry)| (*key != keep && entry.building.load(Ordering::Acquire) == 0).then_some(*key));
        let Some(victim) = victim else { break };
        let Some(entry) = cache.entries.remove(&victim) else { continue };
        cache.retained_bytes = cache.retained_bytes.saturating_sub(entry.retained_bytes.load(Ordering::Acquire));
    }
    // Keep the selected plan even when it alone exceeds the variant-cache budget. Its frame
    // count and per-frame mesh are separately bounded. Dropping it here would rebuild the entire
    // clip on every rendered frame; all other completed variants have already been evicted.
}

fn retain_plan(key: PlanKey, entry: &Arc<PlanEntry>, plan: Arc<StabilizationPlan>) -> Arc<StabilizationPlan> {
    let bytes = plan_retained_bytes(&plan);
    {
        let mut slot = entry.plan.lock();
        if let Some(existing) = slot.as_ref() {
            return existing.clone();
        }
        *slot = Some(plan.clone());
    }
    entry.retained_bytes.store(bytes, Ordering::Release);
    let mut cache = plan_cache().lock();
    if cache.entries.get(&key).is_some_and(|candidate| Arc::ptr_eq(candidate, entry)) {
        cache.retained_bytes = cache.retained_bytes.saturating_add(bytes);
        enforce_plan_cache_budget(&mut cache, key);
    }
    plan
}

fn build_plan_for_entry(analysis: &Arc<Analysis>, settings: &PlanSettings, selection: PlanSelection, entry: &Arc<PlanEntry>) -> Option<StabilizationPlan> {
    let total = analysis.path.steps.len().saturating_add(1);
    let mut snapshot = default_snapshot(analysis);
    if analysis.cancelled() {
        snapshot.stage = StabilizerStage::Cancelled;
        update_plan_snapshot(analysis, selection, entry, snapshot);
        return None;
    }
    if analysis.subspace_model.is_none() {
        snapshot.subspace_frames = None;
        snapshot.fallback_reason = analysis.progress.lock().fallback_reason;
    }
    update_plan_snapshot(analysis, selection, entry, snapshot);

    let plan = if let (Some(data), Some(model)) = (analysis.trajectories.as_ref(), analysis.subspace_model.as_ref()) {
        let mut completed_frames = 0usize;
        let mut solved_frames = 0usize;
        let mut report = |completed: usize, solved: usize| {
            completed_frames = completed.min(total);
            solved_frames = solved;
            update_plan_snapshot(
                analysis,
                selection,
                entry,
                PlanProgressSnapshot { stage: StabilizerStage::Planning, completed_frames, subspace_frames: Some(solved_frames), fallback_reason: None },
            );
        };
        match build_subspace_plan(&analysis.path, data, model, settings, &mut report) {
            Ok(plan) => plan,
            Err(SubspacePlanError::Cancelled) => {
                update_plan_snapshot(
                    analysis,
                    selection,
                    entry,
                    PlanProgressSnapshot { stage: StabilizerStage::Cancelled, completed_frames, subspace_frames: Some(solved_frames), fallback_reason: None },
                );
                return None;
            }
            Err(SubspacePlanError::Fallback(reason)) => {
                update_plan_snapshot(
                    analysis,
                    selection,
                    entry,
                    PlanProgressSnapshot {
                        stage: StabilizerStage::Planning,
                        completed_frames,
                        subspace_frames: Some(solved_frames),
                        fallback_reason: Some(reason),
                    },
                );
                log::warn!("Warp Stabilizer Subspace Warp fallback: {reason}");
                // A cancelled request must not pay for a global fallback that it will discard.
                if analysis.cancelled() {
                    update_plan_snapshot(
                        analysis,
                        selection,
                        entry,
                        PlanProgressSnapshot {
                            stage: StabilizerStage::Cancelled,
                            completed_frames,
                            subspace_frames: Some(solved_frames),
                            fallback_reason: Some(reason),
                        },
                    );
                    return None;
                }
                build_plan(&analysis.path, settings)
            }
        }
    } else {
        build_plan(&analysis.path, settings)
    };

    if analysis.cancelled() {
        let latest_snapshot = *entry.snapshot.lock();
        update_plan_snapshot(
            analysis,
            selection,
            entry,
            PlanProgressSnapshot {
                stage: StabilizerStage::Cancelled,
                completed_frames: latest_snapshot.completed_frames.min(total),
                subspace_frames: latest_snapshot.subspace_frames,
                fallback_reason: latest_snapshot.fallback_reason,
            },
        );
        return None;
    }
    let final_snapshot = *entry.snapshot.lock();
    update_plan_snapshot(
        analysis,
        selection,
        entry,
        PlanProgressSnapshot {
            stage: StabilizerStage::Complete,
            completed_frames: total,
            subspace_frames: final_snapshot.subspace_frames,
            fallback_reason: final_snapshot.fallback_reason,
        },
    );
    Some(plan)
}
fn mark_plan_failed(analysis: &Arc<Analysis>, selection: PlanSelection, key: PlanKey, entry: &Arc<PlanEntry>) {
    entry.building.store(PLAN_BUILD_FAILED, Ordering::Release);
    let latest_snapshot = *entry.snapshot.lock();
    let stage = if analysis.cancelled() { StabilizerStage::Cancelled } else { StabilizerStage::Failed };
    update_plan_snapshot(analysis, selection, entry, PlanProgressSnapshot { stage, ..latest_snapshot });
    remove_entry_if_same(key, entry);
}

fn discard_cancelled_plan(analysis: &Arc<Analysis>, selection: PlanSelection, key: PlanKey, entry: &Arc<PlanEntry>) {
    entry.building.store(0, Ordering::Release);
    let latest_snapshot = *entry.snapshot.lock();
    update_plan_snapshot(
        analysis,
        selection,
        entry,
        PlanProgressSnapshot {
            stage: StabilizerStage::Cancelled,
            completed_frames: latest_snapshot.completed_frames,
            subspace_frames: latest_snapshot.subspace_frames,
            fallback_reason: latest_snapshot.fallback_reason,
        },
    );
    remove_entry_if_same(key, entry);
}

fn run_plan_owner(
    analysis: &Arc<Analysis>,
    settings: &PlanSettings,
    selection: PlanSelection,
    key: PlanKey,
    entry: &Arc<PlanEntry>,
) -> Option<Arc<StabilizationPlan>> {
    #[cfg(test)]
    entry.build_count.fetch_add(1, Ordering::Relaxed);
    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let built =
            analysis.with_cancel(|| std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| build_plan_for_entry(analysis, settings, selection, entry))));
        let built = match built {
            Ok(built) => built,
            Err(_) => {
                log::error!("Warp Stabilizer planning failed: a worker panicked");
                mark_plan_failed(analysis, selection, key, entry);
                return None;
            }
        };
        let Some(plan) = built else {
            discard_cancelled_plan(analysis, selection, key, entry);
            return None;
        };
        if analysis.cancelled() {
            discard_cancelled_plan(analysis, selection, key, entry);
            return None;
        }
        // Check cancellation under the progress lock, but never retain the plan while holding
        // that lock.  retain_plan takes the entry plan lock (and then the global cache lock);
        // keeping those locks outside progress preserves the selected_plan -> progress ->
        // entry/cache ordering used by publication.
        let publish_allowed = {
            let progress = analysis.progress.lock();
            !analysis.cancelled() && progress.stage != StabilizerStage::Cancelled
        };
        if !publish_allowed {
            discard_cancelled_plan(analysis, selection, key, entry);
            return None;
        }
        let retained = retain_plan(key, entry, Arc::new(plan));
        // Cancellation can win while the complete plan is being retained.  Recheck while
        // publishing the owner bit so a cancelled owner cannot leave a usable partial result.
        let progress = analysis.progress.lock();
        if analysis.cancelled() || progress.stage == StabilizerStage::Cancelled {
            drop(progress);
            discard_cancelled_plan(analysis, selection, key, entry);
            return None;
        }
        // Keep the owner bit set until the completed plan is visible, preventing a duplicate
        // expensive build in the small hand-off window between planning and publication.
        entry.building.store(0, Ordering::Release);
        drop(progress);
        Some(retained)
    }));
    match outcome {
        Ok(result) => result,
        Err(_) => {
            log::error!("Warp Stabilizer planning worker terminated unexpectedly");
            mark_plan_failed(analysis, selection, key, entry);
            None
        }
    }
}

fn cached_plan(analysis: &Arc<Analysis>, settings: &PlanSettings) -> Option<Arc<StabilizationPlan>> {
    if analysis.cancelled() {
        return None;
    }
    let key = PlanKey {
        path: Arc::as_ptr(&analysis.path) as usize,
        smooth: settings.smooth_frames.to_bits(),
        crop: settings.crop_less.to_bits(),
        safe: settings.action_safe.to_bits(),
        maximum: settings.max_scale.to_bits(),
        flags: settings.no_motion as u8
            | (settings.preserve_scale as u8) << 1
            | (settings.adaptive as u8) << 2
            | (analysis.subspace_model.is_some() as u8) << 3,
    };
    let entry = cache_entry(analysis, key);
    let selection = PlanSelection { key, generation: entry.generation };
    analysis.select_plan(selection);
    // A joiner never initializes planning progress.  Only the thread that wins `building` below
    // may reset the stage, so scrubbing requests cannot rewind a plan that is already in flight.
    loop {
        // Clone in a separate statement: the entry-plan guard must be gone before publishing the
        // snapshot, which takes selected_plan and then progress.
        let published = { entry.plan.lock().clone() };
        if let Some(plan) = published {
            if analysis.cancelled() {
                analysis.publish_plan_snapshot(
                    selection,
                    PlanProgressSnapshot { stage: StabilizerStage::Cancelled, completed_frames: 0, subspace_frames: None, fallback_reason: None },
                );
                return None;
            }
            let snapshot = *entry.snapshot.lock();
            analysis.publish_plan_snapshot(selection, snapshot);
            return Some(plan);
        }
        #[cfg(test)]
        let waiter_hook = { entry.waiter_hook.lock().clone() };
        #[cfg(test)]
        if let Some(hook) = waiter_hook
            && hook.armed.swap(false, Ordering::AcqRel)
        {
            hook.reached.wait();
            hook.release.wait();
        }
        if analysis.cancelled() {
            update_plan_snapshot(
                analysis,
                selection,
                &entry,
                PlanProgressSnapshot {
                    stage: StabilizerStage::Cancelled,
                    completed_frames: 0,
                    subspace_frames: analysis.subspace_model.as_ref().map(|_| 0),
                    fallback_reason: None,
                },
            );
            // A waiter must not evict an entry owned by another in-flight builder.  The owner
            // removes only its own cancelled build below; leaving this empty entry permits a
            // later retry to claim it cleanly.
            return None;
        }
        if entry.building.load(Ordering::Acquire) == PLAN_BUILD_FAILED {
            return None;
        }
        #[cfg(test)]
        let claim_hook = { entry.claim_hook.lock().clone() };
        #[cfg(test)]
        if let Some(hook) = claim_hook
            && hook.armed.swap(false, Ordering::AcqRel)
        {
            hook.reached.wait();
            hook.release.wait();
        }
        if entry.building.compare_exchange(0, 1, Ordering::AcqRel, Ordering::Acquire).is_ok() {
            // A previous owner may have published immediately before this CAS became visible.
            // Recheck both publication and cancellation before starting an expensive duplicate.
            let published_after_claim = { entry.plan.lock().clone() };
            if let Some(plan) = published_after_claim {
                entry.building.store(0, Ordering::Release);
                if analysis.cancelled() {
                    return None;
                }
                let snapshot = *entry.snapshot.lock();
                analysis.publish_plan_snapshot(selection, snapshot);
                return Some(plan);
            }
            if analysis.cancelled() {
                entry.building.store(0, Ordering::Release);
                return None;
            }
            if !analysis.begin_plan(selection) {
                entry.building.store(0, Ordering::Release);
                return None;
            }
            // Keep the build and publication on a scoped std stack. Rayon render workers only
            // wait/help below; they can never become the plan owner or recursively steal it.
            let result = std::thread::scope(|scope| {
                let spawned =
                    std::thread::Builder::new().name("warp-plan".to_owned()).spawn_scoped(scope, || run_plan_owner(analysis, settings, selection, key, &entry));
                match spawned {
                    Ok(worker) => {
                        while !worker.is_finished() {
                            match rayon::yield_now() {
                                Some(rayon::Yield::Executed) => {}
                                Some(rayon::Yield::Idle) | None => std::thread::park_timeout(std::time::Duration::from_millis(1)),
                            }
                        }
                        match worker.join() {
                            Ok(result) => result,
                            Err(_) => {
                                log::error!("Warp Stabilizer planning worker terminated unexpectedly");
                                mark_plan_failed(analysis, selection, key, &entry);
                                None
                            }
                        }
                    }
                    Err(error) => {
                        log::error!("Warp Stabilizer planning worker could not be spawned: {error}");
                        mark_plan_failed(analysis, selection, key, &entry);
                        None
                    }
                }
            });
            return result;
        }
        // Another settings request is building this exact key.  Do not hold any cache or progress
        // lock while waiting, and let the analysis's cancellation abandon this caller without
        // poisoning the entry.
        if analysis.cancelled() || filmcraft_media::cancel::cancelled() {
            return None;
        }
        match rayon::yield_now() {
            Some(rayon::Yield::Executed) => {}
            Some(rayon::Yield::Idle) | None => std::thread::park_timeout(std::time::Duration::from_millis(1)),
        }
    }
}

fn finite_param(e: &EffectInstance, id: &str, cx: &FxCtx, default: f64, lo: f64, hi: f64) -> f64 {
    let v = fv(e, id, cx) as f64;
    if v.is_finite() { v.clamp(lo, hi) } else { default }
}

/// Analysis coordinates → layer coordinates, with independent horizontal/vertical scaling.
fn frame_transform(correction: &Affine, zoom: f64, analysis: (usize, usize), layer: (usize, usize)) -> Affine {
    let (sx, sy) = (layer.0 as f64 / analysis.0 as f64, layer.1 as f64 / analysis.1 as f64);
    let (cx, cy) = (layer.0 as f64 / 2.0, layer.1 as f64 / 2.0);
    Affine::translate(cx, cy)
        .then_apply(&Affine::scale(zoom, zoom))
        .then_apply(&Affine::translate(-cx, -cy))
        .then_apply(&Affine::scale(sx, sy))
        .then_apply(correction)
        .then_apply(&Affine::scale(1.0 / sx, 1.0 / sy))
}

/// Warp Stabilizer: see the module docs.
pub fn warp_stabilizer(img: &mut Image, e: &EffectInstance, cx: &FxCtx) {
    let Some(env) = cx.env else { return };
    if !analysis::valid_image(img) || !env.clip_offset().is_finite() {
        return;
    }
    let now = env.clip_offset();
    let method_choice = chv(e, "method");
    let subspace = method_choice == 3;
    let method = if method_choice == 0 { TrackMethod::Position } else { TrackMethod::PositionScaleRotation };
    let Some(analysis) = analysed_path(env, now, method, bv(e, "detailed"), bv(e, "fast_analysis"), subspace) else { return };
    let path = &analysis.path;
    let settings = PlanSettings {
        smooth_frames: finite_param(e, "smoothness", cx, 50.0, 0.0, 1000.0) / 100.0 * path.fps,
        no_motion: chv(e, "result") == 1,
        preserve_scale: bv(e, "preserve_scale"),
        crop_less: finite_param(e, "crop_less", cx, 50.0, 0.0, 100.0) / 100.0,
        action_safe: finite_param(e, "action_safe", cx, 0.0, 0.0, 100.0) / 100.0,
        max_scale: finite_param(e, "max_scale", cx, 150.0, 100.0, 1000.0) / 100.0,
        adaptive: chv(e, "zoom_mode") == 1,
    };
    let Some(plan) = cached_plan(&analysis, &settings) else { return };
    let k = (now * path.fps).round().max(0.0).min(path.steps.len() as f64) as usize;
    let Some(correction) = plan.corrections.get(k) else { return };
    let framing = chv(e, "framing");
    let extra = finite_param(e, "additional_scale", cx, 100.0, 50.0, 400.0) / 100.0;
    let zoom = if framing == 2 { plan.zooms.get(k).copied().unwrap_or(1.0) } else { 1.0 } * extra;
    let m = frame_transform(correction, zoom, (path.w, path.h), (img.w, img.h));
    let mesh = if subspace { plan.meshes.get(k).and_then(Option::as_ref) } else { None };
    if framing == 3 {
        synthesis::warp(img, &m, env, e, &analysis, &plan, k, now, mesh);
    } else if let Some(mesh) = mesh {
        synthesis::warp_single(img, &m, mesh, (path.w, path.h));
    } else {
        affine_warp(img, &m);
    }
    if framing == 1 {
        // The crop is clip-global, in the intersection of all corrected source polygons.
        let [l, t, r, b] = plan.crop_rect;
        if r <= l || b <= t {
            // No common crop rectangle (e.g. a pan longer than the frame under No Motion):
            // keep the stabilized frame uncropped instead of blanking every frame of the clip.
            log::warn!("Warp Stabilizer: no common crop rectangle; showing the stabilized frame uncropped");
            return;
        }
        let (aw, ah) = (path.w as f64, path.h as f64);
        let (cx, cy) = (aw / 2.0, ah / 2.0);
        crate::effects::crop(
            img,
            ((cx + (l - cx) * extra) / aw).clamp(0.0, 1.0) as f32,
            ((cy + (t - cy) * extra) / ah).clamp(0.0, 1.0) as f32,
            (1.0 - (cx + (r - cx) * extra) / aw).clamp(0.0, 1.0) as f32,
            (1.0 - (cy + (b - cy) * extra) / ah).clamp(0.0, 1.0) as f32,
            0.0,
        );
    }
}

#[cfg(test)]
mod stabilizer_geometry_tests {
    use super::*;

    #[test]
    fn analysis_to_layer_mapping_scales_both_axes_before_output_zoom() {
        let correction = Affine::translate(54.0, 23.0).then_apply(&Affine::rotate_deg(90.0)).then_apply(&Affine::translate(-50.0, -25.0));
        let matrix = frame_transform(&correction, 1.2, (100, 50), (200, 150));
        let point = matrix.apply(Vec2::new(40.0, 30.0));
        assert!((point.x - 145.6).abs() < 1e-9);
        assert!((point.y + 40.2).abs() < 1e-9);
    }
}
