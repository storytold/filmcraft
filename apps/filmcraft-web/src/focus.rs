//! Which remote files the chunk cache ([`crate::fs`]) fetches with priority: the media of the
//! active sequence while a project loads. Pure (no browser types), so it runs natively in `tests/it`.
//!
//! The project panel opens every item for its thumbnail, and every sequence's media is warmed, so
//! without a priority the link is shared evenly and the file the first frame needs finishes
//! together with files nobody looks at. While a focus file is loading, other files fetch only a
//! little: base read-ahead, a couple of requests at once.

use std::collections::HashSet;

use crate::readahead::{BASE_AHEAD, Scan};

/// Chunks of one focus file fetched at once (more only queues behind the link).
pub const FOCUS_PARALLEL: u64 = 6;
/// Requests in flight, all other files together, while a focus file is loading.
pub const BACKGROUND_INFLIGHT: usize = 2;
/// Files one focus set holds (a project file is not trusted).
const FOCUS_MAX: usize = 10_000;

/// How the cache treats a file right now.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Class {
    /// No focus file is loading: as before.
    Normal,
    /// A file of the active sequence.
    Focus,
    /// Another file while a focus file is loading.
    Background,
}

impl Class {
    /// Chunks of one file fetched at once, given what its scan would use.
    pub fn parallel(self, parallel: u64) -> u64 {
        match self {
            Class::Normal => parallel,
            Class::Focus => parallel.clamp(1, FOCUS_PARALLEL),
            Class::Background => BACKGROUND_INFLIGHT as u64,
        }
    }

    /// Chunks to fetch past the end of a read: a background file never gets the sequential-scan boost.
    pub fn ahead(self, scan: &Scan, parallel: u64, chunk: u64, budget: usize) -> u64 {
        match self {
            Class::Background => BASE_AHEAD,
            _ => crate::readahead::ahead(scan, self.parallel(parallel), chunk, budget),
        }
    }

    /// Whether another request may start when `background` requests of other files are in flight.
    pub fn may_start(self, background: usize) -> bool {
        self != Class::Background || background < BACKGROUND_INFLIGHT
    }
}

/// The files to fetch first and which of them are still being warmed up.
#[derive(Debug, Default)]
pub struct Focus {
    files: HashSet<String>,
    loading: HashSet<String>,
}

impl Focus {
    /// Replace the focus: `files` are fetched first, `loading` (a subset) are being warmed up now.
    pub fn set(&mut self, files: &[String], loading: &[String]) {
        self.files = files.iter().take(FOCUS_MAX).cloned().collect();
        self.loading = loading.iter().filter(|p| self.files.contains(*p)).cloned().collect();
    }

    /// A focus file finished warming up.
    pub fn finish(&mut self, path: &str) {
        self.loading.remove(path);
    }

    /// Whether `path` is a focus file.
    pub fn contains(&self, path: &str) -> bool {
        self.files.contains(path)
    }

    /// The class of `path`; `focus_in_flight` says a chunk of some focus file is being fetched.
    pub fn class(&self, path: &str, focus_in_flight: bool) -> Class {
        if self.files.contains(path) {
            Class::Focus
        } else if !self.loading.is_empty() || focus_in_flight {
            Class::Background
        } else {
            Class::Normal
        }
    }
}
