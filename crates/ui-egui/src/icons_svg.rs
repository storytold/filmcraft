//! Paint Lucide-compatible craft-icons SVGs as egui strokes (24×24, 2px, round caps).

use egui::epaint::{PathShape, PathStroke};
use egui::{Color32, Painter, Pos2, Rect, Stroke, pos2, vec2};

use crate::icons::Icon;

/// SVG source for icons that have a craft-icons glyph. `None` keeps the code-drawn 16×16 icon.
pub fn svg_for(icon: Icon) -> Option<&'static str> {
    Some(match icon {
        Icon::Selection => include_str!("../../../assets/icons/craft/mouse-pointer.svg"),
        Icon::TrackSelectFwd => include_str!("../../../assets/icons/craft/track-select-forward.svg"),
        Icon::TrackSelectBack => include_str!("../../../assets/icons/craft/track-select-backward.svg"),
        Icon::Ripple => include_str!("../../../assets/icons/craft/ripple-edit.svg"),
        Icon::Rolling => include_str!("../../../assets/icons/craft/rolling-edit.svg"),
        Icon::RateStretch => include_str!("../../../assets/icons/craft/rate-stretch.svg"),
        Icon::Razor => include_str!("../../../assets/icons/craft/razor.svg"),
        Icon::Slip => include_str!("../../../assets/icons/craft/slip.svg"),
        Icon::Slide => include_str!("../../../assets/icons/craft/slide.svg"),
        Icon::Pen => include_str!("../../../assets/icons/craft/pen-tool.svg"),
        Icon::Rectangle => include_str!("../../../assets/icons/craft/square.svg"),
        Icon::Hand => include_str!("../../../assets/icons/craft/hand.svg"),
        Icon::Zoom => include_str!("../../../assets/icons/craft/zoom-in.svg"),
        Icon::Type => include_str!("../../../assets/icons/craft/type.svg"),
        Icon::Play => include_str!("../../../assets/icons/craft/play.svg"),
        Icon::Pause => include_str!("../../../assets/icons/craft/pause.svg"),
        Icon::StepBack => include_str!("../../../assets/icons/craft/skip-back.svg"),
        Icon::StepFwd => include_str!("../../../assets/icons/craft/skip-forward.svg"),
        Icon::MarkIn => include_str!("../../../assets/icons/craft/mark-in.svg"),
        Icon::MarkOut => include_str!("../../../assets/icons/craft/mark-out.svg"),
        Icon::Marker => include_str!("../../../assets/icons/craft/marker-timeline.svg"),
        Icon::Insert => include_str!("../../../assets/icons/craft/insert.svg"),
        Icon::Overwrite => include_str!("../../../assets/icons/craft/overwrite.svg"),
        Icon::Lift => include_str!("../../../assets/icons/craft/lift.svg"),
        Icon::Extract => include_str!("../../../assets/icons/craft/extract.svg"),
        Icon::Eye => include_str!("../../../assets/icons/craft/eye.svg"),
        Icon::EyeOff => include_str!("../../../assets/icons/craft/eye-off.svg"),
        Icon::Speaker => include_str!("../../../assets/icons/craft/volume-2.svg"),
        Icon::Mute => include_str!("../../../assets/icons/craft/volume-x.svg"),
        Icon::Lock => include_str!("../../../assets/icons/craft/lock.svg"),
        Icon::Unlock => include_str!("../../../assets/icons/craft/lock-open.svg"),
        Icon::Magnet => include_str!("../../../assets/icons/craft/magnet.svg"),
        Icon::Link => include_str!("../../../assets/icons/craft/link.svg"),
        Icon::Keyframe => include_str!("../../../assets/icons/craft/keyframe.svg"),
        Icon::Undo => include_str!("../../../assets/icons/craft/undo-2.svg"),
        Icon::Redo => include_str!("../../../assets/icons/craft/redo-2.svg"),
        Icon::Trash => include_str!("../../../assets/icons/craft/trash.svg"),
        Icon::Film => include_str!("../../../assets/icons/craft/film.svg"),
        Icon::Image => include_str!("../../../assets/icons/craft/image.svg"),
        Icon::Audio => include_str!("../../../assets/icons/craft/music.svg"),
        Icon::Folder => include_str!("../../../assets/icons/craft/folder.svg"),
        _ => return None,
    })
}

