//! Motion analysis has a bounded preparation batch, independent of stabilization settings.
use parking_lot::Mutex;
use std::collections::HashMap;
use std::sync::{
    Arc, LazyLock, OnceLock,
    atomic::{AtomicBool, AtomicU64, Ordering},
};

use filmcraft_geom::{Affine, Vec2};
use filmcraft_project::{EffectInstance, Project, Sequence, TrackItem, TrackMethod};
use rayon::prelude::*;

use super::subspace::{self, SubspaceError, SubspaceModel};
use super::trajectories::TrajectoryCollector;
use super::trajectory_data::TrajectorySet;
use super::{FxEnv, Image, PlanProgressSnapshot, PlanSelection, StabilizerPath, gray_of};
use crate::track::{MAX_FEATURES, Prepared, features, track_step_with_features};

pub(super) struct Analysis {
    pub path: Arc<StabilizerPath>,
    /// Reliable adjacent-frame links; synthesis never crosses a failed track or cut.
    pub valid_steps: Vec<bool>,
    /// Shared observations retained only for Method 3 (Subspace Warp).
    pub trajectories: Option<Arc<TrajectorySet>>,
    /// Settings-independent subspace factorisation, when it was usable.
    pub subspace_model: Option<Arc<SubspaceModel>>,
    /// The slot's shared progress state; planning updates this same object.
    pub progress: Arc<Mutex<StabilizerProgress>>,
    /// The explicit lifetime of analysis and settings-plan work. It is intentionally independent
    /// from the frame request's cancellation flag.
    cancel: Arc<AtomicBool>,
    /// The currently selected settings-specific plan. In-flight workers check this token before
    /// publishing a progress snapshot, so a stale request cannot overwrite the selected plan.
    selected_plan: Mutex<Option<PlanSelection>>,
    next_plan_generation: AtomicU64,
    #[cfg(test)]
    plan_hook: Option<Arc<PlanHook>>,
}

impl Analysis {
    pub(super) fn next_plan_generation(&self) -> u64 {
        self.next_plan_generation.fetch_add(1, Ordering::Relaxed).wrapping_add(1)
    }

    pub(super) fn cancelled(&self) -> bool {
        self.cancel.load(Ordering::Acquire)
    }

    pub(super) fn with_cancel<R>(&self, work: impl FnOnce() -> R) -> R {
        filmcraft_media::cancel::with_cancel(&self.cancel, work)
    }

    pub(super) fn select_plan(&self, selection: PlanSelection) {
        *self.selected_plan.lock() = Some(selection);
    }

    /// Reset planning progress only for the owner that won this entry's build flight.  The
    /// selection guard keeps a stale owner from rewinding progress after another request has
    /// selected and published a different settings variant.
    pub(super) fn begin_plan(&self, selection: PlanSelection) -> bool {
        let selected = self.selected_plan.lock();
        if selected.as_ref() != Some(&selection) {
            return false;
        }
        let mut progress = self.progress.lock();
        if self.cancelled() || progress.stage == StabilizerStage::Cancelled {
            return false;
        }
        progress.stage = StabilizerStage::Planning;
        progress.completed_frames = 0;
        progress.subspace_frames = self.subspace_model.as_ref().map(|_| 0);
        true
    }

    pub(super) fn publish_plan_snapshot(&self, selection: PlanSelection, snapshot: PlanProgressSnapshot) {
        // Keep selection and progress ordered as selected_plan -> progress.  The guard must span
        // the progress write so a stale owner cannot pass the selection check and then overwrite
        // a newer request's snapshot.
        let _published = {
            let selected = self.selected_plan.lock();
            if selected.as_ref().is_some_and(|current| *current == selection) {
                let mut progress = self.progress.lock();
                if self.cancelled() {
                    progress.stage = StabilizerStage::Cancelled;
                    false
                } else if progress.stage == StabilizerStage::Cancelled {
                    false
                } else {
                    progress.stage = snapshot.stage;
                    progress.completed_frames = snapshot.completed_frames;
                    progress.subspace_frames = snapshot.subspace_frames;
                    progress.fallback_reason = snapshot.fallback_reason;
                    true
                }
            } else {
                false
            }
        };
        // Test hooks are deliberately outside both mutexes: a callback or wait must never hold
        // selected_plan or progress while another request tries to publish or cancel.
        #[cfg(test)]
        if _published
            && snapshot.stage == StabilizerStage::Planning
            && let Some(hook) = self.plan_hook.as_ref()
            && hook.armed.swap(false, Ordering::AcqRel)
        {
            hook.reached.wait();
            hook.release.wait();
        }
    }
}

