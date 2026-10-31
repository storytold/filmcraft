//! Export: render a sequence range and encode it to a file.
//!
//! Frames are rendered in parallel batches (one frame per core, each frame itself row-parallel),
//! then encoded and muxed in order; audio is mixed per batch and interleaved. Progress and cancel
//! are shared atomics so the UI (Export mode, header progress) and MCP can observe/cancel jobs.
//!
//! Video encoders implement [`VideoEncoder`]; codec crates register theirs with
//! [`register_encoder`] (H.264, ProRes …). Built in: Motion-JPEG (MOV), PNG / TIFF / BMP / JPEG /
//! Targa / DPX / OpenEXR sequences, GIF, WAV and AIFF.
//!
//! [`ExportSettings`] carries every Export-mode setting (frame size, rate, bitrate encoding, audio
//! format, multiplexer, captions, effects, metadata) as serde data; [`presets`] defines the
//! built-in presets.
#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable))]

use std::cell::Cell;
use std::io::Write;

mod aac_out;
mod audio_out;
mod deflate;
mod job;
mod mxf_out;
mod pace;
mod pcm;
mod pipeline;
pub mod presets;
mod raster;
pub mod settings;
pub mod still;
pub use audio_out::LoudnessReport;
pub use job::{Exporter, Step, stepped};
pub use mxf_out::opatom_audio_paths;
pub use pcm::{image_sequence_path, write_aiff, write_wav};
pub use pipeline::limit_rgba8;
pub use presets::{ExportPreset, builtin_presets};
pub use settings::*;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock, PoisonError, RwLock};

use filmcraft_isobmff::SampleEntry;
use filmcraft_project::{ItemId, Project};
use filmcraft_render::SourceProvider;
use filmcraft_time::{FrameRate, Tick, TimeRange};
use rayon::prelude::*;
use serde::{Deserialize, Serialize};

mod convert;
mod encoders;
mod format;
mod output;
mod registry;
mod run;
mod stats;
mod still_encode;

pub use convert::{Matrix, Planes444, rgba_to_yuv420_8, rgba_to_yuv422_10, rgbf_to_yuv420_8, rgbf_to_yuv420_10, rgbf_to_yuv422_10, to_yuv444_10};
pub(crate) use encoders::FlacAudio;
#[cfg(test)]
use encoders::{aac_factory, h264_factory};
pub use encoders::{apv_profile, dnx_profile, prores_profile};
pub use format::{ColorSignal, DEFAULT_CRF, ExportSettings, Format, H264Pass};
use output::Out;
pub use output::OutputSink;
pub(crate) use registry::build_frame_renderer;
#[cfg(test)]
pub(crate) use registry::reset_frame_renderers_for_tests;
pub use registry::{
    AudioEncoder, AudioEncoderFactory, EncodedPacket, EncoderFactory, EncoderFrame, FrameRenderer, FrameRendererFactory, PrepareAhead, VideoEncoder, available,
    encoder_registered, frame_renderer_registered, hdr_available, register_audio_encoder, register_encoder, register_format_probe, register_frame_renderer,
    register_hdr_probe,
};
use registry::{audio_factories, video_factories};
#[cfg(test)]
use run::gif_delay_cs;
pub use run::{export, export_range, frame_span, validate_range};
pub(crate) use stats::note_lock_wait;
pub use stats::{
    GpuRenderStats, HwEncodeStats, Stage, export_lock_wait, gpu_render_stats, hw_encode_stats, note_gpu_fallback, note_gpu_frame, note_hw_encode_declined,
    note_hw_encode_frame, note_hw_encode_session, note_stage, reset_export_lock_wait, stage_times, timed,
};
pub use still_encode::{encode_png, encode_still, encode_still_with, encode_still_with_quality};

#[derive(Debug, thiserror::Error)]
pub enum ExportError {
    #[error("no such sequence")]
    NoSequence,
    #[error("unsupported: {0}")]
    Unsupported(String),
    #[error("I/O: {0}")]
    Io(String),
    #[error("encode: {0}")]
    Encode(String),
    #[error("cancelled")]
    Cancelled,
}

pub type Result<T> = std::result::Result<T, ExportError>;

/// Shared progress/cancel state of an export job.
#[derive(Default)]
pub struct Progress {
    pub done: AtomicU64,
    pub total: AtomicU64,
    pub cancel: AtomicBool,
    pub finished: AtomicBool,
    pub status: Mutex<String>,
    pub error: Mutex<Option<String>>,
    /// What loudness normalization measured (when it ran).
    pub loudness: Mutex<Option<LoudnessReport>>,
    /// Readings of `done` over time, for [`Progress::eta`].
    pace: Mutex<pace::Pace>,
}

impl Progress {
    pub fn fraction(&self) -> f32 {
        let t = self.total.load(Ordering::Relaxed).max(1);
        self.done.load(Ordering::Relaxed) as f32 / t as f32
    }

