//! File-access confinement for automation (#985): `filmcraft-cli mcp --automation-read-root <dir>
//! --automation-write-root <dir>`, the flags VectorCraft and EffectCraft ship. While roots are in
//! force, every command an agent runs may only read files under a read root and only write files
//! under a write root; a root left out grants no access of its kind, and without roots nothing is
//! checked.
//!
//! The check runs in [`crate::FilmcraftMcp::run`] before a command is dispatched, on both backends:
//! the headless engine session and the bridge to a live app (where the command would otherwise run
//! confined to nothing but the app's own rights). What the person at the keyboard does — `--project`
//! at start-up, the desktop app's own panels — is not confined.
//!
//! - A root must be an existing folder and is canonicalized, so its own links are resolved.
//! - A path is made absolute against the working directory and its links are followed: the nearest
//!   existing folder's real path is taken and the rest appended (`..` below a folder that doesn't
//!   exist is refused, and so is a link that leads nowhere).
//! - The result must be the root or inside it, compared by whole components (`/work2` is not inside
//!   `/work`), without regard to case on Windows, whose file systems ignore it.
//! - A path must be absolute. A relative one is refused: it would be checked against this
//!   process's working directory and, in bridge mode, opened against the app's.
//! - Every command is classified, and an unclassified one is refused (it fails closed): [`READS`]
//!   and [`WRITES`] name its file parameters, [`REFUSED`] says why it can't run confined, and
//!   [`no_files::NO_FILES`] lists the commands that touch no file an agent chooses. A test walks the
//!   engine's command registry, so a new command fails it until it is classified.
//! - A command that would pick the place itself when its file parameter is left out must be given
//!   one (`tts.create` without `dir` writes beside the project). `file.save` needs a `path` too.
//! - Any other absolute path anywhere in the params, and any string under a path-like key, is
//!   checked the same way, so a path can't slip through an unnamed key; `prefs.set` (whose values
//!   name folders the app writes into later) is held to the write root.
//! - The bridge's UI-driving tools are refused while confined: they could drive the live app's
//!   file dialogs past the roots.
//! - In a headless session, [`crate::confined_services`] checks every path again when it is opened,
//!   which also covers the media a project file names. In bridge mode the session lives in the
//!   app, so such paths are not covered there.
//!
//! Left alone: temporary files, and the app's own settings and libraries, which it keeps under
//! fixed names in its data folder (`--data-dir`) and no parameter can move. Commands that add
//! agent-written files to those libraries are in [`REFUSED`].
//!
//! A path is checked, then opened: a process that changes the folders in between can still win
//! that race. The roots keep an agent to the files it was given; they don't guard against other
//! programs on the computer.

use std::ffi::OsString;
use std::path::{Component, Path, PathBuf};

use serde_json::Value;

mod no_files;
use no_files::NO_FILES;

/// Read access or write access.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Access {
    Read,
    Write,
}

/// The folders automation may read and write; a root left out grants no access of its kind.
#[derive(Debug, Clone)]
pub struct AutomationRoots {
    read: Option<Root>,
    write: Option<Root>,
}

#[derive(Debug, Clone)]
struct Root {
    /// The folder made absolute with its links followed: what paths are compared with.
    resolved: PathBuf,
    /// The same without Windows' `\\?\` prefix, for people.
    shown: PathBuf,
}

impl Root {
    /// The folder at `path`, which must exist.
    fn open(path: &str, access: Access) -> Result<Self, String> {
        let kind = match access {
            Access::Read => "read",
            Access::Write => "write",
        };
        let resolved = std::fs::canonicalize(path)
            .and_then(|p| if p.is_dir() { Ok(p) } else { Err(std::io::ErrorKind::NotADirectory.into()) })
            .map_err(|e| format!("cannot open the automation {kind} root `{path}`: {e}"))?;
        Ok(Self { shown: plain(resolved.clone()), resolved })
    }
}

impl AutomationRoots {
    /// The roots `read` and `write`, which must be existing folders; `None` when neither is given
    /// (nothing is confined then).
    pub fn new(read: Option<&str>, write: Option<&str>) -> Result<Option<Self>, String> {
        if read.is_none() && write.is_none() {
            return Ok(None);
        }
        Ok(Some(Self { read: read.map(|p| Root::open(p, Access::Read)).transpose()?, write: write.map(|p| Root::open(p, Access::Write)).transpose()? }))
    }

    /// The read root, absolute with its links followed.
    pub fn read_root(&self) -> Option<&Path> {
        self.read.as_ref().map(|r| r.shown.as_path())
    }

    /// The write root, absolute with its links followed.
    pub fn write_root(&self) -> Option<&Path> {
        self.write.as_ref().map(|r| r.shown.as_path())
    }

    /// One line naming the roots in force (the start-up diagnostic, `get_info` instructions).
    pub fn describe(&self) -> String {
        let show = |r: Option<&Root>| r.map_or_else(|| "none".to_string(), |r| r.shown.display().to_string());
        format!("read root {}, write root {}", show(self.read.as_ref()), show(self.write.as_ref()))
    }

