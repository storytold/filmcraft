//! Safe `.tar.gz` extraction into a fresh staging directory.
//!
//! The archive is untrusted input: entries may try to climb out with `..` or absolute paths, ship
//! symlinks that point at system files, or expand to gigabytes. Only regular files and directories
//! with plain relative names are written, total size and entry count are capped, files are created
//! with `create_new` (never through an existing link), permissions are normalised and every file is
//! fsynced before the staging is declared complete.

use crate::{Result, UpdateError};
use std::io::Read;
use std::path::{Component, Path, PathBuf};

/// Bounds for one extraction.
#[derive(Debug, Clone, Copy)]
pub struct Limits {
    /// Total bytes of file data we will write.
    pub max_total_bytes: u64,
    /// Number of archive entries we will look at.
    pub max_entries: usize,
}

impl Default for Limits {
    fn default() -> Self {
        // A release tree is well under 200 MB and a few dozen files (binaries, icons, docs).
        Limits { max_total_bytes: 800 * 1024 * 1024, max_entries: 10_000 }
    }
}

fn unsafe_archive(msg: impl Into<String>) -> UpdateError {
    UpdateError::UnsafeArchive(msg.into())
}

/// The path inside the staging directory for an entry name, or an error if the name is not a plain
/// relative path.
fn safe_relative(name: &Path) -> Result<PathBuf> {
    let mut out = PathBuf::new();
    for c in name.components() {
        match c {
            Component::Normal(p) => out.push(p),
            Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                return Err(unsafe_archive(format!("entry {name:?} is not a plain relative path")));
            }
        }
    }
    if out.as_os_str().is_empty() {
        return Err(unsafe_archive("entry with an empty name"));
    }
    Ok(out)
}

/// Extract the gzip-compressed tar at `archive` into the (new, empty or non-existent) directory `dest`.
pub fn extract_tar_gz(archive: &Path, dest: &Path, limits: Limits) -> Result<()> {
    std::fs::create_dir_all(dest)?;
    let file = std::fs::File::open(archive)?;
    let mut tar = tar::Archive::new(flate2::read::GzDecoder::new(file));
    let mut total = 0u64;
    let mut entries = 0usize;
    let mut files_written = 0usize;
    for entry in tar.entries().map_err(|e| unsafe_archive(format!("not a readable tar.gz: {e}")))? {
        let mut entry = entry.map_err(|e| unsafe_archive(format!("corrupt entry: {e}")))?;
        entries += 1;
        if entries > limits.max_entries {
            return Err(unsafe_archive("too many entries"));
        }
        let kind = entry.header().entry_type();
        if kind.is_pax_global_extensions() || kind.is_pax_local_extensions() || kind.is_gnu_longname() || kind.is_gnu_longlink() {
            continue; // metadata records of the format itself, not files
        }
        let name = entry.path().map_err(|e| unsafe_archive(format!("bad entry name: {e}")))?.into_owned();
        let rel = safe_relative(&name)?;
        let target = dest.join(&rel);
        if !target.starts_with(dest) {
            return Err(unsafe_archive(format!("entry {name:?} resolves outside the staging directory")));
        }
        if kind.is_dir() {
            std::fs::create_dir_all(&target)?;
            continue;
        }
        if !kind.is_file() {
            return Err(unsafe_archive(format!("entry {name:?} is a link or special file; only files and directories are allowed")));
        }
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mode = entry.header().mode().unwrap_or(0o644);
        let remaining = limits.max_total_bytes.saturating_sub(total);
        let mut out = std::fs::OpenOptions::new().write(true).create_new(true).open(&target)?;
        // Read one byte past the allowance so an oversize entry is detected, not silently truncated.
        let copied = std::io::copy(&mut (&mut entry).take(remaining.saturating_add(1)), &mut out)?;
        total = total.saturating_add(copied);
        if total > limits.max_total_bytes {
            drop(out);
            let _ = std::fs::remove_file(&target);
            return Err(unsafe_archive("the archive expands beyond the allowed size"));
        }
        out.sync_all()?;
        set_mode(&target, mode)?;
        files_written += 1;
    }
    if files_written == 0 {
        return Err(unsafe_archive("the archive contains no files"));
    }
    sync_dir(dest);
    Ok(())
}

/// Executable if the archive said any execute bit, otherwise a plain file; never setuid or world-writable.
#[cfg(unix)]
fn set_mode(path: &Path, archive_mode: u32) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let mode = if archive_mode & 0o111 != 0 { 0o755 } else { 0o644 };
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))?;
    Ok(())
}

#[cfg(not(unix))]
fn set_mode(_path: &Path, _archive_mode: u32) -> Result<()> {
    Ok(())
}

/// Best-effort fsync of a directory so the new entries survive a power cut.
pub(crate) fn sync_dir(dir: &Path) {
    if let Ok(d) = std::fs::File::open(dir) {
        let _ = d.sync_all();
    }
}

