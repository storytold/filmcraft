//! Click every button: a crawler that visits every place of FilmCraft an agent can reach, lists the
//! automation elements drawn there, clicks each one on a fresh copy of the demo project and records
//! what the click did. The full crawl writes the map agents use to press any button
//! (`docs/ui-map.json`, served by the MCP tool `ui_map`) and its summary (`docs/ui-map.md`).
//!
//! - `every_visible_element_and_menu_command_runs_without_a_panic` (normal runs): every element
//!   visible at start, in Import and Export mode and in each panel, clicked once on a fresh app, and
//!   every menu command invoked once. Any panic fails the test and names the element.
//! - `crawl_writes_the_ui_map` (`--ignored`): the same places, plus every panel maximized, each with
//!   a video clip, an audio clip or a graphic selected, a few prepared states (audio effects with
//!   editors, a mask, mixer inserts and sends, a transcript, a caption track, a bin's items in each
//!   Project view), every dialog a menu command opens (with a selection when it needs one),
//!   right-click context menus, and what each click reveals (popups, dialogs, tabs) up to three
//!   clicks deep:
//!   `cargo test -p filmcraft-ui-egui --test ui_map_crawl -- --ignored`.
//! - `the_ui_map_is_well_formed`: the committed map parses, is sorted and complete.
//!
//! Each click runs on a fresh app (the demo project, the Editing workspace, a 1920×1200 window), so
//! nothing a previous click did leaks into the next. Apps leak their frame worker threads when they
//! are dropped, so the jobs run in short-lived child processes of this test binary (the hidden
//! test `crawl_worker`), a batch each; a process that dies is reported and the rest of its batch
//! re-run. The apps use sandboxed host services: no file of the user's is read, listed or written
//! (exports are encoded in memory and dropped, the Media Browser sees a fake folder tree), and the
//! children get a scratch `FILMCRAFT_DATA_DIR`, so no speech model or preference is touched.

use std::collections::{BTreeMap, BTreeSet};
use std::io::Write as _;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{Sender, channel};

use egui_kittest::Harness;
use egui_kittest::kittest::NodeT;
use filmcraft_engine::media_browser::{DirEntry, Volume, VolumeKind};
use filmcraft_engine::{Services, Session};
use filmcraft_ui_egui::FilmcraftApp;
use filmcraft_ui_egui::control::ControlRequest;
use filmcraft_ui_egui::dock::PanelKind;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

/// The window the crawl runs in (agents' windows vary; elements keep their ids).
const WINDOW: egui::Vec2 = egui::vec2(1920.0, 1200.0);
/// Frames run after each step for the UI to settle.
const SETTLE: usize = 3;
/// Height of the header bar (mode switcher, workspaces, search).
const HEADER_H: f32 = 38.0;
/// Jobs per child process.
const BATCH: usize = 40;
/// How the map is regenerated (also written into it).
const REGENERATE: &str = "cargo test -p filmcraft-ui-egui --test ui_map_crawl -- --ignored";

/// Elements and menu commands the crawl does not press, and why.
const DENY: &[(&str, &str)] = &[
    ("text.transcribe.ok", "starts speech-to-text, which may download a model"),
    ("transcript.generate", "starts speech-to-text, which may download a model"),
    ("transcribe.ok", "starts speech-to-text, which may download a model"),
    ("help.revealLogFiles", "writes the session log and reveals it in the file manager"),
];

fn denied(id: &str) -> Option<&'static str> {
    if id.to_ascii_lowercase().contains("download") {
        return Some("downloads over the network");
    }
    DENY.iter().find(|(d, _)| *d == id).map(|(_, why)| *why)
}

// ------------------------------------------------------------------ sandbox

/// Host services that never touch the user's files: nothing is read, writes are counted and
/// dropped, exports are encoded in memory (and handed to `write_file`), and the Media Browser
/// browses a small fake tree.
#[derive(Default)]
struct Sandbox {
    writes: AtomicUsize,
}

impl Services for Sandbox {
    fn read_file(&self, path: &str) -> std::io::Result<Vec<u8>> {
        Err(std::io::Error::new(std::io::ErrorKind::NotFound, format!("sandbox: {path}")))
    }
    fn write_file(&self, _path: &str, _data: &[u8]) -> std::io::Result<()> {
        self.writes.fetch_add(1, Ordering::Relaxed);
        Ok(())
    }
    fn file_size(&self, path: &str) -> std::io::Result<u64> {
        Err(std::io::Error::new(std::io::ErrorKind::NotFound, format!("sandbox: {path}")))
    }
    fn list_dir(&self, dir: &str) -> Option<std::io::Result<Vec<String>>> {
        Some(Ok(fake_dir(dir).into_iter().map(|e| e.name).collect()))
    }
    fn list_entries(&self, dir: &str) -> Option<std::io::Result<Vec<DirEntry>>> {
        Some(Ok(fake_dir(dir)))
    }
    fn volumes(&self) -> Vec<Volume> {
        vec![Volume { name: "Sandbox".into(), path: "/Sandbox".into(), kind: VolumeKind::Local }]
    }
    fn home_dir(&self) -> Option<String> {
        Some("/Sandbox/Home".into())
    }
    fn export_in_memory(&self) -> bool {
        true
    }
}

fn fake_dir(dir: &str) -> Vec<DirEntry> {
    let d = |name: &str| DirEntry { name: name.into(), is_dir: true, size: None, modified: Some(1_700_000_000) };
    let f = |name: &str, size: u64| DirEntry { name: name.into(), is_dir: false, size: Some(size), modified: Some(1_700_000_000) };
    match dir.trim_end_matches(['/', '\\']) {
        "/Sandbox" => vec![d("Home")],
        "/Sandbox/Home" => vec![d("Footage"), d("Music"), f("notes.txt", 12)],
        "/Sandbox/Home/Footage" => vec![f("interview.mp4", 1_000_000), f("broll.mov", 2_000_000), f("logo.png", 10_000)],
        "/Sandbox/Home/Music" => vec![f("theme.wav", 500_000)],
        _ => Vec::new(),
    }
}

// ------------------------------------------------------------------ driver

struct Driver {
    harness: Harness<'static, FilmcraftApp>,
    tx: Sender<ControlRequest>,
    sandbox: Arc<Sandbox>,
}

impl Driver {
    fn new() -> Self {
        filmcraft_ui_egui::crash::install(None);
        let sandbox = Arc::new(Sandbox::default());
        let mut s = Session::new(sandbox.clone());
        s.execute("file.openDemoProject", json!({})).expect("demo project");
        let (tx, rx) = channel();
        let mut app = FilmcraftApp::new(s).with_control(rx);
        app.hooks.open_path = Some(Box::new(|_, _| Ok(())));
        let harness = Harness::builder().with_size(WINDOW).with_max_steps(10_000).build_eframe(move |_cc| app);
        let mut d = Driver { harness, tx, sandbox };
        d.frames(4);
        d
    }

    fn frames(&mut self, n: usize) {
        for _ in 0..n {
            // the control channel's clicks and keys enter through the input hook
            let ctx = self.harness.ctx.clone();
            let mut raw = std::mem::take(self.harness.input_mut());
            eframe::App::raw_input_hook(self.harness.state_mut(), &ctx, &mut raw);
            *self.harness.input_mut() = raw;
            self.harness.step();
        }
    }

    fn call(&mut self, method: &str, params: Value) -> Value {
        let (req, reply) = ControlRequest::new(method, params);
        if self.tx.send(req).is_err() {
            return json!({"ok": false, "error": "the control channel is closed"});
        }
        for _ in 0..400 {
            self.frames(1);
            if let Ok(v) = reply.try_recv() {
                return v;
            }
        }
        json!({"ok": false, "error": format!("no reply to {method}")})
    }

    /// A few frames, then until the timeline stops zooming (a while at most: playback scrolls it).
    fn settle(&mut self) {
        self.frames(SETTLE);
        for _ in 0..40 {
            if !self.app().ui.timeline.animating() {
                break;
            }
            self.frames(1);
        }
    }

    fn app(&self) -> &FilmcraftApp {
        self.harness.state()
    }

    fn ids(&self) -> BTreeSet<String> {
        self.app().auto.query("").into_iter().map(|e| e.id.clone()).collect()
    }
}

// ------------------------------------------------------------------ jobs

/// One step from a fresh start towards an element.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
enum Step {
    /// `ui.set {mode}`.
    Mode(String),
    /// `ui.panel.show {panel}`.
    Panel(String),
    /// `ui.set <params>` (e.g. the focused panel, the Project panel's bin).
    Set(Value),
    /// `ui.menu.invoke {id}` (a menu-bar command).
    Menu(String),
    /// `engine.execute {command, params}`; `shown` is the params as the map writes them.
    Exec { command: String, params: Value, shown: String },
    /// `ui.click {id, button}`; `pattern` is the id as the map writes it.
    Click { id: String, pattern: String, right: bool },
}

