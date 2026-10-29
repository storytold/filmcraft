//! DaVinci Resolve projects: finding them in the local Resolve project libraries
//! (`file.listResolveProjects`) and importing one (`file.importResolve`). The database is read
//! by `filmcraft_interchange::resolve`; this module finds it and merges the result like any
//! other interchange import.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use filmcraft_project::MediaRef;
use serde_json::{Value, json};

use crate::interchange::{Probe, probe};
use crate::media_browser::DirEntry;
use crate::media_pool::MediaPool;
use crate::{EngineError, Result, Services, Session};

/// Where Resolve keeps its local ("disk") project libraries, relative to the home folder, on
/// Windows, macOS and Linux; older versions called the library "Resolve Disk Database".
const LIBRARY_ROOTS: &[&str] = &[
    "AppData/Roaming/Blackmagic Design/DaVinci Resolve/Support/Resolve Project Library",
    "AppData/Roaming/Blackmagic Design/DaVinci Resolve/Support/Resolve Disk Database",
    "Library/Application Support/Blackmagic Design/DaVinci Resolve/Resolve Project Library",
    "Library/Application Support/Blackmagic Design/DaVinci Resolve/Resolve Disk Database",
    ".local/share/DaVinciResolve/Resolve Project Library",
    ".local/share/DaVinciResolve/Resolve Disk Database",
];

/// Linux installs keep the default library next to the application.
const SYSTEM_ROOTS: &[&str] = &["/opt/resolve/Resolve Project Library", "/opt/resolve/Resolve Disk Database"];

/// The file each project folder holds.
pub const PROJECT_FILE: &str = "Project.db";

/// Project folders nest in Project Manager folders; deeper is a link loop.
const MAX_DEPTH: usize = 8;
/// Directories looked at per scan, so a huge or looping library can't stall the app.
const MAX_DIRS: usize = 5000;

/// One project found in a library.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Found {
    pub name: String,
    /// The Resolve user (profile) it belongs to.
    pub user: String,
    /// Project Manager folders above it.
    pub folder: Vec<String>,
    /// Its `Project.db`.
    pub path: String,
    pub modified: Option<u64>,
    pub size: Option<u64>,
}

/// `dir`/`name`, with the separator `dir` already uses.
fn join(dir: &str, name: &str) -> String {
    let sep = if dir.contains('\\') && !dir.contains('/') { '\\' } else { '/' };
    if dir.ends_with(['/', '\\']) { format!("{dir}{name}") } else { format!("{dir}{sep}{name}") }
}

/// The libraries to scan: `root` when given, else the standard locations that exist.
fn libraries(s: &Session, root: Option<&str>) -> Vec<String> {
    if let Some(r) = root {
        return vec![r.to_string()];
    }
    let mut out: Vec<String> = s.services.home_dir().map(|h| LIBRARY_ROOTS.iter().map(|r| join(&h, r)).collect()).unwrap_or_default();
    out.extend(SYSTEM_ROOTS.iter().map(|r| r.to_string()));
    out
}

/// The entries of `dir` (none when it cannot be listed).
fn entries(s: &Session, dir: &str) -> Vec<DirEntry> {
    match s.services.list_entries(dir) {
        Some(Ok(e)) => e,
        _ => Vec::new(),
    }
}

/// Every project in the libraries, newest first.
pub fn find_projects(s: &Session, root: Option<&str>) -> Vec<Found> {
    let mut out = Vec::new();
    let mut budget = MAX_DIRS;
    for lib in libraries(s, root) {
        // <library>/Resolve Projects/Users/<user>/Projects/…, or a Users or Projects folder given directly
        let users_dir = join(&join(&lib, "Resolve Projects"), "Users");
        let listed = entries(s, &users_dir);
        let users: Vec<(String, String)> = if !listed.is_empty() {
            listed.into_iter().filter(|e| e.is_dir).map(|e| (e.name.clone(), join(&join(&users_dir, &e.name), "Projects"))).collect()
        } else if root.is_some() {
            vec![(String::new(), lib.clone())]
        } else {
            continue;
        };
        for (user, projects) in users {
            scan(s, &projects, &user, &mut Vec::new(), 0, &mut budget, &mut out);
        }
    }
    out.sort_by(|a, b| b.modified.cmp(&a.modified).then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase())));
    out.dedup_by(|a, b| a.path == b.path);
    out
}

