//! Animated images (APNG, animated GIF, and animated WebP) as movie clips (#402).
//!
//! The still decoders read only the first picture of these files. [`open`] decodes every frame
//! instead, composited onto the full canvas by the `image` crate (APNG blend and dispose operations,
//! GIF and WebP disposal methods), and keeps them with their timing:
//!
//! - each frame lasts its own delay; GIF delays of 10 ms or less play as 100 ms and APNG and WebP
//!   delays under 10 ms as 10 ms, as browsers do, so a zero delay never makes a zero-length frame;
//! - consecutive identical frames are merged (one picture, the sum of their delays);
//! - the clip is one pass through the animation (the file's loop count is not applied);
//! - transparency is kept: the source reports alpha when any frame has a pixel that is not fully
//!   opaque.
//!
//! The file is hostile: decoding stops at [`MAX_FRAMES`] frames or [`MAX_DECODED_BYTES`] of
//! pictures (the clip then ends there), and a frame that fails to decode ends the animation at the
//! frames decoded before it. A file that turns out not to be animated (one frame, or no frame that
//! decodes) is left to the still decoders, which is what happened to every animated image before.

use std::io::Cursor;
use std::sync::Arc;

use filmcraft_frame::{AudioBuffer, VideoFrame};
use filmcraft_time::{FrameRate, TICKS_PER_SECOND, Tick};
use image::codecs::gif::GifDecoder;
use image::codecs::png::PngDecoder;
use image::{AnimationDecoder, Delay, Frames, ImageDecoder, Limits};

use crate::{FrameRequest, MediaError, MediaInfo, MediaKind, MediaSource, Result, VideoStreamInfo};

/// Most frames kept from one file (frames past this are dropped; the clip ends there).
pub const MAX_FRAMES: usize = 10_000;
/// Most bytes of decoded RGBA pictures kept from one file (after merging identical frames).
pub const MAX_DECODED_BYTES: usize = 1 << 30;
/// Most RGBA bytes in one animation canvas while its decoder produces a frame.
const MAX_DECODED_FRAME_BYTES: usize = 512 << 20;

/// Animated image formats.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Format {
    Apng,
    Gif,
    WebP,
}

impl Format {
    /// Identify an animated image format from its container signature.
    fn sniff(b: &[u8]) -> Option<Format> {
        if b.starts_with(b"\x89PNG\r\n\x1a\n") {
            Some(Format::Apng)
        } else if b.starts_with(b"GIF87a") || b.starts_with(b"GIF89a") {
            Some(Format::Gif)
        } else if b.get(0..4) == Some(&b"RIFF"[..]) && b.get(8..12) == Some(&b"WEBP"[..]) {
            Some(Format::WebP)
        } else {
            None
        }
    }

    /// Return the codec label shown in media metadata.
    fn codec(self) -> &'static str {
        match self {
            Format::Apng => "APNG",
            Format::Gif => "GIF",
            Format::WebP => "WebP",
        }
    }

    /// A frame's delay in ticks, with browsers' minimum for zero and near-zero delays.
    fn frame_ticks(self, delay: Delay) -> Tick {
        let (numer, denom) = delay.numer_denom_ms();
        let (numer, denom) = (u128::from(numer), u128::from(denom.max(1)));
        // whole milliseconds, rounded down, decide the minimum (GIF delays are 10 ms steps)
        let ms = numer / denom;
        let (numer, denom) = match self {
            Format::Gif if ms <= 10 => (100, 1),
            Format::Apng if ms < 10 => (10, 1),
            Format::WebP if ms < 10 => (10, 1),
            _ => (numer, denom),
        };
        let ticks = numer.saturating_mul(TICKS_PER_SECOND as u128) / denom.saturating_mul(1000);
        Tick(i64::try_from(ticks).unwrap_or(i64::MAX).max(1))
    }
}

/// An animation: its frames (full-canvas pictures) and when each one starts.
pub struct AnimatedSource {
    info: MediaInfo,
    frames: Vec<Arc<VideoFrame>>,
    /// Media time each frame starts at; ascending, `starts[0]` is zero.
    starts: Vec<Tick>,
}

/// Decode `bytes` as an animated image. `None` when it is not an APNG, GIF, or animated WebP with
/// at least two frames that decode (the caller then opens it as a still).
pub fn open(name: &str, bytes: &[u8]) -> Option<Result<AnimatedSource>> {
    let format = Format::sniff(bytes)?;
    let frames = match format {
        Format::Apng => {
            let mut d = PngDecoder::new(Cursor::new(bytes)).ok()?;
            if !d.is_apng().ok()? {
                return None;
            }
            d.set_limits(Limits::default()).ok()?;
            d.apng().ok()?.into_frames()
        }
        Format::Gif => {
            let mut d = GifDecoder::new(Cursor::new(bytes)).ok()?;
            d.set_limits(Limits::default()).ok()?;
            d.into_frames()
        }
        Format::WebP => {
            let mut d = image::codecs::webp::WebPDecoder::new(Cursor::new(bytes)).ok()?;
            if !d.has_animation() {
                return None;
            }
            let (width, height) = d.dimensions();
            if rgba_bytes(width, height).is_none_or(|bytes| bytes > MAX_DECODED_FRAME_BYTES) {
                return Some(Err(MediaError::Unsupported(format!("{name}: animated WebP canvas exceeds the {MAX_DECODED_FRAME_BYTES}-byte frame limit"))));
            }
            d.set_limits(Limits::default()).ok()?;
            d.into_frames()
        }
    };
    AnimatedSource::from_frames(name, format, frames, bytes.len() as u64).map(Ok)
}

