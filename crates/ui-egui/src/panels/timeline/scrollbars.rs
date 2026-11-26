use super::layout::{max_pps, timeline_extent};
use super::{Align2, Color32, CursorIcon, Drag, FilmcraftApp, Pos2, Rect, Sense, Stroke, StrokeKind, Tokens, pos2, vec2};
use crate::icons::{self, Icon};
use crate::widgets::{HotNumberOptions, hot_number_numeric};
use filmcraft_project::Sequence;

mod slider_controls;
mod track_sliders;
mod zoom_bar;
pub use track_sliders::set_track_slider_hidden;
pub(in crate::panels::timeline) use track_sliders::track_slider_settings_popup;
pub(super) use track_sliders::vertical_scrollbar;

/// The narrowest the zoom scroll bar's thumb gets: room for its two handles and a part between
/// them to grab.
const MIN_THUMB_W: f32 = 34.0;

/// The minimap's default indicator colours, used when a stored one is missing or malformed.
const DEFAULT_POS_COLOR: Color32 = Color32::from_rgb(0x58, 0x95, 0xec);
const DEFAULT_VIDEO_COLOR: Color32 = Color32::from_rgb(0x64, 0xb5, 0xf6);
const DEFAULT_AUDIO_COLOR: Color32 = Color32::from_rgb(0x2e, 0x7d, 0x32);
const DEFAULT_OFFLINE_COLOR: Color32 = Color32::from_rgb(0xd3, 0x2f, 0x2f);
const MINIMAP_FLYOUT_HEIGHT: f32 = 288.0;

/// The Maximum Zoom Level, sanitized: the least of the Timeline the view may show, 0.05-3 s
/// (the setting; hostile values give the 0.4 s default).
fn min_view_secs(v: f64) -> f64 {
    if v.is_finite() && v > 0.0 { v.clamp(0.05, 3.0) } else { 0.4 }
}

/// The log-mapped share of the zoom range: 0.0 when the view shows `min_view` seconds (the
/// Maximum Zoom Level setting -- zoomed all the way in), 1.0 when it shows `vis_max` seconds (the
/// whole Timeline; the thumb fills the bar). The thumb's width is this fraction of the span between
/// [`MIN_THUMB_W`] and the bar's width: an
/// exponential zoom meter, so the thumb can keep shrinking to the smallest view no matter how long
/// the Timeline is, while a Premiere-style linear fraction would bottom out (a 34-point thumb at
/// ~28 seconds into a 10-minute Timeline).
fn log_frac(vis: f64, vis_max: f64, min_view: f64) -> f64 {
    let min_view = min_view_secs(min_view);
    let span = (vis_max / min_view).ln();
    if vis.is_finite() && vis > min_view && span > 1e-9 { ((vis / min_view).ln() / span).clamp(0.0, 1.0) } else { 0.0 }
}

/// The seconds in view for a [`log_frac`] fraction of the zoom range.
fn vis_at_frac(f: f64, vis_max: f64, min_view: f64) -> f64 {
    let min_view = min_view_secs(min_view);
    let span = (vis_max / min_view).ln();
    if f.is_finite() && f >= 0.0 && span > 1e-9 { min_view * (f * span.clamp(0.0, 709.0)).exp().min(vis_max / min_view) } else { vis_max }
}

/// Where the zoom scroll bar's thumb is: (left edge, width, seconds of scroll per point of thumb
/// travel). The thumb's width is the log-mapped zoom share ([`log_frac`]) of the span between
/// [`MIN_THUMB_W`] and the bar's full width: exactly [`MIN_THUMB_W`] at the Maximum Zoom Level --
/// the slider's smallest state, reached only there and never before, however long the Timeline is
/// -- and filling the bar when the whole Timeline is visible, shrinking steadily in between. Its
/// position maps the scroll range linearly, so it reaches both ends of the bar and moves with the
/// pointer when dragged. `view_w` is the tracks' content width: the width, in points, that `pps`
/// maps to (differing from the bar's own width by a few points).
fn zoom_thumb(bar: Rect, view_w: f32, seq_seconds: f64, scroll: f64, pps: f64, min_view: f64) -> (f32, f32, f64) {
    let vis = view_w.max(1.0) as f64 / pps.max(1e-9);
    let total = timeline_extent(seq_seconds).max(vis);
    let f = log_frac(vis, total, min_view) as f32;
    let w = (MIN_THUMB_W + f * (bar.width() - MIN_THUMB_W)).clamp(MIN_THUMB_W.min(bar.width()), bar.width());
    let travel = (bar.width() - w).max(0.0);
    let range = (total - vis).max(0.0);
    let at = if range > 0.0 { (scroll / range).clamp(0.0, 1.0) as f32 * travel } else { 0.0 };
    (bar.min.x + at, w, if travel > 0.5 { range / travel as f64 } else { 0.0 })
}

/// Grows or shrinks the view symmetrically about its center: sets a new points-a-second and moves
/// the scroll so the footage at the view's middle stays put. Both ends of the zoom-bar thumb move
/// together like a mirror -- this is how the edge handles and the wheel zoom, and it never lets the
/// scroll escape the timeline. `new_pps` is clamped into (0.05, [`max_pps`] of `view_w`) first.
fn zoom_about_center(v: &mut crate::state::TimelineView, view_w: f32, total: f64, new_pps: f64, min_view: f64) {
    if !(new_pps.is_finite() && new_pps > 0.0) {
        return;
    }
    // the widest zoom is the sequence-dependent floor (the whole Timeline in view), never a
    // fixed constant: a long sequence must be able to zoom out to its full extent
    let floor = (f64::from(view_w.max(0.0)) / total.max(1e-9)).clamp(1e-6, 24_000.0);
    let ceiling = max_pps(view_w, min_view);
    let new_pps = new_pps.clamp(floor.min(ceiling), ceiling);
    let old_vis = view_w.max(1.0) as f64 / v.pps.max(1e-9);
    let center = v.scroll + old_vis / 2.0;
    let new_vis = view_w.max(1.0) as f64 / new_pps;
    let new_range = (total - new_vis).max(0.0);
    v.pps = new_pps;
    v.target_pps = new_pps;
    v.scroll = (center - new_vis / 2.0).clamp(0.0, new_range);
    v.target_scroll = v.scroll;
}

/// The zoom scroll bar's settings popup content. Opened by right-clicking the bar, and by the
/// Timeline menu's "Timeline Slider Settings" row while the bar is hidden -- the same settings,
/// the same way, in both places. The popup only becomes scrollable when the window is too small
/// to show it whole (the height cap is the screen's, so at a normal window size nothing changes).
pub(super) fn zoombar_settings(app: &mut FilmcraftApp, ui: &mut egui::Ui) {
    settings_menu(app, ui, "timeline-zoombar", zoombar_settings_rows);
}

/// Commit a deferred settings change even if its popup stopped drawing during the gesture.
pub fn flush_pending_zoombar_view_change(app: &mut FilmcraftApp, ctx: &egui::Context) {
    let key = egui::Id::unique("tl-zoombar-view-pending");
    if ctx.data(|d| d.get_temp::<bool>(key)).unwrap_or(false) && !ctx.input(|i| i.pointer.any_down()) {
        ctx.data_mut(|d| d.remove_temp::<bool>(key));
        app.session.project_view_changed();
    }
}

fn zoombar_popup_content_height_key() -> egui::Id {
    egui::Id::unique("timeline-zoombar-popup-content-height")
}

fn zoombar_popup_overflow_key() -> egui::Id {
    egui::Id::unique("timeline-zoombar-popup-overflow")
}

fn zoombar_popup_view_height_key() -> egui::Id {
    egui::Id::unique("timeline-zoombar-popup-view-height")
}

fn zoombar_popup_pending_offset_key() -> egui::Id {
    egui::Id::unique("timeline-zoombar-popup-pending-offset")
}

/// Draw the right-click popup at its natural height. Hide the scrollbar when the full content fits
/// the window; only cap the content and allow scrolling when it cannot fit the window at all.
fn zoombar_popup_content(app: &mut FilmcraftApp, ui: &mut egui::Ui, screen_height: f32) {
    ui.style_mut().spacing.scroll = egui::style::ScrollStyle::solid();
    let original_inactive = ui.visuals().widgets.inactive.bg_fill;
    ui.visuals_mut().widgets.inactive.bg_fill = egui::Color32::from_gray(100);
    let fresh_key = egui::Id::unique("timeline-zoombar-popup-fresh");
    let fresh = ui.ctx().data(|data| data.get_temp::<bool>(fresh_key)).unwrap_or(false);
    if fresh {
        ui.ctx().data_mut(|data| data.insert_temp(fresh_key, false));
    }
    let cached_height = ui.ctx().data(|data| data.get_temp::<f32>(zoombar_popup_content_height_key())).unwrap_or(0.0);
    let available_height = (screen_height - 32.0).max(1.0);
    let was_overflow = ui.ctx().data(|data| data.get_temp::<bool>(zoombar_popup_overflow_key())).unwrap_or(false);
    let previous_view_height = ui.ctx().data(|data| data.get_temp::<f32>(zoombar_popup_view_height_key())).unwrap_or(0.0);
    let pending_offset = ui.ctx().data_mut(|data| data.remove_temp::<f32>(zoombar_popup_pending_offset_key()));
    let viewport_shrank = previous_view_height > available_height + 0.5;
    let start_at_bottom = fresh || (cached_height > available_height + 0.5 && (!was_overflow || viewport_shrank));
    // Keep the content in one scroll area from its first frame onward. Switching between a plain
    // layout and a scroll area after measuring the rows shifts the menu by the scroll area's
    // padding; that was visible as a first-open jump. A known overflow starts at the measured
    // bottom; an unknown first-frame height is measured once and applied on the next frame.
    let scroll = egui::ScrollArea::vertical()
        .id_salt("timeline-zoombar-context-popup")
        .max_height(available_height)
        .scroll_bar_visibility(egui::containers::scroll_area::ScrollBarVisibility::VisibleWhenNeeded);
    let scroll = if let Some(offset) = pending_offset {
        scroll.vertical_scroll_offset(offset)
    } else if start_at_bottom {
        scroll.vertical_scroll_offset((cached_height - available_height).max(0.0))
    } else {
        scroll
    };
    let output = scroll.show(ui, |ui| {
        ui.visuals_mut().widgets.inactive.bg_fill = original_inactive;
        zoombar_settings_rows(app, ui);
    });
    let overflow = output.content_size.y > output.inner_rect.height() + 0.5;
    let ctx = ui.ctx().clone();
    let needs_bottom_frame = overflow && (fresh || !was_overflow);
    ui.ctx().data_mut(|data| {
        data.insert_temp(zoombar_popup_content_height_key(), output.content_size.y);
        data.insert_temp(zoombar_popup_overflow_key(), overflow);
        data.insert_temp(zoombar_popup_view_height_key(), output.inner_rect.height());
        if needs_bottom_frame {
            data.insert_temp(zoombar_popup_pending_offset_key(), (output.content_size.y - output.inner_rect.height()).max(0.0));
        }
    });
    if needs_bottom_frame {
        ctx.request_repaint();
    }
}

fn zoombar_popup_open_key() -> egui::Id {
    egui::Id::unique("timeline-zoombar-settings-open")
}

/// One slider settings menu's scrolling, shared by the zoom bar's and the track sliders' popups.
/// The scroll area is given a whole screen of room (a popup's own ui only offers the space left
/// below its anchor, which forced a scrollbar and cut the menu at a normal window size); on a
/// window big enough for the menu nothing changes and no scrollbar ever appears. When the menu
/// DOES have to scroll it starts at its BOTTOM -- measured from the real content on the first
/// frame of each opening, never from a guessed height (the rows grew past the old 560/480 pt
/// guesses and the menu then started at the top on ordinary small windows).
fn settings_menu(app: &mut FilmcraftApp, ui: &mut egui::Ui, menu: &str, rows: fn(&mut FilmcraftApp, &mut egui::Ui)) {
    // The scroll area is only as tall as the window space below the popup: a full screen of
    // `max_height` would put the menu's bottom past the window edge on a popup that opens low.
    let top = ui.available_rect_before_wrap().min.y;
    let screen = ui.ctx().content_rect();
    let max_h = (screen.bottom() - top).min((screen.height() - 16.0).max(1.0)).max(1.0);
    let r = Rect::from_min_max(pos2(ui.available_rect_before_wrap().min.x, top), pos2(ui.available_rect_before_wrap().max.x, top + max_h));
    let generation_key = settings_generation_key(menu);
    let seen_key = settings_seen_key(menu);
    let scroll_key = settings_scroll_key(menu);
    let generation = ui.data(|d| d.get_temp::<u64>(generation_key)).unwrap_or(0);
    let fresh = ui.data(|d| d.get_temp::<u64>(seen_key)).unwrap_or(u64::MAX) != generation;
    if fresh {
        ui.data_mut(|d| d.insert_temp(seen_key, generation));
    }
    ui.scope_builder(egui::UiBuilder::new().max_rect(r), |ui| {
        ui.style_mut().spacing.scroll = egui::style::ScrollStyle::solid();
        let original_inactive = ui.visuals().widgets.inactive.bg_fill;
        ui.visuals_mut().widgets.inactive.bg_fill = egui::Color32::from_gray(100);
        let pending = if fresh { None } else { ui.data(|d| d.get_temp::<f32>(scroll_key)) };
        let area = egui::ScrollArea::vertical().id_salt((menu, "settings-scroll")).max_height(max_h);
        let area = if let Some(offset) = pending { area.vertical_scroll_offset(offset) } else { area };
        let output = area.show(ui, |ui| {
            ui.visuals_mut().widgets.inactive.bg_fill = original_inactive;
            rows(app, ui);
        });
        ui.data_mut(|d| d.insert_temp(egui::Id::NULL.with(("timeline-settings-content-height", menu)), output.content_size.y));
        let overflow_key = egui::Id::NULL.with(("timeline-settings-overflow", menu));
        let overflow = output.content_size.y > output.inner_rect.height() + 0.5;
        let was_overflow = ui.data(|d| d.get_temp::<bool>(overflow_key)).unwrap_or(false);
        ui.data_mut(|d| d.insert_temp(overflow_key, overflow));
        if fresh || (overflow && !was_overflow) {
            let offset = (output.content_size.y - output.inner_rect.height()).max(0.0);
            if offset.is_finite() && offset > 0.5 {
                ui.data_mut(|d| d.insert_temp(scroll_key, offset));
                ui.ctx().request_repaint();
            } else {
                ui.data_mut(|d| d.remove_temp::<f32>(scroll_key));
            }
        } else if pending.is_some() {
            ui.data_mut(|d| d.remove_temp::<f32>(scroll_key));
        }
    });
}

