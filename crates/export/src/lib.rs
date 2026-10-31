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
    static SERIAL_YUV_ROWS: Cell<bool> = const { Cell::new(false) };
}

/// Keep conversion on the encoding thread while the render pool may be occupied.
pub(crate) struct SerialYuvRows(bool);

impl SerialYuvRows {
    pub(crate) fn enter() -> Self {
        Self(SERIAL_YUV_ROWS.replace(true))
    }
}

impl Drop for SerialYuvRows {
    fn drop(&mut self) {
        SERIAL_YUV_ROWS.set(self.0);
    }
}

pub(crate) fn yuv_rows_are_serial() -> bool {
    SERIAL_YUV_ROWS.get() || rayon::current_thread_index().is_some()
}

/// Walk 4:2:0 chroma rows without joining the render pool from an encoder or worker (#1329).
pub(crate) fn for_yuv_rows(y: &mut [u8], u: &mut [u8], v: &mut [u8], w: usize, cw: usize, f: &(impl Fn(usize, &mut [u8], &mut [u8], &mut [u8]) + Sync)) {
    if yuv_rows_are_serial() {
        let (mut y, mut u, mut v) = (y, u, v);
        let mut cy = 0usize;
        while !u.is_empty() && !y.is_empty() {
            let take_u = cw.min(u.len());
            let take_v = cw.min(v.len());
            let rows = if y.len() >= w.saturating_mul(2) { 2 } else { (y.len() / w.max(1)).max(1) };
            let (yr, yrest) = y.split_at_mut(rows.saturating_mul(w).min(y.len()));
            let (ur, urest) = u.split_at_mut(take_u);
            let (vr, vrest) = v.split_at_mut(take_v);
            f(cy, yr, ur, vr);
            y = yrest;
            u = urest;
            v = vrest;
            cy += 1;
        }
    } else {
        y.par_chunks_mut(w * 2).zip(u.par_chunks_mut(cw).zip(v.par_chunks_mut(cw))).enumerate().for_each(|(cy, (yr, (ur, vr)))| f(cy, yr, ur, vr));
    }
}

#[cfg(test)]
mod yuv_pool_tests {
    use super::{Matrix, rgba_to_yuv420_8, rgbf_to_yuv420_8};
    use rayon::iter::{IntoParallelIterator, ParallelIterator};

    /// The overlap scope is already a Rayon worker. Conversion must finish there instead of
    /// joining the same pool (#1329).
    #[test]
    fn conversion_inside_the_export_scope_finishes() {
        let (w, h) = (48usize, 20usize);
        let rgba = vec![40u8; w * h * 4];
        let (mut y, mut u, mut v) = (Vec::new(), Vec::new(), Vec::new());
        rayon::in_place_scope(|s| {
            s.spawn(|_| {
                (0..4).into_par_iter().for_each(|_| std::thread::sleep(std::time::Duration::from_millis(30)));
            });
            rgba_to_yuv420_8(&rgba, w, h, &mut y, &mut u, &mut v);
        });
        assert_eq!(y.len(), w * h);
        assert_eq!(u.len(), 24 * 10);
        assert!(y.iter().any(|&p| p > 16));
    }

    #[test]
    fn worker_conversions_match_parallel_rows_for_odd_and_even_dimensions() {
        let pool = rayon::ThreadPoolBuilder::new().num_threads(1).build().unwrap();
        for (w, h) in [(3, 5), (48, 20)] {
            let rgba: Vec<u8> = (0..w * h * 4).map(|i| (i * 37 % 256) as u8).collect();
            let rgb: Vec<f32> = (0..w * h * 3).map(|i| (i * 17 % 256) as f32 / 255.0).collect();
            for floating in [false, true] {
                let convert = || {
                    let (mut y, mut u, mut v) = (Vec::new(), Vec::new(), Vec::new());
                    if floating {
                        rgbf_to_yuv420_8(&rgb, w, h, Matrix::new(0.2126, 0.0722), &mut y, &mut u, &mut v);
                    } else {
                        rgba_to_yuv420_8(&rgba, w, h, &mut y, &mut u, &mut v);
                    }
                    (y, u, v)
                };
                let parallel = convert();
                let worker = pool.install(convert);
                assert_eq!(worker, parallel, "{w}x{h}, floating={floating}");
            }
        }
    }
}