/// Look for project folders (those holding a `Project.db`) under `dir`, at most [`MAX_DEPTH`] deep
/// and [`MAX_DIRS`] directories in all.
fn scan(s: &Session, dir: &str, user: &str, folder: &mut Vec<String>, depth: usize, budget: &mut usize, out: &mut Vec<Found>) {
    if depth > MAX_DEPTH || *budget == 0 {
        return;
    }
    *budget -= 1;
    let list = entries(s, dir);
    if let Some(db) = list.iter().find(|e| !e.is_dir && e.name.eq_ignore_ascii_case(PROJECT_FILE)) {
        let name = folder.last().cloned().unwrap_or_default();
        let parents = folder.get(..folder.len().saturating_sub(1)).unwrap_or_default().to_vec();
        out.push(Found { name, user: user.to_string(), folder: parents, path: join(dir, &db.name), modified: db.modified, size: db.size });
        return;
    }
    for e in list.iter().filter(|e| e.is_dir && !e.name.starts_with('.')) {
        folder.push(e.name.clone());
        scan(s, &join(dir, &e.name), user, folder, depth + 1, budget, out);
        folder.pop();
    }
}

/// `file.listResolveProjects {root?}`
pub fn list(s: &Session, root: Option<&str>) -> Value {
    let found = find_projects(s, root);
    json!({
        "libraries": libraries(s, root),
        "projects": found.iter().map(|f| json!({
            "name": f.name,
            "user": f.user,
            "folder": f.folder,
            "path": f.path,
            "modified": f.modified,
            "size": f.size,
        })).collect::<Vec<_>>(),
    })
}

/// The `Project.db` of a project folder (or the path itself when it names a file).
pub fn database_path(s: &Session, path: &str) -> String {
    let trimmed = path.trim_end_matches(['/', '\\']);
    let is_file = s.services.file_size(trimmed).is_ok();
    if is_file { trimmed.to_string() } else { join(trimmed, PROJECT_FILE) }
}

/// What an import has read and opened before it changes the project.
pub(crate) struct Prepared {
    db: String,
    fragment: filmcraft_interchange::Imported,
    report: filmcraft_interchange::Report,
    probed: HashMap<String, Probe>,
}

/// How far a background import has got (`file.resolveImportStatus`).
#[derive(Default)]
pub struct Progress {
    /// Media files opened so far, of `total`.
    pub done: AtomicUsize,
    pub total: AtomicUsize,
    pub stage: Mutex<String>,
    /// Set to stop the import before it changes the project.
    pub cancel: AtomicBool,
}

impl Progress {
    /// Say what the import is doing (the `stage` of `file.resolveImportStatus`).
    fn set_stage(&self, stage: &str) {
        *self.stage.lock().unwrap_or_else(PoisonError::into_inner) = stage.to_string();
    }
}

/// An import running on a worker thread ([`Session::resolve_import`]): it reads the database
/// and opens every media file there; [`Session::poll_resolve_import`] then merges the result.
pub struct Running {
    /// The `Project.db` being imported.
    pub path: String,
    pub progress: Arc<Progress>,
    result: Arc<Mutex<Option<std::result::Result<Prepared, String>>>>,
    /// The session's [`Session::document_epoch`] when the import started: it merges only into
    /// that project, never into one opened or created meanwhile.
    epoch: u64,
}

/// Read a database and open its media files (no change to the project yet).
fn prepare(services: &dyn Services, media: &MediaPool, db: &str, progress: &Progress) -> std::result::Result<Prepared, String> {
    progress.set_stage("Reading the project");
    let bytes = services.read_file(db).map_err(|e| format!("{db}: {e}"))?;
    prepare_bytes(services, media, db, &bytes, progress)
}

