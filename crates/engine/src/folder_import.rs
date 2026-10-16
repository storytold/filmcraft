//! `file.import` with `bins: true`: a folder among the paths is imported as a bin of the same name
//! holding its media, and its subfolders as sub-bins. Plain files among the paths import as usual.
//!
//! The walk goes through [`crate::Services::list_entries`], so it works wherever the host can list
//! a directory (native; not the web), and it is capped: a huge or looping tree cannot hang it.

use std::path::Path;

use filmcraft_project::BinId;
use serde_json::{Value, json};

use crate::commands::bad;
use crate::{EngineError, Result, Session};

/// Folders nested deeper than this are not entered.
pub const MAX_DEPTH: usize = 16;
/// Directory entries read in one import, over all folders.
pub const MAX_ENTRIES: usize = 20_000;

/// What the walk has gathered so far.
#[derive(Default)]
struct Walk {
    items: Vec<u64>,
    bins: Vec<u64>,
    errors: Vec<String>,
    /// Entries read so far, against [`MAX_ENTRIES`].
    read: usize,
}

/// Run `file.import` for `p` (`paths`, `bin`) with folders as bins. One undo step.
pub(crate) fn import(s: &mut Session, p: &Value) -> Result<Value> {
    let paths: Vec<String> = match p.get("paths").and_then(Value::as_array) {
        Some(a) => a.iter().filter_map(|v| v.as_str().map(str::to_string)).collect(),
        None => p.get("path").and_then(Value::as_str).map(|x| vec![x.to_string()]).unwrap_or_default(),
    };
    if paths.is_empty() {
        return Err(bad("file.import", "need `paths`"));
    }
    // `replace` starts a new project first; do it before the folders, not after, or it would wipe them
    let mut p = p.clone();
    if p.get("replace").and_then(Value::as_bool).unwrap_or(false) {
        let name = s.project.name.clone();
        s.execute("file.newProject", json!({ "name": name }))?;
        p["replace"] = json!(false);
    }
    let p = &p;
    let parent = p.get("bin").and_then(Value::as_u64).map(BinId);
    s.edit_group("Import", |s| {
        let mut walk = Walk::default();
        let mut files = Vec::new();
        for path in paths {
            match s.services.list_entries(&path) {
                Some(Ok(_)) => folder(s, &path, parent, 0, &mut walk),
                _ => files.push(path),
            }
        }
        if !files.is_empty() {
            let mut q = p.clone();
            q["paths"] = json!(files);
            q["bins"] = json!(false);
            match s.execute("file.import", q) {
                Ok(v) => absorb(&mut walk, &v),
                Err(e) => walk.errors.push(e.to_string()),
            }
        }
        if walk.items.is_empty() && walk.bins.is_empty() && !walk.errors.is_empty() {
            return Err(EngineError::Other(walk.errors.join("; ")));
        }
        Ok(json!({"items": walk.items, "bins": walk.bins, "errors": walk.errors}))
    })
}

/// Import the folder `dir` as a bin under `parent`, then its files and subfolders into it.
fn folder(s: &mut Session, dir: &str, parent: Option<BinId>, depth: usize, walk: &mut Walk) {
    let name = Path::new(dir).file_name().map_or_else(|| dir.to_string(), |n| n.to_string_lossy().into_owned());
    let mut params = json!({"name": name});
    if let Some(parent) = parent {
        params["parent"] = json!(parent.0);
    }
    let bin = match s.execute("file.newBin", params) {
        Ok(v) => v["bin"].as_u64().map(BinId),
        Err(e) => {
            walk.errors.push(format!("{dir}: {e}"));
            None
        }
    };
    let Some(bin) = bin else { return };
    walk.bins.push(bin.0);
    let mut entries = match s.services.list_entries(dir) {
        Some(Ok(e)) => e,
        Some(Err(e)) => {
            walk.errors.push(format!("{dir}: {e}"));
            return;
        }
        None => return,
    };
    walk.read = walk.read.saturating_add(entries.len());
    if walk.read > MAX_ENTRIES {
        walk.errors.push(format!("{dir}: stopped, a folder import reads at most {MAX_ENTRIES} entries"));
        return;
    }
    entries.retain(|e| !e.name.starts_with('.'));
    entries.sort_by_key(|e| e.name.to_lowercase());
    let join = |name: &str| Path::new(dir).join(name).to_string_lossy().into_owned();
    let files: Vec<String> = entries.iter().filter(|e| !e.is_dir).map(|e| join(&e.name)).filter(|f| filmcraft_media::is_importable(Path::new(f))).collect();
    if !files.is_empty() {
        match s.execute("file.import", json!({"paths": files, "bin": bin.0})) {
            Ok(v) => absorb(walk, &v),
            Err(e) => walk.errors.push(format!("{dir}: {e}")),
        }
    }
    for sub in entries.iter().filter(|e| e.is_dir) {
        if depth + 1 >= MAX_DEPTH {
            walk.errors.push(format!("{}: not entered, folders nest at most {MAX_DEPTH} deep", join(&sub.name)));
            continue;
        }
        if walk.read > MAX_ENTRIES {
            break;
        }
        folder(s, &join(&sub.name), Some(bin), depth + 1, walk);
    }
}

/// Take the items and errors of a `file.import` result into the walk.
fn absorb(walk: &mut Walk, v: &Value) {
    walk.items.extend(v["items"].as_array().into_iter().flatten().filter_map(Value::as_u64));
    walk.errors.extend(v["errors"].as_array().into_iter().flatten().filter_map(|e| e.as_str().map(str::to_string)));
}