#[derive(Clone, Copy, Hash, PartialEq, Eq)]
struct Key {
    clip: u64,
    method: u8,
    subspace: bool,
    detailed: bool,
    fast: bool,
    fps: u64,
    duration: u64,
    size: (u32, u32),
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub enum StabilizerStage {
    Analyzing,
    Factorizing,
    Planning,
    Complete,
    Cancelled,
    Failed,
}

/// Actual completed frames in the current stage, not an estimate of elapsed time.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StabilizerProgress {
    pub stage: StabilizerStage,
    pub completed_frames: usize,
    pub total_frames: usize,
    pub subspace_frames: Option<usize>,
    pub fallback_reason: Option<&'static str>,
}

struct Slot {
    result: OnceLock<Option<Arc<Analysis>>>,
    /// Only one caller performs analysis; waiters may abandon their frame request without
    /// affecting that owner.
    running: AtomicBool,
    /// Explicit cancellation for this exact clip/settings key.
    cancel: Arc<AtomicBool>,
    progress: Arc<Mutex<StabilizerProgress>>,
    #[cfg(test)]
    claim_hook: Option<Arc<ClaimHook>>,
    /// Insertion order for the completed-result eviction below.
    created: u64,
}

#[cfg(test)]
struct ClaimHook {
    reached: std::sync::Barrier,
    release: std::sync::Barrier,
}

#[cfg(test)]
struct PlanHook {
    reached: std::sync::Barrier,
    release: std::sync::Barrier,
    armed: AtomicBool,
}

impl Slot {
    fn new(total_frames: usize) -> Self {
        Self {
            result: OnceLock::new(),
            running: AtomicBool::new(false),
            cancel: Arc::new(AtomicBool::new(false)),
            progress: Arc::new(Mutex::new(StabilizerProgress {
                stage: StabilizerStage::Analyzing,
                completed_frames: 0,
                total_frames,
                subspace_frames: None,
                fallback_reason: None,
            })),
            #[cfg(test)]
            claim_hook: None,
            created: CACHE_SEQ.fetch_add(1, Ordering::Relaxed),
        }
    }

    fn cancelled(&self) -> bool {
        self.cancel.load(Ordering::Acquire)
    }

    /// Progress writes and explicit cancellation share the progress lock. Once cancellation has
    /// acquired it, no stale worker update can move the slot out of `Cancelled`.
    fn update_analysis_frame(&self, completed_frames: usize) {
        let mut progress = self.progress.lock();
        if self.cancelled() || progress.stage == StabilizerStage::Cancelled {
            return;
        }
        progress.completed_frames = completed_frames.min(progress.total_frames);
    }

    fn request_cancel(&self) -> bool {
        let mut progress = self.progress.lock();
        if !matches!(progress.stage, StabilizerStage::Analyzing | StabilizerStage::Factorizing | StabilizerStage::Planning) {
            return false;
        }
        self.cancel.store(true, Ordering::Release);
        progress.stage = StabilizerStage::Cancelled;
        true
    }

    fn finish_analysis(&self, result: Option<Arc<Analysis>>, stage: StabilizerStage) -> Option<Arc<Analysis>> {
        let mut progress = self.progress.lock();
        if self.cancelled() {
            progress.stage = StabilizerStage::Cancelled;
            return None;
        }
        progress.stage = stage;
        result
    }

    fn published_result(&self, result: &Option<Arc<Analysis>>) -> Option<Arc<Analysis>> {
        if self.cancelled() { None } else { result.clone() }
    }

    fn publish_analysis(&self, candidate: Option<Arc<Analysis>>, stage: StabilizerStage) -> Option<Arc<Analysis>> {
        let result = self.finish_analysis(candidate, stage);
        // The owner publishes the terminal OnceLock value before releasing the CAS bit. A
        // contender can therefore never acquire an idle slot and start a second analysis.
        let _ = self.result.set(result.clone());
        self.running.store(false, Ordering::Release);
        result
    }

