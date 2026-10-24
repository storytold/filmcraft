//! Background frame rendering for monitors, thumbnails and playback prefetch.
//!
//! A small pool of worker threads pulls prioritised jobs (the frame on screen first, then the
//! frames playback will need next, then thumbnails). Results land in a byte-budgeted cache keyed
//! by (target, frame, scale, revision); the UI shows the exact frame when ready and otherwise holds
//! the nearest frame it already has, so scrubbing never flashes black and never blocks the UI.
//! Sequence frames inside a rendered segment come from its render preview instead of the live
//! render (see `filmcraft_engine::previews`).

use std::cell::Cell;
use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::time::Duration;

use web_time::Instant;

use filmcraft_engine::previews::PreviewStore;
use filmcraft_engine::{MediaPool, Services};
use filmcraft_project::{ItemId, Project};
use filmcraft_time::Tick;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Target {
    /// Composite of a sequence at a timeline time.
    Sequence(ItemId),
    /// A single project item at a media time (Source monitor, thumbnails).
    Item(ItemId),
    /// A GPU frame plan of a sequence (decoded layers + transforms), composited on the GPU.
    SequencePlan(ItemId),
    /// The Multi-Camera view's angle grid of a multi-camera source at its time: one page (grid
    /// side, 0 = automatic; page) of the shown angles, each at the job's scale.
    MulticamGrid(ItemId, u8, u16),
    /// One angle of a multi-camera source (Edit Cameras thumbnails).
    MulticamAngle(ItemId, u32),
    /// Composite of a sequence rendered live, never from a render preview: previews are
    /// flattened over black, and the monitors' Transparency Grid and Alpha display mode need the
    /// frame's alpha.
    SequenceLive(ItemId),
}

/// Item jobs at this priority or a larger number are background work: thumbnails of the Project
/// panel and the timeline (50) and the Media Browser (40). Monitors and playback use 0..=3. Their
/// decoders are small and are not kept (`filmcraft_media::cancel::with_background`).
pub const BACKGROUND_PRIO: u32 = 40;

/// Whether a job is a thumbnail: a single item at a background priority. Sequence frames are never
/// background, whatever their priority (the scopes ask for the program frame at 40 while playing,
/// and must not turn the program's decoders into small ones).
fn is_background(job: &Job) -> bool {
    background_work(job.prio, job.key.target)
}

fn background_work(prio: u32, target: Target) -> bool {
    prio >= BACKGROUND_PRIO && matches!(target, Target::Item(_))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct FrameKey {
    pub target: Target,
    pub frame: i64,
    /// Output width in pixels (thumbnails) or scale ×1000 (monitors).
    pub size: u32,
    pub revision: u64,
    /// Draft decoding (reduced-resolution playback with Settings ▸ Playback ▸ Draft decoding):
    /// sources may decode approximate non-reference pictures
    /// ([`filmcraft_media::cancel::with_draft`]). Draft frames have their own keys, so a paused
    /// monitor, an export or a render never shows one.
    pub draft: bool,
}

/// Whether a monitor frame is requested with draft decoding ([`FrameKey::draft`]): only while
/// playing, at 1/2 resolution or lower, with the preference on.
pub fn draft_playback(playing: bool, scale: f32, enabled: bool) -> bool {
    enabled && playing && scale <= 0.5
}

/// A rendered frame as sRGB RGBA8: the color is the picture over black and the fourth byte is
/// its alpha ([`filmcraft_render::Image::over_black_with_alpha_rgba8`]).
pub struct Rgba {
    pub w: usize,
    pub h: usize,
    pub px: Vec<u8>,
}

impl Rgba {
    /// The frame as a texture image: opaque (the picture over black), or with its alpha, so what
    /// is painted under it shows through its transparent areas.
    pub fn color_image(&self, alpha: bool) -> egui::ColorImage {
        let pixels = if alpha {
            self.px.as_chunks::<4>().0.iter().map(|p| gamma_premultiplied(*p)).collect()
        } else {
            self.px.as_chunks::<4>().0.iter().map(|p| egui::Color32::from_rgb(p[0], p[1], p[2])).collect()
        };
        egui::ColorImage::new([self.w, self.h], pixels)
    }
}

/// A frame pixel (sRGB of the linear color premultiplied by alpha) as egui's color, which is the
/// sRGB of the straight color multiplied by alpha.
fn gamma_premultiplied([r, g, b, a]: [u8; 4]) -> egui::Color32 {
    match a {
        255 => egui::Color32::from_rgb(r, g, b),
        0 => egui::Color32::TRANSPARENT,
        _ => {
            let straight = |v: u8| straight_srgb(a, v);
            egui::Color32::from_rgba_unmultiplied(straight(r), straight(g), straight(b), a)
        }
    }
}

/// The sRGB of the straight color whose premultiplied value is sRGB `v` at alpha `a`, from a table
/// made on first use: a frame with partial alpha would otherwise take two `powf` per channel per
/// pixel on every upload.
fn straight_srgb(a: u8, v: u8) -> u8 {
    static TABLE: std::sync::OnceLock<Vec<u8>> = std::sync::OnceLock::new();
    let table = TABLE.get_or_init(|| {
        use egui::ecolor::{gamma_u8_from_linear_f32, linear_f32_from_gamma_u8};
        let mut t = vec![0u8; 256 * 256];
        for a in 1..=255u8 {
            let k = f32::from(a) / 255.0;
            for v in 0..=255u8 {
                if let Some(x) = t.get_mut(usize::from(a) * 256 + usize::from(v)) {
                    *x = gamma_u8_from_linear_f32((linear_f32_from_gamma_u8(v) / k).min(1.0));
                }
            }
        }
        t
    });
    table.get(usize::from(a) * 256 + usize::from(v)).copied().unwrap_or(v)
}

struct Job {
    key: FrameKey,
    queued: Instant,
    /// Set when the job is no longer wanted; sources poll it (`filmcraft_media::cancel`).
    cancel: Arc<std::sync::atomic::AtomicBool>,
    /// Queued by playback prefetch (dropped when playback stops).
    prefetch: bool,
    /// While playing: how far before this frame the frames are late (the playhead's distance;
    /// zero for the frame due now), so a decoder catching up may skip pictures only late frames
    /// need (`filmcraft_media::cancel::with_catch_up`).
    catch_up: Option<Tick>,
    time: Tick,
    /// Output scale relative to the target's frame size.
    scale: f32,
    project: Arc<Project>,
    prio: u32,
}

struct Shared {
    queue: Mutex<VecDeque<Job>>,
    cv: Condvar,
    done: Mutex<Cache>,
    plans: Mutex<PlanCache>,
    /// Running jobs: key, cancel flag, prefetch.
    in_flight: Mutex<Vec<(FrameKey, Arc<std::sync::atomic::AtomicBool>, bool)>>,
    /// Per-job timings, collected while profiling is on (benchmarks, `ui.inspect`).
    profiling: AtomicBool,
    records: Mutex<Vec<JobRecord>>,
    /// Running estimate (s) of the per-frame work of playback jobs that is not fetching source
    /// frames: compositing, CPU effects. Frames cannot be ready sooner than this after they are
    /// started, and the workers can finish at most `workers / cost` of them per second.
    render_cost: Mutex<f64>,
    workers: usize,
    /// Always-on counters for `perf.stats`.
    stats: Mutex<FrameStats>,
    /// Set (under the queue lock) when the [`FrameServer`] is dropped: the workers return.
    stop: AtomicBool,
}

/// Cumulative frame-job statistics (`perf.stats`): cheap enough to keep on all the time.
#[derive(Clone, Debug, Default)]
pub struct FrameStats {
    /// Jobs finished (including cancelled ones).
    pub jobs: u64,
    /// Jobs whose decode/render panicked (the worker survives; the frame is not cached).
    pub failed: u64,
    pub cancelled: u64,
    /// Jobs served from a render preview.
    pub preview: u64,
    /// Playback prefetch jobs.
    pub prefetch: u64,
    /// Requests answered by a cached frame or one already queued / running.
    pub request_hits: u64,
    /// Requests that queued a new job.
    pub request_misses: u64,
    /// Totals over all jobs (ms): getting source frames (decode), the rest (render), whole job.
    pub source_ms: f64,
    pub render_ms: f64,
    pub job_ms: f64,
    /// The most recent jobs: (source ms, render ms).
    pub recent: VecDeque<(f32, f32)>,
    /// The same jobs' targets and slowest source items, for an estimate of the source being
    /// played ([`FrameServer::plan_lookback`]).
    pub recent_origins: VecDeque<SampleOrigin>,
}

/// Where a frame job's timing sample comes from: its target, and the source item whose fetch took
/// longest (the one that set the job's pace; None when it fetched none, a render preview).
/// Different clips of a sequence share its target, so the target alone does not say which source
/// a sample measured.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SampleOrigin {
    pub target: Target,
    pub source: Option<ItemId>,
}

