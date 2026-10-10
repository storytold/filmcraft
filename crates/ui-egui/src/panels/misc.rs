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

/// The marker colours of the Markers panel filter (each chip stands for the labels that share its
/// marker colour).
pub const MARKER_FILTER: [filmcraft_project::Label; 7] = {
    use filmcraft_project::Label::*;
    [Green, Rose, Purple, Mango, Yellow, Blue, Teal]
};

/// Markers panel: a colour filter row (click a chip to show/hide that colour; automation ids
/// `markers.filter.<label>`), then the sequence markers (`markers.row.<id>`; click = go to it).
pub fn markers(app: &mut FilmcraftApp, ui: &mut egui::Ui, rect: Rect) {
    let t = app.tokens;
    let Some(seq) = app.session.active_sequence().cloned() else {
        crate::dock::placeholder(ui, rect, &t, tl!("(no sequence)"));
        return;
    };
    let hidden: Vec<[u8; 3]> = app.session.state.hidden_marker_colors.iter().map(|l| l.marker_rgb()).collect();
    let mut child = ui.new_child(egui::UiBuilder::new().max_rect(rect.shrink(6.0)));
    let mut go = None;
    let mut toggle = None;
    let mut elems: Vec<(String, Rect, String)> = Vec::new();
    child.horizontal(|ui| {
        for l in MARKER_FILTER {
            let c = l.marker_rgb();
            let on = !hidden.contains(&c);
            let (r, resp) = ui.allocate_exact_size(vec2(16.0, 16.0), Sense::click());
            let col = Color32::from_rgb(c[0], c[1], c[2]);
            if on {
                ui.painter().rect_filled(r.shrink(2.0), 3.0, col);
            } else {
                ui.painter().rect_stroke(r.shrink(2.5), 3.0, egui::Stroke::new(1.0, col), egui::StrokeKind::Inside);
            }
            elems.push((format!("markers.filter.{}", l.name()), r, format!("{} markers {}", l.name(), if on { "shown" } else { "hidden" })));
            if resp.on_hover_text(tlf!("Show or hide {color} markers", color = crate::i18n::t(l.name()))).clicked() {
                toggle = Some((l, !on));
            }
        }
    });
    child.add_space(4.0);
    egui::ScrollArea::vertical().auto_shrink([false, false]).show(&mut child, |ui| {
        let mut shown = 0;
        for m in seq.markers.iter().filter(|m| !hidden.contains(&m.color.marker_rgb())) {
            shown += 1;
            let (r, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 38.0), Sense::click());
            if resp.hovered() {
                ui.painter().rect_filled(r, 3.0, t.hover);
            }
            let c = m.color.marker_rgb();
            ui.painter().rect_filled(Rect::from_min_size(r.min + vec2(4.0, 6.0), vec2(4.0, 26.0)), 2.0, Color32::from_rgb(c[0], c[1], c[2]));
            let name = if m.name.is_empty() { tl!("Marker") } else { &m.name };
            ui.painter().text(pos2(r.min.x + 16.0, r.min.y + 12.0), Align2::LEFT_CENTER, name, Tokens::ui(12.0), t.text);
            let mut tc = format_time(m.start, seq.settings.frame_rate, seq.settings.drop_frame, TimeDisplay::Timecode, 48000);
            if m.duration > filmcraft_time::Tick::ZERO {
                let d = format_time(m.duration, seq.settings.frame_rate, seq.settings.drop_frame, TimeDisplay::Timecode, 48000);
                tc = format!("{tc}  ({d})");
            }
            if m.kind == filmcraft_project::MarkerKind::Chapter {
                tc = tlf!("{tc}  Chapter", tc);
            }
            ui.painter().text(pos2(r.min.x + 16.0, r.min.y + 27.0), Align2::LEFT_CENTER, tc, Tokens::mono(11.0), t.hot_text);
            elems.push((format!("markers.row.{}", m.id.0), r, name.to_string()));
            if resp.clicked() {
                go = Some(m.start);
            }
        }
        if seq.markers.is_empty() {
            ui.label(egui::RichText::new(tl!("No markers. Press M to add one.")).color(t.text_faint));
        } else if shown == 0 {
            ui.label(egui::RichText::new(tl!("All markers are hidden by the colour filter.")).color(t.text_faint));
        }
    });
    for (id, r, l) in elems {
        app.auto.add(&id, r, &l);
    }
    if let Some((l, visible)) = toggle
        && let Err(e) = app.session.execute("markers.filterColors", serde_json::json!({"color": l.name(), "visible": visible}))
    {
        app.ui.status = e.to_string();
    }
    if let Some(g) = go {
        app.session.set_playhead(g);
    }
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
    if let Some(item) = app.session.state.project_selection.first().and_then(|i| app.session.project.item(*i)).cloned() {
        ui.label(egui::RichText::new(&item.name).strong());
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
        ui.label(egui::RichText::new(name).strong());
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
