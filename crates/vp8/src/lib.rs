//! Clean-room pure-Rust VP8 decoder, implemented from RFC 6386 ("VP8 Data Format and Decoding
//! Guide"), sections 1-19.
//!
//! Supported: the whole format — key frames and inter frames, all intra modes (16x16, chroma,
//! the ten 4x4 subblock modes), inter prediction from the last, golden and altref frames with
//! sign bias, NEAREST / NEAR / ZERO / NEW / SPLIT motion vectors, six-tap and bilinear
//! interpolation (versions 0-3, full-pixel chroma), segmentation, the normal and simple loop
//! filters with mode / reference deltas, multiple token partitions, probability persistence
//! (`refresh_entropy_probs`), golden / altref copies and hidden (altref) frames.
//!
//! ```no_run
//! let mut dec = filmcraft_vp8::Decoder::new();
//! let frame: Vec<u8> = Vec::new(); // one IVF / WebM frame
//! if let Some(pic) = dec.decode(&frame, 0).unwrap() {
//!     println!("{}x{}", pic.width, pic.height);
//! }
//! ```

#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable))]

mod boolcoder;
mod decoder;
mod error;
mod header;
mod loopfilter;
mod predict;
#[rustfmt::skip]
mod spec_tables;
mod tables;
mod transform;

pub use decoder::{DecodeStats, Decoder};
pub use error::{Error, Result};

/// A decoded picture: 8-bit 4:2:0, planes tight (stride = width).
#[derive(Clone, Debug)]
pub struct Picture {
    pub width: u32,
    pub height: u32,
    pub chroma_width: u32,
    pub chroma_height: u32,
    pub y: Vec<u8>,
    pub u: Vec<u8>,
    pub v: Vec<u8>,
    /// `pts` passed to [`Decoder::decode`] with the frame.
    pub pts: i64,
    pub key: bool,
    /// `color_space` of the last key frame (0: BT.601 YUV, 1: reserved).
    pub color_space: u8,
}

/// Stream parameters read from a key frame's uncompressed chunk (see [`keyframe_info`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct KeyframeInfo {
    pub width: u32,
    pub height: u32,
    /// Upscaling codes (9.1), informational only.
    pub horizontal_scale: u8,
    pub vertical_scale: u8,
    pub version: u8,
}

/// If `frame` is a key frame (a random access point), its parameters.
pub fn keyframe_info(frame: &[u8]) -> Option<KeyframeInfo> {
    let tag = header::FrameTag::parse(frame).ok()?;
    (tag.key_frame && tag.width > 0 && tag.height > 0).then_some(KeyframeInfo {
        width: u32::from(tag.width),
        height: u32::from(tag.height),
        horizontal_scale: tag.horizontal_scale,
        vertical_scale: tag.vertical_scale,
        version: tag.version,
    })
}

/// Whether `frame` is a key frame (decoding can start there).
pub fn is_keyframe(frame: &[u8]) -> bool {
    keyframe_info(frame).is_some()
}
