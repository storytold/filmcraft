use std::sync::{
    Arc, Barrier, Condvar, LazyLock, Mutex as StdMutex,
    atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
};

use super::super::mesh::MeshFrame;
use super::super::trajectory_data::GRID_CELLS;
use super::super::{
    PLAN_CACHE_BUDGET, PLAN_CACHE_MAX_ENTRIES, PlanEntryHook, PlanKey, PlanProgressSnapshot, PlanSelection, PlanSettings, StabilizerPath, build_plan,
    cache_entry, cached_plan, clear_stabilizer_cache, plan_cache, plan_retained_bytes, retain_plan,
};
use super::*;
use filmcraft_project::EffectInstance;
use parking_lot::Mutex as ParkingMutex;
use std::time::Duration;

struct FrameGate {
    entered: Barrier,
    release: Barrier,
}

struct AbortGate {
    entered: Barrier,
    open: StdMutex<bool>,
    wake: Condvar,
}

impl AbortGate {
    fn wait(&self) {
        let mut open = self.open.lock().expect("abort gate lock");
        while !*open {
            open = self.wake.wait(open).expect("abort gate wait");
        }
    }

    fn release(&self) {
        *self.open.lock().expect("abort gate lock") = true;
        self.wake.notify_all();
    }
}

struct YieldProbe {
    reached: AtomicBool,
}

struct Clip {
    key: u64,
    faulty: bool,
    missing: bool,
    flag: Arc<AtomicBool>,
    cancel_at: AtomicUsize,
    reads: Mutex<Vec<usize>>,
    first_frame_gate: Option<Arc<FrameGate>>,
    abort_gate: Option<Arc<AbortGate>>,
    yield_probe: Option<Arc<YieldProbe>>,
}
impl Clip {
    fn new(key: u64, faulty: bool) -> Self {
        Self {
            key,
            faulty,
            missing: false,
            flag: Arc::new(AtomicBool::new(false)),
            cancel_at: AtomicUsize::new(usize::MAX),
            reads: Mutex::new(Vec::new()),
            first_frame_gate: None,
            abort_gate: None,
            yield_probe: None,
        }
    }

    fn with_first_frame_gate(mut self, gate: Arc<FrameGate>) -> Self {
        self.first_frame_gate = Some(gate);
        self
    }

    fn with_abort_gate(mut self, gate: Arc<AbortGate>) -> Self {
        self.abort_gate = Some(gate);
        self
    }

    fn with_yield_probe(mut self, probe: Arc<YieldProbe>) -> Self {
        self.yield_probe = Some(probe);
        self
    }
}
fn offset(k: usize) -> Vec2 {
    Vec2::new((k as f64 * 1.7).sin() * 0.7, (k as f64 * 2.1).cos() * 0.5)
}
impl FxEnv for Clip {
    fn frame(&self, _: f64, _: &EffectInstance) -> Option<Image> {
        None
    }
    fn source_frame(&self, dt: f64, _: f32) -> Option<Image> {
        let k = (dt * 24.0).round() as usize;
        self.reads.lock().push(k);
        if k == 0 {
            if let Some(gate) = self.first_frame_gate.as_ref() {
                gate.entered.wait();
                gate.release.wait();
            }
            if let Some(gate) = self.abort_gate.as_ref() {
                gate.entered.wait();
                gate.wait();
            }
            if let Some(probe) = self.yield_probe.as_ref() {
                probe.reached.store(true, Ordering::Release);
                let _ = rayon::yield_now();
            }
        }
        if self.missing {
            return None;
        }
        if k == self.cancel_at.load(Ordering::Relaxed) {
            self.flag.store(true, Ordering::Relaxed);
        }
        if self.faulty && k == 9 {
            return Some(Image { w: usize::MAX, h: 2, px: vec![0.0; 4] });
        }
        if self.faulty && k == 25 {
            return None;
        }
        let w = if self.faulty && k == 17 { 127 } else { 128 };
        let mut img = Image::new(w, 96);
        let d = offset(k);
        for (i, p) in img.px.chunks_exact_mut(4).enumerate() {
            let (x, y) = ((i % w) as f64 + d.x, (i / w) as f64 + d.y);
            let v = if self.faulty && k == 30 { 0.5 } else { (0.5 + 0.15 * (x * 0.21).sin() * (y * 0.17).cos() + 0.12 * ((x + y) * 0.43).sin()) as f32 };
            let v = filmcraft_color::srgb_to_linear(v);
            p.copy_from_slice(&[v, v, v, 1.0]);
        }
        Some(img)
    }
    fn track(&self, _: usize) -> Option<Image> {
        None
    }
    fn layer_to_output(&self) -> Affine {
        Affine::IDENTITY
    }
    fn clip_seconds(&self) -> f64 {
        33.0 / 24.0
    }
    fn clip_offset(&self) -> f64 {
        0.0
    }
    fn frame_rate(&self) -> f64 {
        24.0
    }
    fn media_timecode(&self) -> String {
        String::new()
    }
    fn file_name(&self) -> String {
        String::new()
    }
    fn sequence_name(&self) -> String {
        String::new()
    }
    fn sequence_size(&self) -> (u32, u32) {
        (128, 96)
    }
    fn source_size(&self) -> (u32, u32) {
        (128, 96)
    }
    fn clip_key(&self) -> u64 {
        self.key
    }
}

