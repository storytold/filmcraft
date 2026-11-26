//! The Prompter overlay on the Program monitor (`view.prompter`, off by default): a band along the
//! bottom of the picture with the section at the playhead (a range marker), its elapsed time and
//! length with a progress bar, the talking points around the current one, the script at the
//! current position, the next section near the end, the caption at the playhead, and a warning
//! when the talking points need more time than the section has.
//!
//! What it shows comes from [`filmcraft_engine::prompter::prompt_at`]; this module only draws it.
//! It is drawn with the monitor's painter, so it never reaches a render or an export, and it is
//! still drawn in full-screen Program. Automation id `program.prompter`; its label and its
//! accessibility node carry the text it shows.

use egui::text::{LayoutJob, TextFormat, TextWrapping};
use egui::{Align2, Color32, FontId, Rect, Sense, pos2, vec2};
use filmcraft_engine::prompter::{Point, PointState, Prompt, Section, prompt_at};
use filmcraft_time::{TICKS_PER_SECOND, Tick};

use crate::FilmcraftApp;
use crate::panels::monitor::Which;
use crate::state::MonitorView;
use crate::theme::Tokens;

/// Opacity of the band over the picture: high enough that its text keeps its contrast over any frame.
const PLATE_OPACITY: f32 = 0.92;
/// Talking points listed at once: the one before the current point, the current one and those after.
const POINT_ROWS: usize = 4;
/// Script lines shown at once; the current one is the second.
const SCRIPT_ROWS: usize = 3;

/// The overlay's colours, all theme tokens.
#[derive(Clone, Copy, Debug)]
pub struct Palette {
    /// The band (`panel_bg`, nearly opaque).
    pub plate: Color32,
    pub text: Color32,
    /// Finished points and script already read.
    pub dim: Color32,
    /// Behind the current point.
    pub current: Color32,
    /// The progress bar's fill and the script's reading line; the bar's outline is `dim`.
    pub bar: Color32,
}

pub fn palette(t: &Tokens) -> Palette {
    Palette { plate: t.panel_bg.gamma_multiply(PLATE_OPACITY), text: t.text, dim: t.text_dim, current: t.row_selected, bar: t.text }
}

/// `m:ss`, or `h:mm:ss` from an hour on, in whole seconds rounded down (time elapsed).
fn clock(t: Tick) -> String {
    hms(t.0.max(0) / TICKS_PER_SECOND)
}

/// [`clock`] rounded up, for lengths and countdowns: a section a few frames short of 8 s reads
/// 0:08, and a countdown reaches 0:00 only when it is over.
fn clock_up(t: Tick) -> String {
    let t = t.0.max(0);
    hms(t / TICKS_PER_SECOND + i64::from(t % TICKS_PER_SECOND != 0))
}

fn hms(s: i64) -> String {
    let (h, m, s) = (s / 3600, s / 60 % 60, s % 60);
    if h > 0 { format!("{h}:{m:02}:{s:02}") } else { format!("{m}:{s:02}") }
}

/// One line of `text`, cut with an ellipsis to `width`.
fn line_job(text: &str, font: FontId, color: Color32, width: f32) -> LayoutJob {
    let mut job = LayoutJob::simple_singleline(text.to_owned(), font, color);
    job.wrap = TextWrapping { max_width: width.max(1.0), max_rows: 1, break_anywhere: true, overflow_character: Some('…') };
    job
}

fn point_label(p: &Point) -> String {
    if p.text.is_empty() { "—".to_owned() } else { p.text.clone() }
}

/// The text of the overlay, in reading order: its automation label and accessibility name.
pub fn spoken(p: &Prompt) -> String {
    let mut parts = Vec::new();
    match &p.section {
        Some(s) => {
            parts.push(format!("{} {}", s.name, tlf!("{elapsed} of {length}", elapsed = clock(s.elapsed), length = clock_up(s.duration))));
            if let Some(c) = s.current.and_then(|i| s.points.get(i)) {
                parts.push(tlf!("Now: {point}", point = point_label(c)));
                if let Some(n) = s.current.and_then(|i| s.points.get(i + 1)) {
                    parts.push(tlf!("Then: {point}", point = point_label(n)));
                }
            }
            if let Some(line) = s.script.as_ref().and_then(|x| script_excerpt(&x.text, x.offset)) {
                parts.push(tlf!("Script: {text}", text = line));
            }
        }
        None if p.sections == 0 => parts.push(tl!("No sections: range markers on the sequence become prompter sections").to_owned()),
        None => parts.push(tl!("No section at the playhead").to_owned()),
    }
    parts.extend(notices(p));
    // one sentence per part; a part that already ends a sentence gets no second period
    let mut out = String::new();
    for part in parts {
        if !out.is_empty() {
            out.push_str(if out.ends_with(['.', '!', '?', '…']) { " " } else { ". " });
        }
        out.push_str(&part);
    }
    out
}

