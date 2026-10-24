//! Clean-room pure-Rust H.265 / HEVC decoder, implemented from ITU-T Rec. H.265 (ISO/IEC 23008-2).
//!
//! Supported: Main, Main 10 and Main 4:2:2 10 profiles (4:2:0 and 4:2:2, 8/10-bit; up to 12-bit
//! without range-extension tools), all coding tools of those profiles: CABAC, coding / transform quadtrees, AMP, PCM,
//! transform skip, scaling lists, sign data hiding, 35 intra modes with strong intra smoothing,
//! merge / AMVP / TMVP, weighted prediction, deblocking, SAO, tiles, wavefront parallel processing
//! (parsing), dependent slice segments, short- and long-term reference picture sets.
//!
//! ```no_run
//! let mut dec = filmcraft_hevc::Decoder::new();
//! let stream = std::fs::read("video.h265").unwrap();
//! for pic in dec.decode(&stream, 0).unwrap() {
//!     println!("{}x{} poc {}", pic.width, pic.height, pic.poc);
//! }
//! for pic in dec.flush() {
//!     let _ = pic;
//! }
//! ```

// Index loops over fixed-size blocks read more clearly than iterator chains in codec code.
#![allow(clippy::needless_range_loop, clippy::too_many_arguments)]
#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable))]

mod cabac;
mod decoder;
pub mod dpb;
mod error;
mod filter;
mod inter;
mod intra;
mod mvpred;
pub mod params;
mod picture;
pub mod slice;
mod slicedec;
#[rustfmt::skip]
#[allow(dead_code)]
mod spec_tables;
#[cfg(test)]
mod synth_tests;
mod tables;
mod transform;

pub use decoder::{DecodeStats, Decoder};
pub use error::{Error, Result};

/// Colour description from the VUI (ITU-T H.273 code points).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ColorInfo {
    /// `video_full_range_flag`.
    pub full_range: bool,
    pub primaries: u8,
    pub transfer: u8,
    pub matrix: u8,
}

/// Chroma sampling of a decoded picture (`chroma_format_idc`; 4:4:4 and monochrome are not
/// decoded).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChromaFormat {
    /// 4:2:0: chroma planes are half the luma width and height.
    C420,
    /// 4:2:2: chroma planes are half the luma width and the full luma height.
    C422,
}

impl ChromaFormat {
    /// The decoded format for `chroma_format_idc`, if this decoder handles it.
    pub fn from_idc(idc: u32) -> Option<ChromaFormat> {
        match idc {
            1 => Some(ChromaFormat::C420),
            2 => Some(ChromaFormat::C422),
            _ => None,
        }
    }
    /// log2(SubWidthC), log2(SubHeightC): the shifts from luma to chroma sample positions.
    #[inline(always)]
    pub fn shifts(self) -> (u32, u32) {
        match self {
            ChromaFormat::C420 => (1, 1),
            ChromaFormat::C422 => (1, 0),
        }
    }
    /// SubWidthC.
    #[inline(always)]
    pub fn sub_width(self) -> usize {
        1 << self.shifts().0
    }
    /// SubHeightC.
    #[inline(always)]
    pub fn sub_height(self) -> usize {
        1 << self.shifts().1
    }
}

/// Samples of one plane: 8-bit streams produce `U8`, higher bit depths `U16` (value range
/// `0..(1 << bit_depth)`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Plane {
    U8(Vec<u8>),
    U16(Vec<u16>),
}

impl Plane {
    pub fn len(&self) -> usize {
        match self {
            Plane::U8(v) => v.len(),
            Plane::U16(v) => v.len(),
        }
    }
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
    pub fn as_u8(&self) -> Option<&[u8]> {
        match self {
            Plane::U8(v) => Some(v),
            Plane::U16(_) => None,
        }
    }
    pub fn as_u16(&self) -> Option<&[u16]> {
        match self {
            Plane::U8(_) => None,
            Plane::U16(v) => Some(v),
        }
    }
    /// Sample `i` widened to u16.
    pub fn get(&self, i: usize) -> u16 {
        match self {
            Plane::U8(v) => v[i] as u16,
            Plane::U16(v) => v[i],
        }
    }
    /// Raw bytes as written by ffmpeg `rawvideo` (8-bit, or 16-bit little endian).
    pub fn to_le_bytes(&self) -> Vec<u8> {
        match self {
            Plane::U8(v) => v.clone(),
            Plane::U16(v) => v.iter().flat_map(|s| s.to_le_bytes()).collect(),
        }
    }
}

/// A decoded, cropped picture in planar 4:2:0 or 4:2:2 ([`Picture::chroma`]).
#[derive(Clone, Debug)]
pub struct Picture {
    /// Cropped luma width.
    pub width: u32,
    /// Cropped luma height.
    pub height: u32,
    pub chroma_width: u32,
    pub chroma_height: u32,
    /// Chroma sampling of the `u` / `v` planes.
    pub chroma: ChromaFormat,
    /// Luma bit depth (chroma uses the same storage type).
    pub bit_depth: u32,
    pub y: Plane,
    pub u: Plane,
    pub v: Plane,
    /// Strides in samples.
    pub y_stride: usize,
    pub uv_stride: usize,
    /// Presentation timestamp passed to [`Decoder::decode`] with the access unit of this picture.
    pub pts: i64,
    /// Picture order count.
    pub poc: i32,
    /// IRAP (IDR / CRA / BLA) picture.
    pub key: bool,
    pub color: ColorInfo,
    /// Sample aspect ratio (0, 0 when unspecified).
    pub sar: (u16, u16),
    /// Decoded in draft mode without deblocking / SAO ([`Decoder::set_draft`]): approximate,
    /// for reduced-resolution playback only. Never set without draft mode.
    pub draft: bool,
}
