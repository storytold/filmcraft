use super::super::layout::timeline_extent;
use super::{Color32, FilmcraftApp, Pos2, Project, Rect, Sequence, Stroke, StrokeKind, Tokens, parse_hex_color, pos2};
use super::{
    ColorSetting, Sense, color_rect_key, color_setting, controls_arrow, family_contains_prev, family_push, flyout_to_the_right, grace_key, open_pointer_key,
    pill_switch, settings_pill_row,
};

/// Default minimap indicator colours, used when stored values are missing or malformed.
pub(super) const DEFAULT_POS_COLOR: Color32 = Color32::from_rgb(0x58, 0x95, 0xec);
pub(super) const DEFAULT_VIDEO_COLOR: Color32 = Color32::from_rgb(0x64, 0xb5, 0xf6);
pub(super) const DEFAULT_AUDIO_COLOR: Color32 = Color32::from_rgb(0x2e, 0x7d, 0x32);
pub(super) const DEFAULT_OFFLINE_COLOR: Color32 = Color32::from_rgb(0xd3, 0x2f, 0x2f);
pub(super) const MINIMAP_FLYOUT_HEIGHT: f32 = 288.0;

pub(super) fn state_reset(ctx: &egui::Context) {
    let pointer = ctx.input(|i| i.pointer.hover_pos()).unwrap_or(pos2(-1e9, -1e9));
    let flyout_id = egui::Id::unique("timeline-zoombar-minimap-flyout");
    ctx.data_mut(|d| {
        d.insert_temp(open_pointer_key(), pointer);
        d.insert_temp(flyout_id.with("rect"), Rect::NOTHING);
        d.insert_temp(flyout_id.with("stamp"), -1e9f64);
        d.insert_temp(flyout_id.with("height"), MINIMAP_FLYOUT_HEIGHT);
        d.remove_temp::<Pos2>(flyout_id.with("opening-pos"));
        d.insert_temp(flyout_id.with("open"), false);
    });
}

pub(in crate::panels::timeline) fn draw_clips_for_sequence(app: &FilmcraftApp, ui: &egui::Ui, bar: Rect, seq: &Sequence, t: &Tokens) {
    let seq_seconds = seq.duration().seconds();
    let span = minimap_span(app, seq_seconds);
    draw_minimap_clips(app, ui, bar, seq, span, t);
}

pub(in crate::panels::timeline) fn draw_position_for_sequence(app: &FilmcraftApp, ui: &egui::Ui, bar: Rect, view_w: f32, seq: &Sequence, t: &Tokens) {
    let seq_seconds = seq.duration().seconds();
    let visible_seconds = view_w.max(1.0) as f64 / app.ui.timeline.pps.max(1e-9);
    let span = minimap_span(app, seq_seconds);
    draw_position_indicator(app, ui, bar, span, visible_seconds, t);
}

/// Compose the zoom bar with its minimap layers. The zoom-bar module itself stays independent of
/// minimap code, so callers can render the bar alone when the minimap feature is unavailable.
pub(in crate::panels::timeline) fn zoom_scrollbar_with_minimap(
    app: &mut FilmcraftApp,
    ui: &mut egui::Ui,
    bar: Rect,
    view_w: f32,
    seq: &Sequence,
    frame_seconds: f64,
    t: &Tokens,
) {
    super::zoom_bar::zoom_scrollbar_with_layers(
        app,
        ui,
        super::zoom_bar::ZoomBarSurface { bar, view_w, seq, frame_seconds, tokens: t },
        draw_clips_for_sequence,
        draw_position_for_sequence,
    );
}

