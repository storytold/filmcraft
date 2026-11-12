//! Clean-room pure-Rust CELT-only Opus encoder (RFC 6716, updated by RFC 8251) and the
//! `OpusHead` it writes (RFC 7845 §5.1). Decoding Opus is `craft_audio::opus`'s job.
//!
//! Layer L0: no dependencies beyond `std`; no `unsafe`; builds for `wasm32-unknown-unknown`.

#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable))]
pub(crate) mod celt;
mod encoder;
mod header;
pub(crate) mod range;

pub use encoder::{Encoder, EncoderConfig, FRAME_SIZE, MAX_BITRATE, MIN_BITRATE, PRE_SKIP};
pub use header::OpusHead;

/// Errors from encoding and header building.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    InvalidArgument(&'static str),
    InvalidPacket(&'static str),
    InvalidHeader(&'static str),
    Unsupported(&'static str),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::InvalidArgument(s) => write!(f, "invalid argument: {s}"),
            Error::InvalidPacket(s) => write!(f, "invalid packet: {s}"),
            Error::InvalidHeader(s) => write!(f, "invalid header: {s}"),
            Error::Unsupported(s) => write!(f, "unsupported: {s}"),
        }
    }
}

impl std::error::Error for Error {}

pub type Result<T> = std::result::Result<T, Error>;
