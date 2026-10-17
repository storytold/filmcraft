//! Auto-save and crash recovery in OPFS (the desktop recovery journal, mapped to the browser).
//!
//! - While the project has unsaved changes, a snapshot is written every few seconds to
//!   `recovery/snapshot.fcproj` with `recovery/meta.json` saying it is dirty; once the project is
//!   saved (downloaded) the meta is marked clean.
//! - Imported media are copied to `media/<name>` (the browser streams the copy; nothing goes
//!   through wasm memory), so the snapshot's `/files/<name>` references resolve after a reload.
//! - At start-up a dirty snapshot is reopened. `?norecover` skips opening it, and `?fresh` also
//!   skips the media; both hold the snapshot for download before another session can replace it.

use std::cell::RefCell;
use std::rc::Rc;

use filmcraft_engine::Session;
use serde_json::{Value, json};

use crate::opfs;
use crate::recovery_policy::{Action, Policy};

const SNAPSHOT: &str = "recovery/snapshot.fcproj";
const META: &str = "recovery/meta.json";
const MAX_META_BYTES: u64 = 1 << 20;
const MAX_SNAPSHOT_BYTES: u64 = 256 << 20;
/// Media larger than this are not copied to OPFS.
const KEEP_MAX: f64 = 4.0 * 1024.0 * 1024.0 * 1024.0;

/// A finished OPFS write, reported back to the next [`Autosave::tick`].
enum Outcome {
    Snapshot { revision: u64, ok: bool },
    Clean { ok: bool },
}

#[derive(Default)]
pub struct Autosave {
    policy: Policy,
    outcomes: Rc<RefCell<Vec<Outcome>>>,
    warning: crate::recovery_policy::ValidationWarning,
}

impl Autosave {
    /// Called every frame.
    pub fn tick(&mut self, s: &Session, ctx: &egui::Context) -> Option<String> {
        if !opfs::available() {
            return None;
        }
        self.warning.refresh(s.revision, || filmcraft_format::validate_project_dimensions(&s.project));
        let now = ctx.input(|i| i.time);
        for o in self.outcomes.borrow_mut().drain(..) {
            match o {
                Outcome::Snapshot { revision, ok } => self.policy.written(revision, ok, now),
                Outcome::Clean { ok } => self.policy.cleaned(ok, now),
            }
        }
        let action = match self.policy.tick_checked(now, s.revision, s.saved_revision, || filmcraft_format::validate_project_dimensions(&s.project)) {
            Ok(action) => action,
            Err(error) => {
                log::warn!("browser recovery kept the previous snapshot, if any: {error}");
                self.warning.reject(s.revision, error.to_string());
                return self.warning_message();
            }
        };
        match action {
            Action::Nothing => {}
            Action::WakeAfter(secs) => ctx.request_repaint_after(std::time::Duration::from_secs_f64(secs)),
            Action::WriteSnapshot(revision) => {
                self.warning.clear();
                // A failed dirty-meta write must not replace the previous snapshot.
                let bytes = filmcraft_format::encode(&s.project, false);
                let meta = json!({"dirty": true, "name": crate::recovery_policy::recovery_display_name(&s.project.name), "revision": s.revision, "time": js_sys::Date::now()});
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
                self.warning.clear();
                let (out, ctx) = (self.outcomes.clone(), ctx.clone());
                let done: opfs::Done = Box::new(move |ok| {
                    out.borrow_mut().push(Outcome::Clean { ok });
                    ctx.request_repaint();
                });
                opfs::write_then(META.into(), json!({"dirty": false}).to_string().into_bytes(), Some(done));
            }
        }
        self.warning_message()
    }

    fn warning_message(&self) -> Option<String> {
        self.warning.message().map(|error| {
            filmcraft_ui_egui::i18n::fmt(
                filmcraft_ui_egui::i18n::t("Recovery paused for unsaved changes: {error}. Repair the invalid dimensions to resume."),
                &[("error", error)],
            )
        })
    }
}

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
    let name = meta.get("name").and_then(Value::as_str).unwrap_or("Recovered");
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
    if !opfs::available() || file.size() > KEEP_MAX {
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

/// Register the media kept in OPFS under their `/files/` paths; returns how many.
pub async fn restore_media() -> usize {
    let files = opfs::files_in("media").await;
    let n = files.len();
    for (name, f) in files {
        crate::fs::register_blob(&name, f.into());
    }
    n
}
