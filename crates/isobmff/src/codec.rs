//! Sample entries (`stsd` children) and codec configuration records: parsing and serialization.

use crate::bytes::{BoxBuf, Cur, FourCc, boxes};
use crate::error::{Error, Result};
use filmcraft_bitstream::BitReader;

/// One sample description (an entry of `stsd`).
#[derive(Clone, Debug, PartialEq)]
pub struct SampleEntry {
    /// Sample entry format (`avc1`, `mp4a`, `apch`, …).
    pub format: FourCc,
    pub data_reference_index: u16,
    pub codec: CodecConfig,
    /// Present for visual sample entries.
    pub video: Option<VideoParams>,
    /// Present for audio sample entries.
    pub audio: Option<AudioParams>,
    /// `btrt` box.
    pub bitrate: Option<BitRate>,
}

/// Fields common to all visual sample entries plus the optional descriptive boxes.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct VideoParams {
    pub width: u16,
    pub height: u16,
    /// Horizontal/vertical resolution in pixels per inch (16.16 fixed point; 72 dpi = 0x0048_0000).
    pub horiz_resolution: u32,
    pub vert_resolution: u32,
    pub frame_count: u16,
    pub compressor_name: String,
    pub depth: u16,
    /// QuickTime `vendor` / version fields (zero in ISO files).
    pub vendor: FourCc,
    pub temporal_quality: u32,
    pub spatial_quality: u32,
    pub color: Option<ColorInfo>,
    /// `pasp`: (hSpacing, vSpacing).
    pub pixel_aspect: Option<(u32, u32)>,
    pub clean_aperture: Option<CleanAperture>,
    pub field_info: Option<FieldInfo>,
    /// QuickTime `gama` (16.16 fixed point).
    pub gamma: Option<u32>,
    /// `mdcv`: mastering display colour volume (SMPTE ST 2086).
    pub mastering_display: Option<MasteringDisplay>,
    /// `clli`: content light level (MaxCLL, MaxFALL in cd/m²; 0 = unknown).
    pub content_light: Option<(u16, u16)>,
}

/// SMPTE ST 2086 mastering display metadata, in the units of the `mdcv` box and the H.264/HEVC
/// SEI: chromaticities in 0.00002 steps (order G, B, R as the SEI), luminance in 0.0001 cd/m².
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MasteringDisplay {
    pub primaries: [(u16, u16); 3],
    pub white_point: (u16, u16),
    pub max_luminance: u32,
    pub min_luminance: u32,
}

impl MasteringDisplay {
    /// BT.2020 primaries, D65, with the given peak and black in cd/m².
    pub fn bt2020(max_nits: f64, min_nits: f64) -> Self {
        let q = |v: f64| (v / 0.00002).round() as u16;
        MasteringDisplay {
            primaries: [(q(0.170), q(0.797)), (q(0.131), q(0.046)), (q(0.708), q(0.292))],
            white_point: (q(0.3127), q(0.3290)),
            max_luminance: (max_nits * 10_000.0).round() as u32,
            min_luminance: (min_nits * 10_000.0).round() as u32,
        }
    }
    pub fn max_nits(&self) -> f64 {
        self.max_luminance as f64 / 10_000.0
    }
    /// The 24-byte payload shared by `mdcv` and the SEI message.
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut v = Vec::with_capacity(24);
        for (x, y) in self.primaries {
            v.extend_from_slice(&x.to_be_bytes());
            v.extend_from_slice(&y.to_be_bytes());
        }
        v.extend_from_slice(&self.white_point.0.to_be_bytes());
        v.extend_from_slice(&self.white_point.1.to_be_bytes());
        v.extend_from_slice(&self.max_luminance.to_be_bytes());
        v.extend_from_slice(&self.min_luminance.to_be_bytes());
        v
    }
    pub fn parse(p: &[u8]) -> Option<Self> {
        if p.len() < 24 {
            return None;
        }
        let u16_at = |i: usize| u16::from_be_bytes([p[i], p[i + 1]]);
        let u32_at = |i: usize| u32::from_be_bytes([p[i], p[i + 1], p[i + 2], p[i + 3]]);
        Some(MasteringDisplay {
            primaries: [(u16_at(0), u16_at(2)), (u16_at(4), u16_at(6)), (u16_at(8), u16_at(10))],
            white_point: (u16_at(12), u16_at(14)),
            max_luminance: u32_at(16),
            min_luminance: u32_at(20),
        })
    }
}

/// `colr` box contents.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ColorInfo {
    /// ISO `nclx` (with full-range flag).
    Nclx { primaries: u16, transfer: u16, matrix: u16, full_range: bool },
    /// QuickTime `nclc`.
    Nclc { primaries: u16, transfer: u16, matrix: u16 },
    /// Embedded ICC profile (`prof` restricted/unrestricted `rICC`).
    Icc { kind: FourCc, profile: Vec<u8> },
}

/// `clap` clean aperture (all rationals num/den; offsets are signed).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CleanAperture {
    pub width: (u32, u32),
    pub height: (u32, u32),
    pub horiz_offset: (i32, u32),
    pub vert_offset: (i32, u32),
}

/// QuickTime `fiel`: field count (1 progressive, 2 interlaced) and ordering detail
/// (0, 1 = top first, 6 = bottom first, 9 / 14 per the QuickTime spec).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FieldInfo {
    pub fields: u8,
    pub detail: u8,
}

/// `btrt` box.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BitRate {
    pub buffer_size: u32,
    pub max_bitrate: u32,
    pub avg_bitrate: u32,
}

/// Fields of audio sample entries (ISO v0 plus QuickTime sound description v1/v2 extensions).
#[derive(Clone, Debug, PartialEq, Default)]
pub struct AudioParams {
    /// QuickTime sound description version (0, 1, 2). ISO files use 0.
    pub qt_version: u16,
    pub channels: u32,
    /// Bits per sample from the entry (16 for most compressed formats).
    pub sample_size: u16,
    pub sample_rate: f64,
    pub compression_id: i16,
    /// v1 fields.
    pub samples_per_packet: u32,
    pub bytes_per_packet: u32,
    pub bytes_per_frame: u32,
    pub bytes_per_sample: u32,
    /// v2 fields.
    pub const_bits_per_channel: u32,
    pub lpcm_flags: u32,
    pub const_bytes_per_packet: u32,
    pub const_frames_per_packet: u32,
    /// `enda` little-endian flag inside a QuickTime `wave` box.
    pub little_endian: Option<bool>,
}

/// Codec configuration decoded from the sample entry.
#[derive(Clone, Debug, PartialEq)]
pub enum CodecConfig {
    Avc(AvcConfig),
    Hevc(HevcConfig),
    Av1(Av1Config),
    Vp9(VpcConfig),
    /// Apple ProRes; fourcc is `apch`/`apcn`/`apcs`/`apco`/`ap4h`/`ap4x`.
    ProRes {
        fourcc: FourCc,
    },
    /// Motion JPEG (`jpeg`, `mjpa`, `mjpb`).
    Jpeg {
        fourcc: FourCc,
    },
    /// Avid DNxHD/DNxHR (`AVdn`, `AVdh`).
    Dnx {
        fourcc: FourCc,
    },
    /// Advanced Professional Video (`apv1` + `apvC`).
    Apv(ApvConfig),
    Aac(AacConfig),
    /// MPEG-1/2 audio (esds object type 0x69/0x6B, or QuickTime `.mp3` / `.mp2`). Usually layer
    /// III, but these name no layer: ffmpeg stores MP2 as `mp4a` + 0x6B. Each frame header has it.
    Mp3,
    Pcm(PcmConfig),
    /// ALAC magic cookie (ALACSpecificConfig, 24 bytes, optionally followed by a channel layout).
    Alac {
        cookie: Vec<u8>,
    },
    Opus(OpusConfig),
    Flac(FlacConfig),
    /// `dac3` payload.
    Ac3 {
        dac3: Vec<u8>,
    },
    /// `dec3` payload.
    Eac3 {
        dec3: Vec<u8>,
    },
    Timecode(TimecodeConfig),
    /// Anything else; `raw` is the full sample entry payload (after the 8-byte box header).
    Unknown {
        fourcc: FourCc,
        raw: Vec<u8>,
    },
}

