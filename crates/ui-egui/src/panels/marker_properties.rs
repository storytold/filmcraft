//! Marker editing and project palette creation are separate, undoable operations.
use crate::{
    FilmcraftApp,
    widgets::color_picker::{self, ColorDraft},
};
use egui::{Color32, Sense, Stroke, vec2};
use filmcraft_project::{HexColor, Label, Marker};
use serde_json::json;
use std::sync::Arc;

#[derive(Clone, Debug)]
pub struct Draft {
    pub project: Arc<()>,
    pub owner: Owner,
    pub time: String,
    pub duration: String,
    pub range: bool,
    pub original_duration: filmcraft_time::Tick,
    pub initial_time: String,
    pub initial_duration: String,
    pub marker: u64,
    pub name: String,
    pub comment: String,
    pub author: String,
    pub hex: String,
    pub custom: bool,
    pub preset: Label,
    pub color_editor: Option<ColorDraft>,
    pub error: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Owner {
    Sequence(u64),
    Source(u64),
}
impl Owner {
    pub fn params(self) -> serde_json::Value {
        match self {
            Self::Sequence(id) => json!({"target":"program","sequence":id}),
            Self::Source(id) => json!({"target":"source","item":id}),
        }
    }
    pub fn current(app: &FilmcraftApp, source: bool) -> Option<Self> {
        if source { app.session.state.source_item.map(|id| Self::Source(id.0)) } else { app.session.state.active_sequence.map(|id| Self::Sequence(id.0)) }
    }
    pub fn details(self, app: &FilmcraftApp) -> Option<(String, filmcraft_time::FrameRate, bool, Vec<Marker>)> {
        let id = filmcraft_project::ItemId(match self {
            Self::Sequence(id) | Self::Source(id) => id,
        });
        let item = app.session.project.item(id)?;
        match self {
            Self::Sequence(_) => {
                let q = app.session.project.sequence(id)?;
                Some((format!("Sequence: {}", item.name), q.settings.frame_rate, q.settings.drop_frame, q.markers.clone()))
            }
            Self::Source(_) => {
                let v = filmcraft_engine::clip_ops::source_view(&app.session, id)?;
                Some((
                    format!("Source: {}{}", item.name, if v.media != id { " (shared source markers)" } else { "" }),
                    v.rate,
                    app.session.project.sequence(v.media).is_some_and(|q| q.settings.drop_frame),
                    v.markers,
                ))
            }
        }
    }
}
pub fn open(app: &mut FilmcraftApp, marker: &Marker) {
    if let Some(owner) = Owner::current(app, false) {
        open_owned(app, marker, owner);
    }
}
pub fn open_owned(app: &mut FilmcraftApp, marker: &Marker, owner: Owner) {
    let Some((_, rate, drop, _)) = owner.details(app) else { return };
    let format = |ticks| filmcraft_time::format_time(ticks, rate, drop, filmcraft_time::TimeDisplay::Timecode, 48000);
    let time = format(marker.start);
    let duration = format(marker.duration);
    app.ui.marker_properties = Some(Draft {
        project: app.session.project_instance.clone(),
        owner,
        initial_time: time.clone(),
        initial_duration: duration.clone(),
        time,
        duration,
        range: marker.duration > filmcraft_time::Tick::ZERO,
        original_duration: marker.duration,
        marker: marker.id.0,
        name: marker.name.clone(),
        comment: marker.comment.clone(),
        author: marker.author.clone(),
        hex: HexColor(marker.custom_color.map_or_else(|| app.session.prefs.labels.rgb(marker.color), |c| c.0)).hex(),
        custom: marker.custom_color.is_some(),
        preset: marker.color,
        color_editor: None,
        error: String::new(),
    });
}

pub fn show(app: &mut FilmcraftApp, ctx: &egui::Context) {
    let Some(mut draft) = app.ui.marker_properties.take() else { return };
    if !Arc::ptr_eq(&draft.project, &app.session.project_instance) {
        return;
    }
    if draft.color_editor.is_some() {
        color_dialog(app, ctx, &mut draft);
        app.ui.marker_properties = Some(draft);
        return;
    }
    let mut apply = false;
    let mut cancel = false;
    let mut delete = false;
    let modal = egui::Modal::new(egui::Id::new("marker-properties")).show(ctx, |ui| {
        ui.set_width(440.0);
        ui.heading("Edit Marker");
        if let Some((title, _, _, _)) = draft.owner.details(app) {
            ui.label(title);
        }
        ui.horizontal(|ui| {
            ui.label("Time");
            let response = ui.add(egui::TextEdit::singleline(&mut draft.time).desired_width(100.0));
            app.auto.add_response("markerProperties.time", &response, "Time");
            let response = ui.checkbox(&mut draft.range, "Range");
            app.auto.add_response("markerProperties.range", &response, "Range marker");
            if draft.range && draft.original_duration == filmcraft_time::Tick::ZERO && draft.duration == draft.initial_duration {
                draft.duration = "00:00:00:01".into();
            }
            ui.label("Duration");
            let response = ui.add_enabled(draft.range, egui::TextEdit::singleline(&mut draft.duration).desired_width(100.0));
            app.auto.add_response("markerProperties.duration", &response, "Duration");
        });
        ui.add_space(6.0);
        ui.columns(2, |columns| {
            for ((id, label, value), column) in [("name", "Name", &mut draft.name), ("author", "Author", &mut draft.author)].into_iter().zip(columns) {
                column.label(label);
                let response = column.add(egui::TextEdit::singleline(value).desired_width(f32::INFINITY));
                app.auto.add_response(&format!("markerProperties.{id}"), &response, label);
            }
        });
        ui.label("Notes");
        let response = ui.add(egui::TextEdit::multiline(&mut draft.comment).desired_width(f32::INFINITY).desired_rows(3));
        app.auto.add_response("markerProperties.comment", &response, "Notes");
        ui.add_space(8.0);
        ui.label("Color");
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing.x = 5.0;
            for preset in Label::ALL {
                let response = swatch(ui, app.session.prefs.labels.rgb(preset), !draft.custom && draft.preset == preset).on_hover_text(preset.name());
                app.auto.add_response(&format!("markerProperties.preset.{}", preset.name()), &response, preset.name());
                if response.clicked() {
                    draft.preset = preset;
                    draft.custom = false;
                    draft.hex = HexColor(app.session.prefs.labels.rgb(preset)).hex();
                }
            }
        });
        ui.add_space(6.0);
        let mut colors = super::custom_colors::available(app);
        if draft.custom
            && let Some(color) = HexColor::parse(&draft.hex)
            && !colors.contains(&color)
        {
            colors.push(color);
        }
        egui::ScrollArea::vertical().id_salt("marker-custom-colors").max_height(100.0).show(ui, |ui| {
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing.x = 5.0;
                for color in colors {
                    let hex = color.hex();
                    let response =
                        swatch(ui, color.0, draft.custom && draft.hex.eq_ignore_ascii_case(&hex)).on_hover_text(super::custom_colors::name(app, color));
                    app.auto.add_response(&format!("markerProperties.custom.{hex}"), &response, &hex);
                    if response.clicked() {
                        draft.hex = hex;
                        draft.custom = true;
                    }
                }
                let response = add_color_swatch(ui).on_hover_text("Add custom color");
                app.auto.add_response("markerProperties.addColor", &response, "Add custom color");
                if response.clicked() {
                    draft.error.clear();
                    draft.color_editor = Some(ColorDraft::new(HexColor::parse(&draft.hex).map_or(app.session.prefs.labels.rgb(draft.preset), |c| c.0)));
                }
            });
        });
        if !draft.error.is_empty() {
            ui.colored_label(ui.visuals().error_fg_color, &draft.error);
        }
        ui.separator();
        ui.horizontal(|ui| {
            let response = ui.button("Done");
            app.auto.add_response("markerProperties.apply", &response, "Done");
            apply = response.clicked();
            let response = ui.button("Cancel");
            app.auto.add_response("markerProperties.cancel", &response, "Cancel");
            cancel = response.clicked();
            let response = ui.button("Delete Marker");
            app.auto.add_response("markerProperties.delete", &response, "Delete Marker");
            delete = response.clicked();
        });
    });
    if cancel || modal.should_close() {
        return;
    }
    if delete {
        let mut p = draft.owner.params();
        p["marker"] = json!(draft.marker);
        match app.session.execute("markers.delete", p) {
            Ok(_) => return,
            Err(e) => draft.error = e.to_string(),
        }
    } else if apply {
        match edit_params(app, &draft).and_then(|p| app.session.execute("markers.edit", p).map_err(|e| e.to_string())) {
            Ok(_) => return,
            Err(e) => draft.error = e,
        }
    }
    app.ui.marker_properties = Some(draft);
}

