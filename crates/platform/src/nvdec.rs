//! Hardware decoding through NVDEC (Linux, NVIDIA's proprietary driver).
//!
//! Unlike VA-API, NVDEC brings its own stream parser (`libnvcuvid`): [`cuvid::Session`] is fed
//! Annex B access units and returns pictures in presentation order, read back from the GPU through
//! the CUDA driver. Both libraries are loaded at run time; without them there is no NVDEC decoder,
//! never a failure to start. H.264 8-bit and HEVC 8- / 10-bit, 4:2:0 progressive; everything else
//! stays with VA-API or in software.
//!
//! The FFI is in [`ffi`] (declarations) and [`cuvid`] (calls); the rest of this module is safe code.

#[allow(unsafe_code)]
pub mod cuvid;
pub mod ffi;

#[cfg(test)]
mod abi_tests;

use filmcraft_codecs::hw::{NalCodec, NalStreamInfo};
use filmcraft_codecs::{CodecError, DecodedFrame, Result, VideoDecoder};

use crate::annexb::to_annex_b;
use crate::biplanar::Geometry;

/// An NVDEC decoder for one H.264 or HEVC stream.
pub struct NvDecoder {
    session: cuvid::Session,
    info: NalStreamInfo,
    name: &'static str,
    /// Whether the parser has the stream's parameter sets (sent with the first sample after a reset).
    primed: bool,
    /// HEVC, after starting at a CRA / BLA picture: its RASL pictures (they refer to pictures
    /// before it) are not fed until the next random access point.
    skip_rasl: bool,
    fed: u64,
    fail_after: Option<u64>,
}

impl NvDecoder {
    /// A decoder for the stream `info` describes, or why NVDEC does not take it on this system.
    pub fn new(info: NalStreamInfo) -> std::result::Result<Self, String> {
        let bits = info.bit_depth_luma;
        let taken = match info.codec {
            NalCodec::H264 => bits == 8,
            NalCodec::Hevc => matches!(bits, 8 | 10),
        };
        if !taken || info.chroma_format_idc != 1 || info.bit_depth_chroma != bits || info.interlaced {
            return Err(format!(
                "{} chroma format {} at {bits}/{}-bit is not taken",
                if info.interlaced { "interlaced" } else { "progressive" },
                info.chroma_format_idc,
                info.bit_depth_chroma
            ));
        }
        let geometry = Geometry { crop: info.crop, bits, color: info.color, par: info.par };
        let session = cuvid::Session::new(info.codec, info.coded, geometry, info.reorder)?;
        let name = match info.codec {
            NalCodec::H264 => "NVDEC H.264",
            NalCodec::Hevc => "NVDEC HEVC",
        };
        Ok(Self { session, info, name, primed: false, skip_rasl: false, fed: 0, fail_after: None })
    }

    /// Test hook: fail every decode call after the first `n` (a GPU lost mid-stream).
    pub fn fail_after(&mut self, n: u64) {
        self.fail_after = Some(n);
    }
}

fn frames(out: Vec<(i64, filmcraft_frame::VideoFrame)>) -> Vec<DecodedFrame> {
    out.into_iter().map(|(pts, frame)| DecodedFrame { pts, frame, draft: false }).collect()
}

impl VideoDecoder for NvDecoder {
    fn decode(&mut self, sample: &[u8], pts: i64) -> Result<Vec<DecodedFrame>> {
        self.fed += 1;
        if self.fail_after.is_some_and(|n| self.fed > n) {
            return Err(CodecError::Decode("NVDEC decoder failed (test hook)".into()));
        }
        if self.info.codec == NalCodec::Hevc {
            let types = self.info.nal_types(sample);
            if self.info.is_irap(sample) {
                // IDR pictures (19, 20) have no RASL pictures
                self.skip_rasl = !self.primed && !types.iter().any(|t| matches!(t, 19 | 20));
            } else if self.skip_rasl && types.iter().any(|t| matches!(t, 8 | 9)) {
                // Not output by any decoder (the software one drops them too); the parser drops
                // them as well, but then shows the CRA picture with the last one's timestamp.
                return Ok(Vec::new());
            }
        }
        let data = to_annex_b(&self.info, sample, !self.primed).map_err(CodecError::Decode)?;
        self.primed = true;
        self.session.feed(&data, pts).map(frames).map_err(CodecError::Decode)
    }

    fn flush(&mut self) -> Vec<DecodedFrame> {
        // the parser starts over after an end of stream
        self.primed = false;
        match self.session.flush() {
            Ok(out) => frames(out),
            Err(e) => {
                log::warn!("{}: {e} while flushing", self.name);
                Vec::new()
            }
        }
    }

    fn reset(&mut self) {
        self.primed = false;
        self.session.reset();
    }

    fn name(&self) -> &str {
        self.name
    }

    fn is_random_access(&self, sample: &[u8]) -> Option<bool> {
        self.info.is_random_access(sample)
    }

    fn is_disposable(&self, sample: &[u8]) -> bool {
        self.info.is_disposable(sample)
    }
}
