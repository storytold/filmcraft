//! Matroska (MKV) and WebM demuxer and muxer: a re-export of [`craft_media::matroska`] from
//! craft-libs. Supported elements and limitations are in the craft-media README.

#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable))]

pub use craft_media::matroska::*;
