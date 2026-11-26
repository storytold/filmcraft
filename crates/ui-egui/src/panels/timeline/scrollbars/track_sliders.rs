use super::slider_controls::{SHIFT_CONTENT_RATE, SHIFT_THUMB_RATE};
use super::{
    Align2, CONTROLS_FLYOUT_W, Color32, CursorIcon, Drag, FilmcraftApp, HotNumberOptions, MIN_THUMB_W, Rect, Sense, Tokens, alternate_down,
    close_zoombar_popup, controls_arrow, controls_capturing, family_contains, family_reset, gesture, hot_number_numeric, hover_flyout, last_custom_value,
    matches_gesture, pos2, release_slider_cursor, settings_heading, settings_heading_with_hint_center, settings_menu, settings_pill_row, shift_drag_rate,
    tick_slider, uses_default_gesture, vec2,
};

const TRACK_SLIDER_W: f32 = 12.0;
const TRACK_SLIDER_GAP: f32 = 2.5;

pub(in crate::panels::timeline) fn track_slider_column(app: &FilmcraftApp) -> (bool, bool, f32) {
    let hidden = app.session.state.timeline_ui.track_slider_hidden;
    let left = !hidden && app.session.state.timeline_ui.track_slider_alternate_position;
    let width = if hidden { 0.0 } else { TRACK_SLIDER_W + 2.0 * TRACK_SLIDER_GAP };
    (hidden, left, width)
}

pub(in crate::panels::timeline) fn reconcile_track_slider_settings(app: &mut FilmcraftApp) {
    if !app.session.state.timeline_ui.track_slider_decouple && app.session.state.timeline_ui.track_slider_snap {
        app.session.state.timeline_ui.track_slider_snap = false;
        app.session.project_view_changed();
    }
}

pub(in crate::panels::timeline) fn hidden_track_slider_settings_row(app: &mut FilmcraftApp, ui: &mut egui::Ui, ctx: &egui::Context) {
    if !app.session.state.timeline_ui.track_slider_hidden {
        return;
    }
    let settings = ui.button(tl!("Track Slider Settings"));
    app.auto.add("timeline.trackSliderSettings", settings.rect, "Track Slider Settings");
    if settings.clicked() {
        track_slider_state_reset(ctx);
        app.tl.track_slider_settings_pos = Some(settings.rect.min);
        // The zoom and track settings menus are mutually exclusive.
        app.tl.zoombar_settings_pos = None;
        super::close_zoombar_popup(ctx);
        ui.close();
    }
}

pub(in crate::panels::timeline) fn hidden_track_slider_unhide_row(app: &mut FilmcraftApp, ui: &mut egui::Ui, ctx: &egui::Context) {
    if !app.session.state.timeline_ui.track_slider_hidden {
        return;
    }
    let unhide = ui.button(tl!("Unhide Track Slider"));
    app.auto.add("timeline.unhideTrackSlider", unhide.rect, "Unhide Track Slider");
    if unhide.clicked() {
        set_track_slider_hidden(app, ctx, false);
        ui.close();
    }
}

pub(in crate::panels::timeline) fn track_slider_state_reset(ctx: &egui::Context) {
    let generation_key = super::settings_generation_key("timeline-track-slider");
    ctx.data_mut(|d| {
        d.insert_temp(super::controls_listen_key(), None::<usize>);
        // a fresh opening: the settings menu must start scrolled to its bottom again
        let generation = d.get_temp::<u64>(generation_key).unwrap_or(0).wrapping_add(1);
        d.insert_temp(generation_key, generation);
    });
}

/// Geometry and visibility inputs for drawing both side sliders.
pub(in crate::panels::timeline) struct TrackSliderLayout {
    pub rect: Rect,
    pub video_area: Rect,
    pub audio_area: Rect,
    pub tracks_area: Rect,
    pub split_y: f32,
    pub nv: usize,
    pub na: usize,
    pub min_v: f32,
    pub min_a: f32,
    pub bar_top: bool,
    pub side_left: bool,
    pub tracks_hidden: bool,
}