impl CodecConfig {
    /// A short human-readable codec name.
    pub fn name(&self) -> &'static str {
        match self {
            CodecConfig::Avc(_) => "h264",
            CodecConfig::Hevc(_) => "hevc",
            CodecConfig::Av1(_) => "av1",
            CodecConfig::Vp9(_) => "vp9",
            CodecConfig::ProRes { .. } => "prores",
            CodecConfig::Jpeg { .. } => "mjpeg",
            CodecConfig::Dnx { .. } => "dnxhd",
            CodecConfig::Apv(_) => "apv",
            CodecConfig::Aac(_) => "aac",
            CodecConfig::Mp3 => "mp3",
            CodecConfig::Pcm(_) => "pcm",
            CodecConfig::Alac { .. } => "alac",
            CodecConfig::Opus(_) => "opus",
            CodecConfig::Flac(_) => "flac",
            CodecConfig::Ac3 { .. } => "ac3",
            CodecConfig::Eac3 { .. } => "eac3",
            CodecConfig::Timecode(_) => "timecode",
            CodecConfig::Unknown { .. } => "unknown",
        }
    }
}

/// `avcC` — AVCDecoderConfigurationRecord (ISO/IEC 14496-15 §5.3.3).
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct AvcConfig {
    pub profile: u8,
    pub compatibility: u8,
    pub level: u8,
    /// NAL length field size in bytes (1, 2 or 4).
    pub length_size: u8,
    pub sps: Vec<Vec<u8>>,
    pub pps: Vec<Vec<u8>>,
    /// Bytes after the PPS list (high-profile extension: chroma format, bit depths, SPS-ext).
    pub ext: Vec<u8>,
}

impl AvcConfig {
    /// Build from parameter sets (NAL units without start codes); profile/level taken from the first SPS.
    pub fn new(sps: Vec<Vec<u8>>, pps: Vec<Vec<u8>>, length_size: u8) -> Self {
        let (profile, compatibility, level) = sps.first().filter(|s| s.len() >= 4).map(|s| (s[1], s[2], s[3])).unwrap_or((0, 0, 0));
        AvcConfig { profile, compatibility, level, length_size, sps, pps, ext: Vec::new() }
    }

    pub fn parse(data: &[u8]) -> Result<Self> {
        let mut c = Cur::new(data, "avcC");
        let _version = c.u8()?;
        let profile = c.u8()?;
        let compatibility = c.u8()?;
        let level = c.u8()?;
        let length_size = (c.u8()? & 3) + 1;
        let nsps = (c.u8()? & 0x1F) as usize;
        let mut sps = Vec::with_capacity(nsps);
        for _ in 0..nsps {
            let n = c.u16()? as usize;
            sps.push(c.bytes(n)?.to_vec());
        }
        let npps = c.u8()? as usize;
        let mut pps = Vec::with_capacity(npps);
        for _ in 0..npps {
            let n = c.u16()? as usize;
            pps.push(c.bytes(n)?.to_vec());
        }
        Ok(AvcConfig { profile, compatibility, level, length_size, sps, pps, ext: c.rest().to_vec() })
    }

    pub fn to_bytes(&self) -> Vec<u8> {
        let mut b = BoxBuf::new();
        b.u8(1);
        b.u8(self.profile);
        b.u8(self.compatibility);
        b.u8(self.level);
        b.u8(0xFC | (self.length_size.clamp(1, 4) - 1));
        b.u8(0xE0 | (self.sps.len().min(31) as u8));
        for s in self.sps.iter().take(31) {
            b.u16(s.len() as u16);
            b.bytes(s);
        }
        b.u8(self.pps.len().min(255) as u8);
        for p in self.pps.iter().take(255) {
            b.u16(p.len() as u16);
            b.bytes(p);
        }
        b.bytes(&self.ext);
        b.buf
    }
}

/// One NAL array of an `hvcC` record.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct HevcNalArray {
    pub completeness: bool,
    pub nal_type: u8,
    pub nalus: Vec<Vec<u8>>,
}

/// `hvcC` — HEVCDecoderConfigurationRecord (ISO/IEC 14496-15 §8.3.3).
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct HevcConfig {
    pub general_profile_space: u8,
    pub general_tier_flag: bool,
    pub general_profile_idc: u8,
    pub general_profile_compatibility_flags: u32,
    /// 48 bits of general constraint indicator flags.
    pub general_constraint_indicator_flags: u64,
    pub general_level_idc: u8,
    pub min_spatial_segmentation_idc: u16,
    pub parallelism_type: u8,
    pub chroma_format_idc: u8,
    pub bit_depth_luma: u8,
    pub bit_depth_chroma: u8,
    pub avg_frame_rate: u16,
    pub constant_frame_rate: u8,
    pub num_temporal_layers: u8,
    pub temporal_id_nested: bool,
    pub length_size: u8,
    pub arrays: Vec<HevcNalArray>,
}

impl HevcConfig {
    pub fn parse(data: &[u8]) -> Result<Self> {
        let mut c = Cur::new(data, "hvcC");
        let _version = c.u8()?;
        let b = c.u8()?;
        let compat = c.u32()?;
        let hi = c.u16()? as u64;
        let lo = c.u32()? as u64;
        let level = c.u8()?;
        let min_seg = c.u16()? & 0x0FFF;
        let par = c.u8()? & 3;
        let chroma = c.u8()? & 3;
        let bdl = (c.u8()? & 7) + 8;
        let bdc = (c.u8()? & 7) + 8;
        let afr = c.u16()?;
        let x = c.u8()?;
        let narrays = c.u8()? as usize;
        let mut arrays = Vec::new();
        for _ in 0..narrays {
            let t = c.u8()?;
            let n = c.u16()? as usize;
            c.check_count(n, 2)?;
            let mut nalus = Vec::with_capacity(n);
            for _ in 0..n {
                let len = c.u16()? as usize;
                nalus.push(c.bytes(len)?.to_vec());
            }
            arrays.push(HevcNalArray { completeness: t & 0x80 != 0, nal_type: t & 0x3F, nalus });
        }
        Ok(HevcConfig {
            general_profile_space: b >> 6,
            general_tier_flag: b & 0x20 != 0,
            general_profile_idc: b & 0x1F,
            general_profile_compatibility_flags: compat,
            general_constraint_indicator_flags: (hi << 32) | lo,
            general_level_idc: level,
            min_spatial_segmentation_idc: min_seg,
            parallelism_type: par,
            chroma_format_idc: chroma,
            bit_depth_luma: bdl,
            bit_depth_chroma: bdc,
            avg_frame_rate: afr,
            constant_frame_rate: x >> 6,
            num_temporal_layers: (x >> 3) & 7,
            temporal_id_nested: x & 4 != 0,
            length_size: (x & 3) + 1,
            arrays,
        })
    }

    pub fn to_bytes(&self) -> Vec<u8> {
        let mut b = BoxBuf::new();
        b.u8(1);
        b.u8((self.general_profile_space << 6) | ((self.general_tier_flag as u8) << 5) | (self.general_profile_idc & 0x1F));
        b.u32(self.general_profile_compatibility_flags);
        b.u16((self.general_constraint_indicator_flags >> 32) as u16);
        b.u32(self.general_constraint_indicator_flags as u32);
        b.u8(self.general_level_idc);
        b.u16(0xF000 | (self.min_spatial_segmentation_idc & 0x0FFF));
        b.u8(0xFC | (self.parallelism_type & 3));
        b.u8(0xFC | (self.chroma_format_idc & 3));
        b.u8(0xF8 | (self.bit_depth_luma.saturating_sub(8) & 7));
        b.u8(0xF8 | (self.bit_depth_chroma.saturating_sub(8) & 7));
        b.u16(self.avg_frame_rate);
        b.u8((self.constant_frame_rate << 6)
            | ((self.num_temporal_layers & 7) << 3)
            | ((self.temporal_id_nested as u8) << 2)
            | (self.length_size.clamp(1, 4) - 1));
        b.u8(self.arrays.len() as u8);
        for a in &self.arrays {
            b.u8(((a.completeness as u8) << 7) | (a.nal_type & 0x3F));
            b.u16(a.nalus.len() as u16);
            for n in &a.nalus {
                b.u16(n.len() as u16);
                b.bytes(n);
            }
        }
        b.buf
    }

    fn nalus_of(&self, t: u8) -> Vec<&[u8]> {
        self.arrays.iter().filter(|a| a.nal_type == t).flat_map(|a| a.nalus.iter().map(|n| n.as_slice())).collect()
    }
    pub fn vps(&self) -> Vec<&[u8]> {
        self.nalus_of(32)
    }
    pub fn sps(&self) -> Vec<&[u8]> {
        self.nalus_of(33)
    }
    pub fn pps(&self) -> Vec<&[u8]> {
        self.nalus_of(34)
    }
}

/// `av1C` — AV1CodecConfigurationRecord.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct Av1Config {
    pub seq_profile: u8,
    pub seq_level_idx_0: u8,
    pub seq_tier_0: bool,
    pub high_bitdepth: bool,
    pub twelve_bit: bool,
    pub monochrome: bool,
    pub chroma_subsampling_x: bool,
    pub chroma_subsampling_y: bool,
    pub chroma_sample_position: u8,
    pub initial_presentation_delay_minus_one: Option<u8>,
    pub config_obus: Vec<u8>,
}

