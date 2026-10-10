//! Keyboard shortcuts as data: the active key bindings, presets, conflict detection and the
//! `shortcuts.*` commands (Edit ▸ Keyboard Shortcuts…, and the same operations for agents over MCP).
//!
//! A [`Binding`] maps a key chord to a command id, either application-wide or for one panel
//! (panel shortcuts apply while that panel has focus and override application shortcuts).
//! Chords are stored in a canonical, platform-neutral notation:
//!
//! | token | macOS | Windows / Linux |
//! |---|---|---|
//! | `Cmd` | ⌘ Command | Ctrl |
//! | `Ctrl` | ⌃ Control | Ctrl (folds into `Cmd`; conflicts are reported) |
//! | `Alt` | ⌥ Option | Alt |
//! | `Shift` | ⇧ Shift | Shift |
//!
//! so one preset works on every OS. Preset files written for Windows (`"platform": "windows"`)
//! are converted on import: their `Ctrl` is the primary modifier (`Cmd`) and `Win`/`Meta` maps to
//! the macOS Control key.
//!
//! Built-in presets: **FilmCraft Default** (each command's default plus the Premiere-default audit,
//! see [`Shortcuts::audit`]), **Premiere Pro Compatible**, **Final Cut Pro Compatible** and
//! **Avid Media Composer Compatible** ([`crate::shortcut_presets`]). Custom presets are JSON files in
//! `<data dir>/Keyboard Shortcuts/`; the active set persists in `active.json` there.
//!
//! Commands the frontend owns (tools, transport, panels, workspaces) are registered with
//! [`Shortcuts::register_external`] so they can be listed, bound and resolved here too.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::commands::{CommandSpec, always, bad, bool_p, str_p};
use crate::shortcut_presets::{self as presets, Entry};
use crate::{EngineError, Session};

pub const APPLICATION: &str = "Application";

/// Panels that can have panel-specific shortcuts (the shortcut editor's "Commands" menu).
pub const PANELS: &[&str] = &[
    "Timeline",
    "Program",
    "Source",
    "Project",
    "Effect Controls",
    "Effects",
    "History",
    "Markers",
    "Media Browser",
    "Metadata",
    "Audio Track Mixer",
    "Audio Clip Mixer",
    "Essential Graphics",
    "Text",
    "Properties",
    "Lumetri Color",
];

pub const DEFAULT_PRESET: &str = "FilmCraft Default";
pub const PREMIERE_PRESET: &str = "Premiere Pro Compatible";
pub const FCP_PRESET: &str = "Final Cut Pro Compatible";
pub const AVID_PRESET: &str = "Avid Media Composer Compatible";
pub const BUILTIN_PRESETS: [&str; 4] = [DEFAULT_PRESET, PREMIERE_PRESET, FCP_PRESET, AVID_PRESET];

const FILE_FORMAT: &str = "filmcraft-keyboard-shortcuts";
const DIR_NAME: &str = "Keyboard Shortcuts";
const ACTIVE_FILE: &str = "active.json";

/// Every key the editor's keyboard shows, in canonical spelling (row order).
pub const KEYS: &[&str] = &[
    "Escape",
    "F1",
    "F2",
    "F3",
    "F4",
    "F5",
    "F6",
    "F7",
    "F8",
    "F9",
    "F10",
    "F11",
    "F12", //
    "`",
    "1",
    "2",
    "3",
    "4",
    "5",
    "6",
    "7",
    "8",
    "9",
    "0",
    "-",
    "=",
    "Backspace", //
    "Tab",
    "Q",
    "W",
    "E",
    "R",
    "T",
    "Y",
    "U",
    "I",
    "O",
    "P",
    "[",
    "]",
    "\\", //
    "A",
    "S",
    "D",
    "F",
    "G",
    "H",
    "J",
    "K",
    "L",
    ";",
    "'",
    "Enter", //
    "Z",
    "X",
    "C",
    "V",
    "B",
    "N",
    "M",
    ",",
    ".",
    "/", //
    "Space",
    "Insert",
    "Delete",
    "Home",
    "End",
    "PageUp",
    "PageDown",
    "Up",
    "Down",
    "Left",
    "Right",
];

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Platform {
    Mac,
    Windows,
    Linux,
}

impl Platform {
    pub fn current() -> Self {
        if cfg!(target_os = "macos") {
            Platform::Mac
        } else if cfg!(windows) {
            Platform::Windows
        } else {
            Platform::Linux
        }
    }
    pub fn from_name(s: &str) -> Option<Self> {
        match s.to_ascii_lowercase().as_str() {
            "mac" | "macos" | "osx" | "darwin" => Some(Platform::Mac),
            "windows" | "win" => Some(Platform::Windows),
            "linux" => Some(Platform::Linux),
            _ => None,
        }
    }
    pub fn is_mac(self) -> bool {
        self == Platform::Mac
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Mods {
    pub cmd: bool,
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
}

impl Mods {
    pub fn count(self) -> u8 {
        self.cmd as u8 + self.ctrl as u8 + self.alt as u8 + self.shift as u8
    }
    /// Canonical prefix, e.g. `Cmd+Alt+`.
    pub fn prefix(self) -> String {
        let mut s = String::new();
        for (on, n) in [(self.cmd, "Cmd+"), (self.ctrl, "Ctrl+"), (self.alt, "Alt+"), (self.shift, "Shift+")] {
            if on {
                s.push_str(n);
            }
        }
        s
    }
}

/// A key plus modifiers.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Chord {
    pub mods: Mods,
    pub key: &'static str,
}

fn canon_key(tok: &str) -> Option<&'static str> {
    if let Some(k) = KEYS.iter().find(|k| k.eq_ignore_ascii_case(tok)) {
        return Some(k);
    }
    let t = tok.to_ascii_lowercase().replace([' ', '_'], "");
    Some(match t.as_str() {
        "return" | "⏎" | "↩" | "numpadenter" => "Enter",
        "esc" | "⎋" => "Escape",
        "del" | "forwarddelete" | "fwddelete" | "⌦" => "Delete",
        "⌫" | "back" => "Backspace",
        "arrowup" | "↑" => "Up",
        "arrowdown" | "↓" => "Down",
        "arrowleft" | "←" => "Left",
        "arrowright" | "→" => "Right",
        "pgup" | "pageup" => "PageUp",
        "pgdn" | "pagedown" => "PageDown",
        "spacebar" | "␣" => "Space",
        "help" | "ins" => "Insert",
        "⇥" => "Tab",
        "grave" | "backtick" | "backquote" => "`",
        "minus" => "-",
        "equal" | "equals" => "=",
        "semicolon" => ";",
        "quote" | "apostrophe" => "'",
        "comma" => ",",
        "period" | "dot" => ".",
        "slash" => "/",
        "backslash" => "\\",
        "openbracket" | "bracketleft" => "[",
        "closebracket" | "bracketright" => "]",
        _ => return None,
    })
}