    fn run_analysis_owner(&self, env: &dyn FxEnv, now: f64, method: TrackMethod, detailed: bool, fast: bool, subspace: bool) -> Option<Arc<Analysis>> {
        let count = self.progress.lock().total_frames;
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| analysis_work(env, now, method, detailed, fast, subspace, count, self)));
        let (candidate, stage) = match outcome {
            Ok(Ok(Some(analysis))) => (Some(analysis), StabilizerStage::Complete),
            Ok(Ok(None)) if self.cancelled() => (None, StabilizerStage::Cancelled),
            Ok(Ok(None)) => {
                log::error!("Warp Stabilizer analysis failed: no usable bounded analysis frames");
                (None, StabilizerStage::Failed)
            }
            Ok(Err(_)) | Err(_) => {
                log::error!("Warp Stabilizer analysis failed: a worker panicked");
                (None, StabilizerStage::Failed)
            }
        };
        self.publish_analysis(candidate, stage)
    }

    fn get_or_analyse(&self, env: &dyn FxEnv, now: f64, method: TrackMethod, detailed: bool, fast: bool, subspace: bool) -> Option<Arc<Analysis>> {
        if self.cancelled() {
            return None;
        }
        if let Some(result) = self.result.get() {
            return self.published_result(result);
        }
        #[cfg(test)]
        if self.running.load(Ordering::Acquire)
            && let Some(hook) = self.claim_hook.as_ref()
        {
            hook.reached.wait();
            hook.release.wait();
        }
        if self.running.compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire).is_ok() {
            // A previous owner may have published and released the flight after the initial
            // OnceLock check. Do not start a second clip analysis in that hand-off window.
            if let Some(result) = self.result.get() {
                self.running.store(false, Ordering::Release);
                return self.published_result(result);
            }
            if self.cancelled() {
                return self.publish_analysis(None, StabilizerStage::Cancelled);
            }
            // Keep the expensive work on a stack independent of the caller's Rayon worker. A
            // frame worker that is also a contender can then spin without starving the analysis
            // owner; only pure tracking/math batches below use Rayon.
            let result = std::thread::scope(|scope| {
                let spawned = std::thread::Builder::new()
                    .name("warp-analysis".to_owned())
                    .spawn_scoped(scope, || self.run_analysis_owner(env, now, method, detailed, fast, subspace));
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
                                log::error!("Warp Stabilizer analysis worker terminated unexpectedly");
                                self.publish_analysis(None, StabilizerStage::Failed)
                            }
                        }
                    }
                    Err(error) => {
                        log::error!("Warp Stabilizer analysis worker could not be spawned: {error}");
                        self.publish_analysis(None, StabilizerStage::Failed)
                    }
                }
            });
            return result;
        }
        // A second frame request must not block a Rayon worker on the first request's OnceLock.
        // It may abandon only its own render request; the single-flight owner continues.
        loop {
            if let Some(result) = self.result.get() {
                return self.published_result(result);
            }
            if filmcraft_media::cancel::cancelled() || self.cancelled() {
                return None;
            }
            match rayon::yield_now() {
                Some(rayon::Yield::Executed) => {}
                Some(rayon::Yield::Idle) | None => std::thread::park_timeout(std::time::Duration::from_millis(1)),
            }
        }
    }
}
/// Runs on the scoped owner thread. Rayon is used only by the pure frame-preparation and math
/// batches inside `analyse`; source-frame recursion therefore cannot queue an owner behind itself.
fn analysis_work(
    env: &dyn FxEnv,
    now: f64,
    method: TrackMethod,
    detailed: bool,
    fast: bool,
    subspace: bool,
    count: usize,
    slot: &Slot,
) -> Result<Option<Arc<Analysis>>, Box<dyn std::any::Any + Send>> {
    filmcraft_media::cancel::with_draft(false, || {
        filmcraft_media::cancel::with_catch_up(None, || {
            filmcraft_media::cancel::with_background(false, || {
                filmcraft_media::cancel::with_cancel(&slot.cancel, || {
                    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| analyse(env, now, method, detailed, fast, subspace, count, slot)))
                })
            })
        })
    })
}
static CACHE: LazyLock<Mutex<HashMap<Key, Arc<Slot>>>> = LazyLock::new(Default::default);

/// Insertion order for [`Slot::created`]; see the eviction in [`analysed_path`].
static CACHE_SEQ: AtomicU64 = AtomicU64::new(0);