/// The rows of the zoom bar's settings popup (see [`zoombar_settings`]).
// one section per setting keeps the popup's rows in visual order; splitting the function would
// scatter the popup's layout over the file
#[allow(clippy::cognitive_complexity)]
fn zoombar_settings_rows(app: &mut FilmcraftApp, ui: &mut egui::Ui) {
    /// The settings popup's inner content width. Fixed, never derived from the frame's layout, so
    /// widgets measure identically on the popup's first frame and forever after (a first-frame
    /// mismatch is what made the popup jitter).
    const CONTENT_W: f32 = 257.4;
    ui.set_max_width(CONTENT_W + 16.0);
    let mut changed = false;
    family_reset(ui);
    let content_w = CONTENT_W.min(ui.available_width());

    // The very top row: Hide Slider (red) while the bar is showing -- pressing it hides the bar at
    // once and takes this popup with it, and the window that follows only tells the user how to
    // bring the bar back. While the bar is hidden (this popup opened from the Timeline's own menu)
    // the row is a BLUE Unhide Slider instead; that one never exists in the bar's own popup.
    // The popup's content right edge (the row spans the whole content width) -- the Minimap flyout
    // is placed beside the popup, so it needs to know where the popup ends.
    let hidden = app.session.state.timeline_ui.zoom_bar_hidden;
    let mut content_right = 0.0f32;
    ui.allocate_ui_with_layout(egui::vec2(ui.available_width(), 30.0), egui::Layout::left_to_right(egui::Align::Center), |ui| {
        let row_w = ui.available_width();
        let (r, _) = ui.allocate_exact_size(egui::vec2(row_w, 30.0), Sense::click());
        content_right = r.max.x;
        let hide_resp =
            ui.interact(r, egui::Id::unique(if hidden { "timeline-zoombar-unhide-slider" } else { "timeline-zoombar-hide-slider" }), Sense::click());
        if hide_resp.hovered() {
            ui.painter().rect_filled(r, 4.0, app.tokens.switch_border.gamma_multiply(0.35));
        }
        let (label, color) = if hidden { (tl!("Unhide Slider"), app.tokens.accent) } else { (tl!("Hide Slider"), Color32::from_rgb(0xf8, 0x7e, 0x7a)) };
        ui.painter().text(r.center(), Align2::CENTER_CENTER, label, Tokens::ui(11.5), color);
        app.auto.add(if hidden { "timeline.zoomBar.settings.unhideSlider" } else { "timeline.zoomBar.settings.hideSlider" }, r, label);
        if hide_resp.clicked() {
            set_zoombar_hidden(app, ui.ctx(), !hidden);
            if hidden {
                app.tl.zoombar_settings_pos = None;
            } else {
                app.tl.zoombar_hide_dialog_open = true;
            }
            ui.close();
            ui.ctx().request_repaint();
        }
    });
    // "Controls": not a switch -- a list of the slider's gestures, in a flyout beside the menu
    // (the Minimap Settings flyout's exact recipe). The text centers like Hide Slider's; the
    // arrow stays where it was.
    let full_w = ui.available_width();
    let (row, _) = ui.allocate_exact_size(egui::vec2(full_w, 30.0), Sense::hover());
    let label = tl!("Controls");
    let galley = ui.painter().layout_no_wrap(label.to_string(), Tokens::ui(11.5), app.tokens.text);
    ui.painter().galley(pos2(row.center().x - galley.size().x / 2.0, row.center().y - galley.size().y / 2.0), galley, app.tokens.text);
    // the arrow points at whichever side the flyout fits on, the same distance from that wall
    app.auto.add("timeline.zoomBar.settings.controls", row, "Controls");
    let controls_right = hover_flyout(
        app,
        ui,
        egui::Id::unique("timeline-zoombar-advanced-flyout"),
        row,
        content_right,
        CONTROLS_FLYOUT_W,
        true,
        false,
        "timeline.zoomBar.settings.controlsFlyout",
        "Controls",
        &[
            (tl!("Center on Playhead:"), "Double Click", 0),
            (tl!("Explode Slider:"), "Triple Click", 1),
            (tl!("Alternate Drag Speed:"), "Shift + Drag", 2),
            (tl!("Fast Timeline Scroll:"), "Unbound", 5),
            (tl!("Zoom Timeline:"), "Alt + Scroll", 7),
            (tl!("Zoom In:"), "+", 8),
            (tl!("Zoom Out:"), "-", 10),
        ],
    );
    controls_arrow(ui, row, content_w, content_right, controls_right, app.tokens.text_dim);
    let settings = &mut app.session.state.timeline_ui;

    // Smooth Animations: a centered pill switch (left) + label group instead of a checkbox
    let (row, _) = ui.allocate_exact_size(egui::vec2(content_w, 30.0), Sense::hover());
    let t = &app.tokens;
    let label = tl!("Smooth Animations");
    let galley = ui.painter().layout_no_wrap(label.to_string(), Tokens::ui(11.5), t.text);
    let sw_w = 34.0;
    let group_w = sw_w + 10.0 + galley.size().x;
    let x0 = row.min.x + ((content_w - group_w) / 2.0).max(0.0);
    let sw = Rect::from_min_size(pos2(x0, row.center().y - 8.0), egui::vec2(sw_w, 16.0));
    let c = pill_switch(ui, sw, settings.smooth_animations, t, egui::Id::unique("timeline-zoombar-smooth"), tl!("Smooth Animations"));
    ui.painter().galley(pos2(x0 + sw_w + 10.0, row.center().y - galley.size().y / 2.0), galley, t.text);
    app.auto.add("timeline.zoomBar.settings.smoothAnimations", c.rect, tl!("Smooth Animations"));
    setting_hint(ui, row, tl!("The edges of sliders, Center on Playhead, and Mirror Slider have smooth animations when enabled."));
    if c.clicked() {
        settings.smooth_animations = !settings.smooth_animations;
        changed = true;
    }
    // Inverted Controls: the wheel over the tracks scrolls the Timeline the other way in time,
    // and the wheel while grabbing the slider's middle zooms the other way
    let (row, _) = ui.allocate_exact_size(egui::vec2(content_w, 30.0), Sense::hover());
    let label = tl!("Invert Scrolling");
    let galley = ui.painter().layout_no_wrap(label.to_string(), Tokens::ui(11.5), t.text);
    let group_w = sw_w + 10.0 + galley.size().x;
    let x0 = row.min.x + ((content_w - group_w) / 2.0).max(0.0);
    let sw = Rect::from_min_size(pos2(x0, row.center().y - 8.0), egui::vec2(sw_w, 16.0));
    let c = pill_switch(ui, sw, settings.inverted_controls, t, egui::Id::unique("timeline-zoombar-inverted"), tl!("Invert Scrolling"));
    ui.painter().galley(pos2(x0 + sw_w + 10.0, row.center().y - galley.size().y / 2.0), galley, t.text);
    app.auto.add("timeline.zoomBar.settings.invertedControls", c.rect, tl!("Invert Scrolling"));
    setting_hint(ui, row, tl!("Reverses timeline scrolling and zooming when holding the center of the slider."));
    if c.clicked() {
        settings.inverted_controls = !settings.inverted_controls;
        changed = true;
    }
    let c = settings_pill_row(
        ui,
        t,
        content_w,
        tl!("Control Track Sliders"),
        egui::Id::unique("timeline-zoombar-control-tracks"),
        settings.control_tracks,
        Some(tl!("Drag the timeline slider vertically to resize both track sections. Recommended for when Track Sliders are hidden.")),
    );
    app.auto.add("timeline.zoomBar.settings.controlTracks", c.rect, "Control Track Sliders");
    if c.clicked() {
        settings.control_tracks = !settings.control_tracks;
        changed = true;
    }
    // Playhead Centering: same centered pill switch + label group as the row above it
    let (row, _) = ui.allocate_exact_size(egui::vec2(content_w, 30.0), Sense::hover());
    let label = tl!("Playhead Centering");
    let galley = ui.painter().layout_no_wrap(label.to_string(), Tokens::ui(11.5), t.text);
    let group_w = sw_w + 10.0 + galley.size().x;
    let x0 = row.min.x + ((content_w - group_w) / 2.0).max(0.0);
    let sw = Rect::from_min_size(pos2(x0, row.center().y - 8.0), egui::vec2(sw_w, 16.0));
    let c = pill_switch(ui, sw, settings.playhead_centering, t, egui::Id::unique("timeline-zoombar-center"), tl!("Playhead Centering"));
    ui.painter().galley(pos2(x0 + sw_w + 10.0, row.center().y - galley.size().y / 2.0), galley, t.text);
    app.auto.add("timeline.zoomBar.settings.playheadCentering", c.rect, tl!("Playhead Centering"));
    setting_hint(ui, row, tl!("When the playhead enters the visible timeline, zooming in gravitates the view toward it."));
    if c.clicked() {
        settings.playhead_centering = !settings.playhead_centering;
        if !settings.playhead_centering {
            app.tl.playhead_captured = false;
            app.tl.playhead_capture_vis = 0.0;
        }
        changed = true;
    }
    // Alternate Slider Position: same centered pill switch + label group as the rows above it
    let (row, _) = ui.allocate_exact_size(egui::vec2(content_w, 30.0), Sense::hover());
    let label = tl!("Alternate Slider Position");
    let galley = ui.painter().layout_no_wrap(label.to_string(), Tokens::ui(11.5), t.text);
    let group_w = sw_w + 10.0 + galley.size().x;
    let x0 = row.min.x + ((content_w - group_w) / 2.0).max(0.0);
    let sw = Rect::from_min_size(pos2(x0, row.center().y - 8.0), egui::vec2(sw_w, 16.0));
    let c = pill_switch(ui, sw, settings.alternate_slider_position, t, egui::Id::unique("timeline-zoombar-alternate"), tl!("Alternate Slider Position"));
    ui.painter().galley(pos2(x0 + sw_w + 10.0, row.center().y - galley.size().y / 2.0), galley, t.text);
    app.auto.add("timeline.zoomBar.settings.alternateSliderPosition", c.rect, tl!("Alternate Slider Position"));
    setting_hint(ui, row, tl!("Moves the timeline slider to the top."));
    if c.clicked() {
        settings.alternate_slider_position = !settings.alternate_slider_position;
        changed = true;
    }
    ui.separator();
    // Maximum Zoom Level: the smallest view the zoom bar may collapse to
    settings_heading(ui, t, tl!("Maximum Zoom Level"), tl!("Sets the maximum amount you can zoom in."));
    let zoom_presets = [0.1, 0.2, 0.4, 0.8];
    let mut zoom_choice =
        if settings.max_zoom_custom { 4 } else { zoom_presets.iter().position(|v| (*v - settings.max_zoom_seconds).abs() < f64::EPSILON).unwrap_or(2) };
    let zoom_labels = [tl!("Closest"), tl!("Close"), tl!("Default"), tl!("Far"), tl!("Custom")];
    let (r, moved) = ui
        .allocate_ui_with_layout(egui::vec2(content_w, 46.0), egui::Layout::left_to_right(egui::Align::Center), |ui| {
            tick_slider(ui, &mut zoom_choice, &zoom_labels, &app.tokens)
        })
        .inner;
    changed |= moved;
    app.auto.add("timeline.zoomBar.settings.maxZoomLevel.slider", r.rect, "Maximum zoom level");
    if moved {
        match zoom_choice {
            0..=3 => {
                settings.max_zoom_seconds = zoom_presets[zoom_choice];
                settings.max_zoom_custom = false;
            }
            _ => {
                // Custom again: restore the last custom value, never reset it to a default
                if !settings.max_zoom_custom {
                    settings.max_zoom_seconds = if settings.max_zoom_last_custom.is_finite() { settings.max_zoom_last_custom.clamp(0.05, 3.0) } else { 0.4 };
                }
                settings.max_zoom_custom = true;
            }
        }
    }
    // A constant, always-visible and editable value row, centered like the sliders. It keeps the
    // popup's height fixed, so the layout is identical on the popup's first frame and forever after.
    ui.allocate_ui_with_layout(egui::vec2(content_w, 24.0), egui::Layout::left_to_right(egui::Align::Center).with_main_align(egui::Align::Center), |ui| {
        ui.spacing_mut().item_spacing.x = 3.0;
        ui.add_sized(egui::vec2(46.0, 18.0), egui::Label::new(tl!("Value:")).wrap());
        let (response, value) = hot_number_numeric(
            ui,
            tl!("Maximum zoom level in seconds"),
            egui::Id::unique("timeline-zoombar-max-zoom"),
            settings.max_zoom_seconds,
            HotNumberOptions::new(0.05, (0.05, 3.0), 2, "s", &app.tokens),
        );
        app.auto.add("timeline.zoomBar.settings.maxZoomLevel", response.rect, "Maximum zoom level in seconds");
        if let Some(value) = value {
            settings.max_zoom_seconds = value;
            settings.max_zoom_last_custom = value;
            settings.max_zoom_custom = true;
            changed = true;
        }
    });
    // `timeline::show` applies the new ceiling after measuring this frame's content width. This
    // popup runs before that measurement, so clamping here could shrink zoom using stale geometry.
    ui.separator();
    // Alternate Drag Speed first
    // the gesture is part of the heading only while it is still the way to do it: a Controls
    // rebinding makes it disappear
    let alt_speed_label = if uses_default_gesture(&settings.control_alternate_drag_speed, "Shift + Drag") {
        tl!("Alternate Drag Speed (Shift + Drag)")
    } else {
        tl!("Alternate Drag Speed")
    };
    settings_heading(ui, t, alt_speed_label, tl!("Mimics the feature in Blender. A high multiplier can make it faster than dragging normally."));
    let speed_presets = [0.25, 0.5, 1.0, 2.0];
    let mut speed_choice =
        if settings.shift_drag_custom { 4 } else { speed_presets.iter().position(|v| (*v - settings.shift_drag_speed).abs() < f64::EPSILON).unwrap_or(3) };
    let speed_labels = [tl!("Very Slow"), tl!("Slow"), tl!("Default"), tl!("Fast"), tl!("Custom")];
    let (r, moved) = ui
        .allocate_ui_with_layout(egui::vec2(content_w, 46.0), egui::Layout::left_to_right(egui::Align::Center), |ui| {
            tick_slider(ui, &mut speed_choice, &speed_labels, &app.tokens)
        })
        .inner;
    changed |= moved;
    app.auto.add("timeline.zoomBar.settings.shiftDragSpeed.slider", r.rect, "Alternate drag speed");
    if moved {
        match speed_choice {
            0..=3 => {
                settings.shift_drag_speed = speed_presets[speed_choice];
                settings.shift_drag_custom = false;
            }
            _ => {
                // Custom again: restore the last custom value, never reset it to a default
                if !settings.shift_drag_custom {
                    settings.shift_drag_speed = last_custom_value(settings.shift_drag_last_custom, 50.0);
                }
                settings.shift_drag_custom = true;
            }
        }
    }
    // A constant, always-visible and editable value row, centered like the sliders. It keeps the
    // popup's height fixed, so the layout is identical on the popup's first frame and forever
    // after (a first-frame mismatch is what made the popup jitter).
    ui.allocate_ui_with_layout(egui::vec2(content_w, 24.0), egui::Layout::left_to_right(egui::Align::Center).with_main_align(egui::Align::Center), |ui| {
        ui.spacing_mut().item_spacing.x = 3.0;
        ui.add_sized(egui::vec2(46.0, 18.0), egui::Label::new(tl!("Value:")).wrap());
        let (response, value) = hot_number_numeric(
            ui,
            tl!("Alternate drag speed multiplier"),
            egui::Id::unique("timeline-zoombar-shift-speed"),
            settings.shift_drag_speed,
            HotNumberOptions::new(0.1, (0.1, 50.0), 1, "x", &app.tokens),
        );
        app.auto.add("timeline.zoomBar.settings.shiftDragSpeed", response.rect, "Alternate drag speed multiplier");
        if let Some(value) = value {
            settings.shift_drag_speed = value;
            settings.shift_drag_last_custom = value;
            settings.shift_drag_custom = true;
            changed = true;
        }
    });
    ui.separator();
    // Playhead Centering Range second
    let playhead_range_label = if uses_default_gesture(&settings.control_center_on_playhead, "Double Click") {
        tl!("Playhead Centering Range (Double Click)")
    } else {
        tl!("Playhead Centering Range")
    };
    settings_heading(ui, t, playhead_range_label, tl!("Sets how much of the timeline is visible on each side of the playhead."));
    let mut range_choice = if settings.playhead_side_custom {
        4
    } else {
        [3.0, 5.0, 10.0, 30.0].iter().position(|v| (*v - settings.playhead_side_seconds).abs() < f64::EPSILON).unwrap_or(2)
    };
    let range_labels = [tl!("3s"), tl!("5s"), tl!("10s"), tl!("30s"), tl!("Custom")];
    let (r, moved) = ui
        .allocate_ui_with_layout(egui::vec2(content_w, 46.0), egui::Layout::left_to_right(egui::Align::Center), |ui| {
            tick_slider(ui, &mut range_choice, &range_labels, &app.tokens)
        })
        .inner;
    changed |= moved;
    app.auto.add("timeline.zoomBar.settings.playheadSideSeconds.slider", r.rect, "Playhead centering range");
    if moved {
        match range_choice {
            0..=3 => {
                settings.playhead_side_seconds = [3.0, 5.0, 10.0, 30.0][range_choice];
                settings.playhead_side_custom = false;
            }
            _ => {
                // Custom again: restore the last custom value, never reset it to a default
                if !settings.playhead_side_custom {
                    settings.playhead_side_seconds = last_custom_value(settings.playhead_side_last_custom, 3600.0);
                }
                settings.playhead_side_custom = true;
            }
        }
    }
    // A constant, always-visible and editable value row, centered like the sliders, so the popup
    // never resizes.
    ui.allocate_ui_with_layout(egui::vec2(content_w, 24.0), egui::Layout::left_to_right(egui::Align::Center).with_main_align(egui::Align::Center), |ui| {
        ui.spacing_mut().item_spacing.x = 3.0;
        ui.add_sized(egui::vec2(46.0, 18.0), egui::Label::new(tl!("Value:")).wrap());
        let (response, value) = hot_number_numeric(
            ui,
            tl!("Playhead centering range in seconds"),
            egui::Id::unique("timeline-zoombar-side"),
            settings.playhead_side_seconds,
            HotNumberOptions::new(0.1, (0.1, 3600.0), 1, "s", &app.tokens),
        );
        app.auto.add("timeline.zoomBar.settings.playheadSideSeconds", response.rect, "Playhead centering range in seconds");
        if let Some(value) = value {
            settings.playhead_side_seconds = value;
            settings.playhead_side_last_custom = value;
            settings.playhead_side_custom = true;
            changed = true;
        }
    });
    // The same faint divider as between the other sections.
    ui.separator();
    ui.add_space(2.0);
    let mut whole = settings.show_whole_timeline;
    // Slider Minimap: a pill switch like the rows above, with an arrow promising more settings --
    // hovering the row opens the Minimap Settings flyout, as wide as this menu. While the minimap
    // is OFF the label is greyed out, the arrow is gone, and hovering opens nothing.
    let minimap_on = settings.slider_minimap;
    let (row, _) = ui.allocate_exact_size(egui::vec2(content_w, 30.0), Sense::hover());
    let label = tl!("Slider Minimap");
    // off: a darker grey than the other dimmed labels, so the dead row reads as dead
    let label_color = if minimap_on { t.text } else { t.text_faint };
    let galley = ui.painter().layout_no_wrap(label.to_string(), Tokens::ui(11.5), label_color);
    let group_w = sw_w + 10.0 + galley.size().x;
    let x0 = row.min.x + ((content_w - group_w) / 2.0).max(0.0);
    let sw = Rect::from_min_size(pos2(x0, row.center().y - 8.0), egui::vec2(sw_w, 16.0));
    let c = pill_switch(ui, sw, minimap_on, t, egui::Id::unique("timeline-zoombar-minimap"), tl!("Slider Minimap"));
    ui.painter().galley(pos2(x0 + sw_w + 10.0, row.center().y - galley.size().y / 2.0), galley, label_color);
    if minimap_on {
        // the same side-aware arrow as the Controls row, at the same distance from the wall
        controls_arrow(ui, row, content_w, content_right, flyout_to_the_right(ui, row, content_right, CONTENT_W), t.text_dim);
    }
    app.auto.add("timeline.zoomBar.settings.sliderMinimap", c.rect, tl!("Slider Minimap"));
    if c.clicked() {
        settings.slider_minimap = !settings.slider_minimap;
        if !settings.slider_minimap {
            // turning the minimap off kills its flyout in the SAME click -- no grace period,
            // nothing keeps ticking for a setting that is gone
            let flyout_id = egui::Id::unique("timeline-zoombar-minimap-flyout");
            ui.data_mut(|d| {
                d.insert_temp(flyout_id.with("open"), false);
                d.insert_temp(grace_key(), -1e9f64);
            });
        }
        changed = true;
    }
    // Minimap Settings: a draft flyout behind the arrow, kept open while the pointer is on the
    // row, on the settings menu, in the ring of gaps around it, or in the flyout itself. A plain
    // hover panel, not a menu popup: a nested menu popup would close the settings menu it lives in.
    //
    // The keep-alive MUST use plain geometry against the raw pointer position. egui's
    // `rect_contains_pointer` is layer-bound: it reports false for the gap strips (they are over
    // the Timeline's layer) and for the flyout (its own Area, a different layer), which is exactly
    // why the flyout used to vanish the moment the pointer left the row. A 300 ms grace after the
    // last hit covers any one-frame hole in the geometry.
    let flyout_id = egui::Id::unique("timeline-zoombar-minimap-flyout");
    let rect_key = flyout_id.with("rect");
    let height_key = flyout_id.with("height");
    let stamp_key = flyout_id.with("stamp");
    let side_key = flyout_id.with("right-side");
    let pos_key = flyout_id.with("opening-pos");
    let was_open = ui.data(|d| d.get_temp::<bool>(flyout_id.with("open"))).unwrap_or(false);
    let mut fill = settings.fill_clips;
    let mut pri = settings.prioritise_audio;
    let mut pos_ind = settings.show_position_indicator;
    // Beside the settings menu, never on top of it: to the right when the screen has room for it
    // there, otherwise to the left of the menu. Bottom-aligned with the menu's bottom edge and
    // always fully opaque -- an egui Area fades in by default, which is what made a flipped flyout
    // look ghostly. The placement is relative to the menu only, so where the slider was
    // right-clicked cannot shift it.
    let margin = ui.spacing().menu_margin;
    let popup_right = content_right + f32::from(margin.right);
    let popup_left = row.min.x - f32::from(margin.left);
    let flyout_w = CONTENT_W + 16.0 + f32::from(margin.left) + f32::from(margin.right);
    let screen = ui.ctx().content_rect();
    let gap = 4.0;
    let fits_right = popup_right + gap + flyout_w <= screen.right();
    let to_the_right = if was_open { ui.data(|d| d.get_temp::<bool>(side_key)).unwrap_or(fits_right) } else { fits_right };
    let x = if to_the_right { popup_right + gap } else { (popup_left - gap - flyout_w).max(screen.left()) };
    let popup_bottom = row.max.y + f32::from(margin.bottom);
    let flyout_h = ui.data(|d| d.get_temp::<f32>(height_key)).unwrap_or(MINIMAP_FLYOUT_HEIGHT);
    let alternate_top = settings.alternate_slider_position;
    let y = if alternate_top { row.center().y - flyout_h / 2.0 } else { popup_bottom - flyout_h };
    let y = y.clamp(screen.top(), (screen.bottom() - flyout_h).max(screen.top()));
    let (x, y) = if was_open { ui.data(|d| d.get_temp::<Pos2>(pos_key)).map_or((x, y), |p| (p.x, p.y)) } else { (x, y) };
    let flyout_rect = Rect::from_min_size(pos2(x, y), egui::vec2(flyout_w, flyout_h));
    // the keep-alive frame around the menu: the pointer may travel anywhere on the menu or in a
    // 24 pt ring of gaps around it (and across the gap strip to the flyout) without losing it
    let menu = Rect::from_min_max(pos2(popup_left, row.min.y - 30.0 - f32::from(margin.top)), pos2(popup_right, popup_bottom));
    let (gx0, gx1) = if to_the_right { (popup_right, x) } else { (x + flyout_w, popup_left) };
    let side = Rect::from_min_max(pos2(gx0.min(gx1), y), pos2(gx0.max(gx1), popup_bottom.max(y + flyout_h)));
    let under = Rect::from_min_max(pos2(popup_left, popup_bottom), pos2(popup_right, popup_bottom + 24.0));
    let around = Rect::from_min_max(pos2(menu.min.x - 24.0, menu.min.y - 24.0), pos2(menu.max.x + 24.0, menu.max.y + 24.0));
    let pointer = ui.input(|i| i.pointer.hover_pos().or(i.pointer.interact_pos()));
    let over = |r: Rect| pointer.is_some_and(|p| r.contains(p));
    let prev_flyout = ui.data(|d| d.get_temp::<Rect>(rect_key)).unwrap_or(flyout_rect);
    // the settings' own popups count too: a colour picker opened from this flyout hangs below it,
    // and working in it must never take the flyout (and the picker with it) away
    let now = ui.input(|i| i.time);
    // Working in the colour picker (or the three seconds after it) keeps the flyout alive no
    // matter where the pointer goes.
    let keep = ui.data(|d| d.get_temp::<f64>(grace_key())).unwrap_or(-1e9);
    // the row is live only while the minimap is on: off, hovering it opens nothing
    let row_hot = over(row) && settings.slider_minimap;
    let keep_alive = row_hot
        || over(menu)
        || over(side)
        || over(under)
        || over(around)
        || over(prev_flyout)
        || over(flyout_rect)
        || pointer.is_some_and(|p| family_contains_prev(ui, p))
        || now - keep < 3.0;
    // The ROW opens the flyout -- and only on a real hover of it. The menu appears under the
    // cursor, often right on this row, and "the menu opens default" means the flyout must never
    // pop up just because it landed there: the pointer has to move first.
    let opened_at = ui.data(|d| d.get_temp::<Pos2>(open_pointer_key())).unwrap_or(pos2(-1e9, -1e9));
    let moved = pointer.is_some_and(|p| (p - opened_at).length() > 6.0);
    let trigger = row_hot && moved;
    // everything else (the gap strips, the flyout itself, the colour pickers) only KEEPS it open;
    // the flyout never exists at all while the minimap is off
    let stamp = ui.data(|d| d.get_temp::<f64>(stamp_key)).unwrap_or(-1e9);
    let open = settings.slider_minimap && (trigger || (was_open && (keep_alive || now - stamp < 0.3)));
    if open {
        ui.data_mut(|d| d.insert_temp(stamp_key, now));
        ui.data_mut(|d| d.insert_temp(side_key, to_the_right));
        if !was_open {
            ui.data_mut(|d| d.insert_temp(pos_key, pos2(x, y)));
        }
    } else {
        ui.data_mut(|d| d.remove_temp::<bool>(side_key));
        ui.data_mut(|d| d.remove_temp::<Pos2>(pos_key));
    }
    ui.data_mut(|d| d.insert_temp(flyout_id.with("open"), open));
    if open {
        let panel = egui::Area::new(flyout_id).order(egui::Order::Foreground).fixed_pos(pos2(x, y)).fade_in(false).interactable(true).show(ui.ctx(), |ui| {
            egui::Frame::popup(ui.style()).show(ui, |ui| {
                ui.set_min_width(CONTENT_W + 16.0);
                ui.set_max_width(CONTENT_W + 16.0);
                // Show Whole Timeline: with it on the minimap is locked to the full Timeline
                // extent, so the "you are here" line can travel past the last clip.
                let (frow, _) = ui.allocate_exact_size(egui::vec2(CONTENT_W, 30.0), Sense::hover());
                let flabel = tl!("Show Whole Timeline");
                let fgalley = ui.painter().layout_no_wrap(flabel.to_string(), Tokens::ui(11.5), t.text);
                let fsw_w = 34.0;
                let fgroup_w = fsw_w + 10.0 + fgalley.size().x;
                let fx0 = frow.min.x + ((CONTENT_W - fgroup_w) / 2.0).max(0.0);
                let fsw = Rect::from_min_size(pos2(fx0, frow.center().y - 8.0), egui::vec2(fsw_w, 16.0));
                let c = pill_switch(ui, fsw, whole, t, egui::Id::unique("timeline-zoombar-minimap-whole"), tl!("Show Whole Timeline"));
                ui.painter().galley(pos2(fx0 + fsw_w + 10.0, frow.center().y - fgalley.size().y / 2.0), fgalley, t.text);
                app.auto.add("timeline.zoomBar.settings.showWholeTimeline", c.rect, tl!("Show Whole Timeline"));
                if c.clicked() {
                    whole = !whole;
                }
                // Prioritise Audio: where an audio clip and a video clip sit at the same place
                // the audio shows on top of the video instead of under it.
                let (prow, _) = ui.allocate_exact_size(egui::vec2(CONTENT_W, 30.0), Sense::hover());
                let plabel = tl!("Prioritise Audio");
                let pgalley = ui.painter().layout_no_wrap(plabel.to_string(), Tokens::ui(11.5), t.text);
                let pgroup_w = fsw_w + 10.0 + pgalley.size().x;
                let px0 = prow.min.x + ((CONTENT_W - pgroup_w) / 2.0).max(0.0);
                let psw = Rect::from_min_size(pos2(px0, prow.center().y - 8.0), egui::vec2(fsw_w, 16.0));
                let c = pill_switch(ui, psw, pri, t, egui::Id::unique("timeline-zoombar-minimap-pri-audio"), tl!("Prioritise Audio"));
                ui.painter().galley(pos2(px0 + fsw_w + 10.0, prow.center().y - pgalley.size().y / 2.0), pgalley, t.text);
                app.auto.add("timeline.zoomBar.settings.prioritiseAudio", c.rect, tl!("Prioritise Audio"));
                if c.clicked() {
                    pri = !pri;
                }
                // Position Indicator: the "you are here" line on the slider. Off: hidden.
                let c = settings_pill_row(ui, t, CONTENT_W, tl!("Position Indicator"), egui::Id::unique("timeline-zoombar-minimap-pos-ind"), pos_ind, None);
                app.auto.add("timeline.zoomBar.settings.positionIndicator", c.rect, "Position Indicator");
                if c.clicked() {
                    pos_ind = !pos_ind;
                }
                // Fill Clips: on, the minimap's clips are filled in; off (default), outlines.
                ui.separator();
                let (grow, _) = ui.allocate_exact_size(egui::vec2(CONTENT_W, 30.0), Sense::hover());
                let glabel = tl!("Fill Clips");
                let ggalley = ui.painter().layout_no_wrap(glabel.to_string(), Tokens::ui(11.5), t.text);
                let ggroup_w = fsw_w + 10.0 + ggalley.size().x;
                let gxp = grow.min.x + ((CONTENT_W - ggroup_w) / 2.0).max(0.0);
                let gsw = Rect::from_min_size(pos2(gxp, grow.center().y - 8.0), egui::vec2(fsw_w, 16.0));
                let c = pill_switch(ui, gsw, fill, t, egui::Id::unique("timeline-zoombar-minimap-fill"), tl!("Fill Clips"));
                ui.painter().galley(pos2(gxp + fsw_w + 10.0, grow.center().y - ggalley.size().y / 2.0), ggalley, t.text);
                app.auto.add("timeline.zoomBar.settings.fillClips", c.rect, tl!("Fill Clips"));
                if c.clicked() {
                    fill = !fill;
                }
                // Position Indicator Color: the colour of the "you are here" line. One call --
                // copy this line for every future colour setting.
                changed |= color_setting(
                    ui,
                    t,
                    egui::Id::unique("timeline-zoombar-minimap-color"),
                    tl!("Position Indicator Color:"),
                    &mut settings.position_indicator_color,
                    "#5895ec",
                );
                changed |= color_setting(
                    ui,
                    t,
                    egui::Id::unique("timeline-zoombar-minimap-video-color"),
                    tl!("Video Indicator Color:"),
                    &mut settings.video_indicator_color,
                    "#64b5f6",
                );
                changed |= color_setting(
                    ui,
                    t,
                    egui::Id::unique("timeline-zoombar-minimap-audio-color"),
                    tl!("Audio Indicator Color:"),
                    &mut settings.audio_indicator_color,
                    "#2e7d32",
                );
                changed |= color_setting(
                    ui,
                    t,
                    egui::Id::unique("timeline-zoombar-minimap-offline-color"),
                    tl!("Offline Indicator Color:"),
                    &mut settings.offline_indicator_color,
                    "#d32f2f",
                );
            });
        });
        app.auto.add("timeline.zoomBar.settings.minimapFlyout", panel.response.rect, "Minimap Settings");
        family_push(ui, panel.response.rect);
        ui.data_mut(|d| d.insert_temp(rect_key, panel.response.rect));
        ui.data_mut(|d| d.insert_temp(height_key, panel.response.rect.height()));
    }
    if whole != settings.show_whole_timeline {
        settings.show_whole_timeline = whole;
        changed = true;
    }
    if fill != settings.fill_clips {
        settings.fill_clips = fill;
        changed = true;
    }
    if pri != settings.prioritise_audio {
        settings.prioritise_audio = pri;
        changed = true;
    }
    if pos_ind != settings.show_position_indicator {
        settings.show_position_indicator = pos_ind;
        changed = true;
    }
    // A click anywhere outside the colour picker aborts its keep-alive grace at once -- including
    // a click on the settings menu or the flyout themselves.
    if ui.input(|i| i.pointer.button_clicked(egui::PointerButton::Primary))
        && let Some(pos) = ui.input(|i| i.pointer.interact_pos())
        && !ui.data(|d| d.get_temp::<Rect>(color_rect_key())).unwrap_or(Rect::NOTHING).contains(pos)
    {
        ui.data_mut(|d| d.insert_temp(grace_key(), -1e9f64));
    }
    if changed {
        // the colour wheel and the number fields report every frame of a drag: remember the
        // change, but bump the (cache-invalidating) revision only once the gesture ends
        ui.data_mut(|d| d.insert_temp(egui::Id::unique("tl-zoombar-view-pending"), true));
    }
    flush_pending_zoombar_view_change(app, ui.ctx());
}

/// A pill on/off switch for the zoom-bar settings popup: red while off, the accent blue while on.
fn pill_switch(ui: &mut egui::Ui, r: Rect, on: bool, t: &Tokens, id: egui::Id, label: &str) -> egui::Response {
    const SWITCH_OFF: Color32 = Color32::from_rgb(0xf8, 0x7e, 0x7a);
    const KNOB: Color32 = Color32::from_gray(245);
    let resp = ui.interact(r.expand(3.0), id, Sense::click());
    let p = ui.painter();
    let rad = r.height() / 2.0;
    if on {
        p.rect_filled(r, rad, t.accent);
        p.circle_filled(pos2(r.max.x - rad, r.center().y), rad - 3.0, KNOB);
    } else {
        p.rect_filled(r, rad, SWITCH_OFF);
        p.circle_filled(pos2(r.min.x + rad, r.center().y), rad - 3.0, KNOB);
    }
    resp.on_hover_text(label)
}

/// One centered pill row of a slider settings menu: a label and its switch, over the row's full
/// width so the rows line up and nothing sits off-center in the popup.
fn settings_pill_row(ui: &mut egui::Ui, t: &Tokens, row_w: f32, label: &str, id: egui::Id, on: bool, hint: Option<&str>) -> egui::Response {
    let (row, _) = ui.allocate_exact_size(egui::vec2(row_w, 30.0), Sense::hover());
    let galley = ui.painter().layout_no_wrap(label.to_string(), Tokens::ui(11.5), t.text);
    let sw_w = 34.0;
    let group_w = sw_w + 10.0 + galley.size().x;
    let x0 = row.min.x + ((row_w - group_w) / 2.0).max(0.0);
    let sw = Rect::from_min_size(pos2(x0, row.center().y - 8.0), egui::vec2(sw_w, 16.0));
    let c = pill_switch(ui, sw, on, t, id, label);
    let label_pos = pos2(x0 + sw_w + 10.0, row.center().y - galley.size().y / 2.0);
    ui.painter().galley(label_pos, galley.clone(), t.text);
    if let Some(hint) = hint {
        setting_hint(ui, row, hint);
    }
    c
}

/// The next input this frame, as the text a Controls box shows: a key chord ("Ctrl + Shift + K")
/// or a mouse button ("Right Click"). One per frame, the last one wins.
/// Modifier keys egui reports as key events of their own, before the key they modify arrives
/// (the Shortcuts dialog's rule). A chord never binds on a modifier alone: pressing Shift means
/// "Shift is coming", not a binding of `Shift + Shift`.
fn is_modifier_key(key: egui::Key) -> bool {
    use egui::Key::*;
    matches!(key, ShiftLeft | ShiftRight | ControlLeft | ControlRight | AltLeft | AltRight | SuperLeft | SuperRight)
}

fn modifier_count(modifiers: egui::Modifiers) -> u8 {
    u8::from(modifiers.shift) + u8::from(modifiers.alt) + u8::from(modifiers.ctrl || modifiers.mac_cmd)
}

fn key_binding_name(key: egui::Key) -> String {
    match key {
        egui::Key::Plus => "+".into(),
        egui::Key::Equals => "=".into(),
        egui::Key::Minus => "-".into(),
        egui::Key::Backslash => "\\".into(),
        _ => format!("{key:?}"),
    }
}

/// The chord or mouse button a frame's events name: keyboard and clipboard events take precedence
/// over pointer gestures, so a custom Copy chord is never mistaken for a double-click gesture.
/// Modifier presses and all releases are skipped -- the chord completes when the key it modifies
/// arrives, never when a modifier or its release does.
fn chord_and_button(events: &[egui::Event], fallback_modifiers: egui::Modifiers) -> Option<String> {
    for e in events {
        match e {
            egui::Event::Key { key, modifiers, pressed: true, .. } if !is_modifier_key(*key) => {
                let mut s = String::new();
                if modifiers.ctrl || modifiers.mac_cmd {
                    s.push_str("Ctrl + ");
                }
                if modifiers.shift {
                    s.push_str("Shift + ");
                }
                if modifiers.alt {
                    s.push_str("Alt + ");
                }
                s.push_str(&key_binding_name(*key));
                return Some(s);
            }
            egui::Event::Paste(_) | egui::Event::PasteImage(_) if fallback_modifiers.command => return Some("Ctrl + V".into()),
            egui::Event::Copy if fallback_modifiers.command => return Some("Ctrl + C".into()),
            _ => {}
        }
    }
    for e in events {
        match e {
            egui::Event::PointerButton { button, pressed: true, .. } => {
                return Some(
                    match button {
                        egui::PointerButton::Primary => "Left Click",
                        egui::PointerButton::Secondary => "Right Click",
                        egui::PointerButton::Middle => "Middle Click",
                        _ => "Extra Click",
                    }
                    .to_string(),
                );
            }
            egui::Event::MouseWheel { modifiers, .. } => {
                let mut s = String::new();
                if modifiers.ctrl || modifiers.mac_cmd {
                    s.push_str("Ctrl + ");
                }
                if modifiers.shift {
                    s.push_str("Shift + ");
                }
                if modifiers.alt {
                    s.push_str("Alt + ");
                }
                s.push_str("Scroll");
                return Some(s);
            }
            _ => {}
        }
    }
    None
}

/// One Controls binding (0 = Center on Playhead, 1 = Explode Slider, 2 = Alternate Drag Speed,
/// 3 = Settings): empty means "the built-in gesture".
fn control_binding(app: &mut FilmcraftApp, slot: usize) -> &mut String {
    let s = &mut app.session.state.timeline_ui;
    match slot {
        0 => &mut s.control_center_on_playhead,
        1 => &mut s.control_explode_slider,
        2 => &mut s.control_alternate_drag_speed,
        3 => &mut s.control_settings,
        _ => s.timeline_control_bindings.get_mut(slot.saturating_sub(4)).unwrap_or(&mut s.control_settings),
    }
}

/// The track sliders' own Controls bindings, deliberately UNLINKED from the zoom bar's: both
/// menus share the action names, but naming a control on one slider must never rebind the
/// other's (Mirror Slider / Explode Slider / ...).
fn track_control_binding(app: &mut FilmcraftApp, slot: usize) -> &mut String {
    let s = &mut app.session.state.timeline_ui;
    match slot {
        0 => &mut s.track_control_mirror,
        1 => &mut s.track_control_explode,
        2 => &mut s.track_control_alternate_drag_speed,
        4 => s.timeline_control_bindings.get_mut(0).unwrap_or(&mut s.track_control_settings),
        6 => s.timeline_control_bindings.get_mut(2).unwrap_or(&mut s.track_control_settings),
        _ => &mut s.track_control_settings,
    }
}

fn slider_command(track: bool, slot: usize) -> Option<(&'static str, &'static str)> {
    match (track, slot) {
        (false, 0) => Some(("timeline.sliderCenter", "Double Click")),
        (false, 1) => Some(("timeline.sliderExplode", "Triple Click")),
        (false, 2) => Some(("timeline.sliderAlternate", "Shift+Drag")),
        (false, 4) | (true, 4) => Some(("timeline.adjustTrackHeight", "")),
        (false, 5) => Some(("timeline.fastScroll", "")),
        (false, 6) | (true, 6) => Some(("timeline.scrollTrackRows", "Cmd+Scroll")),
        (false, 7) => Some(("timeline.zoomScroll", "Alt+Scroll")),
        (false, 8) => Some(("view.zoomIn", "=")),
        (false, 10) => Some(("view.zoomOut", "-")),
        (false, 11) => Some(("view.zoomToSequence", "\\")),
        (true, 0) => Some(("timeline.trackSliderMirror", "Double Click")),
        (true, 1) => Some(("timeline.trackSliderExplode", "Shift+Double Click")),
        (true, 2) => Some(("timeline.trackSliderAlternate", "Shift+Drag")),
        _ => None,
    }
}

