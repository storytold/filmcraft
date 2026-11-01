//! Smaller panels: History, Markers, Info (the Media Browser is `panels::media_browser`).

use egui::{Align2, Color32, Rect, Sense, pos2, vec2};
use filmcraft_time::{TimeDisplay, format_time, parse_timecode};

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

/// Egui memory key of the open Delete Redoable Actions confirmation.
const CONFIRM_DELETE_REDOABLE: &str = "history-delete-redoable";

/// The History panel's ≡ menu: Delete Redoable Actions and Clear History (automation ids
/// `panel.menu.History.deleteRedoable` and `panel.menu.History.clear`). Returns true to close it.
pub fn history_panel_menu(app: &mut FilmcraftApp, ui: &mut egui::Ui) -> bool {
    let ctx = ui.ctx().clone();
    let mut close = false;
    for (id, label, command) in [("deleteRedoable", tl!("Delete Redoable Actions"), "history.deleteRedoable"), ("clear", tl!("Clear History"), "history.clear")]
    {
        let r = ui.add_enabled(app.session.is_enabled(command), egui::Button::new(label));
        app.auto.add_response(&format!("panel.menu.History.{id}"), &r, label);
        if r.clicked() {
            if let Err(e) = crate::menus::invoke(app, &ctx, command, serde_json::json!({})) {
                app.ui.status = e;
            }
            close = true;
        }
    }
    close
}

/// Delete Redoable Actions from the panel menu or a key (no parameters) asks for OK first, as in
/// Premiere. Returns None for other ids, and for calls with parameters, which run the command.
pub fn route_history(app: &mut FilmcraftApp, ctx: &egui::Context, id: &str, params: &serde_json::Value) -> Option<Result<serde_json::Value, String>> {
    if id != "history.deleteRedoable" || !params.as_object().is_none_or(|m| m.is_empty()) {
        return None;
    }
    if !app.session.history.can_redo() {
        return Some(Err("nothing to redo".into()));
    }
    ctx.data_mut(|d| d.insert_temp(egui::Id::unique(CONFIRM_DELETE_REDOABLE), true));
    Some(Ok(serde_json::json!({"dialog": "deleteRedoable"})))
}

/// The Delete Redoable Actions confirmation (automation ids `history.deleteRedoable.cancel` and
/// `history.deleteRedoable.ok`). It closes by itself when nothing is left to redo.
pub fn history_dialogs(app: &mut FilmcraftApp, ctx: &egui::Context) {
    let key = egui::Id::unique(CONFIRM_DELETE_REDOABLE);
    if !ctx.data(|d| d.get_temp::<bool>(key)).unwrap_or(false) {
        return;
    }
    let mut close = !app.session.history.can_redo();
    egui::Window::new(tl!("Delete Redoable Actions"))
        .id(key.with("window"))
        .collapsible(false)
        .resizable(false)
        .pivot(Align2::CENTER_CENTER)
        .default_pos(crate::widgets::dialog_center(ctx))
        .show(ctx, |ui| {
            ui.label(tl!("Delete the dimmed actions after the current state? They cannot be redone."));
            ui.horizontal(|ui| {
                let r = ui.button(tl!("Cancel"));
                app.auto.add("history.deleteRedoable.cancel", r.rect, "Cancel");
                close |= r.clicked();
                let r = ui.button(tl!("OK"));
                app.auto.add("history.deleteRedoable.ok", r.rect, "OK");
                if r.clicked() {
                    if let Err(e) = app.session.execute("history.deleteRedoable", serde_json::json!({})) {
                        app.ui.status = e.to_string();
                    }
                    close = true;
                }
            });
        });
    if close || crate::widgets::escape_closes(ctx) {
        ctx.data_mut(|d| d.remove::<bool>(key));
    }
}

/// The marker colours of the Markers panel filter (each chip stands for the labels that share its
/// marker colour).
pub const MARKER_FILTER: [filmcraft_project::Label; 7] = {
    use filmcraft_project::Label::*;
    [Green, Rose, Purple, Mango, Yellow, Blue, Teal]
};

/// A marker's Label menu. An explicit program target keeps panel focus and clip/project
/// selections from routing colour changes to another item (#1143).
pub(crate) fn marker_label_menu(app: &mut FilmcraftApp, ui: &mut egui::Ui, marker: &filmcraft_project::Marker, prefix: &str) {
    let label = crate::i18n::t("Label");
    let menu = ui.menu_button(label, |ui| {
        for color in filmcraft_project::Label::ALL {
            let name = crate::i18n::t(color.name());
            let r = ui.selectable_label(color == marker.color, name);
            app.auto.add_response(&format!("{prefix}.color.{}", color.name()), &r, name);
            if r.clicked() {
                if let Err(error) = app.session.execute("markers.edit", serde_json::json!({"target":"program","marker":marker.id.0,"color":color.name()})) {
                    app.ui.status = error.to_string();
                }
                ui.close();
            }
        }
    });
    app.auto.add_response(&format!("{prefix}.label"), &menu.response, label);
}

