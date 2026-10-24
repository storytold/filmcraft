//! Hardware decoding through VA-API (Linux: Intel, AMD and other Mesa / vendor drivers).
//!
//! VA-API decoding is stateless: the application parses the stream and keeps the decoded picture
//! buffer, and the GPU decodes one picture's slice data at a time into a surface. [`h264`] does the
//! host side with the software decoder's own parsers and DPB (so the two decide alike), [`va`]
//! drives libva (loaded at run time; no libva means no hardware decoder, never a failure to start),
//! and [`VaDecoder`] puts them together as a [`VideoDecoder`]. H.264 8-bit 4:2:0 progressive
//! (Constrained Baseline / Main / High) and HEVC 4:2:0 (Main, Main 10, Main Still Picture), the
//! latter through [`hevc`] the same way, and AV1 Main 4:2:0 (8 and 10-bit) through [`av1`];
//! everything else stays in software.
//!
//! The FFI is in [`ffi`] (declarations) and `va` (calls); the rest of this module is safe code.

pub mod av1;
pub mod ffi;
pub mod h264;
pub mod hevc;
#[cfg(target_os = "linux")]
#[allow(unsafe_code)]
pub mod va;

#[cfg(test)]
mod abi_tests;
#[cfg(test)]
mod tests;

#[cfg(target_os = "linux")]
pub use linux::VaDecoder;

use filmcraft_frame::VideoFrame;

/// The hardware a front end ([`h264::Front`], [`hevc::Front`]) drives: a fixed set of surfaces,
/// picture decoding into one of them, and reading one back or filling it.
pub trait Accel: Send {
    /// The VA surface ids of the decoder's surfaces; front ends refer to them by index.
    fn surfaces(&self) -> &[ffi::VASurfaceID];
    /// Decode one H.264 picture (its slices: parameters and the NAL unit, header byte and
    /// emulation prevention included) into surface `target` (an index into [`Accel::surfaces`]).
    fn decode_h264(
        &mut self,
        target: usize,
        pic: &ffi::VAPictureParameterBufferH264,
        iq: &ffi::VAIQMatrixBufferH264,
        slices: &[(ffi::VASliceParameterBufferH264, Vec<u8>)],
    ) -> std::result::Result<(), String>;
    /// Decode one HEVC picture (its slice segments: parameters and the NAL unit as stored) into
    /// surface `target`; `iq` when scaling lists are enabled.
    fn decode_hevc(
        &mut self,
        target: usize,
        pic: &ffi::VAPictureParameterBufferHEVC,
        iq: Option<&ffi::VAIQMatrixBufferHEVC>,
        slices: &[(ffi::VASliceParameterBufferHEVC, Vec<u8>)],
    ) -> std::result::Result<(), String>;
    /// Decode one AV1 frame (one parameter + data buffer per tile) into surface `target`.
    fn decode_av1(
        &mut self,
        target: usize,
        pic: &ffi::VADecPictureParameterBufferAV1,
        tiles: &[(ffi::VASliceParameterBufferAV1, Vec<u8>)],
    ) -> std::result::Result<(), String>;
    /// The picture in surface `index`, once decoded.
    fn read(&mut self, index: usize) -> std::result::Result<VideoFrame, String>;
    /// Make surface `index` a copy of surface `from`, or mid-gray (every sample half the range)
    /// when `None`: the stand-ins for reference pictures that were never decoded, as the software
    /// decoders make them.
    fn fill(&mut self, index: usize, from: Option<usize>) -> std::result::Result<(), String>;
}

#[cfg(target_os = "linux")]
mod linux {
    use filmcraft_bitstream::unescape_rbsp;
    use filmcraft_codecs::hw::{FrameCodec, FrameStreamInfo, NalCodec, NalStreamInfo, StreamInfo};
    use filmcraft_codecs::{CodecError, DecodedFrame, Result, VideoDecoder};

    use super::va::Session;
    use super::{av1, ffi, h264, hevc};
    use crate::biplanar::Geometry;