/// The samples of `recent` (one `origins` entry each) that measure `target`'s current source: its
/// samples whose slowest source is that of its newest sample. When playback moves from a fast clip
/// to a slow one, the first sample of the slow clip replaces the whole estimate instead of being
/// outvoted by the fast clip's samples until they age out of the window.
pub fn current_source_samples(recent: &[(f32, f32)], origins: &[SampleOrigin], target: Target) -> Vec<(f32, f32)> {
    let Some(source) = origins.iter().rev().find(|o| o.target == target).map(|o| o.source) else { return Vec::new() };
    recent.iter().zip(origins).filter(|(_, o)| o.target == target && o.source == source).map(|(r, _)| *r).collect()
}

impl FrameStats {
    const RECENT: usize = 256;

    /// JSON for `perf.stats`.
    pub fn to_json(&self) -> serde_json::Value {
        let pct = |f: &dyn Fn(&(f32, f32)) -> f32, p: f64| -> f64 {
            let mut v: Vec<f32> = self.recent.iter().map(f).collect();
            if v.is_empty() {
                return 0.0;
            }
            v.sort_by(|a, b| a.total_cmp(b));
            v[((v.len() - 1) as f64 * p).round() as usize] as f64
        };
        let n = self.jobs.max(1) as f64;
        let req = self.request_hits + self.request_misses;
        serde_json::json!({
            "jobs": self.jobs, "cancelled": self.cancelled, "failedJobs": self.failed, "previewJobs": self.preview, "prefetchJobs": self.prefetch,
            "requestHitRate": if req == 0 { 0.0 } else { self.request_hits as f64 / req as f64 },
            "decodeMs": {"mean": self.source_ms / n, "p50": pct(&|r| r.0, 0.5), "p95": pct(&|r| r.0, 0.95)},
            "renderMs": {"mean": self.render_ms / n, "p50": pct(&|r| r.1, 0.5), "p95": pct(&|r| r.1, 0.95)},
            "jobMsMean": self.job_ms / n,
        })
    }
}

/// Timing of one finished frame job (collected while [`FrameServer::set_profiling`] is on).
#[derive(Clone, Copy, Debug)]
pub struct JobRecord {
    pub key: FrameKey,
    pub prio: u32,
    pub queued: Instant,
    pub started: Instant,
    pub finished: Instant,
    /// Thread-CPU time of the whole job (robust to machine load; zero where unsupported).
    pub cpu: Duration,
    /// Wall and thread-CPU time spent getting source frames (decode, including waits on a
    /// source's decoder lock; the decoder's own worker threads are not included in `source_cpu`).
    pub source_wall: Duration,
    pub source_cpu: Duration,
    /// The frame came from a render preview.
    pub preview: bool,
    /// The job was cancelled while running (its result was discarded).
    pub cancelled: bool,
}

/// CPU time consumed by the calling thread (None where the platform has no thread clock).
pub fn thread_cpu_time() -> Option<Duration> {
    #[cfg(all(unix, not(target_arch = "wasm32")))]
    {
        let t = rustix::time::clock_gettime(rustix::time::ClockId::ThreadCPUTime);
        Some(Duration::new(t.tv_sec as u64, t.tv_nsec as u32))
    }
    #[cfg(windows)]
    {
        cpu_time::ThreadTime::try_now().ok().map(|t| t.as_duration())
    }
    #[cfg(not(any(windows, all(unix, not(target_arch = "wasm32")))))]
    {
        None
    }
}

/// CPU time consumed by the whole process, all threads (None where unsupported).
pub fn process_cpu_time() -> Option<Duration> {
    #[cfg(all(unix, not(target_arch = "wasm32")))]
    {
        let t = rustix::time::clock_gettime(rustix::time::ClockId::ProcessCPUTime);
        Some(Duration::new(t.tv_sec as u64, t.tv_nsec as u32))
    }
    #[cfg(windows)]
    {
        cpu_time::ProcessTime::try_now().ok().map(|t| t.as_duration())
    }
    #[cfg(not(any(windows, all(unix, not(target_arch = "wasm32")))))]
    {
        None
    }
}

thread_local! {
    /// (wall, cpu) spent in source `video_frame` calls by this worker during the current job.
    static SOURCE_TIME: Cell<(Duration, Duration)> = const { Cell::new((Duration::ZERO, Duration::ZERO)) };
    /// The source item whose single fetch took longest during the current job, and that time.
    static SLOWEST_SOURCE: Cell<Option<(ItemId, Duration)>> = const { Cell::new(None) };
    /// This worker is profiling the current job (thread CPU of source calls is measured).
    static PROFILING: Cell<bool> = const { Cell::new(false) };
}

/// Run `f`, adding its wall/CPU time to this thread's source-time accumulator.
fn timed_source<R>(f: impl FnOnce() -> R) -> R {
    timed_source_of(None, f)
}

/// [`timed_source`] for a fetch from `item`, which becomes the job's slowest source if no fetch
/// took longer (see [`SampleOrigin`]).
fn timed_source_of<R>(item: Option<ItemId>, f: impl FnOnce() -> R) -> R {
    let cpu = || if PROFILING.with(Cell::get) { thread_cpu_time().unwrap_or_default() } else { Duration::ZERO };
    let (w0, c0) = (Instant::now(), cpu());
    let r = f();
    let (dw, dc) = (w0.elapsed(), cpu().saturating_sub(c0));
    SOURCE_TIME.with(|s| {
        let (w, c) = s.get();
        s.set((w + dw, c + dc));
    });
    if let Some(item) = item {
        SLOWEST_SOURCE.with(|s| {
            if s.get().is_none_or(|(_, t)| dw > t) {
                s.set(Some((item, dw)));
            }
        });
    }
    r
}

/// A media source of `item` that times its `video_frame` calls (see [`JobRecord::source_wall`]).
struct TimedSource(filmcraft_media::SharedSource, ItemId);

impl filmcraft_media::MediaSource for TimedSource {
    fn info(&self) -> &filmcraft_media::MediaInfo {
        self.0.info()
    }
    fn video_frame(&self, req: filmcraft_media::FrameRequest) -> filmcraft_media::Result<Arc<filmcraft_frame::VideoFrame>> {
        timed_source_of(Some(self.1), || self.0.video_frame(req))
    }
    fn audio(&self, start: i64, frames: usize, sample_rate: u32) -> filmcraft_media::Result<filmcraft_frame::AudioBuffer> {
        self.0.audio(start, frames, sample_rate)
    }
    fn audio_stream(&self, stream: usize, start: i64, frames: usize, sample_rate: u32) -> filmcraft_media::Result<filmcraft_frame::AudioBuffer> {
        self.0.audio_stream(stream, start, frames, sample_rate)
    }
}

/// The job's source provider: the pool, with source fetches timed (the time feeds
/// [`FrameServer::render_cost`] and, while profiling, [`JobRecord`]).
struct JobProvider {
    inner: filmcraft_engine::media_pool::PoolProvider,
}

impl filmcraft_render::SourceProvider for JobProvider {
    fn source(&self, item: ItemId) -> Option<filmcraft_media::SharedSource> {
        let s = self.inner.pool.monitor_source(item, self.inner.source(item)?);
        Some(Arc::new(TimedSource(s, item)))
    }
}

/// A GPU frame plan with its layers' texel conversions already done (off the UI thread).
pub struct GpuPlan {
    pub plan: filmcraft_render::plan::FramePlan,
    pub prepared: filmcraft_gpu::PreparedPlan,
}

impl GpuPlan {
    /// Bytes this plan keeps alive (layer frames + converted texels).
    fn bytes(&self) -> usize {
        let frames = self.plan.source_bytes();
        frames + self.prepared.bytes()
    }
}

/// Finished plans, bounded by count and bytes (oldest use evicted first).
#[derive(Default)]
struct PlanCache {
    map: HashMap<FrameKey, (Arc<GpuPlan>, u64, usize)>,
    clock: u64,
    bytes: usize,
}

impl PlanCache {
    const MAX: usize = 96;
    const BUDGET: usize = 1 << 30;

    fn get(&mut self, k: &FrameKey) -> Option<Arc<GpuPlan>> {
        self.clock += 1;
        let clock = self.clock;
        self.map.get_mut(k).map(|(p, used, _)| {
            *used = clock;
            p.clone()
        })
    }

    fn insert(&mut self, k: FrameKey, p: GpuPlan) {
        self.clock += 1;
        let b = p.bytes();
        self.bytes += b;
        if let Some((_, _, old)) = self.map.insert(k, (Arc::new(p), self.clock, b)) {
            self.bytes -= old;
        }
        if self.map.len() > Self::MAX || self.bytes > Self::BUDGET {
            let mut v: Vec<(u64, FrameKey, usize)> = self.map.iter().map(|(k, v)| (v.1, *k, v.2)).collect();
            v.sort_unstable_by_key(|x| x.0);
            for (_, k, b) in v {
                if self.map.len() <= Self::MAX && self.bytes <= Self::BUDGET {
                    break;
                }
                self.map.remove(&k);
                self.bytes -= b;
            }
        }
    }
}

struct Cache {
    map: HashMap<FrameKey, (Arc<Rgba>, u64)>,
    bytes: usize,
    budget: usize,
    clock: u64,
}