impl Chord {
    /// Parse canonical (or macOS-style) text: `Cmd+Shift+K`, `⌥⌘K`, `opt+left`…
    pub fn parse(s: &str) -> std::result::Result<Chord, String> {
        Self::parse_for(s, Platform::Mac)
    }

    /// Parse text written for `platform` (on Windows/Linux `Ctrl` is the primary modifier).
    pub fn parse_for(s: &str, platform: Platform) -> std::result::Result<Chord, String> {
        let s = s.trim();
        if s.is_empty() {
            return Err("empty shortcut".into());
        }
        // glyph runs like "⌥⇧⌘K" → tokens
        let mut text = String::new();
        for ch in s.chars() {
            match ch {
                '⌘' => text.push_str("Cmd+"),
                '⌃' => text.push_str("Ctrl+"),
                '⌥' => text.push_str("Alt+"),
                '⇧' => text.push_str("Shift+"),
                c => text.push(c),
            }
        }
        let text = text.replace("++", "+").trim_end_matches('+').to_string();
        let mut mods = Mods::default();
        let mut key = None;
        let parts: Vec<&str> = text.split('+').map(str::trim).filter(|p| !p.is_empty()).collect();
        for (i, p) in parts.iter().enumerate() {
            let low = p.to_ascii_lowercase();
            let is_last = i + 1 == parts.len();
            match low.as_str() {
                "cmd" | "command" | "super" => mods.cmd = true,
                "meta" | "win" | "windows" if !platform.is_mac() => mods.ctrl = true,
                "meta" => mods.cmd = true,
                "ctrl" | "control" | "ctl" => {
                    if platform.is_mac() {
                        mods.ctrl = true
                    } else {
                        mods.cmd = true
                    }
                }
                "alt" | "opt" | "option" => mods.alt = true,
                "shift" => mods.shift = true,
                _ if is_last => key = Some(canon_key(p).ok_or_else(|| format!("unknown key `{p}`"))?),
                _ => return Err(format!("unknown modifier `{p}`")),
            }
        }
        let key = key.ok_or_else(|| format!("`{s}` has no key"))?;
        Ok(Chord { mods, key })
    }

    pub fn canonical(&self) -> String {
        format!("{}{}", self.mods.prefix(), self.key)
    }

    /// The chord as this OS sees it: off macOS the Control key is the primary modifier, so `Ctrl`
    /// and `Cmd` are the same key.
    pub fn effective(&self, p: Platform) -> Chord {
        let mut c = self.clone();
        if !p.is_mac() && c.mods.ctrl {
            c.mods.ctrl = false;
            c.mods.cmd = true;
        }
        c
    }

    /// Human text: `⌥⇧⌘K` on macOS, `Ctrl+Alt+Shift+K` elsewhere.
    pub fn display(&self, p: Platform) -> String {
        if p.is_mac() {
            let mut s = String::new();
            for (on, g) in [(self.mods.ctrl, "⌃"), (self.mods.alt, "⌥"), (self.mods.shift, "⇧"), (self.mods.cmd, "⌘")] {
                if on {
                    s.push_str(g);
                }
            }
            let k = match self.key {
                "Backspace" => "⌫",
                "Delete" => "⌦",
                "Enter" => "↩",
                "Escape" => "⎋",
                "Tab" => "⇥",
                "Left" => "←",
                "Right" => "→",
                "Up" => "↑",
                "Down" => "↓",
                "PageUp" => "Page Up",
                "PageDown" => "Page Down",
                k => k,
            };
            s.push_str(k);
            s
        } else {
            let e = self.effective(p);
            let mut s = String::new();
            for (on, n) in [(e.mods.cmd, "Ctrl+"), (e.mods.alt, "Alt+"), (e.mods.shift, "Shift+")] {
                if on {
                    s.push_str(n);
                }
            }
            s.push_str(match self.key {
                "PageUp" => "Page Up",
                "PageDown" => "Page Down",
                k => k,
            });
            s
        }
    }
}

/// Canonical spelling of a chord (`opt+cmd+k` → `Cmd+Alt+K`).
pub fn normalize(s: &str) -> std::result::Result<String, String> {
    Chord::parse(s).map(|c| c.canonical())
}

/// One key binding. `panel: None` = application-wide.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Binding {
    pub command: String,
    pub keys: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub panel: Option<String>,
}

