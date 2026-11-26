//! The crash-recovery prompt, the Revert confirmation and the replace prompt for a save dialog's
//! answer that got its extension added (`native_dialogs`). Settings ▸ Auto Save lives in
//! `panels::settings`.
//!
//! Automation ids: `recovery.item.<n>`, `recovery.recover`, `recovery.discard`, `recovery.later`;
//! `revert.yes`, `revert.no`; `overwrite.replace`, `overwrite.cancel`.

use egui::{Color32, CornerRadius, Frame, Margin, RichText, Stroke, Ui, vec2};
use serde_json::json;
use std::collections::{HashMap, HashSet};

use crate::theme::Tokens;
use crate::{Dialog, FilmcraftApp};

/// Dialog-local state.
#[derive(Default)]
pub struct FileDialogState {
    pub recovery_choice: usize,
    pub recovery_content_checked: HashSet<String>,
    pub recovery_content_changed: HashSet<String>,
    pub recovery_content_pending: HashSet<String>,
    recovery_confirmation_id: Option<String>,
    recovery_confirmation_key: Option<String>,
    recovery_content_confirmed: HashSet<String>,
}

fn recovery_check_results_key() -> egui::Id {
    egui::Id::unique("recovery-content-check-results")
}

pub(crate) fn modal_frame(t: &Tokens) -> Frame {
    Frame::new().fill(t.panel_bg).stroke(Stroke::new(1.0, t.separator)).corner_radius(CornerRadius::same(10)).inner_margin(Margin::same(0))
}

fn is_view_only(item: &serde_json::Value) -> bool {
    item["dirty"].as_bool() == Some(false) && item["viewDirty"].as_bool() == Some(true)
}

pub(crate) fn button(app: &mut FilmcraftApp, ui: &mut Ui, id: &str, label: &str, primary: bool) -> bool {
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

fn recovery_content_changed(candidate: &filmcraft_engine::autosave::RecoveryCandidate) -> bool {
    candidate.meta.project_path.as_deref().is_some_and(|path| !path.is_empty() && filmcraft_engine::autosave::view_recovery_project_changed(candidate))
}

fn recovery_cache_key(candidate: &filmcraft_engine::autosave::RecoveryCandidate) -> String {
    let generation = candidate.meta.project_path.as_deref().and_then(|path| std::fs::metadata(path).ok()).map_or_else(
        || "missing".to_string(),
        |metadata| {
            let modified = metadata.modified().ok().and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok()).map_or(0, |time| time.as_nanos());
            format!("{}:{modified}", metadata.len())
        },
    );
    format!("{}:{}:{:?}:{generation}", candidate.id, candidate.meta.revision, candidate.meta.snapshot_fingerprint.unwrap_or_default())
}

fn recovery_check_can_start(cache_key: &str, selected_key: Option<&str>, selected_waiting: bool, pending: &HashSet<String>) -> bool {
    pending.is_empty() && (!selected_waiting || selected_key == Some(cache_key))
}

fn recovery_check_result(check: impl FnOnce() -> bool) -> bool {
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(check)) {
        Ok(changed) => changed,
        Err(_) => {
            log::error!("recovery project content comparison panicked; requiring confirmation");
            true
        }
    }
}

fn retire_completed_recovery_checks(state: &mut FileDialogState, ctx: &egui::Context) {
    let completed = ctx.data_mut(|data| {
        let key = recovery_check_results_key();
        let results = data.get_temp::<HashMap<String, bool>>(key).unwrap_or_default();
        data.insert_temp(key, HashMap::<String, bool>::new());
        results
    });
    for (key, changed) in completed {
        state.recovery_content_pending.remove(&key);
        state.recovery_content_checked.insert(key.clone());
        if changed {
            state.recovery_content_changed.insert(key);
        }
    }
}

