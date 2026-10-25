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
use std::sync::{Arc, Condvar, Mutex};
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
    /// Asked for with playback stopped ([`FrameServer::request_while`]): one frame someone waits for
    /// (`filmcraft_media::cancel::with_still`).
    still: bool,
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
    /// What playback jobs learned about throughput-bound sources (see [`SourceRate`]).
    throughput: Mutex<Throughput>,
    workers: usize,
    /// Always-on counters for `perf.stats`.
    stats: Mutex<FrameStats>,
    /// Set (under the queue lock) when the [`FrameServer`] is dropped: the workers return.
    stop: AtomicBool,
}

/// What playback knows about the throughput-bound sources it is playing (sources whose frames
/// decode independently, [`filmcraft_media::FrameDelivery`]): the frames per second the slowest
/// of them delivers and the time a playback job on one takes from its start to its frame. Drives
/// [`throughput_plan`].
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct SourceRate {
    /// Frames per second (smoothed); `None` until a source reported one or enough jobs completed.
    pub rate: Option<f64>,
    /// Seconds from a job's start to its frame (smoothed, rising fast), with the wait for a
    /// decoder slot: what the lead is sized by.
    pub latency: f64,
    /// The same, smoothed slowly both ways: what the workers' throughput is judged by, so one
    /// slow job (a 60 ms one among 40 ms ones at 120 fps) does not read as capacity lost.
    pub latency_slow: f64,
}

#[derive(Default)]
struct Throughput {
    model: SourceRate,
    /// A playback job has touched a throughput-bound source, and the ones since did too.
    active: bool,
    /// Playback jobs in a row that touched none: a few of them (another sequence, the clip after
    /// the cut) clear the model, so an ordinary source is not played with a stride.
    misses: u32,
    /// When jobs on such sources finished (not cancelled), for the rate when the source itself
    /// reports none.
    completions: VecDeque<Instant>,
    /// The pattern playback last planned with (see [`choose_pattern`]).
    pattern: Option<(i64, i64)>,
    /// The refresh window being counted (see [`Self::settle`]): since when, refreshes in it,
    /// how many found the picture late (more than a pattern step behind), how many on time.
    window: Option<(Instant, u32, u32, u32)>,
    /// Windows in a row with the picture on time.
    on_time_windows: u32,
    /// When the pattern was last made sparser because the picture ran late, and how long after
    /// that a denser one may be tried (doubles while trying fails, see [`Self::settle`]).
    backed_off_at: Option<Instant>,
    cooldown: Duration,
    /// A denser pattern was stepped up to at this time, from this one: a step that runs late
    /// within [`Self::HOLD`] goes back to it and doubles the cooldown.
    stepped_up: Option<(Instant, (i64, i64))>,
    /// The source measures its own rate, and when a job last brought a fresh reading of it
    /// (see [`filmcraft_media::FrameDelivery::fresh`]).
    reports_rate: bool,
    fresh_at: Option<Instant>,
    /// When the playback clock started (the preroll ended): the picture is not judged for
    /// [`Self::STARTUP`] after that, while the first planned frames are still on their way.
    clock_started: Option<Instant>,
}

impl Throughput {
    /// Playback jobs without a throughput-bound source after which the model is dropped.
    const MISSES: u32 = 12;
    const COMPLETIONS: usize = 32;
    /// Refreshes are counted in windows this long; a window with two in five refreshes late
    /// makes the pattern sparser, whatever the capacity reads.
    const WINDOW: Duration = Duration::from_millis(500);
    /// Windows in a row with the picture on time that allow a denser pattern, once the
    /// cooldown has passed.
    const SLACK_WINDOWS: u32 = 2;
    /// The cooldown after backing off: this, doubled for every step up that failed within
    /// [`Self::HOLD`], up to a minute; back to this once a step up held that long.
    const COOLDOWN: Duration = Duration::from_secs(4);
    const MAX_COOLDOWN: Duration = Duration::from_secs(60);
    const HOLD: Duration = Duration::from_secs(3);
    /// Lateness is not judged this long after backing off: the frames the denser pattern left
    /// queued are dropped as the playhead passes them, the new pattern's frames are started a
    /// lead ahead and take the latency to arrive, and the picture catches up meanwhile; a
    /// back-off every window would otherwise cascade to every n-th frame.
    const RECOVER: Duration = Duration::from_secs(2);
    /// A reading older than this is stale: playback asks for a few more frames at once to
    /// measure the source again (see [`FrameServer::schedule_playback`]).
    const FRESH_FOR: Duration = Duration::from_secs(2);
    /// The picture is not judged this long after the clock starts: the preroll frames are
    /// ready, the planned run beyond them is still decoding, and a window judged on that
    /// backed a 6K 59.94p clip off from every third frame to every fifth at every start.
    const STARTUP: Duration = Duration::from_secs(2);

    /// The playback clock is running (`false` while prerolling); `Some` once the picture may be
    /// judged, with the time now.
    fn judging(&mut self, running: bool, now: Instant) -> bool {
        if !running {
            self.clock_started = None;
            return false;
        }
        let started = *self.clock_started.get_or_insert(now);
        now.saturating_duration_since(started) >= Self::STARTUP
    }

    /// The pattern to use, given what the capacity reading chose (`chosen`, with the rate's
    /// hysteresis) and how the picture is doing: `lag` is how many frames the newest ready
    /// picture of the pattern in use is behind the frame due (`None` while the clock has not
    /// started).
    ///
    /// The reading is a throughput measured at saturation, within a few per cent of the
    /// sequence rate for a 6K 24p clip on a 22–27 fps pipeline, so the picture itself decides
    /// at the margin, counted over [`Self::WINDOW`]s of refreshes (a pipeline a fifth short of
    /// a pattern still lands most of its frames, so the lag flickers between zero and several
    /// frames rather than staying high): a window with two in five refreshes late → sparser
    /// now, back to the pattern before a step up that just failed; the reading's denser choice
    /// only after [`Self::SLACK_WINDOWS`] on time and the cooldown since backing off; while a
    /// step up is held for [`Self::HOLD`] the reading's sparser verdict is ignored. Stepping up
    /// beyond what the reading allows was tried: a step that fails costs the frames the
    /// decoder decoded for nothing, and a 6K 59.94p clip at every third frame paid
    /// 50–60 cancelled frames and a cascade of back-offs for each try at every second one; a
    /// stale reading is refreshed by asking for a few more frames at once instead.
    fn settle(&mut self, chosen: (i64, i64), lag: Option<i64>, now: Instant) -> (i64, i64) {
        let density = |(keep, of): (i64, i64)| keep as f64 / of.max(1) as f64;
        let Some(in_use) = self.pattern else { return chosen };
        let Some(lag) = lag else {
            // nothing shown yet: no reason to ask for more than before
            return if density(chosen) > density(in_use) { in_use } else { chosen };
        };
        let step = in_use.1.max(1);
        // count this refresh; close the window once it is full
        let (mut late_window, mut closed) = (false, false);
        match &mut self.window {
            Some((since, n, late, ok)) if now.saturating_duration_since(*since) < Self::WINDOW => {
                *n += 1;
                *late += (lag > step) as u32;
                // on time: no further behind than the frames the pattern leaves out of each group
                *ok += (lag <= step - in_use.0.clamp(1, step)) as u32;
            }
            w => {
                if let Some((_, n, late, ok)) = *w {
                    closed = true;
                    late_window = late.saturating_mul(5) >= n.saturating_mul(2);
                    self.on_time_windows = if ok.saturating_mul(10) >= n.saturating_mul(9) { self.on_time_windows.saturating_add(1) } else { 0 };
                }
                *w = Some((now, 1, (lag > step) as u32, (lag <= step - in_use.0.clamp(1, step)) as u32));
            }
        }
        let holding = self.stepped_up.filter(|(at, _)| now.saturating_duration_since(*at) < Self::HOLD);
        if self.stepped_up.is_some() && holding.is_none() {
            // the step up held: trying again later starts from the base cooldown
            self.stepped_up = None;
            self.cooldown = Self::COOLDOWN;
        }
        let recovering = self.backed_off_at.is_some_and(|a| now.saturating_duration_since(a) < Self::RECOVER);
        if closed && late_window && !recovering && density(chosen) >= density(in_use) {
            let back = match holding {
                Some((_, before)) => {
                    self.cooldown = self.cooldown.max(Self::COOLDOWN).saturating_mul(2).min(Self::MAX_COOLDOWN);
                    before
                }
                None => {
                    self.cooldown = Self::COOLDOWN;
                    next_sparser(in_use)
                }
            };
            self.backed_off_at = Some(now);
            self.stepped_up = None;
            self.on_time_windows = 0;
            return back;
        }
        if holding.is_some() && density(chosen) < density(in_use) {
            return in_use;
        }
        let slack = closed && self.on_time_windows >= Self::SLACK_WINDOWS;
        let cooled = self.backed_off_at.is_none_or(|a| now.saturating_duration_since(a) >= self.cooldown.max(Self::COOLDOWN));
        if density(chosen) > density(in_use) {
            if !(slack && cooled) {
                return in_use;
            }
            self.stepped_up = Some((now, in_use));
            self.on_time_windows = 0;
            return chosen;
        }
        chosen
    }