/// Off macOS the Control and Command keys are the same key. The built-in tables are written for
/// macOS, so a binding on the Mac Control key (`⌃T`) can land on the same key as a Command binding
/// (`⌘T`) in the same context: the Command binding wins and the Control one is left out there
/// (that command has no default key on that platform). macOS keeps both.
fn drop_control_collisions(b: &mut Vec<Binding>, p: Platform) {
    if p.is_mac() {
        return;
    }
    let primary: Vec<(String, Option<Chord>)> =
        b.iter().filter(|x| x.chord().is_some_and(|c| !c.mods.ctrl)).map(|x| (x.context().to_string(), x.chord().map(|c| c.effective(p)))).collect();
    b.retain(|x| {
        let Some(c) = x.chord() else { return true };
        !(c.mods.ctrl && primary.iter().any(|(ctx, k)| ctx == x.context() && *k == Some(c.effective(p))))
    });
}

impl Binding {
    fn new(command: &str, keys: &str, panel: Option<&str>) -> Self {
        Binding { command: command.to_string(), keys: normalize(keys).unwrap_or_else(|_| keys.to_string()), panel: panel.map(str::to_string) }
    }
    fn chord(&self) -> Option<Chord> {
        Chord::parse(&self.keys).ok()
    }
    fn context(&self) -> &str {
        self.panel.as_deref().unwrap_or(APPLICATION)
    }
}

/// A command that can be bound (engine command or one registered by the frontend).
#[derive(Clone, Debug, Serialize)]
pub struct CommandInfo {
    pub id: String,
    pub label: String,
    pub category: String,
    pub menu: Vec<String>,
    /// Default bindings: (keys, panel).
    #[serde(skip)]
    pub defaults: Vec<(String, Option<String>)>,
}

impl CommandInfo {
    pub fn new(id: &str, label: &str, menu: &[&str], default: Option<&str>) -> Self {
        CommandInfo {
            id: id.into(),
            label: label.into(),
            category: category_of(id, menu),
            menu: menu.iter().map(|s| s.to_string()).collect(),
            defaults: default.map(|k| vec![(k.to_string(), None)]).unwrap_or_default(),
        }
    }
}

fn category_of(id: &str, menu: &[&str]) -> String {
    let prefix = id.split('.').next().unwrap_or("");
    match prefix {
        "tool" => "Tools".into(),
        "playback" => "Playback".into(),
        "playhead" => "Navigation".into(),
        "trim" => "Trimming".into(),
        "window" => "Window".into(),
        "view" => "View".into(),
        "app" | "help" | "mode" => "Application".into(),
        "captions" => "Captions".into(),
        "tts" => "Text to Speech".into(),
        _ => menu.first().map(|s| s.to_string()).unwrap_or_else(|| {
            let mut c = prefix.chars();
            c.next().map(|f| f.to_uppercase().collect::<String>() + c.as_str()).unwrap_or_default()
        }),
    }
}

/// Engine commands worth binding: undoable actions that need no required parameters.
fn bindable(c: &CommandSpec) -> bool {
    if !c.journal || c.id.starts_with("shortcuts.") || c.id.starts_with("prefs.") || c.id == "trim.tick" {
        return false;
    }
    if c.shortcut.is_some() {
        return true;
    }
    let body = c.params.trim().trim_start_matches('{').trim_end_matches('}');
    if body.trim().is_empty() {
        return true;
    }
    let mut depth = 0;
    let mut fields = vec![String::new()];
    for ch in body.chars() {
        match ch {
            '[' | '{' => depth += 1,
            ']' | '}' => depth -= 1,
            ',' if depth == 0 => {
                fields.push(String::new());
                continue;
            }
            _ => {}
        }
        if let Some(f) = fields.last_mut() {
            f.push(ch);
        }
    }
    fields.iter().all(|f| f.contains('?') || f.contains('='))
}

/// The preset file / `active.json` format.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PresetFile {
    pub format: String,
    pub version: u32,
    pub name: String,
    /// The OS the key names were written for; None = canonical (FilmCraft) notation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub platform: Option<Platform>,
    /// `active.json` only: the active set differs from the preset it started from.
    #[serde(default)]
    pub modified: bool,
    pub bindings: Vec<Binding>,
}

#[derive(Clone, Debug)]
struct Snapshot {
    bindings: Vec<Binding>,
    preset: String,
    modified: bool,
}

/// The active key bindings and their editing history.
#[derive(Clone, Debug)]
pub struct Shortcuts {
    pub bindings: Vec<Binding>,
    /// Preset the active set came from (or was saved as).
    pub preset: String,
    /// Edited since the preset was loaded.
    pub modified: bool,
    /// Bumped on every change (frontends rebuild key maps and menus).
    pub revision: u64,
    undo: Vec<Snapshot>,
    redo: Vec<Snapshot>,
    external: Vec<CommandInfo>,
    dir: Option<PathBuf>,
    /// The editor dialog's starting point (Cancel restores it).
    session_start: Option<Snapshot>,
}

impl Default for Shortcuts {
    fn default() -> Self {
        Self::new()
    }
}

impl Shortcuts {
    pub fn new() -> Self {
        let mut s = Shortcuts {
            bindings: Vec::new(),
            preset: DEFAULT_PRESET.into(),
            modified: false,
            revision: 1,
            undo: Vec::new(),
            redo: Vec::new(),
            external: Vec::new(),
            dir: None,
            session_start: None,
        };
        s.bindings = s.builtin(DEFAULT_PRESET).unwrap_or_default();
        s
    }

    // ---------------------------------------------------------------- commands

    /// Every bindable command (engine + frontend), in registry order.
    pub fn commands(&self) -> Vec<CommandInfo> {
        let mut v: Vec<CommandInfo> =
            crate::commands::command_specs().iter().filter(|c| bindable(c)).map(|c| CommandInfo::new(c.id, c.label, c.menu, c.shortcut)).collect();
        for e in &self.external {
            if !v.iter().any(|c| c.id == e.id) {
                v.push(e.clone());
            }
        }
        v
    }

