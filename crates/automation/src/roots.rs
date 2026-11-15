//! File-access confinement for automation (#985): `filmcraft-cli mcp --automation-read-root <dir>
//! --automation-write-root <dir>`, the flags VectorCraft and EffectCraft ship. While roots are in
//! force, every command an agent runs may only read files under a read root and only write files
//! under a write root; a root left out grants no access of its kind, and without roots nothing is
//! checked. On the bridge that is the command checks only: the app opens, by its own file access, the
//! footage a project names, and saves in its own folders. [`crate::FilmcraftMcp::run`] refuses a
//! bridge command while the app's Auto Save is on.
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
//!   `/work`) and exactly: both have the spelling the file system gives them by then, so another
//!   spelling of the root still matches, and a folder that differs only in case does not.
//! - A path must be absolute. A relative one is refused: it would be checked against this
//!   process's working directory and, in bridge mode, opened against the app's.
//! - On Windows, a path on a server, device or volume other than the root's is refused before any
//!   file call, and so is a link leading to one: looking at `\\host\share\x` would contact a host
//!   the agent chose and send it this user's credentials. A share is only used when a root stands
//!   on the same server and share. Reserved device names (`NUL`, `COM1:`…) are refused before any
//!   file call too.
//! - Every command is classified, and an unclassified one is refused (it fails closed): [`READS`]
//!   and [`WRITES`] name its file parameters, [`REFUSED`] says why it can't run confined, and
//!   [`no_files::NO_FILES`] lists the commands that touch no file an agent chooses. A test walks the
//!   engine's command registry, so a new command fails it until it is classified.
//! - A command that would pick the place itself when its file parameter is left out must be given
//!   one (`tts.create` without `dir` writes beside the project). `file.save` needs a `path` too.
//! - Any other absolute path anywhere in the params, and any string under a path-like key, is
//!   checked the same way, so a path can't slip through an unnamed key; `prefs.set` (whose values
//!   name folders the app writes into later) is held to the write root.
//! - On the bridge, a command that names files is refused while the app's project has ingest on:
//!   the app would copy what is imported to a folder of ingest's own. Importing there takes media
//!   files only ([`IMPORTS`]), apart from `file.importAaf`, which is held to both roots.
//! - The bridge's UI-driving tools are refused while confined: they could drive the live app's
//!   file dialogs past the roots.
//! - A command that writes is refused while the folder it writes in, or any folder below that or
//!   reached through a link from there, holds a link leading out of the write root. Exports add or change the extension, take the
//!   sequence's name in a folder, number their stills and put caption files beside the output, and
//!   an AAF or OMF export writes into a folder of its own beside the file. They open those names
//!   themselves after this check, so the names aren't known here but the folder is. A folder too
//!   large to look through ([`WRITE_SCAN_LIMIT`]) refuses the write.
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