fn edit_params(app: &FilmcraftApp, draft: &Draft) -> Result<serde_json::Value, String> {
    let Some((_, rate, drop, _)) = draft.owner.details(app) else {
        return Err("Marker owner is unavailable".into());
    };
    let parse = |value: &str| -> Result<i64, String> {
        let f = filmcraft_time::parse_timecode(value, rate, drop, 0).map_err(|e| e.to_string())?;
        if f < 0 || f > rate.frame_at(filmcraft_time::Tick::MAX) {
            return Err("Time is outside the supported range".into());
        }
        Ok(f)
    };
    let mut p = draft.owner.params();
    p["marker"] = json!(draft.marker);
    p["name"] = json!(draft.name);
    p["comment"] = json!(draft.comment);
    p["author"] = json!(draft.author);
    p["color"] = json!(if draft.custom { draft.hex.clone() } else { draft.preset.name().to_string() });
    // Do not quantize imported subframe positions when only editing text/color.
    if draft.time != draft.initial_time {
        p["frame"] = json!(parse(&draft.time)?);
    }
    if !draft.range && draft.original_duration != filmcraft_time::Tick::ZERO {
        p["durationFrames"] = json!(0);
    } else if draft.range && (draft.duration != draft.initial_duration || draft.original_duration == filmcraft_time::Tick::ZERO) {
        let duration = parse(&draft.duration)?;
        if duration == 0 {
            return Err("A range marker needs a positive duration".into());
        }
        p["durationFrames"] = json!(duration);
    }
    Ok(p)
}

