//! When the web recovery snapshot is written or marked clean (pure, so it is tested natively in
//! `tests/recovery_policy.rs`; the OPFS side lives in [`crate::recovery`]).
//!
//! - A dirty project is snapshotted at most every [`INTERVAL_S`] seconds, once per revision. A write
//!   that fails is retried after the interval, without waiting for another edit (#197).
//! - The meta is marked clean only to retire a snapshot this session wrote. A snapshot left by an
//!   earlier session that was not reopened (`?norecover`, `?fresh`) stays dirty (#193).

/// Seconds between snapshots of a changing project.
pub const INTERVAL_S: f64 = 5.0;

/// What the caller should do this frame.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Action {
    Nothing,
    /// Write the snapshot and a dirty meta for this revision, then report with [`Policy::written`].
    WriteSnapshot(u64),
    /// Write a clean meta, then report with [`Policy::cleaned`].
    WriteClean,
    /// Nothing now; wake up after this many seconds.
    WakeAfter(f64),
}

#[derive(Debug, Default)]
pub struct Policy {
    /// Revision whose snapshot was last written or is being written.
    last_revision: Option<u64>,
    last_time: Option<f64>,
    /// This session wrote a dirty meta that is not yet retired.
    owns_dirty_meta: bool,
    /// A clean-meta write is in flight.
    cleaning: bool,
    /// Snapshot writes started so far, and how many had started when the clean write did (a
    /// snapshot started after it makes the meta dirty again).
    snapshots: u64,
    snapshots_at_clean: u64,
}

impl Policy {
    /// Called every frame with the session's revisions and the time in seconds.
    pub fn tick(&mut self, now: f64, revision: u64, saved_revision: u64) -> Action {
        if revision != saved_revision {
            if self.last_revision == Some(revision) {
                return Action::Nothing;
            }
            let since = self.last_time.map_or(f64::INFINITY, |t| now - t);
            if since >= INTERVAL_S {
                self.last_revision = Some(revision);
                self.last_time = Some(now);
                self.owns_dirty_meta = true;
                self.snapshots += 1;
                return Action::WriteSnapshot(revision);
            }
            return Action::WakeAfter((INTERVAL_S - since).max(0.05));
        }
        if self.owns_dirty_meta && !self.cleaning {
            let since = self.last_time.map_or(f64::INFINITY, |t| now - t);
            if self.last_revision.is_some() || since >= INTERVAL_S {
                self.cleaning = true;
                self.snapshots_at_clean = self.snapshots;
                return Action::WriteClean;
            }
            return Action::WakeAfter((INTERVAL_S - since).max(0.05));
        }
        Action::Nothing
    }

    /// The snapshot write for `revision` finished.
    pub fn written(&mut self, revision: u64, ok: bool, now: f64) {
        if !ok && self.last_revision == Some(revision) {
            // retry after the interval, even if nothing else changes
            self.last_revision = None;
            self.last_time = Some(now);
        }
    }

    /// The clean-meta write finished.
    pub fn cleaned(&mut self, ok: bool, now: f64) {
        self.cleaning = false;
        if ok {
            self.owns_dirty_meta = self.snapshots != self.snapshots_at_clean;
        } else {
            // retry after the interval
            self.last_revision = None;
            self.last_time = Some(now);
        }
    }
}
