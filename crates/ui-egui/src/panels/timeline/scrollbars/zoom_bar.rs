use super::super::layout::{max_pps, timeline_extent};
use super::{
    CursorIcon, Drag, FilmcraftApp, MIN_THUMB_W, Pos2, Rect, Sense, Sequence, Tokens, alternate_down, controls_capturing, family_contains, gesture, pos2,
    release_slider_cursor, shift_drag_rate, slider_action_triggered, vec2, zoom_trigger_key, zoombar_state_reset,
};

const ZOOM_SHIFT_THUMB_RATE: f64 = 0.06;

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
        super::zoombar_settings_rows(app, ui);
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

pub(super) fn zoombar_popup_open_key() -> egui::Id {
    egui::Id::unique("timeline-zoombar-settings-open")
}

/// The rows of the zoom bar's settings popup (see [`zoombar_settings`]).
// one section per setting keeps the popup's rows in visual order; splitting the function would
// scatter the popup's layout over the file
#[allow(clippy::cognitive_complexity)]
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

/// The Maximum Zoom Level, sanitized: the least of the Timeline the view may show, 0.05-3 s
/// (the setting; hostile values give the 0.4 s default).
fn min_view_secs(v: f64) -> f64 {
    if v.is_finite() && v > 0.0 { v.clamp(0.05, 3.0) } else { 0.4 }
}

/// The log-mapped share of the zoom range: 0.0 when the view shows `min_view` seconds (the
/// Maximum Zoom Level setting -- zoomed all the way in), 1.0 when it shows `vis_max` seconds (the
/// whole Timeline; the thumb fills the bar). The thumb's width is this fraction of the span between
/// [`MIN_THUMB_W`] and the bar's width: an exponential zoom meter, so the thumb can keep shrinking
/// to the smallest view no matter how long the Timeline is.
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
/// [`MIN_THUMB_W`] and the bar's full width. Its position maps the scroll range linearly.
pub(super) fn zoom_thumb(bar: Rect, view_w: f32, seq_seconds: f64, scroll: f64, pps: f64, min_view: f64) -> (f32, f32, f64) {
    let vis = view_w.max(1.0) as f64 / pps.max(1e-9);
    let total = timeline_extent(seq_seconds).max(vis);
    let f = log_frac(vis, total, min_view) as f32;
    let w = (MIN_THUMB_W + f * (bar.width() - MIN_THUMB_W)).clamp(MIN_THUMB_W.min(bar.width()), bar.width());
    let travel = (bar.width() - w).max(0.0);
    let range = (total - vis).max(0.0);
    let at = if range > 0.0 { (scroll / range).clamp(0.0, 1.0) as f32 * travel } else { 0.0 };
    (bar.min.x + at, w, if travel > 0.5 { range / travel as f64 } else { 0.0 })
}

