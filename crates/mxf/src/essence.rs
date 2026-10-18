//! Essence identification and PCM sound decoding.
//!
//! Picture codecs are identified from the descriptor's picture essence coding label, then the
//! essence container label (ST 379-2 mapping numbers), then the first bytes of the essence.

use crate::klv::Ul;
use crate::{Rational, Result};

/// The essence codec of a track.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Codec {
    /// H.264 / AVC byte stream (ST 381-3). `intra`: AVC-Intra / an intra-only profile.
    Avc {
        intra: bool,
    },
    /// SMPTE VC-3 (DNxHD / DNxHR, ST 2019-4).
    Vc3,
    /// Apple ProRes (RDD 44). `profile`: byte 14 of the coding label (1 Proxy … 6 4444 XQ).
    ProRes {
        profile: Option<u8>,
    },
    /// MPEG-2 video (ST 381-1, or D-10 / IMX).
    Mpeg2,
    /// MPEG-4 part 2 visual.
    Mpeg4Visual,
    Jpeg2000,
    Dv,
    /// Uncompressed pictures (ST 384).
    Uncompressed,
    /// Linear PCM, little-endian (Broadcast Wave and AES3 descriptors, ST 382).
    Pcm,
    /// 8-channel AES3 element data (ST 331, D-10 sound).
    Aes3Element,
    Unknown,
}

impl Codec {
    pub fn name(&self) -> &'static str {
        match self {
            Codec::Avc { intra: true } => "AVC-Intra",
            Codec::Avc { intra: false } => "H.264",
            Codec::Vc3 => "VC-3",
            Codec::ProRes { .. } => "Apple ProRes",
            Codec::Mpeg2 => "MPEG-2 Video",
            Codec::Mpeg4Visual => "MPEG-4 Visual",
            Codec::Jpeg2000 => "JPEG 2000",
            Codec::Dv => "DV",
            Codec::Uncompressed => "Uncompressed",
            Codec::Pcm => "PCM",
            Codec::Aes3Element => "AES3 PCM",
            Codec::Unknown => "unknown",
        }
    }
    /// Every picture is coded independently.
    pub fn intra_only(&self) -> bool {
        matches!(self, Codec::Avc { intra: true } | Codec::Vc3 | Codec::ProRes { .. } | Codec::Jpeg2000 | Codec::Dv | Codec::Uncompressed)
    }
}

/// Identify a picture codec from the picture essence coding label and the essence container label.
pub fn identify_picture(coding: Option<Ul>, container: Option<Ul>, mpeg_descriptor: bool) -> Codec {
    if let Some(c) = coding.filter(Ul::is_smpte) {
        let b = &c.0;
        // 04 01 02 02 .. : compressed picture coding
        if b[8..12] == [0x04, 0x01, 0x02, 0x02] {
            match (b[12], b[13]) {
                // 01: MPEG compression (ISO/IEC 13818-2 profiles, 14496-2, 14496-10)
                (0x01, 0x01..=0x1F) => return Codec::Mpeg2,
                (0x01, 0x20..=0x2F) => return Codec::Mpeg4Visual,
                (0x01, 0x32) => return Codec::Avc { intra: true },
                (0x01, 0x30..=0x3F) => return Codec::Avc { intra: false },
                (0x02, _) => return Codec::Dv,
                (0x03, 0x01) => return Codec::Jpeg2000,
                (0x03, 0x02) => return Codec::Vc3,
                (0x03, 0x06) => return Codec::ProRes { profile: Some(b[14]) },
                // Avid-registered VC-3 coding labels
                (0x71, _) => return Codec::Vc3,
                _ => {}
            }
        }
        if b[8..11] == [0x04, 0x01, 0x02] && b[11] == 0x01 {
            return Codec::Uncompressed;
        }
    }
    if let Some(c) = container.filter(Ul::is_smpte)
        && c.item_starts_with(&[0x0D, 0x01, 0x03, 0x01, 0x02])
    {
        match c.0[13] {
            0x01 => return Codec::Mpeg2,
            0x02 => return Codec::Dv,
            0x04 if mpeg_descriptor => return Codec::Mpeg2,
            0x05 => return Codec::Uncompressed,
            0x0C => return Codec::Jpeg2000,
            0x0F | 0x10 => return Codec::Avc { intra: false },
            0x11 => return Codec::Vc3,
            0x1C => return Codec::ProRes { profile: None },
            _ => {}
        }
    }
    if mpeg_descriptor { Codec::Mpeg2 } else { Codec::Unknown }
}

/// Identify a picture codec from the first bytes of an essence element.
pub fn sniff_picture(b: &[u8]) -> Option<Codec> {
    if b.len() >= 8 && &b[4..8] == b"icpf" {
        return Some(Codec::ProRes { profile: None });
    }
    if b.len() >= 5 && b[..4] == [0x00, 0x00, 0x02, 0x80] {
        return Some(Codec::Vc3);
    }
    if b.len() >= 4 && b[..4] == [0x00, 0x00, 0x01, 0xB3] {
        return Some(Codec::Mpeg2);
    }
    let sc4 = b.len() >= 5 && b[..4] == [0, 0, 0, 1];
    let sc3 = b.len() >= 4 && b[..3] == [0, 0, 1];
    if sc4 || sc3 {
        let nal = b[if sc4 { 4 } else { 3 }];
        if nal & 0x80 == 0 && matches!(nal & 0x1F, 1 | 5 | 6 | 7 | 8 | 9) {
            return Some(Codec::Avc { intra: false });
        }
    }
    None
}

