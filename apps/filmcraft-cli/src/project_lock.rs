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

/// Do two paths name one existing file?
fn same_file(a: &str, b: &str) -> bool {
    a == b || matches!((std::fs::canonicalize(a), std::fs::canonicalize(b)), (Ok(a), Ok(b)) if a == b)
}

/// The path this run saves over, when that is the file it opens. `--save-as` somewhere else replaces
/// that file whatever it held, so there is no edit of its to lose.
///
/// The lock goes by this path as written and, for a symlink, by the file it points to as well
/// ([`lock_names`]).
///
/// A missing `--project` is an error here, before anything else, the same one `Backend::open` would
/// give. If the run went on, another one could create the project before `Backend::open` reads it,
/// and this run would edit and save it with no lock. Checking first also means the paths compared
/// below exist, so `canonicalize` tells exactly whether they are one file, however they are spelt.
/// A project that exists but isn't a file gets no lock file beside it, and opening it reports why.
fn saved_in_place(a: &Args) -> Result<Option<&Path>, String> {
    // `--bridge` with `--project` is a usage error, which `Backend::open` reports
    let Some(project) = a.opt("--project").filter(|_| a.opt("--bridge").is_none()) else { return Ok(None) };
    match Path::new(project).metadata() {
        Ok(meta) if meta.is_file() => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Err(format!("{project}: {e}")),
        _ => return Ok(None),
    }
    match a.opt("--save-as") {
        Some(target) if same_file(target, project) => Ok(Some(Path::new(target))),
        None if a.flag("--save") => Ok(Some(Path::new(project))),
        _ => Ok(None),
    }
}

/// The paths whose locks a save of `project` takes, in the order it takes them. A run through a
/// symlink and a run through the real path, or through another link, edit one file, so a link also
/// takes the lock of the file it points to (#4151). Its own lock stays: a save through a link
/// replaces the link with a regular file (#4005), and the runs before and after that save must
/// still share a lock. Every run takes the link's lock before the file's, so two runs can never each
/// hold a lock the other is waiting for.
fn lock_names(project: &Path) -> Vec<PathBuf> {
    let mut names = vec![project.to_path_buf()];
    if std::fs::symlink_metadata(project).is_ok_and(|m| m.file_type().is_symlink())
        && let Ok(target) = std::fs::canonicalize(project)
    {
        names.push(target);
    }
    names
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

/// Take the project's locks if this run saves over the file it opens, or exit if `--project` is
/// missing. Call it before opening the project and keep the result until the save is done. Blocks
/// while another run holds a lock, which is fine here: nothing else is running yet. A lock that
/// can't be had (a read-only folder, a file system without locks) is a warning and not an error, so
/// that saving works where it did.
pub fn for_save(a: &Args) -> Vec<File> {
    let Some(project) = saved_in_place(a).unwrap_or_else(|e| crate::fail(e)) else { return Vec::new() };
    let shown = project.display();
    let mut locks = Vec::new();
    for name in lock_names(project) {
        let waiting = || crate::diag(format_args!("filmcraft-cli: waiting for another run saving {shown}"));
        match lock(&name, waiting) {
            Ok(file) => locks.push(file),
            Err(e) => {
                crate::diag(format_args!("filmcraft-cli: warning: can't lock {} ({e}); another run saving it now could overwrite these edits", name.display()))
            }
        }
    }
    locks
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> Args {
        Args::parse(list.iter().map(|s| s.to_string()))
    }

    /// The moment between this check and `Backend::open` can't be hit on demand, so this pins the
    /// decision that closes it: a run never goes on to open a project that wasn't there, however
    /// its paths are spelt and whatever it saves.
    #[test]
    fn a_missing_project_stops_the_run_before_anything_opens_it() {
        let name = format!("fc-cli-gone-{}.fcproj", std::process::id());
        let gone = std::env::temp_dir().join(&name);
        let gone = gone.to_str().unwrap();
        let alias = std::env::temp_dir().join(".").join(&name);
        let alias = alias.to_str().unwrap();
        let dotted = format!("./{name}");
        // in a folder that doesn't exist either, so no part of either spelling can be resolved
        let new_dir = std::env::temp_dir().join(format!("fc-cli-new-dir-{}", std::process::id()));
        let in_new_dir = new_dir.join("p.fcproj");
        let in_new_dir = in_new_dir.to_str().unwrap();
        let in_new_dir_alias = new_dir.join(".").join("p.fcproj");
        let in_new_dir_alias = in_new_dir_alias.to_str().unwrap();
        for list in [
            &["--project", gone, "--save", "exec"][..],
            &["--project", gone, "--save-as", gone, "exec"],
            &["--project", gone, "--save-as", alias, "exec"],
            &["--project", &name, "--save-as", &dotted, "exec"],
            &["--project", in_new_dir, "--save-as", in_new_dir_alias, "exec"],
            &["--project", gone, "--save-as", "other.fcproj", "exec"],
            &["--project", gone, "exec"],
        ] {
            let error = saved_in_place(&args(list)).unwrap_err();
            assert!(error.starts_with(list[1]), "{list:?}: {error}");
        }
        // --project with --bridge is a usage error, which `Backend::open` reports
        assert_eq!(saved_in_place(&args(&["--bridge", "127.0.0.1:1", "--project", gone, "--save", "exec"])), Ok(None));
    }

    #[cfg(unix)]
    #[test]
    fn a_symlink_takes_its_own_lock_and_then_the_lock_of_the_file_it_points_to() {
        let dir = std::env::temp_dir().join(format!("fc-cli-lock-names-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("links")).unwrap();
        let real = dir.join("p.fcproj");
        std::fs::write(&real, b"{}").unwrap();
        let link = dir.join("links/link.fcproj");
        std::os::unix::fs::symlink("../p.fcproj", &link).unwrap();
        let chained = dir.join("chained.fcproj");
        std::os::unix::fs::symlink(&link, &chained).unwrap();
        let real_name = std::fs::canonicalize(&real).unwrap();
        assert_eq!(lock_names(&real), std::slice::from_ref(&real));
        assert_eq!(lock_names(&link), [link.clone(), real_name.clone()]);
        assert_eq!(lock_names(&chained), [chained.clone(), real_name]);
        let _ = std::fs::remove_dir_all(&dir);
    }

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
