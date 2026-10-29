//! The crash-recovery prompt, the Revert confirmation and File ▸ Import From ▸ DaVinci Resolve
//! Project (Settings ▸ Auto Save lives in `panels::settings`).
//!
//! Automation ids: `recovery.item.<n>`, `recovery.recover`, `recovery.discard`, `recovery.later`;
//! `revert.yes`, `revert.no`; `importResolve.item.<n>`, `importResolve.import`,
//! `importResolve.browse`, `importResolve.cancel`; while importing `importResolve.progress`,
//! `importResolve.cancelImport`.

use egui::{Color32, CornerRadius, Frame, Margin, RichText, Stroke, Ui, vec2};
use serde_json::json;

use crate::native_dialogs::FileDialog;
use crate::theme::Tokens;
use crate::{Dialog, FilmcraftApp};

/// Dialog-local state.
#[derive(Default)]
pub struct FileDialogState {
    pub recovery_choice: usize,
    /// The projects found in the DaVinci Resolve libraries (`file.listResolveProjects`), read
    /// once when the dialog opens.
    pub resolve_projects: Option<Vec<serde_json::Value>>,
    pub resolve_choice: usize,
    /// The running Resolve import was started here (the dialog), so the frame loop collects it.
    /// One started over the control channel or MCP is left to its client's
    /// `file.resolveImportStatus`.
    pub resolve_started: bool,
}

fn modal_frame(t: &Tokens) -> Frame {
    Frame::new().fill(t.panel_bg).stroke(Stroke::new(1.0, t.separator)).corner_radius(CornerRadius::same(10)).inner_margin(Margin::same(0))
}

fn button(app: &mut FilmcraftApp, ui: &mut Ui, id: &str, label: &str, primary: bool) -> bool {
    let t = app.tokens;
    let text = RichText::new(label).size(13.0).color(if primary { Color32::WHITE } else { t.text });
    let b = egui::Button::new(text)
        .min_size(vec2(88.0, 30.0))
        .corner_radius(CornerRadius::same(15))
        .fill(if primary { t.accent } else { Color32::TRANSPARENT })
        .stroke(if primary { Stroke::NONE } else { Stroke::new(1.5, t.text_faint) });
    let r = ui.add(b).on_hover_cursor(egui::CursorIcon::PointingHand);
    app.auto.add(id, r.rect, label);
    r.clicked()
}