static PLAN_TEST_LOCK: LazyLock<ParkingMutex<()>> = LazyLock::new(|| ParkingMutex::new(()));

fn plan_test_guard() -> parking_lot::MutexGuard<'static, ()> {
    PLAN_TEST_LOCK.lock()
}

fn plan_analysis(step_count: usize) -> Arc<Analysis> {
    let path = Arc::new(StabilizerPath { w: 128, h: 96, steps: vec![Affine::IDENTITY; step_count], fps: 24.0 });
    Arc::new(Analysis {
        path,
        valid_steps: vec![true; step_count],
        trajectories: None,
        subspace_model: None,
        progress: Arc::new(ParkingMutex::new(StabilizerProgress {
            stage: StabilizerStage::Planning,
            completed_frames: 0,
            total_frames: step_count.saturating_add(1),
            subspace_frames: None,
            fallback_reason: None,
        })),
        cancel: Arc::new(AtomicBool::new(false)),
        selected_plan: ParkingMutex::new(None),
        next_plan_generation: AtomicU64::new(0),
        plan_hook: None,
    })
}

fn plan_settings(smooth_frames: f64) -> PlanSettings {
    PlanSettings { smooth_frames, no_motion: false, preserve_scale: true, crop_less: 0.0, action_safe: 0.0, max_scale: 2.0, adaptive: false }
}

#[test]
fn batching_preserves_motion_and_never_bridges_missing_or_malformed_frames() {
    let pool = rayon::ThreadPoolBuilder::new().num_threads(4).build().unwrap();
    for faulty in [false, true] {
        let env = Clip::new(0xFA57_0000 + faulty as u64, faulty);
        let serial = analysed_path(&env, 0.0, TrackMethod::PositionScaleRotation, false, false, false).unwrap();
        let parallel = pool.install(|| analysed_path(&env, 0.0, TrackMethod::PositionScaleRotation, false, true, false)).unwrap();
        assert_eq!(serial.path.steps, parallel.path.steps);
        assert_eq!(serial.valid_steps, parallel.valid_steps);
        // The tail is not a complete batch, and the decoder still sees contiguous ordered requests.
        assert_eq!(*env.reads.lock(), (0..33).chain(0..33).collect::<Vec<_>>());
        assert_eq!(parallel.path.steps.len(), 32);
        for (k, step) in parallel.path.steps.iter().enumerate() {
            let rejected = faulty && [8, 9, 16, 17, 24, 25, 29, 30].contains(&k);
            assert_eq!(parallel.valid_steps[k], !rejected, "step {k}");
            if rejected {
                assert_eq!(*step, Affine::IDENTITY);
            } else {
                let p = Vec2::new(64.0, 48.0);
                assert!((step.apply(p) - p - (offset(k) - offset(k + 1))).length() < 0.03, "step {k}: {step:?}");
            }
        }
    }
}