fn set_slider_binding(app: &mut FilmcraftApp, track: bool, slot: usize, value: &str) {
    let Some((command, default)) = slider_command(track, slot) else { return };
    let value = if value.is_empty() { default } else { value };
    let value = match value {
        "+" => "=",
        "Shift + +" => "Shift+=",
        _ => value,
    };
    let result = if value.is_empty() || value == "Unbound" {
        app.session.shortcuts.clear(command, None, None).map(|_| ())
    } else if filmcraft_engine::shortcuts::Chord::parse(value).is_err() {
        app.session.shortcuts.set_gesture(command, value)
    } else {
        app.session.shortcuts.set(command, value, None, false, true).map(|_| ())
    };
    if let Err(error) = result {
        app.ui.status = error;
    }
    refresh_slider_binding(app, track, slot);
}

fn reset_slider_binding(app: &mut FilmcraftApp, track: bool, slot: usize) {
    let Some((command, _)) = slider_command(track, slot) else { return };
    if let Err(error) = app.session.shortcuts.reset_command_to_default(command) {
        app.ui.status = error;
    }
    refresh_slider_binding(app, track, slot);
}

fn refresh_slider_binding(app: &mut FilmcraftApp, track: bool, slot: usize) {
    let Some((id, _)) = slider_command(track, slot) else { return };
    let value = app
        .session
        .shortcuts
        .primary(id)
        .map(|binding| {
            filmcraft_engine::shortcuts::Chord::parse(&binding)
                .map(|chord| chord.display(filmcraft_engine::shortcuts::Platform::current()).replace('+', " + "))
                .unwrap_or(binding)
        })
        .unwrap_or_else(|| "Unbound".into());
    if track {
        *track_control_binding(app, slot) = value;
    } else {
        *control_binding(app, slot) = value;
    }
}

/// Refresh the Timeline's gesture cache from the shared keyboard shortcut set.
pub fn sync_slider_bindings(app: &mut FilmcraftApp) {
    if app.session.shortcuts.primary("timeline.zoomScroll").is_some_and(|s| s == "Cmd+Shift+Scroll" || s == "Ctrl+Shift+Scroll")
        && let Err(error) = app.session.shortcuts.set("timeline.zoomScroll", "Alt+Scroll", None, false, true)
    {
        app.ui.status = error;
    }
    for (track, slot) in [
        (false, 0),
        (false, 1),
        (false, 2),
        (false, 4),
        (false, 5),
        (false, 6),
        (false, 7),
        (false, 8),
        (false, 10),
        (false, 11),
        (true, 0),
        (true, 1),
        (true, 2),
    ] {
        refresh_slider_binding(app, track, slot);
    }
}
/// Which kind of menu (zoom bar = false, track slider = true) the listening Controls box
/// belongs to, so [`capture_controls`] binds into that slider's own fields.
fn controls_listen_track_key() -> egui::Id {
    egui::Id::unique("tl-controls-listening-track")
}

fn controls_listen_document_epoch_key() -> egui::Id {
    egui::Id::unique("tl-controls-listening-document-epoch")
}

/// A track-slider Controls binding that was just completed fires its action, so the user sees
/// the new input work straight away (the Timeline then consumes it). Slots 0/1/3 trigger; slot
/// 2 (Alternate Drag Speed) is a chord for drags, not an action, and triggers nothing here.
fn track_trigger_key() -> egui::Id {
    egui::Id::unique("tl-controls-track-trigger")
}

fn zoom_trigger_key() -> egui::Id {
    egui::Id::unique("tl-controls-zoom-trigger")
}

/// Route assigned slider keys before the application's general shortcut matcher consumes them.
pub fn route_slider_shortcut(app: &FilmcraftApp, ctx: &egui::Context) {
    clear_inactive_alternate_drag_capture(app, ctx);
    if reconcile_controls_capture(app, ctx) {
        return;
    }
    if controls_listening(ctx) || ctx.text_edit_focused() || app.dialog == Some(crate::Dialog::Shortcuts) {
        return;
    }
    let combo_key = egui::Id::unique("timeline-controls-held-modifiers");
    let mods = ctx.input(|i| i.modifiers);
    let count = u8::from(mods.shift) + u8::from(mods.alt) + u8::from(mods.ctrl || mods.mac_cmd);
    let key_text = ctx.input(|i| chord_and_button(&i.events, i.modifiers));
    let combo = if key_text.is_some() {
        ctx.data_mut(|d| d.remove_temp::<String>(combo_key));
        None
    } else if count >= 2 {
        let mut parts = Vec::new();
        if mods.ctrl || mods.mac_cmd {
            parts.push("Ctrl");
        }
        if mods.shift {
            parts.push("Shift");
        }
        if mods.alt {
            parts.push("Alt");
        }
        let text = parts.join(" + ");
        let prior = ctx.data(|d| d.get_temp::<String>(combo_key));
        ctx.data_mut(|d| d.insert_temp(combo_key, text.clone()));
        (prior.as_deref() != Some(text.as_str())).then_some(text)
    } else if ctx.input(|i| i.events.iter().any(|e| matches!(e, egui::Event::ModifiersChanged(_) | egui::Event::Key { pressed: false, .. }))) {
        ctx.data_mut(|d| d.remove_temp::<String>(combo_key))
    } else {
        None
    };
    let Some(text) = key_text.or(combo) else { return };
    let mut events = ctx.input(|i| i.events.clone());
    route_slider_text(app, ctx, &mut events, &text, ctx.pointer_hover_pos(), true);
    ctx.input_mut(|i| i.events = events);
}

/// Route a floating viewport's forwarded chord after application shortcuts had first refusal.
/// There is no Timeline pointer in that viewport, so availability is based on the owning slider
/// and active sequence rather than its hover rectangle.
pub fn route_forwarded_slider_shortcut(app: &FilmcraftApp, ctx: &egui::Context, events: &mut Vec<egui::Event>, modifiers: egui::Modifiers) {
    if reconcile_controls_capture(app, ctx) || controls_listening(ctx) || ctx.text_edit_focused() || app.dialog == Some(crate::Dialog::Shortcuts) {
        return;
    }
    let Some(text) = chord_and_button(events, modifiers) else { return };
    route_slider_text(app, ctx, events, &text, None, false);
}

fn route_slider_text(app: &FilmcraftApp, ctx: &egui::Context, events: &mut Vec<egui::Event>, text: &str, pointer: Option<Pos2>, require_pointer: bool) {
    if text.ends_with("Click") || text.ends_with("Scroll") {
        return;
    }
    let s = &app.session.state.timeline_ui;
    let has_sequence = app.session.state.active_sequence.is_some();
    let over_zoom = !require_pointer
        || pointer.is_some_and(|p| ctx.data(|d| d.get_temp::<Rect>(egui::Id::unique("timeline-zoom-slider-rect"))).is_some_and(|r| r.contains(p)));
    let over_tracks = !require_pointer
        || pointer.is_some_and(|p| {
            ["timeline-track-slider-video-rect", "timeline-track-slider-audio-rect"]
                .into_iter()
                .any(|id| ctx.data(|d| d.get_temp::<Rect>(egui::Id::unique(id))).is_some_and(|r| r.contains(p)))
        });
    let zoom_available = has_sequence && !s.zoom_bar_hidden && over_zoom;
    let track_available = has_sequence && !s.track_slider_hidden && over_tracks;
    let zoom = [&s.control_center_on_playhead, &s.control_explode_slider, &s.control_alternate_drag_speed, &s.control_settings];
    let track = [&s.track_control_mirror, &s.track_control_explode, &s.track_control_alternate_drag_speed, &s.track_control_settings];
    let zoom_match = if zoom_available {
        zoom.iter().enumerate().find(|(i, b)| *i != 2 && !b.is_empty() && b.as_str() == text).map(|(i, _)| (zoom_trigger_key(), i))
    } else {
        None
    };
    let extra_match = s
        .timeline_control_bindings
        .iter()
        .enumerate()
        .find(|(i, b)| {
            let owner_available = match *i {
                0 | 2 => zoom_available || track_available,
                _ => zoom_available,
            };
            owner_available && !b.is_empty() && b.as_str() == text
        })
        .map(|(i, _)| (zoom_trigger_key(), i + 4));
    let track_match = if track_available {
        track.iter().enumerate().find(|(i, b)| *i != 2 && !b.is_empty() && b.as_str() == text).map(|(i, _)| (track_trigger_key(), i))
    } else {
        None
    };
    // Alternate Drag Speed is a held chord, not a shortcut action. Swallow its key only while
    // the owning slider is actually being dragged; elsewhere the application shortcut wins.
    let alternate_drag_active =
        (matches!(app.tl.drag, Some(Drag::ZoomBar { .. })) && !s.control_alternate_drag_speed.is_empty() && s.control_alternate_drag_speed == text)
            || (app.tl.track_slider_dragging && !s.track_control_alternate_drag_speed.is_empty() && s.track_control_alternate_drag_speed == text);
    if alternate_drag_active {
        ctx.data_mut(|d| d.insert_temp(alternate_drag_capture_key(), text.to_string()));
        consume_slider_shortcut_events(events);
        ctx.input_mut(|i| i.events = events.to_vec());
        return;
    }
    let matched = zoom_match.or(extra_match).or(track_match);
    if let Some((id, slot)) = matched {
        if id == zoom_trigger_key() && slot >= 4 {
            ctx.data_mut(|d| d.insert_temp(egui::Id::unique("tl-controls-zoom-extra-action"), slot));
        } else {
            ctx.data_mut(|d| d.insert_temp(id, slot));
        }
        if let Some(delta) = events.iter().find_map(|e| match e {
            egui::Event::MouseWheel { delta, .. } => Some(delta.y + delta.x),
            _ => None,
        }) {
            ctx.data_mut(|d| d.insert_temp(egui::Id::unique("timeline-controls-wheel-delta"), delta));
        }
        consume_slider_shortcut_events(events);
        ctx.input_mut(|i| i.events = events.to_vec());
    }
}

fn consume_slider_shortcut_events(events: &mut Vec<egui::Event>) {
    events.retain(|e| {
        !matches!(
            e,
            egui::Event::Key { pressed: true, .. }
                | egui::Event::Text(_)
                | egui::Event::MouseWheel { .. }
                | egui::Event::Paste(_)
                | egui::Event::PasteImage(_)
                | egui::Event::Copy
        )
    });
}

fn alternate_drag_capture_key() -> egui::Id {
    egui::Id::unique("timeline-active-alternate-drag-chord")
}

fn alternate_drag_binding_captured(ctx: &egui::Context, binding: &str) -> bool {
    ctx.data(|d| d.get_temp::<String>(alternate_drag_capture_key()).as_deref() == Some(binding))
}

fn clear_inactive_alternate_drag_capture(app: &FilmcraftApp, ctx: &egui::Context) {
    let capture = ctx.data(|d| d.get_temp::<String>(alternate_drag_capture_key()));
    let Some(binding) = capture else { return };
    let s = &app.session.state.timeline_ui;
    let still_owned = (matches!(app.tl.drag, Some(Drag::ZoomBar { .. })) && binding == s.control_alternate_drag_speed)
        || (app.tl.track_slider_dragging && binding == s.track_control_alternate_drag_speed);
    let key_name = binding.split(" + ").last().unwrap_or_default();
    let released = egui::Key::from_name(key_name).is_some_and(|key| {
        ctx.input(|i| i.events.iter().any(|event| matches!(event, egui::Event::Key { key: released, pressed: false, .. } if *released == key)))
    });
    if !still_owned || released {
        ctx.data_mut(|d| d.remove_temp::<String>(alternate_drag_capture_key()));
    }
}

/// Stop waiting for a Controls binding when its Timeline context is no longer available.
fn clear_controls_capture(ctx: &egui::Context) {
    ctx.data_mut(|d| {
        d.insert_temp(controls_listen_key(), None::<usize>);
        d.insert_temp(controls_listen_track_key(), false);
        d.remove_temp::<u64>(controls_listen_document_epoch_key());
    });
}

/// Drop an in-progress slider Controls capture as soon as the Timeline can no longer accept it.
/// Called before the app's general keyboard-focus early returns as well as by the slider router.
pub(crate) fn reconcile_controls_capture(app: &FilmcraftApp, ctx: &egui::Context) -> bool {
    if app.modal_open(ctx) || app.ui.focused != crate::dock::PanelKind::Timeline || app.session.state.active_sequence.is_none() {
        clear_controls_capture(ctx);
        return true;
    }
    if controls_listening(ctx) {
        let capture_epoch = ctx.data(|d| d.get_temp::<u64>(controls_listen_document_epoch_key()));
        if capture_epoch != Some(app.session.document_epoch) || !controls_popup_open(app, ctx) {
            clear_controls_capture(ctx);
        }
    }
    false
}

fn controls_popup_open(app: &FilmcraftApp, ctx: &egui::Context) -> bool {
    let track = ctx.data(|d| d.get_temp::<bool>(controls_listen_track_key()).unwrap_or(false));
    if track {
        app.tl.track_slider_settings_pos.is_some()
    } else {
        app.tl.zoombar_settings_pos.is_some() || ctx.data(|d| d.get_temp::<bool>(zoombar_popup_open_key())).unwrap_or(false)
    }
}

#[cfg(test)]
mod slider_clipboard_shortcut_tests {
    use super::chord_and_button;
    use egui::{Event, Modifiers};

    #[test]
    fn zoom_and_track_slider_reset_restore_their_declared_defaults() {
        let mut app = crate::FilmcraftApp::new(filmcraft_engine::Session::default());
        app.session.shortcuts.set("timeline.sliderCenter", "Ctrl+K", None, false, true).expect("assign zoom-slider key");
        app.session.shortcuts.set("timeline.sliderExplode", "Ctrl+J", None, false, true).expect("assign zoom-slider explode key");
        app.session.shortcuts.set("timeline.trackSliderMirror", "Ctrl+L", None, false, true).expect("assign track-slider key");
        app.session.state.timeline_ui.control_center_on_playhead = "Ctrl + K".into();
        app.session.state.timeline_ui.control_explode_slider = "Ctrl + J".into();

        super::reset_slider_binding(&mut app, true, 0);
        assert_eq!(app.session.shortcuts.primary("timeline.trackSliderMirror").as_deref(), Some("Double Click"));
        assert_eq!(app.session.shortcuts.primary("timeline.sliderCenter").as_deref(), Some("Ctrl+K"));
        assert_eq!(app.session.shortcuts.primary("timeline.sliderExplode").as_deref(), Some("Ctrl+J"));
        assert_eq!(app.session.state.timeline_ui.control_center_on_playhead, "Ctrl + K");
        assert_eq!(app.session.state.timeline_ui.control_explode_slider, "Ctrl + J");
        super::reset_slider_binding(&mut app, false, 0);
        assert_eq!(app.session.shortcuts.primary("timeline.sliderCenter").as_deref(), Some("Double Click"));
        assert_eq!(app.session.shortcuts.primary("timeline.sliderExplode").as_deref(), Some("Ctrl+J"));
        assert_eq!(app.session.state.timeline_ui.control_center_on_playhead, "Double Click");
        assert_eq!(app.session.state.timeline_ui.control_explode_slider, "Ctrl + J");
        assert_eq!(app.session.state.timeline_ui.track_control_mirror, "Double Click");
    }

    #[test]
    fn controls_capture_persists_pointer_gestures_and_multi_modifier_chords() {
        let mut app = crate::FilmcraftApp::new(filmcraft_engine::Session::default());
        super::set_slider_binding(&mut app, false, 0, "Double Click");
        assert_eq!(app.session.shortcuts.primary("timeline.sliderCenter").as_deref(), Some("Double Click"));
        assert_eq!(app.session.state.timeline_ui.control_center_on_playhead, "Double Click");
        super::set_slider_binding(&mut app, false, 1, "Triple Click");
        assert_eq!(app.session.shortcuts.primary("timeline.sliderExplode").as_deref(), Some("Triple Click"));
        assert_eq!(app.session.state.timeline_ui.control_explode_slider, "Triple Click");

        super::set_slider_binding(&mut app, false, 0, "Ctrl + Shift + Alt + K");
        let chord = app.session.shortcuts.primary("timeline.sliderCenter").expect("the key chord was recorded");
        let parsed = filmcraft_engine::shortcuts::Chord::parse(&chord).expect("the multi-modifier chord is valid");
        assert_eq!(parsed.key, "K");
        assert!(parsed.mods.shift && parsed.mods.alt && (parsed.mods.ctrl || parsed.mods.cmd));
    }

    #[test]
    fn modifier_only_controls_chord_is_saved_on_release_and_fires_when_pressed() {
        use egui::{Event, Key, Modifiers, RawInput};

        let mut session = filmcraft_engine::Session::default();
        session.execute("file.openDemoProject", serde_json::json!({})).unwrap();
        let mut app = crate::FilmcraftApp::new(session);
        app.ui.focused = crate::dock::PanelKind::Timeline;
        app.tl.zoombar_settings_pos = Some(egui::pos2(10.0, 10.0));
        let ctx = egui::Context::default();
        ctx.data_mut(|d| {
            d.insert_temp(super::controls_listen_key(), Some(0usize));
            d.insert_temp(super::controls_listen_track_key(), false);
            d.insert_temp(super::controls_listen_document_epoch_key(), app.session.document_epoch);
        });
        let ctrl = Modifiers { ctrl: true, ..Default::default() };
        let ctrl_shift = Modifiers { ctrl: true, shift: true, ..Default::default() };
        let all = Modifiers { ctrl: true, shift: true, alt: true, ..Default::default() };
        let press = RawInput {
            time: Some(1.0),
            events: vec![
                Event::ModifiersChanged(all),
                Event::Key { key: Key::ControlLeft, physical_key: Some(Key::ControlLeft), pressed: true, repeat: false, modifiers: ctrl },
                Event::Key { key: Key::ShiftLeft, physical_key: Some(Key::ShiftLeft), pressed: true, repeat: false, modifiers: ctrl_shift },
                Event::Key { key: Key::AltLeft, physical_key: Some(Key::AltLeft), pressed: true, repeat: false, modifiers: all },
            ],
            ..Default::default()
        };
        let mut output = ctx.run_ui(press, |ui| super::capture_controls(&mut app, ui.ctx()));
        output.textures_delta.clear();
        let release = RawInput {
            time: Some(1.1),
            events: vec![
                Event::ModifiersChanged(Modifiers { ctrl: true, ..Default::default() }),
                Event::Key { key: Key::AltLeft, physical_key: Some(Key::AltLeft), pressed: false, repeat: false, modifiers: ctrl },
                Event::Key { key: Key::ShiftLeft, physical_key: Some(Key::ShiftLeft), pressed: false, repeat: false, modifiers: ctrl },
            ],
            ..Default::default()
        };
        let mut output = ctx.run_ui(release, |ui| super::capture_controls(&mut app, ui.ctx()));
        output.textures_delta.clear();
        assert_eq!(app.session.shortcuts.primary("timeline.sliderCenter").as_deref(), Some("Ctrl + Shift + Alt"));

        ctx.data_mut(|d| {
            d.insert_temp(egui::Id::unique("timeline-zoom-slider-rect"), egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(100.0, 100.0)));
        });
        let input = RawInput { time: Some(1.2), events: vec![Event::PointerMoved(egui::pos2(50.0, 50.0)), Event::ModifiersChanged(all)], ..Default::default() };
        let mut output = ctx.run_ui(input, |ui| super::route_slider_shortcut(&app, ui.ctx()));
        output.textures_delta.clear();
        assert_eq!(ctx.data(|d| d.get_temp::<usize>(super::zoom_trigger_key())), Some(0));
    }

    #[test]
    fn click_gesture_conflicts_include_existing_defaults_and_fast_scroll() {
        let mut app = crate::FilmcraftApp::new(filmcraft_engine::Session::default());
        super::set_slider_binding(&mut app, false, 1, "Double Click");
        assert!(super::binding_conflicts(&app, false, 1, "Double Click"), "Explode must warn on Center's default Double Click");

        super::reset_slider_binding(&mut app, false, 1);
        super::set_slider_binding(&mut app, false, 5, "Triple Click");
        assert_eq!(app.session.shortcuts.primary("timeline.fastScroll").as_deref(), Some("Triple Click"));
        assert!(super::binding_conflicts(&app, false, 5, "Triple Click"), "Fast Scroll must warn on Explode's default Triple Click");
    }

    #[test]
    fn paste_event_matches_a_custom_ctrl_v_slider_binding() {
        assert_eq!(chord_and_button(&[Event::Paste("text".into())], Modifiers::COMMAND), Some("Ctrl + V".into()));
        assert_eq!(
            chord_and_button(&[Event::PasteImage(std::sync::Arc::new(egui::ColorImage::filled([1, 1], egui::Color32::WHITE)))], Modifiers::COMMAND),
            Some("Ctrl + V".into())
        );
        assert_eq!(chord_and_button(&[Event::Paste("text".into())], Modifiers::NONE), None);
        assert_eq!(chord_and_button(&[Event::Copy], Modifiers::COMMAND), Some("Ctrl + C".into()));
        assert_eq!(chord_and_button(&[Event::Copy], Modifiers::NONE), None);
        assert_eq!(
            chord_and_button(
                &[
                    Event::PointerButton { pos: egui::pos2(5.0, 5.0), button: egui::PointerButton::Primary, pressed: true, modifiers: Modifiers::NONE },
                    Event::Copy
                ],
                Modifiers::COMMAND,
            ),
            Some("Ctrl + C".into())
        );
    }

    #[test]
    fn copy_binding_is_routed_and_removed_before_the_general_copy_handler() {
        use crate::{FilmcraftApp, dock::PanelKind};
        use filmcraft_engine::Session;
        let mut session = Session::default();
        session.execute("file.openDemoProject", serde_json::json!({})).unwrap();
        session.state.timeline_ui.control_center_on_playhead = "Ctrl + C".into();
        let mut app = FilmcraftApp::new(session);
        app.ui.focused = PanelKind::Timeline;
        let ctx = egui::Context::default();
        ctx.data_mut(|d| {
            d.insert_temp(egui::Id::unique("timeline-zoom-slider-rect"), egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(100.0, 100.0)))
        });
        let input = egui::RawInput {
            events: vec![
                Event::PointerMoved(egui::pos2(50.0, 50.0)),
                Event::Key {
                    key: egui::Key::C,
                    physical_key: Some(egui::Key::C),
                    pressed: true,
                    repeat: false,
                    modifiers: Modifiers { ctrl: true, ..Modifiers::default() },
                },
                Event::Copy,
            ],
            ..Default::default()
        };
        let mut output = ctx.run_ui(input, |ui| super::route_slider_shortcut(&app, ui.ctx()));
        output.textures_delta.clear();
        assert_eq!(ctx.data(|d| d.get_temp::<usize>(super::zoom_trigger_key())), Some(0));
        assert!(!ctx.input(|i| i.events.iter().any(|e| matches!(e, Event::Copy))));
    }

    #[test]
    fn controls_capture_clears_when_timeline_loses_its_session_context() {
        use crate::{FilmcraftApp, dock::PanelKind};
        let app = FilmcraftApp::new(filmcraft_engine::Session::default());
        let ctx = egui::Context::default();
        ctx.data_mut(|d| {
            d.insert_temp(super::controls_listen_key(), Some(2usize));
            d.insert_temp(super::controls_listen_track_key(), true);
        });
        let mut output = ctx.run_ui(Default::default(), |ui| super::route_slider_shortcut(&app, ui.ctx()));
        output.textures_delta.clear();
        assert!(!super::controls_listening(&ctx));
        assert!(!ctx.data(|d| d.get_temp::<bool>(super::controls_listen_track_key()).unwrap_or(false)));

        let mut app = FilmcraftApp::new(filmcraft_engine::Session::default());
        app.ui.focused = PanelKind::Timeline;
        ctx.data_mut(|d| d.insert_temp(super::controls_listen_key(), Some(2usize)));
        let mut output = ctx.run_ui(Default::default(), |ui| super::route_slider_shortcut(&app, ui.ctx()));
        output.textures_delta.clear();
        assert!(!super::controls_listening(&ctx));

        let mut session = filmcraft_engine::Session::default();
        session.execute("file.openDemoProject", serde_json::json!({})).unwrap();
        let mut app = FilmcraftApp::new(session);
        app.ui.focused = PanelKind::Timeline;
        ctx.data_mut(|d| {
            d.insert_temp(super::controls_listen_key(), Some(2usize));
            d.insert_temp(super::controls_listen_track_key(), false);
            d.insert_temp(super::zoombar_popup_open_key(), false);
        });
        let mut output = ctx.run_ui(Default::default(), |ui| super::route_slider_shortcut(&app, ui.ctx()));
        output.textures_delta.clear();
        assert!(!super::controls_listening(&ctx), "a closed settings popup cannot keep Controls listening");

        app.tl.zoombar_settings_pos = Some(egui::pos2(10.0, 10.0));
        ctx.data_mut(|d| {
            d.insert_temp(super::controls_listen_key(), Some(2usize));
            d.insert_temp(super::controls_listen_track_key(), false);
            d.insert_temp(super::controls_listen_document_epoch_key(), app.session.document_epoch);
            d.insert_temp(super::zoombar_popup_open_key(), true);
        });
        app.session.document_epoch = app.session.document_epoch.wrapping_add(1);
        let mut output = ctx.run_ui(Default::default(), |ui| super::route_slider_shortcut(&app, ui.ctx()));
        output.textures_delta.clear();
        assert!(!super::controls_listening(&ctx), "a replacement document cancels its prior Controls capture");
    }

    #[test]
    fn controls_capture_clears_when_track_settings_popup_disappears() {
        use crate::{FilmcraftApp, dock::PanelKind};
        let mut session = filmcraft_engine::Session::default();
        session.execute("file.openDemoProject", serde_json::json!({})).unwrap();
        let mut app = FilmcraftApp::new(session);
        app.ui.focused = PanelKind::Timeline;
        app.tl.track_slider_settings_pos = Some(egui::pos2(10.0, 10.0));
        let ctx = egui::Context::default();
        let set_capture = |ctx: &egui::Context, epoch| {
            ctx.data_mut(|d| {
                d.insert_temp(super::controls_listen_key(), Some(2usize));
                d.insert_temp(super::controls_listen_track_key(), true);
                d.insert_temp(super::controls_listen_document_epoch_key(), epoch);
            });
        };

        set_capture(&ctx, app.session.document_epoch);
        let mut output = ctx.run_ui(Default::default(), |ui| super::route_slider_shortcut(&app, ui.ctx()));
        output.textures_delta.clear();
        assert!(super::controls_listening(&ctx), "the visible Track Slider Settings popup owns the capture");

        app.tl.track_slider_settings_pos = None;
        set_capture(&ctx, app.session.document_epoch);
        let mut output = ctx.run_ui(Default::default(), |ui| super::route_slider_shortcut(&app, ui.ctx()));
        output.textures_delta.clear();
        assert!(!super::controls_listening(&ctx), "a vanished popup cannot keep the Timeline listening");
    }

    #[test]
    fn zoom_setting_change_flushes_after_popup_stops_drawing() {
        let mut app = crate::FilmcraftApp::new(filmcraft_engine::Session::default());
        let before = app.session.view_epoch;
        let ctx = egui::Context::default();
        let key = egui::Id::unique("tl-zoombar-view-pending");
        ctx.data_mut(|d| d.insert_temp(key, true));
        let mut output = ctx.run_ui(Default::default(), |ui| super::flush_pending_zoombar_view_change(&mut app, ui.ctx()));
        output.textures_delta.clear();
        assert_eq!(app.session.view_epoch, before + 1);
        assert_eq!(ctx.data(|d| d.get_temp::<bool>(key)), None);
    }

    #[test]
    fn hidden_or_missing_slider_does_not_reserve_its_shortcut() {
        use crate::{FilmcraftApp, dock::PanelKind};
        use filmcraft_engine::Session;

        for (hidden, open_sequence, expect_reserved) in [(true, true, false), (false, false, false), (false, true, true)] {
            let mut session = Session::default();
            if open_sequence {
                session.execute("file.openDemoProject", serde_json::json!({})).unwrap();
            }
            session.state.timeline_ui.track_slider_hidden = hidden;
            session.state.timeline_ui.track_control_mirror = "Ctrl + K".into();
            let mut app = FilmcraftApp::new(session);
            app.ui.focused = PanelKind::Timeline;
            let ctx = egui::Context::default();
            let modifiers = Modifiers { ctrl: true, ..Modifiers::default() };
            ctx.data_mut(|data| {
                data.insert_temp(
                    egui::Id::unique("timeline-track-slider-video-rect"),
                    egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(100.0, 100.0)),
                );
            });
            let input = egui::RawInput {
                events: vec![
                    Event::PointerMoved(egui::pos2(50.0, 50.0)),
                    Event::Key { key: egui::Key::K, physical_key: Some(egui::Key::K), pressed: true, repeat: false, modifiers },
                ],
                ..Default::default()
            };
            let mut output = ctx.run_ui(input, |ui| super::route_slider_shortcut(&app, ui.ctx()));
            output.textures_delta.clear();
            assert_eq!(ctx.data(|data| data.get_temp::<usize>(super::track_trigger_key()).is_some()), expect_reserved);
        }
    }

    #[test]
    fn zoom_owned_extra_binding_is_not_reserved_when_only_the_track_slider_is_available() {
        use crate::{FilmcraftApp, dock::PanelKind};
        use filmcraft_engine::Session;
        let mut session = Session::default();
        session.execute("file.openDemoProject", serde_json::json!({})).unwrap();
        session.state.timeline_ui.zoom_bar_hidden = true;
        session.state.timeline_ui.track_slider_hidden = false;
        assert!(session.state.timeline_ui.timeline_control_bindings.len() > 4);
        session.state.timeline_ui.timeline_control_bindings[4] = "Ctrl + M".into();
        let mut app = FilmcraftApp::new(session);
        app.ui.focused = PanelKind::Timeline;
        let ctx = egui::Context::default();
        ctx.data_mut(|d| {
            d.insert_temp(egui::Id::unique("timeline-track-slider-video-rect"), egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(100.0, 100.0)))
        });
        let input = egui::RawInput {
            events: vec![
                Event::PointerMoved(egui::pos2(50.0, 50.0)),
                Event::Key {
                    key: egui::Key::M,
                    physical_key: Some(egui::Key::M),
                    pressed: true,
                    repeat: false,
                    modifiers: Modifiers { ctrl: true, ..Modifiers::default() },
                },
            ],
            ..Default::default()
        };
        let mut output = ctx.run_ui(input, |ui| super::route_slider_shortcut(&app, ui.ctx()));
        output.textures_delta.clear();
        assert_eq!(ctx.data(|d| d.get_temp::<usize>(egui::Id::unique("tl-controls-zoom-extra-action"))), None);
        assert!(ctx.input(|i| i.events.iter().any(|e| matches!(e, Event::Key { key: egui::Key::M, pressed: true, .. }))));
    }

    #[test]
    fn custom_slider_binding_does_not_suppress_app_shortcuts_outside_slider_regions() {
        use crate::{FilmcraftApp, dock::PanelKind};
        use filmcraft_engine::Session;
        let mut session = Session::default();
        session.execute("file.openDemoProject", serde_json::json!({})).unwrap();
        session.state.timeline_ui.control_center_on_playhead = "Ctrl + C".into();
        let mut app = FilmcraftApp::new(session);
        app.ui.focused = PanelKind::Timeline;
        let ctx = egui::Context::default();
        ctx.data_mut(|d| {
            d.insert_temp(egui::Id::unique("timeline-zoom-slider-rect"), egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(100.0, 100.0)))
        });
        let input = egui::RawInput {
            events: vec![
                Event::PointerMoved(egui::pos2(200.0, 200.0)),
                Event::Key {
                    key: egui::Key::C,
                    physical_key: Some(egui::Key::C),
                    pressed: true,
                    repeat: false,
                    modifiers: Modifiers { ctrl: true, ..Modifiers::default() },
                },
            ],
            ..Default::default()
        };
        let mut output = ctx.run_ui(input, |ui| super::route_slider_shortcut(&app, ui.ctx()));
        output.textures_delta.clear();
        assert_eq!(ctx.data(|d| d.get_temp::<usize>(super::zoom_trigger_key())), None);
        assert!(ctx.input(|i| i.events.iter().any(|e| matches!(e, Event::Key { key: egui::Key::C, pressed: true, .. }))));
    }

    #[test]
    fn custom_slider_binding_does_not_reserve_a_shortcut_without_a_pointer() {
        use crate::{FilmcraftApp, dock::PanelKind};
        use filmcraft_engine::Session;
        let mut session = Session::default();
        session.execute("file.openDemoProject", serde_json::json!({})).unwrap();
        session.state.timeline_ui.control_center_on_playhead = "Ctrl + C".into();
        let mut app = FilmcraftApp::new(session);
        app.ui.focused = PanelKind::Timeline;
        let ctx = egui::Context::default();
        ctx.data_mut(|d| {
            d.insert_temp(egui::Id::unique("timeline-zoom-slider-rect"), egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(100.0, 100.0)))
        });
        let input = egui::RawInput {
            events: vec![Event::Key {
                key: egui::Key::C,
                physical_key: Some(egui::Key::C),
                pressed: true,
                repeat: false,
                modifiers: Modifiers { ctrl: true, ..Modifiers::default() },
            }],
            ..Default::default()
        };
        let mut output = ctx.run_ui(input, |ui| super::route_slider_shortcut(&app, ui.ctx()));
        output.textures_delta.clear();
        assert_eq!(ctx.data(|d| d.get_temp::<usize>(super::zoom_trigger_key())), None);
        assert!(ctx.input(|i| i.events.iter().any(|e| matches!(e, Event::Key { key: egui::Key::C, pressed: true, .. }))));
    }

    #[test]
    fn alternate_drag_binding_is_reserved_only_during_its_own_drag() {
        use crate::{FilmcraftApp, dock::PanelKind};
        use filmcraft_engine::Session;
        let mut session = Session::default();
        session.execute("file.openDemoProject", serde_json::json!({})).unwrap();
        session.state.timeline_ui.track_control_alternate_drag_speed = "Ctrl + S".into();
        let mut app = FilmcraftApp::new(session);
        app.ui.focused = PanelKind::Timeline;
        let input = || egui::RawInput {
            events: vec![Event::Key {
                key: egui::Key::S,
                physical_key: Some(egui::Key::S),
                pressed: true,
                repeat: false,
                modifiers: Modifiers { ctrl: true, ..Modifiers::default() },
            }],
            ..Default::default()
        };

        let outside_drag = egui::Context::default();
        let mut output = outside_drag.run_ui(input(), |ui| super::route_slider_shortcut(&app, ui.ctx()));
        output.textures_delta.clear();
        assert!(outside_drag.input(|i| i.events.iter().any(|e| matches!(e, Event::Key { key: egui::Key::S, pressed: true, .. }))));

        app.tl.track_slider_dragging = true;
        let during_drag = egui::Context::default();
        let mut alternate_is_down = false;
        let mut output = during_drag.run_ui(input(), |ui| {
            super::route_slider_shortcut(&app, ui.ctx());
            alternate_is_down = super::alternate_down(ui.ctx(), "Ctrl + S");
        });
        output.textures_delta.clear();
        assert!(during_drag.input(|i| !i.events.iter().any(|e| matches!(e, Event::Key { key: egui::Key::S, pressed: true, .. }))));
        assert!(alternate_is_down, "the drag still sees its alternate-speed chord");
        assert_eq!(during_drag.data(|d| d.get_temp::<usize>(super::track_trigger_key())), None);
    }

    #[test]
    fn track_slider_binding_does_not_reserve_shortcut_over_the_zoom_slider() {
        use crate::{FilmcraftApp, dock::PanelKind};
        use filmcraft_engine::Session;
        let mut session = Session::default();
        session.execute("file.openDemoProject", serde_json::json!({})).unwrap();
        session.state.timeline_ui.track_control_mirror = "Ctrl + K".into();
        let mut app = FilmcraftApp::new(session);
        app.ui.focused = PanelKind::Timeline;
        let ctx = egui::Context::default();
        app.session.state.timeline_ui.track_slider_hidden = true;
        ctx.data_mut(|d| {
            d.insert_temp(egui::Id::unique("timeline-zoom-slider-rect"), egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(100.0, 100.0)))
        });
        let input = egui::RawInput {
            events: vec![
                Event::PointerMoved(egui::pos2(50.0, 50.0)),
                Event::Key {
                    key: egui::Key::K,
                    physical_key: Some(egui::Key::K),
                    pressed: true,
                    repeat: false,
                    modifiers: Modifiers { ctrl: true, ..Modifiers::default() },
                },
            ],
            ..Default::default()
        };
        let mut output = ctx.run_ui(input, |ui| super::route_slider_shortcut(&app, ui.ctx()));
        output.textures_delta.clear();
        assert_eq!(ctx.data(|d| d.get_temp::<usize>(super::track_trigger_key())), None);
        assert!(ctx.input(|i| i.events.iter().any(|e| matches!(e, Event::Key { key: egui::Key::K, pressed: true, .. }))));
    }
}

