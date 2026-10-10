//! Project panel dialogs: Metadata Display… (choose and order the List view columns from every
//! metadata field), Save As New View Preset, Manage Saved View Presets, Freeform View Options…
//! and Save Arrangement…. Drafts live in `UiState::project_panel.dialog` (serde, so agents fill
//! them with `ui.set`); OK runs one engine command.

use egui::Align2;
use serde_json::{Value, json};

use crate::FilmcraftApp;
use crate::panels::project::ProjectDialog;

fn push(app: &mut FilmcraftApp, id: &str, r: &egui::Response, label: &str) {
    app.auto.add(id, r.rect, label);
}

pub fn show(app: &mut FilmcraftApp, ctx: &egui::Context) {
    let Some(mut d) = app.ui.project_panel.dialog.clone() else { return };
    let mut close = false;
    let mut run: Option<(String, Value)> = None;
    let title = match &d {
        ProjectDialog::MetadataDisplay { .. } => tl!("Metadata Display"),
        ProjectDialog::SavePresetAs { .. } => tl!("Save As New View Preset"),
        ProjectDialog::ManagePresets { .. } => tl!("Manage Saved View Presets"),
        ProjectDialog::FreeformOptions { .. } => tl!("Freeform View Options"),
        ProjectDialog::SaveArrangement { .. } => tl!("Save Arrangement"),
    };
    let mut win = egui::Window::new(title).id(egui::Id::new("project-dialog")).collapsible(false).resizable(false).anchor(Align2::CENTER_CENTER, [0.0, 0.0]);
    // a fixed size keeps the two-list dialog from shifting while its lists settle
    if matches!(d, ProjectDialog::MetadataDisplay { .. }) {
        win = win.fixed_size(egui::vec2(480.0, 470.0));
    }
    win.show(ctx, |ui| {
        match &mut d {
            ProjectDialog::MetadataDisplay { columns, filter } => metadata_display(app, ui, columns, filter, &mut run),
            ProjectDialog::SavePresetAs { name } => {
                ui.horizontal(|ui| {
                    ui.label(tl!("Name:"));
                    let r = ui.add(egui::TextEdit::singleline(name).desired_width(240.0));
                    push(app, "projectDialog.name", &r, "Name");
                });
                if ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                    run = Some(("project.viewPreset.saveAs".into(), json!({"name": name})));
                }
                ui.label(egui::RichText::new(tl!("Saves the view, columns, sort, thumbnail and font size.")).weak().size(11.0));
                if ok_cancel(app, ui, &mut close) {
                    run = Some(("project.viewPreset.saveAs".into(), json!({"name": name})));
                }
            }
            ProjectDialog::ManagePresets { selected, name } => manage_presets(app, ui, selected, name, &mut run, &mut close),
            ProjectDialog::FreeformOptions { options } => {
                egui::Grid::new("ff-options").num_columns(2).spacing([12.0, 6.0]).show(ui, |ui| {
                    ui.label(tl!("Grid spacing:"));
                    let r = ui.add(egui::DragValue::new(&mut options.grid).range(4.0..=200.0).suffix(" pt"));
                    push(app, "projectDialog.grid", &r, "Grid spacing");
                    ui.end_row();
                    ui.label(tl!("Default clip size:"));
                    let r = ui.add(egui::DragValue::new(&mut options.card_size).range(48.0..=480.0).suffix(" pt"));
                    push(app, "projectDialog.cardSize", &r, "Default clip size");
                    ui.end_row();
                });
                for (id, label, v) in [
                    ("snap", tl!("Snap to grid"), &mut options.snap),
                    ("showNames", tl!("Show clip names"), &mut options.show_names),
                    ("showDurations", tl!("Show durations"), &mut options.show_durations),
                ] {
                    let r = ui.checkbox(v, label);
                    push(app, &format!("projectDialog.{id}"), &r, label);
                }
                if ok_cancel(app, ui, &mut close) {
                    run = Some(("project.freeform.options".into(), json!({"grid": options.grid, "snap": options.snap, "showNames": options.show_names, "showDurations": options.show_durations, "cardSize": options.card_size})));
                }
            }
            ProjectDialog::SaveArrangement { name, bin } => {
                ui.horizontal(|ui| {
                    ui.label(tl!("Name:"));
                    let r = ui.add(egui::TextEdit::singleline(name).desired_width(240.0));
                    push(app, "projectDialog.name", &r, "Name");
                });
                if ok_cancel(app, ui, &mut close) && !name.trim().is_empty() {
                    run = Some(("project.freeform.saveArrangement".into(), json!({"name": name, "bin": bin})));
                }
            }
        }
    });
    if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
        close = true;
    }
    if let Some((cmd, p)) = run {
        match app.session.execute(&cmd, p) {
            Ok(_) => close = !matches!(d, ProjectDialog::ManagePresets { .. }),
            Err(e) => app.ui.status = e.to_string(),
        }
        // Manage keeps the dialog open: refresh its name field
        if let ProjectDialog::ManagePresets { selected, name } = &mut d {
            *name = app.session.prefs.project_panel.presets.get(*selected).cloned().flatten().map(|p| p.name).unwrap_or_default();
        }
    }
    if close {
        app.ui.project_panel.dialog = None;
    } else if app.ui.project_panel.dialog.is_some() {
        app.ui.project_panel.dialog = Some(d);
    }
}