impl Av1Config {
    pub fn parse(data: &[u8]) -> Result<Self> {
        let mut c = Cur::new(data, "av1C");
        let m = c.u8()?;
        if m & 0x80 == 0 {
            return Err(Error::Invalid("av1C marker bit not set".into()));
        }
        let a = c.u8()?;
        let b = c.u8()?;
        let d = c.u8()?;
        Ok(Av1Config {
            seq_profile: a >> 5,
            seq_level_idx_0: a & 0x1F,
            seq_tier_0: b & 0x80 != 0,
            high_bitdepth: b & 0x40 != 0,
            twelve_bit: b & 0x20 != 0,
            monochrome: b & 0x10 != 0,
            chroma_subsampling_x: b & 0x08 != 0,
            chroma_subsampling_y: b & 0x04 != 0,
            chroma_sample_position: b & 3,
            initial_presentation_delay_minus_one: if d & 0x10 != 0 { Some(d & 0x0F) } else { None },
            config_obus: c.rest().to_vec(),
        })
    }
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut b = BoxBuf::new();
        b.u8(0x81);
        b.u8((self.seq_profile << 5) | (self.seq_level_idx_0 & 0x1F));
        b.u8(((self.seq_tier_0 as u8) << 7)
            | ((self.high_bitdepth as u8) << 6)
            | ((self.twelve_bit as u8) << 5)
            | ((self.monochrome as u8) << 4)
            | ((self.chroma_subsampling_x as u8) << 3)
            | ((self.chroma_subsampling_y as u8) << 2)
            | (self.chroma_sample_position & 3));
        b.u8(match self.initial_presentation_delay_minus_one {
            Some(d) => 0x10 | (d & 0x0F),
            None => 0,
        });
        b.bytes(&self.config_obus);
        b.buf
    }
}

/// `vpcC` — VP codec configuration (version 1).
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct VpcConfig {
    pub profile: u8,
    pub level: u8,
    pub bit_depth: u8,
    pub chroma_subsampling: u8,
    pub full_range: bool,
    pub colour_primaries: u8,
    pub transfer_characteristics: u8,
    pub matrix_coefficients: u8,
    pub codec_init: Vec<u8>,
}

impl VpcConfig {
    pub fn parse(data: &[u8]) -> Result<Self> {
        let mut c = Cur::new(data, "vpcC");
        let (version, _) = c.full_header()?;
        if version == 0 {
            // Version 0 draft layout: profile, level, bitDepth(4)|colorSpace(4), chroma(4)|transfer(3)|range(1)
            let profile = c.u8()?;
            let level = c.u8()?;
            let a = c.u8()?;
            let b = c.u8()?;
            return Ok(VpcConfig { profile, level, bit_depth: a >> 4, chroma_subsampling: b >> 4, full_range: b & 1 != 0, ..Default::default() });
        }
        let profile = c.u8()?;
        let level = c.u8()?;
        let a = c.u8()?;
        let colour_primaries = c.u8()?;
        let transfer_characteristics = c.u8()?;
        let matrix_coefficients = c.u8()?;
        let n = c.u16()? as usize;
        let codec_init = c.bytes(n)?.to_vec();
        Ok(VpcConfig {
            profile,
            level,
            bit_depth: a >> 4,
            chroma_subsampling: (a >> 1) & 7,
            full_range: a & 1 != 0,
            colour_primaries,
            transfer_characteristics,
            matrix_coefficients,
            codec_init,
        })
    }
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut b = BoxBuf::new();
        b.u32(1 << 24);
        b.u8(self.profile);
        b.u8(self.level);
        b.u8((self.bit_depth << 4) | ((self.chroma_subsampling & 7) << 1) | self.full_range as u8);
        b.u8(self.colour_primaries);
        b.u8(self.transfer_characteristics);
        b.u8(self.matrix_coefficients);
        b.u16(self.codec_init.len() as u16);
        b.bytes(&self.codec_init);
        b.buf
    }
}

/// AAC (MPEG-4 audio) configuration from `esds`.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct AacConfig {
    /// AudioSpecificConfig bytes (DecoderSpecificInfo).
    pub asc: Vec<u8>,
    /// Audio object type (2 = AAC-LC, 5 = SBR/HE-AAC, 29 = PS …) as signalled first in the ASC.
    pub object_type: u8,
    /// Sampling rate signalled by the ASC (core rate; 0 if the ASC is missing).
    pub sample_rate: u32,
    /// Channel configuration from the ASC (0 = defined elsewhere).
    pub channel_config: u8,
    /// DecoderConfigDescriptor fields.
    pub max_bitrate: u32,
    pub avg_bitrate: u32,
    pub buffer_size: u32,
}

const AAC_RATES: [u32; 13] = [96000, 88200, 64000, 48000, 44100, 32000, 24000, 22050, 16000, 12000, 11025, 8000, 7350];

impl AacConfig {
    /// Build from an AudioSpecificConfig, decoding its header fields.
    pub fn from_asc(asc: Vec<u8>) -> Self {
        let (object_type, sample_rate, channel_config) = parse_asc(&asc).unwrap_or((0, 0, 0));
        AacConfig { asc, object_type, sample_rate, channel_config, max_bitrate: 0, avg_bitrate: 0, buffer_size: 0 }
    }
    /// Channel count implied by the channel configuration (0 if unspecified).
    pub fn channels(&self) -> u32 {
        match self.channel_config {
            1..=6 => self.channel_config as u32,
            7 => 8,
            _ => 0,
        }
    }
}

/// Decode (audio object type, sampling frequency, channel configuration) from an AudioSpecificConfig.
pub fn parse_asc(asc: &[u8]) -> Option<(u8, u32, u8)> {
    let mut r = BitReader::new(asc);
    let aot = |r: &mut BitReader| -> Option<u8> {
        let t = r.read_bits(5).ok()? as u8;
        if t == 31 { Some(32 + r.read_bits(6).ok()? as u8) } else { Some(t) }
    };
    let rate = |r: &mut BitReader| -> Option<u32> {
        let i = r.read_bits(4).ok()? as usize;
        if i == 15 { r.read_bits(24).ok() } else { AAC_RATES.get(i).copied() }
    };
    let t = aot(&mut r)?;
    let sr = rate(&mut r)?;
    let ch = r.read_bits(4).ok()? as u8;
    Some((t, sr, ch))
}

/// Uncompressed PCM description.
#[derive(Clone, Copy, Debug, PartialEq, Default)]
pub struct PcmConfig {
    pub bits: u16,
    pub float: bool,
    pub big_endian: bool,
    /// Signed integer samples (8-bit `raw ` is unsigned).
    pub signed: bool,
    pub channels: u32,
    pub sample_rate: f64,
}

impl PcmConfig {
    /// Bytes per interleaved audio frame (all channels).
    pub fn bytes_per_frame(&self) -> u32 {
        (self.bits as u32).div_ceil(8) * self.channels.max(1)
    }
}

/// `dOps` — Opus specific box.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct OpusConfig {
    pub output_channels: u8,
    pub pre_skip: u16,
    pub input_sample_rate: u32,
    pub output_gain: i16,
    pub channel_mapping_family: u8,
    /// StreamCount, CoupledCount and ChannelMapping bytes when family != 0.
    pub channel_mapping: Vec<u8>,
}

impl OpusConfig {
    pub fn parse(data: &[u8]) -> Result<Self> {
        let mut c = Cur::new(data, "dOps");
        let _version = c.u8()?;
        Ok(OpusConfig {
            output_channels: c.u8()?,
            pre_skip: c.u16()?,
            input_sample_rate: c.u32()?,
            output_gain: c.i16()?,
            channel_mapping_family: c.u8()?,
            channel_mapping: c.rest().to_vec(),
        })
    }
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut b = BoxBuf::new();
        b.u8(0);
        b.u8(self.output_channels);
        b.u16(self.pre_skip);
        b.u32(self.input_sample_rate);
        b.i16(self.output_gain);
        b.u8(self.channel_mapping_family);
        b.bytes(&self.channel_mapping);
        b.buf
    }
}

/// `dfLa` — FLAC metadata blocks, with STREAMINFO decoded.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct FlacConfig {
    /// Raw metadata blocks (each with its 4-byte header) as stored in `dfLa`.
    pub metadata_blocks: Vec<u8>,
    pub sample_rate: u32,
    pub channels: u8,
    pub bits_per_sample: u8,
    pub total_samples: u64,
}