use std::collections::HashSet;
use std::ffi::OsString;
use std::path::{Component, Path, PathBuf, Prefix};

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
        if inside(&resolve(path, &root.resolved)?, &root.resolved) {
            return Ok(());
        }
        Err(format!("automation path rejected: outside the {kind} root {}: {path}", root.shown.display()))
    }

    /// May a command write at `path`, or at another name it derives there? `path` must pass
    /// [`Self::check`], and no link in its folder (or in it, if it's a folder), or in any folder
    /// below that, may lead out of the write root: see the module docs.
    fn check_write_target(&self, path: &str) -> Result<(), String> {
        self.scan_write_target(path, WRITE_SCAN_LIMIT)
    }

    /// [`Self::check_write_target`], giving up with a refusal after `limit` entries.
    fn scan_write_target(&self, path: &str, limit: usize) -> Result<(), String> {
        self.check(path, Access::Write)?;
        let given = Path::new(path);
        let folder = if path.ends_with(['/', '\\']) || given.is_dir() { Some(given) } else { given.parent() };
        // `path` may be a link that leads into the root while the file is made beside the link, so the folder it
        // stands in has to be inside the root too.
        if let Some(text) = folder.and_then(Path::to_str) {
            self.check(text, Access::Write)?;
        }
        let cannot_list = |e: std::io::Error| format!("automation path rejected: cannot list the folders of {path}: {e}");
        let mut folders: Vec<PathBuf> = folder.map(Path::to_path_buf).into_iter().collect();
        let mut seen = HashSet::new();
        let mut entries = 0usize;
        while let Some(folder) = folders.pop() {
            let real = match std::fs::canonicalize(&folder) {
                Ok(real) => real,
                // Nothing there yet, so nothing there leads anywhere.
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
                Err(e) => return Err(cannot_list(e)),
            };
            // Links can lead back to a folder already looked at.
            if !seen.insert(real.clone()) {
                continue;
            }
            for entry in std::fs::read_dir(&real).map_err(cannot_list)? {
                let entry = entry.map_err(cannot_list)?;
                entries += 1;
                if entries > limit {
                    return Err(format!(
                        "automation path rejected: over {limit} files and folders to check for links around {path}: write to a smaller folder"
                    ));
                }
                let kind = entry.file_type().map_err(cannot_list)?;
                if kind.is_symlink() {
                    // Written lossily it would name some other file, which doesn't exist and so passes.
                    let link = entry.path();
                    let Some(link_text) = link.to_str() else {
                        return Err(format!("automation path rejected: a link whose path isn't text, so it can't be checked: {}", link.display()));
                    };
                    self.check(link_text, Access::Write)
                        .map_err(|e| format!("{e} (a command writing to {path} may pick that name, so the link has to go first)"))?;
                    // Inside the root, but what it leads to can hold a link out.
                    if entry.path().is_dir() {
                        folders.push(entry.path());
                    }
                } else if kind.is_dir() {
                    folders.push(entry.path());
                }
            }
        }
        Ok(())
    }

    /// What the bridge refuses on top of [`Self::check_command`]: commands that act on paths the
    /// app already holds, which no parameter names and the app's own file access doesn't check.
    /// That's the export queue, whose items can write anywhere (most of these commands would start
    /// one, `export.queue.list` too while the queue is running, show its path or stop it), and
    /// `file.revert`, which reads the open project again from wherever it's saved. Export with
    /// `file.exportMedia` or `export.quick` instead.
    pub fn check_bridge_command(id: &str, params: &Value) -> Result<(), String> {
        if id.starts_with("export.queue.") {
            return Err(format!("`{id}` is not available while file access is confined: the app's export queue can hold exports to anywhere"));
        }
        if id == "file.revert" {
            return Err(format!("`{id}` is not available while file access is confined: it reads the app's project from wherever it's saved"));
        }
        if IMPORTS.contains(&id) {
            for path in import_paths(params) {
                if !filmcraft_media::is_importable(path) {
                    return Err(format!(
                        "`{id}` takes only media files on the bridge while file access is confined, and not {}: the app adds a preset file to its library and writes a document's audio beside it",
                        path.display()
                    ));
                }
            }
        }
        Ok(())
    }

    /// The rest of the bridge's import rule, once [`Self::check_command`] has passed the paths: each must be a
    /// file. A folder named like media (`batch.mp4`) has the right extension, and the app would take all it holds.
    /// This looks at the files, so it comes after the roots have been asked.
    pub fn check_bridge_media_files(&self, id: &str, params: &Value) -> Result<(), String> {
        if !IMPORTS.contains(&id) {
            return Ok(());
        }
        for path in import_paths(params) {
            if !std::fs::metadata(path).is_ok_and(|m| m.is_file()) {
                return Err(format!("`{id}` takes only media files on the bridge while file access is confined, and {} is not a file", path.display()));
            }
            self.check_sequence_frames(id, path)?;
        }
        Ok(())
    }

    /// The app imports a numbered still together with the stills numbered like it beside it, whether the command,
    /// a preference or the browser asks for a sequence, and reads them through its own unchecked file access. A
    /// link among them that leads out of the read root would be read. Every neighbour that is a link has to
    /// pass the roots, as it does when the session is headless and each frame is opened through them, and so does
    /// the folder they are listed from.
    fn check_sequence_frames(&self, id: &str, seed: &Path) -> Result<(), String> {
        let Some(numbered) = seed.to_str().and_then(filmcraft_media::sequence::Numbered::parse) else { return Ok(()) };
        let folder = seed.parent().unwrap_or(Path::new("."));
        // The app takes the frames from the folder the seed is written in, which is not where a seed that is a
        // link stands when it leads into the root: every frame there would be outside it.
        if let Some(text) = folder.to_str() {
            self.check(text, Access::Read).map_err(|e| format!("{e} (`{id}` also takes the frames of an image sequence numbered like {})", seed.display()))?;
        }
        let cannot_list = |e: std::io::Error| format!("automation path rejected: cannot list the frames beside {}: {e}", seed.display());
        for (seen, entry) in std::fs::read_dir(folder).map_err(cannot_list)?.enumerate() {
            if seen >= WRITE_SCAN_LIMIT {
                return Err(format!("automation path rejected: over {WRITE_SCAN_LIMIT} files beside {} to check as frames", seed.display()));
            }
            let entry = entry.map_err(cannot_list)?;
            let is_frame = entry.file_name().to_str().is_some_and(|name| numbered.number_of(name).is_some());
            if is_frame && entry.file_type().map_err(cannot_list)?.is_symlink() {
                let Some(frame) = entry.path().to_str().map(str::to_string) else { continue };
                self.check(&frame, Access::Read)
                    .map_err(|e| format!("{e} (`{id}` also takes the frames of an image sequence numbered like {})", seed.display()))?;
            }
        }
        Ok(())
    }

    /// Does command `id` name files? On the bridge those are refused while the app's project has
    /// ingest on, since an import then copies files to a folder of ingest's own, and the commands
    /// that import are several and grow.
    pub fn names_files(id: &str) -> bool {
        READS.iter().chain(WRITES).any(|(c, _)| *c == id)
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
        if let Some((_, flag)) =
            GOES_ELSEWHERE.iter().find(|(c, flag)| *c == id && params.get(*flag).is_some_and(|v| !matches!(v, Value::Null | Value::Bool(false))))
        {
            return Err(format!("`{id}` with `{flag}` is not available while file access is confined: it goes to a folder that no path names"));
        }
        let named = |table: &[(&str, &'static [&'static str])]| table.iter().find(|(c, _)| *c == id).map(|(_, keys)| *keys);
        let (writes, reads) = (named(WRITES), named(READS));
        if writes.is_none() && reads.is_none() && !NO_FILES.contains(&id) {
            return Err(format!("`{id}` is not available while file access is confined: it isn't known to stay inside the roots"));
        }
        let check = |path: &str, access| if access == Access::Write { self.check_write_target(path) } else { self.check(path, access) };
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
                    // The engine reads `paths` as a list only, so a text there would name nothing it opens.
                    Some(Value::String(_)) if *key == "paths" => return Err(format!("`{id}`: `{key}` must be a list of file paths")),
                    Some(Value::String(s)) if NAME_OR_PATH.contains(&(id, *key)) && !s.contains(['/', '\\', '.']) => {}
                    Some(Value::String(s)) => check(s, access)?,
                    // A list names files only under `paths`; elsewhere the command reads the value as text,
                    // finds none and picks the place itself, so what the list names would go unused.
                    Some(Value::Array(a)) if *key == "paths" => {
                        if a.is_empty() {
                            return Err(format!("`{id}`: `{key}` must name at least one file while file access is confined"));
                        }
                        for v in a {
                            match v.as_str() {
                                Some(s) => check(s, access)?,
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
            check(p, Access::Write).map_err(|e| format!("{e} (`{id}` writes the embedded audio beside the file)"))?;
        }
        if let (Some(suffix), Some(path)) = (derived_folder_suffix(id), params.get("path").and_then(Value::as_str))
            && let Some(folder) = folder_beside(path, suffix)
        {
            check(&folder, Access::Write).map_err(|e| format!("{e} (`{id}` also writes a folder named after `path` with `{suffix}` added, beside it)"))?;
        }
        // The rest of the params: what a command writes is held to the write root throughout.
        let access = if writes.is_some() { Access::Write } else { Access::Read };
        let skip: Vec<&str> =
            reads.unwrap_or(&[]).iter().chain(writes.unwrap_or(&[])).copied().chain(IGNORED_KEYS.iter().filter(|(c, _)| *c == id).map(|(_, k)| *k)).collect();
        if id == "prefs.set" {
            absolute_folder_prefs(params)?;
        }
        // A reset puts the defaults back, and Auto Save's is on.
        if id == "prefs.reset" && params.get("category").and_then(Value::as_str).is_none_or(|c| c == "autoSave") {
            return Err("`prefs.reset` with no category, or with `autoSave`, would turn Auto Save back on: it stays off while file access is confined".into());
        }
        let mut other = vec![];
        other_paths(params, &skip, id == "prefs.set", &mut other).map_err(|e| format!("`{id}`: {e}"))?;
        for p in other {
            check(&p, access)?;
        }
        Ok(())
    }
}