impl Cache {
    fn insert(&mut self, k: FrameKey, v: Arc<Rgba>) {
        self.clock += 1;
        self.bytes += v.px.len();
        if let Some((old, _)) = self.map.insert(k, (v, self.clock)) {
            self.bytes -= old.px.len();
        }
        if self.bytes > self.budget {
            let mut all: Vec<(u64, FrameKey, usize)> = self.map.iter().map(|(k, (v, s))| (*s, *k, v.px.len())).collect();
            all.sort_unstable_by_key(|x| x.0);
            for (_, k, b) in all {
                if self.bytes <= self.budget * 8 / 10 {
                    break;
                }
                self.map.remove(&k);
                self.bytes -= b;
            }
        }
    }
}

pub struct FrameServer {
    shared: Arc<Shared>,
    pub pool: Arc<MediaPool>,
    pub services: Arc<dyn Services>,
    pub previews: Arc<PreviewStore>,
    repaint: Arc<Mutex<Option<Box<dyn Fn() + Send + Sync>>>>,
}

/// Frames queued ahead of the playhead while playing (and how far ahead a queued job may be
/// before it is dropped as stale).
pub fn prefetch_depth(speed: f64) -> i64 {
    if speed.abs() > 1.5 { 24 } else { 14 }
}

/// Playback starts its clock once this many frames from the playhead are ready, or after
/// [`PREROLL_TIMEOUT_S`]: otherwise the first frames after Play are due before any decoder had a
/// chance to produce them (a seek decodes from the previous keyframe).
pub const PREROLL_FRAMES: i64 = 6;

/// Seconds before a scrubbed-to frame whose frames a seek still decodes in full (see
/// [`FrameServer::request`]); earlier non-reference pictures are skipped on the way.
pub const SCRUB_KEEP_S: f64 = 2.0;
pub const PREROLL_TIMEOUT_S: f64 = 0.5;

/// Which frames playback asks for, given the per-frame render cost (s), the displayed frame rate
/// and the worker count: `(lead, stride)`. While the workers keep up, every frame from the
/// playhead on (`(1, 1)`). When they cannot (CPU effects slower than real time), `lead` is the
/// first frame ahead of the playhead worth starting (nearer ones would be late however soon they
/// start) and `stride` the spacing the workers can finish in real time: the frames in between
/// are dropped on purpose and evenly, instead of every frame arriving late.
///
/// This is the *render* cost, which spreads over the workers. A source that decodes one picture at
/// a time is a different matter: see [`source_cannot_keep_up`] and [`SOURCE_BOUND_AHEAD`].
pub fn playback_plan(render_cost: f64, fps: f64, workers: usize) -> (i64, i64) {
    let frames_per_job = render_cost * fps;
    let load = frames_per_job * 1.15 / workers.max(1) as f64;
    if load <= 1.0 {
        return (1, 1);
    }
    (frames_per_job.ceil() as i64, load.ceil() as i64)
}

/// Frames queued ahead of the playhead while the source cannot make the frame rate: a short queue.
/// The decoder is the serialized stage, so the queue *is* the latency of every picture — and of a
/// user's scrub, which waits behind it.
pub const SOURCE_BOUND_AHEAD: i64 = 3;

/// How many of the most recent frame jobs the source estimate reads (`source_estimate`): old
/// samples (a burst of scrubs, a previous playback) age out instead of pinning it.
pub const SOURCE_WINDOW: usize = 32;

/// Whether one decoder cannot produce the sequence's frame rate (`source_cost` is one picture's
/// decode plus readback, in seconds; the workers cannot share it).
///
/// Spacing requests out does not help such a source: a long-GOP stream decodes the frames between
/// two requests anyway, so asking further ahead only makes each job costlier to reach, and the
/// picture the user is waiting for (or their own scrub) queues behind those multi-second jobs. What
/// helps is asking for the frames just ahead ([`SOURCE_BOUND_AHEAD`]) and letting the presenter show
/// the newest picture the source has ([`FrameServer::plan_lookback`]) — dropping frames instead of
/// freezing (#2696).
pub fn source_cannot_keep_up(source_cost: f64, fps: f64) -> bool {
    source_cost * fps > 1.0
}

/// The most frames [`FrameServer::plan_lookback`] reaches back: a second at 240 fps. The monitors
/// probe each frame in that range, so a hostile frame rate (zero frame duration: infinite fps)
/// must not turn it into an endless loop.
pub const MAX_LOOKBACK: i64 = 240;

/// Where a monitor may look for a picture while the exact frame is not ready: up to `frames`
/// frames *behind* the playhead in the play direction `dir` (frames already passed: earlier ones
/// forward, later ones in reverse playback, where a picture still finishing is above the playhead).
/// Paused, `dir` is 1: a scrub shows an earlier picture, such as the keyframe a seek decodes from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Lookback {
    pub frames: i64,
    pub dir: i64,
}

impl Lookback {
    /// Looking back towards earlier frames (paused, forward playback, the reference frame).
    pub fn backward(frames: i64) -> Self {
        Lookback { frames, dir: 1 }
    }

    /// The frames to try instead of `frame`, nearest first.
    pub fn frames_before(self, frame: i64) -> impl Iterator<Item = i64> {
        let dir = if self.dir < 0 { -1 } else { 1 };
        (1..=self.frames.clamp(0, MAX_LOOKBACK)).map(move |d| frame.saturating_sub(d * dir))
    }
}

/// [`FrameServer::plan_lookback`] for a measured picture `latency` (s; None before any job ran)
/// at `fps`: the latency in frames, at least 2, at most a second of frames and [`MAX_LOOKBACK`].
pub fn lookback_frames(latency: Option<f64>, fps: f64) -> i64 {
    let Some(latency) = latency else { return 6 };
    let fps = if fps.is_finite() { fps.clamp(2.0, MAX_LOOKBACK as f64) } else { MAX_LOOKBACK as f64 };
    let frames = latency * fps;
    if frames.is_finite() { frames.ceil().clamp(2.0, fps) as i64 } else { 2 }
}

/// How far from the playhead (in frames, either way) playback keeps jobs for its view: the frames
/// it asks for (`lead + ahead * stride`, times `step`), plus slack for frames asked a refresh ago
/// while it renders. A source-bound playback gets no slack: frames still queued from the deeper
/// render-bound prefetch would otherwise run on the serialized decoder before the short queue.
pub fn playback_span(lead: i64, ahead: i64, stride: i64, step: i64, source_bound: bool) -> i64 {
    let slack = if source_bound { 0 } else { 26 };
    lead.saturating_add(ahead.saturating_mul(stride)).saturating_mul(step).saturating_add(slack)
}

/// Whether playback keeps a job for the frame `off` frames from the playhead (`dir`: the play
/// direction): ahead of the playhead and within `span`, or at most `behind` frames late. Queued
/// jobs get `behind` 0; a source-bound playback lets *running* ones finish while they are within
/// the monitor's look-back ([`FrameServer::plan_lookback`]): a cancelled job's result is dropped,
/// so cancelling the picture a decoder is finishing would leave the monitor nothing newer to show.
pub fn keeps_playback_job(off: i64, dir: i64, span: i64, behind: i64) -> bool {
    let ahead = off.saturating_mul(dir);
    if ahead >= 0 { ahead < span } else { ahead >= behind.saturating_neg() }
}

impl FrameServer {
    /// Settings ▸ Memory ▸ frame cache budget (bytes); evicts down to it right away.
    pub fn set_cache_budget(&self, bytes: usize) {
        let mut c = self.shared.done.lock().unwrap_or_else(PoisonError::into_inner);
        c.budget = bytes.max(16 << 20);
        if c.bytes > c.budget
            && let Some((k, v)) = c.map.iter().next().map(|(k, (v, _))| (*k, v.clone()))
        {
            // re-inserting an entry runs the eviction pass
            c.insert(k, v);
        }
    }

    /// (bytes cached, budget).
    pub fn cache_usage(&self) -> (usize, usize) {
        let c = self.shared.done.lock().unwrap_or_else(PoisonError::into_inner);
        (c.bytes, c.budget)
    }

    pub fn new(pool: Arc<MediaPool>, services: Arc<dyn Services>, previews: Arc<PreviewStore>, workers: usize) -> Self {
        let shared = Arc::new(Shared {
            queue: Mutex::new(VecDeque::new()),
            cv: Condvar::new(),
            done: Mutex::new(Cache { map: HashMap::new(), bytes: 0, budget: 768 << 20, clock: 0 }),
            plans: Mutex::new(PlanCache::default()),
            in_flight: Mutex::new(Vec::new()),
            profiling: AtomicBool::new(false),
            records: Mutex::new(Vec::new()),
            render_cost: Mutex::new(0.0),
            workers: workers.max(1),
            stats: Mutex::new(FrameStats::default()),
            stop: AtomicBool::new(false),
        });
        let repaint: Arc<Mutex<Option<Box<dyn Fn() + Send + Sync>>>> = Arc::new(Mutex::new(None));
        #[cfg(not(target_arch = "wasm32"))]
        for i in 0..workers.max(1) {
            let sh = shared.clone();
            let pool = pool.clone();
            let services = services.clone();
            let rp = repaint.clone();
            let pv = previews.clone();
            std::thread::Builder::new().name(format!("filmcraft-frames-{i}")).spawn(move || worker(sh, pool, services, pv, rp)).ok();
        }
        #[cfg(target_arch = "wasm32")]
        let _ = workers;
        Self { shared, pool, services, previews, repaint }
    }

