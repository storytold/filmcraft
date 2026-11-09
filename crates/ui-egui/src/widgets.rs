//! Small custom widgets in Premiere's visual language.

pub mod color_picker;

use egui::{Align2, Color32, Rect, Response, Sense, Stroke, StrokeKind, Ui, pos2, vec2};
use filmcraft_time::{FrameRate, Tick, TimeDisplay, parse_time_display};

use crate::icons::{self, Icon};
use crate::theme::Tokens;

#[derive(Clone, Copy)]
pub(crate) enum MarkerPart {
    Whole,
    Left,
    Right,
}

/// Point markers use the whole flag; a range places its two halves at its endpoints.
pub(crate) fn marker_glyph(painter: &egui::Painter, rect: Rect, tip_height: f32, part: MarkerPart, color: Color32) {
    let x = rect.center().x;
    let y = rect.top();
    let shoulder = rect.bottom() - tip_height;
    let points = match part {
        MarkerPart::Whole => {
            vec![pos2(rect.left(), y), pos2(rect.right(), y), pos2(rect.right(), shoulder), pos2(x, rect.bottom()), pos2(rect.left(), shoulder)]
        }
        MarkerPart::Left => vec![pos2(rect.left(), y), pos2(x, y), pos2(x, rect.bottom()), pos2(rect.left(), shoulder)],
        MarkerPart::Right => vec![pos2(x, y), pos2(rect.right(), y), pos2(rect.right(), shoulder), pos2(x, rect.bottom())],
    };
    painter.add(egui::Shape::convex_polygon(points, color, Stroke::NONE));
}

/// Give a widget an explicit accessible name without changing its visible hint text.
///
/// `egui_kittest` 0.37 hard-fails visible input roles that have neither a label nor a placeholder.
/// Text fields that sit beside a drawn label (or that have no room for a placeholder) still need a
/// stable name for screen readers and `get_by_label`.
pub fn accessible_name(response: Response, name: impl Into<String>) -> Response {
    let name = name.into();
    if response.ctx.has_accesskit_node(response.id) {
        response.ctx.accesskit_node_builder(response.id, |node| {
            if node.label().is_none_or(|label| label.trim().is_empty()) {
                node.set_label(name);
            }
        });
    }
    response
}

/// Numeric formatting and theme context for a [`hot_number`] field.
///
/// Grouping the related knobs keeps the widget's call sites readable and below the argument-count
/// limit; the fields are exactly the values the hot-number editor and drag handling need.
pub struct HotNumberOptions<'a> {
    /// Value change per dragged point at the normal modifier setting.
    pub speed: f64,
    /// Inclusive value bounds used when committing a drag or a typed calculation.
    pub range: (f64, f64),
    /// Digits after the decimal point in the displayed value.
    pub decimals: usize,
    /// Unit text drawn after the value and stripped from typed expressions.
    pub suffix: &'a str,
    /// Theme tokens for the hot-text colours.
    pub tokens: &'a Tokens,
}

impl<'a> HotNumberOptions<'a> {
    /// Const constructor grouping speed, range, decimals, suffix and theme tokens; it performs no validation.
    pub const fn new(speed: f64, range: (f64, f64), decimals: usize, suffix: &'a str, tokens: &'a Tokens) -> Self {
        Self { speed, range, decimals, suffix, tokens }
    }
}

/// Premiere "hot text": a blue number you drag horizontally to scrub, click to type a number or a
/// calculation ([`crate::calc`]). `label` is the property's accessible name (for example
/// "Position X"), used when the field switches to a text editor so screen readers can tell the
/// value fields apart.
/// Returns (response, Some(new value) when changed).
pub fn hot_number(ui: &mut Ui, label: &str, id: egui::Id, value: f64, options: HotNumberOptions<'_>) -> (Response, Option<f64>) {
    let editing_id = id.with("editing");
    let mut editing: Option<String> = ui.data(|d| d.get_temp(editing_id));
    if let Some(buf) = editing.as_mut() {
        return hot_number_editor(ui, label, id, value, options.range, options.suffix, buf);
    }
    hot_number_display(ui, id, value, options)
}

/// The single-line text editor shown after a click on a hot number.
fn hot_number_editor(ui: &mut Ui, label: &str, id: egui::Id, value: f64, range: (f64, f64), suffix: &str, buf: &mut String) -> (Response, Option<f64>) {
    let editing_id = id.with("editing");
    let edit_id = id.with("edit");
    let select_all_id = id.with("select-all");
    arrows_start_from_the_end(ui, edit_id, buf);
    // Up/Down step the typed value and commit it, leaving it selected (before the `TextEdit` sees the key)
    let stepped = step_typed_value(ui, edit_id, buf, value, range, suffix);
    let r = accessible_name(ui.add(egui::TextEdit::singleline(buf).id(edit_id).desired_width(56.0).font(Tokens::ui(12.0))), label);
    let mut out = stepped;
    if r.lost_focus() {
        // the field's own unit can be typed anywhere; calc ignores the common ones
        let typed = if suffix.trim().is_empty() { buf.clone() } else { buf.replace(suffix.trim(), "") };
        if let Some(v) = crate::calc::eval(&typed, value) {
            out = Some(v.clamp(range.0, range.1));
        }
        ui.data_mut(|d| d.remove::<String>(editing_id));
    } else {
        let b = buf.clone();
        ui.data_mut(|d| d.insert_temp(editing_id, b));
        // select the whole value once the field has focus (egui collapses a selection to a caret
        // in a field that does not have it yet), so typing replaces it as in Premiere
        let focused = holds_focus(&r);
        let selected_now = focused && ui.data_mut(|d| d.remove_temp::<bool>(select_all_id)).is_some();
        if selected_now {
            let mut st = egui::text_edit::TextEditState::load(ui.ctx(), edit_id).unwrap_or_default();
            st.cursor.set_char_range(Some(egui::text::CCursorRange::two(egui::text::CCursor::new(0), egui::text::CCursor::new(buf.chars().count()))));
            st.store(ui.ctx(), edit_id);
        }
        // the focus and the selection show on the next frame, which nothing else would trigger
        if !focused || selected_now {
            ui.ctx().request_repaint();
        }
        // only until it has focus: `request_focus` resets the field's focus-lock filter, and the
        // arrow keys would then move focus out of the field (committing it) instead of the caret
        if !focused {
            r.request_focus();
        }
    }
    (r, out)
}