    /// The source measures its own rate and its last reading is older than [`Self::FRESH_FOR`]:
    /// a sparse plan keeps too few requests in flight for the source to measure itself.
    fn reading_stale(&self, now: Instant) -> bool {
        self.active && self.reports_rate && self.fresh_at.is_none_or(|f| now.saturating_duration_since(f) > Self::FRESH_FOR)
    }

    /// A playback job finished: `delivery` is what its sources said (`None`: none of them is
    /// throughput-bound), `decoded` whether it asked any source for a frame at all (a frame past
    /// the end of the sequence, a gap or a rendered segment asks none, and says nothing about
    /// what is playing), `took` its start-to-finish time when it was not cancelled.
    fn job(&mut self, delivery: Option<filmcraft_media::FrameDelivery>, decoded: bool, took: Option<f64>) {
        let Some(d) = delivery.filter(|d| d.independent) else {
            if decoded {
                self.misses = self.misses.saturating_add(1);
                if self.misses >= Self::MISSES {
                    *self = Self::default();
                }
            }
            return;
        };
        self.misses = 0;
        self.active = true;
        let now = Instant::now();
        self.reports_rate |= d.rate.is_some();
        if d.fresh {
            self.fresh_at = Some(now);
        }
        if let Some(took) = took.filter(|t| t.is_finite() && *t >= 0.0) {
            // late is worse than early: rise fast, settle slowly
            let l = self.model.latency;
            self.model.latency = if l == 0.0 {
                took
            } else if took > l {
                l * 0.5 + took * 0.5
            } else {
                l * 0.8 + took * 0.2
            };
            let s = self.model.latency_slow;
            self.model.latency_slow = if s == 0.0 { took } else { s * 0.95 + took * 0.05 };
            if self.completions.len() >= Self::COMPLETIONS {
                self.completions.pop_front();
            }
            self.completions.push_back(now);
        }
        let measured = d.rate.filter(|r| r.is_finite() && *r > 0.0).or_else(|| {
            // completions over the last two seconds, once there are enough to mean something
            let recent: Vec<&Instant> = self.completions.iter().filter(|t| now.saturating_duration_since(**t) <= Duration::from_secs(2)).collect();
            let span = recent.first().map(|t| now.saturating_duration_since(**t).as_secs_f64()).unwrap_or(0.0);
            (recent.len() >= 6 && span > 0.0).then(|| (recent.len() - 1) as f64 / span)
        });
        if let Some(r) = measured {
            self.model.rate = Some(match self.model.rate {
                // the source's reading is its best recent moment: believe a rise slowly, a
                // fall quickly, so the stride does not flap on one fast reply
                Some(p) if r > p => p * 0.9 + r * 0.1,
                Some(p) => p * 0.6 + r * 0.4,
                None => r,
            });
        }
    }
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
    /// The most recent jobs: (source ms, render ms, texel preparation ms, plan-cache insert ms);
    /// the last two are part of the render time of a GPU plan job.
    pub recent: VecDeque<(f32, f32, f32, f32)>,
}

impl FrameStats {
    const RECENT: usize = 256;

