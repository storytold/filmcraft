use super::{Align2, Color32, CursorIcon, Drag, FilmcraftApp, Pos2, Rect, Sense, Stroke, StrokeKind, Tokens, pos2, vec2};
use crate::icons::{self, Icon};
use crate::widgets::{HotNumberOptions, hot_number_numeric};
use filmcraft_project::{Project, Sequence};

mod minimap;
mod slider_controls;
mod track_sliders;
mod zoom_bar;
pub use slider_controls::*;
pub(super) use slider_controls::{release_slider_cursor, slider_cursor_release_target};
pub use track_sliders::set_track_slider_hidden;
pub(in crate::panels::timeline) use track_sliders::{
    TrackSliderLayout, animate_track_sliders, clamp_track_animation_scroll_targets, hidden_track_slider_settings_row, hidden_track_slider_unhide_row,
    reconcile_track_slider_settings, resize_tracks_from_timeline, track_slider_column, track_slider_settings_popup, track_slider_state_reset,
};

/// Dispatch the shared Controls capture to the slider whose flyout began listening.
pub(crate) fn capture_controls(app: &mut FilmcraftApp, ctx: &egui::Context) {
    let track = ctx.data(|data| data.get_temp::<bool>(slider_controls::controls_listen_track_key()).unwrap_or(false));
    let profile = if track { slider_controls::track_controls_profile() } else { slider_controls::zoom_controls_profile() };
    slider_controls::capture_controls_for_profile(app, ctx, profile);
}

/// Draw the zoom bar with the minimap layer when enabled; the Timeline stays independent of the
/// minimap compositor and the zoom bar itself remains usable without those layers.
pub(in crate::panels::timeline) fn draw_zoom_scrollbar(
    app: &mut FilmcraftApp,
    ui: &mut egui::Ui,
    bar: Rect,
    view_w: f32,
    seq: &Sequence,
    frame_seconds: f64,
    tokens: &Tokens,
) {
    if app.session.state.timeline_ui.slider_minimap {
        minimap::zoom_scrollbar_with_minimap(app, ui, bar, view_w, seq, frame_seconds, tokens);
    } else {
        zoom_bar::zoom_scrollbar(app, ui, bar, view_w, seq, frame_seconds, tokens);
    }
}

/// Route zoom/common controls first, then let the side-slider layer claim an unmatched binding.
pub fn route_slider_shortcut(app: &FilmcraftApp, ctx: &egui::Context) {
    slider_controls::route_slider_shortcut_with_track(
        app,
        ctx,
        slider_controls::track_shortcut_available_at,
        slider_controls::route_track_shortcut_text,
        slider_controls::track_still_owns_alternate_drag,
    );
}

/// Route a floating viewport's shortcut through the same layered control path.
pub fn route_forwarded_slider_shortcut(app: &FilmcraftApp, ctx: &egui::Context, events: &mut Vec<egui::Event>, modifiers: egui::Modifiers) {
    slider_controls::route_forwarded_slider_shortcut_with_track(
        app,
        ctx,
        events,
        modifiers,
        slider_controls::track_shortcut_available_at,
        slider_controls::route_track_shortcut_text,
    );
}

/// Refresh the bindings for each Timeline slider feature.
pub fn sync_slider_bindings(app: &mut FilmcraftApp) {
    slider_controls::sync_zoom_slider_bindings(app);
    slider_controls::sync_track_slider_bindings(app);
}

/// Shared hidden cursor for zoom-bar and track-slider pointer capture.
pub(super) fn transparent_cursor() -> egui::CustomCursorImage {
    egui::CustomCursorImage { rgba: [0, 0, 0, 0].into(), size: [1, 1], hotspot: [0, 0] }
}

/// The narrowest the zoom scroll bar's thumb gets: room for its two handles and a part between
/// them to grab.
const MIN_THUMB_W: f32 = 34.0;

/// The zoom scroll bar's settings popup content. Opened by right-clicking the bar, and by the
/// Timeline menu's "Timeline Slider Settings" row while the bar is hidden -- the same settings,
/// the same way, in both places. The popup only becomes scrollable when the window is too small
/// to show it whole (the height cap is the screen's, so at a normal window size nothing changes).
pub(super) fn zoombar_settings(app: &mut FilmcraftApp, ui: &mut egui::Ui) {
    settings_menu(app, ui, "timeline-zoombar", zoombar_settings_rows);
}

fn zoombar_settings_rows(app: &mut FilmcraftApp, ui: &mut egui::Ui) {
    zoom_bar::zoombar_settings_rows(app, ui, track_sliders::zoom_settings_row, minimap::settings_rows);
}