impl FlacConfig {
    pub fn parse(data: &[u8]) -> Result<Self> {
        let mut c = Cur::new(data, "dfLa");
        c.full_header()?;
        let blocks = c.rest().to_vec();
        let mut cfg = FlacConfig { metadata_blocks: blocks.clone(), ..Default::default() };
        let mut b = Cur::new(&blocks, "dfLa");
        let hdr = b.u32()?;
        if (hdr >> 24) & 0x7F == 0 {
            let si = b.bytes(((hdr & 0xFF_FFFF) as usize).min(34))?;
            if si.len() >= 18 {
                let mut r = BitReader::new(&si[10..18]);
                cfg.sample_rate = r.read_bits(20).unwrap_or(0);
                cfg.channels = r.read_bits(3).unwrap_or(0) as u8 + 1;
                cfg.bits_per_sample = r.read_bits(5).unwrap_or(0) as u8 + 1;
                cfg.total_samples = r.read_bits_u64(36).unwrap_or(0);
            }
        }
        Ok(cfg)
    }
}

/// QuickTime timecode sample description (`tmcd`) plus the start frame read from its first sample.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct TimecodeConfig {
    /// Bit 0 drop frame, bit 1 24-hour max, bit 2 negative times OK, bit 3 counter.
    pub flags: u32,
    pub timescale: u32,
    pub frame_duration: u32,
    /// Nominal frames per second (`numberOfFrames`), e.g. 30 for 29.97.
    pub frames_per_second: u8,
    /// Frame number of the first timecode sample (filled in by the demuxer).
    pub start_frame: Option<u32>,
    /// Source reel name (`name` box) if present.
    pub name: Option<String>,
}

impl TimecodeConfig {
    pub fn drop_frame(&self) -> bool {
        self.flags & 1 != 0
    }
    /// Format a frame number as `HH:MM:SS:FF` (`;` before the frames when drop-frame).
    pub fn format_frame(&self, frame: u32) -> String {
        let fps = self.frames_per_second.max(1) as u64;
        let mut f = frame as u64;
        if self.drop_frame() && fps >= 30 {
            let drop = (fps / 30) * 2; // 2 frames per minute at 30, 4 at 60
            let per_min = fps * 60 - drop;
            let per_10 = per_min * 10 + drop;
            let d = f / per_10;
            let m = f % per_10;
            f += drop * 9 * d + if m > drop { drop * ((m - drop) / per_min) } else { 0 };
        }
        let ff = f % fps;
        let s = (f / fps) % 60;
        let mi = (f / fps / 60) % 60;
        let h = (f / fps / 3600) % 24;
        let sep = if self.drop_frame() { ';' } else { ':' };
        format!("{h:02}:{mi:02}:{s:02}{sep}{ff:02}")
    }
}

/// `apvC` — APVDecoderConfigurationBox (`FullBox('apvC', version = 0, flags = 0)` containing
/// `APVDecoderConfigurationRecord`).
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct ApvConfig {
    pub profile_idc: u8,
    pub level_idc: u8,
    pub band_idc: u8,
    pub frame_width: u32,
    pub frame_height: u32,
    pub chroma_format_idc: u8,
    pub bit_depth_minus8: u8,
    pub capture_time_distance: u8,
    pub capture_time_distance_ignored: bool,
    pub color_description_present: bool,
    pub color_primaries: u8,
    pub transfer_characteristics: u8,
    pub matrix_coefficients: u8,
    pub full_range: bool,
    /// Raw `APVDecoderConfigurationRecord` bytes (after the 4-byte FullBox header).
    pub raw_record: Vec<u8>,
}

impl ApvConfig {
    /// Build from a raw `APVDecoderConfigurationRecord` (starting with `configurationVersion = 1`).
    pub fn from_record(record: Vec<u8>) -> Result<Self> {
        let mut c = Cur::new(&record, "apvC");
        let version = c.u8()?;
        if version != 1 {
            return Err(Error::Invalid(format!("unsupported APVDecoderConfigurationRecord version {version}")));
        }
        let num_entries = c.u8()?;
        let mut cfg = ApvConfig { raw_record: record.clone(), ..Default::default() };
        if num_entries >= 1 && c.remaining() >= 2 {
            let _pbu_type = c.u8()?;
            let num_fi = c.u8()?;
            if num_fi >= 1 && c.remaining() >= 14 {
                let flags = c.u8()?;
                cfg.color_description_present = (flags & 0x02) != 0;
                cfg.capture_time_distance_ignored = (flags & 0x01) != 0;
                cfg.profile_idc = c.u8()?;
                cfg.level_idc = c.u8()?;
                cfg.band_idc = c.u8()?;
                cfg.frame_width = c.u32()?;
                cfg.frame_height = c.u32()?;
                let cb = c.u8()?;
                cfg.chroma_format_idc = cb >> 4;
                cfg.bit_depth_minus8 = cb & 0x0F;
                cfg.capture_time_distance = c.u8()?;
                if cfg.color_description_present && c.remaining() >= 4 {
                    cfg.color_primaries = c.u8()?;
                    cfg.transfer_characteristics = c.u8()?;
                    cfg.matrix_coefficients = c.u8()?;
                    cfg.full_range = (c.u8()? & 0x80) != 0;
                }
            }
        }
        Ok(cfg)
    }

    /// Parse the payload of an `apvC` box (with or without the 4-byte `FullBox` header).
    pub fn parse(data: &[u8]) -> Result<Self> {
        if data.len() >= 5 && data[..4] == [0, 0, 0, 0] && data[4] == 1 {
            Self::from_record(data[4..].to_vec())
        } else if !data.is_empty() && data[0] == 1 {
            Self::from_record(data.to_vec())
        } else {
            let mut c = Cur::new(data, "apvC");
            let (v, _) = c.full_header()?;
            if v != 0 {
                return Err(Error::Invalid(format!("unsupported apvC FullBox version {v}")));
            }
            Self::from_record(c.rest().to_vec())
        }
    }

    /// Serialize the full `apvC` box payload (4-byte `FullBox` header + `APVDecoderConfigurationRecord`).
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut b = BoxBuf::new();
        b.u32(0); // FullBox version = 0, flags = 0
        if !self.raw_record.is_empty() {
            b.bytes(&self.raw_record);
            return b.buf;
        }
        b.u8(1); // configurationVersion = 1
        b.u8(1); // number_of_configuration_entry = 1
        b.u8(1); // pbu_type = 1 (primary frame)
        b.u8(1); // number_of_frame_info = 1
        let flags = (if self.color_description_present { 0x02 } else { 0 }) | (if self.capture_time_distance_ignored { 0x01 } else { 0 });
        b.u8(flags);
        b.u8(self.profile_idc);
        b.u8(self.level_idc);
        b.u8(self.band_idc);
        b.u32(self.frame_width);
        b.u32(self.frame_height);
        b.u8((self.chroma_format_idc << 4) | (self.bit_depth_minus8 & 0x0F));
        b.u8(self.capture_time_distance);
        if self.color_description_present {
            b.u8(self.color_primaries);
            b.u8(self.transfer_characteristics);
            b.u8(self.matrix_coefficients);
            b.u8(if self.full_range { 0x80 } else { 0 });
        }
        b.buf
    }
}

// ---------------------------------------------------------------------------------------------
// Parsing

fn is_video_fourcc(f: &[u8; 4]) -> bool {
    matches!(
        f,
        b"avc1"
            | b"avc3"
            | b"hvc1"
            | b"hev1"
            | b"av01"
            | b"vp09"
            | b"vp08"
            | b"apch"
            | b"apcn"
            | b"apcs"
            | b"apco"
            | b"ap4h"
            | b"ap4x"
            | b"apv1"
            | b"jpeg"
            | b"mjpa"
            | b"mjpb"
            | b"AVdn"
            | b"AVdh"
            | b"mp4v"
            | b"encv"
            | b"raw "
            | b"2vuy"
            | b"v210"
    )
}

fn is_audio_fourcc(f: &[u8; 4]) -> bool {
    matches!(
        f,
        b"mp4a"
            | b"lpcm"
            | b"sowt"
            | b"twos"
            | b"in24"
            | b"in32"
            | b"fl32"
            | b"fl64"
            | b"ipcm"
            | b"fpcm"
            | b"alac"
            | b"Opus"
            | b"fLaC"
            | b"ac-3"
            | b"ec-3"
            | b".mp3"
            | b".mp2"
            | b"enca"
            | b"ulaw"
            | b"alaw"
            | b"NONE"
    )
}

