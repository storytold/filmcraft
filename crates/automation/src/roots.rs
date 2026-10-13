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
//! - Besides the file parameters each command names ([`READS`], [`WRITES`]), any other absolute
//!   path anywhere in its params is checked the same way, so a path can't slip through an unnamed
//!   key; `prefs.set` (whose values name folders the app writes into later) is held to the write
//!   root.
//! - The bridge's UI-driving tools are refused while confined: they could drive the live app's
//!   file dialogs past the roots.
//!
//! A path is checked, then opened: a process that changes the folders in between can still win
//! that race. The roots keep an agent to the files it was given; they don't guard against other
//! programs on the computer.

use std::ffi::OsString;
use std::path::{Component, Path, PathBuf};

use serde_json::Value;

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

    /// Check command `id` with `params` before it runs: every file parameter the command names must
    /// lie inside the roots, and so must any other absolute path anywhere in `params`.
    pub fn check_command(&self, id: &str, params: &Value) -> Result<(), String> {
        let named = |table: &[(&str, &'static [&'static str])]| table.iter().find(|(c, _)| *c == id).map(|(_, keys)| *keys);
        // A command that writes is held to the write root throughout; one that only reads, and any
        // command with no file parameters, to the read root. `prefs.set` writes later where its
        // values point, so its paths are held to the write root too.
        let writes = named(WRITES);
        let reads = named(READS);
        let access = if writes.is_some() || (reads.is_none() && id == "prefs.set") { Access::Write } else { Access::Read };
        for key in writes.or(reads).unwrap_or(&[]) {
            match params.get(*key).filter(|v| !v.is_null()) {
                None => {}
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
        let mut other = vec![];
        absolute_paths(params, writes.or(reads).unwrap_or(&[]), &mut other);
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
    ("media.linkMedia", &["path"]),
    ("media.replaceFootage", &["path"]),
    ("media.reconnectFullRes", &["path"]),
    ("media.attachProxies", &["path", "paths"]),
    ("media.autoRelink", &["folder"]),
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
    ("captions.import", &["path"]),
    ("graphics.newFromFile", &["path", "paths"]),
    ("graphics.template.install", &["path"]),
    ("edit.pasteMedia", &["paths"]),
    ("presets.import", &["path"]),
    ("shortcuts.import", &["path"]),
    ("labels.importPreset", &["path"]),
    ("export.presets.import", &["path"]),
];

/// Commands that write files named by these parameters. `file.save` without `path` writes the
/// project's own file, which the person at the keyboard chose.
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
    ("clip.extractAudio", &["dir"]),
    ("tts.create", &["dir"]),
    ("tts.edit", &["dir"]),
    ("tts.preview", &["path"]),
    ("lumetri.presetThumbnails", &["path"]),
    ("project.ingestSettings", &["destination"]),
];

/// `path` made absolute with its links followed; for a path that doesn't exist (yet), its nearest
/// existing folder's links followed and the rest appended.
fn resolve(path: &str) -> Result<PathBuf, String> {
    if path.is_empty() {
        return Err("automation path rejected: path is empty".into());
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

/// Strings in `params` (recursively, except under the `skip` keys the command's table already
/// checked) that look like absolute paths: leading `/` or `\`, `~`, a Windows drive, or a `file:`
/// URL. Recursion is bounded, params being input.
fn absolute_paths(params: &Value, skip: &[&str], out: &mut Vec<String>) {
    fn walk(v: &Value, out: &mut Vec<String>, depth: usize) {
        if depth > 64 {
            return;
        }
        match v {
            Value::String(s) => {
                let t = s.trim();
                let drive = t.len() > 2 && t.as_bytes().get(1) == Some(&b':') && t.as_bytes().first().is_some_and(u8::is_ascii_alphabetic);
                if t.starts_with('/') || t.starts_with('\\') || t.starts_with('~') || drive || t.to_ascii_lowercase().starts_with("file:") {
                    out.push(t.to_string());
                }
            }
            Value::Array(a) => a.iter().for_each(|x| walk(x, out, depth + 1)),
            Value::Object(o) => o.values().for_each(|x| walk(x, out, depth + 1)),
            _ => {}
        }
    }
    match params {
        Value::Object(o) => o.iter().filter(|(k, _)| !skip.contains(&k.as_str())).for_each(|(_, v)| walk(v, out, 0)),
        v => walk(v, out, 0),
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
    fn relative_paths_resolve_against_the_working_directory() {
        let (base, roots) = folders("relative");
        // The tests' working directory is the crate's folder: not inside the roots.
        assert!(roots.check("a.cube", Access::Read).unwrap_err().contains("outside the read root"));
        assert_eq!(roots.check("", Access::Read).unwrap_err(), "automation path rejected: path is empty");
        let _ = std::fs::remove_dir_all(base);
    }

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

    #[test]
    fn device_names() {
        for d in ["CON", "nul", "Com1", "LPT9.txt", "aux.svg", "NUL "] {
            assert!(is_device_name(d), "{d}");
        }
        for f in ["console", "com10", "COM0", "lpt", "a.cube", "nul-x"] {
            assert!(!is_device_name(f), "{f}");
        }
    }

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

    /// A relative path in a command is resolved against the working directory before comparing.
    #[test]
    fn relative_command_paths_are_checked_as_they_would_open() {
        let (base, roots) = folders("relative-command");
        assert!(roots.check_command("lut.import", &json!({"path": "a.cube"})).is_err(), "the crate folder is not inside the roots");
        let _ = std::fs::remove_dir_all(base);
    }
}
