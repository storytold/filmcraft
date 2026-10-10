//! Source monitor drag controls. The payload snapshots the marked span at drag start.

use egui::{Rect, Sense, pos2, vec2};
use filmcraft_engine::clip_ops::source_view;
use filmcraft_project::ItemId;
use filmcraft_time::TimeRange;

use crate::FilmcraftApp;

#[derive(Clone, Copy, Debug)]
pub struct SourceDrag {
    pub item: ItemId,
    pub range: TimeRange,
    pub video: bool,
    pub audio: bool,
}

pub fn begin(app: &mut FilmcraftApp, ui: &egui::Ui, video: bool, audio: bool) {
    let Some(item) = app.session.state.source_item else { return };
    let Some(view) = source_view(&app.session, item) else { return };
    let Some(media) = app.session.project.item(view.media) else { return };
    let video = video && media.has_video();
    let audio = audio && media.has_audio();
    let range = view.selected_range();
    if (!video && !audio) || range.duration.0 <= 0 {
        return;
    }
    app.stop_source();
    super::start_drag_source(ui, SourceDrag { item: view.media, range, video, audio });
}

pub fn controls(app: &mut FilmcraftApp, ui: &mut egui::Ui, row: Rect) {
    let media = app.session.state.source_item.and_then(|i| source_view(&app.session, i)).and_then(|v| app.session.project.item(v.media));
    let (has_video, has_audio) = media.map(|i| (i.has_video(), i.has_audio())).unwrap_or_default();
    let buttons = [
        ("source.drag.video", "Drag Video Only", true, false, has_video),
        ("source.drag.audio", "Drag Audio Only", false, true, has_audio),
        ("source.drag.both", "Drag Video and Audio", true, true, has_video && has_audio),
    ];
    let gap = 4.0_f32.min(row.width() / 12.0);
    let width = ((row.width() - gap * 2.0) / 3.0).clamp(1.0, 32.0);
    let left = row.center().x - (width * 3.0 + gap * 2.0) * 0.5;
    for (i, (id, label, video, audio, enabled)) in buttons.into_iter().enumerate() {
        let rect = Rect::from_min_size(pos2(left + i as f32 * (width + gap), row.min.y), vec2(width, row.height()));
        let response = ui.add_enabled_ui(enabled, |ui| ui.interact(rect, egui::Id::new(id), Sense::drag())).inner;
        response.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, enabled, label));
        let t = app.tokens;
        if response.hovered() || response.dragged() {
            ui.painter().rect_filled(rect, 2.0, t.hover);
        }
        let color = if !enabled {
            t.text_faint
        } else if response.hovered() {
            t.tab_text_active
        } else {
            t.icon
        };
        let icon = rect.shrink(4.0);
        if video && audio {
            let half = icon.width() * 0.5;
            crate::icons::paint(ui.painter(), Rect::from_min_size(icon.min, vec2(half, icon.height())), crate::icons::Icon::Film, color);
            crate::icons::paint(
                ui.painter(),
                Rect::from_min_size(pos2(icon.min.x + half, icon.min.y), vec2(half, icon.height())),
                crate::icons::Icon::Audio,
                color,
            );
        } else {
            crate::icons::paint(ui.painter(), icon, if video { crate::icons::Icon::Film } else { crate::icons::Icon::Audio }, color);
        }
        app.auto.add(id, rect, label);
        // Windows maps Grab/Grabbing to crossed arrows; Pointer maps to its hand cursor.
        let hover_cursor = if cfg!(windows) { egui::CursorIcon::PointingHand } else { egui::CursorIcon::Grab };
        let drag_cursor = if cfg!(windows) { egui::CursorIcon::PointingHand } else { egui::CursorIcon::Grabbing };
        let response = response.on_hover_cursor(hover_cursor).on_hover_text(label);
        if response.dragged() {
            ui.ctx().set_cursor_icon(drag_cursor);
        }
        if response.drag_started() {
            begin(app, ui, video, audio);
        }
    }
}
