//! Clean-room Ogg page writer, written from RFC 3533 (the Ogg encapsulation format), RFC 7845
//! (Ogg Opus) and the Vorbis I specification's Ogg mapping (section A).
//!
//! [`PageWriter`] writes the pages of a logical bitstream; [`opus_tags`] and [`vorbis_comment`]
//! build the comment headers. Reading Ogg is `craft_media::ogg`'s job.
//!
//! Layer L0: no dependencies beyond `std`; no `unsafe`; builds for `wasm32-unknown-unknown`.

#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable))]

mod write;
pub use write::{PAGE_TARGET_BYTES, PAGE_TARGET_PACKETS, PageWriter, opus_tags, vorbis_comment};

/// CRC-32 of RFC 3533 §6: polynomial 0x04C11DB7, initial value 0, no reflection, no final XOR.
pub fn crc32(data: &[u8]) -> u32 {
    let mut crc = 0u32;
    for &b in data {
        crc = (crc << 8) ^ CRC_TABLE[((crc >> 24) as u8 ^ b) as usize];
    }
    crc
}

/// `CRC_TABLE[b]`: the CRC register after feeding byte `b` into a zero register.
static CRC_TABLE: [u32; 256] = crc_table();

const fn crc_table() -> [u32; 256] {
    let mut t = [0u32; 256];
    let mut i = 0;
    while i < 256 {
        let mut r = (i as u32) << 24;
        let mut b = 0;
        while b < 8 {
            r = if r & 0x8000_0000 != 0 { (r << 1) ^ 0x04C1_1DB7 } else { r << 1 };
            b += 1;
        }
        t[i] = r;
        i += 1;
    }
    t
}

#[cfg(test)]
mod write_tests;