pub fn show_recovery(app: &mut FilmcraftApp, ctx: &egui::Context) -> bool {
    let t = app.tokens;
    let list = app.session.execute("file.recoveryList", json!({})).unwrap_or_default();
    let items = list.as_array().cloned().unwrap_or_default();
    if items.is_empty() {
        return false;
    }
    let mut choice = app.file_dialogs.recovery_choice.min(items.len() - 1);
    let mut open = true;
    egui::Modal::new(egui::Id::unique("recovery-modal")).frame(modal_frame(&t)).show(ctx, |ui| {
        ui.set_width(520.0);
        Frame::new().inner_margin(Margin { left: 28, right: 28, top: 24, bottom: 20 }).show(ui, |ui| {
            ui.label(RichText::new(tl!("Recover Unsaved Changes")).size(18.0).strong().color(t.text));
            ui.add_space(6.0);
            ui.separator();
            ui.add_space(6.0);
            let c = &items[choice];
            let name = c["projectName"].as_str().unwrap_or(tl!("Untitled"));
            let when = c["savedAt"].as_str().unwrap_or("");
            let why = if c["cleanExit"].as_bool() == Some(true) {
                tlf!("FilmCraft was closed while “{name}” had unsaved changes.", name)
            } else {
                tlf!("FilmCraft quit unexpectedly while “{name}” had unsaved changes.", name)
            };
            ui.label(RichText::new(why).size(13.5).color(t.text));
            ui.add_space(4.0);
            ui.label(RichText::new(tlf!("Recover unsaved changes from {when}?", when)).size(13.5).color(t.text));
            if let Some(p) = c["projectPath"].as_str() {
                ui.label(RichText::new(p).size(11.5).color(t.text_dim));
            } else {
                ui.label(RichText::new(tl!("The project had not been saved yet.")).size(11.5).color(t.text_dim));
            }
            if items.len() > 1 {
                ui.add_space(8.0);
                ui.label(RichText::new(tlf!("{n} sessions have unsaved changes:", n = items.len())).size(12.0).color(t.text_dim));
                for (i, it) in items.iter().enumerate() {
                    let label = format!("{} — {}", it["projectName"].as_str().unwrap_or(tl!("Untitled")), it["savedAt"].as_str().unwrap_or(""));
                    let r = ui.radio(choice == i, RichText::new(&label).size(12.5));
                    app.auto.add(&format!("recovery.item.{i}"), r.rect, &label);
                    if r.clicked() {
                        choice = i;
                    }
                }
            }
            ui.add_space(18.0);
            let id = items[choice]["id"].as_str().unwrap_or("").to_string();
            ui.horizontal(|ui| {
                if button(app, ui, "recovery.discard", tl!("Discard"), false) {
                    if let Err(e) = app.session.execute("file.discardRecovery", json!({ "id": id })) {
                        app.ui.status = e.to_string();
                    }
                    choice = 0;
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if button(app, ui, "recovery.recover", tl!("Recover"), true) {
                        match app.session.execute("file.recover", json!({ "id": id })) {
                            Ok(_) => open = false,
                            Err(e) => app.ui.status = e.to_string(),
                        }
                    }
                    if button(app, ui, "recovery.later", tl!("Not Now"), false) {
                        open = false;
                    }
                });
            });
        });
    });
    app.file_dialogs.recovery_choice = choice;
    open && !app.session.recovery_candidates().is_empty()
}

pub fn show_revert(app: &mut FilmcraftApp, ctx: &egui::Context) -> bool {
    let t = app.tokens;
    let file = app.session.path.as_deref().and_then(|p| std::path::Path::new(p).file_name()).map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let mut open = true;
    let r = egui::Modal::new(egui::Id::unique("revert-modal")).frame(modal_frame(&t)).show(ctx, |ui| {
        ui.set_width(460.0);
        Frame::new().inner_margin(Margin { left: 28, right: 28, top: 24, bottom: 20 }).show(ui, |ui| {
            ui.label(RichText::new(tl!("Revert")).size(18.0).strong().color(t.text));
            ui.add_space(6.0);
            ui.separator();
            ui.add_space(6.0);
            ui.label(RichText::new(tlf!("Are you sure you want to discard your changes to '{file}'?", file)).size(13.5).color(t.text));
            ui.add_space(18.0);
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if button(app, ui, "revert.yes", tl!("Yes"), true) {
                    if let Err(e) = app.session.execute("file.revert", json!({})) {
                        app.ui.status = e.to_string();
                    }
                    open = false;
                }
                if button(app, ui, "revert.no", tl!("No"), false) {
                    open = false;
                }
            });
        });
    });
    // (the modal's own close also fires on Escape: not while a drag is being cancelled, #580)
    open && !(r.should_close() && !ctx.input(|i| i.pointer.any_down()))
}

/// Start importing a Resolve project in the background; this dialog shows its progress until
/// [`finish_resolve_import`] reports the result.
fn import_resolve(app: &mut FilmcraftApp, path: &str) -> Result<serde_json::Value, String> {
    let r = app.session.execute("file.importResolve", json!({ "path": path, "background": true })).map_err(|e| e.to_string());
    match &r {
        Ok(v) if v["started"] == json!(true) => {
            app.file_dialogs.resolve_started = true;
            app.dialog = Some(Dialog::ImportResolve);
        }
        // finished at once (no worker threads): report it now
        _ => finish_resolve_import(app, r.clone()),
    }
    r
}