    /// The VA-API profiles that decode an H.264 stream of `profile_idc`, best fit first (a Main or
    /// High decoder also decodes what the smaller profiles allow).
    pub(crate) fn h264_profiles(profile_idc: u8) -> Option<&'static [ffi::VAProfile]> {
        Some(match profile_idc {
            66 => &[ffi::VAProfileH264ConstrainedBaseline, ffi::VAProfileH264Main, ffi::VAProfileH264High],
            77 => &[ffi::VAProfileH264Main, ffi::VAProfileH264High],
            100 => &[ffi::VAProfileH264High],
            _ => return None,
        })
    }

    /// The VA-API profiles that decode an HEVC stream of `general_profile_idc` (Main, Main 10,
    /// Main Still Picture) at `bits`, best fit first.
    pub(crate) fn hevc_profiles(profile_idc: u8, bits: u32) -> Option<&'static [ffi::VAProfile]> {
        Some(match (profile_idc, bits) {
            (1 | 3, 8) => &[ffi::VAProfileHEVCMain, ffi::VAProfileHEVCMain10],
            (2, 8) => &[ffi::VAProfileHEVCMain10, ffi::VAProfileHEVCMain],
            (2, 10) => &[ffi::VAProfileHEVCMain10],
            _ => return None,
        })
    }

    enum Front {
        H264(Box<h264::Front<Session>>),
        Hevc(Box<hevc::Front<Session>>),
        Av1(Box<av1::Front<Session>>),
    }

    /// A VA-API decoder for one H.264, HEVC or AV1 stream.
    pub struct VaDecoder {
        front: Front,
        info: StreamInfo,
        name: String,
        fed: u64,
        fail_after: Option<u64>,
    }

    /// The first parameter set of NAL unit type `ty` among `sets`, without its header (`header`
    /// bytes) and emulation prevention.
    fn parameter_set(sets: &[Vec<u8>], header: usize, ty: impl Fn(u8) -> bool) -> Option<Vec<u8>> {
        sets.iter().find(|n| n.first().is_some_and(|&h| ty(h))).and_then(|n| Some(unescape_rbsp(n.get(header..)?)))
    }

    impl VaDecoder {
        /// A decoder for the stream `info` describes, or why VA-API does not take it on this system.
        pub fn new(info: NalStreamInfo) -> std::result::Result<Self, String> {
            let bits = info.bit_depth_luma;
            if info.chroma_format_idc != 1 || info.bit_depth_chroma != bits || info.interlaced {
                return Err(format!(
                    "{} chroma format {} at {bits}/{}-bit is not taken",
                    if info.interlaced { "interlaced" } else { "progressive" },
                    info.chroma_format_idc,
                    info.bit_depth_chroma
                ));
            }
            let geometry = Geometry { crop: info.crop, bits, color: info.color, par: info.par };
            let (front, codec, vendor) = match info.codec {
                NalCodec::H264 => {
                    if bits != 8 {
                        return Err(format!("{bits}-bit H.264 is not taken"));
                    }
                    let profiles = h264_profiles(info.profile_idc).ok_or_else(|| format!("H.264 profile_idc {} is not taken", info.profile_idc))?;
                    let rbsp = parameter_set(&info.parameter_sets, 1, |h| h & 0x1f == 7).ok_or("no SPS")?;
                    let sps = filmcraft_h264::params::Sps::parse(&rbsp).map_err(|e| e.to_string())?;
                    sps.check_supported().map_err(|e| e.to_string())?;
                    let mbs = (sps.pic_width_in_mbs, sps.frame_height_in_mbs());
                    // the DPB, the picture being decoded, and one spare
                    let count = sps.max_dpb_frames().saturating_add(2).min(18);
                    let session = Session::new(profiles, (sps.width(), sps.height()), count, geometry)?;
                    let vendor = session.vendor().to_string();
                    (Front::H264(Box::new(h264::Front::new(session, info.length_size, mbs, &info.parameter_sets).map_err(|e| e.to_string())?)), "H.264", vendor)
                }
                NalCodec::Hevc => {
                    let profiles =
                        hevc_profiles(info.profile_idc, bits).ok_or_else(|| format!("HEVC profile {} at {bits}-bit is not taken", info.profile_idc))?;
                    let rbsp = parameter_set(&info.parameter_sets, 2, |h| (h >> 1) & 0x3f == 33).ok_or("no SPS")?;
                    let sps = filmcraft_hevc::params::Sps::parse(&rbsp).map_err(|e| e.to_string())?;
                    sps.check_supported().map_err(|e| e.to_string())?;
                    if sps.range_extension {
                        return Err("HEVC range extensions are not taken".into());
                    }
                    let count = (sps.max_dec_pic_buffering as usize).saturating_add(2).min(18);
                    let size = (sps.width, sps.height);
                    let session = Session::new(profiles, size, count, geometry)?;
                    let vendor = session.vendor().to_string();
                    (
                        Front::Hevc(Box::new(hevc::Front::new(session, info.length_size, size, bits, &info.parameter_sets).map_err(|e| e.to_string())?)),
                        "HEVC",
                        vendor,
                    )
                }
            };
            let name = format!("VA-API {codec} ({vendor})");
            Ok(Self { front, info: info.into(), name, fed: 0, fail_after: None })
        }

        /// A decoder for the AV1 stream `info` describes (Main profile, 8 or 10-bit 4:2:0), or why
        /// VA-API does not take it on this system. VP9 is not taken.
        pub fn new_frame(info: FrameStreamInfo) -> std::result::Result<Self, String> {
            if info.codec != FrameCodec::Av1 {
                return Err(format!("{:?} is not taken", info.codec));
            }
            let bits = info.bit_depth;
            if info.profile != 0 || info.mono || info.subsampling != (1, 1) || !matches!(bits, 8 | 10) {
                return Err(format!(
                    "AV1 profile {} {}-bit {} is not taken",
                    info.profile,
                    bits,
                    if info.mono { "monochrome".to_string() } else { format!("subsampling {:?}", info.subsampling) }
                ));
            }
            let (w, h) = info.size;
            if w == 0 || h == 0 {
                return Err("AV1 stream without a picture size".into());
            }
            let geometry = Geometry { crop: (0, 0, w, h), bits, color: info.color.unwrap_or_default(), par: (1, 1) };
            // the eight reference slots, the frame being decoded, and one spare
            let session = Session::new(&[ffi::VAProfileAV1Profile0], (w, h), 10, geometry)?;
            let vendor = session.vendor().to_string();
            let front = av1::Front::new(session, info.config_obus.clone(), (w, h), bits).map_err(|e| e.to_string())?;
            let name = format!("VA-API AV1 ({vendor})");
            Ok(Self { front: Front::Av1(Box::new(front)), info: info.into(), name, fed: 0, fail_after: None })
        }

        /// Test hook: fail every decode call after the first `n` (a GPU lost mid-stream).
        pub fn fail_after(&mut self, n: u64) {
            self.fail_after = Some(n);
        }
    }

    impl VideoDecoder for VaDecoder {
        fn decode(&mut self, sample: &[u8], pts: i64) -> Result<Vec<DecodedFrame>> {
            self.fed += 1;
            if self.fail_after.is_some_and(|n| self.fed > n) {
                return Err(CodecError::Decode("VA-API decoder failed (test hook)".into()));
            }
            match &mut self.front {
                Front::H264(f) => f.decode(sample, pts),
                Front::Hevc(f) => f.decode(sample, pts),
                Front::Av1(f) => f.decode(sample, pts),
            }
        }

        fn flush(&mut self) -> Vec<DecodedFrame> {
            let flushed = match &mut self.front {
                Front::H264(f) => f.flush(),
                Front::Hevc(f) => f.flush(),
                Front::Av1(f) => f.flush(),
            };
            match flushed {
                Ok(out) => out,
                Err(e) => {
                    log::warn!("{}: {e} while flushing", self.name);
                    Vec::new()
                }
            }
        }

        fn reset(&mut self) {
            match &mut self.front {
                Front::H264(f) => f.reset(),
                Front::Hevc(f) => f.reset(),
                Front::Av1(f) => f.reset(),
            }
        }

        fn name(&self) -> &str {
            &self.name
        }

        fn is_random_access(&self, sample: &[u8]) -> Option<bool> {
            self.info.is_random_access(sample)
        }

        fn is_disposable(&self, sample: &[u8]) -> bool {
            self.info.is_disposable(sample)
        }
    }
}