/// Lucide's 24×24 viewBox keeps a 2-unit pad; mapping 2..22 onto `rect` fills a toolbar
/// slot the way the old 16-grid icons did, instead of sitting small inside extra padding.
const CONTENT: f32 = 20.0;
const ORIGIN: f32 = 2.0;
/// Lucide stroke in viewBox units.
const STROKE: f32 = 2.0;

pub fn paint(painter: &Painter, rect: Rect, svg: &str, color: Color32) {
    let side = rect.width().min(rect.height());
    if side <= 0.0 {
        return;
    }
    let s = side / CONTENT;
    let o = rect.center() - vec2((12.0 - ORIGIN) * s, (12.0 - ORIGIN) * s);
    let map = |x: f32, y: f32| pos2(o.x + (x - ORIGIN) * s, o.y + (y - ORIGIN) * s);
    let ppp = painter.ctx().pixels_per_point();
    let width = (STROKE * s * ppp - 0.5).ceil().max(1.0) / ppp;
    let stroke = PathStroke::new(width, color);
    let snap_line = |pts: Vec<(f32, f32)>, closed: bool| crisp(painter, &pts, closed, map, width);
    for el in elements(svg) {
        match el {
            El::Path(d) => {
                for (pts, closed) in tessellate_path(&d) {
                    if pts.len() < 2 {
                        continue;
                    }
                    let v = snap_line(pts, closed);
                    if closed {
                        painter.add(PathShape::closed_line(v, stroke.clone()));
                    } else {
                        painter.add(PathShape::line(v, stroke.clone()));
                    }
                }
            }
            El::Line(x1, y1, x2, y2) => {
                let v = snap_line(vec![(x1, y1), (x2, y2)], false);
                if let (Some(&a), Some(&b)) = (v.first(), v.get(1)) {
                    painter.line_segment([a, b], Stroke::new(width, color));
                }
            }
            El::Rect(x, y, w, h, rx) => {
                let pts = rounded_rect(x, y, w, h, rx);
                painter.add(PathShape::closed_line(snap_line(pts, true), stroke.clone()));
            }
            El::Circle(cx, cy, r) => {
                painter.circle_stroke(map(cx, cy), r * s, Stroke::new(width, color));
            }
            El::Polyline(pts, closed) => {
                if pts.len() < 2 {
                    continue;
                }
                let v = snap_line(pts, closed);
                if closed {
                    painter.add(PathShape::closed_line(v, stroke.clone()));
                } else {
                    painter.add(PathShape::line(v, stroke.clone()));
                }
            }
        }
    }
}

fn snap_coord(v: f32, centre: f32, mid: bool, ppp: f32, width: f32) -> f32 {
    let half = if mid && (width * ppp).round() as i32 % 2 == 1 { 0.5 } else { 0.0 };
    let base = (centre * ppp).round() + half;
    (base + ((v - centre) * ppp).round()) / ppp
}

