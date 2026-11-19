//! Opt-in per-frame timing log for timeline playback and scrubbing.
//!
//! Off by default. Turn it on with `FILMCRAFT_FRAME_TIMING=1` in the environment of the desktop
//! app, or at run time with the UI command `perf.frameTiming {"on": true}` (control channel, CLI
//! `--bridge`, MCP `command_run`); without `on` the command toggles it. Lines go to the app's log
//! (`<data dir>/Logs/filmcraft.log` and standard error) under the target `filmcraft::frame_timing`.
//!
//! Three kinds of lines, all times in milliseconds:
//!
//! - `pass`: one per UI refresh while playing, scrubbing (the playhead moved) or waiting for the
//!   Program monitor's exact frame. `gap` is the time between the end of the previous UI pass and
//!   the start of this one: egui tessellation, the wgpu paint of the window, present and the wait
//!   for vsync (or for a repaint request). `logic` covers the logic pass, of which `audio` is the
//!   audio-clock poll (`advance_playback`); `ui` the drawing pass, of which `sched` is frame
//!   scheduling, `cpufx` / `upload` / `record` / `submit` the GPU compositor (CPU-side: effects it
//!   cannot run, texture writes, command recording, `queue.submit`), `eguitex` handing its output to
//!   egui and `cputex` uploading a CPU-composited frame; `scrub` is scrub audio.
//! - `job`: one per finished frame job on a worker. `wait` is time in the queue; `source` getting
//!   source frames, of which the GOP cache part is `lock` (waiting for a source another job is
//!   decoding), `hit` / `miss`, `seeks`, `samples` fed and `decode` (the decode pool's wall time,
//!   hardware or software); `plan` the rest of planning / CPU rendering; `prepare` the half-float
//!   conversion for upload (`filmcraft_gpu::prepare`).
//! - `summary`: once a second while lines are being written: p50 / p95 / max of each stage, frames
//!   shown and dropped, decode cache counters and hardware vs software pictures.
//!
//! Measuring costs a few clock reads per pass and per job; nothing is measured or kept while off.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, PoisonError};
use std::time::Duration;

use web_time::Instant;

/// The environment variable that turns the log on at startup (`1`, `true`, `on`, `yes`).
pub const ENV: &str = "FILMCRAFT_FRAME_TIMING";
/// The log target of every line.
pub const TARGET: &str = "filmcraft::frame_timing";

static ENABLED: AtomicBool = AtomicBool::new(false);
/// Bumped every time the log is turned on: a [`PassTimer`] from an earlier session starts over.
static GENERATION: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Whether the log is on.
pub fn enabled() -> bool {
    ENABLED.load(Ordering::Relaxed)
}

/// Turn the log on or off (also the GOP caches' per-request trace).
pub fn set_enabled(on: bool) {
    let was = ENABLED.swap(on, Ordering::Relaxed);
    filmcraft_codecs::gop::trace::set_enabled(on);
    if was != on {
        if on {
            GENERATION.fetch_add(1, Ordering::Relaxed);
        }
        JOBS.lock().unwrap_or_else(PoisonError::into_inner).clear();
        log::info!(target: TARGET, "frame timing {}", if on { "on" } else { "off" });
    }
}

/// Whether an environment value turns the log on.
pub fn env_value_on(v: &str) -> bool {
    matches!(v.trim().to_ascii_lowercase().as_str(), "1" | "true" | "on" | "yes")
}

/// Turn the log on when [`ENV`] says so.
pub fn init_from_env() {
    if std::env::var(ENV).is_ok_and(|v| env_value_on(&v)) {
        set_enabled(true);
    }
}

/// The start of a timed stretch, or None while the log is off (nothing is measured then).
pub fn start() -> Option<Instant> {
    enabled().then(Instant::now)
}

fn ms(d: Duration) -> f32 {
    d.as_secs_f32() * 1e3
}

/// The stages of a UI pass.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stage {
    Gap,
    Logic,
    AudioClock,
    Ui,
    Schedule,
    CpuFx,
    Upload,
    Record,
    Submit,
    EguiTexture,
    CpuTexture,
    ScrubAudio,
}

