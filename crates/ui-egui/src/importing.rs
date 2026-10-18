//! Background media imports.
//!
//! Opening a media source is the slow part of importing: a container's index is scanned at open,
//! and a long-GOP recording (an OBS-style Matroska, seconds per GB) is read through to the end.
//! Doing that on the UI thread froze the whole app for every file, where Premiere opens media in
//! the background — so the open runs on one worker thread, and each finished source is imported
//! on the UI thread by the cheap tail of the import ([`commands::import_source`]), appearing in
//! the Project panel as it lands. Files that are not media the streaming path can take (captions,
//! interchange documents, image sequences) and files small enough to open faster than a thread
//! hop stay on the synchronous path, exactly as before.
//!
//! While a batch is in flight the app repaints continuously ([`pump`] asks for it), so a finished
//! source is applied the frame it arrives and the status line can say what is going on. When the
//! last file of a batch lands, the same after-work `file.import` does runs for the batch:
//! automatic transcription, ingest copies, and relinking (a newly imported file may bring offline
//! items back). An OS file drop that imports onto the timeline places its clips then, at the
//! position the drop was made.

use std::sync::Arc;
use std::sync::mpsc::{Receiver, Sender, channel};

use egui::Pos2;
use filmcraft_engine::{MediaPool, Services};
use filmcraft_media::SharedSource;
use filmcraft_project::{BinId, ItemId, MediaIdentity};
use serde_json::{Value, json};

use crate::FilmcraftApp;

/// Files at least this large are opened on the worker; smaller ones open faster than a thread
/// hop, and importing them synchronously keeps small imports immediate.
const BACKGROUND_MIN_BYTES: u64 = 16 << 20;

/// One file the worker opened, or why it could not.
enum Msg {
    Opened { path: String, src: SharedSource, identity: MediaIdentity },
    Failed { path: String, error: String },
}

/// An import in flight.
pub(crate) struct Batch {
    rx: Receiver<Msg>,
    /// Items imported so far.
    ids: Vec<ItemId>,
    /// Files the worker has not answered yet.
    remaining: usize,
    bin: Option<BinId>,
    /// Where an OS file drop wants the imported clips placed on the timeline (Timeline ▸ drop
    /// imports to the timeline), applied when the last file lands.
    drop_at: Option<Pos2>,
    errors: Vec<String>,
}

/// Begin importing `paths` into `bin`. Media files large enough for the open to be worth a
/// background worker (see [`BACKGROUND_MIN_BYTES`]) are opened there and imported as they finish;
/// everything else imports synchronously through `file.import`, as before. `drop_at` places the
/// imported clips on the timeline at the pointer when the whole batch has landed (OS file drops).
///
/// Returns what the synchronous import returned — the command's result when nothing went to the
/// background, else `{"importing": <count>}`.
pub(crate) fn start(app: &mut FilmcraftApp, paths: Vec<String>, bin: BinId, drop_at: Option<Pos2>) -> Result<Value, String> {
    // no worker threads on the web: everything imports synchronously
    #[cfg(target_arch = "wasm32")]
    let (streamed, rest): (Vec<String>, Vec<String>) = (Vec::new(), paths);
    #[cfg(not(target_arch = "wasm32"))]
    let (streamed, rest) = {
        let mut streamed = Vec::new();
        let mut rest = Vec::new();
        for path in paths {
            if backgroundable(app, &path) {
                streamed.push(path);
            } else {
                rest.push(path);
            }
        }
        (streamed, rest)
    };
    if streamed.is_empty() {
        // nothing worth a worker: the command does it all, including its after-work
        let (r, ids) = sync_import(app, rest, bin);
        place_drop(app, ids, drop_at);
        return r;
    }
    // captions, interchange documents, image sequences and small files: their own paths, run as
    // before (their errors already went to the status line)
    let (_, ids) = sync_import(app, rest, bin);
    let count = streamed.len();
    let (tx, rx) = channel();
    // the worker owns the paths through the Arc so the fallback below can run the very same job
    // inline when no thread could be started for it
    let payload = Arc::new((app.session.media.clone(), app.session.services.clone(), streamed));
    let run = {
        let payload = payload.clone();
        move || open_files(&payload, &tx)
    };
    if std::thread::Builder::new().name("filmcraft-import".into()).spawn(run.clone()).is_err() {
        // resource exhaustion, not a media problem: open them here and now
        run();
    }
    app.ui.status = tlf!("Importing {n} media…", n = count);
    app.imports.push(Batch { rx, ids, remaining: count, bin: Some(bin), drop_at, errors: Vec::new() });
    Ok(json!({"importing": count}))
}

/// Import `paths` synchronously through `file.import`, reporting its errors to the status line;
/// returns the command's result and the items it created.
fn sync_import(app: &mut FilmcraftApp, paths: Vec<String>, bin: BinId) -> (Result<Value, String>, Vec<ItemId>) {
    let r = app.session.execute("file.import", json!({"paths": paths, "bin": bin.0})).map_err(|e| e.to_string());
    let ids = match &r {
        Ok(v) => {
            if let Some(errs) = v["errors"].as_array().filter(|e| !e.is_empty()) {
                app.ui.status = errs.iter().filter_map(Value::as_str).collect::<Vec<_>>().join("; ");
            }
            items_of(v)
        }
        Err(e) => {
            app.ui.status = e.clone();
            Vec::new()
        }
    };
    (r, ids)
}

