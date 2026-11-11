#![cfg(not(target_arch = "wasm32"))]
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

//! FilmCraft side of the suite plug-in host (craft-abi 1). Native only: the lifecycle of
//! `craft-plugin-host` is native only, and no web crate may depend on this one.
//!
//! | Module | What it holds |
//! |---|---|
//! | [`bridge`] | how a host call of a guest reaches the `Session`, and the checkpoint a fault restores |
//! | [`services`] | `Services` for FilmCraft |
//! | [`doc`] | `doc.query` answers |
//! | [`host`] | `PluginHost::start`, the install flow |
//! | [`commands`] | the host's own commands (install, grant, list...) |
//! | [`formats`] | file types a plug-in opens (`format` contributions) |
//! | [`surfaces`] | panels and effects the plug-ins provide |
//! | [`effect`] | `effect` contributions as FilmCraft video effects |
//! | [`ui`] | the permission prompt, the manager view, panel windows |

pub mod bridge;
pub mod commands;
pub mod doc;
pub mod effect;
pub mod formats;
pub mod host;
pub mod services;
pub mod surfaces;
pub mod ui;

pub use host::{HostConfig, InstallError, PluginHost};