/// OK / Cancel row; true when OK was clicked.
fn ok_cancel(app: &mut FilmcraftApp, ui: &mut egui::Ui, close: &mut bool) -> bool {
    ui.separator();
    let mut ok = false;
    ui.horizontal(|ui| {
        let r = ui.button(tl!("Cancel"));
        push(app, "projectDialog.cancel", &r, "Cancel");
        if r.clicked() {
            *close = true;
        }
        let r = ui.button(tl!("OK"));
        push(app, "projectDialog.ok", &r, "OK");
        ok = r.clicked();
    });
    ok
}

fn metadata_display(app: &mut FilmcraftApp, ui: &mut egui::Ui, columns: &mut Vec<String>, filter: &mut String, run: &mut Option<(String, Value)>) {
    let all = filmcraft_engine::project_panel::all_columns(&app.session.project);
    ui.horizontal(|ui| {
        ui.label(tl!("Show:"));
        let r = ui.add(egui::TextEdit::singleline(filter).hint_text(tl!("Search fields")).desired_width(220.0));
        push(app, "projectDialog.filter", &r, "Search fields");
    });
    ui.add_space(4.0);
    let f = filter.to_ascii_lowercase();
    ui.horizontal_top(|ui| {
        // every field, checked when shown
        ui.vertical(|ui| {
            ui.label(egui::RichText::new(tl!("Project Metadata")).strong());
            egui::ScrollArea::vertical().id_salt("md-all").max_height(340.0).min_scrolled_height(340.0).show(ui, |ui| {
                ui.set_min_width(220.0);
                for c in all.iter().filter(|c| f.is_empty() || c.to_ascii_lowercase().contains(&f)) {
                    let mut on = columns.contains(c);
                    let name_col = c == "Name";
                    let r = ui.add_enabled(!name_col, egui::Checkbox::new(&mut on, crate::i18n::t(c)));
                    push(app, &format!("projectDialog.field.{c}"), &r, c);
                    if r.changed() {
                        if on {
                            columns.push(c.clone());
                        } else {
                            columns.retain(|x| x != c);
                        }
                    }
                }
            });
            ui.horizontal(|ui| {
                let r = ui.button(tl!("Add Property…"));
                push(app, "projectDialog.addProperty", &r, "Add Property…");
                if r.clicked() && !filter.trim().is_empty() && !columns.iter().any(|c| c.eq_ignore_ascii_case(filter.trim())) {
                    // a custom metadata field (typed in the search box)
                    columns.push(filter.trim().to_string());
                }
            });
        });
        ui.separator();
        // the shown columns, in order
        ui.vertical(|ui| {
            ui.label(egui::RichText::new(tl!("Column order")).strong());
            egui::ScrollArea::vertical().id_salt("md-order").max_height(340.0).min_scrolled_height(340.0).show(ui, |ui| {
                ui.set_min_width(200.0);
                let mut mv: Option<(usize, isize)> = None;
                for (i, c) in columns.iter().enumerate() {
                    ui.horizontal(|ui| {
                        let up = ui.add_enabled(i > 1, egui::Button::new("▲").small());
                        push(app, &format!("projectDialog.up.{c}"), &up, "Move up");
                        let down = ui.add_enabled(i > 0 && i + 1 < columns.len(), egui::Button::new("▼").small());
                        push(app, &format!("projectDialog.down.{c}"), &down, "Move down");
                        ui.label(crate::i18n::t(c));
                        if up.clicked() {
                            mv = Some((i, -1));
                        }
                        if down.clicked() {
                            mv = Some((i, 1));
                        }
                    });
                }
                if let Some((i, d)) = mv {
                    let j = (i as isize + d) as usize;
                    columns.swap(i, j);
                }
            });
        });
    });
    let mut close = false;
    if ok_cancel(app, ui, &mut close) {
        *run = Some(("project.columns.set".into(), json!({"columns": columns})));
    }
    if close {
        app.ui.project_panel.dialog = None;
    }
}

