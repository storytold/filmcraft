//! Still images (PNG, JPEG, GIF, WebP, TIFF, BMP, Targa, DPX, OpenEXR, Radiance HDR, DNG camera raw, PSD) as media
//! sources. Animated PNG and GIF files open as movies instead ([`crate::animated`]); here they would
//! show their first frame.
//!
//! JPEG, PNG, WebP and TIFF stills (including the uncompressed / PackBits YCbCr TIFFs read by our
//! own decoder) are turned upright by their EXIF Orientation (a phone photo stored sideways
//! decodes with its size swapped), so everything downstream sees the picture as the camera meant
//! it. DNG and the other formats ignore it.

use std::sync::Arc;

use filmcraft_frame::VideoFrame;
use filmcraft_time::{FrameRate, TICKS_PER_SECOND, Tick};
use image::ImageDecoder;
use image::metadata::Orientation;

use crate::{FrameRequest, MediaError, MediaInfo, MediaKind, MediaSource, Result, VideoStreamInfo};

pub mod dng;
pub mod exr;
pub mod hdr;
pub mod psd;
mod ycbcr_tiff;

/// Default still duration (Premiere's preference default is 5 seconds).
pub const DEFAULT_STILL_DURATION: Tick = Tick(5 * TICKS_PER_SECOND);

/// Whether `b` (the contents of the file `name`) is a still image we decode.
pub fn sniff(name: &str, b: &[u8]) -> bool {
    crate::dpx::sniff(b)
        || hdr::sniff(b)
        || psd::sniff(b)
        || crate::tga::sniff(name, b)
        || b.starts_with(b"\x89PNG")
        || b.starts_with(&[0xFF, 0xD8, 0xFF])
        || b.starts_with(b"GIF8")
        || (b.len() > 12 && &b[0..4] == b"RIFF" && &b[8..12] == b"WEBP")
        || b.starts_with(b"II*\0")
        || b.starts_with(b"MM\0*")
        || b.starts_with(b"BM")
}

/// A decoded picture: straight-alpha RGBA8, top row first.
pub struct Raster {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
    /// Whether any pixel is not fully opaque.
    pub has_alpha: bool,
    /// The EXIF Orientation the decoder applied to turn the stored picture upright
    /// (`NoTransforms` when the format ignores it or none was present): the mapping
    /// from stored pixel coordinates to these pixels.
    pub orientation: Orientation,
}

/// Decode a still image file (`name` picks Targa, which has no magic number; the content picks
/// every other format).
pub fn decode_raster(name: &str, bytes: &[u8]) -> Result<Raster> {
    if crate::dpx::sniff(bytes) {
        return crate::dpx::decode(name, bytes);
    }
    if crate::tga::sniff(name, bytes) {
        return crate::tga::decode(name, bytes);
    }
    if psd::sniff(bytes) {
        return psd::decode(name, bytes);
    }
    if ycbcr_tiff::sniff(bytes) {
        return ycbcr_tiff::decode(name, bytes);
    }
    check_pixel_count(name, bytes)?;
    let orientation = exif_orientation(bytes);
    let mut img = image::load_from_memory(bytes).map_err(|e| MediaError::Decode(format!("{name}: {e}")))?;
    let has_alpha = img.color().has_alpha();
    img.apply_orientation(orientation);
    let rgba = img.into_rgba8();
    let (width, height) = rgba.dimensions();
    Ok(Raster { width, height, rgba: rgba.into_raw(), has_alpha, orientation })
}

/// Most pixels a decoded still may have: 1 GiB of RGBA, as the other raster decoders allow. The
/// image crate's own allocation limit covers the native decode only, not the RGBA copy made after it.
const MAX_PIXELS: u64 = 1 << 28;

/// Refuse a picture whose header (read without decoding pixels) declares more than [`MAX_PIXELS`].
fn check_pixel_count(name: &str, bytes: &[u8]) -> Result<()> {
    let dims = image::ImageReader::new(std::io::Cursor::new(bytes)).with_guessed_format().ok().and_then(|r| r.into_decoder().ok()).map(|d| d.dimensions());
    match dims {
        Some((w, h)) if u64::from(w) * u64::from(h) > MAX_PIXELS => Err(MediaError::Unsupported(format!("{name}: picture of {w} x {h} pixels is too large"))),
        _ => Ok(()),
    }
}

/// The EXIF Orientation of a JPEG, PNG, WebP or TIFF, from its headers alone (no pixels are
/// decoded; the probe's copy of the file is gone before the real decode starts). A missing,
/// unknown or unreadable value means no transform: a damaged tag never fails an import.
fn exif_orientation(bytes: &[u8]) -> Orientation {
    let probe = || image::ImageReader::new(std::io::Cursor::new(bytes)).with_guessed_format().ok()?.into_decoder().ok()?.orientation().ok();
    probe().unwrap_or(Orientation::NoTransforms)
}

