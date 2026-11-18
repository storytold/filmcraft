//! Smaller panels: History, Markers, Info (the Media Browser is `panels::media_browser`).

use egui::{Align2, Color32, Rect, Sense, pos2, vec2};
use filmcraft_time::{TimeDisplay, format_time};

use crate::FilmcraftApp;
use crate::icons::{self, Icon};
use crate::state::Tool;
use crate::theme::Tokens;

/// History icons for undoable commands that a timeline tool runs: the tool's icon.
const HISTORY_TOOL_COMMANDS: &[(&[&str], Tool)] = &[
    (&["timeline.move", "timeline.trim", "timeline.place", "timeline.setTrack", "trim.previous", "trim.next"], Tool::Selection),
    (&["timeline.razor", "sequence.addEdit", "sequence.addEditAllTracks", "multicam.cut"], Tool::Razor),
    (&["timeline.roll"], Tool::Rolling),
    (&["timeline.rateStretch"], Tool::RateStretch),
    (&["clip.remix"], Tool::Remix),
    (&["edit.rippleDelete", "edit.rippleDeleteHead", "sequence.closeGap", "trim.rippleNext", "trim.ripplePrevious"], Tool::Ripple),
    (&[filmcraft_engine::trim::RIPPLE_TRIM_ORIGIN], Tool::Ripple),
    (&["graphics.newText", "graphics.newVerticalText", "graphics.setText", "graphics.setCharStyle"], Tool::Type),
    (&["graphics.newRectangle"], Tool::Rectangle),
    (&["graphics.newEllipse"], Tool::Ellipse),
    (&["graphics.newShape", "graphics.newPolygon"], Tool::Pen),
];

/// History icons for other undoable commands with an icon of their own, and the plain square for
/// a command that a family below would otherwise give the wrong icon.
const HISTORY_COMMANDS: &[(&[&str], Icon)] = &[
    (&["trim.applyDefaultTransition"], Icon::Square),
    (&["source.insert", "edit.pasteInsert"], Icon::Insert),
    (&["source.overwrite", "edit.paste"], Icon::Overwrite),
    (&["sequence.lift", "transcript.lift"], Icon::Lift),
    (&["sequence.extract", "transcript.extract"], Icon::Extract),
    (&["markers.markIn"], Icon::MarkIn),
    (&["markers.markOut"], Icon::MarkOut),
    (&["file.newBin", "file.newBinFromSelection", "file.newSearchBin", "project.moveToBin", "project.renameBin"], Icon::Folder),
    (&["file.newSequence", "file.newSequenceFromClip", "sequence.makeSubsequence"], Icon::Sequence),
    (&["clip.nest", "sequence.nestSequences"], Icon::Nest),
    (&["clip.link", "media.linkMedia", "media.relink"], Icon::Link),
    (&["project.delete", "edit.clear", "timeline.delete", "edit.removeUnused", "sequence.deleteTrack", "sequence.deleteTracks"], Icon::Trash),
    (&["file.newAdjustmentLayer"], Icon::Adjust),
    (&["media.makeOffline"], Icon::Offline),
    (&["clip.speedDuration", "clip.frameHold", "clip.insertFrameHoldSegment"], Icon::Stopwatch),
];

/// History icons for families of commands, by the start of their ids. The lists above come first.
const HISTORY_COMMAND_PREFIXES: &[(&str, Icon)] = &[
    ("timeline.nudge", Icon::Selection),
    ("timeline.slip", Icon::Slip),
    ("timeline.slide", Icon::Slide),
    ("trim.", Icon::Rolling),
    ("file.import", Icon::Import),
    ("mediaBrowser.import", Icon::Import),
    ("markers.", Icon::Marker),
    ("effects.", Icon::Fx),
    ("lumetri.", Icon::Fx),
    ("masks.", Icon::Pen),
    ("captions.", Icon::Captions),
    ("transcript.createCaptions", Icon::Captions),
    ("mixer.", Icon::Speaker),
    ("clipMixer.", Icon::Speaker),
    ("essentialSound.", Icon::Speaker),
    ("clip.volume", Icon::Speaker),
    ("multicam.", Icon::Camera),
];

