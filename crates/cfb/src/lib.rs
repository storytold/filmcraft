//! Clean-room Compound File Binary format reader and writer ("structured storage", the container
//! of AAF files), written from the public Microsoft Open Specification **[MS-CFB]: Compound File
//! Binary File Format** (protocol revision 10.0 and earlier editions; the on-disk format has not
//! changed since 1.0).
//!
//! A compound file is a small FAT file system inside one file: fixed-size sectors (512 bytes in
//! version 3, 4096 bytes in version 4) chained by a file allocation table, a directory of 128-byte
//! entries arranged as one red-black tree of siblings per storage, and a "mini stream" with a mini
//! FAT for streams smaller than 4096 bytes.
//!
//! - [`CompoundFile::open`] parses a file held in memory (no copies of large streams; every chain
//!   is bounds- and cycle-checked, so damaged files give errors, never panics or endless loops).
//! - [`Writer`] builds a tree of storages and streams and lays it out in one pass
//!   ([`Writer::finish`]): stream sectors, mini stream, mini FAT, directory, FAT and DIFAT.
//!
//! Layer L0: no dependencies beyond `std`, no `unsafe`, builds for `wasm32-unknown-unknown`.

#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable))]

mod read;
mod write;

pub use read::{CompoundFile, Entry, EntryKind};
pub use write::{NodeId, Version, Writer};

use std::cmp::Ordering;
use std::fmt;

/// The 8-byte header signature (§2.2).
pub const SIGNATURE: [u8; 8] = [0xD0, 0xCF, 0x11, 0xE0, 0xA1, 0xB1, 0x1A, 0xE1];
/// Largest regular sector number (§2.1).
pub const MAXREGSECT: u32 = 0xFFFF_FFFA;
/// Sector used by the DIFAT.
pub const DIFSECT: u32 = 0xFFFF_FFFC;
/// Sector used by the FAT.
pub const FATSECT: u32 = 0xFFFF_FFFD;
/// End of a sector chain.
pub const ENDOFCHAIN: u32 = 0xFFFF_FFFE;
/// Unallocated sector.
pub const FREESECT: u32 = 0xFFFF_FFFF;
/// No sibling / child directory entry.
pub const NOSTREAM: u32 = 0xFFFF_FFFF;
/// Streams shorter than this live in the mini stream (§2.2 `Mini Stream Cutoff Size`).
pub const MINI_STREAM_CUTOFF: u32 = 4096;
/// Mini sector size (§2.2: mini sector shift 6).
pub const MINI_SECTOR_SIZE: usize = 64;
/// Longest directory entry name in UTF-16 code units, without the terminating NUL (§2.6.1).
pub const MAX_NAME_LEN: usize = 31;

/// Errors from the reader and writer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    /// Not a compound file (bad signature or byte order mark).
    NotCfb,
    /// Structurally invalid or truncated.
    Invalid(String),
    /// No entry with that name / path.
    NotFound(String),
    /// A name the format cannot store (too long, illegal characters, duplicate).
    BadName(String),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::NotCfb => f.write_str("not a compound file"),
            Error::Invalid(s) => write!(f, "invalid compound file: {s}"),
            Error::NotFound(s) => write!(f, "no entry {s:?}"),
            Error::BadName(s) => write!(f, "invalid entry name {s:?}"),
        }
    }
}

impl std::error::Error for Error {}

pub type Result<T> = std::result::Result<T, Error>;

/// Whether `head` starts with the compound file signature.
pub fn sniff(head: &[u8]) -> bool {
    head.len() >= 8 && head[..8] == SIGNATURE
}

fn upper(u: u16) -> u16 {
    if (b'a' as u16..=b'z' as u16).contains(&u) {
        return u - 32;
    }
    if u < 0x80 {
        return u;
    }
    match char::from_u32(u as u32) {
        Some(c) => {
            let mut up = c.to_uppercase();
            match (up.next(), up.next()) {
                (Some(x), None) if (x as u32) <= 0xFFFF => x as u32 as u16,
                _ => u,
            }
        }
        None => u,
    }
}

/// The sibling order of directory entries (§2.6.4): shorter names first, then a code-unit
/// comparison of the upper-cased names.
pub fn compare_names(a: &str, b: &str) -> Ordering {
    // No allocation: the iterators are walked in place (`child` calls this once per sibling).
    let (la, lb) = (a.encode_utf16().count(), b.encode_utf16().count());
    la.cmp(&lb).then_with(|| a.encode_utf16().map(upper).cmp(b.encode_utf16().map(upper)))
}

/// Check a directory entry name (§2.6.1): 1–31 UTF-16 code units, none of `/ \ : !`, and no NUL
/// (the stored name is NUL-terminated, so a NUL would cut the name short when read back).
pub fn validate_name(name: &str) -> Result<()> {
    let n = name.encode_utf16().count();
    if n == 0 || n > MAX_NAME_LEN || name.contains(['/', '\\', ':', '!', '\0']) {
        return Err(Error::BadName(name.to_string()));
    }
    Ok(())
}

#[cfg(test)]
mod tests;
