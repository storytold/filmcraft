//! FilmCraft self-update.
//!
//! A first-party, dependency-light updater (pure-Rust TLS, gzip and tar; no C code, no `unsafe`):
//!
//! * [`release`] / [`net`]: find the newest stable release on GitHub and its published SHA-256, over
//!   HTTPS from `github.com/storytold/filmcraft` only; stream-download with size caps.
//! * [`extract`]: unpack the tarball safely (no path traversal, links or zip bombs).
//! * [`install`]: refuse to touch package-managed installs (Flatpak, AppImage, deb/rpm, Nix) and
//!   unwritable directories; take an instance lock.
//! * [`update`]: the check → stage flow, including a `--version` smoke test of the new binary.
//! * [`swap`]: swap-at-relaunch with `.prev` backups and automatic rollback of a release that fails
//!   to start twice.
//!
//! See `README.md` in this crate for how the pieces fit the app and for the trust model.

#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

pub mod error;
pub mod extract;
pub mod install;
pub mod net;
pub mod release;
pub mod swap;
pub mod update;

pub use error::{Result, UpdateError};
pub use install::{InstallKind, Target};
pub use release::Release;
pub use swap::{BootOutcome, Staged, apply, confirm, on_boot};
pub use update::{Availability, CheckState, Updater};
