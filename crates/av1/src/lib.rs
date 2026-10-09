//! Clean-room AV1 decoder, implemented from the AV1 Bitstream & Decoding Process Specification
//! v1.0.0 with Errata 1 (AOMediaCodec/av1-spec).
//!
//! See the crate README for the conformance status of each decoding stage.

// Items used only by stages still being written (inter prediction, post filters).
#![allow(dead_code)]
#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable))]

pub mod accel;
mod bits;
mod cdef;
mod cdf;
mod decoder;
mod frame;
mod grain;
pub mod header;
mod inter;
mod inter_info;
mod intra;
mod mvpred;
mod par;
mod postfilter;
mod restoration;
mod spec_tables;
mod state;
mod stats;
mod symbol;
mod tile;
mod transform;

pub use decoder::Decoder;
pub use header::{ColorConfig, FrameHeader, SequenceHeader};
pub use stats::{DecodeStats, Stage};

/// Decoder errors.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum Error {
    #[error("truncated AV1 data")]
    Truncated,
    #[error("invalid AV1 data: {0}")]
    Invalid(&'static str),
    #[error("unsupported AV1 feature: {0}")]
    Unsupported(&'static str),
}

pub type Result<T> = std::result::Result<T, Error>;

/// A decoded (shown) frame: planar samples without row padding, `bit_depth` significant bits in
/// `u16`. Chroma planes are `((width + ssx) >> ssx) x ((height + ssy) >> ssy)`; they are empty
/// for monochrome streams.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Picture {
    pub width: u32,
    pub height: u32,
    pub bit_depth: u8,
    pub subsampling_x: u8,
    pub subsampling_y: u8,
    pub mono_chrome: bool,
    pub planes: [Vec<u16>; 3],
    pub color_primaries: u8,
    pub transfer_characteristics: u8,
    pub matrix_coefficients: u8,
    pub full_range: bool,
    /// Timestamp of the temporal unit that showed the picture ([`Decoder::decode_pts`]).
    pub pts: i64,
    /// Decoded in draft mode without deblocking / CDEF / loop restoration
    /// ([`Decoder::set_draft`]): approximate, for reduced-resolution playback only.
    pub draft: bool,
}

impl Picture {
    pub fn plane_width(&self, plane: usize) -> u32 {
        if plane == 0 { self.width } else { (self.width + self.subsampling_x as u32) >> self.subsampling_x }
    }
    pub fn plane_height(&self, plane: usize) -> u32 {
        if plane == 0 { self.height } else { (self.height + self.subsampling_y as u32) >> self.subsampling_y }
    }
}

/// Whether a temporal unit starts a random access point: its first frame is a shown key frame
/// (used for keyframe detection: containers may flag samples as sync that are not).
pub fn is_key_frame_unit(data: &[u8]) -> bool {
    let mut pos = 0;
    let mut seq_seen = false;
    while pos < data.len() {
        let h = data[pos];
        let obu_type = (h >> 3) & 0xf;
        let ext = h & 4 != 0;
        let has_size = h & 2 != 0;
        let mut p = pos + 1 + ext as usize;
        let size = if has_size {
            match bits::leb128(data.get(p..).unwrap_or(&[])) {
                Ok((v, n)) => {
                    p += n;
                    v as usize
                }
                Err(_) => return false,
            }
        } else {
            data.len().saturating_sub(p)
        };
        if p > data.len() {
            return false;
        }
        match obu_type {
            1 => seq_seen = true,
            3 | 6 => {
                let b = data.get(p).copied().unwrap_or(0);
                // show_existing_frame = 0, frame_type = KEY_FRAME (0), show_frame = 1
                let show_existing = b >> 7;
                let frame_type = (b >> 5) & 3;
                let show_frame = (b >> 4) & 1;
                let _ = seq_seen;
                return show_existing == 0 && frame_type == 0 && show_frame == 1;
            }
            _ => {}
        }
        pos = p.saturating_add(size);
    }
    false
}