#[test]
fn a_late_single_flight_claim_reuses_the_published_analysis() {
    let frame_gate = Arc::new(FrameGate { entered: Barrier::new(2), release: Barrier::new(2) });
    let env = Arc::new(Clip::new(0xFA57_0008, false).with_first_frame_gate(frame_gate.clone()));
    let claim_hook = Arc::new(ClaimHook { reached: Barrier::new(2), release: Barrier::new(2) });
    let mut slot = Slot::new(33);
    slot.claim_hook = Some(claim_hook.clone());
    let slot = Arc::new(slot);
    let owner_done = Arc::new(Barrier::new(2));

    let owner_slot = slot.clone();
    let owner_env = env.clone();
    let owner_done_thread = owner_done.clone();
    let owner = std::thread::spawn(move || {
        let result = owner_slot.get_or_analyse(owner_env.as_ref(), 0.0, TrackMethod::Position, false, false, false);
        owner_done_thread.wait();
        result
    });

    frame_gate.entered.wait();
    let contender_slot = slot.clone();
    let contender_env = env.clone();
    let contender = std::thread::spawn(move || contender_slot.get_or_analyse(contender_env.as_ref(), 0.0, TrackMethod::Position, false, false, false));

    claim_hook.reached.wait();

    frame_gate.release.wait();
    owner_done.wait();
    claim_hook.release.wait();

    let first = owner.join().expect("analysis owner");
    let second = contender.join().expect("analysis contender");
    let first = first.expect("owner publishes analysis");
    let second = second.expect("contender observes analysis");
    assert!(Arc::ptr_eq(&first, &second));
    assert_eq!(*env.reads.lock(), (0..33).collect::<Vec<_>>(), "late claim started a duplicate analysis");
}
#[test]
fn rayon_waiters_cannot_starve_the_isolated_analysis_work_stack() {
    // Keep one outer-pool worker occupied, enqueue the contender synchronously, then let the
    // owner yield once.  `ThreadPool::spawn` guarantees the contender is submitted before the
    // parent gate opens; a scoped `spawn(pool.install(...))` only schedules a thread and can race
    // that release.
    let pool = rayon::ThreadPoolBuilder::new().num_threads(2).build().expect("custom Rayon pool");
    let parent_gate = Arc::new(AbortGate { entered: Barrier::new(2), open: StdMutex::new(false), wake: Condvar::new() });
    let outer_blocker = Arc::new(AbortGate { entered: Barrier::new(2), open: StdMutex::new(false), wake: Condvar::new() });
    let yield_probe = Arc::new(YieldProbe { reached: AtomicBool::new(false) });
    let slot = Arc::new(Slot::new(33));
    let mut clip = Clip::new(0xFA57_000B, false);
    clip.flag = slot.cancel.clone();
    let env = Arc::new(clip.with_abort_gate(parent_gate.clone()).with_yield_probe(yield_probe.clone()));
    let (done_tx, done_rx) = std::sync::mpsc::channel();
    let runner_slot = slot.clone();
    let runner_env = env.clone();
    let runner_parent_gate = parent_gate.clone();
    let runner_outer_blocker = outer_blocker.clone();
    let runner = std::thread::spawn(move || {
        let result = std::thread::scope(|scope| {
            let blocker_gate = runner_outer_blocker.clone();
            let pool_ref = &pool;
            let blocker = scope.spawn(move || {
                pool_ref.install(|| {
                    blocker_gate.entered.wait();
                    blocker_gate.wait();
                });
            });
            runner_outer_blocker.entered.wait();

            let owner_slot = runner_slot.clone();
            let owner_env = runner_env.clone();
            let pool_ref = &pool;
            let owner = scope.spawn(move || pool_ref.install(|| owner_slot.get_or_analyse(owner_env.as_ref(), 0.0, TrackMethod::Position, false, true, false)));
            runner_parent_gate.entered.wait();

            let contender_slot = runner_slot.clone();
            let contender_env = runner_env.clone();
            let (contender_tx, contender_rx) = std::sync::mpsc::channel();
            pool.spawn(move || {
                let result = contender_slot.get_or_analyse(contender_env.as_ref(), 0.0, TrackMethod::Position, false, true, false);
                let _ = contender_tx.send(result);
            });

            runner_parent_gate.release();
            let owner_result = owner.join().expect("analysis owner");
            runner_outer_blocker.release();
            let contender_result = contender_rx.recv_timeout(Duration::from_secs(5)).expect("analysis contender");
            blocker.join().expect("outer blocker");
            (owner_result, contender_result)
        });
        let _ = done_tx.send(result);
    });

    let results = match done_rx.recv_timeout(Duration::from_secs(5)) {
        Ok(results) => results,
        Err(error) => {
            // A regression must not strand a test process. Cancellation wakes a waiter stolen by
            // the old owner stack, while both abortable gates release any blocked source/worker.
            slot.cancel.store(true, Ordering::Release);
            parent_gate.release();
            outer_blocker.release();
            let _ = runner.join();
            panic!("Rayon single-flight regression did not complete: {error}");
        }
    };
    runner.join().expect("Rayon regression runner");
    let (owner, contender) = results;
    let owner = owner.expect("analysis owner result");
    let contender = contender.expect("analysis contender result");
    assert!(Arc::ptr_eq(&owner, &contender), "Rayon waiter triggered duplicate analysis");
    assert!(yield_probe.reached.load(Ordering::Acquire));
    assert_eq!(*env.reads.lock(), (0..33).collect::<Vec<_>>());
}