/// Up/Down in a hot number's text field step the typed value as Premiere does: by 1, or by the last
/// shown digit when the range is narrow (Exposure steps 0.01); Shift is x10 and Command x0.1, the
/// drag multipliers. The new value replaces the text, stays selected (typing still replaces it) and
/// is returned to be applied. The keys are consumed so the `TextEdit` does not move its caret.
/// Call before drawing the `TextEdit` with id `edit_id` that holds `buf`.
fn step_typed_value(ui: &Ui, edit_id: egui::Id, buf: &mut String, value: f64, range: (f64, f64), suffix: &str) -> Option<f64> {
    let mut direction = 0.0;
    let mut mult = 1.0;
    ui.input_mut(|i| {
        // Shift wins over Command, as in the drag. The modifiers are those of the key press
        // itself, so a Shift+Command press is consumed and gets the Shift multiplier.
        for (key, dir) in [(egui::Key::ArrowUp, 1.0), (egui::Key::ArrowDown, -1.0)] {
            let mods = i
                .events
                .iter()
                .find_map(|e| match e {
                    egui::Event::Key { key: k, pressed: true, modifiers, .. } if *k == key => Some(*modifiers),
                    _ => None,
                })
                .unwrap_or(i.modifiers);
            let m = if mods.shift {
                10.0
            } else if mods.command {
                0.1
            } else {
                1.0
            };
            if direction == 0.0 && i.consume_key(mods, key) {
                direction = dir;
                mult = m;
            }
        }
    });
    if direction == 0.0 {
        return None;
    }
    let typed = if suffix.trim().is_empty() { buf.trim().to_owned() } else { buf.trim().replace(suffix.trim(), "") };
    let current = crate::calc::eval(&typed, value).filter(|v| v.is_finite())?;
    // the last shown digit: of a plain typed number as typed; of a calculation, of its result
    let shown = if typed.trim().parse::<f64>().is_ok() {
        typed.trim().split_once('.').map_or(0, |(_, f)| f.chars().filter(char::is_ascii_digit).count()).min(6)
    } else {
        let r = format!("{current:.6}");
        r.split_once('.').map_or(0, |(_, f)| f.trim_end_matches('0').len())
    };
    let narrow = (range.1 - range.0).abs() <= 10.0;
    let base = if narrow { 10f64.powi(-(shown as i32)) } else { 1.0 };
    let step = base * mult;
    // the step is a power of ten; up to seven digits, so Command keeps a tenth of a sixth digit
    let step_digits = (-step.log10()).round().clamp(0.0, 7.0) as usize;
    let decimals = shown.max(step_digits);
    let text = format!("{:.decimals$}", (current + direction * step).clamp(range.0, range.1));
    let new = text.parse::<f64>().ok()?;
    let len = text.chars().count();
    *buf = text;
    let mut st = egui::text_edit::TextEditState::load(ui.ctx(), edit_id).unwrap_or_default();
    st.cursor.set_char_range(Some(egui::text::CCursorRange::two(egui::text::CCursor::new(0), egui::text::CCursor::new(len))));
    st.store(ui.ctx(), edit_id);
    // at the edge of the range the value does not change: nothing to apply
    ((new - value).abs() > f64::EPSILON).then_some(new)
}

/// Whether a field has keyboard focus, also while the window is in the background: `has_focus` is
/// false then, as it is between control-channel inputs.
fn holds_focus(r: &Response) -> bool {
    r.ctx.memory(|m| m.has_focus(r.id))
}

/// Left/Right on a field's whole selected value start from its last digit: egui would jump to the
/// start. Call before drawing the `TextEdit` with id `edit_id` that holds `text`.
fn arrows_start_from_the_end(ui: &Ui, edit_id: egui::Id, text: &str) {
    let arrow = ui.input(|i| i.modifiers.is_none() && (i.key_pressed(egui::Key::ArrowLeft) || i.key_pressed(egui::Key::ArrowRight)));
    if arrow && let Some(mut st) = egui::text_edit::TextEditState::load(ui.ctx(), edit_id) {
        let len = text.chars().count();
        let all = [egui::text::CCursor::new(0), egui::text::CCursor::new(len)];
        if len > 0 && st.cursor.char_range().is_some_and(|c| c.sorted_cursors() == all) {
            st.cursor.set_char_range(Some(egui::text::CCursorRange::one(egui::text::CCursor::new(len))));
            st.store(ui.ctx(), edit_id);
        }
    }
}

/// Select the whole text of a single-line field when a click or Tab focuses it, so typing replaces the value as in Premiere.
/// A drag that focuses the field still selects from where it started.
pub fn select_all_on_focus(ui: &Ui, r: &Response, text: &str) {
    let pending_id = r.id.with("select-all-on-click");
    if r.gained_focus() && r.is_pointer_button_down_on() {
        // wait for the release: only a click (not a drag) selects everything
        ui.data_mut(|d| d.insert_temp(pending_id, true));
        return;
    }
    let pending = ui.data(|d| d.get_temp::<bool>(pending_id)).is_some();
    if pending && (r.clicked() || r.dragged() || !r.is_pointer_button_down_on()) {
        ui.data_mut(|d| d.remove_temp::<bool>(pending_id));
    }
    let select = if pending { r.clicked() } else { r.gained_focus() };
    if select {
        let mut st = egui::text_edit::TextEditState::load(ui.ctx(), r.id).unwrap_or_default();
        st.cursor.set_char_range(Some(egui::text::CCursorRange::two(egui::text::CCursor::new(0), egui::text::CCursor::new(text.chars().count()))));
        st.store(ui.ctx(), r.id);
        ui.ctx().request_repaint();
    }
}

#[derive(Clone, Debug)]
struct ClearOnFocusSession {
    original: String,
    current: String,
    modified: bool,
}

fn add_clear_edit(ui: &mut Ui, edit_id: egui::Id, buf: &mut String, desired_width: f32, multiline_rows: Option<usize>) -> Response {
    match multiline_rows {
        Some(rows) => ui.add(egui::TextEdit::multiline(buf).id(edit_id).desired_rows(rows).desired_width(desired_width)),
        None => ui.add(egui::TextEdit::singleline(buf).id(edit_id).desired_width(desired_width)),
    }
}

