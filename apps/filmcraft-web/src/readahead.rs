//! Read-ahead decisions of the chunk cache ([`crate::fs`]) and which containers a remote project
//! load warms up. Pure (no browser types), so it runs natively in `tests/it`.
//!
//! A Matroska open scans every cluster header, and each read that is not cached yet fails with
//! "loading" and is retried from the start of the scan. Those retries miss at a frontier that only
//! moves forward; seeing that, the cache fetches many chunks at once instead of a few.

/// Chunks fetched past the end of a read when nothing says the access is sequential.
pub const BASE_AHEAD: u64 = 3;
/// Misses in a row at a non-decreasing frontier that make an access sequential.
pub const SEQUENTIAL_RUN: u32 = 2;

/// The misses seen on one file.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Scan {
    /// The first missing chunk of the latest read that missed.
    last: Option<u64>,
    /// Consecutive misses at a frontier that did not go back and did not jump past the window.
    run: u32,
}

impl Scan {
    /// Record a read whose first missing chunk is `first`. A frontier that stays or moves forward
    /// by at most `window` chunks extends the run; going back or jumping further starts it over.
    pub fn note_miss(&mut self, first: u64, window: u64) {
        let continues = self.last.is_some_and(|l| first >= l && first - l <= window.saturating_add(1));
        self.run = if continues { self.run.saturating_add(1) } else { 1 };
        self.last = Some(first);
    }

    /// Whether the recent misses look like one sequential scan.
    pub fn sequential(&self) -> bool {
        self.run >= SEQUENTIAL_RUN
    }
}

/// Chunks to fetch past the end of a read: [`BASE_AHEAD`], or up to `parallel` while the access is
/// sequential, never more than a quarter of `budget` bytes (the cache) in chunks of `chunk` bytes.
pub fn ahead(scan: &Scan, parallel: u64, chunk: u64, budget: usize) -> u64 {
    if !scan.sequential() {
        return BASE_AHEAD;
    }
    let room = (budget as u64 / 4) / chunk.max(1);
    parallel.min(room).max(BASE_AHEAD)
}

/// Whether a remote project load warms up the container index of this media file: WebM, Matroska
/// and QuickTime, whose index is spread over the file or fetched in many round trips.
pub fn warms_at_load(path: &str) -> bool {
    let p = path.to_ascii_lowercase();
    [".webm", ".mkv", ".mov"].iter().any(|e| p.ends_with(e))
}
