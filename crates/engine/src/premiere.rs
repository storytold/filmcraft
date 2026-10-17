//! Native Premiere import with owned snapshots and read-only source handling. MIT OR Apache-2.0.
use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use filmcraft_interchange::ImportOptions;
use filmcraft_project::{
    ItemKind, MediaRef,
    workflow::{PremiereImport, SourceMedia},
};
use filmcraft_time::FrameRate;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::commands::{CommandSpec, always, bad};
use crate::{EngineError, Result, Session};

const COMMAND: &str = "premiere.import";
fn io(e: std::io::Error) -> EngineError {
    EngineError::Other(format!("Premiere import: {e}"))
}
fn string<'a>(p: &'a Value, key: &str) -> Result<Option<&'a str>> {
    match p.get(key) {
        None => Ok(None),
        Some(Value::String(s)) if !s.is_empty() => Ok(Some(s)),
        _ => Err(bad(COMMAND, format!("{key} must be a nonempty string"))),
    }
}
pub(crate) fn is_project(path: &str) -> bool {
    Path::new(path).extension().is_some_and(|e| e.eq_ignore_ascii_case("prproj"))
}

pub(crate) fn hash_file(path: &Path) -> Result<(String, u64)> {
    let mut f = std::fs::File::open(path).map_err(io)?;
    if !f.metadata().map_err(io)?.is_file() {
        return Err(bad(COMMAND, "source must be a regular file"));
    }
    let mut hash = Sha256::new();
    let mut size = 0u64;
    let mut buf = [0u8; 65536];
    loop {
        let n = f.read(&mut buf).map_err(io)?;
        if n == 0 {
            break;
        }
        hash.update(&buf[..n]);
        size = size.saturating_add(n as u64);
    }
    Ok((format!("{:x}", hash.finalize()), size))
}
fn write_new(path: &Path, bytes: &[u8]) -> Result<()> {
    let mut f = std::fs::OpenOptions::new().write(true).create_new(true).open(path).map_err(io)?;
    f.write_all(bytes).map_err(io)?;
    f.sync_all().map_err(io)
}
fn copy_new(source: &Path, dest: &Path) -> Result<()> {
    let mut input = std::fs::File::open(source).map_err(io)?;
    let mut output = std::fs::OpenOptions::new().write(true).create_new(true).open(dest).map_err(io)?;
    std::io::copy(&mut input, &mut output).map_err(io)?;
    output.sync_all().map_err(io)
}

/// Only a directory created by this invocation is removed on failure. Existing destinations are
/// always refused, including dangling symlinks. Successful snapshots survive undo.
struct OwnedDirectory {
    path: PathBuf,
    committed: bool,
}
impl Drop for OwnedDirectory {
    fn drop(&mut self) {
        if !self.committed {
            let _ = std::fs::remove_dir_all(&self.path);
        }
    }
}

