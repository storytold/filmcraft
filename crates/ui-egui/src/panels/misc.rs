//! Smaller panels: History, Markers, Info (the Media Browser is `panels::media_browser`).

use egui::{Align2, Color32, Rect, Sense, pos2, vec2};
use filmcraft_time::{TimeDisplay, format_time};

use crate::FilmcraftApp;
use crate::theme::Tokens;

pub fn history(app: &mut FilmcraftApp, ui: &mut egui::Ui, rect: Rect) {
    let t = app.tokens;
    let mut child = ui.new_child(egui::UiBuilder::new().max_rect(rect.shrink(6.0)));
    let undo: Vec<String> = app.session.history.undo.iter().map(|h| h.0.clone()).collect();
    let redo: Vec<String> = app.session.history.redo.iter().rev().map(|h| h.0.clone()).collect();
    let mut target: Option<i64> = None;
    egui::ScrollArea::vertical().auto_shrink([false, false]).show(&mut child, |ui| {
        let row = |ui: &mut egui::Ui, label: &str, current: bool, dim: bool| -> bool {
            let (r, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 20.0), Sense::click());
            if current {
                ui.painter().rect_filled(r, 0.0, t.row_selected);
            } else if resp.hovered() {
                ui.painter().rect_filled(r, 0.0, t.hover);
            }
            ui.painter().text(pos2(r.min.x + 8.0, r.center().y), Align2::LEFT_CENTER, label, Tokens::ui(12.0), if dim { t.text_faint } else { t.text });
            resp.clicked()
        };
        if row(ui, tl!("Open"), undo.is_empty(), false) {
            target = Some(-(undo.len() as i64));
        }
        for (i, l) in undo.iter().enumerate() {
            if row(ui, crate::i18n::t(l), i + 1 == undo.len(), false) {
                target = Some(i as i64 + 1 - undo.len() as i64);
            }
        }
        for (i, l) in redo.iter().enumerate() {
            if row(ui, crate::i18n::t(l), false, true) {
                target = Some(i as i64 + 1);
            }
        }
    });
    if let Some(n) = target {
        if n < 0 {
            for _ in 0..-n {
                app.session.undo();
            }
        } else {
            for _ in 0..n {
                app.session.redo();
            }
        }
    }
}

/// Marker filtering uses the same sixteen presets as clip labels.
pub const MARKER_FILTER: [filmcraft_project::Label; 16] = filmcraft_project::Label::ALL;

/// A marker's Label menu. An explicit program target keeps panel focus and clip/project
/// selections from routing colour changes to another item (#1143).
pub(crate) fn marker_label_menu(app: &mut FilmcraftApp, ui: &mut egui::Ui, marker: &filmcraft_project::Marker, prefix: &str) {
    if let Some(owner) = super::marker_properties::Owner::current(app, false) {
        marker_owned_menu(app, ui, marker, prefix, owner);
    }
}
pub(crate) fn marker_owned_menu(
    app: &mut FilmcraftApp,
    ui: &mut egui::Ui,
    marker: &filmcraft_project::Marker,
    prefix: &str,
    owner: super::marker_properties::Owner,
) {
    let params = |color: &str| {
        let mut p = owner.params();
        p["marker"] = serde_json::json!(marker.id.0);
        p["color"] = serde_json::json!(color);
        p
    };
    let label = crate::i18n::t("Label");
    let menu = ui.menu_button(label, |ui| {
        for color in filmcraft_project::Label::ALL {
            let name = crate::i18n::t(color.name());
            let r = ui.selectable_label(marker.custom_color.is_none() && color == marker.color, name);
            app.auto.add_response(&format!("{prefix}.color.{}", color.name()), &r, name);
            if r.clicked() {
                if let Err(error) = app.session.execute("markers.edit", params(color.name())) {
                    app.ui.status = error.to_string();
                }
                ui.close();
            }
        }
        ui.separator();
        let customs = super::custom_colors::available(app);
        egui::ScrollArea::vertical().id_salt(format!("{prefix}.customScroll")).max_height(140.0).show(ui, |ui| {
            for color in &customs {
                let hex = color.hex();
                let [r, g, b] = color.0;
                let response = ui
                    .horizontal(|ui| {
                        let (rect, swatch) = ui.allocate_exact_size(vec2(16.0, 16.0), Sense::click());
                        ui.painter().rect_filled(rect, 2.0, Color32::from_rgb(r, g, b));
                        ui.painter().rect_stroke(rect, 2.0, egui::Stroke::new(1.0, ui.visuals().text_color()), egui::StrokeKind::Inside);
                        swatch.union(ui.selectable_label(marker.custom_color == Some(*color), super::custom_colors::name(app, *color)))
                    })
                    .inner;
                app.auto.add_response(&format!("{prefix}.custom.{hex}"), &response, &hex);
                if response.clicked() {
                    if let Err(e) = app.session.execute("markers.edit", params(&hex)) {
                        app.ui.status = e.to_string();
                    }
                    ui.close();
                }
            }
        });
        let response = ui.button("Custom Color…");
        app.auto.add_response(&format!("{prefix}.customColor"), &response, "Custom Color");
        if response.clicked() {
            super::marker_properties::open_owned(app, marker, owner);
            if let Some(draft) = &mut app.ui.marker_properties {
                draft.color_editor = Some(crate::widgets::color_picker::ColorDraft::new(
                    marker.custom_color.map_or_else(|| app.session.prefs.labels.rgb(marker.color), |c| c.0),
                ));
            }
            ui.close();
        }
    });
    app.auto.add_response(&format!("{prefix}.label"), &menu.response, label);
    let response = ui.button("Edit Marker…");
    app.auto.add_response(&format!("{prefix}.properties"), &response, "Edit Marker");
    if response.clicked() {
        super::marker_properties::open_owned(app, marker, owner);
        ui.close();
    }
    let response = ui.button("Delete Marker");
    app.auto.add_response(&format!("{prefix}.delete"), &response, "Delete Marker");
    if response.clicked() {
        let mut p = owner.params();
        p["marker"] = serde_json::json!(marker.id.0);
        if let Err(e) = app.session.execute("markers.delete", p) {
            app.ui.status = e.to_string();
        }
        ui.close();
    }
}