    /// The default number of frame workers for this machine.
    pub fn default_workers() -> usize {
        std::thread::available_parallelism().map(|n| n.get().clamp(2, 6)).unwrap_or(3)
    }

    pub fn set_context(&self, ctx: &egui::Context) {
        let mut g = self.repaint.lock().unwrap_or_else(PoisonError::into_inner);
        if g.is_none() {
            let ctx = ctx.clone();
            *g = Some(Box::new(move || ctx.request_repaint()));
        }
    }

    /// Collect a [`JobRecord`] per finished job (benchmarks); `take_records` drains them.
    pub fn set_profiling(&self, on: bool) {
        self.shared.profiling.store(on, Ordering::Relaxed);
    }

    pub fn take_records(&self) -> Vec<JobRecord> {
        std::mem::take(&mut *self.shared.records.lock().unwrap_or_else(PoisonError::into_inner))
    }

    /// Cumulative job statistics (`perf.stats`).
    pub fn stats(&self) -> FrameStats {
        self.shared.stats.lock().unwrap_or_else(PoisonError::into_inner).clone()
    }

    /// Worker threads.
    pub fn workers(&self) -> usize {
        self.shared.workers
    }

    /// (cached images, their bytes, cached GPU plans, their bytes).
    pub fn cache_entries(&self) -> (usize, usize, usize, usize) {
        let (n, b) = {
            let c = self.shared.done.lock().unwrap_or_else(PoisonError::into_inner);
            (c.map.len(), c.bytes)
        };
        let p = self.shared.plans.lock().unwrap_or_else(PoisonError::into_inner);
        (n, b, p.map.len(), p.bytes)
    }

    pub fn get(&self, k: &FrameKey) -> Option<Arc<Rgba>> {
        let mut c = self.shared.done.lock().unwrap_or_else(PoisonError::into_inner);
        c.clock += 1;
        let clock = c.clock;
        c.map.get_mut(k).map(|(v, s)| {
            *s = clock;
            v.clone()
        })
    }

    pub fn get_plan(&self, k: &FrameKey) -> Option<Arc<GpuPlan>> {
        self.shared.plans.lock().unwrap_or_else(PoisonError::into_inner).get(k)
    }

    /// Whether the exact frame (image or plan) is ready.
    pub fn is_ready(&self, k: &FrameKey) -> bool {
        match k.target {
            Target::SequencePlan(_) => self.shared.plans.lock().unwrap_or_else(PoisonError::into_inner).map.contains_key(k),
            _ => self.shared.done.lock().unwrap_or_else(PoisonError::into_inner).map.contains_key(k),
        }
    }

    /// Nearest cached plan at or before `frame`.
    /// The plan of `key`'s frame, else of the nearest frame within `back` (see [`Lookback`]).
    pub fn nearest_plan(&self, key: FrameKey, back: Lookback) -> Option<(FrameKey, Arc<GpuPlan>)> {
        let mut g = self.shared.plans.lock().unwrap_or_else(PoisonError::into_inner);
        std::iter::once(key.frame).chain(back.frames_before(key.frame)).find_map(|frame| {
            let k = FrameKey { frame, ..key };
            g.get(&k).map(|p| (k, p))
        })
    }

    /// Queue a job unless it is cached, queued or in flight.
    ///
    /// The frame on screen (`prio` 0: scrubbing, a seek) treats frames more than
    /// [`SCRUB_KEEP_S`] before it as not wanted: a decoder seeking from a keyframe skips their
    /// non-reference pictures, while nearby frames stay decoded (and cached) for scrubbing back.
    pub fn request(&self, key: FrameKey, time: Tick, scale: f32, project: &Arc<Project>, prio: u32) {
        let late = (prio == 0).then(|| Tick::from_seconds_f64(SCRUB_KEEP_S));
        self.request_job(key, time, scale, project, prio, false, late);
    }

    fn request_job(&self, key: FrameKey, time: Tick, scale: f32, project: &Arc<Project>, prio: u32, prefetch: bool, catch_up: Option<Tick>) {
        let hit = || self.shared.stats.lock().unwrap_or_else(PoisonError::into_inner).request_hits += 1;
        if self.shared.done.lock().unwrap_or_else(PoisonError::into_inner).map.contains_key(&key)
            || self.shared.plans.lock().unwrap_or_else(PoisonError::into_inner).map.contains_key(&key)
        {
            hit();
            return;
        }
        if self.shared.in_flight.lock().unwrap_or_else(PoisonError::into_inner).iter().any(|(k, c, _)| *k == key && !c.load(Ordering::Relaxed)) {
            hit();
            return;
        }
        let mut q = self.shared.queue.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(j) = q.iter_mut().find(|j| j.key == key) {
            j.prio = j.prio.min(prio);
            j.prefetch &= prefetch;
            // the playhead only moves on: the newest (smallest) margin
            j.catch_up = match (j.catch_up, catch_up) {
                (Some(a), Some(b)) => Some(a.min(b)),
                (a, b) => a.or(b),
            };
            drop(q);
            hit();
            return;
        }
        self.shared.stats.lock().unwrap_or_else(PoisonError::into_inner).request_misses += 1;
        if prio == 0 {
            // A new frame on screen for this view replaces the one asked for before: scrubbing
            // asks for one per refresh, and without this the oldest position would decode
            // first (or keep a decoder busy seeking to it) while the newest waits.
            //
            // A job already running for the same frame at an older revision (a value being
            // dragged asks for a new revision per refresh) is left to finish: it needs the same
            // source frames, so it holds no decoder back, and when frames take longer than a
            // refresh, cancelling each one for the next would show nothing until the mouse rests.
            let same_view = |k: &FrameKey| k.target == key.target && k.size == key.size;
            q.retain(|j| !(j.prio == 0 && !j.prefetch && same_view(&j.key)));
            for (k, c, prefetch) in self.shared.in_flight.lock().unwrap_or_else(PoisonError::into_inner).iter() {
                if !prefetch && same_view(k) && k.frame != key.frame {
                    c.store(true, Ordering::Relaxed);
                }
            }
        }
        q.push_back(Job { key, queued: Instant::now(), cancel: Default::default(), prefetch, catch_up, time, scale, project: project.clone(), prio });
        drop(q);
        self.shared.cv.notify_one();
    }

    /// Playback scheduling for the monitor showing `key` (the frame due now): request it first,
    /// then the next frames in display order, and drop queued jobs for this target that are
    /// stale (old revision, behind the playhead or too far ahead).
    ///
    /// When frames cost more to render than the frame interval (CPU effects), frames that could
    /// not be ready in time are not started and the workers get evenly spaced frames they can
    /// finish ([`playback_plan`]). While `preroll` (clock not started yet) every frame is wanted.
    pub fn schedule_playback(&self, key: FrameKey, rate: filmcraft_time::FrameRate, scale: f32, project: &Arc<Project>, speed: f64, preroll: bool) {
        let dir = if speed < 0.0 { -1 } else { 1 };
        let step = speed.abs().max(1.0) as i64;
        let fps = rate.as_f64() * speed.abs().max(1.0) / step as f64;
        let (lead, stride) = if preroll { (1, 1) } else { playback_plan(self.render_cost(), fps, self.shared.workers) };
        if lead <= 1 {
            // the frame due now; once the clock runs, the frames before it are late
            self.request_job(key, rate.tick_of(key.frame), scale, project, 0, false, (!preroll && dir > 0).then_some(Tick::ZERO));
        }
        // During preroll the clock has not started: only the frames that gate start, not a
        // prefetch run that walks the rest of a LongGOP and evicts the playhead pictures.
        // the frames the monitor shows per second: below 1x that is fewer than the frame rate
        let shown_fps = rate.as_f64() * speed.abs() / step as f64;
        let source_bound = !preroll && source_cannot_keep_up(self.source_cost(key.target), shown_fps);
        let ahead = if preroll {
            (PREROLL_FRAMES - 1).max(0)
        } else if source_bound {
            SOURCE_BOUND_AHEAD
        } else {
            prefetch_depth(speed)
        };
        for i in 0..ahead {
            let f = key.frame + (lead + i * stride) * dir * step;
            if f >= 0 {
                // forward play: frames before the playhead are late (reverse play skips nothing)
                let late = (!preroll && dir > 0).then(|| rate.tick_of(f) - rate.tick_of(key.frame));
                self.request_job(FrameKey { frame: f, ..key }, rate.tick_of(f), scale, project, i as u32 + 1, true, late);
            }
        }
        let (target, rev, cur) = (key.target, key.revision, key.frame);
        let span = playback_span(lead, ahead, stride, step, source_bound);
        let behind = if source_bound { self.plan_lookback(key.target, rate.as_f64()) } else { 0 };
        let keep = |k: &FrameKey, behind| k.target != target || (k.revision == rev && keeps_playback_job(k.frame - cur, dir, span, behind));
        self.retain_jobs(|k| keep(k, 0), |k| keep(k, behind));
    }

