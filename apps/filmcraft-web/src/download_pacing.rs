//! Spacing of browser downloads (pure, so it is tested natively in `tests/download_pacing.rs`; the
//! anchor clicks live in [`crate::fs::download`]).
//!
//! Chrome drops anchor-click downloads that start in one tight burst, so an image-sequence export
//! (one file per frame) delivered only about half of its frames (#378). Each download is given a
//! slot [`GAP_MS`] after the previous one.

/// Milliseconds between two download clicks.
pub const GAP_MS: f64 = 125.0;

/// Hands out click times.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Pacer {
    next_free: f64,
}

impl Pacer {
    /// Reserve the next slot at time `now` (ms); returns how long to wait before clicking.
    pub fn reserve(&mut self, now: f64) -> f64 {
        let start = now.max(self.next_free);
        self.next_free = start + GAP_MS;
        start - now
    }
}