/// The marker types of the Edit Marker dialog's Options: kind, `markers.edit` name, label.
const MARKER_KINDS: [(filmcraft_project::MarkerKind, &str, &str); 5] = {
    use filmcraft_project::MarkerKind::*;
    [
        (Comment, "comment", "Comment Marker"),
        (Chapter, "chapter", "Chapter Marker"),
        (Segmentation, "segmentation", "Segmentation Marker"),
        (WebLink, "webLink", "Web Link"),
        (FlashCue, "flashCue", "Flash Cue Point"),
    ]
};

/// Opens the Edit Marker dialog on a marker of the active sequence (#2416).
pub(crate) fn open_marker_dialog(app: &mut FilmcraftApp, marker: &filmcraft_project::Marker) {
    let (Some(seq), Some(sequence)) = (app.session.active_sequence(), app.session.state.active_sequence) else { return };
    let duration = format_time(marker.duration, seq.settings.frame_rate, seq.settings.drop_frame, TimeDisplay::Timecode, 48000);
    let kind = MARKER_KINDS.iter().find(|k| k.0 == marker.kind).map_or("comment", |k| k.1);
    app.ui.marker_dialog = Some(crate::state::MarkerDraft {
        sequence: sequence.0,
        generation: app.session.document_epoch,
        duration0: duration.clone(),
        marker: marker.id.0,
        name: marker.name.clone(),
        comment: marker.comment.clone(),
        duration,
        color: marker.color.name().into(),
        kind: kind.into(),
    });
}

/// "Edit Marker…" in a marker's right-click menu (automation id `<prefix>.edit`).
pub(crate) fn marker_edit_item(app: &mut FilmcraftApp, ui: &mut egui::Ui, marker: &filmcraft_project::Marker, prefix: &str) {
    let label = tl!("Edit Marker…");
    let r = ui.button(label);
    app.auto.add_response(&format!("{prefix}.edit"), &r, label);
    if r.clicked() {
        open_marker_dialog(app, marker);
        ui.close();
    }
}

