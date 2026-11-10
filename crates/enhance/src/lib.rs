//! Neural speech enhancement for FilmCraft: the **Enhance Speech (Neural)** audio effect.
//!
//! - [`catalog`]: the catalogue of downloadable models (source URL pinned to a commit, SHA-256,
//!   size, licence) and where they live in the per-user data directory. Weights are **never**
//!   bundled or committed. The download itself is the engine's (`enhance.downloadModel`), with the
//!   verified downloader of `filmcraft-speech`.
//! - [`NeuralEnhance`]: the audio effect. It turns the host's blocks into the model's frames,
//!   converts the sample rate when the sequence is not at the model's rate, and reports a constant
//!   latency. Without a downloaded model, or in a build without an inference backend, it passes
//!   audio through untouched.
//! - `dfn` (feature `deepfilter`): DeepFilterNet inference with libDF on tract (pure Rust, CPU).
//!
//! See `docs/neural-enhance.md` for the user-facing behaviour and the model comparison.

#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable))]

pub mod catalog;
#[cfg(feature = "deepfilter")]
mod dfn;
mod effect;
mod resample;
mod stream;

pub use effect::{Bypass, DEFAULT_ATTENUATION_DB, ID, MODEL_NAMES, NeuralEnhance, PARAMS, UNLIMITED_DB};

/// Whether this build can run the models (feature `deepfilter`).
pub fn available() -> bool {
    cfg!(feature = "deepfilter")
}
