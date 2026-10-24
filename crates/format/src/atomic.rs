//! Crash-safe file replacement.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

static COUNTER: AtomicU64 = AtomicU64::new(0);

/// The temp file a save of `path` goes through (same directory, so the rename never crosses file
/// systems). Hidden, and unique per process and call.
fn temp_path(path: &Path) -> PathBuf {
    let dir = parent_dir(path);
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "project".into());
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    dir.join(format!(".{name}.{}-{n}.tmp", std::process::id()))
}

fn parent_dir(path: &Path) -> PathBuf {
    match path.parent() {
        Some(p) if !p.as_os_str().is_empty() => p.to_path_buf(),
        _ => PathBuf::from("."),
    }
}

/// Replace `path` with `data` atomically and durably:
/// write `data` to a temp file in the same directory, `fsync` it, rename it over `path`, then
/// `fsync` the directory so the rename itself survives a power cut. If anything fails the temp file
/// is removed and `path` is untouched.
pub fn atomic_write(path: &Path, data: &[u8]) -> io::Result<()> {
    let tmp = temp_path(path);
    let r = (|| {
        {
            // closed before the rename
            let mut f = OpenOptions::new().write(true).create_new(true).open(&tmp)?;
            f.write_all(data)?;
            f.sync_all()?;
        }
        fs::rename(&tmp, path)
    })();
    if let Err(e) = r {
        let _ = fs::remove_file(&tmp);
        return Err(e);
    }
    sync_dir(&parent_dir(path));
    Ok(())
}

/// Publish a complete new file without replacing an occupied final path, including a symlink.
/// Bytes are staged and synced in the destination directory before no-clobber publication.
/// A write failure leaves no partial final file; temporary-file cleanup is best effort.
#[cfg(not(target_arch = "wasm32"))]
pub fn atomic_write_new(path: &Path, data: &[u8]) -> io::Result<()> {
    atomic_write_new_with(path, |file| file.write_all(data).and_then(|_| file.sync_all()))
}

#[cfg(target_arch = "wasm32")]
pub fn atomic_write_new(_path: &Path, _data: &[u8]) -> io::Result<()> {
    Err(io::Error::new(io::ErrorKind::Unsupported, "native exclusive file creation is unavailable in the browser"))
}

#[cfg(not(target_arch = "wasm32"))]
fn atomic_write_new_with(path: &Path, write: impl FnOnce(&mut File) -> io::Result<()>) -> io::Result<()> {
    let dir = parent_dir(path);
    #[cfg(unix)]
    let mut staged = {
        use std::os::unix::fs::PermissionsExt;
        // Like an ordinary file creation: request 0666 and let the process umask decide the
        // effective mode (0644 with the usual 022, 0664 with 002).
        tempfile::Builder::new().permissions(fs::Permissions::from_mode(0o666)).tempfile_in(&dir)? // NOSONAR
    };
    #[cfg(not(unix))]
    let mut staged = tempfile::NamedTempFile::new_in(&dir)?;
    write(staged.as_file_mut())?;
    let _published = staged.persist_noclobber(path).map_err(|e| e.error)?;
    sync_dir(&dir);
    Ok(())
}

/// Best-effort directory sync; directory handles are not uniformly readable or syncable.
fn sync_dir(dir: &Path) {
    if let Ok(d) = File::open(dir) {
        let _ = d.sync_all();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmpdir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("filmcraft-atomic-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn replaces_and_leaves_no_temp_files() {
        let d = tmpdir("replace");
        let p = d.join("a.fcproj");
        atomic_write(&p, b"one").unwrap();
        atomic_write(&p, b"two").unwrap();
        assert_eq!(fs::read(&p).unwrap(), b"two");
        let names: Vec<_> = fs::read_dir(&d).unwrap().map(|e| e.unwrap().file_name()).collect();
        assert_eq!(names.len(), 1, "{names:?}");
        fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn failed_write_keeps_old_file() {
        let d = tmpdir("fail");
        let p = d.join("a.fcproj");
        atomic_write(&p, b"good").unwrap();
        // Renaming a file over a non-empty directory fails: the original target must survive.
        let dir_target = d.join("sub");
        fs::create_dir_all(dir_target.join("x")).unwrap();
        assert!(atomic_write(&dir_target, b"bad").is_err());
        assert_eq!(fs::read(&p).unwrap(), b"good");
        let tmp_left = fs::read_dir(&d).unwrap().filter(|e| e.as_ref().unwrap().file_name().to_string_lossy().ends_with(".tmp")).count();
        assert_eq!(tmp_left, 0);
        fs::remove_dir_all(&d).unwrap();
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn new_file_is_published_complete_without_clobbering_or_temp_leaks() {
        let d = tmpdir("new");
        let p = d.join("audio.wav");
        atomic_write_new(&p, b"complete").unwrap();
        assert_eq!(fs::read(&p).unwrap(), b"complete");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let reference = d.join("normal-create");
            OpenOptions::new().write(true).create_new(true).open(&reference).unwrap();
            assert_eq!(fs::metadata(&p).unwrap().permissions().mode() & 0o777, fs::metadata(&reference).unwrap().permissions().mode() & 0o777);
            fs::remove_file(reference).unwrap();
        }
        assert_eq!(atomic_write_new(&p, b"replacement").unwrap_err().kind(), io::ErrorKind::AlreadyExists);
        assert_eq!(fs::read(&p).unwrap(), b"complete");
        #[cfg(unix)]
        {
            let link = d.join("link.wav");
            std::os::unix::fs::symlink(&p, &link).unwrap();
            assert_eq!(atomic_write_new(&link, b"replacement").unwrap_err().kind(), io::ErrorKind::AlreadyExists);
            assert_eq!(fs::read(&p).unwrap(), b"complete");
        }
        let expected = if cfg!(unix) { 2 } else { 1 };
        assert_eq!(fs::read_dir(&d).unwrap().count(), expected, "staging file remained after publication");
        fs::remove_dir_all(&d).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn new_file_can_publish_in_a_directory_without_read_permission() {
        use std::os::unix::fs::PermissionsExt;
        let d = tmpdir("write-only-directory");
        let original_permissions = fs::metadata(&d).unwrap().permissions();
        fs::set_permissions(&d, fs::Permissions::from_mode(0o300)).unwrap();
        let p = d.join("audio.wav");
        let result = atomic_write_new(&p, b"complete");
        fs::set_permissions(&d, original_permissions).unwrap();
        assert!(result.is_ok(), "{result:?}");
        assert_eq!(fs::read(&p).unwrap(), b"complete");
        fs::remove_dir_all(&d).unwrap();
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn failed_staging_leaves_no_partial_destination() {
        let d = tmpdir("failed-new");
        let p = d.join("audio.wav");
        let error = atomic_write_new_with(&p, |file| {
            file.write_all(b"partial")?;
            Err(io::Error::other("simulated full disk"))
        })
        .unwrap_err();
        assert_eq!(error.to_string(), "simulated full disk");
        assert!(!p.exists());
        assert_eq!(fs::read_dir(&d).unwrap().count(), 0);
        fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn missing_directory_is_an_error() {
        let d = tmpdir("missing");
        assert!(atomic_write(&d.join("nope/a.fcproj"), b"x").is_err());
        fs::remove_dir_all(&d).unwrap();
    }
}
