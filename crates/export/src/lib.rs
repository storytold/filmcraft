//! Export: render a sequence range and encode it to a file.
//!
//! Frames are rendered in parallel batches (one frame per core, each frame itself row-parallel),
//! then encoded and muxed in order; audio is mixed per batch and interleaved. Progress and cancel
//! are shared atomics so the UI (Export mode, header progress) and MCP can observe/cancel jobs.
//!
//! Video encoders implement [`VideoEncoder`]; codec crates register theirs with
//! [`register_encoder`] (H.264, ProRes …). Built in: Motion-JPEG (MOV), PNG / TIFF / BMP
//! sequences, GIF, WAV and AIFF.
//!
//! [`ExportSettings`] carries every Export-mode setting (frame size, rate, bitrate encoding, audio
//! format, multiplexer, captions, effects, metadata) as serde data; [`presets`] defines the
//! built-in presets.
#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable))]

use std::io::Write;

mod audio_out;
mod job;
mod mxf_out;
mod pace;
mod pcm;
mod pipeline;
pub mod presets;
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
use std::sync::{Arc, Mutex, OnceLock, RwLock};

use filmcraft_isobmff::SampleEntry;
use filmcraft_project::{ItemId, Project};
use filmcraft_render::SourceProvider;
use filmcraft_time::{FrameRate, Tick, TimeRange};
use rayon::prelude::*;
use serde::{Deserialize, Serialize};

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

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Format {
    /// MPEG-4 (or QuickTime, see [`Multiplexer`]), H.264 video + AAC audio.
    #[default]
    #[serde(rename = "h264", alias = "H264")]
    H264,
    /// MPEG-4 (or QuickTime, see [`Multiplexer`]), H.265 / HEVC Main (8-bit) video + AAC audio. There is no
    /// built-in encoder: a platform hardware encoder registers one ([`register_encoder`]) and says so
    /// with [`register_format_probe`]; [`available`] is false without it. HDR sequences export as HDR
    /// (Main 10, PQ / HLG) only where an encoder also says so with [`register_hdr_probe`]
    /// ([`hdr_available`]); elsewhere they are tone-mapped to SDR.
    #[serde(rename = "hevc", alias = "Hevc")]
    Hevc,
    /// QuickTime, Apple ProRes 422 (HQ unless the settings pick another flavour) + PCM.
    #[serde(rename = "prores", alias = "ProRes")]
    ProRes,
    /// QuickTime, Avid DNxHR (HQ unless the settings pick another profile) + PCM.
    #[serde(rename = "dnxhr", alias = "DnxHr")]
    DnxHr,
    /// QuickTime, APV (Advanced Professional Video, RFC 9924; 422-10 unless the settings pick
    /// another profile) + PCM.
    #[serde(rename = "apv", alias = "Apv")]
    Apv,
    /// QuickTime, Motion-JPEG + PCM.
    #[serde(rename = "mjpeg", alias = "Mjpeg")]
    Mjpeg,
    /// Numbered PNG stills ([`image_sequence_path`]).
    #[serde(rename = "png", alias = "PngSequence")]
    PngSequence,
    /// Numbered TIFF stills.
    #[serde(rename = "tiff", alias = "TiffSequence")]
    TiffSequence,
    /// Numbered BMP stills.
    #[serde(rename = "bmp", alias = "BmpSequence")]
    BmpSequence,
    #[serde(rename = "gif", alias = "Gif")]
    Gif,
    #[serde(rename = "wav", alias = "Wav")]
    Wav,
    #[serde(rename = "aiff", alias = "Aiff")]
    Aiff,
    /// MXF OP1a (SMPTE ST 378): frame-wrapped DNxHR, ProRes or H.264 ([`MxfVideoCodec`]) + PCM.
    #[serde(rename = "mxf-op1a", alias = "MxfOp1a")]
    MxfOp1a,
    /// MXF OP-Atom (SMPTE ST 390, Avid style): the picture in one file, one mono PCM file per
    /// audio channel ([`opatom_audio_paths`]).
    #[serde(rename = "mxf-opatom", alias = "MxfOpAtom")]
    MxfOpAtom,
}

impl Format {
    pub fn from_name(s: &str) -> Option<Format> {
        Some(match s.to_ascii_lowercase().replace([' ', '-', '_', '.'], "").as_str() {
            "h264" | "mp4" | "avc" | "m4v" => Format::H264,
            "hevc" | "h265" | "hvc1" | "hev1" | "x265" => Format::Hevc,
            "prores" | "mov" | "appleprores" => Format::ProRes,
            "dnxhr" | "dnxhd" | "dnx" | "avid" | "aviddnxhr" | "aviddnxhd" | "vc3" => Format::DnxHr,
            "apv" | "apv1" => Format::Apv,
            "mxf" | "mxfop1a" | "op1a" => Format::MxfOp1a,
            "mxfopatom" | "opatom" | "mxfatom" | "avidmxf" => Format::MxfOpAtom,
            "mjpeg" | "motionjpeg" | "jpeg" => Format::Mjpeg,
            "png" | "pngsequence" => Format::PngSequence,
            "tif" | "tiff" | "tiffsequence" => Format::TiffSequence,
            "bmp" | "bmpsequence" => Format::BmpSequence,
            "gif" | "animatedgif" => Format::Gif,
            "wav" | "waveform" | "waveformaudio" => Format::Wav,
            "aif" | "aiff" | "aifc" => Format::Aiff,
            _ => return None,
        })
    }
    /// Stable id, as accepted by [`Format::from_name`] and used in serialized settings.
    pub fn id(self) -> &'static str {
        match self {
            Format::H264 => "h264",
            Format::Hevc => "hevc",
            Format::ProRes => "prores",
            Format::DnxHr => "dnxhr",
            Format::Apv => "apv",
            Format::Mjpeg => "mjpeg",
            Format::PngSequence => "png",
            Format::TiffSequence => "tiff",
            Format::BmpSequence => "bmp",
            Format::Gif => "gif",
            Format::Wav => "wav",
            Format::Aiff => "aiff",
            Format::MxfOp1a => "mxf-op1a",
            Format::MxfOpAtom => "mxf-opatom",
        }
    }
    pub fn extension(self) -> &'static str {
        match self {
            Format::H264 | Format::Hevc => "mp4",
            Format::ProRes | Format::DnxHr | Format::Apv | Format::Mjpeg => "mov",
            Format::PngSequence => "png",
            Format::TiffSequence => "tif",
            Format::BmpSequence => "bmp",
            Format::Gif => "gif",
            Format::Wav => "wav",
            Format::Aiff => "aif",
            Format::MxfOp1a | Format::MxfOpAtom => "mxf",
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            Format::H264 => "H.264",
            Format::Hevc => "H.265 (HEVC)",
            Format::ProRes => "Apple ProRes",
            Format::DnxHr => "Avid DNxHR",
            Format::Apv => "APV",
            Format::Mjpeg => "QuickTime (Motion JPEG)",
            Format::PngSequence => "PNG",
            Format::TiffSequence => "TIFF",
            Format::BmpSequence => "BMP",
            Format::Gif => "Animated GIF",
            Format::Wav => "Waveform Audio",
            Format::Aiff => "AIFF",
            Format::MxfOp1a => "MXF OP1a",
            Format::MxfOpAtom => "MXF OP-Atom",
        }
    }
    /// An MXF container format.
    pub fn is_mxf(self) -> bool {
        matches!(self, Format::MxfOp1a | Format::MxfOpAtom)
    }
    /// Whether the crate carries an encoder for the format; the others need one registered at runtime
    /// ([`register_format_probe`]).
    pub fn has_builtin_encoder(self) -> bool {
        self != Format::Hevc
    }
    /// H.264 or H.265: MPEG-4 (or QuickTime) with AAC audio, set up with the same bitrate controls.
    pub fn is_h26x(self) -> bool {
        matches!(self, Format::H264 | Format::Hevc)
    }
    pub const ALL: [Format; 14] = [
        Format::H264,
        Format::Hevc,
        Format::ProRes,
        Format::DnxHr,
        Format::Apv,
        Format::Mjpeg,
        Format::PngSequence,
        Format::TiffSequence,
        Format::BmpSequence,
        Format::Gif,
        Format::Wav,
        Format::Aiff,
        Format::MxfOp1a,
        Format::MxfOpAtom,
    ];
}

/// Two-pass state of an H.264 export (set by the exporter, not by callers).
#[derive(Clone, Debug, Default)]
pub enum H264Pass {
    #[default]
    Single,
    First,
    Second(filmcraft_h264enc::PassStats),
}