    /// Whether playback can start at `key`: the next [`PREROLL_FRAMES`] frames (up to `last`)
    /// are ready.
    pub fn preroll_ready(&self, key: FrameKey, speed: f64, last: i64) -> bool {
        let step = (speed.abs().max(1.0) as i64) * if speed < 0.0 { -1 } else { 1 };
        (0..PREROLL_FRAMES).map(|i| key.frame + i * step).filter(|f| *f >= 0 && *f <= last).all(|f| self.is_ready(&FrameKey { frame: f, ..key }))
    }

    /// The estimate playback paces on, from the recent frame jobs' source phase (ms, oldest
    /// first): `(cost, latency)`, both in seconds.
    ///
    /// `cost` is the decoder's own time for one picture, taken as a low quantile: the job that
    /// waited least for its turn is the one not queued behind others, so waiting does not feed back
    /// into the plan (a deep queue would otherwise keep raising the estimate). `latency` is the
    /// median, which *is* what a picture waits when the source is behind — what the monitor's
    /// lookback needs.
    ///
    /// Only the most recent [`SOURCE_WINDOW`] jobs count, so a burst of scrubs (each seeking from a
    /// keyframe, seconds long) ages out instead of pinning the estimate at "cannot keep up".
    pub fn source_estimate(recent: &[(f32, f32)]) -> Option<(f64, f64)> {
        let tail = recent.len().saturating_sub(SOURCE_WINDOW);
        let mut v: Vec<f32> = recent.get(tail..)?.iter().map(|r| r.0).collect();
        if v.is_empty() {
            return None;
        }
        v.sort_by(|a, b| a.total_cmp(b));
        // `.get` per AGENTS §0.4: a quantile of a sorted non-empty vector, never an index panic
        let at = |q: f64| f64::from(v.get(((v.len() - 1) as f64 * q).round() as usize).copied().unwrap_or(0.0)) / 1e3;
        Some((at(0.1), at(0.5)))
    }

    /// [`source_estimate`](Self::source_estimate) of this server's recent jobs for `target`'s
    /// current source ([`current_source_samples`]): thumbnails, another monitor's frames or another
    /// clip of the sequence say nothing about the source being played.
    fn source_estimate_now(&self, target: Target) -> Option<(f64, f64)> {
        let st = self.shared.stats.lock().unwrap_or_else(PoisonError::into_inner);
        let (recent, origins): (Vec<(f32, f32)>, Vec<SampleOrigin>) = (st.recent.iter().copied().collect(), st.recent_origins.iter().copied().collect());
        drop(st);
        Self::source_estimate(&current_source_samples(&recent, &origins, target))
    }

    /// The current per-frame source cost estimate (s) of `target`, see [`playback_plan`].
    fn source_cost(&self, target: Target) -> f64 {
        self.source_estimate_now(target).map_or(0.0, |(cost, _)| cost)
    }

    /// How far behind the playhead a plan may be and still be worth showing, in frames: the measured
    /// latency of a picture, bounded to a second of frames.
    ///
    /// A second is deliberate: seeking in a long-GOP stream decodes from the previous keyframe
    /// (0.96 s in the reporter's clip), so showing that keyframe while the exact frame decodes is
    /// what makes a scrub feel immediate — while anything older is a different shot. It also covers
    /// a source that cannot make the frame rate: the monitor shows the newest picture it has rather
    /// than freezing (#2696).
    pub fn plan_lookback(&self, target: Target, fps: f64) -> i64 {
        lookback_frames(self.source_estimate_now(target).map(|(_, latency)| latency), fps)
    }

    /// The current per-frame render cost estimate (s), see [`playback_plan`].
    pub fn render_cost(&self) -> f64 {
        *self.shared.render_cost.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Drop queued jobs that fail `keep` (e.g. stale revisions or frames far from the playhead).
    /// Jobs already running for such frames are cancelled: a worker blocked behind a decoder
    /// would otherwise go on to decode a frame playback has passed, possibly from its keyframe.
    pub fn retain_queue(&self, keep: impl Fn(&FrameKey) -> bool) {
        self.retain_jobs(&keep, &keep);
    }

    /// [`retain_queue`](Self::retain_queue) with one rule for queued jobs and another for running
    /// ones (cancelled when they fail `keep_running`).
    pub fn retain_jobs(&self, keep_queued: impl Fn(&FrameKey) -> bool, keep_running: impl Fn(&FrameKey) -> bool) {
        self.shared.queue.lock().unwrap_or_else(PoisonError::into_inner).retain(|j| keep_queued(&j.key));
        for (k, c, _) in self.shared.in_flight.lock().unwrap_or_else(PoisonError::into_inner).iter() {
            if !keep_running(k) {
                c.store(true, Ordering::Relaxed);
            }
        }
    }

    /// Playback stopped: drop its queued prefetch jobs and cancel the running ones (left alone,
    /// they would keep the workers busy, e.g. rendering effects for frames nobody will see).
    pub fn stop_prefetch(&self) {
        self.shared.queue.lock().unwrap_or_else(PoisonError::into_inner).retain(|j| !j.prefetch);
        for (_, c, prefetch) in self.shared.in_flight.lock().unwrap_or_else(PoisonError::into_inner).iter() {
            if *prefetch {
                c.store(true, Ordering::Relaxed);
            }
        }
    }

    pub fn queue_len(&self) -> usize {
        self.shared.queue.lock().unwrap_or_else(PoisonError::into_inner).len()
    }

    /// The most recent cached frame for `target` at or before `frame` within `max_back` frames.
    pub fn nearest(&self, target: Target, frame: i64, size: u32, revision: u64, max_back: i64) -> Option<Arc<Rgba>> {
        for d in 0..=max_back {
            if let Some(v) = self.get(&FrameKey { target, frame: frame - d, size, revision, draft: false }) {
                return Some(v);
            }
        }
        None
    }

    /// Without worker threads (wasm32): run queued jobs on the calling (UI) thread, highest
    /// priority first, until `budget` has passed (at least one job). Jobs whose media is still
    /// loading go back to the queue. Returns the number of jobs finished. A no-op with workers.
    pub fn pump(&self, budget: Duration) -> usize {
        if cfg!(not(target_arch = "wasm32")) {
            return 0;
        }
        let t0 = Instant::now();
        let mut done = 0;
        let mut retry = Vec::new();
        loop {
            let job = {
                let mut q = self.shared.queue.lock().unwrap_or_else(PoisonError::into_inner);
                if q.is_empty() {
                    break;
                }
                let best = q.iter().enumerate().min_by_key(|(i, j)| (j.prio, *i)).map(|(i, _)| i).unwrap_or(0);
                match q.remove(best) {
                    Some(job) => job,
                    None => break,
                }
            };
            if run_job(&self.shared, &job, &self.pool, &self.services, &self.previews) {
                done += 1;
            } else {
                retry.push(job);
            }
            if t0.elapsed() >= budget {
                break;
            }
        }
        if !retry.is_empty() {
            let mut q = self.shared.queue.lock().unwrap_or_else(PoisonError::into_inner);
            for j in retry.into_iter().rev() {
                if !q.iter().any(|x| x.key == j.key) {
                    q.push_front(j);
                }
            }
        }
        done
    }
}

impl Drop for FrameServer {
    /// Stop the workers: queued jobs are dropped, running ones are cancelled, and each worker
    /// returns once it has no job, releasing the frame cache, the media pool and the services.
    fn drop(&mut self) {
        let mut q = self.shared.queue.lock().unwrap_or_else(PoisonError::into_inner);
        self.shared.stop.store(true, Ordering::Relaxed);
        q.clear();
        drop(q);
        for (_, c, _) in self.shared.in_flight.lock().unwrap_or_else(PoisonError::into_inner).iter() {
            c.store(true, Ordering::Relaxed);
        }
        self.shared.cv.notify_all();
    }
}

/// Counts shown and dropped frames while playing.
///
/// A frame of the timeline is *shown* when its exact picture was on screen at some refresh while
/// it was the frame due, and *dropped* otherwise — including frames the playhead skipped because
/// no refresh happened during their interval. Frames left out on purpose (fast-forward speeds
/// step over frames) are not counted.
#[derive(Clone, Debug, Default)]
pub struct PlaybackMeter {
    pub shown: u64,
    pub dropped: u64,
    /// Frame due at the last refresh, and whether its exact picture has been shown.
    current: Option<(i64, bool)>,
    /// Frames advanced per displayed frame (speed), and direction.
    step: i64,
}

impl PlaybackMeter {
    pub fn start(&mut self, speed: f64) {
        *self = Self { step: (speed.abs().max(1.0) as i64) * if speed < 0.0 { -1 } else { 1 }, ..Default::default() };
    }

    /// Record one display refresh: `frame` is due now and `exact` says whether it is on screen.
    pub fn refresh(&mut self, frame: i64, exact: bool) {
        let step = if self.step == 0 { 1 } else { self.step };
        match self.current {
            Some((f, seen)) if f == frame => self.current = Some((f, seen || exact)),
            Some((f, seen)) => {
                self.close(seen);
                let moved = (frame - f) / step;
                // Forward progress: count frames passed over without a refresh. A jump backwards
                // (loop restart) or a huge jump is a discontinuity, not a drop.
                if moved > 1 && moved < 1000 {
                    self.dropped += (moved - 1) as u64;
                }
                self.current = Some((frame, exact));
            }
            None => self.current = Some((frame, exact)),
        }
    }

