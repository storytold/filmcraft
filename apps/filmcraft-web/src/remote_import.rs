//! Using remote files from the browser's imports and project loads.
//!
//! The engine's `Services` are synchronous, so what a command is going to read has to be on its
//! way before it runs: the sizes of a project's media, the head of a file, the listing of a
//! folder. Commands that read remote files (Media Browser import, Import from Remote, Open In
//! Source Monitor) are therefore run here: prepare, run, and run again while a read answers
//! "still loading".

use filmcraft_engine::Services;
use serde_json::{Value, json};

use crate::remote::{self, ensure_stat, ensure_stat_until};

/// Media paths of one project whose sizes are fetched before it opens.
const MEDIA_MAX: usize = 10_000;
/// Remote files one import command prepares.
const IMPORT_MAX: usize = 10_000;
/// Folders and folder depth one import prepares (as the engine's folder import).
const FOLDERS_MAX: usize = 4096;
const DEPTH_MAX: usize = 32;
/// Runs of one folder listing while it loads (about 25 ms apart).
const RETRIES: usize = 2400;
/// Seconds one import command keeps running again while its bytes load.
const LOAD_BUDGET_S: u64 = 300;
/// Longest wait for a chunk to arrive before a run is repeated anyway (milliseconds).
const WAKE_MS: i32 = 250;
/// Bytes of media files one project load warms up: half the chunk cache, so nothing it fetched is evicted again.
const WARM_BUDGET: u64 = (crate::fs::CACHE_BUDGET / 2) as u64;

/// Longest wait (milliseconds) for the active sequence's media before the other sequences' media
/// starts loading anyway (it is throttled while the active one still loads).
const REST_AFTER_MS: u128 = 20_000;

/// Fetch the size of every remote file a project (its bytes) refers to, so the synchronous
/// `Services` find them when it opens. Missing ones stay missing: the engine reports them offline.
pub async fn prefetch_media(project: &[u8]) {
    prefetch_media_until(project, None).await;
}

/// [`prefetch_media`] that stops starting new lookups once `deadline` has passed (the rest stays
/// unresolved, so the engine reports it offline). A request already running is aborted when
/// `deadline` passes. Only the sizes gate the caller. WebM, Matroska and QuickTime files also get
/// their container index loaded (as an import does), up to [`WARM_BUDGET`] bytes of files, but in
/// the background: the project opens at once and a medium whose bytes are still on their way
/// shows as loading (never missing) until they arrive. The media of the active sequence goes
/// first ([`crate::prewarm_plan`]): the other sequences' media starts when it is loaded (or after
/// [`REST_AFTER_MS`]), and meanwhile other files fetch only a little ([`crate::focus`]).
pub async fn prefetch_media_until(project: &[u8], deadline: Option<web_time::Instant>) {
    let plan = crate::prewarm_plan::plan_bytes(project);
    let nfocus = plan.focus.len();
    let remote_focus: Vec<String> = plan.focus.iter().filter(|p| remote::is_remote(p)).cloned().collect();
    crate::fs::set_focus(&remote_focus, &[]);
    let mut warm_left = WARM_BUDGET;
    let (mut warm_focus, mut warm_rest) = (Vec::new(), Vec::new());
    let mut seen = std::collections::HashSet::new();
    for (i, p) in plan.focus.into_iter().chain(plan.rest).enumerate().filter(|(_, p)| remote::is_remote(p)).take(MEDIA_MAX) {
        if deadline.is_some_and(|d| web_time::Instant::now() >= d) {
            log::info!("remote media: time budget used up, the rest stays unresolved");
            break;
        }
        if !seen.insert(p.clone()) {
            continue;
        }
        match ensure_stat_until(&p, deadline).await {
            Ok(st) if !st.is_dir && crate::readahead::warms_at_load(&p) && st.size <= warm_left => {
                // registers the file's chunk cache; a file the engine cannot read is not warmed
                if crate::fs::WebServices.reader(&p).is_some_and(|r| r.is_ok()) {
                    warm_left -= st.size;
                    let list = if i < nfocus { &mut warm_focus } else { &mut warm_rest };
                    list.push((p, st.size));
                }
            }
            Ok(_) => {}
            Err(e) => log::info!("remote media {p}: {e}"),
        }
    }
    let loading: Vec<String> = warm_focus.iter().map(|(p, _)| p.clone()).collect();
    crate::fs::set_focus(&remote_focus, &loading);
    wasm_bindgen_futures::spawn_local(async move {
        let left = std::rc::Rc::new(std::cell::Cell::new(warm_focus.len()));
        for (p, size) in warm_focus {
            let left = left.clone();
            wasm_bindgen_futures::spawn_local(async move {
                crate::import::prewarm(&p, size).await;
                crate::fs::focus_done(&p);
                left.set(left.get().saturating_sub(1));
            });
        }
        let started = web_time::Instant::now();
        while left.get() > 0 && started.elapsed().as_millis() < REST_AFTER_MS {
            crate::fs::sleep(20).await;
        }
        for (p, size) in warm_rest {
            crate::import::prewarm(&p, size).await;
        }
    });
}

