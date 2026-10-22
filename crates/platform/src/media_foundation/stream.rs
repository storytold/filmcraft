//! What differs between the codecs the Windows decoder takes: which streams the GPU's DXVA decoders
//! can do, how the decoder MFT is asked for them, and how a container sample becomes the MFT's
//! input. H.264 / HEVC (`avcC` / `hvcC`) go in as Annex B; VP9 frames and AV1 temporal units go in
//! as they are in the container. Safe code, with the unit tests of what is declined.

use filmcraft_codecs::hw::{FrameCodec, FrameStreamInfo, NalCodec, NalStreamInfo, StreamInfo};
use windows::Win32::Graphics::Direct3D11::{
    D3D11_DECODER_PROFILE_AV1_VLD_PROFILE0, D3D11_DECODER_PROFILE_H264_VLD_NOFGT, D3D11_DECODER_PROFILE_HEVC_VLD_MAIN, D3D11_DECODER_PROFILE_HEVC_VLD_MAIN10,
    D3D11_DECODER_PROFILE_VP9_VLD_10BIT_PROFILE2, D3D11_DECODER_PROFILE_VP9_VLD_PROFILE0,
};
use windows::Win32::Media::MediaFoundation::{MF_MT_MPEG2_PROFILE, MFVideoFormat_AV1, MFVideoFormat_H264, MFVideoFormat_HEVC, MFVideoFormat_VP90};
use windows::core::GUID;

use super::gpu::SurfaceFormat;
use super::mft::{Attr, Spec};
use crate::annexb::to_annex_b;
use crate::biplanar::Geometry;

/// Largest picture the backend takes (as the VideoToolbox one).
pub const MAX_SIDE: u32 = 8192;

/// A stream the decoder takes.
#[derive(Clone, Debug)]
pub enum Stream {
    /// H.264 / HEVC.
    Nal(NalStreamInfo),
    /// VP9 / AV1.
    Frame(FrameStreamInfo),
}

/// What the GPU must decode for a stream.
pub struct Plan {
    pub format: SurfaceFormat,
    pub profile: GUID,
    /// Size DXVA is asked about (the coded size).
    pub size: (u32, u32),
}

impl TryFrom<StreamInfo> for Stream {
    type Error = String;
    /// Validate the shared stream description against the formats accepted by the Media Foundation adapter.
    fn try_from(i: StreamInfo) -> Result<Self, String> {
        match i {
            StreamInfo::Nal(n) => Ok(Self::Nal(n)),
            StreamInfo::Frame(f) => Ok(Self::Frame(f)),
            StreamInfo::Intra => Err("this Media Foundation backend does not decode intra codecs".into()),
        }
    }
}

impl From<NalStreamInfo> for Stream {
    fn from(i: NalStreamInfo) -> Self {
        Self::Nal(i)
    }
}

impl From<FrameStreamInfo> for Stream {
    fn from(i: FrameStreamInfo) -> Self {
        Self::Frame(i)
    }
}

/// What the decoder needs for an `avcC` / `hvcC` stream, or why it is declined.
pub fn plan_nal(info: &NalStreamInfo) -> Result<Plan, String> {
    if info.interlaced {
        return Err("field-coded H.264".into());
    }
    if info.chroma_format_idc != 1 {
        return Err(format!("chroma_format_idc {}", info.chroma_format_idc));
    }
    if info.bit_depth_luma != info.bit_depth_chroma || !matches!(info.bit_depth_luma, 8 | 10) {
        return Err(format!("{}-bit luma / {}-bit chroma", info.bit_depth_luma, info.bit_depth_chroma));
    }
    let (cx, cy, w, h) = info.crop;
    if w == 0 || h == 0 || w > MAX_SIDE || h > MAX_SIDE || cx.saturating_add(w) > info.coded.0 || cy.saturating_add(h) > info.coded.1 {
        return Err(format!("picture size {w}x{h}"));
    }
    let ten = info.bit_depth_luma == 10;
    let (format, profile) = match (info.codec, info.profile_idc, ten) {
        // Baseline, Main and High (the profiles DXVA H.264 decoders list)
        (NalCodec::H264, 66 | 77 | 100, false) => (SurfaceFormat::Nv12, D3D11_DECODER_PROFILE_H264_VLD_NOFGT),
        (NalCodec::H264, p, _) => return Err(format!("H.264 profile {p} at {} bits", info.bit_depth_luma)),
        // Main and Main Still Picture
        (NalCodec::Hevc, 1 | 3, false) => (SurfaceFormat::Nv12, D3D11_DECODER_PROFILE_HEVC_VLD_MAIN),
        (NalCodec::Hevc, 2, true) => (SurfaceFormat::P010, D3D11_DECODER_PROFILE_HEVC_VLD_MAIN10),
        (NalCodec::Hevc, p, _) => return Err(format!("HEVC profile {p} at {} bits", info.bit_depth_luma)),
    };
    Ok(Plan { format, profile, size: info.coded })
}