pub(super) fn progress(project: &Project, sequence: &Sequence, item: &TrackItem, effect: &EffectInstance) -> Option<StabilizerProgress> {
    let key = analysis_key(project, sequence, item, effect)?;
    let slot = CACHE.lock().get(&key)?.clone();
    Some(*slot.progress.lock())
}

fn analysis_key(project: &Project, sequence: &Sequence, item: &TrackItem, effect: &EffectInstance) -> Option<Key> {
    if effect.effect != "warp_stabilizer" {
        return None;
    }
    let method_choice = super::chv(effect, "method");
    let fps = sequence.settings.frame_rate.as_f64();
    if !fps.is_finite() || fps <= 0.0 {
        return None;
    }
    let duration = item.duration.seconds();
    if !duration.is_finite() || duration <= 0.0 {
        return None;
    }
    let size = crate::source_size(project, item.item).unwrap_or((sequence.settings.width, sequence.settings.height));
    if size.0 == 0 || size.1 == 0 {
        return None;
    }
    let count = (duration * fps).ceil();
    if !count.is_finite() || count > 20_000.0 {
        return None;
    }
    Some(Key {
        clip: super::super::clip_analysis_key(item, sequence.settings.frame_rate),
        method: if method_choice == 0 { TrackMethod::Position as u8 } else { TrackMethod::PositionScaleRotation as u8 },
        subspace: method_choice == 3,
        detailed: super::bv(effect, "detailed"),
        fast: super::bv(effect, "fast_analysis"),
        fps: fps.to_bits(),
        duration: duration.to_bits(),
        size,
    })
}

pub(super) fn clear() {
    CACHE.lock().clear();
}

/// Request explicit cancellation of the matching analysis (including its factorization and
/// planning work). Returns `false` when nothing is in flight or the effect is not a Warp
/// Stabilizer instance. A cancelled job stays `Cancelled` until [`restart`].
pub fn cancel(project: &Project, sequence: &Sequence, item: &TrackItem, effect: &EffectInstance) -> bool {
    let Some(key) = analysis_key(project, sequence, item, effect) else { return false };
    // Keep cache selection and the stage transition atomic with respect to Analyze/restart.  A
    // restart racing this call must either replace an already-cancelled slot or be cancelled
    // itself, never leave the command aimed at an obsolete slot.
    let cache = CACHE.lock();
    let Some(slot) = cache.get(&key).cloned() else { return false };
    slot.request_cancel()
}

/// Queue a fresh analysis after cancellation or failure, or before its first frame request.
/// A frame worker claims the slot; this function never decodes or waits for an active job.
pub fn restart(project: &Project, sequence: &Sequence, item: &TrackItem, effect: &EffectInstance) -> bool {
    let Some(key) = analysis_key(project, sequence, item, effect) else { return false };
    let total = {
        let fps = sequence.settings.frame_rate.as_f64();
        let duration = item.duration.seconds();
        ((duration * fps).ceil().max(1.0)) as usize
    };
    let mut cache = CACHE.lock();
    if cache.get(&key).is_some_and(|slot| !matches!(slot.progress.lock().stage, StabilizerStage::Cancelled | StabilizerStage::Failed)) {
        return false;
    }
    cache.insert(key, Arc::new(Slot::new(total)));
    true
}

pub(super) fn valid_image(img: &Image) -> bool {
    img.w > 0 && img.h > 0 && img.w.checked_mul(img.h).and_then(|n| n.checked_mul(4)).is_some_and(|n| n == img.px.len() && n <= 268_435_456)
}

