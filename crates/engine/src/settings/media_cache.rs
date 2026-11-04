use super::prefs::MediaCachePrefs;
use super::{Path, PathBuf, Session};

// ------------------------------------------------------------------ media cache

/// The folder FilmCraft makes inside a custom media cache location.
pub const CUSTOM_CACHE_FOLDER: &str = "FilmCraft Media Cache";

/// The media cache folder: a folder of its own inside the configured location (as Premiere keeps
/// its cache in a dedicated subfolder), else `<data dir>/Media Cache`. The cache policy and
/// "Delete…" remove every file in this folder, so it must never be a folder the user chose: a
/// location like the home folder or a footage folder would lose unrelated files (#2772).
pub fn media_cache_dir(prefs: &MediaCachePrefs, data_dir: Option<&Path>) -> Option<PathBuf> {
    if !prefs.location.trim().is_empty() {
        return Some(PathBuf::from(prefs.location.trim()).join(CUSTOM_CACHE_FOLDER));
    }
    data_dir.map(|d| d.join("Media Cache"))
}

/// Where unsaved projects keep their render previews inside the media cache.
pub fn previews_root(cache: &Path) -> PathBuf {
    cache.join("Previews")
}

/// A file under the media cache: its path below the cache folder, size and modification time.
struct CacheFile {
    rel: PathBuf,
    len: u64,
    modified: std::time::SystemTime,
}

/// Deepest folder level the cache is walked or pruned to.
const MAX_DEPTH: usize = 64;

/// An opened media cache folder. Every later operation goes through this handle, never through the
/// folder's path again (#3478): the folder is opened once without following a link, and everything
/// below it is opened relative to its parent handle, again without following links. Someone who can
/// write to the cache location and swaps the folder, or anything inside it, for a symlink between
/// two steps can therefore not make a traversal or a deletion leave the folder that was opened.
///
/// On Unix this is `openat`/`unlinkat` on directory handles. Windows has no equivalent in safe
/// Rust, so there the folder is checked by path before use and a swap in the gap is not prevented.
#[cfg(unix)]
struct Root {
    fd: std::os::fd::OwnedFd,
}

#[cfg(unix)]
mod unix_root {
    use super::{CacheFile, MAX_DEPTH, Path, PathBuf, Root};
    use rustix::fs::{AtFlags, Dir, FileType, Mode, OFlags, openat, statat, unlinkat};
    use std::ffi::OsStr;
    use std::os::fd::{AsFd, OwnedFd};
    use std::os::unix::ffi::OsStrExt;

    const DIR_FLAGS: OFlags = OFlags::RDONLY.union(OFlags::DIRECTORY).union(OFlags::NOFOLLOW).union(OFlags::CLOEXEC);

    fn open_below(parent: impl AsFd, name: &OsStr) -> Option<OwnedFd> {
        openat(parent, name, DIR_FLAGS, Mode::empty()).ok()
    }

    /// The entries of the folder `fd` names (not `.` and `..`), each with whether it is a real folder.
    fn entries(fd: &OwnedFd) -> Vec<(std::ffi::OsString, bool)> {
        let Ok(dir) = Dir::read_from(fd) else { return Vec::new() };
        dir.flatten()
            .filter_map(|e| {
                let name = e.file_name().to_bytes();
                if name == b"." || name == b".." {
                    return None;
                }
                let name = OsStr::from_bytes(name).to_os_string();
                let kind = match e.file_type() {
                    FileType::Unknown => statat(fd, &name, AtFlags::SYMLINK_NOFOLLOW).map(|s| FileType::from_raw_mode(s.st_mode)).ok()?,
                    kind => kind,
                };
                Some((name, kind == FileType::Directory))
            })
            .collect()
    }

    impl Root {
        /// Open `dir` unless it is a symlink (or not a folder).
        pub(super) fn open(dir: &Path) -> Option<Root> {
            openat(rustix::fs::CWD, dir, DIR_FLAGS, Mode::empty()).ok().map(|fd| Root { fd })
        }

        /// The folder at `rel` below the root, opened one component at a time without following links.
        fn open_rel(&self, rel: &Path) -> Option<OwnedFd> {
            let mut fd = self.fd.try_clone().ok()?;
            for c in rel.components() {
                let std::path::Component::Normal(name) = c else { return None };
                fd = open_below(&fd, name)?;
            }
            Some(fd)
        }