/// Whether `orientation` turns the picture onto its side (the four transposing EXIF
/// values), so the size it reports is the stored size swapped.
fn transposes(orientation: Orientation) -> bool {
    matches!(orientation, Orientation::Rotate90 | Orientation::Rotate90FlipH | Orientation::Rotate270 | Orientation::Rotate270FlipH)
}

/// The size [`StillSource::decode`] would report for the still `name` in `bytes`, and
/// the orientation its decoder would apply, read from the file's headers alone: no
/// picture is decoded and nothing is retained, so a caller that only needs to compare
/// geometry (the engine's load-time still migration) never pays for a decode. The
/// dispatch mirrors the open path, format by format, and each format's answer comes
/// from the same header its decoder reads. `None` when the file is not a still we
/// open or its geometry cannot be read without decoding: Radiance HDR, whose
/// resolution line only its decoder reads. An HDR picture is never turned by its
/// decoder, so its stored size is the size an import recorded; skipping it here can
/// only miss a file that changed size on disk after it was imported.
pub fn probe_geometry(name: &str, bytes: &[u8]) -> Option<(u32, u32, Orientation)> {
    if exr::sniff(bytes) {
        return exr::geometry(name, bytes);
    }
    if hdr::sniff(bytes) {
        return None;
    }
    if dng::sniff(bytes) {
        return dng::geometry(bytes);
    }
    if crate::dpx::sniff(bytes) {
        return crate::dpx::geometry(bytes);
    }
    if crate::tga::sniff(name, bytes) {
        return crate::tga::geometry(bytes);
    }
    if psd::sniff(bytes) {
        return psd::geometry(bytes);
    }
    if ycbcr_tiff::sniff(bytes) {
        return ycbcr_tiff::geometry(bytes);
    }
    if !sniff(name, bytes) {
        return None;
    }
    // The remaining formats decode through the image crate: dimensions and EXIF
    // Orientation from its headers, under the same pixel ceiling the decode enforces.
    if check_pixel_count(name, bytes).is_err() {
        return None;
    }
    let (w, h) = image::ImageReader::new(std::io::Cursor::new(bytes)).with_guessed_format().ok()?.into_dimensions().ok()?;
    let orientation = exif_orientation(bytes);
    let (w, h) = if transposes(orientation) { (h, w) } else { (w, h) };
    Some((w, h, orientation))
}

/// Bounded `Read + Seek` adapter over a [`crate::ByteReader`], capping cumulative bytes
/// read so a damaged header cannot force an unbounded read.
struct BoundedReader<'a> {
    inner: &'a dyn crate::ByteReader,
    pos: u64,
    remaining: u64,
}

impl<'a> BoundedReader<'a> {
    fn new(inner: &'a dyn crate::ByteReader, budget: u64) -> Self {
        Self { inner, pos: 0, remaining: budget }
    }
}

impl std::io::Read for BoundedReader<'_> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let avail = self.inner.len().saturating_sub(self.pos);
        let want = (buf.len() as u64).min(avail).min(self.remaining) as usize;
        if want == 0 {
            return if avail == 0 { Ok(0) } else { Err(std::io::Error::new(std::io::ErrorKind::InvalidData, "still header probe exceeded read budget")) };
        }
        self.inner.read_at(self.pos, &mut buf[..want])?;
        self.pos += want as u64;
        self.remaining -= want as u64;
        Ok(want)
    }
}

impl std::io::Seek for BoundedReader<'_> {
    fn seek(&mut self, style: std::io::SeekFrom) -> std::io::Result<u64> {
        let len = self.inner.len();
        let next = match style {
            std::io::SeekFrom::Start(n) => i128::from(n),
            std::io::SeekFrom::End(n) => i128::from(len) + i128::from(n),
            std::io::SeekFrom::Current(n) => i128::from(self.pos) + i128::from(n),
        };
        if next < 0 {
            return Err(std::io::Error::new(std::io::ErrorKind::InvalidInput, "seek before start"));
        }
        self.pos = u64::try_from(next).map_err(|_| std::io::Error::new(std::io::ErrorKind::InvalidInput, "seek overflow"))?;
        Ok(self.pos)
    }
}

