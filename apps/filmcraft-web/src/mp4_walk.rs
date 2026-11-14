//! Walking an MP4's top-level boxes to its `moov` before an import (pure, so it is tested natively
//! in `tests/it/mp4_walk.rs`; the reads live in `crate::import`).

/// What the walk does next, after reading the header of the top-level box at some offset.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Step {
    /// The box is the `moov`: load the file up to `end`.
    Moov { end: u64 },
    /// The next box starts at this offset.
    Next(u64),
    /// The header is too short, the box length is below 8, or no box header fits after this box
    /// in the file: stop walking.
    Stop,
}

fn be32(b: &[u8]) -> u64 {
    u32::from_be_bytes([b[0], b[1], b[2], b[3]]) as u64
}

/// The step after reading `header` (up to 16 bytes) at `off` in a file of `size` bytes. A length
/// field near `u64::MAX` stops the walk instead of overflowing the offset.
pub fn step(header: &[u8], off: u64, size: u64) -> Step {
    if header.len() < 8 {
        return Step::Stop;
    }
    let len = match be32(header) {
        1 if header.len() >= 16 => (be32(&header[8..]) << 32) | be32(&header[12..]),
        0 => size.saturating_sub(off),
        n => n,
    };
    if &header[4..8] == b"moov" {
        return Step::Moov { end: off.saturating_add(len).min(size) };
    }
    if len < 8 {
        return Step::Stop;
    }
    off.checked_add(len).filter(|next| next.saturating_add(8) <= size).map_or(Step::Stop, Step::Next)
}
