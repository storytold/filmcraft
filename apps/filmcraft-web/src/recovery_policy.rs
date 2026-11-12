//! When the web recovery snapshot is written or marked clean (pure, so it is tested natively in
//! `tests/it/recovery_policy.rs`; the OPFS side lives in [`crate::recovery`]).
//!
//! - A dirty project is snapshotted at most every [`INTERVAL_S`] seconds, once per revision. A write
//!   that fails is retried after the interval, without waiting for another edit (#197).
//! - The meta is marked clean only to retire a snapshot this session wrote. A snapshot left by an
//!   earlier session that was not reopened (`?norecover`, `?fresh`) stays dirty and is held for
//!   download before this session may replace it (#193).

/// Seconds between snapshots of a changing project.
pub const INTERVAL_S: f64 = 5.0;

/// Engine exports may write user files in OPFS, but browser recovery owns snapshots and persisted media.
pub fn engine_opfs_write_allowed(path: &str) -> bool {
    let Some(relative) = path.strip_prefix("/opfs/") else { return true };
    let mut first = None;
    for segment in relative.split('/').filter(|segment| !segment.is_empty()) {
        if segment == "." || segment == ".." {
            return false;
        }
        if first.is_none() {
            first = Some(segment);
        }
    }
    first.is_some() && !matches!(first, Some("recovery" | "media"))
}

/// A skipped recovery file is held as an OPFS Blob so `?fresh` does not load its contents.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StartupRecoveryMode {
    Open,
    HoldForDownload,
}

/// Choose whether startup opens a snapshot or retains it for an explicit download.
pub fn startup_recovery_mode(skip_recovery: bool) -> StartupRecoveryMode {
    if skip_recovery { StartupRecoveryMode::HoldForDownload } else { StartupRecoveryMode::Open }
}

/// Preserve an existing raw snapshot after a metadata/read error, but do not hold recovery
/// when the raw lookup confirms that no snapshot exists.
#[derive(Debug, PartialEq, Eq)]
pub enum RecoveryReadFallback<T> {
    Proceed,
    Hold(T),
    Unavailable(String),
}

/// Translate a raw snapshot lookup into a hold, confirmed absence, or storage error.
pub fn recovery_read_fallback<T>(raw: Result<Option<T>, String>) -> RecoveryReadFallback<T> {
    match raw {
        Ok(Some(snapshot)) => RecoveryReadFallback::Hold(snapshot),
        Ok(None) => RecoveryReadFallback::Proceed,
        Err(error) => RecoveryReadFallback::Unavailable(error),
    }
}

/// Bound recovery filenames and reject blank names, path separators and control characters without renaming the stored project.
pub fn recovery_display_name(name: &str) -> &str {
    if !name.trim().is_empty() && name.len() <= 512 && !name.chars().any(|c| c.is_control() || c == '/' || c == '\\') { name } else { "Recovered" }
}

/// What the caller should do this frame.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Action {
    Nothing,
    /// Write dirty metadata, then the snapshot for this revision; report with [`Policy::written`].
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
    /// Clean metadata must wait until every earlier snapshot callback has finished.
    pending_snapshots: u64,
}

impl Policy {
    /// Called every frame with the session's revisions and the time in seconds.
    pub fn tick(&mut self, now: f64, revision: u64, saved_revision: u64) -> Action {
        if revision != saved_revision {
            if self.last_revision == Some(revision) {
                return Action::Nothing;
            }
            let since = self.last_time.map_or(f64::INFINITY, |t| now - t);
            // At most two serialized snapshots may wait for OPFS; a stalled store must not
            // retain another copy of the project after every interval.
            if since >= INTERVAL_S && self.pending_snapshots >= 2 {
                return Action::WakeAfter(0.5);
            }
            if since >= INTERVAL_S {
                self.last_revision = Some(revision);
                self.last_time = Some(now);
                self.owns_dirty_meta = true;
                self.snapshots = self.snapshots.saturating_add(1);
                self.pending_snapshots = self.pending_snapshots.saturating_add(1);
                return Action::WriteSnapshot(revision);
            }
            return Action::WakeAfter((INTERVAL_S - since).max(0.05));
        }
        if self.owns_dirty_meta && !self.cleaning {
            // A clean marker written while the dirty-meta callback still schedules its
            // snapshot could make that later snapshot invisible at the next startup.
            if self.pending_snapshots != 0 {
                return Action::Nothing;
            }
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
        self.pending_snapshots = self.pending_snapshots.saturating_sub(1);
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