fn gesture(response: &egui::Response, ui: &egui::Ui) -> Option<String> {
    let name = if response.triple_clicked() {
        "Triple Click"
    } else if response.double_clicked() {
        "Double Click"
    } else if response.clicked() {
        "Left Click"
    } else if response.clicked_by(egui::PointerButton::Secondary) {
        "Right Click"
    } else if response.clicked_by(egui::PointerButton::Middle) {
        "Middle Click"
    } else {
        return None;
    };
    let modifiers = ui.input(|i| i.modifiers);
    Some(format!(
        "{}{}{}{}",
        if modifiers.ctrl || modifiers.mac_cmd { "Ctrl + " } else { "" },
        if modifiers.shift { "Shift + " } else { "" },
        if modifiers.alt { "Alt + " } else { "" },
        name
    ))
}

fn uses_default_gesture(binding: &str, default: &str) -> bool {
    if binding.is_empty() {
        return true;
    }
    binding != "Unbound" && normalize_slider_gesture(binding) == normalize_slider_gesture(default)
}

fn matches_gesture(binding: &str, default: &str, gesture: Option<&str>) -> bool {
    let binding = if binding.is_empty() { default } else { binding };
    gesture.is_some_and(|gesture| binding != "Unbound" && normalize_slider_gesture(binding) == normalize_slider_gesture(gesture))
}

fn normalize_slider_gesture(text: &str) -> String {
    text.replace('⇧', "Shift")
        .replace('⌥', "Alt")
        .replace('⌘', "Ctrl")
        .chars()
        .filter(|c| !c.is_whitespace() && *c != '+')
        .flat_map(char::to_lowercase)
        .collect()
}

/// A routed key chord is already the action itself; pointer gesture modifiers apply only to the
/// pointer path and must not veto that action.
fn slider_action_triggered(key_trigger: Option<usize>, slot: usize, binding: &str, default: &str, gesture: Option<&str>) -> bool {
    key_trigger == Some(slot) || matches_gesture(binding, default, gesture)
}

fn alternate_down(ctx: &egui::Context, binding: &str) -> bool {
    alternate_drag_binding_captured(ctx, binding) || ctx.input(|i| alternate_down_input(i, binding))
}

fn alternate_down_input(input: &egui::InputState, binding: &str) -> bool {
    if binding == "Unbound" {
        return false;
    }
    if binding.is_empty() {
        return input.modifiers.shift;
    }
    if uses_default_gesture(binding, "Shift + Drag") {
        return input.modifiers.shift;
    }
    // Shortcut labels use macOS glyphs on macOS, while input handling uses words.
    // Parse the label first so ⇧Drag and Shift + Drag activate the same gesture.
    let binding = filmcraft_engine::shortcuts::Chord::parse(binding)
        .map(|chord| chord.display(filmcraft_engine::shortcuts::Platform::Windows).replace('+', " + "))
        .unwrap_or_else(|_| binding.to_string());
    let mut parts = binding.split(" + ").collect::<Vec<_>>();
    if parts.len() >= 2 && parts.iter().all(|p| matches!(*p, "Ctrl" | "Shift" | "Alt")) {
        return (!parts.contains(&"Shift") || input.modifiers.shift)
            && (!parts.contains(&"Ctrl") || input.modifiers.ctrl || input.modifiers.mac_cmd)
            && (!parts.contains(&"Alt") || input.modifiers.alt);
    }
    let Some(activation) = parts.pop() else { return false };
    let down = if let Some(key) = egui::Key::from_name(activation) {
        input.key_down(key)
    } else {
        match activation {
            "Drag" => true,
            "Left Click" => input.pointer.button_down(egui::PointerButton::Primary),
            "Right Click" => input.pointer.button_down(egui::PointerButton::Secondary),
            "Middle Click" => input.pointer.button_down(egui::PointerButton::Middle),
            _ => false,
        }
    };
    down && (!parts.contains(&"Shift") || input.modifiers.shift)
        && (!parts.contains(&"Ctrl") || input.modifiers.ctrl || input.modifiers.mac_cmd)
        && (!parts.contains(&"Alt") || input.modifiers.alt)
}

/// The width the Controls flyout always has -- the main slider's -- in every menu.
const CONTROLS_FLYOUT_W: f32 = 257.4;

/// The Shift-drag rates of the track sliders: thumb points per pointer point for the height zoom,
/// content points for the scroll. Both are fixed constants times the Alternate Drag Speed setting
/// SQUARED -- never anything the track counts or the scroll range can skew, so the two sliders
/// move 1:1 with each other.
const SHIFT_THUMB_RATE: f64 = 0.06;
const SHIFT_CONTENT_RATE: f64 = 0.2;

/// The Alternate Drag Speed setting squared: 1x is the slow rate, Custom 10x a hundred times it.
fn shift_drag_rate(speed: f32) -> f64 {
    let s = f64::from(speed.max(0.01));
    s * s
}

/// Whether the minimap's clip indicators fit on screen: hidden below a quarter of the SCREEN's
/// width (the height never matters). A pure predicate, decided fresh every frame.
fn minimap_clips_fit(bar_w: f32, screen_w: f32) -> bool {
    bar_w >= 0.25 * screen_w.max(1.0)
}

/// Keep the straight side of an edge indicator outside the full semicircle's tangent.
fn minimap_indicator_bounds(mut xa: f32, mut xb: f32, left_tangent: f32, right_tangent: f32, hug_left: bool, hug_right: bool) -> (f32, f32) {
    if hug_left {
        xb = xb.max(left_tangent);
    }
    if hug_right {
        xa = xa.min(right_tangent);
    }
    (xa, xb)
}

/// Give an end cap only to the outermost run near that edge, so adjacent runs keep their cut.
fn minimap_indicator_hugs_edges(index: usize, run_count: usize, left_gap: f32, right_gap: f32, cap_zone: f32) -> (bool, bool) {
    (index == 0 && left_gap < cap_zone, index + 1 == run_count && right_gap < cap_zone)
}

/// Fill narrow interior indicators, but keep edge indicators outlined unless Fill Clips is enabled.
fn minimap_indicator_filled(fill_clips: bool, width: f32, min_outline_width: f32, hug_left: bool, hug_right: bool) -> bool {
    fill_clips || (width < min_outline_width && !hug_left && !hug_right)
}

/// Accumulate clip boundaries so overlapping long clips cost constant work each, rather than
/// repainting every covered column for every clip.
fn minimap_add_span(boundaries: &mut [[i64; 3]], start: usize, end: usize, priority: usize) {
    let cols = boundaries.len().saturating_sub(1);
    let start = start.min(cols);
    let end = end.min(cols);
    if start >= end || !(1..=3).contains(&priority) {
        return;
    }
    boundaries[start][priority - 1] += 1;
    boundaries[end][priority - 1] -= 1;
}

fn minimap_classes(boundaries: &[[i64; 3]]) -> Vec<u8> {
    let mut active = [0i64; 3];
    boundaries
        .iter()
        .take(boundaries.len().saturating_sub(1))
        .map(|delta| {
            for (count, change) in active.iter_mut().zip(delta) {
                *count += change;
            }
            if active[2] > 0 {
                3
            } else if active[1] > 0 {
                2
            } else if active[0] > 0 {
                1
            } else {
                0
            }
        })
        .collect()
}

/// Which side a flyout fits on: to the right of the menu when there is room there, otherwise to
/// the left. Computed from geometry alone, so the row's arrow points the right way on the very
/// frame the menu opens -- never a frame of one side and then the other.
fn flyout_to_the_right(ui: &egui::Ui, _row: Rect, content_right: f32, flyout_content_w: f32) -> bool {
    let margin = ui.spacing().menu_margin;
    let popup_right = content_right + f32::from(margin.right);
    let flyout_w = flyout_content_w + 16.0 + f32::from(margin.left) + f32::from(margin.right);
    popup_right + 4.0 + flyout_w <= ui.ctx().content_rect().right()
}

/// The little arrow on a Controls / Minimap row: at the same distance from whichever wall the
/// flyout opens on -- 6 pt past the content edge on the right, the mirror of that distance on the
/// left -- pointing at the flyout. (Measured off the popup's content edge, never the row's own
/// width: the switch rows are narrower and used to sit almost against the left wall.)
fn controls_arrow(ui: &egui::Ui, row: Rect, menu_w: f32, content_right: f32, to_the_right: bool, color: Color32) {
    let x = if to_the_right { row.min.x + menu_w - 6.0 } else { content_right - menu_w + 6.0 };
    let glyph = if to_the_right { "\u{25b6}" } else { "\u{25c0}" };
    ui.painter().text(pos2(x, row.center().y), Align2::CENTER_CENTER, glyph, Tokens::ui(8.0), color);
}

/// Where a Controls box that is listening for a new input remembers itself.
fn controls_listen_key() -> egui::Id {
    egui::Id::unique("tl-controls-listening")
}

/// Whether a Controls box is listening for a new shortcut. While it is, the rest of the
/// application must not hear the key: the shortcut matcher consumes presses from the frame's
/// events (which is also why the capture used to miss them), and a bound key would fire its
/// action in the background while the user is naming a new binding.
pub fn controls_listening(ctx: &egui::Context) -> bool {
    ctx.data(|d| d.get_temp::<Option<usize>>(controls_listen_key())).flatten().is_some()
}

fn controls_capturing(ui: &egui::Ui) -> bool {
    controls_listening(ui.ctx()) || ui.data(|d| d.get_temp::<f64>(egui::Id::unique("tl-controls-consumed-frame"))) == Some(ui.input(|i| i.time))
}

/// Capture the next gesture, or unbind after three seconds of inactivity.
pub(crate) fn capture_controls(app: &mut FilmcraftApp, ctx: &egui::Context) {
    let key = controls_listen_key();
    let Some(slot) = ctx.data(|d| d.get_temp::<Option<usize>>(key)).flatten() else {
        return;
    };
    let frame_time = ctx.input(|i| i.time);
    ctx.data_mut(|d| d.insert_temp(egui::Id::unique("tl-controls-consumed-frame"), frame_time));
    let track = ctx.data(|d| d.get_temp::<bool>(controls_listen_track_key()).unwrap_or(false));
    let pending_key = egui::Id::unique("tl-controls-pending-click");
    let key_text = ctx.input(|i| {
        chord_and_button(
            &i.events
                .iter()
                .filter(|e| matches!(e, egui::Event::Key { .. } | egui::Event::Paste(_) | egui::Event::PasteImage(_) | egui::Event::Copy))
                .cloned()
                .collect::<Vec<_>>(),
            i.modifiers,
        )
    });
    let wheel_text = ctx
        .input(|i| i.events.iter().find(|e| matches!(e, egui::Event::MouseWheel { .. })).and_then(|e| chord_and_button(std::slice::from_ref(e), i.modifiers)));
    let modifier_key = egui::Id::unique("timeline-controls-modifiers");
    let modifiers = ctx.input(|i| i.modifiers);
    let count = modifier_count(modifiers);
    let prior_modifiers = ctx.data(|d| d.get_temp::<(String, egui::Modifiers)>(modifier_key));
    let modifier_text = if prior_modifiers.as_ref().is_some_and(|(_, previous)| modifier_count(*previous) > count) {
        ctx.data_mut(|d| d.remove_temp::<(String, egui::Modifiers)>(modifier_key));
        prior_modifiers.map(|(text, _)| text)
    } else if count >= 2 {
        let mut parts = Vec::new();
        if modifiers.ctrl || modifiers.mac_cmd {
            parts.push("Ctrl");
        }
        if modifiers.shift {
            parts.push("Shift");
        }
        if modifiers.alt {
            parts.push("Alt");
        }
        ctx.data_mut(|d| d.insert_temp(modifier_key, (parts.join(" + "), modifiers)));
        None
    } else if ctx.input(|i| {
        i.events.iter().any(|e| {
            matches!(
                e,
                egui::Event::ModifiersChanged(_)
                    | egui::Event::Key {
                        key: egui::Key::ShiftLeft
                            | egui::Key::ShiftRight
                            | egui::Key::AltLeft
                            | egui::Key::AltRight
                            | egui::Key::ControlLeft
                            | egui::Key::ControlRight,
                        pressed: false,
                        ..
                    }
            )
        })
    }) {
        ctx.data_mut(|d| d.remove_temp::<(String, egui::Modifiers)>(modifier_key).map(|(text, _)| text))
    } else {
        None
    };
    let clicks = ctx.input(|i| {
        i.events
            .iter()
            .filter_map(|e| match e {
                egui::Event::PointerButton { button, pressed: true, modifiers, .. } => Some((*button, *modifiers)),
                _ => None,
            })
            .collect::<Vec<_>>()
    });
    let now = ctx.input(|i| i.time);
    for (button, modifiers) in clicks {
        let prefix = format!(
            "{}{}{}",
            if modifiers.ctrl || modifiers.mac_cmd { "Ctrl + " } else { "" },
            if modifiers.shift { "Shift + " } else { "" },
            if modifiers.alt { "Alt + " } else { "" }
        );
        let name = match button {
            egui::PointerButton::Primary => "Left",
            egui::PointerButton::Secondary => "Right",
            egui::PointerButton::Middle => "Middle",
            _ => "Extra",
        };
        let prior = ctx.data(|d| d.get_temp::<(String, u8, f64)>(pending_key));
        let base = format!("{prefix}{name}");
        let count = prior.filter(|(s, _, t)| *s == base && now - *t <= 0.35).map_or(1, |(_, n, _)| n.saturating_add(1).min(3));
        ctx.data_mut(|d| d.insert_temp(pending_key, (base, count, now)));
        ctx.request_repaint_after(std::time::Duration::from_millis(360));
    }
    let pending = ctx.data(|d| d.get_temp::<(String, u8, f64)>(pending_key));
    let mouse_text = pending.filter(|(_, n, t)| *n == 3 || now - *t >= 0.35).map(|(base, count, _)| {
        if count == 1 {
            format!("{base} Click")
        } else if let Some(prefix) = base.strip_suffix("Left") {
            format!("{prefix}{} Click", if count == 2 { "Double" } else { "Triple" })
        } else {
            format!("{base} {} Click", if count == 2 { "Double" } else { "Triple" })
        }
    });
    let last_input = egui::Id::unique("tl-controls-last-input");
    if ctx.input(|i| !i.events.is_empty()) {
        ctx.data_mut(|d| d.insert_temp(last_input, now));
    }
    let idle = now - ctx.data(|d| d.get_temp::<f64>(last_input)).unwrap_or(now) >= 3.0;
    if !idle {
        ctx.request_repaint_after(std::time::Duration::from_millis(100));
    }
    if idle || ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
        if track {
            set_slider_binding(app, true, slot, "Unbound");
        } else {
            set_slider_binding(app, false, slot, "Unbound");
        }
        ctx.data_mut(|d| d.insert_temp(key, None::<usize>));
        ctx.data_mut(|d| {
            d.remove_temp::<(String, u8, f64)>(pending_key);
            d.remove_temp::<(String, egui::Modifiers)>(modifier_key);
        });
        app.session.project_view_changed();
    } else if let Some(text) = key_text.or(mouse_text).or(wheel_text).or(modifier_text) {
        if (text.contains("Right ") && text.ends_with("Click")) || text == "Left Click" {
            ctx.data_mut(|d| {
                d.insert_temp(egui::Id::unique("timeline-right-click-dialog"), true);
                d.insert_temp(egui::Id::unique("timeline-left-click-rejected"), text == "Left Click");
                d.insert_temp(key, None::<usize>);
                d.remove_temp::<(String, u8, f64)>(pending_key);
                d.remove_temp::<(String, egui::Modifiers)>(modifier_key);
            });
            ctx.request_repaint();
            return;
        }
        if track {
            set_slider_binding(app, true, slot, &text);
        } else {
            set_slider_binding(app, false, slot, &text);
        }
        ctx.data_mut(|d| {
            d.remove_temp::<(String, u8, f64)>(pending_key);
            d.remove_temp::<(String, egui::Modifiers)>(modifier_key);
        });
        app.session.project_view_changed();
        ctx.data_mut(|d| d.insert_temp(key, None::<usize>));
    }
    // While a Controls box is naming a shortcut NOTHING else in the application may hear an
    // input: the shortcut matcher is gated on `controls_listening` upstream, and this takes
    // key, text and mouse-button events off the frame's input so no panel's own Escape/Enter
    // handler -- and no later pass of the same frame -- can act on them either.
    ctx.input_mut(|i| {
        i.events.retain(|e| !matches!(e, egui::Event::Key { .. } | egui::Event::Text(_) | egui::Event::PointerButton { .. } | egui::Event::MouseWheel { .. }))
    });
}

/// The hover flyout beside a settings row (the Minimap Settings' sibling): the very same shape as
/// the minimap's own flyout -- placed beside the menu (to the right when the screen has room for
/// it there, otherwise to the left), bottom-aligned with the row's bottom edge, always fully
/// opaque -- and the very same keep-alive: plain geometry against the raw pointer position (the
/// row, the ring of gaps around the menu, the flyout itself, the settings' own popups) plus the
/// colour pickers' 3 s grace. egui's `rect_contains_pointer` is layer-bound and reports false for
/// the gap strips and for the flyout's own Area, which is exactly why a flyout used to vanish the
/// moment the pointer left its row. Only `trigger` (a real hover of the row, after the pointer has
/// moved) OPENS it; everything else merely keeps it open. No settings in it yet.
fn binding_conflicts(app: &FilmcraftApp, track: bool, slot: usize, shown: &str) -> bool {
    !conflicting_commands(app, track, slot, shown).is_empty()
}

fn conflicting_commands(app: &FilmcraftApp, track: bool, slot: usize, shown: &str) -> Vec<String> {
    let Some((own_command, _)) = slider_command(track, slot) else { return Vec::new() };
    if shown.is_empty() || shown == "Unbound" {
        return Vec::new();
    }
    let chord = filmcraft_engine::shortcuts::Chord::parse(shown).ok();
    let platform = filmcraft_engine::shortcuts::Platform::current();
    let mut commands = app
        .session
        .shortcuts
        .bindings
        .iter()
        .filter_map(|binding| {
            if binding.panel.as_deref().is_some_and(|p| p != "Timeline") {
                return None;
            }
            if own_command == binding.command {
                return None;
            }
            if (shown.ends_with("Click") || shown.ends_with("Drag"))
                && ((!track && binding.command.starts_with("timeline.trackSlider")) || (track && binding.command.starts_with("timeline.slider")))
            {
                return None;
            }
            let matches = if let Some(chord) = &chord {
                filmcraft_engine::shortcuts::Chord::parse(&binding.keys).ok().is_some_and(|other| other.effective(platform) == chord.effective(platform))
            } else {
                filmcraft_engine::shortcuts::Chord::parse(&binding.keys).is_err() && normalize_slider_gesture(shown) == normalize_slider_gesture(&binding.keys)
            };
            matches.then(|| binding.command.clone())
        })
        .collect::<Vec<_>>();
    commands.sort();
    commands.dedup();
    if commands.is_empty() {
        Vec::new()
    } else {
        commands.push(own_command.to_string());
        commands.sort();
        commands.dedup();
        commands
    }
}