    fn close(&mut self, seen: bool) {
        if seen {
            self.shown += 1;
        } else {
            self.dropped += 1;
        }
    }

    /// Continue from `frame` without counting what happened since the last refresh (the window
    /// was hidden, so nothing could be shown).
    pub fn resync(&mut self, frame: i64, exact: bool) {
        self.current = Some((frame, exact));
    }

    /// Playback stopped: account for the frame on screen.
    pub fn finish(&mut self) {
        if let Some((_, seen)) = self.current.take() {
            self.close(seen);
        }
    }

    /// Shown and dropped so far, counting the frame currently due as shown when it is.
    pub fn counts(&self) -> (u64, u64) {
        match self.current {
            Some((_, true)) => (self.shown + 1, self.dropped),
            _ => (self.shown, self.dropped),
        }
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn worker(
    sh: Arc<Shared>,
    pool: Arc<MediaPool>,
    services: Arc<dyn Services>,
    previews: Arc<PreviewStore>,
    repaint: Arc<Mutex<Option<Box<dyn Fn() + Send + Sync>>>>,
) {
    loop {
        let job = {
            let mut q = sh.queue.lock().unwrap_or_else(PoisonError::into_inner);
            loop {
                if sh.stop.load(Ordering::Relaxed) {
                    return;
                }
                if !q.is_empty() {
                    // highest priority (lowest number) first; FIFO among equals
                    let best = q.iter().enumerate().min_by_key(|(i, j)| (j.prio, *i)).map(|(i, _)| i).unwrap_or(0);
                    if let Some(job) = q.remove(best) {
                        break job;
                    }
                }
                q = sh.cv.wait(q).unwrap_or_else(PoisonError::into_inner);
            }
        };
        // A panic while decoding or rendering one frame (bad media, a decoder bug) must not take
        // the worker down with it: a dead worker leaves its frame "in flight" forever and the
        // monitors blank. Log it, forget the job and keep serving.
        let ran = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| run_job(&sh, &job, &pool, &services, &previews)));
        if ran.is_err() {
            sh.in_flight.lock().unwrap_or_else(PoisonError::into_inner).retain(|(k, c, _)| !(*k == job.key && Arc::ptr_eq(c, &job.cancel)));
            sh.stats.lock().unwrap_or_else(PoisonError::into_inner).failed += 1;
            log::error!("frame job {:?} panicked; the worker continues", job.key);
        }
        if let Some(f) = repaint.lock().unwrap_or_else(PoisonError::into_inner).as_ref() {
            f();
        }
    }
}

/// Run one job and cache its result. Returns false when the job's media was still loading (an
/// asynchronous web read): nothing was cached and the job should be retried.
/// Fault injection for robustness tests (and agents checking recovery): the next `n` frame jobs
/// panic as if a decoder had hit a bug.
#[doc(hidden)]
pub fn inject_job_panics(n: u32) {
    INJECT_PANICS.store(n, Ordering::Relaxed);
}
static INJECT_PANICS: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

fn run_job(sh: &Shared, job: &Job, pool: &Arc<MediaPool>, services: &Arc<dyn Services>, previews: &PreviewStore) -> bool {
    {
        let mut in_flight = sh.in_flight.lock().unwrap_or_else(PoisonError::into_inner);
        // A job taken from the queue just before the server was dropped is not in the list
        // that `Drop` cancels.
        if sh.stop.load(Ordering::Relaxed) {
            job.cancel.store(true, Ordering::Relaxed);
        }
        in_flight.push((job.key, job.cancel.clone(), job.prefetch));
    }
    if INJECT_PANICS.load(Ordering::Relaxed) > 0 && INJECT_PANICS.fetch_sub(1, Ordering::Relaxed) > 0 {
        crate::crash::injected_fault("injected frame-job fault");
    }
    let profiling = sh.profiling.load(Ordering::Relaxed);
    let (started, cpu0) = (Instant::now(), if profiling { thread_cpu_time().unwrap_or_default() } else { Duration::ZERO });
    SOURCE_TIME.with(|s| s.set((Duration::ZERO, Duration::ZERO)));
    SLOWEST_SOURCE.with(|s| s.set(None));
    PROFILING.with(|p| p.set(profiling));
    // A cancelled job may have missed layers (its source gave up): its result is not cached.
    // A job whose media bytes were still loading (web) rendered with missing layers: like a
    // cancelled one, its result is not cached (the caller retries it).
    let _ = filmcraft_media::pending::take();
    let mut loading = false;
    let preview = filmcraft_media::cancel::with_cancel(&job.cancel, || {
        filmcraft_media::cancel::with_catch_up(job.catch_up, || {
            filmcraft_media::cancel::with_draft(job.key.draft, || {
                filmcraft_media::cancel::with_background(is_background(job), || {
                    if let Target::SequencePlan(seq) = job.key.target {
                        let (plan, pv) = plan_job(job, seq, pool, services, previews);
                        loading = filmcraft_media::pending::take();
                        if !job.cancel.load(Ordering::Relaxed) && !loading {
                            // Convert texels for upload here, not on the UI thread when the frame is shown.
                            let prepared = filmcraft_gpu::prepare(&plan);
                            sh.plans.lock().unwrap_or_else(PoisonError::into_inner).insert(job.key, GpuPlan { plan, prepared });
                        }
                        pv
                    } else {
                        let (img, pv) = render_job(job, pool, services, previews);
                        loading = filmcraft_media::pending::take();
                        if !job.cancel.load(Ordering::Relaxed) && !loading {
                            sh.done.lock().unwrap_or_else(PoisonError::into_inner).insert(job.key, Arc::new(img));
                        }
                        pv
                    }
                })
            })
        })
    });
    let cancelled = job.cancel.load(Ordering::Relaxed);
    sh.in_flight.lock().unwrap_or_else(PoisonError::into_inner).retain(|(k, c, _)| !(*k == job.key && Arc::ptr_eq(c, &job.cancel)));
    if job.prefetch {
        // Work beyond fetching source frames (decoding is sequential per source and is
        // not saved by skipping frames, so it is left out of the estimate). A cancelled job
        // only tells that the work takes at least this long.
        let cost = started.elapsed().saturating_sub(SOURCE_TIME.with(|s| s.get()).0).as_secs_f64();
        let mut c = sh.render_cost.lock().unwrap_or_else(PoisonError::into_inner);
        if !cancelled || cost > *c {
            *c = if *c == 0.0 { cost } else { *c * 0.8 + cost * 0.2 };
        }
    }
    {
        let job_wall = started.elapsed();
        let source = SOURCE_TIME.with(|s| s.get()).0;
        let (src_ms, render_ms) = (source.as_secs_f64() * 1e3, job_wall.saturating_sub(source).as_secs_f64() * 1e3);
        let mut st = sh.stats.lock().unwrap_or_else(PoisonError::into_inner);
        st.jobs += 1;
        st.cancelled += cancelled as u64;
        st.preview += preview as u64;
        st.prefetch += job.prefetch as u64;
        st.source_ms += src_ms;
        st.render_ms += render_ms;
        st.job_ms += job_wall.as_secs_f64() * 1e3;
        if st.recent.len() >= FrameStats::RECENT {
            st.recent.pop_front();
        }
        st.recent.push_back((src_ms as f32, render_ms as f32));
        if st.recent_origins.len() >= FrameStats::RECENT {
            st.recent_origins.pop_front();
        }
        st.recent_origins.push_back(SampleOrigin { target: job.key.target, source: SLOWEST_SOURCE.with(|s| s.get()).map(|(item, _)| item) });
    }
    if profiling {
        let (source_wall, source_cpu) = SOURCE_TIME.with(|s| s.get());
        let rec = JobRecord {
            key: job.key,
            prio: job.prio,
            queued: job.queued,
            started,
            finished: Instant::now(),
            cpu: thread_cpu_time().unwrap_or_default().saturating_sub(cpu0),
            source_wall,
            source_cpu,
            preview,
            cancelled,
        };
        sh.records.lock().unwrap_or_else(PoisonError::into_inner).push(rec);
    }
    !loading
}

/// The preview frame for a sequence job, when its segment has been rendered.
fn preview_frame(job: &Job, seq: ItemId, pool: &MediaPool, previews: &PreviewStore) -> Option<Arc<filmcraft_frame::VideoFrame>> {
    let rate = job.project.sequence(seq)?.settings.frame_rate;
    timed_source(|| previews.frame(pool, &job.project, seq, rate.frame_at(job.time), job.scale))
}

fn provider(job: &Job, pool: &Arc<MediaPool>, services: &Arc<dyn Services>) -> JobProvider {
    JobProvider { inner: pool.provider(job.project.clone(), services.clone()) }
}

fn plan_job(job: &Job, seq: ItemId, pool: &Arc<MediaPool>, services: &Arc<dyn Services>, previews: &PreviewStore) -> (filmcraft_render::plan::FramePlan, bool) {
    let opts = filmcraft_render::RenderOptions { scale: job.scale, captions: true, ..Default::default() };
    if let Some(frame) = preview_frame(job, seq, pool, previews)
        && let Some(q) = job.project.sequence(seq)
    {
        // One full-frame layer: the preview scaled to the output size. Previews never contain
        // captions (they are not part of the preview hash), so captions are layered on live.
        let (w, h) = filmcraft_render::output_size(q, job.scale);
        let matrix = filmcraft_geom::Affine::scale(w as f64 / frame.width.max(1) as f64, h as f64 / frame.height.max(1) as f64);
        let mut layers = vec![filmcraft_render::plan::PlanLayer { frame, matrix, opacity: 1.0, blend: filmcraft_render::Blend::Normal, fx: None, input: None }];
        for o in filmcraft_render::caption_overlays(q, job.time, w, h) {
            layers.push(filmcraft_render::plan::PlanLayer {
                frame: Arc::new(filmcraft_frame::VideoFrame::rgba_f32(o.w as u32, o.h as u32, o.px)),
                matrix: filmcraft_geom::Affine::translate(o.x as f64, o.y as f64),
                opacity: 1.0,
                blend: filmcraft_render::Blend::Normal,
                fx: None,
                input: None,
            });
        }
        return (filmcraft_render::plan::FramePlan::Layers { width: w, height: h, layers }, true);
    }
    let provider = provider(job, pool, services);
    (filmcraft_render::plan::plan_frame(&job.project, seq, job.time, opts, &provider), false)
}

/// Render a job to RGBA8; the flag says whether it came from a render preview.
fn render_job(job: &Job, pool: &Arc<MediaPool>, services: &Arc<dyn Services>, previews: &PreviewStore) -> (Rgba, bool) {
    if let Target::Sequence(seq) | Target::SequencePlan(seq) = job.key.target
        && let Some(f) = preview_frame(job, seq, pool, previews)
        && let Some(q) = job.project.sequence(seq)
    {
        let (w, h) = filmcraft_render::output_size(q, job.scale);
        let img = filmcraft_render::Image { w: f.width as usize, h: f.height as usize, px: f.to_linear_f32() };
        let mut img = if img.w == w && img.h == h {
            img
        } else {
            img.transformed(w, h, &filmcraft_geom::Affine::scale(w as f64 / img.w.max(1) as f64, h as f64 / img.h.max(1) as f64))
        };
        if matches!(job.key.target, Target::Sequence(_)) {
            for o in filmcraft_render::caption_overlays(q, job.time, w, h) {
                o.composite_onto(&mut img.px, w, h);
            }
        }
        return (Rgba { w: img.w, h: img.h, px: img.over_black_with_alpha_rgba8() }, true);
    }
    let provider = provider(job, pool, services);
    let opts = filmcraft_render::RenderOptions { scale: job.scale, captions: true, ..Default::default() };
    let img = match job.key.target {
        Target::Sequence(seq) | Target::SequencePlan(seq) | Target::SequenceLive(seq) => {
            Some(filmcraft_render::render_sequence(&job.project, seq, job.time, opts, &provider))
        }
        Target::Item(item) => filmcraft_render::render_item(&job.project, item, job.time, job.scale, &provider),
        Target::MulticamGrid(item, side, page) => {
            let side = (side > 0).then_some(side as usize);
            filmcraft_render::multicam::render_grid_page(&job.project, item, job.time, job.scale, side, page as usize, &provider).map(|(img, _)| img)
        }
        Target::MulticamAngle(item, angle) => {
            filmcraft_render::multicam::render_angle_thumbnail(&job.project, item, angle as usize, job.time, job.scale, &provider)
        }
    };
    let rgba = match img {
        Some(img) => Rgba { w: img.w, h: img.h, px: img.over_black_with_alpha_rgba8() },
        None => Rgba { w: 1, h: 1, px: vec![0, 0, 0, 255] },
    };
    (rgba, false)
}

#[cfg(test)]
mod tests {
    use super::{
        Lookback, PlaybackMeter, SOURCE_BOUND_AHEAD, SampleOrigin, Target, background_work, current_source_samples, keeps_playback_job, lookback_frames,
        playback_plan, playback_span, prefetch_depth, source_cannot_keep_up,
    };
    use filmcraft_project::ItemId;