/// Commit a deferred settings change even if its popup stopped drawing during the gesture.
pub fn flush_pending_zoombar_view_change(app: &mut FilmcraftApp, ctx: &egui::Context) {
    let key = egui::Id::unique("tl-zoombar-view-pending");
    if ctx.data(|d| d.get_temp::<bool>(key)).unwrap_or(false) && !ctx.input(|i| i.pointer.any_down()) {
        ctx.data_mut(|d| d.remove_temp::<bool>(key));
        app.session.project_view_changed();
    }
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

/// Forget every transient of the settings popups (the Minimap flyout, the colour pickers and
/// their keep-alives): the next time the menu opens it is in its default state, never with a
/// picker or flyout left over from the last time.
pub(super) fn zoombar_state_reset(ctx: &egui::Context) {
    minimap::state_reset(ctx);
    let generation_key = settings_generation_key("timeline-zoombar");
    ctx.data_mut(|d| {
        d.insert_temp(color_open_key(), None::<egui::Id>);
        d.insert_temp(controls_listen_key(), None::<usize>);
        d.insert_temp(controls_listen_track_key(), false);
        d.insert_temp(color_rect_key(), Rect::NOTHING);
        d.insert_temp(grace_key(), -1e9f64);
        d.insert_temp(egui::Id::unique("tl-zoombar-family"), Vec::<Rect>::new());
        d.insert_temp(egui::Id::unique("tl-zoombar-family-prev"), Vec::<Rect>::new());
        // a fresh opening: the settings menu must start scrolled to its bottom again
        let generation = d.get_temp::<u64>(generation_key).unwrap_or(0).wrapping_add(1);
        d.insert_temp(generation_key, generation);
        for id in ["timeline-zoombar-advanced-flyout", "timeline-track-slider-advanced-flyout"] {
            d.insert_temp(egui::Id::unique(id).with("rect"), Rect::NOTHING);
            d.insert_temp(egui::Id::unique(id).with("stamp"), -1e9f64);
            d.insert_temp(egui::Id::unique(id).with("height"), 110.0f32);
            d.insert_temp(egui::Id::unique(id).with("open"), false);
        }
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
struct ColorSetting<'a> {
    automation_id: &'a str,
    id: egui::Id,
    label: &'a str,
    default: &'a str,
}

fn color_setting(ui: &mut egui::Ui, auto: &mut crate::automation::Registry, t: &Tokens, color: &mut String, setting: ColorSetting<'_>) -> bool {
    let ColorSetting { automation_id, id, label, default } = setting;
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
    auto.add_response(&format!("{automation_id}.swatch"), &resp, &format!("{label} swatch"));
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
    auto.add_response(&format!("{automation_id}.reset"), &rresp, &format!("{label} Reset"));
    icons::paint(ui.painter(), rr.shrink(2.0), Icon::Reset, if rresp.hovered() { t.text } else { t.text_dim });
    if rresp.clicked() {
        *color = default.to_string();
        changed = true;
        ui.data_mut(|d| d.remove_temp::<String>(id.with("hex")));
    }
    // one colour picker open at a time, remembered under a shared key so opening the menu afresh
    // can clear it
    let open_now = ui.data(|d| d.get_temp::<Option<egui::Id>>(color_open_key())).flatten() == Some(id);
    if resp.clicked() {
        ui.data_mut(|d| d.insert_temp(color_open_key(), if open_now { None } else { Some(id) }));
    }
    if !open_now {
        // A closed picker must start from the current setting value when reopened. This also
        // discards text made stale by a Reset click after the picker was closed.
        ui.data_mut(|d| d.remove_temp::<String>(id.with("hex")));
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
                    auto.add_response(&format!("{automation_id}.wheel"), &resp, &format!("{label} color wheel"));
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
                    auto.add_response(&format!("{automation_id}.hex"), &edit, &format!("{label} hex value"));
                    hex_focus = edit.has_focus();
                    if edit.changed() {
                        let digits = normalize_hex_edit(ui.ctx(), edit.id, &mut buf);
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

// the zoom bar's gestures are one state machine (modes, cursor pin, settings popups, minimap);
// splitting it would separate state that must stay in sync within a single frame
#[allow(clippy::cognitive_complexity)]
/// Close the zoom bar's own settings popup (the one opened by right-clicking the bar). The two
/// sliders' settings menus are mutually exclusive: opening one takes the other with it.
pub(super) fn close_zoombar_popup(ctx: &egui::Context) {
    ctx.data_mut(|data| data.insert_temp(zoom_bar::zoombar_popup_open_key(), false));
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
        zoombar_state_reset(ctx);
        if matches!(app.tl.drag, Some(Drag::ZoomBar { .. })) {
            app.tl.drag = None;
        }
        app.tl.zoom_anchor = None;
        app.tl.zoombar_cursor_lock = None;
        app.tl.zoombar_unlock_catchup = false;
        app.tl.zoombar_point_cursor = false;
        app.tl.zoombar_resize_cursor = false;
        release_slider_cursor(ctx, app.tl.zoombar_cursor_viewport.take());
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
    let owner = slider_cursor_release_target(tl.zoombar_cursor_viewport, ctx.viewport_id());
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
        release_slider_cursor(ctx, tl.zoombar_cursor_viewport.take());
    }
}

pub(super) fn track_min_height(window_h: f32, slots: f32) -> f32 {
    track_sliders::track_min_height(window_h, slots)
}

/// Placement shared by the Timeline's scrollbar band and the side track sliders.
#[derive(Clone, Copy)]
pub(super) struct TimelineScrollbarPlacement {
    pub bar_hidden: bool,
    pub tracks_hidden: bool,
    pub side_left: bool,
    pub side_off: f32,
    pub bar_top: bool,
    pub top_off: f32,
    pub bottom_off: f32,
}

pub(super) fn timeline_scrollbar_placement(app: &FilmcraftApp) -> TimelineScrollbarPlacement {
    let bar_hidden = app.session.state.timeline_ui.zoom_bar_hidden;
    let (tracks_hidden, side_left, side_off) = track_slider_column(app);
    let bar_top = !bar_hidden && app.session.state.timeline_ui.alternate_slider_position;
    TimelineScrollbarPlacement {
        bar_hidden,
        tracks_hidden,
        side_left,
        side_off,
        bar_top,
        top_off: if bar_top { super::SCROLLBAR_H } else { 0.0 },
        bottom_off: if !bar_hidden && !bar_top { super::SCROLLBAR_H } else { 0.0 },
    }
}

/// Apply the track-slider height adjustment requested from its Controls binding.
pub(super) fn resize_track_heights_from_control(
    app: &mut FilmcraftApp,
    ctx: &egui::Context,
    video_area: Rect,
    audio_area: Rect,
    split_y: f32,
    video_slots: f32,
    audio_slots: f32,
    bar_top: bool,
    dy: f32,
) {
    let outer = if bar_top { 5.0 } else { 2.0 };
    let timeline = &mut app.ui.timeline;
    resize_tracks_from_timeline(
        &mut timeline.video_track_h,
        &mut timeline.v_scroll,
        video_area.height(),
        video_slots,
        (video_area.height() - outer - 3.0).max(1.0),
        dy,
    );
    resize_tracks_from_timeline(
        &mut timeline.audio_track_h,
        &mut timeline.a_scroll,
        audio_area.height(),
        audio_slots,
        (audio_area.max.y - outer - split_y - super::DIVIDER_H - 3.0).max(1.0),
        dy,
    );
    app.tl.track_anim = None;
    ctx.request_repaint();
}

/// Draw the side sliders and merge their pointer affordance into the shared Timeline cursor state.
pub(in crate::panels::timeline) fn draw_track_sliders(app: &mut FilmcraftApp, ui: &mut egui::Ui, layout: TrackSliderLayout, tokens: &Tokens) {
    let on_side = track_sliders::draw_track_sliders(app, ui, layout, tokens);
    app.tl.zoombar_point_cursor |= on_side;
}

#[cfg(test)]
mod controls_tests {
    use super::{SHIFT_CONTENT_RATE, SHIFT_THUMB_RATE, matches_gesture, shift_drag_rate, slider_action_triggered};

    #[test]
    fn rebound_alternate_drag_chords_and_mouse_buttons_drive_their_actions() {
        let ctx = egui::Context::default();
        let modifiers = egui::Modifiers { ctrl: true, ..Default::default() };
        ctx.begin_pass(egui::RawInput {
            events: vec![
                egui::Event::ModifiersChanged(modifiers),
                egui::Event::Key { key: egui::Key::U, physical_key: None, pressed: true, repeat: false, modifiers },
            ],
            ..Default::default()
        });
        assert!(super::alternate_down(&ctx, "Ctrl + U"), "the captured replacement chord enables Alternate Drag Speed");
        assert!(!super::alternate_down(&ctx, "Ctrl + J"), "a different chord does not enable it");
        let mut output = ctx.end_pass();
        output.textures_delta.clear();

        ctx.begin_pass(egui::RawInput {
            events: vec![
                egui::Event::PointerMoved(egui::pos2(20.0, 20.0)),
                egui::Event::PointerButton {
                    pos: egui::pos2(20.0, 20.0),
                    button: egui::PointerButton::Middle,
                    pressed: true,
                    modifiers: egui::Modifiers::NONE,
                },
            ],
            ..Default::default()
        });
        assert!(super::alternate_down(&ctx, "Middle Click"), "an accepted middle-button binding stays active while held");
        assert!(
            super::slider_action_triggered(None, 0, "Middle Click", "Double Click", Some("Middle Click")),
            "a captured mouse-button binding triggers its action"
        );
        let mut output = ctx.end_pass();
        output.textures_delta.clear();
    }

    #[test]
    fn mac_shift_drag_label_activates_with_shift_held() {
        let ctx = egui::Context::default();
        let input = egui::RawInput { events: vec![egui::Event::ModifiersChanged(egui::Modifiers { shift: true, ..Default::default() })], ..Default::default() };
        ctx.begin_pass(input);
        assert!(super::alternate_down(&ctx, "⇧Drag"));
        assert!(!super::alternate_down(&ctx, "⌥Drag"));
        let mut output = ctx.end_pass();
        output.textures_delta.clear();
    }

    #[test]
    fn unbound_controls_disable_their_original_gesture() {
        assert!(super::uses_default_gesture("", "Shift + Drag"));
        assert!(super::uses_default_gesture("Shift + Drag", "Shift + Drag"));
        assert!(super::uses_default_gesture("⇧Drag", "Shift + Drag"));
        assert!(!super::uses_default_gesture("Unbound", "Shift + Drag"));
        assert!(!super::uses_default_gesture("Ctrl + Drag", "Shift + Drag"));
        assert!(super::uses_default_gesture("", "Double Click"));
        assert!(!matches_gesture("Unbound", "Right Click", Some("Right Click")));
        assert!(matches_gesture("", "Double Click", Some("Double Click")));
        assert!(matches_gesture("⇧Double Click", "Double Click", Some("Shift + Double Click")));
        assert!(!matches_gesture("Ctrl + K", "Double Click", Some("Double Click")));
    }

    #[test]
    fn routed_key_action_precedes_pointer_gesture_modifier_matching() {
        assert!(slider_action_triggered(Some(0), 0, "Ctrl + K", "Double Click", Some("Shift + Double Click")));
        assert!(!slider_action_triggered(None, 0, "Ctrl + K", "Double Click", Some("Shift + Double Click")));
    }

    /// The Alternate Drag Speed setting squares: 1x keeps the slow rate, 0.5x a quarter of it and
    /// Custom 10x a hundred times it -- nothing caps at 1x.
    #[test]
    fn the_speed_setting_squares_and_never_caps_at_one() {
        let one = shift_drag_rate(1.0);
        assert!((shift_drag_rate(0.5) - one * 0.25).abs() < 1e-9);
        assert!((shift_drag_rate(10.0) - one * 100.0).abs() < 1e-9);
        assert!(shift_drag_rate(10.0) > shift_drag_rate(1.0));
        // hostile settings fall back to a slow rate, never zero or NaN
        assert!(shift_drag_rate(f32::NAN) > 0.0);
        assert!(shift_drag_rate(0.0) > 0.0);
    }

    /// Both Shift rates are fixed constants times the same setting term, so a 12 track video
    /// slider and a 3 track audio slider move 1:1 with each other -- there is no per-slider input
    /// left to skew them -- and the scroll rate is a little over three times the height rate.
    #[test]
    fn the_shift_rates_are_constants_so_the_sliders_feel_one_to_one() {
        let one = shift_drag_rate(1.0);
        let ten = shift_drag_rate(10.0);
        assert!(SHIFT_THUMB_RATE * one > 0.0 && SHIFT_CONTENT_RATE * one > 0.0);
        assert!((SHIFT_CONTENT_RATE * one / (SHIFT_THUMB_RATE * one) - 10.0 / 3.0).abs() < 0.01);
        // the setting scales both rates together, never one slider's alone
        assert!((SHIFT_CONTENT_RATE * ten / (SHIFT_CONTENT_RATE * one) - 100.0).abs() < 1e-6);
    }
}

#[cfg(test)]
mod zoom_limit_geometry_tests {
    use super::*;

    #[test]
    fn settings_popup_does_not_clamp_using_the_previous_frame_width() {
        let mut app = crate::FilmcraftApp::new(filmcraft_engine::Session::default());
        app.last_timeline_width = 10.0;
        app.ui.timeline.pps = 1_000.0;
        app.ui.timeline.target_pps = 1_000.0;
        app.tl.zoombar_settings_pos = Some(pos2(80.0, 60.0));
        let ctx = egui::Context::default();
        let input = egui::RawInput { screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(1200.0, 900.0))), ..egui::RawInput::default() };
        let mut output = ctx.run_ui(input, |ui| zoombar_settings_popup(&mut app, ui));
        output.textures_delta.clear();
        assert_eq!((app.ui.timeline.pps, app.ui.timeline.target_pps), (1_000.0, 1_000.0));
    }
}

#[cfg(test)]
mod cursor_watchdog_tests {
    use super::zoombar_cursor_watchdog;
    use crate::panels::timeline::TlState;

    #[test]
    fn watchdog_releases_a_stale_pin_without_a_visible_zoom_bar() {
        let ctx = egui::Context::default();
        let mut timeline =
            TlState { zoombar_cursor_lock: Some(egui::pos2(240.0, 180.0)), zoombar_cursor_viewport: Some(ctx.viewport_id()), ..Default::default() };

        zoombar_cursor_watchdog(&mut timeline, &ctx, &filmcraft_project::TimelineUiSettings::default());

        assert_eq!(timeline.zoombar_cursor_lock, None, "a stale pin is released even when there is no active slider drag");
        assert!(timeline.zoombar_unlock_catchup, "the release warp is excluded from the next slider delta");
    }
}

#[cfg(test)]
mod color_picker_tests {
    use super::{ColorSetting, closest_six, color_open_key, color_setting};

    #[test]
    fn closing_a_color_picker_discards_its_stale_hex_buffer() {
        let ctx = egui::Context::default();
        let app = crate::FilmcraftApp::new(filmcraft_engine::Session::default());
        let tokens = app.tokens;
        let mut auto = crate::automation::Registry::default();
        let mut color = "#ff0000".to_owned();
        let id = egui::Id::unique("timeline-color-stale-buffer-test");
        ctx.data_mut(|data| {
            data.insert_temp(color_open_key(), Some(id));
            data.insert_temp(id.with("hex"), "#ab".to_owned());
        });

        let mut output = ctx.run_ui(egui::RawInput::default(), |ui| {
            color_setting(ui, &mut auto, &tokens, &mut color, ColorSetting { automation_id: "timeline.test.color", id, label: "Color", default: "#5895ec" });
        });
        output.textures_delta.clear();
        assert_eq!(ctx.data(|data| data.get_temp::<String>(id.with("hex"))).as_deref(), Some("#ab"));

        ctx.data_mut(|data| data.insert_temp(color_open_key(), None::<egui::Id>));
        let mut output = ctx.run_ui(egui::RawInput::default(), |ui| {
            color_setting(ui, &mut auto, &tokens, &mut color, ColorSetting { automation_id: "timeline.test.color", id, label: "Color", default: "#5895ec" });
        });
        output.textures_delta.clear();
        assert!(ctx.data(|data| data.get_temp::<String>(id.with("hex"))).is_none());
    }

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

#[cfg(test)]
mod zoom_bar_hide_state_tests {
    use super::{set_zoombar_hidden, zoom_bar};

    #[test]
    fn hiding_the_zoom_bar_closes_its_popup_and_clears_flyout_state() {
        let mut app = crate::FilmcraftApp::new(filmcraft_engine::Session::default());
        let ctx = egui::Context::default();
        let minimap_flyout = egui::Id::unique("timeline-zoombar-minimap-flyout");
        ctx.data_mut(|data| {
            data.insert_temp(zoom_bar::zoombar_popup_open_key(), true);
            data.insert_temp(minimap_flyout.with("open"), true);
            data.insert_temp(super::controls_listen_key(), Some(0usize));
        });

        set_zoombar_hidden(&mut app, &ctx, true);

        assert!(!ctx.data(|data| data.get_temp::<bool>(zoom_bar::zoombar_popup_open_key())).unwrap_or(false));
        assert!(!ctx.data(|data| data.get_temp::<bool>(minimap_flyout.with("open"))).unwrap_or(false));
        assert!(!super::controls_listening(&ctx), "hiding the bar cannot leave Controls capturing keys");
    }
}