/// What an export adds to the name of its `path` for the folder it also writes beside it: the audio
/// and the mixdown of an AAF, the audio files of an OMF. With the write root itself as `path` that
/// folder is a sibling of the root, outside it.
fn derived_folder_suffix(id: &str) -> Option<&'static str> {
    match id {
        "file.exportAaf" => Some(" Media"),
        "file.exportOmf" => Some(" Audio Files"),
        _ => None,
    }
}

/// `<parent of path>/<stem of path><suffix>`, named as the engine names it. `None` when `path` has
/// no parent or stem to take, which `check` of `path` itself already refuses.
fn folder_beside(path: &str, suffix: &str) -> Option<String> {
    let path = Path::new(path);
    let stem = path.file_stem()?.to_str()?;
    path.parent()?.join(format!("{stem}{suffix}")).to_str().map(str::to_string)
}

/// Flags that take a command to a folder no parameter names: the parent of the current one, or one from the
/// history. The folder is read by the app, which is not confined, so the check can't see it.
const GOES_ELSEWHERE: &[(&str, &str)] = &[("mediaBrowser.navigate", "up"), ("mediaBrowser.navigate", "back"), ("mediaBrowser.navigate", "forward")];

/// The files an import names: under `path`, or each one under `paths`.
fn import_paths(params: &Value) -> Vec<&Path> {
    ["path", "paths"]
        .iter()
        .filter_map(|key| params.get(*key))
        .flat_map(|v| v.as_array().map_or(vec![v], |a| a.iter().collect()))
        .filter_map(Value::as_str)
        .map(Path::new)
        .collect()
}

/// Preferences that make the app write somewhere on its own later, which the roots can't see: only `false` is allowed.
const REFUSED_PREFS: &[&str] = &["autoSave.saveCurrentProject", "autoSave.enabled"];

/// The preference values a `prefs.set` gives: `key` with `value`, and each of `values`.
fn prefs_given(params: &Value) -> Vec<(&str, &Value)> {
    let mut given: Vec<(&str, &Value)> = vec![];
    if let (Some(key), Some(value)) = (params.get("key").and_then(Value::as_str), params.get("value")) {
        given.push((key, value));
    }
    if let Some(values) = params.get("values").and_then(Value::as_object) {
        given.extend(values.iter().map(|(k, v)| (k.as_str(), v)));
    }
    given
}

/// Preferences that hold a folder the app writes into later. A test checks this against the settings
/// schema, so a new one can't be left out.
const FOLDER_PREFS: &[&str] = &["mediaCache.location", "mediaCache.databaseLocation"];

/// A [`FOLDER_PREFS`] value in `prefs.set` must be empty (the default folder) or absolute. A bare name
/// such as `cache` has no separator for [`other_paths`] to see, and the app reads it against the
/// server's working folder, which no root covers.
fn absolute_folder_prefs(params: &Value) -> Result<(), String> {
    for (key, value) in prefs_given(params) {
        if REFUSED_PREFS.contains(&key) && value != &Value::Bool(false) {
            return Err(format!("`prefs.set`: `{key}` stays off while file access is confined: the app saves its open project in place"));
        }
    }
    for (key, value) in prefs_given(params).into_iter().filter(|(k, _)| FOLDER_PREFS.contains(k)) {
        match value {
            Value::Null => {}
            Value::String(text) if text.trim().is_empty() || Path::new(text.trim()).is_absolute() => {}
            _ => return Err(format!("`prefs.set`: `{key}` must be an absolute folder, or empty, while file access is confined")),
        }
    }
    Ok(())
}

/// How many links [`refuse_other_hosts`] follows on the way to one path, as a file system would.
const MAX_LINKS: usize = 40;

/// How many files and folders one write looks at for links: unchecked ones could hold a link out,
/// so past this the write is refused.
const WRITE_SCAN_LIMIT: usize = 1_000_000;

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
    ("lumetri.exportCube", &["path"]),
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