/// Every Export-mode setting. Serialized in camelCase; every field is optional on input.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct ExportSettings {
    pub format: Format,
    pub path: String,
    /// Timeline range (default: In/Out if set, else the whole sequence).
    pub range: Option<TimeRange>,
    /// Output scale (1.0 = sequence frame size).
    pub scale: f32,
    pub include_audio: bool,
    /// Quality 0–100 for lossy codecs.
    pub quality: u8,
    /// Target video bitrate (kbps) for bitrate-driven encoders.
    pub bitrate_kbps: u32,
    /// Burn the visible caption tracks into the picture (Export ▸ Captions ▸ Burn Captions Into
    /// Video).
    pub burn_captions: bool,
    /// This export is one part of a larger job (render previews): the caller owns
    /// `progress.total`, `finished` and the final status; this call only adds to `done`.
    #[serde(default)]
    pub part_of_batch: bool,
    /// ProRes flavour: `proxy`, `lt`, `standard` or `hq` (empty = HQ).
    #[serde(default)]
    pub prores_profile: String,
    /// DNxHR profile: `lb`, `sq`, `hq` or `hqx` (empty = HQ).
    #[serde(default)]
    pub dnx_profile: String,
    /// APV profile: `422-10`, `422-12`, `444-10` or `444-12` (empty = `422-10`).
    #[serde(default)]
    pub apv_profile: String,
    /// Video codec of the MXF formats (DNxHR unless set; the ProRes / DNxHR profile fields apply).
    #[serde(default)]
    pub mxf_video_codec: MxfVideoCodec,
    /// Encode display-referred SDR (Rec. 709, tone mapped) even when the sequence works in
    /// Rec. 2100 PQ/HLG. Otherwise H.264 and ProRes exports of an HDR sequence are encoded in the
    /// sequence's HDR space and signal it (VUI / `colr` / `mdcv` / `clli` / SEI).
    #[serde(default)]
    pub sdr: bool,
    /// Keep the alpha channel in PNG and TIFF sequences (straight alpha, Premiere's "Include
    /// Alpha Channel"). Off, every frame is flattened over black (#160). Other formats ignore it.
    #[serde(default)]
    pub alpha: bool,
    /// Output frame size (None = Match Source: the sequence size times `scale`).
    pub frame_size: Option<(u32, u32)>,
    /// Output frame rate (None = Match Source).
    pub frame_rate: Option<FrameRate>,
    /// How the picture fills a frame of another aspect ratio.
    pub scaling: Scaling,
    /// Pixel aspect ratio (None = square pixels). Signalled in the H.264 VUI.
    pub pixel_aspect: Option<(u32, u32)>,
    pub field_order: FieldOrder,
    pub h264_profile: H264Profile,
    /// H.264 level × 10 (41 = 4.1); None = the lowest level that fits. A level below what the
    /// stream needs is raised.
    pub h264_level: Option<u8>,
    pub bitrate_mode: BitrateMode,
    /// May H.264 be encoded by the system's hardware encoder (VideoToolbox on macOS, NVENC on
    /// Windows)? Off unless asked for: hardware output depends on the machine, so it is not
    /// byte-reproducible like the built-in encoder's (`determinism_tests`).
    #[serde(default)]
    pub hardware_encoding: HardwareEncoding,
    /// VBR maximum bitrate (None = 1.5 × target).
    pub max_bitrate_kbps: Option<u32>,
    /// Adaptive bitrate (the Match Source presets): bits per pixel per frame; replaces
    /// `bitrate_kbps` with `width × height × fps × bpp / 1000`.
    pub adaptive_bitrate: Option<f32>,
    /// Frames between keyframes (None = 2 seconds).
    pub keyframe_distance: Option<u32>,
    /// Render at Maximum Depth. FilmCraft always composites in 32-bit float, so this changes
    /// nothing; it is kept so presets round-trip.
    pub render_at_max_depth: bool,
    /// Use Maximum Render Quality: render at full size and scale with an area filter.
    pub max_render_quality: bool,
    pub audio: AudioSettings,
    pub multiplexer: Multiplexer,
    /// Captions ▸ Create Sidecar File: `srt` or `vtt` next to the output (written by the engine).
    pub caption_sidecar: Option<String>,
    pub effects: ExportEffects,
    pub metadata: ExportMetadata,
    /// Two-pass state (set by the exporter).
    #[serde(skip)]
    pub h264_pass: H264Pass,
    /// Colour signalling chosen by [`export`] for the encoders (not set by callers).
    #[serde(skip)]
    pub signal: ColorSignal,
    /// Encode into memory and hand each finished file (path, bytes) to this sink instead of
    /// writing `path` (hosts without a filesystem: the web app offers the file as a download).
    #[serde(skip)]
    pub sink: Option<OutputSink>,
}

/// Receives in-memory export output: `(path, bytes)` per finished file.
#[derive(Clone)]
pub struct OutputSink(pub Arc<dyn Fn(&str, Vec<u8>) -> std::io::Result<()> + Send + Sync>);

impl std::fmt::Debug for OutputSink {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("OutputSink")
    }
}

/// An export's output file: on disk, or in memory for an [`OutputSink`].
enum Out {
    File(std::io::BufWriter<std::fs::File>),
    Mem(std::io::Cursor<Vec<u8>>),
}

impl Out {
    fn create(settings: &ExportSettings) -> Result<Out> {
        Out::create_path(settings, &settings.path)
    }

    /// An output file at `path` (OP-Atom exports write several).
    fn create_path(settings: &ExportSettings, path: &str) -> Result<Out> {
        if settings.sink.is_some() {
            return Ok(Out::Mem(std::io::Cursor::new(Vec::new())));
        }
        let f = std::fs::File::create(path).map_err(|e| ExportError::Io(format!("{path}: {e}")))?;
        Ok(Out::File(std::io::BufWriter::new(f)))
    }

    /// Flush (and hand in-memory output to the sink); returns the file size.
    fn finish(self, settings: &ExportSettings) -> Result<u64> {
        self.finish_path(settings, &settings.path)
    }

    fn finish_path(self, settings: &ExportSettings, path: &str) -> Result<u64> {
        match self {
            Out::File(mut w) => {
                w.flush().map_err(|e| ExportError::Io(e.to_string()))?;
                Ok(std::fs::metadata(path).map(|m| m.len()).unwrap_or(0))
            }
            Out::Mem(c) => write_output(settings, path, c.into_inner()),
        }
    }
}

/// Write one finished output file (to the sink when there is one); returns its size.
fn write_output(settings: &ExportSettings, path: &str, data: Vec<u8>) -> Result<u64> {
    let n = data.len() as u64;
    match &settings.sink {
        Some(sink) => (sink.0)(path, data),
        None => std::fs::write(path, &data),
    }
    .map_err(|e| ExportError::Io(e.to_string()))?;
    Ok(n)
}

impl Write for Out {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        match self {
            Out::File(w) => w.write(buf),
            Out::Mem(w) => w.write(buf),
        }
    }
    fn flush(&mut self) -> std::io::Result<()> {
        match self {
            Out::File(w) => w.flush(),
            Out::Mem(w) => w.flush(),
        }
    }
}

impl std::io::Seek for Out {
    fn seek(&mut self, pos: std::io::SeekFrom) -> std::io::Result<u64> {
        match self {
            Out::File(w) => w.seek(pos),
            Out::Mem(w) => w.seek(pos),
        }
    }
}

/// Colour description of the encoded stream (ITU-T H.273 code points).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ColorSignal {
    pub primaries: u8,
    pub transfer: u8,
    pub matrix: u8,
}

impl Default for ColorSignal {
    fn default() -> Self {
        ColorSignal { primaries: 1, transfer: 1, matrix: 1 }
    }
}

