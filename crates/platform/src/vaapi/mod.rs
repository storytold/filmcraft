//! Hardware decoding through VA-API (Linux: Intel, AMD and other Mesa / vendor drivers).
//!
//! VA-API decoding is stateless: the application parses the stream and keeps the decoded picture
//! buffer, and the GPU decodes one picture's slice data at a time into a surface. [`h264`] does the
//! host side with the software decoder's own parsers and DPB (so the two decide alike), [`va`]
//! drives libva (loaded at run time; no libva means no hardware decoder, never a failure to start),
//! and [`VaDecoder`] puts them together as a [`VideoDecoder`]. H.264 8-bit 4:2:0 progressive
//! (Constrained Baseline / Main / High) and HEVC 4:2:0 (Main, Main 10, Main Still Picture), the
//! latter through [`hevc`] the same way; everything else stays in software. H.264 export encoding
//! goes through `h264enc` (Linux), on the same shared display.
//!
//! The FFI is in [`ffi`] (declarations) and `va` (calls); the rest of this module is safe code.
//! `va::Session::export_surface` provides borrowed, read-only DRM PRIME handles after VA sync;
//! it does not import them into Vulkan or claim zero-copy operation.

pub mod ffi;
pub mod h264;

/// Identity of a DRM render node. Major/minor pair comes from the held device node, not its name.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct DrmNodeId {
    pub major: u32,
    pub minor: u32,
}

#[cfg(target_os = "linux")]
pub mod h264enc;
pub mod hevc;
#[cfg(all(target_os = "linux", target_pointer_width = "64"))]
pub mod hevcenc;
pub(crate) mod nv12;
#[cfg(target_os = "linux")]
#[allow(unsafe_code)]
pub mod va;

#[cfg(test)]
mod abi_tests;
#[cfg(test)]
mod tests;

#[cfg(target_os = "linux")]
pub use linux::{ResidentDecodedFrame, ResidentVaDecoder, VaDecoder};

/// Per-output VUI metadata retained from the exact DPB picture, including mid-stream SPS changes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ResidentColorMetadata {
    pub full_range: bool,
    pub primaries: u8,
    pub transfer: u8,
    pub matrix: u8,
    pub sar: (u16, u16),
}

/// Metadata passed to a resident sink before platform lease acquisition, so it can reserve a
/// bounded consumer slot or cancel before another VA surface becomes leased.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ResidentFrameMetadata {
    pub index: usize,
    pub pts: i64,
    pub crop: (u32, u32, u32, u32),
    pub color: ResidentColorMetadata,
}

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
    /// The picture in surface `index`, once decoded.
    fn read(&mut self, index: usize) -> std::result::Result<VideoFrame, String>;
    /// Make surface `index` a copy of surface `from`, or mid-gray (every sample half the range)
    /// when `None`: the stand-ins for reference pictures that were never decoded, as the software
    /// decoders make them.
    fn fill(&mut self, index: usize, from: Option<usize>) -> std::result::Result<(), String>;

    /// Transfer ownership of a decoded surface to an external GPU consumer. CPU-only/test
    /// accelerators refuse by default; resident decode never falls back to `read`.
    #[cfg(target_os = "linux")]
    fn export_surface_owned(&self, _index: usize) -> std::result::Result<va::ResidentSurfaceLease, String> {
        Err("resident VA surface export is unsupported by this accelerator".into())
    }

    /// Whether an external consumer currently owns the surface at `index`.
    fn is_surface_leased(&self, _index: usize) -> bool {
        false
    }
}

trait FrameSink<A: Accel> {
    type Admission;
    fn reserve(&mut self, frame: ResidentFrameMetadata) -> filmcraft_codecs::Result<Self::Admission>;
    fn emit(&mut self, accel: &mut A, admission: Self::Admission, frame: ResidentFrameMetadata) -> filmcraft_codecs::Result<()>;
}