/// Collect a background Resolve import this app started that has finished (called every frame)
/// and say how it went on the status line.
pub fn poll_resolve_import(app: &mut FilmcraftApp, ctx: &egui::Context) {
    if app.session.resolve_import.is_none() {
        // (collected elsewhere, if at all: the next one may be a client's)
        app.file_dialogs.resolve_started = false;
        return;
    }
    if !app.file_dialogs.resolve_started {
        return;
    }
    match app.session.poll_resolve_import() {
        Some(r) => {
            app.file_dialogs.resolve_started = false;
            finish_resolve_import(app, r.map_err(|e| e.to_string()));
            if app.dialog == Some(Dialog::ImportResolve) {
                app.dialog = None;
            }
            app.file_dialogs.resolve_projects = None;
        }
        None => ctx.request_repaint_after(std::time::Duration::from_millis(100)),
    }
}

/// Say how an import went on the status line.
fn finish_resolve_import(app: &mut FilmcraftApp, r: Result<serde_json::Value, String>) {
    app.ui.status = match &r {
        Ok(v) => {
            let timelines = v["sequences"].as_array().map_or(0, Vec::len);
            let offline = v["offlineMedia"].as_array().map_or(0, Vec::len);
            let notes: Vec<&str> = v["report"].as_array().map(|a| a.iter().filter_map(serde_json::Value::as_str).collect()).unwrap_or_default();
            let mut msg = tlf!("Imported {timelines} timeline(s) from DaVinci Resolve", timelines);
            if offline > 0 {
                msg.push_str(&tlf!("; {offline} media file(s) are offline", offline));
            }
            if let Some(first) = notes.first() {
                msg.push_str("; ");
                msg.push_str(first);
                if notes.len() > 1 {
                    msg.push_str(&tlf!(" (+{more} more notes)", more = notes.len() - 1));
                }
            }
            msg
        }
        Err(e) => e.clone(),
    };
    // the dialog has gone: say so where it can't be missed
    let now = app.now();
    app.toast = Some((app.ui.status.clone(), now));
}

/// The import running in the background: what it is doing, a progress bar and Cancel.
fn show_resolve_progress(app: &mut FilmcraftApp, ctx: &egui::Context) -> bool {
    let t = app.tokens;
    let Some(running) = &app.session.resolve_import else { return false };
    let name = std::path::Path::new(&running.path).parent().and_then(|p| p.file_name()).map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let p = &running.progress;
    let (done, total) = (p.done.load(std::sync::atomic::Ordering::Relaxed), p.total.load(std::sync::atomic::Ordering::Relaxed));
    let stage = p.stage.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone();
    let cancelling = p.cancel.load(std::sync::atomic::Ordering::Relaxed);
    let mut cancel = false;
    // (its own id: the list's modal would keep the list's height)
    egui::Modal::new(egui::Id::unique("import-resolve-progress")).frame(modal_frame(&t)).show(ctx, |ui| {
        ui.set_width(460.0);
        Frame::new().inner_margin(Margin { left: 28, right: 28, top: 24, bottom: 20 }).show(ui, |ui| {
            ui.label(RichText::new(tlf!("Importing “{name}”", name)).size(18.0).strong().color(t.text));
            ui.add_space(10.0);
            let (line, fraction) = if cancelling {
                (tl!("Cancelling…").to_string(), None)
            } else if stage == "Linking media" && total > 0 {
                // (an empty bar animates until the first file is in)
                (tlf!("Linking media {done} of {total}", done, total), (done > 0).then(|| done as f32 / total as f32))
            } else {
                (tl!("Reading the project…").to_string(), None)
            };
            ui.horizontal(|ui| {
                ui.add(egui::Spinner::new().size(14.0));
                ui.label(RichText::new(line).size(13.0).color(t.text));
            });
            ui.add_space(8.0);
            let bar = egui::ProgressBar::new(fraction.unwrap_or(0.0)).desired_width(ui.available_width());
            let r = ui.add(if fraction.is_none() { bar.animate(true) } else { bar });
            app.auto.add("importResolve.progress", r.rect, &format!("{done}/{total}"));
            ui.add_space(16.0);
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if !cancelling && button(app, ui, "importResolve.cancelImport", tl!("Cancel"), false) {
                    cancel = true;
                }
            });
        });
    });
    if cancel {
        app.session.cancel_resolve_import();
    }
    ctx.request_repaint_after(std::time::Duration::from_millis(100));
    // stays up until the frame loop collects the result
    true
}

