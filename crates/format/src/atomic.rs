//! Crash-safe file replacement. Filesystems that cannot provide a durability sync (some SMB
//! shares) still get same-directory atomic replacement; all other I/O errors remain failures.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

static COUNTER: AtomicU64 = AtomicU64::new(0);

/// Most bytes of the target's file name kept in a temp name; the rest of the 255-byte limit is
/// left for the leading dot, pid, counter and `.tmp`.
const MAX_STEM_BYTES: usize = 200;

/// The temp file a save of `path` goes through (same directory, so the rename never crosses file
/// systems). Hidden, and unique per process and call.
fn temp_path(path: &Path) -> PathBuf {
    let dir = parent_dir(path);
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "project".into());
    // Keep the whole temp name under the 255-byte file name limit, cutting at a char boundary.
    let mut end = name.len().min(MAX_STEM_BYTES);
    while !name.is_char_boundary(end) {
        end -= 1;
    }
    let name = &name[..end];
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    dir.join(format!(".{name}.{}-{n}.tmp", std::process::id()))
}

fn parent_dir(path: &Path) -> PathBuf {
    match path.parent() {
        Some(p) if !p.as_os_str().is_empty() => p.to_path_buf(),
        _ => PathBuf::from("."),
    }
}

/// Sync a staging file before publication. Rust requests `F_FULLFSYNC` on macOS; SMB shares may
/// reject that stronger operation while supporting plain POSIX `fsync`, so retry it safely through
/// rustix. Only a filesystem that supports neither operation may omit the durability guarantee.
fn sync_if_supported(file: &File) -> io::Result<()> {
    #[cfg(target_os = "macos")]
    return sync_with_fallback(file.sync_all(), || rustix::fs::fsync(file).map_err(io::Error::from));

    #[cfg(not(target_os = "macos"))]
    file.sync_all()
}

/// Run a weaker synchronization fallback only when the primary operation is unsupported. A real
/// error from either operation remains a failed save.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn sync_with_fallback(primary: io::Result<()>, fallback: impl FnOnce() -> io::Result<()>) -> io::Result<()> {
    match primary {
        Ok(()) => Ok(()),
        Err(error) if unsupported_sync(&error) => match fallback() {
            Ok(()) => Ok(()),
            Err(fallback_error) if unsupported_sync(&fallback_error) => Ok(()),
            Err(fallback_error) => Err(fallback_error),
        },
        Err(error) => Err(error),
    }
}

/// Recognize only an explicit unsupported-operation result. Rust currently categorizes macOS
/// `ENOTSUP` (45) as `Uncategorized`, so retain that platform code as well.
fn unsupported_sync(error: &io::Error) -> bool {
    error.kind() == io::ErrorKind::Unsupported || cfg!(target_os = "macos") && error.raw_os_error() == Some(45)
}

