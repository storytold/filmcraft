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

/// Files under `dir` (recursively): (path, bytes, modified).
fn cache_files(dir: &Path) -> Vec<(PathBuf, u64, std::time::SystemTime)> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&d) else { continue };
        for e in rd.flatten() {
            let Ok(md) = e.metadata() else { continue };
            if md.is_dir() {
                stack.push(e.path());
            } else {
                out.push((e.path(), md.len(), md.modified().unwrap_or(std::time::UNIX_EPOCH)));
            }
        }
    }
    out
}

/// Size and file count of the media cache.
pub fn cache_usage(dir: &Path) -> (u64, usize) {
    let f = cache_files(dir);
    (f.iter().map(|x| x.1).sum(), f.len())
}

/// Remove files under `dir`, skipping anything inside `keep`; returns (files, bytes) removed.
fn remove_files(files: &[(PathBuf, u64, std::time::SystemTime)], keep: Option<&Path>) -> (usize, u64) {
    let mut n = (0usize, 0u64);
    for (p, len, _) in files {
        if keep.is_some_and(|k| p.starts_with(k)) {
            continue;
        }
        if std::fs::remove_file(p).is_ok() {
            n.0 += 1;
            n.1 += len;
        }
    }
    n
}

/// Deepest folder level `prune_empty_dirs` descends to.
const MAX_PRUNE_DEPTH: usize = 64;

/// Remove empty folders under `dir`, except `keep` and its parents. A symlink or junction is never
/// followed: `DirEntry::file_type` doesn't traverse it, where `Path::is_dir` would (#2772).
fn prune_empty_dirs(dir: &Path, keep: Option<&Path>) {
    prune_empty_dirs_at(dir, keep, 0);
}

fn prune_empty_dirs_at(dir: &Path, keep: Option<&Path>, depth: usize) {
    if depth >= MAX_PRUNE_DEPTH {
        return;
    }
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    for e in rd.flatten() {
        if !e.file_type().is_ok_and(|t| t.is_dir()) {
            continue;
        }
        let p = e.path();
        prune_empty_dirs_at(&p, keep, depth + 1);
        if keep.is_none_or(|k| !k.starts_with(&p)) {
            let _ = std::fs::remove_dir(&p);
        }
    }
}

/// Whether `dir` may be cleaned: a real folder, not a symlink or junction to one. The cache
/// folder's name inside a custom location is known, so someone who can write to that location
/// could put a link there to a folder of the user's, and cleaning would delete what's in it.
fn cleanable(dir: &Path) -> bool {
    std::fs::symlink_metadata(dir).is_ok_and(|m| m.file_type().is_dir())
}

/// Media Cache Management: apply the automatic deletion policy to `dir` at `now`, never touching
/// `keep` (the open project's files). Returns (files, bytes) removed; nothing when `dir` is a link.
pub fn enforce_policy(dir: &Path, prefs: &MediaCachePrefs, keep: Option<&Path>, now: std::time::SystemTime) -> (usize, u64) {
    if !cleanable(dir) {
        return (0, 0);
    }
    let mut files = cache_files(dir);
    let removed = match prefs.management.as_str() {
        "olderThan" => {
            let max_age = std::time::Duration::from_secs(prefs.older_than_days as u64 * 86_400);
            let old: Vec<_> = files.into_iter().filter(|f| now.duration_since(f.2).is_ok_and(|a| a > max_age)).collect();
            remove_files(&old, keep)
        }
        "exceedsSize" => {
            let limit = prefs.max_size_gb as u64 * 1_000_000_000;
            let mut total: u64 = files.iter().map(|f| f.1).sum();
            files.sort_by_key(|f| f.2);
            let mut doomed = Vec::new();
            for f in files {
                if total <= limit {
                    break;
                }
                if keep.is_some_and(|k| f.0.starts_with(k)) {
                    continue;
                }
                total -= f.1;
                doomed.push(f);
            }
            remove_files(&doomed, keep)
        }
        _ => (0, 0),
    };
    prune_empty_dirs(dir, keep);
    removed
}

/// Delete everything in the media cache except `keep` ("Delete…" ▸ unused files); nothing when
/// `dir` is a link.
pub fn clean(dir: &Path, keep: Option<&Path>) -> (usize, u64) {
    if !cleanable(dir) {
        return (0, 0);
    }
    let files = cache_files(dir);
    let r = remove_files(&files, keep);
    prune_empty_dirs(dir, keep);
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