impl Stage {
    pub const ALL: [Stage; 12] = [
        Stage::Gap,
        Stage::Logic,
        Stage::AudioClock,
        Stage::Ui,
        Stage::Schedule,
        Stage::CpuFx,
        Stage::Upload,
        Stage::Record,
        Stage::Submit,
        Stage::EguiTexture,
        Stage::CpuTexture,
        Stage::ScrubAudio,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Stage::Gap => "gap",
            Stage::Logic => "logic",
            Stage::AudioClock => "audio",
            Stage::Ui => "ui",
            Stage::Schedule => "sched",
            Stage::CpuFx => "cpufx",
            Stage::Upload => "upload",
            Stage::Record => "record",
            Stage::Submit => "submit",
            Stage::EguiTexture => "eguitex",
            Stage::CpuTexture => "cputex",
            Stage::ScrubAudio => "scrub",
        }
    }

    fn index(self) -> usize {
        Stage::ALL.iter().position(|s| *s == self).unwrap_or(0)
    }
}

/// What the Program monitor showed in a pass.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Shown {
    /// The frame due.
    pub frame: i64,
    /// The frame whose picture was on screen: the one due, a nearby one held while it renders, or
    /// None (no picture yet).
    pub on_screen: Option<i64>,
    /// The picture on screen is the exact one due (same frame, size and revision).
    pub exact: bool,
}

impl Shown {
    /// `f=12 exact`, `f=12 held=11`, `f=12 none`.
    pub fn text(&self) -> String {
        match (self.exact, self.on_screen) {
            (true, _) => format!("f={} exact", self.frame),
            (false, Some(s)) => format!("f={} held={s}", self.frame),
            (false, None) => format!("f={} none", self.frame),
        }
    }
}

/// The timings of the current UI pass and the summary window (owned by the app, UI thread only).
#[derive(Debug, Default)]
pub struct PassTimer {
    stages: [Duration; Stage::ALL.len()],
    upload_bytes: u64,
    /// The logic pass began (start of this refresh).
    began: Option<Instant>,
    /// The previous refresh began, and its UI pass ended.
    prev_began: Option<Instant>,
    prev_end: Option<Instant>,
    shown: Option<Shown>,
    /// What asked for this pass (egui's repaint causes, `file:line`).
    causes: Vec<String>,
    last_playhead: Option<i64>,
    passes: u64,
    window: Window,
    /// The [`GENERATION`] this timer measures for.
    generation: u64,
}

/// Samples of the passes and jobs since the last summary.
#[derive(Debug, Default)]
struct Window {
    started: Option<Instant>,
    stages: Vec<Vec<f32>>,
    totals: Vec<f32>,
    intervals: Vec<f32>,
    upload_bytes: u64,
    passes: u32,
    exact: u32,
    meter: (u64, u64),
    gop: filmcraft_codecs::GopStats,
    hw_frames: u64,
}

/// One finished frame job (worker side).
#[derive(Clone, Copy, Debug)]
pub struct JobSample {
    pub frame: i64,
    pub prio: u32,
    pub prefetch: bool,
    pub cancelled: bool,
    pub preview: bool,
    pub wait: Duration,
    pub total: Duration,
    pub source: Duration,
    pub prepare: Duration,
    pub gop: filmcraft_codecs::gop::trace::RequestTrace,
    /// When the job finished: a summary takes only the jobs of its own interval.
    pub finished: Instant,
}

impl JobSample {
    /// Planning / CPU rendering: the job without its source fetches and upload preparation.
    pub fn plan(&self) -> Duration {
        self.total.saturating_sub(self.source).saturating_sub(self.prepare)
    }

    pub fn line(&self) -> String {
        let g = &self.gop;
        format!(
            "job f={} prio={}{}{}{} wait={:.2} total={:.2} source={:.2} [lock={:.2} hit={} miss={} seeks={} samples={} decode={:.2}] plan={:.2} prepare={:.2}",
            self.frame,
            self.prio,
            if self.prefetch { " prefetch" } else { "" },
            if self.preview { " preview" } else { "" },
            if self.cancelled { " cancelled" } else { "" },
            ms(self.wait),
            ms(self.total),
            ms(self.source),
            ms(g.lock_wait),
            g.hits,
            g.misses,
            g.seeks,
            g.samples,
            ms(g.decode),
            ms(self.plan()),
            ms(self.prepare),
        )
    }
}

/// Jobs finished since the last summary, newest last (bounded: a stalled UI thread can't make it
/// grow; when full, the oldest goes).
static JOBS: Mutex<std::collections::VecDeque<JobSample>> = Mutex::new(std::collections::VecDeque::new());
const MAX_JOBS: usize = 4096;

/// Log a finished frame job and keep it for the next summary (called on workers while enabled).
pub fn record_job(j: JobSample) {
    log::info!(target: TARGET, "{}", j.line());
    let mut jobs = JOBS.lock().unwrap_or_else(PoisonError::into_inner);
    if jobs.len() >= MAX_JOBS {
        jobs.pop_front();
    }
    jobs.push_back(j);
}