#[test]
fn owner_cancel_leaves_a_terminal_slot_until_a_fresh_slot_retries() {
    let frame_gate = Arc::new(FrameGate { entered: Barrier::new(2), release: Barrier::new(2) });
    let env = Arc::new(Clip::new(0xFA57_0009, false).with_first_frame_gate(frame_gate.clone()));
    let slot = Arc::new(Slot::new(33));
    let owner_slot = slot.clone();
    let owner_env = env.clone();
    let owner = std::thread::spawn(move || owner_slot.get_or_analyse(owner_env.as_ref(), 0.0, TrackMethod::Position, false, false, false));

    frame_gate.entered.wait();
    assert!(slot.request_cancel());
    frame_gate.release.wait();
    assert!(owner.join().expect("cancelled analysis owner").is_none());
    let state = *slot.progress.lock();
    assert_eq!(state.stage, StabilizerStage::Cancelled);
    assert!(state.completed_frames < state.total_frames);
    assert_eq!(*env.reads.lock(), vec![0]);
    assert!(!slot.request_cancel(), "a terminal cancellation must not be re-cancelled");

    env.reads.lock().clear();
    assert!(slot.get_or_analyse(env.as_ref(), 0.0, TrackMethod::Position, false, false, false).is_none());
    assert!(env.reads.lock().is_empty(), "a cancelled slot restarted without Analyze");

    let retry_env = Clip::new(0xFA57_000A, false);
    let retry = Slot::new(33);
    assert!(retry.get_or_analyse(&retry_env, 0.0, TrackMethod::Position, false, false, false).is_some());
}

#[test]
fn scrubbing_frame_cancellation_does_not_restart_clip_analysis() {
    let pool = rayon::ThreadPoolBuilder::new().num_threads(4).build().unwrap();
    for fast in [false, true] {
        let env = Clip::new(0xFA57_0010 + fast as u64, false);
        let slot = Slot::new(33);
        env.cancel_at.store(3, Ordering::Relaxed);
        let first = pool
            .install(|| filmcraft_media::cancel::with_cancel(&env.flag, || slot.get_or_analyse(&env, 0.0, TrackMethod::Position, false, fast, false)))
            .expect("clip analysis survives its frame request");
        assert!(env.flag.load(Ordering::Relaxed));
        assert_eq!(first.valid_steps, vec![true; 32]);
        assert_eq!(*env.reads.lock(), (0..33).collect::<Vec<_>>());
        env.reads.lock().clear();
        let cached = slot.get_or_analyse(&env, 0.0, TrackMethod::Position, false, fast, false).expect("completed clip analysis");
        assert!(Arc::ptr_eq(&first, &cached));
        assert!(env.reads.lock().is_empty(), "scrubbing decoded the clip again");
    }
}

#[test]
fn explicit_cancellation_is_sticky_until_a_fresh_analysis_slot() {
    for fast in [false, true] {
        let mut env = Clip::new(0xFA57_0018 + fast as u64, false);
        let slot = Slot::new(33);
        env.flag = slot.cancel.clone();
        env.cancel_at.store(3, Ordering::Relaxed);
        assert!(slot.get_or_analyse(&env, 0.0, TrackMethod::Position, false, fast, false).is_none());
        assert_eq!(slot.progress.lock().stage, StabilizerStage::Cancelled);
        env.reads.lock().clear();
        assert!(slot.get_or_analyse(&env, 0.0, TrackMethod::Position, false, fast, false).is_none());
        assert!(env.reads.lock().is_empty(), "cancelled job restarted without Analyze");
        let fresh = Slot::new(33);
        env.flag = fresh.cancel.clone();
        env.cancel_at.store(usize::MAX, Ordering::Relaxed);
        let completed = fresh.get_or_analyse(&env, 0.0, TrackMethod::Position, false, fast, false).expect("Analyze retries");
        assert_eq!(completed.valid_steps, vec![true; 32]);
        assert_eq!(*env.reads.lock(), (0..33).collect::<Vec<_>>());
    }
}

