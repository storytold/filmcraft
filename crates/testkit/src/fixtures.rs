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

/// Return `out` if it already exists (non-empty); otherwise run `make(tmp)` on a [`temp_path`]
/// and atomically rename the result into place. Concurrent generators race harmlessly.
/// `None` when `make` reports failure (the temporary file is removed).
pub fn generate(out: &Path, make: impl FnOnce(&Path) -> bool) -> Option<PathBuf> {
    if std::fs::metadata(out).is_ok_and(|m| m.len() > 0) {
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
    std::fs::rename(&tmp, out).ok()?;
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
}
