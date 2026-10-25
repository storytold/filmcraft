//! Clean-room Apple ProRes codec (implemented from SMPTE RDD 36).
//!
//! - **Decoder** ([`decode_frame`], [`decode_frame_with`]): ProRes 422 Proxy/LT/Standard/HQ and
//!   4444/4444 XQ including the alpha channel; progressive and interlaced frames; slice-parallel
//!   with the `threads` feature (rayon). Output is planar `u16` at 10 bits for 4:2:2 and 12 bits
//!   for 4:4:4 by default (any depth 8..=16 on request).
//! - **Encoder** ([`Encoder`]): ProRes 422 (all four flavours) and 4444/4444 XQ (optionally with
//!   alpha), progressive, with per-slice quantiser rate control targeting Apple's nominal rates.
//!
//! See the crate README for bitstream details, accuracy and limitations.

#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable))]

mod bits;
mod dct;
mod decode;
mod encode;
pub mod header;
mod tables;

pub use decode::{DecodeOptions, check_frame, decode_frame, decode_frame_into, decode_frame_with, probe};
pub use encode::{Encoder, EncoderConfig};
pub use header::{FrameHeader, PictureHeader};

/// Codec errors.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum Error {
    #[error("truncated ProRes data")]
    Truncated,
    #[error("invalid ProRes data: {0}")]
    Invalid(&'static str),
    #[error("unsupported ProRes feature: {0}")]
    Unsupported(&'static str),
    #[error("corrupt slice {slice} in picture {picture}")]
    Slice { picture: usize, slice: usize },
    #[error("invalid encoder input: {0}")]
    Input(String),
}

pub type Result<T> = std::result::Result<T, Error>;

/// Chroma subsampling of a frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ChromaFormat {
    /// Chroma planes are half width, full height.
    Yuv422,
    /// Chroma planes are full size.
    Yuv444,
}

impl ChromaFormat {
    /// Width of a chroma plane for a luma width.
    pub fn chroma_width(self, width: u32) -> u32 {
        match self {
            ChromaFormat::Yuv422 => width.div_ceil(2),
            ChromaFormat::Yuv444 => width,
        }
    }
}

/// Frame scan structure.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum Interlace {
    #[default]
    Progressive,
    /// Two fields; the first coded picture is the top field (even lines).
    TopFieldFirst,
    /// Two fields; the first coded picture is the bottom field (odd lines).
    BottomFieldFirst,
}

/// Alpha channel coding in the bitstream.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum AlphaType {
    #[default]
    None,
    Bits8,
    Bits16,
}

/// Colour description codes from the frame header (ITU-T H.273 / ISO 23091-2 code points;
/// 0 = unspecified in ProRes, 2 = unspecified in H.273).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ColorInfo {
    pub primaries: u8,
    pub transfer: u8,
    pub matrix: u8,
}

impl Default for ColorInfo {
    /// BT.709 primaries, transfer and matrix.
    fn default() -> Self {
        ColorInfo { primaries: 1, transfer: 1, matrix: 1 }
    }
}

/// ProRes profiles (flavours).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Profile {
    /// `apco`
    Proxy,
    /// `apcs`
    Lt,
    /// `apcn`
    Standard,
    /// `apch`
    Hq,
    /// `ap4h`
    P4444,
    /// `ap4x`
    P4444Xq,
}

impl Profile {
    pub const ALL: [Profile; 6] = [Profile::Proxy, Profile::Lt, Profile::Standard, Profile::Hq, Profile::P4444, Profile::P4444Xq];

    /// The QuickTime sample-entry four-character code.
    pub fn fourcc(self) -> [u8; 4] {
        *match self {
            Profile::Proxy => b"apco",
            Profile::Lt => b"apcs",
            Profile::Standard => b"apcn",
            Profile::Hq => b"apch",
            Profile::P4444 => b"ap4h",
            Profile::P4444Xq => b"ap4x",
        }
    }

    pub fn from_fourcc(f: &[u8; 4]) -> Option<Profile> {
        Profile::ALL.into_iter().find(|p| &p.fourcc() == f)
    }

    pub fn chroma(self) -> ChromaFormat {
        match self {
            Profile::P4444 | Profile::P4444Xq => ChromaFormat::Yuv444,
            _ => ChromaFormat::Yuv422,
        }
    }

    /// Apple's published nominal data rate at 1920×1080, 29.97 fps, in Mbit/s.
    pub fn nominal_mbps_1080p30(self) -> f64 {
        match self {
            Profile::Proxy => 45.0,
            Profile::Lt => 102.0,
            Profile::Standard => 147.0,
            Profile::Hq => 220.0,
            Profile::P4444 => 330.0,
            Profile::P4444Xq => 500.0,
        }
    }

    /// Nominal coded bits per 16×16 macroblock (the nominal rate spread over the 8160
    /// macroblocks of a 1920×1080 frame at 30000/1001 fps). Frame targets scale with area.
    pub fn nominal_bits_per_mb(self) -> u32 {
        (self.nominal_mbps_1080p30() * 1e6 * 1001.0 / 30000.0 / 8160.0).round() as u32
    }
}

/// A decoded (or to-be-encoded) picture: planar `u16` samples, one sample per element,
/// rows packed without padding.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Frame {
    pub width: u32,
    pub height: u32,
    pub chroma: ChromaFormat,
    /// Significant bits per sample of every plane (including alpha).
    pub bit_depth: u8,
    /// `width × height`
    pub y: Vec<u16>,
    /// `chroma_width × height`
    pub cb: Vec<u16>,
    /// `chroma_width × height`
    pub cr: Vec<u16>,
    /// `width × height` (full range: 0 = transparent, max = opaque)
    pub alpha: Option<Vec<u16>>,
    pub interlace: Interlace,
    pub color: ColorInfo,
    /// `aspect_ratio_information` from the frame header.
    pub aspect_ratio: u8,
    /// `frame_rate_code` from the frame header.
    pub frame_rate_code: u8,
}

impl Frame {
    /// A black (video-range) frame with the given geometry.
    pub fn new(width: u32, height: u32, chroma: ChromaFormat, bit_depth: u8, with_alpha: bool) -> Frame {
        let n = width as usize * height as usize;
        let cn = chroma.chroma_width(width) as usize * height as usize;
        let s = bit_depth.saturating_sub(8) as u32;
        Frame {
            width,
            height,
            chroma,
            bit_depth,
            y: vec![(16u32 << s) as u16; n],
            cb: vec![(128u32 << s) as u16; cn],
            cr: vec![(128u32 << s) as u16; cn],
            alpha: with_alpha.then(|| vec![((1u32 << bit_depth) - 1) as u16; n]),
            interlace: Interlace::Progressive,
            color: ColorInfo::default(),
            aspect_ratio: 0,
            frame_rate_code: 0,
        }
    }

    pub fn chroma_width(&self) -> u32 {
        self.chroma.chroma_width(self.width)
    }

    pub fn is_interlaced(&self) -> bool {
        self.interlace != Interlace::Progressive
    }
}
