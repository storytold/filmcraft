//! Clean-room Avid DNxHD / DNxHR codec (VC-3, implemented from SMPTE ST 2019-1:2016 and its
//! Amendment 1:2023).
//!
//! - **Decoder** ([`decode_frame`]): every compression ID of the standard — the DNxHD (HD
//!   profile) IDs 1235–1260 including interlaced field and macroblock-adaptive coding, thin
//!   rasters and 4:4:4 RGB, and the resolution-independent DNxHR IDs 1270–1274 (LB, SQ, HQ, HQX,
//!   444) at 8, 10 and 12 bits in 4:2:0, 4:2:2 and 4:4:4, with DCT or lossless alpha. Macroblock
//!   scan lines decode in parallel with the `threads` feature (rayon).
//! - **Encoder** ([`Encoder`]): DNxHR SQ / HQ (8-bit 4:2:2) and HQX (10- or 12-bit 4:2:2), CBR
//!   at the standard's frame sizes, progressive.
//!
//! See the crate README for accuracy, performance and limitations.

#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable))]

mod bits;
mod dct;
mod decode;
mod encode;
pub mod header;
mod spec_tables;
mod tables;

pub use decode::{DecodeOptions, check_frame, decode_frame, decode_frame_with, probe};
pub use encode::{Encoder, EncoderConfig};
pub use header::{FieldCode, FrameHeader};
pub use tables::ri_frame_size;

/// Codec errors.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum Error {
    #[error("truncated VC-3 data")]
    Truncated,
    #[error("invalid VC-3 data: {0}")]
    Invalid(&'static str),
    #[error("unknown VC-3 compression ID {0}")]
    UnknownCid(u32),
    #[error("corrupt macroblock scan line {0}")]
    Row(usize),
    #[error("invalid encoder input: {0}")]
    Input(String),
}

pub type Result<T> = std::result::Result<T, Error>;

/// Chroma subsampling (`SSC`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ChromaFormat {
    /// Half width, half height.
    Yuv420,
    /// Half width, full height.
    Yuv422,
    /// Full size (also RGB).
    Yuv444,
}

impl ChromaFormat {
    pub fn chroma_width(self, width: u32) -> u32 {
        match self {
            ChromaFormat::Yuv444 => width,
            _ => width.div_ceil(2),
        }
    }
    pub fn chroma_height(self, height: u32) -> u32 {
        match self {
            ChromaFormat::Yuv420 => height.div_ceil(2),
            _ => height,
        }
    }
}

/// Colour volume (`CLV`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum ColorVolume {
    #[default]
    Bt709,
    /// BT.2020 non-constant luminance.
    Bt2020Ncl,
    /// BT.2020 constant luminance.
    Bt2020Cl,
    /// Described out of band.
    OutOfBand,
}

/// Frame scan structure.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum Interlace {
    #[default]
    Progressive,
    /// Interlaced; field 1 (the top field, even lines) comes first.
    TopFieldFirst,
}

/// DNxHR profiles (resolution-independent compression IDs).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Profile {
    /// DNxHR LB (CID 1274), 8-bit.
    Lb,
    /// DNxHR SQ (CID 1273), 8-bit.
    Sq,
    /// DNxHR HQ (CID 1272), 8-bit.
    Hq,
    /// DNxHR HQX (CID 1271), 10- or 12-bit.
    Hqx,
    /// DNxHR 444 (CID 1270), 10- or 12-bit 4:4:4.
    R444,
}

impl Profile {
    pub fn cid(self) -> u32 {
        match self {
            Profile::Lb => 1274,
            Profile::Sq => 1273,
            Profile::Hq => 1272,
            Profile::Hqx => 1271,
            Profile::R444 => 1270,
        }
    }
    pub fn from_cid(cid: u32) -> Option<Profile> {
        [Profile::Lb, Profile::Sq, Profile::Hq, Profile::Hqx, Profile::R444].into_iter().find(|p| p.cid() == cid)
    }
    pub fn name(self) -> &'static str {
        match self {
            Profile::Lb => "DNxHR LB",
            Profile::Sq => "DNxHR SQ",
            Profile::Hq => "DNxHR HQ",
            Profile::Hqx => "DNxHR HQX",
            Profile::R444 => "DNxHR 444",
        }
    }
    /// CBR frame size for a raster (equation 7.1).
    pub fn frame_size(self, width: u32, height: u32) -> u32 {
        let c0 = match self {
            Profile::Lb => 188416,
            Profile::Sq => 606208,
            Profile::Hq | Profile::Hqx => 917504,
            Profile::R444 => 1835008,
        };
        ri_frame_size(width, height, c0, false)
    }
}