    pub fn command(&self, id: &str) -> Option<CommandInfo> {
        self.commands().into_iter().find(|c| c.id == id)
    }

    fn known(&self, id: &str) -> bool {
        crate::commands::find(id).is_some() || self.external.iter().any(|c| c.id == id)
    }

    /// Commands owned by the frontend (tools, transport, panels…). Rebuilds the active set when it
    /// is an unmodified built-in preset, so their defaults appear.
    pub fn register_external(&mut self, cmds: Vec<CommandInfo>) {
        self.external = cmds;
        if !self.modified
            && let Some(b) = self.builtin(&self.preset.clone())
        {
            self.bindings = b;
            self.revision += 1;
        }
    }

    // ---------------------------------------------------------------- presets

    /// Default bindings of every known command, plus FilmCraft's panel shortcuts.
    fn base_defaults(&self) -> Vec<Binding> {
        let mut out = Vec::new();
        for c in self.commands() {
            for (k, p) in &c.defaults {
                out.push(Binding::new(&c.id, k, p.as_deref()));
            }
        }
        for (c, k, p) in presets::FILMCRAFT_PANEL.iter().chain(presets::PREMIERE_PANEL) {
            if self.known(c) {
                out.push(Binding::new(c, k, panel_opt(p)));
            }
        }
        out
    }

    /// Premiere-default entries FilmCraft Default adopts: commands with no shortcut of their own
    /// whose Premiere key is free in that context on this platform.
    pub fn audit(&self) -> Vec<Binding> {
        self.audit_for(Platform::current())
    }

    /// [`Self::audit`] for platform `p`. The Premiere table is Premiere's macOS keyboard: off macOS
    /// the Control and Command keys are the same key, so an entry whose key would collide there
    /// with one already taken (`⌃T` vs `⌘T`) is left out, and the command keeps no default key.
    pub(crate) fn audit_for(&self, platform: Platform) -> Vec<Binding> {
        let base = self.base_defaults();
        let mut added: Vec<Binding> = Vec::new();
        for (c, k, p) in presets::PREMIERE {
            if !self.known(c) || base.iter().any(|b| b.command == *c) {
                continue;
            }
            let b = Binding::new(c, k, panel_opt(p));
            let key = b.chord().map(|c| c.effective(platform));
            let taken = base.iter().chain(added.iter()).any(|o| o.context() == b.context() && o.chord().map(|c| c.effective(platform)) == key);
            if !taken {
                added.push(b);
            }
        }
        added
    }

    /// The bindings of a built-in preset.
    pub fn builtin(&self, name: &str) -> Option<Vec<Binding>> {
        self.builtin_for(name, Platform::current())
    }

    /// [`Self::builtin`] as it is built on platform `p`.
    pub(crate) fn builtin_for(&self, name: &str, p: Platform) -> Option<Vec<Binding>> {
        let mut b = self.base_defaults();
        b.extend(self.audit_for(p));
        let table: Vec<Entry> = match name {
            DEFAULT_PRESET => Vec::new(),
            PREMIERE_PRESET => presets::premiere(),
            FCP_PRESET => presets::FINAL_CUT.to_vec(),
            AVID_PRESET => presets::AVID.to_vec(),
            _ => return None,
        };
        self.apply_table(&mut b, &table);
        drop_control_collisions(&mut b, p);
        Some(b)
    }

    /// Apply a preset table: listed commands get exactly the listed keys, and a listed key is taken
    /// away from whatever else had it in that context.
    fn apply_table(&self, b: &mut Vec<Binding>, table: &[Entry]) {
        let listed: Vec<&str> = table.iter().map(|e| e.0).filter(|c| self.known(c)).collect();
        b.retain(|x| !listed.contains(&x.command.as_str()));
        for (c, k, p) in table {
            if !self.known(c) {
                continue;
            }
            let nb = Binding::new(c, k, panel_opt(p));
            b.retain(|o| !(o.context() == nb.context() && o.chord() == nb.chord()));
            b.push(nb);
        }
    }

    fn presets_dir(&self) -> Option<PathBuf> {
        self.dir.as_ref().map(|d| d.join(DIR_NAME))
    }

    /// Custom preset names (files in the presets directory).
    pub fn user_presets(&self) -> Vec<String> {
        let Some(dir) = self.presets_dir() else { return Vec::new() };
        let Ok(rd) = std::fs::read_dir(dir) else { return Vec::new() };
        let mut v: Vec<String> = rd
            .flatten()
            .filter(|e| e.file_name() != ACTIVE_FILE)
            .filter_map(|e| std::fs::read(e.path()).ok().and_then(|b| serde_json::from_slice::<PresetFile>(&b).ok()).map(|p| p.name))
            .filter(|n| !BUILTIN_PRESETS.contains(&n.as_str()))
            .collect();
        v.sort();
        v.dedup();
        v
    }

    fn preset_path(&self, name: &str) -> Option<PathBuf> {
        let safe: String = name.chars().map(|c| if c.is_alphanumeric() || " -_().".contains(c) { c } else { '_' }).collect();
        self.presets_dir().map(|d| d.join(format!("{}.json", safe.trim())))
    }

    /// Use the per-user data directory: loads the active set saved there (if any).
    pub fn set_dir(&mut self, data_dir: &Path) {
        self.dir = Some(data_dir.to_path_buf());
        let Some(path) = self.presets_dir().map(|d| d.join(ACTIVE_FILE)) else { return };
        let Ok(bytes) = std::fs::read(&path) else { return };
        match serde_json::from_slice::<PresetFile>(&bytes) {
            Ok(f) => {
                self.preset = f.name.clone();
                if !f.modified
                    && let Some(b) = self.builtin(&f.name)
                {
                    self.bindings = b;
                    self.modified = false;
                } else {
                    self.bindings = convert(f.bindings, f.platform);
                    self.modified = f.modified;
                }
                self.revision += 1;
            }
            Err(e) => log::warn!("{}: {e}", path.display()),
        }
    }

