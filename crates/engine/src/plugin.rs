//! Plug-ins: WebAssembly modules that act on the open project through the same commands as the
//! menus, the CLI, the control channel and MCP, inside the sandbox of the shared `craft-plugin`
//! crate (RFC-0046). This module is FilmCraft's side of it: which commands a plug-in may call
//! ([`policy`], [`refusal`]), one undo step per run ([`Session::edit_group`], rolled back when the
//! run fails), read-only access, preferences and background work checked after every call
//! ([`host`]), each plug-in's settings (kept with the preferences) and the installed plug-ins (the
//! user's `Plug-ins` folder in the data directory, or this session only). The `plugin.*` commands
//! are in [`commands`]. See `docs/plugins.md`.
//!
//! A plug-in calls commands in their JSON form ([`Session::execute`]). Times go to those commands
//! as they document them (ticks, `frame`, `timecode`); the host does no time arithmetic of its own.

mod commands;
mod host;
mod install;
mod runtime;

use std::sync::OnceLock;

pub use craft_plugin;
use craft_plugin::{API_VERSION, AppInfo, Instance, KIND_COMMAND, Limits, Manifest, Policy, RunOutput, UiChange};
use serde_json::{Map, Value, json};

use crate::commands::bad;
use crate::{EngineError, Result, Session};
pub use commands::specs;
use host::EngineHost;
pub use install::{Candidate, catalog_packages_with, download, offer, update_for_with};
pub use runtime::{Fetcher, InstalledPlugin, PluginPrefs, Runtime};

/// FilmCraft as manifests name it: command plug-ins of every shared ABI version, and only
/// manifests that name `filmcraft` in `apps`.
pub const APP: AppInfo<'static> =
    AppInfo { id: "filmcraft", version: env!("CARGO_PKG_VERSION"), api: 1..=API_VERSION, kinds: &[KIND_COMMAND], accept_legacy: false, kind_api: &[] };

/// What one plug-in may keep in its settings (keys and JSON values, in bytes), and its longest key.
pub const MAX_SETTINGS_BYTES: usize = 64 * 1024;
pub const MAX_SETTING_KEY: usize = 128;
/// Plug-ins whose settings are kept (more are dropped when the preferences are read).
pub const MAX_SETTINGS_PLUGINS: usize = 256;
/// Catalogs a person may add.
pub const MAX_CATALOGS: usize = 32;
/// Messages a run may show (`notify`), and their length in characters.
const MAX_NOTES: usize = 20;
const MAX_NOTE_CHARS: usize = 1000;
/// Least instruction budget `plugin.run {fuel}` may ask for.
const MIN_FUEL: u64 = 1000;

/// FilmCraft's commands no plug-in may call, on top of [`craft_plugin::ALWAYS_DENIED`] (which
/// already refuses `file.*`, `edit.undo`, `edit.redo`, `app.*`, `window.*` and `plugin.*`). A
/// plug-in works on the open project only: no files or folders, no media on disk, no other
/// project, no undo history, no preferences or user libraries, no clipboard, no recording, no
/// background jobs and no playback.
const DENIED: &[&str] = &[
    // The undo history.
    "history.*",
    // Preferences and the user's libraries, kept outside the project in the data directory.
    "prefs.*",
    "shortcuts.*",
    "presets.*",
    "labels.*",
    "lut.*",
    "graphics.template.install",
    "graphics.template.remove",
    "graphics.template.export",
    "essentialSound.savePreset",
    "essentialSound.deletePreset",
    "project.viewPreset.*",
    // Media on disk: relinking, proxies, offline and replaced footage, searching folders, the
    // Media Browser, the media cache, writing metadata or audio beside media.
    "media.*",
    "mediaBrowser.*",
    "mediaCache.*",
    "clip.extractAudio",
    "clip.updateMetadata",
    // Writing or reading files by name: exports, imports, captions, markers, LUTs, thumbnails.
    "export.*",
    "captions.import",
    "captions.export",
    "markers.exportCsv",
    "lumetri.exportCube",
    "lumetri.presetThumbnails",
    "graphics.newFromFile",
    // Opening another application on the media.
    "edit.editOriginal",
    // The clipboard is the person's, shared with every project.
    "edit.copy",
    "edit.cut",
    "edit.paste",
    "edit.pasteInsert",
    "edit.pasteAttributes",
    "edit.pasteMedia",
    // Rendering and background work whose results land after the run (outside its undo step).
    "sequence.render*",
    "sequence.deleteRenderFiles*",
    "masks.track*",
    "clip.sceneEditDetection",
    "sequence.transcribe",
    "transcript.generate",
    "transcript.downloadModel",
    "tts.*",
    "jobs.*",
    // Recording and live passes: the microphone, mixer and multi-camera recording, playback.
    "audio.voiceover.*",
    "mixer.recordStart",
    "mixer.recordStop",
    "multicam.recordStart",
    "multicam.recordStop",
    "trim.shuttle*",
    "trim.tick",
    "trim.playAround",
    "trim.cancelDynamic",
    "playback.*",
    // The app's own state and the machine: the Events log, performance and system reports.
    "events.*",
    "perf.*",
    "help.*",
];