/// Replace `path` with `data` atomically and durably:
/// write `data` to a temp file in the same directory, `fsync` it, rename it over `path`, then
/// `fsync` the directory so the rename itself survives a power cut. If anything fails the temp file
/// is removed and `path` is untouched.
pub fn atomic_write(path: &Path, data: &[u8]) -> io::Result<()> {
    // `rename` replaces the inode, so a new staging file must inherit an existing project's
    // access mode instead of silently broadening a private file to the process default.
    let permissions = match fs::metadata(path) {
        Ok(meta) => Some(meta.permissions()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => None,
        Err(e) => return Err(e),
    };
    let tmp = temp_path(path);
    let r = (|| {
        {
            // closed before the rename
            let mut f = OpenOptions::new().write(true).create_new(true).open(&tmp)?;
            if let Some(permissions) = permissions.as_ref() {
                f.set_permissions(permissions.clone())?;
            }
            f.write_all(data)?;
            sync_if_supported(&f)?;
        }
        fs::rename(&tmp, path)
    })();
    if let Err(e) = r {
        let _ = fs::remove_file(&tmp);
        return Err(e);
    }
    sync_dir(&parent_dir(path))?;
    Ok(())
}

/// Publish a complete new file without replacing an occupied final path, including a symlink.
/// Bytes are staged and synced in the destination directory before no-clobber publication.
/// A write failure leaves no partial final file; temporary-file cleanup is best effort.
#[cfg(not(target_arch = "wasm32"))]
pub fn atomic_write_new(path: &Path, data: &[u8]) -> io::Result<()> {
    atomic_write_new_with(path, |file| {
        file.write_all(data)?;
        sync_if_supported(file)
    })
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
    sync_dir(&dir)?;
    Ok(())
}

/// Sync the containing directory so publication survives a crash. Opening a directory as a file is
/// not uniformly available (and write-only directories intentionally reject it), but once opened,
/// real synchronization errors such as disconnection or device failure are reported.
#[cfg(unix)]
fn sync_dir(dir: &Path) -> io::Result<()> {
    sync_opened_dir(File::open(dir))
}

/// Apply the platform capability exceptions for an attempted directory handle while preserving
/// every other open error.
#[cfg(unix)]
fn sync_opened_dir(opened: io::Result<File>) -> io::Result<()> {
    match opened {
        Ok(directory) => sync_if_supported(&directory),
        Err(error) if matches!(error.kind(), io::ErrorKind::PermissionDenied | io::ErrorKind::Unsupported) => Ok(()),
        Err(error) => Err(error),
    }
}

/// Windows does not expose ordinary directories as syncable `File` handles through `std`.
#[cfg(not(unix))]
fn sync_dir(_dir: &Path) -> io::Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unsupported_primary_sync_uses_a_fallback_and_real_errors_still_fail() {
        let unsupported = io::Error::new(io::ErrorKind::Unsupported, "network share cannot fsync");
        let denied = io::Error::new(io::ErrorKind::PermissionDenied, "denied");
        assert!(sync_with_fallback(Err(unsupported), || Ok(())).is_ok());
        assert_eq!(sync_with_fallback(Err(denied), || Ok(())).unwrap_err().kind(), io::ErrorKind::PermissionDenied);
    }

    #[test]
    fn unsupported_fallback_is_accepted_but_other_fallback_errors_are_reported() {
        let unsupported = || io::Error::new(io::ErrorKind::Unsupported, "unsupported");
        assert!(sync_with_fallback(Err(unsupported()), || Err(unsupported())).is_ok());
        let error = sync_with_fallback(Err(unsupported()), || Err(io::Error::new(io::ErrorKind::PermissionDenied, "denied"))).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
    }

    #[cfg(unix)]
    #[test]
    fn directory_open_ignores_only_capability_errors() {
        let denied = io::Error::new(io::ErrorKind::PermissionDenied, "write-only directory");
        assert!(sync_opened_dir(Err(denied)).is_ok());
        let missing = io::Error::new(io::ErrorKind::NotFound, "share disconnected");
        assert_eq!(sync_opened_dir(Err(missing)).unwrap_err().kind(), io::ErrorKind::NotFound);
    }

    fn tmpdir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("filmcraft-atomic-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn long_file_name_still_saves() {
        let d = tmpdir("longname");
        let p = d.join(format!("{}.fcproj", "a".repeat(245)));
        fs::write(&p, b"old").unwrap();
        atomic_write(&p, b"new").unwrap();
        assert_eq!(fs::read(&p).unwrap(), b"new");
        let multi = d.join(format!("{}.fcproj", "é".repeat(120)));
        assert!(temp_path(&multi).file_name().unwrap().len() <= 255);
        fs::remove_dir_all(&d).unwrap();
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

    #[cfg(unix)]
    #[test]
    fn replacement_preserves_existing_file_permissions() {
        use std::os::unix::fs::PermissionsExt;

        let d = tmpdir("permissions");
        let p = d.join("private.fcproj");
        fs::write(&p, b"before").unwrap();
        fs::set_permissions(&p, fs::Permissions::from_mode(0o600)).unwrap();
        atomic_write(&p, b"after").unwrap();
        assert_eq!(fs::read(&p).unwrap(), b"after");
        assert_eq!(fs::metadata(&p).unwrap().permissions().mode() & 0o777, 0o600);
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
