use super::*;

/// Hardware encoder counters (`perf.stats` `export.hardware`): pictures encoded by hardware
/// encoders, encoders created, and requests a hardware encoder declined (the software encoder
/// took them).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct HwEncodeStats {
    pub frames: u64,
    pub sessions: u64,
    pub declined: u64,
}

static HW_FRAMES: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

static HW_SESSIONS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

static HW_DECLINED: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// The hardware encoder counters so far.
pub fn hw_encode_stats() -> HwEncodeStats {
    use std::sync::atomic::Ordering::Relaxed;
    HwEncodeStats { frames: HW_FRAMES.load(Relaxed), sessions: HW_SESSIONS.load(Relaxed), declined: HW_DECLINED.load(Relaxed) }
}

/// A hardware encoder encoded a picture.
pub fn note_hw_encode_frame() {
    HW_FRAMES.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
}

/// A hardware encoder was created.
pub fn note_hw_encode_session() {
    HW_SESSIONS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
}

/// A hardware encoder declined a request (the software encoder takes it).
pub fn note_hw_encode_declined() {
    HW_DECLINED.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
}

/// The stages of an export whose wall time [`stage_times`] adds up (`perf.stats` `export.stages`). The
/// render of one batch overlaps the encoding of the previous one (`Exporter::overlapping`), so
/// [`Stage::Render`] and [`Stage::Encode`] with its neighbours can sum to more than the export took;
/// [`Stage::Wait`] is the time left waiting for the render.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stage {
    /// [`Exporter::new`]: settings, pipeline, encoder and audio set-up.
    Setup,
    /// The loudness pass before the first frame (normalised audio only).
    Loudness,
    /// Rendering a batch of frames (parallel; includes decoding the sources, effects, colour).
    Render,
    /// Encoding the rendered frames, colour conversion included (serial).
    Encode,
    /// The part of [`Stage::Encode`] spent converting RGB to the encoder's YUV.
    Convert,
    /// Mixing and encoding audio and writing it to the container.
    Audio,
    /// Writing video samples to the container (and creating it).
    Mux,
    /// Flushing the encoders and finishing the file.
    Finish,
    /// Time the encoding side waited, after encoding a batch, for the next batch to finish rendering
    /// (only with overlap: large means the export is render-bound, near zero encode-bound).
    Wait,
}

const STAGES: usize = 9;

impl Stage {
    pub const ALL: [Stage; STAGES] =
        [Stage::Setup, Stage::Loudness, Stage::Render, Stage::Encode, Stage::Convert, Stage::Audio, Stage::Mux, Stage::Finish, Stage::Wait];

    /// The name in `perf.stats`.
    pub fn name(self) -> &'static str {
        match self {
            Stage::Setup => "setup",
            Stage::Loudness => "loudness",
            Stage::Render => "render",
            Stage::Encode => "encode",
            Stage::Convert => "convert",
            Stage::Audio => "audio",
            Stage::Mux => "mux",
            Stage::Finish => "finish",
            Stage::Wait => "wait",
        }
    }
}

static STAGE_NS: [std::sync::atomic::AtomicU64; STAGES] = [const { std::sync::atomic::AtomicU64::new(0) }; STAGES];

/// Wall time each export stage has taken so far in this process, in nanoseconds, summed over every
/// stepped export (they only grow: diff two readings to measure one export). Measuring costs two
/// clock reads per stage per batch; nothing an export does depends on it.
pub fn stage_times() -> Vec<(Stage, u64)> {
    Stage::ALL.iter().map(|s| (*s, STAGE_NS.get(*s as usize).map_or(0, |a| a.load(std::sync::atomic::Ordering::Relaxed)))).collect()
}

/// Add `d` to a stage's wall time (also for encoders outside this crate: [`Stage::Convert`]).
pub fn note_stage(stage: Stage, d: std::time::Duration) {
    if let Some(a) = STAGE_NS.get(stage as usize) {
        a.fetch_add(u64::try_from(d.as_nanos()).unwrap_or(u64::MAX), std::sync::atomic::Ordering::Relaxed);
    }
}

/// Run `f`, adding its wall time to `stage`.
pub fn timed<T>(stage: Stage, f: impl FnOnce() -> T) -> T {
    let t = web_time::Instant::now();
    let r = f();
    note_stage(stage, t.elapsed());
    r
}

pub(crate) static GPU_FRAMES: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

pub(crate) static GPU_FALLBACKS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

static LOCK_WAIT_NS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// A frame was composited by the registered GPU renderer.
pub fn note_gpu_frame() {
    GPU_FRAMES.fetch_add(1, Ordering::Relaxed);
}

/// The GPU renderer declined a frame (the CPU reference renderer took it).
pub fn note_gpu_fallback() {
    GPU_FALLBACKS.fetch_add(1, Ordering::Relaxed);
}

/// Time an export worker waited for a free GPU renderer (process-wide, for the bench).
pub(crate) fn note_lock_wait(d: std::time::Duration) {
    LOCK_WAIT_NS.fetch_add(u64::try_from(d.as_nanos()).unwrap_or(u64::MAX), Ordering::Relaxed);
}

/// Total time export workers waited for a GPU renderer since [`reset_export_lock_wait`].
pub fn export_lock_wait() -> std::time::Duration {
    std::time::Duration::from_nanos(LOCK_WAIT_NS.load(Ordering::Relaxed))
}

/// Start [`export_lock_wait`] again from zero.
pub fn reset_export_lock_wait() {
    LOCK_WAIT_NS.store(0, Ordering::Relaxed);
}

/// GPU-vs-CPU frame counters of the export renderer (per-process, like the hardware ones).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GpuRenderStats {
    pub frames: u64,
    pub fallbacks: u64,
}

pub fn gpu_render_stats() -> GpuRenderStats {
    GpuRenderStats { frames: GPU_FRAMES.load(Ordering::Relaxed), fallbacks: GPU_FALLBACKS.load(Ordering::Relaxed) }
}
