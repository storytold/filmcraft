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

use crate::cli::Global;
use crate::error::Failure;
use crate::io::Io;
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
fn same_file(a: &Path, b: &Path) -> bool {
    a == b || matches!((std::fs::canonicalize(a), std::fs::canonicalize(b)), (Ok(a), Ok(b)) if a == b)
}

/// The path this run saves over, when that is the file it opens. `--save-as` somewhere else replaces
/// that file whatever it held, so there is no edit of its to lose.
///
/// The lock goes by this path as written, not by where a symlink points. A save replaces the link
/// itself, so resolving it would give the runs before and after that save different locks.
///
/// A missing `--project` is an error here, before anything else, the same one `Backend::open` would
/// give. If the run went on, another one could create the project before `Backend::open` reads it,
/// and this run would edit and save it with no lock. Checking first also means the paths compared
/// below exist, so `canonicalize` tells exactly whether they are one file, however they are spelt.
/// A project that exists but isn't a file gets no lock file beside it, and opening it reports why.
fn saved_in_place(g: &Global) -> Result<Option<&Path>, Failure> {
    // `--bridge` with `--project` is a usage error, which clap reports
    let Some(project) = g.project.as_deref().filter(|_| g.bridge.is_none()) else { return Ok(None) };
    match Path::new(project).metadata() {
        Ok(meta) if meta.is_file() => {}
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => return Err(Failure::Io { path: project.to_path_buf(), source }),
        _ => return Ok(None),
    }
    match g.save_as.as_deref() {
        Some(target) if same_file(target, project) => Ok(Some(target)),
        None if g.save => Ok(Some(project)),
        _ => Ok(None),
    }
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

/// Take the project's lock if this run saves over the file it opens, or fail if `--project` is
/// missing. Call it before opening the project and keep the result until the save is done. Blocks
/// while another run holds the lock, which is fine here: nothing else is running yet. A lock that
/// can't be had (a read-only folder, a file system without locks) is a warning and not an error, so
/// that saving works where it did.
pub fn for_save(g: &Global, io: &mut Io<'_>) -> Result<Option<File>, Failure> {
    let Some(project) = saved_in_place(g)? else { return Ok(None) };
    let shown = project.display();
    match lock(project, || io.diag(format_args!("waiting for another run saving {shown}"))) {
        Ok(file) => Ok(Some(file)),
        Err(e) => {
            io.warning(format_args!("can't lock {shown} ({e}); another run saving it now could overwrite these edits"));
            Ok(None)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The options of a run on `project` with these save options.
    fn global(project: Option<&str>, save: bool, save_as: Option<&str>, bridge: bool) -> Global {
        Global {
            project: project.map(PathBuf::from),
            demo: false,
            save,
            save_as: save_as.map(PathBuf::from),
            bridge: bridge.then(|| crate::cli::BridgeAddr("127.0.0.1:1".into())),
            data_dir: None,
            compact: false,
        }
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
        for g in [
            global(Some(gone), true, None, false),
            global(Some(gone), false, Some(gone), false),
            global(Some(gone), false, Some(alias), false),
            global(Some(&name), false, Some(&dotted), false),
            global(Some(in_new_dir), false, Some(in_new_dir_alias), false),
            global(Some(gone), false, Some("other.fcproj"), false),
            global(Some(gone), false, None, false),
        ] {
            let error = saved_in_place(&g).unwrap_err().to_string();
            let project = g.project.as_deref().unwrap().display().to_string();
            assert!(error.starts_with(&project), "{g:?}: {error}");
        }
        // --project with --bridge is a usage error, which clap reports
        assert!(matches!(saved_in_place(&global(Some(gone), true, None, true)), Ok(None)));
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
