//! Atomic development import from a stable catalog snapshot; direct AVB decoding is separate.
use filmcraft_interchange::{
    avid_catalog::{self as adapter, TransferMode},
    avid_sequence,
};
use filmcraft_project::{BinId, ItemId, Project};
use serde_json::{Value, json};

use crate::{EngineError, Result, Session};

fn failure(e: impl std::fmt::Display) -> EngineError {
    EngineError::Other(format!("Avid catalog import: {e}"))
}

/// A catalog report is a document, not an authorization to follow its media locations.
/// Check every supplied location before binding can inspect or open any component.
fn check_report_locations(doc: &adapter::Document, report_path: &str) -> Result<()> {
    let document = std::path::Path::new(report_path);
    let mut pending: Vec<&Value> = doc.bins().iter().collect();
    let mut visited = 0usize;
    while let Some(value) = pending.pop() {
        visited = visited.checked_add(1).ok_or_else(|| failure("report structure exceeds limit"))?;
        if visited > 4_000_000 {
            return Err(failure("report structure exceeds limit"));
        }
        match value {
            Value::Object(object) => {
                if let Some(locations) = object.get("locations") {
                    let locations = locations.as_array().ok_or_else(|| failure("invalid report media locations"))?;
                    for location in locations {
                        let folder = location.get("folder").and_then(Value::as_str).ok_or_else(|| failure("invalid report media folder"))?;
                        let filename = location
                            .get("filename")
                            .and_then(Value::as_str)
                            .filter(|name| !name.is_empty() && name.len() <= 4096 && !name.contains(['/', '\\', '\0', ':']))
                            .ok_or_else(|| failure("invalid report media filename"))?;
                        crate::doc_path::resolve(folder, Some(document)).map_err(|_| failure("report media location is not allowed"))?;
                        let candidate = std::path::Path::new(folder).join(filename);
                        let candidate = candidate.to_str().ok_or_else(|| failure("invalid report media path"))?;
                        crate::doc_path::resolve(candidate, Some(document)).map_err(|_| failure("report media location is not allowed"))?;
                    }
                }
                pending.extend(object.values());
            }
            Value::Array(values) => pending.extend(values),
            _ => {}
        }
    }
    Ok(())
}

#[derive(Clone)]
pub(crate) struct Prepared {
    pub(crate) project: Project,
    pub(crate) result: Value,
    pub(crate) first_sequence: Option<ItemId>,
}

/// Complete every requested stage on a disposable copy before publishing an edit.
fn prepare(s: &Session, p: &Value) -> Result<Prepared> {
    let mode = match p.get("mode") {
        None => TransferMode::Metadata,
        Some(Value::String(v)) if v == "metadata" => TransferMode::Metadata,
        Some(Value::String(v)) if v == "media" => TransferMode::Media,
        Some(Value::String(v)) if v == "sequences" => TransferMode::Sequences,
        _ => return Err(failure("mode must be metadata, media or sequences")),
    };
    let allow_missing = match p.get("missingMedia").and_then(Value::as_str) {
        None if p.get("missingMedia").is_none() => true,
        Some("offline") => true,
        Some("error") => false,
        _ => return Err(failure("missingMedia must be offline or error")),
    };
    let parent = match p.get("parent") {
        None | Some(Value::Null) => None,
        Some(v) => Some(BinId(v.as_u64().ok_or_else(|| failure("parent must be a bin ID"))?)),
    };
    let bytes = read_report(s, p)?;
    let doc = adapter::read_document(&bytes).map_err(failure)?;
    // Metadata-only import retains these locations for later binding too.
    let path = p.get("path").and_then(Value::as_str).ok_or_else(|| failure("missing catalog path"))?;
    check_report_locations(&doc, path)?;
    prepare_parsed_with_index(&s.project, &*s.services, &doc, ImportOptions { mode, allow_missing, best_effort: false }, parent, &|| Ok(()), None)
}