/// Commands that import what they name through `file.import`, which opens a media file and goes by
/// the content of anything else: it can also add a preset file to the app's library or write a
/// document's audio beside it. A folder imports all it holds. The session's own checks cover that
/// in a headless session, not on the bridge.
const IMPORTS: &[&str] = &[
    "file.import",
    "file.importFromMediaBrowser",
    "file.importImageSequence",
    "mediaBrowser.import",
    "mediaBrowser.openInSource",
    "edit.pasteMedia",
    "graphics.newFromFile",
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
    ("presets.scan", "it reads the library in the app's data folder and rewrites its index"),
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
/// existing folder's links followed and the rest appended. A server, device or volume that `root`
/// doesn't stand on is refused first, before any file call ([`refuse_other_host`]).
fn resolve(path: &str, root: &Path) -> Result<PathBuf, String> {
    if path.is_empty() {
        return Err("automation path rejected: path is empty".into());
    }
    // A server or device is refused by name before anything else, even when no file is named.
    refuse_other_host(Path::new(path), path, root)?;
    if !Path::new(path).is_absolute() {
        return Err(format!("automation path rejected: give an absolute path: {path}"));
    }
    // Checked as given: making `…\NUL` absolute gives the device path `\\.\NUL` on Windows.
    if cfg!(windows) && Path::new(path).components().any(|c| matches!(c, Component::Normal(n) if n.to_str().is_some_and(is_device_name))) {
        return Err(format!("automation path rejected: reserved device names are not allowed: {path}"));
    }
    let absolute = std::path::absolute(path).map_err(|e| format!("automation path rejected: cannot resolve {path}: {e}"))?;
    if cfg!(windows) {
        refuse_other_hosts(&absolute, path, root)?;
    }
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

/// What a Windows path's prefix reaches besides a drive, in one spelling however it is written: the
/// server and share of `\\server\share` or `\\?\UNC\server\share`, a `\\.\` device, or a `\\?\`
/// name such as a volume. `None` for a drive, and for every path on other systems, which have no
/// prefix. Windows ignores the case of servers and shares, so these are compared without it; this
/// only decides who may be asked, and [`inside`] still compares the real path exactly.
fn remote_prefix(path: &Path) -> Option<(&'static str, OsString, OsString)> {
    let Some(Component::Prefix(prefix)) = path.components().next() else { return None };
    let (kind, first, second) = match prefix.kind() {
        Prefix::Disk(_) | Prefix::VerbatimDisk(_) => return None,
        Prefix::UNC(server, share) | Prefix::VerbatimUNC(server, share) => ("share", server, share),
        Prefix::DeviceNS(name) => ("device", name, std::ffi::OsStr::new("")),
        Prefix::Verbatim(name) => ("volume", name, std::ffi::OsStr::new("")),
    };
    Some((kind, first.to_ascii_lowercase(), second.to_ascii_lowercase()))
}

/// `Err`, naming the prefix, when `path` starts on a server, device or volume that `root` doesn't
/// stand on. It calls no file function: even a look at such a path contacts that host and offers it
/// this user's credentials. `given` is what the agent asked for, for the message.
fn refuse_other_host(path: &Path, given: &str, root: &Path) -> Result<(), String> {
    let Some(prefix) = remote_prefix(path) else { return Ok(()) };
    if remote_prefix(root).as_ref() == Some(&prefix) {
        return Ok(());
    }
    let shown = path.components().next().map_or_else(|| given.to_string(), |c| Path::new(c.as_os_str()).display().to_string());
    Err(format!("automation path rejected: {shown} is a network or device path that no root stands on, so it isn't opened: {given}"))
}

/// [`refuse_other_host`] for `path` and for every link on the way to it, taken from the top one at
/// a time, since following a link to a share is what contacts it. Only folders already passed are
/// looked at, so nothing is asked of a host before it is refused. What can't be looked at is left
/// to [`resolve`], which refuses what it can't resolve.
fn refuse_other_hosts(path: &Path, given: &str, root: &Path) -> Result<(), String> {
    let mut at = path.to_path_buf();
    for _ in 0..=MAX_LINKS {
        refuse_other_host(&at, given, root)?;
        match first_link_replaced(&at, given)? {
            Some(next) => at = next,
            None => return Ok(()),
        }
    }
    Err(format!("automation path rejected: over {MAX_LINKS} links on the way to {given}"))
}

/// `at` with the first link on the way to it (from the top) replaced by what it points to, or `None`
/// when no link is there to read. A target that can't be checked from its text is refused: one in the
/// NT namespace, one that isn't text, and one that goes down a name and back up (`a\..\b`), since
/// `a` may itself be a link that the text hides and the file system follows.
fn first_link_replaced(at: &Path, given: &str) -> Result<Option<PathBuf>, String> {
    let names: Vec<Component> = at.components().collect();
    let mut so_far = PathBuf::new();
    for (i, name) in names.iter().enumerate() {
        so_far.push(name);
        if !matches!(name, Component::Normal(_)) {
            continue;
        }
        let Ok(meta) = std::fs::symlink_metadata(&so_far) else { return Ok(None) };
        if !meta.file_type().is_symlink() {
            continue;
        }
        let target = std::fs::read_link(&so_far).map_err(|e| format!("automation path rejected: cannot read the link {}: {e}", so_far.display()))?;
        let refuse = |why: &str| Err(format!("automation path rejected: the link {} {why}: {given}", so_far.display()));
        if target.to_str().is_none_or(|t| t.starts_with(r"\??\")) {
            return refuse("leads into the NT namespace or has a target that isn't text");
        }
        let mut down = false;
        for part in target.components() {
            match part {
                Component::Normal(_) => down = true,
                Component::ParentDir if down => return refuse("goes down a name and back up, so what it reaches can't be read from its text"),
                _ => {}
            }
        }
        // A relative target stands in the link's folder, and an absolute one replaces it all.
        let mut next = so_far.parent().map_or_else(PathBuf::new, Path::to_path_buf).join(target);
        next.extend(&names[i + 1..]);
        return std::path::absolute(&next).map(Some).map_err(|e| format!("automation path rejected: cannot resolve {given}: {e}"));
    }
    Ok(None)
}

/// Is `path` (resolved) the root or inside it, component by component?
fn inside(path: &Path, root: &Path) -> bool {
    let mut path = path.components();
    root.components().all(|r| path.next() == Some(r))
}

/// A canonical path without Windows' `\\?\` (or `\\?\UNC\`) prefix, for people.
fn plain(p: PathBuf) -> PathBuf {
    let Some(s) = p.to_str() else { return p };
    if let Some(rest) = s.strip_prefix(r"\\?\UNC\") {
        return PathBuf::from(format!(r"\\{rest}"));
    }
    match s.strip_prefix(r"\\?\") {
        Some(rest) => PathBuf::from(rest),
        None => p,
    }
}

/// A Windows device name (`CON`, `NUL`, `COM1`, `COM¹`… also with an extension or a stream), which
/// names no file.
fn is_device_name(component: &str) -> bool {
    let stem = component.split(['.', ':']).next().unwrap_or(component).trim_end_matches(' ').to_ascii_uppercase();
    let numbered = |prefix: &str| stem.strip_prefix(prefix).is_some_and(|n| matches!(n, "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9" | "¹" | "²" | "³"));
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
    use crate::scratch::Scratch;
    use serde_json::json;

    /// A fresh folder holding `in` (read root), `out` (write root) and `outside`.
    fn folders(name: &str) -> (Scratch, AutomationRoots) {
        let base = Scratch::new(&format!("roots-{name}"));
        for d in ["in/sub", "out", "outside", "in2"] {
            std::fs::create_dir_all(base.join(d)).unwrap();
        }
        std::fs::write(base.join("in/a.cube"), "x").unwrap();
        std::fs::write(base.join("outside/secret.cube"), "x").unwrap();
        let roots = AutomationRoots::new(Some(&base.join("in").to_string_lossy()), Some(&base.join("out").to_string_lossy())).unwrap().unwrap();
        (base, roots)
    }

    /// `p` as the text a command would be given.
    fn s(p: PathBuf) -> String {
        p.to_string_lossy().into_owned()
    }

    /// Each root allows its own kind of access inside it and nothing outside.
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
    }

    /// Roots compare whole components, so `/work2` is not inside `/work`.
    #[test]
    fn a_sibling_with_the_same_prefix_is_outside() {
        let (base, roots) = folders("prefix");
        assert!(roots.check(&s(base.join("in2/a.cube")), Access::Read).is_err(), "in2 is not inside in");
        assert!(roots.check(&format!("{}2/c.cube", s(base.join("in"))), Access::Read).is_err());
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
    }

    /// A relative path is refused rather than checked against this process's working folder.
    #[test]
    fn relative_paths_are_refused() {
        let (_base, roots) = folders("relative");
        for p in ["a.cube", "../a.cube", "~/a.cube", "file:///etc/passwd"] {
            assert!(roots.check(p, Access::Read).unwrap_err().contains("give an absolute path"), "{p}");
        }
        assert_eq!(roots.check("", Access::Read).unwrap_err(), "automation path rejected: path is empty");
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
    }

    /// A link at `link` leading to `target`, if this system lets us make one.
    fn made(target: &Path, link: &Path) -> bool {
        #[cfg(unix)]
        return std::os::unix::fs::symlink(target, link).is_ok();
        #[cfg(windows)]
        return if target.is_dir() { std::os::windows::fs::symlink_dir(target, link) } else { std::os::windows::fs::symlink_file(target, link) }.is_ok();
        #[cfg(not(any(unix, windows)))]
        return false;
    }

    /// Exports open names they derive after the check, so a link beside the path counts.
    #[test]
    fn a_link_out_beside_a_write_refuses_it() {
        let (base, roots) = folders("derived");
        std::fs::write(base.join("outside/old.mp4"), b"old").unwrap();
        if !made(&base.join("outside/old.mp4"), &base.join("out/result.mp4")) {
            eprintln!("skipped: symbolic links can't be made here");
            return;
        }
        let out = base.join("out");
        std::fs::create_dir(out.join("clean")).unwrap();
        for (id, path) in [
            ("file.exportMedia", s(out.join("result"))),
            ("export.quick", s(out.join("result"))),
            ("export.queue.add", s(out.join("result"))),
            ("markers.exportCsv", s(out.join("other.csv"))),
            ("file.exportMedia", format!("{}/", s(out.clone()))),
            ("file.exportMedia", s(out.clone())),
        ] {
            let e = roots.check_command(id, &json!({"path": path})).unwrap_err();
            assert!(e.contains("may pick that name"), "{id} {path}: {e}");
        }
        // A link in a folder below counts: an AAF writes into `<name> Media` beside the file.
        std::fs::create_dir_all(out.join("aaf/x Media")).unwrap();
        assert!(made(&base.join("outside/old.mp4"), &out.join("aaf/x Media/x Video Mixdown.mov")));
        let e = roots.check_command("file.exportAaf", &json!({"path": s(out.join("aaf/x.aaf"))})).unwrap_err();
        assert!(e.contains("may pick that name"), "{e}");
        // So does one in a folder that a link inside the root leads to.
        std::fs::create_dir_all(out.join("chain")).unwrap();
        std::fs::create_dir_all(out.join("other")).unwrap();
        assert!(made(&out.join("other"), &out.join("chain/y Media")));
        assert!(made(&base.join("outside/old.mp4"), &out.join("other/y Video Mixdown.mov")));
        let e = roots.check_command("file.exportAaf", &json!({"path": s(out.join("chain/y.aaf"))})).unwrap_err();
        assert!(e.contains("may pick that name"), "{e}");
        // Links that go round in a circle end the walk.
        std::fs::create_dir_all(out.join("loop/a")).unwrap();
        assert!(made(&out.join("loop"), &out.join("loop/a/up")));
        assert_eq!(roots.check_command("file.exportAaf", &json!({"path": s(out.join("loop/x.aaf"))})), Ok(()));
        // One level down, the folder is clean, and its own links stay inside the root.
        let params = json!({"path": s(out.join("clean/result"))});
        assert_eq!(roots.check_command("file.exportMedia", &params), Ok(()));
        std::fs::write(out.join("clean/a.mp4"), b"mine").unwrap();
        assert!(made(&out.join("clean/a.mp4"), &out.join("clean/b.mp4")));
        assert_eq!(roots.check_command("file.exportMedia", &params), Ok(()));
        // A link that leads nowhere could create the file outside.
        assert!(made(&base.join("outside/gone.mp4"), &out.join("clean/gone.mp4")));
        assert!(roots.check_command("file.exportMedia", &params).unwrap_err().contains("a link that leads nowhere"));
        // A folder that doesn't exist yet holds nothing.
        assert_eq!(roots.check_command("file.exportMedia", &json!({"path": s(out.join("new/deeper/result"))})), Ok(()));
    }

    /// A write looks at so many files for links and no more, and what it hasn't seen can't pass.
    #[test]
    fn a_write_in_a_folder_too_large_to_check_is_refused() {
        let (base, roots) = folders("too-large");
        let out = base.join("out");
        for name in ["a", "b", "c"] {
            std::fs::write(out.join(name), b"x").unwrap();
        }
        let target = s(out.join("result.mp4"));
        assert_eq!(roots.scan_write_target(&target, 3), Ok(()));
        let e = roots.scan_write_target(&target, 2).unwrap_err();
        assert!(e.contains("write to a smaller folder"), "{e}");
    }

    /// A link whose name can't be written as text can't be checked, so it refuses the write.
    #[cfg(unix)]
    #[test]
    fn a_link_with_a_name_that_is_not_text_refuses_a_write() {
        use std::os::unix::ffi::OsStrExt;
        let (base, roots) = folders("not-text");
        let out = base.join("out");
        let link = out.join(std::ffi::OsStr::from_bytes(b"bad\xff.mp4"));
        // macOS file systems refuse such names: nothing to test there.
        if !made(&base.join("outside/old.mp4"), &link) {
            eprintln!("skipped: this file system refuses such names");
            return;
        }
        let e = roots.check_command("file.exportMedia", &json!({"path": s(out.join("result"))})).unwrap_err();
        assert!(e.contains("isn't text"), "{e}");
    }

    /// Links out of a root are outside it, dangling ones are refused, and links that stay inside pass.
    #[test]
    fn links_are_followed_before_comparing() {
        let (base, roots) = folders("links");
        // Windows needs Developer Mode or elevation to make links: nothing to test without them.
        if !made(&base.join("outside/secret.cube"), &base.join("in/leak.cube")) {
            eprintln!("skipped: symbolic links can't be made here");
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
    }

    /// Another case of a name that exists matches it, and device names are refused.
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
    }

    /// A server, device or volume that no root stands on is refused by name, however it is spelled,
    /// and never reaches `canonicalize`, whose error would say `cannot resolve`. Needs no network:
    /// the addresses are reserved for documentation, and a call that reached one would hang first.
    #[cfg(windows)]
    #[test]
    fn network_paths_are_refused_before_any_file_call() {
        let (_base, roots) = folders("network");
        for (path, prefix) in [
            (r"\\203.0.113.5\s\x.mp4", r"\\203.0.113.5\s"),
            ("//203.0.113.5/s/x.mp4", "//203.0.113.5/s"),
            (r"\\?\UNC\203.0.113.5\s\x.mp4", r"\\?\UNC\203.0.113.5\s"),
            (r"\\host.invalid@SSL\s\x.mp4", r"\\host.invalid@SSL\s"),
            (r"\\host.invalid@SSL@8443\DavWWWRoot\x.mp4", r"\\host.invalid@SSL@8443\DavWWWRoot"),
            (r"\\.\COM1\x", r"\\.\COM1"),
            (r"\\203.0.113.5\s", r"\\203.0.113.5\s"),
            (r"\\.\UNC\203.0.113.5\s\x.mp4", r"\\.\UNC"),
            (r"\\?\GLOBALROOT\Device\Mup\203.0.113.5\s\x.mp4", r"\\?\GLOBALROOT"),
            (r"\\?\Volume{00000000-0000-0000-0000-000000000000}\x.mp4", r"\\?\Volume{00000000-0000-0000-0000-000000000000}"),
        ] {
            for access in [Access::Read, Access::Write] {
                let e = roots.check(path, access).unwrap_err();
                assert!(e.contains(prefix), "{path}: the refusal names the prefix: {e}");
                assert!(e.contains("network or device path") && !e.contains("cannot resolve"), "{path}: {e}");
            }
        }
    }

    /// A share is used only when the root stands on the same server and share, in either spelling,
    /// and a drive is never held to this: the rest of the check decides those.
    #[cfg(windows)]
    #[test]
    fn a_root_on_a_share_allows_that_share_alone() {
        let root = Path::new(r"\\?\UNC\Server\Share\root");
        for same in [r"\\server\share\root\x.mp4", r"\\?\UNC\SERVER\SHARE\x.mp4", "//server/share/x.mp4"] {
            assert_eq!(refuse_other_host(Path::new(same), same, root), Ok(()), "{same}");
        }
        for other in [r"\\server\other\x.mp4", r"\\other\share\x.mp4", r"\\server@SSL\share\x.mp4", r"\\.\server\share"] {
            assert!(refuse_other_host(Path::new(other), other, root).is_err(), "{other}");
        }
        for drive in [r"C:\x.mp4", r"\\?\C:\x.mp4"] {
            assert_eq!(refuse_other_host(Path::new(drive), drive, Path::new(r"D:\root")), Ok(()), "{drive}");
        }
    }

    /// A link to a share is read, not followed, wherever it stands on the way to a path, also
    /// through another link, and when a write looks around for links. Skipped where links can't be
    /// made.
    #[cfg(windows)]
    #[test]
    fn a_link_to_a_share_is_refused_without_following_it() {
        let (base, roots) = folders("link-share");
        let share = Path::new(r"\\203.0.113.5\s");
        let link = |name: &str, target: &Path| std::os::windows::fs::symlink_dir(target, base.join(name)).is_ok();
        if !link("in/share", share) || !link("in/hop", Path::new("share")) || !link("out/share", share) {
            // On a CI runner the links must be possible, or this proves nothing and passes unseen.
            assert!(std::env::var_os("CI").is_none(), "links can't be made on a CI runner");
            eprintln!("skipped: links can't be made here");
            return;
        }
        for path in ["in/share", "in/share/x.mp4", "in/hop/x.mp4"] {
            let e = roots.check(&s(base.join(path)), Access::Read).unwrap_err();
            assert!(e.contains(r"203.0.113.5\s") && !e.contains("cannot resolve"), "{path}: {e}");
        }
        let e = roots.check_write_target(&s(base.join("out/x.mp4"))).unwrap_err();
        assert!(e.contains(r"203.0.113.5\s") && !e.contains("cannot list"), "{e}");
    }

    /// `COM1:` and `NUL:x`, as well as a bare device name, are refused before a file call, and a
    /// plain name holding such a word isn't.
    #[cfg(windows)]
    #[test]
    fn device_names_with_a_stream_are_refused() {
        let (base, roots) = folders("device-stream");
        for name in ["COM1:", "NUL:x", "nul", "COM¹.mp4", "CONIN$"] {
            let e = roots.check(&s(base.join("out").join(name)), Access::Write).unwrap_err();
            assert!(e.contains("reserved device names"), "{name}: {e}");
        }
        assert_eq!(roots.check(&s(base.join("out/null.mp4")), Access::Write), Ok(()));
    }

    /// Links that lead back into each other are given up on after [`MAX_LINKS`], not followed for ever.
    #[cfg(unix)]
    #[test]
    fn a_loop_of_links_is_given_up_on() {
        let (base, _roots) = folders("link-loop");
        let (a, b) = (base.join("in/a"), base.join("in/b"));
        if !made(&b, &a) || !made(&a, &b) {
            // On a CI runner the links must be possible, or this proves nothing and passes unseen.
            assert!(std::env::var_os("CI").is_none(), "links can't be made on a CI runner");
            eprintln!("skipped: links can't be made here");
            return;
        }
        let given = s(a.join("x.mp4"));
        let e = refuse_other_hosts(Path::new(&given), &given, &base).unwrap_err();
        assert!(e.contains("links on the way to"), "{e}");
    }

    /// A link standing outside the write root that leads into it: the file is made beside the link, outside.
    #[cfg(unix)]
    #[test]
    fn a_link_outside_the_write_root_leading_into_it_is_refused() {
        let (base, roots) = folders("link-in");
        let real = base.join("out/real.mp4");
        std::fs::write(&real, "x").unwrap();
        let link = base.join("outside/result");
        if !made(&real, &link) {
            assert!(std::env::var_os("CI").is_none(), "links can't be made on a CI runner");
            eprintln!("skipped: links can't be made here");
            return;
        }
        let e = roots.check_write_target(&s(link)).unwrap_err();
        assert!(e.contains("outside the write root"), "{e}");
        assert_eq!(roots.check_write_target(&s(real)), Ok(()));
    }

    /// A link whose target goes down a name and back up can't be read from its text, so it is refused.
    #[cfg(unix)]
    #[test]
    fn a_link_that_goes_down_and_back_up_is_refused() {
        let (base, _roots) = folders("link-updown");
        let link = base.join("in/l");
        if !made(Path::new("hop/../x"), &link) {
            // On a CI runner the links must be possible, or this proves nothing and passes unseen.
            assert!(std::env::var_os("CI").is_none(), "links can't be made on a CI runner");
            eprintln!("skipped: links can't be made here");
            return;
        }
        let given = s(link.join("x.mp4"));
        let e = refuse_other_hosts(Path::new(&given), &given, &base).unwrap_err();
        assert!(e.contains("goes down a name and back up"), "{e}");
        let plain_up = base.join("in/m");
        assert!(made(Path::new("../outside"), &plain_up));
        let given = s(plain_up.join("secret.cube"));
        assert_eq!(refuse_other_hosts(Path::new(&given), &given, &base), Ok(()), "a way up first is fine");
    }

    /// A folder can be set to tell case apart, and then `Root` is not `root`.
    #[cfg(windows)]
    #[test]
    fn a_folder_differing_only_in_case_is_another_folder() {
        let (base, _) = folders("case-sensitive");
        let parent = base.join("parent");
        std::fs::create_dir(&parent).unwrap();
        let set = std::process::Command::new("fsutil").args(["file", "setCaseSensitiveInfo"]).arg(&parent).arg("enable").output();
        if !set.is_ok_and(|o| o.status.success()) {
            eprintln!("skipped: can't set a folder to tell case apart here");
            return;
        }
        std::fs::create_dir(parent.join("root")).unwrap();
        std::fs::create_dir(parent.join("Root")).unwrap();
        let roots = AutomationRoots::new(None, Some(&s(parent.join("root")))).unwrap().unwrap();
        assert_eq!(roots.check(&s(parent.join("root/x.png")), Access::Write), Ok(()));
        let e = roots.check(&s(parent.join("Root/x.png")), Access::Write).unwrap_err();
        assert!(e.contains("outside the write root"), "{e}");
    }

    /// Importing on the bridge takes media files: the app would add a preset file to its library and
    /// write a document's audio beside it, goes by the content of such a file, and imports all a
    /// folder holds.
    #[test]
    fn the_bridge_imports_only_media_files() {
        let folder = std::env::temp_dir();
        let folder = folder.to_string_lossy();
        for id in IMPORTS {
            for not_media in ["/in/p.prfpset", "/in/cut.aaf", "/in/cut.omf", "/in/cut.xml", "/in/no-extension", &*folder] {
                for params in [json!({"paths": ["/in/a.png", not_media]}), json!({"paths": not_media}), json!({"path": not_media})] {
                    let e = AutomationRoots::check_bridge_command(id, &params).unwrap_err();
                    assert!(e.contains("takes only media files"), "{id} {params}: {e}");
                }
            }
            assert_eq!(AutomationRoots::check_bridge_command(id, &json!({"paths": ["/in/a.png", "/in/a.wav", "/in/a.mp4"]})), Ok(()), "{id}");
        }
        assert_eq!(AutomationRoots::check_bridge_command("captions.import", &json!({"path": "/in/a.srt"})), Ok(()));
    }

    /// Windows device names, with and without an extension, and names that only look like them.
    #[test]
    fn device_names() {
        for d in ["CON", "nul", "Com1", "LPT9.txt", "aux.svg", "NUL ", "COM¹", "lpt³.txt", "com²", "NUL:", "con:x"] {
            assert!(is_device_name(d), "{d}");
        }
        for f in ["console", "com10", "COM0", "lpt", "a.cube", "nul-x", "COM⁴"] {
            assert!(!is_device_name(f), "{f}");
        }
    }

    /// A list in place of a single path, or an empty list of paths, names no checked place: the command
    /// would pick its own, so both are refused.
    #[test]
    fn a_list_that_names_no_checked_place_is_refused() {
        let (base, roots) = folders("lists");
        let inside = s(base.join("out/a.fcproj"));
        for params in [json!({"path": []}), json!({"path": [inside.clone()]}), json!({"path": {"x": inside.clone()}})] {
            let e = roots.check_command("file.save", &params).unwrap_err();
            assert!(e.contains("must be a file path"), "{params}: {e}");
        }
        let e = roots.check_command("file.import", &json!({"paths": []})).unwrap_err();
        assert!(e.contains("at least one file"), "{e}");
        let e = roots.check_command("file.import", &json!({"paths": s(base.join("in/a.cube"))})).unwrap_err();
        assert!(e.contains("list of file paths"), "{e}");
        assert_eq!(roots.check_command("file.save", &json!({"path": inside})), Ok(()));
        assert_eq!(roots.check_command("file.import", &json!({"paths": [s(base.join("in/a.cube"))]})), Ok(()));
    }

    /// With the write root itself as `path`, an AAF or OMF export writes a folder beside the root.
    /// Both backends refuse it: the engine does, headless, and the command check does, so the bridge.
    #[test]
    fn a_folder_derived_beside_the_write_root_is_refused() {
        let (base, roots) = folders("derived-folder");
        let out = base.join("out");
        for id in ["file.exportAaf", "file.exportOmf"] {
            for given in [s(out.clone()), format!("{}/", s(out.clone()))] {
                let e = roots.check_command(id, &json!({"path": given})).unwrap_err();
                assert!(e.contains("outside the write root") && e.contains("also writes a folder"), "{id} {given}: {e}");
            }
            assert_eq!(roots.check_command(id, &json!({"path": s(out.join("edit.aaf"))})), Ok(()), "{id}: a file inside the root is fine");
        }
        let as_path = |p: Option<String>| p.map(PathBuf::from);
        assert_eq!(as_path(folder_beside("/w/out", " Media")), Some(PathBuf::from("/w/out Media")));
        assert_eq!(as_path(folder_beside("/w/out/edit.aaf", " Media")), Some(PathBuf::from("/w/out/edit Media")));
        assert_eq!(folder_beside("/", " Media"), None);
    }

    /// A media-named folder is not a media file, and the app's autosave of the open project stays off.
    #[test]
    fn the_bridge_takes_files_and_leaves_autosave_of_the_project_off() {
        let (base, roots) = folders("bridge-media");
        let folder = base.join("in/batch.mp4");
        std::fs::create_dir(&folder).unwrap();
        let file = base.join("in/clip.mp4");
        std::fs::write(&file, "x").unwrap();
        let named = |p: &Path| json!({"paths": [s(p.to_path_buf())]});
        assert_eq!(roots.check_command("file.import", &named(&folder)), Ok(()), "inside the root, so the roots pass it");
        assert_eq!(AutomationRoots::check_bridge_command("file.import", &named(&folder)), Ok(()), "the extension is right");
        let e = roots.check_bridge_media_files("file.import", &named(&folder)).unwrap_err();
        assert!(e.contains("is not a file"), "{e}");
        assert_eq!(roots.check_bridge_media_files("file.import", &named(&file)), Ok(()));
        assert!(roots.check_bridge_media_files("file.import", &json!({"paths": [s(base.join("in/none.mp4"))]})).is_err());
        for params in [json!({"key": "autoSave.saveCurrentProject", "value": true}), json!({"values": {"autoSave.saveCurrentProject": "yes"}})] {
            let e = roots.check_command("prefs.set", &params).unwrap_err();
            assert!(e.contains("stays off while file access is confined"), "{params}: {e}");
        }
        assert_eq!(roots.check_command("prefs.set", &json!({"key": "autoSave.saveCurrentProject", "value": false})), Ok(()));
        for params in [json!({}), json!({"category": "autoSave"}), json!({"category": 7})] {
            let e = roots.check_command("prefs.reset", &params).unwrap_err();
            assert!(e.contains("turn Auto Save back on"), "{params}: {e}");
        }
        assert_eq!(roots.check_command("prefs.reset", &json!({"category": "playback"})), Ok(()));
    }

    /// A numbered still brings its neighbours: a link among them that leads out of the read root is refused on the
    /// bridge however the sequence is asked for, and a link that stays inside, or a still that isn't numbered, is not.
    #[cfg(unix)]
    #[test]
    fn a_link_among_the_frames_of_an_image_sequence_is_refused_on_the_bridge() {
        let (base, roots) = folders("frames");
        let write = |name: &str| std::fs::write(base.join("in").join(name), "x").unwrap();
        write("frame0001.png");
        write("frame0002.png");
        write("other.png");
        let seed = s(base.join("in/frame0001.png"));
        assert_eq!(roots.check_bridge_media_files("file.import", &json!({"paths": [seed.clone()]})), Ok(()));
        let outside = base.join("outside/secret.cube");
        if !made(&outside, &base.join("in/frame0003.png")) {
            assert!(std::env::var_os("CI").is_none(), "links can't be made on a CI runner");
            eprintln!("skipped: links can't be made here");
            return;
        }
        for id in ["file.import", "file.importImageSequence", "mediaBrowser.import"] {
            let params = if id == "file.importImageSequence" { json!({"path": seed.clone()}) } else { json!({"paths": [seed.clone()]}) };
            let e = roots.check_bridge_media_files(id, &params).unwrap_err();
            assert!(e.contains("outside the read root") && e.contains("frames of an image sequence"), "{id}: {e}");
        }
        let unnumbered = s(base.join("in/other.png"));
        assert_eq!(roots.check_bridge_media_files("file.import", &json!({"paths": [unnumbered]})), Ok(()), "a still with no number has no neighbours");
        // A seed that is a link into the root, standing outside it, brings the outside folder's frames.
        std::fs::write(base.join("outside/frame0002.png"), "x").unwrap();
        assert!(made(&base.join("in/frame0001.png"), &base.join("outside/frame0001.png")));
        let e = roots.check_bridge_media_files("file.import", &json!({"paths": [s(base.join("outside/frame0001.png"))]})).unwrap_err();
        assert!(e.contains("outside the read root"), "{e}");
        std::fs::remove_file(base.join("in/frame0003.png")).unwrap();
        assert!(made(&base.join("in/frame0001.png"), &base.join("in/frame0004.png")));
        assert_eq!(roots.check_bridge_media_files("file.import", &json!({"paths": [seed]})), Ok(()), "a link that stays inside is fine");
    }

    /// A folder preference given as a bare name would be read against the server's working folder.
    #[test]
    fn a_folder_preference_must_be_absolute_or_empty() {
        let (base, roots) = folders("folder-prefs");
        for key in FOLDER_PREFS {
            for params in [json!({"key": key, "value": "cache"}), json!({"values": {(*key): "cache"}}), json!({"key": key, "value": 7})] {
                let e = roots.check_command("prefs.set", &params).unwrap_err();
                assert!(e.contains("must be an absolute folder"), "{params}: {e}");
            }
            assert_eq!(roots.check_command("prefs.set", &json!({"key": key, "value": ""})), Ok(()), "{key}: empty is the default");
            assert_eq!(roots.check_command("prefs.set", &json!({"key": key, "value": s(base.join("out/cache"))})), Ok(()), "{key}");
        }
        assert_eq!(roots.check_command("prefs.set", &json!({"key": "autoSave.maxVersions", "value": 5})), Ok(()));
    }

    /// [`FOLDER_PREFS`] names every preference the settings schema types as a path.
    #[test]
    fn folder_prefs_match_the_settings_schema() {
        /// The `key` of every field in `v` whose `kind` is a path.
        fn paths(v: &Value, out: &mut Vec<String>) {
            match v {
                Value::Object(o) => {
                    if let (Some(Value::String(k)), Some("path")) = (o.get("key"), o.get("kind").and_then(|k| k.get("type")).and_then(Value::as_str)) {
                        out.push(k.clone());
                    }
                    o.values().for_each(|x| paths(x, out));
                }
                Value::Array(a) => a.iter().for_each(|x| paths(x, out)),
                _ => {}
            }
        }
        let mut session = filmcraft_engine::Session::default();
        let schema = session.execute("prefs.schema", json!({})).unwrap();
        let mut found = vec![];
        paths(&schema, &mut found);
        found.sort();
        let mut known: Vec<String> = FOLDER_PREFS.iter().map(|k| k.to_string()).collect();
        known.sort();
        assert_eq!(found, known);
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
    }

    /// Preferences naming a folder the app writes into later are held to the write root.
    #[test]
    fn preferences_are_held_to_the_write_root() {
        let (base, roots) = folders("prefs");
        let e = roots.check_command("prefs.set", &json!({"key": "mediaCache.location", "value": s(base.join("outside"))})).unwrap_err();
        assert!(e.contains("outside the write root"), "{e}");
        assert_eq!(roots.check_command("prefs.set", &json!({"key": "mediaCache.location", "value": s(base.join("out/cache"))})), Ok(()));
        // Scratch disks name folders the app writes into later.
        let e = roots.check_command("file.projectSettings.scratchDisks", &json!({"captured": s(base.join("outside"))})).unwrap_err();
        assert!(e.contains("outside the write root"), "{e}");
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
    }

    /// A refused command names its reason; `media.makeOffline` is refused only when it would delete.
    #[test]
    fn a_refused_command_says_why() {
        let (_base, roots) = folders("refused");
        let e = roots.check_command("clip.updateMetadata", &json!({})).unwrap_err();
        assert!(e.contains("not available while file access is confined") && e.contains("sidecar"), "{e}");
        assert!(roots.check_command("media.makeOffline", &json!({"deleteFiles": true})).is_err());
        assert_eq!(roots.check_command("media.makeOffline", &json!({"deleteFiles": false})), Ok(()));
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
        assert!(roots.check_command("mediaBrowser.navigate", &json!({"up": true})).unwrap_err().contains("not available"));
        let inside = s(base.join("in"));
        for flag in ["up", "back", "forward"] {
            let e = roots.check_command("mediaBrowser.navigate", &json!({"path": inside.clone(), (flag): true})).unwrap_err();
            assert!(e.contains(&format!("`{flag}`")) && e.contains("goes to a folder that no path names"), "{flag}: {e}");
            assert_eq!(roots.check_command("mediaBrowser.navigate", &json!({"path": inside.clone(), (flag): false})), Ok(()), "{flag}: false is no flag");
        }
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
        assert!(e.contains("must be an absolute folder"), "{e}");
        assert_eq!(roots.check_command("prefs.set", &json!({"key": "theme", "value": "dark"})), Ok(()));
    }
}