    fn file(&self, name: &str, modified: bool) -> PresetFile {
        PresetFile { format: FILE_FORMAT.into(), version: 1, name: name.into(), platform: None, modified, bindings: self.bindings.clone() }
    }

    fn persist(&self) {
        let Some(dir) = self.presets_dir() else { return };
        let f = self.file(&self.preset, self.modified);
        if let Err(e) = std::fs::create_dir_all(&dir)
            .and_then(|_| filmcraft_format::atomic_write(&dir.join(ACTIVE_FILE), &serde_json::to_vec_pretty(&f).unwrap_or_default()))
        {
            log::warn!("saving keyboard shortcuts: {e}");
        }
    }

    // ---------------------------------------------------------------- editing

    fn snapshot(&self) -> Snapshot {
        Snapshot { bindings: self.bindings.clone(), preset: self.preset.clone(), modified: self.modified }
    }

    fn restore(&mut self, s: Snapshot) {
        self.bindings = s.bindings;
        self.preset = s.preset;
        self.modified = s.modified;
    }

    fn changed(&mut self, before: Snapshot) {
        self.undo.push(before);
        if self.undo.len() > 500 {
            self.undo.remove(0);
        }
        self.redo.clear();
        self.revision += 1;
        self.persist();
    }

    /// Assign `keys` to `command`. Unless `add`, the command's other shortcuts in that context are
    /// replaced. Unless `keep_conflicts`, the key is taken away from any other command bound to it
    /// in the same context (returned as `reassigned`).
    pub fn set(&mut self, command: &str, keys: &str, panel: Option<&str>, add: bool, keep_conflicts: bool) -> std::result::Result<Vec<Binding>, String> {
        if !self.known(command) {
            return Err(format!("unknown command `{command}`"));
        }
        let panel = check_panel(panel)?;
        let chord = Chord::parse(keys)?;
        let nb = Binding { command: command.into(), keys: chord.canonical(), panel: panel.map(str::to_string) };
        let before = self.snapshot();
        if !add {
            self.bindings.retain(|b| !(b.command == command && b.context() == nb.context()));
        }
        let mut reassigned = Vec::new();
        if !keep_conflicts {
            let p = Platform::current();
            let eff = chord.effective(p);
            self.bindings.retain(|b| {
                let clash = b.command != command && b.context() == nb.context() && b.chord().is_some_and(|c| c.effective(p) == eff);
                if clash {
                    reassigned.push(b.clone());
                }
                !clash
            });
        }
        if !self.bindings.contains(&nb) {
            self.bindings.push(nb);
        }
        self.modified = true;
        self.changed(before);
        Ok(reassigned)
    }

    /// Remove shortcuts of `command` (one key, or all; in one context, or all).
    pub fn clear(&mut self, command: &str, keys: Option<&str>, panel: Option<&str>) -> std::result::Result<usize, String> {
        let chord = keys.map(Chord::parse).transpose()?;
        let panel = match panel {
            Some(p) => Some(check_panel(Some(p))?.map(str::to_string)),
            None => None,
        };
        let before = self.snapshot();
        let n0 = self.bindings.len();
        self.bindings.retain(|b| {
            let hit = b.command == command && chord.as_ref().is_none_or(|c| b.chord().as_ref() == Some(c)) && panel.as_ref().is_none_or(|p| &b.panel == p);
            !hit
        });
        let n = n0 - self.bindings.len();
        if n > 0 {
            self.modified = true;
            self.changed(before);
        }
        Ok(n)
    }

    pub fn undo(&mut self) -> bool {
        let Some(s) = self.undo.pop() else { return false };
        let cur = self.snapshot();
        self.redo.push(cur);
        self.restore(s);
        self.revision += 1;
        self.persist();
        true
    }