/// Settings and the adjacent flyout for the Timeline minimap.
pub(super) fn settings_rows(
    auto: &mut crate::automation::Registry,
    ui: &mut egui::Ui,
    settings: &mut filmcraft_project::TimelineUiSettings,
    content_w: f32,
    content_right: f32,
    t: &Tokens,
) -> bool {
    const CONTENT_W: f32 = 257.4;
    let sw_w = 34.0;
    let mut changed = false;
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
    auto.add("timeline.zoomBar.settings.sliderMinimap", c.rect, tl!("Slider Minimap"));
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
                auto.add("timeline.zoomBar.settings.showWholeTimeline", c.rect, tl!("Show Whole Timeline"));
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
                auto.add("timeline.zoomBar.settings.prioritiseAudio", c.rect, tl!("Prioritise Audio"));
                if c.clicked() {
                    pri = !pri;
                }
                // Position Indicator: the "you are here" line on the slider. Off: hidden.
                let c = settings_pill_row(ui, t, CONTENT_W, tl!("Position Indicator"), egui::Id::unique("timeline-zoombar-minimap-pos-ind"), pos_ind, None);
                auto.add("timeline.zoomBar.settings.positionIndicator", c.rect, "Position Indicator");
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
                auto.add("timeline.zoomBar.settings.fillClips", c.rect, tl!("Fill Clips"));
                if c.clicked() {
                    fill = !fill;
                }
                // Position Indicator Color: the colour of the "you are here" line. One call --
                // copy this line for every future colour setting.
                changed |= color_setting(
                    ui,
                    auto,
                    t,
                    &mut settings.position_indicator_color,
                    ColorSetting {
                        automation_id: "timeline.zoomBar.settings.positionIndicatorColor",
                        id: egui::Id::unique("timeline-zoombar-minimap-color"),
                        label: tl!("Position Indicator Color:"),
                        default: "#5895ec",
                    },
                );
                changed |= color_setting(
                    ui,
                    auto,
                    t,
                    &mut settings.video_indicator_color,
                    ColorSetting {
                        automation_id: "timeline.zoomBar.settings.videoIndicatorColor",
                        id: egui::Id::unique("timeline-zoombar-minimap-video-color"),
                        label: tl!("Video Indicator Color:"),
                        default: "#64b5f6",
                    },
                );
                changed |= color_setting(
                    ui,
                    auto,
                    t,
                    &mut settings.audio_indicator_color,
                    ColorSetting {
                        automation_id: "timeline.zoomBar.settings.audioIndicatorColor",
                        id: egui::Id::unique("timeline-zoombar-minimap-audio-color"),
                        label: tl!("Audio Indicator Color:"),
                        default: "#2e7d32",
                    },
                );
                changed |= color_setting(
                    ui,
                    auto,
                    t,
                    &mut settings.offline_indicator_color,
                    ColorSetting {
                        automation_id: "timeline.zoomBar.settings.offlineIndicatorColor",
                        id: egui::Id::unique("timeline-zoombar-minimap-offline-color"),
                        label: tl!("Offline Indicator Color:"),
                        default: "#d32f2f",
                    },
                );
            });
        });
        auto.add("timeline.zoomBar.settings.minimapFlyout", panel.response.rect, "Minimap Settings");
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
    changed
}
pub(super) fn minimap_span(app: &FilmcraftApp, seq_seconds: f64) -> f64 {
    if app.session.state.timeline_ui.show_whole_timeline {
        timeline_extent(seq_seconds).max(1e-9)
    } else if seq_seconds.is_finite() && seq_seconds > 0.0 {
        seq_seconds
    } else {
        1e-9
    }
}

/// Draw the visible-window marker against the minimap's fixed linear time span.
pub(super) fn draw_position_indicator(app: &FilmcraftApp, ui: &egui::Ui, bar: Rect, minimap_span: f64, vis: f64, t: &Tokens) {
    let p = ui.painter();
    let minimap_x = |t: f64| -> f32 {
        let t = if t.is_finite() { t.clamp(0.0, minimap_span) } else { 0.0 };
        (bar.min.x as f64 + (t / minimap_span) * f64::from(bar.width())) as f32
    };
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
        // The configured position colour distinguishes the view marker from clip indicators. The
        // theme text outline keeps this narrow marker visible on both light and dark surfaces.
        let marker = parse_hex_color(&app.session.state.timeline_ui.position_indicator_color, DEFAULT_POS_COLOR);
        let (my0, my1) =
            if app.session.state.timeline_ui.alternate_slider_position { (bar.max.y + 1.0, bar.max.y + 3.0) } else { (bar.min.y - 3.0, bar.min.y - 1.0) };
        let marker_rect = Rect::from_min_max(pos2(x0, my0), pos2(x1, my1));
        p.rect_filled(marker_rect, 0.0, marker);
        p.rect_stroke(marker_rect, 0.0, Stroke::new(1.0 / ui.ctx().pixels_per_point().max(1.0), t.text), StrokeKind::Outside);
    }
}