        pub(super) fn files(&self) -> Vec<CacheFile> {
            let mut out = Vec::new();
            // Pending folders are kept as paths and opened when their turn comes, so the number of open
            // handles stays constant however many folders a cache folder has.
            let mut stack = vec![(PathBuf::new(), 0usize)];
            while let Some((rel, depth)) = stack.pop() {
                let Some(fd) = self.open_rel(&rel) else { continue };
                for (name, is_dir) in entries(&fd) {
                    if is_dir {
                        if depth + 1 < MAX_DEPTH {
                            stack.push((rel.join(&name), depth + 1));
                        }
                    } else if let Ok(st) = statat(&fd, &name, AtFlags::SYMLINK_NOFOLLOW) {
                        let secs = u64::try_from(st.st_mtime).unwrap_or(0);
                        let nanos = u32::try_from(st.st_mtime_nsec).unwrap_or(0).min(999_999_999);
                        out.push(CacheFile {
                            rel: rel.join(&name),
                            len: u64::try_from(st.st_size).unwrap_or(0),
                            modified: std::time::UNIX_EPOCH + std::time::Duration::new(secs, nanos),
                        });
                    }
                }
            }
            out
        }

        pub(super) fn remove(&self, file: &CacheFile) -> bool {
            let (Some(parent), Some(name)) = (file.rel.parent(), file.rel.file_name()) else { return false };
            self.open_rel(parent).is_some_and(|dir| unlinkat(&dir, name, AtFlags::empty()).is_ok())
        }

        pub(super) fn prune(&self, keep: Option<&Path>) {
            if let Ok(fd) = self.fd.try_clone() {
                prune_in(&fd, Path::new(""), keep, 0);
            }
        }
    }

    fn prune_in(fd: &OwnedFd, rel: &Path, keep: Option<&Path>, depth: usize) {
        if depth >= MAX_DEPTH {
            return;
        }
        for (name, is_dir) in entries(fd) {
            if !is_dir {
                continue;
            }
            let Some(sub) = open_below(fd, &name) else { continue };
            let path = rel.join(&name);
            prune_in(&sub, &path, keep, depth + 1);
            drop(sub);
            if keep.is_none_or(|k| !k.starts_with(&path)) {
                let _ = unlinkat(fd, &name, AtFlags::REMOVEDIR);
            }
        }
    }
}

#[cfg(not(unix))]
struct Root {
    dir: PathBuf,
}

#[cfg(not(unix))]
impl Root {
    /// A real folder, not a symlink or junction to one.
    fn open(dir: &Path) -> Option<Root> {
        std::fs::symlink_metadata(dir).is_ok_and(|m| m.file_type().is_dir()).then(|| Root { dir: dir.to_path_buf() })
    }

    fn files(&self) -> Vec<CacheFile> {
        let mut out = Vec::new();
        let mut stack = vec![(self.dir.clone(), 0usize)];
        while let Some((d, depth)) = stack.pop() {
            let Ok(rd) = std::fs::read_dir(&d) else { continue };
            for e in rd.flatten() {
                let Ok(ft) = e.file_type() else { continue };
                if ft.is_dir() {
                    if depth + 1 < MAX_DEPTH {
                        stack.push((e.path(), depth + 1));
                    }
                } else if let Ok(md) = e.metadata()
                    && let Ok(rel) = e.path().strip_prefix(&self.dir)
                {
                    out.push(CacheFile { rel: rel.to_path_buf(), len: md.len(), modified: md.modified().unwrap_or(std::time::UNIX_EPOCH) });
                }
            }
        }
        out
    }

    fn remove(&self, file: &CacheFile) -> bool {
        std::fs::remove_file(self.dir.join(&file.rel)).is_ok()
    }

    fn prune(&self, keep: Option<&Path>) {
        fn at(dir: &Path, rel: &Path, keep: Option<&Path>, depth: usize) {
            if depth >= MAX_DEPTH {
                return;
            }
            let Ok(rd) = std::fs::read_dir(dir) else { return };
            for e in rd.flatten() {
                // `DirEntry::file_type` doesn't traverse a symlink or junction (#2772)
                if !e.file_type().is_ok_and(|t| t.is_dir()) {
                    continue;
                }
                let path = rel.join(e.file_name());
                at(&e.path(), &path, keep, depth + 1);
                if keep.is_none_or(|k| !k.starts_with(&path)) {
                    let _ = std::fs::remove_dir(e.path());
                }
            }
        }
        at(&self.dir, Path::new(""), keep, 0);
    }
}