    pub fn redo(&mut self) -> bool {
        let Some(s) = self.redo.pop() else { return false };
        let cur = self.snapshot();
        self.undo.push(cur);
        self.restore(s);
        self.revision += 1;
        self.persist();
        true
    }

    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }
    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }

    /// The shortcut editor opened: remember where Cancel goes back to.
    pub fn begin_editing(&mut self) {
        self.session_start = Some(self.snapshot());
        self.undo.clear();
        self.redo.clear();
    }

    /// OK: keep the edits.
    pub fn end_editing(&mut self) {
        self.session_start = None;
    }

    /// Cancel: restore the set the editor opened with.
    pub fn cancel_editing(&mut self) {
        if let Some(s) = self.session_start.take() {
            self.restore(s);
            self.undo.clear();
            self.redo.clear();
            self.revision += 1;
            self.persist();
        }
    }

    pub fn load_preset(&mut self, name: &str) -> std::result::Result<(), String> {
        let bindings = match self.builtin(name) {
            Some(b) => b,
            None => {
                let path = self.preset_path(name).ok_or("no data directory for custom presets")?;
                let f = read_file(&path)?;
                convert(f.bindings, f.platform)
            }
        };
        let before = self.snapshot();
        self.bindings = bindings;
        self.preset = name.into();
        self.modified = false;
        self.changed(before);
        Ok(())
    }

    pub fn save_preset(&mut self, name: &str) -> std::result::Result<PathBuf, String> {
        let name = name.trim();
        if name.is_empty() {
            return Err("a preset needs a name".into());
        }
        if BUILTIN_PRESETS.iter().any(|b| b.eq_ignore_ascii_case(name)) {
            return Err(format!("`{name}` is a built-in preset; choose another name"));
        }
        let path = self.preset_path(name).ok_or("no data directory for custom presets")?;
        write_file(&path, &self.file(name, false))?;
        let before = self.snapshot();
        self.preset = name.into();
        self.modified = false;
        self.changed(before);
        Ok(path)
    }

    pub fn delete_preset(&mut self, name: &str) -> std::result::Result<(), String> {
        if BUILTIN_PRESETS.contains(&name) {
            return Err("built-in presets cannot be deleted".into());
        }
        let path = self.preset_path(name).ok_or("no data directory for custom presets")?;
        std::fs::remove_file(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        if self.preset == name {
            self.modified = true;
            self.revision += 1;
            self.persist();
        }
        Ok(())
    }

    pub fn export(&self, path: &Path) -> std::result::Result<(), String> {
        write_file(path, &self.file(&self.preset, false))
    }

    /// Import a preset file: saved among the custom presets (when there is a data directory) and,
    /// when `activate`, made the active set.
    pub fn import(&mut self, path: &Path, activate: bool) -> std::result::Result<String, String> {
        let f = read_file(path)?;
        let name = if f.name.trim().is_empty() || BUILTIN_PRESETS.contains(&f.name.as_str()) {
            path.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_else(|| "Imported".into())
        } else {
            f.name.clone()
        };
        let bindings: Vec<Binding> = convert(f.bindings, f.platform).into_iter().filter(|b| self.known(&b.command)).collect();
        if let Some(p) = self.preset_path(&name) {
            let file = PresetFile { format: FILE_FORMAT.into(), version: 1, name: name.clone(), platform: None, modified: false, bindings: bindings.clone() };
            write_file(&p, &file)?;
        }
        if activate {
            let before = self.snapshot();
            self.bindings = bindings;
            self.preset = name.clone();
            self.modified = false;
            self.changed(before);
        }
        Ok(name)
    }

    // ---------------------------------------------------------------- queries

    pub fn for_command(&self, id: &str) -> Vec<&Binding> {
        self.bindings.iter().filter(|b| b.command == id).collect()
    }

    /// The shortcut menus show: the first application-wide one.
    pub fn primary(&self, id: &str) -> Option<String> {
        self.bindings.iter().find(|b| b.command == id && b.panel.is_none()).map(|b| b.keys.clone())
    }

    /// The command a chord runs with `panel` focused (panel shortcuts first).
    pub fn resolve(&self, chord: &Chord, panel: Option<&str>, p: Platform) -> Option<&Binding> {
        let eff = chord.effective(p);
        let m = |b: &&Binding| b.chord().is_some_and(|c| c.effective(p) == eff);
        if let Some(pn) = panel
            && let Some(b) = self.bindings.iter().filter(|b| b.panel.as_deref() == Some(pn)).find(m)
        {
            return Some(b);
        }
        self.bindings.iter().filter(|b| b.panel.is_none()).find(m)
    }

    /// Groups of bindings that share a key in one context on platform `p`.
    pub fn conflicts(&self, p: Platform) -> Vec<(String, String, Vec<String>)> {
        let mut map: BTreeMap<(String, String), Vec<String>> = BTreeMap::new();
        for b in &self.bindings {
            let Some(c) = b.chord() else { continue };
            let e = map.entry((b.context().to_string(), c.effective(p).canonical())).or_default();
            if !e.contains(&b.command) {
                e.push(b.command.clone());
            }
        }
        map.into_iter().filter(|(_, v)| v.len() > 1).map(|((ctx, k), v)| (ctx, k, v)).collect()
    }

    /// Panel shortcuts that shadow an application shortcut while their panel has focus.
    pub fn overrides(&self, p: Platform) -> Vec<(String, String, String, String)> {
        let mut out = Vec::new();
        for b in self.bindings.iter().filter(|b| b.panel.is_some()) {
            let Some(c) = b.chord() else { continue };
            if let Some(a) = self.resolve(&c, None, p)
                && a.command != b.command
            {
                out.push((b.panel.clone().unwrap_or_default(), b.keys.clone(), b.command.clone(), a.command.clone()));
            }
        }
        out
    }

    /// Bindings on one key (any modifiers): the shortcut editor's "Key:" list.
    pub fn for_key(&self, key: &str) -> Vec<&Binding> {
        let Some(k) = canon_key(key) else { return Vec::new() };
        let mut v: Vec<&Binding> = self.bindings.iter().filter(|b| b.chord().is_some_and(|c| c.key == k)).collect();
        v.sort_by_key(|b| b.chord().map(|c| (c.mods.count(), c.mods)));
        v
    }

    /// Commands assigned with exactly `mods` held, per key: (application command, panel command).
    pub fn keyboard(&self, mods: Mods, panel: Option<&str>) -> BTreeMap<&'static str, (Option<String>, Option<String>)> {
        let mut m: BTreeMap<&'static str, (Option<String>, Option<String>)> = BTreeMap::new();
        for b in &self.bindings {
            let Some(c) = b.chord() else { continue };
            if c.mods != mods {
                continue;
            }
            let e = m.entry(c.key).or_default();
            match (&b.panel, panel) {
                (None, _) => {
                    e.0.get_or_insert(b.command.clone());
                }
                (Some(bp), Some(p)) if bp == p => {
                    e.1.get_or_insert(b.command.clone());
                }
                (Some(_), None) => {
                    e.1.get_or_insert(b.command.clone());
                }
                _ => {}
            }
        }
        m
    }

    fn binding_json(&self, b: &Binding, p: Platform) -> Value {
        json!({"keys": b.keys, "panel": b.panel, "display": b.chord().map(|c| c.display(p)).unwrap_or_else(|| b.keys.clone())})
    }
}

fn panel_opt(p: &str) -> Option<&str> {
    if p.is_empty() { None } else { Some(p) }
}