/// File ▸ Import From ▸ DaVinci Resolve Project…: the projects in the local Resolve project
/// libraries, newest first, or any project folder chosen with Browse…. While one is being
/// imported, its progress.
pub fn show_import_resolve(app: &mut FilmcraftApp, ctx: &egui::Context) -> bool {
    if app.session.resolve_import.is_some() {
        if app.file_dialogs.resolve_started {
            return show_resolve_progress(app, ctx);
        }
        // a control-channel or MCP client's import: its result is the client's to collect, so
        // its progress (which only that client ends) must not hold the user here
        app.ui.status = tl!("A DaVinci Resolve import is already running.").to_string();
        return false;
    }
    let t = app.tokens;
    if app.file_dialogs.resolve_projects.is_none() {
        let list = app.session.execute("file.listResolveProjects", json!({})).unwrap_or_default();
        app.file_dialogs.resolve_projects = Some(list["projects"].as_array().cloned().unwrap_or_default());
        app.file_dialogs.resolve_choice = 0;
    }
    let projects = app.file_dialogs.resolve_projects.clone().unwrap_or_default();
    let mut choice = app.file_dialogs.resolve_choice.min(projects.len().saturating_sub(1));
    let mut open = true;
    let mut import: Option<String> = None;
    let mut browse = false;
    let r = egui::Modal::new(egui::Id::unique("import-resolve-modal")).frame(modal_frame(&t)).show(ctx, |ui| {
        ui.set_width(560.0);
        Frame::new().inner_margin(Margin { left: 28, right: 28, top: 24, bottom: 20 }).show(ui, |ui| {
            ui.label(RichText::new(tl!("Import DaVinci Resolve Project")).size(18.0).strong().color(t.text));
            ui.add_space(6.0);
            ui.separator();
            ui.add_space(6.0);
            if projects.is_empty() {
                ui.label(
                    RichText::new(tl!("No projects were found in the DaVinci Resolve project library on this computer. Choose a project folder with Browse…."))
                        .size(13.0)
                        .color(t.text),
                );
            } else {
                ui.label(RichText::new(tl!("Choose a project from your DaVinci Resolve project library:")).size(13.0).color(t.text));
                ui.add_space(6.0);
                egui::ScrollArea::vertical().max_height(300.0).auto_shrink([false, true]).show(ui, |ui| {
                    for (i, p) in projects.iter().enumerate() {
                        let name = p["name"].as_str().unwrap_or("?");
                        let mut detail: Vec<String> = Vec::new();
                        if let Some(folders) = p["folder"].as_array().filter(|f| !f.is_empty()) {
                            detail.push(folders.iter().filter_map(|f| f.as_str()).collect::<Vec<_>>().join(" ▸ "));
                        }
                        if let Some(u) = p["user"].as_str().filter(|u| !u.is_empty()) {
                            detail.push(tlf!("user {u}", u));
                        }
                        if let Some(m) = p["modified"].as_u64() {
                            detail.push(tlf!("modified {when}", when = filmcraft_engine::media_browser::format_date(m)));
                        }
                        let text = RichText::new(format!("{name}\n{}", detail.join("  ·  "))).size(12.5).color(t.text);
                        let r = ui.add(egui::Button::selectable(choice == i, text).min_size(vec2(ui.available_width(), 38.0)));
                        app.auto.add(&format!("importResolve.item.{i}"), r.rect, name);
                        if r.clicked() {
                            choice = i;
                        }
                        if r.double_clicked() {
                            import = p["path"].as_str().map(str::to_string);
                        }
                    }
                });
            }
            ui.add_space(8.0);
            ui.label(
                RichText::new(tl!("Timelines, bins, clips, tracks and transitions are imported. Effects, grades, titles and speed changes stay in Resolve."))
                    .size(11.5)
                    .color(t.text_dim),
            );
            ui.add_space(14.0);
            ui.horizontal(|ui| {
                if button(app, ui, "importResolve.browse", tl!("Browse…"), false) {
                    browse = true;
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if !projects.is_empty() && button(app, ui, "importResolve.import", tl!("Import"), true) {
                        import = projects.get(choice).and_then(|p| p["path"].as_str()).map(str::to_string);
                    }
                    if button(app, ui, "importResolve.cancel", tl!("Cancel"), false) {
                        open = false;
                    }
                });
            });
        });
    });
    app.file_dialogs.resolve_choice = choice;
    if let Some(path) = import {
        // the dialog turns into the import's progress
        let _ = import_resolve(app, &path);
        return app.dialog == Some(Dialog::ImportResolve) && app.session.resolve_import.is_some();
    } else if browse {
        // this dialog closes; the folder picker carries on (now, or on a later frame)
        open = false;
        if let Err(e) = app.pick(FileDialog::Folder { at: None }, |app, paths| {
            let Some(path) = paths.into_iter().next() else { return Ok(serde_json::Value::Null) };
            import_resolve(app, &path)
        }) {
            app.ui.status = e;
        }
        // a host whose picker answers at once has started the import already: show its progress
        if app.dialog == Some(Dialog::ImportResolve) && app.session.resolve_import.is_some() {
            return true;
        }
    }
    if !open {
        app.file_dialogs.resolve_projects = None;
    }
    // (the modal closes on Escape too, but not while a drag is being cancelled, #580)
    open && (!r.should_close() || ctx.input(|i| i.pointer.any_down()))
}