pub(super) fn analysed_path(env: &dyn FxEnv, now: f64, method: TrackMethod, detailed: bool, fast: bool, subspace: bool) -> Option<Arc<Analysis>> {
    let (fps, duration) = (env.frame_rate(), env.clip_seconds());
    let size = env.source_size();
    if !now.is_finite() || !fps.is_finite() || fps <= 0.0 || !duration.is_finite() || duration <= 0.0 || size.0 == 0 || size.1 == 0 {
        return None;
    }
    // Do not silently stabilize only a prefix of a long clip.
    let count = (duration * fps).ceil();
    if !count.is_finite() || count > 20_000.0 {
        return None;
    }
    let key = Key { clip: env.clip_key(), method: method as u8, subspace, detailed, fast, fps: fps.to_bits(), duration: duration.to_bits(), size };
    // A terminal failure or cancellation keeps its slot: progress stays visible and the next
    // frame does not silently restart the job. Only [`restart`] claims a fresh slot.
    let slot = {
        let mut cache = CACHE.lock();
        if cache.len() >= 16 && !cache.contains_key(&key) {
            // Dropping an in-flight slot would duplicate its analysis; dropping a terminal
            // cancelled/failed slot would let the next frame silently restart it. Only
            // completed results are evictable, oldest first. With no completed slot to evict
            // the cache grows past 16: non-complete slots hold no analysis payload and are
            // bounded by the number of distinct clip/settings keys.
            let mut completed: Vec<(u64, Key)> = cache
                .iter()
                .filter(|(_, slot)| matches!(slot.progress.lock().stage, StabilizerStage::Complete))
                .map(|(key, slot)| (slot.created, *key))
                .collect();
            completed.sort_unstable_by_key(|&(created, _)| created);
            for (_, evict) in completed {
                if cache.len() < 16 {
                    break;
                }
                cache.remove(&evict);
            }
        }
        cache.entry(key).or_insert_with(|| Arc::new(Slot::new(count.max(1.0) as usize))).clone()
    };
    slot.get_or_analyse(env, now, method, detailed, fast, subspace)
}

// At most eight frames and two megapixels per batch, plus the previous frame's pyramid.
// Decode stays ordered on the requesting thread: concurrent random seeks can thrash a decoder.
const BATCH_PIXELS: usize = 2_097_152;
const BATCH_FRAMES: usize = 8;

struct TrackingFrame {
    prepared: Prepared,
    corners: Vec<[f32; 2]>,
}

fn prepare(frame: Image, detect: bool) -> Option<TrackingFrame> {
    if filmcraft_media::cancel::cancelled() {
        return None;
    }
    let prepared = Prepared::new(gray_of(&frame));
    drop(frame);
    if filmcraft_media::cancel::cancelled() {
        return None;
    }
    let (w, h) = (prepared.base().w as f64, prepared.base().h as f64);
    let corners = if detect && w >= 24.0 && h >= 24.0 {
        let region = [Vec2::new(8.0, 8.0), Vec2::new(w - 8.0, 8.0), Vec2::new(w - 8.0, h - 8.0), Vec2::new(8.0, h - 8.0)];
        features(prepared.base(), &region, MAX_FEATURES, 5.0)
    } else {
        Vec::new()
    };
    Some(TrackingFrame { prepared, corners })
}
fn prepare_tracking_frame(
    context: &filmcraft_media::cancel::Context,
    frame: Option<Image>,
    frame_index: usize,
    count: usize,
) -> Result<Option<TrackingFrame>, Box<dyn std::any::Any + Send>> {
    context.clone().run(|| std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| frame.and_then(|f| prepare(f, frame_index + 1 < count)))))
}

