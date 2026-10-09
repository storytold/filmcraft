//! Clean-room pure-Rust VP9 decoder, implemented from the public VP9 Bitstream & Decoding Process
//! Specification (v0.6, WebM project).
//!
//! Supported: profiles 0-3 (8 / 10 / 12-bit; 4:2:0, 4:2:2, 4:4:0, 4:4:4), all coding tools:
//! boolean decoder, forward and backward probability adaptation, segmentation, loop filter with
//! deltas, all intra modes, inter prediction (8-tap regular / smooth / sharp and bilinear,
//! scaled references), compound prediction, motion vector prediction, DCT / ADST 4..32 and the
//! Walsh-Hadamard lossless transform, tiles (tile columns decode in parallel), superframes,
//! hidden frames, show_existing_frame, intra-only frames, reference frame resizing and error
//! resilient / frame parallel modes.
//!
//! ```no_run
//! let mut dec = filmcraft_vp9::Decoder::new();
//! let chunk: Vec<u8> = Vec::new(); // one IVF / WebM frame
//! for pic in dec.decode(&chunk, 0).unwrap() {
//!     println!("{}x{} {}-bit", pic.width, pic.height, pic.bit_depth);
//! }
//! ```

#![allow(clippy::needless_range_loop, clippy::too_many_arguments)]
#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable))]

pub mod accel;
mod boolcoder;
mod decoder;
mod error;
mod frame;
mod header;
mod inter;
mod intra;
mod loopfilter;
mod probs;
#[rustfmt::skip]
#[allow(dead_code)]
mod spec_tables;
mod tables;
mod tile;
mod transform;
#[rustfmt::skip]
#[allow(dead_code, clippy::all)]
mod transform_gen;

pub use decoder::{DecodeStats, Decoder};

/// Stream parameters read from a key frame's uncompressed header (see [`keyframe_info`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct KeyframeInfo {
    pub profile: u8,
    pub width: u32,
    pub height: u32,
    pub render_width: u32,
    pub render_height: u32,
    pub bit_depth: u32,
    pub subsampling_x: bool,
    pub subsampling_y: bool,
    pub color: ColorInfo,
}

/// If the first frame of `chunk` (one container sample: a frame or a superframe) is a key frame,
/// i.e. a random access point, its stream parameters. Intra-only frames and show_existing_frame
/// are not random access points.
pub fn keyframe_info(chunk: &[u8]) -> Option<KeyframeInfo> {
    let first = header::split_superframe(chunk).into_iter().find(|f| !f.is_empty())?;
    let mut st = header::HeaderState::default();
    let h = header::parse_uncompressed(first, &mut st, &[None; 8]).ok()?;
    if h.show_existing_frame || h.frame_type != header::KEY_FRAME {
        return None;
    }
    Some(KeyframeInfo {
        profile: h.profile,
        width: h.width,
        height: h.height,
        render_width: h.render_width,
        render_height: h.render_height,
        bit_depth: h.color.bit_depth as u32,
        subsampling_x: h.color.subsampling_x,
        subsampling_y: h.color.subsampling_y,
        color: ColorInfo { color_space: h.color.color_space, full_range: h.color.color_range },
    })
}

/// Whether `chunk` starts with a key frame (decoding can start there).
pub fn is_keyframe(chunk: &[u8]) -> bool {
    keyframe_info(chunk).is_some()
}
pub use error::{Error, Result};

/// Colour information from the uncompressed header.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ColorInfo {
    /// `color_space` (0 unknown, 1 BT.601, 2 BT.709, 3 SMPTE-170, 4 SMPTE-240, 5 BT.2020,
    /// 6 reserved, 7 sRGB).
    pub color_space: u8,
    /// `color_range`: full swing when true, studio swing otherwise.
    pub full_range: bool,
}

/// Samples of one plane: 8-bit streams produce `U8`, higher bit depths `U16`.
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

/// A decoded, cropped planar picture.
#[derive(Clone, Debug)]
pub struct Picture {
    /// Frame width / height (decoded size).
    pub width: u32,
    pub height: u32,
    pub chroma_width: u32,
    pub chroma_height: u32,
    pub bit_depth: u32,
    pub subsampling_x: bool,
    pub subsampling_y: bool,
    pub y: Plane,
    pub u: Plane,
    pub v: Plane,
    /// Strides in samples.
    pub y_stride: usize,
    pub uv_stride: usize,
    /// `pts` passed to [`Decoder::decode`] with the chunk that produced this picture.
    pub pts: i64,
    pub key: bool,
    pub intra_only: bool,
    pub color: ColorInfo,
    /// Intended display size (render_size); has no effect on decoding.
    pub render_width: u32,
    pub render_height: u32,
    /// Decoded in draft mode without the loop filter ([`Decoder::set_draft`]): approximate, for
    /// reduced-resolution playback only. Never set without draft mode.
    pub draft: bool,
}
