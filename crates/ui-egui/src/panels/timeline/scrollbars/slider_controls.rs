use super::{
    Align2, Color32, CursorIcon, Drag, FilmcraftApp, Icon, Pos2, Rect, Sense, Tokens, close_zoombar_popup, family_contains_prev, family_push, grace_key, icons,
    pos2, setting_hint, vec2,
};
#[cfg(test)]
use super::{flush_pending_zoombar_view_change, normalize_hex_edit, zoom_bar, zoombar_settings_popup};
mod track;
pub(super) use track::{
    SHIFT_CONTENT_RATE, SHIFT_THUMB_RATE, available_at as track_shortcut_available_at, controls_listen_track_key, controls_profile as track_controls_profile,
    route_track_shortcut_text, still_owns_alternate_drag as track_still_owns_alternate_drag, sync_track_slider_bindings,
};

/// Release the cursor captured by a Timeline slider and restore the viewport that acquired it.
/// The cursor remains where the user's hand left it.
pub(in crate::panels::timeline) fn release_slider_cursor(ctx: &egui::Context, viewport: Option<egui::ViewportId>) {
    ctx.set_cursor_image(None);
    let command = egui::ViewportCommand::CursorGrab(egui::viewport::CursorGrab::None);
    ctx.send_viewport_cmd_to(slider_cursor_release_target(viewport, ctx.viewport_id()), command);
}

pub(in crate::panels::timeline) fn slider_cursor_release_target(acquired: Option<egui::ViewportId>, current: egui::ViewportId) -> egui::ViewportId {
    acquired.unwrap_or(current)
}

pub(in crate::panels::timeline) fn open_pointer_key() -> egui::Id {
    egui::Id::unique("tl-zoombar-open-pointer")
}

#[cfg(test)]
mod cursor_release_tests {
    use super::slider_cursor_release_target;

    #[test]
    fn cursor_release_targets_the_viewport_that_acquired_it() {
        let acquired = egui::ViewportId::from_hash_of("slider-owner");
        let current = egui::ViewportId::from_hash_of("current-window");
        assert_eq!(slider_cursor_release_target(Some(acquired), current), acquired);
        assert_eq!(slider_cursor_release_target(None, current), current);
    }
}

