//! `file.import {async: true}`: the media files of an import are opened on a background thread
//! and added to the project, in the order given, as they finish ([`poll`], once per UI frame).
//!
//! Opening is the slow part of an import: some decoders take ~10 ms a clip and open one clip at a
//! time, so a 109-clip card is over a second. That second used to pass on the UI thread, which runs
//! every command the user gives: the window froze for each import. The interactive entry points (the Import dialog, the
//! Media Browser, files dropped on the window) ask for this; scripted callers keep the synchronous
//! import, whose result lists every item.

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};

use filmcraft_project::{BinId, ItemId};
use serde_json::{Value, json};

use crate::commands::{Prefetched, import_prefetched, open_concurrently};
use crate::{EngineError, Result, Session};

/// What the worker has opened, by path: one result per occurrence of the path, in the order
/// they finished (a path given twice is opened, and added, twice).
type Opened = Arc<Mutex<HashMap<String, VecDeque<Prefetched>>>>;

/// An import whose files are still being opened.
pub struct Pending {
    pub job: u64,
    bin: Option<BinId>,
    paths: Vec<String>,
    /// The next path to add: results are added in the order of `paths`.
    next: usize,
    opened: Opened,
    items: Vec<ItemId>,
    errors: Vec<String>,
    started: web_time::Instant,
    /// The job's progress (its cancel flag) and result: once the result is set, the worker has
    /// ended and nothing more arrives in `opened`.
    progress: Arc<filmcraft_export::Progress>,
    result: Arc<Mutex<Option<std::result::Result<filmcraft_export::Report, String>>>>,
}

impl Pending {
    fn ended(&self) -> bool {
        self.result.lock().unwrap_or_else(|e| e.into_inner()).is_some()
    }
}

/// Start opening `paths` on a background thread for an import into `bin`; returns the job id
/// (its progress shows in the Progress panel).
pub(crate) fn start(s: &mut Session, paths: Vec<String>, bin: Option<BinId>) -> Result<u64> {
    let id = s.jobs.iter().map(|j| j.id).max().unwrap_or(0) + 1;
    let n = paths.len();
    let job = crate::Job {
        id,
        label: if n == 1 { "Import 1 file".into() } else { format!("Import {n} files") },
        progress: Default::default(),
        result: Default::default(),
    };
    job.progress.total.store(n as u64, Ordering::Relaxed);
    let opened: Opened = Arc::default();
    let (progress, result, sink) = (job.progress.clone(), job.result.clone(), opened.clone());
    let (media, services, todo) = (s.media.clone(), s.services.clone(), paths.clone());
    let run = move || {
        let t0 = web_time::Instant::now();
        open_concurrently(&media, &*services, &todo, Some(&progress.cancel), &|path: &str, r: Prefetched| {
            sink.lock().unwrap_or_else(|e| e.into_inner()).entry(path.to_string()).or_default().push_back(r);
            progress.done.fetch_add(1, Ordering::Relaxed);
            *progress.status.lock().unwrap_or_else(|e| e.into_inner()) =
                std::path::Path::new(path).file_name().map(|f| f.to_string_lossy().into_owned()).unwrap_or_default();
        });
        let secs = t0.elapsed().as_secs_f64();
        *progress.status.lock().unwrap_or_else(|e| e.into_inner()) = format!("Opened in {secs:.1}s");
        progress.finished.store(true, Ordering::Relaxed);
        // the UI thread may already have finished the import (every file added) and set a failure
        let mut result = result.lock().unwrap_or_else(|e| e.into_inner());
        if result.is_none() {
            *result = Some(Ok(filmcraft_export::Report {
                path: String::new(),
                frames: todo.len() as u64,
                seconds: secs,
                bytes: 0,
                render_fps: 0.0,
                extra_files: Vec::new(),
            }));
        }
    };
    let run = crate::export_tools::guard_job(job.progress.clone(), job.result.clone(), run);
    // recorded once the thread runs: a thread that could not start leaves no job "running" forever
    std::thread::Builder::new().name("filmcraft-import".into()).spawn(run).map_err(|e| EngineError::Other(format!("import thread: {e}")))?;
    let (progress, result) = (job.progress.clone(), job.result.clone());
    s.jobs.push(job);
    s.background_imports.push(Pending {
        job: id,
        bin,
        paths,
        next: 0,
        opened,
        items: Vec::new(),
        errors: Vec::new(),
        started: web_time::Instant::now(),
        progress,
        result,
    });
    Ok(id)
}