impl Step {
    /// As the map's `reach` writes it.
    fn describe(&self) -> String {
        match self {
            Step::Mode(m) => format!("ui.set mode={m}"),
            Step::Panel(p) => format!("ui.panel.show {p}"),
            Step::Set(v) => format!("ui.set {v}"),
            Step::Menu(id) => format!("ui.menu.invoke {id}"),
            Step::Exec { command, shown, .. } => format!("command_run {command} {shown}"),
            Step::Click { pattern, right: false, .. } => format!("ui.click {pattern}"),
            Step::Click { pattern, right: true, .. } => format!("ui.click {pattern} button=right"),
        }
    }

    fn run(&self, d: &mut Driver) -> Result<(), String> {
        let r = match self {
            Step::Mode(m) => d.call("ui.set", json!({"mode": m})),
            Step::Panel(p) => d.call("ui.panel.show", json!({"panel": p})),
            Step::Set(v) => d.call("ui.set", v.clone()),
            Step::Menu(id) => d.call("ui.menu.invoke", json!({"id": id})),
            Step::Exec { command, params, .. } => d.call("engine.execute", json!({"command": command, "params": params})),
            Step::Click { id, right, .. } => d.call("ui.click", json!({"id": id, "button": if *right { "right" } else { "left" }})),
        };
        d.settle();
        match r["ok"] == json!(true) {
            true => Ok(()),
            false => Err(r["error"].as_str().unwrap_or("failed").to_string()),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
enum Action {
    /// List what is on screen.
    Look,
    /// Click an element (`right`: the secondary button, for context menus).
    Click { id: String, right: bool },
    /// Invoke a menu-bar command.
    Menu(String),
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct Job {
    reach: Vec<Step>,
    action: Action,
}

impl Job {
    fn describe(&self) -> String {
        let mut v: Vec<String> = self.reach.iter().map(Step::describe).collect();
        v.push(match &self.action {
            Action::Look => "look".into(),
            Action::Click { id, right: false } => format!("ui.click {id}"),
            Action::Click { id, right: true } => format!("ui.click {id} button=right"),
            Action::Menu(id) => format!("ui.menu.invoke {id}"),
        });
        v.join(" → ")
    }
}

/// An element on screen.
#[derive(Clone, Debug, Serialize, Deserialize)]
struct Found {
    id: String,
    label: String,
    rect: [f32; 4],
    /// `Header`, `Dock`, `panel:<title>`, `mode:<Import|Export>`, `Dialog`, `Status bar`, `Window`.
    region: String,
    dialog: bool,
    /// Drawn in a popup (a menu or a dropdown's list) above everything else.
    floating: bool,
    /// Its centre is where a click lands on it (not scrolled out of its panel or dialog).
    visible: bool,
    /// The AccessKit role of the widget at exactly its rect, when egui reports one.
    role: Option<String>,
}

/// What a click (or menu command) did.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
struct Effect {
    text: String,
    /// Nothing observable happened.
    dead: bool,
    /// Commands it ran (the session's journal).
    commands: Vec<String>,
    /// Commands it tried that failed or were disabled (the event log).
    failed: Vec<String>,
    /// `dialog`, `menu` (a popup) or `view` (more elements in place, e.g. a tab).
    opened: Option<String>,
    dialog: Option<String>,
    /// How to get rid of what it opened: `ui.key Escape` or `ui.click <id>`.
    close: Option<String>,
    focused: bool,
    toggled: bool,
    /// The tool / mode it switched to.
    tool: Option<String>,
    mode: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
struct Outcome {
    reach_error: Option<String>,
    /// `Look`: every element; actions: the elements that appeared.
    found: Vec<Found>,
    effect: Option<Effect>,
    panic: Option<String>,
}

// ------------------------------------------------------------------ snapshots

struct Snap {
    els: Vec<Found>,
    ids: BTreeSet<String>,
    ui: Value,
    dialog: Option<String>,
    modal: bool,
    journal: Vec<String>,
    /// (id, count, level, source, message) of the event log's newest entries.
    log: Vec<(u64, u32, String, String, String)>,
    undo: Vec<String>,
    redo: usize,
    playhead: i64,
    playing: bool,
    selection: Vec<u64>,
    active: Option<u64>,
    status: String,
    focused: bool,
    popup: bool,
    jobs: Vec<u64>,
    writes: usize,
}

fn rect_of(r: [f32; 4]) -> egui::Rect {
    egui::Rect::from_min_size(egui::pos2(r[0], r[1]), egui::vec2(r[2], r[3]))
}

/// `panel.<Kind>`: the content rect a panel registers before drawing into it.
fn panel_marker(id: &str) -> Option<PanelKind> {
    let k = id.strip_prefix("panel.")?;
    PanelKind::ALL.into_iter().find(|p| p.id() == k)
}

/// A panel's tab, menu button or tab-list entry (drawn in the dock chrome, above its content).
fn chrome_panel(id: &str) -> Option<PanelKind> {
    let k = id.strip_prefix("panel.tab.").or_else(|| id.strip_prefix("panel.menu.")).or_else(|| id.strip_prefix("panel.tabs.list."))?;
    PanelKind::ALL.into_iter().find(|p| p.id() == k)
}

fn capture(d: &Driver) -> Snap {
    let app = d.app();
    let ctx = &d.harness.ctx;
    let all: Vec<filmcraft_ui_egui::automation::Element> = app.auto.query("").into_iter().cloned().collect();
    let from = all.len().saturating_sub(app.auto.dialog_elements().len());
    let panels: Vec<(String, egui::Rect)> = all.iter().filter_map(|e| panel_marker(&e.id).map(|p| (p.title().to_string(), rect_of(e.rect)))).collect();
    let win = egui::Rect::from_min_size(egui::Pos2::ZERO, WINDOW);
    let modal = ctx.memory(|m| m.top_modal_layer()).and_then(|l| ctx.memory(|m| m.area_rect(l.id)));
    let mode = format!("{:?}", app.ui.mode);
    let mut current: Option<(String, egui::Rect)> = None;
    let mut els = Vec::with_capacity(all.len());
    for (i, e) in all.iter().enumerate() {
        let r = rect_of(e.rect);
        let c = r.center();
        let order = ctx.layer_id_at(c).map(|l| l.order);
        let popup_layer = matches!(order, Some(egui::Order::Foreground | egui::Order::Tooltip | egui::Order::Debug));
        let inside = win.contains(c);
        let (region, visible, dialog) = if i >= from {
            if e.id.starts_with("status.") {
                ("Status bar".to_string(), inside, false)
            } else {
                let shown = match modal {
                    Some(m) => m.contains(c),
                    None => order.is_some_and(|o| o != egui::Order::Background),
                };
                ("Dialog".to_string(), inside && shown, true)
            }
        } else if let Some(p) = panel_marker(&e.id) {
            current = Some((p.title().to_string(), r));
            (format!("panel:{}", p.title()), inside, false)
        } else if let Some(p) = chrome_panel(&e.id) {
            (format!("panel:{}", p.title()), inside, false)
        } else if current.as_ref().is_some_and(|(_, pr)| !pr.contains(c)) && order == Some(egui::Order::Middle) {
            // outside the panel drawn last, in a window of its own: a bin or panel floated out of the dock
            ("Floating window".to_string(), inside, false)
        } else if let Some((t, pr)) = &current {
            (format!("panel:{t}"), inside && (pr.contains(c) || popup_layer), false)
        } else if c.y < HEADER_H {
            ("Header".to_string(), inside, false)
        } else if e.id.starts_with("dock.") || e.id.contains("tabs.") {
            ("Dock".to_string(), inside, false)
        } else if let Some((t, _)) = panels.iter().find(|(_, pr)| pr.min.x <= c.x && c.x <= pr.max.x && c.y < pr.min.y && c.y > pr.min.y - 40.0) {
            // a tab strip above a panel (the Timeline's sequence tabs)
            (format!("panel:{t}"), inside, false)
        } else if mode != "Edit" {
            (format!("mode:{mode}"), inside, false)
        } else {
            ("Window".to_string(), inside, false)
        };
        let visible = visible && modal.is_none_or(|m| dialog || m.contains(c));
        els.push(Found { id: e.id.clone(), label: e.label.clone(), rect: e.rect, region, dialog, floating: popup_layer, visible, role: None });
    }
    let ids = els.iter().map(|f| f.id.clone()).collect();
    let mut ui = serde_json::to_value(&app.ui).unwrap_or_default();
    if let Some(o) = ui.as_object_mut() {
        o.remove("status");
    }
    let s = &app.session;
    let log = s.log.entries.iter().rev().take(30).map(|e| (e.id, e.count, format!("{:?}", e.level), e.source.clone(), e.message.clone())).collect();
    Snap {
        els,
        ids,
        ui,
        dialog: app.dialog.map(|d| format!("{d:?}")),
        modal: modal.is_some(),
        journal: s.journal.iter().map(|(id, _)| id.clone()).collect(),
        log,
        undo: s.history.undo.iter().map(|(l, _)| l.clone()).collect(),
        redo: s.history.redo.len(),
        playhead: s.playhead().0,
        playing: app.playback.playing,
        selection: s.state.selection.iter().map(|c| c.0).collect(),
        active: s.state.active_sequence.map(|i| i.0),
        status: app.ui.status.clone(),
        focused: ctx.memory(|m| m.focused().is_some()),
        popup: egui::Popup::is_any_open(ctx),
        jobs: s.jobs.iter().filter(|j| !j.progress.finished.load(Ordering::Relaxed)).map(|j| j.id).collect(),
        writes: d.sandbox.writes.load(Ordering::Relaxed),
    }
}

/// Roles AccessKit reports for widgets drawn at exactly these elements' rects.
fn add_roles(d: &Driver, found: &mut [Found]) {
    let nodes: Vec<(egui::Rect, String)> = d
        .harness
        .root()
        .children_recursive()
        .filter_map(|n| {
            let ak = n.accesskit_node();
            let b = ak.bounding_box()?;
            let role = format!("{:?}", ak.role());
            let generic = matches!(role.as_str(), "Unknown" | "GenericContainer" | "Window" | "ScrollView" | "Pane" | "Group" | "Image" | "Label");
            (!generic).then(|| (egui::Rect::from_min_max(egui::pos2(b.x0 as f32, b.y0 as f32), egui::pos2(b.x1 as f32, b.y1 as f32)), role))
        })
        .collect();
    for f in found {
        let r = rect_of(f.rect);
        let near = |a: egui::Rect| (a.min - r.min).length() < 1.5 && (a.max - r.max).length() < 1.5;
        f.role = nodes.iter().find(|(a, _)| near(*a)).map(|(_, role)| role.clone());
    }
}

/// Paths (to depth 3 below `path`) where two JSON values differ.
fn diff_paths(a: &Value, b: &Value, path: &str, depth: usize, out: &mut Vec<String>) {
    if a == b {
        return;
    }
    match (a, b) {
        // the dock tree changes as a whole (a panel opened, moved or closed)
        (Value::Object(x), Value::Object(y)) if depth < 3 && path != "ui.dock" => {
            let keys: BTreeSet<&String> = x.keys().chain(y.keys()).collect();
            for k in keys {
                diff_paths(x.get(k).unwrap_or(&Value::Null), y.get(k).unwrap_or(&Value::Null), &format!("{path}.{k}"), depth + 1, out);
            }
        }
        _ => out.push(path.to_string()),
    }
}

fn pointer<'a>(v: &'a Value, path: &str) -> Option<&'a Value> {
    v.pointer(&path.trim_start_matches("ui").replace('.', "/"))
}

/// The first id segment the elements share most, e.g. `addTracks` for an Add Tracks dialog.
fn common_prefix(ids: &[&str]) -> Option<String> {
    let mut c: BTreeMap<&str, usize> = BTreeMap::new();
    for id in ids {
        *c.entry(id.split('.').next().unwrap_or(id)).or_default() += 1;
    }
    c.into_iter().max_by_key(|(k, n)| (*n, std::cmp::Reverse(*k))).map(|(k, _)| k.to_string())
}

/// `s` without this machine's paths (the home directory, the scratch and temporary directories):
/// the map is committed and must not depend on (or reveal) who ran the crawl.
fn scrub(s: &str) -> String {
    let mut out = s.to_string();
    let tmp = std::env::temp_dir().to_string_lossy().trim_end_matches(['/', '\\']).to_string();
    let mut dirs: Vec<(String, &str)> = vec![(env!("CARGO_TARGET_TMPDIR").to_string(), "<tmp>"), (tmp, "<tmp>")];
    for k in ["HOME", "USERPROFILE"] {
        if let Some(h) = std::env::var_os(k).map(|h| h.to_string_lossy().to_string()).filter(|h| h.len() > 1) {
            dirs.push((h, "~"));
        }
    }
    dirs.sort_by_key(|(d, _)| std::cmp::Reverse(d.len()));
    for (d, to) in dirs {
        if !d.is_empty() {
            out = out.replace(&d, to);
        }
    }
    out
}

fn clip_text(s: &str, max: usize) -> String {
    let s: String = s.split_whitespace().collect::<Vec<_>>().join(" ");
    match s.char_indices().nth(max) {
        Some((i, _)) => format!("{}…", &s[..i]),
        None => s,
    }
}

/// What happened between `b` (just before the action) and `c` (after it settled); `a` → `b` is
/// the same wait without an action: whatever changes there is background noise.
fn describe(a: &Snap, b: &Snap, c: &Snap, reply: &Value) -> Effect {
    let mut fx: Vec<String> = Vec::new();
    let mut e = Effect::default();
    let error = (reply["ok"] != json!(true)).then(|| reply["error"].as_str().unwrap_or("failed").to_string());
    if let Some(msg) = &error {
        // a click behind a dialog is refused (the control channel names the dialog's buttons)
        let blocked = msg.contains("a dialog is open") || msg.contains("covered by a dialog");
        // a refused command is reported once, as it failed (below)
        let logged = c.log.iter().any(|x| !b.log.iter().any(|y| y.0 == x.0 && y.1 >= x.1) && msg.contains(x.4.as_str()));
        if blocked {
            fx.push(format!("blocked: {}", clip_text(msg, 160)));
        } else if !logged {
            fx.push(format!("error: {}", clip_text(msg, 160)));
        }
    }
    let noise: BTreeSet<&String> = a.ids.symmetric_difference(&b.ids).collect();
    let new: Vec<&Found> = c.els.iter().filter(|f| f.visible && !b.ids.contains(&f.id) && !noise.contains(&f.id)).collect();
    let gone: Vec<&Found> = b.els.iter().filter(|f| !c.ids.contains(&f.id) && !noise.contains(&f.id)).collect();
    // a dropdown's list opened in a dialog is a popup, not another dialog
    let new_dialog: Vec<&str> = new.iter().filter(|f| f.dialog && !f.floating).map(|f| f.id.as_str()).collect();
    if !new_dialog.is_empty() || (c.dialog.is_some() && c.dialog != b.dialog) || (c.modal && !b.modal) {
        let name = reply["result"]["dialog"]
            .as_str()
            .map(str::to_string)
            .or_else(|| c.dialog.clone().filter(|_| c.dialog != b.dialog))
            .or_else(|| common_prefix(&new_dialog))
            .unwrap_or_default();
        fx.push(format!("opens dialog {name}").trim_end().to_string());
        e.opened = Some("dialog".into());
        e.dialog = Some(name);
    } else if !new.is_empty() && ((c.popup && !b.popup) || new.iter().any(|f| f.floating)) {
        fx.push(format!("opens a menu ({} items)", new.len()));
        e.opened = Some("menu".into());
    } else if !new.is_empty() {
        let ids: Vec<&str> = new.iter().map(|f| f.id.as_str()).collect();
        fx.push(format!("shows {} more elements ({}…)", new.len(), common_prefix(&ids).unwrap_or_default()));
        e.opened = Some("view".into());
    }
    if (b.dialog.is_some() && c.dialog.is_none()) || (b.modal && !c.modal) || gone.iter().any(|f| f.dialog && !f.floating) {
        fx.push("closes the dialog".into());
    } else if gone.iter().any(|f| f.floating) && e.opened.is_none() {
        fx.push("closes the menu".into());
    } else if !gone.is_empty() && e.opened.is_none() {
        fx.push(format!("hides {} elements", gone.len()));
    }
    let mut ran: Vec<String> = Vec::new();
    for id in c.journal.get(b.journal.len()..).unwrap_or_default() {
        if !ran.contains(id) {
            ran.push(id.clone());
        }
    }
    if !ran.is_empty() {
        fx.push(format!("runs {}", ran.join(", ")));
    }
    e.commands = ran;
    if c.undo.len() > b.undo.len() || (c.undo.len() == b.undo.len() && c.undo.last() != b.undo.last()) {
        fx.push(format!("edits the project (undo: {})", c.undo.last().cloned().unwrap_or_default()));
    } else if c.undo.len() < b.undo.len() {
        fx.push(format!("undoes {}", b.undo.last().cloned().unwrap_or_default()));
    } else if c.redo < b.redo {
        fx.push("redoes an edit".into());
    }
    let mut failures = Vec::new();
    for (id, count, level, source, message) in &c.log {
        let seen = b.log.iter().find(|x| x.0 == *id);
        if seen.is_some_and(|x| x.1 >= *count) || level == "Info" {
            continue;
        }
        if !e.failed.contains(source) {
            e.failed.push(source.clone());
            failures.push(message.clone());
            fx.push(format!("fails: {source}: {}", clip_text(message, 120)));
        }
    }
    let mut noise_paths = Vec::new();
    diff_paths(&a.ui, &b.ui, "ui", 0, &mut noise_paths);
    let mut paths = Vec::new();
    diff_paths(&b.ui, &c.ui, "ui", 0, &mut paths);
    paths.retain(|p| !noise_paths.contains(p));
    let focus_only = paths == ["ui.focused"];
    // a press inside a panel focuses it: not what the element is for
    paths.retain(|p| p != "ui.focused");
    e.toggled = !paths.is_empty() && paths.iter().all(|p| pointer(&b.ui, p).is_some_and(Value::is_boolean) && pointer(&c.ui, p).is_some_and(Value::is_boolean));
    if paths.iter().any(|p| p == "ui.tool") {
        e.tool = c.ui["tool"].as_str().map(str::to_string);
    }
    if paths.iter().any(|p| p == "ui.mode") {
        e.mode = c.ui["mode"].as_str().map(str::to_string);
    }
    if !paths.is_empty() {
        let more = if paths.len() > 5 { format!(" (+{} more)", paths.len() - 5) } else { String::new() };
        fx.push(format!("sets {}{more}", paths.iter().take(5).cloned().collect::<Vec<_>>().join(", ")));
    }
    if c.playing != b.playing {
        fx.push(if c.playing { "starts playback".into() } else { "stops playback".into() });
    }
    if c.playhead != b.playhead {
        fx.push("moves the playhead".into());
    }
    if c.selection != b.selection {
        fx.push("changes the selection".into());
    }
    if c.active != b.active {
        fx.push("switches the active sequence".into());
    }
    if c.focused && !b.focused {
        fx.push("focuses a text field".into());
        e.focused = true;
    }
    if c.status != b.status && !c.status.is_empty() && !failures.iter().any(|m| c.status.contains(m.as_str())) {
        fx.push(format!("status: {}", clip_text(&c.status, 100)));
    }
    if c.jobs.iter().any(|j| !b.jobs.contains(j)) {
        fx.push("starts a background job (the crawl cancels it)".into());
    }
    if c.writes > b.writes {
        fx.push("writes a file (dropped by the sandbox)".into());
    }
    e.dead = fx.is_empty();
    if focus_only && fx.is_empty() {
        // a press inside a panel focuses it: not what the element is for, so it still counts as dead
        fx.push(format!("only focuses the {} panel", c.ui["focused"].as_str().unwrap_or("")));
    }
    e.text = if fx.is_empty() { "none".into() } else { fx.join("; ") };
    e
}

/// Close what an action opened: Escape, else the opened elements' Cancel / Close / Done / OK.
fn try_close(d: &mut Driver, opened: &[Found]) -> Option<String> {
    let ids: Vec<String> = opened.iter().filter(|f| f.visible).map(|f| f.id.clone()).collect();
    let still = |d: &Driver| {
        let now = d.ids();
        ids.iter().any(|i| now.contains(i))
    };
    d.call("ui.key", json!({"key": "Escape"}));
    d.settle();
    if !still(d) {
        return Some("ui.key Escape".into());
    }
    for suffix in [".cancel", ".close", ".done", ".ok"] {
        let Some(f) = opened.iter().find(|f| f.visible && f.id.ends_with(suffix) && denied(&f.id).is_none()) else { continue };
        let r = d.call("ui.click", json!({"id": f.id}));
        d.settle();
        if r["ok"] == json!(true) && !still(d) {
            return Some(format!("ui.click {}", pattern(&f.id)));
        }
    }
    None
}

fn run_job(job: &Job) -> Outcome {
    let mut d = Driver::new();
    let mut out = Outcome::default();
    for s in &job.reach {
        if let Err(e) = s.run(&mut d) {
            out.reach_error = Some(format!("{}: {e}", s.describe()));
            out.panic = d.app().ui_error.clone();
            return out;
        }
    }
    d.settle();
    match &job.action {
        Action::Look => {
            out.found = capture(&d).els;
            add_roles(&d, &mut out.found);
        }
        Action::Click { .. } | Action::Menu(_) => {
            let a = capture(&d);
            d.frames(SETTLE);
            let b = capture(&d);
            let reply = match &job.action {
                Action::Click { id, right } => d.call("ui.click", json!({"id": id, "button": if *right { "right" } else { "left" }})),
                Action::Menu(id) => d.call("ui.menu.invoke", json!({"id": id})),
                Action::Look => Value::Null,
            };
            d.settle();
            let c = capture(&d);
            // playback would keep the UI moving under the close attempts
            d.harness.state_mut().stop();
            for j in &d.app().session.jobs {
                if !b.jobs.contains(&j.id) {
                    j.progress.cancel.store(true, Ordering::Relaxed);
                }
            }
            let mut e = describe(&a, &b, &c, &reply);
            let noise: BTreeSet<&String> = a.ids.symmetric_difference(&b.ids).collect();
            let mut new: Vec<Found> = c.els.iter().filter(|f| !b.ids.contains(&f.id) && !noise.contains(&f.id)).cloned().collect();
            add_roles(&d, &mut new);
            if d.app().ui_error.is_none() && matches!(e.opened.as_deref(), Some("dialog" | "menu")) {
                e.close = try_close(&mut d, &new);
            }
            out.found = new;
            out.effect = Some(e);
        }
    }
    out.panic = out.panic.or_else(|| d.app().ui_error.clone());
    out
}

// ------------------------------------------------------------------ child processes

fn scratch(name: &str) -> PathBuf {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join("ui-map").join(format!("{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("data")).expect("scratch directory");
    dir
}

fn processes() -> usize {
    std::env::var("FILMCRAFT_UI_MAP_PROCESSES")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or_else(|| std::thread::available_parallelism().map_or(2, |n| (n.get() / 4).clamp(2, 6)))
}

/// Run `jobs` in child processes, `BATCH` at a time; outcomes in job order.
fn run_jobs(dir: &Path, jobs: &[Job]) -> Vec<Outcome> {
    let mut results: Vec<Option<Outcome>> = vec![None; jobs.len()];
    let mut queue: Vec<Vec<usize>> = (0..jobs.len()).collect::<Vec<_>>().chunks(BATCH).map(<[usize]>::to_vec).collect();
    queue.reverse();
    let mut running: Vec<(std::process::Child, Vec<usize>, PathBuf)> = Vec::new();
    // a copy: a build while the crawl runs (another agent's `cargo test`) replaces the binary
    let exe = dir.join(format!("worker{}", std::env::consts::EXE_SUFFIX));
    if !exe.exists() {
        std::fs::copy(std::env::current_exe().expect("test binary"), &exe).expect("copy the test binary");
    }
    let mut n = 0;
    loop {
        while running.len() < processes()
            && let Some(batch) = queue.pop()
        {
            n += 1;
            let input = dir.join(format!("jobs-{n}.json"));
            let output = dir.join(format!("out-{n}.jsonl"));
            let chunk: Vec<&Job> = batch.iter().map(|&i| &jobs[i]).collect();
            std::fs::write(&input, serde_json::to_vec(&chunk).expect("jobs")).expect("write jobs");
            let log = std::fs::File::create(dir.join(format!("log-{n}.txt"))).map(std::process::Stdio::from).unwrap_or(std::process::Stdio::null());
            let child = std::process::Command::new(&exe)
                .args(["crawl_worker", "--exact", "--ignored", "--nocapture", "--test-threads=1"])
                .env("FILMCRAFT_UI_MAP_JOBS", &input)
                .env("FILMCRAFT_UI_MAP_OUT", &output)
                .env("FILMCRAFT_DATA_DIR", dir.join("data"))
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(log)
                .spawn()
                .expect("spawn a crawl worker");
            running.push((child, batch, output));
        }
        if running.is_empty() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
        let mut i = 0;
        while i < running.len() {
            if running[i].0.try_wait().ok().flatten().is_none() {
                i += 1;
                continue;
            }
            let (_, batch, output) = running.swap_remove(i);
            let lines: Vec<Outcome> = std::fs::read_to_string(&output).unwrap_or_default().lines().filter_map(|l| serde_json::from_str(l).ok()).collect();
            let done = lines.len().min(batch.len());
            for (k, o) in lines.into_iter().take(done).enumerate() {
                results[batch[k]] = Some(o);
            }
            if done < batch.len() {
                // the process died on this job: report it, run the rest again
                let dead = batch[done];
                results[dead] = Some(Outcome { panic: Some(format!("the process died running {}", jobs[dead].describe())), ..Default::default() });
                if done + 1 < batch.len() {
                    queue.push(batch[done + 1..].to_vec());
                }
            }
        }
    }
    results.into_iter().map(Option::unwrap_or_default).collect()
}

/// A child process of [`run_jobs`]: runs the jobs in `FILMCRAFT_UI_MAP_JOBS`, one fresh app each,
/// and appends one outcome per line to `FILMCRAFT_UI_MAP_OUT`. Does nothing on its own.
#[test]
#[ignore = "a worker process of the UI-map crawl; does nothing on its own"]
fn crawl_worker() {
    let (Ok(input), Ok(output)) = (std::env::var("FILMCRAFT_UI_MAP_JOBS"), std::env::var("FILMCRAFT_UI_MAP_OUT")) else { return };
    let jobs: Vec<Job> = serde_json::from_slice(&std::fs::read(input).expect("jobs")).expect("jobs");
    let mut out = std::fs::File::create(output).expect("outcomes");
    for job in &jobs {
        let o = catch_unwind(AssertUnwindSafe(|| run_job(job))).unwrap_or_else(|p| {
            let msg = p.downcast_ref::<&str>().map(|s| s.to_string()).or_else(|| p.downcast_ref::<String>().cloned()).unwrap_or_else(|| "panic".into());
            Outcome { panic: Some(msg), ..Default::default() }
        });
        let _ = writeln!(out, "{}", serde_json::to_string(&o).unwrap_or_default());
        let _ = out.flush();
    }
}

// ------------------------------------------------------------------ the map

/// Segments before a number that names a project object (a clip id, an item id…), which differs
/// from project to project. Other numbers (a dropdown's option, an effect slot, a zoom level) stay.
const VOLATILE: &str =
    "clip item bin marker transition caption layer tab seq sequence projectBin renderBar row kf keyframe job node word speaker guide mask track angle cue";

/// The id as the map writes it: numbers that name project objects (clips, items, markers, bins…)
/// become `{clip}`, `{item}`… after the segment before them, track names (`V1`, `A2`) `{track}`,
/// and file names in the Media Browser `{name}`.
fn pattern(id: &str) -> String {
    for p in ["mediaBrowser.entry.", "mediaBrowser.tree.localDrives.", "mediaBrowser.tree.favorites.", "mediaBrowser.tree.recent.", "mediaBrowser.row."] {
        if let Some(rest) = id.strip_prefix(p)
            && !rest.is_empty()
        {
            return format!("{p}{{name}}");
        }
    }
    let segs: Vec<&str> = id.split('.').collect();
    let mut out: Vec<String> = Vec::with_capacity(segs.len());
    for (i, s) in segs.iter().enumerate() {
        let digits = !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
        let track = s.len() >= 2 && matches!(s.as_bytes().first(), Some(b'V' | b'A')) && s.bytes().skip(1).all(|b| b.is_ascii_digit());
        let prev = i.checked_sub(1).and_then(|j| segs.get(j)).copied().unwrap_or("");
        if digits && VOLATILE.split(' ').any(|w| w == prev) {
            out.push(format!("{{{prev}}}"));
        } else if track {
            out.push("{track}".into());
        } else {
            out.push(s.to_string());
        }
    }
    out.join(".")
}

/// A starting state of the crawl: the steps that make it, and where to look then (`None`: the
/// start screen, Import and Export mode and every panel, each also maximized).
struct Prep {
    steps: Vec<Step>,
    views: Option<Vec<Vec<Step>>>,
    /// Retry the menu commands that are disabled at start (they need a selection) here.
    menus: bool,
}

fn exec(command: &str, params: Value, shown: &str) -> Step {
    Step::Exec { command: command.into(), params, shown: shown.into() }
}

/// A panel shown and maximized (Maximize Frame), so long panels show more of their content.
fn maximized(p: PanelKind) -> Vec<Step> {
    vec![Step::Panel(p.id()), Step::Set(json!({"focused": p.id()})), Step::Menu("window.maximizeFrame".into())]
}

fn views() -> Vec<Vec<Step>> {
    let mut v: Vec<Vec<Step>> = vec![vec![], vec![Step::Mode("import".into())], vec![Step::Mode("export".into())]];
    v.extend(PanelKind::ALL.iter().map(|p| vec![Step::Panel(p.id())]));
    v.extend(PanelKind::ALL.iter().map(|p| maximized(*p)));
    v
}

fn preps(probe: &Driver) -> Vec<Prep> {
    let session = &probe.app().session;
    let seq = session.active_sequence().expect("the demo sequence");
    let first =
        |tracks: &[filmcraft_engine::project::Track], solo: bool| tracks.iter().flat_map(|t| t.items.iter()).find(|c| !solo || c.link.is_none()).cloned();
    let video = first(&seq.video_tracks, false).expect("a video clip");
    let audio = first(&seq.audio_tracks, true).or_else(|| first(&seq.audio_tracks, false)).expect("an audio clip");
    let linked_audio = first(&seq.audio_tracks, false).expect("an audio clip");
    let media: Vec<u64> =
        session.project.items.values().filter(|i| matches!(i.kind, filmcraft_engine::project::ItemKind::Media(_))).map(|i| i.id.0).take(2).collect();
    let bin = session.project.root.children.iter().find_map(|e| match e {
        filmcraft_engine::project::BinEntry::Bin(b) => Some(b.id.0),
        _ => None,
    });
    let select = |c: u64, what: &str| exec("timeline.select", json!({"clips": [c]}), &format!(r#"{{"clips":[<{what}>]}}"#));
    let panel = |p: PanelKind| vec![vec![Step::Panel(p.id())], maximized(p)];
    let tk = |s: f64| linked_audio.source_in.0 + (s * filmcraft_time::TICKS_PER_SECOND as f64) as i64;
    let words: Vec<Value> = ["Welcome", "to", "the", "demo."]
        .iter()
        .enumerate()
        .map(|(i, w)| json!({"text": w, "start": tk(0.5 + i as f64 * 0.5), "end": tk(0.9 + i as f64 * 0.5), "speaker": 0}))
        .collect();
    let mut v = vec![
        Prep { steps: vec![], views: None, menus: true },
        Prep { steps: vec![select(video.id.0, "a video clip")], views: None, menus: true },
        Prep { steps: vec![select(audio.id.0, "an audio clip")], views: None, menus: true },
        Prep { steps: vec![exec("graphics.newText", json!({}), "{}")], views: None, menus: true },
        Prep {
            steps: vec![exec("project.select", json!({"items": media}), r#"{"items":[<two clips in the Project panel>]}"#)],
            views: Some(panel(PanelKind::Project)),
            menus: true,
        },
        // Effect Controls' Custom Setup ▸ Edit… rows open the audio effect editors
        Prep {
            steps: ["parametric_eq", "graphic_eq", "multiband_compressor", "dynamics_rack"]
                .iter()
                .map(|fx| exec("effects.apply", json!({"clips": [audio.id.0], "effect": fx}), &format!(r#"{{"clips":[<an audio clip>],"effect":"{fx}"}}"#)))
                .chain([select(audio.id.0, "an audio clip")])
                .collect(),
            views: Some(panel(PanelKind::EffectControls)),
            menus: false,
        },
        Prep {
            steps: vec![
                exec("effects.apply", json!({"clips": [video.id.0], "effect": "gaussian_blur"}), r#"{"clips":[<a video clip>],"effect":"gaussian_blur"}"#),
                exec(
                    "masks.add",
                    json!({"clip": video.id.0, "effect": "gaussian_blur", "shape": "ellipse", "center": [960, 540], "size": [600, 600]}),
                    r#"{"clip":<a video clip>,"effect":"gaussian_blur","shape":"ellipse","center":[960,540],"size":[600,600]}"#,
                ),
                select(video.id.0, "a video clip"),
            ],
            views: Some([panel(PanelKind::EffectControls), panel(PanelKind::Program)].concat()),
            menus: false,
        },
        // track inserts and sends in the Audio Track Mixer
        Prep {
            steps: vec![
                exec("mixer.addInsert", json!({"strip": "A1", "effect": "parametric_eq"}), r#"{"strip":"A1","effect":"parametric_eq"}"#),
                exec("mixer.addSubmix", json!({}), "{}"),
                exec("mixer.addSend", json!({"strip": "A1", "target": "S1"}), r#"{"strip":"A1","target":"S1"}"#),
            ],
            views: Some({
                let mut v = panel(PanelKind::AudioTrackMixer);
                v.push(
                    [
                        maximized(PanelKind::AudioTrackMixer),
                        vec![Step::Click { id: "mixer.showEffects".into(), pattern: "mixer.showEffects".into(), right: false }],
                    ]
                    .concat(),
                );
                v
            }),
            menus: false,
        },
        // History's redo rows
        Prep {
            steps: vec![exec("sequence.addEdit", json!({}), "{}"), exec("edit.undo", json!({}), "{}")],
            views: Some(panel(PanelKind::History)),
            menus: false,
        },
        // a transcript in the Text panel
        Prep {
            steps: vec![exec(
                "transcript.set",
                json!({"item": linked_audio.item.0, "transcript": {"language": "en", "words": words}}),
                r#"{"item":<a clip's media item>,"transcript":{"language":"en","words":[{"text":"Welcome","start":<ticks>,"end":<ticks>},…]}}"#,
            )],
            views: Some(vec![vec![
                Step::Panel("Text".into()),
                Step::Click { id: "text.tab.Transcript".into(), pattern: "text.tab.Transcript".into(), right: false },
            ]]),
            menus: false,
        },
        // a caption track
        Prep {
            steps: vec![exec("captions.newTrack", json!({}), "{}")],
            views: Some(vec![
                vec![Step::Panel("Text".into()), Step::Click { id: "text.tab.Captions".into(), pattern: "text.tab.Captions".into(), right: false }],
                vec![Step::Panel("Timeline".into())],
            ]),
            menus: false,
        },
    ];
    if let Some(b) = bin {
        // a bin's items, in each of the Project panel's views
        for view in ["list", "icon", "freeform"] {
            v.push(Prep {
                steps: vec![
                    Step::Set(json!({"projectPanel": {"bin": b}})),
                    exec("project.view.set", json!({"view": view}), &format!(r#"{{"view":"{view}"}}"#)),
                ],
                views: Some(panel(PanelKind::Project)),
                menus: false,
            });
        }
    }
    v
}

/// Everything known about one element (or one menu command).
#[derive(Clone, Debug, Default)]
struct Entry {
    id: String,
    concrete: String,
    label: String,
    panel: String,
    reach: Vec<Step>,
    depth: usize,
    visible: bool,
    rect: [f32; 4],
    role: Option<String>,
    /// From the element that opened it: `option` (a dropdown's list) or `menu-item`.
    kind_hint: Option<&'static str>,
    examples: BTreeSet<String>,
    effect: Option<Effect>,
    right: Option<Effect>,
    menu: bool,
    shortcut: Option<String>,
    clicked: bool,
    skip: Option<String>,
}

#[derive(Default)]
struct Crawl {
    entries: BTreeMap<String, Entry>,
    menus: BTreeMap<String, Entry>,
    commands: BTreeSet<String>,
    panics: Vec<(String, String)>,
    reach_errors: Vec<String>,
    jobs: usize,
}

/// The in-window menu bar (`menu.<Top>`, `menu.<Top>.<Sub>`, `menu.item.<command>`) is drawn where
/// the OS has no global menu bar; on macOS the same commands are in the system menu bar.
const MENU_BAR: &str = "Menu bar (Windows, Linux, web)";

fn panel_name(region: &str) -> String {
    if let Some(p) = region.strip_prefix("panel:") {
        return p.to_string();
    }
    if let Some(m) = region.strip_prefix("mode:") {
        return format!("{m} mode");
    }
    region.to_string()
}

fn new_entry(pat: &str, f: &Found, reach: &[Step], depth: usize, panel: String, hint: Option<&'static str>) -> Entry {
    Entry {
        id: pat.to_string(),
        concrete: f.id.clone(),
        label: clip_text(&f.label, 80),
        panel,
        reach: reach.to_vec(),
        depth,
        visible: f.visible,
        rect: f.rect,
        role: f.role.clone(),
        kind_hint: hint,
        ..Default::default()
    }
}

impl Crawl {
    /// Record an element seen after `reach`. The first sighting (in job order) gives its reach;
    /// a visible sighting replaces one scrolled out of view.
    fn register(&mut self, f: &Found, reach: &[Step], depth: usize, panel: String, hint: Option<&'static str>) {
        let pat = pattern(&f.id);
        let panel = if f.id.starts_with("menu.") { MENU_BAR.to_string() } else { panel };
        if let Some(e) = self.entries.get_mut(&pat) {
            if e.examples.len() < 6 {
                e.examples.insert(f.id.clone());
            }
            if !e.visible && f.visible && !e.clicked {
                *e = Entry { examples: std::mem::take(&mut e.examples), ..new_entry(&pat, f, reach, depth, panel, hint) };
            }
            return;
        }
        let mut e = new_entry(&pat, f, reach, depth, panel, hint);
        e.examples.insert(f.id.clone());
        self.entries.insert(pat, e);
    }

    fn command_of(&self, e: &Entry) -> Option<String> {
        let fx = e.effect.as_ref();
        if let Some(c) = fx.and_then(|x| x.commands.first().or(x.failed.first())).filter(|c| self.commands.contains(*c)) {
            return Some(c.clone());
        }
        // `program.transport.playback.toggle` → `playback.toggle`
        let segs: Vec<&str> = e.concrete.split('.').collect();
        for k in 1..segs.len().saturating_sub(1) {
            let tail = segs[k..].join(".");
            if self.commands.contains(&tail) {
                return Some(tail);
            }
        }
        let lower_first = |s: &str| {
            let mut c = s.chars();
            c.next().map(|f| f.to_lowercase().chain(c).collect::<String>()).unwrap_or_default()
        };
        if let Some(t) = fx.and_then(|x| x.tool.as_deref()) {
            let id = format!("tool.{}", lower_first(t));
            if self.commands.contains(&id) {
                return Some(id);
            }
        }
        if let Some(m) = fx.and_then(|x| x.mode.as_deref()) {
            let id = format!("mode.{}", m.to_ascii_lowercase());
            if self.commands.contains(&id) {
                return Some(id);
            }
        }
        None
    }
}

fn kind(e: &Entry) -> &'static str {
    if e.menu {
        return "menu-item";
    }
    match e.role.as_deref() {
        Some("ComboBox") => return "dropdown",
        Some("CheckBox" | "Switch") => return "checkbox",
        Some("Slider") => return "slider",
        Some("TextInput" | "MultilineTextInput" | "SearchInput" | "PasswordInput" | "EmailInput") => return "field",
        Some("SpinButton") => return "number",
        Some("Tab") => return "tab",
        Some("RadioButton") => return "option",
        Some("MenuItem" | "MenuItemCheckBox" | "MenuItemRadio") => return "menu-item",
        Some("Link") => return "link",
        _ => {}
    }
    let id = e.id.as_str();
    let last = id.rsplit('.').next().unwrap_or(id);
    if id.starts_with("panel.tab.") || id.contains(".tab.") || id.contains(".tabs.") || id.starts_with("panel.tabs") {
        return "tab";
    }
    if let Some(h) = e.kind_hint {
        return h;
    }
    let fx = e.effect.as_ref();
    if fx.is_some_and(|x| x.focused) {
        return "field";
    }
    if fx.and_then(|x| x.opened.as_deref()) == Some("menu") {
        let menu = ["menu", "wrench", "more", "settings", "workspaces"].iter().any(|k| id.to_ascii_lowercase().contains(k));
        return if menu { "menu" } else { "dropdown" };
    }
    let l = last.to_ascii_lowercase();
    if id.contains("gutter") || ["grip", "splitter", "resize", "handle"].iter().any(|k| l.contains(k)) {
        return "handle";
    }
    if ["fader", "pan", "slider", "knob", "scrubbar", "ruler", "zoombar", "scrollbar"].contains(&l.as_str()) || l.contains("slider") {
        return "slider";
    }
    if fx.is_some_and(|x| x.toggled) || l.starts_with("toggle") || id.contains(".toggle.") {
        return "toggle";
    }
    if e.rect[2] * e.rect[3] > 30_000.0 {
        return "area";
    }
    "button"
}

#[derive(Serialize)]
struct MapEntry {
    id: String,
    label: String,
    panel: String,
    kind: String,
    reach: Vec<String>,
    effect: String,
    command: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    shortcut: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    close: Option<String>,
    #[serde(rename = "rightClick", skip_serializing_if = "Option::is_none")]
    right_click: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    examples: Vec<String>,
}

/// What the crawl covers.
#[derive(Clone, Copy)]
struct Scope {
    /// The extra starting states of [`preps`] (a clip of each kind selected, effects, a transcript,
    /// a caption track, a bin's items…) and every panel maximized.
    preps: bool,
    /// Every menu-bar command (and what its dialog holds).
    menus: bool,
    /// Elements found after this many clicks are clicked too (0: only what is on screen at first).
    depth: usize,
    /// Right-click everything on screen at first (context menus).
    right: bool,
}

/// Panics and failed reaches of a job.
fn note(cr: &mut Crawl, job: &Job, o: &Outcome) {
    if let Some(p) = &o.panic {
        cr.panics.push((job.describe(), p.clone()));
    }
    if let Some(r) = &o.reach_error {
        cr.reach_errors.push(format!("{}: {r}", job.describe()));
    }
}

fn crawl(scope: Scope, name: &str) -> Crawl {
    let dir = scratch(name);
    let mut cr = Crawl::default();
    let mut probe = Driver::new();
    for v in [probe.call("engine.commands", json!({}))["result"].clone(), probe.call("ui.menu.list", json!({}))["result"].clone()] {
        for c in v.as_array().cloned().unwrap_or_default() {
            if let Some(id) = c["id"].as_str() {
                cr.commands.insert(id.to_string());
            }
        }
    }
    for c in filmcraft_ui_egui::menus::UI_COMMANDS.iter().chain(filmcraft_ui_egui::panels::keyboard::COMMANDS) {
        cr.commands.insert(c.id.to_string());
    }
    let menu_items: Vec<Value> = probe.call("ui.menu.list", json!({}))["result"].as_array().cloned().unwrap_or_default();
    let preps = if scope.preps { preps(&probe) } else { vec![Prep { steps: vec![], views: None, menus: true }] };
    drop(probe);

    // round 0: look at every place, invoke every menu command
    let all_views = if scope.preps {
        views()
    } else {
        let mut v: Vec<Vec<Step>> = vec![vec![], vec![Step::Mode("import".into())], vec![Step::Mode("export".into())]];
        v.extend(PanelKind::ALL.iter().map(|p| vec![Step::Panel(p.id())]));
        v
    };
    let mut jobs: Vec<Job> = Vec::new();
    for p in &preps {
        for v in p.views.as_ref().unwrap_or(&all_views) {
            jobs.push(Job { reach: [p.steps.clone(), v.clone()].concat(), action: Action::Look });
        }
    }
    let looks = jobs.len();
    let mut menu_jobs: Vec<(String, usize)> = Vec::new();
    if scope.menus {
        for m in &menu_items {
            let Some(id) = m["id"].as_str() else { continue };
            let path = m["path"].as_array().map(|p| p.iter().filter_map(Value::as_str).collect::<Vec<_>>().join(" ▸ ")).unwrap_or_default();
            let mut e = Entry {
                id: id.to_string(),
                concrete: id.to_string(),
                label: clip_text(m["label"].as_str().unwrap_or(id), 80),
                panel: format!("Menu: {path}"),
                menu: true,
                shortcut: m["shortcut"].as_str().map(str::to_string),
                ..Default::default()
            };
            if let Some(why) = denied(id) {
                e.skip = Some(why.to_string());
                cr.menus.insert(id.to_string(), e);
                continue;
            }
            cr.menus.insert(id.to_string(), e);
            let enabled = m["enabled"].as_bool().unwrap_or(true);
            for (k, p) in preps.iter().enumerate().filter(|(_, p)| p.menus) {
                if k > 0 && enabled {
                    break;
                }
                menu_jobs.push((id.to_string(), jobs.len()));
                jobs.push(Job { reach: p.steps.clone(), action: Action::Menu(id.to_string()) });
            }
        }
    }
    let outcomes = run_jobs(&dir, &jobs);
    cr.jobs += jobs.len();
    for (job, o) in jobs.iter().zip(&outcomes).take(looks) {
        note(&mut cr, job, o);
        for f in &o.found {
            cr.register(f, &job.reach, 0, panel_name(&f.region), None);
        }
    }
    // a menu command: the first starting state where it works
    let mut by_menu: BTreeMap<&str, Vec<usize>> = BTreeMap::new();
    for (id, j) in &menu_jobs {
        by_menu.entry(id.as_str()).or_default().push(*j);
    }
    for (id, js) in by_menu {
        for &j in &js {
            note(&mut cr, &jobs[j], &outcomes[j]);
        }
        let works = |j: &usize| outcomes[*j].effect.as_ref().is_some_and(|e| e.failed.is_empty() && !e.text.starts_with("error"));
        let Some(&j) = js.iter().find(|j| works(j)).or(js.first()) else { continue };
        let (job, o) = (&jobs[j], &outcomes[j]);
        let Some(e) = cr.menus.get_mut(id) else { continue };
        e.reach = job.reach.clone();
        e.effect = o.effect.clone();
        e.clicked = true;
        // Edit ▸ Preferences ▸ <page>… all open the one Settings dialog
        let title = if e.panel.ends_with("Preferences") { "Settings".to_string() } else { e.label.trim_end_matches(['…', '.']).to_string() };
        let opened = o.effect.as_ref().and_then(|x| x.opened.clone());
        let reach = [job.reach.clone(), vec![Step::Menu(id.to_string())]].concat();
        for f in &o.found {
            let panel = if f.dialog { format!("Dialog: {title}") } else { panel_name(&f.region) };
            let hint = (opened.as_deref() == Some("menu")).then_some("menu-item");
            cr.register(f, &reach, 1, panel, hint);
        }
    }

    // then click what the round before found
    for depth in 0..=scope.depth {
        let mut jobs: Vec<Job> = Vec::new();
        let mut keys: Vec<(String, bool)> = Vec::new();
        for (k, e) in &cr.entries {
            if e.depth != depth || e.clicked || e.skip.is_some() || !e.visible || denied(&e.concrete).is_some() {
                continue;
            }
            jobs.push(Job { reach: e.reach.clone(), action: Action::Click { id: e.concrete.clone(), right: false } });
            keys.push((k.clone(), false));
            if scope.right && depth == 0 {
                jobs.push(Job { reach: e.reach.clone(), action: Action::Click { id: e.concrete.clone(), right: true } });
                keys.push((k.clone(), true));
            }
        }
        let outcomes = run_jobs(&dir, &jobs);
        cr.jobs += jobs.len();
        for ((key, right), (job, o)) in keys.iter().zip(jobs.iter().zip(&outcomes)) {
            note(&mut cr, job, o);
            let Some(e) = cr.entries.get_mut(key) else { continue };
            e.clicked = true;
            let (Action::Click { id, .. }, Some(fx)) = (&job.action, &o.effect) else { continue };
            if *right {
                // only what a right-click opens is worth keeping
                if fx.opened.as_deref() != Some("menu") {
                    continue;
                }
                e.right = Some(fx.clone());
            } else {
                e.effect = Some(fx.clone());
            }
            let parent = e.clone();
            let parent_kind = kind(&parent);
            let step = Step::Click { id: id.clone(), pattern: parent.id.clone(), right: *right };
            let reach = [job.reach.clone(), vec![step]].concat();
            let opened = fx.opened.as_deref();
            let item = if parent_kind == "dropdown" && !*right { "option" } else { "menu-item" };
            for f in &o.found {
                let (panel, hint) = match (f.dialog, opened) {
                    (true, Some("menu")) => (parent.panel.clone(), Some(item)),
                    (true, _) if parent.panel.starts_with("Dialog") && opened != Some("dialog") => (parent.panel.clone(), None),
                    (true, _) => (format!("Dialog: {}", parent.label.trim_end_matches(['…', '.'])), None),
                    (false, Some("menu")) => (parent.panel.clone(), Some(item)),
                    (false, _) if f.floating => (parent.panel.clone(), Some(item)),
                    (false, _) => (panel_name(&f.region), None),
                };
                cr.register(f, &reach, depth + 1, panel, hint);
            }
        }
    }
    for e in cr.entries.values_mut() {
        if e.effect.is_none() && e.skip.is_none() {
            e.skip = Some(if let Some(why) = denied(&e.concrete) {
                why.to_string()
            } else if !e.visible {
                "outside the visible area of its panel or dialog (scroll it into view first)".into()
            } else {
                format!("found {} clicks deep (the crawl clicks {} deep)", e.depth, scope.depth + 1)
            });
        }
    }
    let _ = std::fs::remove_dir_all(&dir);
    cr
}

/// A click that did nothing observable (focusing the panel it is in doesn't count).
fn is_dead(effect: &str) -> bool {
    effect == "none" || effect.starts_with("only focuses the ")
}

fn map_entries(cr: &Crawl) -> Vec<MapEntry> {
    let mut out: BTreeMap<String, MapEntry> = BTreeMap::new();
    for e in cr.entries.values().chain(cr.menus.values()) {
        let effect = match (&e.effect, &e.skip) {
            (Some(fx), _) => fx.text.clone(),
            (None, Some(why)) => format!("not clicked by the crawl: {why}"),
            (None, None) => "not clicked by the crawl".into(),
        };
        let examples: Vec<String> = if e.examples.iter().any(|x| *x != e.id) { e.examples.iter().cloned().collect() } else { Vec::new() };
        let mut key = e.id.clone();
        if out.contains_key(&key) {
            key = format!("{key} (menu)");
        }
        out.insert(
            key,
            MapEntry {
                id: e.id.clone(),
                label: scrub(&e.label),
                panel: scrub(&e.panel),
                kind: kind(e).into(),
                reach: e.reach.iter().map(Step::describe).collect(),
                effect: scrub(&effect),
                command: if e.menu { Some(e.id.clone()) } else { cr.command_of(e) },
                shortcut: e.shortcut.clone(),
                close: e.effect.as_ref().and_then(|x| x.close.clone()),
                right_click: e.right.as_ref().map(|x| match &x.close {
                    Some(c) => scrub(&format!("{} (close: {c})", x.text)),
                    None => scrub(&x.text),
                }),
                examples,
            },
        );
    }
    out.into_values().collect()
}

/// Where the map goes: `docs/`, or `FILMCRAFT_UI_MAP_DIR` (to look at a crawl without touching the
/// committed map; the normal-run test writes there only when it is set).
fn map_dir() -> Option<PathBuf> {
    std::env::var_os("FILMCRAFT_UI_MAP_DIR").filter(|v| !v.is_empty()).map(PathBuf::from)
}

fn write_map(cr: &Crawl, docs: &Path) {
    let entries = map_entries(cr);
    let dead = entries.iter().filter(|e| is_dead(&e.effect)).count();
    let not_clicked = entries.iter().filter(|e| e.effect.starts_with("not clicked")).count();
    let about = "Every automation element of FilmCraft and every menu command: where it is (panel), how to make it visible from a fresh start \
                 (reach), what a click did on the demo project (effect) and the engine command behind it (command). Generated by the UI crawler \
                 (crates/ui-egui/tests/ui_map_crawl.rs); do not edit by hand.";
    let syntax = json!({
        "ui.panel.show <Panel>": "ui_control {method: \"ui.panel.show\", params: {panel}}",
        "ui.set mode=<mode>": "ui_control {method: \"ui.set\", params: {mode}}",
        "ui.menu.invoke <command>": "command_run {id: command} (opens the command's dialog, as the menu does)",
        "command_run <command> <params>": "command_run; <…> names something to pick, e.g. a clip id from sequence_inspect",
        "ui.click <id>": "ui_click {id}; `button=right` is a right-click. {clip}, {item}, {track}, {name}… stand for a real id from ui_elements",
    });
    let counts =
        json!({"elements": entries.iter().filter(|e| e.kind != "menu-item").count(), "menuCommands": cr.menus.len(), "dead": dead, "notClicked": not_clicked});
    let mut s = String::from("{\n");
    s.push_str(&format!("  \"about\": {},\n", json!(about)));
    s.push_str(&format!("  \"regenerate\": {},\n", json!(REGENERATE)));
    s.push_str(&format!("  \"window\": [{}, {}],\n", WINDOW.x, WINDOW.y));
    s.push_str(&format!("  \"reachSyntax\": {syntax},\n"));
    s.push_str(&format!("  \"counts\": {counts},\n"));
    s.push_str("  \"elements\": [\n");
    for (i, e) in entries.iter().enumerate() {
        s.push_str("    ");
        s.push_str(&serde_json::to_string(e).expect("entry"));
        s.push_str(if i + 1 < entries.len() { ",\n" } else { "\n" });
    }
    s.push_str("  ]\n}\n");
    std::fs::write(docs.join("ui-map.json"), s).expect("write docs/ui-map.json");
    std::fs::write(docs.join("ui-map.md"), summary(cr, &entries)).expect("write docs/ui-map.md");
}

fn summary(cr: &Crawl, entries: &[MapEntry]) -> String {
    #[derive(Default)]
    struct Row {
        all: usize,
        clicked: usize,
        dead: usize,
        not: usize,
        commands: usize,
    }
    let mut rows: BTreeMap<String, Row> = BTreeMap::new();
    for e in entries {
        let panel = if e.kind == "menu-item" && e.panel.starts_with("Menu: ") {
            "Menu bar".to_string()
        } else if e.panel.starts_with("Dialog: ") {
            "Dialogs".to_string()
        } else {
            e.panel.clone()
        };
        let r = rows.entry(panel).or_default();
        r.all += 1;
        r.clicked += usize::from(!e.effect.starts_with("not clicked"));
        r.dead += usize::from(is_dead(&e.effect));
        r.not += usize::from(e.effect.starts_with("not clicked"));
        r.commands += usize::from(e.command.is_some());
    }
    let dialogs: BTreeSet<&str> = entries.iter().filter_map(|e| e.panel.strip_prefix("Dialog: ")).collect();
    let mut md = String::new();
    md.push_str("# UI map\n\n<!-- Generated by crates/ui-egui/tests/ui_map_crawl.rs; do not edit by hand. -->\n\n");
    md.push_str(
        "[`ui-map.json`](ui-map.json) lists every automation element FilmCraft draws and every menu-bar command: its `id`, `label`, \
         `panel` (or dialog / mode / menu), `kind`, `reach` (the steps from a fresh start that make it visible), `effect` (what a \
         click did on the demo project), `command` (the engine command behind it, when there is one), and for elements that open \
         something `close` (how to get rid of it) and `rightClick` (its context menu). Ids with `{clip}`, `{item}`, `{track}`, \
         `{name}`… stand for a family of elements (one per clip, item, track, file); `examples` gives real ids from the demo \
         project.\n\n",
    );
    md.push_str("## Using it (agents)\n\n");
    md.push_str(
        "The MCP tool `ui_map` searches the map (`prefix`, `panel`, `kind`, free-text `query`). For an entry with a `command`, run \
         it with `command_run` (faster, and it works headless). Otherwise follow `reach` (`ui.panel.show X` → `ui_control`, \
         `ui.click id` → `ui_click`, `ui.menu.invoke id` → `command_run`), look up the real id of a `{…}` family with \
         `ui_elements`, then `ui_click` it. See [agents.md](agents.md#every-button-ui_map-then-ui_click).\n\n",
    );
    md.push_str("## Regenerating\n\n");
    md.push_str(&format!(
        "```sh\n{REGENERATE}\n```\n\nThe crawl opens the demo project in a {}×{} window and visits the start screen, Import and Export mode \
         and every panel, also maximized, each with a video clip, an audio clip or a new graphic selected, plus a few prepared \
         states (audio effects with their editors, a mask, mixer inserts and sends, a transcript, a caption track, a bin's items \
         in each Project view), and invokes every menu command (with a selection when it needs one). It clicks every element it finds on a fresh app, right-clicks those on screen at \
         first (context menus), and clicks what each click reveals (popups, dialogs, tabs) up to three clicks deep. The apps use \
         sandboxed host services and a scratch data directory: none of your files or settings are read or written. The normal-run \
         test `every_visible_element_and_menu_command_runs_without_a_panic` clicks everything on screen at first and runs every \
         menu command without writing the map; any panic fails it.\n\n",
        WINDOW.x, WINDOW.y
    ));
    md.push_str(&format!(
        "Last crawl: {} jobs; {} entries ({} elements, {} menu commands); {} dialogs; {} panics.\n\n",
        cr.jobs,
        entries.len(),
        entries.iter().filter(|e| e.kind != "menu-item").count(),
        cr.menus.len(),
        dialogs.len(),
        cr.panics.len()
    ));
    md.push_str("## Coverage\n\n| Where | Entries | Clicked | Dead | Not clicked | With a command |\n|---|---:|---:|---:|---:|---:|\n");
    for (p, r) in &rows {
        md.push_str(&format!("| {p} | {} | {} | {} | {} | {} |\n", r.all, r.clicked, r.dead, r.not, r.commands));
    }
    md.push_str(
        "\n**Dead**: the click did nothing the crawl could observe (no command, no UI state, no new elements, no playback or \
         selection change). Some are fine: a tab or option that is already selected, a read-out, an empty area, a control that \
         needs a drag rather than a click. Others are worth a look.\n\n**Not clicked**: elements scrolled out of view, found \
         deeper than the crawl clicks, or left alone on purpose (speech-to-text and model downloads, Reveal Log Files).\n\n",
    );
    md.push_str("## Dialogs reached\n\n");
    md.push_str(&dialogs.iter().map(|d| format!("- {d}")).collect::<Vec<_>>().join("\n"));
    md.push_str("\n\n## Dead elements\n\n");
    let mut by_panel: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    for e in entries.iter().filter(|e| is_dead(&e.effect)) {
        by_panel.entry(e.panel.as_str()).or_default().push(e.id.as_str());
    }
    for (p, ids) in by_panel {
        md.push_str(&format!("- **{p}**: {}\n", ids.iter().map(|i| format!("`{i}`")).collect::<Vec<_>>().join(", ")));
    }
    if !cr.panics.is_empty() {
        md.push_str("\n## Panics\n\n");
        for (j, p) in &cr.panics {
            md.push_str(&format!("- `{j}`: {p}\n"));
        }
    }
    md
}

// ------------------------------------------------------------------ tests

#[test]
fn every_visible_element_and_menu_command_runs_without_a_panic() {
    let t0 = std::time::Instant::now();
    let cr = crawl(Scope { preps: false, menus: true, depth: 0, right: false }, "smoke");
    let clicked = cr.entries.values().filter(|e| e.effect.is_some()).count();
    eprintln!("{} jobs, {} elements ({clicked} clicked), {} menu commands in {:?}", cr.jobs, cr.entries.len(), cr.menus.len(), t0.elapsed());
    if let Some(dir) = map_dir() {
        write_map(&cr, &dir);
    }
    // the committed map should know everything on screen; say so (without failing) when it doesn't
    let committed = std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/ui-map.json")).unwrap_or_default();
    let known: BTreeSet<String> = serde_json::from_str::<Value>(&committed)
        .ok()
        .and_then(|m| m["elements"].as_array().map(|a| a.iter().filter_map(|e| e["id"].as_str().map(str::to_string)).collect()))
        .unwrap_or_default();
    let stale: Vec<&String> = cr.entries.keys().filter(|k| !known.contains(*k)).collect();
    if !stale.is_empty() {
        eprintln!("{} elements are not in docs/ui-map.json yet (regenerate it: {REGENERATE}): {:?}", stale.len(), stale.iter().take(20).collect::<Vec<_>>());
    }
    assert!(cr.entries.len() > 300 && clicked > 250, "the crawl found {} elements and clicked {clicked}", cr.entries.len());
    assert!(cr.panics.is_empty(), "panics:\n{}", cr.panics.iter().map(|(j, p)| format!("{j}: {p}")).collect::<Vec<_>>().join("\n"));
}

#[test]
#[ignore = "the full crawl (minutes); writes docs/ui-map.json and docs/ui-map.md"]
fn crawl_writes_the_ui_map() {
    let t0 = std::time::Instant::now();
    let cr = crawl(Scope { preps: true, menus: true, depth: 2, right: true }, "full");
    write_map(&cr, &map_dir().unwrap_or_else(|| Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs")));
    eprintln!(
        "{} jobs, {} elements, {} menu commands, {} reach errors in {:?}",
        cr.jobs,
        cr.entries.len(),
        cr.menus.len(),
        cr.reach_errors.len(),
        t0.elapsed()
    );
    for r in cr.reach_errors.iter().take(20) {
        eprintln!("  reach error: {r}");
    }
    assert!(cr.panics.is_empty(), "panics:\n{}", cr.panics.iter().map(|(j, p)| format!("{j}: {p}")).collect::<Vec<_>>().join("\n"));
}

#[test]
fn the_ui_map_is_well_formed() {
    let text = std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/ui-map.json")).expect("docs/ui-map.json");
    let map: Value = serde_json::from_str(&text).expect("ui-map.json parses");
    let els = map["elements"].as_array().expect("elements");
    assert!(els.len() > 500, "{} entries", els.len());
    let mut ids = BTreeSet::new();
    let mut last = String::new();
    for e in els {
        let id = e["id"].as_str().expect("id").to_string();
        for k in ["label", "panel", "kind", "effect"] {
            assert!(e[k].is_string(), "{id}: `{k}`");
        }
        assert!(e["reach"].is_array() && (e["command"].is_null() || e["command"].is_string()), "{id}");
        let key = if e["kind"] == "menu-item" && ids.contains(&id) { format!("{id} (menu)") } else { id.clone() };
        assert!(key > last, "sorted and unique: {key} after {last}");
        ids.insert(id);
        last = key;
    }
}