/// The script from the start of the word at `offset` to the end of that sentence (or line).
fn script_excerpt(text: &str, offset: usize) -> Option<String> {
    let chars: Vec<char> = text.chars().collect();
    let mut start = offset.min(chars.len());
    while start > 0 && chars.get(start - 1).is_some_and(|c| !c.is_whitespace()) {
        start -= 1;
    }
    let rest = chars.get(start..).unwrap_or_default();
    let end = rest.iter().position(|c| matches!(c, '.' | '!' | '?' | '\n')).map_or(rest.len(), |i| i + 1);
    let excerpt: String = rest.get(..end).unwrap_or_default().iter().collect();
    let excerpt = excerpt.trim();
    (!excerpt.is_empty()).then(|| excerpt.to_owned())
}

/// The lines under the body: the next section, the caption, and warnings.
fn notices(p: &Prompt) -> Vec<String> {
    let mut out = Vec::new();
    if let Some(n) = p.next.as_ref().filter(|n| n.soon) {
        out.push(tlf!("Next: {name} in {time}", name = n.name, time = clock_up(n.starts_in)));
    }
    if let Some(c) = &p.caption {
        out.push(tlf!("Caption: {text}", text = c.lines.join(" / ")));
    }
    out.extend(p.section.iter().flat_map(warnings));
    out
}

fn warnings(s: &Section) -> Vec<String> {
    let mut out = Vec::new();
    if let Some(o) = s.overrun {
        if o.over > Tick::ZERO {
            out.push(tlf!("⚠ Talking points run {time} past the end of the section", time = clock_up(o.over)));
        }
        if o.untimed > 0 {
            out.push(tlf!("⚠ No time left for {n} talking points without a duration", n = o.untimed));
        }
    }
    if s.truncated {
        out.push(tl!("⚠ The marker's comment is too long: only its start is shown").to_owned());
    }
    out
}

/// The Program monitor's Prompter, when it is turned on in `mv` (the settings as drawn).
pub fn monitor_overlay(app: &mut FilmcraftApp, ui: &mut egui::Ui, which: Which, mv: &MonitorView, area: Rect, time: Tick) {
    if which == Which::Program && mv.prompter {
        overlay(app, ui, area, time);
    }
}