/// The kept jobs that finished at or after `since`; the rest are dropped.
fn take_jobs_since(since: Option<Instant>) -> Vec<JobSample> {
    let jobs = std::mem::take(&mut *JOBS.lock().unwrap_or_else(PoisonError::into_inner));
    jobs.into_iter().filter(|j| since.is_none_or(|s| j.finished >= s)).collect()
}

/// p50 / p95 / max of `v` (sorted in place), or zeros.
pub fn percentiles(v: &mut [f32]) -> (f32, f32, f32) {
    if v.is_empty() {
        return (0.0, 0.0, 0.0);
    }
    v.sort_by(|a, b| a.total_cmp(b));
    let at = |p: f32| v.get(((v.len() - 1) as f32 * p).round() as usize).copied().unwrap_or(0.0);
    (at(0.5), at(0.95), v.last().copied().unwrap_or(0.0))
}

fn pct_text(v: &mut [f32]) -> String {
    let (a, b, c) = percentiles(v);
    format!("{a:.2}/{b:.2}/{c:.2}")
}

impl PassTimer {
    /// The logic pass of a refresh starts. A logic pass without a UI pass after it (hidden
    /// window) starts the refresh again.
    pub fn begin_logic(&mut self) {
        if !enabled() {
            return;
        }
        let generation = GENERATION.load(Ordering::Relaxed);
        if self.generation != generation {
            // timing was (re)started: nothing measured before applies (gap, dt, the window)
            *self = PassTimer { generation, ..Default::default() };
        }
        self.stages = Default::default();
        self.upload_bytes = 0;
        self.shown = None;
        self.causes.clear();
        let now = Instant::now();
        if let Some(end) = self.prev_end {
            self.stages[Stage::Gap.index()] = now.saturating_duration_since(end);
        }
        self.began = Some(now);
    }

    /// Add time to a stage of this pass.
    pub fn add(&mut self, stage: Stage, d: Duration) {
        if let Some(s) = self.stages.get_mut(stage.index()) {
            *s += d;
        }
    }

    /// Add the time since `started` (from [`start`]) to a stage.
    pub fn stop(&mut self, stage: Stage, started: Option<Instant>) {
        if let Some(t0) = started {
            self.add(stage, t0.elapsed());
        }
    }

    /// The GPU compositor's last composite.
    pub fn add_composite(&mut self, t: &filmcraft_gpu::CompositeTiming) {
        self.add(Stage::CpuFx, t.cpu_fx + t.cpu_transition);
        self.add(Stage::Upload, t.upload);
        self.add(Stage::Record, t.record);
        self.add(Stage::Submit, t.submit);
        self.upload_bytes += t.upload_bytes;
    }

    /// What asked egui for this pass (`egui::Context::repaint_causes`).
    pub fn note_causes(&mut self, causes: &[egui::RepaintCause]) {
        self.causes = causes
            .iter()
            .map(|c| {
                // the path from the crate's `src/` on (or the file name), and the line
                let f = c.file.replace('\\', "/");
                let short = f.rsplit_once("/src/").map_or_else(|| f.rsplit('/').next().unwrap_or(&f).to_string(), |(_, rest)| rest.to_string());
                format!("{short}:{}", c.line)
            })
            .collect();
        self.causes.sort();
        self.causes.dedup();
    }

    /// What the Program monitor showed.
    pub fn note_shown(&mut self, shown: Shown) {
        self.shown = Some(shown);
    }