/// The items an import result created.
fn items_of(v: &Value) -> Vec<ItemId> {
    v["items"].as_array().map(|a| a.iter().filter_map(|i| i.as_u64()).map(ItemId).collect()).unwrap_or_default()
}

/// Queue a file drop's imported clips for the timeline to place this frame.
fn place_drop(app: &mut FilmcraftApp, ids: Vec<ItemId>, drop_at: Option<Pos2>) {
    if !ids.is_empty()
        && let Some(pos) = drop_at
    {
        app.pending_timeline_drop = Some((ids, pos));
    }
}

/// Whether `path`'s import is the slow streaming open a background worker should do: media the
/// host's reader can open (video and audio containers, stills) that is not an image sequence —
/// those import whole, on their own synchronous path — and large enough that opening it is worth
/// a thread hop.
fn backgroundable(app: &FilmcraftApp, path: &str) -> bool {
    if !filmcraft_media::is_importable(std::path::Path::new(path)) {
        return false;
    }
    let s = &app.session;
    let sequence = filmcraft_media::sequence::Numbered::parse(path).is_some()
        && filmcraft_engine::media_pool::image_sequence_frames(path, &*s.services).map(|f| f.len() > 1).unwrap_or(false);
    !sequence && s.services.reader(path).is_some() && s.services.file_size(path).map(|n| n >= BACKGROUND_MIN_BYTES).unwrap_or(false)
}

/// Open every file of the batch on the calling thread, sending each result as it is ready.
fn open_files(payload: &Arc<(Arc<MediaPool>, Arc<dyn Services>, Vec<String>)>, tx: &Sender<Msg>) {
    let (pool, services, files) = (&payload.0, &payload.1, &payload.2);
    for path in files {
        // one unreadable file must not stop the ones behind it, nor a panic take the thread
        let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let identity = filmcraft_engine::relink::identity_of(&**services, path).map_err(|e| e.to_string())?;
            let src = pool.open_file(path, &**services).map_err(|e| e.to_string())?;
            Ok::<_, String>((src, identity))
        }));
        let msg = match r {
            Ok(Ok((src, identity))) => Msg::Opened { path: path.clone(), src, identity },
            Ok(Err(e)) => Msg::Failed { path: path.clone(), error: e },
            Err(_) => Msg::Failed { path: path.clone(), error: "the import panicked".into() },
        };
        if tx.send(msg).is_err() {
            break; // the app is gone
        }
    }
}

/// Apply what the background import workers have finished: import each opened source (a cheap
/// edit), and when a batch is complete run the import's after-work and place a file drop's clips
/// on the timeline. Repaints while anything is in flight, so results land the frame they arrive.
pub(crate) fn pump(app: &mut FilmcraftApp, ctx: &egui::Context) {
    let mut msgs: Vec<(usize, Msg)> = Vec::new();
    for (i, b) in app.imports.iter_mut().enumerate() {
        while let Ok(m) = b.rx.try_recv() {
            b.remaining -= 1;
            msgs.push((i, m));
        }
    }
    for (i, msg) in msgs {
        match msg {
            Msg::Opened { path, src, identity } => {
                let name = std::path::Path::new(&path).file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_else(|| path.clone());
                let b = &mut app.imports[i];
                match filmcraft_engine::commands::import_source(&mut app.session, &path, name, src, identity, b.bin) {
                    Ok(id) => b.ids.push(id),
                    Err(e) => b.errors.push(format!("{path}: {e}")),
                }
            }
            Msg::Failed { path, error } => app.imports[i].errors.push(format!("{path}: {error}")),
        }
    }
    let ready: Vec<usize> = app.imports.iter().enumerate().filter(|(_, b)| b.remaining == 0).map(|(i, _)| i).collect();
    for i in ready.into_iter().rev() {
        let b = app.imports.remove(i);
        after_import(app, &b.ids);
        place_drop(app, b.ids, b.drop_at);
        if !b.errors.is_empty() {
            app.ui.status = b.errors.join("; ");
        }
    }
    if !app.imports.is_empty() {
        ctx.request_repaint();
    }
}

/// The parts of `file.import` that follow the imports themselves, run for one finished batch:
/// Settings ▸ Media Analysis' automatic transcription, Project Settings ▸ Ingest, and relinking
/// (a newly imported file may resolve paths the project listed as offline, #110).
fn after_import(app: &mut FilmcraftApp, ids: &[ItemId]) {
    let (auto, scope) = {
        let ma = &app.session.prefs.media_analysis;
        (ma.auto_transcribe, ma.auto_transcribe_scope.clone())
    };

    if auto && scope == "allImported" && !ids.is_empty() {
        let audio: Vec<u64> = ids.iter().filter(|i| app.session.project.item(**i).is_some_and(|it| it.has_audio())).map(|i| i.0).collect();

        if !audio.is_empty()
            && let Err(e) = app.session.execute("transcript.generate", serde_json::json!({"items": audio}))
        {
            app.ui.status = format!("transcription: {e}");
        }
    }

    if let Err(e) = filmcraft_engine::proxies::ingest(&mut app.session, ids) {
        app.ui.status = format!("ingest: {e}");
    }

    let was_missing = app.session.offline.missing.clone();

    filmcraft_engine::relink::refresh(&mut app.session);

    for item in was_missing.iter().filter(|i| !app.session.offline.missing.contains(i)) {
        app.session.media.remove(*item);
    }
}