/// Draw the prompter for `time` (the playhead) along the bottom of the visible picture `area`.
fn overlay(app: &mut FilmcraftApp, ui: &mut egui::Ui, area: Rect, time: Tick) {
    let Some(seq) = app.session.active_sequence() else { return };
    if area.width() < 60.0 || area.height() < 40.0 {
        return;
    }
    let prompt = prompt_at(seq, time);
    let c = palette(&app.tokens);
    let size = (area.height() * 0.035).clamp(11.0, 22.0);
    let pad = (size * 0.6).round();
    let row_h = (size * 1.45).round();
    let inner_w = area.width() - 2.0 * pad;
    let painter = ui.painter().with_clip_rect(ui.clip_rect().intersect(area));

    // the band's height from what it holds, at most 45% of the picture
    let notices = notices(&prompt);
    let body_rows = prompt.section.as_ref().map_or(0, |s| {
        let points = s.points.len().min(POINT_ROWS);
        let script = if s.script.is_some() { SCRIPT_ROWS } else { 0 };
        points.max(script)
    });
    let header_h = row_h + (size * 0.5).round() + pad * 0.5;
    let wanted = pad * 2.0 + header_h + body_rows as f32 * row_h + notices.len() as f32 * row_h;
    let band_h = wanted.min(area.height() * 0.45).max(row_h + pad * 2.0);
    let band = Rect::from_min_max(pos2(area.min.x, area.max.y - band_h), area.max);
    painter.rect_filled(band, 0.0, c.plate);
    let content = band.shrink(pad);
    let painter = painter.with_clip_rect(painter.clip_rect().intersect(content.expand(1.0)));

    // ---- header: section name | elapsed / length, then the progress bar
    let mut y = content.min.y;
    match &prompt.section {
        Some(s) => {
            let times = format!("{} / {}", clock(s.elapsed), clock_up(s.duration));
            let tg = painter.layout_no_wrap(times, Tokens::ui(size), c.text);
            let name_w = (inner_w - tg.size().x - pad).max(1.0);
            let ng = painter.layout_job(line_job(&s.name, Tokens::semibold(size * 1.1), c.text, name_w));
            painter.galley(pos2(content.min.x, y), ng, c.text);
            painter.galley(pos2(content.max.x - tg.size().x, y), tg, c.text);
            y += row_h;
            let bar = Rect::from_min_size(pos2(content.min.x, y), vec2(inner_w, (size * 0.35).round().max(3.0)));
            let fill = Rect::from_min_size(bar.min, vec2(bar.width() * s.progress as f32, bar.height()));
            painter.rect_filled(fill, bar.height() / 2.0, c.bar);
            painter.rect_stroke(bar, bar.height() / 2.0, egui::Stroke::new(1.0, c.dim), egui::StrokeKind::Inside);
            y = bar.max.y + pad * 0.5;
        }
        None => {
            let text = if prompt.sections == 0 {
                tl!("No sections: range markers on the sequence become prompter sections")
            } else {
                tl!("No section at the playhead")
            };
            let g = painter.layout_job(line_job(text, Tokens::ui(size), c.dim, inner_w));
            painter.galley(pos2(content.min.x, y), g, c.dim);
            y += row_h;
        }
    }

    // ---- body: talking points | script
    if let Some(s) = &prompt.section {
        let body = Rect::from_min_max(pos2(content.min.x, y), pos2(content.max.x, (y + body_rows as f32 * row_h).min(content.max.y)));
        let (points_r, script_r) = match (s.points.is_empty(), s.script.is_none()) {
            (false, false) => {
                let mid = body.min.x + body.width() * 0.42;
                (Some(Rect::from_min_max(body.min, pos2(mid - pad * 0.5, body.max.y))), Some(Rect::from_min_max(pos2(mid + pad * 0.5, body.min.y), body.max)))
            }
            (false, true) => (Some(body), None),
            (true, false) => (None, Some(body)),
            (true, true) => (None, None),
        };
        if let Some(r) = points_r {
            points(&painter, r, s, size, row_h, &c);
        }
        if let (Some(r), Some(script)) = (script_r, &s.script) {
            script_lines(&painter, r, &script.text, script.offset, size, row_h, &c);
        }
        y = body.max.y;
    }

    // ---- next section, caption, warnings
    for n in &notices {
        if y + row_h > content.max.y + 1.0 {
            break;
        }
        // a warning is told by its sign and its weight, not by a colour
        let font = if n.starts_with('⚠') { Tokens::semibold(size * 0.95) } else { Tokens::ui(size * 0.95) };
        let g = painter.layout_job(line_job(n, font, c.text, inner_w));
        painter.galley(pos2(content.min.x, y), g, c.text);
        y += row_h;
    }

    let label = spoken(&prompt);
    let response = ui.interact(band, egui::Id::unique("program-prompter"), Sense::hover());
    response.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Label, true, &label));
    app.auto.add("program.prompter", band, &label);
}

/// The talking points around the current one: finished ones dimmed with a check, the current one
/// on a highlight in semibold with an arrow, the next ones plain with a bullet.
fn points(painter: &egui::Painter, r: Rect, s: &Section, size: f32, row_h: f32, c: &Palette) {
    let rows = ((r.height() / row_h).floor() as usize).clamp(1, POINT_ROWS);
    // the point to show first: the one before the current, else before the first one still to come
    let focus = s.current.or_else(|| s.points.iter().position(|p| p.state == PointState::Upcoming)).unwrap_or(s.points.len());
    let first = focus.saturating_sub(1).min(s.points.len().saturating_sub(rows));
    // with room for three rows or more, the last says how many are not listed instead of cutting the list silently
    let more = s.points.len().saturating_sub(first + rows);
    let listed = if more > 0 && rows >= 3 { rows - 1 } else { rows };
    for (i, p) in s.points.iter().enumerate().skip(first).take(listed) {
        let row = Rect::from_min_size(pos2(r.min.x, r.min.y + (i - first) as f32 * row_h), vec2(r.width(), row_h));
        let (mark, font, color) = match p.state {
            PointState::Done => ("✓", Tokens::ui(size), c.dim),
            PointState::Current => ("▶", Tokens::semibold(size), c.text),
            PointState::Upcoming => ("•", Tokens::ui(size), c.text),
        };
        if p.state == PointState::Current {
            painter.rect_filled(row, 3.0, c.current);
        }
        painter.text(pos2(row.min.x + 4.0, row.center().y), Align2::LEFT_CENTER, mark, Tokens::ui(size), color);
        let x = row.min.x + size * 1.4;
        let g = painter.layout_job(line_job(&point_label(p), font, color, row.max.x - x - 4.0));
        let gy = row.center().y - g.size().y / 2.0;
        painter.galley(pos2(x, gy), g, color);
    }
    if listed < rows {
        let y = r.min.y + listed as f32 * row_h + row_h / 2.0;
        let hidden = s.points.len().saturating_sub(first + listed);
        painter.text(pos2(r.min.x + size * 1.4, y), Align2::LEFT_CENTER, tlf!("{n} more", n = hidden), Tokens::ui(size * 0.9), c.dim);
    }
}