    /// The estimated time left, from the job's speed over the last 15 seconds. Each call is also a
    /// reading of that speed, so ask regularly (the UI does on every frame it draws the job).
    /// `None` until a second of readings shows progress, and once the job is done or finished.
    pub fn eta(&self) -> Option<std::time::Duration> {
        self.eta_at(web_time::Instant::now())
    }

    /// [`Progress::eta`] at a given time (tests, and hosts with a clock of their own).
    pub fn eta_at(&self, now: web_time::Instant) -> Option<std::time::Duration> {
        let (done, total) = (self.done.load(Ordering::Relaxed), self.total.load(Ordering::Relaxed));
        let mut pace = self.pace.lock().unwrap_or_else(PoisonError::into_inner);
        pace.observe(now, done);
        if self.finished.load(Ordering::Relaxed) || done >= total {
            return None;
        }
        pace.eta(total - done)
    }
    fn set_status(&self, s: impl Into<String>) {
        *self.status.lock().unwrap_or_else(PoisonError::into_inner) = s.into();
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct Report {
    pub path: String,
    pub frames: u64,
    pub seconds: f64,
    pub bytes: u64,
    pub render_fps: f64,
    /// Further files written besides `path` (MXF OP-Atom audio files).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub extra_files: Vec<String>,
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod settings_tests;

#[cfg(test)]
mod surround_tests;

#[cfg(test)]
mod mxf_tests;

#[cfg(test)]
mod determinism_tests;

#[cfg(test)]
mod hdr_tests;

thread_local! {
    static EXPORT_ENCODE_SCOPE: Cell<u32> = const { Cell::new(0) };
    #[cfg(test)]
    static SEQUENTIAL_YUV: Cell<u32> = const { Cell::new(0) };
}

pub(crate) struct ExportEncodeScope;

impl Drop for ExportEncodeScope {
    fn drop(&mut self) {
        EXPORT_ENCODE_SCOPE.with(|depth| depth.set(depth.get().saturating_sub(1)));
    }
}

/// The export overlap runs `encode` on the calling thread, which is not a Rayon
/// worker, while a render job occupies the pool. Nested `par_chunks` from that
/// thread waits for workers that are waiting for this scope (#1329).
pub(crate) fn export_encode_scope() -> ExportEncodeScope {
    EXPORT_ENCODE_SCOPE.with(|depth| depth.set(depth.get().saturating_add(1)));
    ExportEncodeScope
}

pub(crate) fn avoid_nested_rayon() -> bool {
    EXPORT_ENCODE_SCOPE.with(|depth| depth.get() > 0) || rayon::current_thread_index().is_some()
}

pub(crate) fn note_sequential_yuv() {
    #[cfg(test)]
    SEQUENTIAL_YUV.with(|hits| hits.set(hits.get().saturating_add(1)));
}

#[cfg(test)]
pub(crate) fn sequential_yuv_hits() -> u32 {
    SEQUENTIAL_YUV.with(|hits| hits.get())
}

#[cfg(test)]
mod yuv_pool_tests {
    use super::{export_encode_scope, rgba_to_yuv420_8, sequential_yuv_hits};
    use rayon::iter::{IntoParallelIterator, ParallelIterator};

    /// Encode runs on the export thread, which is not a pool worker, while the
    /// overlap scope's render job holds the other workers. The conversion must
    /// take the sequential walk there, and match the parallel result (#1329).
    #[test]
    fn conversion_inside_the_export_scope_matches_the_parallel_walk() {
        for (w, h) in [(48usize, 20usize), (47, 21), (8, 1), (1, 8)] {
            let rgba: Vec<u8> = (0..w * h * 4).map(|i| (i % 251) as u8).collect();
            let (mut yp, mut up, mut vp) = (Vec::new(), Vec::new(), Vec::new());
            let before = sequential_yuv_hits();
            rgba_to_yuv420_8(&rgba, w, h, &mut yp, &mut up, &mut vp);
            assert_eq!(sequential_yuv_hits(), before, "the plain call stays on the parallel walk");
            let (y, u, v) = std::thread::scope(|scope| {
                let handle = scope.spawn(|| {
                    assert!(rayon::current_thread_index().is_none(), "the export thread is not a pool worker");
                    let mut y = Vec::new();
                    let mut u = Vec::new();
                    let mut v = Vec::new();
                    let hits = sequential_yuv_hits();
                    rayon::in_place_scope(|s| {
                        s.spawn(|_| {
                            (0..4).into_par_iter().for_each(|_| std::thread::sleep(std::time::Duration::from_millis(5)));
                        });
                        let _guard = export_encode_scope();
                        rgba_to_yuv420_8(&rgba, w, h, &mut y, &mut u, &mut v);
                    });
                    assert!(sequential_yuv_hits() > hits, "{w}x{h} did not take the sequential walk");
                    (y, u, v)
                });
                handle.join().expect("export thread")
            });
            assert_eq!((y, u, v), (yp, up, vp), "{w}x{h}");
        }
    }
}
