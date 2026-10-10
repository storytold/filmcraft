//! What a hardware decoder needs to know about a VP9 or AV1 stream so that it behaves exactly like
//! our software decoder for it (the frame-coded counterpart of [`crate::hw::NalStreamInfo`]):
//! codec profile, bit depth and chroma format from the container's `vpcC` / `av1C`, the picture
//! size and colour as the software decoder reports them (read from the bitstream the same way),
//! random-access samples, and in-band changes of the stream's parameters.

use filmcraft_color::{ColorInfo, Primaries, Transfer};
use filmcraft_isobmff::{CodecConfig, SampleEntry};

use crate::video::primaries_from_code;
use crate::{CodecError, Result};

/// The two frame-coded codecs hardware decoders take besides H.264 / HEVC.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FrameCodec {
    Vp9,
    Av1,
}

/// Picture parameters read from a random-access sample.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PictureParams {
    /// Picture size in luma samples (the decoded size).
    pub size: (u32, u32),
    pub bit_depth: u32,
    /// `subsampling_x`, `subsampling_y`.
    pub subsampling: (u8, u8),
    /// Colour of every output picture, as the software decoder reports it.
    pub color: ColorInfo,
}

/// A VP9 or AV1 stream, from its sample entry.
#[derive(Clone, Debug)]
pub struct FrameStreamInfo {
    pub codec: FrameCodec,
    /// VP9 `profile` (0..=3) / AV1 `seq_profile` (0..=2).
    pub profile: u8,
    pub bit_depth: u32,
    pub subsampling: (u8, u8),
    pub mono: bool,
    /// Picture size: the sample entry's, checked against the bitstream's.
    pub size: (u32, u32),
    /// AV1: the `av1C` configuration OBUs (the sequence header the decoder is primed with).
    pub config_obus: Vec<u8>,
    /// AV1: the sequence header's payload; VP9: none.
    sequence_header: Option<Vec<u8>>,
    /// Colour of the stream when it is known before the first picture (AV1: from the sequence
    /// header).
    pub color: Option<ColorInfo>,
    /// VP9: transfer and primaries from the container (not in the bitstream).
    transfer: Option<Transfer>,
    primaries: Option<Primaries>,
}

/// The OBUs of a low-overhead AV1 sample: `(type, payload)`, stopping at the first malformed one.
pub(crate) fn obus(data: &[u8]) -> Vec<(u8, &[u8])> {
    let mut out = Vec::new();
    let mut pos = 0usize;
    while let Some(&h) = data.get(pos) {
        let (obu_type, ext, has_size) = ((h >> 3) & 0xf, h & 4 != 0, h & 2 != 0);
        let mut p = pos + 1 + usize::from(ext);
        let size = if has_size {
            let (mut v, mut shift, mut n) = (0usize, 0u32, 0usize);
            loop {
                let Some(&b) = data.get(p + n) else { return out };
                v |= usize::from(b & 0x7f).checked_shl(shift).unwrap_or(0);
                n += 1;
                if b & 0x80 == 0 || n >= 8 {
                    break;
                }
                shift += 7;
            }
            p += n;
            v
        } else {
            data.len().saturating_sub(p)
        };
        let Some(payload) = data.get(p..p.saturating_add(size)) else { return out };
        out.push((obu_type, payload));
        pos = p + size;
    }
    out
}

/// What an AV1 sequence header fixes about the pictures: profile, maximum size and the colour
/// configuration (bit depth, chroma format, colour description). `None` when it does not parse.
fn av1_format(seq: &[u8]) -> Option<(u8, u32, u32, filmcraft_av1::ColorConfig)> {
    let h = filmcraft_av1::SequenceHeader::parse(seq).ok()?;
    Some((h.profile, h.max_frame_width, h.max_frame_height, h.color))
}

/// VP9 colour as our decoder reports it: `color_space` and range from the bitstream, transfer and
/// primaries from the container when it says. (Shared with `Vp9Decoder`.)
pub(crate) fn vp9_color(width: u32, height: u32, color_space: u8, full_range: bool, transfer: Option<Transfer>, primaries: Option<Primaries>) -> ColorInfo {
    use filmcraft_color::{Matrix, Range};
    let mut color = ColorInfo { matrix: filmcraft_frame::default_matrix(width, height), ..ColorInfo::REC709 };
    // color_space (7.2.2): 1 BT.601, 2 BT.709, 3 SMPTE-170, 4 SMPTE-240, 5 BT.2020, 7 sRGB.
    match color_space {
        1 | 3 => color.matrix = Matrix::Bt601,
        2 | 4 => color.matrix = Matrix::Bt709,
        5 => {
            color.matrix = Matrix::Bt2020Ncl;
            color.primaries = Primaries::Bt2020;
        }
        _ => {}
    }
    if let Some(t) = transfer {
        color.transfer = t;
    }
    if let Some(p) = primaries {
        color.primaries = p;
    }
    if full_range {
        color.range = Range::Full;
    }
    color
}