/// [`prepare`] for the bytes of the database at `db`, read already.
fn prepare_bytes(services: &dyn Services, media: &MediaPool, db: &str, bytes: &[u8], progress: &Progress) -> std::result::Result<Prepared, String> {
    // the project folder's name, for a database that names no project
    let folder = std::path::Path::new(db).parent().and_then(|p| p.file_name()).map(|n| n.to_string_lossy().into_owned());
    let opts = filmcraft_interchange::ImportOptions { name: folder, ..Default::default() };
    let (mut fragment, mut report) = filmcraft_interchange::resolve::import(bytes, &opts).map_err(|e| format!("{db}: {e}"))?;
    // Resolve writes recent changes to a write-ahead log first while the project is open
    if services.file_size(&format!("{db}-wal")).is_ok_and(|n| n > 0) {
        report.warn("DaVinci Resolve has changes that are not saved into Project.db yet (the project is probably open in Resolve): close it there and import again to include them");
    }
    let mut paths = Vec::new();
    for m in fragment.project.items.values_mut().filter_map(|i| i.as_media_mut()) {
        if let MediaRef::File { path } = &mut m.media {
            // A relative path is the database's: taken from its folder here, so the project keeps
            // that file. Kept as it was, it would resolve against whichever document stored the
            // same text first (the pool keys stored paths by their text), or against the
            // FilmCraft project once that is saved.
            if !cfg!(target_arch = "wasm32")
                && relative(path)
                && let Ok(file) = crate::doc_path::resolve(path, Some(std::path::Path::new(db)))
            {
                *path = file;
            }
            paths.push(path.clone());
        }
    }
    paths.sort();
    paths.dedup();
    // Opened as stored paths of the database, but not recorded in the shared pool: a cancelled
    // or failed import leaves nothing behind; merge_fragment records them once it is merged.
    progress.total.store(paths.len(), Ordering::Relaxed);
    progress.set_stage("Linking media");
    let mut probed = HashMap::new();
    for (n, path) in paths.into_iter().enumerate() {
        if progress.cancel.load(Ordering::Relaxed) {
            return Err(CANCELLED.into());
        }
        let p = probe(media, services, &path, Some(db));
        probed.insert(path, p);
        progress.done.store(n + 1, Ordering::Relaxed);
    }
    Ok(Prepared { db: db.to_string(), fragment, report, probed })
}

/// Whether a stored media path is relative to the document: not absolute here, nor a drive
/// (`C:`) or share (`\\`) path of another system, which no document folder makes a file here.
fn relative(path: &str) -> bool {
    let b = path.as_bytes();
    std::path::Path::new(path).is_relative() && !b.starts_with(br"\\") && !matches!((b.first(), b.get(1)), (Some(l), Some(b':')) if l.is_ascii_alphabetic())
}

const CANCELLED: &str = "the DaVinci Resolve import was cancelled";
const REPLACED: &str = "the DaVinci Resolve import was cancelled: the project it was importing into was closed";

/// Merge what [`prepare`] read into the project.
fn apply(s: &mut Session, mut p: Prepared) -> Result<Value> {
    let name = p.fragment.project.name.clone();
    let kind = crate::interchange::Kind { format_name: filmcraft_interchange::resolve::FORMAT_NAME, name: Some(name), rebase_timecode: false };
    crate::interchange::merge_fragment(s, &p.db, p.fragment, p.report, kind, &mut p.probed)
}

/// `file.importResolve {path}`: `path` is a project folder or its `Project.db`.
pub fn import(s: &mut Session, path: &str) -> Result<Value> {
    let db = database_path(s, path);
    let p = prepare(&*s.services, &s.media, &db, &Progress::default()).map_err(EngineError::Other)?;
    apply(s, p)
}

/// Import the bytes of a `Project.db` read from `db`.
pub fn import_bytes(s: &mut Session, db: &str, bytes: &[u8]) -> Result<Value> {
    let p = prepare_bytes(&*s.services, &s.media, db, bytes, &Progress::default()).map_err(EngineError::Other)?;
    apply(s, p)
}