impl ColorSignal {
    pub const PQ: ColorSignal = ColorSignal { primaries: 9, transfer: 16, matrix: 9 };
    pub const HLG: ColorSignal = ColorSignal { primaries: 9, transfer: 18, matrix: 9 };
    pub fn is_hdr(&self) -> bool {
        matches!(self.transfer, 16 | 18)
    }
    /// (Kr, Kb) of the matrix.
    pub fn kr_kb(&self) -> (f32, f32) {
        if self.matrix == 9 { (0.2627, 0.0593) } else { (0.2126, 0.0722) }
    }
    /// Sample-entry boxes: `colr` (nclx for MP4, nclc for MOV) plus `mdcv`/`clli` for PQ
    /// (mastering display BT.2020/D65, 1000/0.0001 cd/m²; MaxCLL/MaxFALL 0 = unknown).
    pub fn apply_to(&self, e: &mut SampleEntry, mov: bool) {
        if !self.is_hdr() {
            return;
        }
        if let Some(v) = e.video.as_mut() {
            let (p, t, m) = (self.primaries as u16, self.transfer as u16, self.matrix as u16);
            v.color = Some(if mov {
                filmcraft_isobmff::ColorInfo::Nclc { primaries: p, transfer: t, matrix: m }
            } else {
                filmcraft_isobmff::ColorInfo::Nclx { primaries: p, transfer: t, matrix: m, full_range: false }
            });
            if let Some((md, cll)) = self.static_metadata() {
                v.mastering_display = Some(md);
                v.content_light = Some(cll);
            }
        }
    }
    /// HDR10 static metadata of a PQ signal: the mastering display (BT.2020/D65, 1000 / 0.0001 cd/m²)
    /// and (MaxCLL, MaxFALL) = (0, 0), unknown. `None` for HLG and SDR. The one definition of these
    /// values: the `mdcv` / `clli` boxes and the SEI messages of every encoder come from here.
    pub fn static_metadata(&self) -> Option<(filmcraft_isobmff::MasteringDisplay, (u16, u16))> {
        (self.transfer == 16).then(|| (filmcraft_isobmff::MasteringDisplay::bt2020(1000.0, 0.0001), (0, 0)))
    }
}

impl Default for ExportSettings {
    fn default() -> Self {
        Self {
            format: Format::H264,
            path: String::new(),
            range: None,
            scale: 1.0,
            include_audio: true,
            quality: 90,
            bitrate_kbps: 20_000,
            burn_captions: false,
            part_of_batch: false,
            prores_profile: String::new(),
            dnx_profile: String::new(),
            apv_profile: String::new(),
            mxf_video_codec: MxfVideoCodec::default(),
            sdr: false,
            alpha: false,
            frame_size: None,
            frame_rate: None,
            scaling: Scaling::default(),
            pixel_aspect: None,
            field_order: FieldOrder::Progressive,
            h264_profile: H264Profile::High,
            h264_level: None,
            bitrate_mode: BitrateMode::default(),
            hardware_encoding: HardwareEncoding::default(),
            max_bitrate_kbps: None,
            adaptive_bitrate: None,
            keyframe_distance: None,
            render_at_max_depth: false,
            max_render_quality: false,
            audio: AudioSettings::default(),
            multiplexer: Multiplexer::Mp4,
            caption_sidecar: None,
            effects: ExportEffects::default(),
            metadata: ExportMetadata::default(),
            h264_pass: H264Pass::Single,
            signal: ColorSignal::default(),
            sink: None,
        }
    }
}

impl ExportSettings {
    /// Reject settings the encoders cannot honour.
    pub fn validate(&self) -> Result<()> {
        if let Some(range) = self.range {
            validate_range(range)?;
        }
        if !self.scale.is_finite() || self.scale <= 0.0 {
            return Err(ExportError::Unsupported("output scale must be finite and positive".into()));
        }
        if let Some((width, height)) = self.frame_size {
            filmcraft_project::validate_frame_size(width, height).map_err(ExportError::Unsupported)?;
        }
        if let Some(rate) = self.frame_rate
            && (rate.num <= 0
                || rate.den <= 0
                || rate.as_f64() > 1000.0
                || rate.num > i64::from(u32::MAX)
                || rate.den > i64::from(u32::MAX)
                || (i128::from(filmcraft_time::TICKS_PER_SECOND) * i128::from(rate.den) / i128::from(rate.num.max(1))) > i128::from(Tick::MAX.0))
        {
            return Err(ExportError::Unsupported("output frame rate must be positive and at most 1000 fps".into()));
        }
        if self.audio.sample_rate.is_some_and(|rate| rate == 0 || rate > 384_000) {
            return Err(ExportError::Unsupported("output sample rate must be between 1 and 384000 Hz".into()));
        }
        if self.field_order != FieldOrder::Progressive && self.has_video() {
            return Err(ExportError::Unsupported(format!("{} field order: FilmCraft's encoders write progressive frames", self.field_order.label())));
        }
        if self.effects.image_overlay.enabled && self.effects.image_overlay.path.trim().is_empty() {
            return Err(ExportError::Unsupported("image overlay: no image file chosen".into()));
        }
        if self.format == Format::Hevc && self.bitrate_mode == BitrateMode::Vbr2Pass {
            return Err(ExportError::Unsupported("H.265 export has no two-pass mode: choose CBR or VBR, 1 pass".into()));
        }
        Ok(())
    }
}

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
        let mut pace = self.pace.lock().unwrap_or_else(|e| e.into_inner());
        pace.observe(now, done);
        if self.finished.load(Ordering::Relaxed) || done >= total {
            return None;
        }
        pace.eta(total - done)
    }
    fn set_status(&self, s: impl Into<String>) {
        *self.status.lock().unwrap_or_else(|e| e.into_inner()) = s.into();
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

/// A packet produced by a video encoder.
pub struct EncodedPacket {
    pub data: Vec<u8>,
    pub key: bool,
    /// Duration in encoder timescale units.
    pub duration: u32,
    /// pts − dts in encoder timescale units.
    pub composition_offset: i32,
}

/// Input picture for encoders: straight sRGB RGBA8 (encoders convert to their own YUV), or for
/// HDR exports the encoded R'G'B' (PQ/HLG, BT.2020; 3 floats per pixel, 0..1) in `hdr`.
pub struct EncoderFrame<'a> {
    pub width: u32,
    pub height: u32,
    pub rgba: &'a [u8],
    pub hdr: Option<&'a [f32]>,
    pub index: u64,
}

pub trait VideoEncoder: Send {
    /// MP4/MOV sample entry (codec config) — may only be complete after the first frame.
    fn sample_entry(&self) -> SampleEntry;
    fn timescale(&self) -> u32;
    fn encode(&mut self, frame: &EncoderFrame) -> Result<Vec<EncodedPacket>>;
    fn flush(&mut self) -> Result<Vec<EncodedPacket>>;
    /// Media start offset for an edit list (B-frame delay), in the encoder timescale.
    fn media_start(&self) -> Option<i64> {
        None
    }
    /// First-pass statistics of a two-pass encode (H.264).
    fn pass_stats(&self) -> Option<filmcraft_h264enc::PassStats> {
        None
    }
}

/// Audio encoder (AAC) plugged in by codec crates; PCM is built in.
pub trait AudioEncoder: Send {
    fn sample_entry(&self) -> SampleEntry;
    fn encode(&mut self, planar: &[Vec<f32>]) -> Result<Vec<Vec<u8>>>;
    fn flush(&mut self) -> Result<Vec<Vec<u8>>>;
    /// Encoder delay (priming) in samples.
    fn priming(&self) -> u32;
    /// Samples per access unit (1024 for AAC).
    fn frame_size(&self) -> u32;
}

pub type EncoderFactory = fn(format: Format, width: u32, height: u32, rate: FrameRate, settings: &ExportSettings) -> Option<Result<Box<dyn VideoEncoder>>>;
pub type AudioEncoderFactory = fn(format: Format, sample_rate: u32, channels: u32, settings: &ExportSettings) -> Option<Result<Box<dyn AudioEncoder>>>;

fn video_factories() -> &'static RwLock<Vec<EncoderFactory>> {
    static F: OnceLock<RwLock<Vec<EncoderFactory>>> = OnceLock::new();
    F.get_or_init(|| RwLock::new(vec![h264_factory, prores_factory, dnx_factory, apv_factory, mjpeg_factory]))
}
fn audio_factories() -> &'static RwLock<Vec<AudioEncoderFactory>> {
    static F: OnceLock<RwLock<Vec<AudioEncoderFactory>>> = OnceLock::new();
    F.get_or_init(|| RwLock::new(vec![aac_factory]))
}

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

/// Register a video encoder factory (tried before the built-in ones and those registered earlier).
/// Registering the same factory twice is harmless.
pub fn register_encoder(f: EncoderFactory) {
    let mut g = video_factories().write().unwrap_or_else(|e| e.into_inner());
    if !g.iter().any(|x| std::ptr::fn_addr_eq(*x, f)) {
        g.insert(0, f);
    }
}

/// Whether `f` is among the registered video encoder factories (startup diagnostics, tests).
pub fn encoder_registered(f: EncoderFactory) -> bool {
    video_factories().read().unwrap_or_else(|e| e.into_inner()).iter().any(|x| std::ptr::fn_addr_eq(*x, f))
}
pub fn register_audio_encoder(f: AudioEncoderFactory) {
    audio_factories().write().unwrap_or_else(|e| e.into_inner()).insert(0, f);
}

type FormatProbe = (Format, fn() -> bool);