pub fn import(s: &mut Session, p: &Value) -> Result<Value> {
    if s.services.export_in_memory() {
        return Err(bad(COMMAND, "Premiere link/copy import requires a native filesystem host"));
    }
    if !p.is_object() {
        return Err(bad(COMMAND, "parameters must be an object"));
    }
    let source = Path::new(string(p, "path")?.ok_or_else(|| bad(COMMAND, "need path"))?).canonicalize().map_err(io)?;
    if !source.is_file() {
        return Err(bad(COMMAND, "source must be a regular project file"));
    }
    let mode = string(p, "mediaMode")?.unwrap_or("link");
    if !matches!(mode, "link" | "copy") {
        return Err(bad(COMMAND, "mediaMode must be link or copy"));
    }
    let fps: Option<FrameRate> =
        p.get("frameRate").map(|v| serde_json::from_value(v.clone())).transpose().map_err(|e| bad(COMMAND, format!("frameRate: {e}")))?;
    let mut bytes = Vec::new();
    std::fs::File::open(&source).map_err(io)?.take(filmcraft_interchange::premiere::MAX_DOCUMENT_BYTES as u64 + 1).read_to_end(&mut bytes).map_err(io)?;
    let opts = ImportOptions {
        base_dir: source.parent().map(|p| p.to_string_lossy().into_owned()),
        name: source.file_stem().map(|p| p.to_string_lossy().into_owned()),
        ..Default::default()
    };
    let (mut fragment, tree, mut report) = filmcraft_interchange::premiere::import(&bytes, &opts, fps).map_err(|e| bad(COMMAND, e.to_string()))?;
    let requested = string(p, "sequence")?;
    let selected = if let Some(name) = requested {
        tree.sequences.iter().position(|seq| seq.name == name).ok_or_else(|| bad(COMMAND, "unknown sequence name"))?
    } else {
        tree.sequences.iter().position(|seq| seq.name == "Sequence 01").unwrap_or_else(|| {
            tree.sequences
                .iter()
                .enumerate()
                .max_by_key(|(_, seq)| {
                    let items: Vec<_> = seq.tracks.iter().filter(|t| t.kind == "video").flat_map(|t| &t.items).collect();
                    (items.len(), items.iter().map(|i| i.end).max().unwrap_or_default())
                })
                .map(|(index, _)| index)
                .unwrap_or(0)
        })
    };
    if selected < fragment.sequences.len() {
        fragment.sequences.swap(0, selected);
    }
    let source_hash = format!("{:x}", Sha256::digest(&bytes));
    let destination = if let Some(d) = string(p, "destination")? {
        PathBuf::from(d)
    } else {
        let stem = source.file_stem().and_then(|s| s.to_str()).unwrap_or("Premiere");
        let parent = source.parent().ok_or_else(|| bad(COMMAND, "source has no parent directory"))?;
        (1..=10000)
            .map(|n| parent.join(format!("{stem}.filmcraft-import-{n}")))
            .find(|p| std::fs::symlink_metadata(p).is_err_and(|e| e.kind() == std::io::ErrorKind::NotFound))
            .ok_or_else(|| bad(COMMAND, "choose an unused destination directory"))?
    };
    let parent = destination.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or(Path::new(".")).canonicalize().map_err(io)?;
    let destination = parent.join(destination.file_name().ok_or_else(|| bad(COMMAND, "destination must name a new directory"))?);
    std::fs::create_dir(&destination).map_err(io)?;
    let mut owned = OwnedDirectory { path: destination.clone(), committed: false };
    let snapshot = destination.join("source.prproj");
    write_new(&snapshot, &bytes)?;
    let mut records = BTreeMap::<String, SourceMedia>::new();
    let media_dir = destination.join("media");
    if mode == "copy" {
        std::fs::create_dir(&media_dir).map_err(io)?;
    }
    for item in fragment.project.items.values_mut() {
        let ItemKind::Media(m) = &mut item.kind else { continue };
        let MediaRef::File { path } = &mut m.media else { continue };
        let original = path.clone();
        if !records.contains_key(&original) {
            let canonical = Path::new(&original).canonicalize().or_else(|error| {
                if let Some(path) = original.strip_prefix("/Volumes/Macintosh HD/") { Path::new("/").join(path).canonicalize() } else { Err(error) }
            });
            let mut record = SourceMedia { original_path: original.clone(), imported_path: original.clone(), sha256: None, bytes: None };
            match canonical {
                Ok(src) if src.is_file() => {
                    let (hash, size) = hash_file(&src)?;
                    record.original_path = src.to_string_lossy().into_owned();
                    record.sha256 = Some(hash.clone());
                    record.bytes = Some(size);
                    if mode == "copy" {
                        let ext = src
                            .extension()
                            .and_then(|e| e.to_str())
                            .filter(|s| s.len() <= 16 && s.chars().all(|c| c.is_ascii_alphanumeric()))
                            .unwrap_or("media");
                        let target = media_dir.join(format!("{hash}.{ext}"));
                        if !target.exists() {
                            copy_new(&src, &target)?;
                        }
                        if hash_file(&target)?.0 != hash {
                            return Err(bad(COMMAND, "media changed while copying; import cancelled"));
                        }
                        record.imported_path = target.to_string_lossy().into_owned();
                    } else {
                        record.imported_path = src.to_string_lossy().into_owned();
                    }
                }
                _ => filmcraft_interchange::premiere::warning(&mut report, format!("missing media: {original}; offline placeholder retained")),
            }
            records.insert(original.clone(), record);
        }
        if let Some(record) = records.get(&original) {
            *path = record.imported_path.clone();
        }
        if let Ok(src) = s.media.open_file(path, &*s.services) {
            m.info = src.info().clone();
            m.offline = false;
            m.identity = crate::relink::identity_of(&*s.services, path).ok();
        } else {
            m.offline = true;
            filmcraft_interchange::premiere::warning(&mut report, format!("media could not be decoded: {path}"));
        }
    }
    fragment.project.resolve_placed_auto_points(|_| true, |_| true);
    // Refuse a changed project input rather than attaching the wrong source hash to its snapshot.
    if hash_file(&source)?.0 != source_hash {
        return Err(bad(COMMAND, "project changed during import; try again"));
    }
    let warnings: Vec<String> = report.entries.iter().map(|e| e.message.clone()).collect();
    let record = PremiereImport {
        source_path: source.to_string_lossy().into_owned(),
        source_sha256: source_hash.clone(),
        snapshot_path: snapshot.to_string_lossy().into_owned(),
        media_mode: mode.into(),
        media: records.into_values().collect(),
        source_tree: serde_json::to_value(tree).map_err(|e| bad(COMMAND, e.to_string()))?,
        warnings: warnings.clone(),
    };
    let record_json = serde_json::to_vec_pretty(&record).map_err(|e| bad(COMMAND, e.to_string()))?;
    write_new(&destination.join("provenance.json"), &record_json)?;
    let before: std::collections::BTreeSet<_> = s.project.items.keys().copied().collect();
    let sequences = s.edit("Import Premiere project", |project, state| {
        let sequences = filmcraft_interchange::merge_into(project, fragment, None);
        std::sync::Arc::make_mut(project.workflow.get_or_insert_default()).imports.push(std::sync::Arc::new(record));
        if let Some(&id) = sequences.first() {
            state.active_sequence = Some(id);
            if !state.open_sequences.contains(&id) {
                state.open_sequences.push(id);
            }
        }
        Ok(sequences)
    })?;
    owned.committed = true;
    for item in s.project.items.values().filter(|i| !before.contains(&i.id)) {
        if let ItemKind::Media(m) = &item.kind
            && !m.offline
            && let MediaRef::File { path } = &m.media
            && let Ok(source) = s.media.open_file(path, &*s.services)
        {
            s.media.insert_file(item.id, path, source);
        }
    }
    let offline: Vec<_> = s
        .project
        .items
        .values()
        .filter_map(|i| match &i.kind {
            ItemKind::Media(m) if m.offline => Some(i.name.clone()),
            _ => None,
        })
        .collect();
    Ok(
        json!({"format":"Premiere", "sequences":sequences, "destination":destination, "sourceSha256":source_hash, "mediaMode":mode, "offlineMedia":offline, "report":warnings}),
    )
}

pub fn commands() -> Vec<CommandSpec> {
    vec![CommandSpec {
        id: COMMAND,
        label: "Import Premiere Project",
        menu: &[],
        shortcut: None,
        params: r#"{"path":str,"destination":str? (new directory),"mediaMode":"link"|"copy","sequence":str?,"frameRate":{"num":int,"den":int}?}"#,
        enabled: always,
        run: import,
        journal: true,
    }]
}