/// Stop the background imports (the project is being replaced): what they have not added yet is
/// never added, to this project or the next one.
pub(crate) fn cancel_all(s: &mut Session) {
    for p in s.background_imports.drain(..) {
        p.progress.cancel.store(true, Ordering::Relaxed);
    }
}

/// Add what the background imports have opened, in order, and finish those that are done (call
/// regularly, e.g. once per UI frame).
pub fn poll(s: &mut Session) {
    let mut i = 0;
    while i < s.background_imports.len() {
        // take the results that are next in order, then add them without the lock
        let mut not_opened = 0;
        let ready: Vec<(String, Prefetched)> = {
            let pending = &mut s.background_imports[i];
            // read before the results: once the worker has ended every result it made is in
            // `opened`, and a path still missing never comes (cancelled, or a worker that died)
            let ended = pending.ended();
            let mut opened = pending.opened.lock().unwrap_or_else(|e| e.into_inner());
            let mut ready = Vec::new();
            while let Some(path) = pending.paths.get(pending.next) {
                match opened.get_mut(path).and_then(VecDeque::pop_front) {
                    Some(r) => ready.push((path.clone(), r)),
                    None if ended => not_opened += 1,
                    None => break,
                }
                pending.next += 1;
            }
            ready
        };
        if not_opened > 0 {
            let p = &mut s.background_imports[i];
            let why = if p.progress.cancel.load(Ordering::Relaxed) { "the import was cancelled" } else { "the import stopped" };
            p.errors.push(format!("{not_opened} of {} files not imported: {why}", p.paths.len()));
        }
        let bin = s.background_imports[i].bin;
        for (path, r) in ready {
            match import_prefetched(s, &path, r, bin) {
                Ok(id) => s.background_imports[i].items.push(id),
                Err(e) => s.background_imports[i].errors.push(format!("{path}: {e}")),
            }
        }
        if s.background_imports[i].next >= s.background_imports[i].paths.len() {
            let done = s.background_imports.remove(i);
            finish(s, done);
        } else {
            i += 1;
        }
    }
}

/// An import whose files are all added: ingest and transcription as the synchronous import does,
/// relink what came back, and say how it went.
fn finish(s: &mut Session, done: Pending) {
    let mut errors = done.errors;
    errors.extend(crate::commands::after_import(s, &done.items));
    s.events.push(crate::Event::ImportFinished { job: done.job, items: done.items.clone() });
    let secs = done.started.elapsed().as_secs_f64();
    log::info!("import of {} files done in {secs:.1} s: {} added, {} failed", done.paths.len(), done.items.len(), errors.len());
    if !errors.is_empty() {
        let message = errors.join("; ");
        // nothing was imported: the job failed (`jobs.list`, the Progress panel), not just a toast
        if done.items.is_empty() {
            *done.progress.error.lock().unwrap_or_else(|e| e.into_inner()) = Some(message.clone());
            *done.result.lock().unwrap_or_else(|e| e.into_inner()) = Some(Err(message.clone()));
        }
        s.error_toast("file.import", message);
    }
}

/// What `file.import` reports about a background import.
pub(crate) fn describe(job: u64, pending: usize) -> Value {
    json!({ "importJob": job, "pending": pending })
}

#[cfg(test)]
#[path = "background_import_tests.rs"]
mod tests;