/// Draw and handle both side track sliders in the Timeline's track pane.
pub(in crate::panels::timeline) fn draw_track_sliders(app: &mut FilmcraftApp, ui: &mut egui::Ui, layout: TrackSliderLayout, t: &Tokens) -> bool {
    let TrackSliderLayout { rect, video_area, audio_area, tracks_area, split_y, nv, na, min_v, min_a, bar_top, side_left, tracks_hidden } = layout;
    app.tl.track_slider_dragging = false;
    let mut on_side = false;
    if !tracks_hidden {
        let ctx = ui.ctx().clone();
        let smooth = app.session.state.timeline_ui.track_slider_smooth;
        let inverted = app.session.state.timeline_ui.inverted_controls;
        let decouple = app.session.state.timeline_ui.track_slider_decouple;
        let shift_speed = app.session.state.timeline_ui.track_slider_shift_drag_speed as f32;
        // A few points clear of the A/V divider's black band at each end, so the line never
        // clips over the sliders' rounded caps; the column sits against the body's edge --
        // right normally, left (in front of the headers) with Alternate Slider Position.
        let (sx0, sx1) = if side_left {
            (rect.min.x - TRACK_SLIDER_GAP - TRACK_SLIDER_W, rect.min.x - TRACK_SLIDER_GAP)
        } else {
            (rect.max.x - TRACK_SLIDER_GAP - TRACK_SLIDER_W, rect.max.x - TRACK_SLIDER_GAP)
        };
        // Both sliders use the same outer inset, centered on the A/V divider.
        let outer = if bar_top { 5.0 } else { 2.0 };
        let snap_setting = app.session.state.timeline_ui.track_slider_snap && app.session.state.timeline_ui.track_slider_decouple;
        let track_bindings = {
            let s = &app.session.state.timeline_ui;
            [s.track_control_mirror.clone(), s.track_control_explode.clone(), s.track_control_alternate_drag_speed.clone(), s.track_control_settings.clone()]
        };
        // Extend both sliders outward by half the Mix strip height. The pair stays centered
        // around the A/V divider, and the audio slider reaches through the Mix/master row.
        let outer_extension = super::super::MASTER_H;
        let vbar = Rect::from_min_max(pos2(sx0, video_area.min.y + outer - outer_extension), pos2(sx1, split_y - 3.0));
        let abar = Rect::from_min_max(pos2(sx0, split_y + super::super::DIVIDER_H + 3.0), pos2(sx1, tracks_area.max.y - outer));
        ctx.data_mut(|d| {
            d.insert_temp(egui::Id::unique("timeline-track-slider-video-rect"), vbar);
            d.insert_temp(egui::Id::unique("timeline-track-slider-audio-rect"), abar);
        });
        app.auto.add("timeline.trackSlider.video.track", vbar, "Video track slider track");
        app.auto.add("timeline.trackSlider.audio.track", abar, "Audio track slider track");
        let (on_vs, vthumb, vclicks) = vertical_scrollbar(
            ui,
            &mut app.tl,
            vbar,
            &mut app.ui.timeline.v_scroll,
            video_area.height(),
            nv as f32 + 1.0 / 3.0,
            &mut app.ui.timeline.video_track_h,
            true,
            smooth,
            inverted,
            decouple,
            shift_speed,
            snap_setting,
            &track_bindings,
            t,
            "vs",
        );
        let (on_as, athumb, aclicks) = vertical_scrollbar(
            ui,
            &mut app.tl,
            abar,
            &mut app.ui.timeline.a_scroll,
            audio_area.height(),
            na as f32 + 1.0 / 3.0,
            &mut app.ui.timeline.audio_track_h,
            false,
            smooth,
            inverted,
            decouple,
            shift_speed,
            snap_setting,
            &track_bindings,
            t,
            "as",
        );
        on_side |= on_vs || on_as;
        // A Controls binding named in this frame's settings flyout fires its action once, so
        // the new input does what the built-in gesture would have done when completed.
        let (vclicks, aclicks) = match ctx.data_mut(|d| d.remove_temp::<usize>(egui::Id::unique("tl-controls-track-trigger"))) {
            Some(0) => (1u8, 0u8),
            // An Explode Slider shortcut has no pointer target, so use the video slider.
            Some(1) => (2u8, 0u8),
            Some(3) => {
                app.tl.zoombar_settings_pos = None;
                super::close_zoombar_popup(&ctx);
                track_slider_state_reset(&ctx);
                app.tl.track_slider_settings_pos = Some(vthumb.min);
                (0u8, 0u8)
            }
            _ => (vclicks, aclicks),
        };
        // Double-click copies the other slider's height/position; modified double-clicks fit.
        if vclicks != 0 || aclicks != 0 {
            let video = vclicks != 0;
            let clicks = if video { vclicks } else { aclicks };
            let val_s = if video { app.ui.timeline.v_scroll } else { app.ui.timeline.a_scroll };
            let mut tg = [app.ui.timeline.video_track_h, app.ui.timeline.v_scroll, app.ui.timeline.audio_track_h, app.ui.timeline.a_scroll];
            if clicks == 3 {
                tg = [min_v, 0.0, min_a, 0.0];
            } else if clicks == 2 {
                if video {
                    tg[0] = min_v;
                    tg[1] = 0.0;
                } else {
                    tg[2] = min_a;
                    tg[3] = 0.0;
                }
            } else {
                let source_h = if video { app.ui.timeline.video_track_h } else { app.ui.timeline.audio_track_h };
                let win_o = if video { audio_area.height() } else { video_area.height() };
                let slots = if video { na as f32 + 1.0 / 3.0 } else { nv as f32 + 1.0 / 3.0 };
                let source_min = if video { min_v } else { min_a };
                let target_min = if video { min_a } else { min_v };
                let h_new = if source_h <= source_min + 0.05 { target_min } else { source_h.max(target_min) };
                let max_new = (slots * h_new - win_o).max(0.0);
                // Mirror the visible track index rather than the scroll fraction. The panes can
                // have different heights and the video rows run in the opposite direction.
                let mirrored_scroll = val_s / source_h.max(1e-6) * h_new;
                let v_new = mirrored_scroll.clamp(0.0, max_new);
                if video {
                    tg[0] = app.ui.timeline.video_track_h;
                    tg[1] = app.ui.timeline.v_scroll;
                    tg[2] = h_new;
                    tg[3] = v_new;
                } else {
                    tg[0] = h_new;
                    tg[1] = v_new;
                    tg[2] = app.ui.timeline.audio_track_h;
                    tg[3] = app.ui.timeline.a_scroll;
                }
            }
            if smooth {
                app.tl.track_anim = Some(tg);
                ctx.request_repaint();
            } else {
                app.ui.timeline.video_track_h = tg[0];
                app.ui.timeline.v_scroll = tg[1];
                app.ui.timeline.audio_track_h = tg[2];
                app.ui.timeline.a_scroll = tg[3];
            }
        }
        for (name, thumb) in [("video", vthumb), ("audio", athumb)] {
            let cw = thumb.width();
            app.auto.add(&format!("timeline.trackSlider.{name}"), thumb, "Track slider");
            app.auto.add(
                &format!("timeline.trackSlider.{name}.topCircle"),
                Rect::from_center_size(pos2(thumb.center().x, thumb.min.y + cw / 2.0), egui::vec2(14.0, 14.0)),
                "Zoom the tracks taller",
            );
            app.auto.add(
                &format!("timeline.trackSlider.{name}.bottomCircle"),
                Rect::from_center_size(pos2(thumb.center().x, thumb.max.y - cw / 2.0), egui::vec2(14.0, 14.0)),
                "Zoom the tracks taller",
            );
        }
    }
    on_side
}

/// Ease slider heights and scroll positions toward a gesture's target, or apply it immediately
/// when smooth motion is disabled.
pub(in crate::panels::timeline) fn animate_track_sliders(app: &mut FilmcraftApp, ctx: &egui::Context) {
    let Some(target) = app.tl.track_anim else {
        return;
    };
    let smooth = app.session.state.timeline_ui.track_slider_smooth;
    let view = &mut app.ui.timeline;
    if smooth {
        let dt = ctx.input(|input| input.stable_dt).min(0.05);
        let rate = (1.0 - (-f64::from(dt) * 20.0).exp()) as f32;
        view.video_track_h += (target[0] - view.video_track_h) * rate;
        view.v_scroll += (target[1] - view.v_scroll) * rate;
        view.audio_track_h += (target[2] - view.audio_track_h) * rate;
        view.a_scroll += (target[3] - view.a_scroll) * rate;
    } else {
        view.video_track_h = target[0];
        view.v_scroll = target[1];
        view.audio_track_h = target[2];
        view.a_scroll = target[3];
    }
    if !smooth
        || ((target[0] - view.video_track_h).abs()
            + (target[1] - view.v_scroll).abs()
            + (target[2] - view.audio_track_h).abs()
            + (target[3] - view.a_scroll).abs())
            < 0.05
    {
        view.video_track_h = target[0];
        view.v_scroll = target[1];
        view.audio_track_h = target[2];
        view.a_scroll = target[3];
        app.tl.track_anim = None;
    } else {
        ctx.request_repaint();
    }
}

