//! Test-only helpers shared by every FilmCraft crate (used as a dev-dependency only).
//!
//! - [`oracle`]: find `ffmpeg` / `ffprobe` (env vars, `PATH`, well-known dirs) and skip — or, with
//!   `FILMCRAFT_REQUIRE_ORACLES=1`, fail — when they are missing ([`require_ffmpeg!`] & co.).
//! - [`fixtures`]: the shared fixture directory (`<workspace>/target/fixtures/<crate>`) and
//!   collision-free temporary names for concurrent generators.
//! - [`golden`]: RGBA8 images, PNG I/O, image-difference metrics and golden-file checks
//!   (`FILMCRAFT_BLESS=1` regenerates references).
//!
//! ffmpeg/ffprobe are external test oracles and fixture generators only; they are never linked
//! or shipped (AGENTS.md §2).

pub mod fixtures;
pub mod corpus;
pub mod golden;
pub mod oracle;
pub mod wav;

pub use fixtures::{fixtures_dir, temp_path, workspace_root};
pub use oracle::{ffmpeg, ffmpeg_or_skip, ffprobe, ffprobe_or_skip, oracles_required, skip};

/// `let ff = require_ffmpeg!();` — the ffmpeg path, or return from the test after printing
/// `SKIPPED: … ffmpeg not found` (a hard failure with `FILMCRAFT_REQUIRE_ORACLES=1`).
/// `require_ffmpeg!(value)` returns `value` instead of `()`.
#[macro_export]
macro_rules! require_ffmpeg {
    () => {
        match $crate::ffmpeg_or_skip(concat!(module_path!(), ":", line!())) {
            Some(p) => p,
            None => return,
        }
    };
    ($ret:expr) => {
        match $crate::ffmpeg_or_skip(concat!(module_path!(), ":", line!())) {
            Some(p) => p,
            None => return $ret,
        }
    };
}

/// Like [`require_ffmpeg!`] for ffprobe.
#[macro_export]
macro_rules! require_ffprobe {
    () => {
        match $crate::ffprobe_or_skip(concat!(module_path!(), ":", line!())) {
            Some(p) => p,
            None => return,
        }
    };
    ($ret:expr) => {
        match $crate::ffprobe_or_skip(concat!(module_path!(), ":", line!())) {
            Some(p) => p,
            None => return $ret,
        }
    };
}

/// `let (ffmpeg, ffprobe) = require_oracles!();` — both tools or skip.
#[macro_export]
macro_rules! require_oracles {
    () => {
        ($crate::require_ffmpeg!(), $crate::require_ffprobe!())
    };
}