/// Horizontal and vertical segments snap to the device pixel grid (same rule as `Pen16`).
fn crisp(painter: &Painter, pts: &[(f32, f32)], closed: bool, map: impl Fn(f32, f32) -> Pos2, width: f32) -> Vec<Pos2> {
    let n = pts.len();
    let ppp = painter.ctx().pixels_per_point();
    let c = {
        let mut min = pos2(f32::MAX, f32::MAX);
        let mut max = pos2(f32::MIN, f32::MIN);
        for &(x, y) in pts {
            let q = map(x, y);
            min.x = min.x.min(q.x);
            min.y = min.y.min(q.y);
            max.x = max.x.max(q.x);
            max.y = max.y.max(q.y);
        }
        pos2((min.x + max.x) * 0.5, (min.y + max.y) * 0.5)
    };
    let q: Vec<Pos2> = pts.iter().map(|&(x, y)| map(x, y)).collect();
    let seg = |i: usize, j: usize| {
        let Some(a) = q.get(i) else { return (false, false) };
        let Some(b) = q.get(j) else { return (false, false) };
        let d = (*a - *b) * ppp;
        (d.x.abs() < 1.0, d.y.abs() < 1.0)
    };
    (0..n)
        .map(|i| {
            let mut vert = false;
            let mut horiz = false;
            if i > 0 || closed {
                let s = seg((i + n - 1) % n, i);
                vert |= s.0;
                horiz |= s.1;
            }
            if i + 1 < n || closed {
                let s = seg(i, (i + 1) % n);
                vert |= s.0;
                horiz |= s.1;
            }
            let Some(&q) = q.get(i) else {
                return pos2(0.0, 0.0);
            };
            if !vert && !horiz {
                return q;
            }
            pos2(snap_coord(q.x, c.x, vert, ppp, width), snap_coord(q.y, c.y, horiz, ppp, width))
        })
        .collect()
}

enum El {
    Path(String),
    Line(f32, f32, f32, f32),
    Rect(f32, f32, f32, f32, f32),
    Circle(f32, f32, f32),
    Polyline(Vec<(f32, f32)>, bool),
}

fn elements(svg: &str) -> Vec<El> {
    let mut out = Vec::new();
    let lower = svg;
    let mut i = 0;
    let bytes = lower.as_bytes();
    while i < bytes.len() {
        if bytes.get(i) != Some(&b'<') {
            i += 1;
            continue;
        }
        let rest = &lower[i + 1..];
        let name_end = rest.find(|c: char| c.is_whitespace() || c == '>' || c == '/').unwrap_or(0);
        let tag = rest.get(..name_end).unwrap_or("");
        let close = rest.find('>').unwrap_or(rest.len());
        let attrs = rest.get(name_end..close).unwrap_or("");
        i += 1 + close + 1;
        match tag {
            "path" => {
                if let Some(d) = attr(attrs, "d") {
                    out.push(El::Path(d));
                }
            }
            "line" => out.push(El::Line(num(attrs, "x1"), num(attrs, "y1"), num(attrs, "x2"), num(attrs, "y2"))),
            "rect" => out.push(El::Rect(num(attrs, "x"), num(attrs, "y"), num(attrs, "width"), num(attrs, "height"), num(attrs, "rx"))),
            "circle" => out.push(El::Circle(num(attrs, "cx"), num(attrs, "cy"), num(attrs, "r"))),
            "polyline" => out.push(El::Polyline(parse_points(&attr(attrs, "points").unwrap_or_default()), false)),
            "polygon" => out.push(El::Polyline(parse_points(&attr(attrs, "points").unwrap_or_default()), true)),
            _ => {}
        }
    }
    out
}

fn attr(attrs: &str, key: &str) -> Option<String> {
    let pat = format!("{key}=\"");
    let i = attrs.find(&pat)?;
    let rest = &attrs[i + pat.len()..];
    let j = rest.find('"')?;
    Some(rest[..j].to_string())
}

fn num(attrs: &str, key: &str) -> f32 {
    attr(attrs, key).and_then(|s| s.parse().ok()).unwrap_or(0.0)
}

fn parse_points(s: &str) -> Vec<(f32, f32)> {
    let nums: Vec<f32> = tokenize_nums(s);
    nums.chunks(2).filter_map(|c| match c {
        [x, y] => Some((*x, *y)),
        _ => None,
    }).collect()
}

