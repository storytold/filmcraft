//! The remote storage dialog: pick a project to open, media to import, or a place to save, on
//! the host's remote mounts (`HostHooks::remote`, the web build's WebDAV mounts). It lists with
//! the Media Browser's directory listing (`filmcraft_engine::media_browser::list`), so a listing
//! that is still loading is retried on the next frames.
//!
//! Automation ids: `remoteDialog.crumb.<n>`, `remoteDialog.volume.<name>`,
//! `remoteDialog.entry.<name>`, `remoteDialog.filename`, `remoteDialog.accept`,
//! `remoteDialog.cancel`.

use std::time::Duration;

use egui::{Frame, Margin, RichText};
use filmcraft_engine::media_browser::{self as mb, Volume, VolumeKind};
use serde_json::{Value, json};

use crate::FilmcraftApp;
use crate::native_dialogs::Continuation;
use crate::panels::file_dialogs::{button, modal_frame};

/// What the dialog chooses.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RemoteMode {
    /// One project file to open.
    OpenProject,
    /// One or more media (or interchange) files to import.
    Import,
    /// A folder and a project file name (`.fcproj` is enforced), suggesting `name`.
    Save { name: String },
}

/// Dialog state, kept in [`crate::panels::file_dialogs::FileDialogState`] while it is open.
pub struct RemoteDialog {
    mode: RemoteMode,
    /// The folder shown; empty shows the mounts.
    dir: String,
    selected: Vec<String>,
    name: String,
    then: Continuation,
}

/// The path to save to: `name` in `dir`, with `.fcproj` added. Refuses names that are empty, `.`,
/// `..`, or hold a separator or control character, so a typed name never leaves the folder.
pub fn save_target(dir: &str, name: &str) -> Result<String, String> {
    let name = name.trim();
    if dir.is_empty() {
        return Err(tl!("Choose a folder first").into());
    }
    if name.is_empty() || name == "." || name == ".." || name.chars().any(|c| c == '/' || c == '\\' || c.is_control()) {
        return Err(tl!("Enter a valid file name").into());
    }
    let name = if name.to_ascii_lowercase().ends_with(".fcproj") { name.to_string() } else { format!("{name}.fcproj") };
    Ok(mb::join(dir, &name))
}

/// The remote mounts offered as volumes in the dialog.
fn mounts(app: &FilmcraftApp) -> Vec<Volume> {
    app.session.services.volumes().into_iter().filter(|v| v.kind == VolumeKind::Network).collect()
}

/// Show the dialog; `then` gets the chosen paths when the user accepts (not when they cancel).
pub(crate) fn open(app: &mut FilmcraftApp, mode: RemoteMode, then: Continuation) -> Result<Value, String> {
    if app.hooks.remote.is_none() {
        return Err(tl!("remote storage is not configured").into());
    }
    if app.file_dialogs.remote.is_some() {
        return Err(tl!("A file dialog is already open").into());
    }
    let vols = mounts(app);
    let dir = if let [only] = vols.as_slice() { only.path.clone() } else { String::new() };
    let name = if let RemoteMode::Save { name } = &mode { name.clone() } else { String::new() };
    app.file_dialogs.remote = Some(RemoteDialog { mode, dir, selected: Vec::new(), name, then });
    Ok(json!({"dialog": "remote"}))
}

/// File ▸ Open Remote Project…, Import from Remote… and Save As Remote…: the commands that show the dialog.
pub(crate) fn command(app: &mut FilmcraftApp, id: &str, params: &Value) -> Option<Result<Value, String>> {
    match id {
        "file.openRemote" => Some(app.pick_remote(RemoteMode::OpenProject, |app, paths| match paths.first() {
            Some(p) => app.open_project_path(p),
            None => Ok(Value::Null),
        })),
        // File ▸ Save As Remote…: choose the place, then the usual Save As to that path
        "file.saveAsRemote" => {
            let name = format!("{}.fcproj", app.session.project.name);
            Some(app.pick_remote(RemoteMode::Save { name }, |app, paths| match paths.first() {
                Some(p) => app.session.execute("file.saveAs", json!({"path": p})).map_err(|e| e.to_string()),
                None => Ok(Value::Null),
            }))
        }
        "file.importRemote" => {
            let bin = params.get("bin").cloned();
            Some(app.pick_remote(RemoteMode::Import, move |app, paths| app.import_picked(bin, paths)))
        }
        _ => None,
    }
}

/// The breadcrumb trail of `dir`: (label, path) from its mount down.
fn crumbs(dir: &str, vols: &[Volume]) -> Vec<(String, String)> {
    let Some(v) = vols.iter().find(|v| dir == v.path || dir.strip_prefix(&v.path).is_some_and(|r| r.starts_with('/'))) else { return Vec::new() };
    let mut out = vec![(v.name.clone(), v.path.clone())];
    let mut at = v.path.clone();
    for seg in dir.strip_prefix(&v.path).unwrap_or("").split('/').filter(|s| !s.is_empty()) {
        at = mb::join(&at, seg);
        out.push((seg.to_string(), at.clone()));
    }
    out
}