pub(in crate::panels::timeline) fn clamp_track_animation_scroll_targets(app: &mut FilmcraftApp, max_video_scroll: f32, max_audio_scroll: f32) {
    if let Some(target) = &mut app.tl.track_anim {
        target[1] = target[1].clamp(0.0, max_video_scroll);
        target[3] = target[3].clamp(0.0, max_audio_scroll);
    }
}

/// The track slider's thumb length for a track height: the same kind of zoom meter as the zoom
/// bar's thumb -- the shortest tracks fill the bar, the tallest leave the minimum, log-smooth in between.
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
    grab_offset: f32,
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

pub(in crate::panels::timeline) fn resize_tracks_from_timeline(height: &mut f32, scroll: &mut f32, window: f32, slots: f32, bar_h: f32, dy: f32) {
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

/// The track sliders' settings popup content. Opened by right-clicking a slider and by the
/// Timeline menu's "Track Slider Settings" row. The popup only becomes scrollable when the window
/// is too small to show it whole.
pub(super) fn track_slider_settings(app: &mut FilmcraftApp, ui: &mut egui::Ui) {
    settings_menu(app, ui, "timeline-track-slider", track_slider_settings_rows);
}

pub(super) fn zoom_settings_row(
    auto: &mut crate::automation::Registry,
    ui: &mut egui::Ui,
    settings: &mut filmcraft_project::TimelineUiSettings,
    content_w: f32,
    tokens: &Tokens,
) -> bool {
    let response = settings_pill_row(
        ui,
        tokens,
        content_w,
        tl!("Control Track Sliders"),
        egui::Id::unique("timeline-zoombar-control-tracks"),
        settings.control_tracks,
        Some(tl!("Drag the timeline slider vertically to resize both track sections. Recommended for when Track Sliders are hidden.")),
    );
    auto.add("timeline.zoomBar.settings.controlTracks", response.rect, "Control Track Sliders");
    if response.clicked() {
        settings.control_tracks = !settings.control_tracks;
        true
    } else {
        false
    }
}

/// The rows of the track sliders' settings popup (see [`track_slider_settings`]).
fn track_slider_settings_rows(app: &mut FilmcraftApp, ui: &mut egui::Ui) {
    let t = app.tokens;
    let alt_speed_uses_default = uses_default_gesture(&app.session.state.timeline_ui.track_control_alternate_drag_speed, "Shift + Drag");
    let alt_speed_label = if alt_speed_uses_default { tl!("Alternate Drag Speed (Shift + Drag)") } else { tl!("Alternate Drag Speed") };
    // Track slider settings menu content width.
    let content_w = 220.19;
    ui.set_max_width(content_w + 16.0);
    ui.set_min_width(content_w + 16.0);
    family_reset(ui);
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
    // "Controls": not a switch -- the slider's gestures in a flyout beside the menu (the Minimap
    // Settings flyout's exact recipe, in this menu too).
    let (arow, _) = ui.allocate_exact_size(egui::vec2(row_w, 30.0), Sense::hover());
    let alabel = tl!("Controls");
    let agalley = ui.painter().layout_no_wrap(alabel.to_string(), Tokens::ui(11.5), t.text);
    ui.painter().galley(pos2(arow.center().x - agalley.size().x / 2.0, arow.center().y - agalley.size().y / 2.0), agalley, t.text);
    app.auto.add("timeline.trackSlider.settings.controls", arow, "Controls");
    let controls_right = super::slider_controls::hover_flyout(
        app,
        ui,
        egui::Id::unique("timeline-track-slider-advanced-flyout"),
        arow,
        r.max.x,
        CONTROLS_FLYOUT_W,
        true,
        super::slider_controls::track_controls_profile(),
        "timeline.trackSlider.settings.controlsFlyout",
        "Controls",
        &[
            (tl!("Mirror Sliders:"), "Double Click", 0),
            (tl!("Explode Slider:"), "Shift + Double Click", 1),
            (tl!("Alternate Drag Speed:"), "Shift + Drag", 2),
            (tl!("Scroll Track Rows:"), "Ctrl + Scroll", 6),
            (tl!("Adjust Track Height:"), "Unbound", 4),
        ],
    );
    controls_arrow(ui, arow, content_w, r.max.x, controls_right, t.text_dim);
    // Smooth Animations eases the double-click moves (which land on the OTHER slider);
    // Alternate Slider Position moves the sliders to the left edge of the Timeline.
    let smooth = app.session.state.timeline_ui.track_slider_smooth;
    let c = settings_pill_row(
        ui,
        &t,
        row_w,
        tl!("Smooth Animations"),
        egui::Id::unique("timeline-track-slider-smooth"),
        smooth,
        Some(tl!("The edges of sliders, Center on Playhead, and Mirror Slider have smooth animations when enabled.")),
    );
    app.auto.add("timeline.trackSlider.settings.smoothAnimations", c.rect, tl!("Smooth Animations"));
    if c.clicked() {
        app.session.state.timeline_ui.track_slider_smooth = !smooth;
        app.session.project_view_changed();
    }
    // Inverted Controls: the very same switch as the Timeline slider's menu
    let inv = app.session.state.timeline_ui.inverted_controls;
    let c = settings_pill_row(
        ui,
        &t,
        row_w,
        tl!("Invert Scrolling"),
        egui::Id::unique("timeline-track-slider-inverted"),
        inv,
        Some(tl!("Reverses zooming when holding the center of a slider and scrolling vertically on the timeline.")),
    );
    app.auto.add("timeline.trackSlider.settings.invertedControls", c.rect, tl!("Invert Scrolling"));
    if c.clicked() {
        app.session.state.timeline_ui.inverted_controls = !inv;
        app.session.project_view_changed();
    }
    // Snap the visible clip edge to the section boundary; Shift selects free fine adjustment.
    let snap = app.session.state.timeline_ui.track_slider_snap;
    let c = settings_pill_row(
        ui,
        &t,
        row_w,
        tl!("Snap to Tracks"),
        egui::Id::unique("timeline-track-slider-snap"),
        snap,
        Some(tl!("Use Alternate Drag Speed for fine adjustments when enabled.")),
    );
    app.auto.add("timeline.trackSlider.settings.snapToTracks", c.rect, "Snap to Tracks");
    if c.clicked() {
        if !snap && !app.session.state.timeline_ui.track_slider_decouple {
            ui.ctx().data_mut(|d| d.insert_temp(egui::Id::unique("timeline-snap-requires-decoupling"), true));
            ui.ctx().request_repaint();
        } else {
            app.session.state.timeline_ui.track_slider_snap = !snap;
            app.session.project_view_changed();
        }
    }
    // Decouple Edges: an end circle drag moves only that end, the other one stays put
    let dec = app.session.state.timeline_ui.track_slider_decouple;
    let c = settings_pill_row(
        ui,
        &t,
        row_w,
        tl!("Decouple Edges"),
        egui::Id::unique("timeline-track-slider-decouple"),
        dec,
        Some(tl!("Keeps the opposite slider edge fixed while resizing.")),
    );
    app.auto.add("timeline.trackSlider.settings.decoupleEdges", c.rect, "Decouple Edges");
    if c.clicked() {
        app.session.state.timeline_ui.track_slider_decouple = !dec;
        if dec {
            app.session.state.timeline_ui.track_slider_snap = false;
        }
        app.session.project_view_changed();
    }
    let alt = app.session.state.timeline_ui.track_slider_alternate_position;
    let c = settings_pill_row(
        ui,
        &t,
        row_w,
        tl!("Alternate Slider Position"),
        egui::Id::unique("timeline-track-slider-alt"),
        alt,
        Some(tl!("Moves the track sliders to the left side.")),
    );
    app.auto.add("timeline.trackSlider.settings.alternateSliderPosition", c.rect, tl!("Alternate Slider Position"));
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
    let alt_speed_hint = tl!("Mimics the feature in Blender. A high multiplier can make it faster than dragging normally.");
    if alt_speed_uses_default {
        settings_heading_with_hint_center(ui, &t, alt_speed_label, alt_speed_hint, Some(r.right() - 10.0));
    } else {
        settings_heading(ui, &t, alt_speed_label, alt_speed_hint);
    }
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
pub(in crate::panels::timeline) fn track_slider_settings_popup(app: &mut FilmcraftApp, ui: &mut egui::Ui) {
    let Some(pos) = app.tl.track_slider_settings_pos else {
        return;
    };
    let screen = ui.ctx().content_rect();
    let popup_h = 420.0f32;
    let pos = if screen.height() < popup_h {
        pos2(pos.x, (screen.bottom() - screen.height().min(popup_h)).max(screen.top()))
    } else if pos.y + popup_h > screen.bottom() {
        pos2(pos.x, (screen.bottom() - popup_h).max(screen.top()))
    } else {
        pos
    };
    let panel =
        egui::Area::new(egui::Id::unique("tl-track-slider-settings")).order(egui::Order::Foreground).fixed_pos(pos).fade_in(false).interactable(true).show(
            ui.ctx(),
            |ui| {
                egui::Frame::popup(ui.style()).show(ui, |ui| {
                    track_slider_settings(app, ui);
                });
            },
        );
    if !controls_capturing(ui) && ui.input(|i| i.pointer.any_pressed())
        && let Some(p) = ui.input(|i| i.pointer.interact_pos())
        && !panel.response.rect.contains(p)
        // a click on one of the settings' own popups (the Advanced flyout) is not an outside click
        && !family_contains(ui, p)
    {
        app.tl.track_slider_settings_pos = None;
        track_slider_state_reset(ui.ctx());
    }
}

/// Hide or bring back the side track sliders (Hide Slider / Unhide Track Slider).
pub fn set_track_slider_hidden(app: &mut FilmcraftApp, _ctx: &egui::Context, hidden: bool) {
    if app.session.state.timeline_ui.track_slider_hidden == hidden {
        return;
    }
    let old_width = app.last_timeline_width.max(1.0);
    let new_width = (old_width + if hidden { 17.0 } else { -17.0 }).max(1.0);
    let view_seconds = f64::from(old_width) / app.ui.timeline.target_pps.max(1e-6);
    app.session.state.timeline_ui.track_slider_hidden = hidden;
    // Hiding changes the row canvas width, but it must not alter the Timeline bar's visual
    // position or visible time range. Adjust pps to preserve the exact time span and scroll.
    let pps = f64::from(new_width) / view_seconds.max(1e-9);
    app.ui.timeline.target_pps = pps;
    app.ui.timeline.pps = f64::from(new_width) / (f64::from(old_width) / app.ui.timeline.pps.max(1e-6)).max(1e-9);
    app.session.project_view_changed();
    app.tl.track_slider_settings_pos = None;
    track_slider_state_reset(_ctx);
    if !hidden {
        // a stale "how to bring it back" window means nothing once the sliders are back
        app.tl.track_hide_dialog_open = false;
    }
}

/// The mirrored audio/video track slider. Its handle is a representation of the visible
/// timeline range: edge drags resize that range, and detents are computed from timeline row
/// boundaries while preserving the opposite visible boundary when detached edges are enabled.
/// Returns (pointer on an end circle, thumb rect for automation ids, and the double-click action).
// Pointer, animation, resize and popup state must be processed in a fixed order within one egui
// frame. Keep this event handler together so its response state cannot be split across frames.
#[allow(clippy::too_many_arguments, clippy::cognitive_complexity)]
pub(in crate::panels::timeline) fn vertical_scrollbar(
    ui: &mut egui::Ui,
    tl: &mut super::super::TlState,
    bar: Rect,
    value: &mut f32,
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
    // Snap applies independently to each section.
    snap: bool,
    bindings: &[String; 4],
    t: &Tokens,
    id: &str,
) -> (bool, Rect, u8) {
    if bar.height() < 2.0 || bar.width() < 6.0 {
        return (false, bar, 0);
    }
    let p = ui.painter();
    p.rect_filled(bar, bar.width() / 2.0, t.separator);
    // with nothing to scroll the thumb fills the track -- the slider is always there, it never
    // just disappears at the ends of the A/V divider's range
    // the thumb is the track-height meter, unbounded: 22 pt tracks fill the bar (full zoom out)
    // and 220 pt leave the MIN_THUMB_W minimum. (A cap here once kept the scroll a travel to use,
    // but it also made full zoom-out unreachable and the drag fought the cap and vibrated.)
    let min_height = track_min_height(window_h, slots);
    let live_max = (slots.max(1.0) * *track_h - window_h).max(0.0);
    let min_thumb = MIN_THUMB_W.min(bar.height()).max((bar.width() + 4.0).min(bar.height()));
    let resize_key = egui::Id::unique(("tl-vs-resize", id));
    let mut resize = ui.data(|d| d.get_temp::<TrackResizeState>(resize_key)).filter(|s| {
        s.fixed.is_finite() && s.center.is_finite() && s.free_len.is_finite() && s.view_anchor.is_none_or(|(a, o, _)| a.is_finite() && o.is_finite())
    });
    let active_mode = resize.map_or(0, |s| s.mode);
    let mut h = track_thumb(*track_h, bar.height(), min_height).clamp(min_thumb, bar.height());
    if decouple
        && matches!(active_mode, 1 | 2)
        && let Some(s) = resize
    {
        let room = if s.mode == 1 { s.fixed - bar.min.y } else { bar.max.y - s.fixed };
        // The ring and its center sit just beyond the rail at an endpoint. Preserve a one-point
        // capture margin so the last fractional-track detent remains attainable.
        h = h.min((room + 1.0).max(min_thumb));
    }
    let travel = (bar.height() - h).max(0.0);
    let position = if live_max > 0.5 { (if invert { 1.0 - *value / live_max } else { *value / live_max }).clamp(0.0, 1.0) } else { 0.5 };
    let y = if decouple && matches!(active_mode, 1 | 2) {
        resize.map(|s| if s.mode == 1 { s.fixed - h } else { s.fixed }).unwrap_or(bar.min.y + travel * position)
    } else {
        bar.min.y + travel * position
    };
    let y = if decouple && matches!(active_mode, 1 | 2) { y } else { y.clamp(bar.min.y, (bar.max.y - h).max(bar.min.y)) };
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
    if resp.drag_started() && !controls_capturing(ui) {
        let press = ui.input(|i| i.pointer.press_origin()).or(resp.interact_pointer_pos());
        let mode = if let Some(q) = press {
            if (q.y - circle_t).abs() <= CIRCLE && (q.x - cx).abs() <= CIRCLE {
                1u8
            } else if (q.y - circle_b).abs() <= CIRCLE && (q.x - cx).abs() <= CIRCLE {
                2u8
            } else if !thumb.contains(q) {
                // A press beyond the thumb is a scroll jump/drag, even when it lands in
                // the edge-resize tolerance zone. Only the thumb's own edge can resize.
                0u8
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
        let grab_offset = press.map_or(h * 0.5, |q| if thumb.contains(q) { q.y - thumb.min.y } else { h * 0.5 });
        let fixed = if mode == 1 { thumb.max.y } else { thumb.min.y };
        let outer = (invert && mode == 1) || (!invert && mode == 2);
        // Capture the nearest visible row boundary and its exact screen offset. Keeping the
        // normalized viewport edge alone makes a nearby row drift as track height changes.
        let view_anchor = (decouple && matches!(mode, 1 | 2)).then(|| capture_fixed_view_anchor(*value, window_h, *track_h, outer)).flatten();
        let last_snap_q = view_anchor.map(|(_, _, edge)| if edge { (window_h + *value + 1.0) / *track_h } else { (*value - 1.0) / *track_h });
        resize = Some(TrackResizeState { mode, fixed, center: thumb.center().y, free_len: h, grab_offset, view_anchor, snap_detent: None, last_snap_q });
        if let Some(state) = resize {
            ui.data_mut(|d| d.insert_temp(resize_key, state));
        }
    }
    let mode = resize.map_or(0, |s| s.mode);
    if resp.drag_stopped() {
        ui.data_mut(|d| d.remove_temp::<TrackResizeState>(resize_key));
        resize = None;
        // Track sliders share the zoom bar's Shift-drag cursor pin. Release it on this
        // response's end so a completed drag never leaves the OS cursor confined.
        if !matches!(tl.drag, Some(Drag::ZoomBar { .. })) && tl.zoombar_cursor_lock.take().is_some() {
            tl.zoombar_unlock_catchup = true;
            release_slider_cursor(ui.ctx(), tl.zoombar_cursor_viewport.take());
        }
    }
    // Lost OS releases must not leave a resize mode active forever. A live egui drag always has
    // primary held; otherwise discard the captured geometry and rebuild from the current view.
    if !resp.dragged() && !ui.input(|i| i.pointer.primary_down()) {
        ui.data_mut(|d| d.remove_temp::<TrackResizeState>(resize_key));
        resize = None;
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
    // an end circle reads as a resize handle: a vertical resize cursor there (the caller ORs the
    // answer into the zoom bar's flag, which wins over the tracks' grab cursors later in the
    // frame). It stays a pointer for the WHOLE edge drag -- the slider can run away from the
    // cursor and the middle is never affected
    let point = on_circle || (resp.dragged() && matches!(mode, 1 | 2));
    if point {
        ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
    }
    if resp.dragged() && !controls_capturing(ui) {
        tl.track_slider_dragging = true;
        // a drag writes straight through: no Smooth Animations target may fight it
        tl.track_anim = None;
        // Shift+drag pins the cursor to its press pixel and hides it -- the zoom bar's proven
        // recipe, adapted to the vertical axis: a fully transparent 1x1 cursor image instead of
        // winit's hidden flag (which clips a grabbed + hidden cursor to the window's center), a
        // Confined grab, and a warp back to the pin every frame; the motion is measured by hand
        // against the pin. Without Shift the drag is ordinary pointer motion.
        let shift = alternate_down(ui.ctx(), &bindings[2]);
        let mut pinned_dy = 0.0f64;
        if shift && let Some(origin) = ui.input(|i| i.pointer.hover_pos().or(i.pointer.latest_pos())) {
            if tl.zoombar_cursor_lock.is_none() {
                tl.zoombar_cursor_lock = Some(origin);
                tl.zoombar_cursor_viewport = Some(ui.ctx().viewport_id());
                ui.ctx().send_viewport_cmd(egui::ViewportCommand::CursorGrab(egui::viewport::CursorGrab::Confined));
            }
            let pinned = tl.zoombar_cursor_lock.unwrap_or(origin);
            let now = ui.input(|i| i.pointer.latest_pos().unwrap_or(pinned));
            pinned_dy = f64::from(now.y - pinned.y);
            ui.ctx().send_viewport_cmd(egui::ViewportCommand::CursorPosition(pinned));
            ui.ctx().set_cursor_image(Some(super::transparent_cursor()));
        } else if tl.zoombar_cursor_lock.is_some() && !matches!(tl.drag, Some(Drag::ZoomBar { .. })) {
            tl.zoombar_unlock_catchup = true;
            release_slider_cursor(ui.ctx(), tl.zoombar_cursor_viewport.take());
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
        // Alternate Drag Speed (Shift drags): the setting SQUARED on a FIXED rate per point of
        // pointer motion -- the same rate on both sliders whatever their track counts, so a 12
        // track video slider and a 3 track audio slider feel 1:1 with each other. 1x is slow,
        // Custom 10x is a hundred times it (many times faster than the plain drag).
        let shift_on = shift;
        let rate = shift_drag_rate(shift_speed);
        // the height zoom moves the THUMB (0.06 thumb points per pointer point at 1x), the scroll
        // moves the CONTENT (0.2 content points): each rate is a constant, never one the track
        // count or the scroll range can skew
        let dy = (moved * if shift_on { rate * SHIFT_THUMB_RATE } else { 1.0 }) as f32;
        if matches!(mode, 1 | 2) {
            if let Some(state) = resize
                && let Some((new_height, new_scroll, free_len, snap_detent, last_snap_q)) =
                    resize_track_edge(state, dy, shift_on, snap, invert, decouple, *value, slots, window_h, min_height, bar, min_thumb)
            {
                *track_h = new_height;
                *value = new_scroll;
                // Keep the active thumb on the same scroll-to-rail mapping used on the next
                // frame after release. Previously the active layout stayed pinned to the
                // press-time rail edge while the view range changed; once the resize state was
                // removed, the canonical position was recomputed from `new_scroll` and jumped.
                // The content boundary remains fixed by `view_anchor`; this only synchronizes
                // the thumb's rail-space opposite edge in real time.
                let mut fixed = state.fixed;
                if decouple {
                    let max_scroll = (slots.max(1.0) * new_height - window_h).max(0.0);
                    let position =
                        if max_scroll > 0.5 { (if invert { 1.0 - new_scroll / max_scroll } else { new_scroll / max_scroll }).clamp(0.0, 1.0) } else { 0.5 };
                    let thumb_len = track_thumb(new_height, bar.height(), min_height).clamp(min_thumb, bar.height());
                    let thumb_y = bar.min.y + (bar.height() - thumb_len).max(0.0) * position;
                    fixed = if mode == 1 { thumb_y + thumb_len } else { thumb_y };
                }
                let next_state = TrackResizeState { fixed, free_len, snap_detent, last_snap_q, ..state };
                ui.data_mut(|d| d.insert_temp(resize_key, next_state));
                ui.ctx().request_repaint();
            }
        } else {
            let live_max = (slots.max(1.0) * *track_h - window_h).max(0.0);
            if live_max > 0.5 {
                if shift_on {
                    let delta = (moved * rate * SHIFT_CONTENT_RATE) as f32;
                    *value = (*value + if invert { -delta } else { delta }).clamp(0.0, live_max);
                } else {
                    let pointer_y = ui.input(|i| i.pointer.hover_pos().or(i.pointer.latest_pos())).map_or(y + dy, |p| p.y);
                    let position = ((pointer_y - resize.map_or(h * 0.5, |state| state.grab_offset) - bar.min.y) / travel.max(1.0)).clamp(0.0, 1.0);
                    *value = (if invert { 1.0 - position } else { position }) * live_max;
                }
            }

            let shift = ui.input(|i| i.modifiers.shift);
            let (wy, wx) = ui.input(|i| (i.smooth_scroll_delta.y, i.smooth_scroll_delta.x));
            let mut wheel = if shift { wy + wx } else { wy };
            if shift {
                wheel *= shift_speed.max(0.01) / 4.0;
            }
            if inverted {
                wheel = -wheel;
            }
            let factor = (wheel / 600.0).clamp(-0.30, 0.30);
            if wheel.is_finite() && factor != 0.0 {
                let new_height = (*track_h * (1.0 + factor)).clamp(min_height, track_max_height(min_height));
                let new_len = track_thumb(new_height, bar.height(), min_height).clamp(min_thumb, bar.height());
                let center_y = y + h * 0.5;
                let new_top = (center_y - new_len * 0.5).clamp(bar.min.y, (bar.max.y - new_len).max(bar.min.y));
                let new_travel = (bar.height() - new_len).max(0.0);
                let position = if new_travel > 0.001 { ((new_top - bar.min.y) / new_travel).clamp(0.0, 1.0) } else { 0.5 };
                let new_max = (slots.max(1.0) * new_height - window_h).max(0.0);
                *value = (if invert { 1.0 - position } else { position }) * new_max;
                *track_h = new_height;
                tl.track_anim = None;
            }
        }
    }
    // double-click copies this thumb's length and position to the other slider; Shift+double-
    // click fits every track; Ctrl+Shift+double-click fits BOTH sliders. Either way the single
    // click beneath them is not a jump.
    let mut clicks = 0u8;
    let mouse = (!controls_capturing(ui)).then(|| gesture(&resp, ui)).flatten();
    if matches_gesture(&bindings[0], "Double Click", mouse.as_deref()) {
        clicks = 1;
    } else if matches_gesture(&bindings[1], "Shift + Double Click", mouse.as_deref()) {
        clicks = if bindings[1].is_empty() { 2 } else { 3 };
    } else if bindings[1].is_empty() && mouse.as_deref() == Some("Ctrl + Shift + Double Click") {
        clicks = 3;
    }
    // A mirror is a discrete gesture, so it must never inherit a cursor pin left by a
    // just-completed slider drag. Keep the zoom bar's own active drag as the sole owner.
    if clicks != 0 && !matches!(tl.drag, Some(Drag::ZoomBar { .. })) && tl.zoombar_cursor_lock.take().is_some() {
        tl.zoombar_unlock_catchup = true;
        release_slider_cursor(ui.ctx(), tl.zoombar_cursor_viewport.take());
    }
    // a click beside the thumb jumps the view there, as on the zoom bar
    if clicks == 0
        && !controls_capturing(ui)
        && resp.clicked()
        && let Some(q) = resp.interact_pointer_pos()
        && !thumb.contains(q)
    {
        tl.track_anim = None;
        let f = ((q.y - bar.min.y - h / 2.0) / travel.max(1.0)).clamp(0.0, 1.0);
        *value = if invert { (1.0 - f) * live_max } else { f * live_max };
    }
    // right-click: the settings popup, always to the LEFT of the sliders (their alternate
    // position comes later), at the pointer's height
    let secondary_click = !controls_capturing(ui)
        && ui.input(|i| i.pointer.button_clicked(egui::PointerButton::Secondary) && i.pointer.interact_pos().is_some_and(|p| bar.contains(p)));
    if (secondary_click && (bindings[3].is_empty() || bindings[3] == "Right Click")) || matches_gesture(&bindings[3], "Right Click", mouse.as_deref()) {
        let y = ui.input(|i| i.pointer.interact_pos()).map(|p| p.y).unwrap_or(bar.center().y);
        // to the LEFT of the sliders (their alternate position is the left edge of the Timeline,
        // where that would run off the screen -- then it opens to their right instead)
        let popup_width = 229.0;
        let x = if bar.min.x - popup_width < ui.ctx().content_rect().left() { bar.max.x + 4.0 } else { bar.min.x - popup_width };
        tl.track_slider_settings_pos = Some(pos2(x, y));
        track_slider_state_reset(ui.ctx());
        // the two settings menus are mutually exclusive: this one takes the zoom bar's with it
        tl.zoombar_settings_pos = None;
        close_zoombar_popup(ui.ctx());
    }
    (point, thumb, clicks)
}

#[cfg(test)]
mod track_detent_tests {
    use super::{
        TrackResizeState, capture_fixed_view_anchor, fixed_edge_anchor, height_for_fixed_edge_detent, resize_track_edge, resize_tracks_from_timeline,
        scroll_for_fixed_edge, snap_edge_height, track_min_height, track_thumb,
    };

    #[test]
    fn detached_anchor_keeps_midrow_coordinates_exact_and_snapped_lines_pixel_pinned() {
        let midrow = capture_fixed_view_anchor(6.333 * 100.0, 300.0, 100.0, true).expect("finite mid-row anchor");
        assert!((midrow.0 - 6.333).abs() < 1e-5);
        assert_eq!(midrow.1, 0.0, "a mid-row edge must not quantize to its nearest divider");

        let divider = capture_fixed_view_anchor(101.0, 300.0, 100.0, true).expect("finite divider anchor");
        assert_eq!(divider.0, 1.0);
        assert_eq!(divider.1, -1.0, "an already snapped white line retains its exact rendered inset");
    }

    #[test]
    fn snapped_track_detents_hold_until_the_pointer_crosses_the_next_midpoint() {
        let (window, slots, anchor, offset) = (214.0, 9.0 + 1.0 / 3.0, 1.0, 0.5);
        let min_height = 22.0;
        let max_height = 220.0;
        let before_midpoint = snap_edge_height(4.54, min_height, max_height, anchor, window, offset, true, slots, 0.54, Some(4.0))
            .expect("the current whole-track detent stays latched through jitter");
        assert_eq!(before_midpoint.1, 4.0);
        let beyond_midpoint = snap_edge_height(4.56, min_height, max_height, anchor, window, offset, true, slots, 0.56, Some(4.0))
            .expect("the next detent becomes available after the midpoint");
        assert_eq!(beyond_midpoint.1, 5.0);
        let endpoint = snap_edge_height(9.18, min_height, max_height, anchor, window, offset, true, slots, 0.18, Some(9.0))
            .expect("the fractional reveal edge is reachable from the last whole row");
        assert_eq!(endpoint.1, slots);
        let stationary = snap_edge_height(9.18, min_height, max_height, anchor, window, offset, true, slots, 0.0, Some(slots))
            .expect("the fractional detent remains latched on a stationary frame");
        assert_eq!(stationary.1, slots);
    }

    #[test]
    fn snapped_fractional_endpoint_does_not_bounce_back_on_a_stationary_frame() {
        let (window, slots, bar_h) = (214.0, 9.0 + 1.0 / 3.0, 240.0);
        let min_height = track_min_height(window, slots);
        let target_height = (window + 0.5) / (slots - 1.0);
        let target_len = track_thumb(target_height, bar_h, min_height);
        let bar = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(12.0, bar_h));
        let state = TrackResizeState {
            mode: 2,
            fixed: bar.max.y - (target_len - 0.5),
            center: bar.center().y,
            free_len: 34.0,
            grab_offset: 0.0,
            view_anchor: Some((1.0, 0.5, true)),
            snap_detent: None,
            last_snap_q: Some(1.0),
        };
        let first = resize_track_edge(state, target_len - state.free_len + 1.0, false, true, false, true, 0.0, slots, window, min_height, bar, 34.0)
            .expect("the outer fractional detent is reachable");
        let steady_state = TrackResizeState { free_len: first.2, snap_detent: first.3, last_snap_q: first.4, ..state };
        let second = resize_track_edge(steady_state, 0.0, false, true, false, true, first.1, slots, window, min_height, bar, 34.0)
            .expect("the captured resize remains valid without new motion");

        assert!((first.0 - target_height).abs() < 0.01, "the first frame reaches the one-third reveal detent");
        assert!((second.0 - first.0).abs() < 0.01, "a stationary frame must not snap back to the last whole track");
        assert!((second.1 - first.1).abs() < 0.01, "the visible viewport must stay at the endpoint");
    }

    #[test]
    fn outer_endpoint_snap_latches_from_last_track_during_a_live_drag_for_both_orientations() {
        let (window, bar_h, slots, min_thumb) = (420.0, 240.0, 9.0 + 1.0 / 3.0, 34.0);
        let bar = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(12.0, bar_h));
        let min_height = track_min_height(window, slots);
        let last_whole = (slots - 1.0 / 3.0).round();

        // Video grabs its upper edge (mode 1, inverted rail); audio grabs its lower edge
        // (mode 2, normal rail). Both represent the same outer timeline boundary.
        for (mode, invert, offset) in [(1, true, -0.5), (2, false, 0.5)] {
            for anchor in [1.5, 2.5, 5.5] {
                let start_height =
                    height_for_fixed_edge_detent(anchor, window, offset, true, last_whole).expect("the final whole-track detent has a finite height");
                let start_scroll = scroll_for_fixed_edge(anchor, start_height, window, offset, true);
                let start_len = track_thumb(start_height, bar_h, min_height);
                let max_scroll = (slots * start_height - window).max(0.0);
                let position = if invert { 1.0 - start_scroll / max_scroll } else { start_scroll / max_scroll };
                let thumb_y = bar.min.y + (bar_h - start_len) * position;
                let fixed = if mode == 1 { thumb_y + start_len } else { thumb_y };
                let mut state = TrackResizeState {
                    mode,
                    fixed,
                    center: thumb_y + start_len * 0.5,
                    free_len: start_len,
                    grab_offset: 0.0,
                    view_anchor: Some((anchor, offset, true)),
                    snap_detent: Some(last_whole),
                    last_snap_q: Some(last_whole),
                };
                let mut value = start_scroll;
                let mut reached_endpoint = false;

                for _ in 0..80 {
                    let dy = if mode == 1 { -0.1 } else { 0.1 };
                    let (height, scroll, free_len, snap_detent, last_snap_q) =
                        resize_track_edge(state, dy, false, true, invert, true, value, slots, window, min_height, bar, min_thumb)
                            .expect("the anchored drag remains within range");
                    let detent = snap_detent.expect("the visible boundary remains snapped");
                    let visible_outer = (window + scroll + 1.0) / height;
                    assert!(
                        (visible_outer - detent).abs() < 0.02,
                        "the {mode} orientation's visible timeline boundary must match its detent: {visible_outer} vs {detent}"
                    );
                    if detent == slots {
                        reached_endpoint = true;
                    }
                    assert!(
                        !reached_endpoint || detent == slots,
                        "the {mode} orientation bounced from the one-third endpoint back to a whole track (anchor {anchor}, detent {detent})"
                    );

                    let length = track_thumb(height, bar_h, min_height).clamp(min_thumb, bar_h);
                    let max_scroll = (slots * height - window).max(0.0);
                    let position = if max_scroll > 0.5 { (if invert { 1.0 - scroll / max_scroll } else { scroll / max_scroll }).clamp(0.0, 1.0) } else { 0.5 };
                    let thumb_y = bar.min.y + (bar_h - length).max(0.0) * position;
                    state = TrackResizeState { fixed: if mode == 1 { thumb_y + length } else { thumb_y }, free_len, snap_detent, last_snap_q, ..state };
                    value = scroll;
                }

                assert!(reached_endpoint, "the {mode} orientation never snapped to the final one-third slot (anchor {anchor})");
            }
        }
    }

    #[test]
    fn snapped_audio_outer_edge_preserves_the_inner_viewport_edge() {
        let (window, old_height, old_scroll, offset) = (300.0, 120.0, 84.0, 0.5);
        let mut height = old_height;
        let mut scroll = old_scroll;
        for new_height in [140.0, 100.0, 160.0] {
            let anchor = fixed_edge_anchor(scroll, window, height, true, offset);
            let new_scroll = scroll_for_fixed_edge(anchor, new_height, window, offset, true);
            let after = fixed_edge_anchor(new_scroll, window, new_height, true, offset);
            assert!((after - anchor).abs() < 1e-6, "repeatedly snapping the audio outer edge must leave its inner edge fixed");
            scroll = new_scroll;
            height = new_height;
        }

        // The video slider follows the same outer-edge mapping, with its existing opposite
        // half-point inset. Keep that working mapping unchanged.
        let video_anchor = fixed_edge_anchor(old_scroll, window, old_height, true, -offset);
        let video_scroll = scroll_for_fixed_edge(video_anchor, old_height + 20.0, window, -offset, true);
        assert!((fixed_edge_anchor(video_scroll, window, old_height + 20.0, true, -offset) - video_anchor).abs() < 1e-6);
    }

    #[test]
    fn audio_inner_outer_inner_snap_sequence_keeps_each_fixed_edge_stable() {
        let (window, offset) = (300.0, 0.5);
        let (mut height, mut scroll) = (120.0, 84.0);

        // Inner edge moves first: the opposite (outer) viewport edge stays fixed.
        let outer_anchor = fixed_edge_anchor(scroll, window, height, false, offset);
        height = 130.0;
        scroll = scroll_for_fixed_edge(outer_anchor, height, window, offset, false);
        assert!((fixed_edge_anchor(scroll, window, height, false, offset) - outer_anchor).abs() < 1e-6);

        // Outer edge moves next: the inner viewport edge stays fixed.
        let inner_anchor = fixed_edge_anchor(scroll, window, height, true, offset);
        height = 140.0;
        scroll = scroll_for_fixed_edge(inner_anchor, height, window, offset, true);
        assert!((fixed_edge_anchor(scroll, window, height, true, offset) - inner_anchor).abs() < 1e-6);

        // Moving the inner edge again must still preserve the latest outer anchor.
        let outer_anchor = fixed_edge_anchor(scroll, window, height, false, offset);
        height = 150.0;
        scroll = scroll_for_fixed_edge(outer_anchor, height, window, offset, false);
        assert!((fixed_edge_anchor(scroll, window, height, false, offset) - outer_anchor).abs() < 1e-6);
        assert!(height.is_finite() && scroll.is_finite() && scroll >= 0.0);
    }

    #[test]
    fn linked_resize_respects_fit_and_preserves_each_scroll_independently() {
        let (mut height, mut scroll) = (100.0, 0.0);
        resize_tracks_from_timeline(&mut height, &mut scroll, 200.0, 3.0 + 1.0 / 3.0, 200.0, -10.0);
        assert!(height > 100.0);
        assert_eq!(scroll, 0.0);
        resize_tracks_from_timeline(&mut height, &mut scroll, 200.0, 3.0 + 1.0 / 3.0, 200.0, 1000.0);
        assert!((height - track_min_height(200.0, 3.0 + 1.0 / 3.0)).abs() < 0.001);
        assert_eq!(scroll, 0.0);
    }
}

#[cfg(test)]
mod controls_popup_tests {
    use super::super::route_slider_shortcut;
    use super::super::slider_controls::{controls_listen_document_epoch_key, controls_listen_key, controls_listen_track_key, controls_listening};
    use crate::{FilmcraftApp, dock::PanelKind};

    #[test]
    fn controls_capture_clears_when_track_settings_popup_disappears() {
        let mut session = filmcraft_engine::Session::default();
        session.execute("file.openDemoProject", serde_json::json!({})).unwrap();
        let mut app = FilmcraftApp::new(session);
        app.ui.focused = PanelKind::Timeline;
        app.tl.track_slider_settings_pos = Some(egui::pos2(10.0, 10.0));
        let ctx = egui::Context::default();
        let set_capture = |ctx: &egui::Context, epoch| {
            ctx.data_mut(|d| {
                d.insert_temp(controls_listen_key(), Some(2usize));
                d.insert_temp(controls_listen_track_key(), true);
                d.insert_temp(controls_listen_document_epoch_key(), epoch);
            });
        };

        set_capture(&ctx, app.session.document_epoch);
        let mut output = ctx.run_ui(Default::default(), |ui| route_slider_shortcut(&app, ui.ctx()));
        output.textures_delta.clear();
        assert!(controls_listening(&ctx), "the visible Track Slider Settings popup owns the capture");

        app.tl.track_slider_settings_pos = None;
        set_capture(&ctx, app.session.document_epoch);
        let mut output = ctx.run_ui(Default::default(), |ui| route_slider_shortcut(&app, ui.ctx()));
        output.textures_delta.clear();
        assert!(!controls_listening(&ctx), "a vanished popup cannot keep the Timeline listening");
    }
}

#[cfg(test)]
mod track_animation_limit_tests {
    use super::{FilmcraftApp, animate_track_sliders, clamp_track_animation_scroll_targets};

    #[test]
    fn a_shrunk_scroll_range_rebases_the_animation_target_and_stops_repainting() {
        let mut app = FilmcraftApp::new(filmcraft_engine::Session::default());
        app.session.state.timeline_ui.track_slider_smooth = true;
        app.ui.timeline.a_scroll = 4.0;
        app.tl.track_anim = Some([app.ui.timeline.video_track_h, 0.0, app.ui.timeline.audio_track_h, 10.0]);
        let ctx = egui::Context::default();
        let mut output = ctx.run_ui(Default::default(), |ui| {
            clamp_track_animation_scroll_targets(&mut app, 0.0, 4.0);
            animate_track_sliders(&mut app, ui.ctx());
        });
        output.textures_delta.clear();

        assert_eq!(app.ui.timeline.a_scroll, 4.0);
        assert_eq!(app.tl.track_anim, None, "the unreachable target must be clamped so the animation can finish");
    }
}