/// A human-readable name for a compression ID ("DNxHD 1080p 145/120 8-bit", "DNxHR HQ"…).
pub fn cid_name(cid: u32) -> String {
    if let Some(p) = Profile::from_cid(cid) {
        return p.name().into();
    }
    match cid {
        1235 => "DNxHD 1080p 10-bit (CID 1235)".into(),
        1237 => "DNxHD 1080p 8-bit (CID 1237)".into(),
        1238 => "DNxHD 1080p 8-bit (CID 1238)".into(),
        1241 => "DNxHD 1080i 10-bit (CID 1241)".into(),
        1242 => "DNxHD 1080i 8-bit (CID 1242)".into(),
        1243 => "DNxHD 1080i 8-bit (CID 1243)".into(),
        1244 => "DNxHD 1080i thin raster (CID 1244)".into(),
        1250 => "DNxHD 720p 10-bit (CID 1250)".into(),
        1251 => "DNxHD 720p 8-bit (CID 1251)".into(),
        1252 => "DNxHD 720p 8-bit (CID 1252)".into(),
        1253 => "DNxHD 1080p 36 (CID 1253)".into(),
        1256 => "DNxHD 444 10-bit (CID 1256)".into(),
        1258 => "DNxHD 720p thin raster (CID 1258)".into(),
        1259 => "DNxHD 1080p thin raster (CID 1259)".into(),
        1260 => "DNxHD 1080i thin raster (CID 1260)".into(),
        _ => format!("VC-3 (CID {cid})"),
    }
}

/// A decoded (or to-be-encoded) picture: planar `u16` samples, rows packed without padding.
///
/// For RGB streams (`rgb == true`) the planes `y`, `cb`, `cr` hold G, B and R: the coded channel
/// order Ch1/Ch2/Ch3 as written by ffmpeg and read by its decoder (the standard's text names Ch1
/// red; see the README).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Frame {
    pub width: u32,
    pub height: u32,
    pub chroma: ChromaFormat,
    pub rgb: bool,
    /// Significant bits per sample of every plane (8, 10 or 12).
    pub bit_depth: u8,
    pub y: Vec<u16>,
    pub cb: Vec<u16>,
    pub cr: Vec<u16>,
    /// Full-range alpha (0 = transparent).
    pub alpha: Option<Vec<u16>>,
    pub interlace: Interlace,
    pub color_volume: ColorVolume,
    /// Compression ID of the stream.
    pub cid: u32,
    /// Pixel aspect ratio (0/0 = not signalled).
    pub par: (u16, u16),
}

impl Frame {
    /// A black (video-range) frame.
    pub fn new(width: u32, height: u32, chroma: ChromaFormat, bit_depth: u8, with_alpha: bool) -> Frame {
        let n = width as usize * height as usize;
        let cn = chroma.chroma_width(width) as usize * chroma.chroma_height(height) as usize;
        let s = bit_depth.saturating_sub(8) as u32;
        Frame {
            width,
            height,
            chroma,
            rgb: false,
            bit_depth,
            y: vec![(16u32 << s) as u16; n],
            cb: vec![(128u32 << s) as u16; cn],
            cr: vec![(128u32 << s) as u16; cn],
            alpha: with_alpha.then(|| vec![((1u32 << bit_depth) - 1) as u16; n]),
            interlace: Interlace::Progressive,
            color_volume: ColorVolume::Bt709,
            cid: 0,
            par: (0, 0),
        }
    }

    pub(crate) fn new_like(h: &FrameHeader, width: u32, height: u32) -> Frame {
        let mut f = Frame::new(width, height, h.chroma, h.bit_depth, h.alpha);
        f.rgb = h.rgb_planes();
        f.color_volume = h.color_volume;
        f.cid = h.cid;
        f.par = h.par;
        f
    }

    pub fn chroma_width(&self) -> u32 {
        self.chroma.chroma_width(self.width)
    }

    pub fn chroma_height(&self) -> u32 {
        self.chroma.chroma_height(self.height)
    }
}
#[cfg(test)]
mod ffmpeg_probe;
