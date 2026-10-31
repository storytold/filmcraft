//! Media sources for FilmCraft.
//!
//! A [`MediaSource`] is anything that yields video frames and/or audio for a media time: decoded
//! files (containers + codecs), stills, image sequences and synthetic generators (bars & tone,
//! colour mattes, counting leader, procedural demo scenes). Sources are `Send + Sync`; decoder state
//! lives behind interior mutability so monitors, thumbnails, playback and export can share them.
//!
//! Time passed to a source is **media time** (0 = first frame of the media), in ticks.

#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable))]

pub mod animated;
pub mod cache;
pub mod cancel;
pub mod digits;
pub mod dpx;
pub mod generators;
pub use filmcraft_frame::pending;
pub mod reader;
pub mod sequence;
pub mod still;
pub mod tga;
pub mod wav;

use std::path::Path;
use std::sync::Arc;

use filmcraft_color::ColorInfo;
use filmcraft_frame::{AudioBuffer, VideoFrame};
use filmcraft_time::{FrameRate, Tick};
use serde::{Deserialize, Serialize};

pub use cache::FrameCache;
pub use generators::{DemoScene, Generator};
pub use reader::{ByteReader, ReaderOpener, SharedReader};

#[derive(Debug, thiserror::Error)]
pub enum MediaError {
    #[error("unsupported media: {0}")]
    Unsupported(String),
    #[error("I/O error: {0}")]
    Io(String),
    #[error("decode error: {0}")]
    Decode(String),
    /// Decoded pixels unavailable; not evidence of damaged media.
    #[error("frame access error: {0}")]
    FrameAccess(String),
    #[error("no {0} stream")]
    NoStream(&'static str),
    #[error("media offline: {0}")]
    Offline(String),
    /// The request was cancelled (see [`cancel`]).
    #[error("cancelled")]
    Cancelled,
}

pub type Result<T> = std::result::Result<T, MediaError>;

/// Up to 384 kHz audio (surpassing professional 192 kHz formats).
pub const MAX_AUDIO_SAMPLE_RATE: u32 = 384_000;
/// Up to 64 discrete channels (exceeding standard 32-channel immersive layouts).
pub const MAX_AUDIO_CHANNELS: u32 = 64;
/// Bounded planar audio buffer: 64 channels * 192 kHz * 10 seconds of f32 is ~492 MiB.
pub const MAX_AUDIO_BUFFER_BYTES: usize = 512 * 1024 * 1024;

/// Bound container-advertised audio stream dimensions before they are trusted by the app or
/// allocator.
///
/// Out-of-range values are rejected rather than clamped: publishing a lower sample rate without
/// resampling the samples would make the stream play at the wrong speed, and a clamped channel
/// count would misdescribe the interleaving.
pub fn bounded_audio_dimensions(sample_rate: u32, channels: u32) -> Result<(u32, u32)> {
    if sample_rate == 0 || sample_rate > MAX_AUDIO_SAMPLE_RATE {
        return Err(MediaError::Decode(format!("unsupported audio sample rate {sample_rate}")));
    }
    if channels == 0 || channels > MAX_AUDIO_CHANNELS {
        return Err(MediaError::Decode(format!("unsupported audio channel count {channels}")));
    }
    Ok((sample_rate, channels))
}

/// Check an adapter-owned planar `f32` audio allocation before constructing it.
pub fn validate_audio_buffer_shape(channels: usize, frames: usize) -> Result<()> {
    if channels == 0 || channels > MAX_AUDIO_CHANNELS as usize {
        return Err(MediaError::Decode(format!("unsupported audio channel count {channels}")));
    }
    let bytes = channels
        .checked_mul(frames)
        .and_then(|samples| samples.checked_mul(std::mem::size_of::<f32>()))
        .ok_or_else(|| MediaError::Decode("audio buffer dimensions overflow".into()))?;
    if bytes > MAX_AUDIO_BUFFER_BYTES {
        return Err(MediaError::Decode(format!("audio buffer of {bytes} bytes exceeds the {} byte limit", MAX_AUDIO_BUFFER_BYTES)));
    }
    Ok(())
}

/// Number of source frames needed to resample `frames` output frames. Arithmetic is checked so
/// corrupted rates cannot wrap into a small allocation.
pub fn resample_source_frames(frames: usize, source_rate: u32, output_rate: u32) -> Result<usize> {
    if output_rate == 0 {
        return Err(MediaError::Decode("zero requested audio sample rate".into()));
    }
    let source = usize::try_from(source_rate).map_err(|_| MediaError::Decode("invalid source sample rate".into()))?;
    let output = usize::try_from(output_rate).map_err(|_| MediaError::Decode("invalid output sample rate".into()))?;
    frames
        .checked_mul(source)
        .and_then(|n| n.checked_add(output.saturating_sub(1)))
        .map(|n| n / output)
        .ok_or_else(|| MediaError::Decode("resampled audio window overflows".into()))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum MediaKind {
    /// Video, possibly with audio.
    Movie,
    AudioOnly,
    Still,
    ImageSequence,
    Synthetic,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct VideoStreamInfo {
    pub width: u32,
    pub height: u32,
    pub frame_rate: FrameRate,
    pub par: (u32, u32),
    pub codec: String,
    pub pixel_format: String,
    pub color: ColorInfo,
    pub has_alpha: bool,
    /// Bitrate in bits/s when known.
    pub bitrate: Option<u64>,
    /// HDR static metadata (mastering display, content light level) when the file carries it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hdr: Option<filmcraft_color::HdrMetadata>,
}

/// Most audio streams a source reports (the count comes from the file: never trusted).
pub const MAX_AUDIO_STREAMS: usize = 64;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AudioStreamInfo {
    pub sample_rate: u32,
    pub channels: u32,
    pub codec: String,
    pub bits_per_sample: Option<u32>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MediaInfo {
    pub name: String,
    pub kind: MediaKind,
    /// Media duration. Stills report a default duration (the still-image default preference).
    pub duration: Tick,
    pub video: Option<VideoStreamInfo>,
    /// Every audio stream the container carries, in file order. Stream 0 is the one
    /// [`MediaSource::audio`] reads; the others are read with [`MediaSource::audio_stream`].
    /// Project files written before multi-stream support hold a single `audio` object (or
    /// `null`): both still load.
    #[serde(default, alias = "audio", deserialize_with = "deserialize_audio_streams")]
    pub audio_streams: Vec<AudioStreamInfo>,
    pub container: String,
    /// Timecode of the first frame (in frames at `video.frame_rate`), if the file carries one.
    pub start_timecode: Option<i64>,
    pub file_size: Option<u64>,
}

/// Reads `audio_streams` from a list, or from the single optional stream older project files wrote.
fn deserialize_audio_streams<'de, D: serde::Deserializer<'de>>(d: D) -> std::result::Result<Vec<AudioStreamInfo>, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Streams {
        Many(Vec<AudioStreamInfo>),
        One(Option<AudioStreamInfo>),
    }
    Ok(match Streams::deserialize(d)? {
        Streams::Many(v) => v,
        Streams::One(o) => o.into_iter().collect(),
    })
}

impl MediaInfo {
    pub fn frame_rate(&self) -> FrameRate {
        self.video.as_ref().map(|v| v.frame_rate).unwrap_or_default()
    }
    pub fn has_video(&self) -> bool {
        self.video.is_some()
    }
    pub fn has_audio(&self) -> bool {
        !self.audio_streams.is_empty()
    }
    /// The primary audio stream (stream 0), the one [`MediaSource::audio`] reads.
    pub fn audio(&self) -> Option<&AudioStreamInfo> {
        self.audio_streams.first()
    }
}

/// A request for a video frame.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FrameRequest {
    /// Media time.
    pub time: Tick,
    /// Desired scale relative to full resolution (1.0 = full). Sources may return a larger frame.
    pub scale: f32,
}

impl FrameRequest {
    pub fn full(time: Tick) -> Self {
        Self { time, scale: 1.0 }
    }
}

pub trait MediaSource: Send + Sync {
    fn info(&self) -> &MediaInfo;
    /// The frame displayed at media time `req.time`.
    fn video_frame(&self, req: FrameRequest) -> Result<Arc<VideoFrame>>;
    /// `frames` audio frames starting at sample index `start` (at `sample_rate`, media time).
    fn audio(&self, start: i64, frames: usize, sample_rate: u32) -> Result<AudioBuffer>;
    /// Like [`audio`](Self::audio) for audio stream `stream` (an index into
    /// [`MediaInfo::audio_streams`]). Sources with a single audio stream need not override this:
    /// stream 0 is `audio`, any other index is [`MediaError::NoStream`].
    fn audio_stream(&self, stream: usize, start: i64, frames: usize, sample_rate: u32) -> Result<AudioBuffer> {
        if stream == 0 { self.audio(start, frames, sample_rate) } else { Err(MediaError::NoStream("audio")) }
    }
}

pub type SharedSource = Arc<dyn MediaSource>;

/// File extensions we recognise at import (the media browser filters on these).
pub const STILL_EXTENSIONS: &[&str] = &["png", "jpg", "jpeg", "gif", "webp", "tif", "tiff", "bmp", "tga", "dpx", "exr", "hdr", "dng"];
pub const AUDIO_EXTENSIONS: &[&str] = &["wav", "wave", "bwf", "aif", "aiff", "mp3", "mp2", "m4a", "aac", "flac", "ogg", "opus"];
pub const VIDEO_EXTENSIONS: &[&str] = &[
    "mp4", "m4v", "mov", "mkv", "webm", "avi", "mxf", "mts", "m2ts", "m2t", "ts", "mpg", "mpeg", "vob", "mod", "tod", "m2v", "m1v", "mpv", "3gp", "y4m", "apv",
];

pub fn is_importable(path: &Path) -> bool {
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();
    STILL_EXTENSIONS.contains(&ext.as_str()) || AUDIO_EXTENSIONS.contains(&ext.as_str()) || VIDEO_EXTENSIONS.contains(&ext.as_str())
}

/// A pluggable opener (containers/codecs register themselves here from higher-level crates).
pub type Opener = fn(name: &str, bytes: Arc<[u8]>) -> Option<Result<SharedSource>>;

/// Open media from bytes, trying `extra` openers first, then the built-in ones (WAV, animated
/// images, stills).
pub fn open_bytes(name: &str, bytes: Arc<[u8]>, extra: &[Opener]) -> Result<SharedSource> {
    for o in extra {
        if let Some(r) = o(name, bytes.clone()) {
            return r;
        }
    }
    if still::exr::sniff(&bytes) {
        return Ok(Arc::new(still::exr::still(name, &bytes)?));
    }
    if still::hdr::sniff(&bytes) {
        return Ok(Arc::new(still::hdr::still(name, &bytes)?));
    }
    if wav::sniff(&bytes) {
        return Ok(Arc::new(wav::WavSource::parse(name, bytes)?));
    }
    // APNG, animated GIF, and animated WebP play as movies (#402); single-frame files fall through to the stills
    if let Some(r) = animated::open(name, &bytes) {
        return Ok(Arc::new(r?));
    }
    if still::sniff(name, &bytes) {
        return Ok(Arc::new(still::StillSource::decode(name, &bytes)?));
    }
    Err(MediaError::Unsupported(format!("{name}: unrecognised format")))
}

/// Open a file from disk (native only).
#[cfg(not(target_arch = "wasm32"))]
pub fn open_path(path: &Path, extra: &[Opener]) -> Result<SharedSource> {
    let bytes = std::fs::read(path).map_err(|e| MediaError::Io(format!("{}: {e}", path.display())))?;
    let name = path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
    open_bytes(&name, bytes.into(), extra)
}

/// A source that renders nothing (used for offline media): the "Media Offline" slate.
pub struct OfflineSource {
    pub info: MediaInfo,
}

impl MediaSource for OfflineSource {
    fn info(&self) -> &MediaInfo {
        &self.info
    }
    fn video_frame(&self, req: FrameRequest) -> Result<Arc<VideoFrame>> {
        let v = self.info.video.as_ref().ok_or(MediaError::NoStream("video"))?;
        let s = req.scale.clamp(0.02, 1.0);
        let (w, h) = (((v.width as f32 * s) as u32).max(2), ((v.height as f32 * s) as u32).max(2));
        let mut px = vec![0u8; (w * h * 4) as usize];
        for p in px.as_chunks_mut::<4>().0 {
            p.copy_from_slice(&[140, 16, 16, 255]);
        }
        Ok(Arc::new(VideoFrame::rgba8(w, h, px).with_pts(req.time)))
    }
    fn audio(&self, _start: i64, frames: usize, sample_rate: u32) -> Result<AudioBuffer> {
        Ok(AudioBuffer::silence(sample_rate, self.info.audio().map_or(2, |a| a.channels as usize), frames))
    }
}

#[cfg(test)]
mod importable_tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn camera_extensions_match_regardless_of_case() {
        for name in ["clip.mov", "clip.MOV", "clip.Mov", "shot.MP4", "IMG_0001.JPG", "sound.WAV", "take.M2TS"] {
            assert!(is_importable(Path::new(name)), "{name}");
        }
        assert!(!is_importable(Path::new("notes.txt")));
    }

    #[test]
    fn hostile_audio_dimensions_are_bounded_before_adapters_allocate() {
        assert!(bounded_audio_dimensions(0, 0).is_err());
        assert!(bounded_audio_dimensions(u32::MAX, u32::MAX).is_err());
        assert!(bounded_audio_dimensions(MAX_AUDIO_SAMPLE_RATE + 1, 2).is_err());
        assert_eq!(bounded_audio_dimensions(48_000, 2).ok(), Some((48_000, 2)));
        assert!(validate_audio_buffer_shape(MAX_AUDIO_CHANNELS as usize, MAX_AUDIO_BUFFER_BYTES / 4 / MAX_AUDIO_CHANNELS as usize).is_ok());
        assert!(validate_audio_buffer_shape(MAX_AUDIO_CHANNELS as usize, MAX_AUDIO_BUFFER_BYTES / 4 / MAX_AUDIO_CHANNELS as usize + 1).is_err());
        assert!(resample_source_frames(usize::MAX, MAX_AUDIO_SAMPLE_RATE, 1).is_err());
    }
}