/// Exact ids allowed although [`DENIED`] matches them: they only read, or apply what is already
/// in the user's library to the project (an undoable edit).
const EXCEPTIONS: &[&str] = &["presets.list", "presets.apply", "media.status", "media.colorInfo", "tts.voices", "tts.inspect", "labels.presets"];

/// Parameter names that name a file or folder on disk, at any depth, when their value is text (or
/// a list holding text): a mask's `path` is a list of points, not a file.
const FILE_KEYS: &[&str] = &["path", "paths", "file", "files", "dir", "folder", "directory", "destination", "location"];

/// The rules every plug-in run follows: [`craft_plugin::ALWAYS_DENIED`], [`DENIED`] and
/// [`EXCEPTIONS`].
pub fn policy() -> &'static Policy {
    static POLICY: OnceLock<Policy> = OnceLock::new();
    POLICY.get_or_init(|| Policy::new(DENIED, EXCEPTIONS))
}

/// Why plug-in `m` may not call `command` with `params`, before it runs: a refused command, one
/// outside its manifest's `permissions.commands`, or parameters that name a file.
pub fn refusal(m: &Manifest, command: &str, params: &Value) -> Option<String> {
    if let Err(why) = craft_plugin::check_call(m, policy(), command) {
        return Some(why);
    }
    if names_a_file(params, 0) {
        let keys = FILE_KEYS.iter().map(|k| format!("`{k}`")).collect::<Vec<_>>().join(", ");
        return Some(format!("plug-ins can't name files (`{command}` was given one of {keys}): use what is already in the project"));
    }
    None
}

/// Whether `v` holds one of [`FILE_KEYS`] with text (or a list holding text), at any depth.
fn names_a_file(v: &Value, depth: usize) -> bool {
    let text = |v: &Value| v.is_string() || v.as_array().is_some_and(|a| a.iter().any(Value::is_string));
    // serde_json refuses nesting past 128, so nothing deeper reaches here
    depth < 128
        && match v {
            Value::Object(m) => m.iter().any(|(k, v)| (FILE_KEYS.contains(&k.as_str()) && text(v)) || names_a_file(v, depth + 1)),
            Value::Array(a) => a.iter().any(|v| names_a_file(v, depth + 1)),
            _ => false,
        }
}

/// A plug-in error as the engine reports it.
fn failed(name: &str, e: impl std::fmt::Display) -> EngineError {
    EngineError::Other(format!("{name}: {e}"))
}

/// Store (`Some`) or forget (`None`) plug-in `id`'s setting `key` in `all`, within
/// [`MAX_SETTINGS_BYTES`] and [`MAX_SETTING_KEY`].
pub(crate) fn set_setting(
    all: &mut std::collections::BTreeMap<String, Map<String, Value>>,
    id: &str,
    key: &str,
    value: Option<&str>,
) -> std::result::Result<(), String> {
    let Some(text) = value else {
        if let Some(mine) = all.get_mut(id) {
            mine.remove(key);
            if mine.is_empty() {
                all.remove(id);
            }
        }
        return Ok(());
    };
    if !runtime::valid_setting_key(key) {
        return Err(format!("a setting's key must be 1–{MAX_SETTING_KEY} characters without control characters"));
    }
    let too_big = || format!("a plug-in's settings are limited to {} KB", MAX_SETTINGS_BYTES / 1024);
    if key.len().saturating_add(text.len()) > MAX_SETTINGS_BYTES {
        return Err(too_big());
    }
    let v: Value = serde_json::from_str(text).map_err(|e| format!("a setting must be JSON: {e}"))?;
    let others = all.get(id).map_or(0, |mine| runtime::settings_size(mine.iter().filter(|(k, _)| k.as_str() != key)));
    if others.saturating_add(key.len()).saturating_add(v.to_string().len()) > MAX_SETTINGS_BYTES {
        return Err(too_big());
    }
    if !all.contains_key(id) && all.len() >= MAX_SETTINGS_PLUGINS {
        return Err(format!("settings are kept for at most {MAX_SETTINGS_PLUGINS} plug-ins"));
    }
    all.entry(id.to_string()).or_default().insert(key.to_string(), v);
    Ok(())
}