// The flyout rows share their capture and popup state in one drawing pass.
#[allow(clippy::too_many_arguments, clippy::cognitive_complexity)]
fn hover_flyout(
    app: &mut FilmcraftApp,
    ui: &mut egui::Ui,
    flyout_id: egui::Id,
    row: Rect,
    content_right: f32,
    content_w: f32,
    open_now: bool,
    track: bool,
    auto_id: &str,
    auto_label: &str,
    items: &[(&str, &str, usize)],
) -> bool {
    let listening_width = ui.ctx().data(|d| d.get_temp::<Option<usize>>(controls_listen_key())).flatten();
    let mods = ui.input(|i| i.modifiers);
    let mut live_parts = Vec::new();
    if mods.ctrl || mods.mac_cmd {
        live_parts.push("Ctrl");
    }
    if mods.shift {
        live_parts.push("Shift");
    }
    if mods.alt {
        live_parts.push("Alt");
    }
    let live = if live_parts.is_empty() { String::new() } else { live_parts.join(" + ") };
    let content_w = items.iter().fold(content_w, |width, &(name, default, slot)| {
        let bound = if track { track_control_binding(app, slot).clone() } else { control_binding(app, slot).clone() };
        let shown = if listening_width == Some(slot) {
            if live.is_empty() { tl!("Enter Shortcut") } else { live.as_str() }
        } else if bound.is_empty() {
            if default == "Unbound" { tl!("Unbound") } else { default }
        } else if bound == "Unbound" {
            tl!("Unbound")
        } else {
            bound.as_str()
        };
        let label = ui.painter().layout_no_wrap(name.to_owned(), Tokens::ui(11.5), Color32::WHITE).size().x;
        let binding = ui.painter().layout_no_wrap(shown.to_owned(), Tokens::ui(11.5), Color32::WHITE).size().x.max(52.0) + 14.0;
        let second = if !track && slot == 1 {
            let other = control_binding(app, 11).clone();
            let text = if listening_width == Some(11) {
                if live.is_empty() { tl!("Enter Shortcut") } else { live.as_str() }
            } else if other.is_empty() {
                "\\"
            } else if other == "Unbound" {
                tl!("Unbound")
            } else {
                other.as_str()
            };
            12.0 + (ui.painter().layout_no_wrap(text.to_owned(), Tokens::ui(11.0), Color32::WHITE).size().x + 14.0).max(36.0)
        } else {
            0.0
        };
        let sequence_binding = if !track && slot == 1 { control_binding(app, 11).clone() } else { String::new() };
        let warning_space = if !track && slot == 1 && binding_conflicts(app, false, 11, &sequence_binding) { 80.0 } else { 48.0 };
        width.max(label + binding + second + warning_space)
    });
    let rect_key = flyout_id.with("rect");
    let height_key = flyout_id.with("height");
    let stamp_key = flyout_id.with("stamp");
    let margin = ui.spacing().menu_margin;
    let popup_right = content_right + f32::from(margin.right);
    let popup_left = row.min.x - f32::from(margin.left);
    let flyout_w = content_w + 16.0 + f32::from(margin.left) + f32::from(margin.right);
    let screen = ui.ctx().content_rect();
    let gap = 4.0;
    let to_the_right = flyout_to_the_right(ui, row, content_right, content_w);
    let (x, to_the_right) = if to_the_right { (popup_right + gap, true) } else { ((popup_left - gap - flyout_w).max(screen.left()), false) };
    let popup_bottom = row.max.y + f32::from(margin.bottom);
    // the height is remembered from the last opening; a FRESH opening must not travel on the
    // first frame, so the guess is what the popup frame draws: one 28 pt row per item plus the
    // frame's top/bottom margins (the measured height replaces the guess afterwards)
    let frame = egui::Frame::popup(ui.style());
    let row_count = items.len() + usize::from(!track && app.session.state.timeline_ui.control_tracks);
    let flyout_h = row_count as f32 * 28.0
        + row_count.saturating_sub(1) as f32 * ui.spacing().item_spacing.y
        + f32::from(frame.inner_margin.top)
        + f32::from(frame.inner_margin.bottom)
        + f32::from(frame.outer_margin.top)
        + f32::from(frame.outer_margin.bottom)
        + 2.0 * frame.stroke.width;
    // the flyout centers on the setting row (its own middle at the row's middle), never past the
    // screen's edges
    let y = (row.center().y - flyout_h / 2.0).clamp(screen.top(), (screen.bottom() - flyout_h).max(screen.top()));
    let flyout_rect = Rect::from_min_size(pos2(x, y), egui::vec2(flyout_w, flyout_h));
    // the keep-alive frame around the menu: the pointer may travel anywhere on the menu or in a
    // 24 pt ring of gaps around it (and across the gap strip to the flyout) without losing it
    let menu = Rect::from_min_max(pos2(popup_left, row.min.y - 30.0 - f32::from(margin.top)), pos2(popup_right, popup_bottom));
    let (gx0, gx1) = if to_the_right { (popup_right, x) } else { (x + flyout_w, popup_left) };
    let side = Rect::from_min_max(pos2(gx0.min(gx1), y), pos2(gx0.max(gx1), popup_bottom.max(y + flyout_h)));
    let under = Rect::from_min_max(pos2(popup_left, popup_bottom), pos2(popup_right, popup_bottom + 24.0));
    let around = Rect::from_min_max(pos2(menu.min.x - 24.0, menu.min.y - 24.0), pos2(menu.max.x + 24.0, menu.max.y + 24.0));
    let pointer = ui.input(|i| i.pointer.hover_pos().or(i.pointer.interact_pos()));
    let over = |r: Rect| pointer.is_some_and(|p| r.contains(p));
    let prev_flyout = ui.data(|d| d.get_temp::<Rect>(rect_key)).unwrap_or(flyout_rect);
    let now = ui.input(|i| i.time);
    // working in one of the settings' own popups (or the three seconds after it) keeps the flyout
    // alive no matter where the pointer goes
    let keep = ui.data(|d| d.get_temp::<f64>(grace_key())).unwrap_or(-1e9);
    let row_hot = over(row) && open_now;
    let keep_alive = row_hot
        || over(menu)
        || over(side)
        || over(under)
        || over(around)
        || over(prev_flyout)
        || over(flyout_rect)
        || pointer.is_some_and(|p| family_contains_prev(ui, p))
        || now - keep < 3.0;
    // the ROW opens it -- and only on a real hover of it: the menu appears under the cursor, often
    // right on this row, so the pointer has to move first
    let opened_at = ui.data(|d| d.get_temp::<Pos2>(open_pointer_key())).unwrap_or(pos2(-1e9, -1e9));
    let moved = pointer.is_some_and(|p| (p - opened_at).length() > 6.0);
    let trigger = row_hot && moved;
    let was_open = ui.data(|d| d.get_temp::<bool>(flyout_id.with("open"))).unwrap_or(false);
    let stamp = ui.data(|d| d.get_temp::<f64>(stamp_key)).unwrap_or(-1e9);
    let open = open_now && (trigger || (was_open && (keep_alive || now - stamp < 0.3)));
    if open {
        ui.data_mut(|d| d.insert_temp(stamp_key, now));
    }
    ui.data_mut(|d| d.insert_temp(flyout_id.with("open"), open));
    if open {
        let panel = egui::Area::new(flyout_id).order(egui::Order::Foreground).fixed_pos(pos2(x, y)).fade_in(false).interactable(true).show(ui.ctx(), |ui| {
            egui::Frame::popup(ui.style()).show(ui, |ui| {
                ui.set_min_width(content_w + 16.0);
                ui.set_max_width(content_w + 16.0);
                // the control list: an action and the input that does it (the default gesture in
                // the dark box until the user rebinds it). Clicking a box listens for the next
                // input -- a key chord or a mouse button -- which becomes the new way to do it;
                // the circular arrow at the row's end restores its binding(s). (The listening
                // itself happens in [`capture_controls`], from the menus, every frame.)
                let listening = ui.ctx().data(|d| d.get_temp::<Option<usize>>(controls_listen_key())).flatten();
                for (idx, &(name, default, slot)) in items.iter().enumerate() {
                    // the row spans the flyout's content width, exactly as the Minimap colour
                    // rows do, so the reset arrow lands at their distance from the right wall
                    let row_w = ui.available_width().max(170.0);
                    let (irow, _) = ui.allocate_exact_size(egui::vec2(row_w, 28.0), Sense::hover());
                    let n_galley = ui.painter().layout_no_wrap((*name).to_string(), Tokens::ui(11.5), app.tokens.text);
                    let bound = if track { track_control_binding(app, slot).clone() } else { control_binding(app, slot).clone() };
                    let shown = if listening == Some(slot) {
                        if live.is_empty() { tl!("Enter Shortcut").to_string() } else { live.clone() }
                    } else if bound.is_empty() {
                        if default == "Unbound" { tl!("Unbound").to_string() } else { default.to_string() }
                    } else if bound == "Unbound" {
                        tl!("Unbound").to_string()
                    } else {
                        bound.clone()
                    };
                    let a_galley = ui.painter().layout_no_wrap(shown.clone(), Tokens::ui(11.5), egui::Color32::WHITE);
                    let gap = 6.0;
                    let box_w = a_galley.size().x.max(52.0) + 14.0;
                    let second = !track && slot == 1;
                    let other = if second { control_binding(app, 11).clone() } else { String::new() };
                    let other_text = if listening == Some(11) {
                        if live.is_empty() { tl!("Enter Shortcut") } else { live.as_str() }
                    } else if other.is_empty() {
                        "\\"
                    } else if other == "Unbound" {
                        tl!("Unbound")
                    } else {
                        other.as_str()
                    };
                    let other_w = (ui.painter().layout_no_wrap(other_text.to_owned(), Tokens::ui(11.0), Color32::WHITE).size().x + 14.0).max(36.0);
                    let group_w = n_galley.size().x + gap + box_w + if second { 12.0 + other_w } else { 0.0 };
                    let ix0 = irow.center().x - group_w / 2.0;
                    let ibox = Rect::from_min_size(pos2(ix0 + n_galley.size().x + gap, irow.center().y - 10.0), egui::vec2(box_w, 20.0));
                    let bresp = ui.interact(ibox, flyout_id.with(("box", idx)), Sense::click());
                    if bresp.hovered() {
                        ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
                    }
                    if bresp.clicked() {
                        // the new binding must work the moment it is captured: clicking into the
                        // popup's flyout is work on the Timeline's own sliders
                        app.ui.focused = crate::dock::PanelKind::Timeline;
                        let started = ui.input(|i| i.time);
                        ui.ctx().data_mut(|d| {
                            d.insert_temp(egui::Id::unique("tl-controls-last-input"), started);
                            d.remove_temp::<(String, egui::Modifiers)>(egui::Id::unique("timeline-controls-modifiers"));
                            d.insert_temp(controls_listen_key(), Some(slot));
                            d.insert_temp(controls_listen_track_key(), track);
                            d.insert_temp(controls_listen_document_epoch_key(), app.session.document_epoch);
                            d.remove_temp::<(String, u8, f64)>(egui::Id::unique("tl-controls-pending-click"));
                        });
                    }
                    app.auto.add(&format!("{auto_id}.box{idx}"), ibox, name);
                    ui.painter().rect_filled(ibox, 3.0, egui::Color32::from_gray(16));
                    ui.painter().rect_stroke(ibox, 3.0, egui::Stroke::new(1.0, egui::Color32::from_gray(64)), egui::StrokeKind::Inside);
                    ui.painter().text(ibox.center(), Align2::CENTER_CENTER, &shown, Tokens::ui(11.0), egui::Color32::WHITE);
                    if second {
                        let r = Rect::from_min_size(pos2(ibox.max.x + 12.0, ibox.center().y - 10.0), vec2(other_w, 20.0));
                        let response = ui.interact(r, flyout_id.with("sequence-box"), Sense::click());
                        if response.hovered() {
                            ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
                        }
                        if response.clicked() {
                            app.ui.focused = crate::dock::PanelKind::Timeline;
                            let started = ui.input(|i| i.time);
                            ui.ctx().data_mut(|d| {
                                d.insert_temp(egui::Id::unique("tl-controls-last-input"), started);
                                d.remove_temp::<(String, egui::Modifiers)>(egui::Id::unique("timeline-controls-modifiers"));
                                d.insert_temp(controls_listen_key(), Some(11usize));
                                d.insert_temp(controls_listen_track_key(), false);
                                d.insert_temp(controls_listen_document_epoch_key(), app.session.document_epoch);
                                d.remove_temp::<(String, u8, f64)>(egui::Id::unique("tl-controls-pending-click"));
                            });
                        }
                        app.auto.add(&format!("{auto_id}.sequenceBox"), r, "Zoom to Sequence");
                        ui.painter().rect_filled(r, 3.0, egui::Color32::from_gray(16));
                        ui.painter().rect_stroke(r, 3.0, egui::Stroke::new(1.0, egui::Color32::from_gray(64)), egui::StrokeKind::Inside);
                        ui.painter().text(r.center(), Align2::CENTER_CENTER, other_text, Tokens::ui(11.0), Color32::WHITE);
                    }
                    for (warn_slot, warn_text, warn_rect, warn_id) in
                        std::iter::once((slot, shown.as_str(), ibox, format!("conflict{idx}"))).chain(second.then_some((
                            11,
                            other_text,
                            Rect::from_min_size(pos2(ibox.max.x + 12.0, ibox.min.y), vec2(other_w, 20.0)),
                            "sequenceConflict".to_owned(),
                        )))
                    {
                        let conflicts = conflicting_commands(app, track, warn_slot, warn_text);
                        if listening != Some(warn_slot) && !conflicts.is_empty() {
                            let center = warn_rect.right() + if second && warn_slot == slot { 6.0 } else { 8.0 };
                            let warning =
                                Rect::from_center_size(pos2(center, warn_rect.center().y), vec2(if second && warn_slot == slot { 10.0 } else { 12.0 }, 16.0));
                            ui.painter().text(warning.center(), Align2::CENTER_CENTER, "!", Tokens::ui(12.0), Color32::from_rgb(255, 175, 80));
                            let response = ui
                                .interact(warning, flyout_id.with((&warn_id, idx)), Sense::click())
                                .on_hover_text(tl!("This shortcut is already assigned. Open Keyboard Shortcuts to review it."));
                            if response.hovered() {
                                ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
                            }
                            if response.clicked() {
                                let query = warn_text.to_owned();
                                ui.ctx().data_mut(|d| d.insert_temp(controls_listen_key(), None::<usize>));
                                if track {
                                    app.tl.track_slider_settings_pos = None;
                                    track_slider_state_reset(ui.ctx());
                                } else {
                                    app.tl.zoombar_settings_pos = None;
                                    close_zoombar_popup(ui.ctx());
                                }
                                crate::panels::shortcuts_dialog::open_conflict_review(app, &query, &conflicts);
                            }
                            app.auto.add(&format!("{auto_id}.{warn_id}"), warning, "This shortcut is already assigned.");
                        }
                    }
                    ui.painter().galley(pos2(ix0, irow.center().y - n_galley.size().y / 2.0), n_galley, app.tokens.text);
                    // the circular reset arrow, the Effect Controls / Minimap colour rows' one
                    let rr = Rect::from_center_size(pos2(irow.max.x - 14.0, irow.center().y), vec2(16.0, 16.0));
                    let rresp = ui.interact(rr, flyout_id.with(("reset", idx)), Sense::click()).on_hover_text(tl!("Reset"));
                    app.auto.add(&format!("{auto_id}.reset{idx}"), rr, tl!("Reset"));
                    icons::paint(ui.painter(), rr.shrink(2.0), Icon::Reset, if rresp.hovered() { app.tokens.text } else { app.tokens.text_dim });
                    if rresp.clicked() {
                        if track {
                            reset_slider_binding(app, true, slot);
                        } else {
                            reset_slider_binding(app, false, slot);
                            if second {
                                reset_slider_binding(app, false, 11);
                            }
                        }
                        app.session.project_view_changed();
                    }
                }
                if !track && app.session.state.timeline_ui.control_tracks {
                    let mut left = app.session.state.timeline_ui.control_tracks_angle_left;
                    let mut right = app.session.state.timeline_ui.control_tracks_angle_right;
                    let (row, _) = ui.allocate_exact_size(vec2(ui.available_width(), 28.0), Sense::hover());
                    let label = ui.painter().layout_no_wrap(tl!("Track Control Angle:").to_owned(), Tokens::ui(11.5), app.tokens.text);
                    let group_width = label.size().x + 6.0 + 34.0 + 6.0 + 14.0 + 6.0 + 34.0;
                    let x = row.center().x - group_width / 2.0;
                    ui.painter().galley(pos2(x, row.center().y - label.size().y / 2.0), label.clone(), app.tokens.text);
                    let left_rect = Rect::from_min_size(pos2(x + label.size().x + 6.0, row.center().y - 10.0), vec2(34.0, 20.0));
                    let right_rect = left_rect.translate(vec2(60.0, 0.0));
                    ui.painter().text(pos2(left_rect.right() + 13.0, row.center().y), Align2::CENTER_CENTER, tl!("to"), Tokens::ui(11.5), app.tokens.text);
                    let (l, r) = ui
                        .scope(|ui| {
                            ui.visuals_mut().widgets.inactive.bg_fill = Color32::from_gray(16);
                            let l = ui.put(left_rect, egui::DragValue::new(&mut left).range(30.0..=89.0).speed(1.0).max_decimals(0).suffix("\u{00b0}"));
                            let r = ui.put(right_rect, egui::DragValue::new(&mut right).range(30.0..=89.0).speed(1.0).max_decimals(0).suffix("\u{00b0}"));
                            (l, r)
                        })
                        .inner;
                    app.auto.add("timeline.zoomBar.settings.controlsFlyout.angleLeft", l.rect, "Left angle");
                    app.auto.add("timeline.zoomBar.settings.controlsFlyout.angleRight", r.rect, "Right angle");
                    let reset = Rect::from_center_size(pos2(row.max.x - 14.0, row.center().y), vec2(16.0, 16.0));
                    let reset_response = ui.interact(reset, flyout_id.with("angle-reset"), Sense::click()).on_hover_text(tl!("Reset"));
                    icons::paint(ui.painter(), reset.shrink(2.0), Icon::Reset, if reset_response.hovered() { app.tokens.text } else { app.tokens.text_dim });
                    app.auto.add("timeline.zoomBar.settings.controlsFlyout.angleReset", reset, "Reset angle");
                    if reset_response.clicked() {
                        left = 71.565_05;
                        right = 71.565_05;
                    }
                    if l.changed() || r.changed() || reset_response.clicked() {
                        app.session.state.timeline_ui.control_tracks_angle_left = left.clamp(30.0, 89.0);
                        app.session.state.timeline_ui.control_tracks_angle_right = right.clamp(30.0, 89.0);
                        app.session.project_view_changed();
                    }
                }
            });
        });
        app.auto.add(auto_id, panel.response.rect, auto_label);
        family_push(ui, panel.response.rect);
        ui.data_mut(|d| d.insert_temp(rect_key, panel.response.rect));
        ui.data_mut(|d| d.insert_temp(height_key, panel.response.rect.height()));
    }
    to_the_right
}

/// The zoom-bar settings' own little popups (the Minimap Settings flyout, the colour pickers):
/// their rects, collected fresh every frame while [`zoombar_settings`] draws, so a "click outside"
/// check can tell a click on one of them from a click away from the settings.
fn family_reset(ui: &egui::Ui) {
    // last frame's rects become "prev": the flyout's keep-alive must recognise this frame's
    // children (a colour picker under it) before they are drawn again
    let cur = ui.data_mut(|d| d.get_temp::<Vec<Rect>>(egui::Id::unique("tl-zoombar-family")).unwrap_or_default());
    ui.data_mut(|d| {
        d.insert_temp(egui::Id::unique("tl-zoombar-family-prev"), cur);
        d.insert_temp(egui::Id::unique("tl-zoombar-family"), Vec::<Rect>::new());
    });
}

fn family_push(ui: &egui::Ui, rect: Rect) {
    ui.data_mut(|d| {
        let mut v = d.get_temp::<Vec<Rect>>(egui::Id::unique("tl-zoombar-family")).unwrap_or_default();
        v.push(rect);
        d.insert_temp(egui::Id::unique("tl-zoombar-family"), v);
    });
}

fn family_contains(ui: &egui::Ui, pos: Pos2) -> bool {
    ui.data(|d| d.get_temp::<Vec<Rect>>(egui::Id::unique("tl-zoombar-family")).unwrap_or_default()).iter().any(|r| r.contains(pos))
}

fn family_contains_prev(ui: &egui::Ui, pos: Pos2) -> bool {
    ui.data(|d| d.get_temp::<Vec<Rect>>(egui::Id::unique("tl-zoombar-family-prev")).unwrap_or_default()).iter().any(|r| r.contains(pos))
}

/// Shared one-per-popup state of the settings' little popups: which colour picker is open, where
/// it sits, and the time its keep-alive grace runs from.
fn color_open_key() -> egui::Id {
    egui::Id::unique("tl-zoombar-color-open")
}

fn color_rect_key() -> egui::Id {
    egui::Id::unique("tl-zoombar-color-rect")
}

fn grace_key() -> egui::Id {
    egui::Id::unique("tl-zoombar-grace")
}

/// Where the pointer was when the settings menu opened: the flyout only opens on a real hover of
/// its row, never just because the menu appeared under the cursor on that row.
fn open_pointer_key() -> egui::Id {
    egui::Id::unique("tl-zoombar-open-pointer")
}

/// Forget every transient of the settings popups (the Minimap flyout, the colour pickers and
/// their keep-alives): the next time the menu opens it is in its default state, never with a
/// picker or flyout left over from the last time.
pub(super) fn zoombar_state_reset(ctx: &egui::Context) {
    let pointer = ctx.input(|i| i.pointer.hover_pos()).unwrap_or(pos2(-1e9, -1e9));
    let generation_key = settings_generation_key("timeline-zoombar");
    ctx.data_mut(|d| {
        d.insert_temp(color_open_key(), None::<egui::Id>);
        d.insert_temp(controls_listen_key(), None::<usize>);
        d.insert_temp(controls_listen_track_key(), false);
        d.insert_temp(color_rect_key(), Rect::NOTHING);
        d.insert_temp(grace_key(), -1e9f64);
        d.insert_temp(open_pointer_key(), pointer);
        d.insert_temp(egui::Id::unique("tl-zoombar-family"), Vec::<Rect>::new());
        d.insert_temp(egui::Id::unique("tl-zoombar-family-prev"), Vec::<Rect>::new());
        // a fresh opening: the settings menu must start scrolled to its bottom again
        let generation = d.get_temp::<u64>(generation_key).unwrap_or(0).wrapping_add(1);
        d.insert_temp(generation_key, generation);
        d.insert_temp(egui::Id::unique("timeline-zoombar-minimap-flyout").with("rect"), Rect::NOTHING);
        d.insert_temp(egui::Id::unique("timeline-zoombar-minimap-flyout").with("stamp"), -1e9f64);
        d.insert_temp(egui::Id::unique("timeline-zoombar-minimap-flyout").with("height"), MINIMAP_FLYOUT_HEIGHT);
        d.remove_temp::<Pos2>(egui::Id::unique("timeline-zoombar-minimap-flyout").with("opening-pos"));
        d.insert_temp(egui::Id::unique("timeline-zoombar-minimap-flyout").with("open"), false);
        for id in ["timeline-zoombar-advanced-flyout", "timeline-track-slider-advanced-flyout"] {
            d.insert_temp(egui::Id::unique(id).with("rect"), Rect::NOTHING);
            d.insert_temp(egui::Id::unique(id).with("stamp"), -1e9f64);
            d.insert_temp(egui::Id::unique(id).with("height"), 110.0f32);
            d.insert_temp(egui::Id::unique(id).with("open"), false);
        }
    });
}

pub(super) fn track_slider_state_reset(ctx: &egui::Context) {
    let generation_key = settings_generation_key("timeline-track-slider");
    ctx.data_mut(|d| {
        d.insert_temp(controls_listen_key(), None::<usize>);
        // a fresh opening: the settings menu must start scrolled to its bottom again
        let generation = d.get_temp::<u64>(generation_key).unwrap_or(0).wrapping_add(1);
        d.insert_temp(generation_key, generation);
    });
}

/// The generation counter of one slider settings menu: every opening bumps it, and the menu's
/// first frame after the bump scrolls to the bottom (when the menu has to scroll at all).
fn settings_generation_key(menu: &str) -> egui::Id {
    egui::Id::NULL.with(("timeline-settings-generation", menu))
}

/// The generation the menu last opened at.
fn settings_seen_key(menu: &str) -> egui::Id {
    egui::Id::NULL.with(("timeline-settings-seen", menu))
}

/// The measured scroll offset that takes a freshly opened settings menu to its bottom.
fn settings_scroll_key(menu: &str) -> egui::Id {
    egui::Id::NULL.with(("timeline-settings-scroll-to-bottom", menu))
}

/// Parse "#RRGGBB" or "#RGB" into a colour; anything else gives the fallback (hostile project and
/// control-channel text included).
fn parse_hex_color(s: &str, fallback: Color32) -> Color32 {
    fn parse(s: &str) -> Option<Color32> {
        let t = s.trim().strip_prefix('#').unwrap_or(s.trim()).as_bytes();
        let digit = |c: u8| char::from(c).to_digit(16);
        let mut v = [0u8; 3];
        if t.len() == 6 {
            for (i, slot) in v.iter_mut().enumerate() {
                *slot = ((digit(t[i * 2])? * 16 + digit(t[i * 2 + 1])?) & 0xff) as u8;
            }
        } else if t.len() == 3 {
            for (i, slot) in v.iter_mut().enumerate() {
                let d = digit(t[i])?;
                *slot = ((d * 17) & 0xff) as u8;
            }
        } else {
            return None;
        }
        Some(Color32::from_rgb(v[0], v[1], v[2]))
    }
    parse(s).unwrap_or(fallback)
}

/// A colour as "#rrggbb", the shape the setting stores.
fn hex_color(c: Color32) -> String {
    format!("#{:02x}{:02x}{:02x}", c.r(), c.g(), c.b())
}

fn normalize_hex_edit(ctx: &egui::Context, edit_id: egui::Id, buf: &mut String) -> String {
    let added_prefix = !buf.starts_with('#');
    let digits: String = buf.chars().filter(|c| c.is_ascii_hexdigit()).take(6).collect();
    *buf = format!("#{digits}");
    if added_prefix
        && let Some(mut state) = egui::text_edit::TextEditState::load(ctx, edit_id)
        && let Some(range) = state.cursor.char_range()
    {
        use egui::text::{CCursor, CCursorRange};
        let shift = |cursor: CCursor| CCursor::new(usize::from(cursor.index).saturating_add(1).min(digits.chars().count() + 1));
        state.cursor.set_char_range(Some(CCursorRange::two(shift(range.primary), shift(range.secondary))));
        state.store(ctx, edit_id);
    }
    digits
}

/// HSV (hue 0-360, saturation and value 0-1) to a colour, for the picker's wheel.
fn hsv(h: f32, s: f32, v: f32) -> Color32 {
    let h = (((h % 360.0) + 360.0) % 360.0) / 60.0;
    let c = v * s;
    let x = c * (1.0 - ((h % 2.0) - 1.0).abs());
    let m = v - c;
    let (r, g, b) = match h as u32 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    Color32::from_rgb(((r + m) * 255.0) as u8, ((g + m) * 255.0) as u8, ((b + m) * 255.0) as u8)
}

/// A colour back to hue / saturation, for the picker's wheel (the wheel has no brightness).
fn color_to_hsv(c: Color32) -> (f32, f32) {
    let r = f32::from(c.r()) / 255.0;
    let g = f32::from(c.g()) / 255.0;
    let b = f32::from(c.b()) / 255.0;
    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    let d = max - min;
    let h = if d <= 0.0 {
        0.0
    } else if max == r {
        60.0 * (((g - b) / d) % 6.0)
    } else if max == g {
        60.0 * ((b - r) / d + 2.0)
    } else {
        60.0 * ((r - g) / d + 4.0)
    };
    (if h < 0.0 { h + 360.0 } else { h }, if max <= 0.0 { 0.0 } else { d / max })
}

/// Any hex digits to the closest "#rrggbb": three digits expand the CSS way, longer ones are cut
/// to six, shorter ones are filled out to six. Lowercase, always exactly six digits -- and never
/// panics, not even on no digits at all (clearing the hex field once crashed here).
fn closest_six(digits: &str) -> String {
    let d: Vec<char> = digits.chars().collect();
    let six: String = if d.is_empty() {
        "000000".to_string()
    } else if d.len() == 3 {
        d.iter().flat_map(|c| [*c, *c]).collect()
    } else if d.len() > 6 {
        d[..6].iter().collect()
    } else {
        (0..6).map(|i| d[i % d.len()]).collect()
    };
    format!("#{six}").to_lowercase()
}

