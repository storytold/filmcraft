//! Opus identification header (`OpusHead`, RFC 7845 §5.1) and the ISO-BMFF `dOps` box payload.

use crate::{Error, Result};

/// Decoder configuration: channel layout, multistream mapping, pre-skip and output gain.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OpusHead {
    /// Output channel count (1..=255).
    pub channels: usize,
    /// Samples (at 48 kHz) to discard from the start of the decoded stream.
    pub pre_skip: u16,
    /// Sample rate of the original input (informational only).
    pub input_sample_rate: u32,
    /// Output gain in Q7.8 dB.
    pub output_gain: i16,
    /// Channel mapping family (0: mono/stereo, 1: Vorbis order surround, 255: unspecified order).
    pub mapping_family: u8,
    /// Number of elementary streams.
    pub streams: usize,
    /// Number of coupled (stereo) streams; these come first.
    pub coupled_streams: usize,
    /// Output channel → decoded channel index (255 = silence).
    pub mapping: Vec<u8>,
}

impl OpusHead {
    /// A family-0 configuration for plain mono/stereo streams (no header present).
    pub fn simple(channels: usize) -> Result<OpusHead> {
        if !(1..=2).contains(&channels) {
            return Err(Error::InvalidArgument("family 0 supports 1 or 2 channels"));
        }
        Ok(OpusHead {
            channels,
            pre_skip: 0,
            input_sample_rate: 48000,
            output_gain: 0,
            mapping_family: 0,
            streams: 1,
            coupled_streams: channels - 1,
            mapping: (0..channels as u8).collect(),
        })
    }

    /// Parses an `OpusHead` packet (Ogg first packet, Matroska `CodecPrivate`).
    pub fn parse(data: &[u8]) -> Result<OpusHead> {
        if data.len() < 19 || &data[..8] != b"OpusHead" {
            return Err(Error::InvalidHeader("missing OpusHead magic"));
        }
        let version = data[8];
        if version >> 4 != 0 {
            return Err(Error::Unsupported("OpusHead major version"));
        }
        let channels = data[9] as usize;
        let pre_skip = u16::from_le_bytes([data[10], data[11]]);
        let input_sample_rate = u32::from_le_bytes([data[12], data[13], data[14], data[15]]);
        let output_gain = i16::from_le_bytes([data[16], data[17]]);
        let family = data[18];
        Self::build(channels, pre_skip, input_sample_rate, output_gain, family, &data[19..])
    }

    /// Parses the payload of an ISO-BMFF `dOps` box (Opus in MP4, big-endian fields).
    pub fn from_dops(data: &[u8]) -> Result<OpusHead> {
        if data.len() < 11 {
            return Err(Error::InvalidHeader("dOps too short"));
        }
        if data[0] != 0 {
            return Err(Error::Unsupported("dOps version"));
        }
        let channels = data[1] as usize;
        let pre_skip = u16::from_be_bytes([data[2], data[3]]);
        let input_sample_rate = u32::from_be_bytes([data[4], data[5], data[6], data[7]]);
        let output_gain = i16::from_be_bytes([data[8], data[9]]);
        Self::build(channels, pre_skip, input_sample_rate, output_gain, data[10], &data[11..])
    }