fn color_dialog(app: &mut FilmcraftApp, ctx: &egui::Context, draft: &mut Draft) {
    let Some(editor) = &mut draft.color_editor else { return };
    let mut add = false;
    let mut cancel = false;
    let modal = egui::Modal::new(egui::Id::new("marker-custom-color")).show(ctx, |ui| {
        ui.set_width(360.0);
        ui.heading("Add Custom Color");
        color_picker::show(ui, &mut app.auto, "markerColor", editor);
        if !draft.error.is_empty() {
            ui.colored_label(ui.visuals().error_fg_color, &draft.error);
        }
        ui.separator();
        ui.horizontal(|ui| {
            let response = ui.add_enabled(editor.value().is_some(), egui::Button::new("Add color"));
            app.auto.add_response("markerColor.add", &response, "Add color");
            add = response.clicked();
            let response = ui.button("Cancel");
            app.auto.add_response("markerColor.cancel", &response, "Cancel");
            cancel = response.clicked();
        });
    });
    if cancel || modal.should_close() {
        draft.color_editor = None;
        draft.error.clear();
    } else if add && let Some(color) = editor.value() {
        match app.session.execute("colors.addCustom", json!({"color":color})) {
            Ok(_) => {
                draft.hex = color.hex();
                draft.custom = true;
                draft.color_editor = None;
                draft.error.clear();
            }
            Err(e) => draft.error = e.to_string(),
        }
    }
}

fn add_color_swatch(ui: &mut egui::Ui) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(vec2(22.0, 26.0), Sense::click());
    response.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, ui.is_enabled(), "Add custom color"));
    let chip = rect.shrink2(vec2(3.0, 5.0));
    let visuals = ui.style().interact(&response);
    ui.painter().rect_filled(chip, 3.0, visuals.bg_fill);
    ui.painter().rect_stroke(chip, 3.0, visuals.bg_stroke, egui::StrokeKind::Outside);
    ui.painter().text(chip.center(), egui::Align2::CENTER_CENTER, "+", egui::FontId::proportional(14.0), visuals.text_color());
    response
}

pub(super) fn swatch(ui: &mut egui::Ui, rgb: [u8; 3], selected: bool) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(vec2(22.0, 26.0), Sense::click());
    let [r, g, b] = rgb;
    let chip = rect.shrink2(vec2(3.0, 5.0));
    ui.painter().rect_filled(chip, 3.0, Color32::from_rgb(r, g, b));
    ui.painter().rect_stroke(chip, 3.0, Stroke::new(1.0, ui.visuals().text_color().gamma_multiply(0.5)), egui::StrokeKind::Outside);
    if selected {
        ui.painter().rect_stroke(chip.expand(3.0), 5.0, Stroke::new(2.0, ui.visuals().text_color()), egui::StrokeKind::Outside);
    }
    response
}
