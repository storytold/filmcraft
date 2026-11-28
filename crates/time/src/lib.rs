//! Exact media time for FilmCraft: a re-export of [`craft_timeline::time`] from craft-libs, and
//! [`format_time`] over craft-time's formatters with FilmCraft's compact feet and frames.
//!
//! All time is an integer number of [`Tick`]s at [`TICKS_PER_SECOND`] = 254 016 000 000/s, so
//! edits, frame math and audio alignment never drift. Timecode is only a display of ticks. The
//! module, its tests and its documentation are in craft-libs; fixes go there.

#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable))]

pub use craft_timeline::time::*;

/// `tick` as `display` shows it. Feet and frames are compact, as FilmCraft's timecode fields show
/// them (`1+14`, not `0001+14`); every other display is craft-time's.
pub fn format_time(tick: Tick, rate: FrameRate, drop_frame: bool, display: TimeDisplay, sample_rate: i64) -> String {
    if let TimeDisplay::Feet35 | TimeDisplay::Feet16 = display {
        return format_feet_frames(rate.frame_at(tick), display.frames_per_foot().unwrap_or(16));
    }
    craft_time::format_time(tick, rate, drop_frame, display, sample_rate)
}