/// Parse one `stsd` entry. `handler` is the track handler type; `is_qt` selects QuickTime
/// interpretation of the sound description version field.
pub(crate) fn parse_sample_entry(format: FourCc, payload: &[u8], handler: FourCc, is_qt: bool) -> Result<SampleEntry> {
    let mut c = Cur::new(payload, "sample entry");
    c.skip(6)?;
    let data_reference_index = c.u16()?;
    let f = &format.0;
    let unknown = || CodecConfig::Unknown { fourcc: format, raw: payload.to_vec() };
    if f == b"tmcd" || (handler == b"tmcd" && !is_video_fourcc(f)) {
        let mut tc = TimecodeConfig::default();
        c.skip(4)?;
        tc.flags = c.u32()?;
        tc.timescale = c.u32()?;
        tc.frame_duration = c.u32()?;
        tc.frames_per_second = c.u8()?;
        c.skip(1)?;
        for b in boxes(c.rest()).map_while(|b| b.ok()) {
            if b.kind == b"name" && b.payload.len() >= 4 {
                let n = u16::from_be_bytes([b.payload[0], b.payload[1]]) as usize;
                let s = b.payload.get(4..4 + n).unwrap_or(&b.payload[4..]);
                tc.name = Some(String::from_utf8_lossy(s).into_owned());
            }
        }
        return Ok(SampleEntry { format, data_reference_index, codec: CodecConfig::Timecode(tc), video: None, audio: None, bitrate: None });
    }
    let video_like = is_video_fourcc(f) || (handler == b"vide" && !is_audio_fourcc(f));
    let audio_like = is_audio_fourcc(f) || (handler == b"soun" && !video_like);
    if video_like {
        let mut v = VideoParams::default();
        let _version = c.u16()?;
        let _revision = c.u16()?;
        v.vendor = c.fourcc()?;
        v.temporal_quality = c.u32()?;
        v.spatial_quality = c.u32()?;
        v.width = c.u16()?;
        v.height = c.u16()?;
        v.horiz_resolution = c.u32()?;
        v.vert_resolution = c.u32()?;
        c.skip(4)?;
        v.frame_count = c.u16()?;
        let name = c.bytes(32)?;
        let n = (name[0] as usize).min(31);
        v.compressor_name = String::from_utf8_lossy(&name[1..1 + n]).into_owned();
        v.depth = c.u16()?;
        let _ctab = c.i16()?;
        let mut codec = None;
        let mut bitrate = None;
        for b in boxes(c.rest()).map_while(|b| b.ok()) {
            let p = b.payload;
            match &b.kind.0 {
                b"avcC" if matches!(f, b"avc1" | b"avc3") => codec = Some(CodecConfig::Avc(AvcConfig::parse(p)?)),
                b"hvcC" if matches!(f, b"hvc1" | b"hev1") => codec = Some(CodecConfig::Hevc(HevcConfig::parse(p)?)),
                b"av1C" if f == b"av01" => codec = Some(CodecConfig::Av1(Av1Config::parse(p)?)),
                b"vpcC" if f == b"vp09" => codec = Some(CodecConfig::Vp9(VpcConfig::parse(p)?)),
                b"apvC" if f == b"apv1" => codec = Some(CodecConfig::Apv(ApvConfig::parse(p)?)),
                b"colr" => v.color = parse_colr(p).ok(),
                b"mdcv" => v.mastering_display = MasteringDisplay::parse(p),
                b"clli" if p.len() >= 4 => v.content_light = Some((u16::from_be_bytes([p[0], p[1]]), u16::from_be_bytes([p[2], p[3]]))),
                b"pasp" => {
                    let mut c = Cur::new(p, "pasp");
                    v.pixel_aspect = Some((c.u32()?, c.u32()?));
                }
                b"clap" => {
                    let mut c = Cur::new(p, "clap");
                    v.clean_aperture = Some(CleanAperture {
                        width: (c.u32()?, c.u32()?),
                        height: (c.u32()?, c.u32()?),
                        horiz_offset: (c.i32()?, c.u32()?),
                        vert_offset: (c.i32()?, c.u32()?),
                    });
                }
                b"fiel" => {
                    let mut c = Cur::new(p, "fiel");
                    v.field_info = Some(FieldInfo { fields: c.u8()?, detail: c.u8()? });
                }
                b"gama" => v.gamma = Some(Cur::new(p, "gama").u32()?),
                b"btrt" => bitrate = Some(parse_btrt(p)?),
                _ => {}
            }
        }
        let codec = match codec {
            Some(c) => c,
            None => match f {
                b"apch" | b"apcn" | b"apcs" | b"apco" | b"ap4h" | b"ap4x" => CodecConfig::ProRes { fourcc: format },
                b"jpeg" | b"mjpa" | b"mjpb" => CodecConfig::Jpeg { fourcc: format },
                b"AVdn" | b"AVdh" => CodecConfig::Dnx { fourcc: format },
                b"apv1" => CodecConfig::Apv(ApvConfig::default()),
                _ => unknown(),
            },
        };
        return Ok(SampleEntry { format, data_reference_index, codec, video: Some(v), audio: None, bitrate });
    }
    if audio_like {
        let mut a = AudioParams { qt_version: c.u16()?, ..Default::default() };
        let _revision = c.u16()?;
        let _vendor = c.u32()?;
        a.channels = c.u16()? as u32;
        a.sample_size = c.u16()?;
        a.compression_id = c.i16()?;
        let _packet_size = c.u16()?;
        a.sample_rate = c.u32()? as f64 / 65536.0;
        if is_qt && a.qt_version == 1 {
            a.samples_per_packet = c.u32()?;
            a.bytes_per_packet = c.u32()?;
            a.bytes_per_frame = c.u32()?;
            a.bytes_per_sample = c.u32()?;
        } else if is_qt && a.qt_version == 2 {
            let _size_of_struct = c.u32()?;
            a.sample_rate = c.f64()?;
            a.channels = c.u32()?;
            let _always = c.u32()?;
            a.const_bits_per_channel = c.u32()?;
            a.lpcm_flags = c.u32()?;
            a.const_bytes_per_packet = c.u32()?;
            a.const_frames_per_packet = c.u32()?;
        }
        // Flatten QuickTime `wave` children into the list of extension boxes.
        let mut ext: Vec<(FourCc, &[u8])> = Vec::new();
        for b in boxes(c.rest()).map_while(|b| b.ok()) {
            if b.kind == b"wave" {
                for w in boxes(b.payload).map_while(|b| b.ok()) {
                    ext.push((w.kind, w.payload));
                }
            } else {
                ext.push((b.kind, b.payload));
            }
        }
        let get = |k: &[u8; 4]| ext.iter().find(|(kk, _)| kk == k).map(|(_, p)| *p);
        if let Some(p) = get(b"enda") {
            let v = if p.len() >= 2 { u16::from_be_bytes([p[0], p[1]]) } else { p.first().copied().unwrap_or(0) as u16 };
            a.little_endian = Some(v != 0);
        }
        let bitrate = get(b"btrt").and_then(|p| parse_btrt(p).ok());
        let codec = match f {
            b"mp4a" => match get(b"esds") {
                Some(p) => parse_esds(p)?,
                None => unknown(),
            },
            b".mp3" | b".mp2" => CodecConfig::Mp3,
            b"alac" => match get(b"alac") {
                Some(p) if p.len() >= 4 => CodecConfig::Alac { cookie: p[4..].to_vec() },
                _ => unknown(),
            },
            b"Opus" => match get(b"dOps") {
                Some(p) => CodecConfig::Opus(OpusConfig::parse(p)?),
                None => unknown(),
            },
            b"fLaC" => match get(b"dfLa") {
                Some(p) => CodecConfig::Flac(FlacConfig::parse(p)?),
                None => unknown(),
            },
            b"ac-3" => CodecConfig::Ac3 { dac3: get(b"dac3").unwrap_or(&[]).to_vec() },
            b"ec-3" => CodecConfig::Eac3 { dec3: get(b"dec3").unwrap_or(&[]).to_vec() },
            b"lpcm" | b"sowt" | b"twos" | b"in24" | b"in32" | b"fl32" | b"fl64" | b"raw " | b"NONE" => CodecConfig::Pcm(qt_pcm(f, &a)),
            b"ipcm" | b"fpcm" => {
                let mut pcm =
                    PcmConfig { bits: a.sample_size, float: f == b"fpcm", big_endian: true, signed: true, channels: a.channels, sample_rate: a.sample_rate };
                if let Some(p) = get(b"pcmC") {
                    let mut c = Cur::new(p, "pcmC");
                    c.full_header()?;
                    let flags = c.u8()?;
                    pcm.big_endian = flags & 1 == 0;
                    pcm.bits = c.u8()? as u16;
                }
                CodecConfig::Pcm(pcm)
            }
            _ => unknown(),
        };
        return Ok(SampleEntry { format, data_reference_index, codec, video: None, audio: Some(a), bitrate });
    }
    Ok(SampleEntry { format, data_reference_index, codec: unknown(), video: None, audio: None, bitrate: None })
}