/// Picture descriptor properties (ST 377-1 generic picture / CDCI / RGBA descriptors).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PictureInfo {
    pub stored_width: u32,
    pub stored_height: u32,
    pub display_width: u32,
    pub display_height: u32,
    /// 0 full frame, 1 separate fields, 2 single field, 3 mixed fields, 4 segmented frame.
    pub frame_layout: u8,
    /// Display aspect ratio of the whole image.
    pub aspect_ratio: Rational,
    pub component_depth: u32,
    pub horizontal_subsampling: u32,
    pub vertical_subsampling: u32,
    pub black_ref_level: Option<u32>,
    pub white_ref_level: Option<u32>,
    pub alpha_depth: u32,
    /// RGBA descriptor (pixel layout present).
    pub rgba: bool,
    pub picture_coding: Option<Ul>,
    pub transfer_characteristic: Option<Ul>,
    pub coding_equations: Option<Ul>,
    pub color_primaries: Option<Ul>,
}

impl PictureInfo {
    /// Frame height: field-based layouts code the height of one field.
    pub fn frame_height(&self) -> u32 {
        let h = if self.display_height > 0 { self.display_height } else { self.stored_height };
        if matches!(self.frame_layout, 1 | 3) { h * 2 } else { h }
    }
    pub fn frame_width(&self) -> u32 {
        if self.display_width > 0 { self.display_width } else { self.stored_width }
    }
    /// Full-range coding (black at 0 and white at the maximum code value).
    pub fn full_range(&self) -> bool {
        let d = self.component_depth;
        d > 0 && self.black_ref_level == Some(0) && self.white_ref_level == Some((1u32 << d.min(31)) - 1)
    }
}

/// PCM layout of a sound track.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SoundFormat {
    /// Interleaved little-endian samples (`bits` per sample, `block_align` bytes per frame).
    Pcm,
    /// ST 331 8-channel AES3 element data (4-byte header, 8 × 32-bit sub-frames per sample).
    Aes3Element,
    /// A coding we do not decode.
    Other,
}

/// Sound descriptor properties (ST 377-1 generic sound descriptor, ST 382 wave / AES3).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SoundInfo {
    pub sample_rate: Rational,
    pub channels: u32,
    pub bits: u32,
    pub block_align: u32,
    pub locked: Option<bool>,
    pub sound_coding: Option<Ul>,
}

impl SoundInfo {
    /// Bytes per sample frame of plain PCM.
    pub fn frame_bytes(&self) -> usize {
        if self.block_align > 0 { self.block_align as usize } else { (self.channels.max(1) as usize).saturating_mul((self.bits as usize).div_ceil(8).max(1)) }
    }
}

/// Number of sample frames in a ST 331 AES3 element (from its header; 0 if too short).
pub fn aes3_element_samples(v: &[u8]) -> usize {
    if v.len() < 4 {
        return 0;
    }
    let declared = u16::from_le_bytes([v[1], v[2]]) as usize;
    declared.min((v.len() - 4) / 32)
}