/// Draw the dialog if it is one of ours; returns Some(still open).
pub fn show(app: &mut FilmcraftApp, ctx: &egui::Context, d: Dialog) -> Option<bool> {
    match d {
        Dialog::Preferences => Some(crate::panels::settings::show(app, ctx)),
        Dialog::Recovery => Some(show_recovery(app, ctx)),
        Dialog::RevertConfirm => Some(show_revert(app, ctx)),
        Dialog::ImportResolve => Some(show_import_resolve(app, ctx)),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use filmcraft_engine::Session;

    /// Frames for two seconds: the worker reports a missing database long before that.
    fn frames(app: &mut FilmcraftApp, ctx: &egui::Context) {
        for _ in 0..200 {
            poll_resolve_import(app, ctx);
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    }

    /// An import a control-channel or MCP client started is the client's to collect: the frame
    /// loop leaves its result to `file.resolveImportStatus`. One the dialog started, it collects.
    #[test]
    fn the_frame_loop_collects_only_the_imports_it_started() {
        let ctx = egui::Context::default();
        let missing = std::env::temp_dir().join("filmcraft-no-such-resolve-project").join("Project.db");
        let missing = missing.to_string_lossy().into_owned();
        let mut app = FilmcraftApp::new(Session::default());
        let r = app.session.execute("file.importResolve", json!({ "path": missing, "background": true })).unwrap();
        assert_eq!(r["started"], json!(true));
        frames(&mut app, &ctx);
        assert!(app.session.resolve_import.is_some(), "the client's result is still waiting");
        // the dialog, opened meanwhile, does not trap the user in that import's progress
        app.dialog = Some(Dialog::ImportResolve);
        assert!(!show_import_resolve(&mut app, &ctx), "the dialog closes");
        assert!(app.ui.status.contains("already running"), "{}", app.ui.status);
        assert!(app.session.resolve_import.is_some(), "and leaves the result to the client");
        let e = app.session.execute("file.resolveImportStatus", json!({})).unwrap_err().to_string();
        assert!(e.contains("Project.db"), "{e}");
        // the dialog's own import
        let r = import_resolve(&mut app, &missing).unwrap();
        assert_eq!(r["started"], json!(true));
        frames(&mut app, &ctx);
        assert!(app.session.resolve_import.is_none(), "collected by the frame loop");
        assert!(!app.file_dialogs.resolve_started);
        assert!(app.ui.status.contains("Project.db"), "{}", app.ui.status);
    }
}
