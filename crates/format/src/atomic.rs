//! Crash-safe file replacement.

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

/// Replace `path` with `data` atomically and durably:
/// write `data` to a temp file in the same directory, `fsync` it, rename it over `path`, then
/// `fsync` the directory so the rename itself survives a power cut. If anything fails the temp file
/// is removed and `path` is untouched.
///
/// A symlink at `path` is followed: the file it points to is the one replaced, with the temp file
/// in that file's folder, and the link stays as it was (#4005). A link to nothing is an error.
pub fn atomic_write(path: &Path, data: &[u8]) -> io::Result<()> {
    let target = follow_links(path)?;
    let path = target.as_path();
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

/// Most symlinks followed from one path, the limit Linux uses.
const MAX_LINKS: usize = 40;

/// The file a write to `path` replaces: `path` itself, or the file at the end of the chain of
/// symlinks that starts there. Renaming over a link would replace the link and leave the file it
/// points to as it was. A link to nothing is an error: the save would land wherever it points, in
/// the empty mount point of a drive that isn't plugged in, for one.
fn follow_links(path: &Path) -> io::Result<PathBuf> {
    let mut path = path.to_path_buf();
    let mut followed = 0;
    loop {
        match fs::symlink_metadata(&path) {
            Ok(meta) if meta.file_type().is_symlink() => {
                if followed == MAX_LINKS {
                    return Err(io::Error::other(format!("{}: too many levels of symbolic links", path.display())));
                }
                followed += 1;
                // a relative target is relative to the link's folder; `join` keeps an absolute one
                let next = parent_dir(&path).join(fs::read_link(&path)?);
                if matches!(fs::symlink_metadata(&next), Err(e) if e.kind() == io::ErrorKind::NotFound) {
                    return Err(io::Error::new(io::ErrorKind::NotFound, format!("the symbolic link points to {}, which doesn't exist", next.display())));
                }
                path = next;
            }
            // not a link (or nothing there yet: a new file); the write itself reports any other problem
            _ => return Ok(path),
        }
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

    /// No `.tmp` file is left anywhere under `dir`.
    #[cfg(unix)]
    fn no_temp_files(dir: &Path) -> bool {
        fs::read_dir(dir).unwrap().flatten().all(|e| {
            let p = e.path();
            !e.file_name().to_string_lossy().ends_with(".tmp") && (p.is_symlink() || !p.is_dir() || no_temp_files(&p))
        })
    }

    #[cfg(unix)]
    #[test]
    fn a_save_through_a_symlink_replaces_the_file_it_points_to() {
        use std::os::unix::fs::{PermissionsExt, symlink};

        let d = tmpdir("symlink");
        fs::create_dir_all(d.join("links")).unwrap();
        let real = d.join("real.fcproj");
        fs::write(&real, b"before").unwrap();
        fs::set_permissions(&real, fs::Permissions::from_mode(0o600)).unwrap();
        // relative and in another folder, then a chain with an absolute link at the end
        let relative = d.join("links/relative.fcproj");
        symlink("../real.fcproj", &relative).unwrap();
        let chained = d.join("links/chained.fcproj");
        symlink(&relative, &chained).unwrap();
        for (link, data) in [(&relative, &b"one"[..]), (&chained, b"two")] {
            atomic_write(link, data).unwrap();
            assert!(fs::symlink_metadata(&relative).unwrap().file_type().is_symlink(), "{link:?} replaced the link");
            assert!(fs::symlink_metadata(&chained).unwrap().file_type().is_symlink(), "{link:?} replaced the link");
            assert_eq!(fs::read(&real).unwrap(), data, "{link:?}");
            assert_eq!(fs::metadata(&real).unwrap().permissions().mode() & 0o777, 0o600, "{link:?}");
        }
        assert_eq!(fs::read_link(&relative).unwrap(), Path::new("../real.fcproj"));
        assert!(no_temp_files(&d));
        fs::remove_dir_all(&d).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn a_dangling_symlink_or_a_loop_is_an_error_and_writes_nothing() {
        use std::os::unix::fs::symlink;

        let d = tmpdir("symlink-ends");
        let dangling = d.join("dangling.fcproj");
        symlink("new.fcproj", &dangling).unwrap();
        assert_eq!(atomic_write(&dangling, b"new").unwrap_err().kind(), io::ErrorKind::NotFound);
        assert!(fs::symlink_metadata(&dangling).unwrap().file_type().is_symlink());
        assert!(!d.join("new.fcproj").exists());
        let (a, b) = (d.join("a.fcproj"), d.join("b.fcproj"));
        symlink(&b, &a).unwrap();
        symlink(&a, &b).unwrap();
        assert!(atomic_write(&a, b"x").is_err());
        assert!(fs::symlink_metadata(&a).unwrap().file_type().is_symlink());
        assert!(fs::symlink_metadata(&b).unwrap().file_type().is_symlink());
        assert!(no_temp_files(&d));
        fs::remove_dir_all(&d).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn a_chain_of_forty_symlinks_is_followed_and_one_of_forty_one_is_not() {
        let d = tmpdir("symlink-chain");
        let real = d.join("real.fcproj");
        fs::write(&real, b"before").unwrap();
        // link-1 points to the file, and each next one to the one before
        let mut previous = real.clone();
        for n in 1..=41 {
            let link = d.join(format!("link-{n}"));
            std::os::unix::fs::symlink(&previous, &link).unwrap();
            previous = link;
        }
        atomic_write(&d.join("link-40"), b"after").unwrap();
        assert_eq!(fs::read(&real).unwrap(), b"after");
        assert!(atomic_write(&d.join("link-41"), b"x").is_err());
        assert_eq!(fs::read(&real).unwrap(), b"after");
        assert!(no_temp_files(&d));
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
