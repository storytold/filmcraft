use super::layout::{max_pps, timeline_extent};
use super::{Align2, Color32, CursorIcon, Drag, FilmcraftApp, Pos2, Rect, Sense, Stroke, StrokeKind, Tokens, pos2, vec2};
use crate::icons::{self, Icon};
use crate::widgets::{HotNumberOptions, hot_number_numeric};
use filmcraft_project::Sequence;

/// The narrowest the zoom scroll bar's thumb gets: room for its two handles and a part between
/// them to grab.
const MIN_THUMB_W: f32 = 34.0;

/// The minimap's default indicator colours, used when a stored one is missing or malformed.
const DEFAULT_POS_COLOR: Color32 = Color32::from_rgb(0x58, 0x95, 0xec);
const DEFAULT_VIDEO_COLOR: Color32 = Color32::from_rgb(0x64, 0xb5, 0xf6);
const DEFAULT_AUDIO_COLOR: Color32 = Color32::from_rgb(0x2e, 0x7d, 0x32);
const DEFAULT_OFFLINE_COLOR: Color32 = Color32::from_rgb(0xd3, 0x2f, 0x2f);

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
/// the same way, in both places.
// one section per setting keeps the popup's rows in visual order; splitting the function would
// scatter the popup's layout over the file
#[allow(clippy::cognitive_complexity)]
pub(super) fn zoombar_settings(app: &mut FilmcraftApp, ui: &mut egui::Ui) {
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
    let c = pill_switch(ui, sw, settings.smooth_animations, t, egui::Id::unique("timeline-zoombar-smooth"), "Smooth Animations");
    ui.painter().galley(pos2(x0 + sw_w + 10.0, row.center().y - galley.size().y / 2.0), galley, t.text);
    app.auto.add("timeline.zoomBar.settings.smoothAnimations", c.rect, "Smooth Animations");
    if c.clicked() {
        settings.smooth_animations = !settings.smooth_animations;
        changed = true;
    }
    // Inverted Controls: the wheel over the tracks scrolls the Timeline the other way in time,
    // and the wheel while grabbing the slider's middle zooms the other way
    let (row, _) = ui.allocate_exact_size(egui::vec2(content_w, 30.0), Sense::hover());
    let label = tl!("Inverted Controls");
    let galley = ui.painter().layout_no_wrap(label.to_string(), Tokens::ui(11.5), t.text);
    let group_w = sw_w + 10.0 + galley.size().x;
    let x0 = row.min.x + ((content_w - group_w) / 2.0).max(0.0);
    let sw = Rect::from_min_size(pos2(x0, row.center().y - 8.0), egui::vec2(sw_w, 16.0));
    let c = pill_switch(ui, sw, settings.inverted_controls, t, egui::Id::unique("timeline-zoombar-inverted"), "Inverted Controls");
    ui.painter().galley(pos2(x0 + sw_w + 10.0, row.center().y - galley.size().y / 2.0), galley, t.text);
    app.auto.add("timeline.zoomBar.settings.invertedControls", c.rect, "Inverted Controls");
    if c.clicked() {
        settings.inverted_controls = !settings.inverted_controls;
        changed = true;
    }
    // Playhead Centering: same centered pill switch + label group as the row above it
    let (row, _) = ui.allocate_exact_size(egui::vec2(content_w, 30.0), Sense::hover());
    let label = tl!("Playhead Centering");
    let galley = ui.painter().layout_no_wrap(label.to_string(), Tokens::ui(11.5), t.text);
    let group_w = sw_w + 10.0 + galley.size().x;
    let x0 = row.min.x + ((content_w - group_w) / 2.0).max(0.0);
    let sw = Rect::from_min_size(pos2(x0, row.center().y - 8.0), egui::vec2(sw_w, 16.0));
    let c = pill_switch(ui, sw, settings.playhead_centering, t, egui::Id::unique("timeline-zoombar-center"), "Playhead Centering");
    ui.painter().galley(pos2(x0 + sw_w + 10.0, row.center().y - galley.size().y / 2.0), galley, t.text);
    app.auto.add("timeline.zoomBar.settings.playheadCentering", c.rect, "Playhead Centering");
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
    let c = pill_switch(ui, sw, settings.alternate_slider_position, t, egui::Id::unique("timeline-zoombar-alternate"), "Alternate Slider Position");
    ui.painter().galley(pos2(x0 + sw_w + 10.0, row.center().y - galley.size().y / 2.0), galley, t.text);
    app.auto.add("timeline.zoomBar.settings.alternateSliderPosition", c.rect, "Alternate Slider Position");
    if c.clicked() {
        settings.alternate_slider_position = !settings.alternate_slider_position;
        changed = true;
    }
    ui.separator();
    // Maximum Zoom Level: the smallest view the zoom bar may collapse to
    settings_heading(ui, t, tl!("Maximum Zoom Level"));
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
    ui.separator();
    // Alternate Drag Speed first
    settings_heading(ui, t, tl!("Alternate Drag Speed (Shift + Left Click)"));
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
    settings_heading(ui, t, tl!("Playhead Centering Range (Triple Click)"));
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
    let c = pill_switch(ui, sw, minimap_on, t, egui::Id::unique("timeline-zoombar-minimap"), "Slider Minimap");
    ui.painter().galley(pos2(x0 + sw_w + 10.0, row.center().y - galley.size().y / 2.0), galley, label_color);
    if minimap_on {
        ui.painter().text(pos2(row.max.x - 6.0, row.center().y), Align2::CENTER_CENTER, "\u{25b6}", Tokens::ui(8.0), t.text_dim);
    }
    app.auto.add("timeline.zoomBar.settings.sliderMinimap", c.rect, "Slider Minimap");
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
    let (x, to_the_right) =
        if popup_right + gap + flyout_w <= screen.right() { (popup_right + gap, true) } else { ((popup_left - gap - flyout_w).max(screen.left()), false) };
    let popup_bottom = row.max.y + f32::from(margin.bottom);
    let flyout_h = ui.data(|d| d.get_temp::<f32>(height_key)).unwrap_or(110.0);
    let y = (popup_bottom - flyout_h).min(screen.bottom() - flyout_h);
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
    let was_open = ui.data(|d| d.get_temp::<bool>(flyout_id.with("open"))).unwrap_or(false);
    let stamp = ui.data(|d| d.get_temp::<f64>(stamp_key)).unwrap_or(-1e9);
    let open = settings.slider_minimap && (trigger || (was_open && (keep_alive || now - stamp < 0.3)));
    if open {
        ui.data_mut(|d| d.insert_temp(stamp_key, now));
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
                let c = pill_switch(ui, fsw, whole, t, egui::Id::unique("timeline-zoombar-minimap-whole"), "Show Whole Timeline");
                ui.painter().galley(pos2(fx0 + fsw_w + 10.0, frow.center().y - fgalley.size().y / 2.0), fgalley, t.text);
                app.auto.add("timeline.zoomBar.settings.showWholeTimeline", c.rect, "Show Whole Timeline");
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
                let c = pill_switch(ui, psw, pri, t, egui::Id::unique("timeline-zoombar-minimap-pri-audio"), "Prioritise Audio");
                ui.painter().galley(pos2(px0 + fsw_w + 10.0, prow.center().y - pgalley.size().y / 2.0), pgalley, t.text);
                app.auto.add("timeline.zoomBar.settings.prioritiseAudio", c.rect, "Prioritise Audio");
                if c.clicked() {
                    pri = !pri;
                }
                // Position Indicator: the "you are here" line on the slider. Off: hidden.
                let c = settings_pill_row(ui, t, CONTENT_W, tl!("Position Indicator"), egui::Id::unique("timeline-zoombar-minimap-pos-ind"), pos_ind);
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
                let c = pill_switch(ui, gsw, fill, t, egui::Id::unique("timeline-zoombar-minimap-fill"), "Fill Clips");
                ui.painter().galley(pos2(gxp + fsw_w + 10.0, grow.center().y - ggalley.size().y / 2.0), ggalley, t.text);
                app.auto.add("timeline.zoomBar.settings.fillClips", c.rect, "Fill Clips");
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
        app.session.project_view_changed();
    }
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
fn settings_pill_row(ui: &mut egui::Ui, t: &Tokens, row_w: f32, label: &str, id: egui::Id, on: bool) -> egui::Response {
    let (row, _) = ui.allocate_exact_size(egui::vec2(row_w, 30.0), Sense::hover());
    let galley = ui.painter().layout_no_wrap(label.to_string(), Tokens::ui(11.5), t.text);
    let sw_w = 34.0;
    let group_w = sw_w + 10.0 + galley.size().x;
    let x0 = row.min.x + ((row_w - group_w) / 2.0).max(0.0);
    let sw = Rect::from_min_size(pos2(x0, row.center().y - 8.0), egui::vec2(sw_w, 16.0));
    let c = pill_switch(ui, sw, on, t, id, label);
    ui.painter().galley(pos2(x0 + sw_w + 10.0, row.center().y - galley.size().y / 2.0), galley, t.text);
    c
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
    ctx.data_mut(|d| {
        d.insert_temp(color_open_key(), None::<egui::Id>);
        d.insert_temp(color_rect_key(), Rect::NOTHING);
        d.insert_temp(grace_key(), -1e9f64);
        d.insert_temp(open_pointer_key(), pointer);
        d.insert_temp(egui::Id::unique("tl-zoombar-family"), Vec::<Rect>::new());
        d.insert_temp(egui::Id::unique("tl-zoombar-family-prev"), Vec::<Rect>::new());
        d.insert_temp(egui::Id::unique("timeline-zoombar-minimap-flyout").with("rect"), Rect::NOTHING);
        d.insert_temp(egui::Id::unique("timeline-zoombar-minimap-flyout").with("stamp"), -1e9f64);
        d.insert_temp(egui::Id::unique("timeline-zoombar-minimap-flyout").with("height"), 110.0f32);
        d.insert_temp(egui::Id::unique("timeline-zoombar-minimap-flyout").with("open"), false);
    });
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
                        buf = color.clone();
                    }
                    // the hex field, dead center under the wheel (placed at an exact rect: layout
                    // alignment refused to centre it)
                    let (hrow, _) = ui.allocate_exact_size(egui::vec2(132.0, 26.0), Sense::hover());
                    let field = Rect::from_min_size(pos2(hrow.center().x - 46.0, hrow.center().y - 11.0), egui::vec2(92.0, 22.0));
                    let edit = ui.put(field, egui::TextEdit::singleline(&mut buf).align(egui::Align2::CENTER_CENTER));
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
    let pos = if pos.y + 430.0 > screen.bottom() { pos2(pos.x, (pos.y - 430.0).max(screen.top())) } else { pos };
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
    if ui.input(|i| i.pointer.button_clicked(egui::PointerButton::Primary))
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
fn settings_heading(ui: &mut egui::Ui, t: &Tokens, label: &str) {
    let (r, _) = ui.allocate_exact_size(egui::vec2(ui.available_width(), 26.0), Sense::hover());
    let galley = ui.painter().layout(label.to_string(), Tokens::ui(11.5), t.text, r.width().max(40.0));
    ui.painter().galley(pos2(r.center().x - galley.size().x / 2.0, r.center().y - galley.size().y / 2.0), galley, t.text);
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
fn track_thumb(track_h: f32, bar_h: f32) -> f32 {
    let f = 1.0 - (track_h.clamp(22.0, 220.0) / 22.0).log10() / (220.0f32 / 22.0).log10();
    MIN_THUMB_W + f.clamp(0.0, 1.0) * (bar_h - MIN_THUMB_W).max(1.0)
}

/// Its inverse: the track height a thumb `len` points long stands for.
fn track_height_of(len: f32, bar_h: f32) -> f32 {
    let f = ((len - MIN_THUMB_W) / (bar_h - MIN_THUMB_W).max(1.0)).clamp(0.0, 1.0);
    22.0 * (220.0f32 / 22.0).powf(1.0 - f)
}

/// How far a Shift drag on a track slider moves per point of pointer motion, as a fraction of an
/// ordinary 1:1 drag: about 0.075 at one track in view up to 0.125 at the full view (the Alternate
/// Drag Speed setting multiplies on top; its 1x default is half as slow again as the 1x before it).
fn track_drag_base(visible_fraction: f64) -> f64 {
    (0.075 + 0.05 * visible_fraction.clamp(0.0, 1.0)).clamp(0.05, 0.125)
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

#[cfg(test)]
mod zoom_drag_scale_tests {
    use super::{shift_drag_base, shift_edge_speed_multiplier};

    #[test]
    fn edge_speed_multiplier_hits_the_zoomed_in_anchors_and_survives_hostile_input() {
        // the tuned zoomed-in anchors: 30x the curve at 0.1 s of view, 5x at 1 s, 2x at 10 s
        assert!((shift_edge_speed_multiplier(0.1) - 30.0).abs() < 1e-9);
        assert!((shift_edge_speed_multiplier(1.0) - 5.0).abs() < 1e-9);
        assert!((shift_edge_speed_multiplier(10.0) - 2.0).abs() < 1e-9);
        // past a minute the plain curve is untouched: the full-view sweep stays a sweep
        assert!((shift_edge_speed_multiplier(60.0) - 1.0).abs() < 1e-9);
        assert!((shift_edge_speed_multiplier(1e6) - 1.0).abs() < 1e-9);
        // clamped below the smallest anchor
        assert!((shift_edge_speed_multiplier(0.01) - 30.0).abs() < 1e-9);
        // eased without jumps between the anchors
        let mid = shift_edge_speed_multiplier(0.4);
        assert!(mid > 5.0 && mid < 30.0, "{mid}");
        // hostile input falls back to the plain curve, nothing extreme
        assert!((shift_edge_speed_multiplier(f64::NAN) - 1.0).abs() < 1e-9);
        assert!((shift_edge_speed_multiplier(0.0) - 1.0).abs() < 1e-9);
        assert!((shift_edge_speed_multiplier(-3.0) - 1.0).abs() < 1e-9);
    }

    #[test]
    fn shift_speed_hits_the_tuned_anchors_and_survives_hostile_input() {
        let frame = 1.0 / 24.0;
        // tuned fixed points: one frame in view is 1/4000 of 1:1 speed, a second 0.00575
        assert!((shift_drag_base(frame, 600.0, frame) - 0.00025).abs() < 1e-9);
        assert!((shift_drag_base(1.0, 600.0, frame) - 0.00575).abs() < 1e-9);
        assert!((shift_drag_base(10.0, 600.0, frame) - 0.02).abs() < 1e-9);
        assert!((shift_drag_base(60.0, 600.0, frame) - 0.115).abs() < 1e-9);
        // the whole Timeline in view sweeps, but still slower than 1:1
        assert!((shift_drag_base(600.0, 600.0, frame) - 0.75).abs() < 1e-9);
        assert!(shift_drag_base(30.0, 600.0, frame) > 0.02 && shift_drag_base(30.0, 600.0, frame) < 0.115);
        // one frame of a 15 second project: still the frame anchor, never below it
        assert!((shift_drag_base(1.0 / 24.0, 15.0, frame) - 0.00025).abs() < 1e-9);
        // a slower Alternate Drag Speed setting behaves proportionally at the site calling this
        assert!(shift_drag_base(frame, 600.0, frame) < shift_drag_base(10.0, 600.0, frame) * 3.0);
        // hostile input falls back to a middle value, nothing panics or goes extreme
        let fallback = shift_drag_base(f64::NAN, 600.0, frame);
        assert!(fallback.is_finite());
        assert_eq!(shift_drag_base(10.0, f64::NAN, frame), shift_drag_base(f64::NAN, 600.0, frame));
        assert!(shift_drag_base(2400.0, 600.0, frame).is_finite());
        assert_eq!(shift_drag_base(5.0, 600.0, f64::NAN), shift_drag_base(5.0, 600.0, frame));
    }

    #[test]
    fn the_thumb_is_its_smallest_exactly_at_the_maximum_zoom_level() {
        use super::{MIN_THUMB_W, timeline_extent, zoom_thumb};
        let bar = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(800.0, 13.0));
        let view_w = 800.0f32;
        // the slider's smallest state is reached at the Maximum Zoom Level (the setting) and only
        // there: one step wider and it is already bigger, whatever the Timeline's length
        for seq in [600.0, 3600.0, 36000.0, 86400.0] {
            for min_view in [0.05, 0.1, 0.4, 0.8, 3.0] {
                let at = |secs: f64| zoom_thumb(bar, view_w, seq, 0.0, f64::from(view_w) / secs, min_view).1;
                let w = at(min_view);
                assert!((w - MIN_THUMB_W).abs() < 0.5, "seq {seq} min {min_view}: {w}");
                let just_out = at(min_view * 1.2);
                assert!(just_out > MIN_THUMB_W + 0.5, "seq {seq} min {min_view}: {just_out}");
                assert!(at(min_view * 4.0) > just_out, "seq {seq} min {min_view}");
                // the whole Timeline in view fills the bar
                assert!((at(timeline_extent(seq)) - bar.width()).abs() < 0.5, "seq {seq}");
            }
        }
        // hostile zooms stay inside the bar, nothing panics
        for pps in [0.0, -3.0, f64::NAN, 1e-30, 1e30] {
            let (_, w, _) = zoom_thumb(bar, view_w, 3600.0, 0.0, pps, 0.4);
            assert!(w.is_finite() && w >= MIN_THUMB_W.min(bar.width()) && w <= bar.width(), "{pps}: {w}");
        }
    }
}

#[cfg(test)]
mod color_picker_tests {
    use super::closest_six;

    #[test]
    fn hostile_hex_input_never_panics() {
        // clearing the hex field panicked here once: "index out of bounds: the len is 0 but the
        // index is 0" -- no digits at all must be a plain fallback, never a crash
        assert_eq!(closest_six(""), "#000000");
        // three digits expand the CSS way, longer ones are cut to six, shorter ones filled out
        assert_eq!(closest_six("abc"), "#aabbcc");
        assert_eq!(closest_six("1122334455"), "#112233");
        assert_eq!(closest_six("74b0ff"), "#74b0ff");
        assert_eq!(closest_six("AB"), "#ababab");
    }
}

// the zoom bar's gestures are one state machine (modes, cursor pin, settings popups, minimap);
// splitting it would separate state that must stay in sync within a single frame
#[allow(clippy::cognitive_complexity)]
pub(super) fn zoom_scrollbar(app: &mut FilmcraftApp, ui: &mut egui::Ui, bar: Rect, view_w: f32, seq: &Sequence, frame_seconds: f64, t: &Tokens) {
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
    if app.session.state.timeline_ui.slider_minimap {
        // One class per x column: 0 empty, 1 the lower-priority kind, 2 the higher, 3 offline
        // media (always on top, whatever Prioritise Audio says). The covered kind's rectangle
        // simply ends where the winner begins. Drawn after the thumb (slightly transparent), so
        // clips never disappear behind the slider.
        let cols = bar.width().max(1.0).ceil() as usize;
        let mut class = vec![0u8; cols];
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
                    for cell in class.iter_mut().take(c1).skip(c0) {
                        *cell = (*cell).max(top);
                    }
                }
            }
        }
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
            let xa = x0 + inset;
            let xb = x1 - inset;
            // a cap edge never sits ON the pill's boundary: the outline stroke is half outside
            // its path and the fill antialiases outward -- `inset + px` is what makes the two
            // modes land their edges on the SAME x (the stroke's outer edge == the fill's edge)
            let ins = inset + px;
            let mid = (top + bot) / 2.0;
            // the semicircle begins 2 device pixels right of where it did, so its leftmost point
            // can never poke through the slider's rounded outline (and the right one 2 px left)
            let sh = 2.0 * px;
            let cap_r = ((bot - top) / 2.0).min((xb - (bar.min.x + ins + sh)).max(2.0 * px));
            // the right cap is the left one mirrored exactly: its centre sits the same distance
            // in from the bar's right edge and the curve bends the other way
            let cap_rr = ((bot - top) / 2.0).min(((bar.max.x - ins - sh) - x0).max(2.0 * px));
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
        for &(x0, x1, k) in &runs {
            // "very near the beginning" / "very near the end": within the pill's cap zones
            let hug_left = x0 - bar.min.x < rad;
            let hug_right = bar.max.x - x1 < rad;
            let col = color_for(k);
            if fill || x1 - x0 < 5.0 * px {
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
    // hovering an end circle reads as "click me": a link-style pointing cursor on the circles
    // only (never the slider's middle), and it stays through the press and the WHOLE edge drag --
    // the slider can run away from the cursor while it zooms. Shift drags hide the cursor (it is
    // pinned), so it stays hidden there.
    app.tl.zoombar_point_cursor = app.tl.zoombar_cursor_lock.is_none() && (on_circle || edge_grab);
    if app.tl.zoombar_point_cursor {
        ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
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
    let right_click = ui.input(|input| {
        input.pointer.button_clicked(egui::PointerButton::Secondary).then(|| input.pointer.interact_pos()).flatten().filter(|pos| bar.contains(*pos))
    });
    let screen = ui.ctx().content_rect();
    let popup_width = 274.0;
    if let Some(pos) = right_click {
        let space_above = pos.y - screen.top();
        let space_below = screen.bottom() - pos.y;
        let above = space_above > space_below;
        ui.data_mut(|data| data.insert_temp(anchor_id, (pos, above)));
        // the menu opens in its default state, never with a flyout or colour picker left over
        zoombar_state_reset(ui.ctx());
        // the two settings menus are mutually exclusive: this one takes the sliders' with it
        app.tl.track_slider_settings_pos = None;
    }
    let (requested_pos, above) =
        ui.data(|data| data.get_temp::<(Pos2, bool)>(anchor_id)).unwrap_or_else(|| (resp.interact_pointer_pos().unwrap_or(bar.min), false));
    let popup_pos = pos2(
        (if requested_pos.x + popup_width <= screen.right() { requested_pos.x } else { requested_pos.x - popup_width })
            .clamp(screen.left(), (screen.right() - popup_width).max(screen.left())),
        requested_pos.y.clamp(screen.top(), screen.bottom()),
    );
    let popup_open = if right_click.is_some() {
        Some(egui::SetOpenCommand::Bool(true))
    } else if resp.clicked() || resp.drag_started() {
        // a drag anywhere on the bar (the middle or either edge) takes the settings menu with it
        Some(egui::SetOpenCommand::Bool(false))
    } else {
        None
    };
    let ctx = ui.ctx().clone();
    let popup_response = egui::Popup::menu(&resp)
        .open_memory(popup_open)
        .close_behavior(egui::PopupCloseBehavior::IgnoreClicks)
        .at_position(popup_pos)
        .align(if above { egui::RectAlign::TOP_START } else { egui::RectAlign::BOTTOM_START })
        .align_alternatives(&[if above { egui::RectAlign::TOP_START } else { egui::RectAlign::BOTTOM_START }])
        .show(|ui| zoombar_settings(app, ui));
    if let Some(click_pos) = ui.input(|input| input.pointer.button_clicked(egui::PointerButton::Primary).then(|| input.pointer.interact_pos()).flatten())
        && popup_response.as_ref().is_some_and(|popup| !popup.response.rect.contains(click_pos))
        // a click on one of the settings' own popups (the Minimap flyout, a colour picker) is not
        // an outside click either -- their pills must not close the menu they belong to
        && !family_contains(ui, click_pos)
    {
        egui::Popup::close_id(&ctx, popup_id);
    }
    let range = (total - vis).max(0.0);
    let triple = resp.triple_clicked();
    let double = resp.double_clicked();
    if triple {
        let side = if app.session.state.timeline_ui.playhead_side_seconds.is_finite() {
            app.session.state.timeline_ui.playhead_side_seconds.clamp(0.1, 3600.0)
        } else {
            10.0
        };
        let span = side * 2.0;
        let fit_pps = (view_w.max(1.0) as f64 / span).clamp(0.05, max_pps(view_w, min_view));
        let playhead = app.session.playhead().seconds();
        let target_scroll = (playhead - side).clamp(0.0, (timeline_extent(seq_seconds) - span).max(0.0));
        let v = &mut app.ui.timeline;
        v.target_pps = fit_pps;
        v.target_scroll = target_scroll;
        if !app.session.state.timeline_ui.smooth_animations {
            v.pps = fit_pps;
            v.scroll = target_scroll;
        }
        app.tl.zoom_anchor = None;
    } else if double {
        let v = &mut app.ui.timeline;
        let fit_pps = (view_w.max(1.0) as f64 / timeline_extent(seq_seconds).max(1e-9)).clamp(0.05, max_pps(view_w, min_view));
        v.target_pps = fit_pps;
        v.target_scroll = 0.0;
        if !app.session.state.timeline_ui.smooth_animations {
            v.pps = fit_pps;
            v.scroll = 0.0;
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
    if resp.drag_started()
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
        // a fresh edge zoom is a fresh gravity action: re-anchor Playhead Centering on where the
        // playhead sits in the view right now (or let it go when it is off screen -- zooming with
        // the playhead off screen must never snap to it)
        if mode != 0 && app.session.state.timeline_ui.playhead_centering {
            let ph = app.session.playhead().seconds();
            let vis = (view_w.max(1.0) as f64 / app.ui.timeline.pps.max(0.05)).max(1e-6);
            let sc = app.ui.timeline.scroll;
            if ph.is_finite() && ph >= sc && ph <= sc + vis {
                app.tl.playhead_captured = true;
                app.tl.playhead_capture_vis = vis;
                app.tl.playhead_capture_u = ((ph - sc) / vis).clamp(0.0, 1.0);
            } else {
                app.tl.playhead_captured = false;
                app.tl.playhead_capture_vis = 0.0;
                app.tl.playhead_capture_u = 0.5;
            }
        }
    }
    if matches!(app.tl.drag, Some(Drag::ZoomBar { .. }))
        && ui.input(|i| i.key_pressed(egui::Key::Escape))
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
        let shift = ui.input(|i| i.modifiers.shift);
        // While Shift is held in a zoom-bar drag the OS cursor is made invisible with a fully
        // transparent 1x1 cursor bitmap, confined to the app window, and PINNED to the exact
        // position where Shift went down: every frame it is warped back there, so it never moves
        // while Shift is held, never hits a screen or window edge, and reappears on release at the
        // very pixel where it vanished. The invisible cursor plus confinement exist too -- but
        // winit's own hide must NOT be used, which clips a grabbed + hidden cursor to a one-pixel
        // rect at the window's center on Windows (winit window_state.rs refresh_os_cursor) and
        // freezes it. A transparent bitmap avoids that HIDDEN flag entirely, so confinement works.
        //
        // While pinned, the pointer's frame delta is measured by hand (current position minus the
        // pin position) instead of `drag_delta()`, because the per-frame warps confuse egui's
        // pointer position history.
        let mut shift_dx = 0.0f64;
        if shift && let Some(origin) = ui.input(|i| i.pointer.hover_pos().or(i.pointer.latest_pos())) {
            if app.tl.zoombar_cursor_lock.is_none() {
                app.tl.zoombar_cursor_lock = Some(origin);
                ui.ctx().send_viewport_cmd(egui::ViewportCommand::CursorGrab(egui::viewport::CursorGrab::Confined));
            }
            let pinned = app.tl.zoombar_cursor_lock.unwrap_or(origin);
            let now = ui.input(|i| i.pointer.latest_pos().unwrap_or(pinned));
            shift_dx = f64::from(now.x - pinned.x);
            ui.ctx().send_viewport_cmd(egui::ViewportCommand::CursorPosition(pinned));
            ui.ctx().set_cursor_image(Some(transparent_cursor()));
        } else if let Some(_origin) = app.tl.zoombar_cursor_lock.take() {
            app.tl.zoombar_unlock_catchup = true;
            release_zoombar_cursor(ui.ctx());
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
        let dx = moved * base;
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
                let vis = view_w.max(1.0) as f64 / v.pps.max(0.05);
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
                zoom_about_center(v, view_w, total, view_w.max(1.0) as f64 / new_vis, min_view);
                app.tl.zoom_anchor = None;
                // Playhead Centering: while a zoom drags the playhead into the visible range the
                // view is caught ("gravity"). ONE smooth action afterwards: where the playhead sits
                // in the view eases from where it was caught toward the middle as the view
                // collapses (smoothstep, so it neither starts nor ends with a jolt, fully centered
                // once the visible range has roughly halved). The pull is a function of the zoom
                // itself, so it can never stall at the edge and then snap in a few frames. Once
                // caught the playhead never leaves the screen. Grabbing the slider's middle pulls
                // it off manually (mode 0).
                if app.session.state.timeline_ui.playhead_centering {
                    let vis = view_w.max(1.0) as f64 / v.pps.max(0.05);
                    let range = (total - vis).max(0.0);
                    // a capture left over from an earlier gesture whose playhead some other edit
                    // has since moved off screen is stale: drop it rather than ever yank the view
                    // across the timeline to the playhead
                    if app.tl.playhead_captured && !was_in_view {
                        app.tl.playhead_captured = false;
                        app.tl.playhead_capture_vis = 0.0;
                        app.tl.playhead_capture_u = 0.5;
                    }
                    if !app.tl.playhead_captured && was_in_view {
                        app.tl.playhead_captured = true;
                        app.tl.playhead_capture_vis = vis;
                        app.tl.playhead_capture_u = ((ph - v.scroll) / vis.max(1e-9)).clamp(0.0, 1.0);
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
        // changes nothing here: its slowdown belongs to the drag itself ([`shift_drag_base`]).
        if mode == 0 {
            let wheel = ui.input(|i| i.smooth_scroll_delta.y);
            let mut factor = (wheel as f64 / 600.0).clamp(-0.30, 0.30);
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
        app.tl.zoombar_unlock_catchup = false;
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
fn release_zoombar_cursor(ctx: &egui::Context) {
    ctx.set_cursor_image(None);
    ctx.send_viewport_cmd(egui::ViewportCommand::CursorGrab(egui::viewport::CursorGrab::None));
}

/// Close the zoom bar's own settings popup (the one opened by right-clicking the bar). The two
/// sliders' settings menus are mutually exclusive: opening one takes the other with it.
pub(super) fn close_zoombar_popup(ctx: &egui::Context) {
    egui::Popup::close_id(ctx, egui::Id::unique("tl-zoombar").with("popup"));
}

/// The window that comes up when a slider is hidden: it only tells the user how to bring it
/// back, OK dismisses it. Drawn from the Timeline itself rather than the bar, so it still shows
/// once the slider is gone. One for the zoom scroll bar, one for the side track sliders.
pub fn zoombar_hide_dialog(app: &mut FilmcraftApp, ctx: &egui::Context) {
    hide_dialog(app, ctx, false);
    hide_dialog(app, ctx, true);
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
        if matches!(app.tl.drag, Some(Drag::ZoomBar { .. })) {
            app.tl.drag = None;
        }
        app.tl.zoom_anchor = None;
        app.tl.zoombar_cursor_lock = None;
        app.tl.zoombar_unlock_catchup = false;
        app.tl.zoombar_point_cursor = false;
        release_zoombar_cursor(ctx);
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
pub fn zoombar_cursor_watchdog(tl: &mut super::TlState, ctx: &egui::Context) {
    // the pin is legitimate during a zoom-bar Shift drag or a track slider's (which pins the
    // same cursor to its own press pixel)
    let live_drag = matches!(tl.drag, Some(Drag::ZoomBar { .. })) || tl.track_slider_dragging;
    let still =
        tl.zoombar_cursor_lock.is_some() && live_drag && ctx.input(|i| i.focused && i.pointer.button_down(egui::PointerButton::Primary) && i.modifiers.shift);
    if !still && let Some(_origin) = tl.zoombar_cursor_lock.take() {
        tl.zoombar_unlock_catchup = true;
        release_zoombar_cursor(ctx);
    }
}

/// The side (track) sliders: the zoom bar's exact look turned on its end -- a rounded pill
/// track, a pill thumb, and the two ringed end circles (a dark disc with a white ring around it;
/// hovering an end brings the rings in 1.5x, pressing or dragging 2.25x). The middle drags the
/// scroll 1:1, a click beside the thumb jumps there, and dragging an end circle (or the edge
/// zone under it) zooms the tracks' height: an outward pull on EITHER end (top up, bottom down)
/// grows the thumb (shorter tracks), an inward pull shrinks it, always about the view's CENTER.
/// Double-click copies
/// the thumb's length and position to the other slider, Shift+double-click fits every track.
/// Shift drags run at a fraction of the ordinary speed ([`track_drag_base`] times the Alternate
/// Drag Speed setting). Right-click opens the settings popup, to the left of the sliders. Returns
/// (pointer on an end circle, the thumb's rect for automation ids, 1 = double-click,
/// 2 = Shift+double-click).
// one parameter per piece of slider state keeps the two sliders (video/audio) symmetric at
// their call sites; the drag logic is one state machine like the zoom bar's
#[allow(clippy::type_complexity, clippy::too_many_arguments, clippy::cognitive_complexity)]
pub(super) fn vertical_scrollbar(
    ui: &mut egui::Ui,
    tl: &mut super::TlState,
    bar: Rect,
    value: &mut f32,
    max: f32,
    window_h: f32,
    // how many track slots the content has (the rows plus the empty slot for a new track): the
    // thumb and the zoom maths need it, and it cannot be recovered from `max` (which is 0
    // whenever everything fits)
    slots: f32,
    track_h: &mut f32,
    invert: bool,
    smooth: bool,
    inverted: bool,
    decouple: bool,
    shift_speed: f32,
    t: &Tokens,
    id: &str,
) -> (bool, Rect, u8) {
    if bar.height() < 2.0 || bar.width() < 6.0 {
        return (false, bar, 0);
    }
    /// The thumb's shortest length: the exact same minimum as the zoom bar's thumb
    /// ([`MIN_THUMB_W`]); the two end circles fit inside it.
    const THUMB_MIN: f32 = MIN_THUMB_W;
    let p = ui.painter();
    p.rect_filled(bar, bar.width() / 2.0, t.separator);
    // with nothing to scroll the thumb fills the track -- the slider is always there, it never
    // just disappears at the ends of the A/V divider's range
    let h = track_thumb(*track_h, bar.height()).max(bar.width() + 4.0);
    let travel = (bar.height() - h).max(1.0);
    // with nothing to scroll the thumb sits in the MIDDLE of the track: pinned to an end it
    // could never collapse about its middle (the dragged edge then barely moved while the other
    // one swung the whole way)
    let pos = if max > 0.5 { (if invert { 1.0 - *value / max } else { *value / max }).clamp(0.0, 1.0) } else { 0.5 };
    let y = bar.min.y + travel * pos;
    let thumb = Rect::from_min_size(pos2(bar.min.x, y), vec2(bar.width(), h));
    let resp = ui.interact(bar, egui::Id::unique(("tl-vs", id)), Sense::click_and_drag());
    let hover = ui.rect_contains_pointer(thumb) || resp.dragged();
    p.rect_filled(thumb, thumb.width() / 2.0, if hover { t.scroll_thumb_hover } else { t.scroll_thumb });
    const CIRCLE: f32 = 7.0;
    const RING_OUT: f32 = 6.0;
    const ZONE: f32 = 12.0;
    let cx = thumb.center().x;
    let wi = thumb.width();
    let circle_t = thumb.min.y + wi / 2.0;
    let circle_b = thumb.max.y - wi / 2.0;
    // what the press grabbed (end circle / edge = zoom, middle = scroll), remembered for the drag
    let mode_key = egui::Id::unique(("tl-vs-mode", id));
    if resp.drag_started() {
        let mode = if let Some(q) = ui.input(|i| i.pointer.press_origin()).or(resp.interact_pointer_pos()) {
            if (q.y - circle_t).abs() <= CIRCLE && (q.x - cx).abs() <= CIRCLE {
                1u8
            } else if (q.y - circle_b).abs() <= CIRCLE && (q.x - cx).abs() <= CIRCLE {
                2u8
            } else if q.y < thumb.min.y + ZONE {
                1u8
            } else if q.y > thumb.max.y - ZONE {
                2u8
            } else {
                0u8
            }
        } else {
            0u8
        };
        ui.data_mut(|d| d.insert_temp(mode_key, mode));
    }
    let mode: u8 = ui.data(|d| d.get_temp(mode_key)).unwrap_or(0);
    if resp.drag_stopped() {
        ui.data_mut(|d| d.remove_temp::<u8>(mode_key));
    }
    let hp = resp.hover_pos().or_else(|| resp.interact_pointer_pos());
    // while the MIDDLE is dragged the end circles ignore the pointer entirely (the pinned
    // cursor sits on top of them and used to light their rings up)
    let middle_drag = resp.dragged() && mode == 0;
    let on_circle = !middle_drag
        && hp.is_some_and(|q| {
            ((q.y - circle_t).abs() <= CIRCLE && (q.x - cx).abs() <= CIRCLE) || ((q.y - circle_b).abs() <= CIRCLE && (q.x - cx).abs() <= CIRCLE)
        });
    let ring_target = if middle_drag {
        1.5
    } else if resp.dragged() && (mode == 1 || mode == 2) || (on_circle && ui.input(|i| i.pointer.primary_down())) {
        3.375
    } else if on_circle || hp.is_some_and(|q| thumb.expand2(vec2(4.0, 0.0)).contains(q) && (q.y < thumb.min.y + wi || q.y > thumb.max.y - wi)) {
        2.25
    } else {
        1.5
    };
    // Smooth Animations (this slider's own) eases between the ring states exactly as the zoom
    // bar does; each slider keeps its own eased width so the two can never fight over one
    let ri = if id == "vs" { 0 } else { 1 };
    let ring_w = if smooth {
        let k = (ui.input(|i| i.stable_dt).min(0.05) / 0.05).min(1.0);
        let w = tl.track_ring_w[ri] + (ring_target - tl.track_ring_w[ri]) * k;
        tl.track_ring_w[ri] = w;
        if (w - ring_target).abs() > 0.01 {
            ui.ctx().request_repaint();
        }
        w
    } else {
        tl.track_ring_w[ri] = ring_target;
        ring_target
    };
    for cy2 in [circle_t, circle_b] {
        p.circle_filled(pos2(cx, cy2), RING_OUT, t.icon_active);
        p.circle_filled(pos2(cx, cy2), RING_OUT - ring_w, t.panel_bg);
    }
    // an end circle reads as "click me": a link-style pointing cursor there (the caller ORs the
    // answer into the zoom bar's flag, which wins over the tracks' grab cursors later in the
    // frame). It stays a pointer for the WHOLE edge drag -- the slider can run away from the
    // cursor and the middle is never affected
    let point = on_circle || (resp.dragged() && matches!(mode, 1 | 2));
    if point {
        ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
    }
    if resp.dragged() {
        tl.track_slider_dragging = true;
        // a drag writes straight through: no Smooth Animations target may fight it
        tl.track_anim = None;
        // Shift+drag pins the cursor to its press pixel and hides it -- the zoom bar's proven
        // recipe, adapted to the vertical axis: a fully transparent 1x1 cursor image instead of
        // winit's hidden flag (which clips a grabbed + hidden cursor to the window's center), a
        // Confined grab, and a warp back to the pin every frame; the motion is measured by hand
        // against the pin. Without Shift the drag is ordinary pointer motion.
        let shift = ui.input(|i| i.modifiers.shift);
        let mut pinned_dy = 0.0f64;
        if shift && let Some(origin) = ui.input(|i| i.pointer.hover_pos().or(i.pointer.latest_pos())) {
            if tl.zoombar_cursor_lock.is_none() {
                tl.zoombar_cursor_lock = Some(origin);
                ui.ctx().send_viewport_cmd(egui::ViewportCommand::CursorGrab(egui::viewport::CursorGrab::Confined));
            }
            let pinned = tl.zoombar_cursor_lock.unwrap_or(origin);
            let now = ui.input(|i| i.pointer.latest_pos().unwrap_or(pinned));
            pinned_dy = f64::from(now.y - pinned.y);
            ui.ctx().send_viewport_cmd(egui::ViewportCommand::CursorPosition(pinned));
            ui.ctx().set_cursor_image(Some(transparent_cursor()));
        } else if tl.zoombar_cursor_lock.is_some() && !matches!(tl.drag, Some(Drag::ZoomBar { .. })) {
            tl.zoombar_unlock_catchup = true;
            release_zoombar_cursor(ui.ctx());
            tl.zoombar_cursor_lock = None;
        }
        let plain_dy = resp.drag_delta().y;
        let moved = if tl.zoombar_cursor_lock.is_some() {
            pinned_dy
        } else if plain_dy.is_finite() && !tl.zoombar_unlock_catchup {
            f64::from(plain_dy)
        } else {
            0.0
        };
        if tl.zoombar_unlock_catchup {
            // one frame of no movement: the final warp back to the pin settles here
            tl.zoombar_unlock_catchup = false;
        }
        // Alternate Drag Speed: a Shift drag runs at [`track_drag_base`] (a fraction of the
        // ordinary motion) times the setting
        let base = if shift && tl.zoombar_cursor_lock.is_some() {
            track_drag_base(if max > 0.5 { f64::from(window_h / (window_h + max)) } else { 1.0 }) * f64::from(shift_speed.max(0.01))
        } else {
            1.0
        };
        let dy = (moved * base) as f32;
        match mode {
            // an end circle or the edge zone under it: zoom the tracks' height. The dragged edge
            // follows the mouse 1:1 and the other edge mirrors it, so the thumb collapses/grows
            // about its own middle (never toward the dragged circle). The motion is linear in the
            // THUMB's length -- linear in the track height felt exponential -- and hard-stops at
            // the ends of the range: once the thumb is at its smallest the drag does nothing at
            // all (no drift, no scroll, nothing).
            1 | 2 => {
                let bar_h = bar.height();
                let slots = slots.max(1.0);
                // The DRAWN thumb is the source of truth and the height is its log-meter
                // inverse ([`track_height_of`]): a bijection, so the drag can neither explode
                // nor compound over its frames.
                let height_of = |len: f32| track_height_of(len, bar_h);
                let lo = THUMB_MIN;
                let hi = bar_h;
                // With nothing to scroll the thumb is redrawn at the middle of the bar every
                // frame: a decoupled "the other edge stays put" model cannot survive that, so
                // the drag uses the mirrored-middle geometry there
                let edge = decouple && max > 0.5;
                // geometry is in drawn-thumb points (the thumb cannot leave its track)
                let top_of = |len: f32| {
                    let len = len.clamp(THUMB_MIN, bar_h);
                    if edge { if mode == 1 { y + h - len } else { y } } else { y + h / 2.0 - len / 2.0 }
                };
                let out = if mode == 1 { -dy } else { dy };
                let new_len = (h + if edge { out } else { 2.0 * out }).clamp(lo, hi);
                if (new_len - h).abs() > f32::EPSILON {
                    let len = new_len;
                    let new_h = height_of(len);
                    let max_new = (slots * new_h - window_h).max(0.0);
                    let top = top_of(len);
                    let travel2 = (bar_h - len.clamp(THUMB_MIN, bar_h)).max(1.0);
                    let p2 = ((top - bar.min.y) / travel2).clamp(0.0, 1.0);
                    *value = if invert { (1.0 - p2) * max_new } else { p2 * max_new };
                    *track_h = new_h;
                }
            }
            // the middle: 1:1 scroll, as the zoom bar's middle (Inverted controls never touches
            // it). The wheel while the middle is held zooms the tracks' height about the thumb's
            // middle, like the end circles -- the other way with Inverted controls.
            _ => {
                if max > 0.5 {
                    let d = dy / travel * max;
                    *value = (*value + if invert { -d } else { d }).clamp(0.0, max);
                }
                let mut wheel = ui.input(|i| i.smooth_scroll_delta.y);
                if inverted {
                    wheel = -wheel;
                }
                let factor = (wheel / 600.0).clamp(-0.30, 0.30);
                if wheel.is_finite() && factor != 0.0 {
                    let bar_h = bar.height();
                    let slots = slots.max(1.0);
                    let new_h = (*track_h * (1.0 + factor)).clamp(22.0, 220.0);
                    let glen = track_thumb(new_h, bar_h).clamp(THUMB_MIN, bar_h);
                    let max_new = (slots * new_h - window_h).max(0.0);
                    let mid_y = y + h / 2.0;
                    let travel2 = (bar_h - glen).max(1.0);
                    let p2 = ((mid_y - glen / 2.0 - bar.min.y) / travel2).clamp(0.0, 1.0);
                    *value = if invert { (1.0 - p2) * max_new } else { p2 * max_new };
                    *track_h = new_h;
                    tl.track_anim = None;
                }
            }
        }
    }
    // double-click copies this thumb's length and position to the other slider; Shift+double-
    // click fits every track; Ctrl+Shift+double-click fits BOTH sliders. Either way the single
    // click beneath them is not a jump.
    let mut clicks = 0u8;
    if resp.double_clicked() {
        clicks = if ui.input(|i| i.modifiers.shift && i.modifiers.command) {
            3
        } else if ui.input(|i| i.modifiers.shift) {
            2
        } else {
            1
        };
    }
    // a click beside the thumb jumps the view there, as on the zoom bar
    if clicks == 0
        && resp.clicked()
        && let Some(q) = resp.interact_pointer_pos()
        && !thumb.contains(q)
    {
        tl.track_anim = None;
        let f = ((q.y - bar.min.y - h / 2.0) / travel).clamp(0.0, 1.0);
        *value = if invert { (1.0 - f) * max } else { f * max };
    }
    // right-click: the settings popup, always to the LEFT of the sliders (their alternate
    // position comes later), at the pointer's height
    if ui.rect_contains_pointer(bar) && ui.input(|i| i.pointer.button_clicked(egui::PointerButton::Secondary)) {
        let y = ui.input(|i| i.pointer.interact_pos()).map(|p| p.y).unwrap_or(bar.center().y);
        // to the LEFT of the sliders (their alternate position is the left edge of the Timeline,
        // where that would run off the screen -- then it opens to their right instead)
        let x = if bar.min.x - 210.0 < ui.ctx().content_rect().left() { bar.max.x + 4.0 } else { bar.min.x - 210.0 };
        tl.track_slider_settings_pos = Some(pos2(x, y));
        // the two settings menus are mutually exclusive: this one takes the zoom bar's with it
        tl.zoombar_settings_pos = None;
        close_zoombar_popup(ui.ctx());
    }
    (point, thumb, clicks)
}

/// The track sliders' settings popup content. Only Hide Slider does anything yet; the rows below
/// it are stored but do not drive the sliders.
pub(super) fn track_slider_settings(app: &mut FilmcraftApp, ui: &mut egui::Ui) {
    /// 20% narrower than the zoom bar's settings popup (274 pt of frame).
    const CONTENT_W: f32 = 193.38;
    ui.set_max_width(CONTENT_W + 16.0);
    ui.set_min_width(CONTENT_W + 16.0);
    let t = app.tokens;
    // every row spans the popup's full width and centers its content in it: rows of a fixed
    // narrower width left an uneven gap on the right of the Hide Slider row
    let row_w = ui.available_width();
    let hidden = app.session.state.timeline_ui.track_slider_hidden;
    // the top row: Hide Slider (red) while the sliders show, Unhide Slider (blue) while hidden
    let (r, _) = ui.allocate_exact_size(egui::vec2(row_w, 30.0), Sense::click());
    let hide_resp = ui.interact(r, egui::Id::unique(if hidden { "timeline-track-slider-unhide" } else { "timeline-track-slider-hide" }), Sense::click());
    if hide_resp.hovered() {
        ui.painter().rect_filled(r, 4.0, app.tokens.switch_border.gamma_multiply(0.35));
    }
    let (label, color) = if hidden { (tl!("Unhide Slider"), app.tokens.accent) } else { (tl!("Hide Slider"), Color32::from_rgb(0xf8, 0x7e, 0x7a)) };
    ui.painter().text(r.center(), Align2::CENTER_CENTER, label, Tokens::ui(11.5), color);
    app.auto.add(if hidden { "timeline.trackSlider.settings.unhideSlider" } else { "timeline.trackSlider.settings.hideSlider" }, r, label);
    if hide_resp.clicked() {
        set_track_slider_hidden(app, ui.ctx(), !hidden);
        if !hidden {
            // just hidden: the "how to bring it back" window follows
            app.tl.track_hide_dialog_open = true;
        }
        app.tl.track_slider_settings_pos = None;
        ui.ctx().request_repaint();
    }
    // Smooth Animations eases the double-click moves (which land on the OTHER slider);
    // Alternate Slider Position moves the sliders to the left edge of the Timeline.
    let smooth = app.session.state.timeline_ui.track_slider_smooth;
    let c = settings_pill_row(ui, &t, row_w, tl!("Smooth Animations"), egui::Id::unique("timeline-track-slider-smooth"), smooth);
    app.auto.add("timeline.trackSlider.settings.smoothAnimations", c.rect, "Smooth Animations");
    if c.clicked() {
        app.session.state.timeline_ui.track_slider_smooth = !smooth;
        app.session.project_view_changed();
    }
    // Inverted Controls: the very same switch as the Timeline slider's menu
    let inv = app.session.state.timeline_ui.inverted_controls;
    let c = settings_pill_row(ui, &t, row_w, tl!("Inverted Controls"), egui::Id::unique("timeline-track-slider-inverted"), inv);
    app.auto.add("timeline.trackSlider.settings.invertedControls", c.rect, "Inverted Controls");
    if c.clicked() {
        app.session.state.timeline_ui.inverted_controls = !inv;
        app.session.project_view_changed();
    }
    // Decouple Edges: an end circle drag moves only that end, the other one stays put
    let dec = app.session.state.timeline_ui.track_slider_decouple;
    let c = settings_pill_row(ui, &t, row_w, tl!("Decouple Edges"), egui::Id::unique("timeline-track-slider-decouple"), dec);
    app.auto.add("timeline.trackSlider.settings.decoupleEdges", c.rect, "Decouple Edges");
    if c.clicked() {
        app.session.state.timeline_ui.track_slider_decouple = !dec;
        app.session.project_view_changed();
    }
    let alt = app.session.state.timeline_ui.track_slider_alternate_position;
    let c = settings_pill_row(ui, &t, row_w, tl!("Alternate Slider Position"), egui::Id::unique("timeline-track-slider-alt"), alt);
    app.auto.add("timeline.trackSlider.settings.alternateSliderPosition", c.rect, "Alternate Slider Position");
    if c.clicked() {
        app.session.state.timeline_ui.track_slider_alternate_position = !alt;
        app.session.project_view_changed();
    }
    // the white divider, then Alternate Drag Speed: Slow (0.5x), Default (1x) or Custom
    ui.separator();
    ui.add_space(2.0);
    let speed = app.session.state.timeline_ui.track_slider_shift_drag_speed;
    // once the Value has been typed the tick slider stays on Custom, even when the number
    // happens to match a preset (the main slider's `shift_drag_custom` the same way)
    let custom = app.session.state.timeline_ui.track_slider_shift_drag_custom;
    let mut idx = if custom {
        2
    } else if (speed - 0.5).abs() < 0.01 {
        0
    } else if (speed - 1.0).abs() < 0.01 {
        1
    } else {
        2
    };
    settings_heading(ui, &t, tl!("Alternate Drag Speed"));
    let (resp, ch) = tick_slider(ui, &mut idx, &[tl!("Slow"), tl!("Default"), tl!("Custom")], &t);
    app.auto.add("timeline.trackSlider.settings.shiftDragSpeed", resp.rect, "Alternate Drag Speed");
    if ch {
        match idx {
            0..=1 => {
                app.session.state.timeline_ui.track_slider_shift_drag_speed = if idx == 0 { 0.5 } else { 1.0 };
                app.session.state.timeline_ui.track_slider_shift_drag_custom = false;
            }
            _ => {
                // Custom again: restore the last custom value, never reset it to a preset
                if !custom {
                    app.session.state.timeline_ui.track_slider_shift_drag_speed =
                        last_custom_value(app.session.state.timeline_ui.track_slider_shift_drag_last_custom, 10.0);
                }
                app.session.state.timeline_ui.track_slider_shift_drag_custom = true;
            }
        }
        app.session.project_view_changed();
    }
    // the Custom value row: 0.1x-10x
    ui.allocate_ui_with_layout(egui::vec2(row_w, 24.0), egui::Layout::left_to_right(egui::Align::Center).with_main_align(egui::Align::Center), |ui| {
        ui.spacing_mut().item_spacing.x = 3.0;
        ui.add_sized(egui::vec2(46.0, 18.0), egui::Label::new(tl!("Value:")).wrap());
        let (response, value) = hot_number_numeric(
            ui,
            tl!("Alternate drag speed for the track sliders"),
            egui::Id::unique("timeline-track-slider-speed"),
            app.session.state.timeline_ui.track_slider_shift_drag_speed,
            HotNumberOptions::new(0.1, (0.1, 10.0), 1, "x", &app.tokens),
        );
        app.auto.add("timeline.trackSlider.settings.shiftDragSpeedValue", response.rect, "Alternate drag speed for the track sliders");
        if let Some(value) = value {
            app.session.state.timeline_ui.track_slider_shift_drag_speed = value.clamp(0.1, 10.0);
            app.session.state.timeline_ui.track_slider_shift_drag_last_custom = value.clamp(0.1, 10.0);
            app.session.state.timeline_ui.track_slider_shift_drag_custom = true;
            app.session.project_view_changed();
        }
    });
}

/// The track sliders' settings popup opened from the Timeline's own menu ("Track Slider
/// Settings", shown only while the sliders are hidden): the same settings as right-clicking a
/// slider.
pub(super) fn track_slider_settings_popup(app: &mut FilmcraftApp, ui: &mut egui::Ui) {
    let Some(pos) = app.tl.track_slider_settings_pos else {
        return;
    };
    let screen = ui.ctx().content_rect();
    let pos = if pos.y + 300.0 > screen.bottom() { pos2(pos.x, (pos.y - 300.0).max(screen.top())) } else { pos };
    let panel =
        egui::Area::new(egui::Id::unique("tl-track-slider-settings")).order(egui::Order::Foreground).fixed_pos(pos).fade_in(false).interactable(true).show(
            ui.ctx(),
            |ui| {
                egui::Frame::popup(ui.style()).show(ui, |ui| {
                    track_slider_settings(app, ui);
                });
            },
        );
    if ui.input(|i| i.pointer.any_pressed())
        && let Some(p) = ui.input(|i| i.pointer.interact_pos())
        && !panel.response.rect.contains(p)
    {
        app.tl.track_slider_settings_pos = None;
    }
}

/// Hide or bring back the side track sliders (Hide Slider / Unhide Track Slider).
pub fn set_track_slider_hidden(app: &mut FilmcraftApp, _ctx: &egui::Context, hidden: bool) {
    if app.session.state.timeline_ui.track_slider_hidden == hidden {
        return;
    }
    app.session.state.timeline_ui.track_slider_hidden = hidden;
    app.session.project_view_changed();
    app.tl.track_slider_settings_pos = None;
    if !hidden {
        // a stale "how to bring it back" window means nothing once the sliders are back
        app.tl.track_hide_dialog_open = false;
    }
}
