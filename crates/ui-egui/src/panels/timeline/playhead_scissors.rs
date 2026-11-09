use crate::icons::{self, Icon};

use super::{Color32, CursorIcon, FilmcraftApp, Layout, Rect, Sense, Sequence, Tokens, Tool, json, pos2, vec2};

/// Offer a single cut when the selected clip straddles the playhead.
pub(super) fn playhead_scissors(app: &mut FilmcraftApp, ui: &mut egui::Ui, seq: &Sequence, layout: &Layout, theme: &Tokens) {
    if app.ui.tool != Tool::Selection || app.playback.playing || app.session.voiceover.recording() || app.tl.drag.is_some() {
        return;
    }
    let Some((selected, partners)) = app.session.state.selection.split_first() else { return };
    let Some((track_id, clip)) = seq.find_item(*selected) else { return };
    // Treat a linked selection as one edit so the cut keeps audio and video in sync.
    if !partners.is_empty()
        && !(app.session.state.linked_selection
            && clip.link.is_some()
            && partners.iter().all(|id| seq.find_item(*id).is_some_and(|(_, partner)| partner.link == clip.link)))
    {
        return;
    }
    let time = app.session.playhead();
    let x = layout.x_of(time);
    let Some(row) = layout.rows.iter().find(|row| row.track == track_id) else { return };
    let Some(track) = seq.track(row.track) else { return };
    let shown = layout.shown(row).intersect(layout.content);
    if row.lane || track.locked || time <= clip.start || time >= clip.end() || x < shown.left() || x > shown.right() {
        return;
    }
    // Keep the button above the waveform's volume line and inside short or partially visible rows.
    let button_height = 14.0_f32.min(shown.height());
    if button_height < 12.0 {
        return;
    }
    let center = pos2(x, shown.top() + button_height * 0.5);
    let button = Rect::from_center_size(center, vec2(24.0, button_height)).intersect(shown);
    let resp = ui.interact(button, egui::Id::unique(("playhead-scissors", row.track.0)), Sense::click());
    app.auto.add("timeline.playheadScissors", button, "Split clip at playhead");
    let painter = ui.painter().with_clip_rect(button);
    painter.rect_filled(button, 3.0, theme.playhead);
    let icon_rect = Rect::from_center_size(center, vec2(16.0, 16.0));
    icons::paint(&painter, icon_rect, Icon::Scissors, Color32::WHITE);
    if resp.hovered() {
        ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
    }
    if resp.clicked() {
        let target = if partners.is_empty() { json!({"track": row.track.0, "time": time.0}) } else { json!({"clip": selected.0, "time": time.0}) };
        if let Err(error) = app.session.execute("timeline.razor", target) {
            app.ui.status = error.to_string();
        }
    }
}
