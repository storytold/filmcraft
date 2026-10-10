//! Small custom widgets in Premiere's visual language.

use egui::{Align2, Color32, Rect, Response, Sense, Stroke, StrokeKind, Ui, pos2, vec2};
use filmcraft_time::{FrameRate, Tick, parse_timecode};

use crate::icons::{self, Icon};
use crate::theme::Tokens;

/// Premiere "hot text": a blue number you drag horizontally to scrub, click to type.
/// Returns (response, Some(new value) when changed).
pub fn hot_number(ui: &mut Ui, id: egui::Id, value: f64, speed: f64, range: (f64, f64), decimals: usize, suffix: &str, t: &Tokens) -> (Response, Option<f64>) {
    let editing_id = id.with("editing");
    let mut editing: Option<String> = ui.data(|d| d.get_temp(editing_id));
    if let Some(buf) = editing.as_mut() {
        let r = ui.add(egui::TextEdit::singleline(buf).desired_width(56.0).font(Tokens::ui(12.0)));
        let mut out = None;
        if r.lost_focus() {
            if let Ok(v) = buf.trim().trim_end_matches(suffix).trim().parse::<f64>() {
                out = Some(v.clamp(range.0, range.1));
            }
            ui.data_mut(|d| d.remove::<String>(editing_id));
        } else {
            let b = buf.clone();
            ui.data_mut(|d| d.insert_temp(editing_id, b));
            r.request_focus();
        }
        return (r, out);
    }
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
        let mult = ui.input(|i| {
            if i.modifiers.shift {
                10.0
            } else if i.modifiers.command {
                0.1
            } else {
                1.0
            }
        });
        let nv = (value + dx * speed * mult).clamp(range.0, range.1);
        if (nv - value).abs() > f64::EPSILON {
            out = Some(nv);
        }
    }
    if resp.clicked() {
        ui.data_mut(|d| d.insert_temp(editing_id, format!("{value:.decimals$}")));
    }
    (resp, out)
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
    color: Color32,
) -> Option<Result<Tick, String>> {
    let editing_id = id.with("editing");
    let edit_id = id.with("edit");
    let select_all_id = id.with("select-all");
    if let Some(mut buf) = ui.data(|d| d.get_temp::<String>(editing_id)) {
        // Left/Right on the whole selected value start from its last digit (egui would jump to the start)
        let arrow = ui.input(|i| i.modifiers.is_none() && (i.key_pressed(egui::Key::ArrowLeft) || i.key_pressed(egui::Key::ArrowRight)));
        if arrow && let Some(mut st) = egui::text_edit::TextEditState::load(ui.ctx(), edit_id) {
            let len = buf.chars().count();
            let all = [egui::text::CCursor::new(0), egui::text::CCursor::new(len)];
            if len > 0 && st.cursor.char_range().is_some_and(|c| c.sorted_cursors() == all) {
                st.cursor.set_char_range(Some(egui::text::CCursorRange::one(egui::text::CCursor::new(len))));
                st.store(ui.ctx(), edit_id);
            }
        }
        let r = ui.put(rect, egui::TextEdit::singleline(&mut buf).id(edit_id).font(Tokens::timecode()).desired_width(rect.width()));
        if r.lost_focus() {
            ui.data_mut(|d| d.remove::<String>(editing_id));
            if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                return None;
            }
            let typed = parse_timecode(&buf, rate, drop_frame, rate.frame_at(time));
            return Some(typed.map(|f| rate.tick_of(f).max(min)).map_err(|e| e.to_string()));
        }
        // select the whole value once the field has focus: egui collapses a selection to a caret
        // in a field that does not have it yet
        let selected_now = r.has_focus() && ui.data_mut(|d| d.remove_temp::<bool>(select_all_id)).is_some();
        if selected_now {
            let mut st = egui::text_edit::TextEditState::load(ui.ctx(), edit_id).unwrap_or_default();
            st.cursor.set_char_range(Some(egui::text::CCursorRange::two(egui::text::CCursor::new(0), egui::text::CCursor::new(buf.chars().count()))));
            st.store(ui.ctx(), edit_id);
        }
        // the focus and the selection show on the next frame, which nothing else would trigger
        if !r.has_focus() || selected_now {
            ui.ctx().request_repaint();
        }
        ui.data_mut(|d| d.insert_temp(editing_id, buf));
        // only until it has focus: `request_focus` resets the field's focus-lock filter, and the
        // arrow keys would then move focus out of the field (committing it) instead of the caret
        if !r.has_focus() {
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

/// A twirl-down section header (Effect Controls / Lumetri style). Returns open state.
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
    let resp = ui.interact(rect, id, Sense::click());
    let bg = if on {
        on_color
    } else if resp.hovered() {
        t.hover
    } else {
        Color32::TRANSPARENT
    };
    ui.painter().rect_filled(rect, 2.0, bg);
    if !on {
        ui.painter().rect_stroke(rect, 2.0, Stroke::new(1.0, t.separator), StrokeKind::Inside);
    }
    ui.painter().text(rect.center(), Align2::CENTER_CENTER, letter, Tokens::semibold(10.5), if on { Color32::BLACK } else { t.text_dim });
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
    let mut child = ui.new_child(egui::UiBuilder::new().max_rect(inner));
    let resp = child.add(egui::TextEdit::singleline(text).hint_text(hint).frame(egui::Frame::NONE).desired_width(inner.width()).font(Tokens::ui(12.0)));
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
