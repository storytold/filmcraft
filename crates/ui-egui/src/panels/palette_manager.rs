//! A separate project palette manager, shared by marker and clip menus.
use crate::{
    FilmcraftApp,
    widgets::color_picker::{self, ColorDraft},
};
use filmcraft_project::HexColor;
use serde_json::json;
use std::sync::Arc;
#[derive(Clone, Debug)]
pub struct Draft {
    project: Arc<()>,
    editing: bool,
    old: Option<HexColor>,
    color: ColorDraft,
    name: String,
    error: String,
}
pub fn open(app: &mut FilmcraftApp) {
    app.ui.palette_manager = Some(Draft {
        project: app.session.project_instance.clone(),
        editing: false,
        old: None,
        color: ColorDraft::new([128, 128, 128]),
        name: String::new(),
        error: String::new(),
    });
}
pub fn show(app: &mut FilmcraftApp, ctx: &egui::Context) {
    let Some(mut draft) = app.ui.palette_manager.take() else { return };
    if !Arc::ptr_eq(&draft.project, &app.session.project_instance) {
        return;
    }
    let mut close = false;
    let modal = egui::Modal::new(egui::Id::new("palette-manager")).show(ctx, |ui| {
        ui.set_width(360.0);
        ui.heading(if draft.editing { if draft.old.is_some() { "Edit Custom Color" } else { "Add Custom Color" } } else { "Project Colors" });
        if draft.editing {
            ui.label("Name");
            let r = ui.text_edit_singleline(&mut draft.name);
            app.auto.add_response("palette.name", &r, "Color name");
            color_picker::show(ui, &mut app.auto, "paletteColor", &mut draft.color);
            ui.horizontal(|ui| {
                let r = ui.add_enabled(draft.color.value().is_some(), egui::Button::new("Save"));
                app.auto.add_response("palette.save", &r, "Save color");
                if r.clicked()
                    && let Some(color) = draft.color.value()
                {
                    let result = if let Some(old) = draft.old {
                        app.session.execute("colors.updateCustom", json!({"color":old,"replacement":color,"name":draft.name}))
                    } else {
                        app.session.execute("colors.addCustom", json!({"color":color,"name":draft.name}))
                    };
                    match result {
                        Ok(_) => {
                            draft.editing = false;
                            draft.error.clear();
                        }
                        Err(e) => draft.error = e.to_string(),
                    }
                }
                let r = ui.button("Cancel");
                app.auto.add_response("palette.cancelEdit", &r, "Cancel");
                if r.clicked() {
                    draft.editing = false;
                    draft.error.clear();
                }
            });
        } else {
            let colors = app.session.project.custom_colors.clone();
            egui::ScrollArea::vertical().max_height(300.0).show(ui, |ui| {
                if colors.is_empty() {
                    ui.label("No saved colors");
                }
                for color in colors.into_iter().take(64) {
                    ui.horizontal(|ui| {
                        let r = super::marker_properties::swatch(ui, color.0, false);
                        app.auto.add_response(&format!("palette.swatch.{}", color.hex()), &r, &color.hex());
                        ui.label(super::custom_colors::name(app, color));
                        let edit = ui.button("Edit");
                        app.auto.add_response(&format!("palette.edit.{}", color.hex()), &edit, "Edit color");
                        if edit.clicked() || r.clicked() {
                            draft.old = Some(color);
                            draft.color = ColorDraft::new(color.0);
                            draft.name = app.session.project.custom_color_names.get(&color).cloned().unwrap_or_default();
                            draft.editing = true;
                            draft.error.clear();
                        }
                        let r = ui.button("Remove");
                        app.auto.add_response(&format!("palette.remove.{}", color.hex()), &r, "Remove saved color");
                        if r.clicked()
                            && let Err(e) = app.session.execute("colors.removeCustom", json!({"color":color}))
                        {
                            draft.error = e.to_string();
                        }
                    });
                }
            });
            ui.small("Editing or removing a swatch leaves assigned colors unchanged.");
            ui.horizontal(|ui| {
                let r = ui.button("+");
                app.auto.add_response("palette.add", &r, "Add custom color");
                if r.clicked() {
                    draft.editing = true;
                    draft.old = None;
                    draft.name.clear();
                    draft.color = ColorDraft::new([128, 128, 128]);
                    draft.error.clear();
                }
                let r = ui.button("Done");
                app.auto.add_response("palette.close", &r, "Done");
                close = r.clicked();
            });
        }
        if !draft.error.is_empty() {
            ui.colored_label(ui.visuals().error_fg_color, &draft.error);
        }
    });
    app.auto.add_response("palette.dialog", &modal.response, "Project colors");
    if !close && !modal.should_close() {
        app.ui.palette_manager = Some(draft);
    }
}