/// `keep` (an absolute path in the cache) below the cache folder `dir`.
fn keep_below(dir: &Path, keep: Option<&Path>) -> Option<PathBuf> {
    keep.and_then(|k| k.strip_prefix(dir).ok()).map(Path::to_path_buf)
}

/// Size and file count of the media cache (nothing when `dir` is a link).
pub fn cache_usage(dir: &Path) -> (u64, usize) {
    let Some(root) = Root::open(dir) else { return (0, 0) };
    let f = root.files();
    (f.iter().map(|x| x.len).sum(), f.len())
}

/// Remove files, skipping anything inside `keep`; returns (files, bytes) removed.
fn remove_files(root: &Root, files: &[CacheFile], keep: Option<&Path>) -> (usize, u64) {
    let mut n = (0usize, 0u64);
    for f in files {
        if keep.is_some_and(|k| f.rel.starts_with(k)) {
            continue;
        }
        if root.remove(f) {
            n.0 += 1;
            n.1 += f.len;
        }
    }
    n
}

/// Media Cache Management: apply the automatic deletion policy to `dir` at `now`, never touching
/// `keep` (the open project's files). Returns (files, bytes) removed; nothing when `dir` is a link.
pub fn enforce_policy(dir: &Path, prefs: &MediaCachePrefs, keep: Option<&Path>, now: std::time::SystemTime) -> (usize, u64) {
    match Root::open(dir) {
        Some(root) => enforce_in(&root, prefs, keep_below(dir, keep).as_deref(), now),
        None => (0, 0),
    }
}

fn enforce_in(root: &Root, prefs: &MediaCachePrefs, keep: Option<&Path>, now: std::time::SystemTime) -> (usize, u64) {
    let mut files = root.files();
    let removed = match prefs.management.as_str() {
        "olderThan" => {
            let max_age = std::time::Duration::from_secs(prefs.older_than_days as u64 * 86_400);
            let old: Vec<_> = files.into_iter().filter(|f| now.duration_since(f.modified).is_ok_and(|a| a > max_age)).collect();
            remove_files(root, &old, keep)
        }
        "exceedsSize" => {
            let limit = prefs.max_size_gb as u64 * 1_000_000_000;
            let mut total: u64 = files.iter().map(|f| f.len).sum();
            files.sort_by_key(|f| f.modified);
            let mut doomed = Vec::new();
            for f in files {
                if total <= limit {
                    break;
                }
                if keep.is_some_and(|k| f.rel.starts_with(k)) {
                    continue;
                }
                total = total.saturating_sub(f.len);
                doomed.push(f);
            }
            remove_files(root, &doomed, keep)
        }
        _ => (0, 0),
    };
    root.prune(keep);
    removed
}

/// Delete everything in the media cache except `keep` ("Delete…" ▸ unused files); nothing when
/// `dir` is a link.
pub fn clean(dir: &Path, keep: Option<&Path>) -> (usize, u64) {
    match Root::open(dir) {
        Some(root) => clean_in(&root, keep_below(dir, keep).as_deref()),
        None => (0, 0),
    }
}

fn clean_in(root: &Root, keep: Option<&Path>) -> (usize, u64) {
    let files = root.files();
    let r = remove_files(root, &files, keep);
    root.prune(keep);
    r
}

impl Session {
    /// The media cache folder (None without a data directory, e.g. tests and the web).
    pub fn media_cache_dir(&self) -> Option<PathBuf> {
        if cfg!(target_arch = "wasm32") {
            return None;
        }
        media_cache_dir(&self.prefs.media_cache, self.prefs_path.as_deref().and_then(Path::parent))
    }

    /// Point unsaved projects' render previews into the media cache and apply the automatic
    /// deletion policy (on startup and when the Media Cache settings change).
    pub fn apply_media_cache(&mut self) {
        let Some(dir) = self.media_cache_dir() else { return };
        let root = previews_root(&dir);
        if self.previews.temp_root() != Some(root.clone()) {
            self.previews.set_temp_root(Some(root));
            if self.path.is_none() && self.previews.count() == 0 {
                self.previews.reset_temp();
            }
        }
        let keep = self.previews.dir();
        enforce_policy(&dir, &self.prefs.media_cache, keep.as_deref(), std::time::SystemTime::now());
    }