    /// The UI pass ended: write this pass's line (when playing, scrubbing or waiting for the
    /// exact frame) and, once a second, the summary.
    pub fn end_pass(&mut self, playing: bool, playhead: i64, meter: (u64, u64)) {
        if !enabled() {
            return;
        }
        let Some(began) = self.began.take() else { return };
        let now = Instant::now();
        let moved = self.last_playhead.is_some_and(|p| p != playhead);
        self.last_playhead = Some(playhead);
        let waiting = self.shown.is_some_and(|s| !s.exact);
        let mode = if playing {
            "play"
        } else if moved {
            "scrub"
        } else if waiting {
            "settle"
        } else {
            ""
        };
        let interval = self.prev_began.map(|p| began.saturating_duration_since(p));
        self.prev_began = Some(began);
        self.prev_end = Some(now);
        if mode.is_empty() {
            // idle: no line, and the window restarts with the next active pass; jobs finished
            // while idle (thumbnails) are not part of it
            self.window = Window::default();
            drop(take_jobs_since(None));
            return;
        }
        self.passes += 1;
        let total = now.saturating_duration_since(began);
        let mut line = format!("pass #{} {mode}", self.passes);
        if let Some(s) = self.shown {
            line += &format!(" {}", s.text());
        }
        for stage in Stage::ALL {
            let d = self.stages[stage.index()];
            if stage == Stage::Upload {
                line += &format!(" upload={:.2}({:.1}MB)", ms(d), self.upload_bytes as f64 / 1e6);
            } else {
                line += &format!(" {}={:.2}", stage.name(), ms(d));
            }
        }
        line += &format!(" total={:.2}", ms(total));
        if let Some(i) = interval {
            line += &format!(" dt={:.2}", ms(i));
        }
        if !self.causes.is_empty() {
            line += &format!(" causes=[{}]", self.causes.join(" "));
        }
        log::info!(target: TARGET, "{line}");
        self.add_to_window(now, total, interval, meter, mode);
    }

    fn add_to_window(&mut self, now: Instant, total: Duration, interval: Option<Duration>, meter: (u64, u64), mode: &str) {
        let w = &mut self.window;
        let start = *w.started.get_or_insert(now);
        if w.stages.is_empty() {
            w.stages = vec![Vec::new(); Stage::ALL.len()];
            w.meter = meter;
            w.gop = filmcraft_codecs::gop_stats();
            w.hw_frames = filmcraft_codecs::hw::hw_stats().frames;
        }
        for (v, d) in w.stages.iter_mut().zip(self.stages) {
            v.push(ms(d));
        }
        w.totals.push(ms(total));
        if let Some(i) = interval {
            w.intervals.push(ms(i));
        }
        w.upload_bytes += self.upload_bytes;
        w.passes += 1;
        w.exact += u32::from(self.shown.is_some_and(|s| s.exact));
        let span = now.saturating_duration_since(start);
        if span >= Duration::from_secs(1) {
            let summary = self.window.summary(span, mode, meter);
            log::info!(target: TARGET, "{summary}");
            self.window = Window::default();
        }
    }
}