pub fn pill_switch(ui: &mut egui::Ui, r: Rect, on: bool, t: &Tokens, id: egui::Id, label: &str) -> egui::Response {
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
pub fn settings_pill_row(ui: &mut egui::Ui, t: &Tokens, row_w: f32, label: &str, id: egui::Id, on: bool, hint: Option<&str>) -> egui::Response {
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
/// The width the Controls flyout always has -- the main slider's -- in every menu.
pub const CONTROLS_FLYOUT_W: f32 = 257.4;

/// The Alternate Drag Speed setting squared: 1x is the slow rate, Custom 10x a hundred times it.
pub fn shift_drag_rate(speed: f32) -> f64 {
    let s = f64::from(speed.max(0.01));
    s * s
}

/// Which side a flyout fits on: to the right of the menu when there is room there, otherwise to
/// the left. Computed from geometry alone, so the row's arrow points the right way on the very
/// frame the menu opens -- never a frame of one side and then the other.
pub fn flyout_to_the_right(ui: &egui::Ui, _row: Rect, content_right: f32, flyout_content_w: f32) -> bool {
    let margin = ui.spacing().menu_margin;
    let popup_right = content_right + f32::from(margin.right);
    let flyout_w = flyout_content_w + 16.0 + f32::from(margin.left) + f32::from(margin.right);
    popup_right + 4.0 + flyout_w <= ui.ctx().content_rect().right()
}

/// The little arrow on a Controls / Minimap row: at the same distance from whichever wall the
/// flyout opens on -- 6 pt past the content edge on the right, the mirror of that distance on the
/// left -- pointing at the flyout. (Measured off the popup's content edge, never the row's own
/// width: the switch rows are narrower and used to sit almost against the left wall.)
pub fn controls_arrow(ui: &egui::Ui, row: Rect, menu_w: f32, content_right: f32, to_the_right: bool, color: Color32) {
    let x = if to_the_right { row.min.x + menu_w - 6.0 } else { content_right - menu_w + 6.0 };
    let glyph = if to_the_right { "\u{25b6}" } else { "\u{25c0}" };
    ui.painter().text(pos2(x, row.center().y), Align2::CENTER_CENTER, glyph, Tokens::ui(8.0), color);
}

/// Where a Controls box that is listening for a new input remembers itself.
pub fn controls_listen_key() -> egui::Id {
    egui::Id::unique("tl-controls-listening")
}

/// Whether a Controls box is listening for a new shortcut. While it is, the rest of the
/// application must not hear the key: the shortcut matcher consumes presses from the frame's
/// events (which is also why the capture used to miss them), and a bound key would fire its
/// action in the background while the user is naming a new binding.
pub fn controls_listening(ctx: &egui::Context) -> bool {
    ctx.data(|d| d.get_temp::<Option<usize>>(controls_listen_key())).flatten().is_some()
}

pub fn controls_capturing(ui: &egui::Ui) -> bool {
    controls_listening(ui.ctx()) || ui.data(|d| d.get_temp::<f64>(egui::Id::unique("tl-controls-consumed-frame"))) == Some(ui.input(|i| i.time))
}

/// Capture the next gesture, or unbind after three seconds of inactivity.
pub(crate) fn capture_controls_for_profile(app: &mut FilmcraftApp, ctx: &egui::Context, profile: SliderControlProfile) {
    let key = controls_listen_key();
    let Some(slot) = ctx.data(|d| d.get_temp::<Option<usize>>(key)).flatten() else {
        return;
    };
    let frame_time = ctx.input(|i| i.time);
    ctx.data_mut(|d| d.insert_temp(egui::Id::unique("tl-controls-consumed-frame"), frame_time));
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
        set_slider_binding_for(app, profile, slot, "Unbound");
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
        set_slider_binding_for(app, profile, slot, &text);
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
#[derive(Clone, Copy)]
pub struct SliderControlProfile {
    pub captures_track: bool,
    pub binding: fn(&mut FilmcraftApp, usize) -> &mut String,
    pub command: fn(usize) -> Option<(&'static str, &'static str)>,
    pub reset: fn(&mut FilmcraftApp, usize),
    pub close_for_conflict: fn(&mut FilmcraftApp, &egui::Context),
    pub secondary_binding: Option<(usize, usize)>,
    pub show_control_tracks_angle: bool,
}

fn close_zoom_controls_for_conflict(app: &mut FilmcraftApp, ctx: &egui::Context) {
    app.tl.zoombar_settings_pos = None;
    close_zoombar_popup(ctx);
}

fn reset_zoom_control(app: &mut FilmcraftApp, slot: usize) {
    reset_slider_binding_for(app, zoom_controls_profile(), slot);
}

pub fn zoom_controls_profile() -> SliderControlProfile {
    SliderControlProfile {
        captures_track: false,
        binding: control_binding,
        command: zoom_slider_command,
        reset: reset_zoom_control,
        close_for_conflict: close_zoom_controls_for_conflict,
        secondary_binding: Some((1, 11)),
        show_control_tracks_angle: true,
    }
}

#[cfg(test)]
pub fn binding_conflicts(app: &FilmcraftApp, track: bool, slot: usize, shown: &str) -> bool {
    !conflicting_commands(app, track, slot, shown).is_empty()
}

#[cfg(test)]
pub fn conflicting_commands(app: &FilmcraftApp, track: bool, slot: usize, shown: &str) -> Vec<String> {
    conflicting_commands_for(app, test_slider_profile(track).command, slot, shown)
}

pub fn binding_conflicts_for(app: &FilmcraftApp, command: fn(usize) -> Option<(&'static str, &'static str)>, slot: usize, shown: &str) -> bool {
    !conflicting_commands_for(app, command, slot, shown).is_empty()
}

fn conflicting_commands_for(app: &FilmcraftApp, command: fn(usize) -> Option<(&'static str, &'static str)>, slot: usize, shown: &str) -> Vec<String> {
    let Some((own_command, _)) = command(slot) else { return Vec::new() };
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
pub fn hover_flyout(
    app: &mut FilmcraftApp,
    ui: &mut egui::Ui,
    flyout_id: egui::Id,
    row: Rect,
    content_right: f32,
    content_w: f32,
    open_now: bool,
    profile: SliderControlProfile,
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
        let bound = (profile.binding)(app, slot).clone();
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
        let secondary_slot = profile.secondary_binding.filter(|(for_slot, _)| *for_slot == slot).map(|(_, other_slot)| other_slot);
        let (secondary_width, sequence_binding) = if let Some(other_slot) = secondary_slot {
            let other = (profile.binding)(app, other_slot).clone();
            let text = if listening_width == Some(other_slot) {
                if live.is_empty() { tl!("Enter Shortcut") } else { live.as_str() }
            } else if other.is_empty() {
                "\\"
            } else if other == "Unbound" {
                tl!("Unbound")
            } else {
                other.as_str()
            };
            (12.0 + (ui.painter().layout_no_wrap(text.to_owned(), Tokens::ui(11.0), Color32::WHITE).size().x + 14.0).max(36.0), other)
        } else {
            (0.0, String::new())
        };
        let warning_space = if let Some(other_slot) = secondary_slot {
            if binding_conflicts_for(app, profile.command, other_slot, &sequence_binding) { 80.0 } else { 48.0 }
        } else {
            48.0
        };
        width.max(label + binding + secondary_width + warning_space)
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
    let row_count = items.len() + usize::from(profile.show_control_tracks_angle && app.session.state.timeline_ui.control_tracks);
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
                    let bound = (profile.binding)(app, slot).clone();
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
                    let secondary_slot = profile.secondary_binding.filter(|(for_slot, _)| *for_slot == slot).map(|(_, other_slot)| other_slot);
                    let second = secondary_slot.is_some();
                    let other = secondary_slot.map_or_else(String::new, |other_slot| (profile.binding)(app, other_slot).clone());
                    let other_text = if listening == secondary_slot {
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
                            d.insert_temp(controls_listen_track_key(), profile.captures_track);
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
                        let conflicts = conflicting_commands_for(app, profile.command, warn_slot, warn_text);
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
                                (profile.close_for_conflict)(app, ui.ctx());
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
                        (profile.reset)(app, slot);
                        if let Some(other_slot) = secondary_slot {
                            (profile.reset)(app, other_slot);
                        }
                        app.session.project_view_changed();
                    }
                }
                if profile.show_control_tracks_angle && app.session.state.timeline_ui.control_tracks {
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
pub fn is_modifier_key(key: egui::Key) -> bool {
    use egui::Key::*;
    matches!(key, ShiftLeft | ShiftRight | ControlLeft | ControlRight | AltLeft | AltRight | SuperLeft | SuperRight)
}

pub fn modifier_count(modifiers: egui::Modifiers) -> u8 {
    u8::from(modifiers.shift) + u8::from(modifiers.alt) + u8::from(modifiers.ctrl || modifiers.mac_cmd)
}

pub fn key_binding_name(key: egui::Key) -> String {
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
pub fn chord_and_button(events: &[egui::Event], fallback_modifiers: egui::Modifiers) -> Option<String> {
    if fallback_modifiers.command {
        if events.iter().any(|event| matches!(event, egui::Event::Copy)) {
            return Some("Ctrl + C".into());
        }
        if events.iter().any(|event| matches!(event, egui::Event::Paste(_) | egui::Event::PasteImage(_))) {
            return Some("Ctrl + V".into());
        }
    }
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
pub fn control_binding(app: &mut FilmcraftApp, slot: usize) -> &mut String {
    let s = &mut app.session.state.timeline_ui;
    match slot {
        0 => &mut s.control_center_on_playhead,
        1 => &mut s.control_explode_slider,
        2 => &mut s.control_alternate_drag_speed,
        3 => &mut s.control_settings,
        _ => s.timeline_control_bindings.get_mut(slot.saturating_sub(4)).unwrap_or(&mut s.control_settings),
    }
}

fn zoom_slider_command(slot: usize) -> Option<(&'static str, &'static str)> {
    match slot {
        0 => Some(("timeline.sliderCenter", "Double Click")),
        1 => Some(("timeline.sliderExplode", "Triple Click")),
        2 => Some(("timeline.sliderAlternate", "Shift+Drag")),
        4 => Some(("timeline.adjustTrackHeight", "")),
        5 => Some(("timeline.fastScroll", "")),
        6 => Some(("timeline.scrollTrackRows", "Ctrl+Scroll")),
        7 => Some(("timeline.zoomScroll", "Alt+Scroll")),
        8 => Some(("view.zoomIn", "=")),
        10 => Some(("view.zoomOut", "-")),
        11 => Some(("view.zoomToSequence", "\\")),
        _ => None,
    }
}

fn set_slider_binding_for(app: &mut FilmcraftApp, profile: SliderControlProfile, slot: usize, value: &str) {
    let Some((command, default)) = (profile.command)(slot) else { return };
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
    refresh_slider_binding_for(app, profile, slot);
}

pub fn reset_slider_binding_for(app: &mut FilmcraftApp, profile: SliderControlProfile, slot: usize) {
    let Some((command, _)) = (profile.command)(slot) else { return };
    if let Err(error) = app.session.shortcuts.reset_command_to_default(command) {
        app.ui.status = error;
    }
    refresh_slider_binding_for(app, profile, slot);
}

pub fn refresh_slider_binding_for(app: &mut FilmcraftApp, profile: SliderControlProfile, slot: usize) {
    let Some((id, _)) = (profile.command)(slot) else { return };
    let value = app
        .session
        .shortcuts
        .primary(id)
        .map(|binding| {
            filmcraft_engine::shortcuts::Chord::parse(&binding)
                .map(|chord| chord.display(filmcraft_engine::shortcuts::Platform::current()).replace('+', " + "))
                .unwrap_or_else(|_| binding.split('+').map(str::trim).filter(|part| !part.is_empty()).collect::<Vec<_>>().join(" + "))
        })
        .unwrap_or_else(|| "Unbound".into());
    *(profile.binding)(app, slot) = value;
}

#[cfg(test)]
fn test_slider_profile(track: bool) -> SliderControlProfile {
    if track { track_controls_profile() } else { zoom_controls_profile() }
}

#[cfg(test)]
pub fn set_slider_binding(app: &mut FilmcraftApp, track: bool, slot: usize, value: &str) {
    set_slider_binding_for(app, test_slider_profile(track), slot, value);
}

#[cfg(test)]
pub fn reset_slider_binding(app: &mut FilmcraftApp, track: bool, slot: usize) {
    reset_slider_binding_for(app, test_slider_profile(track), slot);
}

/// Refresh the Timeline's gesture cache from the shared keyboard shortcut set.
pub fn sync_zoom_slider_bindings(app: &mut FilmcraftApp) {
    if app.session.shortcuts.primary("timeline.zoomScroll").is_some_and(|s| s == "Cmd+Shift+Scroll" || s == "Ctrl+Shift+Scroll")
        && let Err(error) = app.session.shortcuts.set("timeline.zoomScroll", "Alt+Scroll", None, false, true)
    {
        app.ui.status = error;
    }
    for slot in [0, 1, 2, 4, 5, 6, 7, 8, 10, 11] {
        refresh_slider_binding_for(app, zoom_controls_profile(), slot);
    }
}
/// Which kind of menu (zoom bar = false, track slider = true) the listening Controls box
/// belongs to, so [`capture_controls`] binds into that slider's own fields.
pub fn controls_listen_document_epoch_key() -> egui::Id {
    egui::Id::unique("tl-controls-listening-document-epoch")
}

/// A track-slider Controls binding that was just completed fires its action, so the user sees
/// the new input work straight away (the Timeline then consumes it). Slots 0/1/3 trigger; slot
/// 2 (Alternate Drag Speed) is a chord for drags, not an action, and triggers nothing here.
pub fn zoom_trigger_key() -> egui::Id {
    egui::Id::unique("tl-controls-zoom-trigger")
}

/// Route assigned slider keys before the application's general shortcut matcher consumes them.
#[cfg(test)]
pub fn route_zoom_slider_shortcut(app: &FilmcraftApp, ctx: &egui::Context) {
    route_slider_shortcut_with_track(app, ctx, no_track_shortcuts_available, no_track_shortcut_route, no_track_alternate_drag_owner);
}

pub fn route_slider_shortcut_with_track(
    app: &FilmcraftApp,
    ctx: &egui::Context,
    track_available: TrackShortcutAvailability,
    route_track: TrackShortcutRouter,
    track_owns_alternate: TrackAlternateDragOwner,
) {
    clear_inactive_alternate_drag_capture_with_track(app, ctx, track_owns_alternate);
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
    route_slider_text_with_track(app, ctx, &mut events, &text, ctx.pointer_hover_pos(), true, track_available, route_track);
    ctx.input_mut(|i| i.events = events);
}

/// Route a floating viewport's forwarded chord after application shortcuts had first refusal.
/// There is no Timeline pointer in that viewport, so availability is based on the owning slider
/// and active sequence rather than its hover rectangle.
pub fn route_forwarded_slider_shortcut_with_track(
    app: &FilmcraftApp,
    ctx: &egui::Context,
    events: &mut Vec<egui::Event>,
    modifiers: egui::Modifiers,
    track_available: TrackShortcutAvailability,
    route_track: TrackShortcutRouter,
) {
    if reconcile_controls_capture(app, ctx) || controls_listening(ctx) || ctx.text_edit_focused() || app.dialog == Some(crate::Dialog::Shortcuts) {
        return;
    }
    let Some(text) = chord_and_button(events, modifiers) else { return };
    route_slider_text_with_track(app, ctx, events, &text, None, false, track_available, route_track);
}

pub fn route_slider_text_with_track(
    app: &FilmcraftApp,
    ctx: &egui::Context,
    events: &mut Vec<egui::Event>,
    text: &str,
    pointer: Option<Pos2>,
    require_pointer: bool,
    track_shortcuts_available: TrackShortcutAvailability,
    route_track: TrackShortcutRouter,
) {
    if text.ends_with("Click") || text.ends_with("Scroll") {
        return;
    }
    let s = &app.session.state.timeline_ui;
    let has_sequence = app.session.state.active_sequence.is_some();
    let over_zoom = !require_pointer
        || pointer.is_some_and(|p| ctx.data(|d| d.get_temp::<Rect>(egui::Id::unique("timeline-zoom-slider-rect"))).is_some_and(|r| r.contains(p)));
    let zoom_available = has_sequence && !s.zoom_bar_hidden && over_zoom;
    let track_available = has_sequence && track_shortcuts_available(app, ctx, pointer, require_pointer);
    let zoom = [&s.control_center_on_playhead, &s.control_explode_slider, &s.control_alternate_drag_speed, &s.control_settings];
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
    // Alternate Drag Speed is a held chord, not a shortcut action. Swallow its key only while
    // the zoom bar is actually being dragged; track-slider capture is handled in its own module.
    let alternate_drag_active =
        matches!(app.tl.drag, Some(Drag::ZoomBar { .. })) && !s.control_alternate_drag_speed.is_empty() && s.control_alternate_drag_speed == text;
    if alternate_drag_active {
        ctx.data_mut(|d| d.insert_temp(alternate_drag_capture_key(), text.to_string()));
        consume_slider_shortcut_events(events);
        ctx.input_mut(|i| i.events = events.to_vec());
        return;
    }
    let matched = zoom_match.or(extra_match);
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
        return;
    }
    route_track(app, ctx, events, text, pointer, require_pointer);
}

pub type TrackShortcutAvailability = fn(&FilmcraftApp, &egui::Context, Option<Pos2>, bool) -> bool;
pub type TrackShortcutRouter = fn(&FilmcraftApp, &egui::Context, &mut Vec<egui::Event>, &str, Option<Pos2>, bool);
pub type TrackAlternateDragOwner = fn(&FilmcraftApp, &str) -> bool;

#[cfg(test)]
fn no_track_shortcuts_available(_: &FilmcraftApp, _: &egui::Context, _: Option<Pos2>, _: bool) -> bool {
    false
}

#[cfg(test)]
fn no_track_shortcut_route(_: &FilmcraftApp, _: &egui::Context, _: &mut Vec<egui::Event>, _: &str, _: Option<Pos2>, _: bool) {}

#[cfg(test)]
fn no_track_alternate_drag_owner(_: &FilmcraftApp, _: &str) -> bool {
    false
}

pub fn consume_slider_shortcut_events(events: &mut Vec<egui::Event>) {
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

pub fn alternate_drag_capture_key() -> egui::Id {
    egui::Id::unique("timeline-active-alternate-drag-chord")
}

pub fn alternate_drag_binding_captured(ctx: &egui::Context, binding: &str) -> bool {
    ctx.data(|d| d.get_temp::<String>(alternate_drag_capture_key()).as_deref() == Some(binding))
}

pub fn clear_inactive_alternate_drag_capture_with_track(app: &FilmcraftApp, ctx: &egui::Context, track_owns_alternate: TrackAlternateDragOwner) {
    let capture = ctx.data(|d| d.get_temp::<String>(alternate_drag_capture_key()));
    let Some(binding) = capture else { return };
    let s = &app.session.state.timeline_ui;
    let still_owned = (matches!(app.tl.drag, Some(Drag::ZoomBar { .. })) && binding == s.control_alternate_drag_speed) || track_owns_alternate(app, &binding);
    let key_name = binding.split(" + ").last().unwrap_or_default();
    let released = egui::Key::from_name(key_name).is_some_and(|key| {
        ctx.input(|i| i.events.iter().any(|event| matches!(event, egui::Event::Key { key: released, pressed: false, .. } if *released == key)))
    });
    if !still_owned || released {
        ctx.data_mut(|d| d.remove_temp::<String>(alternate_drag_capture_key()));
    }
}

/// Stop waiting for a Controls binding when its Timeline context is no longer available.
pub fn clear_controls_capture(ctx: &egui::Context) {
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

pub fn controls_popup_open(app: &FilmcraftApp, ctx: &egui::Context) -> bool {
    let track = ctx.data(|d| d.get_temp::<bool>(controls_listen_track_key()).unwrap_or(false));
    if track {
        app.tl.track_slider_settings_pos.is_some()
    } else {
        app.tl.zoombar_settings_pos.is_some() || ctx.data(|d| d.get_temp::<bool>(super::zoom_bar::zoombar_popup_open_key())).unwrap_or(false)
    }
}

#[cfg(test)]
mod slider_clipboard_shortcut_tests {
    use super::chord_and_button;
    use egui::{Event, Modifiers};

    #[test]
    fn reordered_modifier_shortcut_is_normalized_and_activates_center_action() {
        let mut session = filmcraft_engine::Session::default();
        session.execute("file.openDemoProject", serde_json::json!({})).expect("open a Timeline project");
        let mut app = crate::FilmcraftApp::new(session);
        app.ui.focused = crate::dock::PanelKind::Timeline;

        super::set_slider_binding(&mut app, false, 0, "Shift + Ctrl + K");
        assert_eq!(app.session.shortcuts.primary("timeline.sliderCenter").as_deref(), Some("Ctrl+Shift+K"));
        assert_eq!(app.session.state.timeline_ui.control_center_on_playhead, "Ctrl + Shift + K");

        let ctx = egui::Context::default();
        ctx.data_mut(|data| {
            data.insert_temp(egui::Id::unique("timeline-zoom-slider-rect"), egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(100.0, 100.0)));
        });
        let modifiers = Modifiers { ctrl: true, shift: true, ..Default::default() };
        let input = egui::RawInput {
            events: vec![
                Event::PointerMoved(egui::pos2(50.0, 50.0)),
                Event::Key { key: egui::Key::K, physical_key: Some(egui::Key::K), pressed: true, repeat: false, modifiers },
            ],
            ..Default::default()
        };
        let mut output = ctx.run_ui(input, |ui| super::route_zoom_slider_shortcut(&app, ui.ctx()));
        output.textures_delta.clear();

        assert_eq!(ctx.data(|data| data.get_temp::<usize>(super::zoom_trigger_key())), Some(0));
        assert!(!ctx.input(|input| input.events.iter().any(|event| matches!(event, Event::Key { key: egui::Key::K, pressed: true, .. }))));
    }

    #[test]
    fn zoom_slider_reset_restores_its_declared_defaults() {
        let mut app = crate::FilmcraftApp::new(filmcraft_engine::Session::default());
        app.session.shortcuts.set("timeline.sliderCenter", "Ctrl+K", None, false, true).expect("assign zoom-slider key");
        app.session.shortcuts.set("timeline.sliderExplode", "Ctrl+J", None, false, true).expect("assign zoom-slider explode key");
        app.session.state.timeline_ui.control_center_on_playhead = "Ctrl + K".into();
        app.session.state.timeline_ui.control_explode_slider = "Ctrl + J".into();

        super::reset_slider_binding(&mut app, false, 0);
        assert_eq!(app.session.shortcuts.primary("timeline.sliderCenter").as_deref(), Some("Double Click"));
        assert_eq!(app.session.shortcuts.primary("timeline.sliderExplode").as_deref(), Some("Ctrl+J"));
        assert_eq!(app.session.state.timeline_ui.control_center_on_playhead, "Double Click");
        assert_eq!(app.session.state.timeline_ui.control_explode_slider, "Ctrl + J");
    }

    #[test]
    fn resetting_zoom_controls_restores_spaced_gesture_labels() {
        let mut app = crate::FilmcraftApp::new(filmcraft_engine::Session::default());
        app.session.shortcuts.set_gesture("timeline.zoomScroll", "Ctrl + Shift + Scroll").expect("set a custom zoom gesture");
        super::reset_slider_binding(&mut app, false, 7);
        assert_eq!(super::control_binding(&mut app, 7), "Alt + Scroll");

        super::set_slider_binding(&mut app, false, 2, "Alt + Drag");
        super::reset_slider_binding(&mut app, false, 2);
        assert_eq!(super::control_binding(&mut app, 2), "Shift + Drag");
    }

    #[test]
    fn adding_a_hex_prefix_keeps_the_caret_after_the_typed_digit() {
        use egui::text::{CCursor, CCursorRange};

        let ctx = egui::Context::default();
        let edit_id = egui::Id::unique("timeline-color-hex-test");
        let mut state = egui::text_edit::TextEditState::default();
        state.cursor.set_char_range(Some(CCursorRange::one(CCursor::new(1))));
        state.store(&ctx, edit_id);
        let mut buf = "a".to_owned();

        assert_eq!(super::normalize_hex_edit(&ctx, edit_id, &mut buf), "a");
        assert_eq!(buf, "#a");
        let cursor = egui::text_edit::TextEditState::load(&ctx, edit_id).and_then(|state| state.cursor.char_range()).unwrap().primary.index;
        assert_eq!(usize::from(cursor), 2, "the inserted prefix must shift the caret past the typed hex digit");
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
        let mut output = ctx.run_ui(press, |ui| super::capture_controls_for_profile(&mut app, ui.ctx(), super::zoom_controls_profile()));
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
        let mut output = ctx.run_ui(release, |ui| super::capture_controls_for_profile(&mut app, ui.ctx(), super::zoom_controls_profile()));
        output.textures_delta.clear();
        assert_eq!(app.session.shortcuts.primary("timeline.sliderCenter").as_deref(), Some("Ctrl + Shift + Alt"));

        ctx.data_mut(|d| {
            d.insert_temp(egui::Id::unique("timeline-zoom-slider-rect"), egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(100.0, 100.0)));
        });
        let input = RawInput { time: Some(1.2), events: vec![Event::PointerMoved(egui::pos2(50.0, 50.0)), Event::ModifiersChanged(all)], ..Default::default() };
        let mut output = ctx.run_ui(input, |ui| super::route_zoom_slider_shortcut(&app, ui.ctx()));
        output.textures_delta.clear();
        assert_eq!(ctx.data(|d| d.get_temp::<usize>(super::zoom_trigger_key())), Some(0));
    }

    #[test]
    fn click_gesture_conflicts_include_existing_defaults_and_fast_scroll() {
        let mut app = crate::FilmcraftApp::new(filmcraft_engine::Session::default());
        super::set_slider_binding(&mut app, false, 1, "Double Click");
        assert!(super::binding_conflicts(&app, false, 1, "Double Click"), "Explode must warn on Center's default Double Click");

        super::set_slider_binding(&mut app, true, 0, "Double Click");
        assert!(super::binding_conflicts(&app, true, 0, "Double Click"), "the changed track binding must warn on Center's Double Click");
        assert!(super::binding_conflicts(&app, false, 0, "Double Click"), "Center must also warn on the changed track binding");

        super::reset_slider_binding(&mut app, false, 1);
        super::set_slider_binding(&mut app, false, 5, "Triple Click");
        assert_eq!(app.session.shortcuts.primary("timeline.fastScroll").as_deref(), Some("Triple Click"));
        assert!(super::binding_conflicts(&app, false, 5, "Triple Click"), "Fast Scroll must warn on Explode's default Triple Click");
    }

    #[test]
    fn rendered_conflict_warnings_appear_on_both_gesture_rows() {
        use egui::{RawInput, Rect, pos2, vec2};

        let mut app = crate::FilmcraftApp::new(filmcraft_engine::Session::default());
        super::set_slider_binding(&mut app, false, 1, "Double Click");
        app.tl.zoombar_settings_pos = Some(pos2(16.0, 16.0));
        let ctx = egui::Context::default();
        let screen_rect = Rect::from_min_size(pos2(0.0, 0.0), vec2(1280.0, 900.0));
        let mut output = ctx.run_ui(RawInput { screen_rect: Some(screen_rect), ..Default::default() }, |ui| super::zoombar_settings_popup(&mut app, ui));
        output.textures_delta.clear();
        let row = app.auto.elements.iter().find(|element| element.id == "timeline.zoomBar.settings.controls").expect("Controls row").rect;
        let pointer = pos2(row[0] + row[2] / 2.0, row[1] + row[3] / 2.0);
        let mut output = ctx.run_ui(
            RawInput { screen_rect: Some(screen_rect), time: Some(0.1), events: vec![egui::Event::PointerMoved(pointer)], ..Default::default() },
            |ui| super::zoombar_settings_popup(&mut app, ui),
        );
        output.textures_delta.clear();

        for index in 0..=1 {
            let warning_id = format!("timeline.zoomBar.settings.controlsFlyout.conflict{index}");
            assert!(
                app.auto.elements.iter().any(|element| element.id == warning_id),
                "the conflicting gesture row must show its warning: {warning_id}; registered {:?}",
                app.auto.elements.iter().map(|element| element.id.as_str()).collect::<Vec<_>>()
            );
        }
    }

    #[test]
    fn escape_closes_zoom_settings_popup_when_controls_are_not_capturing() {
        use egui::{Event, Key, Modifiers, RawInput, Rect, pos2};

        let mut session = filmcraft_engine::Session::default();
        session.execute("file.openDemoProject", serde_json::json!({})).unwrap();
        let mut app = crate::FilmcraftApp::new(session);
        let sequence = app.session.state.active_sequence.and_then(|id| app.session.project.sequence(id)).unwrap().clone();
        let bar = Rect::from_min_max(pos2(10.0, 100.0), pos2(990.0, 116.0));
        let ctx = egui::Context::default();
        ctx.data_mut(|data| data.insert_temp(super::zoom_bar::zoombar_popup_open_key(), true));
        let tokens = app.tokens;
        let input = RawInput {
            screen_rect: Some(Rect::from_min_max(pos2(0.0, 0.0), pos2(1_000.0, 600.0))),
            events: vec![Event::Key { key: Key::Escape, physical_key: Some(Key::Escape), pressed: true, repeat: false, modifiers: Modifiers::NONE }],
            ..Default::default()
        };

        let mut output = ctx.run_ui(input, |ui| super::zoom_bar::zoom_scrollbar(&mut app, ui, bar, bar.width(), &sequence, 1.0 / 24.0, &tokens));
        output.textures_delta.clear();
        assert!(!ctx.data(|data| data.get_temp::<bool>(super::zoom_bar::zoombar_popup_open_key())).unwrap_or(false));
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
        let mut output = ctx.run_ui(input, |ui| super::route_zoom_slider_shortcut(&app, ui.ctx()));
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
        let mut output = ctx.run_ui(Default::default(), |ui| super::route_zoom_slider_shortcut(&app, ui.ctx()));
        output.textures_delta.clear();
        assert!(!super::controls_listening(&ctx));
        assert!(!ctx.data(|d| d.get_temp::<bool>(super::controls_listen_track_key()).unwrap_or(false)));

        let mut app = FilmcraftApp::new(filmcraft_engine::Session::default());
        app.ui.focused = PanelKind::Timeline;
        ctx.data_mut(|d| d.insert_temp(super::controls_listen_key(), Some(2usize)));
        let mut output = ctx.run_ui(Default::default(), |ui| super::route_zoom_slider_shortcut(&app, ui.ctx()));
        output.textures_delta.clear();
        assert!(!super::controls_listening(&ctx));

        let mut session = filmcraft_engine::Session::default();
        session.execute("file.openDemoProject", serde_json::json!({})).unwrap();
        let mut app = FilmcraftApp::new(session);
        app.ui.focused = PanelKind::Timeline;
        ctx.data_mut(|d| {
            d.insert_temp(super::controls_listen_key(), Some(2usize));
            d.insert_temp(super::controls_listen_track_key(), false);
            d.insert_temp(super::zoom_bar::zoombar_popup_open_key(), false);
        });
        let mut output = ctx.run_ui(Default::default(), |ui| super::route_zoom_slider_shortcut(&app, ui.ctx()));
        output.textures_delta.clear();
        assert!(!super::controls_listening(&ctx), "a closed settings popup cannot keep Controls listening");

        app.tl.zoombar_settings_pos = Some(egui::pos2(10.0, 10.0));
        ctx.data_mut(|d| {
            d.insert_temp(super::controls_listen_key(), Some(2usize));
            d.insert_temp(super::controls_listen_track_key(), false);
            d.insert_temp(super::controls_listen_document_epoch_key(), app.session.document_epoch);
            d.insert_temp(super::zoom_bar::zoombar_popup_open_key(), true);
        });
        app.session.document_epoch = app.session.document_epoch.wrapping_add(1);
        let mut output = ctx.run_ui(Default::default(), |ui| super::route_zoom_slider_shortcut(&app, ui.ctx()));
        output.textures_delta.clear();
        assert!(!super::controls_listening(&ctx), "a replacement document cancels its prior Controls capture");
    }

    #[test]
    fn visible_zoom_bar_popup_keeps_controls_capture_active() {
        use crate::{FilmcraftApp, dock::PanelKind};
        use egui::{Event, Key, PointerButton, RawInput, Rect, pos2, vec2};
        use filmcraft_engine::Session;

        let mut session = Session::default();
        session.execute("file.openDemoProject", serde_json::json!({})).expect("open demo project");
        let mut app = FilmcraftApp::new(session);
        app.ui.focused = PanelKind::Timeline;
        let sequence = app.session.state.active_sequence.and_then(|id| app.session.project.sequence(id)).expect("demo sequence").clone();
        let bar = Rect::from_min_max(pos2(10.0, 100.0), pos2(990.0, 116.0));
        let pointer = pos2(400.0, bar.center().y);
        let ctx = egui::Context::default();
        let screen = Rect::from_min_size(pos2(0.0, 0.0), vec2(1_000.0, 700.0));
        let tokens = app.tokens;
        let open = RawInput {
            screen_rect: Some(screen),
            time: Some(1.0),
            events: vec![
                Event::PointerMoved(pointer),
                Event::PointerButton { pos: pointer, button: PointerButton::Secondary, pressed: true, modifiers: egui::Modifiers::NONE },
                Event::PointerButton { pos: pointer, button: PointerButton::Secondary, pressed: false, modifiers: egui::Modifiers::NONE },
            ],
            ..Default::default()
        };
        app.auto.begin_frame();
        let mut output = ctx.run_ui(open, |ui| super::zoom_bar::zoom_scrollbar(&mut app, ui, bar, bar.width(), &sequence, 1.0 / 24.0, &tokens));
        output.textures_delta.clear();
        assert!(
            ctx.data(|data| data.get_temp::<bool>(super::zoom_bar::zoombar_popup_open_key())).unwrap_or(false),
            "right-click opens the visible bar's popup"
        );

        // Simulate the capture state installed by clicking a binding box in the visible Controls flyout.
        ctx.data_mut(|data| {
            data.insert_temp(super::controls_listen_key(), Some(0usize));
            data.insert_temp(super::controls_listen_track_key(), false);
            data.insert_temp(super::controls_listen_document_epoch_key(), app.session.document_epoch);
        });
        assert!(!super::reconcile_controls_capture(&app, &ctx), "the active Timeline accepts the capture");
        assert!(super::controls_listening(&ctx), "the visible zoom-bar popup owns the Controls capture");
        let key = RawInput {
            screen_rect: Some(screen),
            time: Some(1.1),
            events: vec![Event::Key { key: Key::F9, physical_key: Some(Key::F9), pressed: true, repeat: false, modifiers: egui::Modifiers::NONE }],
            ..Default::default()
        };
        app.auto.begin_frame();
        let mut output = ctx.run_ui(key, |ui| {
            assert!(!super::reconcile_controls_capture(&app, ui.ctx()), "the visible popup keeps capture alive while the key arrives");
            super::capture_controls_for_profile(&mut app, ui.ctx(), super::zoom_controls_profile());
        });
        output.textures_delta.clear();
        assert_eq!(app.session.state.timeline_ui.control_center_on_playhead, "F9", "the visible popup captures the shortcut");
        assert!(!super::controls_listening(&ctx), "capture completes after the key is stored");

        super::close_zoombar_popup(&ctx);
        assert!(!super::reconcile_controls_capture(&app, &ctx), "the Timeline remains active");
        assert!(!super::controls_listening(&ctx), "closing the visible popup leaves no stale Controls capture");
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
        let mut output = ctx.run_ui(input, |ui| super::route_zoom_slider_shortcut(&app, ui.ctx()));
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
        let mut output = ctx.run_ui(input, |ui| super::route_zoom_slider_shortcut(&app, ui.ctx()));
        output.textures_delta.clear();
        assert_eq!(ctx.data(|d| d.get_temp::<usize>(super::zoom_trigger_key())), None);
        assert!(ctx.input(|i| i.events.iter().any(|e| matches!(e, Event::Key { key: egui::Key::C, pressed: true, .. }))));
    }
}

pub fn gesture(response: &egui::Response, ui: &egui::Ui) -> Option<String> {
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

pub fn uses_default_gesture(binding: &str, default: &str) -> bool {
    if binding.is_empty() {
        return true;
    }
    binding != "Unbound" && normalize_slider_gesture(binding) == normalize_slider_gesture(default)
}

pub fn matches_gesture(binding: &str, default: &str, gesture: Option<&str>) -> bool {
    let binding = if binding.is_empty() { default } else { binding };
    gesture.is_some_and(|gesture| binding != "Unbound" && normalize_slider_gesture(binding) == normalize_slider_gesture(gesture))
}

pub fn normalize_slider_gesture(text: &str) -> String {
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
pub fn slider_action_triggered(key_trigger: Option<usize>, slot: usize, binding: &str, default: &str, gesture: Option<&str>) -> bool {
    key_trigger == Some(slot) || matches_gesture(binding, default, gesture)
}

pub fn alternate_down(ctx: &egui::Context, binding: &str) -> bool {
    alternate_drag_binding_captured(ctx, binding) || ctx.input(|i| alternate_down_input(i, binding))
}

pub fn alternate_down_input(input: &egui::InputState, binding: &str) -> bool {
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
