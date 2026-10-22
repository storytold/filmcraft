//! Small custom widgets in Premiere's visual language.

use egui::{Align2, Color32, Rect, Response, Sense, Stroke, StrokeKind, Ui, pos2, vec2};
use filmcraft_time::{FrameRate, Tick, parse_timecode};

use crate::icons::{self, Icon};
use crate::theme::Tokens;

/// Premiere "hot text": a blue number you drag horizontally to scrub, click to type a number or a
/// calculation ([`crate::calc`]).
/// Returns (response, Some(new value) when changed).
pub fn hot_number(ui: &mut Ui, id: egui::Id, value: f64, speed: f64, range: (f64, f64), decimals: usize, suffix: &str, t: &Tokens) -> (Response, Option<f64>) {
    let editing_id = id.with("editing");
    let edit_id = id.with("edit");
    let select_all_id = id.with("select-all");
    let mut editing: Option<String> = ui.data(|d| d.get_temp(editing_id));
    if let Some(buf) = editing.as_mut() {
        let r = ui.add(egui::TextEdit::singleline(buf).id(edit_id).desired_width(56.0).font(Tokens::ui(12.0)));
        let mut out = None;
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
        ui.data_mut(|d| {
            d.insert_temp(editing_id, format!("{value:.decimals$}"));
            d.insert_temp(select_all_id, true);
        });
        ui.ctx().request_repaint();
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

/// A menu body (drawn by `menus::menu_level`), bounded so a menu taller than the window scrolls
/// instead of running off the screen — on a phone in landscape the Edit menu is about 42 rows
/// against the 26 that fit, and everything from Select All to Preferences sits below the fold.
///
/// The drag is forced on ([`ScrollSource::ALL`]: `DragScroll::Always`) rather than left to egui's
/// default `DragScroll::OnTouch`, which turns drag-to-scroll on only once
/// `InputState::has_touch_screen` has seen an `Event::Touch`. A menu must scroll by dragging
/// whatever pointer the user holds, and must not depend on that detection; the scroll bar and the
/// wheel are untouched.
///
/// [`ScrollSource::ALL`]: egui::containers::scroll_area::ScrollSource::ALL
pub fn menu_scroll<R>(ui: &mut Ui, add: impl FnOnce(&mut Ui) -> R) -> R {
    menu_scroll_area(ui).show(ui, add).inner
}

/// The area [`menu_scroll`] shows: from the popup's own top (this frame's cursor) to the bottom of
/// the window, so a menu opened low on the screen is bounded by what is actually below it.
fn menu_scroll_area(ui: &mut Ui) -> egui::ScrollArea {
    let max_h = (ui.ctx().content_rect().bottom() - ui.cursor().min.y - 8.0).max(120.0);
    egui::ScrollArea::vertical().max_height(max_h).scroll_source(egui::containers::scroll_area::ScrollSource::ALL)
}

/// A vertical scroll area a finger scrolls by construction: its contents drag
/// ([`ScrollSource::ALL`]) whatever egui has or has not detected about touch, like
/// [`menu_scroll`]. Used where the picker's own bar is the scroll affordance (see
/// [`finger_bar`]).
///
/// [`ScrollSource::ALL`]: egui::containers::scroll_area::ScrollSource::ALL
pub fn finger_scroll(id_salt: impl std::hash::Hash + std::fmt::Debug) -> egui::ScrollArea {
    egui::ScrollArea::vertical().id_salt(id_salt).scroll_source(egui::containers::scroll_area::ScrollSource::ALL)
}

/// The scroll bar a fingertip can land on: egui's default track is 6 pt, which a finger cannot
/// hit. Call at the top of the scope that shows a [`finger_scroll`] area — the bar is reserved
/// from the area's width, so nothing is drawn under it.
pub fn finger_bar(ui: &mut Ui) {
    let s = &mut ui.spacing_mut().scroll;
    s.bar_width = 24.0;
    s.bar_inner_margin = 2.0;
    s.bar_outer_margin = 2.0;
    s.floating = false;
}

/// What a finger press on a draggable list row does instead of the desktop's instant grab.
///
/// egui decides a press is a drag as soon as the pointer moves past its click distance, and turns
/// a still hold into a long-press click (context menu) after its own timeout — both steal the
/// gesture from the scroll area the row sits in, so a finger that wanted to scroll the list picks
/// the row up instead. The row calls [`touch_row`] with its own response and the id of its
/// enclosing scroll area; one shared per-press state (there is one finger) settles what the finger
/// meant, once, for the rest of the press:
///
/// - moving more than `MOVED` points before the hold expires → [`TouchRow::Scroll`]: the row must
///   not start its drag; the helper pans the scroll area with the finger. Only a *touch* press
///   registers — `InputState::any_touches` is a finger and nothing else — so a mouse press keeps
///   every existing instant-grab path unchanged.
/// - holding `HOLD` seconds and then moving → [`TouchRow::Grab`]: the row starts its drag on that
///   frame and keeps it while the finger is down. `HOLD` is shorter than egui's own long-press
///   timeout, and the movement latches egui's "moved too much for a click", so the row's context
///   menu never opens out of a drag.
/// - holding `HOLD` seconds with the finger still → [`TouchRow::Hold`]: the row must not grab, and
///   egui's own long-press opens the row's context menu as it always did. A move after the hold and
///   before that long-press fires is the [`TouchRow::Grab`] above; a move after the menu has opened
///   stays [`TouchRow::Hold`].
/// - a quick release → [`TouchRow::Pressing`] until then; egui's click fires as usual.
mod touch {
    /// The distance a finger may wobble before the press is no longer a click
    /// (matches egui's `max_click_dist`).
    pub(super) const MOVED: f32 = 6.0;
    /// The hold a finger keeps before a move becomes a drag instead of a scroll. Before egui's
    /// own long-press timeout (0.8 s), so holding and then dragging beats the context menu.
    pub(super) const HOLD: f64 = 0.55;
}
use touch::{HOLD, MOVED};

/// What a finger's press turned out to mean. Latched for the rest of the press, so a scroll never
/// turns back into a grab because the finger later travelled past the hold.
#[derive(Clone, Copy, Debug, PartialEq)]
enum TouchKind {
    /// Not settled yet: still a candidate for a click, a scroll, the menu and a drag.
    Undecided,
    /// The finger moved before the hold expired: the enclosing list is scrolling.
    Scroll,
    /// The hold expired with the finger still: egui's long-press is on the press. A move before it
    /// opens the row's menu turns the hold into the row's drag, a move after it does not.
    Held,
    /// The hold expired and the finger then moved: the row's drag.
    Grab,
}

/// Which scroll area a finger press started in and where: temp data, gone after the lift. The area
/// id is the gesture's identity (and gates the pan: one finger pans one area).
#[derive(Clone, Copy, Debug, PartialEq)]
struct TouchPress {
    /// Saved-state id of the enclosing scroll area (its `id_salt`).
    area: egui::Id,
    /// The row the finger went down on: only that row may act on the press (every other row of the
    /// area asks too, and gets no verdict).
    row: egui::Id,
    /// Where the finger went down.
    origin: egui::Pos2,
    /// What the finger meant.
    kind: TouchKind,
    /// Whether egui's long-press opened the row's context menu during this press: under an open
    /// menu a finger moving on must not pick the row up.
    menu: bool,
}

fn touch_press_id() -> egui::Id {
    egui::Id::new("filmcraft-touch-row-press")
}

/// The (pass, area) that already spent this pass's pan (see [`touch_row`]).
fn touch_pan_id() -> egui::Id {
    egui::Id::new("filmcraft-touch-row-pan")
}

/// Whether this pass has already spent the finger's pan on `area`. Every row of a scroll area
/// consults [`touch_row`], and `Ui::scroll_with_delta_animation` *accumulates* — one pan per pass,
/// not one per row.
fn panned_this_pass(ctx: &egui::Context, pass: u64, area: egui::Id) -> bool {
    if ctx.data(|d| d.get_temp::<(u64, egui::Id)>(touch_pan_id())) == Some((pass, area)) {
        return true;
    }
    ctx.data_mut(|d| d.insert_temp(touch_pan_id(), (pass, area)));
    false
}

/// What the arbiter says this frame about a row's press (see [`touch_row`]).
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum TouchRow {
    /// No arbitrated press this frame (a mouse press, or none).
    None,
    /// A finger press neither held long enough nor moved: keep waiting.
    Pressing,
    /// The finger moved before the hold expired: the row must not grab; the list is scrolling.
    Scroll,
    /// The hold expired with the finger still: the row must not grab; egui's own long-press
    /// opens the row's context menu.
    Hold,
    /// The finger held and then moved: the row grabs now and keeps its drag while the finger is
    /// down.
    Grab,
}

/// A draggable row consults this every frame so a finger can scroll instead of picking it up.
///
/// `row` is the row's own response (only the row the press began on may act on it), `area` the
/// saved-state id of the enclosing scroll area — its `id_salt`, the key `egui::ScrollArea::State`
/// stores its offset under. While the press is a scroll the helper pans that area with the
/// pointer delta; the row only needs to skip starting its drag and its click.
pub fn touch_row(ui: &Ui, row: &Response, area: egui::Id) -> TouchRow {
    touch_row_impl(ui, row, area, false)
}

/// [`touch_row`] for a control that moves sideways (a horizontal slider): a mostly-horizontal
/// finger move before the hold is the control's own drag, not a scroll, so the panel keeps
/// handling the slider and the panel does not pan underneath it. A mostly-vertical move still
/// scrolls the panel.
pub fn touch_row_horizontal(ui: &Ui, row: &Response, area: egui::Id) -> TouchRow {
    touch_row_impl(ui, row, area, true)
}

fn touch_row_impl(ui: &Ui, row: &Response, area: egui::Id, horizontal_control: bool) -> TouchRow {
    let ctx = ui.ctx().clone();
    // All reads up front (no nested `data`/`input` calls — those lock for write):
    let (touch, pressed, down, origin, press_start, pos, delta, time) = ctx.input(|i| {
        (
            i.any_touches(),
            i.pointer.primary_pressed(),
            i.pointer.any_down(),
            i.pointer.press_origin(),
            i.pointer.press_start_time(),
            i.pointer.interact_pos().or(i.pointer.hover_pos()),
            i.pointer.delta(),
            i.time,
        )
    });
    let pass = ctx.cumulative_pass_nr();
    let press = ctx.data(|d| d.get_temp::<Option<TouchPress>>(touch_press_id())).flatten();
    let press = match press {
        Some(p) => Some(p),
        None if touch && pressed && origin.is_some_and(|o| row.rect.contains(o)) => {
            let p = TouchPress { area, row: row.id, origin: origin.unwrap_or_else(|| row.rect.center()), kind: TouchKind::Undecided, menu: false };
            ctx.data_mut(|d| d.insert_temp(touch_press_id(), Some(p)));
            Some(p)
        }
        None => None,
    };
    let Some(press) = press else { return TouchRow::None };

    if !down {
        // The finger lifted this frame (egui keeps interact_pos through a touch release, and the
        // click answers from the row's own response): the arbitration is over.
        ctx.data_mut(|d| d.remove::<Option<TouchPress>>(touch_press_id()));
        return TouchRow::None;
    }
    if !touch {
        // A mouse pressed while a press was arbitrated (or the touch state lost): this frame adds
        // no meaning.
        return TouchRow::None;
    }
    let Some(pos) = pos else {
        // A `PointerGone` while held (touch release is such a frame): keep waiting.
        return TouchRow::Pressing;
    };
    // Settle the press once: the finger decides what it meant by moving, and only *when* it moved
    // against the hold says which — early is a scroll, after the hold is the row's drag. A finger
    // that never moves leaves the press to egui, whose long-press opens the row's context menu; a
    // move after that menu opened must not pick the row up under it.
    let held = press_start.is_some_and(|t| time - t > HOLD);
    let moved = press.origin.distance(pos) > MOVED;
    let menu = row.long_touched() || press.menu;
    // A control that moves sideways (a slider) owns a mostly-sideways move before the hold: the
    // press stays unlatched, so it is never a scroll (the panel keeps handling the slider) and
    // never pans the area underneath it.
    let sideways = (pos.x - press.origin.x).abs() > (pos.y - press.origin.y).abs();
    let kind = match press.kind {
        TouchKind::Undecided if moved && horizontal_control && sideways && !held => TouchKind::Undecided,
        TouchKind::Undecided if moved => {
            if held {
                TouchKind::Grab
            } else {
                TouchKind::Scroll
            }
        }
        TouchKind::Undecided if held => TouchKind::Held,
        TouchKind::Held if moved && !menu => TouchKind::Grab,
        settled => settled,
    };
    if kind != press.kind || menu != press.menu {
        let settled = TouchPress { kind, menu, ..press };
        ctx.data_mut(|d| d.insert_temp(touch_press_id(), Some(settled)));
    }
    if kind == TouchKind::Scroll {
        // A scrolling finger: hand this pass's pointer delta to the enclosing scroll area
        // (`scroll_with_delta` feeds the same pass_state the area consumes at its end, so the
        // write cannot be clobbered the way a direct `State::store` mid-pass is), instantly —
        // the drag *is* the animation. The content follows the finger: a finger moving up
        // (negative delta.y, pressing the pointer down on the content) scrolls *down* (the
        // offset grows), which `scroll_delta`'s inverted accounting produces from the same sign.
        // Only the area the press began in pans, and only once per pass: every row of the area
        // asks, and the delta accumulates.
        if press.area == area && delta != egui::Vec2::ZERO && !panned_this_pass(&ctx, pass, area) {
            ui.scroll_with_delta_animation(delta, egui::style::ScrollAnimation::none());
        }
    }
    if press.row != row.id {
        // Another row of the area: it adds no verdict. A hold-then-drag must grab only the row the
        // finger went down on, never every visible row it passes. The pan above still runs here,
        // so the area keeps following the finger even if the pressed row has scrolled out of view.
        return TouchRow::None;
    }
    match kind {
        TouchKind::Scroll => TouchRow::Scroll,
        TouchKind::Grab => TouchRow::Grab,
        TouchKind::Held => TouchRow::Hold,
        TouchKind::Undecided => TouchRow::Pressing,
    }
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

#[cfg(test)]
mod touch_row_tests {
    use super::TouchRow;

    /// An arbitered row inside a scrollable area, fed real Android-style touch input (an
    /// `Event::Touch` plus the emulated pointer events egui-winit sends): `answers` stacks the
    /// answer row 7 got each frame, `offset_y` the area's saved offset after each frame. With
    /// `all_rows` every row consults the arbiter, the way the real panels draw theirs.
    struct Bench {
        answers: Vec<TouchRow>,
        /// Every row's answer, when `all_rows`: `(row, answer)` per consulting row per frame.
        all: Vec<(usize, TouchRow)>,
        offset_y: Vec<f32>,
        all_rows: bool,
        /// Row 7 stands for a sideways control (a slider): it consults [`touch_row_horizontal`].
        horizontal: bool,
    }

    fn touch_frame(b: &mut Bench, ctx: &egui::Context, events: Vec<egui::Event>, at: f64) {
        let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(300.0, 200.0));
        let area = egui::Id::new("bench-scroll");
        let input = egui::RawInput { screen_rect: Some(screen), time: Some(at), events, ..Default::default() };
        let mut offset = 0.0;
        let mut run = ctx.run_ui(input, |ui| {
            let out = egui::ScrollArea::vertical().id_salt(area).auto_shrink([false, false]).show(ui, |ui| {
                for i in 0..20 {
                    let r = ui.add_sized([ui.available_width(), 18.0], egui::Button::new(format!("row {i}")));
                    if i == 7 || b.all_rows {
                        let answer =
                            if i == 7 && b.horizontal { crate::widgets::touch_row_horizontal(ui, &r, area) } else { crate::widgets::touch_row(ui, &r, area) };
                        if b.all_rows {
                            b.all.push((i, answer));
                        }
                        if i == 7 {
                            b.answers.push(answer);
                        }
                    }
                }
            });
            offset = out.state.offset.y;
        });
        run.textures_delta.clear();
        b.offset_y.push(offset);
    }

    /// A finger on row 7 that drags up before the hold expires: the row sees `Scroll`, never
    /// `Hold`/`Grab`, and the area's offset actually moves.
    #[test]
    fn an_early_finger_move_scrolls_and_does_not_grab() {
        let ctx = egui::Context::default();
        let mut b = Bench { answers: Vec::new(), all: Vec::new(), offset_y: Vec::new(), all_rows: false, horizontal: false };
        let at = egui::pos2(120.0, 150.0);
        let touch = |phase, pos| egui::Event::Touch { device_id: egui::TouchDeviceId(1), id: egui::TouchId(1), phase, pos, force: Some(0.5) };
        let button = |pos, pressed| egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed, modifiers: Default::default() };
        // press (touch Start + emulated button), then moves, then the lift (Ended + PointerGone)
        // a warm-up pass, as the app has run many frames before a finger lands: egui hit-tests the
        // press against the previous pass's widgets, and with none it has no click candidate
        touch_frame(&mut b, &ctx, vec![], -0.1);
        touch_frame(&mut b, &ctx, vec![touch(egui::TouchPhase::Start, at), egui::Event::PointerMoved(at), button(at, true)], 0.0);
        assert_eq!(b.answers.last(), Some(&TouchRow::Pressing), "a wait at first: {:?}", b.answers);
        for k in 1..=5 {
            let p = egui::pos2(at.x, at.y - 15.0 * k as f32);
            touch_frame(&mut b, &ctx, vec![touch(egui::TouchPhase::Move, p), egui::Event::PointerMoved(p)], 0.1 * k as f64);
        }
        // the instant scroll lands the frame after each delta (a zero-length animation); one
        // settling frame shows the accumulated offset
        touch_frame(&mut b, &ctx, vec![], 0.6);
        assert!(b.answers.contains(&TouchRow::Scroll), "an early move scrolls: {:?}", b.answers);
        assert!(!b.answers.iter().any(|a| matches!(a, TouchRow::Hold | TouchRow::Grab)), "no grab: {:?}", b.answers);
        assert!(*b.offset_y.last().unwrap() > 20.0, "the area really panned: {:?}", b.offset_y);
    }

    /// A finger held on row 7 without moving is told `Hold` every frame and never grabs: the row
    /// leaves the press to egui, whose own long-press opens the row's context menu. Moving *after*
    /// the hold turns it into `Grab` — the row's drag — and the area's offset stays put throughout.
    #[test]
    fn a_still_hold_is_left_to_the_long_press_and_only_a_move_after_it_grabs() {
        let ctx = egui::Context::default();
        let mut b = Bench { answers: Vec::new(), all: Vec::new(), offset_y: Vec::new(), all_rows: false, horizontal: false };
        let at = egui::pos2(120.0, 150.0);
        let touch = |phase, pos| egui::Event::Touch { device_id: egui::TouchDeviceId(1), id: egui::TouchId(1), phase, pos, force: Some(0.5) };
        let button = |pos, pressed| egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed, modifiers: Default::default() };
        // a warm-up pass, as the app has run many frames before a finger lands: egui hit-tests the
        // press against the previous pass's widgets, and with none it has no click candidate
        touch_frame(&mut b, &ctx, vec![], -0.1);
        touch_frame(&mut b, &ctx, vec![touch(egui::TouchPhase::Start, at), egui::Event::PointerMoved(at), button(at, true)], 0.0);
        // held frames past the hold (no events, frames must run on their own)
        for k in 1..=8 {
            touch_frame(&mut b, &ctx, vec![], 0.1 * k as f64);
        }
        assert!(b.answers.contains(&TouchRow::Hold), "the hold is reported: {:?}", b.answers);
        assert!(!b.answers.contains(&TouchRow::Grab), "a still hold never grabs: {:?}", b.answers);
        assert_eq!(*b.offset_y.last().unwrap_or(&0.0), 0.0, "a still hold does not pan the area");
        // moving while still down, after the hold: the row's drag, and still no pan
        let p = egui::pos2(at.x, at.y - 30.0);
        touch_frame(&mut b, &ctx, vec![touch(egui::TouchPhase::Move, p), egui::Event::PointerMoved(p)], 1.0);
        assert_eq!(b.answers.last(), Some(&TouchRow::Grab), "a move after the hold grabs: {:?}", b.answers);
        // the instant scroll lands a frame later (a zero-length animation); settle one frame so a
        // stray scroll would show, and check the area never moved
        touch_frame(&mut b, &ctx, vec![], 1.05);
        assert_eq!(*b.offset_y.last().unwrap_or(&0.0), 0.0, "a grabbed press does not pan the area");
        // and it stays grabbed for the rest of the press, however far the finger travels
        let q = egui::pos2(at.x, at.y - 60.0);
        touch_frame(&mut b, &ctx, vec![touch(egui::TouchPhase::Move, q), egui::Event::PointerMoved(q)], 1.1);
        assert_eq!(b.answers.last(), Some(&TouchRow::Grab), "a held finger keeps its grab: {:?}", b.answers);
        touch_frame(&mut b, &ctx, vec![], 1.15);
        assert_eq!(*b.offset_y.last().unwrap_or(&0.0), 0.0, "a grabbed press does not pan the area");
        // lift: the answers end and nothing grabs afterwards
        touch_frame(&mut b, &ctx, vec![touch(egui::TouchPhase::End, q), button(q, false), egui::Event::PointerGone], 1.2);
        assert_eq!(b.answers.last(), Some(&TouchRow::None));
    }

    /// egui's own long-press opens the row's context menu once the finger has been still past its
    /// own timeout; a finger that then moves must not pick the row up underneath the open menu.
    #[test]
    fn a_move_under_the_long_press_menu_does_not_grab() {
        let ctx = egui::Context::default();
        let mut b = Bench { answers: Vec::new(), all: Vec::new(), offset_y: Vec::new(), all_rows: false, horizontal: false };
        let at = egui::pos2(120.0, 150.0);
        let touch = |phase, pos| egui::Event::Touch { device_id: egui::TouchDeviceId(1), id: egui::TouchId(1), phase, pos, force: Some(0.5) };
        let button = |pos, pressed| egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed, modifiers: Default::default() };
        // a warm-up pass, as the app has run many frames before a finger lands: egui hit-tests the
        // press against the previous pass's widgets, and with none it has no click candidate
        touch_frame(&mut b, &ctx, vec![], -0.1);
        touch_frame(&mut b, &ctx, vec![touch(egui::TouchPhase::Start, at), egui::Event::PointerMoved(at), button(at, true)], 0.0);
        // still, past egui's long-press timeout (0.8 s): its "secondary click" now owns the press
        for k in 1..=9 {
            touch_frame(&mut b, &ctx, vec![], 0.1 * k as f64);
        }
        assert!(b.answers.contains(&TouchRow::Hold), "the still hold is reported: {:?}", b.answers);
        // moving under the menu is not the row's drag
        let p = egui::pos2(at.x, at.y - 30.0);
        touch_frame(&mut b, &ctx, vec![touch(egui::TouchPhase::Move, p), egui::Event::PointerMoved(p)], 1.05);
        assert_eq!(b.answers.last(), Some(&TouchRow::Hold), "under the menu the row does not grab: {:?}", b.answers);
        touch_frame(&mut b, &ctx, vec![], 1.1);
        assert_eq!(*b.offset_y.last().unwrap_or(&0.0), 0.0, "and it does not pan either");
        touch_frame(&mut b, &ctx, vec![touch(egui::TouchPhase::End, p), button(p, false), egui::Event::PointerGone], 1.15);
        assert_eq!(b.answers.last(), Some(&TouchRow::None));
    }

    /// Every row of a scroll area consults the arbiter — that is how the real panels draw them —
    /// but one finger pans the area once per frame, never once per row.
    #[test]
    fn every_row_asking_still_pans_the_area_once() {
        let ctx = egui::Context::default();
        let mut b = Bench { answers: Vec::new(), all: Vec::new(), offset_y: Vec::new(), all_rows: true, horizontal: false };
        let at = egui::pos2(120.0, 150.0);
        let touch = |phase, pos| egui::Event::Touch { device_id: egui::TouchDeviceId(1), id: egui::TouchId(1), phase, pos, force: Some(0.5) };
        let button = |pos, pressed| egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed, modifiers: Default::default() };
        // a warm-up pass, as the app has run many frames before a finger lands: egui hit-tests the
        // press against the previous pass's widgets, and with none it has no click candidate
        touch_frame(&mut b, &ctx, vec![], -0.1);
        touch_frame(&mut b, &ctx, vec![touch(egui::TouchPhase::Start, at), egui::Event::PointerMoved(at), button(at, true)], 0.0);
        for k in 1..=5 {
            let p = egui::pos2(at.x, at.y - 15.0 * k as f32);
            touch_frame(&mut b, &ctx, vec![touch(egui::TouchPhase::Move, p), egui::Event::PointerMoved(p)], 0.1 * k as f64);
        }
        touch_frame(&mut b, &ctx, vec![], 0.6);
        let moved = *b.offset_y.last().unwrap_or(&0.0);
        // the finger travelled 75 px; twenty rows each adding their own copy would pin the area to
        // its 160 px end instead.
        assert!(moved > 20.0, "the area panned: {moved}");
        assert!(moved < 100.0, "one pan per frame, not one per row: {moved}");
    }

    /// A mouse press on the same row is None every frame: the desktop's own drag path answers
    /// from the response, untouched.
    #[test]
    fn a_mouse_press_is_not_arbitrated() {
        let ctx = egui::Context::default();
        let mut b = Bench { answers: Vec::new(), all: Vec::new(), offset_y: Vec::new(), all_rows: false, horizontal: false };
        let at = egui::pos2(120.0, 150.0);
        let button = |pos, pressed| egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed, modifiers: Default::default() };
        touch_frame(&mut b, &ctx, vec![egui::Event::PointerMoved(at), button(at, true)], 0.0);
        touch_frame(&mut b, &ctx, vec![], 0.6);
        assert!(b.answers.iter().all(|a| *a == TouchRow::None), "mouse presses stay None: {:?}", b.answers);
        let p = egui::pos2(at.x, at.y - 40.0);
        touch_frame(&mut b, &ctx, vec![egui::Event::PointerMoved(p)], 0.7);
        touch_frame(&mut b, &ctx, vec![button(p, false)], 0.8);
        assert!(*b.offset_y.last().unwrap_or(&0.0) == 0.0, "the area never pans from a mouse drag here");
    }

    /// Only the row the finger went down on may act on the press. Every other row of the area gets
    /// `None`: a hold-then-drag on one file must not import every visible file, and a scroll must
    /// not move whatever bin card it passes over.
    #[test]
    fn only_the_row_the_finger_went_down_on_gets_the_verdict() {
        let ctx = egui::Context::default();
        let mut b = Bench { answers: Vec::new(), all: Vec::new(), offset_y: Vec::new(), all_rows: true, horizontal: false };
        let at = egui::pos2(120.0, 150.0);
        let touch = |phase, pos| egui::Event::Touch { device_id: egui::TouchDeviceId(1), id: egui::TouchId(1), phase, pos, force: Some(0.5) };
        let button = |pos, pressed| egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed, modifiers: Default::default() };
        touch_frame(&mut b, &ctx, vec![], -0.1);
        touch_frame(&mut b, &ctx, vec![touch(egui::TouchPhase::Start, at), egui::Event::PointerMoved(at), button(at, true)], 0.0);
        // held past the hold, then moved: the pressed row's drag
        for k in 1..=8 {
            touch_frame(&mut b, &ctx, vec![], 0.1 * k as f64);
        }
        let p = egui::pos2(at.x, at.y - 30.0);
        touch_frame(&mut b, &ctx, vec![touch(egui::TouchPhase::Move, p), egui::Event::PointerMoved(p)], 0.9);
        assert_eq!(b.answers.last(), Some(&TouchRow::Grab), "the pressed row grabs: {:?}", b.answers);
        assert!(b.all.iter().any(|(i, a)| *i == 7 && *a == TouchRow::Grab), "row 7 grabbed: {:?}", b.all);
        assert!(b.all.iter().all(|(i, a)| *i == 7 || *a == TouchRow::None), "every other row must get None, never a verdict: {:?}", b.all);
    }

    /// A sideways drag on a control that moves sideways (a slider) is the control's own drag, not a
    /// scroll: the press settles as not-a-scroll, so the panel's slider branch runs and the area
    /// never pans.
    #[test]
    fn a_sideways_finger_drag_is_a_control_not_a_scroll() {
        let ctx = egui::Context::default();
        let mut b = Bench { answers: Vec::new(), all: Vec::new(), offset_y: Vec::new(), all_rows: false, horizontal: true };
        let at = egui::pos2(120.0, 150.0);
        let touch = |phase, pos| egui::Event::Touch { device_id: egui::TouchDeviceId(1), id: egui::TouchId(1), phase, pos, force: Some(0.5) };
        let button = |pos, pressed| egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed, modifiers: Default::default() };
        touch_frame(&mut b, &ctx, vec![], -0.1);
        touch_frame(&mut b, &ctx, vec![touch(egui::TouchPhase::Start, at), egui::Event::PointerMoved(at), button(at, true)], 0.0);
        // sideways, before the hold: a slider drag, never a scroll
        for k in 1..=4 {
            let p = egui::pos2(at.x + 15.0 * k as f32, at.y);
            touch_frame(&mut b, &ctx, vec![touch(egui::TouchPhase::Move, p), egui::Event::PointerMoved(p)], 0.1 * k as f64);
        }
        touch_frame(&mut b, &ctx, vec![], 0.5);
        assert!(!b.answers.contains(&TouchRow::Scroll), "a sideways drag is not a scroll: {:?}", b.answers);
        assert_eq!(*b.offset_y.last().unwrap_or(&0.0), 0.0, "and the area does not pan");
    }

    /// The same sideways move on an ordinary row is still a scroll: the hint marks the control,
    /// it does not turn every list sideways.
    #[test]
    fn a_sideways_finger_drag_still_scrolls_an_ordinary_row() {
        let ctx = egui::Context::default();
        let mut b = Bench { answers: Vec::new(), all: Vec::new(), offset_y: Vec::new(), all_rows: false, horizontal: false };
        let at = egui::pos2(120.0, 150.0);
        let touch = |phase, pos| egui::Event::Touch { device_id: egui::TouchDeviceId(1), id: egui::TouchId(1), phase, pos, force: Some(0.5) };
        let button = |pos, pressed| egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed, modifiers: Default::default() };
        touch_frame(&mut b, &ctx, vec![], -0.1);
        touch_frame(&mut b, &ctx, vec![touch(egui::TouchPhase::Start, at), egui::Event::PointerMoved(at), button(at, true)], 0.0);
        for k in 1..=4 {
            let p = egui::pos2(at.x + 15.0 * k as f32, at.y);
            touch_frame(&mut b, &ctx, vec![touch(egui::TouchPhase::Move, p), egui::Event::PointerMoved(p)], 0.1 * k as f64);
        }
        assert!(b.answers.contains(&TouchRow::Scroll), "an ordinary row still scrolls: {:?}", b.answers);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme::ThemeKind;

    fn frame(ctx: &egui::Context, events: Vec<egui::Event>, id: egui::Id) {
        let t = Tokens::for_kind(ThemeKind::Dark);
        let input = egui::RawInput { events, ..Default::default() };
        // no renderer takes the font atlas upload here; drop it explicitly (epaint panics otherwise)
        ctx.run_ui(input, |ui| {
            hot_number(ui, id, 123.5, 1.0, (0.0, 1000.0), 1, "", &t);
        })
        .textures_delta
        .clear();
    }

    #[test]
    fn clicking_a_hot_number_selects_its_whole_value() {
        let ctx = egui::Context::default();
        let id = egui::Id::new("hot");
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

    /// #269: a menu taller than the window has to be scrollable, and the Edit menu on a phone in
    /// landscape is exactly that — about 42 rows against the 26 visible, so everything from Select
    /// All down to Preferences sits below the fold. Dragging must scroll it whatever the host has
    /// detected about touch, because egui's default `DragScroll::OnTouch` waits for an
    /// `Event::Touch` that never arrives on the Android build.
    #[test]
    fn a_menu_taller_than_the_window_scrolls_by_drag() {
        let ctx = egui::Context::default();
        let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(400.0, 300.0));
        let mut rows: Vec<f32> = Vec::new();
        let frame = |events: Vec<egui::Event>, rows: &mut Vec<f32>| {
            let input = egui::RawInput { screen_rect: Some(screen), events, ..Default::default() };
            let mut out = ctx.run_ui(input, |ui| {
                menu_scroll(ui, |ui| {
                    rows.clear();
                    for i in 0..42 {
                        // A menu row spans the popup, so a finger anywhere on a row starts the drag.
                        let r = ui.add_sized([ui.available_width(), 20.0], egui::Button::new(format!("row {i}")));
                        rows.push(r.rect.min.y);
                    }
                });
            });
            out.textures_delta.clear();
        };
        frame(Vec::new(), &mut rows);
        frame(Vec::new(), &mut rows);
        // without scrolling the menu simply runs off the screen
        let last = *rows.last().unwrap_or(&0.0);
        assert!(last > screen.height(), "the test menu must overflow its window: last row at {last} pt of {} pt", screen.height());
        // a press on a row, then a drag upward: the rows move, so the ones below the fold
        // (Select All through Preferences) become reachable
        let start = egui::pos2(120.0, 200.0);
        let press = |p, pressed| egui::Event::PointerButton { pos: p, button: egui::PointerButton::Primary, pressed, modifiers: Default::default() };
        frame(vec![egui::Event::PointerMoved(start)], &mut rows);
        frame(vec![press(start, true)], &mut rows);
        for k in 1..=4 {
            frame(vec![egui::Event::PointerMoved(start - egui::vec2(0.0, 25.0 * k as f32))], &mut rows);
        }
        let scrolled = *rows.last().unwrap_or(&0.0);
        frame(vec![press(start - egui::vec2(0.0, 100.0), false)], &mut rows);
        assert!(scrolled < last - 50.0, "the drag must scroll the menu: last row was at {last} pt, now {scrolled}");
    }
}