/// The icon a History step shows for the command that made it: the tool's icon for a command a
/// timeline tool runs, the command's own icon where FilmCraft has one, and a plain square for
/// every other command and for a step made outside a command.
pub(crate) fn history_icon(command: Option<&str>) -> Icon {
    let Some(id) = command else { return Icon::Square };
    if let Some((_, tool)) = HISTORY_TOOL_COMMANDS.iter().find(|(ids, _)| ids.contains(&id)) {
        return tool.icon();
    }
    if let Some((_, icon)) = HISTORY_COMMANDS.iter().find(|(ids, _)| ids.contains(&id)) {
        return *icon;
    }
    HISTORY_COMMAND_PREFIXES.iter().find(|(p, _)| id.starts_with(p)).map_or(Icon::Square, |(_, icon)| *icon)
}

/// Where a History row draws its icon: a 14-point square at the row's left.
fn history_icon_rect(row: Rect) -> Rect {
    Rect::from_center_size(pos2(row.min.x + 13.0, row.center().y), vec2(14.0, 14.0))
}

pub fn history(app: &mut FilmcraftApp, ui: &mut egui::Ui, rect: Rect) {
    let t = app.tokens;
    let mut child = ui.new_child(egui::UiBuilder::new().max_rect(rect.shrink(6.0)));
    let undo: Vec<(String, Icon)> = app.session.history.undo.iter().map(|h| (h.0.clone(), history_icon(h.2.as_deref()))).collect();
    let redo: Vec<(String, Icon)> = app.session.history.redo.iter().rev().map(|h| (h.0.clone(), history_icon(h.2.as_deref()))).collect();
    let mut target: Option<i64> = None;
    // the visible rect and label of each row drawn, oldest first, with its icon and the icon's visible rect
    let mut shown: Vec<(Rect, String, Option<(Icon, Rect)>)> = Vec::new();
    egui::ScrollArea::vertical().auto_shrink([false, false]).show(&mut child, |ui| {
        let mut row = |ui: &mut egui::Ui, label: &str, icon: Option<Icon>, current: bool, dim: bool| -> bool {
            let (r, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 20.0), Sense::click());
            if current {
                ui.painter().rect_filled(r, 0.0, t.row_selected);
            } else if resp.hovered() {
                ui.painter().rect_filled(r, 0.0, t.hover);
            }
            if let Some(icon) = icon {
                icons::paint(ui.painter(), history_icon_rect(r), icon, if dim { t.text_faint } else { t.icon });
            }
            ui.painter().text(pos2(r.min.x + 26.0, r.center().y), Align2::LEFT_CENTER, label, Tokens::ui(12.0), if dim { t.text_faint } else { t.text });
            let clip = ui.clip_rect();
            shown.push((r.intersect(clip), label.to_string(), icon.map(|icon| (icon, history_icon_rect(r).intersect(clip)))));
            resp.clicked()
        };
        if row(ui, tl!("Open"), None, undo.is_empty(), false) {
            target = Some(-(undo.len() as i64));
        }
        for (i, (l, icon)) in undo.iter().enumerate() {
            if row(ui, crate::i18n::t(l), Some(*icon), i + 1 == undo.len(), false) {
                target = Some(i as i64 + 1 - undo.len() as i64);
            }
        }
        for (i, (l, icon)) in redo.iter().enumerate() {
            if row(ui, crate::i18n::t(l), Some(*icon), false, true) {
                target = Some(i as i64 + 1);
            }
        }
    });
    // automation ids `history.row.<n>` (0 is Open) and `history.row.<n>.icon`, labeled with the icon's name
    for (n, (r, label, icon)) in shown.into_iter().enumerate() {
        if !r.is_positive() {
            continue;
        }
        app.auto.add(&format!("history.row.{n}"), r, &label);
        if let Some((icon, icon_rect)) = icon
            && icon_rect.is_positive()
        {
            app.auto.add(&format!("history.row.{n}.icon"), icon_rect, &format!("{icon:?}"));
        }
    }
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
        let response = ui.button(tl!("Custom Color…"));
        app.auto.add_response(&format!("{prefix}.customColor"), &response, tl!("Custom Color"));
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
    let response = ui.button(tl!("Edit Marker…"));
    app.auto.add_response(&format!("{prefix}.properties"), &response, tl!("Edit Marker"));
    if response.clicked() {
        super::marker_properties::open_owned(app, marker, owner);
        ui.close();
    }
    let response = ui.button(tl!("Delete Marker"));
    app.auto.add_response(&format!("{prefix}.delete"), &response, tl!("Delete Marker"));
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn history_icons_follow_the_tool_or_command() {
        assert_eq!(history_icon(Some("timeline.razor")), Tool::Razor.icon());
        assert_eq!(history_icon(Some("timeline.move")), Tool::Selection.icon());
        assert_eq!(history_icon(Some("graphics.newText")), Tool::Type.icon());
        assert_eq!(history_icon(Some("timeline.slipLeft5")), Tool::Slip.icon());
        assert_eq!(history_icon(Some("source.insert")), Icon::Insert);
        assert_eq!(history_icon(Some("file.newBin")), Icon::Folder);
        assert_eq!(history_icon(Some("markers.add")), Icon::Marker);
        assert_eq!(history_icon(Some("markers.markIn")), Icon::MarkIn, "an exact entry comes before its family");
        assert_eq!(history_icon(Some("trim.rippleNext")), Tool::Ripple.icon(), "an exact entry comes before its family");
        assert_eq!(history_icon(Some(filmcraft_engine::trim::RIPPLE_TRIM_ORIGIN)), Tool::Ripple.icon(), "a ripple trim");
        assert_eq!(history_icon(Some("timeline.trim")), Tool::Selection.icon(), "a regular trim");
        assert_eq!(history_icon(Some("trim.next")), Tool::Selection.icon(), "a regular trim to the playhead");
        assert_eq!(history_icon(Some("timeline.roll")), Tool::Rolling.icon());
        assert_eq!(
            history_icon(Some("trim.applyDefaultTransition")),
            history_icon(Some("sequence.applyDefaultTransitionsToSelection")),
            "adding transitions at edit points is not a rolling edit"
        );
        assert_eq!(history_icon(Some("effects.setParam")), Icon::Fx);
        assert_eq!(history_icon(Some("edit.label.rose")), Icon::Square, "a command without an icon gets the square");
        assert_eq!(history_icon(Some("someday.newCommand")), Icon::Square);
        assert_eq!(history_icon(None), Icon::Square, "an edit outside a command gets the square");
    }

    /// Every command id in the History icon lists names a real command (or a ripple trim's
    /// step id), and every prefix starts at least one, so a renamed command does not lose its
    /// icon unnoticed.
    #[test]
    fn history_icon_lists_name_real_commands() {
        let specs = filmcraft_engine::commands::command_specs();
        let ids = HISTORY_TOOL_COMMANDS.iter().flat_map(|(ids, _)| ids.iter()).chain(HISTORY_COMMANDS.iter().flat_map(|(ids, _)| ids.iter()));
        for id in ids.filter(|id| **id != filmcraft_engine::trim::RIPPLE_TRIM_ORIGIN) {
            assert!(filmcraft_engine::commands::find(id).is_some(), "{id} is not a command");
        }
        for (prefix, _) in HISTORY_COMMAND_PREFIXES {
            assert!(specs.iter().any(|c| c.id.starts_with(prefix)), "no command starts with {prefix}");
        }
    }
}
