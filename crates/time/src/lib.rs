//! Exact media time for FilmCraft: a re-export of [`craft_timeline::time`] from craft-libs.
//!
//! All time is an integer number of [`Tick`]s at [`TICKS_PER_SECOND`] = 254 016 000 000/s, so
//! edits, frame math and audio alignment never drift. Timecode is only a display of ticks. The
//! module, its tests and its documentation are in craft-libs; fixes go there.

#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable))]

pub use craft_timeline::time::*;