/// What the decoder needs for a VP9 / AV1 stream, or why it is declined: 4:2:0 only, 8-bit
/// (NV12) or 10-bit (P010), the profiles DXVA lists (VP9 profile 0 and 2, AV1 main profile).
pub fn plan_frame(info: &FrameStreamInfo) -> Result<Plan, String> {
    let (w, h) = info.size;
    if w == 0 || h == 0 || w > MAX_SIDE || h > MAX_SIDE {
        return Err(format!("picture size {w}x{h}"));
    }
    if info.mono || info.subsampling != (1, 1) {
        return Err(format!("chroma subsampling {:?}{}", info.subsampling, if info.mono { " (monochrome)" } else { "" }));
    }
    let (format, profile) = match (info.codec, info.profile, info.bit_depth) {
        (FrameCodec::Vp9, 0, 8) => (SurfaceFormat::Nv12, D3D11_DECODER_PROFILE_VP9_VLD_PROFILE0),
        (FrameCodec::Vp9, 2, 10) => (SurfaceFormat::P010, D3D11_DECODER_PROFILE_VP9_VLD_10BIT_PROFILE2),
        (FrameCodec::Vp9, p, b) => return Err(format!("VP9 profile {p} at {b} bits")),
        (FrameCodec::Av1, 0, 8) => (SurfaceFormat::Nv12, D3D11_DECODER_PROFILE_AV1_VLD_PROFILE0),
        (FrameCodec::Av1, 0, 10) => (SurfaceFormat::P010, D3D11_DECODER_PROFILE_AV1_VLD_PROFILE0),
        (FrameCodec::Av1, p, b) => return Err(format!("AV1 profile {p} at {b} bits")),
    };
    Ok(Plan { format, profile, size: info.size })
}

impl Stream {
    pub fn plan(&self) -> Result<Plan, String> {
        match self {
            Self::Nal(i) => plan_nal(i),
            Self::Frame(i) => plan_frame(i),
        }
    }

    /// The hybrid decoder's view of the stream.
    pub fn info(&self) -> StreamInfo {
        match self {
            Self::Nal(i) => i.clone().into(),
            Self::Frame(i) => i.clone().into(),
        }
    }

