//! Project-local custom palette and clip label application using the shared sRGB editor.
use crate::{
    FilmcraftApp,
    widgets::color_picker::{self, ColorDraft},
};
use egui::{Color32, Sense, Stroke, vec2};
use filmcraft_project::HexColor;
use serde_json::{Value, json};
use std::sync::Arc;

#[derive(Clone, Debug)]
pub struct Draft {
    pub project: Arc<()>,
    pub color: ColorDraft,
    pub params: Value,
    pub error: String,
}

/// Saved project swatches first, then custom colors already used in this sequence.
/// Keep saved order and bound the menu, including when loading hostile project data.
pub fn available(app: &FilmcraftApp) -> Vec<HexColor> {
    let mut colors = Vec::new();
    for color in app
        .session
        .project
        .custom_colors
        .iter()
        .copied()
        .chain(app.session.active_sequence().into_iter().flat_map(|s| &s.markers).filter_map(|m| m.custom_color))
        .chain(app.session.active_sequence().into_iter().flat_map(|s| s.all_tracks()).flat_map(|t| &t.items).filter_map(|i| i.custom_label_color))
        .chain(app.session.project.items.values().filter_map(|i| i.custom_label_color))
        .chain(
            app.session
                .state
                .source_item
                .and_then(|id| filmcraft_engine::clip_ops::source_view(&app.session, id))
                .into_iter()
                .flat_map(|v| v.markers)
                .filter_map(|m| m.custom_color),
        )
    {
        if !colors.contains(&color) {
            colors.push(color);
            if colors.len() == 64 {
                break;
            }
        }
    }
    colors
}

pub fn menu(app: &mut FilmcraftApp, ui: &mut egui::Ui, params: Value, prefix: &str) {
    ui.separator();
    egui::ScrollArea::vertical().id_salt(format!("{prefix}.customScroll")).max_height(140.0).show(ui, |ui| {
        for color in available(app) {
            let hex = color.hex();
            let response = ui
                .horizontal(|ui| {
                    let (rect, swatch) = ui.allocate_exact_size(vec2(16.0, 16.0), Sense::click());
                    let [r, g, b] = color.0;
                    ui.painter().rect_filled(rect, 2.0, Color32::from_rgb(r, g, b));
                    ui.painter().rect_stroke(rect, 2.0, Stroke::new(1.0, ui.visuals().text_color()), egui::StrokeKind::Inside);
                    swatch.union(ui.button(name(app, color)))
                })
                .inner;
            app.auto.add_response(&format!("{prefix}.custom.{hex}"), &response, &hex);
            if response.clicked() {
                let mut command = params.clone();
                command["label"] = json!(hex);
                if let Err(e) = app.session.execute("edit.label", command) {
                    app.ui.status = e.to_string();
                }
                ui.close();
            }
        }
    });
    let response = ui.button("Manage Colors…");
    app.auto.add_response(&format!("{prefix}.manageColors"), &response, "Manage Colors");
    if response.clicked() {
        super::palette_manager::open(app);
        ui.close();
    }
    let response = ui.button("Add custom color…");
    app.auto.add_response(&format!("{prefix}.addColor"), &response, "Add custom color");
    if response.clicked() {
        app.ui.custom_label_color =
            Some(Draft { project: app.session.project_instance.clone(), color: ColorDraft::new([128, 128, 128]), params, error: String::new() });
        ui.close();
    }
}

pub fn show(app: &mut FilmcraftApp, ctx: &egui::Context) {
    let Some(mut draft) = app.ui.custom_label_color.take() else { return };
    if !Arc::ptr_eq(&draft.project, &app.session.project_instance) {
        return;
    }
    let mut apply = false;
    let mut cancel = false;
    let modal = egui::Modal::new(egui::Id::new("custom-clip-color")).show(ctx, |ui| {
        ui.set_width(360.0);
        ui.heading("Add Custom Color");
        color_picker::show(ui, &mut app.auto, "labelColor", &mut draft.color);
        if !draft.error.is_empty() {
            ui.colored_label(ui.visuals().error_fg_color, &draft.error);
        }
        ui.separator();
        ui.horizontal(|ui| {
            let response = ui.add_enabled(draft.color.value().is_some(), egui::Button::new("Add and apply"));
            app.auto.add_response("labelColor.apply", &response, "Add and apply");
            apply = response.clicked();
            let response = ui.button("Cancel");
            app.auto.add_response("labelColor.cancel", &response, "Cancel");
            cancel = response.clicked();
        });
    });
    if cancel || modal.should_close() {
        return;
    }
    if apply && let Some(color) = draft.color.value() {
        draft.params["label"] = json!(color);
        draft.params["saveColor"] = json!(true);
        match app.session.execute("edit.label", draft.params.clone()) {
            Ok(_) => return,
            Err(e) => draft.error = e.to_string(),
        }
    }
    app.ui.custom_label_color = Some(draft);
}

pub fn name(app: &FilmcraftApp, color: HexColor) -> String {
    app.session.project.custom_color_names.get(&color).filter(|s| !s.is_empty()).cloned().unwrap_or_else(|| color.hex())
}