#[derive(Clone, Copy)]
pub(crate) struct ImportOptions {
    pub mode: TransferMode,
    pub allow_missing: bool,
    pub best_effort: bool,
}
/// Shared transaction builder for JSON research reports and the native AVB decoder.
#[cfg(test)]
pub(crate) fn prepare_document(
    original: &Project,
    services: &dyn crate::Services,
    bytes: &[u8],
    options: ImportOptions,
    parent: Option<BinId>,
    checkpoint: &dyn Fn() -> Result<()>,
) -> Result<Prepared> {
    prepare_document_with_index(original, services, bytes, options, parent, checkpoint, None)
}
#[cfg(test)]
pub(crate) fn prepare_document_with_index(
    original: &Project,
    services: &dyn crate::Services,
    bytes: &[u8],
    options: ImportOptions,
    parent: Option<BinId>,
    checkpoint: &dyn Fn() -> Result<()>,
    index: Option<&crate::component_relink::IdentityIndex>,
) -> Result<Prepared> {
    checkpoint()?;
    let doc = adapter::read_document(bytes).map_err(failure)?;
    prepare_parsed_with_index(original, services, &doc, options, parent, checkpoint, index)
}
pub(crate) fn prepare_parsed_with_index(
    original: &Project,
    services: &dyn crate::Services,
    doc: &adapter::Document,
    options: ImportOptions,
    parent: Option<BinId>,
    checkpoint: &dyn Fn() -> Result<()>,
    index: Option<&crate::component_relink::IdentityIndex>,
) -> Result<Prepared> {
    let ImportOptions { mode, allow_missing, best_effort } = options;
    checkpoint()?;
    let mut errors = Vec::new();
    let mut media_binding_failed = false;
    let plan = adapter::plan_document(doc, mode).map_err(failure)?;
    let mut warnings = plan.warnings.clone();
    let mut project = original.clone();
    let bins = adapter::apply(&mut project, plan, parent).map_err(failure)?;
    let mut bound = Vec::new();
    let mut sequences = Vec::new();
    let mut first_sequence = None;
    for b in &bins {
        checkpoint()?;
        if mode != TransferMode::Metadata {
            // Referenced sequence sources bind too: a converted timeline must not hold clips
            // whose media can never be found. An unbindable one is not an import failure
            // though, even in strict mode: it is not bin contents, and it costs only its own
            // sequence clip a placeholder, which the sequence converter reports by name.
            // Subclips are deliberately absent: they bind through the master they restrict.
            let items = adapter::imported_items(&project, b.bin).map_err(failure)?;
            for (items, tolerate) in [(items.placed, best_effort), (items.referenced, true)] {
                // Keep decoder work sequential and bounded, without a session/history snapshot per batch.
                for chunk in items.chunks(if tolerate { 1 } else { 64 }) {
                    checkpoint()?;
                    let binding = match crate::avid_audio::prepare_binding_with_index(&project, services, chunk, true, allow_missing, index) {
                        Ok(v) => v,
                        Err(e) if tolerate => {
                            let item = project.item_mut(chunk[0]).ok_or_else(|| failure("imported item disappeared"))?;
                            let message = format!("{}: media remains offline: {e}", item.name);
                            item.metadata.insert("filmcraft:avid-import-error".into(), message.clone());
                            errors.push(message);
                            media_binding_failed = true;
                            continue;
                        }
                        Err(e) => return Err(e),
                    };
                    bound.extend(binding.reports.iter().cloned());
                    binding.apply(&mut project)?;
                }
            }
        }
        if mode == TransferMode::Sequences && b.compositions > 0 {
            let plan = avid_sequence::plan_document(&project, doc, b.bin, best_effort);
            let plan = match plan {
                Ok(v) => v,
                Err(e) if best_effort => {
                    errors.push(format!("{}: sequence conversion failed: {e}", b.name));
                    continue;
                }
                Err(e) => return Err(failure(e)),
            };
            if best_effort {
                errors.extend(plan.warnings.iter().filter(|s| s.contains("placeholder") || s.contains("effect skipped")).cloned());
            }
            warnings.extend(plan.warnings.iter().cloned());
            let ids = if best_effort {
                let (ids, rejected) = avid_sequence::apply_supported(&mut project, plan).map_err(failure)?;
                errors.extend(rejected);
                ids
            } else {
                avid_sequence::apply(&mut project, plan).map_err(failure)?
            };
            for id in ids {
                let item = project.item(id).ok_or_else(|| failure("new sequence missing"))?;
                let seq = project.sequence(id).ok_or_else(|| failure("new sequence missing"))?;
                sequences.push(json!({"item":id,"bin":b.bin,"name":item.name,"duration":seq.duration(),"frameRate":seq.settings.frame_rate}));
                first_sequence = first_sequence.or(Some(id));
            }
        }
    }
    let reconciled = bound.iter().filter(|b| b["audioTimingReconciled"] == true).count();
    if reconciled > 0 {
        warnings.push(format!("{reconciled} audio masters use Avid's nominal 23.976 clock corroborated by identified MXF track sample counts. Raw rates/counts are retained; component ends use declared whole samples, excluding stored padding."));
    }
    let has_offline = media_binding_failed || bound.iter().any(|b| b["offlineComponents"].as_u64().is_some_and(|n| n > 0));
    let result = json!({"bins":bins,"warnings":warnings,"errors":errors,"bound":bound,"sequences":sequences,
        "mediaBinding":if mode == TransferMode::Metadata { "unbound" } else if has_offline { "offline_components" } else { "verified_components" }});
    Ok(Prepared { project, result, first_sequence })
}

pub(crate) fn read_report(s: &Session, p: &Value) -> Result<Vec<u8>> {
    let path = p
        .get("path")
        .and_then(Value::as_str)
        .filter(|p| !p.is_empty() && p.len() <= 4096 && !p.contains('\0'))
        .ok_or_else(|| failure("need a nonempty catalog `path`"))?;
    let size = s.services.file_size(path).map_err(failure)?;
    if size > adapter::MAX_BYTES as u64 {
        return Err(failure("report exceeds 256 MiB"));
    }
    let limit = usize::try_from(size).ok().and_then(|n| n.checked_add(1)).ok_or_else(|| failure("file size overflow"))?;
    let bytes = s.services.read_range(path, 0, limit).map_err(failure)?;
    if bytes.len() as u64 != size || s.services.file_size(path).map_err(failure)? != size {
        return Err(failure("catalog size changed while reading; retry a stable report"));
    }
    Ok(bytes)
}

/// Read-only preflight of all requested stages. Returned IDs are prospective, not reserved.
pub fn preview(s: &Session, p: &Value) -> Result<Value> {
    Ok(prepare(s, p)?.result)
}

/// Transfer all supported bin contents in one undoable transaction.
pub fn import(s: &mut Session, p: &Value) -> Result<Value> {
    let Prepared { project, result, first_sequence } = prepare(s, p)?;
    s.edit("Import Avid Catalog", move |destination, state| {
        *destination = project;
        if let Some(id) = first_sequence {
            state.active_sequence = Some(id);
            state.open_sequences.push(id);
            state.playheads.insert(id, filmcraft_time::Tick::ZERO);
        }
        Ok(())
    })?;
    crate::relink::refresh(s);
    Ok(result)
}