pub(super) fn draw_minimap_clips(app: &FilmcraftApp, ui: &egui::Ui, bar: Rect, seq: &Sequence, minimap_span: f64, t: &Tokens) {
    let p = ui.painter();
    // Below 25% of the SCREEN's width the minimap's clip indicators disappear (they become
    // unreadable that narrow): a fresh decision every frame from the bar's width alone, so it can
    // never stick. The window's height never enters into it. The position indicator stays.
    let minimap_room = super::minimap::minimap_clips_fit(bar.width(), ui.ctx().content_rect().width());
    if app.session.state.timeline_ui.slider_minimap && minimap_room {
        // One class per x column: 0 empty, 1 the lower-priority kind, 2 the higher, 3 offline
        // media (always on top, whatever Prioritise Audio says). The covered kind's rectangle
        // simply ends where the winner begins. Drawn after the thumb (slightly transparent), so
        // clips never disappear behind the slider.
        let cols = bar.width().max(1.0).ceil() as usize;
        let pri_audio = app.session.state.timeline_ui.prioritise_audio;
        let (class, cuts) = super::minimap::minimap_coverage(&app.session.project, seq, minimap_span, bar.min.x, bar.width(), cols, pri_audio);
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
                if c < cols && cuts[c] {
                    break;
                }
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
            (xa, xb) = super::minimap::minimap_indicator_bounds(xa, xb, bar.min.x + ins + sh + cap_r, bar.max.x - ins - sh - cap_rr, hug_left, hug_right);
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
            let (hug_left, hug_right) = super::minimap::minimap_indicator_hugs_edges(index, runs.len(), x0 - bar.min.x, bar.max.x - x1, rad);
            let col = color_for(k);
            let is_filled = super::minimap::minimap_indicator_filled(fill, x1 - x0, 5.0 * px, hug_left, hug_right);
            if is_filled {
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
            let bounds = Rect::from_min_max(pos2(x0, y0), pos2(x1, y1));
            let ink = if is_filled { super::minimap::minimap_pattern_ink(col) } else { t.text };
            for line in super::minimap::minimap_pattern_segments(bounds, k, pri_audio, px) {
                p.line_segment(line, Stroke::new(px, ink));
            }
        }
    }
}
/// Whether the minimap's clip indicators fit on screen: hidden below a quarter of the SCREEN's
/// width (the height never matters). A pure predicate, decided fresh every frame.
pub(super) fn minimap_clips_fit(bar_w: f32, screen_w: f32) -> bool {
    bar_w >= 0.25 * screen_w.max(1.0)
}

/// Keep the straight side of an edge indicator outside the full semicircle's tangent.
pub(super) fn minimap_indicator_bounds(mut xa: f32, mut xb: f32, left_tangent: f32, right_tangent: f32, hug_left: bool, hug_right: bool) -> (f32, f32) {
    if hug_left {
        xb = xb.max(left_tangent);
    }
    if hug_right {
        xa = xa.min(right_tangent);
    }
    (xa, xb)
}

/// Give an end cap only to the outermost run near that edge, so adjacent runs keep their cut.
pub(super) fn minimap_indicator_hugs_edges(index: usize, run_count: usize, left_gap: f32, right_gap: f32, cap_zone: f32) -> (bool, bool) {
    (index == 0 && left_gap < cap_zone, index + 1 == run_count && right_gap < cap_zone)
}

/// Fill narrow interior indicators, but keep edge indicators outlined unless Fill Clips is enabled.
pub(super) fn minimap_indicator_filled(fill_clips: bool, width: f32, min_outline_width: f32, hug_left: bool, hug_right: bool) -> bool {
    fill_clips || (width < min_outline_width && !hug_left && !hug_right)
}