    /// Note a project file as the most recent one (General ▸ At Startup ▸ Open Most Recent).
    pub fn note_recent_project(&mut self) {
        let Some(p) = self.path.clone() else { return };
        if self.prefs.general.recent_projects.first() == Some(&p) {
            return;
        }
        let mut next = self.prefs.clone();
        next.note_recent(&p);
        let _ = self.set_prefs(next);
    }
}

#[cfg(all(test, unix))]
mod race_tests {
    use super::*;

    fn tmp(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("fc-cache-race-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    /// #3478: the cache folder is swapped for a symlink to a folder of the user's after it was opened and
    /// before it is traversed and deleted from. Everything still happens in the folder that was opened.
    #[test]
    fn swapping_the_cache_folder_for_a_symlink_after_it_is_opened_deletes_nothing_outside() {
        let d = tmp("root");
        let cache = d.join("cache");
        std::fs::create_dir_all(cache.join("sub")).unwrap();
        std::fs::write(cache.join("a.bin"), b"cache").unwrap();
        std::fs::write(cache.join("sub").join("b.bin"), b"cache").unwrap();
        let documents = d.join("documents");
        std::fs::create_dir_all(documents.join("drafts")).unwrap();
        std::fs::write(documents.join("a.bin"), b"precious").unwrap();
        std::fs::write(documents.join("sub-letter.txt"), b"precious").unwrap();
        std::fs::create_dir_all(documents.join("sub")).unwrap();
        std::fs::write(documents.join("sub").join("b.bin"), b"precious").unwrap();

        let root = Root::open(&cache).expect("a real folder opens");
        // the swap: the opened folder moves away and a link to the user's folder takes its place
        std::fs::rename(&cache, d.join("moved")).unwrap();
        std::os::unix::fs::symlink(&documents, &cache).unwrap();

        assert_eq!(clean_in(&root, None), (2, 10), "the files of the folder that was opened go");
        assert!(!d.join("moved").join("a.bin").exists() && !d.join("moved").join("sub").join("b.bin").exists());
        assert!(documents.join("a.bin").exists() && documents.join("sub").join("b.bin").exists(), "a file outside the cache was deleted");
        assert!(documents.join("sub-letter.txt").exists() && documents.join("drafts").exists());
    }

    /// #3478: a folder inside the cache is swapped for a symlink after the files were listed.
    #[test]
    fn swapping_a_subfolder_for_a_symlink_between_listing_and_deleting_deletes_nothing_outside() {
        let d = tmp("sub");
        let cache = d.join("cache");
        std::fs::create_dir_all(cache.join("sub")).unwrap();
        std::fs::write(cache.join("sub").join("b.bin"), b"cache").unwrap();
        let outside = d.join("outside");
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(outside.join("b.bin"), b"precious").unwrap();

        let root = Root::open(&cache).unwrap();
        let files = root.files();
        assert_eq!(files.len(), 1);
        std::fs::remove_dir_all(cache.join("sub")).unwrap();
        std::os::unix::fs::symlink(&outside, cache.join("sub")).unwrap();
        assert_eq!(remove_files(&root, &files, None), (0, 0), "the swapped folder is not entered");
        root.prune(None);
        assert!(outside.join("b.bin").exists(), "a file outside the cache was deleted");
        assert!(outside.exists());
    }

    #[test]
    fn a_symlinked_cache_folder_does_not_open_and_a_deep_tree_is_bounded() {
        let d = tmp("deep");
        let real = d.join("real");
        std::fs::create_dir_all(&real).unwrap();
        std::os::unix::fs::symlink(&real, d.join("link")).unwrap();
        assert!(Root::open(&d.join("link")).is_none());
        assert_eq!(cache_usage(&d.join("link")), (0, 0));
        let mut deep = real.clone();
        for _ in 0..(MAX_DEPTH + 8) {
            deep.push("d");
        }
        std::fs::create_dir_all(&deep).unwrap();
        std::fs::write(deep.join("far.bin"), b"x").unwrap();
        std::fs::write(real.join("near.bin"), b"x").unwrap();
        assert_eq!(cache_usage(&real), (1, 1), "files past the depth bound are not reached");
    }
}