fn emit_outputs<A: Accel, S: FrameSink<A>>(
    accel: &mut A,
    outputs: impl IntoIterator<Item = ResidentFrameMetadata>,
    sink: &mut S,
) -> filmcraft_codecs::Result<()> {
    for output in outputs {
        let admission = sink.reserve(output)?;
        sink.emit(accel, admission, output)?;
    }
    Ok(())
}

#[derive(Default)]
struct CpuFrameSink {
    frames: Vec<filmcraft_codecs::DecodedFrame>,
}

impl<A: Accel> FrameSink<A> for CpuFrameSink {
    type Admission = ();

    fn reserve(&mut self, _frame: ResidentFrameMetadata) -> filmcraft_codecs::Result<()> {
        Ok(())
    }

    fn emit(&mut self, accel: &mut A, (): (), meta: ResidentFrameMetadata) -> filmcraft_codecs::Result<()> {
        let frame = accel.read(meta.index).map_err(filmcraft_codecs::CodecError::Decode)?;
        self.frames.push(filmcraft_codecs::DecodedFrame { pts: meta.pts, frame, draft: false });
        Ok(())
    }
}

#[cfg(test)]
mod resident_sink_tests {
    use super::*;

    struct AccelStub {
        surfaces: [ffi::VASurfaceID; 1],
        reads: usize,
        exports: std::cell::Cell<usize>,
    }

    impl Accel for AccelStub {
        fn surfaces(&self) -> &[ffi::VASurfaceID] {
            &self.surfaces
        }
        fn decode_h264(
            &mut self,
            _: usize,
            _: &ffi::VAPictureParameterBufferH264,
            _: &ffi::VAIQMatrixBufferH264,
            _: &[(ffi::VASliceParameterBufferH264, Vec<u8>)],
        ) -> std::result::Result<(), String> {
            Ok(())
        }
        fn decode_hevc(
            &mut self,
            _: usize,
            _: &ffi::VAPictureParameterBufferHEVC,
            _: Option<&ffi::VAIQMatrixBufferHEVC>,
            _: &[(ffi::VASliceParameterBufferHEVC, Vec<u8>)],
        ) -> std::result::Result<(), String> {
            Ok(())
        }
        fn read(&mut self, _: usize) -> std::result::Result<VideoFrame, String> {
            self.reads += 1;
            Err("unexpected CPU read".into())
        }
        fn fill(&mut self, _: usize, _: Option<usize>) -> std::result::Result<(), String> {
            Ok(())
        }
        #[cfg(target_os = "linux")]
        fn export_surface_owned(&self, _: usize) -> std::result::Result<va::ResidentSurfaceLease, String> {
            self.exports.set(self.exports.get() + 1);
            Err("unexpected surface export".into())
        }
    }

    struct ThreeAdmissionSink {
        reserved: usize,
        emitted: usize,
    }
    impl FrameSink<AccelStub> for ThreeAdmissionSink {
        type Admission = usize;
        fn reserve(&mut self, _: ResidentFrameMetadata) -> filmcraft_codecs::Result<usize> {
            if self.reserved == 3 {
                return Err(filmcraft_codecs::CodecError::Decode("queue full".into()));
            }
            let admission = self.reserved;
            self.reserved += 1;
            Ok(admission)
        }
        fn emit(&mut self, _: &mut AccelStub, admission: usize, frame: ResidentFrameMetadata) -> filmcraft_codecs::Result<()> {
            assert_eq!(admission, self.emitted);
            assert_eq!(frame.pts, self.emitted as i64);
            self.emitted += 1;
            Ok(())
        }
    }

    #[test]
    fn output_burst_reserves_before_emitting_fourth_and_never_reads_cpu_pixels() {
        let mut accel = AccelStub { surfaces: [0], reads: 0, exports: std::cell::Cell::new(0) };
        let mut sink = ThreeAdmissionSink { reserved: 0, emitted: 0 };
        let color = ResidentColorMetadata { full_range: false, primaries: 1, transfer: 1, matrix: 1, sar: (1, 1) };
        let outputs = (0..8).map(|index| ResidentFrameMetadata { index, pts: index as i64, crop: (0, 0, 16, 16), color });
        assert!(emit_outputs(&mut accel, outputs, &mut sink).is_err());
        assert_eq!((sink.reserved, sink.emitted), (3, 3));
        assert_eq!(accel.reads, 0);
    }

