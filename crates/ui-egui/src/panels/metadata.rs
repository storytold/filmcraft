//! Metadata panel: the clip and file properties of the selected item (Project panel selection, or
//! the clip selected in the Timeline when it has focus). Name, Label and the log fields
//! (Description, Scene, Shot, Log Note, Comment, Tape Name, Client, Camera Angle) are editable;
//! each committed edit is one `metadata.set` (undoable, saved in the project).
//!
//! Automation ids: `metadata.empty` (nothing selected), `metadata.header`, `metadata.field.<Name>` (spaces removed: `metadata.field.LogNote`),
//! `metadata.label`, `metadata.section.<Clip|File>`.

use egui::{Rect, RichText, Sense, vec2};
use filmcraft_engine::panels::{Field, metadata_fields, metadata_target};
use filmcraft_project::{ItemId, Label};
use serde_json::json;

use crate::FilmcraftApp;
use crate::dock::PanelKind;
use crate::theme::Tokens;

/// The item the panel shows.
pub fn target(app: &FilmcraftApp) -> Option<ItemId> {
    let s = &app.session;
    if app.ui.focused == PanelKind::Timeline
        && let Some(c) = s.state.selection.first()
    {
        return metadata_target(s, &json!({"clip": c.0}));
    }
    metadata_target(s, &json!({}))
}

pub fn field_id(name: &str) -> String {
    format!("metadata.field.{}", name.replace(' ', ""))
}

pub fn show(app: &mut FilmcraftApp, ui: &mut egui::Ui, rect: Rect) {
    let t = app.tokens;
    let Some(item) = target(app) else {
        crate::dock::placeholder(ui, rect, &t, tl!("Select a clip to view its metadata."));
        app.auto.add("metadata.empty", rect, "Select a clip to view its metadata.");
        return;
    };
    let fields = metadata_fields(&app.session.project, item);
    let name = app.session.project.item(item).map(|i| i.name.clone()).unwrap_or_default();
    let mut child = ui.new_child(egui::UiBuilder::new().max_rect(rect.shrink2(vec2(10.0, 8.0))));
    let ui = &mut child;
    let mut elems: Vec<(String, Rect, String)> = Vec::new();
    let mut commit: Option<(String, String)> = None;
    // header: "Clip: name" with a rule (like the Info panel)
    let (hr, _) = ui.allocate_exact_size(vec2(ui.available_width(), 26.0), Sense::hover());
    let g = ui.painter().layout_no_wrap(tlf!("Clip: {name}", name), Tokens::semibold(14.0), t.text);
    let gw = g.size().x;
    ui.painter().galley(egui::pos2(hr.min.x, hr.center().y - g.size().y / 2.0), g, t.text);
    ui.painter().line_segment([egui::pos2(hr.min.x + gw + 10.0, hr.center().y), egui::pos2(hr.max.x, hr.center().y)], egui::Stroke::new(2.0, t.separator));
    elems.push(("metadata.header".into(), hr, format!("Clip: {name}")));
    let label_w = 120.0;
    egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
        let mut section = "";
        for f in &fields {
            if f.section != section {
                section = f.section;
                ui.add_space(4.0);
                let r = ui.label(RichText::new(crate::i18n::t(section)).color(t.text_dim).strong());
                elems.push((format!("metadata.section.{section}"), r.rect, section.to_string()));
            }
            ui.horizontal(|ui| {
                ui.add_sized(vec2(label_w, 20.0), egui::Label::new(RichText::new(format!("{}:", crate::i18n::t(&f.name))).color(t.text_dim)).truncate());
                if f.name == "Label" {
                    let cur = Label::from_name(&f.value).unwrap_or(Label::Violet);
                    let mut sel = cur;
                    let r = egui::ComboBox::from_id_salt(("md-label", item.0))
                        .selected_text(crate::i18n::t(cur.name()))
                        .width(ui.available_width().min(200.0))
                        .show_ui(ui, |ui| {
                            for l in Label::ALL {
                                let c = l.rgb();
                                let rr = ui.horizontal(|ui| {
                                    let (sw, _) = ui.allocate_exact_size(vec2(12.0, 12.0), Sense::hover());
                                    ui.painter().rect_filled(sw, 2.0, egui::Color32::from_rgb(c[0], c[1], c[2]));
                                    ui.selectable_value(&mut sel, l, crate::i18n::t(l.name()))
                                });
                                elems.push((format!("metadata.label.{}", l.name()), rr.inner.rect, l.name().to_string()));
                            }
                        });
                    elems.push(("metadata.label".into(), r.response.rect, cur.name().to_string()));
                    if sel != cur {
                        commit = Some(("Label".into(), sel.name().to_string()));
                    }
                } else if f.editable {
                    if let Some(c) = text_field(ui, item, f, &mut elems) {
                        commit = Some(c);
                    }
                } else {
                    let r = ui.add(egui::Label::new(RichText::new(&f.value).color(t.text)).truncate());
                    elems.push((field_id(&f.name), r.rect, f.value.clone()));
                }
            });
        }
    });
    for (id, r, l) in elems {
        app.auto.add(&id, r, &l);
    }
    if let Some((field, value)) = commit
        && let Err(e) = app.session.execute("metadata.set", json!({"item": item.0, "field": field, "value": value}))
    {
        app.ui.status = e.to_string();
    }
}

/// An editable value: a text field whose edit is committed when it loses focus (Enter, Tab or a
/// click elsewhere). Returns (field, new value) on commit.
fn text_field(ui: &mut egui::Ui, item: ItemId, f: &Field, elems: &mut Vec<(String, Rect, String)>) -> Option<(String, String)> {
    let id = egui::Id::new(("md-draft", item.0, f.name.clone()));
    let mut draft: String = ui.data(|d| d.get_temp(id)).unwrap_or_else(|| f.value.clone());
    let r = ui.add(egui::TextEdit::singleline(&mut draft).desired_width(ui.available_width()).id_salt(("md-edit", item.0, f.name.clone())));
    elems.push((field_id(&f.name), r.rect, f.value.clone()));
    let mut out = None;
    if r.has_focus() {
        ui.data_mut(|d| d.insert_temp(id, draft.clone()));
    } else {
        if r.lost_focus() && draft != f.value {
            out = Some((f.name.clone(), draft));
        }
        ui.data_mut(|d| d.remove::<String>(id));
    }
    out
}