fn clear_on_focus_text_edit_impl(ui: &mut Ui, id: egui::Id, value: &str, desired_width: f32, multiline_rows: Option<usize>) -> (Response, Option<String>) {
    let edit_id = id.with("edit");
    let session_id = id.with("session");
    let rect_id = id.with("rect");

    let was_focused = ui.ctx().memory(|m| m.focused() == Some(edit_id));
    let last_rect: Option<egui::Rect> = ui.data(|d| d.get_temp(rect_id));
    let clicked_into = !was_focused
        && last_rect.is_some_and(|rect| {
            ui.input(|i| {
                let ptr = &i.pointer;
                (ptr.primary_down() || ptr.primary_clicked()) && ptr.interact_pos().is_some_and(|p| rect.contains(p))
            })
        });

    let mut session: Option<ClearOnFocusSession> = ui.data(|d| d.get_temp(session_id));

    // If an edit session is active and Escape was pressed, cancel and revert to original
    let escape = ui.input(|i| i.key_pressed(egui::Key::Escape));
    if let Some(s) = session.as_ref() {
        if escape {
            ui.data_mut(|d| d.remove::<ClearOnFocusSession>(session_id));
            let revert = if s.modified { Some(s.original.clone()) } else { None };
            let mut buf = s.original.clone();
            let r = add_clear_edit(ui, edit_id, &mut buf, desired_width, multiline_rows);
            r.surrender_focus();
            ui.data_mut(|d| d.insert_temp(rect_id, r.rect));
            return (r, revert);
        }

        // If focus was lost before this frame, finalize the session before drawing
        if !was_focused && !clicked_into {
            ui.data_mut(|d| d.remove::<ClearOnFocusSession>(session_id));
            let commit = if s.modified && s.current != value { Some(s.current.clone()) } else { None };
            let mut buf = value.to_string();
            let r = add_clear_edit(ui, edit_id, &mut buf, desired_width, multiline_rows);
            ui.data_mut(|d| d.insert_temp(rect_id, r.rect));
            return (r, commit);
        }
    }

    let newly_focused = (!was_focused && clicked_into) || (was_focused && session.is_none());
    if newly_focused {
        let new_session = ClearOnFocusSession { original: value.to_string(), current: String::new(), modified: false };
        ui.data_mut(|d| d.insert_temp(session_id, new_session.clone()));
        session = Some(new_session);
        let mut st = egui::text_edit::TextEditState::load(ui.ctx(), edit_id).unwrap_or_default();
        st.cursor.set_char_range(Some(egui::text::CCursorRange::one(egui::text::CCursor::new(0))));
        st.store(ui.ctx(), edit_id);
    }

    let mut buf = if let Some(s) = &session { s.current.clone() } else { value.to_string() };

    let r = add_clear_edit(ui, edit_id, &mut buf, desired_width, multiline_rows);
    ui.data_mut(|d| d.insert_temp(rect_id, r.rect));

    // Handle focus gained via click or keyboard (e.g. Tab) if not already caught by clicked_into
    if (r.has_focus() || r.gained_focus() || r.clicked()) && session.is_none() {
        let new_session = ClearOnFocusSession { original: value.to_string(), current: String::new(), modified: false };
        ui.data_mut(|d| d.insert_temp(session_id, new_session.clone()));
        session = Some(new_session);
        let mut st = egui::text_edit::TextEditState::load(ui.ctx(), edit_id).unwrap_or_default();
        st.cursor.set_char_range(Some(egui::text::CCursorRange::one(egui::text::CCursor::new(0))));
        st.store(ui.ctx(), edit_id);
        ui.ctx().request_repaint();
    }

    let is_focused = ui.ctx().memory(|m| m.focused() == Some(edit_id));

    let mut commit = None;
    if is_focused {
        if let Some(mut s) = session {
            let backspace_or_delete = ui.input(|i| i.key_pressed(egui::Key::Backspace) || i.key_pressed(egui::Key::Delete));
            if r.changed() || backspace_or_delete {
                s.modified = true;
                s.current = buf.clone();
                ui.data_mut(|d| d.insert_temp(session_id, s));
                commit = Some(buf);
            }
        }
    } else if let Some(s) = session {
        ui.data_mut(|d| d.remove::<ClearOnFocusSession>(session_id));
        if s.modified {
            if s.current != value {
                commit = Some(s.current);
            }
        } else {
            ui.ctx().request_repaint();
        }
    }

    (r, commit)
}

/// Single-line text edit field that hides its text on focus/click so typing starts fresh immediately.
/// If unfocused without typing anything (or on Escape), restores the original text without committing.
/// If user types or explicitly deletes, commits changes as they occur.
pub fn clear_on_focus_text_edit(ui: &mut Ui, id: egui::Id, value: &str, desired_width: f32) -> (Response, Option<String>) {
    clear_on_focus_text_edit_impl(ui, id, value, desired_width, None)
}

/// Multi-line text edit field that hides its text on focus/click so typing starts fresh immediately.
/// If unfocused without typing anything (or on Escape), restores the original text without committing.
/// If user types or explicitly deletes, commits changes as they occur.
pub fn clear_on_focus_multiline_text_edit(ui: &mut Ui, id: egui::Id, value: &str, desired_width: f32, rows: usize) -> (Response, Option<String>) {
    clear_on_focus_text_edit_impl(ui, id, value, desired_width, Some(rows))
}

/// The painted, draggable value shown while the field is not in text-edit mode.
fn hot_number_display(ui: &mut Ui, id: egui::Id, value: f64, options: HotNumberOptions<'_>) -> (Response, Option<f64>) {
    let HotNumberOptions { speed, range, decimals, suffix, tokens: t } = options;
    let text = format!("{value:.decimals$}{suffix}");
    let galley = ui.painter().layout_no_wrap(text, Tokens::ui(12.0), t.hot_text);
    let (rect, resp) = ui.allocate_exact_size(galley.size() + vec2(4.0, 4.0), Sense::click_and_drag());
    let col = if resp.hovered() || resp.dragged() { t.accent_hover } else { t.hot_text };
    ui.painter().galley_with_override_text_color(rect.min + vec2(2.0, 2.0), galley, col);
    if resp.hovered() || resp.dragged() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeHorizontal);
        let y = rect.max.y - 1.0;
        ui.painter().line_segment([pos2(rect.min.x + 2.0, y), pos2(rect.max.x - 2.0, y)], Stroke::new(1.0, col.gamma_multiply(0.6)));
    }
    let mut out = None;
    if resp.dragged() {
        let dx = resp.drag_delta().x as f64;
        let mult = hot_number_drag_multiplier(ui);
        let nv = (value + dx * speed * mult).clamp(range.0, range.1);
        if (nv - value).abs() > f64::EPSILON {
            out = Some(nv);
        }
    }
    if resp.clicked() {
        ui.data_mut(|d| {
            d.insert_temp(id.with("editing"), format!("{value:.decimals$}"));
            d.insert_temp(id.with("select-all"), true);
        });
        ui.ctx().request_repaint();
    }
    (resp, out)
}

/// Drag sensitivity: Shift ×10, Command ×0.1, otherwise ×1. Typed values are unaffected.
fn hot_number_drag_multiplier(ui: &Ui) -> f64 {
    ui.input(|i| {
        if i.modifiers.shift {
            10.0
        } else if i.modifiers.command {
            0.1
        } else {
            1.0
        }
    })
}