fn analyse(env: &dyn FxEnv, now: f64, method: TrackMethod, detailed: bool, fast: bool, subspace: bool, count: usize, slot: &Slot) -> Option<Arc<Analysis>> {
    let progress = &slot.progress;
    let fps = env.frame_rate();
    let size = env.source_size();
    let scale = ((if detailed { 960.0 } else { 480.0 }) / size.0.max(size.1) as f32).min(1.0);
    let context = filmcraft_media::cancel::Context::current();
    let workers = if fast { rayon::current_num_threads().clamp(1, BATCH_FRAMES) } else { 1 };
    let mut previous: Option<TrackingFrame> = None;
    let mut dimensions: Option<(usize, usize)> = None;
    let mut collector: Option<TrajectoryCollector> = None;
    let mut steps = Vec::with_capacity(count.saturating_sub(1));
    let mut valid_steps = Vec::with_capacity(count.saturating_sub(1));
    let mut k = 0;
    while k < count {
        if slot.cancelled() {
            return None;
        }
        let pixels = dimensions.map_or(BATCH_PIXELS, |(w, h)| w.saturating_mul(h));
        let batch_len = workers.min(BATCH_PIXELS.checked_div(pixels).unwrap_or(1).max(1)).min(count - k);
        let mut frames = Vec::with_capacity(batch_len);
        for index in k..k + batch_len {
            if slot.cancelled() {
                return None;
            }
            let frame = env
                .source_frame(index as f64 / fps - now, scale)
                .filter(valid_image)
                .filter(|f| f.w.saturating_mul(f.h) <= BATCH_PIXELS && dimensions.is_none_or(|size| size == (f.w, f.h)));
            if let Some(f) = &frame {
                dimensions = Some((f.w, f.h));
            }
            frames.push(frame);
        }
        // Catch panics inside the cancellation context: unwinding through Context::run would
        // leave a Rayon worker's thread-local cancellation flag installed for its next job.
        let prepared: Vec<_> = frames.into_par_iter().enumerate().map(|(offset, frame)| prepare_tracking_frame(&context, frame, k + offset, count)).collect();
        for (offset, current) in prepared.into_iter().enumerate() {
            let Ok(current) = current else {
                log::error!("Warp Stabilizer analysis failed: a preparation worker panicked");
                return None;
            };
            let frame_index = k + offset;
            if slot.cancelled() {
                return None;
            }
            if subspace
                && collector.is_none()
                && let Some((w, h)) = dimensions
            {
                collector = TrajectoryCollector::new(w, h, count);
            }
            let fit = if subspace {
                collector.as_mut().and_then(|c| {
                    let corners = current.as_ref().map_or(&[][..], |f| f.corners.as_slice());
                    c.step(frame_index, previous.as_ref().map(|f| &f.prepared), current.as_ref().map(|f| &f.prepared), corners, method)
                })
            } else if frame_index > 0 {
                previous.as_ref().zip(current.as_ref()).and_then(|(a, b)| track_step_with_features(&a.prepared, &b.prepared, &a.corners, method))
            } else {
                None
            };
            if frame_index > 0 {
                let usable = fit.filter(|s| {
                    s.inliers >= 6
                        && s.inliers.saturating_mul(2) >= s.tracked
                        && [s.transform.a, s.transform.b, s.transform.c, s.transform.d, s.transform.e, s.transform.f].iter().all(|v| v.is_finite())
                });
                valid_steps.push(usable.is_some());
                steps.push(usable.map_or(Affine::IDENTITY, |s| s.transform));
            }
            previous = current;
            slot.update_analysis_frame(frame_index.saturating_add(1));
        }
        k += batch_len;
    }
    if slot.cancelled() {
        return None;
    }
    let (w, h) = dimensions?;
    let path = Arc::new(StabilizerPath { w, h, steps, fps });
    let trajectories = if subspace { collector.and_then(|c| c.finish().map(Arc::new)) } else { None };
    let mut subspace_model = None;
    if subspace {
        let Some(data) = trajectories.as_ref() else {
            {
                let mut state = progress.lock();
                if !slot.cancelled() && state.stage != StabilizerStage::Cancelled {
                    state.fallback_reason = Some("invalidInput");
                }
            }
            log::warn!("Warp Stabilizer Subspace Warp fallback: invalidInput");
            return Some(Arc::new(Analysis {
                path,
                valid_steps,
                trajectories,
                subspace_model,
                progress: progress.clone(),
                cancel: slot.cancel.clone(),
                selected_plan: Mutex::new(None),
                next_plan_generation: AtomicU64::new(0),
                #[cfg(test)]
                plan_hook: None,
            }));
        };
        {
            let mut state = progress.lock();
            if slot.cancelled() || state.stage == StabilizerStage::Cancelled {
                return None;
            }
            state.stage = StabilizerStage::Factorizing;
            state.completed_frames = 0;
            state.subspace_frames = None;
        }
        let mut report = |frame: usize| {
            let mut state = progress.lock();
            if slot.cancelled() || state.stage == StabilizerStage::Cancelled {
                return;
            }
            state.completed_frames = frame.min(data.frames);
        };
        match subspace::factorize(data, &mut report) {
            Ok(model) => subspace_model = Some(Arc::new(model)),
            Err(SubspaceError::Cancelled) => return None,
            Err(error) => {
                let reason = error.reason();
                let mut state = progress.lock();
                if !slot.cancelled() && state.stage != StabilizerStage::Cancelled {
                    state.fallback_reason = Some(reason);
                }
                log::warn!("Warp Stabilizer Subspace Warp fallback: {reason}");
            }
        }
    }
    Some(Arc::new(Analysis {
        path,
        valid_steps,
        trajectories,
        subspace_model,
        progress: progress.clone(),
        cancel: slot.cancel.clone(),
        selected_plan: Mutex::new(None),
        next_plan_generation: AtomicU64::new(0),
        #[cfg(test)]
        plan_hook: None,
    }))
}

#[cfg(test)]
#[path = "analysis_tests.rs"]
mod tests;