fn check_panel(panel: Option<&str>) -> std::result::Result<Option<&'static str>, String> {
    match panel {
        None => Ok(None),
        Some(p) if p.is_empty() || p.eq_ignore_ascii_case(APPLICATION) => Ok(None),
        Some(p) => {
            let n = p.to_ascii_lowercase().replace([' ', '_', '-'], "");
            let alias = match n.as_str() {
                "programmonitor" => "program",
                "sourcemonitor" => "source",
                "projects" => "project",
                "timelines" => "timeline",
                _ => n.as_str(),
            };
            PANELS
                .iter()
                .copied()
                .find(|x| x.to_ascii_lowercase().replace(' ', "") == alias)
                .map(Some)
                .ok_or_else(|| format!("unknown panel `{p}` (one of {})", PANELS.join(", ")))
        }
    }
}

/// Normalise bindings read from a file written for `platform`.
fn convert(bindings: Vec<Binding>, platform: Option<Platform>) -> Vec<Binding> {
    let p = platform.unwrap_or(Platform::Mac);
    bindings
        .into_iter()
        .filter_map(|b| {
            let c = Chord::parse_for(&b.keys, p).ok()?;
            let panel = check_panel(b.panel.as_deref()).ok()?.map(str::to_string);
            Some(Binding { command: b.command, keys: c.canonical(), panel })
        })
        .collect()
}

fn read_file(path: &Path) -> std::result::Result<PresetFile, String> {
    let bytes = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let f: PresetFile = serde_json::from_slice(&bytes).map_err(|e| format!("{}: {e}", path.display()))?;
    if f.format != FILE_FORMAT {
        return Err(format!("{}: not a FilmCraft keyboard shortcuts file", path.display()));
    }
    Ok(f)
}

fn write_file(path: &Path, f: &PresetFile) -> std::result::Result<(), String> {
    if let Some(d) = path.parent() {
        std::fs::create_dir_all(d).map_err(|e| format!("{}: {e}", d.display()))?;
    }
    filmcraft_format::atomic_write(path, &serde_json::to_vec_pretty(f).unwrap_or_default()).map_err(|e| format!("{}: {e}", path.display()))
}

// ------------------------------------------------------------------ commands

fn platform_p(p: &Value) -> Platform {
    str_p(p, "platform").and_then(Platform::from_name).unwrap_or_else(Platform::current)
}

fn err(cmd: &str) -> impl Fn(String) -> EngineError + '_ {
    move |m| bad(cmd, m)
}

fn presets_json(s: &Session) -> Value {
    let sc = &s.shortcuts;
    json!({"active": sc.preset, "modified": sc.modified, "builtin": BUILTIN_PRESETS, "custom": sc.user_presets(), "canUndo": sc.can_undo(), "canRedo": sc.can_redo()})
}

fn list(s: &Session, p: &Value) -> Value {
    let sc = &s.shortcuts;
    let plat = platform_p(p);
    let q = str_p(p, "query").map(str::to_ascii_lowercase);
    let assigned_only = bool_p(p, "assigned").unwrap_or(false);
    let panel = str_p(p, "panel").and_then(|x| check_panel(Some(x)).ok().flatten());
    let rows: Vec<Value> = sc
        .commands()
        .into_iter()
        .filter_map(|c| {
            let bs: Vec<&Binding> = sc.for_command(&c.id).into_iter().filter(|b| panel.is_none() || b.panel.as_deref() == panel).collect();
            if assigned_only && bs.is_empty() {
                return None;
            }
            if let Some(q) = &q {
                let hit = c.label.to_ascii_lowercase().contains(q.as_str())
                    || c.id.to_ascii_lowercase().contains(q.as_str())
                    || bs.iter().any(|b| b.keys.to_ascii_lowercase().contains(q.as_str()) || b.chord().is_some_and(|ch| ch.display(plat).to_ascii_lowercase().contains(q.as_str())));
                if !hit {
                    return None;
                }
            }
            Some(json!({"id": c.id, "label": c.label, "category": c.category, "menu": c.menu, "shortcuts": bs.iter().map(|b| sc.binding_json(b, plat)).collect::<Vec<_>>()}))
        })
        .collect();
    json!(rows)
}

