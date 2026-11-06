//! FilmCraft side of Craft Link.
//!
//! Not wasm-compatible: `craft-ipc` uses local sockets and threads. No web crate may depend on it.

#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable))]

pub mod commands;
pub mod edit_in;
#[cfg(test)]
mod fixtures;
#[cfg(test)]
mod flow_tests;
pub mod host;
pub mod lift;
pub mod link_ops;
pub mod manager;
pub mod notice;
pub mod peer;
pub mod place;
pub mod prepare;
pub mod prompt;
pub mod record;
pub mod reopen;
pub mod source;
pub mod spawn;
pub mod ui;