fn tokenize_nums(s: &str) -> Vec<f32> {
    let mut out = Vec::new();
    let b = s.as_bytes();
    let mut i = 0;
    while i < b.len() {
        let c = b[i];
        if c == b'+' || c == b'-' || c == b'.' || c.is_ascii_digit() {
            let start = i;
            if c == b'+' || c == b'-' {
                i += 1;
            }
            while i < b.len() && b[i].is_ascii_digit() {
                i += 1;
            }
            if i < b.len() && b[i] == b'.' {
                i += 1;
                while i < b.len() && b[i].is_ascii_digit() {
                    i += 1;
                }
            }
            if i < b.len() && (b[i] == b'e' || b[i] == b'E') {
                i += 1;
                if i < b.len() && (b[i] == b'+' || b[i] == b'-') {
                    i += 1;
                }
                while i < b.len() && b[i].is_ascii_digit() {
                    i += 1;
                }
            }
            if let Ok(v) = s[start..i].parse() {
                out.push(v);
            }
        } else {
            i += 1;
        }
    }
    out
}

fn rounded_rect(x: f32, y: f32, w: f32, h: f32, rx: f32) -> Vec<(f32, f32)> {
    let rx = rx.min(w / 2.0).min(h / 2.0).max(0.0);
    if rx <= 0.05 {
        return vec![(x, y), (x + w, y), (x + w, y + h), (x, y + h)];
    }
    let mut pts = Vec::new();
    let q = |cx: f32, cy: f32, a0: f32, a1: f32| {
        let n = 6;
        (0..=n).map(move |i| {
            let t = a0 + (a1 - a0) * i as f32 / n as f32;
            (cx + rx * t.cos(), cy + rx * t.sin())
        })
    };
    pts.extend(q(x + w - rx, y + rx, -std::f32::consts::FRAC_PI_2, 0.0));
    pts.extend(q(x + w - rx, y + h - rx, 0.0, std::f32::consts::FRAC_PI_2));
    pts.extend(q(x + rx, y + h - rx, std::f32::consts::FRAC_PI_2, std::f32::consts::PI));
    pts.extend(q(x + rx, y + rx, std::f32::consts::PI, 1.5 * std::f32::consts::PI));
    pts
}