fn format_probes() -> &'static RwLock<Vec<FormatProbe>> {
    static P: OnceLock<RwLock<Vec<FormatProbe>>> = OnceLock::new();
    P.get_or_init(|| RwLock::new(Vec::new()))
}

/// Say how to find out whether the encoder of a format without a built-in one ([`Format::has_builtin_encoder`])
/// works on this machine: `probe` runs when [`available`] asks (it should cache its answer).
/// Registering the same probe twice is harmless.
pub fn register_format_probe(format: Format, probe: fn() -> bool) {
    let mut g = format_probes().write().unwrap_or_else(|e| e.into_inner());
    if !g.iter().any(|(f, p)| *f == format && std::ptr::fn_addr_eq(*p, probe)) {
        g.push((format, probe));
    }
}

fn hdr_probes() -> &'static RwLock<Vec<FormatProbe>> {
    static P: OnceLock<RwLock<Vec<FormatProbe>>> = OnceLock::new();
    P.get_or_init(|| RwLock::new(Vec::new()))
}

/// Say how to find out whether the registered encoder of `format` can write HDR (PQ / HLG) on this
/// machine: `probe` runs when [`hdr_available`] asks (it should cache its answer). Only H.265 needs
/// one: its encoders are hardware-only and not all of them take 10-bit pictures, so an HDR sequence
/// exports as tone-mapped SDR unless a probe says otherwise. Registering the same probe twice is harmless.
pub fn register_hdr_probe(format: Format, probe: fn() -> bool) {
    let mut g = hdr_probes().write().unwrap_or_else(|e| e.into_inner());
    if !g.iter().any(|(f, p)| *f == format && std::ptr::fn_addr_eq(*p, probe)) {
        g.push((format, probe));
    }
}

/// Whether a registered probe says the encoder of `format` writes HDR here (false without one).
pub fn hdr_available(format: Format) -> bool {
    // the probes run outside the lock: one may take a while
    let probes: Vec<fn() -> bool> = hdr_probes().read().unwrap_or_else(|e| e.into_inner()).iter().filter(|(f, _)| *f == format).map(|(_, p)| *p).collect();
    probes.into_iter().any(|p| p())
}

/// Whether a format can currently be exported: it has a built-in encoder, or a registered probe
/// says its encoder works here.
pub fn available(format: Format) -> bool {
    if !Format::ALL.contains(&format) {
        return false;
    }
    format.has_builtin_encoder() || format_probes().read().unwrap_or_else(|e| e.into_inner()).iter().any(|(f, probe)| *f == format && probe())
}

struct MjpegEncoder {
    w: u16,
    h: u16,
    quality: u8,
    rate: FrameRate,
}

impl VideoEncoder for MjpegEncoder {
    fn sample_entry(&self) -> SampleEntry {
        SampleEntry::jpeg(self.w, self.h)
    }
    fn timescale(&self) -> u32 {
        self.rate.num as u32
    }
    fn encode(&mut self, f: &EncoderFrame) -> Result<Vec<EncodedPacket>> {
        let rgb: Vec<u8> = f.rgba.as_chunks::<4>().0.iter().flat_map(|p| [p[0], p[1], p[2]]).collect();
        let mut out = Vec::new();
        let mut enc = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, self.quality);
        enc.encode(&rgb, f.width, f.height, image::ExtendedColorType::Rgb8).map_err(|e| ExportError::Encode(e.to_string()))?;
        Ok(vec![EncodedPacket { data: out, key: true, duration: self.rate.den as u32, composition_offset: 0 }])
    }
    fn flush(&mut self) -> Result<Vec<EncodedPacket>> {
        Ok(Vec::new())
    }
}

fn mjpeg_factory(format: Format, w: u32, h: u32, rate: FrameRate, s: &ExportSettings) -> Option<Result<Box<dyn VideoEncoder>>> {
    (format == Format::Mjpeg).then(|| Ok(Box::new(MjpegEncoder { w: w as u16, h: h as u16, quality: s.quality.clamp(1, 100), rate }) as Box<dyn VideoEncoder>))
}

/// AAC-LC (our encoder), 320 kbps stereo by default.
struct AacEncoder {
    enc: filmcraft_aac::Encoder,
    rate: u32,
    channels: u32,
}

impl AudioEncoder for AacEncoder {
    fn sample_entry(&self) -> SampleEntry {
        SampleEntry::aac(self.enc.audio_specific_config(), self.channels, self.rate)
    }
    fn encode(&mut self, planar: &[Vec<f32>]) -> Result<Vec<Vec<u8>>> {
        let refs: Vec<&[f32]> = if planar.len() == 6 {
            // ours: L, R, C, LFE, Ls, Rs → AAC channel configuration 6: C, L, R, Ls, Rs, LFE
            [2usize, 0, 1, 4, 5, 3].iter().map(|&c| planar[c].as_slice()).collect()
        } else {
            planar.iter().map(Vec::as_slice).collect()
        };
        Ok(self.enc.encode(&refs))
    }
    fn flush(&mut self) -> Result<Vec<Vec<u8>>> {
        Ok(self.enc.flush())
    }
    fn priming(&self) -> u32 {
        self.enc.priming_samples()
    }
    fn frame_size(&self) -> u32 {
        1024
    }
}

fn aac_factory(_format: Format, sample_rate: u32, channels: u32, s: &ExportSettings) -> Option<Result<Box<dyn AudioEncoder>>> {
    // AAC caps a frame at 6144 bits per channel (ISO/IEC 14496-3 §4.5.3.2): 6 bits per sample, so
    // 264.6 kbps for stereo at 22.05 kHz; a higher setting is capped instead of refused
    let max = (6 * sample_rate as u64 * channels.max(1) as u64).min(u32::MAX as u64) as u32;
    let bps = (s.audio.bitrate_kbps.clamp(32, 512) * 1000).min(max);
    Some(
        filmcraft_aac::Encoder::new(filmcraft_aac::EncoderConfig::cbr(sample_rate, channels as usize, bps))
            .map(|enc| Box::new(AacEncoder { enc, rate: sample_rate, channels }) as Box<dyn AudioEncoder>)
            .map_err(|e| ExportError::Encode(e.to_string())),
    )
}

/// ProRes 422 encoder (HQ unless the settings pick another flavour): sRGB/709 RGBA8 → 10-bit limited-range BT.709 4:2:2.
struct ProResEncoder {
    enc: filmcraft_prores::Encoder,
    profile: filmcraft_prores::Profile,
    w: u32,
    h: u32,
    rate: FrameRate,
    signal: ColorSignal,
}

impl VideoEncoder for ProResEncoder {
    fn sample_entry(&self) -> SampleEntry {
        let mut e = SampleEntry::prores(filmcraft_isobmff::FourCc(self.profile.fourcc()), self.w as u16, self.h as u16);
        self.signal.apply_to(&mut e, true);
        e
    }
    fn timescale(&self) -> u32 {
        self.rate.num as u32
    }
    fn encode(&mut self, f: &EncoderFrame) -> Result<Vec<EncodedPacket>> {
        let mut fr = filmcraft_prores::Frame::new(f.width, f.height, filmcraft_prores::ChromaFormat::Yuv422, 10, false);
        timed(Stage::Convert, || match f.hdr {
            Some(rgb) => {
                let (kr, kb) = self.signal.kr_kb();
                rgbf_to_yuv422_10(rgb, f.width as usize, f.height as usize, kr, kb, &mut fr.y, &mut fr.cb, &mut fr.cr)
            }
            None => rgba_to_yuv422_10(f.rgba, f.width as usize, f.height as usize, &mut fr.y, &mut fr.cb, &mut fr.cr),
        });
        let data = self.enc.encode(&fr).map_err(|e| ExportError::Encode(e.to_string()))?;
        Ok(vec![EncodedPacket { data, key: true, duration: self.rate.den as u32, composition_offset: 0 }])
    }
    fn flush(&mut self) -> Result<Vec<EncodedPacket>> {
        Ok(Vec::new())
    }
}