/// A large blue timecode readout (monitors/timeline) that moves its playhead: drag horizontally to
/// scrub (1 frame per point, Shift ×10), click to type a time (Enter or clicking away commits,
/// Escape cancels). Returns the time to go to (never before `min`), or why a typed time is invalid.
#[allow(clippy::too_many_arguments)]
pub fn timecode_field(
    ui: &mut Ui,
    id: egui::Id,
    rect: Rect,
    text: &str,
    time: Tick,
    min: Tick,
    rate: FrameRate,
    drop_frame: bool,
    display: TimeDisplay,
    color: Color32,
) -> Option<Result<Tick, String>> {
    let editing_id = id.with("editing");
    let edit_id = id.with("edit");
    let select_all_id = id.with("select-all");
    if let Some(mut buf) = ui.data(|d| d.get_temp::<String>(editing_id)) {
        arrows_start_from_the_end(ui, edit_id, &buf);
        let r =
            accessible_name(ui.put(rect, egui::TextEdit::singleline(&mut buf).id(edit_id).font(Tokens::timecode()).desired_width(rect.width())), "Timecode");
        if r.lost_focus() {
            ui.data_mut(|d| d.remove::<String>(editing_id));
            if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                return None;
            }
            let typed = parse_time_display(&buf, rate, drop_frame, display, rate.frame_at(time));
            return Some(typed.map(|f| rate.tick_of(f).max(min)).map_err(|e| e.to_string()));
        }
        // select the whole value once the field has focus: egui collapses a selection to a caret
        // in a field that does not have it yet
        let focused = holds_focus(&r);
        let selected_now = focused && ui.data_mut(|d| d.remove_temp::<bool>(select_all_id)).is_some();
        if selected_now {
            let mut st = egui::text_edit::TextEditState::load(ui.ctx(), edit_id).unwrap_or_default();
            st.cursor.set_char_range(Some(egui::text::CCursorRange::two(egui::text::CCursor::new(0), egui::text::CCursor::new(buf.chars().count()))));
            st.store(ui.ctx(), edit_id);
        }
        // the focus and the selection show on the next frame, which nothing else would trigger
        if !focused || selected_now {
            ui.ctx().request_repaint();
        }
        ui.data_mut(|d| d.insert_temp(editing_id, buf));
        // only until it has focus: `request_focus` resets the field's focus-lock filter, and the
        // arrow keys would then move focus out of the field (committing it) instead of the caret
        if !focused {
            r.request_focus();
        }
        return None;
    }
    let resp = ui.interact(rect, id, Sense::click_and_drag());
    ui.painter().text(pos2(rect.min.x, rect.center().y), Align2::LEFT_CENTER, text, Tokens::timecode(), color);
    // (frame at the press, frames dragged so far, where the pointer was pressed)
    let drag_id = id.with("drag");
    let mult = if ui.input(|i| i.modifiers.shift) { 10.0 } else { 1.0 };
    if resp.drag_started() {
        let origin = ui.input(|i| i.pointer.press_origin()).unwrap_or(rect.center());
        let now = resp.interact_pointer_pos().unwrap_or(origin);
        // egui reports a drag only past its click threshold: count the movement before that,
        // less this frame's delta, which the `dragged` branch below adds
        let early = f64::from(now.x - origin.x - resp.drag_delta().x) * mult;
        ui.data_mut(|d| d.insert_temp(drag_id, (rate.frame_at(time), early, origin)));
    }
    let mut out = None;
    if resp.dragged() {
        // hidden while scrubbing, put back where it was pressed on release (below)
        ui.ctx().set_cursor_icon(egui::CursorIcon::None);
        if let Some((start, acc, origin)) = ui.data(|d| d.get_temp::<(i64, f64, egui::Pos2)>(drag_id)) {
            // held at `min` so dragging back moves again at once
            let acc = (acc + f64::from(resp.drag_delta().x) * mult).max((rate.frame_at(min) - start) as f64);
            ui.data_mut(|d| d.insert_temp(drag_id, (start, acc, origin)));
            let target = rate.tick_of(start.saturating_add(acc.trunc() as i64)).max(min);
            if target != time {
                out = Some(Ok(target));
            }
        }
    } else if resp.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeHorizontal);
    }
    if resp.drag_stopped()
        && let Some((_, _, origin)) = ui.data_mut(|d| d.remove_temp::<(i64, f64, egui::Pos2)>(drag_id))
    {
        ui.ctx().send_viewport_cmd(egui::ViewportCommand::CursorPosition(origin));
    }
    if resp.clicked() {
        // the field asks for focus itself next frame: asking now, before it exists, leaves the
        // accessibility tree focused on a node it does not have
        ui.data_mut(|d| {
            d.insert_temp(editing_id, text.to_string());
            d.insert_temp(select_all_id, true);
        });
        ui.ctx().request_repaint();
    }
    out
}

/// Escape closes a dialog or popup only when no mouse button is held: with one held it cancels the
/// drag in progress instead (#580).
pub fn escape_closes(ctx: &egui::Context) -> bool {
    ctx.input(|i| i.key_pressed(egui::Key::Escape) && !i.pointer.any_down())
}

/// Keep a dialog's `draft` as a mouse press begins; Escape while the button is held puts it back,
/// so a dragged number, slider or color returns to its value (#580). Call before drawing the
/// dialog's widgets, and close the dialog with [`escape_closes`] so the same Escape keeps it open.
/// The draft is put back on every frame until the button comes up: on the Escape frame itself a
/// color picker's popup, drawn after this call, still sets its color from the pointer.
pub fn revert_drag_on_escape<T: Clone + Send + Sync + 'static>(ctx: &egui::Context, id: egui::Id, draft: &mut T) {
    let (pressed, down, escape) = ctx.input(|i| (i.pointer.any_pressed(), i.pointer.any_down(), i.key_pressed(egui::Key::Escape)));
    if pressed && down {
        ctx.data_mut(|d| d.insert_temp(id, (draft.clone(), false)));
        return;
    }
    // (the draft when the press began, whether Escape cancelled the drag)
    let Some((before, cancelled)) = ctx.data(|d| d.get_temp::<(T, bool)>(id)) else { return };
    if escape || cancelled {
        *draft = before.clone();
    }
    if !down {
        ctx.data_mut(|d| d.remove::<(T, bool)>(id));
    } else if escape && !cancelled {
        ctx.data_mut(|d| d.insert_temp(id, (before, true)));
    }
}

/// A twirl-down section header (Effect Controls / Color Grading style). Returns open state.
pub fn section_header(ui: &mut Ui, id: egui::Id, title: &str, open: bool, t: &Tokens, bold: bool) -> (Response, bool) {
    let w = ui.available_width();
    let (rect, resp) = ui.allocate_exact_size(vec2(w, 22.0), Sense::click());
    if resp.hovered() {
        ui.painter().rect_filled(rect, 0.0, t.hover);
    }
    let chev = Rect::from_center_size(pos2(rect.min.x + 10.0, rect.center().y), vec2(12.0, 12.0));
    icons::paint(ui.painter(), chev, if open { Icon::ChevronDown } else { Icon::ChevronRight }, t.text_dim);
    ui.painter().text(
        pos2(rect.min.x + 20.0, rect.center().y),
        Align2::LEFT_CENTER,
        title,
        if bold { Tokens::semibold(12.0) } else { Tokens::ui(12.0) },
        t.text,
    );
    let _ = id;
    let open = if resp.clicked() { !open } else { open };
    (resp, open)
}

/// A small pill toggle button with text (e.g. "M", "S" on track headers).
pub fn letter_toggle(ui: &mut Ui, rect: Rect, letter: &str, on: bool, on_color: Color32, t: &Tokens, id: egui::Id) -> Response {
    letter_toggle_clipped(ui, rect, ui.clip_rect(), letter, on, on_color, t, id)
}

/// [`letter_toggle`] shown only where `rect` meets `clip`, such as a Timeline track header scrolled
/// partly out of its half: the button keeps its size and is cut off at the edge, takes clicks only
/// where it shows, and draws nothing once it is scrolled out entirely.
#[allow(clippy::too_many_arguments)]
pub fn letter_toggle_clipped(ui: &mut Ui, rect: Rect, clip: Rect, letter: &str, on: bool, on_color: Color32, t: &Tokens, id: egui::Id) -> Response {
    let visible = rect.intersect(clip);
    let resp = ui.interact(if visible.is_positive() { visible } else { Rect::NOTHING }, id, Sense::click());
    if !visible.is_positive() {
        return resp;
    }
    let p = ui.painter().with_clip_rect(visible.intersect(ui.clip_rect()));
    let bg = if on {
        on_color
    } else if resp.hovered() {
        t.hover
    } else {
        Color32::TRANSPARENT
    };
    p.rect_filled(rect, 2.0, bg);
    if !on {
        p.rect_stroke(rect, 2.0, Stroke::new(1.0, t.separator), StrokeKind::Inside);
    }
    p.text(rect.center(), Align2::CENTER_CENTER, letter, Tokens::semibold(10.5), if on { Color32::BLACK } else { t.text_dim });
    resp
}