/// Write the preferences (the plug-ins' settings and catalogs are part of them), when the session
/// keeps them in a file.
pub(crate) fn save_prefs(s: &mut Session) {
    let Some(path) = s.prefs_path.clone() else { return };
    if let Err(e) = s.prefs.save(&path) {
        s.error_toast("plugin", format!("saving the plug-ins' settings: {e}"));
    }
}

// ---------- running ----------

/// The selected timeline clips' ids, as a plug-in sees the selection.
fn selection_json(s: &Session) -> Value {
    json!(s.state.selection.iter().map(|c| c.0).collect::<Vec<_>>())
}

/// Run plug-in `id`'s `command` (default: its first) with `params` on the open project, as one
/// undo step named after the plug-in ([`Session::edit_group`]). A run that fails (a trap, an
/// exhausted budget, a refusal it passes on, a non-zero status) leaves the project, the selection
/// and the rest of the editing state, and the undo history as they were.
pub fn run(s: &mut Session, id: &str, command: Option<&str>, params: &Value, fuel: Option<u64>) -> Result<Value> {
    let cmd = "plugin.run";
    let p = s.plugins.get(id).cloned().ok_or_else(|| bad(cmd, format!("no plug-in `{id}` is installed (see plugin.list)")))?;
    let m = &p.manifest;
    let api = m.target(&APP).map_err(|e| bad(cmd, format!("{} can't run here: {e}", m.name)))?.api();
    let c = m.command(command).ok_or_else(|| bad(cmd, format!("{} has no command `{}`", m.name, command.unwrap_or_default())))?;
    let params = c.resolve_params(params).map_err(|e| bad(cmd, e))?;
    let mut limits = Limits::default();
    if let Some(f) = fuel {
        limits.fuel = f.clamp(MIN_FUEL, limits.fuel);
    }
    let mut instance = Instance::new(&p.wasm, &limits).map_err(|e| failed(&m.name, e))?;
    let input = json!({"api": api, "command": c.id, "params": params, "selection": selection_json(s)}).to_string();
    s.plugins.last = Some((m.id.clone(), c.id.clone(), params));
    let label = if m.commands.len() > 1 { format!("{}: {}", m.name, c.label.trim_end_matches(['…', '.'])) } else { m.name.clone() };
    let settings_before = s.prefs.plugin_data.settings.clone();
    let (project_before, clean) = (s.project.clone(), !s.is_dirty());

    let ran = s.edit_group(&label, |s| {
        let mut host = EngineHost::new(s, m);
        let ran = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| instance.call(&input, &mut host)));
        let (notes, strayed) = host.finish();
        let out: std::result::Result<RunOutput, String> = match ran {
            Ok(Ok(_)) if strayed => Err("the run left the project it started in".into()),
            Ok(Ok(out)) => Ok(out),
            Ok(Err(e)) => Err(e.to_string()),
            Err(_) => Err("internal error (see the crash log); the run did not complete".into()),
        };
        out.map(|out| (out, notes)).map_err(|e| failed(&m.name, e))
    });
    if s.prefs.plugin_data.settings != settings_before {
        save_prefs(s);
    }
    // a failed run puts the project back: one without unsaved changes stays without them
    if ran.is_err() && clean && std::sync::Arc::ptr_eq(&s.project, &project_before) {
        s.saved_revision = s.revision;
    }
    let (out, notes) = ran?;
    for note in &notes {
        s.toast(format!("{}: {note}", m.name));
    }
    let output = serde_json::from_str::<Value>(&out.output).unwrap_or(Value::String(out.output));
    // Panels (`ui_show`) aren't shown in FilmCraft yet: say so rather than drop it silently
    let panel = matches!(out.ui, Some(UiChange::Show(_))).then_some("FilmCraft doesn't show plug-in panels yet");
    Ok(json!({"plugin": m.id, "command": c.id, "output": output, "notes": notes, "logs": out.logs, "calls": out.calls, "fuel": out.fuel, "panel": panel}))
}