/// BT.709 limited-range 10-bit 4:2:2 from straight RGBA8 (chroma averaged horizontally).
pub fn rgba_to_yuv422_10(rgba: &[u8], w: usize, h: usize, y: &mut [u16], cb: &mut [u16], cr: &mut [u16]) {
    let cw = w.div_ceil(2);
    debug_assert!(y.len() >= w * h && rgba.len() >= w * h * 4);
    y.par_chunks_mut(w).zip(cb.par_chunks_mut(cw).zip(cr.par_chunks_mut(cw))).enumerate().for_each(|(row, (yr, (cbr, crr)))| {
        let src = &rgba[row * w * 4..(row + 1) * w * 4];
        let mut us = vec![0f32; w];
        let mut vs = vec![0f32; w];
        for x in 0..w {
            let (r, g, b) = (src[x * 4] as f32 / 255.0, src[x * 4 + 1] as f32 / 255.0, src[x * 4 + 2] as f32 / 255.0);
            let yy = 0.2126 * r + 0.7152 * g + 0.0722 * b;
            yr[x] = (64.0 + 876.0 * yy).round().clamp(4.0, 1019.0) as u16;
            us[x] = (b - yy) / 1.8556;
            vs[x] = (r - yy) / 1.5748;
        }
        for cx in 0..cw {
            let a = cx * 2;
            let b2 = (a + 1).min(w - 1);
            let u = (us[a] + us[b2]) * 0.5;
            let v = (vs[a] + vs[b2]) * 0.5;
            cbr[cx] = (512.0 + 896.0 * u).round().clamp(4.0, 1019.0) as u16;
            crr[cx] = (512.0 + 896.0 * v).round().clamp(4.0, 1019.0) as u16;
        }
    });
}

/// The ProRes profile named by [`ExportSettings::prores_profile`].
pub fn prores_profile(name: &str) -> filmcraft_prores::Profile {
    use filmcraft_prores::Profile;
    match name.to_ascii_lowercase().as_str() {
        "proxy" => Profile::Proxy,
        "lt" => Profile::Lt,
        "standard" | "422" => Profile::Standard,
        _ => Profile::Hq,
    }
}

/// Limited-range 10-bit 4:2:2 from encoded R'G'B' floats with matrix (Kr, Kb).
#[allow(clippy::too_many_arguments)]
pub fn rgbf_to_yuv422_10(rgb: &[f32], w: usize, h: usize, kr: f32, kb: f32, y: &mut [u16], cb: &mut [u16], cr: &mut [u16]) {
    let cw = w.div_ceil(2);
    debug_assert!(rgb.len() >= w * h * 3 && y.len() >= w * h);
    let kg = 1.0 - kr - kb;
    let (sb, sr) = (2.0 * (1.0 - kb), 2.0 * (1.0 - kr));
    y.par_chunks_mut(w).zip(cb.par_chunks_mut(cw).zip(cr.par_chunks_mut(cw))).enumerate().for_each(|(row, (yr, (cbr, crr)))| {
        let src = &rgb[row * w * 3..(row + 1) * w * 3];
        let mut us = vec![0f32; w];
        let mut vs = vec![0f32; w];
        for x in 0..w {
            let (r, g, b) = (src[x * 3], src[x * 3 + 1], src[x * 3 + 2]);
            let yy = kr * r + kg * g + kb * b;
            yr[x] = (64.0 + 876.0 * yy).round().clamp(4.0, 1019.0) as u16;
            us[x] = (b - yy) / sb;
            vs[x] = (r - yy) / sr;
        }
        for cx in 0..cw {
            let a = cx * 2;
            let b2 = (a + 1).min(w - 1);
            cbr[cx] = (512.0 + 896.0 * (us[a] + us[b2]) * 0.5).round().clamp(4.0, 1019.0) as u16;
            crr[cx] = (512.0 + 896.0 * (vs[a] + vs[b2]) * 0.5).round().clamp(4.0, 1019.0) as u16;
        }
    });
}

fn prores_factory(format: Format, w: u32, h: u32, rate: FrameRate, s: &ExportSettings) -> Option<Result<Box<dyn VideoEncoder>>> {
    (format == Format::ProRes).then(|| {
        let profile = prores_profile(&s.prores_profile);
        let mut cfg = filmcraft_prores::EncoderConfig::new(profile, w, h);
        if s.signal.is_hdr() {
            cfg.color = filmcraft_prores::ColorInfo { primaries: s.signal.primaries, transfer: s.signal.transfer, matrix: s.signal.matrix };
        }
        Ok(Box::new(ProResEncoder { enc: filmcraft_prores::Encoder::with_config(cfg), profile, w, h, rate, signal: s.signal }) as Box<dyn VideoEncoder>)
    })
}

/// The DNxHR profile named by [`ExportSettings::dnx_profile`].
pub fn dnx_profile(name: &str) -> filmcraft_dnx::Profile {
    use filmcraft_dnx::Profile;
    match name.to_ascii_lowercase().as_str() {
        "lb" => Profile::Lb,
        "sq" => Profile::Sq,
        "hqx" => Profile::Hqx,
        _ => Profile::Hq,
    }
}

/// DNxHR encoder (RGBA8 or HDR floats → BT.709 / BT.2020 limited-range 4:2:2; 8-bit for
/// LB/SQ/HQ, 10-bit for HQX).
struct DnxEncoder {
    enc: filmcraft_dnx::Encoder,
    w: u32,
    h: u32,
    rate: FrameRate,
    signal: ColorSignal,
}

impl VideoEncoder for DnxEncoder {
    fn sample_entry(&self) -> SampleEntry {
        let mut e = SampleEntry::dnx(filmcraft_isobmff::FourCc(*b"AVdh"), self.w as u16, self.h as u16);
        self.signal.apply_to(&mut e, true);
        e
    }
    fn timescale(&self) -> u32 {
        self.rate.num as u32
    }
    fn encode(&mut self, f: &EncoderFrame) -> Result<Vec<EncodedPacket>> {
        let mut fr = filmcraft_dnx::Frame::new(f.width, f.height, filmcraft_dnx::ChromaFormat::Yuv422, 10, false);
        match f.hdr {
            Some(rgb) => {
                let (kr, kb) = self.signal.kr_kb();
                rgbf_to_yuv422_10(rgb, f.width as usize, f.height as usize, kr, kb, &mut fr.y, &mut fr.cb, &mut fr.cr)
            }
            None => rgba_to_yuv422_10(f.rgba, f.width as usize, f.height as usize, &mut fr.y, &mut fr.cb, &mut fr.cr),
        }
        // the encoder rescales 10-bit input to its coded depth
        let data = self.enc.encode(&fr).map_err(|e| ExportError::Encode(e.to_string()))?;
        Ok(vec![EncodedPacket { data, key: true, duration: self.rate.den as u32, composition_offset: 0 }])
    }
    fn flush(&mut self) -> Result<Vec<EncodedPacket>> {
        Ok(Vec::new())
    }
}

fn dnx_factory(format: Format, w: u32, h: u32, rate: FrameRate, s: &ExportSettings) -> Option<Result<Box<dyn VideoEncoder>>> {
    (format == Format::DnxHr).then(|| {
        let mut cfg = filmcraft_dnx::EncoderConfig::new(dnx_profile(&s.dnx_profile), w, h);
        if s.signal.primaries == 9 {
            cfg.color_volume = filmcraft_dnx::ColorVolume::Bt2020Ncl;
        }
        let enc = filmcraft_dnx::Encoder::with_config(cfg).map_err(|e| ExportError::Encode(e.to_string()))?;
        Ok(Box::new(DnxEncoder { enc, w, h, rate, signal: s.signal }) as Box<dyn VideoEncoder>)
    })
}

/// The APV profile named by [`ExportSettings::apv_profile`].
pub fn apv_profile(name: &str) -> filmcraft_apv::Profile {
    use filmcraft_apv::Profile;
    match name.to_ascii_lowercase().replace([' ', '_'], "-").trim_start_matches("apv-").trim_start_matches("apv") {
        "422-12" | "42212" => Profile::P422_12,
        "444-10" | "44410" => Profile::P444_10,
        "444-12" | "44412" => Profile::P444_12,
        "4444-10" | "444410" => Profile::P4444_10,
        "4444-12" | "444412" => Profile::P4444_12,
        "400-10" | "40010" => Profile::P400_10,
        _ => Profile::P422_10,
    }
}

/// APV encoder (RFC 9924: 10/12-bit 4:2:2 or 4:4:4 intra).
struct ApvEncoder {
    enc: filmcraft_apv::Encoder,
    chroma: filmcraft_apv::ChromaFormat,
    w: u32,
    h: u32,
    rate: FrameRate,
    signal: ColorSignal,
}