/// A flat icon toggle inside a rect.
pub fn icon_toggle(ui: &mut Ui, rect: Rect, icon: Icon, on: bool, t: &Tokens, id: egui::Id, on_color: Option<Color32>) -> Response {
    let resp = ui.interact(rect, id, Sense::click());
    if resp.hovered() {
        ui.painter().rect_filled(rect, 2.0, t.hover);
    }
    let col = if on { on_color.unwrap_or(t.icon) } else { t.text_faint };
    icons::paint(ui.painter(), rect.shrink(rect.width() * 0.16), icon, col);
    resp
}

/// Search field with a magnifier icon.
pub fn search_field(ui: &mut Ui, text: &mut String, hint: &str, width: f32, t: &Tokens) -> Response {
    let h = 22.0;
    let (rect, _) = ui.allocate_exact_size(vec2(width, h), Sense::hover());
    ui.painter().rect_filled(rect, h / 2.0, t.field_bg);
    ui.painter().rect_stroke(rect, h / 2.0, Stroke::new(1.0, t.field_border), StrokeKind::Inside);
    icons::paint(ui.painter(), Rect::from_center_size(pos2(rect.min.x + 12.0, rect.center().y), vec2(12.0, 12.0)), Icon::Search, t.text_dim);
    let inner = Rect::from_min_max(pos2(rect.min.x + 22.0, rect.min.y + 2.0), pos2(rect.max.x - 8.0, rect.max.y - 2.0));
    // centred across the field: a text edit aligns its text to the top unless told otherwise
    let mut child = ui.new_child(egui::UiBuilder::new().max_rect(inner));
    let resp = accessible_name(
        child.add(
            egui::TextEdit::singleline(text)
                .hint_text(hint)
                .frame(egui::Frame::NONE)
                .desired_width(inner.width())
                .font(Tokens::ui(12.0))
                .align(egui::Align2::LEFT_CENTER),
        ),
        hint,
    );
    // Select Find Box (Shift+F) / Open Search (Cmd+Shift+F)
    crate::panels::keyboard::take_search_focus(ui, rect, &resp);
    resp
}

/// Premiere-style dropdown text ("Fit ▾").
pub fn dropdown_text(ui: &mut Ui, rect: Rect, text: &str, t: &Tokens, id: egui::Id) -> Response {
    // Premiere: 24 pt, #0e0e0e fill, 1 pt #303030 border, radius 4, chevron at the right.
    let resp = ui.interact(rect, id, Sense::click());
    ui.painter().rect_filled(rect, 4.0, t.field_bg);
    ui.painter().rect_stroke(
        rect,
        4.0,
        Stroke::new(1.0, if resp.hovered() { Color32::from_rgb(0x4b, 0x4b, 0x4b) } else { t.field_border }),
        StrokeKind::Inside,
    );
    ui.painter().text(pos2(rect.min.x + 9.0, rect.center().y), Align2::LEFT_CENTER, text, Tokens::ui(12.0), t.text);
    icons::paint(ui.painter(), Rect::from_center_size(pos2(rect.max.x - 11.0, rect.center().y), vec2(9.0, 9.0)), Icon::ChevronDown, t.text_dim);
    resp
}

/// Where dialogs open: the centre of the app window. Dialogs use it as `default_pos` with a
/// centre pivot instead of `Window::anchor`, which would pin them: egui makes anchored windows
/// immovable, so they could not be dragged by the title bar.
pub fn dialog_center(ctx: &egui::Context) -> egui::Pos2 {
    ctx.content_rect().center()
}

/// `.pivot(CENTER_CENTER).default_pos(dialog_center(ctx))` in one call, for short window chains.
pub trait CenteredDialog {
    fn centered(self, ctx: &egui::Context) -> Self;
}

impl CenteredDialog for egui::Window<'_> {
    fn centered(self, ctx: &egui::Context) -> Self {
        self.pivot(Align2::CENTER_CENTER).default_pos(dialog_center(ctx))
    }
}

/// Level meter bar (dBFS), vertical.
pub fn meter_bar(painter: &egui::Painter, rect: Rect, db: f32, peak_db: f32, t: &Tokens) {
    // Premiere: green gradient (#579f51 → #70dc5d), yellow above −12 dB, red above −3 dB.
    let norm = |d: f32| ((d + 60.0) / 60.0).clamp(0.0, 1.0);
    let h = rect.height() * norm(db);
    if h > 0.0 {
        let n = 40;
        for i in 0..n {
            let f0 = i as f32 / n as f32;
            let f1 = (i + 1) as f32 / n as f32;
            if f0 * rect.height() > h {
                break;
            }
            let y1 = rect.max.y - f0 * rect.height();
            let y0 = rect.max.y - (f1 * rect.height()).min(h);
            let db_here = -60.0 + f1 * 60.0;
            let c = if db_here > -3.0 {
                Color32::from_rgb(0xe3, 0x48, 0x50)
            } else if db_here > -12.0 {
                Color32::from_rgb(0xf0, 0xf0, 0x4f)
            } else {
                let k = f1 / 0.8;
                let lerp = |a: u8, b: u8| (a as f32 + (b as f32 - a as f32) * k) as u8;
                Color32::from_rgb(lerp(0x57, 0x70), lerp(0x9f, 0xdc), lerp(0x51, 0x5d))
            };
            painter.rect_filled(Rect::from_min_max(pos2(rect.min.x, y0), pos2(rect.max.x, y1)), 0.0, c);
        }
    }
    let py = (rect.max.y - rect.height() * norm(peak_db)).min(rect.max.y - 1.0);
    painter.line_segment(
        [pos2(rect.min.x, py), pos2(rect.max.x, py)],
        Stroke::new(1.0, if peak_db > -0.5 { t.danger } else { Color32::from_rgb(0xf0, 0xf0, 0x4f) }),
    );
}

/// An egui `DragValue` (the number fields of dialogs and Settings) that also takes a calculation
/// ([`crate::calc`]) applied to the value it holds: `100*2`, or `*2` to double it.
pub fn number_drag<Num: egui::emath::Numeric>(value: &mut Num) -> egui::DragValue<'_> {
    let current = value.to_f64();
    egui::DragValue::new(value).custom_parser(move |s| crate::calc::eval(s, current))
}

/// What a pointer is over on a zoom scroll bar (the Timeline's and the monitors'): the circular
/// handle at either end of the thumb zooms, the middle scrolls, and a press beside the thumb brings
/// it there. The press and the hover cursor ask the same question here, so they cannot disagree.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ZoomThumbPart {
    Beside,
    Left,
    Middle,
    Right,
}

impl ZoomThumbPart {
    pub(crate) fn at(thumb: Rect, pos: egui::Pos2) -> Self {
        if !thumb.expand2(vec2(0.0, 4.0)).contains(pos) {
            Self::Beside
        } else if (pos.x - thumb.min.x).abs() < 9.0 {
            Self::Left
        } else if (pos.x - thumb.max.x).abs() < 9.0 {
            Self::Right
        } else {
            Self::Middle
        }
    }
}

/// The horizontal resize cursor of a zoom scroll bar: over either handle of its `thumb`, and for as
/// long as a drag of a handle goes on (`handle_grabbed`), which the pointer can take off the bar.
/// `resp` is the bar's response.
pub(crate) fn zoom_thumb_cursor(ui: &Ui, resp: &Response, thumb: Rect, handle_grabbed: bool) {
    let on_handle = if resp.dragged() {
        handle_grabbed
    } else {
        resp.hover_pos().is_some_and(|pos| matches!(ZoomThumbPart::at(thumb, pos), ZoomThumbPart::Left | ZoomThumbPart::Right))
    };
    if on_handle {
        ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeHorizontal);
    }
}