/// Decode PCM chunk bytes into planar f32 (`channels` channels).
///
/// `Pcm`: interleaved little-endian signed integers of `bits` (8-bit is unsigned, per WAVE),
/// `frame_bytes` per sample frame. `Aes3Element`: the 24-bit sample of each ST 331 sub-frame
/// (bits 4-27 of the little-endian 32-bit word), first `channels` of the 8 channels.
pub fn decode_pcm(v: &[u8], format: SoundFormat, channels: usize, bits: u32, frame_bytes: usize) -> Result<Vec<Vec<f32>>> {
    let ch = channels.max(1);
    if ch > 256 {
        return Err(crate::Error::Invalid("PCM channel count exceeds 256".into()));
    }
    match format {
        SoundFormat::Pcm => {
            let bps = (bits as usize).div_ceil(8);
            if bps == 0 || bps > 4 || frame_bytes < bps * ch {
                return Err(crate::Error::Unsupported(format!("{bits}-bit PCM with {frame_bytes}-byte frames")));
            }
            // `frame_bytes >= bps * ch`, so the output is bounded by the bytes actually supplied.
            let n = v.len() / frame_bytes;
            let mut out = vec![Vec::with_capacity(n); ch];
            let scale = 1.0 / (1u64 << (8 * bps - 1)) as f32;
            for i in 0..n {
                let f = &v[i * frame_bytes..];
                for (c, o) in out.iter_mut().enumerate() {
                    let s = &f[c * bps..c * bps + bps];
                    let x = match bps {
                        1 => (s[0] as i32 - 128) << 24,
                        2 => i32::from_le_bytes([0, 0, s[0], s[1]]),
                        3 => i32::from_le_bytes([0, s[0], s[1], s[2]]),
                        _ => i32::from_le_bytes([s[0], s[1], s[2], s[3]]),
                    };
                    o.push((x >> (32 - 8 * bps)) as f32 * scale);
                }
            }
            Ok(out)
        }
        SoundFormat::Aes3Element => {
            // SMPTE 331M: an AES3 element carries at most eight channels (and an output buffer per
            // declared channel would multiply a hostile header's allocation).
            if ch > 8 {
                return Err(crate::Error::Invalid("an AES3 element has at most eight channels".into()));
            }
            let n = aes3_element_samples(v);
            let mut out = vec![Vec::with_capacity(n); ch];
            for i in 0..n {
                for (c, o) in out.iter_mut().enumerate().take(8) {
                    let p = 4 + (i * 8 + c) * 4;
                    let w = u32::from_le_bytes([v[p], v[p + 1], v[p + 2], v[p + 3]]);
                    let s = (((w >> 4) & 0xFF_FFFF) << 8) as i32 >> 8;
                    o.push(s as f32 / 8_388_608.0);
                }
            }
            Ok(out)
        }
        SoundFormat::Other => Err(crate::Error::Unsupported("sound coding".into())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ul(item: &[u8]) -> Ul {
        let mut b = [0x06, 0x0E, 0x2B, 0x34, 0x04, 0x01, 0x01, 0x0A, 0, 0, 0, 0, 0, 0, 0, 0];
        b[8..8 + item.len()].copy_from_slice(item);
        Ul(b)
    }

    #[test]
    fn identifies_picture_codings() {
        assert_eq!(identify_picture(Some(ul(&[4, 1, 2, 2, 1, 0x31, 0x40, 1])), None, false), Codec::Avc { intra: false });
        assert_eq!(identify_picture(Some(ul(&[4, 1, 2, 2, 1, 0x32, 0x21, 1])), None, false), Codec::Avc { intra: true });
        assert_eq!(identify_picture(Some(ul(&[4, 1, 2, 2, 0x71, 0x11])), None, false), Codec::Vc3);
        assert_eq!(identify_picture(Some(ul(&[4, 1, 2, 2, 3, 6, 4])), None, false), Codec::ProRes { profile: Some(4) });
        assert_eq!(identify_picture(Some(ul(&[4, 1, 2, 2, 1, 1, 0x11])), None, true), Codec::Mpeg2);
        // container fallback
        assert_eq!(identify_picture(None, Some(ul(&[0x0D, 1, 3, 1, 2, 0x11, 1])), false), Codec::Vc3);
        assert_eq!(identify_picture(None, Some(ul(&[0x0D, 1, 3, 1, 2, 0x04, 0x60, 1])), true), Codec::Mpeg2);
        assert_eq!(identify_picture(None, None, false), Codec::Unknown);
    }

    #[test]
    fn sniffs_essence() {
        assert_eq!(sniff_picture(&[0, 0, 0, 1, 0x09, 0x10]), Some(Codec::Avc { intra: false }));
        assert_eq!(sniff_picture(&[0, 0, 1, 0xB3, 0]), Some(Codec::Mpeg2));
        assert_eq!(sniff_picture(&[0, 0, 2, 0x80, 1]), Some(Codec::Vc3));
        assert_eq!(sniff_picture(&[0, 0, 0, 9, b'i', b'c', b'p', b'f']), Some(Codec::ProRes { profile: None }));
        assert_eq!(sniff_picture(&[1, 2, 3]), None);
    }

    #[test]
    fn decodes_pcm_and_aes3() {
        let v = [0x00, 0x80, 0xFF, 0x7F, 0x01, 0x00, 0x00, 0x00];
        let p = decode_pcm(&v, SoundFormat::Pcm, 2, 16, 4).unwrap();
        assert_eq!(p[0], vec![-1.0, 1.0 / 32768.0]);
        assert_eq!(p[1], vec![32767.0 / 32768.0, 0.0]);
        let v24 = [0x00, 0x00, 0x80, 0xFF, 0xFF, 0x7F];
        let p = decode_pcm(&v24, SoundFormat::Pcm, 1, 24, 3).unwrap();
        assert_eq!(p[0], vec![-1.0, 8_388_607.0 / 8_388_608.0]);
        // one AES3 element: 1 sample, channel 0 = -1, channel 1 = 0.5
        let mut e = vec![0x00, 1, 0, 0xFF];
        for c in 0..8u32 {
            let s: i32 = match c {
                0 => -8_388_608,
                1 => 4_194_304,
                _ => 0,
            };
            let w = (((s as u32) & 0xFF_FFFF) << 4) | c;
            e.extend_from_slice(&w.to_le_bytes());
        }
        assert_eq!(aes3_element_samples(&e), 1);
        let p = decode_pcm(&e, SoundFormat::Aes3Element, 2, 24, 0).unwrap();
        assert_eq!(p, vec![vec![-1.0], vec![0.5]]);
        // truncated: no panic, no samples
        assert!(decode_pcm(&e[..10], SoundFormat::Aes3Element, 2, 24, 0).unwrap()[0].is_empty());
    }
}