    #[test]
    fn only_item_thumbnails_are_background_work() {
        let item = Target::Item(ItemId(1));
        // thumbnails (50) and the Media Browser (40)
        assert!(background_work(50, item));
        assert!(background_work(40, item));
        // the Source monitor shows an item at a foreground priority
        assert!(!background_work(1, item));
        // the scopes ask for the program frame at 40 while playing: never background
        assert!(!background_work(40, Target::Sequence(ItemId(1))));
        assert!(!background_work(50, Target::SequencePlan(ItemId(1))));
    }

    #[test]
    fn the_straight_color_table_matches_the_formula() {
        use egui::ecolor::{gamma_u8_from_linear_f32, linear_f32_from_gamma_u8};
        for a in 1..=255u8 {
            for v in 0..=255u8 {
                let want = gamma_u8_from_linear_f32((linear_f32_from_gamma_u8(v) / (f32::from(a) / 255.0)).min(1.0));
                assert_eq!(super::straight_srgb(a, v), want, "a {a} v {v}");
            }
        }
    }

    #[test]
    fn a_frame_goes_up_opaque_or_with_its_alpha() {
        use egui::Color32;
        // opaque red, transparent, and half-transparent white: linear 0.5 over black is sRGB 188
        let img = super::Rgba { w: 3, h: 1, px: vec![255, 0, 0, 255, 0, 0, 0, 0, 188, 188, 188, 128] };
        assert_eq!(img.color_image(false).pixels, [Color32::from_rgb(255, 0, 0), Color32::BLACK, Color32::from_rgb(188, 188, 188)]);
        // egui's colors are the sRGB of the straight color times alpha: white at half is 128
        assert_eq!(img.color_image(true).pixels, [Color32::from_rgb(255, 0, 0), Color32::TRANSPARENT, Color32::from_rgba_premultiplied(128, 128, 128, 128)]);
    }

    #[test]
    fn draft_decoding_only_for_reduced_resolution_playback() {
        use super::draft_playback;
        assert!(draft_playback(true, 0.5, true) && draft_playback(true, 0.25, true));
        assert!(!draft_playback(true, 1.0, true), "full resolution is never draft");
        assert!(!draft_playback(false, 0.5, true), "a paused frame is never draft");
        assert!(!draft_playback(true, 0.5, false), "off unless enabled");
        assert!(!filmcraft_engine::autosave::Preferences::default().playback.draft_decode, "off by default");
    }

    #[test]
    fn source_estimate_ignores_the_queue_and_ages_out_scrubs() {
        use super::{FrameServer, SOURCE_WINDOW};
        let source_estimate = FrameServer::source_estimate;
        // The cost is a low quantile: a queue that stretches most jobs must not stretch the cost
        // (that would feed back into the plan), while the latency is the median and does see it.
        let queued: Vec<(f32, f32)> = (0..SOURCE_WINDOW).map(|i| if i % 8 == 0 { (30.0, 1.0) } else { (900.0, 1.0) }).collect();
        let (cost, latency) = source_estimate(&queued).expect("samples");
        assert!((cost - 0.030).abs() < 1e-6, "cost {cost}");
        assert!(latency > 0.5, "latency {latency}");
        // A burst of scrubs (each seeking from a keyframe, seconds long) ages out: only the last
        // SOURCE_WINDOW jobs count, so playback afterwards is not pinned at "cannot keep up".
        let mut burst: Vec<(f32, f32)> = vec![(4000.0, 1.0); 200];
        burst.extend((0..SOURCE_WINDOW).map(|_| (28.0f32, 1.0f32)));
        let (cost, latency) = source_estimate(&burst).expect("samples");
        assert!((cost - 0.028).abs() < 1e-6 && (latency - 0.028).abs() < 1e-6, "cost {cost}, latency {latency}");
        // Nothing ran yet.
        assert!(source_estimate(&[]).is_none());
    }

    #[test]
    fn playback_plan_skips_frames_only_when_workers_cannot_keep_up() {
        // decode-only / light frames: every frame from the playhead on
        assert_eq!(playback_plan(0.0, 24.0, 6), (1, 1));
        assert_eq!(playback_plan(0.15, 24.0, 6), (1, 1));
        // a serialized source that cannot make the frame rate gets a short queue, never a stride:
        // asking further ahead would only decode more frames before the one that is waited for
        assert!(source_cannot_keep_up(0.030, 50.0));
        assert!(!source_cannot_keep_up(0.012, 50.0));
        assert!(SOURCE_BOUND_AHEAD < prefetch_depth(1.0), "shorter than the render-bound lookahead");
        // 0.5 s of effects per frame at 24 fps on 6 workers: 12 frames pass while one renders,
        // so start 12 ahead, and every 3rd frame is what 6 workers can finish
        assert_eq!(playback_plan(0.5, 24.0, 6), (12, 3));
    }

    #[test]
    fn lookback_is_bounded_for_any_frame_rate() {
        assert_eq!(lookback_frames(None, 50.0), 6);
        // 0.8 s of picture latency at 50 fps: 40 frames; at most a second
        assert_eq!(lookback_frames(Some(0.8), 50.0), 40);
        assert_eq!(lookback_frames(Some(5.0), 50.0), 50);
        assert_eq!(lookback_frames(Some(0.001), 50.0), 2);
        // a zero frame duration (infinite fps), NaN or a negative latency stay bounded
        for (latency, fps) in [(0.8, f64::INFINITY), (0.8, f64::NAN), (f64::NAN, 50.0), (f64::INFINITY, 50.0), (-1.0, 50.0), (0.8, 0.0)] {
            let n = lookback_frames(Some(latency), fps);
            assert!((2..=super::MAX_LOOKBACK).contains(&n), "{latency} s at {fps} fps: {n}");
        }
    }

