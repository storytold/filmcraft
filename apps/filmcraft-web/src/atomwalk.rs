//! Walking the top-level atoms of an ISO-BMFF (MP4/MOV) file to its `moov`. Pure (no browser
//! types), so it runs natively in `tests/it`.
//!
//! A file whose `moov` is last (`ftyp`, `wide`, `mdat`, `moov`) is not scanned: each hop reads
//! one atom header and jumps over the atom by its size, so a 40 MB `mdat` is never fetched.

/// Most atoms one walk visits; a file with more top-level atoms is walked no further.
pub const MAX_HOPS: usize = 256;
/// Bytes read at each atom offset: the 8-byte header, or the 16 of a 64-bit size.
pub const HEADER_LEN: u64 = 16;

/// What to do after the atom header read at some offset.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Step {
    /// The `moov` atom: `len` bytes at `off`, entirely inside the file.
    Moov { off: u64, len: u64 },
    /// Another atom: its header is at this offset.
    Next(u64),
    /// Nothing more to find (end, truncated, malformed or hostile size).
    Stop,
}

fn be32(b: &[u8]) -> Option<u64> {
    Some(u32::from_be_bytes(b.get(..4)?.try_into().ok()?) as u64)
}

/// The step after the atom whose header `h` (up to [`HEADER_LEN`] bytes) was read at `off` in a
/// file of `size` bytes. Every size is checked: a size below its own header, one that overflows
/// or leaves the file never moves the walk backwards or panics.
pub fn step(h: &[u8], off: u64, size: u64) -> Step {
    if off.saturating_add(8) > size || h.len() < 8 {
        return Step::Stop;
    }
    let (Some(n), Some(kind)) = (be32(h), h.get(4..8)) else { return Step::Stop };
    let (len, min) = match n {
        // 64-bit size follows the type
        1 => match (h.get(8..12).and_then(be32), h.get(12..16).and_then(be32)) {
            (Some(hi), Some(lo)) => ((hi << 32) | lo, 16),
            _ => return Step::Stop,
        },
        // extends to the end of the file
        0 => (size - off, 8),
        n => (n, 8),
    };
    if len < min {
        return Step::Stop;
    }
    let Some(next) = off.checked_add(len) else { return Step::Stop };
    if kind == b"moov" {
        return if next <= size { Step::Moov { off, len } } else { Step::Stop };
    }
    if next >= size { Step::Stop } else { Step::Next(next) }
}
