//! File ▸ New ▸ Project… (Cmd+Alt+N): the project's name and, optionally, the preset of the
//! sequence it starts with: the user's custom presets first, then the built-in groups, or none
//! for an empty project. OK runs `file.newProject {name, sequencePreset?}`.
//!
//! Automation ids (`newProject.` + …): `name`, `preset` (+ `preset.option.none` and
//! `preset.option.<preset name>` while open), `ok`, `cancel`.

use serde_json::{Value, json};

use crate::FilmcraftApp;
use crate::i18n::t;
use crate::state::NewProjectDraft;

/// Open the dialog with a fresh draft.
pub fn open(app: &mut FilmcraftApp) {
    app.ui.new_project = NewProjectDraft { name: "Untitled".into(), preset: String::new() };
    app.dialog = Some(crate::Dialog::NewProject);
}

/// The `file.newProject` parameters for a draft.
fn params(d: &NewProjectDraft) -> Value {
    let name = d.name.trim();
    let mut p = json!({"name": if name.is_empty() { "Untitled" } else { name }});
    if !d.preset.is_empty() {
        p["sequencePreset"] = json!(d.preset);
    }
    p
}

/// Draw the dialog. Returns whether it stays open.
pub fn show(app: &mut FilmcraftApp, ctx: &egui::Context) -> bool {
    let mut d = app.ui.new_project.clone();
    let customs: Vec<String> = app.session.sequence_presets.user.iter().map(|p| p.name.clone()).collect();
    let builtins = filmcraft_project::sequence_presets_cached();
    let accent = app.tokens.accent;
    let mut keep = true;
    let mut apply = false;
    let mut elems: Vec<(String, egui::Rect, String)> = Vec::new();
    egui::Window::new(tl!("New Project"))
        .id(egui::Id::new("New Project"))
        .collapsible(false)
        .resizable(false)
        .default_width(460.0)
        .pivot(egui::Align2::CENTER_CENTER)
        .default_pos(crate::widgets::dialog_center(ctx))
        .show(ctx, |ui| {
            ui.set_width(460.0);
            egui::Grid::new("new-project-grid").num_columns(2).spacing([8.0, 6.0]).show(ui, |ui| {
                ui.label(tl!("Name:"));
                let r = ui.add(egui::TextEdit::singleline(&mut d.name).desired_width(320.0));
                elems.push(("newProject.name".into(), r.rect, d.name.clone()));
                ui.end_row();
                ui.label(tl!("Sequence:"));
                let none = tl!("None (an empty project)");
                let shown = if d.preset.is_empty() { none.to_string() } else { d.preset.clone() };
                let resp = egui::ComboBox::from_id_salt("new-project-preset").selected_text(&shown).width(320.0).height(400.0).show_ui(ui, |ui| {
                    let o = ui.selectable_label(d.preset.is_empty(), none);
                    if o.clicked() {
                        d.preset.clear();
                    }
                    elems.push(("newProject.preset.option.none".into(), o.rect, none.to_string()));
                    let mut group = "";
                    let names = customs
                        .iter()
                        .map(|n| (filmcraft_engine::sequence_presets::CUSTOM_GROUP, n.as_str()))
                        .chain(builtins.iter().map(|p| (p.group, p.name)));
                    for (g, name) in names {
                        if g != group {
                            group = g;
                            ui.label(egui::RichText::new(t(g)).strong());
                        }
                        let o = ui.selectable_label(d.preset == name, name);
                        if o.clicked() {
                            d.preset = name.to_string();
                        }
                        elems.push((format!("newProject.preset.option.{name}"), o.rect, name.to_string()));
                    }
                });
                elems.push(("newProject.preset".into(), resp.response.rect, shown));
                ui.end_row();
            });
            ui.label(egui::RichText::new(tl!("The project starts with a sequence made from this preset (Custom or built-in), or with none.")).weak());
            ui.add_space(10.0);
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let o = ui.add(egui::Button::new(egui::RichText::new(tl!("OK")).color(egui::Color32::WHITE)).fill(accent));
                elems.push(("newProject.ok".into(), o.rect, "OK".into()));
                if o.clicked() {
                    apply = true;
                }
                let c = ui.button(tl!("Cancel"));
                elems.push(("newProject.cancel".into(), c.rect, "Cancel".into()));
                if c.clicked() {
                    keep = false;
                }
            });
        });
    for (id, r, l) in elems {
        app.auto.add(&id, r, &l);
    }
    if crate::widgets::escape_closes(ctx) {
        keep = false;
    }
    if ctx.input(|i| i.key_pressed(egui::Key::Enter)) && !ctx.egui_wants_keyboard_input() {
        apply = true;
    }
    if apply {
        match app.session.execute("file.newProject", params(&d)) {
            Ok(_) => keep = false,
            Err(e) => {
                app.ui.status = e.to_string();
                keep = true;
            }
        }
    }
    app.ui.new_project = d;
    keep
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_draft_becomes_the_command_parameters() {
        assert_eq!(params(&NewProjectDraft { name: " Short ".into(), preset: String::new() }), json!({"name": "Short"}));
        assert_eq!(params(&NewProjectDraft { name: "  ".into(), preset: "HD 1080p 25".into() }), json!({"name": "Untitled", "sequencePreset": "HD 1080p 25"}));
    }
}
