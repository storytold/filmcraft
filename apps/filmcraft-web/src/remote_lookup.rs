//! Bookkeeping for the stats of remote files that are not cached yet.
//!
//! The engine's `Services` are synchronous, so a file the stat cache does not know cannot be
//! answered on the spot: the first look starts one fetch (a listing of its folder, else `HEAD`)
//! and answers "loading"; looks while it runs wait; a definite answer from the server ends the
//! waiting. A path the server does not have (or could not be asked about) is reported missing
//! for a while instead of loading forever.
//!
//! Pure (no browser types), so it runs natively in `tests/it`.

use std::collections::{HashMap, HashSet};

/// A path the server answered 404 or 410 for is not asked again for this long (seconds).
pub const ABSENT_RETRY_S: f64 = 30.0;
/// A path whose lookup failed (network, server error) is not asked again for this long (seconds).
pub const FAILED_RETRY_S: f64 = 5.0;
/// Remembered misses; the table is cleared when it grows past this (hostile project files).
const MISSING_MAX: usize = 10_000;
/// Lookups that may run at once.
const INFLIGHT_MAX: usize = 64;

/// How a lookup ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    Found,
    /// The server has no such file (404, 410).
    Absent,
    /// The server could not be asked or gave no usable answer.
    Failed,
}

/// What a look at an uncached path should do.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Next {
    /// Start the lookup now (it is now marked as running); answer "loading".
    Fetch,
    /// A lookup is running (or the limit of concurrent ones is reached); answer "loading".
    Wait,
    /// Not there: answer "not found".
    Missing,
}

#[derive(Default)]
pub struct Lookups {
    inflight: HashSet<String>,
    /// Path → time until which it counts as missing.
    missing: HashMap<String, f64>,
}

impl Lookups {
    /// A look at `path`, which the stat cache does not have, at time `now` (seconds).
    pub fn miss(&mut self, path: &str, now: f64) -> Next {
        if let Some(until) = self.missing.get(path).copied() {
            if now < until {
                return Next::Missing;
            }
            self.missing.remove(path);
        }
        if self.inflight.contains(path) || self.inflight.len() >= INFLIGHT_MAX {
            return Next::Wait;
        }
        self.inflight.insert(path.to_string());
        Next::Fetch
    }

    /// The lookup of `path` ended.
    pub fn done(&mut self, path: &str, outcome: Outcome, now: f64) {
        self.inflight.remove(path);
        let hold = match outcome {
            Outcome::Found => {
                self.missing.remove(path);
                return;
            }
            Outcome::Absent => ABSENT_RETRY_S,
            Outcome::Failed => FAILED_RETRY_S,
        };
        if self.missing.len() >= MISSING_MAX {
            self.missing.clear();
        }
        self.missing.insert(path.to_string(), now + hold);
    }

    /// Forget a remembered miss (the file was written).
    pub fn forget(&mut self, path: &str) {
        self.missing.remove(path);
    }
}