/// A colour setting row: the label, and a dark swatch (a lighter outline) showing the colour's hex
/// code in white. Clicking the swatch (a pointing hand) opens the colour picker -- a hue /
/// saturation wheel, a brightness slider and a hex field, everything live. ONE call draws the row
/// and runs its picker; copy that one line to add another colour setting anywhere.
fn color_setting(ui: &mut egui::Ui, t: &Tokens, id: egui::Id, label: &str, color: &mut String, default: &str) -> bool {
    let mut changed = false;
    let row_w = ui.available_width().max(170.0);
    let (row, _) = ui.allocate_exact_size(egui::vec2(row_w, 30.0), Sense::hover());
    let base = parse_hex_color(color, Color32::from_rgb(0x58, 0x95, 0xec));
    let galley = ui.painter().layout_no_wrap(label.to_string(), Tokens::ui(11.5), t.text);
    // the row is a centered group like the pill rows: label, a small gap, the hex box
    let box_w = 72.0;
    let gap = 6.0;
    let group_w = galley.size().x + gap + box_w;
    let x0 = row.center().x - group_w / 2.0;
    let swatch = Rect::from_min_size(pos2(x0 + galley.size().x + gap, row.center().y - 11.0), egui::vec2(box_w, 22.0));
    let resp = ui.interact(swatch, id, Sense::click());
    if resp.hovered() {
        ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
    }
    let p = ui.painter();
    p.rect_filled(swatch, 3.0, Color32::from_gray(16));
    p.rect_stroke(swatch, 3.0, Stroke::new(1.0, Color32::from_gray(64)), StrokeKind::Inside);
    p.text(swatch.center(), Align2::CENTER_CENTER, color.as_str(), Tokens::ui(12.5), Color32::WHITE);
    ui.painter().galley(pos2(x0, row.center().y - galley.size().y / 2.0), galley, t.text);
    // the reset button at the row's right end, the same as Effect Controls' parameter reset
    let rr = Rect::from_center_size(pos2(row.max.x - 14.0, row.center().y), vec2(16.0, 16.0));
    let rresp = ui.interact(rr, id.with("reset"), Sense::click()).on_hover_text(tl!("Reset"));
    icons::paint(ui.painter(), rr.shrink(2.0), Icon::Reset, if rresp.hovered() { t.text } else { t.text_dim });
    if rresp.clicked() {
        *color = default.to_string();
        changed = true;
    }
    // one colour picker open at a time, remembered under a shared key so opening the menu afresh
    // can clear it
    let open_now = ui.data(|d| d.get_temp::<Option<egui::Id>>(color_open_key())).flatten() == Some(id);
    if resp.clicked() {
        ui.data_mut(|d| d.insert_temp(color_open_key(), if open_now { None } else { Some(id) }));
    }
    if open_now {
        let mut hex_focus = false;
        let panel = egui::Area::new(id.with("wheel"))
            .order(egui::Order::Foreground)
            .fixed_pos(pos2(swatch.min.x - 30.0, swatch.max.y + 4.0))
            .fade_in(false)
            .interactable(true)
            .show(ui.ctx(), |ui| {
                egui::Frame::popup(ui.style()).show(ui, |ui| {
                    // the wheel: hue around the ring, saturation towards the edge. One seamless
                    // mesh, so it is just colours -- no seams between the parts.
                    let (slot, resp) = ui.allocate_exact_size(egui::vec2(132.0, 132.0), Sense::click_and_drag());
                    let c = slot.center();
                    let outer = 62.0;
                    let mut mesh = egui::Mesh::default();
                    const ANGLES: usize = 48;
                    const RINGS: usize = 3;
                    for ring in 0..=RINGS {
                        for seg in 0..=ANGLES {
                            let a = seg as f32 / ANGLES as f32 * std::f32::consts::TAU;
                            let r = outer * ring as f32 / RINGS as f32;
                            let col = hsv(seg as f32 / ANGLES as f32 * 360.0, ring as f32 / RINGS as f32, 1.0);
                            mesh.colored_vertex(c + vec2(a.cos(), a.sin()) * r, col);
                        }
                    }
                    for ring in 0..RINGS {
                        for seg in 0..ANGLES {
                            let i = (ring * (ANGLES + 1) + seg) as u32;
                            let n = ANGLES as u32 + 1;
                            mesh.add_triangle(i, i + 1, i + n);
                            mesh.add_triangle(i + 1, i + n + 1, i + n);
                        }
                    }
                    ui.painter().add(egui::Shape::mesh(mesh));
                    let (h, s) = color_to_hsv(base);
                    let at = c + vec2(h.to_radians().cos(), h.to_radians().sin()) * (s * outer);
                    ui.painter().circle_stroke(at, 4.0, Stroke::new(1.5, Color32::WHITE));
                    if (resp.clicked() || resp.dragged())
                        && let Some(pos) = resp.interact_pointer_pos()
                    {
                        let d = pos - c;
                        let nh = (d.y.atan2(d.x).to_degrees() + 360.0) % 360.0;
                        let ns = (d.length() / outer).clamp(0.0, 1.0);
                        *color = hex_color(hsv(nh, ns, 1.0));
                        changed = true;
                    }
                    // the hex field, centered under the wheel: six digits only -- longer pastes
                    // are cut to six, shorter ones filled out, both to the closest colour
                    let mut buf = ui.data(|d| d.get_temp::<String>(id.with("hex"))).unwrap_or_else(|| color.clone());
                    if changed {
                        // the wheel or Reset changed the colour outside the field: the stored
                        // text must follow it, or the field shows the last typed value again
                        buf = color.clone();
                        ui.data_mut(|d| d.insert_temp(id.with("hex"), buf.clone()));
                    }
                    // the hex field, dead center under the wheel (placed at an exact rect: layout
                    // alignment refused to centre it)
                    let (hrow, _) = ui.allocate_exact_size(egui::vec2(132.0, 26.0), Sense::hover());
                    let field = Rect::from_min_size(pos2(hrow.center().x - 46.0, hrow.center().y - 11.0), egui::vec2(92.0, 22.0));
                    let edit = ui.put(field, egui::TextEdit::singleline(&mut buf).align(egui::Align2::CENTER_CENTER).char_limit(7));
                    hex_focus = edit.has_focus();
                    if edit.changed() {
                        let digits: String = buf.chars().filter(|c| c.is_ascii_hexdigit()).take(6).collect();
                        buf = format!("#{digits}");
                        ui.data_mut(|d| d.insert_temp(id.with("hex"), buf.clone()));
                        if !digits.is_empty() {
                            *color = closest_six(&digits);
                            changed = true;
                        }
                    }
                });
            });
        family_push(ui, panel.response.rect);
        ui.data_mut(|d| d.insert_temp(color_rect_key(), panel.response.rect));
        // Working in the picker keeps every menu alive for three more seconds after it stops
        // (wheel drag, the hex field's focus, or simply pointing at the picker).
        let now = ui.input(|i| i.time);
        let pointer = ui.input(|i| i.pointer.hover_pos());
        let touching = resp.dragged() || hex_focus || pointer.is_some_and(|p| panel.response.rect.contains(p));
        if touching {
            ui.data_mut(|d| d.insert_temp(grace_key(), now));
        }
        // a click outside the picker and its swatch closes it
        if ui.input(|i| i.pointer.button_clicked(egui::PointerButton::Primary))
            && let Some(pos) = ui.input(|i| i.pointer.interact_pos())
            && !panel.response.rect.contains(pos)
            && !swatch.contains(pos)
        {
            ui.data_mut(|d| d.insert_temp(color_open_key(), None::<egui::Id>));
        }
    }
    changed
}

/// The zoom-bar settings popup opened from the Timeline's own menu ("Timeline Slider Settings",
/// shown only while the bar is hidden): the exact same settings, the same way, as right-clicking
/// the bar itself.
pub(super) fn zoombar_settings_popup(app: &mut FilmcraftApp, ui: &mut egui::Ui) {
    let Some(pos) = app.tl.zoombar_settings_pos else {
        return;
    };
    let screen = ui.ctx().content_rect();
    let popup_h = 560.0f32;
    let pos = if screen.height() < popup_h {
        pos2(pos.x, (screen.bottom() - screen.height().min(popup_h)).max(screen.top()))
    } else if pos.y + popup_h > screen.bottom() {
        pos2(pos.x, (screen.bottom() - popup_h).max(screen.top()))
    } else {
        pos
    };
    let panel = egui::Area::new(egui::Id::unique("tl-zoombar-settings-from-menu"))
        .order(egui::Order::Foreground)
        .fixed_pos(pos)
        .fade_in(false)
        .interactable(true)
        .show(ui.ctx(), |ui| {
            egui::Frame::popup(ui.style()).show(ui, |ui| {
                zoombar_settings(app, ui);
            });
        });
    if !controls_capturing(ui)
        && ui.input(|i| i.pointer.button_clicked(egui::PointerButton::Primary))
        && let Some(p) = ui.input(|i| i.pointer.interact_pos())
        && !panel.response.rect.contains(p)
        && !family_contains(ui, p)
    {
        app.tl.zoombar_settings_pos = None;
    }
}

/// The value to fall back to when Custom is picked again and no custom value was stored yet.
fn last_custom_value(stored: f64, max: f64) -> f64 {
    if stored.is_finite() { stored.clamp(0.1, max) } else { 15.0 }
}

/// A centered section heading for the settings popup (wrapped, so it stays inside a narrow menu).
fn settings_heading(ui: &mut egui::Ui, t: &Tokens, label: &str, hint: &str) {
    settings_heading_with_hint_center(ui, t, label, hint, None);
}

/// Draw a settings heading with an optional exact question-mark center.
fn settings_heading_with_hint_center(ui: &mut egui::Ui, t: &Tokens, label: &str, hint: &str, hint_center_x: Option<f32>) {
    let (r, _) = ui.allocate_exact_size(egui::vec2(ui.available_width(), 26.0), Sense::hover());
    let galley = ui.painter().layout(label.to_string(), Tokens::ui(11.5), t.text, r.width().max(40.0));
    let right = r.center().x + galley.size().x / 2.0;
    ui.painter().galley(pos2(r.center().x - galley.size().x / 2.0, r.center().y - galley.size().y / 2.0), galley, t.text);
    setting_hint_at(ui, pos2(hint_center_x.unwrap_or(right + 12.0), r.center().y), hint);
}

/// An immediate, non-interactive hint: it exists only while the question mark is hovered.
fn setting_hint(ui: &egui::Ui, row: Rect, text: &str) {
    setting_hint_at(ui, pos2(row.right() - 10.0, row.center().y), text);
}

fn setting_hint_at(ui: &egui::Ui, center: Pos2, text: &str) {
    let mark = Rect::from_center_size(center, vec2(16.0, 16.0));
    ui.painter().circle_stroke(mark.center(), 5.2, Stroke::new(0.9, Color32::from_gray(125)));
    ui.painter().text(mark.center(), Align2::CENTER_CENTER, "?", Tokens::ui(7.15), Color32::from_gray(125));
    if ui.input(|i| i.pointer.hover_pos().is_some_and(|p| mark.contains(p))) {
        let frame = egui::Frame::popup(ui.style());
        let margin = frame.total_margin();
        let screen = ui.ctx().content_rect();
        let max_text_width = (screen.width() - margin.left - margin.right - 8.0).max(40.0);
        let natural = ui.painter().layout_no_wrap(text.to_string(), egui::TextStyle::Body.resolve(ui.style()), ui.visuals().text_color());
        let width = natural.size().x.min(max_text_width);
        let galley = ui.painter().layout(text.to_string(), egui::TextStyle::Body.resolve(ui.style()), ui.visuals().text_color(), width);
        let outer_w = width + margin.left + margin.right;
        let x = (mark.center().x - outer_w / 2.0).clamp(screen.left(), (screen.right() - outer_w).max(screen.left()));
        let y = (mark.top() - galley.size().y - margin.top - margin.bottom - 4.0).max(screen.top());
        egui::Area::new(egui::Id::unique(("slider-setting-hint", text)))
            .order(egui::Order::Tooltip)
            .fixed_pos(pos2(x, y))
            .interactable(false)
            .fade_in(false)
            .show(ui.ctx(), |ui| {
                frame.show(ui, |ui| {
                    ui.set_width(width);
                    ui.label(text);
                });
            });
    }
}

fn tick_slider(ui: &mut egui::Ui, value: &mut usize, labels: &[&str], t: &Tokens) -> (egui::Response, bool) {
    let row_width = ui.available_width().max(100.0);
    let (row, response) = ui.allocate_exact_size(egui::vec2(row_width, 46.0), Sense::click_and_drag());
    // keep the widest end label inside the menu
    let margin = (row_width * 0.09).clamp(18.0, 24.0);
    let left = row.left() + margin;
    let right = row.right() - margin;
    let y = row.top() + 16.0;
    let steps = (labels.len() - 1).max(1) as f32;
    let x_at = |i: usize| left + (right - left) * i as f32 / steps;
    let before = *value;
    if (response.clicked() || response.dragged())
        && let Some(pos) = response.interact_pointer_pos()
    {
        let frac = ((pos.x - left) / (right - left).max(1.0)).clamp(0.0, 1.0);
        *value = (frac * steps).round() as usize;
    }
    let p = ui.painter();
    p.line_segment([pos2(left, y), pos2(right, y)], Stroke::new(3.5, t.separator));
    p.line_segment([pos2(left, y), pos2(x_at(*value), y)], Stroke::new(3.5, t.accent));
    for (i, label) in labels.iter().enumerate() {
        p.line_segment([pos2(x_at(i), y - 6.0), pos2(x_at(i), y + 6.0)], Stroke::new(1.0, t.icon_active));
        p.text(pos2(x_at(i), y + 9.0), Align2::CENTER_TOP, *label, Tokens::ui(10.0), t.text_dim);
    }
    p.circle_filled(pos2(x_at(*value), y), 7.0, t.accent);
    (response, before != *value)
}

/// The track slider's thumb length for a track height: the same kind of zoom meter as the zoom
/// bar's thumb -- the shortest tracks fill the bar, the tallest leave the [`MIN_THUMB_W`]
/// minimum, log-smooth in between. A bijection, so a drag can never explode or compound.
fn track_thumb(track_h: f32, bar_h: f32, min_h: f32) -> f32 {
    let max_h = track_max_height(min_h);
    let f = 1.0 - (track_h.clamp(min_h, max_h) / min_h).ln() / (max_h / min_h).ln();
    MIN_THUMB_W + f.clamp(0.0, 1.0) * (bar_h - MIN_THUMB_W).max(1.0)
}

/// Content coordinate held still when the opposite thumb edge changes height.
#[cfg(test)]
fn fixed_edge_anchor(scroll: f32, window_h: f32, track_h: f32, outer: bool, offset: f32) -> f32 {
    ((scroll + if outer { 0.0 } else { window_h }) + offset) / track_h
}

/// Scroll value that keeps a captured viewport edge at the same content coordinate.
fn scroll_for_fixed_edge(anchor: f32, track_h: f32, window_h: f32, offset: f32, outer: bool) -> f32 {
    anchor * track_h - if outer { offset } else { window_h + offset }
}

/// Keep a real rendered row boundary when the viewport edge is already on one; otherwise keep
/// the exact visible content coordinate. Quantizing a mid-row viewport edge to its nearest line
/// can push it outside the legal scroll range after a resize and trap the opposite edge.
fn capture_fixed_view_anchor(value: f32, window_h: f32, track_h: f32, outer: bool) -> Option<(f32, f32, bool)> {
    if !(value.is_finite() && window_h.is_finite() && track_h.is_finite() && track_h > 0.0) {
        return None;
    }
    let viewport_edge = if outer { value } else { value + window_h };
    let raw_anchor = viewport_edge / track_h;
    let nearest_line = raw_anchor.round();
    let line_offset = nearest_line * track_h - viewport_edge;
    let (anchor, offset) = if line_offset.abs() <= 1.25 { (nearest_line, line_offset) } else { (raw_anchor, 0.0) };
    (anchor.is_finite() && offset.is_finite()).then_some((anchor, offset, outer))
}

/// Exact track height for a snapped boundary while the opposite viewport edge is fixed.
fn height_for_fixed_edge_detent(anchor: f32, window_h: f32, offset: f32, outer: bool, detent: f32) -> Option<f32> {
    let (numerator, divisor) = if outer { (window_h - offset + 1.0, detent - anchor) } else { (window_h + offset + 1.0, anchor - detent) };
    (numerator.is_finite() && divisor.is_finite() && numerator > 0.0 && divisor > 1e-6).then_some(numerator / divisor)
}

#[derive(Clone, Copy, Default)]
struct TrackResizeState {
    mode: u8,
    fixed: f32,
    center: f32,
    free_len: f32,
    /// (row boundary index, its screen offset from that viewport edge, anchored edge orientation)
    view_anchor: Option<(f32, f32, bool)>,
    /// The detent currently latched during a resize and the raw boundary coordinate last seen.
    snap_detent: Option<f32>,
    last_snap_q: Option<f32>,
}

/// Its inverse: the track height a thumb `len` points long stands for.
fn track_height_of(len: f32, bar_h: f32, min_h: f32) -> f32 {
    let f = ((len - MIN_THUMB_W) / (bar_h - MIN_THUMB_W).max(1.0)).clamp(0.0, 1.0);
    min_h * (track_max_height(min_h) / min_h).powf(1.0 - f)
}

pub(super) fn track_min_height(window_h: f32, slots: f32) -> f32 {
    (window_h.max(0.0) / slots.max(1.0)).clamp(22.0, 10000.0)
}

fn track_max_height(min_h: f32) -> f32 {
    220.0f32.max(min_h * 2.0)
}

pub(super) fn resize_tracks_from_timeline(height: &mut f32, scroll: &mut f32, window: f32, slots: f32, bar_h: f32, dy: f32) {
    if !dy.is_finite() || bar_h <= MIN_THUMB_W {
        return;
    }
    let min_h = track_min_height(window, slots);
    let old_len = track_thumb(*height, bar_h, min_h).min(bar_h);
    let old_max = (slots * *height - window).max(0.0);
    let fraction = if old_max > 0.5 { (*scroll / old_max).clamp(0.0, 1.0) } else { 0.0 };
    // At the divider only the outer edge moves; away from it, zoom out symmetrically.
    let multiplier = if fraction > 0.001 && dy > 0.0 { 2.0 } else { 1.0 };
    let len = (old_len + dy * multiplier).clamp(MIN_THUMB_W, bar_h);
    let new_h = track_height_of(len, bar_h, min_h).max(min_h);
    let new_max = (slots * new_h - window).max(0.0);
    // As a symmetric expansion reaches the divider, the scroll reaches home and stays there.
    let travel = (bar_h - old_len).max(1.0);
    let new_fraction = if multiplier > 1.0 { (fraction * travel - dy).max(0.0) / (bar_h - len).max(1.0) } else { fraction };
    *height = new_h;
    *scroll = (new_fraction * new_max).clamp(0.0, new_max);
}

/// How far a Shift drag on the zoom bar moves the view per point of pointer motion, as a fraction
/// of an ordinary 1:1 drag. Tuned by feel -- log-interpolated between fixed points: one frame of
/// footage in view -> 0.00025 (per-pixel nudges at the tightest zoom), one second -> 0.00575,
/// 10 seconds -> 1/50, one minute -> 0.115, and fitting the whole Timeline -> 0.75. Narrow views get
/// frame-level precision, wide ones sweep quickly. One curve instead of the old
/// Auto-percent-times-multiplier stack: the Alternate Drag Speed setting still multiplies on top.
fn shift_drag_base(visible_seconds: f64, full_view_seconds: f64, frame_seconds: f64) -> f64 {
    /// multiplier of 1:1 at each fixed point (see the function docs)
    const AT_FRAME: f64 = 0.00025;
    const AT_ONE_SECOND: f64 = 0.00575;
    const AT_TEN_SECONDS: f64 = 0.02;
    const AT_MINUTE: f64 = 0.115;
    const AT_FULL_VIEW: f64 = 0.75;
    let log_lerp = |y0: f64, y1: f64, x: f64, x0: f64, x1: f64| {
        let f = if x.is_finite() && x.is_sign_positive() && x0.is_finite() && x1.is_finite() && x0 > 0.0 && x1 > x0 {
            ((x / x0).ln() / (x1 / x0).ln()).clamp(0.0, 1.0)
        } else {
            0.0
        };
        (y0.ln() + (y1.ln() - y0.ln()) * f).exp()
    };
    let frame = if frame_seconds.is_finite() { frame_seconds.clamp(0.001, 9.999) } else { 1.0 / 24.0 };
    if !(visible_seconds.is_finite() && visible_seconds > 0.0 && full_view_seconds.is_finite() && full_view_seconds > 0.0) {
        return AT_MINUTE;
    }
    if visible_seconds >= full_view_seconds {
        AT_FULL_VIEW
    } else if visible_seconds >= 60.0 {
        log_lerp(AT_MINUTE, AT_FULL_VIEW, visible_seconds, 60.0, full_view_seconds)
    } else if visible_seconds >= 10.0 {
        log_lerp(AT_TEN_SECONDS, AT_MINUTE, visible_seconds, 10.0, 60.0)
    } else if visible_seconds >= 1.0 {
        log_lerp(AT_ONE_SECOND, AT_TEN_SECONDS, visible_seconds, 1.0, 10.0)
    } else if visible_seconds >= frame {
        log_lerp(AT_FRAME, AT_ONE_SECOND, visible_seconds, frame, 1.0)
    } else {
        AT_FRAME
    }
}

/// How much faster a Shift drag on the zoom bar's EDGES runs than the tuned [`shift_drag_base`]
/// curve at that zoom level: 30x when only 0.1 s is in view, 5x at 1 s, 2x at 10 s, easing back to
/// the plain curve by a minute (so the full-view sweep stays a sweep). Log-interpolated between the
/// anchors, 30x below 0.1 s, 1.0 for hostile input. The slider's middle never uses this.
fn shift_edge_speed_multiplier(visible_seconds: f64) -> f64 {
    /// multiplier of the tuned curve at each fixed point (see the function docs)
    const AT_TENTH_SECOND: f64 = 30.0;
    const AT_ONE_SECOND: f64 = 5.0;
    const AT_TEN_SECONDS: f64 = 2.0;
    const AT_MINUTE: f64 = 1.0;
    let log_lerp = |y0: f64, y1: f64, x: f64, x0: f64, x1: f64| {
        let f = if x.is_finite() && x.is_sign_positive() && x0.is_finite() && x1.is_finite() && x0 > 0.0 && x1 > x0 {
            ((x / x0).ln() / (x1 / x0).ln()).clamp(0.0, 1.0)
        } else {
            0.0
        };
        (y0.ln() + (y1.ln() - y0.ln()) * f).exp()
    };
    if !(visible_seconds.is_finite() && visible_seconds > 0.0) {
        return 1.0;
    }
    if visible_seconds <= 0.1 {
        AT_TENTH_SECOND
    } else if visible_seconds < 1.0 {
        log_lerp(AT_TENTH_SECOND, AT_ONE_SECOND, visible_seconds, 0.1, 1.0)
    } else if visible_seconds < 10.0 {
        log_lerp(AT_ONE_SECOND, AT_TEN_SECONDS, visible_seconds, 1.0, 10.0)
    } else if visible_seconds < 60.0 {
        log_lerp(AT_TEN_SECONDS, AT_MINUTE, visible_seconds, 10.0, 60.0)
    } else {
        AT_MINUTE
    }
}

// the zoom bar's gestures are one state machine (modes, cursor pin, settings popups, minimap);
// splitting it would separate state that must stay in sync within a single frame
pub(super) fn zoom_scrollbar(app: &mut FilmcraftApp, ui: &mut egui::Ui, bar: Rect, view_w: f32, seq: &Sequence, frame_seconds: f64, t: &Tokens) {
    if app.session.state.timeline_ui.slider_minimap {
        zoom_scrollbar_with_minimap_legacy(app, ui, bar, view_w, seq, frame_seconds, t);
    } else {
        zoom_bar::zoom_scrollbar(app, ui, bar, view_w, seq, frame_seconds, t);
    }
}