#[test]
fn progress_reports_actual_completion_and_cancellation_independently_of_cache_eviction() {
    let env = Clip::new(0xFA57_0020, false);
    let slot = Slot::new(33);
    assert_eq!(
        *slot.progress.lock(),
        StabilizerProgress { stage: StabilizerStage::Analyzing, completed_frames: 0, total_frames: 33, subspace_frames: None, fallback_reason: None }
    );
    assert!(slot.get_or_analyse(&env, 0.0, TrackMethod::Position, false, true, false).is_some());
    assert_eq!(
        *slot.progress.lock(),
        StabilizerProgress { stage: StabilizerStage::Complete, completed_frames: 33, total_frames: 33, subspace_frames: None, fallback_reason: None }
    );

    let mut cancelled_env = Clip::new(0xFA57_0021, false);
    let cancelled = Slot::new(33);
    cancelled_env.flag = cancelled.cancel.clone();
    cancelled_env.cancel_at.store(3, Ordering::Relaxed);
    assert!(cancelled.get_or_analyse(&cancelled_env, 0.0, TrackMethod::Position, false, true, false).is_none());
    let state = *cancelled.progress.lock();
    assert_eq!(state.stage, StabilizerStage::Cancelled);
    assert!(state.completed_frames > 0 && state.completed_frames < state.total_frames);
}

#[test]
fn missing_source_frames_finish_as_failed_instead_of_remaining_in_progress() {
    let mut env = Clip::new(0xFA57_0030, false);
    env.missing = true;
    let slot = Slot::new(33);
    assert!(slot.get_or_analyse(&env, 0.0, TrackMethod::Position, false, true, false).is_none());
    assert_eq!(
        *slot.progress.lock(),
        StabilizerProgress { stage: StabilizerStage::Failed, completed_frames: 33, total_frames: 33, subspace_frames: None, fallback_reason: None }
    );
}

#[test]
fn plan_progress_generation_rejects_stale_concurrent_updates() {
    let _guard = plan_test_guard();
    let analysis = plan_analysis(2);
    let selection_a =
        PlanSelection { key: PlanKey { path: 1, smooth: 1, crop: 0, safe: 0, maximum: 0, flags: 0 }, generation: analysis.next_plan_generation() };
    let selection_b =
        PlanSelection { key: PlanKey { path: 2, smooth: 2, crop: 0, safe: 0, maximum: 0, flags: 0 }, generation: analysis.next_plan_generation() };
    analysis.select_plan(selection_a);
    analysis.publish_plan_snapshot(
        selection_a,
        PlanProgressSnapshot { stage: StabilizerStage::Planning, completed_frames: 1, subspace_frames: None, fallback_reason: None },
    );
    let ready = Arc::new(Barrier::new(2));
    let stale_analysis = analysis.clone();
    let stale_ready = ready.clone();
    let stale = std::thread::spawn(move || {
        stale_ready.wait();
        stale_analysis.publish_plan_snapshot(
            selection_a,
            PlanProgressSnapshot { stage: StabilizerStage::Cancelled, completed_frames: 0, subspace_frames: None, fallback_reason: Some("stale") },
        );
    });
    analysis.select_plan(selection_b);
    ready.wait();
    stale.join().expect("stale publisher");
    analysis.publish_plan_snapshot(
        selection_b,
        PlanProgressSnapshot { stage: StabilizerStage::Complete, completed_frames: 3, subspace_frames: Some(2), fallback_reason: None },
    );
    assert_eq!(
        *analysis.progress.lock(),
        StabilizerProgress { stage: StabilizerStage::Complete, completed_frames: 3, total_frames: 3, subspace_frames: Some(2), fallback_reason: None }
    );
}
#[test]
fn late_plan_claim_rechecks_publication_before_starting_duplicate() {
    let _guard = plan_test_guard();
    clear_stabilizer_cache();
    let analysis = plan_analysis(4);
    let settings = plan_settings(4.0);
    let key = PlanKey {
        path: Arc::as_ptr(&analysis.path) as usize,
        smooth: settings.smooth_frames.to_bits(),
        crop: 0,
        safe: 0,
        maximum: settings.max_scale.to_bits(),
        flags: 2,
    };
    let entry = cache_entry(&analysis, key);
    let hook = Arc::new(PlanEntryHook { reached: Barrier::new(2), release: Barrier::new(2), armed: AtomicBool::new(true) });
    *entry.claim_hook.lock() = Some(hook.clone());

    let owner_analysis = analysis.clone();
    let owner_settings = settings.clone();
    let owner = std::thread::spawn(move || cached_plan(&owner_analysis, &owner_settings));
    hook.reached.wait();

    // The contender observed an empty entry but pauses immediately before its CAS. Publish the
    // completed plan in that hand-off window, just as a previous owner can do before its release.
    let plan = Arc::new(build_plan(&analysis.path, &settings));
    retain_plan(key, &entry, plan.clone());
    *entry.snapshot.lock() = PlanProgressSnapshot { stage: StabilizerStage::Complete, completed_frames: 5, subspace_frames: None, fallback_reason: None };
    hook.release.wait();

    let reused = owner.join().expect("late plan owner").expect("published plan");
    assert!(Arc::ptr_eq(&reused, &plan), "late CAS rebuilt instead of reusing the publication");
    assert_eq!(entry.build_count.load(Ordering::Acquire), 0, "late CAS started a duplicate plan owner");
    assert_eq!(analysis.progress.lock().stage, StabilizerStage::Complete);
}

