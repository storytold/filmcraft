//! Background native AVB import. The reviewed snapshot is committed once, never re-read at apply.
use crate::avid_catalog::Prepared;
use crate::{EngineError, Result, Session};
use filmcraft_interchange::avid_catalog::{self, TransferMode};
use filmcraft_project::{BinId, Project};
use serde_json::{Value, json};
use std::sync::{Arc, Mutex, atomic::Ordering};

fn bad(message: impl std::fmt::Display) -> EngineError {
    EngineError::Other(format!("Avid bin import: {message}"))
}
fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}
pub struct Preparation {
    job: u64,
    project: Arc<Project>,
    plan: Arc<Mutex<Option<Prepared>>>,
}
fn cancelled(progress: &filmcraft_export::Progress) -> Result<()> {
    if progress.cancel.load(Ordering::Relaxed) { Err(bad("cancelled; no project changes were made")) } else { Ok(()) }
}
pub(crate) fn prepare(s: &mut Session, p: &Value) -> Result<Value> {
    if cfg!(target_arch = "wasm32") {
        return Err(bad("native file preparation currently requires the desktop"));
    }
    let path = p["path"].as_str().filter(|v| !v.is_empty() && v.len() <= 4096 && !v.contains('\0')).ok_or_else(|| bad("choose an AVB path"))?.to_owned();
    let mode = match p.get("mode") {
        None => TransferMode::Sequences,
        Some(Value::String(v)) if v == "metadata" => TransferMode::Metadata,
        Some(Value::String(v)) if v == "media" => TransferMode::Media,
        Some(Value::String(v)) if v == "sequences" => TransferMode::Sequences,
        _ => return Err(bad("mode must be metadata, media or sequences")),
    };
    // Keep the original single-root parameter for existing automation clients.
    if p.get("mediaFolder").is_some_and(|v| !v.is_null()) && p.get("mediaFolders").is_some() {
        return Err(bad("use mediaFolders or mediaFolder, not both"));
    }
    let inputs: Vec<&Value> = match p.get("mediaFolders") {
        Some(Value::Array(values)) if values.len() <= 32 => values.iter().collect(),
        None => p.get("mediaFolder").filter(|v| !v.is_null()).into_iter().collect(),
        _ => return Err(bad("mediaFolders must be an array of up to 32 folders")),
    };
    let folders: Vec<String> = inputs
        .into_iter()
        .map(|value| {
            value
                .as_str()
                .filter(|v| !v.is_empty() && v.len() <= 4096 && !v.contains('\0') && std::path::Path::new(v).is_absolute() && std::path::Path::new(v).is_dir())
                .map(str::to_owned)
                .ok_or_else(|| bad("media folders must be existing absolute folders"))
        })
        .collect::<Result<_>>()?;
    if !folders.is_empty() && mode == TransferMode::Metadata {
        return Err(bad("folder search requires media or sequences mode"));
    }
    let parent = match p.get("parent") {
        None | Some(Value::Null) => None,
        Some(v) => Some(BinId(v.as_u64().ok_or_else(|| bad("invalid parent bin"))?)),
    };
    if p.get("wait").is_some_and(|v| !v.is_boolean()) {
        return Err(bad("wait must be a boolean"));
    }
    if s.avid_bin_preparation.as_ref().and_then(|old| s.jobs.iter().find(|j| j.id == old.job)).is_some_and(|j| !j.progress.finished.load(Ordering::Acquire)) {
        return Err(bad("another bin is being prepared; cancel or wait for it"));
    }
    let id = s.jobs.iter().map(|j| j.id).max().unwrap_or(0).checked_add(1).ok_or_else(|| bad("job ID overflow"))?;
    let job = crate::Job { id, label: "Read Avid bin".into(), progress: Default::default(), result: Default::default() };
    let project = s.project.clone();
    let source_project = project.clone();
    let services = s.services.clone();
    let plan = Arc::new(Mutex::new(None));
    let output = plan.clone();
    let progress = job.progress.clone();
    let result = job.result.clone();
    let run = move || {
        let started = web_time::Instant::now();
        let read = || -> Result<Prepared> {
            cancelled(&progress)?;
            let size = services.file_size(&path).map_err(bad)?;
            if size > filmcraft_interchange::avid_bin::MAX_BYTES as u64 {
                return Err(bad("file exceeds 512 MiB"));
            }
            let size = usize::try_from(size).map_err(bad)?;
            let mut bytes = Vec::new();
            bytes.try_reserve_exact(size).map_err(bad)?;
            progress.total.store(size as u64, Ordering::Relaxed);
            while bytes.len() < size {
                cancelled(&progress)?;
                let n = (size - bytes.len()).min(1024 * 1024);
                let chunk = services.read_range(&path, bytes.len() as u64, n).map_err(bad)?;
                if chunk.len() != n {
                    return Err(bad("bin changed while reading"));
                }
                bytes.extend_from_slice(&chunk);
                progress.done.store(bytes.len() as u64, Ordering::Relaxed);
            }
            *lock(&progress.status) = "Decoding Avid bin".into();
            let mut media_index = None;
            let mut prepared = if mode == TransferMode::Metadata {
                let plan = avid_catalog::plan_native(&bytes, &path).map_err(bad)?;
                let mut project = (*source_project).clone();
                let warnings = plan.warnings.clone();
                let bins = avid_catalog::apply(&mut project, plan, parent).map_err(bad)?;
                Prepared { project, result: json!({"bins":bins,"warnings":warnings,"mode":"metadata","mediaBinding":"unbound"}), first_sequence: None }
            } else {
                let document = avid_catalog::read_native_with_checkpoint(&bytes, &path, &|| {
                    cancelled(&progress).map_err(|e| filmcraft_interchange::Error::Other(e.to_string()))
                })
                .map_err(bad)?;
                if !folders.is_empty() {
                    *lock(&progress.status) = "Finding media by Avid identity".into();
                    let mut identities = std::collections::BTreeSet::new();
                    for bin in document.bins() {
                        for clip in bin["mobs"].as_array().into_iter().flatten().filter(|m| m["kind"] == "MasterMob") {
                            for track in clip["tracks"].as_array().into_iter().flatten() {
                                for reference in track["references"].as_array().into_iter().flatten() {
                                    if let Ok(id) = avid_catalog::full_identity(&reference["source_id"]) {
                                        identities.insert(id);
                                    }
                                    if identities.len() > 4096 {
                                        return Err(bad("import exceeds 4096 source identities; import without folder search"));
                                    }
                                }
                            }
                        }
                    }
                    media_index = Some(crate::component_relink::index_identities(identities, folders.clone(), &*services, progress.clone())?);
                    cancelled(&progress)?;
                    *lock(&progress.status) = "Binding Avid media".into();
                }
                crate::avid_catalog::prepare_parsed_with_index(
                    &source_project,
                    &*services,
                    &document,
                    crate::avid_catalog::ImportOptions { mode, allow_missing: true, best_effort: true },
                    parent,
                    &|| cancelled(&progress),
                    media_index.as_ref(),
                )?
            };
            if let Some(index) = media_index.as_ref() {
                *lock(&progress.status) = "Finding media by Avid identity".into();
                let ids = prepared.result["bound"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .map(|b| b["item"].as_u64().map(filmcraft_project::ItemId).ok_or_else(|| bad("invalid prepared item")))
                    .collect::<Result<Vec<_>>>()?;
                let links = crate::component_relink::link_prepared_index(&mut prepared.project, &ids, index, &*services, progress.clone())?;
                prepared.result["mediaBinding"] = json!(if links["offlineComponents"] == 0 && prepared.result["mediaBinding"] == "verified_components" {
                    "verified_components"
                } else {
                    "offline_components"
                });
                if let Some(bound) = prepared.result["bound"].as_array_mut() {
                    for b in bound {
                        let id = b["item"].as_u64().map(filmcraft_project::ItemId).ok_or_else(|| bad("invalid prepared item"))?;
                        let media = prepared.project.item(id).and_then(|i| i.as_media()).ok_or_else(|| bad("prepared media disappeared"))?;
                        if let filmcraft_project::MediaRef::Components { channels, picture } = &media.media {
                            b["offlineComponents"] = json!(
                                channels.iter().filter(|c| c.path.is_none()).count()
                                    + usize::from(picture.as_ref().is_some_and(|c| c.path.is_none() || c.format_pending))
                            );
                            b["preSearchWarnings"] = b["warnings"].take();
                        }
                    }
                }
                prepared.result["mediaSearch"] = links;
            }
            prepared.result["mode"] = json!(match mode {
                TransferMode::Metadata => "metadata",
                TransferMode::Media => "media",
                TransferMode::Sequences => "sequences",
            });
            cancelled(&progress)?;
            // Compare all bytes after decoding, including same-size edits. Never write the source.
            for (offset, chunk) in bytes.chunks(1024 * 1024).enumerate() {
                cancelled(&progress)?;
                if services.read_range(&path, (offset * 1024 * 1024) as u64, chunk.len()).map_err(bad)? != chunk {
                    return Err(bad("bin changed while decoding; close/save it in Avid and retry"));
                }
            }
            if services.file_size(&path).map_err(bad)? != size as u64 {
                return Err(bad("bin size changed while decoding"));
            }
            Ok(prepared)
        }();
        match read {
            Ok(plan) => {
                *lock(&output) = Some(plan);
                *lock(&progress.status) = "Ready to review".into();
                *lock(&result) = Some(Ok(filmcraft_export::Report {
                    path: String::new(),
                    frames: 0,
                    seconds: started.elapsed().as_secs_f64(),
                    bytes: 0,
                    render_fps: 0.0,
                    extra_files: Vec::new(),
                }));
            }
            Err(e) => {
                *lock(&progress.status) = e.to_string();
                *lock(&result) = Some(Err(e.to_string()));
            }
        }
        progress.finished.store(true, Ordering::Release);
    };
    let run = crate::export_tools::guard_job(job.progress.clone(), job.result.clone(), run);
    if p["wait"] == true {
        run();
    } else {
        std::thread::Builder::new().name("filmcraft-avid-import".into()).spawn(run).map_err(bad)?;
    }
    s.avid_bin_preparation = Some(Preparation { job: id, project, plan });
    s.jobs.push(job);
    Ok(json!({"job":id}))
}
fn pending<'a>(s: &'a Session, p: &Value) -> Result<&'a Preparation> {
    let id = p["job"].as_u64().ok_or_else(|| bad("need a preparation job"))?;
    s.avid_bin_preparation.as_ref().filter(|v| v.job == id).ok_or_else(|| bad("preparation is no longer available"))
}
pub(crate) fn preview(s: &Session, p: &Value) -> Result<Value> {
    let pending = pending(s, p)?;
    let job = s.jobs.iter().find(|j| j.id == pending.job).ok_or_else(|| bad("job no longer available"))?;
    if !Arc::ptr_eq(&s.project, &pending.project) {
        return Err(bad("project changed after preparation; read the bin again"));
    }
    if let Some(Err(e)) = lock(&job.result).as_ref() {
        return Err(bad(e));
    }
    if job.progress.cancel.load(Ordering::Relaxed) {
        return Err(bad("preparation cancelled"));
    }
    let plan = lock(&pending.plan);
    let report = plan.as_ref().map(|p| p.result.clone());
    Ok(json!({"job":pending.job,"finished":job.progress.finished.load(Ordering::Acquire),"report":report}))
}
pub(crate) fn import(s: &mut Session, p: &Value) -> Result<Value> {
    let preview = preview(s, p)?;
    if preview["finished"] != true {
        return Err(bad("preparation is still running"));
    }
    let pending = pending(s, p)?;
    let prepared = lock(&pending.plan).as_ref().cloned().ok_or_else(|| bad("no prepared import"))?;
    let result = prepared.result;
    s.edit("Import Avid Bin", move |project, state| {
        *project = prepared.project;
        if let Some(id) = prepared.first_sequence {
            state.active_sequence = Some(id);
            state.open_sequences.push(id);
            state.playheads.insert(id, filmcraft_time::Tick::ZERO);
        }
        Ok(())
    })?;
    s.avid_bin_preparation = None;
    crate::relink::refresh(s);
    Ok(result)
}

#[cfg(test)]
mod tests;