fn tessellate_path(d: &str) -> Vec<(Vec<(f32, f32)>, bool)> {
    let cmds = parse_path(d);
    let mut out = Vec::new();
    let mut cur = (0.0f32, 0.0f32);
    let mut start = cur;
    let mut prev_c: Option<(f32, f32)> = None;
    let mut pts: Vec<(f32, f32)> = Vec::new();
    let flush = |pts: &mut Vec<(f32, f32)>, out: &mut Vec<(Vec<(f32, f32)>, bool)>, closed: bool| {
        if pts.len() >= 2 {
            out.push((std::mem::take(pts), closed));
        } else {
            pts.clear();
        }
    };
    for (cmd, args) in cmds {
        let rel = cmd.is_ascii_lowercase();
        let c = cmd.to_ascii_uppercase();
        let mut a = args.as_slice();
        match c {
            'M' => {
                flush(&mut pts, &mut out, false);
                while let [x, y, rest @ ..] = a {
                    cur = abs_pt(rel, cur, *x, *y);
                    if pts.is_empty() {
                        start = cur;
                    }
                    pts.push(cur);
                    a = rest;
                }
                prev_c = None;
            }
            'L' => {
                while let [x, y, rest @ ..] = a {
                    cur = abs_pt(rel, cur, *x, *y);
                    pts.push(cur);
                    a = rest;
                }
                prev_c = None;
            }
            'H' => {
                while let [x, rest @ ..] = a {
                    let x = if rel { cur.0 + *x } else { *x };
                    cur = (x, cur.1);
                    pts.push(cur);
                    a = rest;
                }
                prev_c = None;
            }
            'V' => {
                while let [y, rest @ ..] = a {
                    let y = if rel { cur.1 + *y } else { *y };
                    cur = (cur.0, y);
                    pts.push(cur);
                    a = rest;
                }
                prev_c = None;
            }
            'C' => {
                while let [x1, y1, x2, y2, x3, y3, rest @ ..] = a {
                    let p1 = abs_pt(rel, cur, *x1, *y1);
                    let p2 = abs_pt(rel, cur, *x2, *y2);
                    let p3 = abs_pt(rel, cur, *x3, *y3);
                    sample_cubic(&mut pts, cur, p1, p2, p3);
                    prev_c = Some(p2);
                    cur = p3;
                    a = rest;
                }
            }
            'S' => {
                while let [x2, y2, x3, y3, rest @ ..] = a {
                    let p1 = match prev_c {
                        Some(p) => (2.0 * cur.0 - p.0, 2.0 * cur.1 - p.1),
                        None => cur,
                    };
                    let p2 = abs_pt(rel, cur, *x2, *y2);
                    let p3 = abs_pt(rel, cur, *x3, *y3);
                    sample_cubic(&mut pts, cur, p1, p2, p3);
                    prev_c = Some(p2);
                    cur = p3;
                    a = rest;
                }
            }
            'Q' => {
                while let [x1, y1, x2, y2, rest @ ..] = a {
                    let p1 = abs_pt(rel, cur, *x1, *y1);
                    let p2 = abs_pt(rel, cur, *x2, *y2);
                    sample_quad(&mut pts, cur, p1, p2);
                    prev_c = Some(p1);
                    cur = p2;
                    a = rest;
                }
            }
            'T' => {
                while let [x, y, rest @ ..] = a {
                    let p1 = match prev_c {
                        Some(p) => (2.0 * cur.0 - p.0, 2.0 * cur.1 - p.1),
                        None => cur,
                    };
                    let p2 = abs_pt(rel, cur, *x, *y);
                    sample_quad(&mut pts, cur, p1, p2);
                    prev_c = Some(p1);
                    cur = p2;
                    a = rest;
                }
            }
            'A' => {
                while let [rx, ry, rot, large, sweep, x, y, rest @ ..] = a {
                    let end = abs_pt(rel, cur, *x, *y);
                    sample_arc(&mut pts, cur, rx.abs(), ry.abs(), *rot, *large != 0.0, *sweep != 0.0, end);
                    cur = end;
                    a = rest;
                }
                prev_c = None;
            }
            'Z' => {
                pts.push(start);
                flush(&mut pts, &mut out, true);
                cur = start;
                prev_c = None;
            }
            _ => {}
        }
    }
    flush(&mut pts, &mut out, false);
    out
}

fn abs_pt(rel: bool, cur: (f32, f32), x: f32, y: f32) -> (f32, f32) {
    if rel { (cur.0 + x, cur.1 + y) } else { (x, y) }
}

fn sample_cubic(pts: &mut Vec<(f32, f32)>, p0: (f32, f32), p1: (f32, f32), p2: (f32, f32), p3: (f32, f32)) {
    for i in 1..=8 {
        let t = i as f32 / 8.0;
        let u = 1.0 - t;
        let x = u * u * u * p0.0 + 3.0 * u * u * t * p1.0 + 3.0 * u * t * t * p2.0 + t * t * t * p3.0;
        let y = u * u * u * p0.1 + 3.0 * u * u * t * p1.1 + 3.0 * u * t * t * p2.1 + t * t * t * p3.1;
        pts.push((x, y));
    }
}

fn sample_quad(pts: &mut Vec<(f32, f32)>, p0: (f32, f32), p1: (f32, f32), p2: (f32, f32)) {
    for i in 1..=6 {
        let t = i as f32 / 6.0;
        let u = 1.0 - t;
        pts.push((u * u * p0.0 + 2.0 * u * t * p1.0 + t * t * p2.0, u * u * p0.1 + 2.0 * u * t * p1.1 + t * t * p2.1));
    }
}

