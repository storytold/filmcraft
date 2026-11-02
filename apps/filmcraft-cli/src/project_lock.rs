//! One saving run at a time per project file.
//!
//! A `--save` run reads the project, edits it and writes the whole file back, so two at once would
//! silently drop the edits of whichever saved first (#3924). Each run holds an OS lock from before
//! its read until it exits; the next one waits, then reads what the first one saved.
//!
//! The lock is on a file next to the project, not on the project: a save replaces the project's
//! inode, and on Windows a locked file can't be read by other processes. The OS drops the lock when
//! the process ends, however it ends, so a crash leaves nothing stale. The file itself stays, since
//! removing it would let one run wait on the old file while another locks a new one.

use crate::args::Args;
use std::fs::{File, TryLockError};
use std::path::{Path, PathBuf};

/// Most bytes of the project's file name kept in the lock's name; the rest of the 255-byte limit
/// is left for the leading dot and `.lock`.
const MAX_NAME_BYTES: usize = 200;

fn lock_path(project: &Path) -> PathBuf {
    let name = project.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    project.with_file_name(format!(".{}.lock", &name[..name.floor_char_boundary(MAX_NAME_BYTES)]))
}

/// `path` with symlinks resolved, so two names for one file share a lock; as given if that fails.
fn real_path(path: &str) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| PathBuf::from(path))
}

/// The project file this run will read and then write back, if any. `--save-as` somewhere else
/// replaces that file whatever it held, so there is no edit of its to lose. A project that isn't a
/// file gets no lock file beside it: opening it fails with its own error.
fn saved_in_place(a: &Args) -> Option<PathBuf> {
    let project = real_path(a.opt("--project")?);
    let in_place = match a.opt("--save-as") {
        Some(target) => real_path(target) == project,
        None => a.flag("--save"),
    };
    (in_place && project.is_file()).then_some(project)
}

/// Lock `project`, calling `waiting` first if another run holds the lock.
fn lock(project: &Path, waiting: impl FnOnce()) -> std::io::Result<File> {
    let file = File::options().create(true).truncate(false).read(true).write(true).open(lock_path(project))?;
    match file.try_lock() {
        Ok(()) => {}
        Err(TryLockError::WouldBlock) => {
            waiting();
            file.lock()?;
        }
        Err(TryLockError::Error(e)) => return Err(e),
    }
    Ok(file)
}

/// Take the project's lock if this run saves over the file it opens. Call it before opening the
/// project and keep the result until the save is done. Blocks while another run holds the lock,
/// which is fine here: nothing else is running yet. A lock that can't be had (a read-only folder, a
/// file system without locks) is a warning and not an error, so that saving works where it did.
pub fn for_save(a: &Args) -> Option<File> {
    let project = saved_in_place(a)?;
    let shown = a.opt("--project").unwrap_or_default();
    let waiting = || crate::diag(format_args!("filmcraft-cli: waiting for another run saving {shown}"));
    match lock(&project, waiting) {
        Ok(file) => Some(file),
        Err(e) => {
            crate::diag(format_args!("filmcraft-cli: warning: can't lock {shown} ({e}); another run saving it now could overwrite these edits"));
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_lock_sits_next_to_the_project() {
        assert_eq!(lock_path(Path::new("/a/b/p.fcproj")), Path::new("/a/b/.p.fcproj.lock"));
        assert_eq!(lock_path(Path::new("p.fcproj")), Path::new(".p.fcproj.lock"));
    }

    #[test]
    fn a_long_project_name_still_gets_a_lock_name_the_file_system_takes() {
        for name in ["a".repeat(255), "é".repeat(127)] {
            let lock = lock_path(Path::new(&name));
            let lock = lock.to_str().unwrap();
            assert!(lock.len() <= 255, "{} bytes", lock.len());
            assert!(lock.ends_with(".lock"));
        }
    }
}