    /// May the file or folder at `path` be read (or written)? `Err` says why not.
    pub fn check(&self, path: &str, access: Access) -> Result<(), String> {
        let (root, kind) = match access {
            Access::Read => (self.read.as_ref(), "read"),
            Access::Write => (self.write.as_ref(), "write"),
        };
        let Some(root) = root else {
            return Err(format!("automation path rejected: no {kind} root is in force: {path}"));
        };
        if inside(&resolve(path)?, &root.resolved) {
            return Ok(());
        }
        Err(format!("automation path rejected: outside the {kind} root {}: {path}", root.shown.display()))
    }

    /// Check command `id` with `params` before it runs. A command that isn't classified is refused;
    /// a classified one must keep every file parameter inside the roots, and name one where leaving
    /// it out would let the command pick the place itself.
    pub fn check_command(&self, id: &str, params: &Value) -> Result<(), String> {
        if let Some((_, why)) = REFUSED.iter().find(|(c, _)| *c == id) {
            return Err(format!("`{id}` is not available while file access is confined: {why}"));
        }
        if id == "media.makeOffline" && params.get("deleteFiles").is_some_and(|v| !matches!(v, Value::Null | Value::Bool(false))) {
            return Err(format!("`{id}` with `deleteFiles` is not available while file access is confined: it deletes the files the project names"));
        }
        let named = |table: &[(&str, &'static [&'static str])]| table.iter().find(|(c, _)| *c == id).map(|(_, keys)| *keys);
        let (writes, reads) = (named(WRITES), named(READS));
        if writes.is_none() && reads.is_none() && !NO_FILES.contains(&id) {
            return Err(format!("`{id}` is not available while file access is confined: it isn't known to stay inside the roots"));
        }
        for (table, access) in [(reads, Access::Read), (writes, Access::Write)] {
            let Some(keys) = table else { continue };
            let given = keys.iter().filter(|k| params.get(**k).is_some_and(|v| !v.is_null())).count();
            let discarding = id == "audio.voiceover.stop" && params.get("discard") == Some(&Value::Bool(true));
            if given == 0 && !keys.is_empty() && !LOCATION_OPTIONAL.contains(&id) && !discarding {
                return Err(format!("`{id}` needs `{}` while file access is confined: without it the command picks the place itself", keys.join("` or `")));
            }
            for key in keys {
                match params.get(*key).filter(|v| !v.is_null()) {
                    None => {}
                    Some(Value::String(s)) if NAME_OR_PATH.contains(&(id, *key)) && !s.contains(['/', '\\', '.']) => {}
                    Some(Value::String(s)) => self.check(s, access)?,
                    Some(Value::Array(a)) => {
                        for v in a {
                            match v.as_str() {
                                Some(s) => self.check(s, access)?,
                                None => return Err(format!("`{id}`: `{key}` must be file paths")),
                            }
                        }
                    }
                    Some(_) => return Err(format!("`{id}`: `{key}` must be a file path")),
                }
            }
        }
        if id == "file.importAaf"
            && let Some(p) = params.get("path").and_then(Value::as_str)
        {
            self.check(p, Access::Write).map_err(|e| format!("{e} (`{id}` writes the embedded audio beside the file)"))?;
        }
        // The rest of the params: what a command writes is held to the write root throughout.
        let access = if writes.is_some() { Access::Write } else { Access::Read };
        let skip: Vec<&str> =
            reads.unwrap_or(&[]).iter().chain(writes.unwrap_or(&[])).copied().chain(IGNORED_KEYS.iter().filter(|(c, _)| *c == id).map(|(_, k)| *k)).collect();
        let mut other = vec![];
        other_paths(params, &skip, id == "prefs.set", &mut other).map_err(|e| format!("`{id}`: {e}"))?;
        for p in other {
            self.check(&p, access)?;
        }
        Ok(())
    }
}

/// Commands that read files named by these parameters.
const READS: &[(&str, &[&str])] = &[
    ("file.open", &["path"]),
    ("file.import", &["paths"]),
    ("file.importAaf", &["path"]),
    ("file.importFromMediaBrowser", &["paths"]),
    ("file.importImageSequence", &["path"]),
    ("file.mediaPropertiesFile", &["path"]),
    ("file.newProjectFromTemplate", &["template"]),
    ("media.relink", &["path"]),
    ("media.replaceFootage", &["path"]),
    ("media.reconnectFullRes", &["path"]),
    ("media.attachProxies", &["path", "paths"]),
    ("media.autoRelink", &["folder", "to"]),
    ("media.search", &["folder"]),
    ("mediaBrowser.navigate", &["path"]),
    ("mediaBrowser.list", &["path"]),
    ("mediaBrowser.probe", &["path"]),
    ("mediaBrowser.openInSource", &["path"]),
    ("mediaBrowser.favorite", &["path"]),
    ("mediaBrowser.import", &["paths"]),
    ("mediaBrowser.select", &["paths"]),
    ("lut.import", &["path"]),
    ("lumetri.setInputLut", &["path"]),
    ("lumetri.setLook", &["path"]),
    ("lumetri.presets", &["folder"]),
    ("lumetri.presetThumbnails", &["folder"]),
    ("captions.import", &["path"]),
    ("graphics.newFromFile", &["path"]),
    ("graphics.template.apply", &["template"]),
    ("graphics.template.thumbnail", &["template"]),
    ("edit.pasteMedia", &["paths"]),
];

