//! Background frame rendering for monitors, thumbnails and playback prefetch.
//!
//! A small pool of worker threads pulls prioritised jobs (the frame on screen first, then the
//! frames playback will need next, then thumbnails). Results land in a byte-budgeted cache keyed
//! by (target, frame, scale, revision); the UI shows the exact frame when ready and otherwise holds
//! the nearest frame it already has, so scrubbing never flashes black and never blocks the UI.
//! Sequence frames inside a rendered segment come from its render preview instead of the live
//! render (see `filmcraft_engine::previews`).

use std::cell::Cell;
use std::collections::{HashMap, HashSet, VecDeque};
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
    /// A GPU frame plan of a sequence (decoded layers + transforms), planned for and composited on
    /// a device that runs these caps. A plan made for other caps is another frame: it may hold
    /// steps this device would have to composite on the CPU, on the UI thread.
    SequencePlan(ItemId, filmcraft_render::plan::DeviceCaps),
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
    /// While playing: when this frame is due on screen (None: not a playback frame, or the
    /// clock has not started), so a worker can leave out a frame it could not finish in time
    /// ([`starts_too_late`]).
    due: Option<Instant>,
    /// Queued by playback (the frame due now or a prefetch): sources may decode ahead for the
    /// frames to come and drop one they passed (`filmcraft_media::cancel::with_playing`).
    playing: bool,
    /// Asked for by a view beside a running play ([`beside_playback`]): a source playback is
    /// decoding from gives it the frames it holds and nothing else
    /// (`filmcraft_media::cancel::with_beside_playback`).
    beside: bool,
    time: Tick,
    /// Output scale relative to the target's frame size.
    scale: f32,
    project: Arc<Project>,
    prio: u32,
}

struct Shared {
    queue: Mutex<VecDeque<Job>>,
    cv: Condvar,
    /// Upload-only jobs (the frames of deferred plans, [`GpuPlan::deferred`]), for the upload thread: a worker pool busy decoding
    /// the prefetch run would hold an upload behind decodes until the UI thread, needing the
    /// frame, uploaded it itself (with a 30 ms stall when the device grew its staging memory).
    uploads: Mutex<VecDeque<Job>>,
    upload_cv: Condvar,
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
    /// Playback frames a source dropped (a decoder that fell behind left them out): not asked
    /// for again while playing (the monitor shows the nearest frame); cleared when playback
    /// stops, and a paused request decodes them exactly.
    dropped: Mutex<HashSet<FrameKey>>,
    /// A monitor is playing ([`FrameServer::schedule_playback`] until
    /// [`FrameServer::stop_prefetch`]).
    playing: AtomicBool,
    /// Set (under the queue lock) when the [`FrameServer`] is dropped: the workers return.
    stop: AtomicBool,
    /// Uploads GPU plans' frames from the workers (the GPU compositor's, when there is one), so
    /// the UI thread draws them without uploading.
    uploader: Mutex<Option<filmcraft_gpu::Uploader>>,
}

/// Cumulative frame-job statistics (`perf.stats`): cheap enough to keep on all the time.
#[derive(Clone, Debug, Default)]
pub struct FrameStats {
    /// Jobs finished (including cancelled ones).
    pub jobs: u64,
    /// Jobs whose decode/render panicked (the worker survives; the frame is not cached).
    pub failed: u64,
    pub cancelled: u64,
    /// Cancelled jobs whose sources had finished (nothing gave up): their frame was cached.
    pub kept_late: u64,
    /// Playback jobs left out when a worker got to them: they could not have been ready in time
    /// ([`starts_too_late`]).
    pub late_skipped: u64,
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
            "jobs": self.jobs, "cancelled": self.cancelled, "keptLate": self.kept_late, "lateSkipped": self.late_skipped, "failedJobs": self.failed, "previewJobs": self.preview, "prefetchJobs": self.prefetch,
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
}

/// Run `f`, adding its wall/CPU time to this thread's source-time accumulator.
fn timed_source<R>(f: impl FnOnce() -> R) -> R {
    let cpu = || thread_cpu_time().unwrap_or_default();
    let (w0, c0) = (Instant::now(), cpu());
    let r = f();
    let (dw, dc) = (w0.elapsed(), cpu().saturating_sub(c0));
    SOURCE_TIME.with(|s| {
        let (w, c) = s.get();
        s.set((w + dw, c + dc));
    });
    r
}

/// A media source that times its `video_frame` calls (see [`JobRecord::source_wall`]).
struct TimedSource(filmcraft_media::SharedSource);

impl filmcraft_media::MediaSource for TimedSource {
    fn info(&self) -> &filmcraft_media::MediaInfo {
        self.0.info()
    }
    fn video_frame(&self, req: filmcraft_media::FrameRequest) -> filmcraft_media::Result<Arc<filmcraft_frame::VideoFrame>> {
        timed_source(|| self.0.video_frame(req))
    }
    /// The inner source's unturned frame, timed like [`Self::video_frame`].
    fn video_frame_unturned(&self, req: filmcraft_media::FrameRequest) -> filmcraft_media::Result<(Arc<filmcraft_frame::VideoFrame>, u8)> {
        timed_source(|| self.0.video_frame_unturned(req))
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
        Some(Arc::new(TimedSource(s)))
    }
}

/// A GPU frame plan with its layers' texel conversions already done (off the UI thread).
pub struct GpuPlan {
    pub plan: filmcraft_render::plan::FramePlan,
    pub prepared: filmcraft_gpu::PreparedPlan,
    /// Nothing uploaded yet: the frame was a prefetch more than [`UPLOAD_AHEAD`] frames ahead
    /// when it was decoded. A request for it within that distance queues the upload.
    pub deferred: bool,
}

impl GpuPlan {
    /// Bytes this plan keeps alive (layer frames + converted texels).
    fn bytes(&self) -> usize {
        let frames = self.plan.source_bytes();
        frames + self.prepared.bytes()
    }
}

/// Finished plans, bounded by count and bytes.
///
/// Over the budget the least recently used plan goes first (made, or taken for display), but
/// the plans a playing monitor shows and is about to show stay ([`PlayWindow`]), and the cache
/// never drops below `min` plans. While playing, the plans prefetched ahead of the playhead
/// have not been used since they were made, so least-recently-used eviction over the whole
/// cache took exactly the frame due next, and `BUDGET` holds 15 plans of a 4K 4:2:2 10-bit
/// sequence (~66 MB each: the frame and its texture), fewer than the prefetch depth plus the
/// jobs in flight. FX6 XAVC-I playback at 50 fps showed 229 of 400 frames that way, with 93%
/// of them finished before they were due; with the window kept, 352.
///
/// The window is told to the cache, not guessed from which plans were shown. Evicting every
/// shown plan before any plan never shown kept the frames ahead too, but plans never shown
/// collect behind the playhead as well (a frame that finished after it was due, the frames
/// prefetched past the point where the last play stopped). Once those filled the cache, the
/// plan on screen was the only shown one and went at the next insert. Its frame was still due
/// at the next refresh (50 fps on a 60 Hz display shows every fifth frame twice), was missing,
/// and the monitor showed the nearest plan behind it instead: a frame from four to eleven
/// frames earlier, for one refresh (FX3 XAVC S long-GOP: 15 times in a 9 s play).
struct PlanCache {
    map: HashMap<FrameKey, PlanEntry>,
    clock: u64,
    bytes: usize,
    /// Bytes of plans kept at most ([`PlanCache::BUDGET`]; a test's own).
    budget: usize,
    /// Plans kept whatever their bytes: the prefetch window plus the jobs in flight
    /// ([`PlanCache::new`]).
    min: usize,
    /// The frames the playing monitor shows and is about to show, while one plays plans.
    window: Option<PlayWindow>,
}