pub fn markers(app: &mut FilmcraftApp, ui: &mut egui::Ui, rect: Rect) {
    super::markers_panel::show(app, ui, rect);
}

pub fn info(app: &mut FilmcraftApp, ui: &mut egui::Ui, rect: Rect) {
    let t = app.tokens;
    let mut child = ui.new_child(egui::UiBuilder::new().max_rect(rect.shrink(8.0)));
    let ui = &mut child;
    let line = |ui: &mut egui::Ui, k: &str, v: String| {
        ui.horizontal(|ui| {
            ui.add_sized(vec2(90.0, 16.0), egui::Label::new(egui::RichText::new(k).color(t.text_dim)));
            ui.label(v);
        });
    };
    if let Some(item) = super::metadata::target(app).and_then(|id| app.session.project.item(id)).cloned() {
        let header = ui.label(egui::RichText::new(&item.name).strong());
        app.auto.add_response("info.item", &header, &item.name);
        line(ui, tl!("Type:"), crate::i18n::t(item.type_label()).to_string());
        if let Some(m) = item.as_media() {
            if let Some(v) = &m.info.video {
                line(ui, tl!("Video:"), format!("{} fps, {} x {} ({:.4})", v.frame_rate.label(), v.width, v.height, v.par.0 as f32 / v.par.1 as f32));
                line(ui, tl!("Codec:"), v.codec.clone());
            }
            if let Some(a) = m.info.audio() {
                line(ui, tl!("Audio:"), format!("{} Hz - {} ch - {}", a.sample_rate, a.channels, a.codec));
            }
        }
        line(ui, tl!("Duration:"), format_time(item.duration(), item.frame_rate(), false, TimeDisplay::Timecode, 48000));
        ui.separator();
    }
    if let Some(seq) = app.session.active_sequence() {
        let name = app.session.state.active_sequence.and_then(|s| app.session.project.item(s)).map(|i| i.name.clone()).unwrap_or_default();
        let header = ui.label(egui::RichText::new(&name).strong());
        app.auto.add_response("info.sequence", &header, &name);
        line(
            ui,
            tl!("Settings:"),
            format!("{}x{} · {} fps · {} Hz", seq.settings.width, seq.settings.height, seq.settings.frame_rate.label(), seq.settings.sample_rate),
        );
        line(ui, tl!("Playhead:"), format_time(app.session.playhead(), seq.settings.frame_rate, seq.settings.drop_frame, TimeDisplay::Timecode, 48000));
        for (i, tr) in seq.video_tracks.iter().enumerate().rev() {
            let at = tr.item_at(app.session.playhead()).map(|x| x.name.clone()).unwrap_or_default();
            line(ui, &tlf!("Video {n}:", n = i + 1), at);
        }
        for (i, tr) in seq.audio_tracks.iter().enumerate() {
            let at = tr.item_at(app.session.playhead()).map(|x| x.name.clone()).unwrap_or_default();
            line(ui, &tlf!("Audio {n}:", n = i + 1), at);
        }
    }
    ui.separator();
    line(ui, tl!("UI:"), tlf!("{fps} fps · {n} frames queued", fps = format!("{:.0}", app.fps), n = app.frames.queue_len()));
}