/// The script, wrapped to `r`, scrolled so the narrator's position sits on the second line (marked
/// by a bar at the left); what is already read is dimmed.
fn script_lines(painter: &egui::Painter, r: Rect, text: &str, offset: usize, size: f32, row_h: f32, c: &Palette) {
    let split = text.char_indices().nth(offset).map_or(text.len(), |(b, _)| b);
    let font = Tokens::ui(size);
    let mut job = LayoutJob::default();
    let format = |color| TextFormat { font_id: font.clone(), color, line_height: Some(row_h), ..Default::default() };
    job.append(text.get(..split).unwrap_or(""), 0.0, format(c.dim));
    job.append(text.get(split..).unwrap_or(""), 0.0, format(c.text));
    job.wrap.max_width = (r.width() - 8.0).max(1.0);
    let galley = painter.layout_job(job);
    // the row holding `offset`, and how far through it the position is
    let mut seen = 0usize;
    let mut y = 0.0;
    let last = galley.rows.len().saturating_sub(1);
    for (i, row) in galley.rows.iter().enumerate() {
        let n = row.char_count_including_newline().0;
        if offset < seen + n || i == last {
            let within = if n > 0 { (offset.saturating_sub(seen) as f32 / n as f32).clamp(0.0, 1.0) } else { 0.0 };
            y = row.rect().min.y + within * row.rect().height();
            break;
        }
        seen += n;
    }
    let reading_line = r.min.y + row_h * (SCRIPT_ROWS as f32 / 2.0 - 0.5).max(0.0);
    let clip = painter.clip_rect().intersect(r);
    let p = painter.with_clip_rect(clip);
    p.rect_filled(Rect::from_min_size(pos2(r.min.x, reading_line), vec2(3.0, row_h)), 0.0, c.bar);
    p.galley(pos2(r.min.x + 8.0, reading_line + row_h * 0.5 - y), galley, c.text);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme::ThemeKind;

    fn luminance(c: Color32) -> f64 {
        let ch = |v: u8| {
            let v = f64::from(v) / 255.0;
            if v <= 0.040_45 { v / 12.92 } else { ((v + 0.055) / 1.055).powf(2.4) }
        };
        0.2126 * ch(c.r()) + 0.7152 * ch(c.g()) + 0.0722 * ch(c.b())
    }

    fn contrast(a: Color32, b: Color32) -> f64 {
        let (x, y) = (luminance(a), luminance(b));
        (x.max(y) + 0.05) / (x.min(y) + 0.05)
    }

    /// `top` (premultiplied) over the opaque `under`.
    fn over(top: Color32, under: Color32) -> Color32 {
        let a = f32::from(top.a()) / 255.0;
        let mix = |t: u8, u: u8| (f32::from(t) + f32::from(u) * (1.0 - a)).round().min(255.0) as u8;
        Color32::from_rgb(mix(top.r(), under.r()), mix(top.g(), under.g()), mix(top.b(), under.b()))
    }

    #[test]
    fn the_prompter_text_keeps_its_contrast_over_any_picture_in_every_theme() {
        for kind in [ThemeKind::Dark, ThemeKind::Medium, ThemeKind::Light] {
            let c = palette(&Tokens::for_kind(kind));
            for picture in [Color32::BLACK, Color32::WHITE, Color32::from_rgb(255, 0, 0), Color32::from_rgb(0, 0, 255)] {
                let plate = over(c.plate, picture);
                for (what, fg, bg, min) in [
                    ("text", c.text, plate, 4.5),
                    ("finished points and read script", c.dim, plate, 4.5),
                    ("the current point", c.text, c.current, 4.5),
                    ("the progress bar and reading line", c.bar, plate, 3.0),
                ] {
                    let ratio = contrast(fg, bg);
                    assert!(ratio >= min, "{kind:?} over {picture:?}: {what} has {ratio:.2}:1");
                }
            }
        }
    }
}