/// Accumulate clip boundaries so overlapping long clips cost constant work each, rather than
/// repainting every covered column for every clip.
pub(super) fn minimap_add_span(boundaries: &mut [[i64; 3]], endpoints: &mut [[[[usize; 2]; 3]; 2]], start: usize, end: usize, priority: usize, kind: usize) {
    let cols = boundaries.len().saturating_sub(1);
    let start = start.min(cols);
    let end = end.min(cols);
    if start >= end || !(1..=3).contains(&priority) || kind >= 2 {
        return;
    }
    boundaries[start][priority - 1] += 1;
    boundaries[end][priority - 1] -= 1;
    endpoints[start][kind][priority - 1][0] = endpoints[start][kind][priority - 1][0].saturating_add(1);
    endpoints[end][kind][priority - 1][1] = endpoints[end][kind][priority - 1][1].saturating_add(1);
}

/// A join is visible only when same-kind clips meet there and no same-kind clip crosses it.
pub(super) fn minimap_cuts(classes: &[u8], endpoints: &[[[[usize; 2]; 3]; 2]]) -> Vec<bool> {
    let mut active = [[0usize; 3]; 2];
    endpoints
        .iter()
        .enumerate()
        .map(|(boundary, by_kind)| {
            let mut visible_cut = false;
            for (kind_active, kind_endpoints) in active.iter_mut().zip(by_kind) {
                for (priority, (active_count, &[starts, ends])) in kind_active.iter_mut().zip(kind_endpoints).enumerate() {
                    let crossing = (*active_count).saturating_sub(ends);
                    if boundary > 0 && classes.get(boundary - 1) == Some(&((priority + 1) as u8)) && classes.get(boundary) == Some(&((priority + 1) as u8)) {
                        visible_cut |= starts > 0 && ends > 0 && crossing == 0;
                    }
                    *active_count = crossing.saturating_add(starts);
                }
            }
            boundary + 1 < endpoints.len() && visible_cut
        })
        .collect()
}