/// The single top-level directory of an extracted release (`filmcraft-<v>-linux-<arch>/`).
pub fn release_root(staging: &Path) -> Result<PathBuf> {
    let mut dirs = std::fs::read_dir(staging)?.filter_map(|e| e.ok()).map(|e| e.path()).filter(|p| p.is_dir());
    match (dirs.next(), dirs.next()) {
        (Some(root), None) => Ok(root),
        _ => Err(unsafe_archive("expected exactly one top-level directory in the release archive")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    /// A gzip'd tar from `(name, bytes, mode)` entries, with names written verbatim into the header
    /// (so tests can include `..` and absolute paths that `tar::Builder` would refuse to write).
    fn archive(entries: &[(&str, &[u8], u32, tar::EntryType)]) -> Vec<u8> {
        let mut tar_bytes = Vec::new();
        {
            let mut b = tar::Builder::new(&mut tar_bytes);
            for (name, data, mode, kind) in entries {
                let mut h = tar::Header::new_gnu();
                {
                    let raw = h.as_old_mut();
                    raw.name = [0; 100];
                    let n = name.as_bytes();
                    raw.name[..n.len()].copy_from_slice(n);
                    if kind.is_symlink() {
                        let link = b"/etc/passwd";
                        raw.linkname[..link.len()].copy_from_slice(link);
                    }
                }
                h.set_entry_type(*kind);
                h.set_mode(*mode);
                h.set_size(data.len() as u64);
                h.set_cksum();
                b.append(&h, *data).unwrap();
            }
            b.finish().unwrap();
        }
        let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        gz.write_all(&tar_bytes).unwrap();
        gz.finish().unwrap()
    }

    fn write_archive(dir: &Path, bytes: &[u8]) -> PathBuf {
        let p = dir.join("a.tar.gz");
        std::fs::write(&p, bytes).unwrap();
        p
    }

    use tar::EntryType::{Directory, Regular, Symlink};

    #[test]
    fn extracts_a_normal_release_tree() {
        let dir = tempfile::tempdir().unwrap();
        let a = archive(&[
            ("filmcraft-0.7.3-linux-x86_64/", b"", 0o755, Directory),
            ("filmcraft-0.7.3-linux-x86_64/bin/filmcraft", b"binary", 0o755, Regular),
            ("filmcraft-0.7.3-linux-x86_64/share/doc/README", b"docs", 0o666, Regular),
        ]);
        let out = dir.path().join("stage");
        extract_tar_gz(&write_archive(dir.path(), &a), &out, Limits::default()).unwrap();
        let root = release_root(&out).unwrap();
        assert_eq!(std::fs::read(root.join("bin/filmcraft")).unwrap(), b"binary");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let m = |p: &str| std::fs::metadata(root.join(p)).unwrap().permissions().mode() & 0o777;
            assert_eq!(m("bin/filmcraft"), 0o755);
            assert_eq!(m("share/doc/README"), 0o644, "world-writable bits are dropped");
        }
    }

    #[test]
    fn rejects_path_traversal_and_absolute_names() {
        for name in ["../escape", "a/../../escape", "/etc/cron.d/evil"] {
            let dir = tempfile::tempdir().unwrap();
            let a = archive(&[(name, b"x", 0o644, Regular)]);
            let out = dir.path().join("stage");
            let err = extract_tar_gz(&write_archive(dir.path(), &a), &out, Limits::default()).unwrap_err();
            assert!(matches!(err, UpdateError::UnsafeArchive(_)), "{name}: {err:?}");
            assert!(!dir.path().join("escape").exists());
        }
    }

    #[test]
    fn rejects_symlinks() {
        let dir = tempfile::tempdir().unwrap();
        let a = archive(&[("root/link", b"", 0o777, Symlink)]);
        let err = extract_tar_gz(&write_archive(dir.path(), &a), &dir.path().join("stage"), Limits::default()).unwrap_err();
        assert!(matches!(err, UpdateError::UnsafeArchive(_)), "{err:?}");
    }

    #[test]
    fn caps_expanded_size_and_entry_count() {
        let dir = tempfile::tempdir().unwrap();
        let big = vec![0u8; 100_000]; // compresses to almost nothing: a tiny zip bomb
        let a = archive(&[("root/big", &big, 0o644, Regular)]);
        let p = write_archive(dir.path(), &a);
        let err = extract_tar_gz(&p, &dir.path().join("s1"), Limits { max_total_bytes: 50_000, max_entries: 100 }).unwrap_err();
        assert!(matches!(err, UpdateError::UnsafeArchive(_)), "{err:?}");
        let many: Vec<(String, Vec<u8>)> = (0..20).map(|i| (format!("root/f{i}"), vec![1u8])).collect();
        let refs: Vec<(&str, &[u8], u32, tar::EntryType)> = many.iter().map(|(n, d)| (n.as_str(), d.as_slice(), 0o644, Regular)).collect();
        let p2 = write_archive(dir.path(), &archive(&refs));
        let err = extract_tar_gz(&p2, &dir.path().join("s2"), Limits { max_total_bytes: 1 << 20, max_entries: 5 }).unwrap_err();
        assert!(matches!(err, UpdateError::UnsafeArchive(_)), "{err:?}");
    }

    #[test]
    fn garbage_is_an_error_not_a_panic() {
        let dir = tempfile::tempdir().unwrap();
        let p = write_archive(dir.path(), b"<html>Please log in to the hotel WiFi</html>");
        assert!(extract_tar_gz(&p, &dir.path().join("stage"), Limits::default()).is_err());
        let a = archive(&[("root/f", b"hello", 0o644, Regular)]);
        for cut in [0, 10, a.len() / 2, a.len() - 3] {
            let truncated = a.get(..cut).unwrap_or_default();
            let _ = extract_tar_gz(&write_archive(dir.path(), truncated), &dir.path().join(format!("t{cut}")), Limits::default());
        }
    }
}