    pub fn name(&self) -> &'static str {
        match self {
            Self::Nal(i) if i.codec == NalCodec::H264 => "Media Foundation H.264",
            Self::Nal(_) => "Media Foundation HEVC",
            Self::Frame(i) if i.codec == FrameCodec::Vp9 => "Media Foundation VP9",
            Self::Frame(_) => "Media Foundation AV1",
        }
    }

    /// The decoder MFT request.
    pub fn spec(&self, format: SurfaceFormat) -> Spec {
        match self {
            Self::Nal(info) => {
                let (_, _, w, h) = info.crop;
                match info.codec {
                    NalCodec::H264 => Spec { subtype: MFVideoFormat_H264, label: "H.264", missing: "", size: (w, h), format, attrs: Vec::new() },
                    NalCodec::Hevc => Spec {
                        subtype: MFVideoFormat_HEVC,
                        label: "HEVC",
                        missing: " (the HEVC Video Extensions are not installed)",
                        size: (w, h),
                        format,
                        // the HEVC decoder lists P010 output only when told the stream is Main 10
                        // (eAVEncH265VProfile_Main_420_8 = 1, eAVEncH265VProfile_Main_420_10 = 2)
                        attrs: vec![(MF_MT_MPEG2_PROFILE, Attr::U32(if format == SurfaceFormat::P010 { 2 } else { 1 }))],
                    },
                }
            }
            Self::Frame(info) => match info.codec {
                FrameCodec::Vp9 => Spec {
                    subtype: MFVideoFormat_VP90,
                    label: "VP9",
                    missing: " (the VP9 Video Extensions are not installed)",
                    size: info.size,
                    format,
                    attrs: Vec::new(),
                },
                FrameCodec::Av1 => Spec {
                    subtype: MFVideoFormat_AV1,
                    label: "AV1",
                    missing: " (the AV1 Video Extension is not installed)",
                    size: info.size,
                    format,
                    attrs: Vec::new(),
                },
            },
        }
    }

    /// The geometry of the first picture: the stream's own (H.264 / HEVC), or the expected one
    /// until the first VP9 key frame says (colour and size come from its header).
    pub fn first_geometry(&self, format: SurfaceFormat) -> Geometry {
        match self {
            Self::Nal(i) => Geometry { crop: i.crop, bits: format.bits(), color: i.color, par: i.par },
            Self::Frame(i) => {
                Geometry { crop: (0, 0, i.size.0, i.size.1), bits: format.bits(), color: i.color.unwrap_or(filmcraft_color::ColorInfo::REC709), par: (1, 1) }
            }
        }
    }

    /// The even-rectangle test sharing a surface needs (see `MfDecoder::can_share`).
    pub fn picture_is_even(&self, g: &Geometry) -> bool {
        let (x, y, w, h) = g.crop;
        [x, y, w, h].iter().all(|v| v % 2 == 0)
    }

    /// Whether the decoder can start (again) at `sample` after a drain: an IDR picture, a key frame.
    pub fn is_restart(&self, sample: &[u8]) -> bool {
        match self {
            Self::Nal(info) => info.nal_types(sample).iter().any(|&t| match info.codec {
                NalCodec::H264 => t == 5,
                NalCodec::Hevc => matches!(t, 19 | 20),
            }),
            Self::Frame(i) => i.is_random_access(sample),
        }
    }

    /// `sample` as the MFT's input. With `headers` the stream's parameter sets / sequence header go
    /// first (unless the sample carries them): a decoder starting at this sample has what it needs.
    pub fn input(&self, sample: &[u8], headers: bool) -> Result<Vec<u8>, String> {
        match self {
            Self::Nal(info) => to_annex_b(info, sample, headers),
            Self::Frame(i) => {
                if sample.is_empty() {
                    return Err("empty sample".into());
                }
                if headers && !i.config_obus.is_empty() && !i.carries_sequence_header(sample) {
                    let mut v = Vec::with_capacity(i.config_obus.len() + sample.len());
                    v.extend_from_slice(&i.config_obus);
                    v.extend_from_slice(sample);
                    return Ok(v);
                }
                Ok(sample.to_vec())
            }
        }
    }

    /// Picture format a VP9 key frame declares (size and colour); `None` for other samples and codecs.
    pub fn declared_picture(&self, sample: &[u8]) -> Option<Geometry> {
        let Self::Frame(i) = self else { return None };
        if i.codec != FrameCodec::Vp9 {
            return None;
        }
        let p = i.picture_params(sample)?;
        Some(Geometry { crop: (0, 0, p.size.0, p.size.1), bits: p.bit_depth, color: p.color, par: (1, 1) })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn nal(codec: NalCodec, profile_idc: u8, bits: u32, chroma_format_idc: u32) -> NalStreamInfo {
        NalStreamInfo {
            codec,
            length_size: 4,
            highest_tid: None,
            parameter_sets: Vec::new(),
            coded: (1920, 1088),
            crop: (0, 0, 1920, 1080),
            chroma_format_idc,
            bit_depth_luma: bits,
            bit_depth_chroma: bits,
            interlaced: false,
            profile_idc,
            color: filmcraft_color::ColorInfo::REC709,
            par: (1, 1),
            reorder: 2,
        }
    }

    fn frame(codec: FrameCodec, profile: u8, bits: u32, subsampling: (u8, u8)) -> FrameStreamInfo {
        FrameStreamInfo::for_tests(codec, profile, bits, subsampling, (1920, 1080))
    }

    #[test]
    fn takes_h264_baseline_main_high_and_hevc_main_main10() {
        for p in [66, 77, 100] {
            assert_eq!(plan_nal(&nal(NalCodec::H264, p, 8, 1)).unwrap().format, SurfaceFormat::Nv12, "H.264 profile {p}");
        }
        let main = plan_nal(&nal(NalCodec::Hevc, 1, 8, 1)).unwrap();
        assert_eq!((main.format, main.profile), (SurfaceFormat::Nv12, D3D11_DECODER_PROFILE_HEVC_VLD_MAIN));
        let main10 = plan_nal(&nal(NalCodec::Hevc, 2, 10, 1)).unwrap();
        assert_eq!((main10.format, main10.profile), (SurfaceFormat::P010, D3D11_DECODER_PROFILE_HEVC_VLD_MAIN10));
    }

    #[test]
    fn declines_what_dxva_does_not_decode() {
        // Hi10, High 4:2:2 / 4:4:4, Extended; 10-bit under an 8-bit profile; 4:2:2 / 4:4:4 / mono HEVC
        for (codec, p, bits, chroma) in [
            (NalCodec::H264, 110, 10, 1),
            (NalCodec::H264, 122, 10, 2),
            (NalCodec::H264, 244, 8, 3),
            (NalCodec::H264, 88, 8, 1),
            (NalCodec::H264, 100, 10, 1),
            (NalCodec::H264, 100, 8, 2),
            (NalCodec::Hevc, 4, 10, 2),
            (NalCodec::Hevc, 4, 12, 1),
            (NalCodec::Hevc, 1, 10, 1),
            (NalCodec::Hevc, 2, 8, 1),
            (NalCodec::Hevc, 1, 8, 0),
            (NalCodec::Hevc, 1, 8, 3),
        ] {
            assert!(plan_nal(&nal(codec, p, bits, chroma)).is_err(), "{codec:?} profile {p} {bits}-bit chroma {chroma}");
        }
        let mut field_coded = nal(NalCodec::H264, 100, 8, 1);
        field_coded.interlaced = true;
        assert!(plan_nal(&field_coded).is_err());
        // luma and chroma depths that differ
        let mut mixed = nal(NalCodec::Hevc, 2, 10, 1);
        mixed.bit_depth_chroma = 8;
        assert!(plan_nal(&mixed).is_err());
    }

    #[test]
    fn declines_absurd_pictures() {
        for crop in [(0, 0, 0, 1080), (0, 0, 1920, 0), (0, 0, 9000, 1080), (0, 0, 1080, 9000), (8, 0, 1920, 1080), (0, 16, 1920, 1080), (u32::MAX, 0, 16, 16)] {
            let mut i = nal(NalCodec::H264, 100, 8, 1);
            i.crop = crop;
            assert!(plan_nal(&i).is_err(), "{crop:?}");
        }
    }

    #[test]
    fn takes_vp9_profiles_0_and_2_and_av1_main_in_420() {
        let a = plan_frame(&frame(FrameCodec::Vp9, 0, 8, (1, 1))).unwrap();
        assert_eq!((a.format, a.profile), (SurfaceFormat::Nv12, D3D11_DECODER_PROFILE_VP9_VLD_PROFILE0));
        let b = plan_frame(&frame(FrameCodec::Vp9, 2, 10, (1, 1))).unwrap();
        assert_eq!((b.format, b.profile), (SurfaceFormat::P010, D3D11_DECODER_PROFILE_VP9_VLD_10BIT_PROFILE2));
        let c = plan_frame(&frame(FrameCodec::Av1, 0, 8, (1, 1))).unwrap();
        assert_eq!((c.format, c.profile), (SurfaceFormat::Nv12, D3D11_DECODER_PROFILE_AV1_VLD_PROFILE0));
        assert_eq!(plan_frame(&frame(FrameCodec::Av1, 0, 10, (1, 1))).unwrap().format, SurfaceFormat::P010);
    }

    #[test]
    fn declines_the_rest_of_vp9_and_av1() {
        for (codec, p, bits, ss) in [
            (FrameCodec::Vp9, 1, 8, (0, 0)),
            (FrameCodec::Vp9, 1, 8, (1, 0)),
            (FrameCodec::Vp9, 3, 10, (0, 0)),
            (FrameCodec::Vp9, 2, 12, (1, 1)),
            (FrameCodec::Vp9, 0, 10, (1, 1)),
            (FrameCodec::Vp9, 2, 8, (1, 1)),
            (FrameCodec::Av1, 1, 8, (0, 0)),
            (FrameCodec::Av1, 2, 12, (1, 0)),
            (FrameCodec::Av1, 0, 12, (1, 1)),
            (FrameCodec::Av1, 0, 8, (1, 0)),
        ] {
            assert!(plan_frame(&frame(codec, p, bits, ss)).is_err(), "{codec:?} profile {p} {bits}-bit {ss:?}");
        }
        let mut mono = frame(FrameCodec::Av1, 0, 8, (1, 1));
        mono.mono = true;
        assert!(plan_frame(&mono).is_err());
        for size in [(0, 1080), (1920, 0), (9000, 1080), (1920, 9000)] {
            let mut i = frame(FrameCodec::Vp9, 0, 8, (1, 1));
            i.size = size;
            assert!(plan_frame(&i).is_err(), "{size:?}");
        }
    }
}
