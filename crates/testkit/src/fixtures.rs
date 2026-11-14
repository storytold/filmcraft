//! Fixture locations and race-free generation.
//!
//! Fixtures live in `<workspace>/target/fixtures/<crate>/` whatever `CARGO_TARGET_DIR` is, so
//! parallel agents with private target dirs share one generated set. `FILMCRAFT_FIXTURES_DIR`
//! overrides the root. Media is never committed.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

/// The workspace root (the directory holding the top-level `Cargo.toml`).
pub fn workspace_root() -> PathBuf {
    let here = Path::new(env!("CARGO_MANIFEST_DIR"));
    let root = here.parent().and_then(Path::parent).unwrap_or(here);
    root.canonicalize().unwrap_or_else(|_| root.to_path_buf())
}

/// Root of all generated fixtures: `$FILMCRAFT_FIXTURES_DIR` or `<workspace>/target/fixtures`.
pub fn fixtures_root() -> PathBuf {
    match std::env::var_os("FILMCRAFT_FIXTURES_DIR").filter(|v| !v.is_empty()) {
        Some(d) => PathBuf::from(d),
        None => workspace_root().join("target").join("fixtures"),
    }
}

/// `<fixtures root>/<sub>` (created). `sub` may contain `/` (e.g. `"opus/oracle"`).
pub fn fixtures_dir(sub: &str) -> PathBuf {
    let mut d = fixtures_root();
    for part in sub.split('/').filter(|p| !p.is_empty()) {
        d.push(part);
    }
    std::fs::create_dir_all(&d).unwrap_or_else(|e| panic!("create {}: {e}", d.display()));
    d
}

/// A temporary sibling of `out` that no other thread or process will pick: the stem gets
/// `.tmp-<pid>-<thread>-<n>` and the extension is kept (ffmpeg chooses the muxer from it).
pub fn temp_path(out: &Path) -> PathBuf {
    static N: AtomicUsize = AtomicUsize::new(0);
    let tid = format!("{:?}", std::thread::current().id()).replace(|c: char| !c.is_ascii_alphanumeric(), "");
    let n = N.fetch_add(1, Ordering::Relaxed);
    let stem = out.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
    let name = match out.extension() {
        Some(ext) => format!("{stem}.tmp-{}-{tid}-{n}.{}", std::process::id(), ext.to_string_lossy()),
        None => format!("{stem}.tmp-{}-{tid}-{n}", std::process::id()),
    };
    out.with_file_name(name)
}

fn complete_file(path: &Path) -> bool {
    std::fs::metadata(path).is_ok_and(|m| m.is_file() && m.len() > 0)
}

/// Publish a complete temporary file, or reuse a complete concurrently published
/// winner. On failure, attempt to remove only the caller's temporary file.
/// This is for files, not directories; callers must use a unique [`temp_path`].
pub fn publish(tmp: &Path, out: &Path) -> std::io::Result<()> {
    if !complete_file(tmp) {
        let winner = complete_file(out);
        let _ = std::fs::remove_file(tmp);
        return if winner { Ok(()) } else { Err(std::io::Error::new(std::io::ErrorKind::InvalidData, "temporary fixture is not a non-empty file")) };
    }
    match std::fs::rename(tmp, out) {
        Ok(()) => Ok(()),
        Err(error) => {
            let winner = complete_file(out);
            let _ = std::fs::remove_file(tmp);
            if winner { Ok(()) } else { Err(error) }
        }
    }
}

/// Return `out` if it is a non-empty file; otherwise run `make(tmp)` on a [`temp_path`]
/// and atomically rename the result into place. Concurrent generators race harmlessly.
/// `None` when `make` reports failure (the temporary file is removed).
pub fn generate(out: &Path, make: impl FnOnce(&Path) -> bool) -> Option<PathBuf> {
    if complete_file(out) {
        return Some(out.to_path_buf());
    }
    if let Some(dir) = out.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let tmp = temp_path(out);
    if !make(&tmp) || !tmp.exists() {
        let _ = std::fs::remove_file(&tmp);
        return None;
    }
    publish(&tmp, out).ok()?;
    Some(out.to_path_buf())
}

/// Status of one fixture for `cargo xtask fixtures`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status {
    Made,
    Cached,
    Skipped,
}

/// Print a `FIXTURE <made|cached|skipped> <name>` line (parsed by `cargo xtask fixtures`).
pub fn report(name: &str, status: Status) {
    let s = match status {
        Status::Made => "made",
        Status::Cached => "cached",
        Status::Skipped => "skipped",
    };
    println!("FIXTURE {s} {name}");
}

