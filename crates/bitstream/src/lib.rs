//! Bit-level reading and writing: a re-export of [`craft_media::bitstream`] from craft-libs.

#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable))]

pub use craft_media::bitstream::*;