/// `file.importResolve {path, background: true}`: read the project and open its media on a
/// worker thread, so the app stays responsive; [`Session::poll_resolve_import`] (the UI, every
/// frame) or `file.resolveImportStatus` merges it when it is ready. Without threads (the web)
/// the import runs at once.
pub fn start(s: &mut Session, path: &str) -> Result<Value> {
    if s.resolve_import.is_some() {
        return Err(EngineError::Other("a DaVinci Resolve import is already running".into()));
    }
    if cfg!(target_arch = "wasm32") {
        return import(s, path);
    }
    let db = database_path(s, path);
    let (services, media) = (s.services.clone(), s.media.clone());
    let progress = Arc::new(Progress::default());
    let result: Arc<Mutex<Option<std::result::Result<Prepared, String>>>> = Arc::default();
    let (worker_progress, worker_result, worker_db) = (progress.clone(), result.clone(), db.clone());
    std::thread::Builder::new()
        .name("resolve-import".into())
        .spawn(move || {
            let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| prepare(&*services, &media, &worker_db, &worker_progress)))
                .unwrap_or_else(|_| Err(format!("{worker_db}: the import failed unexpectedly")));
            *worker_result.lock().unwrap_or_else(PoisonError::into_inner) = Some(r);
        })
        .map_err(|e| EngineError::Other(format!("cannot start the import: {e}")))?;
    s.resolve_import = Some(Running { path: db.clone(), progress, result, epoch: s.document_epoch });
    Ok(json!({ "started": true, "path": db, "poll": "file.resolveImportStatus", "cancel": "file.cancelResolveImport" }))
}

/// `file.resolveImportStatus`: the running import's progress; once it has finished, merges it
/// and returns its result as `file.importResolve` does (`finished: true`).
pub fn status(s: &mut Session) -> Result<Value> {
    if let Some(r) = s.poll_resolve_import() {
        return r.map(|mut v| {
            v["finished"] = json!(true);
            v
        });
    }
    Ok(match &s.resolve_import {
        None => json!({ "running": false }),
        Some(r) => json!({
            "running": true,
            "path": r.path,
            "stage": r.progress.stage.lock().unwrap_or_else(PoisonError::into_inner).clone(),
            "done": r.progress.done.load(Ordering::Relaxed),
            "total": r.progress.total.load(Ordering::Relaxed),
        }),
    })
}

impl Session {
    /// Merge a background Resolve import that has finished (`None` while none is, or it is
    /// still running). A cancelled import is an error and leaves the project as it was.
    pub fn poll_resolve_import(&mut self) -> Option<Result<Value>> {
        let running = self.resolve_import.as_ref()?;
        // The project was replaced (opened, created, closed, recovered) since the import started:
        // it belongs to a document that is gone. Stop the worker opening files for it and end the
        // import now, without waiting for the worker (it may be in a long read): whatever it still
        // produces goes into a result nobody holds, and it recorded nothing in the shared pool.
        if running.epoch != self.document_epoch {
            running.progress.cancel.store(true, Ordering::Relaxed);
            self.resolve_import = None;
            return Some(Err(EngineError::Other(REPLACED.into())));
        }
        let r = running.result.lock().unwrap_or_else(PoisonError::into_inner).take()?;
        // The worker looks at the flag only between media files: a cancel while it read the
        // database, after its last file, or for a project without media must still hold.
        let cancelled = running.progress.cancel.load(Ordering::Relaxed);
        self.resolve_import = None;
        Some(match r {
            Ok(_) if cancelled => Err(EngineError::Other(CANCELLED.into())),
            Ok(p) => apply(self, p),
            Err(e) => Err(EngineError::Other(e)),
        })
    }

    /// Stop a running Resolve import before it changes the project.
    pub fn cancel_resolve_import(&mut self) -> bool {
        match &self.resolve_import {
            Some(r) => {
                r.progress.cancel.store(true, Ordering::Relaxed);
                true
            }
            None => false,
        }
    }
}