/// Commands that write files named by these parameters.
const WRITES: &[(&str, &[&str])] = &[
    ("file.save", &["path"]),
    ("file.saveAs", &["path"]),
    ("file.saveCopy", &["path"]),
    ("file.exportMedia", &["path"]),
    ("file.exportInterchange", &["path"]),
    ("file.exportEdl", &["path"]),
    ("file.exportFcp7Xml", &["path"]),
    ("file.exportFcpxml", &["path"]),
    ("file.exportOtio", &["path"]),
    ("file.exportAaf", &["path"]),
    ("file.exportOmf", &["path"]),
    ("file.exportFrame", &["path"]),
    ("file.exportAle", &["path"]),
    ("file.exportSelectionProject", &["path"]),
    ("file.exportGraphicsTemplate", &["path"]),
    ("file.saveAsTemplate", &["path"]),
    ("file.projectSettings.scratchDisks", &["captured", "videoPreviews", "audioPreviews", "autoSave"]),
    ("markers.exportCsv", &["path"]),
    ("lut.export", &["path"]),
    ("captions.export", &["path"]),
    ("graphics.template.export", &["path"]),
    ("graphics.template.thumbnail", &["path"]),
    ("presets.export", &["path"]),
    ("shortcuts.export", &["path"]),
    ("labels.exportPreset", &["path"]),
    ("export.presets.export", &["path"]),
    ("export.queue.add", &["path"]),
    ("export.quick", &["path"]),
    ("export.resolve", &["path"]),
    ("clip.extractAudio", &["dir"]),
    ("audio.voiceover.stop", &["dir"]),
    ("tts.create", &["dir"]),
    ("tts.edit", &["dir"]),
    ("tts.preview", &["path"]),
    ("lumetri.presetThumbnails", &["path"]),
    ("prefs.set", &[]),
];

/// Commands in [`READS`] or [`WRITES`] that touch no file when their parameter is left out.
const LOCATION_OPTIONAL: &[&str] = &[
    "lumetri.setInputLut",
    "lumetri.setLook",
    "lumetri.presets",
    "lumetri.presetThumbnails",
    "graphics.template.thumbnail",
    "file.projectSettings.scratchDisks",
    "export.resolve",
    "tts.preview",
];

/// Parameters that hold a library name or a path: only a value that could be a path is checked.
const NAME_OR_PATH: &[(&str, &str)] =
    &[("file.newProjectFromTemplate", "template"), ("graphics.template.apply", "template"), ("graphics.template.thumbnail", "template")];

/// Path-like keys that name no file here.
const IGNORED_KEYS: &[(&str, &str)] = &[
    ("effects.list", "folder"),   // a folder of the Effects panel
    ("masks.add", "path"),        // the mask's outline
    ("media.autoRelink", "from"), // the part of the stored paths to replace: not opened
    ("masks.set", "path"),        // the mask's outline
    ("mixer.setStrip", "output"), // the bus the strip feeds
];

/// Commands an agent can't run while confined, and why.
const REFUSED: &[(&str, &str)] = &[
    ("file.saveAll", "it saves to the project's own path; use `file.save` with a `path`"),
    ("file.autoSaveNow", "it writes to the app's auto-save folder"),
    ("file.listAutoSaves", "it reads the app's auto-save folder"),
    ("file.recover", "it reads the app's recovery folder"),
    ("file.recoveryList", "it reads the app's recovery folder"),
    ("file.discardRecovery", "it deletes from the app's recovery folder"),
    ("file.reveal", "it opens the system's file manager"),
    ("file.projectManager", "it copies the media a project names, wherever that is"),
    ("edit.editOriginal", "it opens the media a project names in another application"),
    ("clip.updateMetadata", "it writes sidecar files beside the media a project names"),
    ("media.refreshChanged", "it looks at the media a project names, wherever that is"),
    ("media.createProxies", "it reads the media a project names, wherever that is"),
    ("project.ingestSettings", "ingest copies imported files to a folder of its own"),
    ("mediaBrowser.roots", "it lists the computer's drives and home folder"),
    ("mediaCache.clean", "it deletes from the media cache"),
    ("fonts.list", "it scans the system's font folders"),
    ("help.systemReport", "it reports on the computer and writes logs"),
    ("sequence.renderBar", "render previews are kept beside the project"),
    ("sequence.renderSelection", "render previews are kept beside the project"),
    ("sequence.renderInToOut", "render previews are kept beside the project"),
    ("sequence.renderEffectsInToOut", "render previews are kept beside the project"),
    ("sequence.renderAudio", "render previews are kept beside the project"),
    ("sequence.deleteRenderFiles", "render previews are kept beside the project"),
    ("sequence.deleteRenderFilesInToOut", "render previews are kept beside the project"),
    ("presets.save", "it adds to the library in the app's data folder"),
    ("presets.delete", "it changes the library in the app's data folder"),
    ("presets.rename", "it changes the library in the app's data folder"),
    ("presets.import", "it adds to the library in the app's data folder"),
    ("export.presets.save", "it adds to the library in the app's data folder"),
    ("export.presets.delete", "it changes the library in the app's data folder"),
    ("export.presets.favorite", "it changes the library in the app's data folder"),
    ("export.presets.import", "it adds to the library in the app's data folder"),
    ("shortcuts.set", "it changes the shortcuts in the app's data folder"),
    ("shortcuts.clear", "it changes the shortcuts in the app's data folder"),
    ("shortcuts.undo", "it changes the shortcuts in the app's data folder"),
    ("shortcuts.redo", "it changes the shortcuts in the app's data folder"),
    ("shortcuts.loadPreset", "it changes the shortcuts in the app's data folder"),
    ("shortcuts.savePreset", "it adds to the library in the app's data folder"),
    ("shortcuts.deletePreset", "it deletes from the library in the app's data folder"),
    ("shortcuts.import", "it adds to the library in the app's data folder"),
    ("labels.importPreset", "it adds to the library in the app's data folder"),
    ("labels.deletePreset", "it deletes from the library in the app's data folder"),
    ("graphics.template.install", "it adds to the library in the app's data folder"),
    ("graphics.template.remove", "it deletes from the library in the app's data folder"),
    ("transcript.downloadModel", "it downloads into the app's data folder"),
    ("tts.downloadVoices", "it downloads into the app's data folder"),
];