fn qt_pcm(f: &[u8; 4], a: &AudioParams) -> PcmConfig {
    let mut p = PcmConfig { channels: a.channels, sample_rate: a.sample_rate, signed: true, ..Default::default() };
    match f {
        b"lpcm" => {
            p.bits = a.const_bits_per_channel as u16;
            p.float = a.lpcm_flags & 1 != 0;
            p.big_endian = a.lpcm_flags & 2 != 0;
            p.signed = a.lpcm_flags & 4 != 0 || p.float;
            if p.bits == 0 {
                p.bits = a.sample_size;
            }
        }
        b"sowt" => {
            p.bits = if a.sample_size == 0 { 16 } else { a.sample_size };
        }
        b"twos" => {
            p.bits = if a.sample_size == 0 { 16 } else { a.sample_size };
            p.big_endian = true;
        }
        b"raw " | b"NONE" => {
            p.bits = if a.sample_size == 0 { 8 } else { a.sample_size };
            p.signed = p.bits != 8;
            p.big_endian = true;
        }
        b"in24" | b"in32" | b"fl32" | b"fl64" => {
            p.bits = match f {
                b"in24" => 24,
                b"in32" | b"fl32" => 32,
                _ => 64,
            };
            p.float = f[0] == b'f';
            p.big_endian = !a.little_endian.unwrap_or(false);
        }
        _ => {}
    }
    p
}

fn parse_btrt(p: &[u8]) -> Result<BitRate> {
    let mut c = Cur::new(p, "btrt");
    Ok(BitRate { buffer_size: c.u32()?, max_bitrate: c.u32()?, avg_bitrate: c.u32()? })
}

fn parse_colr(p: &[u8]) -> Result<ColorInfo> {
    let mut c = Cur::new(p, "colr");
    let kind = c.fourcc()?;
    Ok(match &kind.0 {
        b"nclx" => ColorInfo::Nclx { primaries: c.u16()?, transfer: c.u16()?, matrix: c.u16()?, full_range: c.u8().map(|v| v & 0x80 != 0).unwrap_or(false) },
        b"nclc" => ColorInfo::Nclc { primaries: c.u16()?, transfer: c.u16()?, matrix: c.u16()? },
        _ => ColorInfo::Icc { kind, profile: c.rest().to_vec() },
    })
}

fn descriptor(c: &mut Cur) -> Result<(u8, usize)> {
    let tag = c.u8()?;
    let mut len = 0usize;
    for _ in 0..4 {
        let b = c.u8()?;
        len = (len << 7) | (b & 0x7F) as usize;
        if b & 0x80 == 0 {
            break;
        }
    }
    Ok((tag, len))
}

fn parse_esds(p: &[u8]) -> Result<CodecConfig> {
    let mut c = Cur::new(p, "esds");
    c.full_header()?;
    let (tag, len) = descriptor(&mut c)?;
    if tag != 3 {
        return Err(Error::Invalid(format!("esds: expected ES_Descriptor, got tag {tag}")));
    }
    let es = c.bytes(len.min(c.remaining()))?;
    let mut c = Cur::new(es, "ES_Descriptor");
    c.u16()?;
    let fl = c.u8()?;
    if fl & 0x80 != 0 {
        c.u16()?;
    }
    if fl & 0x40 != 0 {
        let n = c.u8()? as usize;
        c.skip(n)?;
    }
    if fl & 0x20 != 0 {
        c.u16()?;
    }
    while c.remaining() > 0 {
        let (tag, len) = descriptor(&mut c)?;
        let body = c.bytes(len.min(c.remaining()))?;
        if tag != 4 {
            continue;
        }
        let mut d = Cur::new(body, "DecoderConfigDescriptor");
        let oti = d.u8()?;
        let _stream_type = d.u8()?;
        let buffer_size = d.u24()?;
        let max_bitrate = d.u32()?;
        let avg_bitrate = d.u32()?;
        let mut asc = Vec::new();
        while d.remaining() > 0 {
            let (t, l) = descriptor(&mut d)?;
            let b = d.bytes(l.min(d.remaining()))?;
            if t == 5 {
                asc = b.to_vec();
                break;
            }
        }
        return Ok(match oti {
            0x69 | 0x6B => CodecConfig::Mp3,
            _ => {
                let mut a = AacConfig::from_asc(asc);
                a.max_bitrate = max_bitrate;
                a.avg_bitrate = avg_bitrate;
                a.buffer_size = buffer_size;
                CodecConfig::Aac(a)
            }
        });
    }
    Err(Error::Invalid("esds without DecoderConfigDescriptor".into()))
}

// ---------------------------------------------------------------------------------------------
// Serialization (muxer)

fn put_descriptor(b: &mut BoxBuf, tag: u8, body: &[u8]) {
    b.u8(tag);
    let n = body.len() as u32;
    // Always use the 4-byte length form for simplicity (as many encoders do).
    b.u8(0x80 | ((n >> 21) & 0x7F) as u8);
    b.u8(0x80 | ((n >> 14) & 0x7F) as u8);
    b.u8(0x80 | ((n >> 7) & 0x7F) as u8);
    b.u8((n & 0x7F) as u8);
    b.bytes(body);
}

pub(crate) fn esds_payload(oti: u8, asc: &[u8], max_bitrate: u32, avg_bitrate: u32, buffer_size: u32) -> Vec<u8> {
    let mut dcd = BoxBuf::new();
    dcd.u8(oti);
    dcd.u8(0x15); // streamType audio (5) << 2 | upstream 0 | reserved 1
    dcd.u8((buffer_size >> 16) as u8);
    dcd.u16(buffer_size as u16);
    dcd.u32(max_bitrate);
    dcd.u32(avg_bitrate);
    if !asc.is_empty() {
        put_descriptor(&mut dcd, 5, asc);
    }
    let mut es = BoxBuf::new();
    es.u16(1); // ES_ID
    es.u8(0);
    put_descriptor(&mut es, 4, &dcd.buf);
    put_descriptor(&mut es, 6, &[2]); // SLConfigDescriptor, predefined = 2 (MP4)
    let mut b = BoxBuf::new();
    b.u32(0);
    put_descriptor(&mut b, 3, &es.buf);
    b.buf
}

impl SampleEntry {
    /// Convenience constructor for a video entry.
    pub fn video(format: FourCc, codec: CodecConfig, width: u16, height: u16) -> Self {
        let compressor_name = match &codec {
            CodecConfig::ProRes { fourcc } => match &fourcc.0 {
                b"apch" => "Apple ProRes 422 HQ",
                b"apcn" => "Apple ProRes 422",
                b"apcs" => "Apple ProRes 422 LT",
                b"apco" => "Apple ProRes 422 Proxy",
                b"ap4h" => "Apple ProRes 4444",
                _ => "Apple ProRes 4444 XQ",
            },
            CodecConfig::Jpeg { .. } => "Photo - JPEG",
            _ => "",
        };
        SampleEntry {
            format,
            data_reference_index: 1,
            codec,
            video: Some(VideoParams {
                width,
                height,
                horiz_resolution: 0x0048_0000,
                vert_resolution: 0x0048_0000,
                frame_count: 1,
                compressor_name: compressor_name.into(),
                depth: 24,
                ..Default::default()
            }),
            audio: None,
            bitrate: None,
        }
    }

    /// Convenience constructor for an audio entry (ISO v0 layout; the muxer upgrades PCM in MOV to v2).
    pub fn audio(format: FourCc, codec: CodecConfig, channels: u32, sample_rate: f64, sample_size: u16) -> Self {
        SampleEntry {
            format,
            data_reference_index: 1,
            codec,
            video: None,
            audio: Some(AudioParams { channels, sample_rate, sample_size, ..Default::default() }),
            bitrate: None,
        }
    }