fn sample_arc(pts: &mut Vec<(f32, f32)>, start: (f32, f32), rx: f32, ry: f32, rot_deg: f32, large: bool, sweep: bool, end: (f32, f32)) {
    if rx < 1e-6 || ry < 1e-6 {
        pts.push(end);
        return;
    }
    let phi = rot_deg.to_radians();
    let (sin_p, cos_p) = phi.sin_cos();
    let dx = (start.0 - end.0) / 2.0;
    let dy = (start.1 - end.1) / 2.0;
    let x1 = cos_p * dx + sin_p * dy;
    let y1 = -sin_p * dx + cos_p * dy;
    let mut rx = rx;
    let mut ry = ry;
    let lambda = (x1 * x1) / (rx * rx) + (y1 * y1) / (ry * ry);
    if lambda > 1.0 {
        let s = lambda.sqrt();
        rx *= s;
        ry *= s;
    }
    let rx2 = rx * rx;
    let ry2 = ry * ry;
    let n = rx2 * y1 * y1 + ry2 * x1 * x1;
    let d = rx2 * ry2 - n;
    let mut sq = if d > 0.0 { (d / n.max(1e-12)).sqrt() } else { 0.0 };
    if large == sweep {
        sq = -sq;
    }
    let cx1 = sq * rx * y1 / ry;
    let cy1 = sq * -ry * x1 / rx;
    let cx = cos_p * cx1 - sin_p * cy1 + (start.0 + end.0) / 2.0;
    let cy = sin_p * cx1 + cos_p * cy1 + (start.1 + end.1) / 2.0;
    let ux = (x1 - cx1) / rx;
    let uy = (y1 - cy1) / ry;
    let vx = (-x1 - cx1) / rx;
    let vy = (-y1 - cy1) / ry;
    let ang = |ux: f32, uy: f32, vx: f32, vy: f32| {
        let n = ((ux * ux + uy * uy) * (vx * vx + vy * vy)).sqrt().max(1e-12);
        let a = ((ux * vx + uy * vy) / n).clamp(-1.0, 1.0).acos();
        if ux * vy - uy * vx < 0.0 { -a } else { a }
    };
    let a1 = ang(1.0, 0.0, ux, uy);
    let mut da = ang(ux, uy, vx, vy);
    if !sweep && da > 0.0 {
        da -= 2.0 * std::f32::consts::PI;
    }
    if sweep && da < 0.0 {
        da += 2.0 * std::f32::consts::PI;
    }
    let n = 12;
    for i in 1..=n {
        let t = a1 + da * i as f32 / n as f32;
        let x = rx * t.cos();
        let y = ry * t.sin();
        pts.push((cos_p * x - sin_p * y + cx, sin_p * x + cos_p * y + cy));
    }
}

fn parse_path(d: &str) -> Vec<(char, Vec<f32>)> {
    let mut out = Vec::new();
    let b = d.as_bytes();
    let mut i = 0;
    let mut cmd = 'M';
    while i < b.len() {
        let c = b[i] as char;
        if c.is_ascii_alphabetic() && c != 'e' && c != 'E' {
            cmd = c;
            i += 1;
        }
        let nums = take_nums(d, &mut i);
        if c.is_ascii_alphabetic() && c != 'e' && c != 'E' || !nums.is_empty() {
            if cmd == 'Z' || cmd == 'z' {
                out.push((cmd, vec![]));
            } else if !nums.is_empty() || cmd == 'Z' || cmd == 'z' {
                out.push((cmd, nums));
            }
        }
        while i < b.len() && b[i].is_ascii_whitespace() {
            i += 1;
        }
    }
    out
}

fn take_nums(s: &str, i: &mut usize) -> Vec<f32> {
    let b = s.as_bytes();
    let start = *i;
    while *i < b.len() {
        let c = b[*i] as char;
        if c.is_ascii_alphabetic() && c != 'e' && c != 'E' {
            break;
        }
        *i += 1;
    }
    tokenize_nums(&s[start..*i])
}