/// AV1 colour as our decoder reports it. (Shared with `Av1Decoder`.)
pub(crate) fn av1_color(width: u32, height: u32, matrix_coefficients: u8, transfer: u8, primaries: u8, full_range: bool) -> ColorInfo {
    let mut color = ColorInfo::REC709;
    color.matrix = filmcraft_color::Matrix::from_code(matrix_coefficients).unwrap_or_else(|| filmcraft_frame::default_matrix(width, height));
    if let Some(t) = Transfer::from_code(transfer) {
        color.transfer = t;
    }
    color.primaries = match primaries {
        9 => Primaries::Bt2020,
        12 => Primaries::P3D65,
        5 => Primaries::Bt601_625,
        6 => Primaries::Bt601_525,
        _ => Primaries::Bt709,
    };
    if full_range {
        color.range = filmcraft_color::Range::Full;
    }
    color
}

impl FrameStreamInfo {
    /// The stream info of a `vpcC` / `av1C` sample entry (`None` for other codecs).
    pub fn from_entry(e: &SampleEntry) -> Option<Result<Self>> {
        let size = e.video.as_ref().map_or((0, 0), |v| (u32::from(v.width), u32::from(v.height)));
        match &e.codec {
            CodecConfig::Vp9(c) => {
                // vpcC chroma_subsampling: 0 / 1 4:2:0, 2 4:2:2, 3 4:4:4
                let subsampling = match c.chroma_subsampling {
                    0 | 1 => (1, 1),
                    2 => (1, 0),
                    _ => (0, 0),
                };
                Some(Ok(Self {
                    codec: FrameCodec::Vp9,
                    profile: c.profile,
                    bit_depth: u32::from(c.bit_depth),
                    subsampling,
                    mono: false,
                    size,
                    config_obus: Vec::new(),
                    sequence_header: None,
                    color: None,
                    transfer: Transfer::from_code(c.transfer_characteristics),
                    primaries: primaries_from_code(c.colour_primaries),
                }))
            }
            CodecConfig::Av1(c) => Some(Self::from_av1(c, size)),
            _ => None,
        }
    }

    fn from_av1(c: &filmcraft_isobmff::Av1Config, entry_size: (u32, u32)) -> Result<Self> {
        let seq =
            obus(&c.config_obus).into_iter().find(|(t, _)| *t == 1).map(|(_, p)| p).ok_or_else(|| CodecError::Decode("av1C has no sequence header".into()))?;
        let h = filmcraft_av1::SequenceHeader::parse(seq).map_err(|e| CodecError::Decode(e.to_string()))?;
        let size = (h.max_frame_width, h.max_frame_height);
        if entry_size != (0, 0) && entry_size != size {
            // the sample entry and the sequence header disagree: leave the stream to our decoder
            return Err(CodecError::Decode(format!("sample entry says {}x{}, the sequence header {}x{}", entry_size.0, entry_size.1, size.0, size.1)));
        }
        let cc = &h.color;
        Ok(Self {
            codec: FrameCodec::Av1,
            profile: h.profile,
            bit_depth: u32::from(cc.bit_depth),
            subsampling: (cc.subsampling_x, cc.subsampling_y),
            mono: cc.mono_chrome,
            size,
            config_obus: c.config_obus.clone(),
            sequence_header: Some(seq.to_vec()),
            color: Some(av1_color(size.0, size.1, cc.matrix_coefficients, cc.transfer_characteristics, cc.color_primaries, cc.color_range)),
            transfer: None,
            primaries: None,
        })
    }

    /// Whether `sample` has a sequence header OBU of its own (AV1).
    pub fn carries_sequence_header(&self, sample: &[u8]) -> bool {
        self.codec == FrameCodec::Av1 && obus(sample).iter().any(|(t, _)| *t == 1)
    }

    /// A description made by hand (tests of what a backend declines).
    #[doc(hidden)]
    pub fn for_tests(codec: FrameCodec, profile: u8, bit_depth: u32, subsampling: (u8, u8), size: (u32, u32)) -> Self {
        Self {
            codec,
            profile,
            bit_depth,
            subsampling,
            mono: false,
            size,
            config_obus: Vec::new(),
            sequence_header: None,
            color: None,
            transfer: None,
            primaries: None,
        }
    }