impl VideoEncoder for ApvEncoder {
    fn sample_entry(&self) -> SampleEntry {
        let apvc = filmcraft_isobmff::ApvConfig::parse(&self.enc.decoder_config_record()).unwrap_or_default();
        let mut e = SampleEntry::apv(apvc, self.w as u16, self.h as u16);
        self.signal.apply_to(&mut e, true);
        e
    }
    fn timescale(&self) -> u32 {
        self.rate.num as u32
    }
    fn encode(&mut self, f: &EncoderFrame) -> Result<Vec<EncodedPacket>> {
        let (w, h) = (f.width as usize, f.height as usize);
        let mut fr = filmcraft_apv::Frame::new(f.width, f.height, self.chroma, 10, self.chroma == filmcraft_apv::ChromaFormat::Yuv4444);
        let mut y422 = vec![0u16; w * h];
        let cw = w.div_ceil(2);
        let mut cb422 = vec![0u16; cw * h];
        let mut cr422 = vec![0u16; cw * h];
        match f.hdr {
            Some(rgb) => {
                let (kr, kb) = self.signal.kr_kb();
                rgbf_to_yuv422_10(rgb, w, h, kr, kb, &mut y422, &mut cb422, &mut cr422);
            }
            None => rgba_to_yuv422_10(f.rgba, w, h, &mut y422, &mut cb422, &mut cr422),
        }
        fr.y = y422;
        match self.chroma {
            filmcraft_apv::ChromaFormat::Monochrome => {
                fr.cb.clear();
                fr.cr.clear();
            }
            filmcraft_apv::ChromaFormat::Yuv422 => {
                fr.cb = cb422;
                fr.cr = cr422;
            }
            filmcraft_apv::ChromaFormat::Yuv444 | filmcraft_apv::ChromaFormat::Yuv4444 => {
                for row in 0..h {
                    for x in 0..w {
                        fr.cb[row * w + x] = cb422[row * cw + x / 2];
                        fr.cr[row * w + x] = cr422[row * cw + x / 2];
                    }
                }
                if let Some(a) = fr.alpha.as_mut() {
                    for (i, dst) in a.iter_mut().enumerate() {
                        *dst = f.rgba.get(i * 4 + 3).map_or(1023, |&v| ((v as u32 * 1023 + 127) / 255) as u16);
                    }
                }
            }
        }
        let data = self.enc.encode_raw_au(&fr).map_err(|e| ExportError::Encode(e.to_string()))?;
        Ok(vec![EncodedPacket { data, key: true, duration: self.rate.den as u32, composition_offset: 0 }])
    }
    fn flush(&mut self) -> Result<Vec<EncodedPacket>> {
        Ok(Vec::new())
    }
}

fn apv_factory(format: Format, w: u32, h: u32, rate: FrameRate, s: &ExportSettings) -> Option<Result<Box<dyn VideoEncoder>>> {
    (format == Format::Apv).then(|| {
        let profile = apv_profile(&s.apv_profile);
        let mut cfg = filmcraft_apv::EncoderConfig::new(profile, w, h);
        if s.signal.is_hdr() {
            cfg.color = filmcraft_apv::ColorInfo { primaries: s.signal.primaries, transfer: s.signal.transfer, matrix: s.signal.matrix, full_range: false };
        }
        let chroma = cfg.chroma;
        let enc = filmcraft_apv::Encoder::with_config(cfg).map_err(|e| ExportError::Encode(e.to_string()))?;
        Ok(Box::new(ApvEncoder { enc, chroma, w, h, rate, signal: s.signal }) as Box<dyn VideoEncoder>)
    })
}

/// H.264 High (our encoder): sRGB/709 RGBA8 → 8-bit limited-range BT.709 4:2:0, VBR at the
/// requested bitrate, length-prefixed samples with the `avcC` in the sample entry.
struct H264Encoder {
    enc: filmcraft_h264enc::Encoder,
    w: u32,
    h: u32,
    rate: FrameRate,
    y: Vec<u8>,
    u: Vec<u8>,
    v: Vec<u8>,
    signal: ColorSignal,
}

impl H264Encoder {
    fn packets(&self, ps: Vec<filmcraft_h264enc::Packet>) -> Vec<EncodedPacket> {
        ps.into_iter()
            .map(|p| EncodedPacket { data: p.data, key: p.keyframe, duration: self.rate.den as u32, composition_offset: (p.pts - p.dts) as i32 })
            .collect()
    }
}

impl VideoEncoder for H264Encoder {
    fn sample_entry(&self) -> SampleEntry {
        let cfg = filmcraft_isobmff::AvcConfig::parse(&self.enc.avcc()).unwrap_or_else(|_| {
            let (sps, pps) = self.enc.sps_pps();
            filmcraft_isobmff::AvcConfig::new(vec![sps], vec![pps], 4)
        });
        let mut e = SampleEntry::avc(cfg, self.w as u16, self.h as u16);
        self.signal.apply_to(&mut e, false);
        e
    }
    fn timescale(&self) -> u32 {
        self.rate.num as u32
    }
    fn encode(&mut self, f: &EncoderFrame) -> Result<Vec<EncodedPacket>> {
        timed(Stage::Convert, || match f.hdr {
            Some(rgb) => {
                let (kr, kb) = self.signal.kr_kb();
                rgbf_to_yuv420_8(rgb, f.width as usize, f.height as usize, kr, kb, &mut self.y, &mut self.u, &mut self.v)
            }
            None => rgba_to_yuv420_8(f.rgba, f.width as usize, f.height as usize, &mut self.y, &mut self.u, &mut self.v),
        });
        let cw = (f.width as usize).div_ceil(2);
        let frame = filmcraft_h264enc::YuvFrame { y: &self.y, u: &self.u, v: &self.v, y_stride: f.width as usize, uv_stride: cw };
        let ps = self.enc.try_encode(&frame, f.index as i64 * self.rate.den).map_err(|e| ExportError::Encode(e.to_string()))?;
        Ok(self.packets(ps))
    }
    fn flush(&mut self) -> Result<Vec<EncodedPacket>> {
        let ps = self.enc.flush();
        Ok(self.packets(ps))
    }
    fn media_start(&self) -> Option<i64> {
        // With B-frames the first DTS is one frame before the first PTS.
        (self.enc.delay() > 0).then_some(self.rate.den)
    }
    fn pass_stats(&self) -> Option<filmcraft_h264enc::PassStats> {
        self.enc.pass_stats()
    }
}

/// BT.709 limited-range 8-bit 4:2:0 from straight RGBA8 (2×2 chroma average).
pub fn rgba_to_yuv420_8(rgba: &[u8], w: usize, h: usize, y: &mut Vec<u8>, u: &mut Vec<u8>, v: &mut Vec<u8>) {
    let (cw, ch) = (w.div_ceil(2), h.div_ceil(2));
    y.resize(w * h, 0);
    u.resize(cw * ch, 0);
    v.resize(cw * ch, 0);
    y.par_chunks_mut(w * 2).zip(u.par_chunks_mut(cw).zip(v.par_chunks_mut(cw))).enumerate().for_each(|(cy, (yr, (ur, vr)))| {
        let rows = yr.len() / w;
        let mut us = vec![0f32; cw];
        let mut vs = vec![0f32; cw];
        let mut cnt = vec![0f32; cw];
        for dy in 0..rows {
            let row = cy * 2 + dy;
            let src = &rgba[row * w * 4..(row + 1) * w * 4];
            for x in 0..w {
                let (r, g, b) = (src[x * 4] as f32 / 255.0, src[x * 4 + 1] as f32 / 255.0, src[x * 4 + 2] as f32 / 255.0);
                let yy = 0.2126 * r + 0.7152 * g + 0.0722 * b;
                yr[dy * w + x] = (16.0 + 219.0 * yy).round().clamp(1.0, 254.0) as u8;
                us[x / 2] += (b - yy) / 1.8556;
                vs[x / 2] += (r - yy) / 1.5748;
                cnt[x / 2] += 1.0;
            }
        }
        for cx in 0..cw {
            ur[cx] = (128.0 + 224.0 * us[cx] / cnt[cx]).round().clamp(1.0, 254.0) as u8;
            vr[cx] = (128.0 + 224.0 * vs[cx] / cnt[cx]).round().clamp(1.0, 254.0) as u8;
        }
    });
}

