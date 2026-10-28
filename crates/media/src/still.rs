//! Still images (PNG, JPEG, GIF, WebP, TIFF, BMP, Targa, DPX, OpenEXR, Radiance HDR, DNG camera raw) as media
//! sources. Animated PNG and GIF files open as movies instead ([`crate::animated`]); here they would
//! show their first frame.

use std::{io::Cursor, sync::Arc};

use filmcraft_frame::VideoFrame;
use filmcraft_time::{FrameRate, TICKS_PER_SECOND, Tick};

use crate::{FrameRequest, MediaError, MediaInfo, MediaKind, MediaSource, Result, VideoStreamInfo};

pub mod dng;
pub mod exr;
pub mod hdr;
mod ycbcr_tiff;

/// Default still duration (Premiere's preference default is 5 seconds).
pub const DEFAULT_STILL_DURATION: Tick = Tick(5 * TICKS_PER_SECOND);

/// Whether `b` (the contents of the file `name`) is a still image we decode.
pub fn sniff(name: &str, b: &[u8]) -> bool {
    crate::dpx::sniff(b)
        || hdr::sniff(b)
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
    if ycbcr_tiff::sniff(bytes) {
        return ycbcr_tiff::decode(name, bytes);
    }
    let img = if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
        decode_with_orientation(image::codecs::jpeg::JpegDecoder::new(Cursor::new(bytes)).map_err(|e| MediaError::Decode(format!("{name}: {e}")))?, name)?
    } else if bytes.starts_with(b"II*\0") || bytes.starts_with(b"MM\0*") {
        let decoder = image::ImageReader::with_format(Cursor::new(bytes), image::ImageFormat::Tiff)
            .into_decoder()
            .map_err(|e| MediaError::Decode(format!("{name}: {e}")))?;
        decode_with_orientation(decoder, name)?
    } else {
        image::load_from_memory(bytes).map_err(|e| MediaError::Decode(format!("{name}: {e}")))?
    };
    let has_alpha = img.color().has_alpha();
    let rgba = img.to_rgba8();
    let (width, height) = rgba.dimensions();
    Ok(Raster { width, height, rgba: rgba.into_raw(), has_alpha })
}

fn decode_with_orientation(decoder: impl image::ImageDecoder, name: &str) -> Result<image::DynamicImage> {
    let mut decoder = decoder;
    // EXIF is optional metadata; malformed EXIF must not prevent decoding valid image pixels.
    let orientation = decoder.orientation().unwrap_or(image::metadata::Orientation::NoTransforms);
    let mut img = image::DynamicImage::from_decoder(decoder).map_err(|e| MediaError::Decode(format!("{name}: {e}")))?;
    img.apply_orientation(orientation);
    Ok(img)
}

pub struct StillSource {
    info: MediaInfo,
    frame: Arc<VideoFrame>,
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
        Ok(Self::from_rgba(name, r.width, r.height, r.rgba, r.has_alpha, bytes.len() as u64))
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
            }),
            audio_streams: Vec::new(),
            container: "Image".into(),
            start_timecode: None,
            file_size: Some(file_size),
        };
        Self { info, frame: Arc::new(VideoFrame::rgba8(w, h, rgba)) }
    }
}