struct PlanEntry {
    plan: Arc<GpuPlan>,
    /// The clock when the plan was made or last taken for display.
    used: u64,
    bytes: usize,
}

/// How many frames behind the frame due a [`PlayWindow`] reaches: the frame on screen when the
/// frame due is not ready yet, and the one before it.
const WINDOW_BEHIND: i64 = 2;

/// The plans a playing monitor shows and is about to show: the frame due (`key`), the
/// [`WINDOW_BEHIND`] frames before it and the `ahead` frames after it that its prefetch asks
/// for, counted in the direction of play (`dir`: 1, or -1 in reverse).
#[derive(Clone, Copy)]
struct PlayWindow {
    key: FrameKey,
    dir: i64,
    ahead: i64,
}

impl PlayWindow {
    /// How many frames after the frame due `k` plays (negative: before it), when `k` is one of
    /// the window's plans.
    fn offset(&self, k: &FrameKey) -> Option<i64> {
        if (FrameKey { frame: self.key.frame, ..*k }) != self.key {
            return None;
        }
        let d = k.frame.checked_sub(self.key.frame)?.checked_mul(self.dir)?;
        (-WINDOW_BEHIND..=self.ahead).contains(&d).then_some(d)
    }
}

impl PlanCache {
    const MAX: usize = 96;
    const BUDGET: usize = 1 << 30;

    /// A cache that keeps at least the deepest prefetch window plus the `workers` jobs that
    /// may finish while it is full (and a couple of frames of slack).
    fn new(workers: usize) -> Self {
        let min = (prefetch_depth(2.0) as usize).saturating_add(workers).saturating_add(2).min(Self::MAX);
        PlanCache { map: HashMap::new(), clock: 0, bytes: 0, budget: Self::BUDGET, min, window: None }
    }

    fn get(&mut self, k: &FrameKey) -> Option<Arc<GpuPlan>> {
        self.clock += 1;
        let clock = self.clock;
        self.map.get_mut(k).map(|e| {
            e.used = clock;
            e.plan.clone()
        })
    }

    fn insert(&mut self, k: FrameKey, p: GpuPlan) {
        self.clock += 1;
        let bytes = p.bytes();
        self.bytes = self.bytes.saturating_add(bytes);
        if let Some(old) = self.map.insert(k, PlanEntry { plan: Arc::new(p), used: self.clock, bytes }) {
            self.bytes = self.bytes.saturating_sub(old.bytes);
        }
        if self.map.len() > Self::MAX || self.bytes > self.budget {
            let window = self.window;
            let mut v: Vec<(FrameKey, u64, Option<i64>, usize)> =
                self.map.iter().map(|(k, e)| (*k, e.used, window.and_then(|w| w.offset(k)), e.bytes)).collect();
            sort_for_eviction(&mut v);
            for (k, _, _, b) in v {
                if self.map.len() > Self::MAX || (self.bytes > self.budget && self.map.len() > self.min) {
                    self.map.remove(&k);
                    self.bytes = self.bytes.saturating_sub(b);
                } else {
                    break;
                }
            }
        }
    }
}

/// Order `(key, used, offset in the play window, bytes)` entries for eviction (see
/// [`PlanCache`]): the plans outside the window first, least recently used first, then the
/// window's own from the one due last.
fn sort_for_eviction<K>(v: &mut [(K, u64, Option<i64>, usize)]) {
    v.sort_unstable_by_key(|&(_, used, offset, _)| match offset {
        None => (0u8, used, 0i64),
        Some(d) => (1, 0, d.saturating_neg()),
    });
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

struct FrameRequest<'a> {
    key: FrameKey,
    time: Tick,
    scale: f32,
    project: &'a Arc<Project>,
    priority: u32,
    prefetch: bool,
    catch_up: Option<Tick>,
    due: Option<Instant>,
    playing: bool,
    beside: bool,
}

/// Whether a frame asked for at `prio` outside playback scheduling is for a view beside a play
/// (`play_on`: a monitor is playing): the Video Scopes and the reference monitor following the
/// playhead, thumbnails. The frame on screen of a paused monitor (`prio` 0) is not: it is
/// decoded exactly, whatever plays elsewhere.
fn beside_playback(prio: u32, play_on: bool) -> bool {
    play_on && prio > 0
}

/// What a queued job is (`playing`, `beside`) once the same frame is asked for again: the
/// newest request says whether it is playback (a paused monitor asking for the frame a play
/// left queued gets it decoded exactly, not dropped as a late playback frame), except that a
/// view beside the play never changes what the job is.
fn merged_kind(job: (bool, bool), request: (bool, bool)) -> (bool, bool) {
    let (playing, beside) = request;
    (if beside { job.0 } else { playing }, job.1 && beside)
}

/// Frames queued ahead of the playhead while playing (and how far ahead a queued job may be
/// before it is dropped as stale).
pub fn prefetch_depth(speed: f64) -> i64 {
    if speed.abs() > 1.5 { 24 } else { 14 }
}

/// Prefetched frames are uploaded to the GPU only once they are within this many frames of
/// the playhead; further ahead they wait, decoded, in the plan cache. Uploads share the GPU's
/// queue with the monitors' drawing and presenting: when the workers uploaded every frame as
/// soon as it was decoded, the start of playback queued the whole prefetch run at once (at
/// 4K 10-bit, 14 or more frames of 33 MB) and the UI thread waited 100-150 ms for its next
/// swapchain image behind those copies, dropping the first frames of every play. Four frames
/// (130 MB) copy in well under a frame's time, and after the start one frame enters the
/// window per frame shown.
pub const UPLOAD_AHEAD: u32 = 4;

/// Whether a finished playback job uploads its frame now: not a prefetch more than
/// [`UPLOAD_AHEAD`] frames ahead when there is an uploader (without one there is nothing to
/// wait for: the UI thread uploads when it draws).
pub fn upload_now(has_uploader: bool, prefetch: bool, frames_ahead: u32) -> bool {
    !has_uploader || !prefetch || frames_ahead <= UPLOAD_AHEAD
}

/// Playback starts its clock once this many frames from the playhead are ready, or after
/// [`PREROLL_TIMEOUT_S`]: otherwise the first frames after Play are due before any decoder had a
/// chance to produce them (a seek decodes from the previous keyframe).
pub const PREROLL_FRAMES: i64 = 6;

/// Seconds before a scrubbed-to frame whose frames a seek still decodes in full (see
/// [`FrameServer::request`]); earlier non-reference pictures are skipped on the way.
pub const SCRUB_KEEP_S: f64 = 2.0;
pub const PREROLL_TIMEOUT_S: f64 = 0.5;
/// ... unless the first frames are still being made ([`FrameServer::preroll_pending`]): a
/// long-GOP decoder's first pictures take a while (a seek to the sync sample and the frames in
/// flight of a frame-threaded decoder) and starting the clock before they come leaves playback
/// behind from the first frame. The preroll then ends at this many seconds at the latest.
pub const PREROLL_MAX_S: f64 = 2.5;
/// A preroll whose frames are ready still waits for the GPU's clocks to come up
/// ([`filmcraft_gpu::GpuCompositor::wake_warm`]), at most this long after Play: a discrete GPU
/// that idled between plays took the first eight frames or so to get there, and ran a frame's
/// copies and composite in 10-30 ms until then.
pub const WAKE_MAX_S: f64 = 0.5;

/// How many frames behind the playhead a playing monitor looks for a ready picture when the
/// frame due is not: a sequential (long-GOP) decoder that falls behind delivers its frames a
/// little late, and the latest of them is the picture to show; a few frames further than the
/// late frames a decoder near the playhead produces (`NEAR_SAMPLES` in the GOP cache).
pub const PLAYING_LOOK_BACK: i64 = 12;