#[test]
fn cached_plan_publication_drops_plan_guard_before_progress_callback() {
    let _guard = plan_test_guard();
    clear_stabilizer_cache();
    let mut analysis = plan_analysis(2);
    let hook = Arc::new(PlanHook { reached: Barrier::new(2), release: Barrier::new(2), armed: AtomicBool::new(true) });
    Arc::get_mut(&mut analysis).expect("unique analysis before cache hit").plan_hook = Some(hook.clone());
    let settings = plan_settings(4.0);
    let key = PlanKey {
        path: Arc::as_ptr(&analysis.path) as usize,
        smooth: settings.smooth_frames.to_bits(),
        crop: 0,
        safe: 0,
        maximum: settings.max_scale.to_bits(),
        flags: 2,
    };
    let entry = cache_entry(&analysis, key);
    let plan = Arc::new(build_plan(&analysis.path, &settings));
    retain_plan(key, &entry, plan.clone());
    *entry.snapshot.lock() = PlanProgressSnapshot { stage: StabilizerStage::Planning, completed_frames: 1, subspace_frames: None, fallback_reason: None };

    let worker_analysis = analysis.clone();
    let worker_settings = settings.clone();
    let worker = std::thread::spawn(move || cached_plan(&worker_analysis, &worker_settings));
    hook.reached.wait();
    assert!(entry.plan.try_lock().is_some(), "cache-hit callback retained the entry plan guard");
    hook.release.wait();

    let reused = worker.join().expect("cached plan publisher").expect("cached plan");
    assert!(Arc::ptr_eq(&reused, &plan));
}

#[test]
fn joining_plan_request_does_not_reset_in_progress_counts() {
    let _guard = plan_test_guard();
    clear_stabilizer_cache();
    let mut analysis = plan_analysis(4);
    let owner_hook = Arc::new(PlanHook { reached: Barrier::new(2), release: Barrier::new(2), armed: AtomicBool::new(true) });
    Arc::get_mut(&mut analysis).expect("unique analysis before planning").plan_hook = Some(owner_hook.clone());
    let settings = plan_settings(4.0);
    let key = PlanKey {
        path: Arc::as_ptr(&analysis.path) as usize,
        smooth: settings.smooth_frames.to_bits(),
        crop: 0,
        safe: 0,
        maximum: settings.max_scale.to_bits(),
        flags: 2,
    };

    let owner_analysis = analysis.clone();
    let owner_settings = settings.clone();
    let owner = std::thread::spawn(move || cached_plan(&owner_analysis, &owner_settings));
    owner_hook.reached.wait();

    {
        let mut progress = analysis.progress.lock();
        progress.completed_frames = 2;
    }
    let entry = cache_entry(&analysis, key);
    let waiter_hook = Arc::new(PlanEntryHook { reached: Barrier::new(2), release: Barrier::new(2), armed: AtomicBool::new(true) });
    *entry.waiter_hook.lock() = Some(waiter_hook.clone());
    let waiter_analysis = analysis.clone();
    let waiter_settings = settings.clone();
    let waiter = std::thread::spawn(move || cached_plan(&waiter_analysis, &waiter_settings));

    waiter_hook.reached.wait();
    assert_eq!(analysis.progress.lock().completed_frames, 2, "joining request reset active planning progress");
    waiter_hook.release.wait();
    owner_hook.release.wait();

    let first = owner.join().expect("planning owner").expect("owner plan");
    let second = waiter.join().expect("planning joiner").expect("joiner plan");
    assert!(Arc::ptr_eq(&first, &second));
    assert_eq!(analysis.progress.lock().stage, StabilizerStage::Complete);
}