/// Run a fixture generator and [`report`] it: `cached` when every path in `outputs` existed
/// before, `made` when `generate` succeeded, `skipped` otherwise.
pub fn generate_and_report<T>(name: &str, outputs: &[PathBuf], generate: impl FnOnce() -> Option<T>) -> Option<T> {
    let existed = !outputs.is_empty() && outputs.iter().all(|p| std::fs::metadata(p).is_ok_and(|m| m.len() > 0));
    let r = generate();
    report(
        name,
        match (&r, existed) {
            (None, _) => Status::Skipped,
            (Some(_), true) => Status::Cached,
            (Some(_), false) => Status::Made,
        },
    );
    r
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(windows)]
    #[test]
    fn direct_publication_reuses_a_locked_complete_winner() {
        use std::os::windows::fs::OpenOptionsExt;
        let dir = temp_path(&std::env::temp_dir().join("filmcraft-testkit-direct-publish"));
        std::fs::create_dir_all(&dir).unwrap();
        let out = dir.join("fixture.bin");
        let tmp = temp_path(&out);
        std::fs::write(&out, b"winner").unwrap();
        std::fs::write(&tmp, b"candidate").unwrap();
        let reader = std::fs::OpenOptions::new().read(true).share_mode(1).open(&out).unwrap();
        assert!(publish(&tmp, &out).is_ok());
        assert_eq!(std::fs::read(&out).unwrap(), b"winner");
        assert!(!tmp.exists());
        drop(reader);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn direct_publication_rejects_invalid_temporary_without_output() {
        for kind in ["empty", "missing", "directory"] {
            let dir = temp_path(&std::env::temp_dir().join("filmcraft-testkit-invalid-publish"));
            std::fs::create_dir_all(&dir).unwrap();
            let out = dir.join("fixture.bin");
            let tmp = temp_path(&out);
            match kind {
                "empty" => std::fs::write(&tmp, b"").unwrap(),
                "directory" => {
                    std::fs::create_dir(&tmp).unwrap();
                    std::fs::write(tmp.join("child"), b"caller owned").unwrap();
                }
                _ => {}
            }
            assert!(publish(&tmp, &out).is_err(), "accepted {kind} temporary");
            assert!(!out.exists(), "published {kind} temporary");
            if kind == "directory" {
                assert_eq!(std::fs::read(tmp.join("child")).unwrap(), b"caller owned");
                std::fs::remove_file(tmp.join("child")).unwrap();
                std::fs::remove_dir(&tmp).unwrap();
            } else {
                assert!(!tmp.exists(), "left {kind} temporary file");
            }
            std::fs::remove_dir(&dir).unwrap();
        }
    }

    #[test]
    fn direct_publication_rejects_invalid_temporary_with_empty_output() {
        for kind in ["empty", "missing", "directory"] {
            let dir = temp_path(&std::env::temp_dir().join("filmcraft-testkit-invalid-empty-out"));
            std::fs::create_dir_all(&dir).unwrap();
            let out = dir.join("fixture.bin");
            let tmp = temp_path(&out);
            std::fs::write(&out, b"").unwrap();
            match kind {
                "empty" => std::fs::write(&tmp, b"").unwrap(),
                "directory" => {
                    std::fs::create_dir(&tmp).unwrap();
                    std::fs::write(tmp.join("child"), b"caller owned").unwrap();
                }
                _ => {}
            }
            assert!(publish(&tmp, &out).is_err(), "accepted {kind} temporary");
            assert!(out.is_file());
            assert_eq!(std::fs::read(&out).unwrap(), b"");
            if kind == "directory" {
                assert_eq!(std::fs::read(tmp.join("child")).unwrap(), b"caller owned");
                std::fs::remove_file(tmp.join("child")).unwrap();
                std::fs::remove_dir(&tmp).unwrap();
            } else {
                assert!(!tmp.exists(), "left {kind} temporary file");
            }
            std::fs::remove_file(&out).unwrap();
            std::fs::remove_dir(&dir).unwrap();
        }
    }

    #[test]
    fn direct_publication_preserves_complete_winner_with_invalid_temporary() {
        for kind in ["empty", "missing", "directory"] {
            let dir = temp_path(&std::env::temp_dir().join("filmcraft-testkit-invalid-winner"));
            std::fs::create_dir_all(&dir).unwrap();
            let out = dir.join("fixture.bin");
            let tmp = temp_path(&out);
            std::fs::write(&out, b"complete winner").unwrap();
            match kind {
                "empty" => std::fs::write(&tmp, b"").unwrap(),
                "directory" => {
                    std::fs::create_dir(&tmp).unwrap();
                    std::fs::write(tmp.join("child"), b"caller owned").unwrap();
                }
                _ => {}
            }
            assert!(publish(&tmp, &out).is_ok(), "did not reuse winner for {kind} temporary");
            assert_eq!(std::fs::read(&out).unwrap(), b"complete winner", "changed winner for {kind} temporary");
            if kind == "directory" {
                assert_eq!(std::fs::read(tmp.join("child")).unwrap(), b"caller owned");
                std::fs::remove_file(tmp.join("child")).unwrap();
                std::fs::remove_dir(&tmp).unwrap();
            } else {
                assert!(!tmp.exists(), "left {kind} temporary file");
            }
            std::fs::remove_file(&out).unwrap();
            std::fs::remove_dir(&dir).unwrap();
        }
    }

    // These callbacks expose an inherited generate validation gap: success alone
    // does not establish that the generated path is a complete fixture file.
    #[test]
    fn generate_rejects_successful_callback_producing_empty_file() {
        let dir = temp_path(&std::env::temp_dir().join("filmcraft-testkit-generate-empty"));
        std::fs::create_dir_all(&dir).unwrap();
        let out = dir.join("fixture.bin");
        let mut temporary = None;
        let result = generate(&out, |tmp| {
            temporary = Some(tmp.to_path_buf());
            std::fs::write(tmp, b"").unwrap();
            true
        });
        assert!(result.is_none());
        assert!(!out.exists());
        assert!(!temporary.unwrap().exists());
        std::fs::remove_dir(&dir).unwrap();
    }

    #[test]
    fn generate_rejects_successful_callback_producing_directory() {
        let dir = temp_path(&std::env::temp_dir().join("filmcraft-testkit-generate-directory"));
        std::fs::create_dir_all(&dir).unwrap();
        let out = dir.join("fixture.bin");
        let mut temporary = None;
        let result = generate(&out, |tmp| {
            temporary = Some(tmp.to_path_buf());
            std::fs::create_dir(tmp).unwrap();
            std::fs::write(tmp.join("child"), b"caller owned").unwrap();
            true
        });
        assert!(result.is_none());
        assert!(!out.exists());
        let tmp = temporary.unwrap();
        assert!(tmp.is_dir());
        assert_eq!(std::fs::read(tmp.join("child")).unwrap(), b"caller owned");
        std::fs::remove_file(tmp.join("child")).unwrap();
        std::fs::remove_dir(&tmp).unwrap();
        std::fs::remove_dir(&dir).unwrap();
    }

    #[test]
    fn root_is_the_workspace() {
        assert!(workspace_root().join("Cargo.toml").exists());
        assert!(workspace_root().join("crates").join("testkit").exists());
    }

    #[test]
    fn temp_paths_are_unique_and_keep_the_extension() {
        let out = Path::new("/x/clip.mp4");
        let a = temp_path(out);
        let b = temp_path(out);
        assert_ne!(a, b);
        assert_eq!(a.extension().unwrap(), "mp4");
        let other = std::thread::spawn(move || temp_path(Path::new("/x/clip.mp4"))).join().unwrap();
        assert_ne!(a, other);
    }

    #[test]
    fn generate_is_cached_and_atomic() {
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("f.bin");
        let _ = std::fs::remove_file(&out);
        assert!(generate(&out, |_| false).is_none());
        assert!(!out.exists());
        let p = generate(&out, |t| std::fs::write(t, b"abc").is_ok()).unwrap();
        assert_eq!(std::fs::read(&p).unwrap(), b"abc");
        // cached: the generator is not called again
        let p2 = generate(&out, |_| panic!("regenerated")).unwrap();
        assert_eq!(p, p2);
    }

    #[cfg(windows)]
    #[test]
    fn concurrent_complete_fixture_survives_blocked_publication() {
        use std::os::windows::fs::OpenOptionsExt;

        let dir = temp_path(&std::env::temp_dir().join("filmcraft-testkit-publish"));
        std::fs::create_dir_all(&dir).unwrap();
        let out = dir.join("fixture.bin");
        let mut reader = None;
        let mut temporary = None;
        let result = generate(&out, |tmp| {
            temporary = Some(tmp.to_path_buf());
            std::fs::write(&out, b"complete winner").unwrap();
            // Simulate another test reading the winner without delete sharing.
            reader = Some(std::fs::OpenOptions::new().read(true).share_mode(1).open(&out).unwrap());
            std::fs::write(tmp, b"complete candidate").is_ok()
        });
        assert_eq!(result.as_deref(), Some(out.as_path()));
        assert_eq!(std::fs::read(&out).unwrap(), b"complete winner");
        assert!(!temporary.unwrap().exists());
        drop(reader);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn failed_publication_does_not_accept_a_directory_or_leave_temporary() {
        let dir = temp_path(&std::env::temp_dir().join("filmcraft-testkit-publish-directory"));
        std::fs::create_dir_all(&dir).unwrap();
        let out = dir.join("fixture.bin");
        let mut temporary = None;
        let result = generate(&out, |tmp| {
            temporary = Some(tmp.to_path_buf());
            std::fs::create_dir_all(&out).unwrap();
            std::fs::write(out.join("child"), b"not a fixture").unwrap();
            std::fs::write(tmp, b"candidate").is_ok()
        });
        assert!(result.is_none());
        assert!(!temporary.unwrap().exists());
        let mut called = false;
        assert!(
            generate(&out, |_| {
                called = true;
                false
            })
            .is_none()
        );
        assert!(called, "a directory must not be reused as a cached file");
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