    /// H.264 entry (`avc1`).
    pub fn avc(cfg: AvcConfig, width: u16, height: u16) -> Self {
        Self::video(FourCc(*b"avc1"), CodecConfig::Avc(cfg), width, height)
    }
    /// HEVC entry (`hvc1`, parameter sets in the config only).
    pub fn hevc(cfg: HevcConfig, width: u16, height: u16) -> Self {
        Self::video(FourCc(*b"hvc1"), CodecConfig::Hevc(cfg), width, height)
    }
    /// AV1 entry (`av01`).
    pub fn av1(cfg: Av1Config, width: u16, height: u16) -> Self {
        Self::video(FourCc(*b"av01"), CodecConfig::Av1(cfg), width, height)
    }
    /// ProRes entry; `fourcc` one of `apch`/`apcn`/`apcs`/`apco`/`ap4h`/`ap4x`.
    pub fn prores(fourcc: FourCc, width: u16, height: u16) -> Self {
        let mut e = Self::video(fourcc, CodecConfig::ProRes { fourcc }, width, height);
        if let Some(v) = e.video.as_mut()
            && matches!(&fourcc.0, b"ap4h" | b"ap4x")
        {
            v.depth = 32;
        }
        e
    }
    /// Avid DNxHD / DNxHR entry (`AVdn`, or `AVdh` for DNxHR).
    pub fn dnx(fourcc: FourCc, width: u16, height: u16) -> Self {
        let mut e = Self::video(fourcc, CodecConfig::Dnx { fourcc }, width, height);
        if let Some(v) = e.video.as_mut() {
            v.depth = 24;
        }
        e
    }
    /// Advanced Professional Video entry (`apv1` + `apvC`).
    pub fn apv(cfg: ApvConfig, width: u16, height: u16) -> Self {
        let mut e = Self::video(FourCc(*b"apv1"), CodecConfig::Apv(cfg), width, height);
        if let Some(v) = e.video.as_mut() {
            v.compressor_name = "APV".into();
        }
        e
    }
    /// Motion-JPEG entry (`jpeg`).
    pub fn jpeg(width: u16, height: u16) -> Self {
        let f = FourCc(*b"jpeg");
        Self::video(f, CodecConfig::Jpeg { fourcc: f }, width, height)
    }
    /// AAC entry (`mp4a` + `esds`) from an AudioSpecificConfig.
    pub fn aac(asc: Vec<u8>, channels: u32, sample_rate: u32) -> Self {
        Self::audio(FourCc(*b"mp4a"), CodecConfig::Aac(AacConfig::from_asc(asc)), channels, sample_rate as f64, 16)
    }
    /// PCM entry. The muxer picks the on-disk format (`sowt`/`twos`/`lpcm` for MOV, `ipcm`/`fpcm` for MP4).
    pub fn pcm(cfg: PcmConfig) -> Self {
        Self::audio(FourCc(*b"lpcm"), CodecConfig::Pcm(cfg), cfg.channels, cfg.sample_rate, cfg.bits)
    }

    /// Serialize as a complete sample entry box. For PCM the format is chosen from the brand.
    pub(crate) fn write(&self, b: &mut BoxBuf, qt: bool) -> Result<()> {
        if let CodecConfig::Unknown { fourcc, raw } = &self.codec {
            let m = b.start(&fourcc.0);
            b.bytes(raw);
            b.end(m);
            return Ok(());
        }
        if let CodecConfig::Timecode(tc) = &self.codec {
            let m = b.start(b"tmcd");
            b.zeros(6);
            b.u16(self.data_reference_index);
            b.u32(0);
            b.u32(tc.flags);
            b.u32(tc.timescale);
            b.u32(tc.frame_duration);
            b.u8(tc.frames_per_second);
            b.u8(0);
            if let Some(name) = &tc.name {
                let n = b.start(b"name");
                b.u16(name.len() as u16);
                b.u16(0);
                b.bytes(name.as_bytes());
                b.end(n);
            }
            b.end(m);
            return Ok(());
        }
        if let Some(v) = &self.video {
            let m = b.start(&self.format.0);
            b.zeros(6);
            b.u16(self.data_reference_index);
            b.u16(0);
            b.u16(0);
            b.u32(v.vendor.as_u32());
            b.u32(v.temporal_quality);
            b.u32(v.spatial_quality);
            b.u16(v.width);
            b.u16(v.height);
            b.u32(v.horiz_resolution);
            b.u32(v.vert_resolution);
            b.u32(0);
            b.u16(v.frame_count);
            let name = v.compressor_name.as_bytes();
            let n = name.len().min(31);
            let mut nb = [0u8; 32];
            nb[0] = n as u8;
            nb[1..1 + n].copy_from_slice(&name[..n]);
            b.bytes(&nb);
            b.u16(v.depth);
            b.i16(-1);
            match &self.codec {
                CodecConfig::Avc(c) => b.leaf(b"avcC", &c.to_bytes()),
                CodecConfig::Hevc(c) => b.leaf(b"hvcC", &c.to_bytes()),
                CodecConfig::Av1(c) => b.leaf(b"av1C", &c.to_bytes()),
                CodecConfig::Vp9(c) => b.leaf(b"vpcC", &c.to_bytes()),
                CodecConfig::Apv(c) => b.leaf(b"apvC", &c.to_bytes()),
                _ => {}
            }
            if let Some(g) = v.gamma {
                b.leaf(b"gama", &g.to_be_bytes());
            }
            if let Some(fi) = v.field_info {
                b.leaf(b"fiel", &[fi.fields, fi.detail]);
            }
            if let Some(ci) = &v.color {
                let m = b.start(b"colr");
                match ci {
                    ColorInfo::Nclx { primaries, transfer, matrix, full_range } => {
                        b.bytes(b"nclx");
                        b.u16(*primaries);
                        b.u16(*transfer);
                        b.u16(*matrix);
                        b.u8(if *full_range { 0x80 } else { 0 });
                    }
                    ColorInfo::Nclc { primaries, transfer, matrix } => {
                        b.bytes(b"nclc");
                        b.u16(*primaries);
                        b.u16(*transfer);
                        b.u16(*matrix);
                    }
                    ColorInfo::Icc { kind, profile } => {
                        b.bytes(&kind.0);
                        b.bytes(profile);
                    }
                }
                b.end(m);
            }
            if let Some(md) = &v.mastering_display {
                b.leaf(b"mdcv", &md.to_bytes());
            }
            if let Some((cll, fall)) = v.content_light {
                let mut d = cll.to_be_bytes().to_vec();
                d.extend_from_slice(&fall.to_be_bytes());
                b.leaf(b"clli", &d);
            }
            if let Some((h, vv)) = v.pixel_aspect {
                let m = b.start(b"pasp");
                b.u32(h);
                b.u32(vv);
                b.end(m);
            }
            if let Some(c) = v.clean_aperture {
                let m = b.start(b"clap");
                b.u32(c.width.0);
                b.u32(c.width.1);
                b.u32(c.height.0);
                b.u32(c.height.1);
                b.i32(c.horiz_offset.0);
                b.u32(c.horiz_offset.1);
                b.i32(c.vert_offset.0);
                b.u32(c.vert_offset.1);
                b.end(m);
            }
            if let Some(br) = self.bitrate {
                write_btrt(b, br);
            }
            b.end(m);
            return Ok(());
        }
        let a = self.audio.clone().ok_or_else(|| Error::Mux("sample entry has neither video nor audio params".into()))?;
        // Choose the on-disk format and sound description version.
        let (format, version, pcm) = match &self.codec {
            CodecConfig::Pcm(p) => {
                if qt {
                    if p.bits == 16 && !p.float {
                        (if p.big_endian { *b"twos" } else { *b"sowt" }, 0u16, Some(*p))
                    } else if p.bits == 8 && !p.float {
                        (*b"raw ", 0, Some(*p))
                    } else {
                        (*b"lpcm", 2, Some(*p))
                    }
                } else {
                    (if p.float { *b"fpcm" } else { *b"ipcm" }, 0, Some(*p))
                }
            }
            _ => (self.format.0, if qt { a.qt_version.min(2) } else { 0 }, None),
        };
        let m = b.start(&format);
        b.zeros(6);
        b.u16(self.data_reference_index);
        if version == 2 {
            let p = pcm.unwrap_or_default();
            b.u16(2);
            b.u16(0);
            b.u32(0);
            b.u16(3);
            b.u16(16);
            b.i16(-2);
            b.u16(0);
            b.u32(65536);
            b.u32(72);
            b.f64(p.sample_rate);
            b.u32(p.channels);
            b.u32(0x7F00_0000);
            b.u32(p.bits as u32);
            let flags = (p.float as u32) | ((p.big_endian as u32) << 1) | (((p.signed && !p.float) as u32) << 2) | 8;
            b.u32(flags);
            b.u32(p.bytes_per_frame());
            b.u32(1);
        } else {
            b.u16(version);
            b.u16(0);
            b.u32(0);
            b.u16(a.channels.min(u16::MAX as u32) as u16);
            b.u16(match pcm {
                Some(p) => p.bits,
                None => {
                    if a.sample_size == 0 {
                        16
                    } else {
                        a.sample_size
                    }
                }
            });
            b.i16(if version == 1 { a.compression_id } else { 0 });
            b.u16(0);
            let sr = if a.sample_rate < 65536.0 { (a.sample_rate * 65536.0).round() as u32 } else { 0 };
            b.u32(sr);
            if version == 1 {
                b.u32(a.samples_per_packet);
                b.u32(a.bytes_per_packet);
                b.u32(a.bytes_per_frame);
                b.u32(a.bytes_per_sample);
            }
        }
        match &self.codec {
            CodecConfig::Aac(c) => {
                b.leaf(b"esds", &esds_payload(0x40, &c.asc, c.max_bitrate, c.avg_bitrate, c.buffer_size));
            }
            CodecConfig::Mp3 if format == *b"mp4a" => {
                b.leaf(b"esds", &esds_payload(0x6B, &[], 0, 0, 0));
            }
            CodecConfig::Alac { cookie } => {
                let m = b.start_full(b"alac", 0, 0);
                b.bytes(cookie);
                b.end(m);
            }
            CodecConfig::Opus(o) => b.leaf(b"dOps", &o.to_bytes()),
            CodecConfig::Flac(fl) => {
                let m = b.start_full(b"dfLa", 0, 0);
                b.bytes(&fl.metadata_blocks);
                b.end(m);
            }
            CodecConfig::Ac3 { dac3 } => b.leaf(b"dac3", dac3),
            CodecConfig::Eac3 { dec3 } => b.leaf(b"dec3", dec3),
            CodecConfig::Pcm(p) if !qt => {
                let m = b.start_full(b"pcmC", 0, 0);
                b.u8(if p.big_endian { 0 } else { 1 });
                b.u8(p.bits as u8);
                b.end(m);
            }
            CodecConfig::Pcm(p) if p.channels == 6 => {
                // QuickTime channel layout: kAudioChannelLayoutTag_MPEG_5_1_A (L R C LFE Ls Rs)
                let m = b.start_full(b"chan", 0, 0);
                b.u32((121 << 16) | 6);
                b.u32(0);
                b.u32(0);
                b.end(m);
            }
            _ => {}
        }
        if let Some(br) = self.bitrate {
            write_btrt(b, br);
        }
        b.end(m);
        Ok(())
    }
}

