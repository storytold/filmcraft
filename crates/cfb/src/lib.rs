//! Compatibility facade for the shared `craft-cfb` reader and writer.
//! AAF application mapping stays in `filmcraft-interchange`; all CFB format logic lives upstream.
#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable))]

pub use craft_cfb::*;

#[cfg(test)]
mod tests;