    #[cfg(target_os = "linux")]
    struct SkipUnwantedPts {
        reserved: usize,
        submitted: usize,
    }

    #[cfg(target_os = "linux")]
    impl ResidentFrameSink for SkipUnwantedPts {
        type Admission = ();

        fn wants_frame(&self, frame: ResidentFrameMetadata) -> bool {
            frame.pts >= 10
        }

        fn reserve(&mut self, _: ResidentFrameMetadata) -> std::result::Result<(), String> {
            self.reserved += 1;
            Ok(())
        }

        fn submit(&mut self, _: (), _: ResidentDecodedFrame) -> std::result::Result<(), String> {
            self.submitted += 1;
            Ok(())
        }
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn unwanted_pts_skip_admission_and_va_surface_export() {
        let mut accel = AccelStub { surfaces: [0], reads: 0, exports: std::cell::Cell::new(0) };
        let mut sink = SkipUnwantedPts { reserved: 0, submitted: 0 };
        let color = ResidentColorMetadata { full_range: false, primaries: 1, transfer: 1, matrix: 1, sar: (1, 1) };
        let frame = ResidentFrameMetadata { index: 0, pts: 9, crop: (0, 0, 16, 16), color };
        let mut output = ResidentOutputSink { sink: &mut sink, color: filmcraft_color::ColorInfo::default(), par: (1, 1) };
        let admission = FrameSink::<AccelStub>::reserve(&mut output, frame).expect("filtered frame reserves without error");
        assert_eq!(admission, None);
        FrameSink::<AccelStub>::emit(&mut output, &mut accel, admission, frame).expect("filtered frame emits no lease");
        assert_eq!((sink.reserved, sink.submitted, accel.exports.get()), (0, 0, 0));
    }
}

/// Consumer contract for resident decode. `reserve` runs before the VA lease is acquired; bounded
/// sinks block or cancel there. `submit` takes ownership of one frame and must retain it until the
/// exact GPU read is retired. Return errors to stop decode; no CPU fallback occurs.
#[cfg(target_os = "linux")]
pub trait ResidentFrameSink {
    type Admission;
    /// Whether this decoded picture is needed by the caller (for example, after source preroll).
    /// Rejected frames do not acquire or export a VA surface lease.
    fn wants_frame(&self, _frame: ResidentFrameMetadata) -> bool {
        true
    }
    fn reserve(&mut self, frame: ResidentFrameMetadata) -> std::result::Result<Self::Admission, String>;
    fn submit(&mut self, admission: Self::Admission, frame: ResidentDecodedFrame) -> std::result::Result<(), String>;
}

#[cfg(target_os = "linux")]
struct ResidentOutputSink<'a, S> {
    sink: &'a mut S,
    color: filmcraft_color::ColorInfo,
    par: (u32, u32),
}

#[cfg(target_os = "linux")]
impl<A: Accel, S: ResidentFrameSink> FrameSink<A> for ResidentOutputSink<'_, S> {
    type Admission = Option<S::Admission>;

    fn reserve(&mut self, frame: ResidentFrameMetadata) -> filmcraft_codecs::Result<Self::Admission> {
        if self.sink.wants_frame(frame) { self.sink.reserve(frame).map(Some).map_err(filmcraft_codecs::CodecError::Decode) } else { Ok(None) }
    }

    fn emit(&mut self, accel: &mut A, admission: Self::Admission, meta: ResidentFrameMetadata) -> filmcraft_codecs::Result<()> {
        let Some(admission) = admission else { return Ok(()) };
        let lease = accel.export_surface_owned(meta.index).map_err(filmcraft_codecs::CodecError::Decode)?;
        self.sink
            .submit(
                admission,
                ResidentDecodedFrame { index: meta.index, pts: meta.pts, crop: meta.crop, color: self.color, color_metadata: meta.color, par: self.par, lease },
            )
            .map_err(filmcraft_codecs::CodecError::Decode)
    }
}