impl Window {
    fn summary(&mut self, span: Duration, mode: &str, meter: (u64, u64)) -> String {
        let secs = span.as_secs_f64().max(1e-3);
        let gop = filmcraft_codecs::gop_stats() - self.gop;
        let hw = filmcraft_codecs::hw::hw_stats().frames.saturating_sub(self.hw_frames);
        let mut s = format!(
            "summary {:.2}s {mode} passes={} ({:.1}/s) exact={} shown=+{} dropped=+{} | pass ms p50/p95/max: dt {} total {}",
            secs,
            self.passes,
            f64::from(self.passes) / secs,
            self.exact,
            meter.0.saturating_sub(self.meter.0),
            meter.1.saturating_sub(self.meter.1),
            pct_text(&mut self.intervals),
            pct_text(&mut self.totals),
        );
        for (stage, v) in Stage::ALL.iter().zip(self.stages.iter_mut()) {
            if v.iter().any(|x| *x > 0.0) {
                s += &format!(" {} {}", stage.name(), pct_text(v));
            }
        }
        s += &format!(" | upload {:.1} MB/s", self.upload_bytes as f64 / 1e6 / secs);
        let jobs = take_jobs_since(self.started);
        let (n, cancelled) = (jobs.len(), jobs.iter().filter(|j| j.cancelled).count());
        let col = |f: &dyn Fn(&JobSample) -> Duration| -> String {
            let mut v: Vec<f32> = jobs.iter().map(|j| ms(f(j))).collect();
            pct_text(&mut v)
        };
        let (wait, total, source, lock, decode, plan, prepare) = (
            col(&|j| j.wait),
            col(&|j| j.total),
            col(&|j| j.source),
            col(&|j| j.gop.lock_wait),
            col(&|j| j.gop.decode),
            col(&|j| j.plan()),
            col(&|j| j.prepare),
        );
        s += &format!(
            " | jobs={n} cancelled={cancelled} ms p50/p95/max: wait {wait} total {total} source {source} lock {lock} decode {decode} plan {plan} prepare {prepare}"
        );
        s += &format!(
            " | gop hits=+{} misses=+{} seeks=+{} samples=+{} skipped=+{} decodeMsPerSample={:.2} pictures hw=+{} sw=+{}",
            gop.hits,
            gop.misses,
            gop.seeks,
            gop.decoded,
            gop.skipped,
            gop.decode_ms_per_sample(),
            hw,
            gop.frames.saturating_sub(hw),
        );
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_environment_switch_takes_the_usual_spellings() {
        for on in ["1", "true", "ON", " yes "] {
            assert!(env_value_on(on), "{on}");
        }
        for off in ["", "0", "false", "off", "no", "2", "verbose"] {
            assert!(!env_value_on(off), "{off}");
        }
    }

    #[test]
    fn percentiles_of_nothing_are_zero_and_otherwise_nearest_rank() {
        assert_eq!(percentiles(&mut []), (0.0, 0.0, 0.0));
        let mut v: Vec<f32> = (1..=100).rev().map(|x| x as f32).collect();
        assert_eq!(percentiles(&mut v), (51.0, 95.0, 100.0));
        let mut nan = vec![1.0, f32::NAN, 2.0];
        let (_, _, max) = percentiles(&mut nan);
        assert!(max.is_nan(), "NaN sorts last instead of panicking");
    }

    #[test]
    fn stage_names_are_unique_and_every_stage_has_a_slot() {
        let names: std::collections::HashSet<&str> = Stage::ALL.iter().map(|s| s.name()).collect();
        assert_eq!(names.len(), Stage::ALL.len());
        for (i, s) in Stage::ALL.iter().enumerate() {
            assert_eq!(s.index(), i);
        }
    }

    #[test]
    fn a_job_line_separates_decode_plan_and_prepare() {
        let j = JobSample {
            frame: 42,
            prio: 1,
            prefetch: true,
            cancelled: false,
            preview: false,
            wait: Duration::from_millis(3),
            total: Duration::from_millis(20),
            source: Duration::from_millis(12),
            prepare: Duration::from_millis(5),
            gop: filmcraft_codecs::gop::trace::RequestTrace { misses: 1, samples: 2, decode: Duration::from_millis(11), ..Default::default() },
            finished: Instant::now(),
        };
        assert_eq!(j.plan(), Duration::from_millis(3));
        let line = j.line();
        for part in ["f=42", "prefetch", "wait=3.00", "source=12.00", "miss=1", "samples=2", "decode=11.00", "plan=3.00", "prepare=5.00"] {
            assert!(line.contains(part), "{part} in {line}");
        }
        // inconsistent parts (a clock step) never underflow
        let odd = JobSample { source: Duration::from_millis(30), ..j };
        assert_eq!(odd.plan(), Duration::ZERO);
    }

    #[test]
    fn a_disabled_timer_measures_nothing() {
        // (the switch is process-wide; this test only relies on it being off, its default)
        if enabled() {
            return;
        }
        assert!(start().is_none());
        let mut t = PassTimer::default();
        t.begin_logic();
        t.stop(Stage::Logic, start());
        t.end_pass(true, 10, (0, 0));
        assert_eq!(t.passes, 0);
        assert!(t.began.is_none());
    }

    #[test]
    fn the_pass_line_names_the_frame_on_screen() {
        assert_eq!(Shown { frame: 12, on_screen: Some(12), exact: true }.text(), "f=12 exact");
        assert_eq!(Shown { frame: 12, on_screen: Some(11), exact: false }.text(), "f=12 held=11");
        // the right frame at an older revision or size is held too, not exact
        assert_eq!(Shown { frame: 12, on_screen: Some(12), exact: false }.text(), "f=12 held=12");
        assert_eq!(Shown { frame: 12, on_screen: None, exact: false }.text(), "f=12 none");
    }

    #[test]
    fn a_summary_takes_only_jobs_from_its_own_interval() {
        // (JOBS is process-wide; this test owns it only while timing is off, its default)
        if enabled() {
            return;
        }
        let sample = |finished| JobSample {
            frame: 1,
            prio: 0,
            prefetch: false,
            cancelled: false,
            preview: false,
            wait: Duration::ZERO,
            total: Duration::ZERO,
            source: Duration::ZERO,
            prepare: Duration::ZERO,
            gop: Default::default(),
            finished,
        };
        let t0 = Instant::now();
        let later = t0 + Duration::from_millis(5);
        {
            let mut jobs = JOBS.lock().unwrap_or_else(PoisonError::into_inner);
            jobs.clear();
            jobs.push_back(sample(t0));
            jobs.push_back(sample(later));
        }
        let kept = take_jobs_since(Some(later));
        assert_eq!(kept.len(), 1, "the job from before the window is dropped");
        assert!(JOBS.lock().unwrap_or_else(PoisonError::into_inner).is_empty(), "taken jobs are gone");
    }
}
