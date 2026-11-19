//! Radiance HDR (`.hdr`, `.pic`) stills, read from Greg Ward's public RGBE format description
//! ("Real Pixels", Graphics Gems II): a text header (`#?RADIANCE` or `#?RGBE`, `FORMAT=32-bit_rle_rgbe`),
//! a blank line, a resolution line (`-Y h +X w` and the seven other axis orders), then one RGBE
//! pixel (shared exponent) per sample, flat, in the old run-length form or in the "new" per-channel
//! run-length form (scan lines of 8 to 32767 pixels). RGBE is scene-linear light, so the frame keeps
//! 32-bit floats (values above 1.0 survive into effects and colour work). The `EXPOSURE` and
//! `PRIMARIES` header lines are not applied, which is what other readers do too; the XYZE colour
//! format is refused.

use std::sync::Arc;

use filmcraft_color::{ColorInfo, Transfer};
use filmcraft_frame::VideoFrame;
use filmcraft_time::FrameRate;

use super::{DEFAULT_STILL_DURATION, StillSource};
use crate::{MediaError, MediaInfo, MediaKind, Result, VideoStreamInfo};

/// Largest picture we decode (each pixel takes 16 bytes as RGBA floats).
const MAX_PIXELS: u64 = 1 << 26;
/// Widest or tallest picture we decode: validation expands one scan line into a scratch buffer
/// of that many pixels before the whole picture is checked.
const MAX_AXIS: usize = 1 << 15;
/// Longest header we read (real ones are a few hundred bytes).
const MAX_HEADER: usize = 64 << 10;

/// The colour of HDR frames: scene-linear light, Rec.709 primaries.
const LINEAR_709: ColorInfo = ColorInfo { transfer: Transfer::Linear, ..ColorInfo::SRGB_FULL };

/// Whether `b` starts like a Radiance picture.
pub fn sniff(b: &[u8]) -> bool {
    craft_hdr::sniff(b)
}

fn err(name: &str, what: &str) -> MediaError {
    MediaError::Decode(format!("{name}: Radiance HDR: {what}"))
}

/// Open a Radiance picture as a still.
pub fn still(name: &str, bytes: &[u8]) -> Result<StillSource> {
    let frame = decode(name, bytes)?;
    let info = MediaInfo {
        name: name.to_string(),
        kind: MediaKind::Still,
        duration: DEFAULT_STILL_DURATION,
        video: Some(VideoStreamInfo {
            width: frame.width,
            height: frame.height,
            frame_rate: FrameRate::FPS_29_97,
            par: (1, 1),
            codec: "HDR".into(),
            pixel_format: "RGB 32-bit float (RGBE)".into(),
            color: LINEAR_709,
            has_alpha: false,
            bitrate: None,
            hdr: None,
            color_assumed: None,
        }),
        audio_streams: Vec::new(),
        container: "Image".into(),
        start_timecode: None,
        file_size: Some(bytes.len() as u64),
    };
    Ok(StillSource { info, frame: Arc::new(frame) })
}

/// Decode through the shared codec, retaining FilmCraft's image and scratch limits.
pub fn decode(name: &str, b: &[u8]) -> Result<VideoFrame> {
    let mut limits = craft_hdr::Limits::default();
    limits.max_pixels = MAX_PIXELS;
    limits.max_axis = MAX_AXIS as u32;
    limits.max_header = MAX_HEADER;
    let picture = craft_hdr::decode_rgba(b, &limits).map_err(|error| match error {
        craft_hdr::Error::LimitExceeded("axis length" | "pixel count") => err(name, "picture size out of range"),
        other => err(name, &other.to_string()),
    })?;
    let mut frame = VideoFrame::rgba_f32(picture.width, picture.height, picture.rgba);
    frame.color = LINEAR_709;
    Ok(frame)
}

#[cfg(test)]
use craft_hdr::rgbe_to_linear as rgbe;

#[cfg(test)]
mod tests;
