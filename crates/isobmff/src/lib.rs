//! ISO BMFF (MP4) and QuickTime (MOV) demuxer and muxer: a re-export of
//! [`craft_media::isobmff`] from craft-libs. Supported boxes and limitations are in the
//! craft-media README.

#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable))]

pub use craft_media::isobmff::*;
