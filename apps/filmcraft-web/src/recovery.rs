//! Auto-save and crash recovery in OPFS (the desktop recovery journal, mapped to the browser).
//!
//! - While the project has unsaved changes, a snapshot is written every few seconds to
//!   `recovery/snapshot.fcproj` with `recovery/meta.json` saying it is dirty; once the project is
//!   saved (downloaded) the meta is marked clean.
//! - Imported media are copied to `media/<name>` (the browser streams the copy; nothing goes
//!   through wasm memory), so the snapshot's `/files/<name>` references resolve after a reload.
//! - At start-up a dirty snapshot is reopened (`?norecover` skips it, `?fresh` also skips the media).
//!   A skipped snapshot stays dirty until this session writes its own ([`crate::recovery_policy`]).

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use filmcraft_engine::Session;
use serde_json::{Value, json};

use crate::opfs;
use crate::recovery_policy::{Action, Policy};

const SNAPSHOT: &str = "recovery/snapshot.fcproj";
const META: &str = "recovery/meta.json";
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
}

impl Autosave {
    /// Called every frame.
    pub fn tick(&mut self, s: &Session, ctx: &egui::Context) {
        if !opfs::available() {
            return;
        }
        let now = ctx.input(|i| i.time);
        for o in self.outcomes.borrow_mut().drain(..) {
            match o {
                Outcome::Snapshot { revision, ok } => self.policy.written(revision, ok, now),
                Outcome::Clean { ok } => self.policy.cleaned(ok, now),
            }
        }
        match self.policy.tick(now, s.revision, s.saved_revision) {
            Action::Nothing => {}
            Action::WakeAfter(secs) => ctx.request_repaint_after(std::time::Duration::from_secs_f64(secs)),
            Action::WriteSnapshot(revision) => {
                // Snapshot and meta both have to land; either failing retries the pair.
                let failed = Rc::new(Cell::new(false));
                let left = Rc::new(Cell::new(2u8));
                let done = |out: Rc<RefCell<Vec<Outcome>>>, failed: Rc<Cell<bool>>, left: Rc<Cell<u8>>, ctx: egui::Context| -> opfs::Done {
                    Box::new(move |ok| {
                        failed.set(failed.get() || !ok);
                        left.set(left.get().saturating_sub(1));
                        if left.get() == 0 {
                            out.borrow_mut().push(Outcome::Snapshot { revision, ok: !failed.get() });
                            ctx.request_repaint();
                        }
                    })
                };
                let meta = json!({"dirty": true, "name": s.project.name, "revision": s.revision, "time": js_sys::Date::now()});
                let d1 = done(self.outcomes.clone(), failed.clone(), left.clone(), ctx.clone());
                let d2 = done(self.outcomes.clone(), failed, left, ctx.clone());
                opfs::write_then(SNAPSHOT.into(), filmcraft_format::encode(&s.project, false), Some(d1));
                opfs::write_then(META.into(), meta.to_string().into_bytes(), Some(d2));
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

/// The unsaved snapshot of a session that ended without saving: (file name, bytes).
pub async fn load_snapshot() -> Option<(String, Vec<u8>)> {
    if !opfs::available() {
        return None;
    }
    let meta: Value = serde_json::from_slice(&opfs::read(META).await?).ok()?;
    if meta.get("dirty").and_then(Value::as_bool) != Some(true) {
        return None;
    }
    let bytes = opfs::read(SNAPSHOT).await?;
    let name = meta.get("name").and_then(Value::as_str).unwrap_or("Recovered").to_string();
    Some((format!("{name}.fcproj"), bytes))
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