/// `text` laid out with each digit centred in a cell as wide as the widest digit (tabular figures,
/// which egui's fonts don't switch on), so a number that changes doesn't move the text after it:
/// the pieces with their x offsets, and the width of the whole.
pub fn tabular_text(painter: &egui::Painter, text: &str, font: &egui::FontId, color: Color32) -> (Vec<(f32, std::sync::Arc<egui::Galley>)>, f32) {
    let layout = |s: String| painter.layout_no_wrap(s, font.clone(), color);
    let cell = ('0'..='9').map(|c| layout(c.to_string()).size().x).fold(0.0, f32::max);
    // each digit alone; the runs between them whole, keeping their kerning
    let mut runs: Vec<(bool, String)> = Vec::new();
    for c in text.chars() {
        match runs.last_mut() {
            Some((false, run)) if !c.is_ascii_digit() => run.push(c),
            _ => runs.push((c.is_ascii_digit(), c.to_string())),
        }
    }
    let (mut pieces, mut x) = (Vec::with_capacity(runs.len()), 0.0);
    for (digit, run) in runs {
        let g = layout(run);
        let w = g.size().x;
        pieces.push((if digit { x + (cell - w) / 2.0 } else { x }, g));
        x += if digit { cell } else { w };
    }
    (pieces, x)
}

/// Paint [`tabular_text`]'s pieces with their left edge at `left_center.x`, centred on its y.
pub fn paint_tabular(painter: &egui::Painter, left_center: egui::Pos2, pieces: Vec<(f32, std::sync::Arc<egui::Galley>)>, color: Color32) {
    for (dx, g) in pieces {
        let y = left_center.y - g.size().y / 2.0;
        painter.galley(pos2(left_center.x + dx, y), g, color);
    }
}

