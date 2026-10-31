//! J / L shuttle speed: the one state machine behind the Program monitor, the Source monitor and
//! dynamic trimming (`trim.shuttle`).
//!
//! Two behaviours, chosen by Settings ▸ Playback ▸ "J and L step speed through zero (shuttle)"
//! (`playback.jklSignedSpeed`):
//!
//! * **Direction switching** (default, as in Premiere): L plays forward, J plays backward. Pressing
//!   the key for the direction already playing doubles the speed (1×, 2×, 4×, 8×); pressing the
//!   other key turns around at normal speed.
//! * **Signed speed** (opt-in): speed is one signed value on the ladder
//!   −8, −4, −2, −1, 0, 1, 2, 4, 8. L moves one rung up, J one rung down, so J while fast-forwarding
//!   slows down first and only reverses after passing through 0. Reaching 0 pauses (the playhead
//!   stays where it is); the next L or J starts from there at ±1×. A speed off the ladder (the
//!   ±¼× slow shuttle) steps to the nearest rung in the key's direction: from +¼×, L → 1× and
//!   J → 0 (pause).
//!
//! Slow shuttle (`playback.slowForward` / `playback.slowReverse`) always sets ±¼× in both modes, and
//! K always stops.

/// Which shuttle key was pressed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ShuttleKey {
    /// L: forward / faster.
    Forward,
    /// J: backward / slower.
    Reverse,
}

impl ShuttleKey {
    fn sign(self) -> f64 {
        match self {
            Self::Forward => 1.0,
            Self::Reverse => -1.0,
        }
    }
}

/// Fastest shuttle speed (× real time) either way.
pub const MAX_SHUTTLE_SPEED: f64 = 8.0;

/// The signed-speed ladder, slowest backward to fastest forward.
pub const SIGNED_LADDER: [f64; 9] = [-8.0, -4.0, -2.0, -1.0, 0.0, 1.0, 2.0, 4.0, 8.0];

/// Next shuttle speed after pressing `key` while playing at `current` (× real time, negative =
/// backward; pass 0 when stopped). `signed` selects signed-speed mode. A result of exactly 0 means
/// pause (signed mode only). Never returns a non-finite value or one beyond ±8×.
pub fn next_speed(current: f64, key: ShuttleKey, signed: bool) -> f64 {
    let current = if current.is_finite() { current.clamp(-MAX_SHUTTLE_SPEED, MAX_SHUTTLE_SPEED) } else { 0.0 };
    let dir = key.sign();
    if !signed {
        return if current * dir > 0.0 { (current * 2.0).clamp(-MAX_SHUTTLE_SPEED, MAX_SHUTTLE_SPEED) } else { dir };
    }
    match key {
        ShuttleKey::Forward => SIGNED_LADDER.iter().copied().find(|&v| v > current).unwrap_or(MAX_SHUTTLE_SPEED),
        ShuttleKey::Reverse => SIGNED_LADDER.iter().rev().copied().find(|&v| v < current).unwrap_or(-MAX_SHUTTLE_SPEED),
    }
}