    /// Serialises as an `OpusHead` packet.
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut v = b"OpusHead".to_vec();
        v.push(1);
        v.push(self.channels as u8);
        v.extend(self.pre_skip.to_le_bytes());
        v.extend(self.input_sample_rate.to_le_bytes());
        v.extend(self.output_gain.to_le_bytes());
        v.push(self.mapping_family);
        if self.mapping_family != 0 {
            v.push(self.streams as u8);
            v.push(self.coupled_streams as u8);
            v.extend(&self.mapping);
        }
        v
    }

    /// Validate a header independently of its wire parser. This is required for callers of
    /// [`crate::Decoder::from_head`], which may construct an `OpusHead` directly with `usize`
    /// values that cannot occur in the byte format.
    pub fn validate(&self) -> Result<()> {
        if !(1..=255).contains(&self.channels) || self.mapping.len() != self.channels {
            return Err(Error::InvalidHeader("bad channel count or mapping length"));
        }
        if self.mapping_family == 0 {
            if self.channels > 2 || self.streams != 1 || self.coupled_streams != self.channels - 1 || self.mapping.iter().copied().ne(0..self.channels as u8) {
                return Err(Error::InvalidHeader("invalid family 0 mapping"));
            }
            return Ok(());
        }
        if self.mapping_family == 3 {
            return Err(Error::Unsupported("channel mapping family 3 (projection)"));
        }
        if self.streams == 0
            || self.streams > 255
            || self.coupled_streams > self.streams
            || self.streams.checked_add(self.coupled_streams).is_none_or(|n| n > 255)
        {
            return Err(Error::InvalidHeader("bad stream counts"));
        }
        if self.mapping_family == 1 && !(1..=8).contains(&self.channels) {
            return Err(Error::InvalidHeader("family 1 allows 1 to 8 channels"));
        }
        if self.mapping.iter().any(|&m| m != 255 && m as usize >= self.streams + self.coupled_streams) {
            return Err(Error::InvalidHeader("channel mapping out of range"));
        }
        Ok(())
    }

    fn build(channels: usize, pre_skip: u16, input_sample_rate: u32, output_gain: i16, family: u8, rest: &[u8]) -> Result<OpusHead> {
        if channels == 0 {
            return Err(Error::InvalidHeader("zero channels"));
        }
        let (streams, coupled, mapping) = if family == 0 {
            if channels > 2 {
                return Err(Error::InvalidHeader("family 0 allows at most 2 channels"));
            }
            (1, channels - 1, (0..channels as u8).collect::<Vec<_>>())
        } else {
            if family == 3 {
                return Err(Error::Unsupported("channel mapping family 3 (projection)"));
            }
            if rest.len() < 2 + channels {
                return Err(Error::InvalidHeader("truncated channel mapping table"));
            }
            let streams = rest[0] as usize;
            let coupled = rest[1] as usize;
            if streams == 0 || coupled > streams || streams + coupled > 255 {
                return Err(Error::InvalidHeader("bad stream counts"));
            }
            if family == 1 && !(1..=8).contains(&channels) {
                return Err(Error::InvalidHeader("family 1 allows 1 to 8 channels"));
            }
            let mapping = rest[2..2 + channels].to_vec();
            if mapping.iter().any(|&m| m != 255 && m as usize >= streams + coupled) {
                return Err(Error::InvalidHeader("channel mapping out of range"));
            }
            (streams, coupled, mapping)
        };
        let head = OpusHead { channels, pre_skip, input_sample_rate, output_gain, mapping_family: family, streams, coupled_streams: coupled, mapping };
        head.validate()?;
        Ok(head)
    }

    /// Linear output gain factor.
    pub fn gain_linear(&self) -> f32 {
        10f32.powf(self.output_gain as f32 / (20.0 * 256.0))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_family0_and_roundtrip() {
        let mut h = b"OpusHead".to_vec();
        h.extend([1, 2, 0x38, 0x01, 0x80, 0xBB, 0, 0, 0x00, 0x01, 0]);
        let head = OpusHead::parse(&h).unwrap();
        assert_eq!(head.channels, 2);
        assert_eq!(head.pre_skip, 312);
        assert_eq!(head.input_sample_rate, 48000);
        assert_eq!(head.output_gain, 256);
        assert_eq!((head.streams, head.coupled_streams), (1, 1));
        assert_eq!(head.to_bytes(), h);
        assert!((head.gain_linear() - 1.122).abs() < 1e-3);
    }

    #[test]
    fn parse_family1_51() {
        let mut h = b"OpusHead".to_vec();
        h.extend([1, 6, 0x38, 0x01, 0x80, 0xBB, 0, 0, 0, 0, 1, 4, 2, 0, 4, 1, 2, 3, 5]);
        let head = OpusHead::parse(&h).unwrap();
        assert_eq!((head.channels, head.streams, head.coupled_streams), (6, 4, 2));
        assert_eq!(head.mapping, vec![0, 4, 1, 2, 3, 5]);
        assert_eq!(OpusHead::parse(&head.to_bytes()).unwrap(), head);
    }

    #[test]
    fn rejects_garbage() {
        assert!(OpusHead::parse(b"OpusHea").is_err());
        assert!(OpusHead::parse(b"OpusTags...........").is_err());
        let mut h = b"OpusHead".to_vec();
        h.extend([1, 3, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
        assert!(OpusHead::parse(&h).is_err());
        let mut h = b"OpusHead".to_vec();
        h.extend([1, 2, 0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 0, 0, 5]);
        assert!(OpusHead::parse(&h).is_err());
    }

    #[test]
    fn dops() {
        let d = [0u8, 2, 0x01, 0x38, 0, 0, 0xBB, 0x80, 0, 0, 0];
        let head = OpusHead::from_dops(&d).unwrap();
        assert_eq!(head.pre_skip, 312);
        assert_eq!(head.channels, 2);
    }

    #[test]
    fn directly_constructed_headers_are_checked_before_decoder_allocation() {
        let head = OpusHead {
            channels: 1,
            pre_skip: 0,
            input_sample_rate: 48_000,
            output_gain: 0,
            mapping_family: 255,
            streams: usize::MAX,
            coupled_streams: 0,
            mapping: vec![0],
        };
        assert!(crate::Decoder::from_head(head, 48_000).is_err());
    }
}
