//! Developer-only project-folder conversion. See docs/project-files.md.
use craft_fcproj_format::records::ProjectRecords;
use filmcraft_format::{MAX_PROJECT_BYTES, atomic_write_new};
use filmcraft_media::reader::read_regular_file_bounded;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::Path;

#[path = "paths.rs"]
mod paths;

pub(super) const ROOT: &str = "project.fcmod";
const FORMAT: &str = "filmcraft.project-folder";
const MANIFEST_LIMIT: u64 = 16 << 20;
const RECORD_LIMIT: usize = 100_000;

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Manifest {
    format: String,
    layout: u32,
    envelope: String,
    bins: Vec<(u64, String)>,
    items: Vec<(u64, String)>,
}

fn invalid(reason: impl std::fmt::Display) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, reason.to_string())
}

fn digest(data: &[u8]) -> String {
    format!("{:x}", Sha256::digest(data))
}

fn read_member(path: &Path, limit: u64) -> io::Result<Vec<u8>> {
    if !fs::symlink_metadata(path)?.is_file() {
        return Err(invalid(format!("expected a regular file at {}", path.display())));
    }
    read_regular_file_bounded(path, limit)
}

fn load_payload(folder: &Path, hash: &str, remaining: &mut u64) -> io::Result<Value> {
    if hash.len() != 64 || !hash.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)) {
        return Err(invalid("invalid SHA-256 digest"));
    }
    let bytes = read_member(&folder.join(hash), *remaining)?;
    if digest(&bytes) != hash {
        return Err(invalid("SHA-256 mismatch"));
    }
    *remaining -= bytes.len() as u64;
    Ok(serde_json::from_slice(&bytes)?)
}

fn load_group(folder: &Path, kind: &str, entries: &[(u64, String)], budget: &mut u64) -> io::Result<BTreeMap<u64, Value>> {
    let mut group = BTreeMap::new();
    for (id, hash) in entries {
        let data = load_payload(folder, hash, budget).map_err(|e| invalid(format!("{kind} {id}: {e}")))?;
        if data.get("id").and_then(Value::as_u64) != Some(*id) || group.insert(*id, data).is_some() {
            return Err(invalid(format!("mismatched or repeated {kind} {id}")));
        }
    }
    Ok(group)
}

fn assemble(folder: &Path, manifest: &Manifest, destination: &Path) -> io::Result<Vec<u8>> {
    if manifest.format != FORMAT || manifest.layout != 1 {
        return Err(invalid(format!("unsupported project folder format {:?}, layout {}", manifest.format, manifest.layout)));
    }
    if manifest.bins.len().saturating_add(manifest.items.len()) > RECORD_LIMIT {
        return Err(invalid("too many project records"));
    }
    let mut budget = MAX_PROJECT_BYTES;
    let envelope = load_payload(folder, &manifest.envelope, &mut budget).map_err(|e| invalid(format!("project metadata: {e}")))?;
    let bins = load_group(folder, "bin", &manifest.bins, &mut budget)?;
    let items = load_group(folder, "item", &manifest.items, &mut budget)?;
    let mut records = ProjectRecords { envelope, bins, items };
    paths::rebase(&mut records, &folder.join(ROOT), destination)?;
    let bytes = serde_json::to_vec(&records.into_document().map_err(invalid)?)?;
    filmcraft_format::decode(&bytes).map_err(invalid)?;
    Ok(bytes)
}

fn write_payload(folder: &Path, value: Value, write_new: &mut impl FnMut(&Path, &[u8]) -> io::Result<()>) -> io::Result<String> {
    let bytes = serde_json::to_vec(&value)?;
    let hash = digest(&bytes);
    let path = folder.join(&hash);
    match write_new(&path, &bytes) {
        Ok(()) => (),
        // Equal content may share a payload. assemble verifies every referenced byte before publication.
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => (),
        Err(e) => return Err(e),
    }
    Ok(hash)
}

pub(super) fn pack(source: &Path, folder: &Path, mut write_new: impl FnMut(&Path, &[u8]) -> io::Result<()>) -> io::Result<()> {
    let mut records = {
        let bytes = read_regular_file_bounded(source, MAX_PROJECT_BYTES)?;
        filmcraft_format::decode(&bytes).map_err(invalid)?;
        ProjectRecords::split(&bytes).map_err(invalid)?
    };
    if records.bins.len().saturating_add(records.items.len()) > RECORD_LIMIT {
        return Err(invalid("too many project records"));
    }
    let root = folder.join(ROOT);
    paths::rebase(&mut records, source, &root)?;
    fs::create_dir(folder)?; // Exclusively reserve a fresh destination. Never adopt an existing directory.
    let envelope = write_payload(folder, records.envelope, &mut write_new)?;
    let mut group = |values: BTreeMap<u64, Value>| -> io::Result<Vec<_>> {
        values.into_iter().map(|(id, value)| Ok((id, write_payload(folder, value, &mut write_new)?))).collect()
    };
    let manifest = Manifest { format: FORMAT.to_owned(), layout: 1, envelope, bins: group(records.bins)?, items: group(records.items)? };
    let bytes = serde_json::to_vec(&manifest)?;
    if bytes.len() as u64 > MANIFEST_LIMIT {
        return Err(invalid("manifest exceeds the folder layout's size limit"));
    }
    assemble(folder, &manifest, &root)?;
    #[cfg(unix)]
    fs::File::open(folder)?.sync_all()?;
    write_new(&root, &bytes)
}

pub(super) fn unpack(folder: &Path, destination: &Path) -> io::Result<()> {
    let manifest = serde_json::from_slice(&read_member(&folder.join(ROOT), MANIFEST_LIMIT)?)?;
    atomic_write_new(destination, &assemble(folder, &manifest, destination)?)
}