// Keep the existing layered drawing path together until the dedicated minimap layer is extracted.
#[allow(clippy::cognitive_complexity)]
fn zoom_scrollbar_with_minimap_legacy(app: &mut FilmcraftApp, ui: &mut egui::Ui, bar: Rect, view_w: f32, seq: &Sequence, frame_seconds: f64, t: &Tokens) {
    let seq_seconds = seq.duration().seconds();
    let p = ui.painter();
    let resp = ui.interact(bar, egui::Id::unique("tl-zoombar"), Sense::click_and_drag());
    p.rect_filled(bar, bar.height() / 2.0, t.separator);
    let vis = view_w.max(1.0) as f64 / app.ui.timeline.pps.max(1e-9);
    let total = timeline_extent(seq_seconds).max(vis);
    let min_view = app.session.state.timeline_ui.max_zoom_secs();
    let (left, w, per_point) = zoom_thumb(bar, view_w, seq_seconds, app.ui.timeline.scroll, app.ui.timeline.pps, min_view);
    // Slider Minimap (draft): a NON-detailed map of where clips are on the whole Timeline. The
    // clips are laid out over the minimap's span and never move when the view zooms or pans -- the
    // thumb is the control, not the viewport indicator. Clips that overlap in time (a clip with
    // one on top of it, in any track) read as ONE rectangle; every rectangle has a 1 pt gap to the
    // next, so each cut shows as a vertical line across the whole minimap. Show Whole Timeline (on
    // by default) locks the span to the full Timeline extent (the sequence plus its ten minute
    // tail), so the "you are here" line can travel past the last clip; off, the map stops at the
    // end of the sequence.
    let minimap_span = if app.session.state.timeline_ui.show_whole_timeline {
        timeline_extent(seq_seconds).max(1e-9)
    } else if seq_seconds.is_finite() && seq_seconds > 0.0 {
        seq_seconds
    } else {
        1e-9
    };
    let minimap_x = |t: f64| -> f32 {
        let t = if t.is_finite() { t.clamp(0.0, minimap_span) } else { 0.0 };
        (bar.min.x as f64 + (t / minimap_span) * f64::from(bar.width())) as f32
    };
    let thumb = Rect::from_min_max(pos2(left, bar.min.y), pos2(left + w, bar.max.y));
    // lit while the pointer is over the thumb, and for as long as any zoom-bar drag is going on --
    // dragging by an edge handle moves the pointer off the thumb, and the thumb must not flash grey
    // while it is being worked (the highlight follows the drag, not just the pointer)
    let hover = ui.rect_contains_pointer(thumb) || matches!(app.tl.drag, Some(Drag::ZoomBar { .. })) && resp.dragged();
    p.rect_filled(thumb, thumb.height() / 2.0, if hover { t.scroll_thumb_hover } else { t.scroll_thumb });
    // Below 25% of the SCREEN's width the minimap's clip indicators disappear (they become
    // unreadable that narrow): a fresh decision every frame from the bar's width alone, so it can
    // never stick. The window's height never enters into it. The position indicator stays.
    let minimap_room = minimap_clips_fit(bar.width(), ui.ctx().content_rect().width());
    if app.session.state.timeline_ui.slider_minimap && minimap_room {
        // One class per x column: 0 empty, 1 the lower-priority kind, 2 the higher, 3 offline
        // media (always on top, whatever Prioritise Audio says). The covered kind's rectangle
        // simply ends where the winner begins. Drawn after the thumb (slightly transparent), so
        // clips never disappear behind the slider.
        let cols = bar.width().max(1.0).ceil() as usize;
        let mut boundaries = vec![[0i64; 3]; cols.saturating_add(1)];
        let pri_audio = app.session.state.timeline_ui.prioritise_audio;
        for (tracks, is_audio) in [(&seq.video_tracks, false), (&seq.audio_tracks, true)] {
            for tr in tracks.iter() {
                for it in tr.items.iter() {
                    let offline = app.session.project.item(it.item).and_then(|i| i.as_media()).is_some_and(|m| m.offline);
                    let top = if offline {
                        3u8
                    } else if is_audio == pri_audio {
                        2
                    } else {
                        1
                    };
                    let a = minimap_x(it.start.seconds());
                    let b = minimap_x(it.end().seconds());
                    let c0 = ((a - bar.min.x).floor() as isize).max(0) as usize;
                    let c1 = ((b - bar.min.x).ceil() as isize).clamp(0, cols as isize) as usize;
                    minimap_add_span(&mut boundaries, c0, c1, usize::from(top));
                }
            }
        }
        let class = minimap_classes(&boundaries);
        // Everything on the device pixel grid: one solid, continuous shape per rectangle, the
        // same height whatever the width.
        let ppp = ui.ctx().pixels_per_point().max(1.0);
        let px = 1.0 / ppp;
        let snap = |v: f32| (v * ppp).round() / ppp;
        // exactly 2 device pixels of space above and below every indicator, both edges on the
        // device pixel grid (the half-pixel edges rendered as 2 px one side and 3 the other)
        let y0 = ((bar.min.y * ppp).round() + 2.0) / ppp;
        let y1 = (((bar.max.y * ppp).round() - 2.0).max((bar.min.y * ppp).round() + 4.0)) / ppp;
        let fill = app.session.state.timeline_ui.fill_clips;
        let vcol = parse_hex_color(&app.session.state.timeline_ui.video_indicator_color, DEFAULT_VIDEO_COLOR);
        let acol = parse_hex_color(&app.session.state.timeline_ui.audio_indicator_color, DEFAULT_AUDIO_COLOR);
        let ocol = parse_hex_color(&app.session.state.timeline_ui.offline_indicator_color, DEFAULT_OFFLINE_COLOR);
        let color_for = |k: u8| {
            if k == 3 {
                ocol
            } else if (k == 2) != pri_audio {
                vcol
            } else {
                acol
            }
        };
        // 1 pt off each run's ends: the gap between two rectangles is the vertical cut line
        let mut runs: Vec<(f32, f32, u8)> = Vec::new();
        let mut c = 0;
        while c < cols {
            if class[c] == 0 {
                c += 1;
                continue;
            }
            let k = class[c];
            let start = c;
            while c < cols && class[c] == k {
                c += 1;
            }
            let x0 = snap(bar.min.x + start as f32 + 0.5);
            let x1 = snap(bar.min.x + c as f32 - 0.5).max(x0 + 2.0 * px);
            runs.push((x0, x1, k));
        }
        // The bar is a pill: the first clip near the Timeline's start gets a true semicircle on
        // its left -- centered on the band, tangent to the top and bottom edges (a clean D, no
        // lopsided end angles, the same pixels top and bottom). Its leftmost point touches the
        // slider's rounded outline; everywhere else the smaller circle sits inside the pill's
        // own cap, so the indicator can never poke off the track. The right edge is left alone
        // (beyond clamping into the far cap).
        let rad = bar.height() / 2.0;
        let cy = bar.center().y;
        let outline = |x0: f32, x1: f32, inset: f32, hug_left: bool, hug_right: bool| -> Vec<Pos2> {
            let top = y0 + inset;
            let bot = y1 - inset;
            let mut xa = x0 + inset;
            let mut xb = x1 - inset;
            // a cap edge never sits ON the pill's boundary: the outline stroke is half outside
            // its path and the fill antialiases outward -- `inset + px` is what makes the two
            // modes land their edges on the SAME x (the stroke's outer edge == the fill's edge)
            let ins = inset + px;
            let mid = (top + bot) / 2.0;
            // the semicircle sits exactly 2 device pixels in from the slider's edge (the 1 px
            // stroke inset plus this 1 px), the same clearance the runs in the middle keep
            let sh = 1.0 * px;
            // Keep edge caps circular even when the clip itself is only a few pixels long.
            // Previously the radius was limited by the run's opposite edge, which flattened
            // the semicircle into a wedge as the Timeline window got narrower.
            let cap_r = (bot - top) / 2.0;
            // the right cap is the left one mirrored exactly: its centre sits the same distance
            // in from the bar's right edge and the curve bends the other way
            let cap_rr = (bot - top) / 2.0;
            // A run that reaches a rounded end owns a full semicircle. Move its opposite edge
            // to the circle's tangent when necessary, so the far edge cannot cut into the cap.
            (xa, xb) = minimap_indicator_bounds(xa, xb, bar.min.x + ins + sh + cap_r, bar.max.x - ins - sh - cap_rr, hug_left, hug_right);
            let at = |y: f32, left: bool| -> f32 {
                if left {
                    if hug_left {
                        let dy = (y - mid).abs().min(cap_r);
                        bar.min.x + ins + sh + cap_r - (cap_r * cap_r - dy * dy).max(0.0).sqrt()
                    } else {
                        let dy = (y - cy).abs().min(rad);
                        let edge = rad - (rad * rad - dy * dy).max(0.0).sqrt();
                        xa.max(bar.min.x + edge + ins)
                    }
                } else if hug_right {
                    let dy = (y - mid).abs().min(cap_rr);
                    bar.max.x - ins - sh - cap_rr + (cap_rr * cap_rr - dy * dy).max(0.0).sqrt()
                } else {
                    let dy = (y - cy).abs().min(rad);
                    let edge = rad - (rad * rad - dy * dy).max(0.0).sqrt();
                    xb.min(bar.max.x - edge - ins)
                }
            };
            let mut pts = Vec::new();
            pts.push(pos2(at(top, true), top));
            pts.push(pos2(at(top, false), top));
            // every sample sits at the y it is drawn at -- the old half-step skew sampled the
            // curve below the point it drew, leaving the top of the semicircle a pixel further
            // out than the bottom
            let mut y = top;
            while y < bot - 1e-3 {
                y = (y + px).min(bot);
                pts.push(pos2(at(y, false), y));
            }
            pts.push(pos2(at(bot, true), bot));
            let mut y = bot;
            while y > top + 1e-3 {
                y = (y - px).max(top);
                pts.push(pos2(at(y, true), y));
            }
            pts
        };
        // fully opaque, drawn over the slider so nothing ever hides behind it
        for (index, &(x0, x1, k)) in runs.iter().enumerate() {
            // "very near the beginning" / "very near the end": within the pill's cap zones
            let (hug_left, hug_right) = minimap_indicator_hugs_edges(index, runs.len(), x0 - bar.min.x, bar.max.x - x1, rad);
            let col = color_for(k);
            if minimap_indicator_filled(fill, x1 - x0, 5.0 * px, hug_left, hug_right) {
                // solid: one triangle fan over the whole shape (rounded cap included)
                let pts = outline(x0, x1, 0.0, hug_left, hug_right);
                let n = pts.len().max(3);
                let mut cx = 0.0;
                let mut cy2 = 0.0;
                for q in &pts {
                    cx += q.x;
                    cy2 += q.y;
                }
                let mut mesh = egui::Mesh::default();
                mesh.colored_vertex(pos2(cx / n as f32, cy2 / n as f32), col);
                for q in &pts {
                    mesh.colored_vertex(*q, col);
                }
                for i in 0..n as u32 {
                    mesh.add_triangle(0, 1 + i, 1 + (i + 1) % n as u32);
                }
                p.add(egui::Shape::mesh(mesh));
            } else {
                // a 2 device-pixel outline as one continuous closed stroke around the shape
                p.add(egui::Shape::Path(egui::epaint::PathShape::closed_line(outline(x0, x1, px, hug_left, hug_right), Stroke::new(2.0 * px, col))));
            }
        }
    }
    // Each end circle: a dark 4.5 disc with a white ring AROUND it (4.5..6.0). While a circle is
    // touched or an edge is dragged the ring only comes further IN -- its outer edge never moves --
    // and it stops at the thickness whose dark middle looked right. Touching either circle lights
    // up the other one too.
    let hp = resp.hover_pos().or_else(|| resp.interact_pointer_pos());
    const CIRCLE: f32 = 7.0;
    let cy = thumb.center().y;
    let hi = thumb.height();
    let circle_l = thumb.min.x + hi / 2.0;
    let circle_r = thumb.max.x - hi / 2.0;
    // while the MIDDLE is grabbed the end circles ignore the pointer entirely (the pinned
    // cursor sits on top of them and used to light their rings up)
    let middle_grab = matches!(app.tl.drag, Some(Drag::ZoomBar { mode: 0, .. }));
    let on_circle = !middle_grab
        && hp.is_some_and(|p| {
            ((p.x - circle_l).abs() <= CIRCLE && (p.y - cy).abs() <= CIRCLE) || ((p.x - circle_r).abs() <= CIRCLE && (p.y - cy).abs() <= CIRCLE)
        });
    let edge_grab = matches!(app.tl.drag, Some(Drag::ZoomBar { mode: 1 | 2, .. }));
    /// The ring's outer edge: a 4.5 disc with a 1.5 ring around it. It never grows outwards.
    const RING_OUT: f32 = 6.0;
    // Hovering an edge (circle or edge zone) brings both rings in 1.5x; clicking or dragging an
    // edge brings them in the full 2.25x. Smooth Animations eases between the states quickly.
    let edge_hover =
        on_circle || (!middle_grab && hp.is_some_and(|p| thumb.expand2(vec2(0.0, 4.0)).contains(p) && (p.x < thumb.min.x + hi || p.x > thumb.max.x - hi)));
    let edge_press = edge_grab || (edge_hover && ui.input(|i| i.pointer.primary_down()));
    let ring_target = if edge_press {
        3.375
    } else if edge_hover {
        2.25
    } else {
        1.5
    };
    let ring_w = if app.session.state.timeline_ui.smooth_animations {
        let k = (ui.input(|i| i.stable_dt).min(0.05) / 0.05).min(1.0);
        let w = app.tl.zoombar_ring_w + (ring_target - app.tl.zoombar_ring_w) * k;
        app.tl.zoombar_ring_w = w;
        if (w - ring_target).abs() > 0.01 {
            ui.ctx().request_repaint();
        }
        w
    } else {
        app.tl.zoombar_ring_w = ring_target;
        ring_target
    };
    for x in [circle_l, circle_r] {
        p.circle_filled(pos2(x, cy), RING_OUT, t.icon_active);
        p.circle_filled(pos2(x, cy), RING_OUT - ring_w, t.panel_bg);
    }
    // hovering an end circle reads as a resize handle
    // only (never the slider's middle), and it stays through the press and the WHOLE edge drag --
    // the slider can run away from the cursor while it zooms. Shift drags hide the cursor (it is
    // pinned), so it stays hidden there.
    app.tl.zoombar_point_cursor = app.tl.zoombar_cursor_lock.is_none() && (on_circle || edge_grab);
    if app.tl.zoombar_point_cursor && on_circle && !edge_grab {
        ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
    } else if edge_grab {
        ui.ctx().set_cursor_icon(CursorIcon::ResizeHorizontal);
    }
    // The "you are here" marker on the fixed minimap: the true visible window in the plain linear
    // map, drawn as one blue line (2 pt) with a 1 pt gap to the slider -- above it normally, below
    // it with Alternate Slider Position (where the band sits at the top of the Timeline). The
    // marker is CENTERED on the true window (min width 2 pt, grown symmetrically): under a
    // center-zoom the view's center never moves, so the marker must never drift either -- growing
    // it to the right instead is what made it hop a pixel near the Maximum Zoom Level.
    if app.session.state.timeline_ui.slider_minimap && app.session.state.timeline_ui.show_position_indicator {
        let scroll = if app.ui.timeline.scroll.is_finite() { app.ui.timeline.scroll.max(0.0) } else { 0.0 };
        let vis_s = if vis.is_finite() && vis > 0.0 { vis } else { minimap_span };
        let a = minimap_x(scroll);
        let b = minimap_x(scroll + vis_s);
        // The marker is the visible window on the map: 2 px shorter overall and at least 2 px
        // wide. Each edge tracks its own side of the window -- the left edge never moves when
        // the view sits at the very start of the Timeline and only collapses, and the right edge
        // never moves at the very end -- so zooming at an end cannot make it creep either way.
        let x0 = (a + 1.0).min(bar.max.x - 2.0).max(bar.min.x);
        let x1 = ((b - 1.0).max(x0 + 2.0)).min(bar.max.x);
        // The playhead's EXACT colour (#5895ec): at the default setting the marker is drawn with
        // the theme's own playhead token, so the two can never drift apart (the playhead follows
        // a custom Appearance highlight). A custom setting still wins.
        let marker = if app.session.state.timeline_ui.position_indicator_color.eq_ignore_ascii_case("#5895ec") {
            t.playhead
        } else {
            parse_hex_color(&app.session.state.timeline_ui.position_indicator_color, DEFAULT_POS_COLOR)
        };
        let (my0, my1) =
            if app.session.state.timeline_ui.alternate_slider_position { (bar.max.y + 1.0, bar.max.y + 3.0) } else { (bar.min.y - 3.0, bar.min.y - 1.0) };
        p.rect_filled(Rect::from_min_max(pos2(x0, my0), pos2(x1, my1)), 0.0, marker);
    }
    app.auto.add("timeline.zoomBar", thumb, "zoom scroll bar");
    app.auto.add("timeline.zoomBar.track", bar, "zoom scroll bar track");
    let popup_id = egui::Popup::default_response_id(&resp);
    let anchor_id = popup_id.with("timeline-settings-anchor");
    // a Controls binding the user chose: its chord does what the gesture used to do (the
    // built-in gestures still work). A chord with Ctrl/Alt fires even while a text field has
    // focus (it is a shortcut, not typing); a plain key only when nothing else wants the keyboard.
    let key_trigger = ui.data_mut(|d| d.remove_temp::<usize>(zoom_trigger_key()));
    if key_trigger.is_some_and(|slot| slot >= 4) {
        ui.data_mut(|d| d.insert_temp(egui::Id::unique("tl-controls-zoom-extra-action"), key_trigger.unwrap_or(0)));
    }
    let mouse = (!controls_capturing(ui)).then(|| gesture(&resp, ui)).flatten();
    let binds = {
        let s = &app.session.state.timeline_ui;
        [s.control_center_on_playhead.clone(), s.control_explode_slider.clone(), s.control_alternate_drag_speed.clone(), s.control_settings.clone()]
    };
    let extra_mouse_trigger = if let Some(mouse) = mouse.as_deref() {
        app.session
            .state
            .timeline_ui
            .timeline_control_bindings
            .iter()
            .enumerate()
            .find(|(i, binding)| *i != 1 && !binding.is_empty() && binding.as_str() == mouse)
            .map(|(i, _)| i + 4)
    } else {
        None
    };
    if let Some(slot) = extra_mouse_trigger {
        ui.data_mut(|d| d.insert_temp(egui::Id::unique("tl-controls-zoom-extra-action"), slot));
    }
    // mouse buttons are capturable and shown in the box, but only a key chord triggers an
    // action here -- a "Left Click" binding must never hijack every click in the app
    let center_bound = slider_action_triggered(key_trigger, 0, &binds[0], "Double Click", mouse.as_deref());
    let explode_bound = slider_action_triggered(key_trigger, 1, &binds[1], "Triple Click", mouse.as_deref());
    // while a Controls box is capturing, a mouse input is a candidate binding -- it must never
    // also open the settings (which would reset the capture) or otherwise act on the bar
    let secondary_click = !controls_capturing(ui)
        && ui.input(|i| i.pointer.button_clicked(egui::PointerButton::Secondary) && i.pointer.interact_pos().is_some_and(|p| bar.contains(p)));
    let settings_bound = key_trigger == Some(3) || (secondary_click && (binds[3].is_empty() || binds[3] == "Right Click"));
    let open_at =
        settings_bound.then(|| ui.input(|i| i.pointer.interact_pos().or(i.pointer.hover_pos()).filter(|p| bar.contains(*p)).unwrap_or_else(|| bar.center())));
    let screen = ui.ctx().content_rect();
    let fallback_popup_width = 283.4;
    let ctx = ui.ctx().clone();
    if let Some(pos) = open_at {
        let space_above = pos.y - screen.top();
        let space_below = screen.bottom() - pos.y;
        let above = space_above > space_below;
        ui.data_mut(|data| data.insert_temp(anchor_id, (pos, above)));
        ui.data_mut(|data| data.insert_temp(zoombar_popup_open_key(), true));
        ui.data_mut(|data| data.insert_temp(egui::Id::unique("timeline-zoombar-popup-fresh"), true));
        // the menu opens in its default state, never with a flyout or colour picker left over
        zoombar_state_reset(ui.ctx());
        // work on the Timeline's sliders: the keys bound here must fire afterwards
        app.ui.focused = crate::dock::PanelKind::Timeline;
        // the two settings menus are mutually exclusive: this one takes the sliders' with it
        app.tl.track_slider_settings_pos = None;
    } else if ui.input(|i| i.pointer.button_clicked(egui::PointerButton::Primary) && i.pointer.interact_pos().is_some_and(|p| bar.contains(p))) {
        ui.data_mut(|data| data.insert_temp(zoombar_popup_open_key(), false));
    }
    let (requested_pos, above) =
        ui.data(|data| data.get_temp::<(Pos2, bool)>(anchor_id)).unwrap_or_else(|| (resp.interact_pointer_pos().unwrap_or(bar.min), false));
    let area_size = egui::containers::AreaState::load(&ctx, popup_id).and_then(|state| state.size);
    let popup_width = area_size.map_or(fallback_popup_width, |size| size.x);
    // Keep the click as the menu's horizontal anchor. Flipping to the opposite side near the
    // right edge made the menu appear unrelated to the pointer; the popup can scroll vertically
    // when the window is short.
    let popup_pos = pos2(requested_pos.x, requested_pos.y.clamp(screen.top(), screen.bottom()));
    let is_open = ctx.data(|data| data.get_temp::<bool>(zoombar_popup_open_key())).unwrap_or(false);
    // Keep one Area and layer alive even while the menu is closed. It needs enough layout room
    // for the complete settings list; 660 points covers the natural menu plus its frame. The
    // inner ScrollArea caps itself to the viewport only when the window is too short.
    let popup_height = screen.height().clamp(1.0, 660.0);
    // `Frame::popup` paints inset from the Area's allocation. Align the visible frame itself to
    // the pointer, so the padding does not leave the menu a handful of pixels away from the click.
    let x = if is_open { popup_pos.x - 5.0 } else { screen.left() };
    let y = if is_open && above {
        (popup_pos.y.min(screen.bottom()) - popup_height + 21.0).clamp(screen.top(), (screen.bottom() - popup_height).max(screen.top()))
    } else if is_open {
        (popup_pos.y - 5.0).clamp(screen.top(), (screen.bottom() - popup_height).max(screen.top()))
    } else {
        screen.top()
    };
    let panel = egui::Area::new(popup_id)
        .order(egui::Order::Foreground)
        .pivot(egui::Align2::LEFT_TOP)
        .default_size(egui::vec2(popup_width, popup_height))
        .fixed_pos(pos2(x, y))
        .constrain_to(screen)
        .fade_in(false)
        .interactable(is_open)
        .show(&ctx, |ui| {
            if is_open {
                ui.set_min_size(egui::vec2(popup_width, popup_height));
            }
            if is_open {
                egui::Frame::popup(ui.style()).show(ui, |ui| zoombar_popup_content(app, ui, screen.height())).inner
            }
        });
    let popup_response = if is_open {
        ctx.data_mut(|data| data.insert_temp(egui::Id::unique("timeline-zoombar-popup-rect"), panel.response.rect));
        Some(panel)
    } else {
        None
    };
    if !controls_capturing(ui)
        && let Some(click_pos) = ui.input(|input| input.pointer.button_clicked(egui::PointerButton::Primary).then(|| input.pointer.interact_pos()).flatten())
        && popup_response.as_ref().is_some_and(|popup| !popup.response.rect.contains(click_pos))
        // a click on one of the settings' own popups (the Minimap flyout, a colour picker) is not
        // an outside click either -- their pills must not close the menu they belong to
        && !family_contains(ui, click_pos)
    {
        ctx.data_mut(|data| data.insert_temp(zoombar_popup_open_key(), false));
    }
    let range = (total - vis).max(0.0);
    let triple = explode_bound;
    let double = center_bound;
    if triple {
        // triple-click: the whole Timeline in view
        let v = &mut app.ui.timeline;
        // fitting the whole Timeline: the floor is the sequence-dependent one (the same value
        // the fit computes), never a fixed 0.05, or a long sequence can never fit
        let fit_pps = (view_w.max(1.0) as f64 / timeline_extent(seq_seconds).max(1e-9)).min(max_pps(view_w, min_view)).max(1e-6);
        v.target_pps = fit_pps;
        v.target_scroll = 0.0;
        if !app.session.state.timeline_ui.smooth_animations {
            v.pps = fit_pps;
            v.scroll = 0.0;
        }
        app.tl.zoom_anchor = None;
    } else if double {
        // double-click: the Playhead Centering Range around the playhead
        let side = if app.session.state.timeline_ui.playhead_side_seconds.is_finite() {
            app.session.state.timeline_ui.playhead_side_seconds.clamp(0.1, 3600.0)
        } else {
            10.0
        };
        let span = side * 2.0;
        // fitting the centring range: no fixed 0.05 floor (it stopped a long range from ever
        // fitting), just the Maximum Zoom Level ceiling
        let fit_pps = (view_w.max(1.0) as f64 / span.max(1e-9)).min(max_pps(view_w, min_view)).max(1e-6);
        let playhead = app.session.playhead().seconds();
        // The Maximum Zoom Level can force a wider view than the requested range. Center against
        // that actual visible width, or the playhead sits left of center at the zoom cap.
        let visible = view_w.max(1.0) as f64 / fit_pps;
        let target_scroll = (playhead - visible / 2.0).clamp(0.0, (timeline_extent(seq_seconds) - visible).max(0.0));
        let v = &mut app.ui.timeline;
        v.target_pps = fit_pps;
        v.target_scroll = target_scroll;
        if !app.session.state.timeline_ui.smooth_animations {
            v.pps = fit_pps;
            v.scroll = target_scroll;
        }
        app.tl.zoom_anchor = None;
    }
    // a press beside the thumb brings the thumb's middle under the pointer (and a drag goes on
    // from there)
    let jump = |v: &mut crate::state::TimelineView, x: f32| {
        if per_point > 0.0 {
            v.target_scroll = ((x - bar.min.x - w / 2.0) as f64 * per_point).clamp(0.0, range);
            v.scroll = v.target_scroll;
        }
    };
    if !double
        && !triple
        && !controls_capturing(ui)
        && ui.rect_contains_pointer(bar)
        && ui.input(|i| i.pointer.button_clicked(egui::PointerButton::Primary))
        && let Some(pos) = resp.interact_pointer_pos().filter(|pos| !thumb.contains(*pos))
    {
        // a jump is manual too: it escapes Playhead Centering's hold
        app.tl.playhead_captured = false;
        jump(&mut app.ui.timeline, pos.x);
    }
    // (a drag is known to be one only after the pointer has moved: what was grabbed is what was
    // under the pointer when the button went down)
    if !controls_capturing(ui)
        && resp.drag_started()
        && let Some(pos) = ui.input(|i| i.pointer.press_origin()).or(resp.interact_pointer_pos())
    {
        // Each end circle is a grey 4.5 disc with a white 1.5 ring around it, centred height/2
        // inside the thumb. Grabbing ANY of the circle -- grey or white, inside or outside the
        // thumb -- drags that edge: the circles win over every other interpretation, so a click on
        // the ring can never fall through to "beside the thumb" or to the thumb's middle. The
        // reach is deliberately a little wider than the ring so the smaller target always wins.
        // Distance to the circle centres decides.
        const CIRCLE: f32 = 7.0;
        let hi = thumb.height();
        let cy = thumb.center().y;
        let circle_l = thumb.min.x + hi / 2.0;
        let circle_r = thumb.max.x - hi / 2.0;
        let zone = (hi / 2.0 + 4.5).min((thumb.width() - 1.0) / 2.0).max(1.0);
        let mode = if (pos.x - circle_l).abs() <= CIRCLE && (pos.y - cy).abs() <= CIRCLE {
            1
        } else if (pos.x - circle_r).abs() <= CIRCLE && (pos.y - cy).abs() <= CIRCLE {
            2
        } else if !thumb.expand2(vec2(0.0, 4.0)).contains(pos) {
            // a jump is manual too: it escapes Playhead Centering's hold
            app.tl.playhead_captured = false;
            jump(&mut app.ui.timeline, pos.x);
            0
        } else if pos.x < thumb.min.x + zone {
            1
        } else if pos.x > thumb.max.x - zone {
            2
        } else {
            0
        };
        // hovering an edge circle reads as "click me": point like a link cursor -- only on the
        // circles, never the slider's middle; skipped while dragging (the cursor is pinned)
        if app.tl.drag.is_none() {
            let on_l = (pos.x - circle_l).abs() <= CIRCLE && (pos.y - cy).abs() <= CIRCLE;
            let on_r = (pos.x - circle_r).abs() <= CIRCLE && (pos.y - cy).abs() <= CIRCLE;
            if on_l || on_r {
                ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
            }
        }
        app.tl.drag = Some(Drag::ZoomBar { grab: pos.x, start: app.ui.timeline.target_scroll, start_pps: app.ui.timeline.target_pps, mode });
        app.tl.zoombar_track_control = false;
        ui.data_mut(|d| d.insert_temp(egui::Id::unique("timeline-control-tracks-axis"), false));
        // a fresh edge zoom is a fresh gravity action: re-anchor Playhead Centering on where the
        // playhead sits in the view right now (or let it go when it is off screen -- zooming with
        // the playhead off screen must never snap to it)
        if mode != 0 {
            app.tl.playhead_captured = false;
            app.tl.playhead_capture_vis = 0.0;
        }
    }
    if matches!(app.tl.drag, Some(Drag::ZoomBar { .. }))
        && crate::framework::escape_pressed(ui.ctx())
        && let Some(Drag::ZoomBar { start, start_pps, .. }) = app.tl.drag.take()
    {
        let v = &mut app.ui.timeline;
        v.target_scroll = start;
        v.scroll = start;
        v.target_pps = start_pps;
        v.pps = start_pps;
        // the cancelled drag takes its gravity anchor with it
        app.tl.playhead_captured = false;
        app.tl.playhead_capture_vis = 0.0;
        app.tl.playhead_capture_u = 0.5;
    }
    if let Some(Drag::ZoomBar { mode, .. }) = app.tl.drag
        && resp.dragged()
    {
        let shift = alternate_down(ui.ctx(), &app.session.state.timeline_ui.control_alternate_drag_speed);
        let delta = resp.drag_delta();
        let axis_key = egui::Id::unique("timeline-control-tracks-axis");
        let decided = ui.data(|d| d.get_temp::<bool>(axis_key)).unwrap_or(false);
        // Measure from the press, not this frame's tiny mouse movement: a single vertical
        // pixel must not lock the gesture before its intended direction is clear.
        let direction = ui.input(|i| i.pointer.interact_pos().zip(i.pointer.press_origin()).map(|(pos, origin)| pos - origin)).unwrap_or(delta);
        let choose_axis = mode == 0 && !decided && direction.length_sq() >= 36.0;
        let settings = &app.session.state.timeline_ui;
        let angle = if direction.x < 0.0 { settings.control_tracks_angle_left } else { settings.control_tracks_angle_right };
        let angle = if angle.is_finite() { angle.clamp(30.0, 89.0) } else { 71.565_05 };
        let just_control = choose_axis && settings.control_tracks && direction.x.abs() <= direction.y.abs() / angle.to_radians().tan();
        if choose_axis {
            ui.data_mut(|d| d.insert_temp(axis_key, true));
        }
        if just_control {
            app.tl.zoombar_track_control = true;
        }
        if mode == 0 && settings.control_tracks && !decided && !choose_axis {
            return;
        }
        // While Shift is held in a normal zoom-bar drag the OS cursor is made invisible with a fully
        // transparent 1x1 cursor bitmap, confined to the app window, and PINNED to the exact
        // position where Shift went down: every frame it is warped back there, so it never moves
        // while Shift is held, never hits a screen or window edge, and reappears on release at the
        // very pixel where it vanished. The invisible cursor plus confinement exist too -- but
        // winit's own hide must NOT be used, which clips a grabbed + hidden cursor to a one-pixel
        // rect at the window's center on Windows (winit window_state.rs refresh_os_cursor) and
        // freezes it. A transparent bitmap avoids that HIDDEN flag entirely, so confinement works.
        //
        // The linked vertical resize stays unpinned so its frame delta remains measurable; normal
        // Shift zoom/pan gestures use the pinned cursor path.
        let mut shift_dx = 0.0f64;
        let mut shift_dy = 0.0f32;
        let mut track_dy = if just_control { delta.y } else { 0.0 };
        if shift && let Some(origin) = ui.input(|i| i.pointer.hover_pos().or(i.pointer.latest_pos())) {
            if app.tl.zoombar_cursor_lock.is_none() {
                app.tl.zoombar_cursor_lock = Some(origin);
                app.tl.zoombar_cursor_viewport = Some(ui.ctx().viewport_id());
                ui.ctx().send_viewport_cmd(egui::ViewportCommand::CursorGrab(egui::viewport::CursorGrab::Confined));
            }
            let pinned = app.tl.zoombar_cursor_lock.unwrap_or(origin);
            let now = ui.input(|i| i.pointer.latest_pos().unwrap_or(pinned));
            shift_dx = if just_control { f64::from(delta.x) } else { f64::from(now.x - pinned.x) };
            shift_dy = now.y - pinned.y;
            ui.ctx().send_viewport_cmd(egui::ViewportCommand::CursorPosition(pinned));
            ui.ctx().set_cursor_image(Some(transparent_cursor()));
        } else if let Some(_origin) = app.tl.zoombar_cursor_lock.take() {
            app.tl.zoombar_unlock_catchup = true;
            release_zoombar_cursor(ui.ctx(), app.tl.zoombar_cursor_viewport.take());
        }
        if app.tl.zoombar_track_control && !just_control {
            track_dy = if app.tl.zoombar_cursor_lock.is_some() { shift_dy } else { delta.y };
        }
        if mode == 0 && app.tl.zoombar_track_control && track_dy.is_finite() {
            let rate = if shift {
                let configured = app.session.state.timeline_ui.shift_drag_speed;
                let speed = if configured.is_finite() { configured.clamp(0.1, 50.0) as f32 } else { 1.0 };
                (SHIFT_THUMB_RATE * shift_drag_rate(speed)) as f32
            } else {
                1.0
            };
            ui.data_mut(|d| d.insert_temp(egui::Id::unique("timeline-control-tracks-dy"), track_dy * rate));
        }
        // While pinned the hand-measured delta is exact; otherwise plain egui pointer motion,
        // exactly since the last frame. Never retroactive: pressing and letting go of Shift mid-drag
        // just changes the speed and never re-moves what was already dragged --
        // `zoombar_unlock_catchup` skips the one unlock frame whose warp would otherwise show up as
        // a bogus negative delta.
        let plain_dx = resp.drag_delta().x;
        let moved = if app.tl.zoombar_cursor_lock.is_some() {
            shift_dx
        } else if plain_dx.is_finite() && !app.tl.zoombar_unlock_catchup {
            plain_dx as f64
        } else {
            0.0
        };
        if app.tl.zoombar_unlock_catchup {
            // one frame of no movement: the final warp back to the pin position settles here
            app.tl.zoombar_unlock_catchup = false;
        }
        // The Alternate Drag Speed setting scales Shift's sensitivity above/below the tuned curve
        // for the view's current zoom (see [`shift_drag_base`]). Without Shift the drag is exactly
        // 1:1 -- the thumb moves with the mouse, one point moves it one point.
        let base = if shift && app.tl.zoombar_cursor_lock.is_some() {
            let full_view = timeline_extent(seq_seconds) * view_w.max(1.0) as f64 / f64::from(bar.width().max(1.0));
            let pps = app.ui.timeline.pps;
            let visible = if pps.is_finite() && pps > 0.0 { view_w.max(1.0) as f64 / pps } else { full_view };
            let speed =
                if app.session.state.timeline_ui.shift_drag_speed.is_finite() { app.session.state.timeline_ui.shift_drag_speed.clamp(0.1, 50.0) } else { 1.0 };
            shift_drag_base(visible, full_view, frame_seconds) * speed
        } else {
            1.0
        };
        let dx = if app.tl.zoombar_track_control { 0.0 } else { moved * base };
        let v = &mut app.ui.timeline;
        match mode {
            // the thumb's middle: scroll, the thumb staying under the pointer. Dragging the slider
            // by hand is the one way out of Playhead Centering's hold ("event horizon") -- it
            // releases the capture until a zoom catches the playhead in view again.
            0 => {
                if moved != 0.0 && app.tl.playhead_captured {
                    app.tl.playhead_captured = false;
                }
                v.target_scroll = (v.target_scroll + dx * per_point).clamp(0.0, range);
                v.scroll = v.target_scroll;
            }
            // either edge handle: the whole slider is mirrored about its middle -- dragging an
            // edge outward grows both edges equally (the view sees less), dragging it inward
            // shrinks both. The footage at the view's center stays put.
            _ => {
                // 1:1 with the mirrored thumb in the LOG-mapped range: dragging an edge `dx` points
                // also moves the other edge `dx` points, so the thumb grows by `2 * dx` points of
                // bar width -- the zoom changes however much that share of the log range means. The
                // mirror runs the same direction on both edges; for the LEFT edge the drag
                // direction flips the meaning, hence the sign.
                let dir = if mode == 1 { -1.0 } else { 1.0 };
                let vis = view_w.max(1.0) as f64 / v.pps.max(1e-9);
                // Alternate Drag Speed adjusts edge grabs only -- the middle is never touched by
                // the extra zoomed-in sensitivity ([`shift_edge_speed_multiplier`]).
                let dx = if shift && app.tl.zoombar_cursor_lock.is_some() { dx * shift_edge_speed_multiplier(vis) } else { dx };
                let f0 = log_frac(vis, total, min_view);
                // the thumb is MIN_THUMB_W + f * (bar - MIN_THUMB_W) wide, so this keeps the
                // growth at exactly 2 points of thumb per point of drag
                let nf = (f0 + dir * dx / f64::from((bar.width() - MIN_THUMB_W).max(1.0)) * 2.0).clamp(0.0, 1.0);
                let new_vis = vis_at_frac(nf, total, min_view);
                // Playhead Centering works off the view as it was BEFORE this frame's zoom: with
                // the playhead off screen the zoom is plain zooming and nothing ever snaps to it.
                let ph = app.session.playhead().seconds();
                let was_in_view = ph >= v.scroll && ph <= v.scroll + vis;
                let zooming_in = new_vis < vis - 1e-9;
                if new_vis > vis + 1e-9 {
                    app.tl.playhead_captured = false;
                }
                if zooming_in && app.session.state.timeline_ui.playhead_centering && was_in_view && !app.tl.playhead_captured {
                    app.tl.playhead_captured = true;
                    app.tl.playhead_capture_vis = vis;
                    app.tl.playhead_capture_u = ((ph - v.scroll) / vis.max(1e-9)).clamp(0.0, 1.0);
                }
                let old_scroll = v.scroll;
                zoom_about_center(v, view_w, total, view_w.max(1.0) as f64 / new_vis, min_view);
                // Keep the opposite edge of the view fixed: the right handle changes the
                // timeline's right edge while the left handle keeps its right edge in place.
                let anchored_scroll = if mode == 2 { old_scroll } else { old_scroll + vis - new_vis };
                let range = (total - new_vis).max(0.0);
                v.scroll = anchored_scroll.clamp(0.0, range);
                v.target_scroll = v.scroll;
                app.tl.zoom_anchor = None;
                // Playhead Centering: while a zoom drags the playhead into the visible range the
                // view is caught ("gravity"). ONE smooth action afterwards: where the playhead sits
                // in the view eases from where it was caught toward the middle as the view
                // collapses (smoothstep, so it neither starts nor ends with a jolt, fully centered
                // once the visible range has roughly halved). The pull is a function of the zoom
                // itself, so it can never stall at the edge and then snap in a few frames. Once
                // caught the playhead never leaves the screen. Grabbing the slider's middle pulls
                // it off manually (mode 0).
                if zooming_in && app.session.state.timeline_ui.playhead_centering {
                    let vis = view_w.max(1.0) as f64 / v.pps.max(1e-9);
                    let range = (total - vis).max(0.0);
                    // a capture left over from an earlier gesture whose playhead some other edit
                    // has since moved off screen is stale: drop it rather than ever yank the view
                    // across the timeline to the playhead
                    if app.tl.playhead_captured && !was_in_view {
                        app.tl.playhead_captured = false;
                        app.tl.playhead_capture_vis = 0.0;
                        app.tl.playhead_capture_u = 0.5;
                    }
                    if app.tl.playhead_captured {
                        let vis0 = if app.tl.playhead_capture_vis.is_finite() && app.tl.playhead_capture_vis > 0.0 { app.tl.playhead_capture_vis } else { vis };
                        let u0 = if app.tl.playhead_capture_u.is_finite() { app.tl.playhead_capture_u.clamp(0.0, 1.0) } else { 0.5 };
                        let s = ((1.0 - vis / vis0) * 2.0).clamp(0.0, 1.0);
                        let e = s * s * (3.0 - 2.0 * s);
                        let u = u0 + (0.5 - u0) * e;
                        // the hard floor under the smooth pull: the scroll that keeps the playhead
                        // inside the visible range (ph - vis ..= ph), within the timeline
                        let lo = (ph - vis).max(0.0).min(range);
                        let hi = ph.max(0.0).min(range);
                        v.scroll = (ph - u * vis).max(lo).min(hi);
                        v.target_scroll = v.scroll;
                    }
                }
            }
        }
        // the wheel while grabbing the middle: zoom evenly about the view's center -- both edges of
        // the thumb move toward or away from each other symmetrically, no matter where within the
        // middle was grabbed. Wheel up zooms IN (the thumb shrinks), wheel down zooms out. Shift
        // slows the wheel down (the Alternate Drag Speed setting on top of a tenth of the ordinary
        // effect: 1x is ten times slower, Custom 50x five times faster) and counts a sideways
        // gesture too -- Shift + wheel reports as horizontal scroll on some platforms.
        if mode == 0 {
            let shift = ui.input(|i| i.modifiers.shift);
            let (wy, wx) = ui.input(|i| (i.smooth_scroll_delta.y, i.smooth_scroll_delta.x));
            let wheel = if shift { wy + wx } else { wy };
            let mut factor = (wheel as f64 / 600.0).clamp(-0.30, 0.30);
            if shift {
                let speed = if app.session.state.timeline_ui.shift_drag_speed.is_finite() {
                    app.session.state.timeline_ui.shift_drag_speed.clamp(0.1, 50.0)
                } else {
                    1.0
                };
                factor = (factor * speed / 10.0).clamp(-0.9, 0.9);
            }
            // Inverted controls: the wheel zooms the other way here too
            if app.session.state.timeline_ui.inverted_controls {
                factor = -factor;
            }
            if wheel.is_finite() && factor != 0.0 {
                let v = &mut app.ui.timeline;
                zoom_about_center(v, view_w, total, v.pps * (1.0 + factor), min_view);
                app.tl.zoom_anchor = None;
            }
        }
    }
    if resp.drag_stopped() {
        app.tl.drag = None;
        app.tl.zoombar_track_control = false;
        ui.data_mut(|d| d.remove_temp::<bool>(egui::Id::unique("timeline-control-tracks-axis")));
        if app.tl.zoombar_cursor_lock.take().is_some() {
            release_zoombar_cursor(ui.ctx(), app.tl.zoombar_cursor_viewport.take());
            app.tl.zoombar_unlock_catchup = true;
        }
    }
}

