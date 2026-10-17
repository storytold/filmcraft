//! When the web recovery snapshot is written or marked clean (pure, so it is tested natively in
//! `tests/recovery_policy.rs`; the OPFS side lives in [`crate::recovery`]).
//!
//! - A dirty project is snapshotted at most every [`INTERVAL_S`] seconds, once per revision. A write
//!   that fails is retried after the interval, without waiting for another edit (#197).
//! - The meta is marked clean only to retire a snapshot this session wrote. A snapshot left by an
//!   earlier session that was not reopened (`?norecover`, `?fresh`) stays dirty and is held for
//!   download before this session may replace it (#193).

/// Seconds between snapshots of a changing project.
pub const INTERVAL_S: f64 = 5.0;

/// Engine exports may write user files in OPFS, but Autosave owns its recovery directory.
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
    first.is_some() && first != Some("recovery")
}

/// A skipped recovery file is held as an OPFS Blob so `?fresh` does not load its contents.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StartupRecoveryMode {
    Open,
    HoldForDownload,
}

pub fn startup_recovery_mode(skip_recovery: bool) -> StartupRecoveryMode {
    if skip_recovery { StartupRecoveryMode::HoldForDownload } else { StartupRecoveryMode::Open }
}

/// Keep recovery metadata small without changing the project name stored in the snapshot.
pub fn recovery_display_name(name: &str) -> &str {
    if name.len() <= 512 && !name.chars().any(|c| c.is_control() || c == '/' || c == '\\') { name } else { "Recovered" }
}

/// Keep a recovery warning visible without hiding a different status from the current action.
pub fn should_show_warning(current_status: &str, previous_warning: Option<&str>) -> bool {
    current_status.is_empty() || previous_warning == Some(current_status)
}

/// A dimension warning persists for the rejected revision and clears only after a later
/// revision validates. Saving the same invalid revision does not dismiss the warning.
#[derive(Clone, Debug, Default)]
pub struct ValidationWarning {
    rejected: Option<(u64, String)>,
}

impl ValidationWarning {
    pub fn reject(&mut self, revision: u64, error: String) {
        self.rejected = Some((revision, error));
    }

    pub fn refresh<E: std::fmt::Display>(&mut self, revision: u64, validate: impl FnOnce() -> Result<(), E>) {
        if self.rejected.as_ref().is_some_and(|(rejected, _)| *rejected != revision) {
            match validate() {
                Ok(()) => self.rejected = None,
                Err(error) => self.reject(revision, error.to_string()),
            }
        }
    }

    pub fn clear(&mut self) {
        self.rejected = None;
    }

    pub fn message(&self) -> Option<&str> {
        self.rejected.as_ref().map(|(_, error)| error.as_str())
    }
}

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

#[derive(Clone, Debug, Default)]
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
    /// A rejected revision is checked again only after the revision changes.
    rejected_revision: Option<u64>,
}

impl Policy {
    /// Validate a due snapshot before changing policy state, preserving the previous snapshot on rejection.
    pub fn tick_checked<E>(&mut self, now: f64, revision: u64, saved_revision: u64, validate: impl FnOnce() -> Result<(), E>) -> Result<Action, E> {
        if self.rejected_revision == Some(revision) {
            return Ok(Action::Nothing);
        }
        let mut next = self.clone();
        let action = next.tick(now, revision, saved_revision);
        let due_revision = match action {
            Action::WriteSnapshot(due_revision) => Some(due_revision),
            Action::WriteClean => Some(revision),
            Action::Nothing | Action::WakeAfter(_) => None,
        };
        if let Some(due_revision) = due_revision {
            if let Err(error) = validate() {
                self.rejected_revision = Some(due_revision);
                return Err(error);
            }
            next.rejected_revision = None;
        }
        *self = next;
        Ok(action)
    }

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