/// Edit Marker (Premiere's Marker dialog): Name, Duration, Comments, Marker Color and Options; OK is
/// one `markers.edit`, so one undo step. Cancel or Escape leave the marker as it was.
///
/// Automation ids: `markerDialog.name`, `markerDialog.duration`, `markerDialog.comment`,
/// `markerDialog.color.<label>`, `markerDialog.kind.<kind>`, `markerDialog.ok`, `markerDialog.cancel`.
pub(crate) fn marker_dialog(app: &mut FilmcraftApp, ctx: &egui::Context) {
    let Some(mut d) = app.ui.marker_dialog.clone() else { return };
    // another project or sequence: the marker id may name another marker there
    let same_place = app.session.state.active_sequence.map(|s| s.0) == Some(d.sequence) && app.session.document_epoch == d.generation;
    let Some(seq) = app.session.active_sequence().filter(|_| same_place) else {
        app.ui.marker_dialog = None;
        return;
    };
    let (rate, drop_frame) = (seq.settings.frame_rate, seq.settings.drop_frame);
    // the marker may be gone (undo, Clear Markers) while the dialog is open
    let Some(start) = seq.markers.iter().find(|m| m.id.0 == d.marker).map(|m| m.start) else {
        app.ui.marker_dialog = None;
        return;
    };
    let accent = app.tokens.accent;
    let mut keep = true;
    let mut ok = false;
    let mut elems: Vec<(String, Rect, String)> = Vec::new();
    egui::Window::new(tl!("Marker"))
        .id(egui::Id::unique("Edit Marker"))
        .collapsible(false)
        .resizable(false)
        .pivot(Align2::CENTER_CENTER)
        .default_pos(crate::widgets::dialog_center(ctx))
        .show(ctx, |ui| {
            egui::Grid::new("marker-dialog").num_columns(2).spacing([12.0, 8.0]).show(ui, |ui| {
                ui.label(tl!("Name:"));
                let r = ui.add(egui::TextEdit::singleline(&mut d.name).desired_width(260.0));
                elems.push(("markerDialog.name".into(), r.rect, d.name.clone()));
                ui.end_row();
                ui.label(tl!("In:"));
                ui.label(egui::RichText::new(format_time(start, rate, drop_frame, TimeDisplay::Timecode, 48000)).font(Tokens::mono(12.0)));
                ui.end_row();
                ui.label(tl!("Duration:"));
                let r = ui.add(egui::TextEdit::singleline(&mut d.duration).font(Tokens::mono(12.0)).desired_width(120.0));
                elems.push(("markerDialog.duration".into(), r.rect, d.duration.clone()));
                ui.end_row();
                ui.label(tl!("Comments:"));
                let r = ui.add(egui::TextEdit::multiline(&mut d.comment).desired_width(260.0).desired_rows(4));
                elems.push(("markerDialog.comment".into(), r.rect, d.comment.clone()));
                ui.end_row();
                ui.label(tl!("Marker Color:"));
                ui.horizontal(|ui| {
                    for l in MARKER_FILTER {
                        let c = l.marker_rgb();
                        let (r, resp) = ui.allocate_exact_size(vec2(18.0, 18.0), Sense::click());
                        let col = Color32::from_rgb(c[0], c[1], c[2]);
                        ui.painter().rect_filled(r.shrink(3.0), 3.0, col);
                        if d.color == l.name() {
                            ui.painter().rect_stroke(r, 3.0, egui::Stroke::new(1.5, Color32::WHITE), egui::StrokeKind::Inside);
                        }
                        elems.push((format!("markerDialog.color.{}", l.name()), r, l.name().into()));
                        if resp.on_hover_text(crate::i18n::t(l.name())).clicked() {
                            d.color = l.name().into();
                        }
                    }
                });
                ui.end_row();
                ui.label(tl!("Options:"));
                ui.vertical(|ui| {
                    for (_, id, label) in MARKER_KINDS {
                        let label = crate::i18n::t(label);
                        let r = ui.radio(d.kind == id, label);
                        elems.push((format!("markerDialog.kind.{id}"), r.rect, label.to_string()));
                        if r.clicked() {
                            d.kind = id.into();
                        }
                    }
                });
                ui.end_row();
            });
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                let c = ui.button(tl!("Cancel"));
                elems.push(("markerDialog.cancel".into(), c.rect, "Cancel".into()));
                if c.clicked() {
                    keep = false;
                }
                let o = ui.add(egui::Button::new(egui::RichText::new(tl!("OK")).color(Color32::WHITE)).fill(accent));
                elems.push(("markerDialog.ok".into(), o.rect, "OK".into()));
                if o.clicked() {
                    ok = true;
                }
            });
        });
    for (id, r, l) in elems {
        app.auto.add(&id, r, &l);
    }
    if crate::widgets::escape_closes(ctx) {
        keep = false;
    }
    if ok {
        let mut p = serde_json::json!({"target": "program", "marker": d.marker, "name": d.name, "comment": d.comment, "color": d.color, "kind": d.kind});
        // the duration only when it was edited: the field shows whole frames, and a marker's
        // duration need not be (#2470 review)
        let duration = if d.duration.trim() == d.duration0.trim() { Ok(None) } else { parse_timecode(&d.duration, rate, drop_frame, 0).map(Some) };
        match duration {
            Ok(frames) => {
                if let Some(f) = frames {
                    p["durationFrames"] = serde_json::json!(f);
                }
                match app.session.execute("markers.edit", p) {
                    Ok(_) => keep = false,
                    Err(e) => app.ui.status = e.to_string(),
                }
            }
            Err(e) => app.ui.status = tlf!("Invalid duration: {error}", error = e),
        }
    }
    app.ui.marker_dialog = keep.then_some(d);
}

/// Markers panel: a colour filter row (click a chip to show/hide that colour; automation ids
/// `markers.filter.<label>`), then the sequence markers (`markers.row.<id>`; click = go to it).
pub fn markers(app: &mut FilmcraftApp, ui: &mut egui::Ui, rect: Rect) {
    let t = app.tokens;
    // Keep the immutable project snapshot alive while UI commands mutate the session.
    // Cloning a sequence here copies every clip and effect on every redraw.
    let project = app.session.project.clone();
    let Some(seq) = app.session.state.active_sequence.and_then(|id| project.sequence(id)) else {
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
            if resp.double_clicked() {
                open_marker_dialog(app, m);
            }
            resp.context_menu(|ui| {
                marker_edit_item(app, ui, m, "markers.menu");
                marker_label_menu(app, ui, m, "markers.menu");
            });
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