/// Like [`probe_geometry`], reading only bounded header bytes from `r` instead of
/// requiring the whole file in memory. Large stills (e.g. >64 MiB uncompressed TIFFs,
/// even with an IFD at the end of the file) are probed via `Read + Seek` without
/// reading pixel data or rejecting by total file size.
pub fn probe_geometry_reader(name: &str, r: &dyn crate::ByteReader) -> Option<(u32, u32, Orientation)> {
    const SNIFF_MAX: u64 = 64 * 1024;
    const HEADER_PREFIX_MAX: u64 = 1024 * 1024;
    const HEADER_READ_BUDGET: u64 = 16 * 1024 * 1024;

    let len = r.len();
    if len <= SNIFF_MAX {
        let mut buf = vec![0u8; usize::try_from(len).ok()?];
        r.read_at(0, &mut buf).ok()?;
        return probe_geometry(name, &buf);
    }

    let mut prefix = vec![0u8; SNIFF_MAX as usize];
    r.read_at(0, &mut prefix).ok()?;
    if hdr::sniff(&prefix) {
        return None;
    }
    if exr::sniff(&prefix)
        || dng::sniff(&prefix)
        || crate::dpx::sniff(&prefix)
        || crate::tga::sniff(name, &prefix)
        || psd::sniff(&prefix)
        || ycbcr_tiff::sniff(&prefix)
    {
        let cap = usize::try_from(len.min(HEADER_PREFIX_MAX)).ok()?;
        if cap > prefix.len() {
            prefix.resize(cap, 0);
            r.read_at(0, &mut prefix).ok()?;
        }
        return probe_geometry(name, &prefix);
    }
    if !sniff(name, &prefix) {
        return None;
    }

    let mut dec =
        image::ImageReader::new(std::io::BufReader::new(BoundedReader::new(r, HEADER_READ_BUDGET))).with_guessed_format().ok()?.into_decoder().ok()?;
    let (w, h) = dec.dimensions();
    if u64::from(w) * u64::from(h) > MAX_PIXELS {
        return None;
    }
    let orientation = dec.orientation().ok().unwrap_or(Orientation::NoTransforms);
    let (w, h) = if transposes(orientation) { (h, w) } else { (w, h) };
    Some((w, h, orientation))
}

pub struct StillSource {
    info: MediaInfo,
    frame: Arc<VideoFrame>,
    /// The EXIF Orientation the decoder applied to this picture (see [`Raster::orientation`]).
    orientation: Orientation,
}

impl StillSource {
    pub fn decode(name: &str, bytes: &[u8]) -> Result<Self> {
        // a DNG is a TIFF too: develop its raw image instead of reading its preview
        if dng::sniff(bytes) {
            return dng::still(name, bytes);
        }
        if hdr::sniff(bytes) {
            return hdr::still(name, bytes);
        }
        let r = decode_raster(name, bytes)?;
        Ok(Self::from_rgba(name, r.width, r.height, r.rgba, r.has_alpha, bytes.len() as u64).with_orientation(r.orientation))
    }

    /// Record the orientation the decoder applied; `from_rgba` pictures are already
    /// upright, so they keep `NoTransforms`.
    fn with_orientation(mut self, orientation: Orientation) -> Self {
        self.orientation = orientation;
        self
    }

    pub fn from_rgba(name: &str, w: u32, h: u32, rgba: Vec<u8>, has_alpha: bool, file_size: u64) -> Self {
        let codec = name.rsplit('.').next().unwrap_or("image").to_ascii_uppercase();
        let info = MediaInfo {
            name: name.to_string(),
            kind: MediaKind::Still,
            duration: DEFAULT_STILL_DURATION,
            video: Some(VideoStreamInfo {
                width: w,
                height: h,
                frame_rate: FrameRate::FPS_29_97,
                par: (1, 1),
                codec,
                pixel_format: if has_alpha { "RGBA 8-bit".into() } else { "RGB 8-bit".into() },
                color: filmcraft_color::ColorInfo::SRGB_FULL,
                has_alpha,
                bitrate: None,
                hdr: None,
                color_assumed: None,
            }),
            audio_streams: Vec::new(),
            container: "Image".into(),
            start_timecode: None,
            file_size: Some(file_size),
        };
        Self { info, frame: Arc::new(VideoFrame::rgba8(w, h, rgba)), orientation: Orientation::NoTransforms }
    }
}

impl MediaSource for StillSource {
    fn info(&self) -> &MediaInfo {
        &self.info
    }
    fn still_orientation(&self) -> Orientation {
        self.orientation
    }
    fn video_frame(&self, _req: FrameRequest) -> Result<Arc<VideoFrame>> {
        Ok(self.frame.clone())
    }
    fn audio(&self, _start: i64, _frames: usize, _sample_rate: u32) -> Result<filmcraft_frame::AudioBuffer> {
        Err(MediaError::NoStream("audio"))
    }
}