pub fn commands() -> Vec<CommandSpec> {
    macro_rules! sc {
        ($id:literal, $label:literal, $params:literal, $journal:expr, $run:expr) => {
            CommandSpec { id: $id, label: $label, menu: &[], shortcut: None, params: $params, enabled: always, run: $run, journal: $journal }
        };
    }
    vec![
        sc!("shortcuts.list", "List Keyboard Shortcuts", r#"{"query":str?,"panel":str?,"assigned":bool?,"platform":"mac|windows|linux"?}"#, false, |s, p| Ok(
            list(s, p)
        )),
        sc!("shortcuts.get", "Get Shortcuts of a Command", r#"{"command":id,"platform":str?}"#, false, |s, p| {
            let id = str_p(p, "command").ok_or_else(|| bad("shortcuts.get", "need `command`"))?;
            let c = s.shortcuts.command(id).ok_or_else(|| bad("shortcuts.get", format!("unknown command `{id}`")))?;
            let plat = platform_p(p);
            let bs: Vec<Value> = s.shortcuts.for_command(id).into_iter().map(|b| s.shortcuts.binding_json(b, plat)).collect();
            Ok(json!({"id": c.id, "label": c.label, "category": c.category, "shortcuts": bs}))
        }),
        sc!("shortcuts.set", "Assign Shortcut", r#"{"command":id,"keys":"Cmd+Shift+K","panel":str?,"add":bool?,"keepConflicts":bool?}"#, true, |s, p| {
            let id = str_p(p, "command").ok_or_else(|| bad("shortcuts.set", "need `command`"))?;
            let keys = str_p(p, "keys").ok_or_else(|| bad("shortcuts.set", "need `keys`"))?;
            let r = s
                .shortcuts
                .set(id, keys, str_p(p, "panel"), bool_p(p, "add").unwrap_or(false), bool_p(p, "keepConflicts").unwrap_or(false))
                .map_err(err("shortcuts.set"))?;
            let plat = Platform::current();
            let conflicts: Vec<Value> = s.shortcuts.conflicts(plat).into_iter().map(|(c, k, v)| json!({"context": c, "keys": k, "commands": v})).collect();
            Ok(json!({"keys": normalize(keys).unwrap_or_default(), "reassigned": r, "conflicts": conflicts}))
        }),
        sc!("shortcuts.clear", "Clear Shortcut", r#"{"command":id,"keys":str?,"panel":str?}"#, true, |s, p| {
            let id = str_p(p, "command").ok_or_else(|| bad("shortcuts.clear", "need `command`"))?;
            let n = s.shortcuts.clear(id, str_p(p, "keys"), str_p(p, "panel")).map_err(err("shortcuts.clear"))?;
            Ok(json!({"removed": n}))
        }),
        sc!("shortcuts.undo", "Undo Shortcut Change", "{}", true, |s, _| Ok(json!({"undone": s.shortcuts.undo()}))),
        sc!("shortcuts.redo", "Redo Shortcut Change", "{}", true, |s, _| Ok(json!({"redone": s.shortcuts.redo()}))),
        sc!("shortcuts.conflicts", "Shortcut Conflicts", r#"{"platform":"mac|windows|linux"?}"#, false, |s, p| {
            let plat = platform_p(p);
            let c: Vec<Value> = s.shortcuts.conflicts(plat).into_iter().map(|(c, k, v)| json!({"context": c, "keys": k, "commands": v})).collect();
            let o: Vec<Value> =
                s.shortcuts.overrides(plat).into_iter().map(|(pn, k, cmd, app)| json!({"panel": pn, "keys": k, "command": cmd, "overrides": app})).collect();
            Ok(json!({"conflicts": c, "panelOverrides": o}))
        }),
        sc!("shortcuts.forKey", "Shortcuts on a Key", r#"{"key":"K","platform":str?}"#, false, |s, p| {
            let k = str_p(p, "key").ok_or_else(|| bad("shortcuts.forKey", "need `key`"))?;
            let plat = platform_p(p);
            Ok(json!(
                s.shortcuts
                    .for_key(k)
                    .into_iter()
                    .map(|b| json!({"keys": b.keys, "display": b.chord().map(|c| c.display(plat)), "command": b.command, "panel": b.panel}))
                    .collect::<Vec<_>>()
            ))
        }),
        sc!("shortcuts.resolve", "Resolve Shortcut", r#"{"keys":str,"panel":str?,"platform":str?}"#, false, |s, p| {
            let keys = str_p(p, "keys").ok_or_else(|| bad("shortcuts.resolve", "need `keys`"))?;
            let c = Chord::parse(keys).map_err(err("shortcuts.resolve"))?;
            let panel = check_panel(str_p(p, "panel")).map_err(err("shortcuts.resolve"))?;
            Ok(s.shortcuts.resolve(&c, panel, platform_p(p)).map(|b| json!({"command": b.command, "panel": b.panel})).unwrap_or(Value::Null))
        }),
        sc!("shortcuts.presets", "Keyboard Shortcut Presets", "{}", false, |s, _| Ok(presets_json(s))),
        sc!("shortcuts.loadPreset", "Load Shortcut Preset", r#"{"name":str}"#, true, |s, p| {
            let n = str_p(p, "name").ok_or_else(|| bad("shortcuts.loadPreset", "need `name`"))?;
            s.shortcuts.load_preset(n).map_err(err("shortcuts.loadPreset"))?;
            Ok(presets_json(s))
        }),
        sc!("shortcuts.savePreset", "Save Shortcut Preset As", r#"{"name":str}"#, true, |s, p| {
            let n = str_p(p, "name").ok_or_else(|| bad("shortcuts.savePreset", "need `name`"))?;
            let path = s.shortcuts.save_preset(n).map_err(err("shortcuts.savePreset"))?;
            Ok(json!({"path": path, "presets": presets_json(s)}))
        }),
        sc!("shortcuts.deletePreset", "Delete Shortcut Preset", r#"{"name":str}"#, true, |s, p| {
            let n = str_p(p, "name").ok_or_else(|| bad("shortcuts.deletePreset", "need `name`"))?;
            s.shortcuts.delete_preset(n).map_err(err("shortcuts.deletePreset"))?;
            Ok(presets_json(s))
        }),
        sc!("shortcuts.export", "Export Keyboard Shortcuts", r#"{"path":str}"#, true, |s, p| {
            let path = str_p(p, "path").ok_or_else(|| bad("shortcuts.export", "need `path`"))?;
            s.shortcuts.export(Path::new(path)).map_err(err("shortcuts.export"))?;
            Ok(json!({"path": path, "bindings": s.shortcuts.bindings.len()}))
        }),
        sc!("shortcuts.import", "Import Keyboard Shortcuts", r#"{"path":str,"activate":bool=true}"#, true, |s, p| {
            let path = str_p(p, "path").ok_or_else(|| bad("shortcuts.import", "need `path`"))?;
            let name = s.shortcuts.import(Path::new(path), bool_p(p, "activate").unwrap_or(true)).map_err(err("shortcuts.import"))?;
            Ok(json!({"name": name, "presets": presets_json(s)}))
        }),
        sc!("shortcuts.audit", "Shortcut Audit", "{}", false, |s, _| {
            let a = s.shortcuts.audit();
            Ok(json!({"count": a.len(), "assigned": a}))
        }),
    ]
}
