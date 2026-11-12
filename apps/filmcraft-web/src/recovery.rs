//! Auto-save and crash recovery in OPFS (the desktop recovery journal, mapped to the browser).
//!
//! - While the project has unsaved changes, a snapshot is written every few seconds to
//!   `recovery/snapshot.fcproj` with `recovery/meta.json` saying it is dirty; once the project is
//!   saved (downloaded) the meta is marked clean after pending snapshots finish.
//! - Imported media are copied to `media/<name>` (the browser streams the copy; nothing goes
//!   through wasm memory), so the snapshot's `/files/<name>` references resolve after a reload.
//! - At start-up a dirty snapshot is reopened. `?norecover` skips opening it, and `?fresh` also
//!   skips the media; their names remain reserved so new imports cannot replace the held media.
//!   Both hold the snapshot for download before this tab can replace it.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use filmcraft_engine::Session;
use serde_json::{Value, json};

use crate::opfs;
use crate::recovery_policy::{Action, Policy};

const SNAPSHOT: &str = "recovery/snapshot.fcproj";
const META: &str = "recovery/meta.json";
const MAX_META_BYTES: u64 = 1 << 20;
// Match fs::WHOLE_FILE_MAX: the normal browser file.open byte limit.
const MAX_SNAPSHOT_BYTES: u64 = 256 << 20;
/// Media larger than this are not copied to OPFS.
const KEEP_MAX: f64 = 4.0 * 1024.0 * 1024.0 * 1024.0;

thread_local! {
    static MEDIA_NAMES_READY: Cell<bool> = const { Cell::new(false) };
}

/// Media copies are safe only after standalone startup reserves existing durable names.
pub(crate) fn set_media_names_ready(ready: bool) {
    MEDIA_NAMES_READY.with(|state| state.set(ready));
}

pub(crate) fn media_names_ready() -> bool {
    MEDIA_NAMES_READY.with(Cell::get)
}

/// A finished OPFS write, reported back to the next [`Autosave::tick`].
enum Outcome {
    Snapshot { revision: u64, ok: bool },
    Clean { ok: bool },
}

pub struct Autosave {
    policy: Policy,
    outcomes: Rc<RefCell<Vec<Outcome>>>,
    /// Monotonic host clock. Web `logic` may run for hidden tabs; egui's input time is stale there.
    clock: web_time::Instant,
}

impl Default for Autosave {
    /// Default policy with a monotonic host clock, so hidden-tab logic never uses a stale egui input time.
    fn default() -> Self {
        Self { policy: Policy::default(), outcomes: Rc::default(), clock: web_time::Instant::now() }
    }
}

impl Autosave {
    /// Process completed writes and schedule recovery snapshots or clean metadata.
    pub fn tick(&mut self, s: &Session, ctx: &egui::Context) {
        if !opfs::available() {
            return;
        }
        let now = self.clock.elapsed().as_secs_f64();
        for o in self.outcomes.borrow_mut().drain(..) {
            match o {
                Outcome::Snapshot { revision, ok } => self.policy.written(revision, ok, now),
                Outcome::Clean { ok } => self.policy.cleaned(ok, now),
            }
        }
        let action = self.policy.tick(now, s.revision, s.saved_revision);
        match action {
            Action::Nothing => {}
            Action::WakeAfter(secs) => ctx.request_repaint_after(std::time::Duration::from_secs_f64(secs)),
            Action::WriteSnapshot(revision) => {
                // A failed dirty-meta write must not replace the previous snapshot.
                let bytes = filmcraft_format::encode(&s.project, false);
                let meta = json!({"dirty": true, "name": crate::recovery_policy::recovery_display_name(&s.project.name), "revision": s.revision});
                let (out, ctx) = (self.outcomes.clone(), ctx.clone());
                let after_meta: opfs::Done = Box::new(move |ok| {
                    if !ok {
                        out.borrow_mut().push(Outcome::Snapshot { revision, ok: false });
                        ctx.request_repaint();
                        return;
                    }
                    let after_snapshot: opfs::Done = Box::new(move |ok| {
                        out.borrow_mut().push(Outcome::Snapshot { revision, ok });
                        ctx.request_repaint();
                    });
                    opfs::write_then(SNAPSHOT.into(), bytes, Some(after_snapshot));
                });
                opfs::write_then(META.into(), meta.to_string().into_bytes(), Some(after_meta));
            }
            Action::WriteClean => {
                let (out, ctx) = (self.outcomes.clone(), ctx.clone());
                let done: opfs::Done = Box::new(move |ok| {
                    out.borrow_mut().push(Outcome::Clean { ok });
                    ctx.request_repaint();
                });
                opfs::write_then(META.into(), json!({"dirty": false}).to_string().into_bytes(), Some(done));
            }
        }
    }
}