/// Parameter names that hold a file or folder, whatever the command.
const PATH_KEYS: &[&str] = &[
    "path",
    "paths",
    "dir",
    "folder",
    "destination",
    "file",
    "files",
    "output",
    "outputs",
    "sidecar",
    "captured",
    "videoPreviews",
    "audioPreviews",
    "autoSave",
];

/// Parameter names that hold what a person wrote, where `/x` or `~` is just text.
const TEXT_KEYS: &[&str] =
    &["text", "name", "comment", "description", "title", "label", "author", "query", "value", "values", "logNote", "note", "notes", "category"];

/// `path` made absolute with its links followed; for a path that doesn't exist (yet), its nearest
/// existing folder's links followed and the rest appended.
fn resolve(path: &str) -> Result<PathBuf, String> {
    if path.is_empty() {
        return Err("automation path rejected: path is empty".into());
    }
    if !Path::new(path).is_absolute() {
        return Err(format!("automation path rejected: give an absolute path: {path}"));
    }
    // Checked as given: making `…\NUL` absolute gives the device path `\\.\NUL` on Windows.
    if cfg!(windows) && Path::new(path).components().any(|c| matches!(c, Component::Normal(n) if is_device_name(&n.to_string_lossy()))) {
        return Err(format!("automation path rejected: reserved device names are not allowed: {path}"));
    }
    let absolute = std::path::absolute(path).map_err(|e| format!("automation path rejected: cannot resolve {path}: {e}"))?;
    // The names below the nearest existing folder, last first.
    let mut missing: Vec<OsString> = vec![];
    let mut at = absolute.as_path();
    loop {
        match std::fs::canonicalize(at) {
            Ok(mut found) => {
                found.extend(missing.iter().rev().cloned());
                return Ok(found);
            }
            Err(e) => match std::fs::symlink_metadata(at) {
                // It is there, but is a link to nowhere: opening it would create the target.
                Ok(m) if m.file_type().is_symlink() => return Err(format!("automation path rejected: a link that leads nowhere: {path}")),
                // There, yet its real path can't be had (no permission): nothing to compare.
                Ok(_) => return Err(format!("automation path rejected: cannot resolve {path}: {e}")),
                Err(e2) if e2.kind() == std::io::ErrorKind::NotFound => {}
                Err(e2) => return Err(format!("automation path rejected: cannot resolve {path}: {e2}")),
            },
        }
        match (at.components().next_back(), at.parent()) {
            (Some(Component::Normal(name)), Some(parent)) => {
                missing.push(name.to_os_string());
                at = parent;
            }
            (Some(Component::ParentDir), _) => {
                return Err(format!("automation path rejected: `..` below a folder that doesn't exist: {path}"));
            }
            // Down to the root (or drive) and it doesn't exist either.
            _ => return Err(format!("automation path rejected: cannot resolve {path}: no such folder")),
        }
    }
}

/// Is `path` (resolved) the root or inside it, component by component (without regard to case on
/// Windows, whose file systems ignore it)?
fn inside(path: &Path, root: &Path) -> bool {
    let mut path = path.components();
    root.components().all(|r| path.next().is_some_and(|p| same_component(p, r)))
}

/// Are two path components the same (without regard to case on Windows)?
fn same_component(a: Component, b: Component) -> bool {
    a == b || (cfg!(windows) && matches!((a.as_os_str().to_str(), b.as_os_str().to_str()), (Some(a), Some(b)) if a.eq_ignore_ascii_case(b)))
}

/// A canonical path without Windows' `\\?\` (or `\\?\UNC\`) prefix, for people.
fn plain(p: PathBuf) -> PathBuf {
    let s = p.as_os_str().to_string_lossy();
    if let Some(rest) = s.strip_prefix(r"\\?\UNC\") {
        return PathBuf::from(format!(r"\\{rest}"));
    }
    match s.strip_prefix(r"\\?\") {
        Some(rest) => PathBuf::from(rest),
        None => p,
    }
}