    /// JSON for `perf.stats`.
    pub fn to_json(&self) -> serde_json::Value {
        let pct = |f: &dyn Fn(&(f32, f32, f32, f32)) -> f32, p: f64| -> f64 {
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
            "prepareMs": {"p50": pct(&|r| r.2, 0.5), "p95": pct(&|r| r.2, 0.95)},
            "insertMs": {"p50": pct(&|r| r.3, 0.5), "p95": pct(&|r| r.3, 0.95)},
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
    /// What the throughput-bound sources of the current job said about their delivery (the
    /// slowest rate among them); `None` when none was asked for a frame.
    static DELIVERY: Cell<Option<filmcraft_media::FrameDelivery>> = const { Cell::new(None) };
    /// The current job asked a source for a frame.
    static DECODED: Cell<bool> = const { Cell::new(false) };
    /// This worker is profiling the current job (thread CPU of source calls is measured).
    static PROFILING: Cell<bool> = const { Cell::new(false) };
}

/// Run `f`, adding its wall/CPU time to this thread's source-time accumulator.
fn timed_source<R>(f: impl FnOnce() -> R) -> R {
    let cpu = || if PROFILING.with(Cell::get) { thread_cpu_time().unwrap_or_default() } else { Duration::ZERO };
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
        let d = self.0.frame_delivery();
        if d.independent {
            DELIVERY.with(|c| {
                let m = c.get().unwrap_or_default();
                let rate = match (m.rate, d.rate) {
                    (Some(a), Some(b)) => Some(a.min(b)),
                    (a, b) => a.or(b),
                };
                c.set(Some(filmcraft_media::FrameDelivery { independent: true, rate, fresh: m.fresh || d.fresh }));
            });
        }
        DECODED.with(|c| c.set(true));
        timed_source(|| self.0.video_frame(req))
    }
    fn frame_delivery(&self) -> filmcraft_media::FrameDelivery {
        self.0.frame_delivery()
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
}

impl GpuPlan {
    /// Bytes this plan keeps alive (layer frames + converted texels).
    fn bytes(&self) -> usize {
        let frames = self.plan.source_bytes();
        frames + self.prepared.bytes()
    }
}

/// Finished plans, bounded by count and bytes (see [`eviction_key`] for which go first).
#[derive(Default)]
struct PlanCache {
    /// Plan, when it was last looked up, when it was inserted, its bytes.
    map: HashMap<FrameKey, (Arc<GpuPlan>, u64, u64, usize)>,
    clock: u64,
    bytes: usize,
    /// Bytes of the plan inserted last: what one frame of what is playing costs.
    last_bytes: usize,
}

/// Frames playback may hold ahead of the playhead when the cache holds `capacity` of them:
/// all of `depth` while they fit, else what fits beside the frame on screen and one behind it.
/// Decoders that run a little faster than the frames are due would otherwise fill the cache
/// with frames far ahead and evict the one due next (a 6K half-resolution float plan is 123 MB,
/// eight to the budget, while the prefetch runs 14 deep).
fn lookahead(capacity: usize, depth: i64) -> i64 {
    depth.min(i64::try_from(capacity).unwrap_or(i64::MAX).saturating_sub(2)).max(2)
}

/// Which cache entries go first when the cache is over budget, given when an entry was last
/// looked up and when it was inserted: entries looked up since they were inserted (shown, or
/// scrubbed past), least recently used first; then the ones never looked up, oldest first.
/// Playback prefetches frames ahead of the playhead, and nothing looks at one until it is due:
/// under plain LRU they were the oldest entries, and a 6K half-resolution frame (then 123 MB as
/// a plan, eight to the budget) decoded 26 frames ahead was evicted before the playhead reached
/// it, while the frames behind the playhead stayed. A never-used entry keeps that place only
/// while it is fresh (inserted within [`PlanCache::FRESH`] ticks of the cache's `clock`) and
/// playback has not stopped since ([`PlanCache::stale_prefetch`]): a frame playback skipped or
/// prefetched before it stopped, or one of an old revision, would otherwise stay until nothing
/// else is left, and a scrub would evict the frames it had just shown instead.
fn eviction_key(used: u64, inserted: u64, clock: u64) -> (bool, u64) {
    (used == inserted && clock.saturating_sub(inserted) < PlanCache::FRESH, used)
}

/// When a plan last looked up at `used` counts as inserted once playback stopped (see
/// [`PlanCache::stale_prefetch`]): one nobody looked up no longer counts as fresh.
fn stopped(used: u64, inserted: u64) -> u64 {
    if used == inserted { inserted.saturating_sub(PlanCache::FRESH) } else { inserted }
}

impl PlanCache {
    const MAX: usize = 96;
    const BUDGET: usize = 1 << 30;
    /// Lookups and inserts within which a prefetched plan is still due (playback refreshes look
    /// up a few plans a frame; it prefetches at most [`prefetch_depth`] frames ahead).
    const FRESH: u64 = 1024;

    /// Playback stopped: the plans it prefetched and nobody looked up count as used when they
    /// were inserted, so they go before the frames a scrub shows next.
    fn stale_prefetch(&mut self) {
        for (_, used, inserted, _) in self.map.values_mut() {
            *inserted = stopped(*used, *inserted);
        }
    }

    fn get(&mut self, k: &FrameKey) -> Option<Arc<GpuPlan>> {
        self.clock += 1;
        let clock = self.clock;
        self.map.get_mut(k).map(|(p, used, _, _)| {
            *used = clock;
            p.clone()
        })
    }

    /// How many plans like the last one fit the budget.
    fn capacity(&self) -> usize {
        (Self::BUDGET / self.last_bytes.max(1)).min(Self::MAX)
    }

    fn insert(&mut self, k: FrameKey, p: GpuPlan) {
        self.clock += 1;
        let b = p.bytes();
        self.last_bytes = b;
        self.bytes += b;
        if let Some((_, _, _, old)) = self.map.insert(k, (Arc::new(p), self.clock, self.clock, b)) {
            self.bytes -= old;
        }
        if self.map.len() > Self::MAX || self.bytes > Self::BUDGET {
            let clock = self.clock;
            let mut v: Vec<((bool, u64), FrameKey, usize)> = self.map.iter().map(|(k, v)| (eviction_key(v.1, v.2, clock), *k, v.3)).collect();
            v.sort_unstable_by_key(|x| x.0);
            for (_, k, b) in v {
                if self.map.len() <= Self::MAX && self.bytes <= Self::BUDGET {
                    break;
                }
                if let Some((p, ..)) = self.map.remove(&k) {
                    recycle_plan(p);
                }
                self.bytes -= b;
            }
        }
    }
}

/// An evicted plan's decoded frames go back to the frame pool when nothing else holds them (a
/// 6K camera-RAW frame is 41 MB: allocated fresh for every frame, it page-faults for ~20 ms on
/// the worker; see `filmcraft_frame::pool`).
fn recycle_plan(plan: Arc<GpuPlan>) {
    use filmcraft_render::plan::{FramePlan, PlanStep};
    let Ok(p) = Arc::try_unwrap(plan) else { return };
    let layers = |ls: Vec<filmcraft_render::plan::PlanLayer>| ls.into_iter().for_each(|l| filmcraft_frame::pool::recycle(l.frame));
    match p.plan {
        FramePlan::Layers { layers: ls, .. } => layers(ls),
        FramePlan::Composite { steps, .. } => {
            for s in steps {
                match s {
                    PlanStep::Layers(ls) => layers(ls),
                    PlanStep::Transition { inputs, .. } => inputs.into_iter().for_each(&layers),
                }
            }
        }
        FramePlan::Image(_) => {}
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
pub fn playback_plan(render_cost: f64, fps: f64, workers: usize) -> (i64, i64) {
    let frames_per_job = render_cost * fps;
    let load = frames_per_job * 1.15 / workers.max(1) as f64;
    if load <= 1.0 {
        return (1, 1);
    }
    (frames_per_job.ceil() as i64, load.ceil() as i64)
}

/// The patterns a throughput-bound plan may keep, `(keep, of)` frames, densest first: every
/// frame, three of four, two of three, every second, two of five, three of eight, every third,
/// every fourth, then every n-th. Three of four is what a 6K 24p clip gets from a pipeline that
/// manages 20 fps, instead of every second frame. Two of five is 24 fps of a 59.94p clip: a 6K
/// half-resolution pipeline delivers 25–27 fps, and without it the plan fell to every third
/// frame (20 fps) where 22.5–24 fps is possible.
const PATTERNS: [(i64, i64); 8] = [(1, 1), (3, 4), (2, 3), (1, 2), (2, 5), (3, 8), (1, 3), (1, 4)];
const SPARSEST: i64 = 64;

/// The densest pattern whose share of the frames the source can deliver (`ratio` = frames per
/// second it delivers over the sequence's), with hysteresis against `previous`: a sparser
/// pattern once the share is 10 % short of the current one (the source fell behind; a source
/// that just keeps up reads as exactly the sequence rate, since saturation is what is measured,
/// and must not be read as falling behind on noise), a denser one only with 5 % to spare. The
/// reading alone is not what decides at the margin: [`Throughput::settle`] backs off as soon
/// as the picture runs late and waits for slack before asking for more, so a pipeline a few
/// per cent short of every frame does not fall behind for seconds before the reading says so.
fn choose_pattern(ratio: f64, previous: Option<(i64, i64)>) -> (i64, i64) {
    if !ratio.is_finite() || ratio <= 0.0 {
        return previous.unwrap_or((1, SPARSEST));
    }
    let density = |(keep, of): (i64, i64)| keep as f64 / of.max(1) as f64;
    let fit = PATTERNS.into_iter().find(|p| density(*p) <= ratio).unwrap_or_else(|| (1, ((1.0 / ratio).ceil() as i64).clamp(5, SPARSEST)));
    match previous {
        Some(p) if density(p) > ratio => {
            if ratio < density(p) * 0.9 {
                fit
            } else {
                p
            }
        }
        Some(p) if density(fit) > density(p) && ratio < density(fit) * 1.05 => p,
        _ => fit,
    }
}

/// The next sparser pattern after `p`: the next of [`PATTERNS`], then every n-th.
fn next_sparser(p: (i64, i64)) -> (i64, i64) {
    match PATTERNS.iter().position(|q| *q == p) {
        Some(i) => PATTERNS.get(i + 1).copied().unwrap_or((1, p.1.saturating_add(1).min(SPARSEST))),
        None => (1, p.1.max(1).saturating_add(1).min(SPARSEST)),
    }
}

/// Whether frame `f` is in the pattern: the frames where `f × keep / of` crosses an integer,
/// `keep` of every `of`, spread evenly and the same set whatever the playhead is at.
fn in_pattern(f: i64, keep: i64, of: i64) -> bool {
    let of = of.max(1);
    let keep = keep.clamp(1, of);
    keep == of || f.saturating_mul(keep).div_euclid(of) != f.saturating_sub(1).saturating_mul(keep).div_euclid(of)
}

/// Which frames playback asks for from a throughput-bound source (frames decode independently,
/// [`SourceRate`]): `(lead, keep, of)`. The plan keeps `keep` of every `of` frames
/// ([`choose_pattern`]) according to what the source delivers against what the sequence needs,
/// bounded as well by the frame workers (each holds a job for the source's latency: six workers
/// at 330 ms sustain 18 frames per second whatever the decoder manages), so that a 25 fps
/// decoder under 59.94 shows every fourth frame evenly instead of all of them each a little
/// late. `lead` is the latency in frames with a quarter and a frame of slack, so a frame started
/// that early is ready when it is due. `previous` is the pattern in use, for hysteresis.
pub fn throughput_plan(model: SourceRate, fps: f64, workers: usize, previous: Option<(i64, i64)>) -> (i64, i64, i64) {
    let (keep, of) = match capacity_ratio(model, fps, workers) {
        Some(r) => choose_pattern(r, previous),
        None => (1, 1),
    };
    let lead = (model.latency * fps * 1.25).ceil() as i64 + 1;
    (lead.clamp(1, 600), keep, of)
}

/// What the source delivers against what the sequence needs: the lesser of the source's rate
/// and the workers' bound (`workers / latency`, slowly smoothed), over `fps`; `None` with
/// nothing measured yet.
fn capacity_ratio(model: SourceRate, fps: f64, workers: usize) -> Option<f64> {
    let rate = model.rate.filter(|r| r.is_finite() && *r > 0.0);
    let slow = if model.latency_slow > 0.0 { model.latency_slow } else { model.latency };
    let by_workers = (slow.is_finite() && slow > 0.0).then(|| workers.max(1) as f64 / slow);
    let capacity = match (rate, by_workers) {
        (Some(a), Some(b)) => Some(a.min(b)),
        (a, b) => a.or(b),
    };
    capacity.filter(|_| fps > 0.0).map(|c| c / fps)
}

/// The frames a playback refresh at `cur` asks for: `ahead` of them from `lead` frames on.
/// Plain (`anchored` false): every `of`-th. Anchored: the frames of the pattern `keep` of `of`
/// ([`in_pattern`]), so the set does not shift with the playhead at every refresh: the frames
/// between are left out for good, not asked for a refresh later when they could not be ready
/// either.
fn planned_frames(cur: i64, dir: i64, step: i64, lead: i64, keep: i64, of: i64, ahead: i64, anchored: bool) -> Vec<i64> {
    let (step, of) = (step.max(1), of.max(1));
    let keep = keep.clamp(1, of);
    let first = cur.saturating_add(lead.saturating_mul(step).saturating_mul(dir));
    let ahead = ahead.max(0);
    if !anchored || keep == of {
        let unit = if anchored { step } else { of.saturating_mul(step) };
        return (0..ahead).map(|i| first.saturating_add(i.saturating_mul(unit).saturating_mul(dir))).collect();
    }
    let mut out = Vec::with_capacity(ahead as usize);
    let mut f = first;
    // every `of` consecutive frames hold at least `keep` of the pattern: the walk is bounded
    for _ in 0..ahead.saturating_mul(of).saturating_add(of) {
        if out.len() as i64 >= ahead {
            break;
        }
        if in_pattern(f, keep, of) {
            out.push(f);
        }
        f = f.saturating_add(step.saturating_mul(dir));
    }
    out
}

/// The pattern `FILMCRAFT_PLAYBACK_PATTERN=keep/of` pins (read once), if any.
fn forced_pattern() -> Option<(i64, i64)> {
    static FORCED: std::sync::OnceLock<Option<(i64, i64)>> = std::sync::OnceLock::new();
    *FORCED.get_or_init(|| {
        let s = std::env::var("FILMCRAFT_PLAYBACK_PATTERN").ok()?;
        let (k, n) = s.trim().split_once('/')?;
        let (k, n) = (k.trim().parse::<i64>().ok()?, n.trim().parse::<i64>().ok()?);
        (n >= 1 && (1..=n).contains(&k) && n <= SPARSEST).then_some((k, n))
    })
}

/// Frames of the pattern asked for beyond the look-ahead while a throughput-bound source's
/// reading is stale: a pipelined decoder's slots and a couple more, so its replies are
/// saturated ones.
const REMEASURE_BURST: i64 = 6;

/// How far behind the playhead a running job on a throughput-bound source is left to finish:
/// its decoder has the frame in hand (cancelling it saves nothing), and the monitor shows a late
/// frame when nothing newer is ready, looking this far back.
pub const LATE_GRACE: i64 = 6;

impl FrameServer {
    /// Settings ▸ Memory ▸ frame cache budget (bytes); evicts down to it right away.
    pub fn set_cache_budget(&self, bytes: usize) {
        let mut c = self.shared.done.lock().unwrap_or_else(|e| e.into_inner());
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
        let c = self.shared.done.lock().unwrap_or_else(|e| e.into_inner());
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
            throughput: Mutex::new(Throughput::default()),
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

    /// The default number of frame workers for this machine. `FILMCRAFT_FRAME_WORKERS` (2..=32)
    /// overrides it: a bake-off knob, not a user setting.
    pub fn default_workers() -> usize {
        if let Some(n) = std::env::var("FILMCRAFT_FRAME_WORKERS").ok().and_then(|s| s.trim().parse::<usize>().ok()) {
            return n.clamp(2, 32);
        }
        std::thread::available_parallelism().map(|n| n.get().clamp(2, 6)).unwrap_or(3)
    }

    pub fn set_context(&self, ctx: &egui::Context) {
        let mut g = self.repaint.lock().unwrap_or_else(|e| e.into_inner());
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
        std::mem::take(&mut *self.shared.records.lock().unwrap_or_else(|e| e.into_inner()))
    }

    /// Cumulative job statistics (`perf.stats`).
    pub fn stats(&self) -> FrameStats {
        self.shared.stats.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// Worker threads.
    pub fn workers(&self) -> usize {
        self.shared.workers
    }

    /// (cached images, their bytes, cached GPU plans, their bytes).
    pub fn cache_entries(&self) -> (usize, usize, usize, usize) {
        let (n, b) = {
            let c = self.shared.done.lock().unwrap_or_else(|e| e.into_inner());
            (c.map.len(), c.bytes)
        };
        let p = self.shared.plans.lock().unwrap_or_else(|e| e.into_inner());
        (n, b, p.map.len(), p.bytes)
    }

    pub fn get(&self, k: &FrameKey) -> Option<Arc<Rgba>> {
        let mut c = self.shared.done.lock().unwrap_or_else(|e| e.into_inner());
        c.clock += 1;
        let clock = c.clock;
        c.map.get_mut(k).map(|(v, s)| {
            *s = clock;
            v.clone()
        })
    }

    pub fn get_plan(&self, k: &FrameKey) -> Option<Arc<GpuPlan>> {
        self.shared.plans.lock().unwrap_or_else(|e| e.into_inner()).get(k)
    }

    /// Whether the exact frame (image or plan) is ready.
    pub fn is_ready(&self, k: &FrameKey) -> bool {
        match k.target {
            Target::SequencePlan(_) => self.shared.plans.lock().unwrap_or_else(|e| e.into_inner()).map.contains_key(k),
            _ => self.shared.done.lock().unwrap_or_else(|e| e.into_inner()).map.contains_key(k),
        }
    }

    /// Nearest cached plan at or before `frame`.
    pub fn nearest_plan(&self, key: FrameKey, max_back: i64) -> Option<(FrameKey, Arc<GpuPlan>)> {
        let mut g = self.shared.plans.lock().unwrap_or_else(|e| e.into_inner());
        (0..=max_back).find_map(|d| {
            let k = FrameKey { frame: key.frame - d, ..key };
            g.get(&k).map(|p| (k, p))
        })
    }

    /// Queue a job unless it is cached, queued or in flight.
    ///
    /// The frame on screen (`prio` 0: scrubbing, a seek) treats frames more than
    /// [`SCRUB_KEEP_S`] before it as not wanted: a decoder seeking from a keyframe skips their
    /// non-reference pictures, while nearby frames stay decoded (and cached) for scrubbing back.
    pub fn request(&self, key: FrameKey, time: Tick, scale: f32, project: &Arc<Project>, prio: u32) {
        self.request_while(key, time, scale, project, prio, false);
    }

    /// [`Self::request`] from a view that also draws while playback runs (scopes, the Reference
    /// monitor, thumbnails): with `playing` its frame is not a still someone waits for, so a
    /// source does not move it to its still-frame path (`filmcraft_media::cancel::with_still`).
    pub fn request_while(&self, key: FrameKey, time: Tick, scale: f32, project: &Arc<Project>, prio: u32, playing: bool) {
        let late = (prio == 0).then(|| Tick::from_seconds_f64(SCRUB_KEEP_S));
        self.request_job(key, time, scale, project, prio, false, late, !playing);
    }

    #[allow(clippy::too_many_arguments)]
    fn request_job(&self, key: FrameKey, time: Tick, scale: f32, project: &Arc<Project>, prio: u32, prefetch: bool, catch_up: Option<Tick>, still: bool) {
        let hit = || self.shared.stats.lock().unwrap_or_else(|e| e.into_inner()).request_hits += 1;
        if self.shared.done.lock().unwrap_or_else(|e| e.into_inner()).map.contains_key(&key)
            || self.shared.plans.lock().unwrap_or_else(|e| e.into_inner()).map.contains_key(&key)
        {
            hit();
            return;
        }
        if self.shared.in_flight.lock().unwrap_or_else(|e| e.into_inner()).iter().any(|(k, c, _)| *k == key && !c.load(Ordering::Relaxed)) {
            hit();
            return;
        }
        let mut q = self.shared.queue.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(j) = q.iter_mut().find(|j| j.key == key) {
            j.prio = j.prio.min(prio);
            j.prefetch &= prefetch;
            // playback wanting the frame too: decode it for throughput
            j.still &= still;
            // the playhead only moves on: the newest (smallest) margin
            j.catch_up = match (j.catch_up, catch_up) {
                (Some(a), Some(b)) => Some(a.min(b)),
                (a, b) => a.or(b),
            };
            drop(q);
            hit();
            return;
        }
        self.shared.stats.lock().unwrap_or_else(|e| e.into_inner()).request_misses += 1;
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
            for (k, c, prefetch) in self.shared.in_flight.lock().unwrap_or_else(|e| e.into_inner()).iter() {
                if !prefetch && same_view(k) && k.frame != key.frame {
                    c.store(true, Ordering::Relaxed);
                }
            }
        }
        q.push_back(Job { key, queued: Instant::now(), cancel: Default::default(), prefetch, catch_up, time, scale, project: project.clone(), prio, still });
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
        let model = self.source_rate();
        // A throughput-bound source (camera RAW) sets the plan when it is the slower constraint.
        let (lead, keep, of, anchored) = match (preroll, model) {
            (true, _) => (1, 1, 1, false),
            (false, None) => {
                let (l, s) = playback_plan(self.render_cost(), fps, self.shared.workers);
                (l, 1, s, false)
            }
            (false, Some(m)) => {
                let (l, s) = playback_plan(self.render_cost(), fps, self.shared.workers);
                // forward play: how late the picture is tells the plan what the reading cannot
                let (in_use, judging) = {
                    let mut t = self.shared.throughput.lock().unwrap_or_else(|e| e.into_inner());
                    (t.pattern, t.judging(true, Instant::now()))
                };
                let lag = (dir > 0 && judging).then(|| self.lag_of(key, in_use));
                let (lt, keep, of) = self.throughput_plan(m, fps, lag);
                // the sparser of the two
                let (keep, of) = if keep * s < of { (keep, of) } else { (1, s) };
                (l.max(lt), keep, of, true)
            }
        };
        if lead <= 1 {
            // the frame due now; once the clock runs, the frames before it are late
            self.request_job(key, rate.tick_of(key.frame), scale, project, 0, false, (!preroll && dir > 0).then_some(Tick::ZERO), false);
        }
        // During preroll the clock has not started: only the frames that gate start, not a
        // prefetch run that walks the rest of a LongGOP and evicts the playhead pictures.
        // A throughput-bound source whose reading has gone stale (a sparse plan keeps too few
        // requests in flight for it to measure itself; a cold start read 68 fps for a 130 fps
        // pipeline and the plan stayed at every second frame) is asked for a few more frames of
        // the pattern at once, enough to fill its decoder's slots and measure again: frames the
        // plan would ask for a moment later anyway, nothing beyond the pattern.
        let burst = if !preroll && model.is_some() && self.shared.throughput.lock().unwrap_or_else(|e| e.into_inner()).reading_stale(Instant::now()) {
            REMEASURE_BURST
        } else {
            0
        };
        let depth = lookahead(self.plan_capacity(key.target), prefetch_depth(speed) + burst);
        let ahead = if preroll { (PREROLL_FRAMES - 1).max(0) } else { depth };
        let mut prio = 1;
        for f in planned_frames(key.frame, dir, step, lead, keep, of, ahead, anchored) {
            if f >= 0 {
                // forward play: frames before the playhead are late (reverse play skips nothing)
                let late = (!preroll && dir > 0).then(|| rate.tick_of(f) - rate.tick_of(key.frame));
                self.request_job(FrameKey { frame: f, ..key }, rate.tick_of(f), scale, project, prio, true, late, false);
            }
            prio += 1;
        }
        // Queued frames beyond the planned run are stale: an ordinary source may keep a few
        // (its decoder reaches them in order anyway), a throughput-bound one only a pattern's
        // worth (its decoder would spend the look-ahead on them and evict the frames due next).
        let mut span = (lead + ahead * of) * step + if anchored { of * step } else { 26 };
        if preroll && let Some(m) = model {
            // A throughput-bound source's planned run as well, from right after the preroll
            // frames: the clock has not started, so every frame decoded now is ahead of it,
            // and the clock must not start into frames nobody asked for.
            self.shared.throughput.lock().unwrap_or_else(|e| e.into_inner()).judging(false, Instant::now());
            let (lt, keep, of) = self.throughput_plan(m, fps, None);
            let (lt, n) = (lt.min(PREROLL_FRAMES), depth);
            for f in planned_frames(key.frame, dir, step, lt, keep, of, n, true) {
                if f >= 0 {
                    self.request_job(FrameKey { frame: f, ..key }, rate.tick_of(f), scale, project, prio, true, None, false);
                }
                prio += 1;
            }
            span = span.max((lt + n * of) * step + of * step);
        }
        let (target, rev, cur) = (key.target, key.revision, key.frame);
        // Queued frames behind the playhead are late; a running one on a throughput-bound
        // source is left to finish while it is within the grace (its frame is still shown).
        let grace = if model.is_some() { LATE_GRACE } else { 0 };
        let wanted = |k: &FrameKey, behind: i64| k.target != target || (k.revision == rev && (k.frame - cur) * dir >= -behind && (k.frame - cur).abs() < span);
        // An anchored plan whose pattern just got sparser leaves the frames it no longer keeps
        // queued beyond the lead: dropped, or they would be decoded at the expense of the frames
        // the pattern does keep (a failed step up left both patterns' frames in the queue).
        let planned = |k: &FrameKey| !anchored || k.target != target || (k.frame - cur) * dir < lead * step || in_pattern(k.frame, keep, of);
        self.retain_playback(|k| wanted(k, 0) && planned(k), |k| wanted(k, grace));
    }

    /// Whether playback can start at `key`: the next [`PREROLL_FRAMES`] frames (up to `last`)
    /// are ready.
    pub fn preroll_ready(&self, key: FrameKey, speed: f64, last: i64) -> bool {
        let step = (speed.abs().max(1.0) as i64) * if speed < 0.0 { -1 } else { 1 };
        (0..PREROLL_FRAMES).map(|i| key.frame + i * step).filter(|f| *f >= 0 && *f <= last).all(|f| self.is_ready(&FrameKey { frame: f, ..key }))
    }

    /// The current per-frame render cost estimate (s), see [`playback_plan`].
    pub fn render_cost(&self) -> f64 {
        *self.shared.render_cost.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// How many frames of `target` its cache holds (see [`lookahead`]); only GPU plans are
    /// large enough to matter, the image cache's entries are 8-bit at the monitor's size.
    fn plan_capacity(&self, target: Target) -> usize {
        match target {
            Target::SequencePlan(_) => self.shared.plans.lock().unwrap_or_else(|e| e.into_inner()).capacity(),
            _ => usize::MAX,
        }
    }

    /// The plan cache's look-ahead for playback and the bytes of the plan inserted last, for
    /// `perf.stats`.
    pub fn plan_lookahead(&self) -> (i64, usize) {
        let p = self.shared.plans.lock().unwrap_or_else(|e| e.into_inner());
        (lookahead(p.capacity(), prefetch_depth(1.0)), p.last_bytes)
    }

    /// What playback learned about the throughput-bound sources it is playing, while it is
    /// playing one (see [`throughput_plan`]); `None` for ordinary sources.
    pub fn source_rate(&self) -> Option<SourceRate> {
        let t = self.shared.throughput.lock().unwrap_or_else(|e| e.into_inner());
        t.active.then_some(t.model)
    }

    /// [`throughput_plan`] for the model, remembering the pattern for the next refresh.
    fn throughput_plan(&self, model: SourceRate, fps: f64, lag: Option<i64>) -> (i64, i64, i64) {
        let mut t = self.shared.throughput.lock().unwrap_or_else(|e| e.into_inner());
        let (lead, keep, of) = throughput_plan(model, fps, self.shared.workers, t.pattern);
        let (keep, of) = t.settle((keep, of), lag, Instant::now());
        // `FILMCRAFT_PLAYBACK_PATTERN=keep/of` pins the pattern (a bake-off knob: what a pipeline
        // does under a given demand, whatever it reads its capacity as)
        let (keep, of) = forced_pattern().unwrap_or((keep, of));
        t.pattern = Some((keep, of));
        (lead, keep, of)
    }

    /// How many frames the newest ready picture of the pattern in use is behind `key` (0: `key`
    /// itself is ready), up to `LATE_GRACE × 4` when nothing that recent is, for
    /// [`Throughput::settle`]. Only the pattern's own frames count: after a step up, the frames
    /// decoded for the sparser pattern before it keep the picture looking on time for as long
    /// as they were prefetched (a second), while the denser pattern's frames fall behind unseen.
    fn lag_of(&self, key: FrameKey, pattern: Option<(i64, i64)>) -> i64 {
        let max_back = LATE_GRACE * 4;
        let (keep, of) = pattern.unwrap_or((1, 1));
        let counts = |d: i64| in_pattern(key.frame - d, keep, of);
        match key.target {
            Target::SequencePlan(_) => {
                let plans = self.shared.plans.lock().unwrap_or_else(|e| e.into_inner());
                (0..=max_back).find(|d| counts(*d) && plans.map.contains_key(&FrameKey { frame: key.frame - d, ..key })).unwrap_or(max_back)
            }
            _ => (0..=max_back).find(|d| counts(*d) && self.get(&FrameKey { frame: key.frame - d, ..key }).is_some()).unwrap_or(max_back),
        }
    }

    /// The plan playback at `fps` would use now: `(lead, keep, of)` (see [`playback_plan`] and
    /// [`throughput_plan`]), for `perf.stats`; it does not change the pattern in use.
    pub fn plan_for(&self, fps: f64) -> (i64, i64, i64) {
        let (l, s) = playback_plan(self.render_cost(), fps, self.shared.workers);
        match self.source_rate() {
            Some(m) => {
                // the pattern in use (settled on the picture, see [`Throughput::settle`]), not
                // only what the reading would choose
                let previous = self.shared.throughput.lock().unwrap_or_else(|e| e.into_inner()).pattern;
                let (lt, keep, of) = throughput_plan(m, fps, self.shared.workers, previous);
                let (keep, of) = previous.unwrap_or((keep, of));
                let (keep, of) = if keep * s < of { (keep, of) } else { (1, s) };
                (l.max(lt), keep, of)
            }
            None => (l, 1, s),
        }
    }

    /// Drop queued jobs that fail `keep` (e.g. stale revisions or frames far from the playhead).
    /// Jobs already running for such frames are cancelled: a worker blocked behind a decoder
    /// would otherwise go on to decode a frame playback has passed, possibly from its keyframe.
    pub fn retain_queue(&self, keep: impl Fn(&FrameKey) -> bool) {
        self.retain_playback(&keep, &keep);
    }

    /// [`retain_queue`](Self::retain_queue) with its own rule for running jobs.
    fn retain_playback(&self, keep_queued: impl Fn(&FrameKey) -> bool, keep_running: impl Fn(&FrameKey) -> bool) {
        self.shared.queue.lock().unwrap_or_else(|e| e.into_inner()).retain(|j| keep_queued(&j.key));
        for (k, c, _) in self.shared.in_flight.lock().unwrap_or_else(|e| e.into_inner()).iter() {
            if !keep_running(k) {
                c.store(true, Ordering::Relaxed);
            }
        }
    }

    /// Playback stopped: drop its queued prefetch jobs and cancel the running ones (left alone,
    /// they would keep the workers busy, e.g. rendering effects for frames nobody will see).
    pub fn stop_prefetch(&self) {
        self.shared.queue.lock().unwrap_or_else(|e| e.into_inner()).retain(|j| !j.prefetch);
        self.shared.plans.lock().unwrap_or_else(|e| e.into_inner()).stale_prefetch();
        for (_, c, prefetch) in self.shared.in_flight.lock().unwrap_or_else(|e| e.into_inner()).iter() {
            if *prefetch {
                c.store(true, Ordering::Relaxed);
            }
        }
    }

    pub fn queue_len(&self) -> usize {
        self.shared.queue.lock().unwrap_or_else(|e| e.into_inner()).len()
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
                let mut q = self.shared.queue.lock().unwrap_or_else(|e| e.into_inner());
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
            let mut q = self.shared.queue.lock().unwrap_or_else(|e| e.into_inner());
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
        let mut q = self.shared.queue.lock().unwrap_or_else(|e| e.into_inner());
        self.shared.stop.store(true, Ordering::Relaxed);
        q.clear();
        drop(q);
        for (_, c, _) in self.shared.in_flight.lock().unwrap_or_else(|e| e.into_inner()).iter() {
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
            let mut q = sh.queue.lock().unwrap_or_else(|e| e.into_inner());
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
                q = sh.cv.wait(q).unwrap_or_else(|e| e.into_inner());
            }
        };
        // A panic while decoding or rendering one frame (bad media, a decoder bug) must not take
        // the worker down with it: a dead worker leaves its frame "in flight" forever and the
        // monitors blank. Log it, forget the job and keep serving.
        let ran = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| run_job(&sh, &job, &pool, &services, &previews)));
        if ran.is_err() {
            sh.in_flight.lock().unwrap_or_else(|e| e.into_inner()).retain(|(k, c, _)| !(*k == job.key && Arc::ptr_eq(c, &job.cancel)));
            sh.stats.lock().unwrap_or_else(|e| e.into_inner()).failed += 1;
            log::error!("frame job {:?} panicked; the worker continues", job.key);
        }
        if let Some(f) = repaint.lock().unwrap_or_else(|e| e.into_inner()).as_ref() {
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
        let mut in_flight = sh.in_flight.lock().unwrap_or_else(|e| e.into_inner());
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
    DELIVERY.with(|d| d.set(None));
    DECODED.with(|d| d.set(false));
    PROFILING.with(|p| p.set(profiling));
    // A cancelled job may have missed layers (its source gave up): its result is not cached.
    // A job whose media bytes were still loading (web) rendered with missing layers: like a
    // cancelled one, its result is not cached (the caller retries it).
    let _ = filmcraft_media::pending::take();
    let mut loading = false;
    // (texel preparation, plan-cache insert) of a GPU plan job, for `perf.stats`
    let mut stages = (Duration::ZERO, Duration::ZERO);
    let preview = filmcraft_media::cancel::with_cancel(&job.cancel, || {
        filmcraft_media::cancel::with_catch_up(job.catch_up, || {
            filmcraft_media::cancel::with_draft(job.key.draft, || {
                filmcraft_media::cancel::with_background(is_background(job), || {
                    filmcraft_media::cancel::with_still(job.still, || {
                        if let Target::SequencePlan(seq) = job.key.target {
                            let (plan, pv) = plan_job(job, seq, pool, services, previews);
                            loading = filmcraft_media::pending::take();
                            if !job.cancel.load(Ordering::Relaxed) && !loading {
                                // Convert texels for upload here, not on the UI thread when the frame is shown.
                                let t0 = Instant::now();
                                let prepared = filmcraft_gpu::prepare(&plan);
                                let t1 = Instant::now();
                                sh.plans.lock().unwrap_or_else(|e| e.into_inner()).insert(job.key, GpuPlan { plan, prepared });
                                stages = (t1.saturating_duration_since(t0), t1.elapsed());
                            }
                            pv
                        } else {
                            let (img, pv) = render_job(job, pool, services, previews);
                            loading = filmcraft_media::pending::take();
                            if !job.cancel.load(Ordering::Relaxed) && !loading {
                                sh.done.lock().unwrap_or_else(|e| e.into_inner()).insert(job.key, Arc::new(img));
                            }
                            pv
                        }
                    })
                })
            })
        })
    });
    let cancelled = job.cancel.load(Ordering::Relaxed);
    sh.in_flight.lock().unwrap_or_else(|e| e.into_inner()).retain(|(k, c, _)| !(*k == job.key && Arc::ptr_eq(c, &job.cancel)));
    if job.prefetch {
        // Work beyond fetching source frames (decoding is sequential per source and is
        // not saved by skipping frames, so it is left out of the estimate). A cancelled job
        // only tells that the work takes at least this long.
        //
        // Sources whose frames decode independently are the exception, and a per-job cost is
        // the wrong model for them too (tried on a 6K camera RAW source under a 59.94 fps
        // sequence: the job time includes queueing behind the other workers and the plan's lead
        // had no slack, so every planned frame arrived late). They get a rate and a latency of
        // their own ([`SourceRate`], [`throughput_plan`]).
        let cost = started.elapsed().saturating_sub(SOURCE_TIME.with(|s| s.get()).0).as_secs_f64();
        let mut c = sh.render_cost.lock().unwrap_or_else(|e| e.into_inner());
        if !cancelled || cost > *c {
            *c = if *c == 0.0 { cost } else { *c * 0.8 + cost * 0.2 };
        }
        drop(c);
        let took = (!cancelled).then(|| started.elapsed().as_secs_f64());
        sh.throughput.lock().unwrap_or_else(|e| e.into_inner()).job(DELIVERY.with(Cell::take), DECODED.with(Cell::get), took);
    }
    {
        let job_wall = started.elapsed();
        let source = SOURCE_TIME.with(|s| s.get()).0;
        let (src_ms, render_ms) = (source.as_secs_f64() * 1e3, job_wall.saturating_sub(source).as_secs_f64() * 1e3);
        let mut st = sh.stats.lock().unwrap_or_else(|e| e.into_inner());
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
        st.recent.push_back((src_ms as f32, render_ms as f32, stages.0.as_secs_f32() * 1e3, stages.1.as_secs_f32() * 1e3));
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
        sh.records.lock().unwrap_or_else(|e| e.into_inner()).push(rec);
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
        PlaybackMeter, SPARSEST, SourceRate, Target, Throughput, background_work, choose_pattern, eviction_key, in_pattern, lookahead, next_sparser,
        planned_frames, playback_plan, stopped, throughput_plan,
    };
    use filmcraft_media::FrameDelivery;
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
    fn playback_plan_skips_frames_only_when_workers_cannot_keep_up() {
        // decode-only / light frames: every frame from the playhead on
        assert_eq!(playback_plan(0.0, 24.0, 6), (1, 1));
        assert_eq!(playback_plan(0.15, 24.0, 6), (1, 1));
        // 0.5 s of effects per frame at 24 fps on 6 workers: 12 frames pass while one renders,
        // so start 12 ahead, and every 3rd frame is what 6 workers can finish
        assert_eq!(playback_plan(0.5, 24.0, 6), (12, 3));
    }

    #[test]
    fn throughput_plan_asks_for_what_the_source_delivers_early_enough() {
        let plan = |rate: Option<f64>, latency, fps| throughput_plan(SourceRate { rate, latency, latency_slow: latency }, fps, 6, None);
        // a 25 fps camera RAW decoder under 59.94 fps, 240 ms from a job's start to its frame:
        // two of every five frames (24 fps, evenly), started 19 frames ahead (240 ms × 1.25 + 1)
        assert_eq!(plan(Some(25.0), 0.24, 59.94), (19, 2, 5));
        // keeping up: every frame, still started early enough
        assert_eq!(plan(Some(30.0), 0.1, 29.97), (5, 1, 1));
        // short of the rate: three of four frames (18 fps), not every second one
        assert_eq!(plan(Some(23.0), 0.1, 24.0), (5, 3, 4));
        assert_eq!(plan(Some(31.0), 0.1, 60.0), (9, 1, 2));
        assert_eq!(plan(Some(20.5), 0.1, 60.0), (9, 1, 3));
        // six workers each holding a job for 330 ms sustain 18 fps whatever the decoder says
        assert_eq!(plan(Some(25.0), 0.33, 59.94), (26, 1, 4));
        assert_eq!(throughput_plan(SourceRate { rate: Some(25.0), latency: 0.33, latency_slow: 0.33 }, 59.94, 8, None), (26, 2, 5));
        // the workers' bound follows the slow average: a spike in the fast one sizes the lead only
        assert_eq!(throughput_plan(SourceRate { rate: Some(150.0), latency: 0.06, latency_slow: 0.04 }, 119.88, 6, Some((1, 1))), (10, 1, 1));
        // nothing measured yet: every frame from the playhead on
        assert_eq!(plan(None, 0.0, 59.94), (1, 1, 1));
        // no rate from the source yet, but the workers' bound already
        assert_eq!(plan(None, 0.24, 59.94).2, 5);
        // hostile values stay bounded
        assert_eq!(plan(Some(0.001), 1e9, 60.0), (600, 1, 64));
        assert_eq!(plan(Some(-1.0), f64::NAN, 60.0), (1, 1, 1));
    }

    #[test]
    fn pattern_choice_has_hysteresis() {
        // the densest pattern the share allows
        assert_eq!(choose_pattern(1.2, None), (1, 1));
        assert_eq!(choose_pattern(0.8, None), (3, 4));
        assert_eq!(choose_pattern(0.7, None), (2, 3));
        assert_eq!(choose_pattern(0.45, None), (2, 5), "a 27 fps pipeline under 59.94p shows 24 fps");
        assert_eq!(choose_pattern(0.38, None), (3, 8));
        assert_eq!(choose_pattern(0.34, None), (1, 3));
        assert_eq!(choose_pattern(0.1, None), (1, 10));
        assert_eq!(choose_pattern(1e-9, None), (1, 64));
        // falling behind by more than 10 %: sparser; denser only with 5 % to spare
        assert_eq!(choose_pattern(0.29, Some((1, 3))), (1, 4));
        assert_eq!(choose_pattern(0.31, Some((1, 3))), (1, 3));
        assert_eq!(choose_pattern(0.93, Some((1, 1))), (1, 1), "keeping up reads as ~1.0");
        assert_eq!(choose_pattern(0.34, Some((1, 4))), (1, 4));
        assert_eq!(choose_pattern(0.36, Some((1, 4))), (1, 3));
        assert_eq!(choose_pattern(1.0, Some((3, 4))), (3, 4));
        assert_eq!(choose_pattern(1.04, Some((3, 4))), (3, 4));
        assert_eq!(choose_pattern(1.06, Some((3, 4))), (1, 1));
        assert_eq!(choose_pattern(0.72, Some((3, 4))), (3, 4));
        assert_eq!(choose_pattern(0.67, Some((3, 4))), (2, 3));
        assert_eq!(choose_pattern(1.25, Some((3, 4))), (1, 1));
        assert_eq!(choose_pattern(f64::NAN, Some((1, 3))), (1, 3));
        // the next sparser pattern, then every n-th, bounded
        assert_eq!(next_sparser((1, 1)), (3, 4));
        assert_eq!(next_sparser((1, 2)), (2, 5));
        assert_eq!(next_sparser((3, 8)), (1, 3));
        assert_eq!(next_sparser((1, 4)), (1, 5));
        assert_eq!(next_sparser((1, 9)), (1, 10));
        assert_eq!(next_sparser((1, SPARSEST)), (1, SPARSEST));
        // the pattern spreads its frames evenly and the same way for any playhead
        let kept = |keep, of| (0..12).filter(|f| in_pattern(*f, keep, of)).collect::<Vec<i64>>();
        assert_eq!(kept(1, 3), vec![0, 3, 6, 9]);
        assert_eq!(kept(3, 4), vec![0, 2, 3, 4, 6, 7, 8, 10, 11]);
        assert_eq!(kept(2, 3), vec![0, 2, 3, 5, 6, 8, 9, 11]);
        assert_eq!(kept(2, 5), vec![0, 3, 5, 8, 10], "gaps of three and two frames, alternating");
        assert_eq!(kept(1, 1).len(), 12);
        assert!(in_pattern(i64::MAX, 3, 4) || !in_pattern(i64::MAX, 3, 4), "no overflow");
    }

    /// Refreshes every 50 ms from `from` to `to` (ms), each with `lag`; the pattern after the
    /// last. The pattern in use follows what `settle` returns, as the frame server's does.
    fn drive(t: &mut Throughput, chosen: (i64, i64), lag: i64, t0: super::Instant, from: u64, to: u64) -> (i64, i64) {
        let mut out = t.pattern.unwrap_or(chosen);
        let mut ms = from;
        while ms <= to {
            out = t.settle(chosen, Some(lag), t0 + std::time::Duration::from_millis(ms));
            t.pattern = Some(out);
            ms += 50;
        }
        out
    }

    #[test]
    fn pattern_backs_off_when_the_picture_runs_late_and_climbs_back_after_slack() {
        let t0 = super::Instant::now();
        let mut t = Throughput::default();
        // the first choice stands; nothing shown yet keeps what is in use unless sparser
        assert_eq!(t.settle((1, 1), None, t0), (1, 1));
        t.pattern = Some((1, 1));
        assert_eq!(t.settle((1, 1), None, t0), (1, 1));
        assert_eq!(t.settle((3, 4), None, t0), (3, 4));
        t.pattern = Some((1, 1));
        // every frame, the reading still says so, but the picture is two frames late: once a
        // half-second window of that closes, three of four
        assert_eq!(drive(&mut t, (1, 1), 2, t0, 0, 450), (1, 1));
        assert_eq!(drive(&mut t, (1, 1), 2, t0, 500, 500), (3, 4));
        // on time again: no denser within the cooldown (4 s from 500 ms)
        assert_eq!(drive(&mut t, (1, 1), 0, t0, 550, 4400), (3, 4));
        assert_eq!(drive(&mut t, (1, 1), 0, t0, 4450, 4600), (1, 1));
        // the step up fails within the hold: back to three of four, and the next try waits twice
        // as long (8 s), then 16 s
        assert_eq!(drive(&mut t, (1, 1), 2, t0, 4650, 5200), (3, 4));
        assert_eq!(drive(&mut t, (1, 1), 0, t0, 5250, 12950), (3, 4), "8 s cooldown from 5 s");
        assert_eq!(drive(&mut t, (1, 1), 0, t0, 13000, 13300), (1, 1));
        assert_eq!(drive(&mut t, (1, 1), 2, t0, 13350, 14000), (3, 4), "late in the window closing at 14 s");
        assert_eq!(drive(&mut t, (1, 1), 0, t0, 14050, 29950), (3, 4), "16 s cooldown from 14 s");
        assert_eq!(drive(&mut t, (1, 1), 0, t0, 30000, 30200), (1, 1));
        // held for three seconds: a later late episode is an ordinary back-off (base cooldown)
        assert_eq!(drive(&mut t, (1, 1), 0, t0, 30250, 34000), (1, 1));
        assert_eq!(drive(&mut t, (1, 1), 2, t0, 34050, 34500), (3, 4));
        assert_eq!(drive(&mut t, (1, 1), 0, t0, 34550, 38450), (3, 4));
        assert_eq!(drive(&mut t, (1, 1), 0, t0, 38500, 38750), (1, 1), "4 s cooldown from 34.5 s");
        // a lag that flickers (most frames land, one in three is late) still counts as late
        let mut t = Throughput { pattern: Some((1, 2)), ..Default::default() };
        let mut ms = 0;
        let mut out = (1, 2);
        while ms <= 600 {
            out = t.settle((1, 2), Some(if ms % 150 == 0 { 4 } else { 0 }), t0 + std::time::Duration::from_millis(ms));
            t.pattern = Some(out);
            ms += 50;
        }
        assert_eq!(out, (2, 5), "the next sparser pattern after every second frame");
        // sparser from the reading is always allowed, and a lag within the pattern's step is fine
        let mut t = Throughput { pattern: Some((1, 3)), ..Default::default() };
        assert_eq!(t.settle((1, 4), Some(0), t0), (1, 4));
        t.pattern = Some((1, 3));
        assert_eq!(drive(&mut t, (1, 3), 3, t0, 0, 1200), (1, 3));
        assert_eq!(drive(&mut t, (1, 3), 5, t0, 1250, 1800), (1, 4));
        // right after backing off (at 1.5 s) the picture is still late: not judged for two seconds
        assert_eq!(drive(&mut t, (1, 4), 9, t0, 1850, 3450), (1, 4));
        assert_eq!(drive(&mut t, (1, 4), 9, t0, 3500, 5450), (1, 5));
        assert_eq!(drive(&mut t, (1, 5), 9, t0, 5500, 5600), (1, 6));
        // the clock: not judged for two seconds after it starts, and a preroll resets that
        let mut t = Throughput::default();
        assert!(!t.judging(false, t0));
        assert!(!t.judging(true, t0));
        assert!(!t.judging(true, t0 + std::time::Duration::from_millis(1900)));
        assert!(t.judging(true, t0 + std::time::Duration::from_millis(2000)));
        assert!(!t.judging(false, t0 + std::time::Duration::from_millis(2100)));
        assert!(!t.judging(true, t0 + std::time::Duration::from_millis(2200)));
    }

    #[test]
    fn a_stale_reading_asks_for_a_burst_and_never_a_denser_pattern() {
        use std::time::Duration;
        let t0 = super::Instant::now();
        let mut t = Throughput::default();
        // nothing reported: nothing to refresh
        assert!(!t.reading_stale(t0));
        t.job(Some(FrameDelivery { independent: true, rate: Some(25.0), fresh: true }), true, Some(0.2));
        let now = super::Instant::now();
        assert!(!t.reading_stale(now));
        assert!(t.reading_stale(now + Duration::from_millis(2100)));
        // a source that reports no rate of its own is measured by playback: never stale
        let mut u = Throughput::default();
        u.job(Some(FrameDelivery { independent: true, rate: None, fresh: false }), true, Some(0.05));
        assert!(!u.reading_stale(super::Instant::now() + Duration::from_secs(60)));
        // the pattern in use is never stepped up beyond the reading's choice, however long on time
        let mut t = Throughput { pattern: Some((1, 2)), ..Default::default() };
        assert_eq!(drive(&mut t, (1, 2), 0, t0, 0, 12000), (1, 2));
        t.pattern = Some((1, 1));
        assert_eq!(drive(&mut t, (1, 1), 0, t0, 12050, 16000), (1, 1), "nothing denser than every frame");
    }

    #[test]
    fn anchored_plan_keeps_the_same_frames_as_the_playhead_moves() {
        let frames = |cur, dir, anchored| planned_frames(cur, dir, 1, 19, 1, 3, 4, anchored);
        // multiples of the stride: a refresh one frame later asks for the same frames
        assert_eq!(frames(100, 1, true), vec![120, 123, 126, 129]);
        assert_eq!(frames(101, 1, true), vec![120, 123, 126, 129]);
        assert_eq!(frames(102, 1, true), vec![123, 126, 129, 132]);
        // the plain plan shifts with every refresh (every frame gets asked for eventually)
        assert_eq!(frames(100, 1, false), vec![119, 122, 125, 128]);
        assert_eq!(frames(101, 1, false), vec![120, 123, 126, 129]);
        // reverse play anchors the same way
        assert_eq!(frames(100, -1, true), vec![81, 78, 75, 72]);
        assert_eq!(frames(99, -1, true), vec![78, 75, 72, 69]);
        // 2× speed: the stride is in played frames
        assert_eq!(planned_frames(100, 1, 2, 19, 1, 3, 3, true), vec![138, 144, 150]);
        assert_eq!(planned_frames(0, 1, 1, 1, 1, 1, 3, true), vec![1, 2, 3]);
        assert!(planned_frames(i64::MAX, 1, 1, 1, 1, 2, 2, true).len() <= 2, "no overflow, and the walk ends");
        // three of four: the same frames whatever the playhead, one of every four left out
        assert_eq!(planned_frames(100, 1, 1, 19, 3, 4, 4, true), vec![119, 120, 122, 123]);
        assert_eq!(planned_frames(101, 1, 1, 19, 3, 4, 4, true), vec![120, 122, 123, 124]);
        assert_eq!(planned_frames(100, -1, 1, 19, 3, 4, 4, true), vec![80, 79, 78, 76]);
    }

    #[test]
    fn throughput_model_follows_the_source_and_clears_for_ordinary_ones() {
        let raw = Some(FrameDelivery { independent: true, rate: Some(25.0), fresh: true });
        let mut t = Throughput::default();
        assert!(!t.active);
        t.job(raw, true, Some(0.2));
        assert!(t.active);
        assert_eq!(t.model, SourceRate { rate: Some(25.0), latency: 0.2, latency_slow: 0.2 });
        // latency rises fast and settles slowly; a cancelled job (no time) still keeps it active
        t.job(raw, true, Some(0.4));
        assert!((t.model.latency - 0.3).abs() < 1e-9);
        assert!((t.model.latency_slow - 0.21).abs() < 1e-9);
        t.job(raw, true, None);
        assert!(t.active);
        // a source without a rate of its own: measured from completions once there are enough
        let mut u = Throughput::default();
        for _ in 0..5 {
            u.job(Some(FrameDelivery { independent: true, rate: None, fresh: false }), true, Some(0.05));
        }
        assert_eq!(u.model.rate, None);
        u.job(Some(FrameDelivery { independent: true, rate: None, fresh: false }), true, Some(0.05));
        assert!(u.model.rate.is_some_and(|r| r > 100.0), "{:?}", u.model);
        // frames that decode nothing (past the end of the sequence while it loops, a gap, a
        // rendered segment) say nothing about what is playing: the model stays
        for _ in 0..(4 * Throughput::MISSES) {
            t.job(None, false, Some(0.001));
        }
        assert!(t.active);
        // playback moves on to a sequence of ordinary sources: a dozen jobs clear the model
        for i in 0..Throughput::MISSES {
            assert!(t.active, "{i}");
            t.job(None, true, Some(0.01));
        }
        assert!(!t.active);
        assert_eq!(t.model, SourceRate::default());
        // a GOP source's delivery is not throughput-bound either
        t.job(Some(FrameDelivery::default()), true, Some(0.01));
        assert!(!t.active);
    }

    #[test]
    fn lookahead_fits_the_plan_cache() {
        // 1080p plans by the hundred: the prefetch depth as it is
        assert_eq!(lookahead(128, 14), 14);
        assert_eq!(lookahead(usize::MAX, 24), 24);
        // eight 6K half-resolution plans: six ahead, the frame on screen and one behind
        assert_eq!(lookahead(8, 14), 6);
        // never less than two, whatever the plan costs
        assert_eq!(lookahead(1, 14), 2);
        assert_eq!(lookahead(0, 14), 2);
    }

    #[test]
    fn prefetched_frames_outlive_the_frames_already_shown() {
        // (name, last looked up, inserted): the playhead's frame is looked up every refresh,
        // the frames behind it were, the frames ahead never
        let mut entries = [("ahead-near", 5, 5), ("ahead-far", 8, 8), ("shown-now", 9, 4), ("shown-before", 7, 3), ("shown-long-ago", 2, 1)];
        entries.sort_by_key(|e| eviction_key(e.1, e.2, 10));
        let order: Vec<&str> = entries.iter().map(|e| e.0).collect();
        assert_eq!(order, ["shown-long-ago", "shown-before", "shown-now", "ahead-near", "ahead-far"]);
        // long after: a frame nobody looked up (skipped, or of an old revision) is just old
        let mut entries = [("never-looked-up", 5, 5), ("shown-now", 3000, 9)];
        entries.sort_by_key(|e| eviction_key(e.1, e.2, 3000));
        assert_eq!(entries.map(|e| e.0), ["never-looked-up", "shown-now"]);
    }

    /// Playback prefetched frames ahead and stopped; a scrub then shows new frames. Over budget,
    /// the stale prefetched plans go first, not the frames the scrub just showed.
    #[test]
    fn prefetched_plans_go_first_once_playback_stopped() {
        // (last looked up, inserted) at clock 100: two prefetched, two shown by the scrub
        let mut v = [(95, 95), (98, 98), (99, 96), (100, 97)];
        for e in &mut v {
            e.1 = stopped(e.0, e.1);
        }
        let mut order: Vec<usize> = (0..v.len()).collect();
        order.sort_by_key(|&i| eviction_key(v[i].0, v[i].1, 100));
        assert_eq!(order, [0, 1, 2, 3], "the prefetched (0, 1) before the shown (2, 3)");
        assert_eq!(stopped(99, 96), 96, "a plan looked up keeps its place");
        assert_eq!(stopped(3, 3), 0, "no underflow");
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
            still: false,
        };
        // what `Drop` sets before it cancels the jobs in flight
        server.shared.stop.store(true, Ordering::Relaxed);
        run_job(&server.shared, &job, &server.pool, &server.services, &server.previews);
        assert!(job.cancel.load(Ordering::Relaxed), "the job ran without its cancel flag");
        assert!(!server.is_ready(&job.key), "a cancelled job's frame is not cached");
    }
}
