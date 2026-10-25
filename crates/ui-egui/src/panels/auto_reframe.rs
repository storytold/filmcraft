//! Sequence ▸ Auto Reframe Sequence… (#1564): a dialog for `sequence.autoReframe` — the new
//! sequence's name, the target aspect (9:16, 1:1, 4:5, 16:9) and how quickly the framing follows
//! the subject, as in Premiere.
//!
//! Automation ids: `autoReframe.name`, `autoReframe.aspect[.option.<i>]`,
//! `autoReframe.motion[.option.<i>]`, `autoReframe.ok`, `autoReframe.cancel`.

use egui::{Color32, RichText};
use filmcraft_engine::auto_reframe::ASPECTS;
use serde_json::{Value, json};

use crate::FilmcraftApp;
use crate::state::AutoReframeDraft;

/// Auto Reframe's Motion Preset names, slower to faster.
const MOTION: [&str; 3] = ["Slower Motion", "Default", "Faster Motion"];

/// The name a sequence gets for an aspect: `Main Edit - Vertical 9x16`.
fn default_name(sequence: &str, aspect: usize) -> String {
    let label = ASPECTS.get(aspect).map_or("", |a| a.label);
    format!("{sequence} - {}", label.replace(':', "x"))
}

/// The menu entry (no params) opens the dialog; with params the command runs as it is.
pub fn route(app: &mut FilmcraftApp, id: &str, params: &Value) -> Option<Result<Value, String>> {
    if id != "sequence.autoReframe" || params.as_object().is_some_and(|m| !m.is_empty()) {
        return None;
    }
    let Some(seq) = app.session.state.active_sequence else { return Some(Err("no sequence is open".into())) };
    let name = app.session.project.item(seq).map(|i| i.name.clone()).unwrap_or_default();
    app.ui.auto_reframe = Some(AutoReframeDraft { sequence: seq.0, name: default_name(&name, 0), aspect: 0, motion: 1 });
    Some(Ok(json!({"dialog": "autoReframe"})))
}

pub fn show(app: &mut FilmcraftApp, ctx: &egui::Context) {
    let Some(mut d) = app.ui.auto_reframe.clone() else { return };
    let source = app.session.project.item(filmcraft_project::ItemId(d.sequence)).map(|i| i.name.clone()).unwrap_or_default();
    let mut elems: Vec<(String, egui::Rect, String)> = Vec::new();
    let (mut keep, mut ok) = (true, false);
    egui::Window::new(tl!("Auto Reframe Sequence"))
        .id(egui::Id::new("Auto Reframe Sequence"))
        .collapsible(false)
        .resizable(false)
        .pivot(egui::Align2::CENTER_CENTER)
        .default_pos(crate::widgets::dialog_center(ctx))
        .show(ctx, |ui| {
            egui::Grid::new("auto-reframe-grid").num_columns(2).spacing([12.0, 8.0]).show(ui, |ui| {
                ui.label(tl!("Sequence Name"));
                let r = ui.add(egui::TextEdit::singleline(&mut d.name).desired_width(260.0));
                elems.push(("autoReframe.name".into(), r.rect, d.name.clone()));
                ui.end_row();
                ui.label(tl!("Target Aspect"));
                let shown = ASPECTS.get(d.aspect).map_or("", |a| a.label);
                let r = egui::ComboBox::from_id_salt("auto-reframe-aspect").selected_text(crate::i18n::t(shown)).width(260.0).show_ui(ui, |ui| {
                    for (i, a) in ASPECTS.iter().enumerate() {
                        let o = ui.selectable_label(d.aspect == i, crate::i18n::t(a.label));
                        elems.push((format!("autoReframe.aspect.option.{i}"), o.rect, a.label.into()));
                        if o.clicked() && d.aspect != i {
                            // a name that is still the default follows the aspect
                            if d.name == default_name(&source, d.aspect) {
                                d.name = default_name(&source, i);
                            }
                            d.aspect = i;
                        }
                    }
                });
                elems.push(("autoReframe.aspect".into(), r.response.rect, shown.into()));
                ui.end_row();
                ui.label(tl!("Motion Preset"));
                let shown = MOTION.get(d.motion).copied().unwrap_or("Default");
                let r = egui::ComboBox::from_id_salt("auto-reframe-motion").selected_text(crate::i18n::t(shown)).width(260.0).show_ui(ui, |ui| {
                    for (i, m) in MOTION.iter().enumerate() {
                        let o = ui.selectable_label(d.motion == i, crate::i18n::t(m));
                        elems.push((format!("autoReframe.motion.option.{i}"), o.rect, (*m).into()));
                        if o.clicked() {
                            d.motion = i;
                        }
                    }
                });
                elems.push(("autoReframe.motion".into(), r.response.rect, shown.into()));
                ui.end_row();
            });
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                let r = ui.button(tl!("Cancel"));
                elems.push(("autoReframe.cancel".into(), r.rect, "Cancel".into()));
                keep &= !r.clicked();
                let r = ui.add(egui::Button::new(RichText::new(tl!("OK")).color(Color32::WHITE)).fill(app.tokens.accent));
                elems.push(("autoReframe.ok".into(), r.rect, "OK".into()));
                ok = r.clicked();
            });
        });
    for (id, r, l) in elems {
        app.auto.add(&id, r, &l);
    }
    if ok {
        let aspect = ASPECTS.get(d.aspect).map_or("9:16".to_string(), |a| format!("{}:{}", a.w, a.h));
        let motion = ["slower", "default", "faster"].get(d.motion).copied().unwrap_or("default");
        let p = json!({"sequence": d.sequence, "aspect": aspect, "motion": motion, "name": d.name});
        // the new sequence opens in the Timeline: that is the confirmation
        if let Err(e) = app.session.execute("sequence.autoReframe", p) {
            app.ui.status = e.to_string();
        }
        keep = false;
    }
    app.ui.auto_reframe = (keep && !crate::widgets::escape_closes(ctx)).then_some(d);
}