/// Read the dirty marker and sanitize its suggested filename; report unreadable or inconsistent metadata.
async fn dirty_snapshot_name() -> Result<Option<String>, String> {
    if !opfs::available() {
        return Ok(None);
    }
    let Some(bytes) = opfs::read_capped_result(META, MAX_META_BYTES).await.map_err(|error| format!("reading recovery metadata: {error:?}"))? else {
        return match opfs::file_result(SNAPSHOT).await {
            Ok(Some(_)) => Err("recovery metadata is missing for an existing snapshot".into()),
            Ok(None) => Ok(None),
            Err(error) => Err(format!("checking recovery snapshot: {error:?}")),
        };
    };
    let meta: Value = serde_json::from_slice(&bytes).map_err(|error| format!("parsing recovery metadata: {error}"))?;
    match meta.get("dirty").and_then(Value::as_bool) {
        Some(false) => return Ok(None),
        Some(true) => {}
        None => return Err("recovery metadata has no dirty flag".into()),
    }
    let name = crate::recovery_policy::recovery_display_name(meta.get("name").and_then(Value::as_str).unwrap_or("Recovered"));
    Ok(Some(format!("{name}.fcproj")))
}

/// The unsaved snapshot of a session that ended without saving: (file name, bytes).
pub async fn load_snapshot() -> Result<Option<(String, Vec<u8>)>, String> {
    let Some(name) = dirty_snapshot_name().await? else { return Ok(None) };
    let bytes = opfs::read_capped_result(SNAPSHOT, MAX_SNAPSHOT_BYTES)
        .await
        .map_err(|error| format!("reading recovery snapshot: {error:?}"))?
        .ok_or("dirty recovery snapshot is missing")?;
    Ok(Some((name, bytes)))
}

/// Hold a skipped dirty snapshot for download without loading its contents into wasm memory.
pub async fn load_snapshot_blob() -> Result<Option<(String, web_sys::Blob)>, String> {
    let Some(name) = dirty_snapshot_name().await? else { return Ok(None) };
    let file =
        opfs::file_result(SNAPSHOT).await.map_err(|error| format!("opening recovery snapshot: {error:?}"))?.ok_or("dirty recovery snapshot is missing")?;
    Ok(Some((name, file.into())))
}

/// Offer the existing snapshot for download even if its metadata could not be read.
pub async fn load_raw_snapshot_blob() -> Result<Option<web_sys::Blob>, String> {
    opfs::file_result(SNAPSHOT).await.map(|file| file.map(Into::into)).map_err(|error| format!("opening recovery snapshot: {error:?}"))
}

/// Keep a copy of an imported file in OPFS (background).
pub fn keep_media(path: &str, file: &web_sys::File) {
    if !MEDIA_NAMES_READY.with(Cell::get) || !opfs::available() || file.size() > KEEP_MAX {
        return;
    }
    let Some(name) = path.strip_prefix("/files/").map(str::to_string) else { return };
    let blob: web_sys::Blob = file.clone().into();
    wasm_bindgen_futures::spawn_local(async move {
        if let Err(e) = opfs::store_blob(&format!("media/{name}"), &blob).await {
            log::warn!("keeping {name} in OPFS: {e:?}");
        }
    });
}

/// Protect recovery media from same-name imports, including files skipped or unreadable during restoration.
pub async fn reserve_media_names() -> Result<(), String> {
    let names = opfs::file_names_in_result("media").await.map_err(|error| {
        js_sys::Reflect::get(&error, &"message".into())
            .ok()
            .and_then(|message| message.as_string())
            .or_else(|| error.as_string())
            .unwrap_or_else(|| format!("{error:?}"))
    })?;
    for name in names {
        crate::fs::reserve_blob_name(&name);
    }
    Ok(())
}

/// Register the media kept in OPFS under their `/files/` paths; returns how many.
pub async fn restore_media() -> usize {
    let files = opfs::files_in("media").await;
    let n = files.len();
    for (name, f) in files {
        crate::fs::register_recovery_blob(&name, f.into());
    }
    n
}
