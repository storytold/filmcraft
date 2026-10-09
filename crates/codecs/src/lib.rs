//! The container + codec hub.
//!
//! - [`VideoDecoder`]: the trait every video codec implements (our own H.264/HEVC/VP9/ProRes/…, MJPEG, and
//!   OS hardware decoders registered by the platform layer). Factories are tried in registration
//!   order, so a hardware decoder can take precedence over the pure-Rust one.
//! - [`Mp4Source`]: a [`MediaSource`](filmcraft_media::MediaSource) over MP4/MOV using
//!   `filmcraft-isobmff`: GOP-aware random access (seek to the preceding sync sample and decode
//!   forward, caching every decoded frame of the GOP), sequential fast path for playback, and
//!   packet-cached audio decoding.
//! - [`MkvSource`], [`MxfSource`] (OP1a / OP-Atom: AVC, MPEG-2 incl. D-10 / XDCAM, VC-3, ProRes,
//!   PCM) and [`OggSource`] (Ogg Opus with granule-position seeking, Ogg Vorbis): the same
//!   GOP-aware video access and packet-cached audio.
//! - [`MpegSource`]: MPEG-2 transport streams (`.ts`, `.m2ts`, `.mts`), program streams (`.mpg`,
//!   `.vob`, `.mod`) and MPEG-1/2 video elementary streams: MPEG-1/2, H.264 and HEVC video; MPEG
//!   audio, AAC (ADTS / LATM), AC-3 and LPCM.
//! - [`AudioFileSource`]: standalone compressed audio files (MP3, FLAC, AIFF, …).
//! - [`openers`]: the openers to register with the engine's media pool.

#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable))]

pub mod apv;
pub mod audio;
pub mod gop;
pub mod hw;
mod hw_frame;
pub mod mkv;
pub mod mp4;
pub mod mpeg;
pub mod mxf;
pub mod ogg;
pub mod video;

use std::sync::{Arc, RwLock};

pub use apv::ApvSource;
pub use audio::AudioFileSource;
pub use gop::{FRAME_BUDGET, GopStats, cached_bytes, gop_stats, live_decoders};
pub use mkv::MkvSource;
pub use mp4::Mp4Source;
pub use mpeg::MpegSource;
pub use mxf::MxfSource;
pub use ogg::OggSource;
pub use video::{DecodedFrame, VideoDecoder, VideoDecoderFactory};

#[derive(Debug, thiserror::Error)]
pub enum CodecError {
    #[error("unsupported codec: {0}")]
    Unsupported(String),
    #[error("decode error: {0}")]
    Decode(String),
    #[error("container: {0}")]
    Container(String),
    /// The frame is no longer wanted (`filmcraft_media::cancel`).
    #[error("cancelled")]
    Cancelled,
}

pub type Result<T> = std::result::Result<T, CodecError>;

impl From<CodecError> for filmcraft_media::MediaError {
    fn from(e: CodecError) -> Self {
        match e {
            CodecError::Unsupported(s) => filmcraft_media::MediaError::Unsupported(s),
            CodecError::Cancelled => filmcraft_media::MediaError::Cancelled,
            other => filmcraft_media::MediaError::Decode(other.to_string()),
        }
    }
}

/// Our own (pure-Rust) decoders, in the order they are tried.
const BUILTIN_FACTORIES: [VideoDecoderFactory; 9] = [
    video::h264_factory,
    video::hevc_factory,
    video::vp9_factory,
    video::av1_factory,
    video::apv_factory,
    video::prores_factory,
    video::dnx_factory,
    video::mjpeg_factory,
    video::mpeg2_factory,
];

fn factories() -> &'static RwLock<Vec<VideoDecoderFactory>> {
    static F: std::sync::OnceLock<RwLock<Vec<VideoDecoderFactory>>> = std::sync::OnceLock::new();
    F.get_or_init(|| RwLock::new(BUILTIN_FACTORIES.to_vec()))
}

/// Register a video decoder factory (tried before previously registered ones).
pub fn register_video_decoder(f: VideoDecoderFactory) {
    let mut g = factories().write().unwrap_or_else(|e| e.into_inner());
    if !g.iter().any(|x| std::ptr::fn_addr_eq(*x, f)) {
        g.insert(0, f);
    }
}

/// Whether `f` is among the registered video decoder factories (startup diagnostics, tests).
pub fn video_decoder_registered(f: VideoDecoderFactory) -> bool {
    factories().read().unwrap_or_else(|e| e.into_inner()).iter().any(|x| std::ptr::fn_addr_eq(*x, f))
}

/// Create a decoder for a sample entry.
pub fn make_video_decoder(entry: &filmcraft_isobmff::SampleEntry) -> Result<Box<dyn VideoDecoder>> {
    let g = factories().read().unwrap_or_else(|e| e.into_inner());
    for f in g.iter() {
        if let Some(r) = f(entry) {
            return r;
        }
    }
    Err(CodecError::Unsupported(format!("no decoder for {} video", entry.codec.name())))
}

/// Create one of our own (software) decoders for a sample entry, skipping registered factories:
/// what a hardware decoder falls back to when it fails mid-stream.
pub fn software_video_decoder(entry: &filmcraft_isobmff::SampleEntry) -> Result<Box<dyn VideoDecoder>> {
    for f in BUILTIN_FACTORIES {
        if let Some(r) = f(entry) {
            return r;
        }
    }
    Err(CodecError::Unsupported(format!("no decoder for {} video", entry.codec.name())))
}