/// Limited-range 8-bit 4:2:0 from encoded R'G'B' floats with matrix (Kr, Kb).
#[allow(clippy::too_many_arguments)]
pub fn rgbf_to_yuv420_8(rgb: &[f32], w: usize, h: usize, kr: f32, kb: f32, y: &mut Vec<u8>, u: &mut Vec<u8>, v: &mut Vec<u8>) {
    let (cw, ch) = (w.div_ceil(2), h.div_ceil(2));
    let kg = 1.0 - kr - kb;
    let (sb, sr) = (2.0 * (1.0 - kb), 2.0 * (1.0 - kr));
    y.resize(w * h, 0);
    u.resize(cw * ch, 0);
    v.resize(cw * ch, 0);
    y.par_chunks_mut(w * 2).zip(u.par_chunks_mut(cw).zip(v.par_chunks_mut(cw))).enumerate().for_each(|(cy, (yr, (ur, vr)))| {
        let rows = yr.len() / w;
        let mut us = vec![0f32; cw];
        let mut vs = vec![0f32; cw];
        let mut cnt = vec![0f32; cw];
        for dy in 0..rows {
            let row = cy * 2 + dy;
            let src = &rgb[row * w * 3..(row + 1) * w * 3];
            for x in 0..w {
                let (r, g, b) = (src[x * 3], src[x * 3 + 1], src[x * 3 + 2]);
                let yy = kr * r + kg * g + kb * b;
                yr[dy * w + x] = (16.0 + 219.0 * yy).round().clamp(1.0, 254.0) as u8;
                us[x / 2] += (b - yy) / sb;
                vs[x / 2] += (r - yy) / sr;
                cnt[x / 2] += 1.0;
            }
        }
        for cx in 0..cw {
            ur[cx] = (128.0 + 224.0 * us[cx] / cnt[cx]).round().clamp(1.0, 254.0) as u8;
            vr[cx] = (128.0 + 224.0 * vs[cx] / cnt[cx]).round().clamp(1.0, 254.0) as u8;
        }
    });
}

/// Limited-range 10-bit 4:2:0 (code values 0..1023, not MSB-aligned) from encoded R'G'B' floats with
/// matrix (Kr, Kb): Y = 64 + 876·Y', C = 512 + 896·C, chroma averaged over each 2x2 block (one pixel
/// or pair at an odd edge). `rgb` is 3 floats per pixel; NaN counts as 0, infinities and
/// out-of-range values clamp to 0..1 first, and codes clamp to 4..1019 like [`rgbf_to_yuv422_10`]
/// (so black is 64, peak white 940 and the chroma of any colour stays within 64..960).
/// The planes are resized to `w * h` and `ceil(w / 2) * ceil(h / 2)`. A `rgb` slice of the wrong
/// length, a zero size or a size whose arithmetic overflows is an error.
#[allow(clippy::too_many_arguments)]
pub fn rgbf_to_yuv420_10(rgb: &[f32], w: usize, h: usize, kr: f32, kb: f32, y: &mut Vec<u16>, u: &mut Vec<u16>, v: &mut Vec<u16>) -> Result<()> {
    let bad = || ExportError::Encode(format!("a {w}x{h} HDR picture needs w*h*3 floats, got {}", rgb.len()));
    let px = w.checked_mul(h).ok_or_else(bad)?;
    if w == 0 || h == 0 || px.checked_mul(3) != Some(rgb.len()) {
        return Err(bad());
    }
    let (cw, ch) = (w.div_ceil(2), h.div_ceil(2));
    let kg = 1.0 - kr - kb;
    let (sb, sr) = (2.0 * (1.0 - kb), 2.0 * (1.0 - kr));
    y.clear();
    y.resize(px, 0);
    u.clear();
    u.resize(cw * ch, 0);
    v.clear();
    v.resize(cw * ch, 0);
    let unit = |x: f32| if x.is_nan() { 0.0 } else { x.clamp(0.0, 1.0) };
    let code = |x: f32| x.round().clamp(4.0, 1019.0) as u16;
    y.par_chunks_mut(w * 2).zip(u.par_chunks_mut(cw).zip(v.par_chunks_mut(cw))).enumerate().for_each(|(cy, (yr, (ur, vr)))| {
        let rows = yr.len() / w;
        let mut us = vec![0f32; cw];
        let mut vs = vec![0f32; cw];
        let mut cnt = vec![0f32; cw];
        for (dy, yrow) in yr.chunks_exact_mut(w).enumerate().take(rows) {
            let row = cy * 2 + dy;
            let Some(src) = rgb.get(row * w * 3..(row + 1) * w * 3) else { continue };
            for (x, (&[r, g, b], yo)) in src.as_chunks::<3>().0.iter().zip(yrow.iter_mut()).enumerate() {
                let (r, g, b) = (unit(r), unit(g), unit(b));
                let yy = kr * r + kg * g + kb * b;
                *yo = code(64.0 + 876.0 * yy);
                if let (Some(a), Some(c), Some(n)) = (us.get_mut(x / 2), vs.get_mut(x / 2), cnt.get_mut(x / 2)) {
                    *a += (b - yy) / sb;
                    *c += (r - yy) / sr;
                    *n += 1.0;
                }
            }
        }
        for (cx, (uo, vo)) in ur.iter_mut().zip(vr.iter_mut()).enumerate() {
            let n = cnt.get(cx).copied().unwrap_or(1.0).max(1.0);
            *uo = code(512.0 + 896.0 * us.get(cx).copied().unwrap_or(0.0) / n);
            *vo = code(512.0 + 896.0 * vs.get(cx).copied().unwrap_or(0.0) / n);
        }
    });
    Ok(())
}

/// Slices per H.264 picture: one per four macroblock rows. The encoder's default follows the core
/// count, which would make the stream (and every decoded picture) depend on the machine; this is
/// what it picks on a machine with enough cores.
pub(crate) fn h264_slices(height: u32) -> usize {
    (height.div_ceil(16) as usize).div_ceil(4).max(1)
}

fn h264_factory(format: Format, w: u32, h: u32, rate: FrameRate, s: &ExportSettings) -> Option<Result<Box<dyn VideoEncoder>>> {
    if format != Format::H264 {
        return None;
    }
    let mut cfg = filmcraft_h264enc::EncoderConfig::new(w, h, rate.num as u32, rate.den as u32);
    // MXF carries the Annex B byte stream (ST 381-3) with in-band parameter sets
    cfg.format = if s.format.is_mxf() { filmcraft_h264enc::PacketFormat::AnnexB } else { filmcraft_h264enc::PacketFormat::LengthPrefixed };
    cfg.aud = s.format.is_mxf();
    cfg.keyint = s.keyframe_distance.filter(|k| *k > 0).unwrap_or_else(|| (rate.num as f64 / rate.den as f64 * 2.0).round().max(1.0) as u32);
    cfg.slices = h264_slices(h);
    let kbps = s.bitrate_kbps.max(100);
    let max = s.max_bitrate_kbps.filter(|m| *m >= kbps).unwrap_or_else(|| (u64::from(kbps) * 3 / 2).min(u64::from(u32::MAX)) as u32);
    cfg.rate = match s.bitrate_mode {
        BitrateMode::Cbr => filmcraft_h264enc::RateControl::Cbr { kbps },
        _ => filmcraft_h264enc::RateControl::Vbr { target_kbps: kbps, max_kbps: max },
    };
    cfg.pass = match &s.h264_pass {
        H264Pass::Single => filmcraft_h264enc::Pass::Single,
        H264Pass::First => filmcraft_h264enc::Pass::First,
        H264Pass::Second(st) => filmcraft_h264enc::Pass::Second(st.clone()),
    };
    cfg.profile = match s.h264_profile {
        H264Profile::Baseline => filmcraft_h264enc::Profile::Baseline,
        H264Profile::Main => filmcraft_h264enc::Profile::Main,
        H264Profile::High => filmcraft_h264enc::Profile::High,
    };
    cfg.level = s.h264_level;
    if let Some((n, d)) = s.pixel_aspect {
        cfg.sar = (n.clamp(1, 65535) as u16, d.clamp(1, 65535) as u16);
    }
    if s.signal.is_hdr() {
        cfg.color = filmcraft_h264enc::ColorConfig { primaries: s.signal.primaries, transfer: s.signal.transfer, matrix: s.signal.matrix, full_range: false };
        if let Some((md, cll)) = s.signal.static_metadata() {
            let mut b = [0u8; 24];
            b.copy_from_slice(&md.to_bytes());
            cfg.mastering_display = Some(b);
            cfg.content_light = Some(cll);
        }
    }
    Some(
        filmcraft_h264enc::Encoder::new(cfg)
            .map(|enc| Box::new(H264Encoder { enc, w, h, rate, y: Vec::new(), u: Vec::new(), v: Vec::new(), signal: s.signal }) as Box<dyn VideoEncoder>)
            .map_err(|e| ExportError::Encode(e.to_string())),
    )
}

/// The range to export (settings → In/Out → whole sequence).
pub fn export_range(project: &Project, seq: ItemId, settings: &ExportSettings) -> Result<TimeRange> {
    let q = project.sequence(seq).ok_or(ExportError::NoSequence)?;
    q.check_bounds().map_err(ExportError::Unsupported)?;
    if let Some(r) = settings.range {
        validate_range(r)?;
        return Ok(r);
    }
    let fd = q.settings.frame_rate.frame_duration();
    let a = q.mark_in.unwrap_or(Tick::ZERO);
    let minimum_end = a.0.checked_add(fd.0).ok_or_else(|| ExportError::Unsupported("export In point overflows the time range".into()))?;
    let b = match q.mark_out {
        Some(out) => out.0.checked_add(fd.0).ok_or_else(|| ExportError::Unsupported("export Out point overflows the time range".into()))?,
        None => q.duration().0,
    };
    let duration = b.max(minimum_end).checked_sub(a.0).ok_or_else(|| ExportError::Unsupported("export range duration overflows".into()))?;
    let range = TimeRange::new(a, Tick(duration));
    validate_range(range)?;
    Ok(range)
}