impl AnimatedSource {
    /// Build a clip from decoded full-canvas frames, merging consecutive identical pictures.
    fn from_frames(name: &str, format: Format, frames: Frames<'_>, file_size: u64) -> Option<Self> {
        let mut pictures: Vec<(u32, u32, Vec<u8>)> = Vec::new();
        let mut durations: Vec<Tick> = Vec::new();
        let mut decoded_frames = 0usize;
        let mut decoded = 0usize;
        let mut has_alpha = false;
        for frame in frames.take(MAX_FRAMES) {
            // a damaged frame ends the animation at the frames before it
            let Ok(frame) = frame else { break };
            decoded_frames = decoded_frames.saturating_add(1);
            let ticks = format.frame_ticks(frame.delay());
            let buffer = frame.into_buffer();
            let (w, h) = buffer.dimensions();
            let rgba = buffer.into_raw();
            if let (Some(last), Some(d)) = (pictures.last(), durations.last_mut())
                && (last.0, last.1) == (w, h)
                && last.2 == rgba
            {
                *d = Tick(d.0.saturating_add(ticks.0));
                continue;
            }
            decoded = decoded.saturating_add(rgba.len());
            if decoded > MAX_DECODED_BYTES {
                break;
            }
            has_alpha = has_alpha || rgba.as_chunks::<4>().0.iter().any(|p| p[3] < 255);
            pictures.push((w, h, rgba));
            durations.push(ticks);
        }
        if pictures.len() < 2 && !(format == Format::WebP && decoded_frames >= 2) {
            return None;
        }
        let (width, height) = pictures.first().map(|p| (p.0, p.1))?;
        let mut starts = Vec::with_capacity(durations.len());
        let mut t = Tick::ZERO;
        for d in &durations {
            starts.push(t);
            t = Tick(t.0.saturating_add(d.0));
        }
        let info = MediaInfo {
            name: name.to_string(),
            kind: MediaKind::Movie,
            duration: t,
            video: Some(VideoStreamInfo {
                width,
                height,
                frame_rate: rate_of(&durations),
                par: (1, 1),
                codec: format.codec().into(),
                pixel_format: if has_alpha { "RGBA 8-bit".into() } else { "RGB 8-bit".into() },
                color: filmcraft_color::ColorInfo::SRGB_FULL,
                has_alpha,
                bitrate: None,
                hdr: None,
            }),
            audio_streams: Vec::new(),
            container: format.codec().into(),
            start_timecode: None,
            file_size: Some(file_size),
        };
        let frames = pictures.into_iter().map(|(w, h, rgba)| Arc::new(VideoFrame::rgba8(w, h, rgba))).collect();
        Some(Self { info, frames, starts })
    }

    /// Number of distinct frames kept.
    pub fn frame_count(&self) -> usize {
        self.frames.len()
    }

    /// Media time frame `i` starts at.
    pub fn frame_start(&self, i: usize) -> Option<Tick> {
        self.starts.get(i).copied()
    }

    /// The frame shown at media time `t` (the first before the start, the last after the end).
    fn index_at(&self, t: Tick) -> usize {
        self.starts.partition_point(|s| *s <= t).saturating_sub(1)
    }
}

/// Checked byte size of an RGBA8 image.
fn rgba_bytes(width: u32, height: u32) -> Option<usize> {
    usize::try_from(width).ok()?.checked_mul(usize::try_from(height).ok()?)?.checked_mul(4)
}

/// The clip's frame rate: the rate of its most common frame duration, when that is a sensible video
/// rate (1 to 120 frames per second), else 30 fps. Frames are shown for their own durations
/// whatever this says; it sets the clip's timebase (e.g. a sequence made from the clip).
fn rate_of(durations: &[Tick]) -> FrameRate {
    let mut sorted: Vec<i64> = durations.iter().map(|d| d.0).collect();
    sorted.sort_unstable();
    let mut best = (0usize, 0i64);
    let mut i = 0;
    while let Some(&v) = sorted.get(i) {
        let run = sorted.get(i..).map_or(0, |rest| rest.iter().take_while(|&&x| x == v).count()).max(1);
        if run > best.0 {
            best = (run, v);
        }
        i = i.saturating_add(run);
    }
    let ticks = best.1;
    if ticks <= 0 {
        return FrameRate::FPS_30;
    }
    let rate = FrameRate::new(TICKS_PER_SECOND, ticks);
    let fps = rate.as_f64();
    if (1.0..=120.0).contains(&fps) { rate } else { FrameRate::FPS_30 }
}

impl MediaSource for AnimatedSource {
    /// Return the clip's media metadata.
    fn info(&self) -> &MediaInfo {
        &self.info
    }

    /// Return the picture displayed at the requested media time.
    fn video_frame(&self, req: FrameRequest) -> Result<Arc<VideoFrame>> {
        self.frames.get(self.index_at(req.time)).cloned().ok_or(MediaError::NoStream("video"))
    }

    /// Animated image files do not provide audio.
    fn audio(&self, _start: i64, _frames: usize, _sample_rate: u32) -> Result<AudioBuffer> {
        Err(MediaError::NoStream("audio"))
    }
}

#[cfg(test)]
mod tests;
