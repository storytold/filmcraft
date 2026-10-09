//! Media sources for FilmCraft.
//!
//! A [`MediaSource`] is anything that yields video frames and/or audio for a media time: decoded
//! files (containers + codecs), stills, image sequences and synthetic generators (bars & tone,
//! colour mattes, counting leader, procedural demo scenes). Sources are `Send + Sync`; decoder state
//! lives behind interior mutability so monitors, thumbnails, playback and export can share them.
//!
//! Time passed to a source is **media time** (0 = first frame of the media), in ticks.

#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable))]

pub mod cache;
pub mod cancel;
pub mod digits;
pub mod generators;
pub mod pending;
pub mod reader;
pub mod sequence;
pub mod still;
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
    #[error("no {0} stream")]
    NoStream(&'static str),
    #[error("media offline: {0}")]
    Offline(String),
    /// The request was cancelled (see [`cancel`]).
    #[error("cancelled")]
    Cancelled,
}

pub type Result<T> = std::result::Result<T, MediaError>;

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
    /// The first audio stream's format. With several audio streams, `channels` is their total and
    /// the streams are listed by [`MediaSource::audio_streams`].
    pub audio: Option<AudioStreamInfo>,
    pub container: String,
    /// Timecode of the first frame (in frames at `video.frame_rate`), if the file carries one.
    pub start_timecode: Option<i64>,
    pub file_size: Option<u64>,
}

impl MediaInfo {
    pub fn frame_rate(&self) -> FrameRate {
        self.video.as_ref().map(|v| v.frame_rate).unwrap_or_default()
    }
    pub fn has_video(&self) -> bool {
        self.video.is_some()
    }
    pub fn has_audio(&self) -> bool {
        self.audio.is_some()
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
    /// Every audio stream of the source, in channel order: [`audio`](Self::audio) returns the
    /// channels of the first stream, then the second's, and so on. A file with several audio tracks
    /// (game and microphone, say) lists them all; one with a single stream lists just `info().audio`.
    fn audio_streams(&self) -> Vec<AudioStreamInfo> {
        self.info().audio.clone().into_iter().collect()
    }
}

pub type SharedSource = Arc<dyn MediaSource>;

/// File extensions we recognise at import (the media browser filters on these).
pub const STILL_EXTENSIONS: &[&str] = &["png", "jpg", "jpeg", "gif", "webp", "tif", "tiff", "bmp"];
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

/// Open media from bytes, trying `extra` openers first, then the built-in ones (stills, WAV).
pub fn open_bytes(name: &str, bytes: Arc<[u8]>, extra: &[Opener]) -> Result<SharedSource> {
    for o in extra {
        if let Some(r) = o(name, bytes.clone()) {
            return r;
        }
    }
    if wav::sniff(&bytes) {
        return Ok(Arc::new(wav::WavSource::parse(name, bytes)?));
    }
    if still::sniff(&bytes) {
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
        Ok(AudioBuffer::silence(sample_rate, self.info.audio.as_ref().map_or(2, |a| a.channels as usize), frames))
    }
}