/// Validate input time before frame/sample conversion or any `TimeRange::end` arithmetic.
pub fn validate_range(range: TimeRange) -> Result<()> {
    if range.start.0 < 0 || range.duration.0 <= 0 || range.start.0.checked_add(range.duration.0).is_none_or(|end| end > Tick::MAX.0) {
        return Err(ExportError::Unsupported("export range must start at or after zero, have positive duration and end within supported time bounds".into()));
    }
    Ok(())
}

/// Output frames `[f0, f1)` of `range` at `rate` (frame `f` is at `rate.tick_of(f)`).
pub fn frame_span(rate: FrameRate, range: TimeRange) -> (i64, i64) {
    let f0 = rate.frame_at(range.start);
    let f1 = rate.frame_at(range.end() - Tick(1)) + 1;
    (f0, f1.max(f0))
}

/// An in-memory writer that stays reachable after the encoder that owns a clone is dropped.
#[derive(Clone, Default)]
struct SharedBuf(Arc<Mutex<Vec<u8>>>);

impl Write for SharedBuf {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap_or_else(|e| e.into_inner()).extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// Encode straight sRGB RGBA8 pixels as a PNG file (thumbnails, previews).
pub fn encode_png(rgba: Vec<u8>, w: u32, h: u32) -> Result<Vec<u8>> {
    encode_still(Format::PngSequence, rgba, w, h, false)
}

/// Encode one still of an image sequence (also Export Frame). PNG always carries the alpha
/// channel it is given; `alpha` keeps it in TIFF too, which is otherwise written as RGB.
pub fn encode_still(format: Format, rgba: Vec<u8>, w: u32, h: u32, alpha: bool) -> Result<Vec<u8>> {
    let enc = |e: image::ImageError| ExportError::Encode(e.to_string());
    let mut out = std::io::Cursor::new(Vec::new());
    match format {
        Format::PngSequence => {
            image::ImageEncoder::write_image(image::codecs::png::PngEncoder::new(&mut out), &rgba, w, h, image::ExtendedColorType::Rgba8).map_err(enc)?
        }
        Format::TiffSequence | Format::BmpSequence => {
            let img = image::RgbaImage::from_raw(w, h, rgba).ok_or_else(|| ExportError::Encode("frame size".into()))?;
            let f = if format == Format::TiffSequence { image::ImageFormat::Tiff } else { image::ImageFormat::Bmp };
            if alpha && format == Format::TiffSequence {
                img.write_to(&mut out, f).map_err(enc)?
            } else {
                image::DynamicImage::ImageRgba8(img).to_rgb8().write_to(&mut out, f).map_err(enc)?
            }
        }
        _ => return Err(ExportError::Unsupported(format!("{} is not an image sequence", format.label()))),
    }
    Ok(out.into_inner())
}

/// Run an export (blocking; call from a worker thread).
pub fn export(project: &Arc<Project>, seq: ItemId, settings: &ExportSettings, sources: &dyn SourceProvider, progress: &Progress) -> Result<Report> {
    settings.validate()?;
    if stepped(settings.format) {
        let mut ex = Exporter::new(project.clone(), seq, settings, progress)?;
        loop {
            match ex.step(sources, progress)? {
                Step::Progress => {}
                Step::Done(r) => return Ok(r),
                // only asynchronous (web) sources defer; a blocking export cannot wait for them
                Step::Pending => return Err(ExportError::Io("media data is still loading; run the export as a stepped job".into())),
            }
        }
    }
    let t0 = web_time::Instant::now();
    let range = export_range(project, seq, settings)?;
    let cancelled = || progress.cancel.load(Ordering::Relaxed);
    let batch = rayon::current_num_threads().clamp(2, 16) as i64;
    let (bytes, nframes) = match settings.format {
        Format::Wav | Format::Aiff => {
            if !settings.part_of_batch {
                progress.total.store(1, Ordering::Relaxed);
                progress.set_status(format!("Exporting audio ({})", settings.format.label()));
            }
            let mut a = audio_out::AudioOut::new(project.clone(), seq, settings, range)?;
            a.measure(settings, sources, &cancelled)?;
            *progress.loudness.lock().unwrap_or_else(|e| e.into_inner()) = a.loudness;
            let planar = a.rest(sources).unwrap_or_else(|| vec![Vec::new(); a.channels]);
            let inter = audio_out::interleave(&planar);
            let (ch, sr, bits) = (a.channels as u16, a.sr, settings.audio.bits);
            let data = if settings.format == Format::Wav { pcm::write_wav(&inter, ch, sr, bits) } else { pcm::write_aiff(&inter, ch, sr, bits) };
            let n = write_output(settings, &settings.path, data)?;
            progress.done.store(1, Ordering::Relaxed);
            (n, planar.first().map_or(0, Vec::len) as u64)
        }
        Format::PngSequence | Format::TiffSequence | Format::BmpSequence | Format::Gif => {
            let pipe = pipeline::Pipeline::new(project.clone(), seq, settings, false)?;
            let (f0, f1) = frame_span(pipe.rate, range);
            let count = (f1 - f0) as u64;
            let (w, h) = (pipe.w, pipe.h);
            if !settings.part_of_batch {
                progress.total.store(count, Ordering::Relaxed);
                progress.set_status(format!("Exporting {count} frames ({})", settings.format.label()));
            }
            let mut total = 0u64;
            let gif_buf = SharedBuf::default();
            let mut gif = if settings.format == Format::Gif {
                let mut enc = image::codecs::gif::GifEncoder::new_with_speed(gif_buf.clone(), 10);
                enc.set_repeat(image::codecs::gif::Repeat::Infinite).map_err(|e| ExportError::Encode(e.to_string()))?;
                Some(enc)
            } else {
                None
            };
            let delay = image::Delay::from_numer_denom_ms((1000 * pipe.rate.den) as u32, pipe.rate.num as u32);
            let mut f = f0;
            while f < f1 {
                if cancelled() {
                    return Err(ExportError::Cancelled);
                }
                let end = (f + batch).min(f1);
                if let Some(enc) = gif.as_mut() {
                    let frames: Vec<Vec<u8>> = (f..end).into_par_iter().map(|fi| pipe.frame(fi, sources).0).collect();
                    for rgba in frames {
                        let img = image::RgbaImage::from_raw(w, h, rgba).ok_or_else(|| ExportError::Encode("frame".into()))?;
                        enc.encode_frame(image::Frame::from_parts(img, 0, 0, delay)).map_err(|e| ExportError::Encode(e.to_string()))?;
                    }
                } else {
                    let written: Vec<Result<u64>> = (f..end)
                        .into_par_iter()
                        .map(|fi| {
                            let data = encode_still(settings.format, pipe.frame(fi, sources).0, w, h, settings.alpha)?;
                            write_output(settings, &pcm::image_sequence_path(&settings.path, (fi - f0) as u64, count), data)
                        })
                        .collect();
                    for r in written {
                        total += r?;
                    }
                }
                progress.done.fetch_add((end - f) as u64, Ordering::Relaxed);
                f = end;
            }
            if let Some(enc) = gif {
                let buf = gif_buf.clone();
                drop(enc);
                let data = std::mem::take(&mut *buf.0.lock().unwrap_or_else(|e| e.into_inner()));
                total = write_output(settings, &settings.path, data)?;
            }
            (total, count)
        }
        Format::H264 | Format::Hevc | Format::ProRes | Format::DnxHr | Format::Apv | Format::Mjpeg | Format::MxfOp1a | Format::MxfOpAtom => {
            // Handled by the stepped exporter above; reaching here would be a dispatch bug.
            return Err(ExportError::Unsupported(format!("{:?} must run as a stepped export", settings.format)));
        }
    };
    let secs = t0.elapsed().as_secs_f64();
    if !settings.part_of_batch {
        progress.finished.store(true, Ordering::Relaxed);
        progress.set_status(format!("Done in {secs:.1}s"));
    }
    Ok(Report { path: settings.path.clone(), frames: nframes, seconds: secs, bytes, render_fps: nframes as f64 / secs.max(1e-6), extra_files: Vec::new() })
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
