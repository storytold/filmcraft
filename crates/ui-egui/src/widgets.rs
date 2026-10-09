//! Small custom widgets in Premiere's visual language.

use egui::{Align2, Color32, Rect, Response, Sense, Stroke, StrokeKind, Ui, pos2, vec2};

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

/// The part of `r` a click can reach: `r` clipped to the ui's visible area, or `None` when its centre
/// is scrolled out of view (an entry of a long dropdown list). Register an automation id with it so
/// `ui.click` never presses whatever lies under a hidden entry.
pub fn visible(ui: &Ui, r: Rect) -> Option<Rect> {
    let clip = ui.clip_rect();
    clip.contains(r.center()).then(|| r.intersect(clip))
}

/// The title-bar close button (×) of an `egui::Window` with a close button (`.open(&mut open)`),
/// from the window's outer rect (its response rect; for a window whose content outgrows its
/// `fixed_size`, the rect its title bar spans: that size from the window's left edge) and its
/// frame's inner margin (`None`: the default window frame), as egui lays the title bar out: the
/// button is the last item of the title row, a heading-line-high square inside the frame's margin
/// and stroke. Register an automation id on it so `ui.click` closes the window as the mouse does.
pub fn window_close_rect(ctx: &egui::Context, window: Rect, margin: Option<egui::Margin>) -> Rect {
    let style = ctx.global_style();
    let m = margin.unwrap_or(style.spacing.window_margin);
    let stroke = style.visuals.window_stroke.width;
    let h = ctx.fonts_mut(|f| f.row_height(&egui::TextStyle::Heading.resolve(&style)));
    let icon = style.spacing.icon_width;
    let c = pos2(window.max.x - stroke - f32::from(m.right) - h / 2.0, window.min.y + stroke + f32::from(m.top) + h / 2.0);
    Rect::from_center_size(c, vec2(icon, icon))
}

/// A large blue timecode readout (monitors/timeline). Click to type a new time.
pub fn timecode_label(ui: &mut Ui, rect: Rect, text: &str, size: f32, color: Color32, align: Align2) -> Response {
    let resp = ui.interact(rect, ui.id().with(("tc", rect.min.x as i32, rect.min.y as i32)), Sense::click_and_drag());
    ui.painter().text(align.pos_in_rect(&rect), align, text, Tokens::mono(size), color);
    resp
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