fn write_btrt(b: &mut BoxBuf, br: BitRate) {
    let m = b.start(b"btrt");
    b.u32(br.buffer_size);
    b.u32(br.max_bitrate);
    b.u32(br.avg_bitrate);
    b.end(m);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn roundtrip(e: &SampleEntry, qt: bool, handler: &[u8; 4]) -> SampleEntry {
        let mut b = BoxBuf::new();
        e.write(&mut b, qt).unwrap();
        let bx = crate::bytes::boxes(&b.buf).next().unwrap().unwrap();
        parse_sample_entry(bx.kind, bx.payload, FourCc(*handler), qt).unwrap()
    }

    #[test]
    fn avcc_roundtrip() {
        let cfg = AvcConfig::new(vec![vec![0x67, 0x64, 0x00, 0x1F, 0xAC]], vec![vec![0x68, 0xEB]], 4);
        assert_eq!(cfg.profile, 0x64);
        assert_eq!(AvcConfig::parse(&cfg.to_bytes()).unwrap(), cfg);
        let mut e = SampleEntry::avc(cfg, 1920, 1080);
        e.video.as_mut().unwrap().color = Some(ColorInfo::Nclx { primaries: 1, transfer: 1, matrix: 1, full_range: false });
        e.video.as_mut().unwrap().pixel_aspect = Some((4, 3));
        e.video.as_mut().unwrap().field_info = Some(FieldInfo { fields: 2, detail: 9 });
        e.video.as_mut().unwrap().clean_aperture = Some(CleanAperture { width: (1920, 1), height: (1080, 1), horiz_offset: (-2, 1), vert_offset: (0, 1) });
        e.video.as_mut().unwrap().mastering_display = Some(MasteringDisplay::bt2020(1000.0, 0.0001));
        e.video.as_mut().unwrap().content_light = Some((1000, 400));
        e.bitrate = Some(BitRate { buffer_size: 1, max_bitrate: 2, avg_bitrate: 3 });
        assert_eq!(roundtrip(&e, false, b"vide"), e);
        assert_eq!(MasteringDisplay::bt2020(1000.0, 0.0001).max_nits(), 1000.0);
    }

    #[test]
    fn hvcc_roundtrip() {
        let cfg = HevcConfig {
            general_profile_idc: 1,
            general_profile_compatibility_flags: 0x6000_0000,
            general_constraint_indicator_flags: 0x9000_0000_0000,
            general_level_idc: 93,
            chroma_format_idc: 1,
            bit_depth_luma: 8,
            bit_depth_chroma: 8,
            length_size: 4,
            num_temporal_layers: 1,
            temporal_id_nested: true,
            arrays: vec![
                HevcNalArray { completeness: true, nal_type: 32, nalus: vec![vec![0x40, 1]] },
                HevcNalArray { completeness: true, nal_type: 33, nalus: vec![vec![0x42, 1, 2]] },
                HevcNalArray { completeness: true, nal_type: 34, nalus: vec![vec![0x44, 1]] },
            ],
            ..Default::default()
        };
        let back = HevcConfig::parse(&cfg.to_bytes()).unwrap();
        assert_eq!(back, cfg);
        assert_eq!(back.sps(), vec![&[0x42u8, 1, 2][..]]);
    }

    #[test]
    fn av1_vp9_opus_roundtrip() {
        let a = Av1Config { seq_profile: 1, seq_level_idx_0: 8, high_bitdepth: true, config_obus: vec![1, 2], ..Default::default() };
        assert_eq!(Av1Config::parse(&a.to_bytes()).unwrap(), a);
        let v = VpcConfig {
            profile: 2,
            level: 31,
            bit_depth: 10,
            chroma_subsampling: 1,
            full_range: true,
            colour_primaries: 9,
            transfer_characteristics: 16,
            matrix_coefficients: 9,
            codec_init: vec![],
        };
        assert_eq!(VpcConfig::parse(&v.to_bytes()).unwrap(), v);
        let o = OpusConfig { output_channels: 2, pre_skip: 312, input_sample_rate: 48000, output_gain: 0, channel_mapping_family: 0, channel_mapping: vec![] };
        assert_eq!(OpusConfig::parse(&o.to_bytes()).unwrap(), o);
    }

    #[test]
    fn aac_esds_roundtrip() {
        let e = SampleEntry::aac(vec![0x12, 0x10], 2, 44100);
        match &e.codec {
            CodecConfig::Aac(a) => {
                assert_eq!((a.object_type, a.sample_rate, a.channel_config), (2, 44100, 2));
            }
            _ => panic!(),
        }
        assert_eq!(roundtrip(&e, false, b"soun"), e);
        assert_eq!(roundtrip(&e, true, b"soun"), e);
    }

    /// QuickTime's `.mp2` entry (what ffmpeg writes for MP2 in MOV) is MPEG audio, as `.mp3` is,
    /// even under a handler that does not say "sound" (#800).
    #[test]
    fn quicktime_mpeg_audio_entries() {
        for fourcc in [*b".mp3", *b".mp2"] {
            let e = SampleEntry::audio(FourCc(fourcc), CodecConfig::Mp3, 2, 48000.0, 16);
            for handler in [b"soun", b"\0\0\0\0"] {
                let back = roundtrip(&e, true, handler);
                assert_eq!(back.codec, CodecConfig::Mp3, "{fourcc:?} {handler:?}");
                assert_eq!(back.audio.map(|a| (a.channels, a.sample_rate)), Some((2, 48000.0)), "{fourcc:?} {handler:?}");
            }
        }
    }

    #[test]
    fn pcm_entries() {
        for (bits, float, be, qt) in [
            (16, false, false, true),
            (16, false, true, true),
            (24, false, false, true),
            (32, true, false, true),
            (24, false, true, false),
            (32, true, false, false),
        ] {
            let p = PcmConfig { bits, float, big_endian: be, signed: true, channels: 2, sample_rate: 48000.0 };
            let e = SampleEntry::pcm(p);
            let back = roundtrip(&e, qt, b"soun");
            assert_eq!(back.codec, CodecConfig::Pcm(p), "{bits} {float} {be} {qt}");
        }
    }

    #[test]
    fn timecode_and_prores() {
        let tc = TimecodeConfig { flags: 1, timescale: 30000, frame_duration: 1001, frames_per_second: 30, start_frame: None, name: Some("reel".into()) };
        let e = SampleEntry {
            format: FourCc(*b"tmcd"),
            data_reference_index: 1,
            codec: CodecConfig::Timecode(tc.clone()),
            video: None,
            audio: None,
            bitrate: None,
        };
        assert_eq!(roundtrip(&e, true, b"tmcd"), e);
        assert_eq!(tc.format_frame(107892), "01:00:00;00");
        let nd = TimecodeConfig { flags: 0, frames_per_second: 24, ..Default::default() };
        assert_eq!(nd.format_frame(86400), "01:00:00:00");
        let p = SampleEntry::prores(FourCc(*b"apch"), 1920, 1080);
        assert_eq!(roundtrip(&p, true, b"vide"), p);
    }

    #[test]
    fn asc_parse() {
        assert_eq!(parse_asc(&[0x11, 0x90]), Some((2, 48000, 2)));
        assert_eq!(parse_asc(&[]), None);
    }
}
