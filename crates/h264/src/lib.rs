//! Clean-room pure-Rust H.264 / AVC decoder, implemented from ITU-T Rec. H.264 (ISO/IEC 14496-10).
//!
//! Supported: progressive (frame) coding, 8-bit 4:2:0, Baseline / Main / High profiles — CAVLC and CABAC,
//! I/P/B slices, 8x8 transform, custom scaling matrices, weighted prediction, spatial/temporal direct,
//! multiple slices, long-term references and all MMCOs, deblocking.
//!
//! ```no_run
//! let mut dec = filmcraft_h264::Decoder::new();
//! let stream = std::fs::read("video.h264").unwrap();
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

pub mod accel;
mod cabac;
mod cabac_mb;
#[rustfmt::skip]
mod cabac_tables;
mod cavlc;
#[rustfmt::skip]
mod cavlc_tables;
mod deblock;
mod decoder;
mod dpb;
mod error;
mod inter;
mod intra;
mod mbtypes;
pub mod params;
mod picture;
pub use picture::set_plane_allocator;
pub mod slice;
mod slicedec;
#[cfg(test)]
mod synth_tests;
mod tables;
mod transform;

pub use decoder::{DecodeStats, Decoder, default_threads};
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

/// A decoded, cropped picture in 8-bit planar 4:2:0.
#[derive(Clone, Debug)]
pub struct Picture {
    /// Cropped luma width.
    pub width: u32,
    /// Cropped luma height.
    pub height: u32,
    pub chroma_width: u32,
    pub chroma_height: u32,
    pub y: Vec<u8>,
    pub u: Vec<u8>,
    pub v: Vec<u8>,
    pub y_stride: usize,
    pub uv_stride: usize,
    /// Presentation timestamp passed to [`Decoder::decode`] with the access unit of this picture.
    pub pts: i64,
    /// Picture order count.
    pub poc: i32,
    /// IDR picture.
    pub key: bool,
    pub color: ColorInfo,
    /// Sample aspect ratio (0, 0 when unspecified).
    pub sar: (u16, u16),
    /// Decoded in draft mode ([`Decoder::set_draft`]): a non-reference picture whose deblocking
    /// filter was skipped. Its samples are approximate (not the conforming output); no other
    /// picture is affected.
    pub draft: bool,
}