/// How many revisions behind the project a playing monitor's picture may be while the frame due
/// is not ready at the current one (#1673). A value dragged during playback makes a revision per
/// UI pass, so no frame was ever ready at the current revision and the picture froze until the
/// mouse rested; frames of the last few changes are what the monitor shows meanwhile.
pub const PLAYING_REVISION_LAG: u64 = 64;

/// The frames [`FrameServer::recent`] looks for, in order: nearest frame first (`key.frame`, then
/// the ones already played: lower frames, or higher ones when `dir` < 0), newest revision first.
fn recent_keys(key: FrameKey, dir: i64, max_back: i64, lag: u64) -> impl Iterator<Item = FrameKey> {
    let back = if dir < 0 { -1 } else { 1 };
    (0..=max_back.max(0))
        .flat_map(move |d| (0..=lag.min(key.revision)).map(move |r| FrameKey { frame: key.frame - d * back, revision: key.revision - r, ..key }))
}

/// Which frames playback asks for, given the per-frame render cost (s), the displayed frame rate
/// and the worker count: `(lead, stride)`. While the workers keep up, every frame from the
/// playhead on (`(1, 1)`). When they cannot (CPU effects slower than real time), `lead` is the
/// first frame ahead of the playhead worth starting (nearer ones would be late however soon they
/// start) and `stride` the spacing the workers can finish in real time: the frames in between
/// are dropped on purpose and evenly, instead of every frame arriving late.
pub fn playback_plan(render_cost: f64, fps: f64, workers: usize) -> (i64, i64) {
    let frames_per_job = render_cost * fps;
    let load = frames_per_job * 1.15 / workers.max(1) as f64;
    if load <= 1.0 {
        return (1, 1);
    }
    (frames_per_job.ceil() as i64, load.ceil() as i64)
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
            uploads: Mutex::new(VecDeque::new()),
            upload_cv: Condvar::new(),
            done: Mutex::new(Cache { map: HashMap::new(), bytes: 0, budget: 768 << 20, clock: 0 }),
            plans: Mutex::new(PlanCache::new(workers)),
            in_flight: Mutex::new(Vec::new()),
            profiling: AtomicBool::new(false),
            records: Mutex::new(Vec::new()),
            render_cost: Mutex::new(0.0),
            workers: workers.max(1),
            stats: Mutex::new(FrameStats::default()),
            stop: AtomicBool::new(false),
            dropped: Mutex::new(HashSet::new()),
            uploader: Mutex::new(None),
            playing: AtomicBool::new(false),
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
        #[cfg(not(target_arch = "wasm32"))]
        {
            let sh = shared.clone();
            let rp = repaint.clone();
            std::thread::Builder::new().name("filmcraft-upload".into()).spawn(move || upload_worker(sh, rp)).ok();
        }
        #[cfg(target_arch = "wasm32")]
        let _ = workers;
        Self { shared, pool, services, previews, repaint }
    }

    /// The default number of frame workers for this machine.
    pub fn default_workers() -> usize {
        std::thread::available_parallelism().map(|n| n.get().clamp(2, 6)).unwrap_or(3)
    }

    /// The uploader the workers put GPU plans' frames on the device with (None: the UI thread
    /// uploads when it draws). Plans already finished keep their converted texels.
    pub fn set_uploader(&self, up: Option<filmcraft_gpu::Uploader>) {
        *self.shared.uploader.lock().unwrap_or_else(PoisonError::into_inner) = up;
    }

    pub fn set_context(&self, ctx: &egui::Context) {
        let mut g = self.repaint.lock().unwrap_or_else(PoisonError::into_inner);
        if g.is_none() {
            let ctx = ctx.clone();
            *g = Some(Box::new(move || ctx.request_repaint()));
        }
    }

    /// Whether finished jobs wake the UI ([`Self::set_context`] has run).
    pub fn wakes_ui(&self) -> bool {
        self.repaint.lock().unwrap_or_else(|e| e.into_inner()).is_some()
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
            Target::SequencePlan(..) => self.shared.plans.lock().unwrap_or_else(PoisonError::into_inner).map.contains_key(k),
            _ => self.shared.done.lock().unwrap_or_else(PoisonError::into_inner).map.contains_key(k),
        }
    }

    /// Nearest cached plan at or before `frame`.
    pub fn nearest_plan(&self, key: FrameKey, max_back: i64) -> Option<(FrameKey, Arc<GpuPlan>)> {
        let mut g = self.shared.plans.lock().unwrap_or_else(PoisonError::into_inner);
        (0..=max_back).find_map(|d| {
            let k = FrameKey { frame: key.frame - d, ..key };
            g.get(&k).map(|p| (k, p))
        })
    }

    /// While playing: the cached frame closest to `key.frame` on the side already played (within
    /// `max_back` frames; `dir` -1 plays in reverse, so higher frames lie behind) at the newest
    /// revision from `key.revision` back to `lag` revisions before it, image or plan as
    /// `key.target` renders (#1673). `None` when there is none.
    pub fn recent(&self, key: FrameKey, dir: i64, max_back: i64, lag: u64) -> Option<FrameKey> {
        let keys = recent_keys(key, dir, max_back, lag);
        match key.target {
            Target::SequencePlan(..) => {
                let g = self.shared.plans.lock().unwrap_or_else(|e| e.into_inner());
                keys.into_iter().find(|k| g.map.contains_key(k))
            }
            _ => {
                let c = self.shared.done.lock().unwrap_or_else(|e| e.into_inner());
                keys.into_iter().find(|k| c.map.contains_key(k))
            }
        }
    }

    /// Queue a job unless it is cached, queued or in flight.
    ///
    /// The frame on screen (`prio` 0: scrubbing, a seek) treats frames more than
    /// [`SCRUB_KEEP_S`] before it as not wanted: a decoder seeking from a keyframe skips their
    /// non-reference pictures, while nearby frames stay decoded (and cached) for scrubbing back.
    pub fn request(&self, key: FrameKey, time: Tick, scale: f32, project: &Arc<Project>, prio: u32) {
        let late = (prio == 0).then(|| Tick::from_seconds_f64(SCRUB_KEEP_S));
        let beside = beside_playback(prio, self.shared.playing.load(Ordering::Relaxed));
        self.request_job(FrameRequest { key, time, scale, project, priority: prio, prefetch: false, catch_up: late, due: None, playing: false, beside });
    }

    fn request_job(&self, request: FrameRequest<'_>) {
        let FrameRequest { key, time, scale, project, priority: prio, prefetch, catch_up, due, playing, beside } = request;
        let hit = || self.shared.stats.lock().unwrap_or_else(PoisonError::into_inner).request_hits += 1;
        let deferred = self.shared.plans.lock().unwrap_or_else(PoisonError::into_inner).map.get(&key).map(|e| e.plan.deferred);
        if self.shared.done.lock().unwrap_or_else(PoisonError::into_inner).map.contains_key(&key) || deferred == Some(false) {
            hit();
            return;
        }
        // a decoded frame whose upload waits: queue it once the playhead is close enough
        if deferred == Some(true) {
            if !upload_now(true, prefetch, prio) {
                hit();
                return;
            }
            if self.shared.in_flight.lock().unwrap_or_else(PoisonError::into_inner).iter().any(|(k, c, _)| *k == key && !c.load(Ordering::Relaxed)) {
                hit();
                return;
            }
            let mut q = self.shared.uploads.lock().unwrap_or_else(PoisonError::into_inner);
            if let Some(j) = q.iter_mut().find(|j| j.key == key) {
                j.prio = j.prio.min(prio);
            } else {
                q.push_back(Job {
                    key,
                    queued: Instant::now(),
                    cancel: Default::default(),
                    prefetch,
                    catch_up,
                    due,
                    playing,
                    beside,
                    time,
                    scale,
                    project: project.clone(),
                    prio,
                });
            }
            drop(q);
            self.shared.upload_cv.notify_one();
            return;
        }
        if self.shared.in_flight.lock().unwrap_or_else(PoisonError::into_inner).iter().any(|(k, c, _)| *k == key && !c.load(Ordering::Relaxed)) {
            hit();
            return;
        }
        if playing && self.shared.dropped.lock().unwrap_or_else(PoisonError::into_inner).contains(&key) {
            return;
        }
        let mut q = self.shared.queue.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(j) = q.iter_mut().find(|j| j.key == key) {
            j.prio = j.prio.min(prio);
            j.prefetch &= prefetch;
            (j.playing, j.beside) = merged_kind((j.playing, j.beside), (playing, beside));
            // the playhead only moves on: the newest (smallest) margin
            j.catch_up = match (j.catch_up, catch_up) {
                (Some(a), Some(b)) => Some(a.min(b)),
                (a, b) => a.or(b),
            };
            // the newest estimate of when the frame is due
            if due.is_some() {
                j.due = due;
            }
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
        q.push_back(Job {
            key,
            queued: Instant::now(),
            cancel: Default::default(),
            prefetch,
            catch_up,
            due,
            playing,
            beside,
            time,
            scale,
            project: project.clone(),
            prio,
        });
        drop(q);
        self.shared.cv.notify_one();
    }

    /// Playback scheduling for the monitor showing `key` (the frame due now): request it first,
    /// then the next frames in display order, and drop queued jobs for this target that are
    /// stale (old revision, behind the playhead or too far ahead).
    ///
    /// When frames cost more than the workers can finish at the frame rate (CPU effects, a
    /// source that decodes on the worker), frames that could not be ready in time are not asked
    /// for and the workers get evenly spaced frames they can finish ([`playback_plan`]); a queued
    /// frame that can no longer be ready when a worker gets to it is left out there
    /// ([`starts_too_late`]). While `preroll` (clock not started yet) every frame is wanted.
    pub fn schedule_playback(&self, key: FrameKey, rate: filmcraft_time::FrameRate, scale: f32, project: &Arc<Project>, speed: f64, preroll: bool) {
        let dir = if speed < 0.0 { -1 } else { 1 };
        let step = speed.abs().max(1.0) as i64;
        let fps = rate.as_f64() * speed.abs().max(1.0) / step as f64;
        let (lead, stride) = if preroll { (1, 1) } else { playback_plan(self.render_cost(), fps, self.shared.workers) };
        self.shared.playing.store(true, Ordering::Relaxed);
        if matches!(key.target, Target::SequencePlan(..)) {
            // the plans this play shows next are the last the plan cache gives up ([`PlayWindow`])
            let ahead = prefetch_depth(speed).saturating_mul(stride).saturating_add(lead).saturating_mul(step);
            self.shared.plans.lock().unwrap_or_else(PoisonError::into_inner).window = Some(PlayWindow { key, dir, ahead });
        }
        if lead <= 1 {
            // the frame due now; once the clock runs, the frames before it are late
            self.request_job(FrameRequest {
                key,
                time: rate.tick_of(key.frame),
                scale,
                project,
                priority: 0,
                prefetch: false,
                catch_up: (!preroll && dir > 0).then_some(Tick::ZERO),
                due: None,
                playing: true,
                beside: false,
            });
        }
        // During preroll the clock has not started: only the frames that gate start, not a
        // prefetch run that walks the rest of a LongGOP and evicts the playhead pictures. Once
        // those are ready the run starts (the preroll may go on for the GPU's sake,
        // [`WAKE_MAX_S`]): the frame after them is due [`PREROLL_FRAMES`] frames into the play,
        // and asked for only then it was the one frame a play showed late.
        let ahead = if preroll && !self.preroll_ready(key, speed, i64::MAX) { (PREROLL_FRAMES - 1).max(0) } else { prefetch_depth(speed) };
        let now = Instant::now();
        for i in 0..ahead {
            let f = key.frame + (lead + i * stride) * dir * step;
            if f >= 0 {
                // forward play: frames before the playhead are late (reverse play skips nothing)
                let late = (!preroll && dir > 0).then(|| rate.tick_of(f) - rate.tick_of(key.frame));
                // when the frame is due: (lead + i * stride) displayed frames from now
                let due = (!preroll && fps > 0.0)
                    .then(|| (lead + i * stride) as f64 / fps)
                    .and_then(|secs| Duration::try_from_secs_f64(secs).ok())
                    .and_then(|d| now.checked_add(d));
                self.request_job(FrameRequest {
                    key: FrameKey { frame: f, ..key },
                    time: rate.tick_of(f),
                    scale,
                    project,
                    priority: i as u32 + 1,
                    prefetch: true,
                    catch_up: late,
                    due,
                    playing: true,
                    beside: false,
                });
            }
        }
        let (target, rev, cur) = (key.target, key.revision, key.frame);
        let span = (lead + ahead * stride) * step + 26;
        let ahead_of = |k: &FrameKey| (k.frame - cur) * dir >= 0 && (k.frame - cur).abs() < span;
        // Queued jobs of an older revision are dropped, but one already rendering a frame still
        // ahead is left to finish (#1673): a value dragged during playback asks for a new
        // revision on every pass, and cancelling those jobs each time left nothing ever ready.
        self.retain_jobs(
            |k| k.target != target || (k.revision == rev && ahead_of(k)),
            |k| k.target != target || (k.revision.saturating_add(PLAYING_REVISION_LAG) >= rev && k.revision <= rev && ahead_of(k)),
        );
    }

    /// Whether playback can start at `key`: the next [`PREROLL_FRAMES`] frames (up to `last`)
    /// are ready.
    pub fn preroll_ready(&self, key: FrameKey, speed: f64, last: i64) -> bool {
        let step = (speed.abs().max(1.0) as i64) * if speed < 0.0 { -1 } else { 1 };
        (0..PREROLL_FRAMES).map(|i| key.frame + i * step).filter(|f| *f >= 0 && *f <= last).all(|f| self.is_ready(&FrameKey { frame: f, ..key }))
    }

    /// Whether a job for `k` is queued or running (and still wanted).
    pub fn is_pending(&self, k: &FrameKey) -> bool {
        self.shared.queue.lock().unwrap_or_else(PoisonError::into_inner).iter().any(|j| j.key == *k)
            || self.shared.in_flight.lock().unwrap_or_else(PoisonError::into_inner).iter().any(|(key, cancel, _)| key == k && !cancel.load(Ordering::Relaxed))
    }

    /// Whether any of the frames [`Self::preroll_ready`] waits for is still being made: the
    /// preroll may wait for it up to [`PREROLL_MAX_S`].
    pub fn preroll_pending(&self, key: FrameKey, speed: f64, last: i64) -> bool {
        let step = (speed.abs().max(1.0) as i64) * if speed < 0.0 { -1 } else { 1 };
        (0..PREROLL_FRAMES).map(|i| key.frame + i * step).filter(|f| *f >= 0 && *f <= last).any(|f| self.is_pending(&FrameKey { frame: f, ..key }))
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

    /// [`Self::retain_queue`] with one test for queued jobs and another for running ones.
    fn retain_jobs(&self, keep_queued: impl Fn(&FrameKey) -> bool, keep_running: impl Fn(&FrameKey) -> bool) {
        self.shared.queue.lock().unwrap_or_else(PoisonError::into_inner).retain(|j| keep_queued(&j.key));
        self.shared.uploads.lock().unwrap_or_else(PoisonError::into_inner).retain(|j| keep_queued(&j.key));
        for (k, c, _) in self.shared.in_flight.lock().unwrap_or_else(PoisonError::into_inner).iter() {
            if !keep_running(k) {
                c.store(true, Ordering::Relaxed);
            }
        }
    }

    /// A play starts (Play, or a looping play going round): the frames a source dropped in the
    /// play before are asked for again. Their keys are the same on every round of a loop, and a
    /// frame the decoder left out once would otherwise be missing from each round after it.
    pub fn play_starts(&self) {
        self.shared.dropped.lock().unwrap_or_else(PoisonError::into_inner).clear();
    }

    /// Playback stopped: drop its queued prefetch jobs and cancel the running ones (left alone,
    /// they would keep the workers busy, e.g. rendering effects for frames nobody will see).
    pub fn stop_prefetch(&self) {
        self.shared.playing.store(false, Ordering::Relaxed);
        self.shared.dropped.lock().unwrap_or_else(PoisonError::into_inner).clear();
        self.shared.plans.lock().unwrap_or_else(PoisonError::into_inner).window = None;
        self.shared.queue.lock().unwrap_or_else(PoisonError::into_inner).retain(|j| !j.prefetch);
        self.shared.uploads.lock().unwrap_or_else(PoisonError::into_inner).retain(|j| !j.prefetch);
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
        self.shared.uploads.lock().unwrap_or_else(PoisonError::into_inner).clear();
        for (_, c, _) in self.shared.in_flight.lock().unwrap_or_else(PoisonError::into_inner).iter() {
            c.store(true, Ordering::Relaxed);
        }
        self.shared.cv.notify_all();
        self.shared.upload_cv.notify_all();
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
/// The upload thread: puts the frames of plans decoded earlier on the GPU ([`GpuPlan::deferred`],
/// [`UPLOAD_AHEAD`]), highest priority first, and wakes the UI for each.
#[cfg(not(target_arch = "wasm32"))]
fn upload_worker(sh: Arc<Shared>, repaint: Arc<Mutex<Option<Box<dyn Fn() + Send + Sync>>>>) {
    loop {
        let job = {
            let mut q = sh.uploads.lock().unwrap_or_else(PoisonError::into_inner);
            loop {
                if sh.stop.load(Ordering::Relaxed) {
                    return;
                }
                let best = q.iter().enumerate().min_by_key(|(i, j)| (j.prio, *i)).map(|(i, _)| i);
                if let Some(job) = best.and_then(|i| q.remove(i)) {
                    break job;
                }
                q = sh.upload_cv.wait(q).unwrap_or_else(PoisonError::into_inner);
            }
        };
        sh.in_flight.lock().unwrap_or_else(PoisonError::into_inner).push((job.key, job.cancel.clone(), job.prefetch));
        let ran = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| run_upload(&sh, &job)));
        sh.in_flight.lock().unwrap_or_else(PoisonError::into_inner).retain(|(k, c, _)| !(*k == job.key && Arc::ptr_eq(c, &job.cancel)));
        if ran.is_err() {
            sh.stats.lock().unwrap_or_else(PoisonError::into_inner).failed += 1;
            log::error!("upload job {:?} panicked; the upload thread continues", job.key);
        }
        if let Some(f) = repaint.lock().unwrap_or_else(PoisonError::into_inner).as_ref() {
            f();
        }
    }
}

/// Upload the frames of the cached plan of `job.key` (nothing when it is gone or already up).
fn run_upload(sh: &Shared, job: &Job) {
    let plan = sh.plans.lock().unwrap_or_else(PoisonError::into_inner).map.get(&job.key).filter(|e| e.plan.deferred).map(|e| e.plan.clone());
    if let Some(p) = plan.filter(|_| !job.cancel.load(Ordering::Relaxed)) {
        let up = sh.uploader.lock().unwrap_or_else(PoisonError::into_inner).clone();
        let prepared = filmcraft_gpu::prepare_ahead(&p.plan, up.as_ref(), job.prio);
        sh.plans.lock().unwrap_or_else(PoisonError::into_inner).insert(job.key, GpuPlan { plan: p.plan.clone(), prepared, deferred: false });
    }
    sh.stats.lock().unwrap_or_else(PoisonError::into_inner).jobs += 1;
}

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
                        // A playback frame that cannot be ready by the time it is due would only
                        // be cancelled once finished: this worker goes on to one it can finish.
                        let cost = Duration::from_secs_f64(*sh.render_cost.lock().unwrap_or_else(PoisonError::into_inner));
                        if job.prefetch && starts_too_late(job.due, Instant::now(), cost) {
                            sh.stats.lock().unwrap_or_else(PoisonError::into_inner).late_skipped += 1;
                            continue;
                        }
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

/// Whether a job's result is the frame and goes into the cache: not when a source gave up on
/// it (a layer missing, a decode given up or a late frame dropped on purpose), and not when it
/// was cancelled because the server is stopping. A job cancelled for another reason (playback
/// moved on) whose sources all finished has the frame: it is cached for the next request.
fn keep_result(cancelled: bool, gave_up: bool, stopping: bool) -> bool {
    !gave_up && !(cancelled && stopping)
}

/// Whether a playback job taken from the queue `now` would finish after its frame is `due`, at
/// the workers' current per-frame `cost`. Workers take the nearest frame first; when that frame
/// can no longer be on time, starting it only has it cancelled once finished, while a frame
/// further ahead could still have been ready. Left unchecked, every worker was busy on frames
/// that were already lost and nothing reached the screen once a picture cost more than the
/// frame interval (a 4K intra picture decoded on the worker: ~170 ms at 50 fps).
fn starts_too_late(due: Option<Instant>, now: Instant, cost: Duration) -> bool {
    due.is_some_and(|due| due.saturating_duration_since(now) < cost)
}

/// The work of one frame job that occupied its worker, for [`FrameServer::render_cost`]: the
/// job's wall time outside its source calls (compositing, CPU effects) plus the CPU time the
/// worker itself spent inside them. A source that decodes the frame on the calling thread (an
/// intra-only stream: every frame worker decodes a different picture at once) is work the
/// workers can only finish at `workers / cost` frames per second, so playback must skip frames
/// evenly once that is below the frame rate; without it, every job was cancelled just before
/// its picture came out and nothing was shown. Waiting inside a source call, for a decoder's own
/// threads or for another worker's turn at a sequential decoder, is left out: skipping frames
/// saves none of it.
fn job_cost(job_wall: Duration, source_wall: Duration, source_cpu: Duration) -> Duration {
    job_wall.saturating_sub(source_wall) + source_cpu.min(source_wall)
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
    // A cancelled job whose source gave up (`cancel::gave_up`) may have missed layers: its
    // result is not cached. One cancelled after its sources had finished is complete and is
    // cached: playback that fell behind shows it as the nearest frame, and decoding it again
    // would cost the same (unless the server is stopping). A job whose media bytes were still
    // loading (web) rendered with missing layers: like a cancelled one, its result is not
    // cached (the caller retries it).
    let _ = filmcraft_media::pending::take();
    let mut loading = false;
    let mut kept_late = false;
    let mut gave_up = false;
    let complete = |cancel: &std::sync::atomic::AtomicBool, kept: &mut bool, gave_up: &mut bool| {
        let cancelled = cancel.load(Ordering::Relaxed);
        *gave_up = filmcraft_media::cancel::gave_up();
        let keep = keep_result(cancelled, *gave_up, sh.stop.load(Ordering::Relaxed));
        *kept = keep && cancelled;
        keep
    };
    let preview = filmcraft_media::cancel::with_beside_playback(job.beside, || {
        filmcraft_media::cancel::with_cancel(&job.cancel, || {
            filmcraft_media::cancel::with_playing(job.playing, || {
                filmcraft_media::cancel::with_catch_up(job.catch_up, || {
                    filmcraft_media::cancel::with_draft(job.key.draft, || {
                        filmcraft_media::cancel::with_background(is_background(job), || {
                            if let Target::SequencePlan(seq, caps) = job.key.target {
                                let (plan, pv) = plan_job(job, seq, caps, pool, services, previews);
                                loading = filmcraft_media::pending::take();
                                if !loading && complete(&job.cancel, &mut kept_late, &mut gave_up) {
                                    // Upload (or convert) texels here, not on the UI thread when the frame is shown;
                                    // a prefetch far ahead of the playhead waits for its upload ([`UPLOAD_AHEAD`]).
                                    let up = sh.uploader.lock().unwrap_or_else(PoisonError::into_inner).clone();
                                    let now = upload_now(up.is_some(), job.prefetch, job.prio);
                                    let prepared = if now { filmcraft_gpu::prepare_ahead(&plan, up.as_ref(), job.prio) } else { Default::default() };
                                    sh.plans.lock().unwrap_or_else(PoisonError::into_inner).insert(job.key, GpuPlan { plan, prepared, deferred: !now });
                                }
                                pv
                            } else {
                                let (img, pv) = render_job(job, pool, services, previews);
                                loading = filmcraft_media::pending::take();
                                if !loading && complete(&job.cancel, &mut kept_late, &mut gave_up) {
                                    sh.done.lock().unwrap_or_else(PoisonError::into_inner).insert(job.key, Arc::new(img));
                                }
                                pv
                            }
                        })
                    })
                })
            })
        })
    });
    let cancelled = job.cancel.load(Ordering::Relaxed);
    sh.in_flight.lock().unwrap_or_else(PoisonError::into_inner).retain(|(k, c, _)| !(*k == job.key && Arc::ptr_eq(c, &job.cancel)));
    if job.playing && !loading && !cancelled && gave_up {
        let mut dropped = sh.dropped.lock().unwrap_or_else(PoisonError::into_inner);
        if dropped.len() >= 1024 {
            dropped.clear();
        }
        dropped.insert(job.key);
    }
    if job.prefetch {
        // A cancelled job only tells that the work takes at least this long.
        let (source_wall, source_cpu) = SOURCE_TIME.with(|s| s.get());
        let cost = job_cost(started.elapsed(), source_wall, source_cpu).as_secs_f64();
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
        st.kept_late += kept_late as u64;
        st.preview += preview as u64;
        st.prefetch += job.prefetch as u64;
        st.source_ms += src_ms;
        st.render_ms += render_ms;
        st.job_ms += job_wall.as_secs_f64() * 1e3;
        if st.recent.len() >= FrameStats::RECENT {
            st.recent.pop_front();
        }
        st.recent.push_back((src_ms as f32, render_ms as f32));
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

fn plan_job(
    job: &Job,
    seq: ItemId,
    caps: filmcraft_render::plan::DeviceCaps,
    pool: &Arc<MediaPool>,
    services: &Arc<dyn Services>,
    previews: &PreviewStore,
) -> (filmcraft_render::plan::FramePlan, bool) {
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
    (filmcraft_render::plan::plan_frame(&job.project, seq, job.time, opts, caps, &provider), false)
}

/// Render a job to RGBA8; the flag says whether it came from a render preview.
fn render_job(job: &Job, pool: &Arc<MediaPool>, services: &Arc<dyn Services>, previews: &PreviewStore) -> (Rgba, bool) {
    if let Target::Sequence(seq) | Target::SequencePlan(seq, _) = job.key.target
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
        Target::Sequence(seq) | Target::SequencePlan(seq, _) | Target::SequenceLive(seq) => {
            let (w, h, px) = filmcraft_render::render_sequence_rgba8(&job.project, seq, job.time, opts, &provider);
            return (Rgba { w, h, px }, false);
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
    #[test]
    fn plan_eviction_spares_the_play_window() {
        // 1..=3 lie outside the window (3 used most recently); 4 is on screen, 5 is due and
        // 6..=8 are ahead of it, all made before 1..=3 were last used: the ones outside go
        // first, least recently used first, then the window's from the one due last
        let mut v = vec![
            (4u32, 14u64, Some(-1i64), 1usize),
            (1, 21, None, 1),
            (8, 18, Some(3), 1),
            (2, 22, None, 1),
            (6, 16, Some(1), 1),
            (3, 23, None, 1),
            (5, 15, Some(0), 1),
            (7, 17, Some(2), 1),
        ];
        super::sort_for_eviction(&mut v);
        assert_eq!(v.iter().map(|e| e.0).collect::<Vec<_>>(), [1, 2, 3, 8, 7, 6, 5, 4]);
    }

    fn plan_key(frame: i64) -> FrameKey {
        FrameKey { target: Target::SequencePlan(ItemId(1), filmcraft_render::plan::DeviceCaps::NONE), frame, size: 1000, revision: 0, draft: false }
    }

    fn small_plan() -> super::GpuPlan {
        super::GpuPlan { plan: filmcraft_render::plan::FramePlan::Image(filmcraft_render::Image::new(2, 2)), prepared: Default::default(), deferred: false }
    }

    /// A plan cache over its budget from the first plan, so that only `min` and the play window
    /// decide what stays.
    fn full_plan_cache(workers: usize) -> super::PlanCache {
        super::PlanCache { budget: 1, ..super::PlanCache::new(workers) }
    }

    /// The monitor of a play at 1x takes the frame `due` for a refresh; whether it was there.
    fn shown(cache: &mut super::PlanCache, due: i64) -> bool {
        cache.window = Some(super::PlayWindow { key: plan_key(due), dir: 1, ahead: super::prefetch_depth(1.0) + 1 });
        cache.get(&plan_key(due)).is_some()
    }

    /// What reaches the cache between two refreshes of that play: the frames ahead of `due`, up
    /// to the prefetch depth, that are not there yet, and one of them a second time (a frame
    /// that was made again).
    fn deliver(cache: &mut super::PlanCache, due: i64) {
        for f in due + 1..=due + super::prefetch_depth(1.0) {
            if !cache.map.contains_key(&plan_key(f)) {
                cache.insert(plan_key(f), small_plan());
            }
        }
        cache.insert(plan_key(due + 4), small_plan());
    }

    /// Play frames `from..to` at 50 fps on a 60 Hz display at its worst: every frame is due for
    /// two refreshes, and must be there at both.
    fn play(cache: &mut super::PlanCache, from: i64, to: i64) {
        for due in from..to {
            assert!(shown(cache, due), "frame {due} was not there when due");
            deliver(cache, due);
            assert!(shown(cache, due), "frame {due} was gone at its second refresh");
            deliver(cache, due);
        }
    }

    /// The frames prefetched past the point where a play stopped are never shown. A later play
    /// used to lose the plan on screen to them: every shown plan was evicted before any unshown
    /// one, so with the cache full of unshown plans the frame on screen went at the next insert
    /// and was missing at the next refresh, where 50 fps on a 60 Hz display shows it again.
    #[test]
    fn the_plan_on_screen_outlives_plans_left_over_from_an_earlier_play() {
        let mut cache = full_plan_cache(2);
        // an earlier play stopped at frame 1000 with its prefetch run decoded
        for f in 1000..1000 + 24 {
            cache.insert(plan_key(f), small_plan());
        }
        cache.insert(plan_key(0), small_plan());
        play(&mut cache, 0, 60);
        // the leftovers went instead, and the frames ahead are all there
        assert!((1000..1024).all(|f| !cache.map.contains_key(&plan_key(f))), "plans of the earlier play outlived this play's");
        assert!((60..60 + super::prefetch_depth(1.0)).all(|f| cache.map.contains_key(&plan_key(f))));
    }

    /// A frame that finished after the playhead passed it lies behind the play window: it goes
    /// before the plan on screen does, and the monitor has no older frame to fall back on.
    #[test]
    fn a_plan_that_finished_late_goes_before_the_plan_on_screen() {
        let mut cache = full_plan_cache(2);
        cache.insert(plan_key(0), small_plan());
        play(&mut cache, 0, 40);
        // frames 30 and 31 were dropped and arrive now, with the playhead on 40; so do a
        // cache's worth of others through the play
        for late in 0..40 {
            cache.insert(plan_key(late), small_plan());
            play(&mut cache, 40 + late, 41 + late);
        }
        play(&mut cache, 80, 140);
        assert!((0..40).all(|f| !cache.map.contains_key(&plan_key(f))), "the late plans are still cached");
    }

    /// Stopped (scrubbing, stepping), nothing plays and the cache is least-recently-used again.
    #[test]
    fn without_a_play_window_the_least_recently_used_plan_goes_first() {
        let mut cache = full_plan_cache(0);
        let min = cache.min as i64;
        for f in 0..min {
            cache.insert(plan_key(f), small_plan());
        }
        assert!(cache.get(&plan_key(0)).is_some());
        cache.insert(plan_key(min), small_plan());
        assert!(cache.map.contains_key(&plan_key(0)), "the plan just shown stays");
        assert!(!cache.map.contains_key(&plan_key(1)), "the oldest unused plan goes");
    }

    #[test]
    fn a_play_window_holds_the_frames_around_the_frame_due_in_the_direction_of_play() {
        let w = super::PlayWindow { key: plan_key(100), dir: 1, ahead: 15 };
        assert_eq!(w.offset(&plan_key(100)), Some(0));
        assert_eq!(w.offset(&plan_key(98)), Some(-2));
        assert_eq!(w.offset(&plan_key(97)), None);
        assert_eq!(w.offset(&plan_key(115)), Some(15));
        assert_eq!(w.offset(&plan_key(116)), None);
        // another revision, size or sequence is another picture
        assert_eq!(w.offset(&FrameKey { revision: 1, ..plan_key(101) }), None);
        assert_eq!(w.offset(&FrameKey { size: 500, ..plan_key(101) }), None);
        assert_eq!(w.offset(&FrameKey { target: Target::Sequence(ItemId(1)), ..plan_key(101) }), None);
        // in reverse the frames about to be shown are the lower ones
        let back = super::PlayWindow { key: plan_key(100), dir: -1, ahead: 15 };
        assert_eq!(back.offset(&plan_key(90)), Some(10));
        assert_eq!(back.offset(&plan_key(102)), Some(-2));
        assert_eq!(back.offset(&plan_key(103)), None);
        // hostile frame numbers neither overflow nor match
        assert_eq!(w.offset(&plan_key(i64::MIN)), None);
        assert_eq!(super::PlayWindow { key: plan_key(i64::MAX), dir: -1, ahead: 15 }.offset(&plan_key(i64::MIN)), None);
    }

    #[test]
    fn plan_cache_min_covers_prefetch_and_workers() {
        let c = super::PlanCache::new(6);
        assert_eq!(c.min, 24 + 6 + 2);
        assert!(c.min <= super::PlanCache::MAX);
    }

    use super::{FrameKey, PlaybackMeter, Target, UPLOAD_AHEAD, background_work, playback_plan, recent_keys, upload_now};
    use filmcraft_project::ItemId;

    #[test]
    fn a_result_is_cached_unless_a_source_gave_up_or_the_server_stops() {
        use super::keep_result;
        assert!(keep_result(false, false, false));
        assert!(keep_result(true, false, false), "cancelled after its sources finished: the frame is there");
        assert!(!keep_result(true, false, true), "cancelled because the server stops");
        assert!(!keep_result(false, true, false), "a source dropped a late frame on purpose");
        assert!(!keep_result(true, true, false), "a source stopped short");
    }

    #[test]
    fn views_beside_a_play_are_the_ones_that_do_not_show_a_paused_frame() {
        use super::beside_playback;
        // the scopes (40 while playing), the reference monitor (3), thumbnails (50)
        assert!(beside_playback(40, true) && beside_playback(3, true) && beside_playback(50, true));
        // a paused monitor's frame while another monitor plays
        assert!(!beside_playback(0, true));
        // nothing plays: every request is decoded exactly
        assert!(!beside_playback(40, false) && !beside_playback(0, false));
    }

    /// (playing, beside) of a queued job after the same frame is asked for again.
    #[test]
    fn the_newest_request_says_whether_a_queued_job_is_playback() {
        use super::merged_kind;
        // playback stopped with the frame due still queued, and the paused monitor asks for it
        assert_eq!(merged_kind((true, false), (false, false)), (false, false));
        // Play pressed while the paused frame was still queued
        assert_eq!(merged_kind((false, false), (true, false)), (true, false));
        // a view beside the play asks for a frame playback (or a paused monitor) waits for
        assert_eq!(merged_kind((true, false), (false, true)), (true, false));
        assert_eq!(merged_kind((false, false), (false, true)), (false, false));
        // a frame only views beside the play asked for, until someone else wants it
        assert_eq!(merged_kind((false, true), (false, true)), (false, true));
        assert_eq!(merged_kind((false, true), (true, false)), (true, false));
        assert_eq!(merged_kind((false, true), (false, false)), (false, false));
    }

    /// A looping play asks again, on its next round, for the frames a source dropped.
    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn a_new_play_asks_again_for_the_frames_dropped_in_the_one_before() {
        use super::{FrameKey, FrameServer};
        use std::sync::PoisonError;
        let s = filmcraft_engine::Session::default();
        let server = FrameServer::new(s.media.clone(), s.services.clone(), s.previews.clone(), 1);
        let key = FrameKey { target: Target::Item(ItemId(999)), frame: 7, size: 32, revision: 0, draft: false };
        server.shared.dropped.lock().unwrap_or_else(PoisonError::into_inner).insert(key);
        server.play_starts();
        assert!(server.shared.dropped.lock().unwrap_or_else(PoisonError::into_inner).is_empty());
    }

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
        assert!(!background_work(50, Target::SequencePlan(ItemId(1), filmcraft_render::plan::DeviceCaps::NONE)));
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

    /// #1673: the recent-frame fallback looks at frames already played, newest revision first.
    #[test]
    fn recent_frames_lie_on_the_played_side() {
        let key = FrameKey { target: Target::Sequence(ItemId(1)), frame: 10, size: 1000, revision: 5, draft: false };
        let fwd: Vec<(i64, u64)> = recent_keys(key, 1, 2, 1).map(|k| (k.frame, k.revision)).collect();
        assert_eq!(fwd, [(10, 5), (10, 4), (9, 5), (9, 4), (8, 5), (8, 4)]);
        let rev: Vec<(i64, u64)> = recent_keys(key, -1, 2, 1).map(|k| (k.frame, k.revision)).collect();
        assert_eq!(rev, [(10, 5), (10, 4), (11, 5), (11, 4), (12, 5), (12, 4)]);
        // no revision before the first
        assert_eq!(recent_keys(FrameKey { revision: 0, ..key }, 1, 0, 64).count(), 1);
    }

    /// The cost that paces playback counts the worker's own decoding (an intra-only source
    /// decodes on the calling thread) but not its waits for a decoder's threads.
    #[test]
    fn job_cost_counts_the_workers_own_decoding_but_not_its_waits() {
        use super::job_cost;
        use std::time::Duration;
        let ms = Duration::from_millis;
        // 5 ms of compositing; the frame came from the cache
        assert_eq!(job_cost(ms(5), ms(0), ms(0)), ms(5));
        // a 4K intra picture decoded on this worker: 140 ms of CPU inside the source call
        assert_eq!(job_cost(ms(145), ms(140), ms(138)), ms(143));
        // waited 450 ms for a frame-threaded decoder to output the picture: not this worker's work
        assert_eq!(job_cost(ms(455), ms(450), ms(2)), ms(7));
        // a clock quirk (CPU time over wall time) cannot inflate the cost
        assert_eq!(job_cost(ms(10), ms(8), ms(20)), ms(10));
    }

    #[test]
    fn a_frame_that_cannot_be_ready_in_time_is_not_started() {
        use super::starts_too_late;
        use std::time::{Duration, Instant};
        let now = Instant::now();
        let ms = Duration::from_millis;
        assert!(!starts_too_late(None, now, ms(170)), "no due time: thumbnails, the frame on screen");
        assert!(!starts_too_late(Some(now + ms(200)), now, ms(170)));
        assert!(starts_too_late(Some(now + ms(150)), now, ms(170)));
        assert!(starts_too_late(Some(now - ms(10)), now, ms(1)), "already due");
        assert!(!starts_too_late(Some(now - ms(10)), now, ms(0)), "an unknown cost never leaves a frame out");
    }

    /// With an uploader, a prefetch more than [`UPLOAD_AHEAD`] frames ahead waits; the frame
    /// due now, a paused request and the near prefetches upload at once; without an uploader
    /// nothing is deferred (the UI thread uploads as it draws).
    #[test]
    fn far_prefetches_wait_for_their_upload() {
        assert!(upload_now(true, false, 0));
        assert!(upload_now(true, false, 40));
        assert!(upload_now(true, true, 1));
        assert!(upload_now(true, true, UPLOAD_AHEAD));
        assert!(!upload_now(true, true, UPLOAD_AHEAD + 1));
        assert!(!upload_now(true, true, u32::MAX));
        assert!(upload_now(false, true, u32::MAX));
    }

    #[test]
    fn playback_plan_skips_frames_only_when_workers_cannot_keep_up() {
        // decode-only / light frames: every frame from the playhead on
        assert_eq!(playback_plan(0.0, 24.0, 6), (1, 1));
        assert_eq!(playback_plan(0.15, 24.0, 6), (1, 1));
        // 0.5 s of effects per frame at 24 fps on 6 workers: 12 frames pass while one renders,
        // so start 12 ahead, and every 3rd frame is what 6 workers can finish
        assert_eq!(playback_plan(0.5, 24.0, 6), (12, 3));
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
            due: None,
            playing: false,
            beside: false,
            time: filmcraft_time::Tick::ZERO,
            scale: 0.1,
            project: Arc::new(filmcraft_project::Project::new("Frames")),
            prio: 0,
        };
        // what `Drop` sets before it cancels the jobs in flight
        server.shared.stop.store(true, Ordering::Relaxed);
        run_job(&server.shared, &job, &server.pool, &server.services, &server.previews);
        assert!(job.cancel.load(Ordering::Relaxed), "the job ran without its cancel flag");
        assert!(!server.is_ready(&job.key), "a cancelled job's frame is not cached while the server stops");
    }

    /// Playback that moved past a frame cancels its job; when the job's sources had already
    /// produced their frames (nothing gave up), the result is complete and is cached: a late
    /// frame is still the nearest picture to show, and nothing is saved by throwing it away.
    #[test]
    fn a_job_cancelled_after_its_sources_finished_is_still_cached() {
        use super::{FrameKey, FrameServer, Job, Target, run_job};
        use filmcraft_project::ItemId;
        use std::sync::Arc;
        use std::sync::atomic::Ordering;
        let s = filmcraft_engine::Session::default();
        let server = FrameServer::new(s.media.clone(), s.services.clone(), s.previews.clone(), 1);
        let job = Job {
            key: FrameKey { target: Target::Item(ItemId(999)), frame: 0, size: 32, revision: 0, draft: false },
            queued: web_time::Instant::now(),
            cancel: Default::default(),
            prefetch: true,
            catch_up: None,
            due: None,
            playing: false,
            beside: false,
            time: filmcraft_time::Tick::ZERO,
            scale: 0.1,
            project: Arc::new(filmcraft_project::Project::new("Frames")),
            prio: 1,
        };
        // cancelled before it ran: the missing item renders its slate without asking any source
        job.cancel.store(true, Ordering::Relaxed);
        run_job(&server.shared, &job, &server.pool, &server.services, &server.previews);
        assert!(server.is_ready(&job.key), "a complete result is cached although the job was cancelled");
        let st = server.shared.stats.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        assert_eq!((st.cancelled, st.kept_late), (1, 1));
    }
    /// A sequence plan is made for the caps in its key: the same frame asked for a device without
    /// an effect stage and for one with it gives two cached plans, the CPU image and the plan with
    /// the adjustment step, so a plan made for one device is never shown on the other.
    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn a_sequence_plan_is_made_for_the_caps_in_its_key() {
        use super::{FrameKey, FrameServer};
        use filmcraft_media::generators::GeneratorSource;
        use filmcraft_media::{Generator, MediaSource};
        use filmcraft_project::{ItemKind, Label, MediaClip, MediaRef, Project, SequenceSettings, TrackKind};
        use filmcraft_render::plan::{DeviceCaps, FramePlan, PlanStep};
        use filmcraft_time::{FrameRate, TICKS_PER_SECOND, Tick, TimeRange};
        use std::sync::Arc;
        use std::time::{Duration, Instant};

        let r = FrameRate::FPS_24;
        let mut p = Project::new("Frames");
        let g = GeneratorSource::new(Generator::ColorMatte { color: [0.9, 0.2, 0.1, 1.0] }, 320, 180, r, Tick(2 * TICKS_PER_SECOND));
        let info = g.info().clone();
        let matte = p.add_item(
            &info.name.clone(),
            Label::Iris,
            ItemKind::Media(Box::new(MediaClip {
                media: MediaRef::Generator(g.generator.clone()),
                info,
                interpret: Default::default(),
                mark_in: None,
                mark_out: None,
                markers: vec![],
                offline: false,
                proxy: None,
                identity: None,
            })),
            None,
        );
        let adj =
            p.add_item("Adjustment Layer", Label::Lavender, ItemKind::AdjustmentLayer { width: 320, height: 180, rate: r, duration: r.tick_of(24) }, None);
        let seq = p.new_sequence("s", SequenceSettings { width: 320, height: 180, frame_rate: r, ..Default::default() }, 2, 0, None);
        for (track, item) in [(0, matte), (1, adj)] {
            let mut ti = p.make_track_item(item, TrackKind::Video, Tick::ZERO, TimeRange::new(Tick::ZERO, r.tick_of(24)), r).unwrap();
            if item == adj {
                ti.effects.insert(0, filmcraft_project::find_effect("invert").unwrap().instance());
            }
            p.sequence_mut(seq).unwrap().video_tracks[track].items.push(ti);
        }
        let p = Arc::new(p);
        let s = filmcraft_engine::Session::default();
        let server = FrameServer::new(s.media.clone(), s.services.clone(), s.previews.clone(), 2);
        let key = |caps| FrameKey { target: Target::SequencePlan(seq, caps), frame: 0, size: 1000, revision: 0, draft: false };
        for caps in [DeviceCaps::NONE, DeviceCaps::ALL] {
            server.request(key(caps), Tick::ZERO, 1.0, &p, 0);
        }
        let t0 = Instant::now();
        while !(server.is_ready(&key(DeviceCaps::NONE)) && server.is_ready(&key(DeviceCaps::ALL))) && t0.elapsed() < Duration::from_secs(30) {
            std::thread::sleep(Duration::from_millis(5));
        }
        let none = server.get_plan(&key(DeviceCaps::NONE)).expect("the plan for a device without an effect stage");
        assert!(matches!(none.plan, FramePlan::Image(_)), "no effect stage: the worker renders the frame on the CPU");
        let all = server.get_plan(&key(DeviceCaps::ALL)).expect("the plan for a device with an effect stage");
        assert!(
            matches!(&all.plan, FramePlan::Composite { steps, .. } if steps.iter().any(|st| matches!(st, PlanStep::Adjust { .. }))),
            "an effect stage: the adjustment layer is a step of the plan"
        );
    }
}