/// Openers for the engine's media pool (MP4/MOV, Matroska/WebM, MXF, Ogg Opus/Vorbis, MPEG TS/PS and
/// MPEG-1/2 video elementary streams, APV raw bitstreams, standalone audio).
pub fn openers() -> Vec<filmcraft_media::Opener> {
    vec![mp4::opener, mkv::opener, mxf::opener, ogg::opener, mpeg::opener, apv::opener, audio::opener]
}

fn reader_registry() -> &'static RwLock<Vec<filmcraft_media::ReaderOpener>> {
    static R: std::sync::OnceLock<RwLock<Vec<filmcraft_media::ReaderOpener>>> = std::sync::OnceLock::new();
    R.get_or_init(|| RwLock::new(vec![mp4::reader_opener, mkv::reader_opener, mxf::reader_opener, ogg::reader_opener, mpeg::reader_opener, apv::reader_opener]))
}

/// Openers that read containers through a [`filmcraft_media::ByteReader`] (index now, samples on
/// demand) instead of the whole file, in the order they are tried.
pub fn reader_openers() -> Vec<filmcraft_media::ReaderOpener> {
    reader_registry().read().unwrap_or_else(|e| e.into_inner()).clone()
}

/// Register a reader opener tried before the built-in ones (platform media sources such as the
/// web app's WebCodecs-decoded MP4).
pub fn register_reader_opener(f: filmcraft_media::ReaderOpener) {
    let mut g = reader_registry().write().unwrap_or_else(|e| e.into_inner());
    if !g.iter().any(|x| std::ptr::fn_addr_eq(*x, f)) {
        g.insert(0, f);
    }
}

/// A media reader as the demuxers' byte source.
#[derive(Clone)]
pub(crate) struct Src(pub filmcraft_media::SharedReader);

impl filmcraft_isobmff::ByteSource for Src {
    fn len(&self) -> u64 {
        self.0.len()
    }
    fn read_at(&self, offset: u64, buf: &mut [u8]) -> std::io::Result<()> {
        self.0.read_at(offset, buf)
    }
}

impl filmcraft_matroska::ByteSource for Src {
    fn len(&self) -> u64 {
        self.0.len()
    }
    fn read_at(&self, offset: u64, buf: &mut [u8]) -> std::io::Result<()> {
        self.0.read_at(offset, buf)
    }
}

/// How many frame durations the demuxers look at to find a stream's frame rate.
pub(crate) const RATE_SAMPLES: usize = 2000;

/// A video stream's frame rate from its frame durations (`units_per_second` units to a second).
///
/// Muxers that store times in whole milliseconds (OBS, WebM / Matroska, some MP4 writers) give
/// 60 fps frames as 17, 17, 16 ms, so any single duration lies (the median, 17 ms, is 58.82 fps);
/// and a dropped frame makes one duration two frames long. So this takes the mean of the
/// durations within half a frame of the median, and snaps it to the nearest standard rate when
/// the timestamps are too coarse to tell the two apart.
pub(crate) fn rate_from_durations(mut durs: Vec<i64>, units_per_second: f64) -> filmcraft_time::FrameRate {
    use filmcraft_time::FrameRate;
    durs.retain(|d| *d > 0);
    durs.sort_unstable();
    let Some(&median) = durs.get(durs.len() / 2) else { return FrameRate::default() };
    let typical: Vec<i64> = durs.into_iter().filter(|d| d.saturating_mul(2) >= median && d.saturating_mul(2) <= median.saturating_mul(3)).collect();
    let n = typical.len().max(1) as f64;
    let mean = typical.iter().map(|d| *d as f64).sum::<f64>() / n;
    if !units_per_second.is_finite() || units_per_second <= 0.0 || !mean.is_finite() || mean <= 0.0 {
        return FrameRate::default();
    }
    let fps = units_per_second / mean;
    // The stored times step in multiples of `grain` (1 ms = 90 units of a 90 kHz timescale for
    // OBS), so over n frames the mean is off by up to about grain / n.
    let grain = typical.iter().fold(0i64, |g, d| gcd(g, *d)).max(1) as f64;
    // (min then max, not clamp: for a nonsense rate the bounds would cross)
    let tol = (fps * grain / (n * mean)).min(fps * 0.01).max(0.005);
    let nearest = FrameRate::COMMON.iter().copied().min_by(|a, b| (a.as_f64() - fps).abs().total_cmp(&(b.as_f64() - fps).abs()));
    match nearest {
        Some(r) if (r.as_f64() - fps).abs() <= tol => r,
        _ => FrameRate::from_f64(fps).sane(),
    }
}

fn gcd(a: i64, b: i64) -> i64 {
    let (mut a, mut b) = (a.unsigned_abs(), b.unsigned_abs());
    while b != 0 {
        (a, b) = (b, a % b);
    }
    i64::try_from(a).unwrap_or(i64::MAX)
}

/// Convenience: an `Arc` media source from bytes (tries MP4/MOV then audio files).
pub fn open_bytes(name: &str, bytes: Arc<[u8]>) -> std::result::Result<filmcraft_media::SharedSource, filmcraft_media::MediaError> {
    filmcraft_media::open_bytes(name, bytes, &openers())
}

#[cfg(test)]
mod audio_timing_tests;
#[cfg(test)]
mod rounded_pts_tests;
#[cfg(test)]
mod tests;