impl MediaSource for StillSource {
    fn info(&self) -> &MediaInfo {
        &self.info
    }
    fn video_frame(&self, _req: FrameRequest) -> Result<Arc<VideoFrame>> {
        Ok(self.frame.clone())
    }
    fn audio(&self, _start: i64, _frames: usize, _sample_rate: u32) -> Result<filmcraft_frame::AudioBuffer> {
        Err(MediaError::NoStream("audio"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn jpeg() -> Vec<u8> {
        let image = image::RgbImage::from_fn(3, 2, |x, y| image::Rgb([(x * 73 + y * 11) as u8, (y * 101 + x * 7) as u8, (x * 29 + y * 53) as u8]));
        let mut bytes = Cursor::new(Vec::new());
        image::DynamicImage::ImageRgb8(image).write_to(&mut bytes, image::ImageFormat::Jpeg).expect("encode test JPEG");
        bytes.into_inner()
    }

    fn with_app1(jpeg: &[u8], payload: &[u8]) -> Vec<u8> {
        let length = (payload.len() + 2) as u16;
        let mut out = Vec::with_capacity(jpeg.len() + payload.len() + 4);
        out.extend_from_slice(&jpeg[..2]);
        out.extend_from_slice(&[0xFF, 0xE1]);
        out.extend_from_slice(&length.to_be_bytes());
        out.extend_from_slice(payload);
        out.extend_from_slice(&jpeg[2..]);
        out
    }

    fn with_orientation(jpeg: &[u8], value: u16) -> Vec<u8> {
        let exif =
            [b"Exif\0\0".as_slice(), b"II\x2a\0\x08\0\0\0\x01\0\x12\x01\x03\0\x01\0\0\0".as_slice(), &value.to_le_bytes(), b"\0\0\0\0".as_slice()].concat();
        with_app1(jpeg, &exif)
    }

    fn tiff_with_orientation(orientation: u16) -> Vec<u8> {
        fn short_entry(bytes: &mut Vec<u8>, tag: u16, value: u16) {
            bytes.extend_from_slice(&tag.to_le_bytes());
            bytes.extend_from_slice(&3u16.to_le_bytes());
            bytes.extend_from_slice(&1u32.to_le_bytes());
            bytes.extend_from_slice(&value.to_le_bytes());
            bytes.extend_from_slice(&[0; 2]);
        }
        fn long_entry(bytes: &mut Vec<u8>, tag: u16, value: u32) {
            bytes.extend_from_slice(&tag.to_le_bytes());
            bytes.extend_from_slice(&4u16.to_le_bytes());
            bytes.extend_from_slice(&1u32.to_le_bytes());
            bytes.extend_from_slice(&value.to_le_bytes());
        }

        let mut bytes = b"II\x2a\0\x08\0\0\0".to_vec();
        bytes.extend_from_slice(&11u16.to_le_bytes());
        long_entry(&mut bytes, 256, 3); // width
        long_entry(&mut bytes, 257, 2); // height
        bytes.extend_from_slice(&258u16.to_le_bytes()); // bits per sample at offset 146
        bytes.extend_from_slice(&3u16.to_le_bytes());
        bytes.extend_from_slice(&3u32.to_le_bytes());
        bytes.extend_from_slice(&146u32.to_le_bytes());
        short_entry(&mut bytes, 259, 1); // no compression
        short_entry(&mut bytes, 262, 2); // RGB
        long_entry(&mut bytes, 273, 152); // strip offset
        short_entry(&mut bytes, 274, orientation);
        short_entry(&mut bytes, 277, 3); // RGB samples
        long_entry(&mut bytes, 278, 2); // rows per strip
        long_entry(&mut bytes, 279, 18); // strip byte count
        short_entry(&mut bytes, 284, 1); // chunky planar layout
        bytes.extend_from_slice(&0u32.to_le_bytes()); // no next IFD
        for _ in 0..3 {
            bytes.extend_from_slice(&8u16.to_le_bytes());
        }
        bytes.extend_from_slice(&[250, 5, 10, 5, 240, 20, 10, 15, 230, 20, 220, 30, 30, 25, 210, 40, 30, 200]);
        bytes
    }

    fn expected_orientation(source: &Raster, orientation: u16) -> (u32, u32, Vec<u8>) {
        let (w, h) = (source.width, source.height);
        let (out_w, out_h) = if (5..=8).contains(&orientation) { (h, w) } else { (w, h) };
        let mut pixels = vec![0; (out_w * out_h * 4) as usize];
        for y in 0..h {
            for x in 0..w {
                let (dx, dy) = match orientation {
                    2 => (w - 1 - x, y),
                    3 => (w - 1 - x, h - 1 - y),
                    4 => (x, h - 1 - y),
                    5 => (y, x),
                    6 => (h - 1 - y, x),
                    7 => (h - 1 - y, w - 1 - x),
                    8 => (y, w - 1 - x),
                    _ => (x, y),
                };
                let src = ((y * w + x) * 4) as usize;
                let dst = ((dy * out_w + dx) * 4) as usize;
                pixels[dst..dst + 4].copy_from_slice(&source.rgba[src..src + 4]);
            }
        }
        (out_w, out_h, pixels)
    }

    #[test]
    fn jpeg_exif_orientations_transform_decoded_pixels_and_import_dimensions() {
        let plain = jpeg();
        let original = decode_raster("sample.jpg", &plain).expect("decode generated JPEG");
        for orientation in 1..=8 {
            let bytes = with_orientation(&plain, orientation);
            let raster = decode_raster("sample.jpg", &bytes).expect("decode oriented JPEG");
            let (width, height, pixels) = expected_orientation(&original, orientation);
            assert_eq!((raster.width, raster.height), (width, height), "orientation {orientation}");
            assert_eq!(raster.rgba, pixels, "orientation {orientation}");

            let source = StillSource::decode("sample.jpg", &bytes).expect("import oriented JPEG");
            let video = source.info().video.as_ref().expect("still video info");
            assert_eq!((video.width, video.height), (width, height), "orientation {orientation}");
            let frame = source.video_frame(FrameRequest::full(Tick::ZERO)).expect("read still frame");
            assert_eq!((frame.width, frame.height), (width, height), "orientation {orientation}");
        }
    }

    #[test]
    fn jpeg_without_usable_exif_orientation_stays_unchanged() {
        let plain = jpeg();
        let original = decode_raster("sample.jpg", &plain).expect("decode generated JPEG");
        let invalid = with_orientation(&plain, 9);
        let mut oversized = b"Exif\0\0".to_vec();
        oversized.resize(60_000, 0);
        let malformed = [
            with_app1(&plain, &[]),                              // APP1 with no EXIF bytes
            with_app1(&plain, b"Exif\0\0"),                      // truncated TIFF header
            with_app1(&plain, b"Exif\0\0XX\0\0"),                // invalid byte-order marker
            with_app1(&plain, b"Exif\0\0II*\0\xFF\xFF\xFF\xFF"), // out-of-bounds IFD offset
            with_app1(&plain, b"Exif\0\0garbled"),               // corrupt EXIF data
            with_app1(&plain, &oversized),                       // large but within the JPEG APP1 segment limit
        ];
        for bytes in std::iter::once(&plain).chain(std::iter::once(&invalid)).chain(malformed.iter()) {
            let decoded = decode_raster("sample.jpg", bytes).expect("decode JPEG despite unusable EXIF");
            assert_eq!((decoded.width, decoded.height), (original.width, original.height));
            assert_eq!(decoded.rgba, original.rgba);
        }
    }

    #[test]
    fn ordinary_tiff_orientation_is_applied_once() {
        let bytes = tiff_with_orientation(6);
        let raster = decode_raster("oriented.tif", &bytes).expect("decode oriented TIFF");
        assert_eq!((raster.width, raster.height), (2, 3));
        let source = StillSource::decode("oriented.tif", &bytes).expect("import oriented TIFF");
        let video = source.info().video.as_ref().expect("still video info");
        assert_eq!((video.width, video.height), (2, 3));
        assert_eq!((source.frame.width, source.frame.height), (2, 3));
        assert_eq!(
            raster.rgba,
            match &source.frame.data {
                filmcraft_frame::PixelData::Rgba8(pixels) => pixels.as_slice().to_vec(),
                _ => panic!("still frame should retain RGBA8 pixels"),
            }
        );
    }

    #[test]
    fn malformed_jpeg_app1_never_panics() {
        let jpeg = jpeg();
        let mut truncated = jpeg[..2].to_vec();
        truncated.extend_from_slice(&[0xFF, 0xE1, 0xFF, 0xFF]);
        truncated.extend_from_slice(b"Exif\0\0II");
        truncated.extend_from_slice(&jpeg[2..]);
        for bytes in [truncated, with_app1(&jpeg, b"Exif\0\0II*\0\x01\0"), with_app1(&jpeg, &[0; 60_000])] {
            assert!(std::panic::catch_unwind(|| decode_raster("sample.jpg", &bytes)).is_ok());
        }
    }
}