fn manage_presets(app: &mut FilmcraftApp, ui: &mut egui::Ui, selected: &mut usize, name: &mut String, run: &mut Option<(String, Value)>, close: &mut bool) {
    let presets = app.session.prefs.project_panel.presets.clone();
    egui::ScrollArea::vertical().max_height(220.0).show(ui, |ui| {
        ui.set_min_width(300.0);
        for i in 0..filmcraft_engine::project_panel::PRESET_SLOTS {
            let p = presets.get(i).cloned().flatten();
            let label = match &p {
                Some(p) => format!("{}. {}", i + 1, p.name),
                None => tlf!("{n}. (empty)", n = i + 1),
            };
            let r = ui.selectable_label(*selected == i, label.clone());
            push(app, &format!("projectDialog.preset.{}", i + 1), &r, &label);
            if r.clicked() {
                *selected = i;
                *name = p.map(|p| p.name).unwrap_or_default();
            }
        }
    });
    let filled = presets.get(*selected).is_some_and(Option::is_some);
    ui.horizontal(|ui| {
        ui.label(tl!("Name:"));
        let r = ui.add_enabled(filled, egui::TextEdit::singleline(name).desired_width(200.0));
        push(app, "projectDialog.name", &r, "Name");
        let b = ui.add_enabled(filled, egui::Button::new(tl!("Rename")));
        push(app, "projectDialog.rename", &b, "Rename");
        if b.clicked() {
            *run = Some(("project.viewPreset.rename".into(), json!({"slot": *selected + 1, "name": name})));
        }
    });
    ui.horizontal(|ui| {
        let b = ui.add_enabled(filled, egui::Button::new(tl!("Restore")));
        push(app, "projectDialog.restore", &b, "Restore");
        if b.clicked() {
            *run = Some(("project.viewPreset.restore".into(), json!({"slot": *selected + 1})));
        }
        let b = ui.add_enabled(filled, egui::Button::new(tl!("Delete")));
        push(app, "projectDialog.delete", &b, "Delete");
        if b.clicked() {
            *run = Some(("project.viewPreset.delete".into(), json!({"slot": *selected + 1})));
        }
        let b = ui.button(tl!("Overwrite with Current View"));
        push(app, "projectDialog.overwrite", &b, "Overwrite with Current View");
        if b.clicked() {
            *run = Some((
                "project.viewPreset.saveAs".into(),
                json!({"slot": *selected + 1, "name": if name.trim().is_empty() { Value::Null } else { json!(name) }}),
            ));
        }
    });
    ui.separator();
    let r = ui.button(tl!("Done"));
    push(app, "projectDialog.ok", &r, "Done");
    if r.clicked() {
        *close = true;
    }
}