/// A 1x1 fully transparent cursor bitmap: hides the OS cursor without using winit's hidden flag
/// (which would clip a confined cursor to the window's center on Windows). `Arc<[u8]>` lets the
/// integration dedupe, but the 4-byte allocation per frame is trivial anyway.
fn transparent_cursor() -> egui::CustomCursorImage {
    egui::CustomCursorImage { rgba: [0, 0, 0, 0].into(), size: [1, 1], hotspot: [0, 0] }
}

/// Release the invisible, confined zoom-bar drag cursor: restore the standard cursor and let the
/// window edges go. Nothing warps -- the cursor is simply wherever the user's hand left it.
fn release_zoombar_cursor(ctx: &egui::Context, viewport: Option<egui::ViewportId>) {
    ctx.set_cursor_image(None);
    let command = egui::ViewportCommand::CursorGrab(egui::viewport::CursorGrab::None);
    ctx.send_viewport_cmd_to(cursor_release_target(viewport, ctx.viewport_id()), command);
}

fn cursor_release_target(acquired: Option<egui::ViewportId>, current: egui::ViewportId) -> egui::ViewportId {
    acquired.unwrap_or(current)
}

fn release_slider_cursor(ctx: &egui::Context, viewport: Option<egui::ViewportId>) {
    release_zoombar_cursor(ctx, viewport);
}


/// Close the zoom bar's own settings popup (the one opened by right-clicking the bar). The two
/// sliders' settings menus are mutually exclusive: opening one takes the other with it.
pub(super) fn close_zoombar_popup(ctx: &egui::Context) {
    ctx.data_mut(|data| data.insert_temp(zoombar_popup_open_key(), false));
}

/// The window that comes up when a slider is hidden: it only tells the user how to bring it
/// back, OK dismisses it. Drawn from the Timeline itself rather than the bar, so it still shows
/// once the slider is gone. One for the zoom scroll bar, one for the side track sliders.
pub fn zoombar_hide_dialog(app: &mut FilmcraftApp, ctx: &egui::Context) {
    hide_dialog(app, ctx, false);
    hide_dialog(app, ctx, true);
    right_click_dialog(app, ctx);
    snap_requires_decoupling_dialog(app, ctx);
}

fn right_click_dialog(app: &mut FilmcraftApp, ctx: &egui::Context) {
    let id = egui::Id::unique("timeline-right-click-dialog");
    if !ctx.data(|d| d.get_temp::<bool>(id)).unwrap_or(false) {
        return;
    }
    let mut dismiss = false;
    let left = ctx.data(|d| d.get_temp::<bool>(egui::Id::unique("timeline-left-click-rejected"))).unwrap_or(false);
    egui::Window::new(tl!("Shortcut unavailable"))
        .id(egui::Id::unique("timeline-right-click-dialog-window"))
        .order(egui::Order::Tooltip)
        .collapsible(false)
        .resizable(false)
        .fixed_size(egui::vec2(440.0, 0.0))
        .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
        .show(ctx, |ui| {
            ui.horizontal_wrapped(|ui| {
                ui.label(if left {
                    tl!("Left click is reserved for controlling the application and cannot be assigned.")
                } else {
                    tl!("Right click is reserved for opening slider settings and cannot be assigned.")
                });
            });
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let ok = ui.button(tl!("OK"));
                app.auto.add("timeline.rightClickDialog.ok", ok.rect, "Close the dialog");
                if ok.clicked() {
                    dismiss = true;
                }
            });
        });
    if dismiss {
        ctx.data_mut(|d| d.insert_temp(id, false));
        ctx.request_repaint();
    }
}
fn snap_requires_decoupling_dialog(app: &mut FilmcraftApp, ctx: &egui::Context) {
    let id = egui::Id::unique("timeline-snap-requires-decoupling");
    if !ctx.data(|d| d.get_temp::<bool>(id)).unwrap_or(false) {
        return;
    }
    let mut dismiss = false;
    egui::Window::new(tl!("Snap to Tracks"))
        .id(egui::Id::unique("timeline-snap-requires-decoupling-window"))
        .order(egui::Order::Tooltip)
        .collapsible(false)
        .resizable(false)
        .fixed_size(egui::vec2(440.0, 0.0))
        .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
        .show(ctx, |ui| {
            ui.horizontal_wrapped(|ui| {
                ui.label(tl!("Turn on Decouple Edges to use Snap to Tracks; linked edges cannot snap independently."));
            });
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let ok = ui.button(tl!("OK"));
                app.auto.add("timeline.trackSlider.snapRequiresDecoupling.ok", ok.rect, "Close the dialog");
                if ok.clicked() {
                    dismiss = true;
                }
            });
        });
    if dismiss {
        ctx.data_mut(|d| d.insert_temp(id, false));
        ctx.request_repaint();
    }
}

pub(super) fn promote_slider_dialogs(app: &FilmcraftApp, ctx: &egui::Context) {
    if ctx.memory(|m| m.top_modal_layer()).is_some() {
        return;
    }
    for track in [false, true] {
        if if track { app.tl.track_hide_dialog_open } else { app.tl.zoombar_hide_dialog_open } {
            ctx.move_to_top(egui::LayerId::new(egui::Order::Tooltip, egui::Id::unique(("timeline-zoombar-hide-dialog", track))));
        }
    }
    for (state, window) in [
        ("timeline-right-click-dialog", "timeline-right-click-dialog-window"),
        ("timeline-snap-requires-decoupling", "timeline-snap-requires-decoupling-window"),
    ] {
        if ctx.data(|d| d.get_temp::<bool>(egui::Id::unique(state))).unwrap_or(false) {
            ctx.move_to_top(egui::LayerId::new(egui::Order::Tooltip, egui::Id::unique(window)));
        }
    }
}

fn hide_dialog(app: &mut FilmcraftApp, ctx: &egui::Context, track: bool) {
    let open = if track { app.tl.track_hide_dialog_open } else { app.tl.zoombar_hide_dialog_open };
    if !open {
        return;
    }
    let row = if track { tl!("Unhide Track Slider") } else { tl!("Unhide Timeline Slider") };
    let mut dismiss = false;
    egui::Window::new(tl!("Hide Slider"))
        .id(egui::Id::unique(("timeline-zoombar-hide-dialog", track)))
        .order(egui::Order::Tooltip)
        .collapsible(false)
        .resizable(false)
        .fixed_size(egui::vec2(440.0, 0.0))
        .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
        .show(ctx, |ui| {
            ui.horizontal_wrapped(|ui| {
                ui.label(tlf!("You can bring it back anytime by right clicking the timeline and selecting \"{row}\" at the bottom of the list.", row = row));
            });
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let ok = ui.button(tl!("OK"));
                app.auto.add("timeline.zoomBar.hideDialog.ok", ok.rect, "Close the dialog");
                if ok.clicked() {
                    dismiss = true;
                }
            });
        });
    if dismiss {
        if track {
            app.tl.track_hide_dialog_open = false;
        } else {
            app.tl.zoombar_hide_dialog_open = false;
        }
        ctx.request_repaint();
    }
}

/// Hide or bring back the zoom scroll bar (Hide Slider / Unhide Timeline Slider). Hiding unwinds
/// everything the bar keeps running -- a Shift drag mid-hide would leave the cursor pinned and
/// invisible -- so nothing of it is left for later frames to tick over until it is brought back.
pub fn set_zoombar_hidden(app: &mut FilmcraftApp, ctx: &egui::Context, hidden: bool) {
    if app.session.state.timeline_ui.zoom_bar_hidden == hidden {
        return;
    }
    app.session.state.timeline_ui.zoom_bar_hidden = hidden;
    app.session.project_view_changed();
    if hidden {
        close_zoombar_popup(ctx);
        if matches!(app.tl.drag, Some(Drag::ZoomBar { .. })) {
            app.tl.drag = None;
        }
        app.tl.zoom_anchor = None;
        app.tl.zoombar_cursor_lock = None;
        app.tl.zoombar_unlock_catchup = false;
        app.tl.zoombar_point_cursor = false;
        release_zoombar_cursor(ctx, app.tl.zoombar_cursor_viewport.take());
    } else {
        // a stale "how to bring it back" window means nothing once the bar is back
        app.tl.zoombar_hide_dialog_open = false;
    }
}

/// A global safety net for the invisible zoom-bar drag cursor: the lock only exists while a
/// zoom-bar drag is really going on under Shift with the window focused and the primary button
/// down. Every frame this unwinds anything that slipped through (a lag spike, a lost focus, a
/// changed panel, Escape mid-press), so the cursor can never stay invisible or confined when it
/// shouldn't.
pub fn zoombar_cursor_watchdog(tl: &mut super::TlState, ctx: &egui::Context, settings: &filmcraft_project::TimelineUiSettings) {
    // the pin is legitimate during a zoom-bar Shift drag or a track slider's (which pins the
    // same cursor to its own press pixel)
    let live_drag = matches!(tl.drag, Some(Drag::ZoomBar { .. })) || tl.track_slider_dragging;
    let owner = cursor_release_target(tl.zoombar_cursor_viewport, ctx.viewport_id());
    let alternate_binding = if tl.track_slider_dragging { &settings.track_control_alternate_drag_speed } else { &settings.control_alternate_drag_speed };
    let alternate_captured = alternate_drag_binding_captured(ctx, alternate_binding);
    let still = tl.zoombar_cursor_lock.is_some()
        && live_drag
        && ctx.input_for(owner, |i| {
            i.focused
                && (i.pointer.button_down(egui::PointerButton::Primary) || i.pointer.button_down(egui::PointerButton::Secondary))
                && (tl.zoombar_track_control || alternate_down_input(i, alternate_binding) || alternate_captured)
        });
    if !still && let Some(_origin) = tl.zoombar_cursor_lock.take() {
        tl.zoombar_track_control = false;
        tl.zoombar_unlock_catchup = true;
        release_zoombar_cursor(ctx, tl.zoombar_cursor_viewport.take());
    }
}

// one parameter per piece of slider state keeps the two sliders (video/audio) symmetric at
// their call sites; the drag logic is one state machine like the zoom bar's
#[allow(clippy::type_complexity, clippy::too_many_arguments, clippy::cognitive_complexity)]
fn snap_edge_height(
    q: f32,
    min_height: f32,
    max_height: f32,
    anchor: f32,
    window_h: f32,
    offset: f32,
    outer: bool,
    slots: f32,
    q_delta: f32,
    previous: Option<f32>,
) -> Option<(f32, f32)> {
    let tracks = (slots - 1.0 / 3.0).round().max(1.0);
    let first = if outer { 1.0 } else { 0.0 };
    let last = if outer { slots } else { (tracks - 1.0).max(0.0) };
    let last_predecessor = if outer { (last - 1.0 / 3.0).round().min(last) } else { (last - 1.0).max(first) };
    let detent_height = |detent: f32| {
        height_for_fixed_edge_detent(anchor, window_h, offset, outer, detent)
            .filter(|height| height.is_finite() && *height >= min_height && *height <= max_height)
            .map(|height| (height, detent))
    };
    if q_delta.abs() <= 0.0001
        && let Some(previous) = previous.filter(|previous| previous.is_finite() && *previous >= first && *previous <= last)
    {
        return detent_height(previous);
    }
    let toward_high = q_delta > 0.0001;
    // Resolve the fractional reveal endpoint before the general latch. Its final whole-row
    // neighbor is only one-third of a track away, so the ordinary midpoint latch would otherwise
    // keep snapping back to that whole row while the pointer advances toward the endpoint.
    if ((q - last).abs() <= 0.01 || (toward_high && q > last_predecessor + 0.001))
        && let Some(result) = detent_height(last)
    {
        return Some(result);
    }
    if ((q - first).abs() <= 0.01 || (!toward_high && q < first + 0.5))
        && let Some(result) = detent_height(first)
    {
        return Some(result);
    }
    if let Some(previous) = previous.filter(|previous| previous.is_finite() && *previous >= first && *previous <= last) {
        let next = if toward_high {
            if outer && (previous - last_predecessor).abs() < 0.01 { last } else { (previous + 1.0).min(last) }
        } else if outer && (previous - last).abs() < 0.01 {
            last_predecessor
        } else {
            (previous - 1.0).max(first)
        };
        let gap = (next - previous).abs();
        let hysteresis = (gap * 0.05).min(0.04);
        let midpoint = (next + previous) * 0.5;
        let keep_previous = if toward_high { q <= midpoint + hysteresis } else { q >= midpoint - hysteresis };
        if gap > 0.0
            && keep_previous
            && (q - previous).abs() <= gap + hysteresis
            && let Some(result) = detent_height(previous)
        {
            return Some(result);
        }
    }
    let nearest = q.round().clamp(first, last);
    let mut candidates = [nearest, nearest - 1.0, nearest + 1.0, first, last];
    candidates.sort_by(|a, b| {
        let distance = (q - *a).abs().total_cmp(&(q - *b).abs());
        if distance.is_eq() { if toward_high { b.total_cmp(a) } else { a.total_cmp(b) } } else { distance }
    });
    for detent in candidates {
        if detent < first || detent > last || (q - detent).abs() > 0.5 {
            continue;
        }
        let Some(height) = height_for_fixed_edge_detent(anchor, window_h, offset, outer, detent) else { continue };
        if height.is_finite() && height >= min_height && height <= max_height {
            return Some((height, detent));
        }
    }
    None
}

// These values form one resize calculation; keeping them explicit makes the geometry and
// hostile-value bounds visible at the call site.
#[allow(clippy::too_many_arguments)]
fn resize_track_edge(
    state: TrackResizeState,
    dy: f32,
    shift_on: bool,
    snap: bool,
    invert: bool,
    decouple: bool,
    value: f32,
    slots: f32,
    window_h: f32,
    min_height: f32,
    bar: Rect,
    min_thumb: f32,
) -> Option<(f32, f32, f32, Option<f32>, Option<f32>)> {
    if !matches!(state.mode, 1 | 2) || !dy.is_finite() || !state.free_len.is_finite() || !state.fixed.is_finite() || !state.center.is_finite() {
        return None;
    }
    let bar_h = bar.height();
    let slots = slots.max(1.0);
    let min_height = min_height.max(0.0);
    let lo = min_thumb.min(bar_h);
    let max_height = track_max_height(min_height).max(min_height);
    let visual_outer = (invert && state.mode == 1) || (!invert && state.mode == 2);
    let default_offset = 0.0;
    let (anchor, offset, outer) = state.view_anchor.map_or((None, default_offset, visual_outer), |(a, o, edge)| (Some(a), o, edge));
    let anchor_floor = anchor.map_or(min_height, |a| {
        let (numerator, divisor) = if outer { (window_h + offset, slots - a) } else { (window_h - offset, a) };
        if divisor > 1e-6 && numerator.is_finite() { (numerator / divisor).max(min_height) } else { max_height }
    });
    let room = if !decouple {
        bar_h
    } else if state.mode == 1 {
        state.fixed - bar.min.y
    } else {
        bar.max.y - state.fixed
    }
    .max(0.0);
    // The grabbed edge may overhang the rail by one point. Apply that same endpoint allowance
    // to the available drag length, its zoom floor, and snapping so a detent cannot be undone
    // by the next no-motion frame.
    let available = (room + 1.0).clamp(0.0, bar_h);
    if available < lo || anchor_floor > max_height {
        return None;
    }
    let geometry_floor = track_height_of(available.max(lo), bar_h, min_height).max(min_height);
    let floor = anchor_floor.max(geometry_floor).min(max_height);
    let hi = track_thumb(floor, bar_h, min_height).clamp(lo, available);
    if hi <= lo || !hi.is_finite() {
        return None;
    }
    let out = if state.mode == 1 { -dy } else { dy };
    let home = snap && outer && value <= 0.5;
    let speed = if decouple || home { 1.0 } else { 2.0 };
    let base_len = if shift_on { state.free_len.clamp(lo, hi) } else { state.free_len };
    let free_len = (base_len + speed * out).clamp(lo, hi);
    let height_of = |len: f32| track_height_of(len, bar_h, min_height).clamp(floor, max_height);
    let scroll_for = |height: f32, length: f32| {
        let max_scroll = (slots * height - window_h).max(0.0);
        if let Some(a) = anchor {
            scroll_for_fixed_edge(a, height, window_h, offset, outer).clamp(0.0, max_scroll)
        } else {
            let top = if decouple { if state.mode == 1 { state.fixed - length } else { state.fixed } } else { state.center - length * 0.5 }
                .clamp(bar.min.y, (bar.max.y - length).max(bar.min.y));
            let travel = (bar_h - length).max(0.0);
            let position = if travel > 0.001 { ((top - bar.min.y) / travel).clamp(0.0, 1.0) } else { 0.5 };
            (if invert { 1.0 - position } else { position }) * max_scroll
        }
    };
    let snapping = snap && !shift_on && anchor.is_some();
    let mut height = height_of(free_len);
    let mut snap_detent = state.snap_detent;
    let mut last_snap_q = state.last_snap_q;
    if snapping && let Some(a) = anchor {
        let free_scroll = scroll_for(height, free_len);
        let q = if outer { (window_h + free_scroll + 1.0) / height } else { (free_scroll - 1.0) / height };
        if q.is_finite() {
            let q_delta = last_snap_q.map_or(0.0, |previous| q - previous);
            if let Some((snapped, detent)) = snap_edge_height(q, min_height, max_height, a, window_h, offset, outer, slots, q_delta, snap_detent) {
                // Keep the content boundary on its exact detent even if its ideal thumb is a
                // fraction longer than the currently captured rail room. The active thumb is
                // clipped to `hi` below, then its live rail anchor is synchronized for the next
                // frame. Rejecting this detent cleared the latch, making audio bounce between
                // the last whole row and the one-third endpoint during a held drag.
                height = snapped;
                snap_detent = Some(detent);
            } else {
                snap_detent = None;
            }
            last_snap_q = Some(q);
        }
    } else {
        snap_detent = None;
        last_snap_q = None;
    }
    let length = track_thumb(height, bar_h, min_height).clamp(lo, hi);
    let scroll = scroll_for(height, length);
    Some((height, scroll, free_len, snap_detent, last_snap_q))
}