/// Grows or shrinks the view symmetrically about its center, keeping the footage at the middle put.
fn zoom_about_center(v: &mut crate::state::TimelineView, view_w: f32, total: f64, new_pps: f64, min_view: f64) {
    if !(new_pps.is_finite() && new_pps > 0.0) {
        return;
    }
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

fn zoombar_popup_controls(app: &mut FilmcraftApp, ui: &mut egui::Ui, bar: Rect, resp: &egui::Response) -> (bool, bool) {
    let popup_id = egui::Popup::default_response_id(resp);
    let anchor_id = popup_id.with("timeline-settings-anchor");
    // a Controls binding the user chose: its chord does what the gesture used to do (the
    // built-in gestures still work). A chord with Ctrl/Alt fires even while a text field has
    // focus (it is a shortcut, not typing); a plain key only when nothing else wants the keyboard.
    let key_trigger = ui.data_mut(|d| d.remove_temp::<usize>(zoom_trigger_key()));
    if key_trigger.is_some_and(|slot| slot >= 4) {
        ui.data_mut(|d| d.insert_temp(egui::Id::unique("tl-controls-zoom-extra-action"), key_trigger.unwrap_or(0)));
    }
    let mouse = (!controls_capturing(ui)).then(|| gesture(resp, ui)).flatten();
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
    if is_open && !controls_capturing(ui) && ui.input(|input| input.key_pressed(egui::Key::Escape)) {
        ctx.data_mut(|data| data.insert_temp(zoombar_popup_open_key(), false));
    }
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
    (center_bound, explode_bound)
}

struct ZoomDrag {
    bar: Rect,
    view_w: f32,
    seq_seconds: f64,
    total: f64,
    min_view: f64,
    frame_seconds: f64,
    per_point: f64,
    range: f64,
}

fn update_zoom_drag(app: &mut FilmcraftApp, ui: &mut egui::Ui, resp: &egui::Response, mode: u8, state: ZoomDrag) -> bool {
    let ZoomDrag { bar, view_w, seq_seconds, total, min_view, frame_seconds, per_point, range } = state;

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
        return false;
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
        ui.ctx().set_cursor_image(Some(super::transparent_cursor()));
    } else if let Some(_origin) = app.tl.zoombar_cursor_lock.take() {
        app.tl.zoombar_unlock_catchup = true;
        release_slider_cursor(ui.ctx(), app.tl.zoombar_cursor_viewport.take());
    }
    if app.tl.zoombar_track_control && !just_control {
        track_dy = if app.tl.zoombar_cursor_lock.is_some() { shift_dy } else { delta.y };
    }
    if mode == 0 && app.tl.zoombar_track_control && track_dy.is_finite() {
        let rate = if shift {
            let configured = app.session.state.timeline_ui.shift_drag_speed;
            let speed = if configured.is_finite() { configured.clamp(0.1, 50.0) as f32 } else { 1.0 };
            (ZOOM_SHIFT_THUMB_RATE * shift_drag_rate(speed)) as f32
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
    if mode == 0 {
        if moved != 0.0 && app.tl.playhead_captured {
            app.tl.playhead_captured = false;
        }
        let v = &mut app.ui.timeline;
        v.target_scroll = (v.target_scroll + dx * per_point).clamp(0.0, range);
        v.scroll = v.target_scroll;
    } else {
        zoom_edge_drag(app, state, mode, shift, dx);
    }
    zoom_drag_wheel(app, ui, view_w, total, min_view, mode);

    true
}

fn zoom_edge_drag(app: &mut FilmcraftApp, state: ZoomDrag, mode: u8, shift: bool, dx: f64) {
    let ZoomDrag { bar, view_w, total, min_view, .. } = state;
    let v = &mut app.ui.timeline;
    // either edge handle mirrors the slider around its middle, keeping the footage at the view's center put
    let vis = view_w.max(1.0) as f64 / v.pps.max(1e-9);
    let dx = if shift && app.tl.zoombar_cursor_lock.is_some() { dx * shift_edge_speed_multiplier(vis) } else { dx };
    let dir = if mode == 1 { -1.0 } else { 1.0 };
    let f0 = log_frac(vis, total, min_view);
    let nf = (f0 + dir * dx / f64::from((bar.width() - MIN_THUMB_W).max(1.0)) * 2.0).clamp(0.0, 1.0);
    let new_vis = vis_at_frac(nf, total, min_view);
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
    let anchored_scroll = if mode == 2 { old_scroll } else { old_scroll + vis - new_vis };
    v.scroll = anchored_scroll.clamp(0.0, (total - new_vis).max(0.0));
    v.target_scroll = v.scroll;
    app.tl.zoom_anchor = None;
    let smooth_animations = app.session.state.timeline_ui.smooth_animations;
    if let Some(scroll) = immediate_center_scroll(
        smooth_animations,
        app.session.state.timeline_ui.playhead_centering && zooming_in,
        was_in_view,
        ph,
        view_w.max(1.0) as f64 / v.pps.max(1e-9),
        total,
    ) {
        v.scroll = scroll;
        v.target_scroll = scroll;
    } else if zooming_in && app.session.state.timeline_ui.playhead_centering && smooth_animations {
        apply_playhead_centering(app, view_w, total, ph, was_in_view);
    }
}

fn immediate_center_scroll(smooth: bool, enabled: bool, was_in_view: bool, playhead: f64, visible: f64, total: f64) -> Option<f64> {
    (!smooth && enabled && was_in_view).then(|| (playhead - visible / 2.0).clamp(0.0, (total - visible).max(0.0)))
}

fn apply_playhead_centering(app: &mut FilmcraftApp, view_w: f32, total: f64, ph: f64, was_in_view: bool) {
    let v = &mut app.ui.timeline;
    let vis = view_w.max(1.0) as f64 / v.pps.max(1e-9);
    let range = (total - vis).max(0.0);
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
        let lo = (ph - vis).max(0.0).min(range);
        let hi = ph.max(0.0).min(range);
        v.scroll = (ph - u * vis).max(lo).min(hi);
        v.target_scroll = v.scroll;
    }
}

fn finish_zoom_drag(app: &mut FilmcraftApp, ui: &mut egui::Ui, resp: &egui::Response) {
    if resp.drag_stopped() {
        app.tl.drag = None;
        app.tl.zoombar_track_control = false;
        ui.data_mut(|d| d.remove_temp::<bool>(egui::Id::unique("timeline-control-tracks-axis")));
        if app.tl.zoombar_cursor_lock.take().is_some() {
            release_slider_cursor(ui.ctx(), app.tl.zoombar_cursor_viewport.take());
            app.tl.zoombar_unlock_catchup = true;
        }
    }
}

fn zoom_drag_wheel(app: &mut FilmcraftApp, ui: &mut egui::Ui, view_w: f32, total: f64, min_view: f64, mode: u8) {
    if mode == 0 {
        let shift = ui.input(|i| i.modifiers.shift);
        let (wy, wx) = ui.input(|i| (i.smooth_scroll_delta.y, i.smooth_scroll_delta.x));
        let wheel = if shift { wy + wx } else { wy };
        let mut factor = (wheel as f64 / 600.0).clamp(-0.30, 0.30);
        if shift {
            let speed =
                if app.session.state.timeline_ui.shift_drag_speed.is_finite() { app.session.state.timeline_ui.shift_drag_speed.clamp(0.1, 50.0) } else { 1.0 };
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

/// Draw the zoom bar without any minimap layers; callers that do not ship the minimap can use it directly.
pub(crate) fn zoom_scrollbar(app: &mut FilmcraftApp, ui: &mut egui::Ui, bar: Rect, view_w: f32, seq: &Sequence, frame_seconds: f64, t: &Tokens) {
    zoom_scrollbar_with_layers(app, ui, ZoomBarSurface { bar, view_w, seq, frame_seconds, tokens: t }, |_, _, _, _, _| {}, |_, _, _, _, _, _| {});
}

pub(super) struct ZoomBarSurface<'a> {
    pub bar: Rect,
    pub view_w: f32,
    pub seq: &'a Sequence,
    pub frame_seconds: f64,
    pub tokens: &'a Tokens,
}

/// Draw the zoom bar while allowing independent Timeline layers at the bar's original paint points.
/// The minimap uses these callbacks without making the zoom-bar module depend on minimap code.
pub(in crate::panels::timeline) fn zoom_scrollbar_with_layers(
    app: &mut FilmcraftApp,
    ui: &mut egui::Ui,
    surface: ZoomBarSurface<'_>,
    mut draw_underlay: impl FnMut(&FilmcraftApp, &egui::Ui, Rect, &Sequence, &Tokens),
    mut draw_overlay: impl FnMut(&FilmcraftApp, &egui::Ui, Rect, f32, &Sequence, &Tokens),
) {
    let ZoomBarSurface { bar, view_w, seq, frame_seconds, tokens: t } = surface;
    let seq_seconds = seq.duration().seconds();
    let p = ui.painter();
    let resp = ui.interact(bar, egui::Id::unique("tl-zoombar"), Sense::click_and_drag());
    p.rect_filled(bar, bar.height() / 2.0, t.separator);
    let vis = view_w.max(1.0) as f64 / app.ui.timeline.pps.max(1e-9);
    let total = timeline_extent(seq_seconds).max(vis);
    let min_view = app.session.state.timeline_ui.max_zoom_secs();
    let (left, w, per_point) = zoom_thumb(bar, view_w, seq_seconds, app.ui.timeline.scroll, app.ui.timeline.pps, min_view);
    let thumb = Rect::from_min_max(pos2(left, bar.min.y), pos2(left + w, bar.max.y));
    // lit while the pointer is over the thumb, and for as long as any zoom-bar drag is going on --
    // dragging by an edge handle moves the pointer off the thumb, and the thumb must not flash grey
    // while it is being worked (the highlight follows the drag, not just the pointer)
    let hover = ui.rect_contains_pointer(thumb) || matches!(app.tl.drag, Some(Drag::ZoomBar { .. })) && resp.dragged();
    p.rect_filled(thumb, thumb.height() / 2.0, if hover { t.scroll_thumb_hover } else { t.scroll_thumb });
    draw_underlay(app, ui, bar, seq, t);
    // Each end circle: a dark 4.5 disc with a white ring AROUND it (4.5..6.0). While a circle is
    // touched or an edge is dragged the ring only comes further IN -- its outer edge never moves --
    // and it stops at the thickness whose dark middle looked right. Touching either circle lights
    // up the other one too.
    let hp = ui.input(|i| i.pointer.hover_pos().or(i.pointer.interact_pos()));
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
    // The edge circles are click targets, so use the pointing hand while hovering them and during
    // their drag. Shift drags hide the cursor (it is pinned), so it stays hidden there.
    app.tl.zoombar_point_cursor = app.tl.zoombar_cursor_lock.is_none() && on_circle && !edge_grab;
    if app.tl.zoombar_point_cursor {
        ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
    }
    draw_overlay(app, ui, bar, view_w, seq, t);
    app.auto.add("timeline.zoomBar", thumb, "zoom scroll bar");
    app.auto.add("timeline.zoomBar.track", bar, "zoom scroll bar track");
    let (center_bound, explode_bound) = zoombar_popup_controls(app, ui, bar, &resp);
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
        && ui.input(|input| input.key_pressed(egui::Key::Escape))
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
        && !update_zoom_drag(app, ui, &resp, mode, ZoomDrag { bar, view_w, seq_seconds, total, min_view, frame_seconds, per_point, range })
    {
        return;
    }
    finish_zoom_drag(app, ui, &resp);
}

#[cfg(test)]
mod playhead_centering_tests {
    use super::immediate_center_scroll;

    #[test]
    fn disabled_smooth_motion_centers_immediately_without_capturing_gravity() {
        assert_eq!(immediate_center_scroll(false, true, true, 8.0, 4.0, 20.0), Some(6.0));
        assert_eq!(immediate_center_scroll(false, true, false, 8.0, 4.0, 20.0), None);
        assert_eq!(immediate_center_scroll(true, true, true, 8.0, 4.0, 20.0), None);
        assert_eq!(immediate_center_scroll(false, false, true, 8.0, 4.0, 20.0), None);
    }
}

#[cfg(test)]
mod zoom_drag_scale_tests {
    use super::{shift_drag_base, shift_edge_speed_multiplier};

    #[test]
    fn hovering_zoom_bar_edge_circle_uses_pointing_hand_cursor() {
        use egui::{Event, RawInput, Rect, pos2};

        let mut session = filmcraft_engine::Session::default();
        session.execute("file.openDemoProject", serde_json::json!({})).expect("open demo project");
        let mut app = crate::FilmcraftApp::new(session);
        app.session.state.timeline_ui.slider_minimap = false;
        let sequence = app.session.state.active_sequence.and_then(|id| app.session.project.sequence(id)).expect("demo sequence").clone();
        let bar = Rect::from_min_max(pos2(10.0, 100.0), pos2(990.0, 116.0));
        let thumb = super::zoom_thumb(
            bar,
            bar.width(),
            sequence.duration().seconds(),
            app.ui.timeline.scroll,
            app.ui.timeline.pps,
            app.session.state.timeline_ui.max_zoom_secs(),
        );
        let edge_circles = [pos2(thumb.0 + bar.height() / 2.0, bar.center().y), pos2(thumb.0 + thumb.1 - bar.height() / 2.0, bar.center().y)];
        let ctx = egui::Context::default();
        let tokens = app.tokens;
        for (side, edge_circle) in ["left", "right"].into_iter().zip(edge_circles) {
            let input = RawInput {
                screen_rect: Some(Rect::from_min_max(pos2(0.0, 0.0), pos2(1_000.0, 600.0))),
                events: vec![Event::PointerMoved(edge_circle)],
                ..Default::default()
            };
            let mut output = ctx.run_ui(input, |ui| {
                super::zoom_scrollbar(&mut app, ui, bar, bar.width(), &sequence, 1.0 / 24.0, &tokens);
            });
            assert!(app.tl.zoombar_point_cursor, "pointer at {edge_circle:?} should hit the zoom thumb's {side} circle (left={}, width={})", thumb.0, thumb.1);
            assert_eq!(output.platform_output.cursor_icon, egui::CursorIcon::PointingHand, "the {side} edge circle should present the pointing cursor");
            output.textures_delta.clear();
        }
    }

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
        use super::super::super::layout::timeline_extent;
        use super::MIN_THUMB_W;
        use super::zoom_thumb;
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