    /// Whether decoding can start at `sample` (a VP9 key frame / an AV1 temporal unit whose first
    /// frame is a shown key frame): the software decoders' own test.
    pub fn is_random_access(&self, sample: &[u8]) -> bool {
        match self.codec {
            FrameCodec::Vp9 => filmcraft_vp9::is_keyframe(sample),
            FrameCodec::Av1 => filmcraft_av1::is_key_frame_unit(sample),
        }
    }

    /// The picture parameters a random-access `sample` declares (a VP9 key frame header; an AV1
    /// sample carrying a sequence header), or `None`.
    pub fn picture_params(&self, sample: &[u8]) -> Option<PictureParams> {
        match self.codec {
            FrameCodec::Vp9 => {
                let k = filmcraft_vp9::keyframe_info(sample)?;
                let color = vp9_color(k.width, k.height, k.color.color_space, k.color.full_range, self.transfer, self.primaries);
                Some(PictureParams {
                    size: (k.width, k.height),
                    bit_depth: k.bit_depth,
                    subsampling: (u8::from(k.subsampling_x), u8::from(k.subsampling_y)),
                    color,
                })
            }
            FrameCodec::Av1 => {
                let seq = obus(sample).into_iter().find(|(t, _)| *t == 1).map(|(_, p)| p)?;
                let h = filmcraft_av1::SequenceHeader::parse(seq).ok()?;
                let cc = &h.color;
                let size = (h.max_frame_width, h.max_frame_height);
                let color = av1_color(size.0, size.1, cc.matrix_coefficients, cc.transfer_characteristics, cc.color_primaries, cc.color_range);
                Some(PictureParams { size, bit_depth: u32::from(cc.bit_depth), subsampling: (cc.subsampling_x, cc.subsampling_y), color })
            }
        }
    }

    /// Whether `sample` declares parameters the hardware session was not set up for: an AV1
    /// sequence header whose picture format (profile, size, colour configuration) differs from
    /// `av1C`'s, a VP9 key frame of another size, bit depth or chroma format. The decoder then
    /// hands the stream to our software decoder.
    pub fn parameters_changed(&self, sample: &[u8]) -> bool {
        match self.codec {
            // Only what the picture format depends on counts: encoders such as SVT-AV1 put a
            // provisional sequence header in `av1C` whose coding tool flags (CDEF, restoration,
            // warped motion…) differ from the in-band one, and decoders follow the in-band one.
            FrameCodec::Av1 => obus(sample).into_iter().any(|(t, p)| t == 1 && self.sequence_header.as_deref().is_none_or(|s| av1_format(s) != av1_format(p))),
            FrameCodec::Vp9 => {
                self.picture_params(sample).is_some_and(|p| p.size != self.size || p.bit_depth != self.bit_depth || p.subsampling != self.subsampling)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An `av1C` sequence header from SVT-AV1 (1280x720 10-bit) and the in-band one of the same
    /// stream: they differ only in coding tool flags.
    const AV1C: [u8; 14] = [0x0a, 0x0c, 0x02, 0x00, 0x00, 0x2d, 0x6a, 0x67, 0xfd, 0x9e, 0x01, 0x7c, 0x20, 0x20];
    const IN_BAND: [u8; 14] = [0x0a, 0x0c, 0x02, 0x00, 0x00, 0x2d, 0x6a, 0x67, 0xfd, 0x9e, 0x35, 0x7c, 0xe0, 0x20];

    fn info() -> FrameStreamInfo {
        let c = filmcraft_isobmff::Av1Config {
            seq_profile: 0,
            seq_level_idx_0: 0,
            seq_tier_0: false,
            high_bitdepth: true,
            twelve_bit: false,
            monochrome: false,
            chroma_subsampling_x: true,
            chroma_subsampling_y: true,
            chroma_sample_position: 0,
            initial_presentation_delay_minus_one: None,
            config_obus: AV1C.to_vec(),
        };
        FrameStreamInfo::from_av1(&c, (0, 0)).unwrap()
    }

    #[test]
    fn av1_sequence_headers_compare_by_picture_format() {
        let i = info();
        assert_eq!((i.size, i.bit_depth), ((1280, 720), 10));
        assert!(!i.parameters_changed(&AV1C), "the same header");
        // used to send every SVT-AV1 MP4 to the software decoder from its first frame
        assert!(!i.parameters_changed(&IN_BAND), "only coding tool flags differ");
        // a header that does not parse is a change
        assert!(i.parameters_changed(&[0x0a, 0x02, 0xe0, 0x00]));
        // no sequence header in the sample: nothing changed
        assert!(!i.parameters_changed(&[0x12, 0x00]));
    }
}