pub fn show_recovery(app: &mut FilmcraftApp, ctx: &egui::Context) -> bool {
    let t = app.tokens;
    let list = app.session.execute("file.recoveryList", json!({})).unwrap_or_default();
    let mut items = list.as_array().cloned().unwrap_or_default();
    if items.is_empty() {
        return false;
    }
    let candidates = app.session.recovery_candidates().to_vec();
    let selected_id = items.get(app.file_dialogs.recovery_choice.min(items.len() - 1)).and_then(|item| item["id"].as_str());
    let selected_key = selected_id
        .and_then(|id| candidates.iter().find(|candidate| candidate.id == id))
        .filter(|candidate| items.iter().any(|item| item["id"].as_str() == Some(candidate.id.as_str()) && is_view_only(item)))
        .map(recovery_cache_key);
    let selected_waiting = selected_key
        .as_ref()
        .is_some_and(|key| !app.file_dialogs.recovery_content_checked.contains(key) && !app.file_dialogs.recovery_content_pending.contains(key));
    // Retire finished jobs independently of the current candidate list and cache keys. A file
    // changing during a check gives it a new key; leaving the old key pending would block every
    // later serial check.
    retire_completed_recovery_checks(&mut app.file_dialogs, ctx);
    if let Some(id) = app.file_dialogs.recovery_confirmation_id.clone()
        && let Some(candidate) = candidates.iter().find(|candidate| candidate.id == id)
    {
        let key = recovery_cache_key(candidate);
        if app.file_dialogs.recovery_confirmation_key.as_deref() != Some(key.as_str()) {
            app.file_dialogs.recovery_confirmation_id = None;
            app.file_dialogs.recovery_confirmation_key = None;
        } else if app.file_dialogs.recovery_content_checked.contains(&key) && !app.file_dialogs.recovery_content_pending.contains(&key) {
            let changed = app.file_dialogs.recovery_content_changed.contains(&key);
            let confirmed = app.file_dialogs.recovery_content_confirmed.remove(&key);
            app.file_dialogs.recovery_confirmation_id = None;
            app.file_dialogs.recovery_confirmation_key = None;
            if !changed || confirmed {
                match app.session.execute("file.recover", json!({ "id": id })) {
                    Ok(_) => return false,
                    Err(e) => app.ui.status = e.to_string(),
                }
            }
        }
    }
    for item in &mut items {
        if !is_view_only(item) {
            continue;
        }
        let Some(id) = item["id"].as_str() else { continue };
        let Some(candidate) = candidates.iter().find(|candidate| candidate.id == id) else { continue };
        let cache_key = recovery_cache_key(candidate);
        if !app.file_dialogs.recovery_content_checked.contains(&cache_key)
            && recovery_check_can_start(&cache_key, selected_key.as_deref(), selected_waiting, &app.file_dialogs.recovery_content_pending)
            && app.file_dialogs.recovery_content_pending.insert(cache_key.clone())
        {
            let candidate = candidate.clone();
            let thread_ctx = ctx.clone();
            let thread_key = cache_key.clone();
            let task = std::thread::Builder::new().name("filmcraft-recovery-check".into()).spawn(move || {
                let changed = recovery_check_result(|| recovery_content_changed(&candidate));
                thread_ctx.data_mut(|data| {
                    let key = recovery_check_results_key();
                    let mut results = data.get_temp::<HashMap<String, bool>>(key).unwrap_or_default();
                    results.insert(thread_key, changed);
                    data.insert_temp(key, results);
                });
                thread_ctx.request_repaint();
            });
            if task.is_err() {
                app.file_dialogs.recovery_content_pending.remove(&cache_key);
                app.file_dialogs.recovery_content_checked.insert(cache_key.clone());
                app.file_dialogs.recovery_content_changed.insert(cache_key.clone());
            }
        }
        if app.file_dialogs.recovery_content_changed.contains(&cache_key) {
            item["dirty"] = json!(true);
            item["contentChanged"] = json!(true);
        }
        if app.file_dialogs.recovery_content_pending.contains(&cache_key) {
            item["contentCheckPending"] = json!(true);
        }
    }
    let mut choice = app.file_dialogs.recovery_choice.min(items.len() - 1);
    let mut open = true;
    egui::Modal::new(egui::Id::unique("recovery-modal")).frame(modal_frame(&t)).show(ctx, |ui| {
        ui.set_width(520.0);
        Frame::new().inner_margin(Margin { left: 28, right: 28, top: 24, bottom: 20 }).show(ui, |ui| {
            let c = &items[choice];
            let view_only = is_view_only(c);
            let title = if view_only { tl!("Recover Timeline View Settings") } else { tl!("Recover Unsaved Changes") };
            let title_response = ui.label(RichText::new(title).size(18.0).strong().color(t.text));
            app.auto.add("recovery.title", title_response.rect, title);
            ui.add_space(6.0);
            ui.separator();
            ui.add_space(6.0);
            let name = c["projectName"].as_str().unwrap_or(tl!("Untitled"));
            let when = c["savedAt"].as_str().unwrap_or("");
            let why = if view_only && c["cleanExit"].as_bool() == Some(true) {
                tlf!("FilmCraft was closed with unsaved Timeline view settings for “{name}”.", name)
            } else if view_only {
                tlf!("FilmCraft quit unexpectedly with unsaved Timeline view settings for “{name}”.", name)
            } else if c["cleanExit"].as_bool() == Some(true) {
                tlf!("FilmCraft was closed while “{name}” had unsaved changes.", name)
            } else {
                tlf!("FilmCraft quit unexpectedly while “{name}” had unsaved changes.", name)
            };
            ui.label(RichText::new(why).size(13.5).color(t.text));
            ui.add_space(4.0);
            let recovery_prompt =
                if view_only { tlf!("Recover Timeline view settings from {when}?", when) } else { tlf!("Recover unsaved changes from {when}?", when) };
            ui.label(RichText::new(recovery_prompt).size(13.5).color(t.text));
            if c["contentChanged"].as_bool() == Some(true) {
                ui.add_space(4.0);
                ui.label(
                    RichText::new(tl!("The project file has changed since this recovery snapshot. Recovering will replace its current contents."))
                        .size(12.5)
                        .color(t.text),
                );
            }
            if let Some(p) = c["projectPath"].as_str() {
                ui.label(RichText::new(p).size(11.5).color(t.text_dim));
            } else {
                ui.label(RichText::new(tl!("The project had not been saved yet.")).size(11.5).color(t.text_dim));
            }
            if items.len() > 1 {
                ui.add_space(8.0);
                let view_only_count = items.iter().filter(|item| is_view_only(item)).count();
                let heading = if view_only_count == items.len() {
                    tlf!("{n} sessions have unsaved Timeline view settings:", n = view_only_count)
                } else if view_only_count == 0 {
                    tlf!("{n} sessions have unsaved changes:", n = items.len())
                } else {
                    tlf!("{n} recovery sessions ({view_only} with unsaved Timeline view settings):", n = items.len(), view_only = view_only_count)
                };
                let heading_response = ui.label(RichText::new(&heading).size(12.0).color(t.text_dim));
                app.auto.add("recovery.heading", heading_response.rect, &heading);
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
            let view_only = is_view_only(&items[choice]);
            ui.horizontal(|ui| {
                let discard_label = if view_only { tl!("Discard Timeline View Settings") } else { tl!("Discard") };
                if button(app, ui, "recovery.discard", discard_label, false) {
                    if let Err(e) = app.session.execute("file.discardRecovery", json!({ "id": id })) {
                        app.ui.status = e.to_string();
                    }
                    choice = 0;
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let check_pending = items[choice]["contentCheckPending"].as_bool() == Some(true);
                    if button(app, ui, "recovery.recover", if check_pending { tl!("Checking…") } else { tl!("Recover") }, true) && !check_pending {
                        if view_only {
                            if let Some(candidate) = candidates.iter().find(|candidate| candidate.id == id) {
                                let key = recovery_cache_key(candidate);
                                if app.file_dialogs.recovery_content_changed.contains(&key) {
                                    app.file_dialogs.recovery_content_confirmed.insert(key.clone());
                                }
                                let candidate = candidate.clone();
                                app.file_dialogs.recovery_content_pending.insert(key.clone());
                                app.file_dialogs.recovery_content_checked.remove(&key);
                                app.file_dialogs.recovery_content_changed.remove(&key);
                                app.file_dialogs.recovery_confirmation_id = Some(id.clone());
                                app.file_dialogs.recovery_confirmation_key = Some(key.clone());
                                ctx.request_repaint();
                                let thread_ctx = ctx.clone();
                                let thread_key = key.clone();
                                let task = std::thread::Builder::new().name("filmcraft-recovery-final-check".into()).spawn(move || {
                                    let changed = recovery_check_result(|| recovery_content_changed(&candidate));
                                    thread_ctx.data_mut(|data| {
                                        let result_key = recovery_check_results_key();
                                        let mut results = data.get_temp::<HashMap<String, bool>>(result_key).unwrap_or_default();
                                        results.insert(thread_key, changed);
                                        data.insert_temp(result_key, results);
                                    });
                                    thread_ctx.request_repaint();
                                });
                                if task.is_err() {
                                    app.file_dialogs.recovery_content_pending.remove(&key);
                                    app.file_dialogs.recovery_content_checked.insert(key.clone());
                                    app.file_dialogs.recovery_content_changed.insert(key);
                                    app.file_dialogs.recovery_confirmation_id = None;
                                    app.file_dialogs.recovery_confirmation_key = None;
                                }
                            }
                        } else {
                            match app.session.execute("file.recover", json!({ "id": id })) {
                                Ok(_) => open = false,
                                Err(e) => app.ui.status = e.to_string(),
                            }
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
    open && (!r.should_close() || ctx.input(|i| i.pointer.any_down()))
}

/// "“cut.fcproj” already exists in “edits”. Do you want to replace it?" for a save dialog's answer
/// that names an existing file once its extension was added (drawn after every other dialog, so
/// it stays on top). Escape cancels.
pub fn show_overwrite(app: &mut FilmcraftApp, ctx: &egui::Context) {
    let Some(path) = app.overwrite_prompt_path().map(std::path::PathBuf::from) else { return };
    let t = app.tokens;
    let file = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let folder = path.parent().and_then(|d| d.file_name()).map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let mut answer = None;
    let r = egui::Modal::new(egui::Id::unique("overwrite-modal")).frame(modal_frame(&t)).show(ctx, |ui| {
        ui.set_width(460.0);
        Frame::new().inner_margin(Margin { left: 28, right: 28, top: 24, bottom: 20 }).show(ui, |ui| {
            ui.label(RichText::new(tl!("Replace File")).size(18.0).strong().color(t.text));
            ui.add_space(6.0);
            ui.separator();
            ui.add_space(6.0);
            ui.label(RichText::new(tlf!("“{file}” already exists in “{folder}”. Do you want to replace it?", file, folder)).size(13.5).color(t.text));
            ui.add_space(18.0);
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if button(app, ui, "overwrite.replace", tl!("Replace"), true) {
                    answer = Some(true);
                }
                if button(app, ui, "overwrite.cancel", tl!("Cancel"), false) {
                    answer = Some(false);
                }
            });
        });
    });
    // (the modal's own close also fires on Escape: not while a drag is being cancelled, #580)
    if answer.is_none() && r.should_close() && !ctx.input(|i| i.pointer.any_down()) {
        answer = Some(false);
    }
    if let Some(replace) = answer {
        app.answer_overwrite(replace);
    }
}

/// Draw the dialog if it is one of ours; returns Some(still open).
pub fn show(app: &mut FilmcraftApp, ctx: &egui::Context, d: Dialog) -> Option<bool> {
    match d {
        Dialog::Preferences => Some(crate::panels::settings::show(app, ctx)),
        Dialog::Recovery => Some(show_recovery(app, ctx)),
        Dialog::RevertConfirm => Some(show_revert(app, ctx)),
        _ => None,
    }
}

#[cfg(test)]
mod recovery_revalidation_tests {
    use super::{FileDialogState, recovery_check_can_start, recovery_check_results_key, retire_completed_recovery_checks};
    use std::collections::HashMap;

    #[test]
    fn recovery_content_checks_are_serial_and_selected_candidate_goes_first() {
        let mut pending = std::collections::HashSet::new();
        assert!(!super::recovery_check_can_start("other", Some("selected"), true, &pending));
        assert!(super::recovery_check_can_start("selected", Some("selected"), true, &pending));
        pending.insert("selected".to_string());
        assert!(!super::recovery_check_can_start("other", Some("selected"), false, &pending));
        pending.clear();
        assert!(super::recovery_check_can_start("other", Some("selected"), false, &pending));
    }

    #[test]
    fn completed_recovery_check_retires_its_key_after_candidate_changes() {
        let ctx = egui::Context::default();
        let old_key = "candidate:old-generation".to_string();
        let mut state = FileDialogState::default();
        state.recovery_content_pending.insert(old_key.clone());
        ctx.data_mut(|data| data.insert_temp(recovery_check_results_key(), HashMap::from([(old_key.clone(), true)])));

        retire_completed_recovery_checks(&mut state, &ctx);

        assert!(!state.recovery_content_pending.contains(&old_key));
        assert!(state.recovery_content_checked.contains(&old_key));
        assert!(state.recovery_content_changed.contains(&old_key));
        assert!(recovery_check_can_start("new-generation", None, false, &state.recovery_content_pending));
    }

    #[test]
    fn recovery_content_check_panic_requires_confirmation_instead_of_sticking_pending() {
        assert!(super::recovery_check_result(|| panic!("synthetic comparison failure")));
        assert!(!super::recovery_check_result(|| false));
    }
}