/// [`tabular_text`] as a label in `ui`'s layout, read by screen readers as `egui::Label` is.
pub fn tabular_label(ui: &mut egui::Ui, text: &str, font: &egui::FontId, color: Color32) -> egui::Response {
    let (pieces, width) = tabular_text(ui.painter(), text, font, color);
    let height = pieces.iter().map(|(_, g)| g.size().y).fold(font.size, f32::max);
    let sense = if ui.memory(|m| m.options.screen_reader) { egui::Sense::focusable_noninteractive() } else { egui::Sense::hover() };
    let (rect, resp) = ui.allocate_exact_size(vec2(width, height), sense);
    paint_tabular(ui.painter(), pos2(rect.min.x, rect.center().y), pieces, color);
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Label, ui.is_enabled(), text));
    resp
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme::ThemeKind;

    /// The vertical centres of the text shapes `search_field` draws, for a field 22 points tall
    /// drawn at the top of the Ui (its centre is at y = 11).
    fn search_text_centres(text: &str) -> Vec<f32> {
        let ctx = egui::Context::default();
        let t = Tokens::for_kind(ThemeKind::Dark);
        let mut out = ctx.run_ui(egui::RawInput { screen_rect: Some(Rect::from_min_size(pos2(0.0, 0.0), vec2(400.0, 200.0))), ..Default::default() }, |ui| {
            let mut value = text.to_string();
            search_field(ui, &mut value, "Search", 200.0, &t);
        });
        out.textures_delta.clear();
        out.shapes
            .iter()
            .filter_map(|s| match &s.shape {
                egui::Shape::Text(t) => Some(t.visual_bounding_rect().center().y),
                _ => None,
            })
            // (an empty text has no extent)
            .filter(|c| c.is_finite())
            .collect()
    }

    /// Tabular text: every digit takes the same room, so `11:11` and `88:08` are as wide.
    #[test]
    fn tabular_text_gives_every_digit_the_same_room() {
        let ctx = egui::Context::default();
        crate::theme::install(&ctx, &Tokens::for_kind(ThemeKind::Dark)); // the app's UI font
        let mut widths = Vec::new();
        ctx.run_ui(egui::RawInput::default(), |ui| {
            for s in ["Exporting… 11% · 11:11 left", "Exporting… 88% · 88:08 left"] {
                widths.push(tabular_text(ui.painter(), s, &Tokens::ui(11.0), Color32::WHITE).1);
            }
            let plain = |s: &str| ui.painter().layout_no_wrap(s.to_string(), Tokens::ui(11.0), Color32::WHITE).size().x;
            assert!(plain("11:11") < plain("88:08"), "the font's own digits differ in width");
        })
        .textures_delta
        .clear();
        assert!((widths[0] - widths[1]).abs() < 0.01, "{widths:?}");
    }

    #[test]
    fn the_text_of_a_search_field_is_centred_vertically_in_it() {
        for text in ["", "effects"] {
            let centres = search_text_centres(text);
            assert!(!centres.is_empty(), "no text drawn for {text:?}");
            for c in centres {
                assert!((c - 11.0).abs() <= 1.0, "{text:?}: text centred at {c}, the field's centre is 11");
            }
        }
    }

    fn frame(ctx: &egui::Context, events: Vec<egui::Event>, id: egui::Id) {
        let t = Tokens::for_kind(ThemeKind::Dark);
        let input = egui::RawInput { events, ..Default::default() };
        // no renderer takes the font atlas upload here; drop it explicitly (epaint panics otherwise)
        ctx.run_ui(input, |ui| {
            hot_number(ui, "Value", id, 123.5, HotNumberOptions::new(1.0, (0.0, 1000.0), 1, "", &t));
        })
        .textures_delta
        .clear();
    }

    /// The queue's progress line is painted digit by digit; a screen reader still reads it whole.
    #[test]
    fn a_tabular_label_is_read_as_its_text() {
        use egui_kittest::Harness;
        use egui_kittest::kittest::Queryable;
        let mut harness = Harness::builder().with_size(egui::vec2(240.0, 60.0)).build_ui_state(
            |ui, _: &mut ()| {
                tabular_label(ui, "Encoding 88% · 37:00 left", &Tokens::ui(11.0), Color32::WHITE);
            },
            (),
        );
        harness.run_steps(2);
        harness.check_accessibility();
        assert!(harness.query_by_label("Encoding 88% · 37:00 left").is_some());
    }

    /// The edit field behind a hot number uses the caller's property label, not the generic
    /// "Value", so Effect Controls and graphics fields are distinguishable by screen readers.
    #[test]
    fn a_hot_number_edit_field_uses_the_parameter_label() {
        use egui_kittest::Harness;
        use egui_kittest::kittest::Queryable;
        let id = egui::Id::unique("hot-labelled");
        let mut harness = Harness::builder().with_size(egui::vec2(240.0, 60.0)).build_ui_state(
            move |ui, _: &mut ()| {
                let t = Tokens::for_kind(ThemeKind::Dark);
                ui.data_mut(|d| {
                    d.insert_temp(id.with("editing"), "12.5".to_string());
                    d.insert_temp(id.with("select-all"), false);
                });
                hot_number(ui, "Position X", id, 12.5, HotNumberOptions::new(1.0, (-1000.0, 1000.0), 1, "", &t));
            },
            (),
        );
        harness.run_steps(2);
        harness.check_accessibility();
        assert!(harness.query_by_label("Position X").is_some(), "the field is reachable by its parameter label");
        assert!(harness.query_by_label("Value").is_none(), "the generic label is gone");
    }

    /// A Timeline track's Mute and Solo buttons drew their letter outside the header once its track
    /// was scrolled out of the audio half, over the video tracks: what a clipped letter toggle
    /// draws stays inside the clip, and nothing is drawn once it is out entirely.
    #[test]
    fn a_clipped_letter_toggle_draws_only_inside_its_clip() {
        let t = Tokens::for_kind(ThemeKind::Dark);
        let ctx = egui::Context::default();
        crate::theme::install(&ctx, &t); // the letters' semibold font
        let clip = Rect::from_min_max(pos2(0.0, 100.0), pos2(200.0, 300.0));
        for (y, shows) in [(80.0, false), (95.0, true), (150.0, true), (295.0, true), (310.0, false)] {
            for _ in 0..2 {
                let mut out = ctx.run_ui(egui::RawInput::default(), |ui| {
                    let r = Rect::from_min_size(pos2(20.0, y), vec2(14.0, 14.0));
                    letter_toggle_clipped(ui, r, clip, "M", false, Color32::WHITE, &t, egui::Id::unique("m"));
                    letter_toggle_clipped(ui, r.translate(vec2(20.0, 0.0)), clip, "S", true, Color32::YELLOW, &t, egui::Id::unique("s"));
                });
                out.textures_delta.clear();
                assert_eq!(!out.shapes.is_empty(), shows, "button at y {y}");
                for s in &out.shapes {
                    assert!(clip.contains_rect(s.clip_rect), "button at y {y} draws outside its clip: {:?}", s.clip_rect);
                }
            }
        }
    }

    #[test]
    fn clicking_a_hot_number_selects_its_whole_value() {
        let ctx = egui::Context::default();
        let id = egui::Id::unique("hot");
        let pos = egui::pos2(14.0, 12.0);
        let button = |pressed| egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE };
        frame(&ctx, vec![egui::Event::PointerMoved(pos)], id);
        frame(&ctx, vec![button(true)], id);
        frame(&ctx, vec![button(false)], id);
        for _ in 0..4 {
            frame(&ctx, vec![], id);
        }
        let st = egui::text_edit::TextEditState::load(&ctx, id.with("edit")).unwrap_or_default();
        let range = st.cursor.char_range().map(|c| c.sorted_cursors());
        assert_eq!(range, Some([egui::text::CCursor::new(0), egui::text::CCursor::new(5)]));
    }

    /// Runs one frame per entry of `frames` over a "Hello" field using [`select_all_on_focus`] and returns its sorted selection.
    fn text_field_selection(frames: Vec<Vec<egui::Event>>) -> Option<[egui::text::CCursor; 2]> {
        let ctx = egui::Context::default();
        let mut buf = "Hello".to_string();
        let mut field_id = egui::Id::NULL;
        for events in frames.into_iter().chain(std::iter::repeat_with(Vec::new).take(3)) {
            ctx.run_ui(egui::RawInput { events, ..Default::default() }, |ui| {
                let r = ui.add(egui::TextEdit::singleline(&mut buf).desired_width(160.0));
                select_all_on_focus(ui, &r, &buf);
                field_id = r.id;
            })
            .textures_delta
            .clear();
        }
        let st = egui::text_edit::TextEditState::load(&ctx, field_id).unwrap_or_default();
        st.cursor.char_range().map(|c| c.sorted_cursors())
    }

    fn primary_button(pos: egui::Pos2, pressed: bool) -> egui::Event {
        egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE }
    }

    #[test]
    fn clicking_a_single_line_text_field_selects_its_whole_text() {
        let pos = egui::pos2(14.0, 12.0);
        let range = text_field_selection(vec![vec![egui::Event::PointerMoved(pos)], vec![primary_button(pos, true)], vec![primary_button(pos, false)]]);
        assert_eq!(range, Some([egui::text::CCursor::new(0), egui::text::CCursor::new(5)]));
    }

    /// A drag that focuses the field selects from where it started, not from the start of the text.
    #[test]
    fn dragging_into_a_single_line_text_field_keeps_drag_selection() {
        let start = egui::pos2(34.0, 12.0);
        let end = egui::pos2(150.0, 12.0);
        let range = text_field_selection(vec![
            vec![egui::Event::PointerMoved(start)],
            vec![primary_button(start, true)],
            vec![egui::Event::PointerMoved(egui::pos2(80.0, 12.0))],
            vec![egui::Event::PointerMoved(end)],
            vec![primary_button(end, false)],
        ]);
        let Some([from, to]) = range else { panic!("the drag leaves a selection") };
        assert_ne!(from, egui::text::CCursor::new(0), "the selection starts where the drag began");
        assert_eq!(to, egui::text::CCursor::new(5));
    }

    /// Dialogs open centred and can be dragged by their title bar (anchored ones could not).
    #[test]
    fn dialogs_open_centered_and_drag() {
        let ctx = egui::Context::default();
        let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1200.0, 800.0));
        let frame = |events: Vec<egui::Event>| {
            let input = egui::RawInput { screen_rect: Some(screen), events, ..Default::default() };
            let mut rect = egui::Rect::NOTHING;
            let mut out = ctx.run_ui(input, |ui| {
                let ctx = ui.ctx().clone();
                let win = egui::Window::new("Dialog").collapsible(false).resizable(false);
                if let Some(r) = win.pivot(egui::Align2::CENTER_CENTER).default_pos(super::dialog_center(&ctx)).show(&ctx, |ui| ui.label("body")) {
                    rect = r.response.rect;
                }
            });
            out.textures_delta.clear();
            rect
        };
        frame(Vec::new());
        let start = frame(Vec::new());
        assert!((start.center() - screen.center()).length() < 1.0, "opens centred: {start:?}");
        let grab = egui::pos2(start.center().x, start.min.y + 8.0);
        let to = grab + egui::vec2(-200.0, -150.0);
        let press = |p, pressed| egui::Event::PointerButton { pos: p, button: egui::PointerButton::Primary, pressed, modifiers: Default::default() };
        frame(vec![egui::Event::PointerMoved(grab), press(grab, true)]);
        frame(vec![egui::Event::PointerMoved(grab + egui::vec2(-100.0, -75.0))]);
        frame(vec![egui::Event::PointerMoved(to)]);
        frame(vec![press(to, false)]);
        let end = frame(Vec::new());
        assert!((end.center() - (start.center() + egui::vec2(-200.0, -150.0))).length() < 2.0, "moved with the drag: {start:?} -> {end:?}");
    }

    #[test]
    fn zoom_thumb_part_identifies_handles_middle_and_outside() {
        let thumb = Rect::from_min_max(pos2(100.0, 10.0), pos2(200.0, 24.0));
        assert_eq!(ZoomThumbPart::at(thumb, pos2(104.0, 17.0)), ZoomThumbPart::Left);
        assert_eq!(ZoomThumbPart::at(thumb, pos2(196.0, 17.0)), ZoomThumbPart::Right);
        assert_eq!(ZoomThumbPart::at(thumb, pos2(150.0, 17.0)), ZoomThumbPart::Middle);
        assert_eq!(ZoomThumbPart::at(thumb, pos2(50.0, 17.0)), ZoomThumbPart::Beside);
        assert_eq!(ZoomThumbPart::at(thumb, pos2(104.0, 50.0)), ZoomThumbPart::Beside);
    }

    #[test]
    fn clear_on_focus_text_edit_clears_on_focus_and_restores_on_blur_if_untyped() {
        let ctx = egui::Context::default();
        let id = egui::Id::unique("test_edit");
        let edit_id = id.with("edit");

        // Frame 1: field is unfocused with text "Hello"
        let mut out = ctx.run_ui(egui::RawInput::default(), |ui| {
            let (_, commit) = clear_on_focus_text_edit(ui, id, "Hello", 100.0);
            assert_eq!(commit, None);
        });
        out.textures_delta.clear();

        // Frame 2: simulate focus gained
        ctx.memory_mut(|m| m.request_focus(edit_id));
        let mut out = ctx.run_ui(egui::RawInput::default(), |ui| {
            let (_, commit) = clear_on_focus_text_edit(ui, id, "Hello", 100.0);
            assert_eq!(commit, None);
            let s: Option<ClearOnFocusSession> = ui.data(|d| d.get_temp(id.with("session")));
            assert!(s.is_some());
            assert_eq!(s.as_ref().unwrap().current, "");
            assert!(!s.as_ref().unwrap().modified);
        });
        out.textures_delta.clear();

        // Frame 3: lose focus without typing
        ctx.memory_mut(|m| m.surrender_focus(edit_id));
        let mut out = ctx.run_ui(egui::RawInput::default(), |ui| {
            let (_, commit) = clear_on_focus_text_edit(ui, id, "Hello", 100.0);
            assert_eq!(commit, None);
            let s: Option<ClearOnFocusSession> = ui.data(|d| d.get_temp(id.with("session")));
            assert!(s.is_none());
        });
        out.textures_delta.clear();
    }

    #[test]
    fn clear_on_focus_text_edit_commits_when_typed() {
        let ctx = egui::Context::default();
        let id = egui::Id::unique("test_edit_type");
        let edit_id = id.with("edit");

        // Frame 1: unfocused
        let mut out = ctx.run_ui(egui::RawInput::default(), |ui| {
            let (_, commit) = clear_on_focus_text_edit(ui, id, "Hello", 100.0);
            assert_eq!(commit, None);
        });
        out.textures_delta.clear();

        // Frame 2: focus gained
        ctx.memory_mut(|m| m.request_focus(edit_id));
        let mut out = ctx.run_ui(egui::RawInput::default(), |ui| {
            let (_, commit) = clear_on_focus_text_edit(ui, id, "Hello", 100.0);
            assert_eq!(commit, None);
        });
        out.textures_delta.clear();

        // Frame 3: type characters "World"
        let input = egui::RawInput { events: vec![egui::Event::Text("World".into())], ..Default::default() };
        let mut out = ctx.run_ui(input, |ui| {
            let (_, commit) = clear_on_focus_text_edit(ui, id, "Hello", 100.0);
            assert_eq!(commit, Some("World".to_string()));
        });
        out.textures_delta.clear();
    }

    #[test]
    fn clear_on_focus_text_edit_escape_cancels_and_reverts() {
        let ctx = egui::Context::default();
        let id = egui::Id::unique("test_edit_esc");
        let edit_id = id.with("edit");

        // Focus and type
        ctx.memory_mut(|m| m.request_focus(edit_id));
        let mut out = ctx.run_ui(egui::RawInput::default(), |ui| {
            let _ = clear_on_focus_text_edit(ui, id, "Original", 100.0);
        });
        out.textures_delta.clear();

        let input = egui::RawInput { events: vec![egui::Event::Text("Changed".into())], ..Default::default() };
        let mut out = ctx.run_ui(input, |ui| {
            let (_, commit) = clear_on_focus_text_edit(ui, id, "Original", 100.0);
            assert_eq!(commit, Some("Changed".to_string()));
        });
        out.textures_delta.clear();

        // Frame: press Escape
        let input_esc = egui::RawInput {
            events: vec![egui::Event::Key { key: egui::Key::Escape, physical_key: None, pressed: true, repeat: false, modifiers: Default::default() }],
            ..Default::default()
        };
        let mut out = ctx.run_ui(input_esc, |ui| {
            let (_, commit) = clear_on_focus_text_edit(ui, id, "Changed", 100.0);
            assert_eq!(commit, Some("Original".to_string()));
        });
        out.textures_delta.clear();
    }

    #[test]
    fn clear_on_focus_multiline_text_edit_commits_and_reverts() {
        let ctx = egui::Context::default();
        let id = egui::Id::unique("test_multiline_clear");
        let edit_id = id.with("edit");

        // Focus and type multiline text
        ctx.memory_mut(|m| m.request_focus(edit_id));
        let mut out = ctx.run_ui(egui::RawInput::default(), |ui| {
            let _ = clear_on_focus_multiline_text_edit(ui, id, "Line1\nLine2", 100.0, 2);
        });
        out.textures_delta.clear();

        let input = egui::RawInput { events: vec![egui::Event::Text("New\nText".into())], ..Default::default() };
        let mut out = ctx.run_ui(input, |ui| {
            let (_, commit) = clear_on_focus_multiline_text_edit(ui, id, "Line1\nLine2", 100.0, 2);
            assert_eq!(commit, Some("New\nText".to_string()));
        });
        out.textures_delta.clear();

        // Escape reverts
        let input_esc = egui::RawInput {
            events: vec![egui::Event::Key { key: egui::Key::Escape, physical_key: None, pressed: true, repeat: false, modifiers: Default::default() }],
            ..Default::default()
        };
        let mut out = ctx.run_ui(input_esc, |ui| {
            let (_, commit) = clear_on_focus_multiline_text_edit(ui, id, "New\nText", 100.0, 2);
            assert_eq!(commit, Some("Line1\nLine2".to_string()));
        });
        out.textures_delta.clear();
    }

    #[test]
    fn clear_on_focus_multiline_text_edit_click_clears() {
        let ctx = egui::Context::default();
        let id = egui::Id::unique("test_multiline_click");

        let mut rect = egui::Rect::NOTHING;
        let mut out = ctx.run_ui(egui::RawInput::default(), |ui| {
            let (r, _) = clear_on_focus_multiline_text_edit(ui, id, "Line1\nLine2", 100.0, 2);
            rect = r.rect;
        });
        out.textures_delta.clear();

        let click_input = egui::RawInput {
            events: vec![
                egui::Event::PointerMoved(rect.center()),
                egui::Event::PointerButton { pos: rect.center(), button: egui::PointerButton::Primary, pressed: true, modifiers: Default::default() },
            ],
            ..Default::default()
        };
        let mut out = ctx.run_ui(click_input, |ui| {
            let (r, commit) = clear_on_focus_multiline_text_edit(ui, id, "Line1\nLine2", 100.0, 2);
            let s: Option<ClearOnFocusSession> = ui.data(|d| d.get_temp(id.with("session")));
            assert!(s.is_some());
            assert_eq!(s.as_ref().unwrap().current, "");
            assert!(!s.as_ref().unwrap().modified);
            assert_eq!(commit, None);
            assert!(r.has_focus());
        });
        out.textures_delta.clear();
    }
}
