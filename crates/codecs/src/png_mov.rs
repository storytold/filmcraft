//! PNG-coded video in QuickTime / MP4 (`png ` sample entries): every sample is a whole PNG image
//! file (ISO/IEC 15948 — the W3C PNG specification, Second Edition), which is how many tools hand
//! over graphics with an alpha channel. Pictures are intra-only, so any sample can be decoded on
//! its own and the alpha channel reaches the compositor as straight RGBA.

use filmcraft_frame::VideoFrame;
use filmcraft_isobmff::{CodecConfig, SampleEntry};

use crate::video::{DecodedFrame, VideoDecoder};
use crate::{CodecError, Result};

/// Whether a sample entry is a PNG track.
pub fn is_png(entry: &SampleEntry) -> bool {
    matches!(&entry.codec, CodecConfig::Unknown { fourcc, .. } if fourcc.0 == *b"png ")
}

/// Whether the PNG image in `sample` carries an alpha channel: from its IHDR colour type
/// (ISO/IEC 15948 §11.2.2), which is the third byte after the mandatory signature and length.
pub fn has_alpha(sample: &[u8]) -> bool {
    const PNG_SIGNATURE: [u8; 8] = [0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];
    if sample.get(..8) != Some(&PNG_SIGNATURE) || sample.get(12..16) != Some(b"IHDR") {
        return false;
    }
    // colour type: 4 = grey + alpha, 6 = RGB + alpha
    matches!(sample.get(25), Some(4 | 6))
}

/// One PNG picture per sample.
pub struct PngSampleDecoder {
    width: u32,
    height: u32,
}

impl PngSampleDecoder {
    pub fn new(width: u32, height: u32) -> Self {
        Self { width, height }
    }
}

impl VideoDecoder for PngSampleDecoder {
    fn decode(&mut self, sample: &[u8], pts: i64) -> Result<Vec<DecodedFrame>> {
        let img = image::load_from_memory_with_format(sample, image::ImageFormat::Png).map_err(|e| CodecError::Decode(format!("PNG sample: {e}")))?;
        let rgba = img.into_rgba8();
        if (rgba.width(), rgba.height()) != (self.width, self.height) {
            return Err(CodecError::Decode(format!("PNG sample is {}x{}, the sample entry says {}x{}", rgba.width(), rgba.height(), self.width, self.height)));
        }
        let frame = VideoFrame::rgba8(self.width, self.height, rgba.into_raw());
        Ok(vec![DecodedFrame { pts, frame, draft: false }])
    }

    fn flush(&mut self) -> Vec<DecodedFrame> {
        Vec::new()
    }

    fn reset(&mut self) {}

    fn name(&self) -> &str {
        "png"
    }

    fn intra_only(&self) -> bool {
        true
    }
}

pub fn factory(e: &SampleEntry) -> Option<Result<Box<dyn VideoDecoder>>> {
    let v = e.video.as_ref()?;
    if !is_png(e) {
        return None;
    }
    let (w, h) = (u32::from(v.width), u32::from(v.height));
    if w == 0 || h == 0 || w > 16_384 || h > 16_384 {
        return Some(Err(CodecError::Unsupported(format!("{w}x{h} PNG video"))));
    }
    Some(Ok(Box::new(PngSampleDecoder::new(w, h)) as Box<dyn VideoDecoder>))
}

#[cfg(test)]
mod tests {
    use super::has_alpha;

    fn ihdr(colour_type: u8) -> Vec<u8> {
        let mut v = vec![0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a, 0, 0, 0, 13];
        v.extend_from_slice(b"IHDR");
        v.extend_from_slice(&[0, 0, 0, 2, 0, 0, 0, 2, 8, colour_type]);
        v
    }

    #[test]
    fn alpha_comes_from_the_colour_type() {
        assert!(has_alpha(&ihdr(6)));
        assert!(has_alpha(&ihdr(4)));
        assert!(!has_alpha(&ihdr(2)));
        assert!(!has_alpha(&ihdr(0)));
        assert!(!has_alpha(b"not a png"));
    }
}