/// A Windows device name (`CON`, `NUL`, `COM1`… also with an extension), which names no file.
fn is_device_name(component: &str) -> bool {
    let stem = component.split('.').next().unwrap_or(component).trim_end_matches(' ').to_ascii_uppercase();
    let numbered = |prefix: &str| stem.strip_prefix(prefix).is_some_and(|n| matches!(n, "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9"));
    matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL" | "CLOCK$" | "CONIN$" | "CONOUT$") || numbered("COM") || numbered("LPT")
}

/// Does `s` look like an absolute path: leading `/` or `\`, `~`, a Windows drive, or a `file:` URL?
fn looks_absolute(s: &str) -> bool {
    let drive = s.len() > 2 && s.as_bytes().get(1) == Some(&b':') && s.as_bytes().first().is_some_and(u8::is_ascii_alphabetic);
    s.starts_with('/') || s.starts_with('\\') || s == "~" || s.starts_with("~/") || s.starts_with("~\\") || drive || s.to_ascii_lowercase().starts_with("file:")
}

/// Strings in `params` (recursively, except under the `skip` keys) to check as paths: anything
/// under a [`PATH_KEYS`] key, and what looks absolute elsewhere, leaving [`TEXT_KEYS`] out. With
/// `every`, also any string with a separator or `..`, and text keys are not left out. Params are
/// input, so depth is bounded: nesting deeper than that is an error, not a pass.
fn other_paths(params: &Value, skip: &[&str], every: bool, out: &mut Vec<String>) -> Result<(), String> {
    /// Collect from `v`; `under_path_key` says a key above it is in [`PATH_KEYS`].
    fn walk(v: &Value, under_path_key: bool, every: bool, out: &mut Vec<String>, depth: usize) -> Result<(), String> {
        if depth > 64 {
            return Err("params are nested too deeply to check".into());
        }
        match v {
            Value::String(s) => {
                let t = s.trim();
                if under_path_key || looks_absolute(t) || (every && (t.contains(['/', '\\']) || t.contains(".."))) {
                    out.push(s.clone());
                }
            }
            Value::Array(a) => {
                for x in a {
                    walk(x, under_path_key, every, out, depth + 1)?;
                }
            }
            Value::Object(o) => {
                for (k, x) in o {
                    if every || !TEXT_KEYS.contains(&k.as_str()) {
                        walk(x, under_path_key || PATH_KEYS.contains(&k.as_str()), every, out, depth + 1)?;
                    }
                }
            }
            _ => {}
        }
        Ok(())
    }
    match params {
        Value::Object(o) => {
            for (k, v) in o.iter().filter(|(k, _)| !skip.contains(&k.as_str())) {
                if every || !TEXT_KEYS.contains(&k.as_str()) {
                    walk(v, PATH_KEYS.contains(&k.as_str()), every, out, 0)?;
                }
            }
            Ok(())
        }
        v => walk(v, false, every, out, 0),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// A fresh folder holding `in` (read root), `out` (write root) and `outside`.
    fn folders(name: &str) -> (PathBuf, AutomationRoots) {
        let base = std::env::temp_dir().join(format!("fc-roots-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        for d in ["in/sub", "out", "outside", "in2"] {
            std::fs::create_dir_all(base.join(d)).unwrap();
        }
        std::fs::write(base.join("in/a.cube"), "x").unwrap();
        std::fs::write(base.join("outside/secret.cube"), "x").unwrap();
        let roots = AutomationRoots::new(Some(&base.join("in").to_string_lossy()), Some(&base.join("out").to_string_lossy())).unwrap().unwrap();
        (base, roots)
    }

    fn s(p: PathBuf) -> String {
        p.to_string_lossy().into_owned()
    }

    #[test]
    fn paths_inside_the_roots_pass_and_others_dont() {
        let (base, roots) = folders("inside");
        assert_eq!(roots.check(&s(base.join("in/a.cube")), Access::Read), Ok(()));
        assert_eq!(roots.check(&s(base.join("in")), Access::Read), Ok(()), "the root itself");
        assert_eq!(roots.check(&s(base.join("in/sub/new.png")), Access::Read), Ok(()), "missing files are checked by their folder");
        assert_eq!(roots.check(&s(base.join("out/new/deeper/x.png")), Access::Write), Ok(()), "and by their nearest folder");
        let e = roots.check(&s(base.join("outside/secret.cube")), Access::Read).unwrap_err();
        assert!(e.starts_with("automation path rejected: outside the read root"), "{e}");
        // Read and write access are separate.
        assert!(roots.check(&s(base.join("in/a.cube")), Access::Write).unwrap_err().contains("outside the write root"));
        assert!(roots.check(&s(base.join("out/x.png")), Access::Read).unwrap_err().contains("outside the read root"));
        let _ = std::fs::remove_dir_all(base);
    }

    #[test]
    fn a_sibling_with_the_same_prefix_is_outside() {
        let (base, roots) = folders("prefix");
        assert!(roots.check(&s(base.join("in2/a.cube")), Access::Read).is_err(), "in2 is not inside in");
        assert!(roots.check(&format!("{}2/c.cube", s(base.join("in"))), Access::Read).is_err());
        let _ = std::fs::remove_dir_all(base);
    }

    /// `..` is followed where it exists and refused below a folder that doesn't.
    #[test]
    fn parent_traversal_is_resolved_or_refused() {
        let (base, roots) = folders("dotdot");
        let sep = std::path::MAIN_SEPARATOR;
        let up = format!("{}{sep}..{sep}outside{sep}secret.cube", s(base.join("in")));
        assert!(roots.check(&up, Access::Read).unwrap_err().contains("outside the read root"), "`..` out of the root");
        let back = format!("{}{sep}..{sep}a.cube", s(base.join("in/sub")));
        assert_eq!(roots.check(&back, Access::Read), Ok(()), "`..` that stays inside");
        let missing = format!("{}{sep}nope{sep}..{sep}..{sep}outside{sep}x.png", s(base.join("out")));
        let e = roots.check(&missing, Access::Write).unwrap_err();
        // Windows resolves `..` by name first (so does opening the file there): then it's outside.
        assert!(e.contains("`..` below a folder that doesn't exist") || e.contains("outside the write root"), "{e}");
        let _ = std::fs::remove_dir_all(base);
    }

    #[test]
    fn relative_paths_are_refused() {
        let (base, roots) = folders("relative");
        for p in ["a.cube", "../a.cube", "~/a.cube", "file:///etc/passwd"] {
            assert!(roots.check(p, Access::Read).unwrap_err().contains("give an absolute path"), "{p}");
        }
        assert_eq!(roots.check("", Access::Read).unwrap_err(), "automation path rejected: path is empty");
        let _ = std::fs::remove_dir_all(base);
    }

    /// A root must be an existing folder; no roots at all confines nothing, and one root grants only its own access.
    #[test]
    fn a_missing_root_is_an_error_and_none_is_no_confinement() {
        let (base, _) = folders("missing");
        let e = AutomationRoots::new(Some(&base.join("nope").to_string_lossy()), None).unwrap_err();
        assert!(e.starts_with("cannot open the automation read root"), "{e}");
        let e = AutomationRoots::new(None, Some(&base.join("in/a.cube").to_string_lossy())).unwrap_err();
        assert!(
            e.starts_with("cannot open the automation write root") && e.ends_with(&std::io::Error::from(std::io::ErrorKind::NotADirectory).to_string()),
            "{e}"
        );
        assert!(AutomationRoots::new(Some(""), None).unwrap_err().starts_with("cannot open the automation read root ``"));
        assert!(AutomationRoots::new(None, None).unwrap().is_none());
        // One root alone grants only its own access.
        let read_only = AutomationRoots::new(Some(&base.join("in").to_string_lossy()), None).unwrap().unwrap();
        let e = read_only.check(&s(base.join("in/x.cube")), Access::Write).unwrap_err();
        assert!(e.contains("no write root is in force"), "{e}");
        assert!(AutomationRoots::new(None, Some(&base.join("out").to_string_lossy())).unwrap().unwrap().describe().contains("read root none"));
        let _ = std::fs::remove_dir_all(base);
    }

    /// Links out of a root are outside it, dangling ones are refused, and links that stay inside pass.
    #[test]
    fn links_are_followed_before_comparing() {
        let (base, roots) = folders("links");
        #[cfg(unix)]
        let made = |target: &Path, link: &Path| std::os::unix::fs::symlink(target, link).is_ok();
        #[cfg(windows)]
        let made = |target: &Path, link: &Path| {
            let made = if target.is_dir() { std::os::windows::fs::symlink_dir(target, link) } else { std::os::windows::fs::symlink_file(target, link) };
            made.is_ok()
        };
        #[cfg(not(any(unix, windows)))]
        let made = |_: &Path, _: &Path| false;
        // Windows needs Developer Mode or elevation to make links: nothing to test without them.
        if !made(&base.join("outside/secret.cube"), &base.join("in/leak.cube")) {
            eprintln!("skipped: symbolic links can't be made here");
            let _ = std::fs::remove_dir_all(base);
            return;
        }
        assert!(roots.check(&s(base.join("in/leak.cube")), Access::Read).unwrap_err().contains("outside the read root"), "a file link out");
        assert!(made(&base.join("outside"), &base.join("out/away")));
        let e = roots.check(&s(base.join("out/away/new.png")), Access::Write).unwrap_err();
        assert!(e.contains("outside the write root"), "a folder link out: {e}");
        assert!(made(&base.join("outside/gone.cube"), &base.join("out/dangling.cube")));
        let e = roots.check(&s(base.join("out/dangling.cube")), Access::Write).unwrap_err();
        assert!(e.contains("a link that leads nowhere"), "writing would create the file outside: {e}");
        // A link to a folder inside the root stays inside.
        assert!(made(&base.join("in/sub"), &base.join("in/alias")));
        assert_eq!(roots.check(&s(base.join("in/alias/x.cube")), Access::Read), Ok(()));
        // A root given through a link is the folder it leads to.
        assert!(made(&base.join("in"), &base.join("in-link")));
        let linked = AutomationRoots::new(Some(&base.join("in-link").to_string_lossy()), None).unwrap().unwrap();
        assert_eq!(linked.check(&s(base.join("in/a.cube")), Access::Read), Ok(()));
        let _ = std::fs::remove_dir_all(base);
    }

    #[cfg(windows)]
    #[test]
    fn windows_ignores_case_and_refuses_devices() {
        let (base, roots) = folders("case");
        let upper = s(base.join("IN/A.CUBE"));
        assert_eq!(roots.check(&upper, Access::Read), Ok(()), "{upper}");
        assert_eq!(roots.check(&s(base.join("In/Sub/New.png")), Access::Read), Ok(()));
        let e = roots.check(&s(base.join("out/NUL")), Access::Write).unwrap_err();
        assert!(e.contains("reserved device names"), "{e}");
        assert!(roots.check(&s(base.join("out/com1.txt")), Access::Write).is_err());
        assert_eq!(roots.check(&s(base.join("out/console.txt")), Access::Write), Ok(()));
        let _ = std::fs::remove_dir_all(base);
    }

    /// Windows device names, with and without an extension, and names that only look like them.
    #[test]
    fn device_names() {
        for d in ["CON", "nul", "Com1", "LPT9.txt", "aux.svg", "NUL "] {
            assert!(is_device_name(d), "{d}");
        }
        for f in ["console", "com10", "COM0", "lpt", "a.cube", "nul-x"] {
            assert!(!is_device_name(f), "{f}");
        }
    }

    /// Named parameters are held to the command's own root, and paths in unnamed keys are caught.
    #[test]
    fn commands_check_their_file_parameters() {
        let (base, roots) = folders("commands");
        let r = roots.check_command("file.import", &json!({"paths": [s(base.join("in/a.cube"))]}));
        assert_eq!(r, Ok(()));
        let e = roots.check_command("file.import", &json!({"paths": [s(base.join("outside/secret.cube"))]})).unwrap_err();
        assert!(e.contains("outside the read root"), "{e}");
        let e = roots.check_command("lut.import", &json!({"path": s(base.join("outside/secret.cube"))})).unwrap_err();
        assert!(e.contains("outside the read root"), "{e}");
        // A write command is held to the write root, not the read root.
        let e = roots.check_command("file.saveCopy", &json!({"path": s(base.join("in/c.fcproj"))})).unwrap_err();
        assert!(e.contains("outside the write root"), "{e}");
        assert_eq!(roots.check_command("file.saveCopy", &json!({"path": s(base.join("out/c.fcproj"))})), Ok(()));
        // A path in an unnamed key is caught too.
        let e = roots
            .check_command("file.exportMedia", &json!({"path": s(base.join("out/x.mp4")), "settings": {"sidecar": s(base.join("in/y.srt"))}}))
            .unwrap_err();
        assert!(e.contains("outside the write root"), "{e}");
        // A command with no path parameters: nothing to check.
        assert_eq!(roots.check_command("timeline.razor", &json!({"seconds": 3.5})), Ok(()));
        // A non-string in a path parameter is refused in so many words.
        let e = roots.check_command("file.import", &json!({"paths": [7]})).unwrap_err();
        assert!(e.contains("`file.import`"), "{e}");
        let _ = std::fs::remove_dir_all(base);
    }

    #[test]
    fn preferences_are_held_to_the_write_root() {
        let (base, roots) = folders("prefs");
        let e = roots.check_command("prefs.set", &json!({"key": "mediaCache.location", "value": s(base.join("outside"))})).unwrap_err();
        assert!(e.contains("outside the write root"), "{e}");
        assert_eq!(roots.check_command("prefs.set", &json!({"key": "mediaCache.location", "value": s(base.join("out/cache"))})), Ok(()));
        // Scratch disks name folders the app writes into later.
        let e = roots.check_command("file.projectSettings.scratchDisks", &json!({"captured": s(base.join("outside"))})).unwrap_err();
        assert!(e.contains("outside the write root"), "{e}");
        let _ = std::fs::remove_dir_all(base);
    }

    /// Every command in the engine's registry is classified exactly once, so a new one fails here
    /// until someone decides what it may touch; and every path-like parameter its schema names is
    /// checked. Without this the tables go stale silently (#985: `audio.voiceover.stop`).
    #[test]
    fn every_command_in_the_registry_is_classified() {
        let specs = filmcraft_engine::command_specs();
        let known: Vec<&str> = specs.iter().map(|c| c.id).collect();
        let tabled = |table: &[(&str, &[&str])], id: &str| table.iter().any(|(c, _)| *c == id);
        let mut problems = vec![];
        for c in specs {
            let places = [tabled(READS, c.id) || tabled(WRITES, c.id), REFUSED.iter().any(|(r, _)| *r == c.id), NO_FILES.contains(&c.id)];
            if places.iter().filter(|p| **p).count() != 1 {
                problems.push(format!("{} must be in exactly one of READS/WRITES, REFUSED, NO_FILES: {places:?}", c.id));
            }
            if REFUSED.iter().any(|(r, _)| *r == c.id) {
                continue;
            }
            for key in schema_keys(c.params).into_iter().filter(|k| PATH_KEYS.contains(&k.as_str())) {
                let named = READS.iter().chain(WRITES).any(|(i, keys)| *i == c.id && keys.contains(&key.as_str()));
                if !named && !IGNORED_KEYS.contains(&(c.id, key.as_str())) {
                    problems.push(format!("{} takes `{key}`, which no table names: {}", c.id, c.params));
                }
            }
        }
        // Nothing in the tables that the registry has lost.
        let listed = READS
            .iter()
            .chain(WRITES)
            .map(|(i, _)| *i)
            .chain(REFUSED.iter().map(|(i, _)| *i))
            .chain(NO_FILES.iter().copied())
            .chain(LOCATION_OPTIONAL.iter().copied());
        for id in listed.chain(NAME_OR_PATH.iter().map(|(i, _)| *i)).chain(IGNORED_KEYS.iter().map(|(i, _)| *i)) {
            if !known.contains(&id) {
                problems.push(format!("{id} is listed but is not a command"));
            }
        }
        assert!(problems.is_empty(), "{}", problems.join("\n"));
    }

    /// The `"key":` names in a command's parameter doc.
    fn schema_keys(params: &str) -> Vec<String> {
        params.split('"').collect::<Vec<_>>().windows(2).filter(|w| w[1].trim_start().starts_with(':')).map(|w| w[0].to_string()).collect()
    }

    /// A command in no table is refused, whatever its params.
    #[test]
    fn unknown_commands_are_refused() {
        let (base, roots) = folders("unknown");
        for params in [json!({}), json!({"path": s(base.join("in/a.cube"))})] {
            let e = roots.check_command("no.suchCommand", &params).unwrap_err();
            assert!(e.contains("isn't known to stay inside the roots"), "{e}");
        }
        let _ = std::fs::remove_dir_all(base);
    }

    /// A refused command names its reason; `media.makeOffline` is refused only when it would delete.
    #[test]
    fn a_refused_command_says_why() {
        let (base, roots) = folders("refused");
        let e = roots.check_command("clip.updateMetadata", &json!({})).unwrap_err();
        assert!(e.contains("not available while file access is confined") && e.contains("sidecar"), "{e}");
        assert!(roots.check_command("media.makeOffline", &json!({"deleteFiles": true})).is_err());
        assert_eq!(roots.check_command("media.makeOffline", &json!({"deleteFiles": false})), Ok(()));
        let _ = std::fs::remove_dir_all(base);
    }

    /// The commands the review named: the take's folder is held to the write root, and leaving it
    /// out is refused rather than letting the command choose.
    #[test]
    fn implicit_locations_must_be_named() {
        let (base, roots) = folders("implicit");
        for (id, key) in [
            ("audio.voiceover.stop", "dir"),
            ("tts.create", "dir"),
            ("tts.edit", "dir"),
            ("clip.extractAudio", "dir"),
            ("graphics.template.export", "path"),
            ("file.save", "path"),
            ("file.saveAsTemplate", "path"),
            ("export.quick", "path"),
            ("file.exportFrame", "path"),
        ] {
            let e = roots.check_command(id, &json!({})).unwrap_err();
            assert!(e.contains(&format!("needs `{key}`")), "{id}: {e}");
            let e = roots.check_command(id, &json!({key: s(base.join("in"))})).unwrap_err();
            assert!(e.contains("outside the write root"), "{id}: {e}");
            let e = roots.check_command(id, &json!({key: "../elsewhere"})).unwrap_err();
            assert!(e.contains("give an absolute path"), "{id}: {e}");
            assert_eq!(roots.check_command(id, &json!({key: s(base.join("out"))})), Ok(()), "{id}");
        }
        assert_eq!(roots.check_command("audio.voiceover.stop", &json!({"discard": true})), Ok(()), "a discarded take writes nothing");
        // Listing the current folder, or walking out of the one given, is a read of a place not named.
        assert!(roots.check_command("mediaBrowser.list", &json!({})).unwrap_err().contains("needs `path`"));
        assert!(roots.check_command("mediaBrowser.navigate", &json!({"up": true})).unwrap_err().contains("needs `path`"));
        let _ = std::fs::remove_dir_all(base);
    }

    /// Path-like keys are checked with relative values too; text is left alone.
    #[test]
    fn the_sweep_checks_path_keys_and_leaves_text() {
        let (base, roots) = folders("sweep");
        let e = roots.check_command("playhead.set", &json!({"nested": {"output": "../../x"}})).unwrap_err();
        assert!(e.contains("give an absolute path"), "{e}");
        assert!(roots.check_command("playhead.set", &json!({"other": s(base.join("outside/x"))})).is_err());
        assert_eq!(roots.check_command("captions.setText", &json!({"text": "/ ~ not a path"})), Ok(()));
        assert_eq!(roots.check_command("effects.list", &json!({"folder": "Video Transitions/Wipe"})), Ok(()));
        assert_eq!(roots.check_command("graphics.template.apply", &json!({"template": "Lower Third"})), Ok(()), "a library name");
        assert!(roots.check_command("graphics.template.apply", &json!({"template": "../x.fcgt"})).is_err());
        let mut deep = json!("x");
        for _ in 0..80 {
            deep = json!([deep]);
        }
        assert!(roots.check_command("playhead.set", &json!({"a": deep})).unwrap_err().contains("nested too deeply"));
        let e = roots.check_command("prefs.set", &json!({"key": "mediaCache.location", "value": "../cache"})).unwrap_err();
        assert!(e.contains("give an absolute path"), "{e}");
        assert_eq!(roots.check_command("prefs.set", &json!({"key": "theme", "value": "dark"})), Ok(()));
        let _ = std::fs::remove_dir_all(base);
    }
}