#[test]
fn stale_plan_owner_cannot_rewind_a_newer_selected_progress_snapshot() {
    let _guard = plan_test_guard();
    let analysis = plan_analysis(2);
    let selection_a =
        PlanSelection { key: PlanKey { path: 1, smooth: 1, crop: 0, safe: 0, maximum: 0, flags: 0 }, generation: analysis.next_plan_generation() };
    let selection_b =
        PlanSelection { key: PlanKey { path: 2, smooth: 2, crop: 0, safe: 0, maximum: 0, flags: 0 }, generation: analysis.next_plan_generation() };
    analysis.select_plan(selection_a);
    analysis.select_plan(selection_b);
    analysis.publish_plan_snapshot(
        selection_b,
        PlanProgressSnapshot { stage: StabilizerStage::Complete, completed_frames: 3, subspace_frames: None, fallback_reason: None },
    );
    assert!(!analysis.begin_plan(selection_a), "stale owner initialized a superseded selection");
    assert_eq!(
        *analysis.progress.lock(),
        StabilizerProgress { stage: StabilizerStage::Complete, completed_frames: 3, total_frames: 3, subspace_frames: None, fallback_reason: None }
    );
}

#[test]
fn explicit_cancellation_during_settings_planning_publishes_no_partial_plan() {
    let _guard = plan_test_guard();
    let slot = Slot::new(5);
    let mut analysis = plan_analysis(4);
    let hook = Arc::new(PlanHook { reached: Barrier::new(2), release: Barrier::new(2), armed: AtomicBool::new(true) });
    let value = Arc::get_mut(&mut analysis).expect("unique analysis before planning");
    value.cancel = slot.cancel.clone();
    value.progress = slot.progress.clone();
    value.plan_hook = Some(hook.clone());
    let settings = plan_settings(4.0);
    let worker_analysis = analysis.clone();
    let worker_settings = settings.clone();
    let worker = std::thread::spawn(move || cached_plan(&worker_analysis, &worker_settings));

    hook.reached.wait();
    assert!(slot.request_cancel());
    hook.release.wait();

    assert!(worker.join().expect("planning owner").is_none());
    assert_eq!(slot.progress.lock().stage, StabilizerStage::Cancelled);
    assert!(plan_cache().lock().entries.values().all(|entry| !Arc::ptr_eq(&entry._analysis, &analysis)), "cancelled planning left a partial cache entry");

    let retry = plan_analysis(4);
    let retried = cached_plan(&retry, &settings).expect("fresh Analyze retries settings planning");
    assert_eq!(retried.corrections.len(), 5);
    assert_eq!(retry.progress.lock().stage, StabilizerStage::Complete);
}

#[test]
fn frame_cancellation_does_not_poison_planning_and_cached_a_hit_republishes() {
    let _guard = plan_test_guard();
    clear_stabilizer_cache();
    let analysis = plan_analysis(4);
    let settings_a = plan_settings(4.0);
    let settings_b = plan_settings(8.0);
    let not_cancelled = Arc::new(AtomicBool::new(false));
    let first_a = filmcraft_media::cancel::with_cancel(&not_cancelled, || cached_plan(&analysis, &settings_a)).expect("plan A");
    assert_eq!(first_a.corrections.len(), 5);

    let cancelled = Arc::new(AtomicBool::new(true));
    let plan_b = filmcraft_media::cancel::with_cancel(&cancelled, || cached_plan(&analysis, &settings_b)).expect("clip-wide planning survives a scrub");
    assert_eq!(plan_b.corrections.len(), 5);

    let retry_a = filmcraft_media::cancel::with_cancel(&not_cancelled, || cached_plan(&analysis, &settings_a)).expect("cached A");
    assert!(Arc::ptr_eq(&first_a, &retry_a));
    let state = *analysis.progress.lock();
    assert_eq!(state.stage, StabilizerStage::Complete);
    assert_eq!(state.completed_frames, state.total_frames);
}