#[cfg(target_os = "linux")]
mod linux {
    use filmcraft_bitstream::unescape_rbsp;
    use filmcraft_codecs::hw::{NalCodec, NalStreamInfo};
    use filmcraft_codecs::{CodecError, DecodedFrame, Result, VideoDecoder};

    use super::va::Session;
    use super::{DrmNodeId, ffi, h264, hevc};
    use crate::biplanar::Geometry;
    use crate::vulkan_import::MAX_IN_FLIGHT_TRANSFERS;

    /// Surfaces for a DPB of `dpb_frames`: the DPB, the picture being decoded, one spare, and one
    /// more per transfer lease that can be in flight (a leased surface is never reused).
    pub(crate) fn pool_size(dpb_frames: usize) -> usize {
        dpb_frames.saturating_add(2).saturating_add(MAX_IN_FLIGHT_TRANSFERS).min(18 + MAX_IN_FLIGHT_TRANSFERS)
    }

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
    }

    /// A VA-API decoder for one H.264 or HEVC stream.
    pub struct VaDecoder {
        front: Front,
        info: NalStreamInfo,
        name: String,
        fed: u64,
        fail_after: Option<u64>,
    }

    /// Decoded VA surface output for a GPU consumer. Lease must be retired after its exact GPU
    /// read submission; crop/color/PAR preserve the software decoder's interpretation.
    pub struct ResidentDecodedFrame {
        pub index: usize,
        pub pts: i64,
        pub crop: (u32, u32, u32, u32),
        pub color: filmcraft_color::ColorInfo,
        pub color_metadata: super::ResidentColorMetadata,
        pub par: (u32, u32),
        pub lease: super::va::ResidentSurfaceLease,
    }

    /// Strict resident decode facade. It never implements `VideoDecoder`, so generic decode
    /// fallback cannot turn an unavailable resident surface into CPU pixels.
    pub struct ResidentVaDecoder {
        inner: VaDecoder,
    }

    /// The first parameter set of NAL unit type `ty` among `sets`, without its header (`header`
    /// bytes) and emulation prevention.
    fn parameter_set(sets: &[Vec<u8>], header: usize, ty: impl Fn(u8) -> bool) -> Option<Vec<u8>> {
        sets.iter().find(|n| n.first().is_some_and(|&h| ty(h))).and_then(|n| Some(unescape_rbsp(n.get(header..)?)))
    }

    impl VaDecoder {
        /// A decoder for the stream `info` describes, or why VA-API does not take it on this system.
        pub fn new(info: NalStreamInfo) -> std::result::Result<Self, String> {
            Self::new_with_node(info, None)
        }

        /// Diagnostic decoder bound to `node`; fails if NV12 LINEAR allocation is unavailable there.
        pub fn new_linear_diagnostic(info: NalStreamInfo, node: DrmNodeId) -> std::result::Result<Self, String> {
            Self::new_with_node(info, Some(node))
        }

        fn new_with_node(info: NalStreamInfo, node: Option<DrmNodeId>) -> std::result::Result<Self, String> {
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
                    let count = pool_size(sps.max_dpb_frames());
                    let session = match node {
                        Some(node) => Session::new_linear_diagnostic(node, profiles, (sps.width(), sps.height()), count, geometry)?,
                        None => Session::new(profiles, (sps.width(), sps.height()), count, geometry)?,
                    };
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
                    let count = pool_size(sps.max_dec_pic_buffering as usize);
                    let size = (sps.width, sps.height);
                    let session = match node {
                        Some(node) => Session::new_linear_diagnostic(node, profiles, size, count, geometry)?,
                        None => Session::new(profiles, size, count, geometry)?,
                    };
                    let vendor = session.vendor().to_string();
                    (
                        Front::Hevc(Box::new(hevc::Front::new(session, info.length_size, size, bits, &info.parameter_sets).map_err(|e| e.to_string())?)),
                        "HEVC",
                        vendor,
                    )
                }
            };
            let name = format!("VA-API {codec} ({vendor})");
            Ok(Self { front, info, name, fed: 0, fail_after: None })
        }

        fn count_decode(&mut self) -> std::result::Result<(), String> {
            self.fed = self.fed.checked_add(1).ok_or("VA-API decode count overflow")?;
            if self.fail_after.is_some_and(|n| self.fed > n) {
                return Err("VA-API decoder failed (test hook)".into());
            }
            Ok(())
        }

        fn decode_resident_into<S: super::ResidentFrameSink>(&mut self, sample: &[u8], pts: i64, sink: &mut S) -> std::result::Result<(), String> {
            self.count_decode()?;
            let (color, par) = (self.info.color, self.info.par);
            match &mut self.front {
                Front::H264(front) => front.decode_resident_into(sample, pts, color, par, sink).map_err(|e| e.to_string()),
                Front::Hevc(front) => front.decode_resident_into(sample, pts, color, par, sink).map_err(|e| e.to_string()),
            }
        }

        fn flush_resident_into<S: super::ResidentFrameSink>(&mut self, sink: &mut S) -> std::result::Result<(), String> {
            let (color, par) = (self.info.color, self.info.par);
            match &mut self.front {
                Front::H264(front) => front.flush_resident_into(color, par, sink).map_err(|e| e.to_string()),
                Front::Hevc(front) => front.flush_resident_into(color, par, sink).map_err(|e| e.to_string()),
            }
        }

        /// Test hook: fail every decode call after the first `n` (a GPU lost mid-stream).
        pub fn fail_after(&mut self, n: u64) {
            self.fail_after = Some(n);
        }
    }

    impl ResidentVaDecoder {
        pub fn new(info: NalStreamInfo) -> std::result::Result<Self, String> {
            Ok(Self { inner: VaDecoder::new(info)? })
        }

        /// Diagnostic resident decoder bound to `node`; never retries another device or layout.
        pub fn new_linear_diagnostic(info: NalStreamInfo, node: DrmNodeId) -> std::result::Result<Self, String> {
            Ok(Self { inner: VaDecoder::new_linear_diagnostic(info, node)? })
        }

        pub fn decode<S: super::ResidentFrameSink>(&mut self, sample: &[u8], pts: i64, sink: &mut S) -> std::result::Result<(), String> {
            self.inner.decode_resident_into(sample, pts, sink)
        }

        pub fn flush<S: super::ResidentFrameSink>(&mut self, sink: &mut S) -> std::result::Result<(), String> {
            self.inner.flush_resident_into(sink)
        }

        pub fn reset(&mut self) {
            self.inner.reset();
        }
    }

    impl VideoDecoder for VaDecoder {
        fn decode(&mut self, sample: &[u8], pts: i64) -> Result<Vec<DecodedFrame>> {
            self.count_decode().map_err(CodecError::Decode)?;
            match &mut self.front {
                Front::H264(f) => f.decode(sample, pts),
                Front::Hevc(f) => f.decode(sample, pts),
            }
        }

        fn flush(&mut self) -> Vec<DecodedFrame> {
            let flushed = match &mut self.front {
                Front::H264(f) => f.flush(),
                Front::Hevc(f) => f.flush(),
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

    #[cfg(test)]
    mod pool_tests {
        use super::*;

        #[test]
        fn three_leases_leave_dpb_and_current_picture_surfaces_free() {
            // DPB of one: its reference and the picture being decoded must stay free
            // while all MAX_IN_FLIGHT_TRANSFERS transfers still hold a surface.
            let free = pool_size(1) - MAX_IN_FLIGHT_TRANSFERS;
            assert!(free >= 2, "pool {} leaves {free} free surfaces with 3 leases", pool_size(1));
            assert_eq!(pool_size(usize::MAX), 18 + MAX_IN_FLIGHT_TRANSFERS);
        }
    }
}