/// Load the listing of `dir` and of the folders below it (their files' sizes come with them).
async fn prefetch_tree(dir: String, depth: usize, budget: &mut usize) {
    let Some(left) = budget.checked_sub(1) else { return };
    *budget = left;
    for _ in 0..RETRIES {
        match remote::list(&dir) {
            Some(Ok(entries)) => {
                if depth < DEPTH_MAX {
                    for e in entries.into_iter().filter(|e| e.is_dir) {
                        Box::pin(prefetch_tree(filmcraft_engine::media_browser::join(&dir, &e.name), depth + 1, budget)).await;
                    }
                }
                return;
            }
            Some(Err(e)) if e.kind() == std::io::ErrorKind::WouldBlock => crate::fs::sleep(25).await,
            _ => return,
        }
    }
}

/// Fetch what importing `path` reads first: the file's size and container index, a folder's
/// listings, a project's media sizes.
async fn prepare(path: &str, budget: &mut usize) {
    match ensure_stat(path).await {
        Ok(st) if !st.is_dir => {
            // registers the file's chunk cache; the engine reports a failure itself
            if crate::fs::WebServices.reader(path).is_none_or(|r| r.is_err()) {
                return;
            }
            crate::import::prewarm(path, st.size).await;
            if path.to_ascii_lowercase().ends_with(".fcproj")
                && st.size <= crate::remote_dav::MAX_PROJECT_BYTES
                && let Ok(bytes) = crate::fs::WebServices.read_file(path)
            {
                prefetch_media(&bytes).await;
            }
        }
        Ok(_) => prefetch_tree(path.to_string(), 0, budget).await,
        Err(e) => {
            log::info!("remote import {path}: {e}");
            // possibly a mount root or a folder the parent's listing does not show
            prefetch_tree(path.to_string(), 0, budget).await;
        }
    }
}

/// Run `id` with `params` on the UI thread, again while a read of it was still loading. A repeat
/// waits for the next chunk to arrive (not for every fetch in flight), so it never waits for the
/// slowest chunk of a wide read-ahead.
async fn run(id: &str, params: Value) -> Result<Value, String> {
    let give_up = web_time::Instant::now() + std::time::Duration::from_secs(LOAD_BUDGET_S);
    while web_time::Instant::now() < give_up {
        let (id, params) = (id.to_string(), params.clone());
        let (r, pending) = crate::import::on_ui(move |w, _| {
            let _ = filmcraft_media::pending::take();
            let r = w.app.session.execute(&id, params).map_err(|e| e.to_string());
            (r, filmcraft_media::pending::take())
        })
        .await;
        let failed = match &r {
            Ok(v) => v.get("errors").and_then(Value::as_array).is_some_and(|e| !e.is_empty()),
            Err(_) => true,
        };
        if failed && pending {
            if crate::fs::inflight() == 0 {
                // nothing is being fetched: a size or listing is on its way
                crate::fs::sleep(25).await;
            } else {
                crate::fs::next_data(WAKE_MS).await;
            }
            continue;
        }
        return r;
    }
    Err("timed out loading the file".into())
}

/// Take over an import command that names remote files; `false` when none does.
fn start_import(id: &str, params: &Value) -> bool {
    let paths: Vec<String> =
        params.get("paths").and_then(Value::as_array).map(|a| a.iter().filter_map(|v| v.as_str().map(str::to_string)).collect()).unwrap_or_default();
    if !paths.iter().any(|p| remote::is_remote(p)) {
        return false;
    }
    let (id, params) = (id.to_string(), params.clone());
    wasm_bindgen_futures::spawn_local(async move {
        let mut budget = FOLDERS_MAX;
        for p in paths.iter().filter(|p| remote::is_remote(p)).take(IMPORT_MAX) {
            prepare(p, &mut budget).await;
        }
        let mut problems = Vec::new();
        // One path per run: a retry must not import the files that already went in again.
        let wanted = if id == "mediaBrowser.openInSource" { &paths[..paths.len().min(1)] } else { &paths[..] };
        for p in wanted {
            let mut q = params.clone();
            q["paths"] = json!([p]);
            match run(&id, q).await {
                Ok(v) => problems.extend(v.get("errors").and_then(Value::as_array).into_iter().flatten().filter_map(Value::as_str).map(str::to_string)),
                Err(e) => problems.push(e),
            }
        }
        if !problems.is_empty() {
            let msg = problems.join("; ");
            crate::post(move |w, _| w.app.status(msg));
        }
    });
    true
}

/// Route "open project by path" and the import commands through the remote fetch (Media Browser,
/// Open Recent, Home).
pub fn install_hooks(app: &mut filmcraft_ui_egui::FilmcraftApp) {
    if !remote::enabled() {
        return;
    }
    let name = remote::volumes().into_iter().next().map(|v| v.name).unwrap_or_default();
    app.hooks.remote = Some(filmcraft_ui_egui::RemoteInfo { name });
    app.hooks.import_command = Some(Box::new(start_import));
    app.hooks.open_project = Some(Box::new(|path: &str| {
        let path = path.to_string();
        wasm_bindgen_futures::spawn_local(async move {
            let r = if remote::is_remote(&path) {
                remote::open_project(&path).await
            } else {
                let p = path.clone();
                crate::import::on_ui(move |w, _| w.app.session.execute("file.open", json!({"path": p})).map_err(|e| e.to_string())).await
            };
            if let Err(e) = r {
                crate::post(move |w, _| w.app.status(e));
            }
        });
    }));
}