pub(super) fn minimap_classes(boundaries: &[[i64; 3]]) -> Vec<u8> {
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

/// Return the minimap's priority class per column and the visible edit cuts for a sequence.
/// Keeping clip-to-column mapping and cut suppression together lets the regression exercise the
/// same coverage path that the Timeline paints.
pub(super) fn minimap_coverage(
    project: &Project,
    seq: &Sequence,
    span_seconds: f64,
    minimap_left: f32,
    minimap_width: f32,
    cols: usize,
    prioritise_audio: bool,
) -> (Vec<u8>, Vec<bool>) {
    let cols = cols.max(1);
    let span_seconds = if span_seconds.is_finite() && span_seconds > 0.0 { span_seconds } else { 1e-9 };
    let mut boundaries = vec![[0i64; 3]; cols.saturating_add(1)];
    let mut endpoints = vec![[[[0usize; 2]; 3]; 2]; cols.saturating_add(1)];
    for (tracks, is_audio) in [(&seq.video_tracks, false), (&seq.audio_tracks, true)] {
        for track in tracks {
            for item in &track.items {
                let offline = project.item(item.item).and_then(|project_item| project_item.as_media()).is_some_and(|media| media.offline);
                let priority = if offline {
                    3
                } else if is_audio == prioritise_audio {
                    2
                } else {
                    1
                };
                let minimap_x = |seconds: f64| -> f32 {
                    let seconds = if seconds.is_finite() { seconds.clamp(0.0, span_seconds) } else { 0.0 };
                    (minimap_left as f64 + (seconds / span_seconds) * f64::from(minimap_width)) as f32
                };
                let a = minimap_x(item.start.seconds());
                let b = minimap_x(item.end().seconds());
                let c0 = ((a - minimap_left).floor() as isize).max(0) as usize;
                let c1 = ((b - minimap_left).ceil() as isize).clamp(0, cols as isize) as usize;
                minimap_add_span(&mut boundaries, &mut endpoints, c0, c1, priority, usize::from(is_audio));
            }
        }
    }
    let classes = minimap_classes(&boundaries);
    let cuts = minimap_cuts(&classes, &endpoints);
    (classes, cuts)
}

/// Non-colour clip cues for the tiny minimap: audio uses vertical ticks and offline media uses
/// diagonal hatching. Video remains an uninterrupted bar. Lines stay inset from clip edges so
/// the rounded ends and adjacent cuts remain readable.
pub(super) fn minimap_pattern_segments(rect: Rect, class: u8, prioritise_audio: bool, pixel: f32) -> Vec<[Pos2; 2]> {
    let pixel = pixel.max(f32::EPSILON);
    let left = rect.left() + pixel;
    let right = rect.right() - pixel;
    let top = rect.top() + pixel;
    let bottom = rect.bottom() - pixel;
    let height = bottom - top;
    if class == 0 || right <= left || height <= 0.0 {
        return Vec::new();
    }

    if class == 3 {
        let spacing = 4.0 * pixel;
        let mut x = left - height;
        let mut lines = Vec::new();
        while x < right {
            let x0 = x.max(left);
            let x1 = (x + height).min(right);
            if x1 > x0 {
                lines.push([pos2(x0, top + (x0 - x)), pos2(x1, top + (x1 - x))]);
            }
            x += spacing;
        }
        return lines;
    }

    let is_audio = !((class == 2) != prioritise_audio);
    if !is_audio {
        return Vec::new();
    }
    let spacing = 4.0 * pixel;
    let mut x = left + pixel;
    let mut lines = Vec::new();
    while x < right {
        lines.push([pos2(x, top), pos2(x, bottom)]);
        x += spacing;
    }
    lines
}

/// Pick black or white hatch ink with at least 4.5:1 contrast against any opaque clip colour.
pub(super) fn minimap_pattern_ink(color: Color32) -> Color32 {
    fn linear(channel: u8) -> f32 {
        let value = f32::from(channel) / 255.0;
        if value <= 0.04045 { value / 12.92 } else { ((value + 0.055) / 1.055).powf(2.4) }
    }
    let luminance = 0.2126 * linear(color.r()) + 0.7152 * linear(color.g()) + 0.0722 * linear(color.b());
    if luminance > 0.179 { Color32::BLACK } else { Color32::WHITE }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn minimap_boundaries_match_per_column_priority_for_overlaps() {
        let spans = [(0, 8, 1), (2, 6, 2), (4, 20, 3), (0, 8, 1), (7, 3, 2), (9, 12, 1)];
        let mut boundaries = vec![[0i64; 3]; 9];
        let mut endpoints = vec![[[[0usize; 2]; 3]; 2]; 9];
        let mut expected = vec![0u8; 8];
        for (start, end, priority) in spans {
            minimap_add_span(&mut boundaries, &mut endpoints, start, end, priority, 0);
            for cell in expected.iter_mut().take(end).skip(start) {
                *cell = (*cell).max(priority as u8);
            }
        }
        assert_eq!(minimap_classes(&boundaries), expected);
    }

    #[test]
    fn adjacent_same_class_minimap_clips_keep_cuts_unless_another_clip_crosses() {
        let mut boundaries = vec![[0i64; 3]; 11];
        let mut endpoints = vec![[[[0usize; 2]; 3]; 2]; 11];
        for (start, end) in [(0, 5), (5, 10)] {
            minimap_add_span(&mut boundaries, &mut endpoints, start, end, 2, 0);
        }
        assert_eq!(minimap_classes(&boundaries), vec![2; 10]);
        let classes = minimap_classes(&boundaries);
        assert!(minimap_cuts(&classes, &endpoints)[5], "adjacent clips of the same class keep their edit point");

        minimap_add_span(&mut boundaries, &mut endpoints, 3, 7, 2, 0);
        let classes = minimap_classes(&boundaries);
        assert!(!minimap_cuts(&classes, &endpoints)[5], "a same-kind clip crossing that edit point on another track hides the cut");
    }

    #[test]
    fn minimap_audio_and_offline_clips_have_non_colour_patterns() {
        let rect = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(24.0, 7.0));
        let video = minimap_pattern_segments(rect, 2, false, 1.0);
        let audio = minimap_pattern_segments(rect, 1, false, 1.0);
        let audio_when_prioritised = minimap_pattern_segments(rect, 2, true, 1.0);
        let offline = minimap_pattern_segments(rect, 3, false, 1.0);
        assert!(video.is_empty(), "video stays an uninterrupted bar");
        assert!(!audio.is_empty(), "audio has visible ticks");
        assert!(!audio_when_prioritised.is_empty(), "prioritised audio keeps the same cue");
        assert!(audio.iter().all(|line| (line[0].x - line[1].x).abs() < f32::EPSILON));
        assert!(!offline.is_empty(), "offline media has visible hatching");
        assert!(offline.iter().all(|line| (line[0].x - line[1].x).abs() > f32::EPSILON));
        for line in audio.iter().chain(offline.iter()) {
            assert!(line.iter().all(|point| rect.contains(*point)), "pattern stays inside the clip: {line:?}");
        }
    }

    #[test]
    fn minimap_indicator_and_pattern_ink_keep_contrast_in_all_themes() {
        use super::{DEFAULT_AUDIO_COLOR, DEFAULT_OFFLINE_COLOR, DEFAULT_POS_COLOR, DEFAULT_VIDEO_COLOR};
        use crate::theme::{ThemeKind, Tokens};
        fn luminance(color: egui::Color32) -> f32 {
            fn channel(value: u8) -> f32 {
                let value = f32::from(value) / 255.0;
                if value <= 0.04045 { value / 12.92 } else { ((value + 0.055) / 1.055).powf(2.4) }
            }
            0.2126 * channel(color.r()) + 0.7152 * channel(color.g()) + 0.0722 * channel(color.b())
        }
        fn contrast(a: egui::Color32, b: egui::Color32) -> f32 {
            let (a, b) = (luminance(a), luminance(b));
            (a.max(b) + 0.05) / (a.min(b) + 0.05)
        }
        for kind in [ThemeKind::Dark, ThemeKind::Medium, ThemeKind::Light] {
            let theme = Tokens::for_kind(kind);
            // The position marker keeps its configured blue fill, with this theme's high-contrast
            // text stroke outside it. Test the rendered marker's outline rather than requiring
            // the user's selected fill colour to contrast by itself.
            assert!(contrast(theme.text, theme.tl_track_bg) >= 4.5, "position marker outline and outline-mode pattern on {kind:?}");
        }
        for clip in [DEFAULT_VIDEO_COLOR, DEFAULT_AUDIO_COLOR, DEFAULT_OFFLINE_COLOR] {
            assert!(contrast(minimap_pattern_ink(clip), clip) >= 4.5, "{clip:?}");
        }
        assert_ne!(DEFAULT_POS_COLOR, DEFAULT_VIDEO_COLOR, "the position marker has its own configured colour");
        assert!(luminance(DEFAULT_POS_COLOR) < luminance(DEFAULT_VIDEO_COLOR), "the position marker remains darker than video indicators");
    }

    #[test]
    fn minimap_cut_suppression_uses_covering_tracks_of_the_same_kind() {
        use filmcraft_project::{ItemKind, Label, Project, SequenceSettings, TrackKind};
        use filmcraft_time::{FrameRate, TICKS_PER_SECOND, Tick, TimeRange};

        let mut project = Project::new("Minimap coverage");
        let sequence_id = project.new_sequence("Sequence", SequenceSettings::default(), 2, 2, None);
        let rate = FrameRate::FPS_24;
        let asset = project.add_item("Layer", Label::Iris, ItemKind::AdjustmentLayer { width: 1920, height: 1080, rate, duration: rate.tick_of(240) }, None);
        let mut clip = |kind, start: i64, end: i64| {
            project
                .make_track_item(
                    asset,
                    kind,
                    Tick(start * TICKS_PER_SECOND),
                    TimeRange::new(Tick(start * TICKS_PER_SECOND), Tick((end - start) * TICKS_PER_SECOND)),
                    rate,
                )
                .expect("the fixture clip is valid")
        };
        let left = clip(TrackKind::Video, 0, 5);
        let right = clip(TrackKind::Video, 5, 10);
        let crossing_audio = clip(TrackKind::Audio, 3, 7);
        let touching_video = clip(TrackKind::Video, 3, 5);
        let crossing_video = clip(TrackKind::Video, 3, 7);
        let sequence = project.sequence_mut(sequence_id).unwrap();
        sequence.video_tracks[0].items = vec![left, right];
        sequence.audio_tracks[0].items = vec![crossing_audio];

        let (classes, cuts) = minimap_coverage(&project, project.sequence(sequence_id).unwrap(), 10.0, 0.0, 10.0, 10, false);
        assert!(classes.iter().all(|class| *class > 0), "both video clips cover the map");
        assert!(cuts[5], "a clip on another kind of track crossing the edit point does not hide the cut");

        project.sequence_mut(sequence_id).unwrap().video_tracks[1].items.push(touching_video);
        let (_, cuts) = minimap_coverage(&project, project.sequence(sequence_id).unwrap(), 10.0, 0.0, 10.0, 10, false);
        assert!(cuts[5], "a later same-kind track that only touches the edit point does not hide the cut");

        project.sequence_mut(sequence_id).unwrap().video_tracks[1].items = vec![crossing_video];
        let (_, cuts) = minimap_coverage(&project, project.sequence(sequence_id).unwrap(), 10.0, 0.0, 10.0, 10, false);
        assert!(!cuts[5], "a clip on a later video track crossing the edit point hides the cut");
    }

    /// The minimap's clips vanish below a quarter of the screen's width -- exactly at the quarter
    /// they still show -- and nothing else (the width alone decides).
    #[test]
    fn minimap_clips_vanish_below_a_quarter_of_the_screen() {
        assert!(!minimap_clips_fit(249.0, 1000.0), "a hair under the quarter is hidden");
        assert!(minimap_clips_fit(250.0, 1000.0), "exactly the quarter shows");
        assert!(minimap_clips_fit(800.0, 1000.0));
        assert!(!minimap_clips_fit(0.0, 1000.0));
        // a hostile screen width can never divide by zero or flip the answer
        assert!(minimap_clips_fit(10.0, 0.0) == minimap_clips_fit(10.0, 1.0));
    }

    #[test]
    fn minimap_edge_caps_keep_full_semicircles_for_short_runs() {
        // Even a sub-pixel run at an extreme gets its straight edge moved to the circle tangent.
        assert_eq!(minimap_indicator_bounds(20.0, 20.2, 24.0, 76.0, true, false), (20.0, 24.0));
        assert_eq!(minimap_indicator_bounds(79.8, 80.0, 24.0, 76.0, false, true), (76.0, 80.0));
        // Interior runs and runs already beyond the tangent keep their existing bounds.
        assert_eq!(minimap_indicator_bounds(40.0, 45.0, 24.0, 76.0, false, false), (40.0, 45.0));
        assert_eq!(minimap_indicator_bounds(20.0, 30.0, 24.0, 76.0, true, false), (20.0, 30.0));
    }

    #[test]
    fn minimap_only_outermost_short_runs_claim_edge_caps() {
        assert_eq!(minimap_indicator_hugs_edges(0, 2, 1.0, 50.0, 10.0), (true, false));
        assert_eq!(minimap_indicator_hugs_edges(1, 2, 5.0, 1.0, 10.0), (false, true));
        assert_eq!(minimap_indicator_hugs_edges(1, 3, 5.0, 50.0, 10.0), (false, false));
        assert_eq!(minimap_indicator_hugs_edges(0, 1, 1.0, 1.0, 10.0), (true, true));
    }

    #[test]
    fn minimap_semicircles_stay_outlined_when_extremely_narrow() {
        assert!(!minimap_indicator_filled(false, 0.5, 3.0, true, false));
        assert!(!minimap_indicator_filled(false, 0.5, 3.0, false, true));
        assert!(minimap_indicator_filled(true, 0.5, 3.0, true, false));
        assert!(minimap_indicator_filled(false, 0.5, 3.0, false, false));
    }
}