#[test]
fn retained_budget_accounts_actual_plan_storage_and_evicts_variants() {
    let _guard = plan_test_guard();
    clear_stabilizer_cache();
    let analysis = plan_analysis(4);
    let settings = plan_settings(4.0);
    let mut plan = build_plan(&analysis.path, &settings);
    plan.meshes[0] = Some(MeshFrame::solve(128, 96, Affine::IDENTITY, &[], &[0.5; GRID_CELLS]).expect("actual mesh"));
    let plan = Arc::new(plan);
    let bytes = plan_retained_bytes(&plan);
    assert!(bytes > 1024, "actual plan storage was not measured");
    assert!(bytes < PLAN_CACHE_BUDGET);

    for variant in 0..=PLAN_CACHE_MAX_ENTRIES {
        let key = PlanKey { path: Arc::as_ptr(&analysis.path) as usize, smooth: variant as u64, crop: 0, safe: 0, maximum: 0, flags: 0 };
        let entry = cache_entry(&analysis, key);
        let retained = retain_plan(key, &entry, plan.clone());
        assert!(Arc::ptr_eq(&retained, &plan));
    }

    let cache = plan_cache().lock();
    assert!(cache.entries.len() <= PLAN_CACHE_MAX_ENTRIES);
    assert!(cache.retained_bytes <= PLAN_CACHE_BUDGET);
    let accounted = cache.entries.values().map(|entry| entry.retained_bytes.load(Ordering::Acquire)).sum::<usize>();
    assert_eq!(cache.retained_bytes, accounted);
}

#[test]
fn oversized_selected_plan_is_reused_instead_of_rebuilt_each_frame() {
    let _guard = plan_test_guard();
    clear_stabilizer_cache();
    let analysis = plan_analysis(4);
    let settings = plan_settings(4.0);
    let original = cached_plan(&analysis, &settings).expect("initial plan");
    let key = *plan_cache().lock().entries.keys().next().expect("selected key");
    clear_stabilizer_cache();
    let mut plan = build_plan(&analysis.path, &settings);
    // Reserved storage is part of the retained-byte accounting even before every frame is filled.
    plan.zooms.reserve(PLAN_CACHE_BUDGET / std::mem::size_of::<f64>() + 1);
    let plan = Arc::new(plan);
    assert!(plan_retained_bytes(&plan) > PLAN_CACHE_BUDGET);
    let entry = cache_entry(&analysis, key);
    retain_plan(key, &entry, plan.clone());
    drop(original);
    let reused = cached_plan(&analysis, &settings).expect("oversized selected plan");
    assert!(Arc::ptr_eq(&reused, &plan), "selected plan was evicted and rebuilt");
    assert_eq!(plan_cache().lock().entries.len(), 1);
    clear_stabilizer_cache();
}

#[test]
fn capacity_eviction_keeps_terminal_and_in_flight_slots() {
    let _guard = plan_test_guard();
    clear_stabilizer_cache();
    let key = |clip: u64| Key { clip, method: 0, subspace: false, detailed: false, fast: false, fps: 0, duration: 0, size: (64, 48) };
    let mut completed = Vec::new();
    {
        let mut cache = CACHE.lock();
        for index in 0..14u64 {
            let slot = Arc::new(Slot::new(4));
            slot.progress.lock().stage = StabilizerStage::Complete;
            let key = key(0xE51C_0000 + index);
            cache.insert(key, slot);
            completed.push(key);
        }
        let failed = Arc::new(Slot::new(4));
        failed.progress.lock().stage = StabilizerStage::Failed;
        cache.insert(key(0xE51C_00FE), failed);
        // Default stage is Analyzing: an in-flight slot must never be evicted or duplicated.
        cache.insert(key(0xE51C_00FF), Arc::new(Slot::new(4)));
    }

    let env = Clip::new(0xE51C_0100, false);
    let analysis = analysed_path(&env, 0.0, TrackMethod::Position, false, false, false).expect("new clip completes");
    assert_eq!(analysis.progress.lock().stage, StabilizerStage::Complete);

    let cache = CACHE.lock();
    assert_eq!(cache.len(), 16, "one completed slot is evicted for the new key");
    assert!(!cache.contains_key(&completed[0]), "oldest completed result evicted first");
    assert!(cache.contains_key(&completed[13]), "newest completed result kept");
    assert!(cache.contains_key(&key(0xE51C_00FE)), "failed slot kept: its job must not silently restart");
    assert!(cache.contains_key(&key(0xE51C_00FF)), "in-flight slot kept: its analysis must not be duplicated");
    assert!(cache.keys().any(|key| key.clip == 0xE51C_0100), "the requested key inserted");
    drop(cache);
    clear_stabilizer_cache();
}