/// Draw the dialog when one is open (called with the other dialogs every frame).
pub fn show(app: &mut FilmcraftApp, ctx: &egui::Context) {
    let Some(mut st) = app.file_dialogs.remote.take() else { return };
    let t = app.tokens;
    let vols = mounts(app);
    let filter = if st.mode == RemoteMode::Import { "all" } else { "project" };
    let listing = (!st.dir.is_empty()).then(|| mb::list(&*app.session.services, &st.dir, filter));
    let multi = st.mode == RemoteMode::Import;
    let (mut go, mut accept, mut cancel) = (None::<String>, false, false);
    let r = egui::Modal::new(egui::Id::unique("remote-modal")).frame(modal_frame(&t)).show(ctx, |ui| {
        ui.set_width(560.0);
        Frame::new().inner_margin(Margin { left: 24, right: 24, top: 20, bottom: 16 }).show(ui, |ui| {
            let (title, action) = match &st.mode {
                RemoteMode::OpenProject => (tl!("Open Remote Project"), tl!("Open")),
                RemoteMode::Import => (tl!("Import from Remote"), tl!("Import")),
                RemoteMode::Save { .. } => (tl!("Save to Remote"), tl!("Save")),
            };
            ui.label(RichText::new(title).size(18.0).strong().color(t.text));
            ui.add_space(6.0);
            ui.horizontal_wrapped(|ui| {
                let root = ui.link(tl!("Remote"));
                app.auto.add("remoteDialog.crumb.0", root.rect, "Remote");
                if root.clicked() {
                    go = Some(String::new());
                }
                for (i, (label, path)) in crumbs(&st.dir, &vols).into_iter().enumerate() {
                    ui.label("›");
                    let c = ui.link(&label);
                    app.auto.add(&format!("remoteDialog.crumb.{}", i + 1), c.rect, &label);
                    if c.clicked() {
                        go = Some(path);
                    }
                }
            });
            ui.separator();
            egui::ScrollArea::vertical().max_height(300.0).auto_shrink([false, false]).show(ui, |ui| match &listing {
                None => {
                    for v in &vols {
                        let r = ui.selectable_label(false, &v.name);
                        app.auto.add(&format!("remoteDialog.volume.{}", v.name), r.rect, &v.name);
                        if r.clicked() {
                            go = Some(v.path.clone());
                        }
                    }
                    if vols.is_empty() {
                        ui.label(tl!("No remote storage"));
                    }
                }
                Some(Ok(entries)) => {
                    for e in entries {
                        let label = if e.is_dir { format!("{}/", e.name) } else { e.name.clone() };
                        let r = ui.selectable_label(st.selected.contains(&e.path), label);
                        app.auto.add(&format!("remoteDialog.entry.{}", e.name), r.rect, &e.name);
                        if r.clicked() && e.is_dir {
                            go = Some(e.path.clone());
                        } else if r.clicked() {
                            if multi {
                                match st.selected.iter().position(|p| *p == e.path) {
                                    Some(i) => drop(st.selected.remove(i)),
                                    None => st.selected.push(e.path.clone()),
                                }
                            } else {
                                st.selected = vec![e.path.clone()];
                                if matches!(st.mode, RemoteMode::Save { .. }) {
                                    st.name = e.name.clone();
                                }
                            }
                        }
                        if r.double_clicked() && !e.is_dir && !multi {
                            accept = true;
                        }
                    }
                    if entries.is_empty() {
                        ui.label(tl!("Nothing here"));
                    }
                }
                Some(Err(e)) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    ui.label(tl!("Loading…"));
                    ctx.request_repaint_after(Duration::from_millis(100));
                }
                Some(Err(e)) => {
                    ui.label(RichText::new(e.to_string()).color(t.text_dim));
                }
            });
            ui.separator();
            let saving = matches!(st.mode, RemoteMode::Save { .. });
            let mut ready = saving || !st.selected.is_empty();
            if saving {
                let f = ui.add(egui::TextEdit::singleline(&mut st.name).desired_width(f32::INFINITY));
                app.auto.add("remoteDialog.filename", f.rect, &st.name);
                if let Err(e) = save_target(&st.dir, &st.name) {
                    ready = false;
                    ui.label(RichText::new(e).size(11.5).color(t.text_dim));
                }
            }
            ui.add_space(8.0);
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.add_enabled_ui(ready, |ui| accept |= button(app, ui, "remoteDialog.accept", action, true));
                cancel |= button(app, ui, "remoteDialog.cancel", tl!("Cancel"), false);
            });
        });
    });
    if let Some(d) = go {
        st.dir = d;
        st.selected.clear();
    }
    let escaped = r.should_close() && !ctx.input(|i| i.pointer.any_down());
    if cancel || escaped {
        return;
    }
    if !accept {
        app.file_dialogs.remote = Some(st);
        return;
    }
    let paths = match &st.mode {
        RemoteMode::Save { .. } => match save_target(&st.dir, &st.name) {
            Ok(p) => vec![p],
            Err(e) => {
                app.ui.status = e;
                app.file_dialogs.remote = Some(st);
                return;
            }
        },
        _ => st.selected.clone(),
    };
    if let Err(e) = (st.then)(app, paths) {
        app.ui.status = e;
    }
}