    #[test]
    fn source_bound_playback_keeps_a_short_queue_and_the_pictures_being_finished() {
        // switching to source-bound: frames queued under the render-bound depth fall outside
        let span = playback_span(1, SOURCE_BOUND_AHEAD, 1, 1, true);
        assert!((1..=SOURCE_BOUND_AHEAD).all(|off| keeps_playback_job(off, 1, span, 0)), "the frames it asks for");
        assert!(!keeps_playback_job(prefetch_depth(1.0), 1, span, 0), "a deep prefetch frame is dropped");
        assert!(keeps_playback_job(prefetch_depth(1.0), 1, playback_span(1, prefetch_depth(1.0), 1, 1, false), 0));
        // behind the playhead: queued jobs go, a running one within the look-back may finish
        assert!(!keeps_playback_job(-2, 1, span, 0));
        assert!(keeps_playback_job(-2, 1, span, 5));
        assert!(!keeps_playback_job(-6, 1, span, 5), "older than the look-back");
        // the look-back, not the (short) forward span, bounds how late a running job may be
        assert!(keeps_playback_job(-10, 1, span, 40) && span < 10);
        // reverse play: "behind" is the other way
        assert!(keeps_playback_job(-1, -1, span, 0) && !keeps_playback_job(1, -1, span, 0));
    }

    #[test]
    fn source_estimate_follows_the_source_being_played() {
        use super::FrameServer;
        let seq = Target::SequencePlan(ItemId(1));
        let (fast, slow) = (Some(ItemId(2)), Some(ItemId(3)));
        // a fast clip played for 31 jobs, then the playhead reached a slow clip of the same
        // sequence (the same target): one slow sample
        let mut recent: Vec<(f32, f32)> = vec![(5.0, 1.0); 31];
        let mut origins: Vec<SampleOrigin> = vec![SampleOrigin { target: seq, source: fast }; 31];
        recent.push((80.0, 1.0));
        origins.push(SampleOrigin { target: seq, source: slow });
        // a thumbnail of another target finished last: it is not the playback source
        recent.push((1.0, 1.0));
        origins.push(SampleOrigin { target: Target::Item(ItemId(9)), source: Some(ItemId(9)) });
        let samples = current_source_samples(&recent, &origins, seq);
        assert_eq!(samples, vec![(80.0, 1.0)], "only the slow clip's samples");
        let (cost, _) = FrameServer::source_estimate(&samples).expect("a sample");
        assert!(source_cannot_keep_up(cost, 50.0), "the slow clip makes playback source-bound at once ({cost} s)");
        // by target alone the fast clip's samples kept the estimate low
        let by_target: Vec<(f32, f32)> = recent.iter().zip(&origins).filter(|(_, o)| o.target == seq).map(|(r, _)| *r).collect();
        let (cost, _) = FrameServer::source_estimate(&by_target).expect("samples");
        assert!(!source_cannot_keep_up(cost, 50.0), "{cost} s");
        // and back on the fast clip, the slow samples stop counting
        recent.push((5.0, 1.0));
        origins.push(SampleOrigin { target: seq, source: fast });
        assert!(current_source_samples(&recent, &origins, seq).iter().all(|r| r.0 == 5.0));
        // nothing for a target that ran no job
        assert!(current_source_samples(&recent, &origins, Target::Item(ItemId(4))).is_empty());
    }

    #[test]
    fn lookback_follows_the_play_direction() {
        // paused or playing forward: earlier frames; playing in reverse: later ones
        assert_eq!(Lookback::backward(3).frames_before(10).collect::<Vec<_>>(), vec![9, 8, 7]);
        assert_eq!(Lookback { frames: 3, dir: -1 }.frames_before(10).collect::<Vec<_>>(), vec![11, 12, 13]);
        assert_eq!(Lookback::backward(0).frames_before(10).count(), 0);
        // a reverse source-bound playback keeps a running job 3 frames above the playhead, and the
        // monitor's look-back reaches it (it used to search below the playhead only)
        let span = playback_span(1, SOURCE_BOUND_AHEAD, 1, 1, true);
        assert!(keeps_playback_job(3, -1, span, 5));
        assert!(Lookback { frames: 5, dir: -1 }.frames_before(100).any(|f| f == 103));
        // hostile sizes stay bounded and never overflow
        assert_eq!(Lookback::backward(i64::MAX).frames_before(0).count() as i64, super::MAX_LOOKBACK);
        assert_eq!(Lookback { frames: -5, dir: 1 }.frames_before(0).count(), 0);
        assert!(Lookback { frames: 3, dir: -1 }.frames_before(i64::MAX).all(|f| f == i64::MAX));
        assert!(Lookback::backward(3).frames_before(i64::MIN).all(|f| f == i64::MIN));
    }

    #[test]
    fn meter_resync_does_not_count_hidden_time() {
        let mut m = PlaybackMeter::default();
        m.start(1.0);
        m.refresh(0, true);
        m.resync(60, false); // window hidden for 2.5 s
        m.refresh(60, true);
        m.refresh(61, true);
        m.finish();
        assert_eq!((m.shown, m.dropped), (2, 0));
    }

    #[test]
    fn meter_counts_frames_not_refreshes() {
        // 24 fps on a 60 Hz display: each frame is due for 2–3 refreshes.
        let mut m = PlaybackMeter::default();
        m.start(1.0);
        for (f, exact) in [(0, true), (0, true), (0, true), (1, false), (1, true), (2, false), (2, false), (3, true)] {
            m.refresh(f, exact);
        }
        m.finish();
        // frame 1 arrived late but was on screen while due → shown; frame 2 never → dropped.
        assert_eq!((m.shown, m.dropped), (3, 1));
    }

    #[test]
    fn meter_counts_skipped_frames_but_not_speed_steps_or_loops() {
        let mut m = PlaybackMeter::default();
        m.start(1.0);
        m.refresh(10, true);
        m.refresh(14, true); // no refresh during 11..13 (UI stalled)
        m.refresh(0, true); // loop restart
        m.finish();
        assert_eq!((m.shown, m.dropped), (3, 3));
        let mut m = PlaybackMeter::default();
        m.start(2.0); // 2× steps over every other frame on purpose
        for f in [0, 2, 4, 6] {
            m.refresh(f, true);
        }
        m.finish();
        assert_eq!((m.shown, m.dropped), (4, 0));
    }

    /// The workers of a dropped server return and release its state (the frame cache, the media
    /// pool, the projects of queued jobs) instead of waiting for jobs for the rest of the process.
    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn dropping_the_server_ends_its_workers() {
        use super::{FrameKey, FrameServer};
        use std::sync::Arc;
        use std::time::{Duration, Instant};

        let s = filmcraft_engine::Session::default();
        let server = FrameServer::new(s.media.clone(), s.services.clone(), s.previews.clone(), 3);
        let p = Arc::new(filmcraft_project::Project::new("Frames"));
        let key = FrameKey { target: Target::Item(ItemId(999)), frame: 0, size: 32, revision: 0, draft: false };
        server.request(key, filmcraft_time::Tick::ZERO, 0.1, &p, 0);
        let (shared, project) = (Arc::downgrade(&server.shared), Arc::downgrade(&p));
        drop(p);
        drop(server);
        let t0 = Instant::now();
        while (shared.strong_count() > 0 || project.strong_count() > 0) && t0.elapsed() < Duration::from_secs(30) {
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(shared.strong_count(), 0, "a worker still holds the dropped server's state");
        assert_eq!(project.strong_count(), 0, "a queued job still holds its project");
    }

    /// A job a worker took from the queue before the server was dropped runs cancelled.
    #[test]
    fn a_job_started_after_the_drop_is_cancelled() {
        use super::{FrameKey, FrameServer, Job, run_job};
        use std::sync::Arc;
        use std::sync::atomic::Ordering;

        let s = filmcraft_engine::Session::default();
        let server = FrameServer::new(s.media.clone(), s.services.clone(), s.previews.clone(), 1);
        let job = Job {
            key: FrameKey { target: Target::Item(ItemId(999)), frame: 0, size: 32, revision: 0, draft: false },
            queued: web_time::Instant::now(),
            cancel: Default::default(),
            prefetch: false,
            catch_up: None,
            time: filmcraft_time::Tick::ZERO,
            scale: 0.1,
            project: Arc::new(filmcraft_project::Project::new("Frames")),
            prio: 0,
        };
        // what `Drop` sets before it cancels the jobs in flight
        server.shared.stop.store(true, Ordering::Relaxed);
        run_job(&server.shared, &job, &server.pool, &server.services, &server.previews);
        assert!(job.cancel.load(Ordering::Relaxed), "the job ran without its cancel flag");
        assert!(!server.is_ready(&job.key), "a cancelled job's frame is not cached");
    }
}
