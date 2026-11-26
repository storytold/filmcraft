//! Vector icons drawn in code on a 16×16 design grid (crisp at any DPI, recolourable, no
//! third-party or Adobe artwork). Shapes follow NLE conventions (ripple brackets, razor blade…).

use egui::epaint::{PathShape, PathStroke};
use egui::{Color32, Painter, Pos2, Rect, Stroke, pos2, vec2};
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Icon {
    Selection,
    TrackSelectFwd,
    TrackSelectBack,
    Ripple,
    Rolling,
    RateStretch,
    Remix,
    Razor,
    Slip,
    Slide,
    Pen,
    Rectangle,
    Ellipse,
    Hand,
    Zoom,
    Type,
    Play,
    Pause,
    StepBack,
    StepFwd,
    GoToIn,
    GoToOut,
    /// Go to Previous / Next Edit Point: a triangle towards a cut (an I-beam: two clip edges meeting).
    PrevEdit,
    NextEdit,
    MarkIn,
    MarkOut,
    Marker,
    Insert,
    Overwrite,
    Lift,
    Extract,
    Camera,
    Loop,
    Wrench,
    Plus,
    Eye,
    EyeOff,
    Speaker,
    Mute,
    Lock,
    Unlock,
    SyncLock,
    Mic,
    Folder,
    Film,
    Sequence,
    Audio,
    Image,
    Search,
    ListView,
    IconView,
    Freeform,
    NewItem,
    Trash,
    Home,
    Workspaces,
    Hamburger,
    ChevronDown,
    ChevronRight,
    Magnet,
    Link,
    /// Open a web address: an arrow leaving an open frame.
    ExternalLink,
    Keyframe,
    Stopwatch,
    Fx,
    Reset,
    Close,
    Fullscreen,
    Export,
    /// An arrow down into a tray (Import).
    Import,
    Gear,
    Info,
    Captions,
    Adjust,
    Nest,
    Undo,
    Redo,
    Bell,
    /// Community chat (the ArtCraft Discord): a speech bubble with three dots. Our own drawing,
    /// not the Discord logo.
    Chat,
    /// Website: a globe.
    Globe,
    /// Source code (GitHub repository): angle brackets and a slash.
    Code,
    Sparkle,
    Grid,
    Square,
    /// Toggle Proxies: a small frame inside a large one.
    Proxy,
    /// Offline media: a broken link.
    Offline,
    /// Mask tracking: backward continuously / one frame, forward one frame / continuously.
    TrackMaskBack,
    TrackMaskBackFrame,
    TrackMaskFwdFrame,
    TrackMaskFwd,
    /// Project panel footer: Sort Icons (lines of decreasing length).
    SortIcons,
    /// Project panel footer: Automate to Sequence (three clips in a row).
    Automate,
    /// Media Browser: a favourite (five-pointed star).
    Star,
    /// Media Browser: back / forward / up one level.
    ChevronLeft,
    ArrowUp,
    /// Media Browser: a local drive and a network location.
    Drive,
    Network,
    /// Media Browser: recent directories (a clock face).
    Clock,
    /// Appearance Mode: Auto (a display on a stand), Light (a sun), Dark (a crescent moon).
    Monitor,
    Sun,
    Moon,
    /// Colour parameters: pick a colour from the Program monitor (a pipette).
    Eyedropper,
}

pub struct Pen16<'a> {
    painter: &'a Painter,
    rect: Rect,
    color: Color32,
    width: f32,
}

impl Pen16<'_> {
    fn p(&self, x: f32, y: f32) -> Pos2 {
        let s = self.rect.width().min(self.rect.height()) / 16.0;
        let o = self.rect.center() - vec2(8.0 * s, 8.0 * s);
        pos2(o.x + x * s, o.y + y * s)
    }
    fn s(&self) -> f32 {
        self.rect.width().min(self.rect.height()) / 16.0
    }
    /// Stroke width in points covering a whole number of device pixels, at least one (a half pixel
    /// rounds down: 1.25 pt at scale 1 is 1 px at 1x and 2 px at 2x), so lines never straddle pixels.
    fn stroke_w(&self) -> f32 {
        let ppp = self.painter.pixels_per_point();
        (self.width * self.s() * ppp - 0.5).ceil().max(1.0) / ppp
    }
    fn stroke(&self) -> PathStroke {
        PathStroke::new(self.stroke_w(), self.color)
    }
    /// Snap one coordinate (points) to the device pixel grid. `mid` puts it where the middle of a
    /// stroke must sit to cover whole pixels (a pixel centre for an odd pixel width, an edge for an
    /// even one); otherwise on a pixel edge (where a line ends or a fill begins). Offsets are rounded
    /// from the icon's centre, the same way on both sides, so equal steps on the 16 grid stay equal
    /// on screen (the three lines of the menu mark get the same two gaps).
    fn snap(&self, v: f32, centre: f32, mid: bool) -> f32 {
        let ppp = self.painter.pixels_per_point();
        let half = if mid && (self.stroke_w() * ppp).round() as i32 % 2 == 1 { 0.5 } else { 0.0 };
        let base = (centre * ppp).round() + half;
        (base + ((v - centre) * ppp).round()) / ppp
    }
    fn snap_pos(&self, q: Pos2, mid_x: bool, mid_y: bool) -> Pos2 {
        let c = self.rect.center();
        pos2(self.snap(q.x, c.x, mid_x), self.snap(q.y, c.y, mid_y))
    }
    /// Grid points to screen. Points on a horizontal or vertical segment (one that runs less than a
    /// device pixel sideways, so it draws straight anyway) snap to device pixels so the segment is
    /// drawn crisp; points between slanted segments keep their exact place.
    fn crisp(&self, pts: &[(f32, f32)], closed: bool) -> Vec<Pos2> {
        let n = pts.len();
        let ppp = self.painter.pixels_per_point();
        let q: Vec<Pos2> = pts.iter().map(|(x, y)| self.p(*x, *y)).collect();
        let seg = |i: usize, j: usize| {
            let d = (q[i] - q[j]) * ppp;
            (d.x.abs() < 1.0, d.y.abs() < 1.0) // (vertical, horizontal)
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
                let q = q[i];
                if !vert && !horiz {
                    return q;
                }
                // across a straight segment: the stroke's middle; along it: a pixel edge, where it ends
                self.snap_pos(q, vert, horiz)
            })
            .collect()
    }
    /// Like `crisp`, but for fill boundaries: an axis-aligned edge lands on a pixel edge, and a
    /// contour never collapses below one device pixel in either axis.
    fn crisp_fill(&self, pts: &[(f32, f32)]) -> Vec<Pos2> {
        snap_fill(pts, self.rect, self.painter.pixels_per_point())
    }
    fn line(&self, pts: &[(f32, f32)]) {
        let v = self.crisp(pts, false);
        self.painter.add(PathShape::line(v, self.stroke()));
    }
    fn closed(&self, pts: &[(f32, f32)]) {
        let v = self.crisp(pts, true);
        self.painter.add(PathShape::closed_line(v, self.stroke()));
    }
    fn fill(&self, pts: &[(f32, f32)]) {
        let v: Vec<Pos2> = pts.iter().map(|(x, y)| self.p(*x, *y)).collect();
        self.painter.add(PathShape::convex_polygon(v, self.color, Stroke::NONE));
    }
    fn circle(&self, x: f32, y: f32, r: f32) {
        self.painter.circle_stroke(self.p(x, y), r * self.s(), Stroke::new(self.stroke_w(), self.color));
    }
    fn dot(&self, x: f32, y: f32, r: f32) {
        self.painter.circle_filled(self.p(x, y), r * self.s(), self.color);
    }
    fn rect(&self, x0: f32, y0: f32, x1: f32, y1: f32) {
        let r = Rect::from_min_max(self.snap_pos(self.p(x0, y0), true, true), self.snap_pos(self.p(x1, y1), true, true));
        self.painter.rect_stroke(r, 1.0 * self.s(), Stroke::new(self.stroke_w(), self.color), egui::StrokeKind::Middle);
    }
    fn rect_fill(&self, x0: f32, y0: f32, x1: f32, y1: f32) {
        let r = Rect::from_min_max(self.snap_pos(self.p(x0, y0), false, false), self.snap_pos(self.p(x1, y1), false, false));
        self.painter.rect_filled(r, 0.8 * self.s(), self.color);
    }
    fn arc(&self, cx: f32, cy: f32, r: f32, a0: f32, a1: f32) {
        let n = 20;
        let pts: Vec<(f32, f32)> = (0..=n)
            .map(|i| {
                let a = (a0 + (a1 - a0) * i as f32 / n as f32).to_radians();
                (cx + r * a.cos(), cy + r * a.sin())
            })
            .collect();
        self.line(&pts);
    }
    /// Fill one converted design glyph (outer contour, then holes) as a feathered mesh.
    fn glyph(&self, alpha: f32, contours: &[&[(f32, f32)]], tris: &[u16]) {
        let mut pts: Vec<Pos2> = Vec::new();
        let mut ranges: Vec<(usize, usize)> = Vec::new();
        for c in contours {
            let start = pts.len();
            pts.extend(self.crisp_fill(c));
            ranges.push((start, pts.len()));
        }
        keep_walls(&mut pts, &ranges, self.painter.pixels_per_point());
        keep_holes_apart(&mut pts, &ranges, self.painter.pixels_per_point());
        keep_walls(&mut pts, &ranges, self.painter.pixels_per_point());
        if pts.len() < 3 {
            return;
        }
        debug_assert!(tris.len().is_multiple_of(3) && tris.iter().all(|&i| (i as usize) < pts.len()), "bad glyph triangle indices");
        let color = self.color.gamma_multiply(alpha);
        let mut mesh = egui::Mesh::default();
        for p in &pts {
            mesh.colored_vertex(*p, color);
        }
        for t in tris.chunks(3) {
            if t.len() == 3 && t.iter().all(|&i| (i as usize) < pts.len()) {
                mesh.add_triangle(t[0] as u32, t[1] as u32, t[2] as u32);
            }
        }
        // Feather the outside of each contour by half a device pixel.
        let w = 0.5 / self.painter.pixels_per_point();
        for &(a, b) in &ranges {
            if b - a < 3 {
                continue;
            }
            let dir = self.feather_dir(&pts, &ranges, a, b, w);
            for i in a..b {
                let j = if i + 1 == b { a } else { i + 1 };
                let (p0, p1) = (pts[i], pts[j]);
                let d = p1 - p0;
                let len = d.length();
                if len < 1e-6 {
                    continue;
                }
                let n = vec2(d.y, -d.x) / len * (w * dir);
                let o0 = mesh.vertices.len() as u32;
                mesh.colored_vertex(p0 + n, Color32::TRANSPARENT);
                let o1 = mesh.vertices.len() as u32;
                mesh.colored_vertex(p1 + n, Color32::TRANSPARENT);
                let (i0, i1) = (i as u32, j as u32);
                mesh.add_triangle(i0, i1, o1);
                mesh.add_triangle(i0, o1, o0);
            }
        }
        self.painter.add(egui::Shape::mesh(mesh));
    }
    fn feather_dir(&self, pts: &[Pos2], ranges: &[(usize, usize)], a: usize, b: usize, w: f32) -> f32 {
        for i in a..b {
            let j = if i + 1 == b { a } else { i + 1 };
            let (p0, p1) = (pts[i], pts[j]);
            let d = p1 - p0;
            let len = d.length();
            if len < 1e-6 {
                continue;
            }
            let n = vec2(d.y, -d.x) / len;
            let mid = (p0 + p1.to_vec2()) / 2.0;
            let step = w.max(0.05);
            return if self.inside(pts, ranges, mid + n * step) { -1.0 } else { 1.0 };
        }
        1.0
    }
    fn inside(&self, pts: &[Pos2], ranges: &[(usize, usize)], q: Pos2) -> bool {
        let mut hit = false;
        for &(a, b) in ranges {
            if b - a < 3 {
                continue;
            }
            let mut j = b - 1;
            for i in a..b {
                let (pi, pj) = (pts[i], pts[j]);
                if (pi.y > q.y) != (pj.y > q.y) && q.x < (pj.x - pi.x) * (q.y - pi.y) / (pj.y - pi.y) + pi.x {
                    hit = !hit;
                }
                j = i;
            }
        }
        hit
    }
}

/// Snap a fill contour to device-pixel edges; keep every contour at least one device pixel wide
/// and high, so a thin slot or bar cannot disappear at 1×.
fn snap_fill(pts: &[(f32, f32)], rect: Rect, ppp: f32) -> Vec<Pos2> {
    let s = rect.width().min(rect.height()) / 16.0;
    let origin = rect.center() - vec2(8.0 * s, 8.0 * s);
    let screen: Vec<Pos2> = pts.iter().map(|&(x, y)| pos2(origin.x + x * s, origin.y + y * s)).collect();
    let n = screen.len();
    if n < 3 {
        return screen;
    }
    let seg = |i: usize, j: usize| {
        let d = (screen[i] - screen[j]) * ppp;
        (d.x.abs() < 1.0, d.y.abs() < 1.0)
    };
    let c = rect.center();
    let snap = |v: f32, centre: f32| {
        let base = (centre * ppp).round();
        (base + ((v - centre) * ppp).round()) / ppp
    };
    let mut out: Vec<Pos2> = (0..n)
        .map(|i| {
            let prev = seg((i + n - 1) % n, i);
            let next = seg(i, (i + 1) % n);
            if !prev.0 && !prev.1 && !next.0 && !next.1 {
                return screen[i];
            }
            pos2(snap(screen[i].x, c.x), snap(screen[i].y, c.y))
        })
        .collect();

    let (mut x0, mut x1, mut y0, mut y1) = (f32::MAX, f32::MIN, f32::MAX, f32::MIN);
    for p in &screen {
        x0 = x0.min(p.x);
        x1 = x1.max(p.x);
        y0 = y0.min(p.y);
        y1 = y1.max(p.y);
    }
    if (y1 - y0) * ppp < 1.0 - 1e-4 {
        let cy = (y0 + y1) / 2.0;
        let lo = ((cy * ppp - 0.5).round()) / ppp;
        let hi = lo + 1.0 / ppp;
        for (i, p) in out.iter_mut().enumerate() {
            p.y = if screen[i].y <= cy { lo } else { hi };
        }
    }
    if (x1 - x0) * ppp < 1.0 - 1e-4 {
        let cx = (x0 + x1) / 2.0;
        let lo = ((cx * ppp - 0.5).round()) / ppp;
        let hi = lo + 1.0 / ppp;
        for (i, p) in out.iter_mut().enumerate() {
            p.x = if screen[i].x <= cx { lo } else { hi };
        }
    }
    out
}

/// Snap the holes of a glyph so each one keeps at least one device pixel of wall from its outer
/// contour; snapping contours independently can put an outer and a hole edge on the same row.
fn bounds(pts: &[Pos2]) -> (f32, f32, f32, f32) {
    pts.iter().fold((f32::MAX, f32::MIN, f32::MAX, f32::MIN), |(x0, x1, y0, y1), p| (x0.min(p.x), x1.max(p.x), y0.min(p.y), y1.max(p.y)))
}

fn keep_walls(pts: &mut [Pos2], ranges: &[(usize, usize)], ppp: f32) {
    if ranges.len() < 2 {
        return;
    }
    let min = 1.0 / ppp;
    let (ox0, ox1, oy0, oy1) = bounds(&pts[ranges[0].0..ranges[0].1]);
    for &(a, b) in &ranges[1..] {
        if b - a < 3 {
            continue;
        }
        let (mut hx0, hx1, mut hy0, hy1) = bounds(&pts[a..b]);
        if hx0 - ox0 < min {
            let target = ox0 + min;
            let target = if hx1 - target >= min - 1e-4 { target } else { (ox0 + min * 0.5).min(hx1 - min * 0.5) };
            if target > hx0 + 1e-4 {
                for p in &mut pts[a..b] {
                    if (p.x - hx0).abs() < 0.01 {
                        p.x = target;
                    }
                }
                hx0 = target;
            }
        }
        if ox1 - hx1 < min {
            let target = ox1 - min;
            let target = if target - hx0 >= min - 1e-4 { target } else { (ox1 - min * 0.5).max(hx0 + min * 0.5) };
            if target < hx1 - 1e-4 {
                for p in &mut pts[a..b] {
                    if (p.x - hx1).abs() < 0.01 {
                        p.x = target;
                    }
                }
            }
        }
        if hy0 - oy0 < min {
            let target = oy0 + min;
            let target = if hy1 - target >= min - 1e-4 { target } else { (oy0 + min * 0.5).min(hy1 - min * 0.5) };
            if target > hy0 + 1e-4 {
                for p in &mut pts[a..b] {
                    if (p.y - hy0).abs() < 0.01 {
                        p.y = target;
                    }
                }
                hy0 = target;
            }
        }
        if oy1 - hy1 < min {
            let target = oy1 - min;
            let target = if target - hy0 >= min - 1e-4 { target } else { (oy1 - min * 0.5).max(hy0 + min * 0.5) };
            if target < hy1 - 1e-4 {
                for p in &mut pts[a..b] {
                    if (p.y - hy1).abs() < 0.01 {
                        p.y = target;
                    }
                }
            }
        }
    }
}

fn edge_span(pts: &[Pos2], value: f32, vertical: bool) -> Option<(f32, f32)> {
    let mut lo = f32::MAX;
    let mut hi = f32::MIN;
    let mut n = 0;
    for p in pts {
        let v = if vertical { p.y } else { p.x };
        if (v - value).abs() < 0.01 {
            let other = if vertical { p.x } else { p.y };
            lo = lo.min(other);
            hi = hi.max(other);
            n += 1;
        }
    }
    (n >= 2).then_some((lo, hi))
}

/// Keep sibling holes at least one device pixel apart where their spans overlap.
fn keep_holes_apart(pts: &mut [Pos2], ranges: &[(usize, usize)], ppp: f32) {
    let min = 1.0 / ppp;
    for i in 1..ranges.len() {
        for j in i + 1..ranges.len() {
            let (a, b) = ranges[i];
            let (c, d) = ranges[j];
            if b - a < 3 || d - c < 3 {
                continue;
            }
            let (ax0, ax1, ay0, ay1) = bounds(&pts[a..b]);
            let (bx0, bx1, by0, by1) = bounds(&pts[c..d]);
            if ax0 < bx1 && bx0 < ax1 {
                let (up, lo, up1, lo0) = if ay1 <= by1 { ((a, b), (c, d), ay1, by0) } else { ((c, d), (a, b), by1, ay0) };
                let facing =
                    edge_span(&pts[up.0..up.1], up1, true).zip(edge_span(&pts[lo.0..lo.1], lo0, true)).is_some_and(|((u0, u1), (l0, l1))| u0 < l1 && l0 < u1);
                if facing && lo0 - up1 < min - 1e-4 {
                    let target = lo0 - min;
                    let (_, _, up0, _) = bounds(&pts[up.0..up.1]);
                    if target - up0 >= min - 1e-4 {
                        for p in &mut pts[up.0..up.1] {
                            if (p.y - up1).abs() < 0.01 {
                                p.y = target;
                            }
                        }
                    } else {
                        let target = up1 + min;
                        let (_, _, _, lo1) = bounds(&pts[lo.0..lo.1]);
                        if lo1 - target >= min - 1e-4 {
                            for p in &mut pts[lo.0..lo.1] {
                                if (p.y - lo0).abs() < 0.01 {
                                    p.y = target;
                                }
                            }
                        } else {
                            let target = up1 + min * 0.5;
                            if lo1 - target >= min * 0.5 - 1e-4 {
                                for p in &mut pts[lo.0..lo.1] {
                                    if (p.y - lo0).abs() < 0.01 {
                                        p.y = target;
                                    }
                                }
                            }
                        }
                    }
                }
            }
            let (ax0, ax1, ay0, ay1) = bounds(&pts[a..b]);
            let (bx0, bx1, by0, by1) = bounds(&pts[c..d]);
            if ay0 < by1 && by0 < ay1 {
                let (left, right, left1, right0) = if ax1 <= bx1 { ((a, b), (c, d), ax1, bx0) } else { ((c, d), (a, b), bx1, ax0) };
                let facing = edge_span(&pts[left.0..left.1], left1, false)
                    .zip(edge_span(&pts[right.0..right.1], right0, false))
                    .is_some_and(|((l0, l1), (r0, r1))| l0 < r1 && r0 < l1);
                if facing && right0 - left1 < min - 1e-4 {
                    let target = right0 - min;
                    let (left0, _, _, _) = bounds(&pts[left.0..left.1]);
                    if target - left0 >= min - 1e-4 {
                        for p in &mut pts[left.0..left.1] {
                            if (p.x - left1).abs() < 0.01 {
                                p.x = target;
                            }
                        }
                    } else {
                        let target = left1 + min;
                        let (_, right1, _, _) = bounds(&pts[right.0..right.1]);
                        if right1 - target >= min - 1e-4 {
                            for p in &mut pts[right.0..right.1] {
                                if (p.x - right0).abs() < 0.01 {
                                    p.x = target;
                                }
                            }
                        } else {
                            let target = left1 + min * 0.5;
                            if right1 - target >= min * 0.5 - 1e-4 {
                                for p in &mut pts[right.0..right.1] {
                                    if (p.x - right0).abs() < 0.01 {
                                        p.x = target;
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

/// Paint `icon` centred in `rect`.
// Geometry rows are packed to the column limit; rustfmt would split every point onto its own line.
#[rustfmt::skip]
// Coordinates can land near PI/TAU/E; they are data, not constants.
#[allow(clippy::approx_constant)]
pub fn paint(painter: &Painter, rect: Rect, icon: Icon, color: Color32) {
    let pen = Pen16 { painter, rect, color, width: 1.25 };
    use Icon::*;
    match icon {
        Selection => pen.glyph(1.0, &[
            &[
                (4.643, 2.177), (4.56, 2.132), (4.465, 2.141), (4.392, 2.201), (4.364, 2.291), (4.364, 13.676), (4.391, 13.767), (4.465, 13.828),
                (4.559, 13.837), (4.643, 13.792), (8.134, 10.289), (12.387, 10.289), (12.478, 10.262), (12.539, 10.188), (12.548, 10.094), (12.503, 10.01),
            ],
        ], &[
            1, 0, 15, 1, 15, 14, 1, 14, 13, 1, 13, 12, 1, 12, 11, 1, 11, 10, 1, 10, 9, 1, 9, 8, 1, 8, 7, 1, 7, 6, 1, 6, 5, 1, 5, 4, 1, 4, 3, 1, 3, 2,
        ]),
        TrackSelectFwd => {
            pen.glyph(1.0, &[
                &[
                    (0.909, 13.818), (2.545, 13.818), (2.545, 12.727), (1.818, 12.727), (1.818, 12.0), (0.727, 12.0), (0.727, 13.636), (0.741, 13.706),
                    (0.781, 13.765), (0.84, 13.804),
                ],
            ], &[
                1, 0, 9, 1, 9, 8, 1, 8, 7, 1, 7, 6, 1, 6, 5, 5, 4, 3, 3, 2, 1, 3, 1, 5,
            ]);
            pen.glyph(1.0, &[
                &[
                    (0.909, 2.545), (2.545, 2.545), (2.545, 3.636), (1.818, 3.636), (1.818, 4.364), (0.727, 4.364), (0.727, 2.727), (0.741, 2.658),
                    (0.781, 2.599), (0.84, 2.559),
                ],
            ], &[
                8, 9, 0, 8, 0, 1, 8, 1, 2, 8, 2, 3, 8, 3, 4, 8, 4, 5, 8, 5, 6, 8, 6, 7,
            ]);
            pen.glyph(1.0, &[
                &[
                    (11.818, 2.545), (11.888, 2.559), (11.947, 2.599), (11.986, 2.658), (12.0, 2.727), (12.0, 4.364), (10.909, 4.364), (10.909, 3.636),
                    (10.182, 3.636), (10.182, 2.545),
                ],
            ], &[
                8, 9, 0, 8, 0, 1, 8, 1, 2, 8, 2, 3, 8, 3, 4, 4, 5, 6, 4, 6, 7, 4, 7, 8,
            ]);
            pen.glyph(1.0, &[
                &[
                    (9.091, 7.636), (12.727, 7.636), (12.727, 5.184), (12.759, 5.082), (12.842, 5.015), (12.948, 5.006), (13.041, 5.059), (16.0, 8.182),
                    (13.041, 11.305), (12.948, 11.358), (12.842, 11.349), (12.759, 11.282), (12.727, 11.18), (12.727, 8.727), (9.091, 8.727), (8.952, 8.7),
                    (8.834, 8.621), (8.755, 8.503), (8.727, 8.364), (8.727, 8.0), (8.755, 7.861), (8.834, 7.743), (8.952, 7.664),
                ],
            ], &[
                21, 22, 0, 21, 0, 1, 1, 2, 3, 1, 3, 4, 1, 4, 5, 1, 5, 6, 1, 6, 7, 1, 7, 8, 1, 8, 9, 1, 9, 10, 1, 10, 11, 11, 12, 13, 13, 14, 15, 13, 15, 16,
                13, 16, 17, 13, 17, 18, 13, 18, 19, 13, 19, 20, 13, 20, 21, 13, 21, 1, 13, 1, 11,
            ]);
            pen.glyph(1.0, &[
                &[
                    (3.636, 2.545), (4.727, 2.545), (4.727, 3.636), (3.636, 3.636),
                ],
            ], &[
                2, 3, 0, 2, 0, 1,
            ]);
            pen.glyph(1.0, &[
                &[
                    (5.818, 2.545), (6.909, 2.545), (6.909, 3.636), (5.818, 3.636),
                ],
            ], &[
                2, 3, 0, 2, 0, 1,
            ]);
            pen.glyph(1.0, &[
                &[
                    (8.0, 2.545), (9.091, 2.545), (9.091, 3.636), (8.0, 3.636),
                ],
            ], &[
                2, 3, 0, 2, 0, 1,
            ]);
            pen.glyph(1.0, &[
                &[
                    (0.727, 9.818), (1.818, 9.818), (1.818, 10.909), (0.727, 10.909),
                ],
            ], &[
                2, 3, 0, 2, 0, 1,
            ]);
            pen.glyph(1.0, &[
                &[
                    (0.727, 7.636), (1.818, 7.636), (1.818, 8.727), (0.727, 8.727),
                ],
            ], &[
                2, 3, 0, 2, 0, 1,
            ]);
            pen.glyph(1.0, &[
                &[
                    (0.727, 5.455), (1.818, 5.455), (1.818, 6.545), (0.727, 6.545),
                ],
            ], &[
                2, 3, 0, 2, 0, 1,
            ]);
            pen.glyph(1.0, &[
                &[
                    (8.0, 12.727), (9.091, 12.727), (9.091, 13.818), (8.0, 13.818),
                ],
            ], &[
                2, 3, 0, 2, 0, 1,
            ]);
            pen.glyph(1.0, &[
                &[
                    (5.818, 12.727), (6.909, 12.727), (6.909, 13.818), (5.818, 13.818),
                ],
            ], &[
                2, 3, 0, 2, 0, 1,
            ]);
            pen.glyph(1.0, &[
                &[
                    (3.636, 12.727), (4.727, 12.727), (4.727, 13.818), (3.636, 13.818),
                ],
            ], &[
                2, 3, 0, 2, 0, 1,
            ]);
            pen.glyph(1.0, &[
                &[
                    (10.182, 13.818), (10.182, 12.727), (10.909, 12.727), (10.909, 12.0), (12.0, 12.0), (12.0, 13.636), (11.986, 13.706), (11.947, 13.765),
                    (11.888, 13.804), (11.818, 13.818),
                ],
            ], &[
                8, 9, 0, 8, 0, 1, 8, 1, 2, 8, 2, 3, 8, 3, 4, 8, 4, 5, 8, 5, 6, 8, 6, 7,
            ]);
        }
        TrackSelectBack => {
            pen.glyph(1.0, &[
                &[
                    (15.091, 2.182), (13.455, 2.182), (13.455, 3.273), (14.182, 3.273), (14.182, 4.0), (15.273, 4.0), (15.273, 2.364), (15.259, 2.294),
                    (15.219, 2.235), (15.16, 2.196),
                ],
            ], &[
                1, 0, 9, 1, 9, 8, 1, 8, 7, 1, 7, 6, 1, 6, 5, 5, 4, 3, 3, 2, 1, 3, 1, 5,
            ]);
            pen.glyph(1.0, &[
                &[
                    (15.091, 13.455), (13.455, 13.455), (13.455, 12.364), (14.182, 12.364), (14.182, 11.636), (15.273, 11.636), (15.273, 13.273),
                    (15.259, 13.342), (15.219, 13.401), (15.16, 13.441),
                ],
            ], &[
                8, 9, 0, 8, 0, 1, 8, 1, 2, 8, 2, 3, 8, 3, 4, 8, 4, 5, 8, 5, 6, 8, 6, 7,
            ]);
            pen.glyph(1.0, &[
                &[
                    (4.182, 13.455), (4.112, 13.441), (4.053, 13.401), (4.014, 13.342), (4.0, 13.273), (4.0, 11.636), (5.091, 11.636), (5.091, 12.364),
                    (5.818, 12.364), (5.818, 13.455),
                ],
            ], &[
                8, 9, 0, 8, 0, 1, 8, 1, 2, 8, 2, 3, 8, 3, 4, 4, 5, 6, 4, 6, 7, 4, 7, 8,
            ]);
            pen.glyph(1.0, &[
                &[
                    (6.909, 8.364), (3.273, 8.364), (3.273, 10.816), (3.241, 10.918), (3.158, 10.985), (3.052, 10.994), (2.959, 10.941), (0.0, 7.818),
                    (2.959, 4.695), (3.052, 4.642), (3.158, 4.651), (3.241, 4.718), (3.273, 4.82), (3.273, 7.273), (6.909, 7.273), (7.048, 7.3),
                    (7.166, 7.379), (7.245, 7.497), (7.273, 7.636), (7.273, 8.0), (7.245, 8.139), (7.166, 8.257), (7.048, 8.336),
                ],
            ], &[
                21, 22, 0, 21, 0, 1, 1, 2, 3, 1, 3, 4, 1, 4, 5, 1, 5, 6, 1, 6, 7, 1, 7, 8, 1, 8, 9, 1, 9, 10, 1, 10, 11, 11, 12, 13, 13, 14, 15, 13, 15, 16,
                13, 16, 17, 13, 17, 18, 13, 18, 19, 13, 19, 20, 13, 20, 21, 13, 21, 1, 13, 1, 11,
            ]);
            pen.glyph(1.0, &[
                &[
                    (11.273, 12.364), (12.364, 12.364), (12.364, 13.455), (11.273, 13.455),
                ],
            ], &[
                2, 3, 0, 2, 0, 1,
            ]);
            pen.glyph(1.0, &[
                &[
                    (9.091, 12.364), (10.182, 12.364), (10.182, 13.455), (9.091, 13.455),
                ],
            ], &[
                2, 3, 0, 2, 0, 1,
            ]);
            pen.glyph(1.0, &[
                &[
                    (6.909, 12.364), (8.0, 12.364), (8.0, 13.455), (6.909, 13.455),
                ],
            ], &[
                2, 3, 0, 2, 0, 1,
            ]);
            pen.glyph(1.0, &[
                &[
                    (14.182, 5.091), (15.273, 5.091), (15.273, 6.182), (14.182, 6.182),
                ],
            ], &[
                2, 3, 0, 2, 0, 1,
            ]);
            pen.glyph(1.0, &[
                &[
                    (14.182, 7.273), (15.273, 7.273), (15.273, 8.364), (14.182, 8.364),
                ],
            ], &[
                2, 3, 0, 2, 0, 1,
            ]);
            pen.glyph(1.0, &[
                &[
                    (14.182, 9.455), (15.273, 9.455), (15.273, 10.545), (14.182, 10.545),
                ],
            ], &[
                2, 3, 0, 2, 0, 1,
            ]);
            pen.glyph(1.0, &[
                &[
                    (6.909, 2.182), (8.0, 2.182), (8.0, 3.273), (6.909, 3.273),
                ],
            ], &[
                2, 3, 0, 2, 0, 1,
            ]);
            pen.glyph(1.0, &[
                &[
                    (9.091, 2.182), (10.182, 2.182), (10.182, 3.273), (9.091, 3.273),
                ],
            ], &[
                2, 3, 0, 2, 0, 1,
            ]);
            pen.glyph(1.0, &[
                &[
                    (11.273, 2.182), (12.364, 2.182), (12.364, 3.273), (11.273, 3.273),
                ],
            ], &[
                2, 3, 0, 2, 0, 1,
            ]);
            pen.glyph(1.0, &[
                &[
                    (5.818, 2.182), (5.818, 3.273), (5.091, 3.273), (5.091, 4.0), (4.0, 4.0), (4.0, 2.364), (4.014, 2.294), (4.053, 2.235), (4.112, 2.196),
                    (4.182, 2.182),
                ],
            ], &[
                8, 9, 0, 8, 0, 1, 8, 1, 2, 8, 2, 3, 8, 3, 4, 8, 4, 5, 8, 5, 6, 8, 6, 7,
            ]);
        }
        Ripple => {
            // a ripple moves the edge; later material follows it
            pen.rect_fill(1.5, 3.0, 3.0, 13.0);
            pen.rect_fill(3.0, 7.3, 9.5, 8.7);
            pen.fill(&[(12.0, 8.0), (9.5, 5.7), (9.5, 10.3)]);
            pen.rect_fill(13.0, 3.0, 14.5, 13.0);
        }
        Rolling => {
            // a roll moves the shared edge; both clips trade frames
            pen.rect_fill(7.25, 2.5, 8.75, 13.5);
            for y in [5.2, 10.8] {
                pen.fill(&[(1.0, y), (4.6, y - 1.5), (4.6, y + 1.5)]);
                pen.rect_fill(4.6, y - 0.4, 7.25, y + 0.4);
                pen.fill(&[(15.0, y), (11.4, y - 1.5), (11.4, y + 1.5)]);
                pen.rect_fill(8.75, y - 0.4, 11.4, y + 0.4);
            }
        }
        RateStretch => {
            // one new speed for the whole clip
            pen.rect(1.5, 3.0, 14.5, 13.0);
            pen.fill(&[(3.6, 8.0), (5.8, 6.3), (5.8, 9.7)]);
            pen.rect_fill(5.6, 7.35, 10.4, 8.65);
            pen.fill(&[(12.4, 8.0), (10.2, 6.3), (10.2, 9.7)]);
        }
        Remix => {
            pen.glyph(1.0, &[
                &[
                    (15.2, 7.491), (13.382, 5.855), (13.289, 5.819), (13.191, 5.837), (13.118, 5.905), (13.091, 6.0), (13.091, 7.273), (11.818, 7.273),
                    (11.749, 7.287), (11.69, 7.326), (11.65, 7.385), (11.636, 7.455), (11.636, 7.818), (11.65, 7.888), (11.69, 7.947), (11.749, 7.986),
                    (11.818, 8.0), (13.091, 8.0), (13.091, 9.273), (13.118, 9.368), (13.191, 9.435), (13.289, 9.454), (13.382, 9.418), (15.2, 7.782),
                    (15.254, 7.718), (15.273, 7.636), (15.254, 7.555),
                ],
            ], &[
                1, 0, 26, 1, 26, 25, 1, 25, 24, 1, 24, 23, 1, 23, 22, 1, 22, 21, 1, 21, 20, 1, 20, 19, 1, 19, 18, 1, 18, 17, 17, 16, 15, 17, 15, 14, 17, 14,
                13, 17, 13, 12, 17, 12, 11, 17, 11, 10, 17, 10, 9, 17, 9, 8, 17, 8, 7, 17, 7, 6, 6, 5, 4, 6, 4, 3, 6, 3, 2, 6, 2, 1, 6, 1, 17,
            ]);
            pen.glyph(1.0, &[
                &[
                    (0.8, 7.491), (2.618, 5.855), (2.711, 5.819), (2.809, 5.837), (2.882, 5.905), (2.909, 6.0), (2.909, 7.273), (4.182, 7.273), (4.251, 7.287),
                    (4.31, 7.326), (4.35, 7.385), (4.364, 7.455), (4.364, 7.818), (4.35, 7.888), (4.31, 7.947), (4.251, 7.986), (4.182, 8.0), (2.909, 8.0),
                    (2.909, 9.273), (2.882, 9.368), (2.809, 9.435), (2.711, 9.454), (2.618, 9.418), (0.8, 7.782), (0.746, 7.718), (0.727, 7.636),
                    (0.746, 7.555),
                ],
            ], &[
                25, 26, 0, 25, 0, 1, 25, 1, 2, 25, 2, 3, 25, 3, 4, 25, 4, 5, 25, 5, 6, 25, 6, 7, 25, 7, 8, 25, 8, 9, 25, 9, 10, 25, 10, 11, 25, 11, 12, 25, 12,
                13, 25, 13, 14, 25, 14, 15, 25, 15, 16, 25, 16, 17, 25, 17, 18, 25, 18, 19, 25, 19, 20, 25, 20, 21, 25, 21, 22, 25, 22, 23, 25, 23, 24,
            ]);
            pen.glyph(1.0, &[
                &[
                    (10.909, 10.545), (10.909, 2.546), (10.871, 2.384), (10.765, 2.256), (10.614, 2.188), (10.448, 2.195), (5.357, 3.813), (5.25, 3.862),
                    (5.165, 3.943), (5.11, 4.047), (5.091, 4.163), (5.091, 10.564), (5.062, 10.711), (4.975, 10.833), (4.846, 10.909), (4.697, 10.925),
                    (4.538, 10.915), (4.379, 10.914), (4.22, 10.925), (4.062, 10.946), (3.906, 10.977), (3.752, 11.019), (3.602, 11.071), (3.455, 11.133),
                    (3.013, 11.384), (2.654, 11.693), (2.388, 12.042), (2.228, 12.413), (2.183, 12.785), (2.266, 13.14), (2.473, 13.439), (2.778, 13.654),
                    (3.159, 13.782), (3.595, 13.817), (4.065, 13.756), (4.545, 13.594), (4.908, 13.396), (5.218, 13.158), (5.468, 12.888), (5.654, 12.597),
                    (5.77, 12.296), (5.809, 11.994), (5.815, 12.0), (5.818, 5.967), (10.182, 4.512), (10.182, 9.109), (10.153, 9.255), (10.066, 9.377),
                    (9.938, 9.453), (9.79, 9.47), (9.631, 9.46), (9.472, 9.46), (9.313, 9.471), (9.156, 9.493), (9.0, 9.525), (8.846, 9.567), (8.696, 9.619),
                    (8.55, 9.682), (8.108, 9.933), (7.748, 10.243), (7.481, 10.593), (7.319, 10.964), (7.274, 11.335), (7.356, 11.689), (7.561, 11.988),
                    (7.865, 12.202), (8.246, 12.329), (8.682, 12.363), (9.151, 12.3), (9.632, 12.136), (9.995, 11.938), (10.305, 11.699), (10.556, 11.429),
                    (10.742, 11.139), (10.859, 10.838), (10.9, 10.536),
                ],
            ], &[
                1, 0, 75, 1, 75, 74, 1, 74, 73, 1, 73, 72, 1, 72, 71, 1, 71, 70, 70, 69, 68, 70, 68, 67, 70, 67, 66, 70, 66, 65, 70, 65, 64, 70, 64, 63, 70,
                63, 62, 70, 62, 61, 70, 61, 60, 70, 60, 59, 70, 59, 58, 70, 58, 57, 70, 57, 56, 70, 56, 55, 70, 55, 54, 70, 54, 53, 70, 53, 52, 70, 52, 51, 70,
                51, 50, 70, 50, 49, 70, 49, 48, 70, 48, 47, 70, 47, 46, 70, 46, 45, 43, 42, 41, 43, 41, 40, 43, 40, 39, 43, 39, 38, 43, 38, 37, 43, 37, 36, 36,
                35, 34, 36, 34, 33, 36, 33, 32, 36, 32, 31, 36, 31, 30, 36, 30, 29, 36, 29, 28, 36, 28, 27, 36, 27, 26, 36, 26, 25, 36, 25, 24, 36, 24, 23, 36,
                23, 22, 36, 22, 21, 36, 21, 20, 36, 20, 19, 36, 19, 18, 36, 18, 17, 36, 17, 16, 36, 16, 15, 36, 15, 14, 36, 14, 13, 36, 13, 12, 36, 12, 11, 11,
                10, 9, 11, 9, 8, 11, 8, 7, 11, 7, 6, 6, 5, 4, 6, 4, 3, 6, 3, 2, 6, 2, 1, 1, 70, 45, 1, 45, 44, 1, 44, 43, 43, 36, 11, 43, 11, 6, 43, 6, 1,
            ]);
        }
        Razor => pen.glyph(1.0, &[
            &[
                (14.269, 8.895), (14.024, 8.653), (13.366, 7.997), (12.413, 7.045), (11.284, 5.915), (10.096, 4.726), (8.967, 3.595), (8.014, 2.641),
                (7.356, 1.982), (7.111, 1.736), (6.742, 1.381), (6.46, 1.167), (6.221, 1.096), (5.981, 1.167), (5.695, 1.381), (5.321, 1.736), (4.873, 2.182),
                (4.689, 2.368), (4.564, 2.506), (4.487, 2.623), (4.445, 2.742), (4.429, 2.89), (4.426, 3.091), (4.426, 3.527), (4.384, 3.527), (4.274, 3.53),
                (4.123, 3.541), (3.958, 3.567), (3.803, 3.611), (3.685, 3.681), (3.61, 3.794), (3.564, 3.948), (3.54, 4.115), (3.53, 4.267), (3.53, 4.379),
                (3.531, 4.422), (3.097, 4.422), (2.912, 4.426), (2.772, 4.445), (2.654, 4.489), (2.533, 4.567), (2.386, 4.691), (2.188, 4.869), (1.741, 5.317),
                (1.38, 5.684), (1.163, 5.964), (1.091, 6.201), (1.163, 6.441), (1.38, 6.727), (1.741, 7.107), (8.9, 14.264), (9.274, 14.619), (9.56, 14.833),
                (9.8, 14.904), (10.039, 14.833), (10.321, 14.619), (10.69, 14.264), (11.137, 13.818), (11.32, 13.621), (11.443, 13.476), (11.518, 13.36),
                (11.559, 13.252), (11.577, 13.128), (11.585, 12.965), (11.585, 12.473), (11.631, 12.472), (11.749, 12.467), (11.908, 12.454), (12.078, 12.427),
                (12.227, 12.384), (12.326, 12.319), (12.379, 12.176), (12.421, 12.03), (12.452, 11.881), (12.471, 11.73), (12.48, 11.578), (12.941, 11.578),
                (13.121, 11.57), (13.257, 11.552), (13.371, 11.512), (13.488, 11.436), (13.63, 11.313), (13.822, 11.131), (13.85, 11.097), (13.932, 11.015),
                (14.04, 10.907), (14.15, 10.799), (14.235, 10.716), (14.269, 10.683), (14.625, 10.315), (14.838, 10.034), (14.909, 9.795), (14.838, 9.555),
                (14.625, 9.269),
            ],
            &[
                (11.767, 10.867), (10.873, 10.867), (10.873, 11.761), (9.613, 11.761), (9.613, 10.356), (9.241, 9.995), (9.194, 10.005), (9.146, 10.013),
                (9.098, 10.018), (9.049, 10.02), (8.973, 10.016), (8.899, 9.998), (8.831, 9.964), (8.771, 9.917), (8.708, 9.811), (8.674, 9.692),
                (8.671, 9.568), (8.698, 9.447), (8.351, 9.104), (8.204, 9.148), (8.055, 9.183), (7.905, 9.21), (7.753, 9.227), (7.6, 9.236), (7.432, 9.232),
                (7.268, 9.193), (7.115, 9.123), (6.979, 9.023), (6.843, 8.815), (6.78, 8.568), (6.771, 8.305), (6.8, 8.047), (6.849, 7.818), (6.9, 7.641),
                (6.582, 7.309), (6.531, 7.321), (6.48, 7.329), (6.428, 7.334), (6.376, 7.337), (6.297, 7.333), (6.221, 7.314), (6.15, 7.28), (6.087, 7.232),
                (6.021, 7.119), (5.985, 6.993), (5.981, 6.862), (6.009, 6.735), (5.665, 6.39), (4.244, 6.39), (4.244, 5.133), (5.139, 5.133), (5.139, 4.239),
                (6.397, 4.239), (6.397, 5.687), (6.715, 6.004), (6.77, 5.992), (6.826, 5.982), (6.883, 5.976), (6.94, 5.973), (7.021, 5.977), (7.101, 5.997),
                (7.174, 6.033), (7.239, 6.083), (7.307, 6.202), (7.344, 6.334), (7.348, 6.472), (7.317, 6.605), (7.62, 6.899), (7.771, 6.856), (7.924, 6.821),
                (8.079, 6.795), (8.236, 6.777), (8.393, 6.768), (8.563, 6.772), (8.73, 6.81), (8.886, 6.878), (9.026, 6.975), (9.168, 7.183), (9.234, 7.434),
                (9.242, 7.704), (9.212, 7.97), (9.161, 8.206), (9.108, 8.388), (9.401, 8.691), (9.456, 8.678), (9.512, 8.669), (9.568, 8.663), (9.624, 8.661),
                (9.705, 8.664), (9.783, 8.683), (9.856, 8.718), (9.921, 8.765), (9.991, 8.881), (10.028, 9.011), (10.032, 9.146), (10.002, 9.277),
                (10.344, 9.609), (11.767, 9.609),
            ],
        ], &[
            142, 141, 140, 142, 140, 139, 142, 139, 138, 142, 138, 137, 142, 137, 136, 142, 136, 135, 142, 135, 134, 129, 128, 127, 129, 127, 126, 129, 126,
            125, 129, 125, 124, 129, 124, 123, 113, 112, 111, 113, 111, 110, 113, 110, 109, 113, 109, 108, 113, 108, 107, 100, 99, 98, 100, 98, 97, 96, 95, 94,
            191, 190, 189, 191, 189, 188, 191, 188, 187, 191, 187, 186, 191, 186, 185, 191, 185, 184, 191, 184, 183, 178, 177, 176, 178, 176, 175, 178, 175,
            174, 178, 174, 173, 178, 173, 172, 162, 161, 160, 162, 160, 159, 162, 159, 158, 162, 158, 157, 162, 157, 156, 149, 148, 147, 149, 147, 146, 145,
            144, 143, 145, 143, 43, 145, 43, 42, 145, 42, 41, 145, 41, 40, 40, 39, 38, 40, 38, 37, 40, 37, 36, 40, 36, 35, 35, 34, 33, 35, 33, 32, 35, 32, 31,
            35, 31, 30, 35, 30, 29, 35, 29, 28, 35, 28, 27, 35, 27, 26, 35, 26, 25, 35, 25, 24, 35, 24, 23, 23, 22, 21, 23, 21, 20, 23, 20, 19, 23, 19, 18, 23,
            18, 17, 23, 17, 16, 23, 16, 15, 23, 15, 14, 23, 14, 13, 23, 13, 12, 23, 12, 11, 23, 11, 10, 23, 10, 9, 23, 9, 8, 23, 8, 7, 23, 7, 6, 23, 6, 5, 23,
            5, 4, 1, 0, 93, 1, 93, 92, 1, 92, 91, 1, 91, 90, 1, 90, 89, 1, 89, 88, 1, 88, 87, 1, 87, 86, 1, 86, 85, 1, 85, 84, 1, 84, 83, 1, 83, 82, 1, 82, 81,
            1, 81, 80, 1, 80, 79, 1, 79, 78, 1, 78, 77, 1, 77, 76, 1, 76, 75, 75, 74, 73, 75, 73, 72, 75, 72, 71, 75, 71, 70, 75, 70, 69, 75, 69, 68, 75, 68,
            67, 75, 67, 66, 75, 66, 65, 75, 65, 64, 64, 63, 62, 64, 62, 61, 64, 61, 60, 64, 60, 59, 64, 59, 58, 64, 58, 57, 64, 57, 56, 64, 56, 55, 64, 55, 54,
            64, 54, 53, 64, 53, 52, 64, 52, 51, 64, 51, 50, 50, 49, 48, 50, 48, 47, 50, 47, 46, 50, 46, 45, 50, 45, 44, 50, 44, 43, 43, 143, 142, 43, 142, 134,
            130, 129, 123, 114, 113, 107, 101, 100, 97, 179, 178, 172, 163, 162, 156, 150, 149, 146, 145, 40, 35, 145, 35, 23, 2, 1, 75, 2, 75, 64, 50, 43,
            134, 131, 130, 123, 131, 123, 122, 115, 114, 107, 102, 101, 97, 180, 179, 172, 180, 172, 171, 164, 163, 156, 151, 150, 146, 146, 145, 23, 146, 23,
            4, 146, 4, 3, 146, 3, 2, 132, 131, 122, 116, 115, 107, 116, 107, 106, 103, 102, 97, 181, 180, 171, 165, 164, 156, 165, 156, 155, 152, 151, 146,
            133, 132, 122, 117, 116, 106, 104, 103, 97, 182, 181, 171, 166, 165, 155, 153, 152, 146, 134, 133, 122, 118, 117, 106, 105, 104, 97, 183, 182, 171,
            167, 166, 155, 154, 153, 146, 154, 146, 2, 50, 134, 122, 50, 122, 121, 50, 121, 120, 50, 120, 119, 50, 119, 118, 50, 118, 106, 50, 106, 105, 50,
            105, 97, 50, 97, 96, 50, 96, 94, 191, 183, 171, 191, 171, 170, 168, 167, 155, 155, 154, 2, 64, 50, 94, 169, 168, 155, 169, 155, 2, 2, 64, 94, 2,
            94, 191, 2, 191, 170, 2, 170, 169,
        ]),
        Slip => {
            pen.glyph(1.0, &[
                &[
                    (14.909, 2.182), (15.048, 2.209), (15.166, 2.288), (15.245, 2.406), (15.273, 2.545), (15.273, 13.455), (15.245, 13.594), (15.166, 13.712),
                    (15.048, 13.791), (14.909, 13.818), (13.818, 13.818), (13.679, 13.791), (13.561, 13.712), (13.482, 13.594), (13.455, 13.455),
                    (13.455, 2.545), (13.482, 2.406), (13.561, 2.288), (13.679, 2.209), (13.818, 2.182),
                ],
            ], &[
                18, 19, 0, 18, 0, 1, 18, 1, 2, 18, 2, 3, 18, 3, 4, 18, 4, 5, 18, 5, 6, 18, 6, 7, 18, 7, 8, 18, 8, 9, 18, 9, 10, 18, 10, 11, 18, 11, 12, 18, 12,
                13, 18, 13, 14, 18, 14, 15, 18, 15, 16, 18, 16, 17,
            ]);
            pen.glyph(1.0, &[
                &[
                    (2.182, 2.182), (2.321, 2.209), (2.439, 2.288), (2.518, 2.406), (2.545, 2.545), (2.545, 13.455), (2.518, 13.594), (2.439, 13.712),
                    (2.321, 13.791), (2.182, 13.818), (1.091, 13.818), (0.952, 13.791), (0.834, 13.712), (0.755, 13.594), (0.727, 13.455), (0.727, 2.545),
                    (0.755, 2.406), (0.834, 2.288), (0.952, 2.209), (1.091, 2.182),
                ],
            ], &[
                18, 19, 0, 18, 0, 1, 18, 1, 2, 18, 2, 3, 18, 3, 4, 18, 4, 5, 18, 5, 6, 18, 6, 7, 18, 7, 8, 18, 8, 9, 18, 9, 10, 18, 10, 11, 18, 11, 12, 18, 12,
                13, 18, 13, 14, 18, 14, 15, 18, 15, 16, 18, 16, 17,
            ]);
            pen.glyph(1.0, &[
                &[
                    (10.132, 4.695), (10.039, 4.642), (9.933, 4.651), (9.849, 4.718), (9.818, 4.82), (9.818, 7.273), (6.182, 7.273), (6.182, 4.82),
                    (6.151, 4.718), (6.067, 4.651), (5.961, 4.642), (5.868, 4.695), (2.909, 7.818), (5.868, 10.941), (5.961, 10.994), (6.067, 10.985),
                    (6.151, 10.918), (6.182, 10.816), (6.182, 8.364), (9.818, 8.364), (9.818, 10.816), (9.849, 10.918), (9.933, 10.985), (10.039, 10.994),
                    (10.132, 10.941), (13.091, 7.818),
                ],
            ], &[
                1, 0, 25, 1, 25, 24, 1, 24, 23, 1, 23, 22, 1, 22, 21, 1, 21, 20, 1, 20, 19, 18, 17, 16, 18, 16, 15, 18, 15, 14, 18, 14, 13, 18, 13, 12, 18, 12,
                11, 18, 11, 10, 18, 10, 9, 18, 9, 8, 8, 7, 6, 5, 4, 3, 5, 3, 2, 5, 2, 1, 5, 1, 19, 5, 19, 18, 18, 8, 6, 18, 6, 5,
            ]);
        }
        Slide => {
            pen.glyph(1.0, &[
                &[
                    (9.818, 3.636), (9.818, 6.545), (10.909, 6.545), (10.909, 2.727), (10.895, 2.658), (10.856, 2.599), (10.797, 2.559), (10.727, 2.545),
                    (5.273, 2.545), (5.203, 2.559), (5.144, 2.599), (5.105, 2.658), (5.091, 2.727), (5.091, 6.545), (6.182, 6.545), (6.182, 3.636),
                ],
            ], &[
                15, 14, 13, 15, 13, 12, 15, 12, 11, 15, 11, 10, 15, 10, 9, 15, 9, 8, 15, 8, 7, 15, 7, 6, 15, 6, 5, 15, 5, 4, 15, 4, 3, 3, 2, 1, 3, 1, 0, 3, 0,
                15,
            ]);
            pen.glyph(1.0, &[
                &[
                    (6.182, 12.0), (6.182, 9.091), (5.091, 9.091), (5.091, 12.909), (5.105, 12.979), (5.144, 13.038), (5.203, 13.077), (5.273, 13.091),
                    (10.727, 13.091), (10.797, 13.077), (10.856, 13.038), (10.895, 12.979), (10.909, 12.909), (10.909, 9.091), (9.818, 9.091), (9.818, 12.0),
                ],
            ], &[
                15, 14, 13, 15, 13, 12, 15, 12, 11, 15, 11, 10, 15, 10, 9, 15, 9, 8, 15, 8, 7, 15, 7, 6, 15, 6, 5, 15, 5, 4, 15, 4, 3, 3, 2, 1, 3, 1, 0, 3, 0,
                15,
            ]);
            pen.glyph(1.0, &[
                &[
                    (13.041, 4.695), (12.948, 4.642), (12.842, 4.651), (12.759, 4.718), (12.727, 4.82), (12.727, 7.273), (3.273, 7.273), (3.273, 4.82),
                    (3.241, 4.718), (3.158, 4.651), (3.052, 4.642), (2.959, 4.695), (0.0, 7.818), (2.959, 10.941), (3.052, 10.994), (3.158, 10.985),
                    (3.241, 10.918), (3.273, 10.816), (3.273, 8.364), (12.727, 8.364), (12.727, 10.816), (12.759, 10.918), (12.842, 10.985), (12.948, 10.994),
                    (13.041, 10.941), (16.0, 7.818),
                ],
            ], &[
                1, 0, 25, 1, 25, 24, 1, 24, 23, 1, 23, 22, 1, 22, 21, 1, 21, 20, 1, 20, 19, 18, 17, 16, 18, 16, 15, 18, 15, 14, 18, 14, 13, 18, 13, 12, 18, 12,
                11, 18, 11, 10, 18, 10, 9, 18, 9, 8, 8, 7, 6, 5, 4, 3, 5, 3, 2, 5, 2, 1, 5, 1, 19, 5, 19, 18, 18, 8, 6, 18, 6, 5,
            ]);
        }
        Pen => {
            pen.glyph(1.0, &[
                &[
                    (14.231, 4.822), (11.17, 1.762), (11.044, 1.678), (10.896, 1.649), (10.747, 1.678), (10.621, 1.762), (9.343, 3.04), (9.27, 3.144),
                    (9.235, 3.265), (9.242, 3.391), (9.289, 3.509), (12.422, 6.631), (12.548, 6.715), (12.696, 6.744), (12.845, 6.715), (12.971, 6.631),
                    (14.231, 5.371), (14.315, 5.245), (14.344, 5.096), (14.315, 4.948),
                ],
            ], &[
                1, 0, 19, 1, 19, 18, 1, 18, 17, 1, 17, 16, 1, 16, 15, 1, 15, 14, 1, 14, 13, 1, 13, 12, 1, 12, 11, 1, 11, 10, 1, 10, 9, 1, 9, 8, 1, 8, 7, 1, 7,
                6, 1, 6, 5, 1, 5, 4, 1, 4, 3, 1, 3, 2,
            ]);
            pen.glyph(1.0, &[
                &[
                    (8.532, 4.139), (4.576, 6.186), (4.459, 6.254), (4.356, 6.342), (4.272, 6.448), (4.209, 6.568), (1.445, 13.081), (1.419, 13.185),
                    (1.424, 13.292), (1.46, 13.392), (1.524, 13.478), (1.714, 13.668), (5.503, 9.881), (5.484, 9.823), (5.47, 9.764), (5.46, 9.704),
                    (5.455, 9.643), (5.472, 9.466), (5.524, 9.295), (5.608, 9.138), (5.721, 9.0), (5.859, 8.887), (6.016, 8.803), (6.186, 8.752),
                    (6.364, 8.734), (6.541, 8.752), (6.712, 8.803), (6.869, 8.887), (7.006, 9.0), (7.12, 9.138), (7.204, 9.295), (7.255, 9.466),
                    (7.273, 9.643), (7.255, 9.821), (7.204, 9.991), (7.12, 10.148), (7.006, 10.286), (6.869, 10.399), (6.712, 10.483), (6.541, 10.535),
                    (6.364, 10.552), (6.3, 10.547), (6.237, 10.536), (6.175, 10.522), (6.115, 10.502), (2.333, 14.284), (2.548, 14.5), (2.634, 14.563),
                    (2.735, 14.599), (2.841, 14.604), (2.945, 14.579), (9.458, 11.815), (9.578, 11.752), (9.684, 11.667), (9.772, 11.565), (9.84, 11.447),
                    (11.887, 7.492),
                ],
            ], &[
                1, 0, 56, 1, 56, 55, 1, 55, 54, 1, 54, 53, 1, 53, 52, 1, 52, 51, 51, 50, 49, 51, 49, 48, 51, 48, 47, 51, 47, 46, 51, 46, 45, 51, 45, 44, 51,
                44, 43, 51, 43, 42, 51, 42, 41, 51, 41, 40, 51, 40, 39, 51, 39, 38, 51, 38, 37, 51, 37, 36, 51, 36, 35, 51, 35, 34, 51, 34, 33, 51, 33, 32, 51,
                32, 31, 51, 31, 30, 51, 30, 29, 51, 29, 28, 13, 12, 11, 13, 11, 10, 13, 10, 9, 13, 9, 8, 13, 8, 7, 13, 7, 6, 13, 6, 5, 13, 5, 4, 13, 4, 3, 13,
                3, 2, 13, 2, 1, 1, 51, 28, 1, 28, 27, 1, 27, 26, 1, 26, 25, 1, 25, 24, 1, 24, 23, 1, 23, 22, 1, 22, 21, 1, 21, 20, 1, 20, 19, 1, 19, 18, 1, 18,
                17, 1, 17, 16, 1, 16, 15, 1, 15, 14, 1, 14, 13,
            ]);
        }
        Rectangle => pen.glyph(1.0, &[
            &[
                (1.455, 2.909), (1.455, 13.091), (1.482, 13.23), (1.561, 13.348), (1.679, 13.427), (1.818, 13.455), (14.182, 13.455), (14.321, 13.427),
                (14.439, 13.348), (14.518, 13.23), (14.545, 13.091), (14.545, 2.909), (14.518, 2.77), (14.439, 2.652), (14.321, 2.573), (14.182, 2.545),
                (1.818, 2.545), (1.679, 2.573), (1.561, 2.652), (1.482, 2.77),
            ],
            &[
                (13.455, 12.364), (2.545, 12.364), (2.545, 3.636), (13.455, 3.636),
            ],
        ], &[
            0, 22, 21, 23, 22, 0, 23, 0, 19, 23, 19, 18, 23, 18, 17, 23, 17, 16, 23, 16, 15, 23, 15, 14, 23, 14, 13, 23, 13, 12, 23, 12, 11, 23, 11, 10, 23,
            10, 9, 23, 9, 8, 23, 8, 7, 23, 7, 6, 6, 5, 4, 6, 4, 3, 6, 3, 2, 6, 2, 1, 1, 0, 21, 1, 21, 20, 20, 23, 6, 20, 6, 1,
        ]),
        Ellipse => pen.glyph(1.0, &[
            &[
                (8.0, 2.545), (6.82, 2.617), (5.701, 2.824), (4.658, 3.154), (3.705, 3.598), (2.857, 4.143), (2.13, 4.779), (1.539, 5.493), (1.098, 6.276),
                (0.822, 7.115), (0.727, 8.0), (0.822, 8.885), (1.098, 9.724), (1.539, 10.507), (2.13, 11.221), (2.857, 11.857), (3.705, 12.402),
                (4.658, 12.846), (5.701, 13.176), (6.82, 13.383), (8.0, 13.455), (9.18, 13.383), (10.299, 13.176), (11.342, 12.846), (12.295, 12.402),
                (13.143, 11.857), (13.87, 11.221), (14.461, 10.507), (14.902, 9.724), (15.178, 8.885), (15.273, 8.0), (15.178, 7.115), (14.902, 6.276),
                (14.461, 5.493), (13.87, 4.779), (13.143, 4.143), (12.295, 3.598), (11.342, 3.154), (10.299, 2.824), (9.18, 2.617),
            ],
            &[
                (8.0, 3.636), (9.244, 3.725), (10.404, 3.98), (11.454, 4.383), (12.369, 4.916), (13.125, 5.562), (13.695, 6.303), (14.056, 7.122),
                (14.182, 8.0), (14.056, 8.878), (13.695, 9.697), (13.125, 10.438), (12.369, 11.084), (11.454, 11.617), (10.404, 12.02), (9.244, 12.275),
                (8.0, 12.364), (6.756, 12.275), (5.596, 12.02), (4.546, 11.617), (3.631, 11.084), (2.875, 10.438), (2.305, 9.697), (1.944, 8.878),
                (1.818, 8.0), (1.944, 7.122), (2.305, 6.303), (2.875, 5.562), (3.631, 4.916), (4.546, 4.383), (5.596, 3.98), (6.756, 3.725),
            ],
        ], &[
            10, 64, 63, 10, 63, 62, 10, 62, 61, 65, 64, 10, 65, 10, 9, 65, 9, 8, 65, 8, 7, 65, 7, 6, 65, 6, 5, 5, 4, 3, 5, 3, 2, 5, 2, 1, 5, 1, 0, 5, 0, 39, 5,
            39, 38, 5, 38, 37, 37, 36, 35, 37, 35, 34, 37, 34, 33, 37, 33, 32, 37, 32, 31, 37, 31, 30, 30, 29, 28, 30, 28, 27, 30, 27, 26, 30, 26, 25, 30, 25,
            24, 30, 24, 23, 23, 22, 21, 23, 21, 20, 23, 20, 19, 23, 19, 18, 23, 18, 17, 23, 17, 16, 23, 16, 15, 15, 14, 13, 15, 13, 12, 15, 12, 11, 15, 11, 10,
            15, 10, 61, 15, 61, 60, 15, 60, 59, 15, 59, 58, 15, 58, 57, 66, 65, 5, 23, 15, 57, 23, 57, 56, 23, 56, 55, 23, 55, 54, 23, 54, 53, 23, 53, 52, 23,
            52, 51, 23, 51, 50, 67, 66, 5, 30, 23, 50, 30, 50, 49, 30, 49, 48, 30, 48, 47, 30, 47, 46, 30, 46, 45, 68, 67, 5, 37, 30, 45, 37, 45, 44, 37, 44,
            43, 37, 43, 42, 37, 42, 41, 37, 41, 40, 37, 40, 71, 69, 68, 5, 5, 37, 71, 5, 71, 70, 5, 70, 69,
        ]),
        Hand => pen.glyph(1.0, &[
            &[
                (14.252, 4.764), (14.042, 4.733), (13.833, 4.772), (13.635, 4.867), (13.455, 5.009), (13.302, 5.186), (13.184, 5.387), (11.984, 7.369),
                (11.93, 7.461), (11.858, 7.545), (11.775, 7.613), (11.686, 7.659), (11.596, 7.677), (11.511, 7.66), (11.441, 7.613), (11.39, 7.545),
                (11.359, 7.456), (11.349, 7.346), (11.358, 7.217), (11.388, 7.067), (11.964, 3.443), (11.996, 3.27), (11.994, 3.095), (11.96, 2.923),
                (11.894, 2.76), (11.798, 2.612), (11.677, 2.486), (11.534, 2.384), (11.374, 2.311), (11.204, 2.269), (11.042, 2.276), (10.883, 2.312),
                (10.734, 2.375), (10.598, 2.463), (10.48, 2.574), (10.383, 2.704), (10.31, 2.849), (10.265, 3.005), (9.716, 6.383), (9.712, 6.416),
                (9.694, 6.496), (9.655, 6.601), (9.59, 6.704), (9.49, 6.781), (9.349, 6.807), (9.215, 6.767), (9.129, 6.68), (9.082, 6.569), (9.061, 6.46),
                (9.057, 6.377), (9.059, 6.343), (9.059, 2.389), (9.052, 2.211), (9.009, 2.037), (8.932, 1.876), (8.824, 1.734), (8.689, 1.617), (8.533, 1.53),
                (8.362, 1.476), (8.185, 1.457), (8.007, 1.476), (7.837, 1.53), (7.681, 1.617), (7.546, 1.734), (7.438, 1.876), (7.36, 2.037), (7.318, 2.211),
                (7.311, 2.389), (7.311, 6.325), (7.285, 6.428), (7.217, 6.49), (7.127, 6.512), (7.032, 6.498), (6.948, 6.447), (6.895, 6.361), (6.719, 5.809),
                (6.512, 5.151), (6.301, 4.479), (6.116, 3.885), (5.984, 3.462), (5.933, 3.3), (5.879, 3.148), (5.798, 3.008), (5.693, 2.884), (5.568, 2.782),
                (5.427, 2.703), (5.273, 2.65), (5.113, 2.626), (4.952, 2.63), (4.784, 2.684), (4.629, 2.768), (4.493, 2.879), (4.379, 3.013), (4.293, 3.167),
                (4.237, 3.334), (4.213, 3.508), (4.222, 3.684), (4.263, 3.855), (5.455, 7.562), (5.489, 7.686), (5.519, 7.811), (5.544, 7.937), (5.564, 8.065),
                (5.586, 8.226), (5.575, 8.388), (5.533, 8.544), (5.46, 8.689), (5.36, 8.817), (5.236, 8.922), (5.003, 8.871), (4.549, 8.663), (3.993, 8.372),
                (3.451, 8.072), (3.04, 7.838), (2.877, 7.743), (2.469, 7.493), (2.115, 7.363), (1.816, 7.332), (1.57, 7.379), (1.378, 7.48), (1.239, 7.613),
                (1.152, 7.781), (1.126, 7.972), (1.156, 8.178), (1.235, 8.392), (1.356, 8.603), (1.513, 8.804), (4.604, 11.665), (4.658, 11.733),
                (4.707, 11.804), (4.753, 11.877), (4.795, 11.952), (4.875, 12.109), (4.958, 12.265), (5.043, 12.419), (5.131, 12.571), (5.221, 12.722),
                (5.314, 12.872), (5.409, 13.02), (5.507, 13.166), (5.607, 13.311), (5.709, 13.455), (6.037, 13.784), (6.412, 14.056), (6.839, 14.269),
                (7.322, 14.422), (7.866, 14.514), (8.476, 14.545), (9.252, 14.491), (9.949, 14.333), (10.556, 14.078), (11.063, 13.733), (11.46, 13.304),
                (11.738, 12.8), (11.882, 12.327), (12.019, 11.832), (12.147, 11.346), (12.262, 10.9), (12.36, 10.525), (12.437, 10.252), (12.63, 9.84),
                (13.047, 9.026), (13.571, 8.022), (14.089, 7.041), (14.484, 6.296), (14.641, 6.0), (14.731, 5.744), (14.763, 5.492), (14.734, 5.256),
                (14.641, 5.048), (14.481, 4.88),
            ],
        ], &[
            1, 0, 170, 1, 170, 169, 1, 169, 168, 1, 168, 167, 1, 167, 166, 1, 166, 165, 1, 165, 164, 1, 164, 163, 1, 163, 162, 1, 162, 161, 1, 161, 160, 1,
            160, 159, 157, 156, 155, 157, 155, 154, 157, 154, 153, 157, 153, 152, 157, 152, 151, 157, 151, 150, 157, 150, 149, 157, 149, 148, 157, 148, 147,
            157, 147, 146, 157, 146, 145, 157, 145, 144, 157, 144, 143, 157, 143, 142, 157, 142, 141, 157, 141, 140, 157, 140, 139, 157, 139, 138, 157, 138,
            137, 157, 137, 136, 157, 136, 135, 157, 135, 134, 157, 134, 133, 157, 133, 132, 157, 132, 131, 157, 131, 130, 157, 130, 129, 157, 129, 128, 157,
            128, 127, 157, 127, 126, 157, 126, 125, 157, 125, 124, 157, 124, 123, 157, 123, 122, 122, 121, 120, 122, 120, 119, 122, 119, 118, 122, 118, 117,
            122, 117, 116, 122, 116, 115, 122, 115, 114, 122, 114, 113, 122, 113, 112, 122, 112, 111, 122, 111, 110, 122, 110, 109, 98, 97, 96, 98, 96, 95, 98,
            95, 94, 98, 94, 93, 98, 93, 92, 98, 92, 91, 98, 91, 90, 98, 90, 89, 98, 89, 88, 98, 88, 87, 98, 87, 86, 98, 86, 85, 98, 85, 84, 98, 84, 83, 98, 83,
            82, 98, 82, 81, 98, 81, 80, 98, 80, 79, 98, 79, 78, 98, 78, 77, 98, 77, 76, 98, 76, 75, 98, 75, 74, 98, 74, 73, 98, 73, 72, 98, 72, 71, 98, 71, 70,
            68, 67, 66, 68, 66, 65, 68, 65, 64, 68, 64, 63, 68, 63, 62, 68, 62, 61, 68, 61, 60, 68, 60, 59, 68, 59, 58, 68, 58, 57, 68, 57, 56, 68, 56, 55, 68,
            55, 54, 68, 54, 53, 68, 53, 52, 68, 52, 51, 68, 51, 50, 68, 50, 49, 68, 49, 48, 68, 48, 47, 68, 47, 46, 68, 46, 45, 68, 45, 44, 39, 38, 37, 39, 37,
            36, 39, 36, 35, 39, 35, 34, 39, 34, 33, 39, 33, 32, 39, 32, 31, 39, 31, 30, 39, 30, 29, 39, 29, 28, 39, 28, 27, 39, 27, 26, 39, 26, 25, 39, 25, 24,
            39, 24, 23, 39, 23, 22, 39, 22, 21, 39, 21, 20, 39, 20, 19, 39, 19, 18, 39, 18, 17, 39, 17, 16, 39, 16, 15, 39, 15, 14, 8, 7, 6, 6, 5, 4, 6, 4, 3,
            6, 3, 2, 6, 2, 1, 6, 1, 159, 6, 159, 158, 6, 158, 157, 157, 122, 109, 157, 109, 108, 157, 108, 107, 157, 107, 106, 157, 106, 105, 157, 105, 104,
            157, 104, 103, 157, 103, 102, 157, 102, 101, 157, 101, 100, 157, 100, 99, 157, 99, 98, 157, 98, 70, 157, 70, 69, 157, 69, 68, 157, 68, 44, 157, 44,
            43, 157, 43, 42, 157, 42, 41, 157, 41, 40, 157, 40, 39, 157, 39, 14, 157, 14, 13, 157, 13, 12, 157, 12, 11, 157, 11, 10, 157, 10, 9, 157, 9, 8,
            157, 8, 6,
        ]),
        Zoom => {
            pen.glyph(1.0, &[
                &[
                    (9.455, 6.49025), (9.455, 6.96345), (9.427, 7.14415), (9.348, 7.29755), (9.23, 7.40025), (9.091, 7.43665), (7.43665, 7.43665),
                    (7.43665, 9.091), (7.40025, 9.23), (7.29755, 9.348), (7.14415, 9.427), (6.96345, 9.455), (6.49025, 9.455), (6.30955, 9.427),
                    (6.15615, 9.348), (6.05345, 9.23), (6.01835, 9.091), (6.01835, 7.43665), (4.364, 7.43665), (4.224, 7.40025), (4.107, 7.29755),
                    (4.028, 7.14415), (4.0, 6.96345), (4.0, 6.49025), (4.028, 6.30955), (4.107, 6.15615), (4.224, 6.05345), (4.364, 6.01835),
                    (6.01835, 6.01835), (6.01835, 4.364), (6.05345, 4.224), (6.15615, 4.107), (6.30955, 4.028), (6.49025, 4.0), (6.96345, 4.0),
                    (7.14415, 4.028), (7.29755, 4.107), (7.40025, 4.224), (7.43665, 4.364), (7.43665, 6.01835), (9.091, 6.01835), (9.23, 6.05345),
                    (9.348, 6.15615), (9.427, 6.30955),
                ],
            ], &[
                42, 43, 0, 42, 0, 1, 42, 1, 2, 42, 2, 3, 42, 3, 4, 42, 4, 5, 42, 5, 6, 6, 7, 8, 6, 8, 9, 6, 9, 10, 6, 10, 11, 6, 11, 12, 6, 12, 13, 6, 13, 14,
                6, 14, 15, 6, 15, 16, 6, 16, 17, 17, 18, 19, 17, 19, 20, 17, 20, 21, 17, 21, 22, 17, 22, 23, 17, 23, 24, 17, 24, 25, 17, 25, 26, 17, 26, 27,
                17, 27, 28, 28, 29, 30, 28, 30, 31, 28, 31, 32, 28, 32, 33, 28, 33, 34, 28, 34, 35, 28, 35, 36, 28, 36, 37, 28, 37, 38, 28, 38, 39, 39, 40, 41,
                39, 41, 42, 39, 42, 6, 39, 6, 17, 39, 17, 28,
            ]);
            pen.glyph(1.0, &[
                &[
                    (14.226, 14.226), (14.096, 14.337), (13.95, 14.426), (13.792, 14.492), (13.625, 14.532), (13.454, 14.545), (13.284, 14.532),
                    (13.117, 14.492), (12.959, 14.426), (12.813, 14.337), (12.683, 14.226), (9.6, 11.144), (8.443, 11.71), (7.184, 11.979), (5.898, 11.935),
                    (4.66, 11.581), (3.545, 10.938), (2.618, 10.044), (1.935, 8.953), (1.537, 7.729), (1.447, 6.445), (1.67, 5.177), (2.193, 4.0),
                    (2.986, 2.986), (4.0, 2.193), (5.177, 1.67), (6.445, 1.447), (7.729, 1.537), (8.953, 1.935), (10.044, 2.618), (10.938, 3.545),
                    (11.581, 4.66), (11.935, 5.898), (11.979, 7.184), (11.71, 8.443), (11.144, 9.6), (14.226, 12.681), (14.337, 12.812), (14.427, 12.958),
                    (14.493, 13.116), (14.533, 13.283), (14.546, 13.454), (14.533, 13.625), (14.493, 13.791), (14.427, 13.949), (14.337, 14.096),
                ],
                &[
                    (6.727, 10.909), (7.543, 10.829), (8.328, 10.591), (9.051, 10.204), (9.684, 9.684), (10.204, 9.051), (10.591, 8.328), (10.829, 7.543),
                    (10.909, 6.727), (10.829, 5.911), (10.591, 5.127), (10.204, 4.404), (9.684, 3.77), (9.051, 3.25), (8.328, 2.864), (7.543, 2.626),
                    (6.727, 2.545), (5.911, 2.626), (5.127, 2.864), (4.404, 3.25), (3.77, 3.77), (3.25, 4.404), (2.864, 5.127), (2.626, 5.911), (2.545, 6.727),
                    (2.554, 7.001), (2.581, 7.273), (2.626, 7.543), (2.688, 7.81), (2.767, 8.071), (2.864, 8.328), (2.977, 8.577), (3.106, 8.818),
                    (3.25, 9.051), (3.41, 9.273), (3.583, 9.485), (3.77, 9.684), (3.97, 9.871), (4.182, 10.045), (4.404, 10.204), (4.636, 10.349),
                    (4.878, 10.478), (5.127, 10.591), (5.383, 10.687), (5.645, 10.767), (5.911, 10.829), (6.181, 10.873), (6.454, 10.9),
                ],
            ], &[
                20, 70, 71, 20, 71, 72, 20, 72, 73, 20, 73, 74, 20, 74, 75, 20, 75, 76, 20, 76, 77, 20, 77, 78, 20, 78, 79, 69, 70, 20, 69, 20, 21, 69, 21, 22,
                69, 22, 23, 23, 24, 25, 23, 25, 26, 23, 26, 27, 23, 27, 28, 28, 29, 30, 28, 30, 31, 28, 31, 32, 28, 32, 33, 33, 34, 35, 35, 36, 37, 35, 37, 38,
                35, 38, 39, 35, 39, 40, 35, 40, 41, 35, 41, 42, 35, 42, 43, 35, 43, 44, 35, 44, 45, 35, 45, 0, 35, 0, 1, 35, 1, 2, 35, 2, 3, 35, 3, 4, 35, 4,
                5, 35, 5, 6, 35, 6, 7, 35, 7, 8, 35, 8, 9, 35, 9, 10, 35, 10, 11, 35, 11, 12, 35, 12, 13, 35, 13, 14, 14, 15, 16, 14, 16, 17, 14, 17, 18, 14,
                18, 19, 19, 20, 79, 19, 79, 80, 19, 80, 81, 19, 81, 82, 19, 82, 83, 68, 69, 23, 14, 19, 83, 14, 83, 84, 14, 84, 85, 14, 85, 86, 14, 86, 87, 14,
                87, 88, 14, 88, 89, 14, 89, 90, 14, 90, 91, 14, 91, 92, 14, 92, 93, 14, 93, 46, 14, 46, 47, 14, 47, 48, 14, 48, 49, 67, 68, 23, 35, 14, 49, 35,
                49, 50, 35, 50, 51, 35, 51, 52, 35, 52, 53, 35, 53, 54, 66, 67, 23, 33, 35, 54, 33, 54, 55, 33, 55, 56, 33, 56, 57, 65, 66, 23, 28, 33, 57, 28,
                57, 58, 28, 58, 59, 28, 59, 60, 28, 60, 61, 28, 61, 62, 28, 62, 63, 64, 65, 23, 23, 28, 63, 23, 63, 64,
            ]);
        }
        Type => pen.glyph(1.0, &[
            &[
                (2.545, 2.182), (2.406, 2.209), (2.288, 2.288), (2.209, 2.406), (2.182, 2.545), (2.182, 5.091), (2.209, 5.23), (2.288, 5.348), (2.406, 5.427),
                (2.545, 5.455), (3.636, 5.455), (3.776, 5.427), (3.893, 5.348), (3.972, 5.23), (4.0, 5.091), (4.0, 4.0), (6.909, 4.0), (6.909, 12.0),
                (5.818, 12.0), (5.679, 12.028), (5.561, 12.107), (5.482, 12.224), (5.455, 12.364), (5.455, 13.455), (5.482, 13.594), (5.561, 13.712),
                (5.679, 13.791), (5.818, 13.818), (9.818, 13.818), (9.957, 13.791), (10.075, 13.712), (10.154, 13.594), (10.182, 13.455), (10.182, 12.364),
                (10.154, 12.224), (10.075, 12.107), (9.957, 12.028), (9.818, 12.0), (8.727, 12.0), (8.727, 4.0), (11.636, 4.0), (11.636, 5.091),
                (11.664, 5.23), (11.743, 5.348), (11.861, 5.427), (12.0, 5.455), (13.091, 5.455), (13.23, 5.427), (13.348, 5.348), (13.427, 5.23),
                (13.455, 5.091), (13.455, 2.545), (13.427, 2.406), (13.348, 2.288), (13.23, 2.209), (13.091, 2.182),
            ],
        ], &[
            1, 0, 55, 1, 55, 54, 1, 54, 53, 1, 53, 52, 1, 52, 51, 51, 50, 49, 51, 49, 48, 51, 48, 47, 51, 47, 46, 51, 46, 45, 51, 45, 44, 51, 44, 43, 51, 43,
            42, 51, 42, 41, 51, 41, 40, 51, 40, 39, 38, 37, 36, 38, 36, 35, 38, 35, 34, 38, 34, 33, 38, 33, 32, 38, 32, 31, 38, 31, 30, 38, 30, 29, 38, 29, 28,
            38, 28, 27, 38, 27, 26, 38, 26, 25, 38, 25, 24, 38, 24, 23, 38, 23, 22, 38, 22, 21, 38, 21, 20, 38, 20, 19, 19, 18, 17, 15, 14, 13, 15, 13, 12, 15,
            12, 11, 15, 11, 10, 15, 10, 9, 15, 9, 8, 15, 8, 7, 15, 7, 6, 15, 6, 5, 15, 5, 4, 15, 4, 3, 15, 3, 2, 15, 2, 1, 15, 1, 51, 38, 19, 17, 38, 17, 16,
            16, 15, 51, 16, 51, 39, 16, 39, 38,
        ]),
        Play => pen.glyph(1.0, &[
            &[
                (4.0, 3.636), (5.72, 3.636), (5.768, 3.64), (5.815, 3.649), (5.86, 3.664), (5.902, 3.685), (12.727, 7.636), (5.903, 11.587), (5.86, 11.608),
                (5.815, 11.624), (5.768, 11.633), (5.721, 11.636), (4.0, 11.636), (3.861, 11.609), (3.743, 11.53), (3.664, 11.412), (3.636, 11.273),
                (3.636, 4.0), (3.664, 3.861), (3.743, 3.743), (3.861, 3.664),
            ],
        ], &[
            19, 20, 0, 19, 0, 1, 19, 1, 2, 19, 2, 3, 19, 3, 4, 19, 4, 5, 19, 5, 6, 19, 6, 7, 19, 7, 8, 19, 8, 9, 19, 9, 10, 19, 10, 11, 19, 11, 12, 19, 12, 13,
            19, 13, 14, 19, 14, 15, 19, 15, 16, 19, 16, 17, 19, 17, 18,
        ]),
        Pause => {
            pen.glyph(1.0, &[
                &[
                    (4.727, 3.636), (6.909, 3.636), (7.048, 3.664), (7.166, 3.743), (7.245, 3.861), (7.273, 4.0), (7.273, 11.273), (7.245, 11.412),
                    (7.166, 11.53), (7.048, 11.609), (6.909, 11.636), (4.727, 11.636), (4.588, 11.609), (4.47, 11.53), (4.391, 11.412), (4.364, 11.273),
                    (4.364, 4.0), (4.391, 3.861), (4.47, 3.743), (4.588, 3.664),
                ],
            ], &[
                18, 19, 0, 18, 0, 1, 18, 1, 2, 18, 2, 3, 18, 3, 4, 18, 4, 5, 18, 5, 6, 18, 6, 7, 18, 7, 8, 18, 8, 9, 18, 9, 10, 18, 10, 11, 18, 11, 12, 18, 12,
                13, 18, 13, 14, 18, 14, 15, 18, 15, 16, 18, 16, 17,
            ]);
            pen.glyph(1.0, &[
                &[
                    (9.091, 3.636), (11.273, 3.636), (11.412, 3.664), (11.53, 3.743), (11.609, 3.861), (11.636, 4.0), (11.636, 11.273), (11.609, 11.412),
                    (11.53, 11.53), (11.412, 11.609), (11.273, 11.636), (9.091, 11.636), (8.952, 11.609), (8.834, 11.53), (8.755, 11.412), (8.727, 11.273),
                    (8.727, 4.0), (8.755, 3.861), (8.834, 3.743), (8.952, 3.664),
                ],
            ], &[
                18, 19, 0, 18, 0, 1, 18, 1, 2, 18, 2, 3, 18, 3, 4, 18, 4, 5, 18, 5, 6, 18, 6, 7, 18, 7, 8, 18, 8, 9, 18, 9, 10, 18, 10, 11, 18, 11, 12, 18, 12,
                13, 18, 13, 14, 18, 14, 15, 18, 15, 16, 18, 16, 17,
            ]);
        }
        StepBack => {
            pen.rect_fill(3.5, 3.5, 5.0, 12.5);
            pen.fill(&[(12.5, 3.5), (6.0, 8.0), (12.5, 12.5)]);
        }
        StepFwd => {
            pen.rect_fill(11.0, 3.5, 12.5, 12.5);
            pen.fill(&[(3.5, 3.5), (10.0, 8.0), (3.5, 12.5)]);
        }
        GoToIn => {
            pen.line(&[(5.0, 3.0), (4.0, 3.0), (3.5, 3.5), (3.5, 6.0), (3.0, 7.0), (2.0, 8.0), (3.0, 9.0), (3.5, 10.0), (3.5, 12.5), (4.0, 13.0), (5.0, 13.0)]);
            pen.fill(&[(13.0, 3.5), (6.0, 8.0), (13.0, 12.5)]);
        }
        GoToOut => {
            pen.line(&[
                (11.0, 3.0),
                (12.0, 3.0),
                (12.5, 3.5),
                (12.5, 6.0),
                (13.0, 7.0),
                (14.0, 8.0),
                (13.0, 9.0),
                (12.5, 10.0),
                (12.5, 12.5),
                (12.0, 13.0),
                (11.0, 13.0),
            ]);
            pen.fill(&[(3.0, 3.5), (10.0, 8.0), (3.0, 12.5)]);
        }
        PrevEdit => {
            pen.glyph(1.0, &[
                &[
                    (4.364, 3.273), (4.364, 12.0), (4.336, 12.139), (4.257, 12.257), (4.139, 12.336), (4.0, 12.364), (3.273, 12.364), (3.134, 12.336),
                    (3.016, 12.257), (2.937, 12.139), (2.909, 12.0), (2.909, 3.273), (2.937, 3.134), (3.016, 3.016), (3.134, 2.937), (3.273, 2.909),
                    (4.0, 2.909), (4.139, 2.937), (4.257, 3.016), (4.336, 3.134),
                ],
            ], &[
                18, 19, 0, 18, 0, 1, 18, 1, 2, 18, 2, 3, 18, 3, 4, 18, 4, 5, 18, 5, 6, 18, 6, 7, 18, 7, 8, 18, 8, 9, 18, 9, 10, 18, 10, 11, 18, 11, 12, 18, 12,
                13, 18, 13, 14, 18, 14, 15, 18, 15, 16, 18, 16, 17,
            ]);
            pen.glyph(1.0, &[
                &[
                    (12.727, 6.545), (9.455, 6.545), (9.455, 4.772), (9.425, 4.673), (9.347, 4.606), (9.244, 4.592), (9.151, 4.637), (5.818, 7.636),
                    (9.151, 10.636), (9.244, 10.68), (9.347, 10.667), (9.425, 10.6), (9.455, 10.501), (9.455, 8.727), (12.727, 8.727), (12.866, 8.7),
                    (12.984, 8.621), (13.063, 8.503), (13.091, 8.364), (13.091, 6.909), (13.063, 6.77), (12.984, 6.652), (12.866, 6.573),
                ],
            ], &[
                1, 0, 22, 1, 22, 21, 1, 21, 20, 1, 20, 19, 1, 19, 18, 1, 18, 17, 1, 17, 16, 1, 16, 15, 1, 15, 14, 1, 14, 13, 13, 12, 11, 13, 11, 10, 13, 10, 9,
                13, 9, 8, 13, 8, 7, 13, 7, 6, 13, 6, 5, 13, 5, 4, 13, 4, 3, 3, 2, 1, 3, 1, 13,
            ]);
        }
        NextEdit => {
            pen.glyph(1.0, &[
                &[
                    (11.636, 3.273), (11.636, 12.0), (11.664, 12.139), (11.743, 12.257), (11.861, 12.336), (12.0, 12.364), (12.727, 12.364), (12.866, 12.336),
                    (12.984, 12.257), (13.063, 12.139), (13.091, 12.0), (13.091, 3.273), (13.063, 3.134), (12.984, 3.016), (12.866, 2.937), (12.727, 2.909),
                    (12.0, 2.909), (11.861, 2.937), (11.743, 3.016), (11.664, 3.134),
                ],
            ], &[
                1, 0, 19, 1, 19, 18, 1, 18, 17, 1, 17, 16, 1, 16, 15, 1, 15, 14, 1, 14, 13, 1, 13, 12, 1, 12, 11, 1, 11, 10, 1, 10, 9, 1, 9, 8, 1, 8, 7, 1, 7,
                6, 1, 6, 5, 1, 5, 4, 1, 4, 3, 1, 3, 2,
            ]);
            pen.glyph(1.0, &[
                &[
                    (3.273, 6.545), (6.545, 6.545), (6.545, 4.772), (6.575, 4.673), (6.653, 4.606), (6.756, 4.592), (6.849, 4.637), (10.182, 7.636),
                    (6.849, 10.636), (6.756, 10.68), (6.653, 10.667), (6.575, 10.6), (6.545, 10.501), (6.545, 8.727), (3.273, 8.727), (3.134, 8.7),
                    (3.016, 8.621), (2.937, 8.503), (2.909, 8.364), (2.909, 6.909), (2.937, 6.77), (3.016, 6.652), (3.134, 6.573),
                ],
            ], &[
                21, 22, 0, 21, 0, 1, 1, 2, 3, 1, 3, 4, 1, 4, 5, 1, 5, 6, 1, 6, 7, 1, 7, 8, 1, 8, 9, 1, 9, 10, 1, 10, 11, 11, 12, 13, 13, 14, 15, 13, 15, 16,
                13, 16, 17, 13, 17, 18, 13, 18, 19, 13, 19, 20, 13, 20, 21, 13, 21, 1, 13, 1, 11,
            ]);
        }
        MarkIn => pen.glyph(1.0, &[
            &[
                (9.091, 0.727), (8.0, 0.727), (7.861, 0.755), (7.743, 0.834), (7.664, 0.952), (7.636, 1.091), (7.636, 5.818), (5.413, 7.724), (5.319, 7.848),
                (5.286, 8.0), (5.319, 8.152), (5.413, 8.276), (7.636, 10.182), (7.636, 14.909), (7.664, 15.048), (7.743, 15.166), (7.861, 15.245),
                (8.0, 15.273), (9.091, 15.273), (9.23, 15.245), (9.348, 15.166), (9.427, 15.048), (9.455, 14.909), (9.455, 9.818), (7.273, 8.0),
                (9.455, 6.182), (9.455, 1.091), (9.427, 0.952), (9.348, 0.834), (9.23, 0.755),
            ],
        ], &[
            1, 0, 29, 1, 29, 28, 1, 28, 27, 1, 27, 26, 1, 26, 25, 24, 23, 22, 24, 22, 21, 24, 21, 20, 24, 20, 19, 24, 19, 18, 18, 17, 16, 18, 16, 15, 18, 15,
            14, 18, 14, 13, 18, 13, 12, 12, 11, 10, 12, 10, 9, 12, 9, 8, 12, 8, 7, 6, 5, 4, 6, 4, 3, 6, 3, 2, 6, 2, 1, 6, 1, 25, 6, 25, 24, 24, 18, 12, 24, 12,
            7, 24, 7, 6,
        ]),
        MarkOut => pen.glyph(1.0, &[
            &[
                (6.909, 0.727), (8.0, 0.727), (8.139, 0.755), (8.257, 0.834), (8.336, 0.952), (8.364, 1.091), (8.364, 5.818), (10.587, 7.724), (10.681, 7.848),
                (10.714, 8.0), (10.681, 8.152), (10.587, 8.276), (8.364, 10.182), (8.364, 14.909), (8.336, 15.048), (8.257, 15.166), (8.139, 15.245),
                (8.0, 15.273), (6.909, 15.273), (6.77, 15.245), (6.652, 15.166), (6.573, 15.048), (6.545, 14.909), (6.545, 9.818), (8.727, 8.0),
                (6.545, 6.182), (6.545, 1.091), (6.573, 0.952), (6.652, 0.834), (6.77, 0.755),
            ],
        ], &[
            28, 29, 0, 28, 0, 1, 28, 1, 2, 28, 2, 3, 28, 3, 4, 28, 4, 5, 28, 5, 6, 6, 7, 8, 6, 8, 9, 6, 9, 10, 6, 10, 11, 12, 13, 14, 12, 14, 15, 12, 15, 16,
            12, 16, 17, 12, 17, 18, 12, 18, 19, 12, 19, 20, 12, 20, 21, 12, 21, 22, 12, 22, 23, 12, 23, 24, 24, 25, 26, 24, 26, 27, 24, 27, 28, 24, 28, 6, 24,
            6, 11, 24, 11, 12,
        ]),
        Marker => pen.glyph(1.0, &[
            &[
                (3.273, 3.636), (3.273, 9.818), (8.182, 13.818), (12.727, 9.818), (12.727, 3.636), (12.709, 3.475), (12.655, 3.321), (12.569, 3.183),
                (12.453, 3.068), (12.316, 2.981), (12.162, 2.927), (12.0, 2.909), (4.0, 2.909), (3.838, 2.927), (3.684, 2.981), (3.547, 3.068), (3.431, 3.183),
                (3.345, 3.321), (3.291, 3.475),
            ],
        ], &[
            1, 0, 18, 1, 18, 17, 1, 17, 16, 1, 16, 15, 1, 15, 14, 1, 14, 13, 1, 13, 12, 1, 12, 11, 1, 11, 10, 1, 10, 9, 1, 9, 8, 1, 8, 7, 1, 7, 6, 1, 6, 5, 1,
            5, 4, 1, 4, 3, 1, 3, 2,
        ]),
        Insert => {
            pen.glyph(1.0, &[
                &[
                    (0.727, 6.909), (0.727, 11.273), (0.755, 11.412), (0.834, 11.53), (0.952, 11.609), (1.091, 11.636), (5.455, 11.636), (5.594, 11.609),
                    (5.712, 11.53), (5.791, 11.412), (5.818, 11.273), (5.818, 8.727), (9.455, 8.727), (9.455, 11.273), (9.482, 11.412), (9.561, 11.53),
                    (9.679, 11.609), (9.818, 11.636), (14.182, 11.636), (14.321, 11.609), (14.439, 11.53), (14.518, 11.412), (14.545, 11.273), (14.545, 6.909),
                    (14.518, 6.77), (14.439, 6.652), (14.321, 6.573), (14.182, 6.545), (10.909, 6.545), (10.909, 3.273), (10.881, 3.134), (10.803, 3.016),
                    (10.685, 2.937), (10.545, 2.909), (4.727, 2.909), (4.588, 2.937), (4.47, 3.016), (4.391, 3.134), (4.364, 3.273), (4.364, 6.545),
                    (1.091, 6.545), (0.952, 6.573), (0.834, 6.652), (0.755, 6.77),
                ],
                &[
                    (11.636, 10.519), (11.636, 7.659), (11.665, 7.562), (11.741, 7.495), (11.841, 7.479), (11.934, 7.519), (13.818, 9.08), (11.935, 10.659),
                    (11.842, 10.7), (11.741, 10.684), (11.665, 10.617),
                ],
                &[
                    (5.091, 8.0), (5.091, 3.636), (10.182, 3.636), (10.182, 8.0),
                ],
            ], &[
                28, 45, 44, 28, 27, 26, 28, 26, 25, 28, 25, 24, 28, 24, 23, 23, 22, 21, 23, 21, 20, 23, 20, 19, 23, 19, 18, 18, 17, 16, 18, 16, 15, 18, 15, 14,
                18, 14, 13, 11, 10, 9, 11, 9, 8, 11, 8, 7, 11, 7, 6, 11, 6, 5, 11, 5, 4, 11, 4, 3, 11, 3, 2, 11, 2, 1, 11, 1, 0, 11, 0, 43, 11, 43, 42, 11, 42,
                41, 11, 41, 40, 39, 38, 56, 39, 56, 55, 57, 56, 38, 57, 38, 37, 57, 37, 36, 57, 36, 35, 57, 35, 34, 57, 34, 33, 57, 33, 32, 57, 32, 31, 57, 31,
                30, 57, 30, 29, 57, 29, 28, 57, 28, 44, 46, 45, 28, 40, 39, 55, 58, 57, 44, 58, 44, 54, 47, 46, 28, 47, 28, 23, 11, 40, 55, 11, 55, 58, 48, 47,
                23, 12, 11, 58, 12, 58, 54, 12, 54, 53, 49, 48, 23, 13, 12, 53, 13, 53, 52, 50, 49, 23, 50, 23, 18, 18, 13, 52, 18, 52, 51, 18, 51, 50,
            ]);
            pen.glyph(1.0, &[
                &[
                    (7.636, 7.284), (9.208, 5.389), (9.248, 5.296), (9.232, 5.195), (9.165, 5.119), (9.068, 5.091), (6.205, 5.091), (6.108, 5.119),
                    (6.041, 5.195), (6.025, 5.296), (6.065, 5.389),
                ],
            ], &[
                1, 0, 10, 1, 10, 9, 1, 9, 8, 1, 8, 7, 1, 7, 6, 1, 6, 5, 1, 5, 4, 1, 4, 3, 1, 3, 2,
            ]);
        }
        Overwrite => {
            pen.glyph(1.0, &[
                &[
                    (4.364, 3.273), (4.364, 7.273), (1.091, 7.273), (0.952, 7.3), (0.834, 7.379), (0.755, 7.497), (0.727, 7.636), (0.727, 11.273),
                    (0.755, 11.412), (0.834, 11.53), (0.952, 11.609), (1.091, 11.636), (14.182, 11.636), (14.321, 11.609), (14.439, 11.53), (14.518, 11.412),
                    (14.545, 11.273), (14.545, 7.636), (14.518, 7.497), (14.439, 7.379), (14.321, 7.3), (14.182, 7.273), (10.909, 7.273), (10.909, 3.273),
                    (10.881, 3.134), (10.803, 3.016), (10.685, 2.937), (10.545, 2.909), (4.727, 2.909), (4.588, 2.937), (4.47, 3.016), (4.391, 3.134),
                ],
                &[
                    (5.091, 8.0), (5.091, 3.636), (10.182, 3.636), (10.182, 8.0),
                ],
                &[
                    (4.364, 8.727), (10.909, 8.727), (10.909, 9.455), (4.364, 9.455),
                ],
            ], &[
                0, 33, 32, 34, 33, 0, 34, 0, 31, 34, 31, 30, 34, 30, 29, 34, 29, 28, 34, 28, 27, 34, 27, 26, 34, 26, 25, 34, 25, 24, 34, 24, 23, 34, 23, 22,
                22, 21, 20, 22, 20, 19, 22, 19, 18, 22, 18, 17, 22, 17, 16, 22, 16, 15, 22, 15, 14, 22, 14, 13, 22, 13, 12, 12, 11, 10, 12, 10, 9, 12, 9, 8,
                12, 8, 7, 7, 6, 36, 7, 36, 39, 7, 39, 38, 37, 36, 6, 37, 6, 5, 5, 4, 3, 5, 3, 2, 5, 2, 1, 1, 0, 32, 35, 34, 22, 35, 22, 12, 12, 7, 38, 12, 38,
                37, 5, 1, 32, 35, 12, 37, 37, 5, 32, 37, 32, 35,
            ]);
            pen.glyph(1.0, &[
                &[
                    (6.064, 5.388), (7.636, 7.296), (9.209, 5.389), (9.249, 5.296), (9.234, 5.195), (9.167, 5.119), (9.069, 5.091), (6.204, 5.091),
                    (6.106, 5.119), (6.039, 5.195), (6.024, 5.295),
                ],
            ], &[
                1, 0, 10, 1, 10, 9, 1, 9, 8, 1, 8, 7, 1, 7, 6, 1, 6, 5, 1, 5, 4, 1, 4, 3, 1, 3, 2,
            ]);
        }
        Lift => {
            pen.glyph(1.0, &[
                &[
                    (0.727, 6.909), (0.727, 11.273), (0.755, 11.412), (0.834, 11.53), (0.952, 11.609), (1.091, 11.636), (5.455, 11.636), (5.594, 11.609),
                    (5.712, 11.53), (5.791, 11.412), (5.818, 11.273), (5.818, 8.727), (9.455, 8.727), (9.455, 11.273), (9.482, 11.412), (9.561, 11.53),
                    (9.679, 11.609), (9.818, 11.636), (14.182, 11.636), (14.321, 11.609), (14.439, 11.53), (14.518, 11.412), (14.545, 11.273), (14.545, 6.909),
                    (14.518, 6.77), (14.439, 6.652), (14.321, 6.573), (14.182, 6.545), (10.909, 6.545), (10.909, 3.273), (10.881, 3.134), (10.803, 3.016),
                    (10.685, 2.937), (10.545, 2.909), (4.727, 2.909), (4.588, 2.937), (4.47, 3.016), (4.391, 3.134), (4.364, 3.273), (4.364, 6.545),
                    (1.091, 6.545), (0.952, 6.573), (0.834, 6.652), (0.755, 6.77),
                ],
                &[
                    (5.091, 8.0), (5.091, 3.636), (10.182, 3.636), (10.182, 8.0),
                ],
            ], &[
                38, 45, 44, 46, 45, 38, 46, 38, 37, 46, 37, 36, 46, 36, 35, 46, 35, 34, 46, 34, 33, 46, 33, 32, 46, 32, 31, 46, 31, 30, 46, 30, 29, 46, 29, 28,
                28, 27, 26, 28, 26, 25, 28, 25, 24, 28, 24, 23, 28, 23, 22, 28, 22, 21, 28, 21, 20, 28, 20, 19, 28, 19, 18, 28, 18, 17, 28, 17, 16, 28, 16, 15,
                28, 15, 14, 28, 14, 13, 11, 10, 9, 11, 9, 8, 11, 8, 7, 11, 7, 6, 11, 6, 5, 11, 5, 4, 11, 4, 3, 11, 3, 2, 11, 2, 1, 11, 1, 0, 11, 0, 43, 11, 43,
                42, 11, 42, 41, 11, 41, 40, 39, 38, 44, 47, 46, 28, 47, 28, 13, 47, 13, 12, 47, 12, 11, 40, 39, 44, 44, 47, 11, 44, 11, 40,
            ]);
            pen.glyph(1.0, &[
                &[
                    (6.206, 6.545), (9.064, 6.545), (9.161, 6.517), (9.228, 6.44), (9.244, 6.34), (9.203, 6.247), (7.625, 4.375), (6.067, 6.247),
                    (6.026, 6.34), (6.042, 6.441), (6.109, 6.517),
                ],
            ], &[
                1, 0, 10, 1, 10, 9, 1, 9, 8, 1, 8, 7, 1, 7, 6, 1, 6, 5, 1, 5, 4, 1, 4, 3, 1, 3, 2,
            ]);
        }
        Extract => {
            pen.glyph(1.0, &[
                &[
                    (0.727, 6.909), (0.727, 11.273), (0.755, 11.412), (0.834, 11.53), (0.952, 11.609), (1.091, 11.636), (5.455, 11.636), (5.594, 11.609),
                    (5.712, 11.53), (5.791, 11.412), (5.818, 11.273), (5.818, 8.727), (9.455, 8.727), (9.455, 11.273), (9.482, 11.412), (9.561, 11.53),
                    (9.679, 11.609), (9.818, 11.636), (14.182, 11.636), (14.321, 11.609), (14.439, 11.53), (14.518, 11.412), (14.545, 11.273), (14.545, 6.909),
                    (14.518, 6.77), (14.439, 6.652), (14.321, 6.573), (14.182, 6.545), (10.909, 6.545), (10.909, 3.273), (10.881, 3.134), (10.803, 3.016),
                    (10.685, 2.937), (10.545, 2.909), (4.727, 2.909), (4.588, 2.937), (4.47, 3.016), (4.391, 3.134), (4.364, 3.273), (4.364, 6.545),
                    (1.091, 6.545), (0.952, 6.573), (0.834, 6.652), (0.755, 6.77),
                ],
                &[
                    (11.636, 9.08), (13.52, 7.519), (13.613, 7.479), (13.714, 7.495), (13.79, 7.562), (13.818, 7.659), (13.818, 10.519), (13.79, 10.617),
                    (13.713, 10.684), (13.613, 10.699), (13.52, 10.659),
                ],
                &[
                    (1.455, 10.524), (1.455, 7.658), (1.483, 7.561), (1.559, 7.494), (1.659, 7.478), (1.752, 7.518), (3.659, 9.091), (1.752, 10.664),
                    (1.659, 10.704), (1.559, 10.688), (1.483, 10.621),
                ],
                &[
                    (5.091, 8.0), (5.091, 3.636), (10.182, 3.636), (10.182, 8.0),
                ],
            ], &[
                12, 44, 54, 45, 44, 12, 45, 12, 11, 11, 10, 9, 11, 9, 8, 11, 8, 7, 11, 7, 6, 11, 6, 5, 11, 5, 4, 11, 4, 3, 11, 3, 2, 11, 2, 1, 1, 0, 56, 1, 56,
                55, 1, 55, 65, 1, 65, 64, 1, 64, 63, 1, 63, 62, 57, 56, 0, 57, 0, 43, 57, 43, 42, 57, 42, 41, 57, 41, 40, 39, 38, 67, 39, 67, 66, 68, 67, 38,
                68, 38, 37, 68, 37, 36, 68, 36, 35, 68, 35, 34, 68, 34, 33, 68, 33, 32, 68, 32, 31, 68, 31, 30, 68, 30, 29, 68, 29, 28, 28, 27, 26, 28, 26, 25,
                28, 25, 24, 28, 24, 23, 23, 22, 21, 23, 21, 20, 23, 20, 19, 23, 19, 18, 18, 17, 16, 18, 16, 15, 18, 15, 14, 18, 14, 13, 18, 13, 12, 18, 12, 54,
                18, 54, 53, 18, 53, 52, 18, 52, 51, 18, 51, 50, 18, 50, 49, 46, 45, 11, 11, 1, 62, 11, 62, 61, 11, 61, 60, 11, 60, 59, 58, 57, 40, 58, 40, 39,
                69, 68, 28, 69, 28, 23, 23, 18, 49, 23, 49, 48, 23, 48, 47, 23, 47, 46, 59, 58, 39, 59, 39, 66, 69, 23, 46, 69, 46, 11, 11, 59, 66, 11, 66, 69,
            ]);
            pen.glyph(1.0, &[
                &[
                    (6.208, 6.545), (9.065, 6.545), (9.163, 6.517), (9.23, 6.441), (9.245, 6.34), (9.204, 6.247), (7.636, 4.375), (6.068, 6.247),
                    (6.027, 6.34), (6.043, 6.441), (6.11, 6.517),
                ],
            ], &[
                1, 0, 10, 1, 10, 9, 1, 9, 8, 1, 8, 7, 1, 7, 6, 1, 6, 5, 1, 5, 4, 1, 4, 3, 1, 3, 2,
            ]);
        }
        Camera => {
            pen.glyph(1.0, &[
                &[
                    (14.182, 4.364), (11.345, 4.364), (10.509, 3.319), (10.398, 3.199), (10.271, 3.098), (10.13, 3.017), (9.978, 2.957), (9.82, 2.921),
                    (9.657, 2.909), (6.343, 2.909), (6.18, 2.921), (6.022, 2.957), (5.87, 3.017), (5.729, 3.098), (5.602, 3.199), (5.491, 3.319),
                    (4.655, 4.364), (1.818, 4.364), (1.679, 4.391), (1.561, 4.47), (1.482, 4.588), (1.455, 4.727), (1.455, 12.727), (1.482, 12.866),
                    (1.561, 12.984), (1.679, 13.063), (1.818, 13.091), (14.182, 13.091), (14.321, 13.063), (14.439, 12.984), (14.518, 12.866),
                    (14.545, 12.727), (14.545, 4.727), (14.518, 4.588), (14.439, 4.47), (14.321, 4.391),
                ],
                &[
                    (8.0, 11.636), (7.362, 11.573), (6.748, 11.387), (6.182, 11.085), (5.686, 10.678), (5.279, 10.182), (4.976, 9.616), (4.79, 9.002),
                    (4.727, 8.364), (4.79, 7.725), (4.976, 7.111), (5.279, 6.545), (5.686, 6.049), (6.182, 5.642), (6.748, 5.34), (7.362, 5.154), (8.0, 5.091),
                    (8.638, 5.154), (9.252, 5.34), (9.818, 5.642), (10.314, 6.049), (10.721, 6.545), (11.024, 7.111), (11.21, 7.725), (11.273, 8.364),
                    (11.266, 8.578), (11.245, 8.791), (11.21, 9.002), (11.161, 9.211), (11.099, 9.416), (11.024, 9.616), (10.935, 9.811), (10.834, 10.0),
                    (10.721, 10.182), (10.596, 10.356), (10.461, 10.521), (10.314, 10.678), (10.158, 10.824), (9.992, 10.96), (9.818, 11.085), (9.636, 11.198),
                    (9.447, 11.299), (9.252, 11.387), (9.052, 11.463), (8.847, 11.525), (8.638, 11.573), (8.427, 11.608), (8.214, 11.629),
                ],
            ], &[
                21, 44, 43, 21, 43, 42, 21, 42, 41, 45, 44, 21, 45, 21, 20, 45, 20, 19, 45, 19, 18, 45, 18, 17, 45, 17, 16, 45, 16, 15, 45, 15, 14, 45, 14, 13,
                45, 13, 12, 45, 12, 11, 45, 11, 10, 10, 9, 8, 10, 8, 7, 10, 7, 6, 10, 6, 5, 10, 5, 4, 10, 4, 3, 10, 3, 2, 10, 2, 1, 1, 0, 35, 1, 35, 34, 1, 34,
                33, 1, 33, 32, 1, 32, 31, 1, 31, 30, 1, 30, 29, 1, 29, 28, 1, 28, 27, 27, 26, 25, 27, 25, 24, 27, 24, 23, 27, 23, 22, 22, 21, 41, 22, 41, 40,
                22, 40, 39, 22, 39, 38, 22, 38, 37, 22, 37, 36, 22, 36, 83, 22, 83, 82, 46, 45, 10, 27, 22, 82, 27, 82, 81, 27, 81, 80, 27, 80, 79, 27, 79, 78,
                27, 78, 77, 27, 77, 76, 27, 76, 75, 27, 75, 74, 27, 74, 73, 27, 73, 72, 27, 72, 71, 27, 71, 70, 27, 70, 69, 27, 69, 68, 27, 68, 67, 27, 67, 66,
                27, 66, 65, 27, 65, 64, 27, 64, 63, 27, 63, 62, 27, 62, 61, 27, 61, 60, 27, 60, 59, 27, 59, 58, 47, 46, 10, 1, 27, 58, 1, 58, 57, 1, 57, 56, 1,
                56, 55, 1, 55, 54, 1, 54, 53, 1, 53, 52, 1, 52, 51, 48, 47, 10, 10, 1, 51, 10, 51, 50, 10, 50, 49, 10, 49, 48,
            ]);
            pen.glyph(1.0, &[
                &[
                    (10.182, 8.364), (10.155, 8.705), (10.075, 9.038), (9.944, 9.354), (9.765, 9.646), (9.543, 9.906), (9.282, 10.129), (8.991, 10.308),
                    (8.674, 10.439), (8.341, 10.519), (8.0, 10.545), (7.659, 10.519), (7.326, 10.439), (7.009, 10.308), (6.718, 10.129), (6.457, 9.906),
                    (6.235, 9.646), (6.056, 9.354), (5.925, 9.038), (5.845, 8.705), (5.818, 8.364), (5.845, 8.022), (5.925, 7.689), (6.056, 7.373),
                    (6.235, 7.081), (6.457, 6.821), (6.718, 6.599), (7.009, 6.42), (7.326, 6.289), (7.659, 6.209), (8.0, 6.182), (8.341, 6.209),
                    (8.674, 6.289), (8.991, 6.42), (9.282, 6.599), (9.543, 6.821), (9.765, 7.081), (9.944, 7.373), (10.075, 7.689), (10.155, 8.022),
                ],
            ], &[
                38, 39, 0, 38, 0, 1, 38, 1, 2, 38, 2, 3, 38, 3, 4, 38, 4, 5, 38, 5, 6, 38, 6, 7, 38, 7, 8, 38, 8, 9, 38, 9, 10, 38, 10, 11, 38, 11, 12, 38, 12,
                13, 38, 13, 14, 38, 14, 15, 38, 15, 16, 38, 16, 17, 38, 17, 18, 38, 18, 19, 38, 19, 20, 38, 20, 21, 38, 21, 22, 38, 22, 23, 38, 23, 24, 38, 24,
                25, 38, 25, 26, 38, 26, 27, 38, 27, 28, 38, 28, 29, 38, 29, 30, 38, 30, 31, 38, 31, 32, 38, 32, 33, 38, 33, 34, 38, 34, 35, 38, 35, 36, 38, 36,
                37,
            ]);
        }
        Loop => {
            pen.glyph(1.0, &[
                &[
                    (13.333, 8.137), (13.333, 10.667), (2.667, 10.667), (2.667, 5.333), (7.111, 5.333), (7.111, 3.556), (2.667, 3.556), (2.452, 3.569),
                    (2.241, 3.607), (2.036, 3.671), (1.84, 3.759), (1.657, 3.87), (1.488, 4.003), (1.336, 4.154), (1.204, 4.323), (1.093, 4.507),
                    (1.004, 4.703), (0.941, 4.908), (0.902, 5.119), (0.889, 5.333), (0.889, 10.667), (0.902, 10.881), (0.941, 11.092), (1.004, 11.297),
                    (1.093, 11.493), (1.204, 11.677), (1.336, 11.846), (1.488, 11.997), (1.657, 12.13), (1.84, 12.241), (2.036, 12.329), (2.241, 12.393),
                    (2.452, 12.431), (2.667, 12.444), (13.333, 12.444), (13.548, 12.431), (13.759, 12.393), (13.964, 12.329), (14.16, 12.241), (14.343, 12.13),
                    (14.512, 11.997), (14.664, 11.846), (14.796, 11.677), (14.907, 11.493), (14.996, 11.297), (15.059, 11.092), (15.098, 10.881),
                    (15.111, 10.667), (15.111, 7.471), (15.083, 7.362), (15.006, 7.282), (14.899, 7.249), (14.789, 7.272), (13.456, 7.939), (13.406, 7.973),
                    (13.366, 8.021), (13.342, 8.077),
                ],
            ], &[
                1, 0, 56, 1, 56, 55, 1, 55, 54, 1, 54, 53, 1, 53, 52, 1, 52, 51, 1, 51, 50, 1, 50, 49, 1, 49, 48, 1, 48, 47, 1, 47, 46, 1, 46, 45, 1, 45, 44,
                1, 44, 43, 1, 43, 42, 1, 42, 41, 1, 41, 40, 1, 40, 39, 1, 39, 38, 1, 38, 37, 1, 37, 36, 1, 36, 35, 1, 35, 34, 1, 34, 33, 1, 33, 32, 1, 32, 31,
                1, 31, 30, 1, 30, 29, 1, 29, 28, 1, 28, 27, 1, 27, 26, 1, 26, 25, 1, 25, 24, 1, 24, 23, 1, 23, 22, 1, 22, 21, 21, 20, 19, 21, 19, 18, 21, 18,
                17, 21, 17, 16, 21, 16, 15, 21, 15, 14, 21, 14, 13, 21, 13, 12, 21, 12, 11, 21, 11, 10, 21, 10, 9, 21, 9, 8, 21, 8, 7, 21, 7, 6, 6, 5, 4, 6, 4,
                3, 2, 1, 21, 21, 6, 3, 21, 3, 2,
            ]);
            pen.glyph(1.0, &[
                &[
                    (7.556, 1.778), (8.378, 1.778), (8.495, 1.806), (13.217, 4.251), (13.302, 4.332), (13.333, 4.446), (13.302, 4.559), (13.217, 4.641),
                    (8.492, 7.085), (8.381, 7.111), (7.556, 7.111), (7.385, 7.077), (7.241, 6.981), (7.145, 6.837), (7.111, 6.667), (7.111, 2.222),
                    (7.145, 2.052), (7.241, 1.908), (7.385, 1.812),
                ],
            ], &[
                17, 18, 0, 17, 0, 1, 17, 1, 2, 17, 2, 3, 17, 3, 4, 17, 4, 5, 17, 5, 6, 17, 6, 7, 17, 7, 8, 17, 8, 9, 17, 9, 10, 17, 10, 11, 17, 11, 12, 17, 12,
                13, 17, 13, 14, 17, 14, 15, 17, 15, 16,
            ]);
        }
        Wrench => pen.glyph(1.0, &[
            &[
                (14.327, 12.234), (9.27, 7.177), (8.663, 6.323), (8.356, 5.408), (8.201, 4.466), (8.053, 3.532), (7.766, 2.641), (7.194, 1.827),
                (6.448, 1.301), (5.581, 1.004), (4.684, 0.892), (3.848, 0.921), (3.162, 1.046), (2.718, 1.221), (2.696, 1.247), (2.691, 1.28), (2.703, 1.312),
                (2.729, 1.333), (5.502, 2.711), (5.536, 2.735), (5.564, 2.766), (5.582, 2.804), (5.591, 2.844), (5.72, 4.469), (5.715, 4.557), (5.686, 4.642),
                (5.637, 4.715), (5.569, 4.772), (4.198, 5.65), (4.16, 5.668), (4.118, 5.676), (4.076, 5.673), (4.036, 5.659), (1.244, 4.27), (1.216, 4.262),
                (1.187, 4.267), (1.164, 4.284), (1.15, 4.31), (1.171, 4.602), (1.294, 5.006), (1.505, 5.483), (1.792, 5.992), (2.14, 6.495), (2.539, 6.95),
                (3.286, 7.491), (4.095, 7.786), (4.916, 7.937), (5.699, 8.05), (6.391, 8.228), (6.944, 8.574), (11.889, 14.421), (11.93, 14.476),
                (11.973, 14.528), (12.018, 14.58), (12.064, 14.629), (12.224, 14.769), (12.4, 14.887), (12.589, 14.982), (12.79, 15.052), (12.997, 15.096),
                (13.209, 15.112), (13.42, 15.101), (13.629, 15.063), (13.831, 14.998), (14.023, 14.908), (14.202, 14.794), (14.365, 14.658), (14.509, 14.503),
                (14.624, 14.33), (14.717, 14.145), (14.787, 13.949), (14.833, 13.747), (14.854, 13.541), (14.85, 13.334), (14.821, 13.128), (14.767, 12.928),
                (14.689, 12.736), (14.589, 12.554), (14.468, 12.386),
            ],
        ], &[
            1, 0, 78, 1, 78, 77, 1, 77, 76, 1, 76, 75, 1, 75, 74, 1, 74, 73, 1, 73, 72, 1, 72, 71, 1, 71, 70, 1, 70, 69, 1, 69, 68, 1, 68, 67, 1, 67, 66, 1,
            66, 65, 1, 65, 64, 1, 64, 63, 1, 63, 62, 1, 62, 61, 1, 61, 60, 1, 60, 59, 1, 59, 58, 1, 58, 57, 1, 57, 56, 1, 56, 55, 1, 55, 54, 1, 54, 53, 1, 53,
            52, 1, 52, 51, 1, 51, 50, 1, 50, 49, 1, 49, 48, 1, 48, 47, 1, 47, 46, 1, 46, 45, 1, 45, 44, 1, 44, 43, 1, 43, 42, 1, 42, 41, 1, 41, 40, 1, 40, 39,
            39, 38, 37, 39, 37, 36, 39, 36, 35, 39, 35, 34, 39, 34, 33, 39, 33, 32, 39, 32, 31, 18, 17, 16, 18, 16, 15, 18, 15, 14, 18, 14, 13, 18, 13, 12, 18,
            12, 11, 18, 11, 10, 18, 10, 9, 18, 9, 8, 18, 8, 7, 18, 7, 6, 18, 6, 5, 18, 5, 4, 1, 39, 31, 1, 31, 30, 1, 30, 29, 1, 29, 28, 1, 28, 27, 1, 27, 26,
            1, 26, 25, 1, 25, 24, 1, 24, 23, 1, 23, 22, 1, 22, 21, 1, 21, 20, 19, 18, 4, 19, 4, 3, 2, 1, 20, 20, 19, 3, 20, 3, 2,
        ]),
        Plus => pen.glyph(1.0, &[
            &[
                (12.889, 7.111), (8.889, 7.111), (8.889, 3.111), (8.855, 2.941), (8.759, 2.797), (8.615, 2.7), (8.444, 2.667), (7.556, 2.667), (7.385, 2.7),
                (7.241, 2.797), (7.145, 2.941), (7.111, 3.111), (7.111, 7.111), (3.111, 7.111), (2.941, 7.145), (2.797, 7.241), (2.7, 7.385), (2.667, 7.556),
                (2.667, 8.444), (2.7, 8.615), (2.797, 8.759), (2.941, 8.855), (3.111, 8.889), (7.111, 8.889), (7.111, 12.889), (7.145, 13.059),
                (7.241, 13.203), (7.385, 13.3), (7.556, 13.333), (8.444, 13.333), (8.615, 13.3), (8.759, 13.203), (8.855, 13.059), (8.889, 12.889),
                (8.889, 8.889), (12.889, 8.889), (13.059, 8.855), (13.203, 8.759), (13.3, 8.615), (13.333, 8.444), (13.333, 7.556), (13.3, 7.385),
                (13.203, 7.241), (13.059, 7.145),
            ],
        ], &[
            1, 0, 43, 1, 43, 42, 1, 42, 41, 1, 41, 40, 1, 40, 39, 1, 39, 38, 1, 38, 37, 1, 37, 36, 1, 36, 35, 1, 35, 34, 34, 33, 32, 34, 32, 31, 34, 31, 30,
            34, 30, 29, 34, 29, 28, 34, 28, 27, 34, 27, 26, 34, 26, 25, 34, 25, 24, 34, 24, 23, 23, 22, 21, 23, 21, 20, 23, 20, 19, 23, 19, 18, 23, 18, 17, 23,
            17, 16, 23, 16, 15, 23, 15, 14, 23, 14, 13, 23, 13, 12, 12, 11, 10, 12, 10, 9, 12, 9, 8, 12, 8, 7, 12, 7, 6, 12, 6, 5, 12, 5, 4, 12, 4, 3, 12, 3,
            2, 12, 2, 1, 12, 1, 34, 12, 34, 23,
        ]),
        Eye => {
            pen.glyph(1.0, &[
                &[
                    (10.939, 3.813), (10.743, 3.717), (10.544, 3.627), (10.343, 3.544), (10.138, 3.468), (9.931, 3.398), (9.722, 3.335), (9.512, 3.279),
                    (9.299, 3.23), (9.085, 3.188), (8.869, 3.153), (8.653, 3.125), (8.436, 3.104), (8.218, 3.09), (8.0, 3.083), (6.4, 3.299), (4.928, 3.872),
                    (3.628, 4.69), (2.543, 5.64), (1.714, 6.61), (1.186, 7.487), (1.0, 8.16), (1.194, 8.844), (1.742, 9.686), (2.592, 10.592), (3.692, 11.464),
                    (4.99, 12.206), (6.434, 12.723), (7.972, 12.916), (9.522, 12.722), (10.977, 12.206), (12.286, 11.463), (13.395, 10.591), (14.252, 9.685),
                    (14.804, 8.843), (15.0, 8.16), (14.851, 7.549), (14.433, 6.808), (13.79, 6.0), (12.964, 5.189), (12.0, 4.439),
                ],
                &[
                    (8.0, 12.1), (7.2, 12.021), (6.431, 11.788), (5.722, 11.409), (5.101, 10.899), (4.591, 10.278), (4.212, 9.569), (3.979, 8.8), (3.9, 8.0),
                    (3.979, 7.2), (4.212, 6.431), (4.591, 5.722), (5.101, 5.101), (5.722, 4.591), (6.431, 4.212), (7.2, 3.979), (8.0, 3.9), (8.8, 3.979),
                    (9.569, 4.212), (10.278, 4.591), (10.899, 5.101), (11.409, 5.722), (11.788, 6.431), (12.021, 7.2), (12.1, 8.0), (12.091, 8.268),
                    (12.065, 8.535), (12.021, 8.8), (11.96, 9.061), (11.882, 9.318), (11.788, 9.569), (11.677, 9.813), (11.551, 10.05), (11.409, 10.278),
                    (11.253, 10.496), (11.083, 10.703), (10.899, 10.899), (10.703, 11.083), (10.496, 11.253), (10.278, 11.409), (10.05, 11.551),
                    (9.813, 11.677), (9.569, 11.788), (9.318, 11.882), (9.061, 11.96), (8.8, 12.021), (8.535, 12.065), (8.268, 12.091),
                ],
            ], &[
                21, 49, 48, 21, 48, 47, 21, 47, 46, 21, 46, 45, 21, 45, 44, 50, 49, 21, 50, 21, 20, 50, 20, 19, 50, 19, 18, 50, 18, 17, 50, 17, 16, 16, 15, 14,
                16, 14, 13, 16, 13, 12, 16, 12, 11, 16, 11, 10, 16, 10, 9, 16, 9, 8, 16, 8, 7, 16, 7, 6, 16, 6, 5, 16, 5, 4, 16, 4, 3, 16, 3, 2, 16, 2, 1, 16,
                1, 0, 0, 40, 39, 0, 39, 38, 0, 38, 37, 0, 37, 36, 0, 36, 35, 0, 35, 34, 0, 34, 33, 0, 33, 32, 32, 31, 30, 32, 30, 29, 32, 29, 28, 28, 27, 26,
                28, 26, 25, 28, 25, 24, 28, 24, 23, 28, 23, 22, 22, 21, 44, 51, 50, 16, 28, 22, 44, 28, 44, 43, 28, 43, 42, 28, 42, 41, 28, 41, 88, 28, 88, 87,
                28, 87, 86, 28, 86, 85, 28, 85, 84, 28, 84, 83, 28, 83, 82, 28, 82, 81, 28, 81, 80, 52, 51, 16, 32, 28, 80, 32, 80, 79, 32, 79, 78, 32, 78, 77,
                32, 77, 76, 32, 76, 75, 32, 75, 74, 32, 74, 73, 32, 73, 72, 32, 72, 71, 32, 71, 70, 32, 70, 69, 32, 69, 68, 32, 68, 67, 32, 67, 66, 32, 66, 65,
                32, 65, 64, 32, 64, 63, 53, 52, 16, 0, 32, 63, 0, 63, 62, 0, 62, 61, 0, 61, 60, 0, 60, 59, 0, 59, 58, 0, 58, 57, 54, 53, 16, 16, 0, 57, 16, 57,
                56, 16, 56, 55, 16, 55, 54,
            ]);
            pen.glyph(1.0, &[
                &[
                    (9.185, 8.037), (8.98, 8.019), (8.78, 7.965), (8.593, 7.878), (8.424, 7.76), (8.277, 7.614), (8.159, 7.444), (8.072, 7.257),
                    (8.018, 7.058), (8.0, 6.852), (8.02, 6.648), (8.075, 6.451), (8.164, 6.267), (8.284, 6.101), (8.431, 5.958), (8.6, 5.844), (8.453, 5.805),
                    (8.303, 5.776), (8.152, 5.758), (8.0, 5.75), (7.561, 5.793), (7.139, 5.922), (6.75, 6.129), (6.409, 6.409), (6.129, 6.75), (5.921, 7.139),
                    (5.793, 7.562), (5.75, 8.001), (5.793, 8.44), (5.921, 8.862), (6.129, 9.251), (6.409, 9.592), (6.75, 9.872), (7.139, 10.079),
                    (7.561, 10.207), (8.0, 10.251), (8.439, 10.207), (8.861, 10.079), (9.25, 9.871), (9.591, 9.591), (9.871, 9.25), (10.079, 8.861),
                    (10.207, 8.439), (10.25, 8.0), (10.244, 7.865), (10.229, 7.731), (10.205, 7.598), (10.172, 7.467), (10.057, 7.628), (9.916, 7.767),
                    (9.753, 7.881), (9.574, 7.965), (9.383, 8.018),
                ],
            ], &[
                48, 47, 46, 48, 46, 45, 48, 45, 44, 48, 44, 43, 48, 43, 42, 48, 42, 41, 48, 41, 40, 48, 40, 39, 48, 39, 38, 48, 38, 37, 48, 37, 36, 48, 36, 35,
                48, 35, 34, 34, 33, 32, 34, 32, 31, 34, 31, 30, 34, 30, 29, 34, 29, 28, 34, 28, 27, 34, 27, 26, 34, 26, 25, 34, 25, 24, 34, 24, 23, 34, 23, 22,
                34, 22, 21, 34, 21, 20, 34, 20, 19, 19, 18, 17, 19, 17, 16, 19, 16, 15, 19, 15, 14, 19, 14, 13, 19, 13, 12, 19, 12, 11, 19, 11, 10, 19, 10, 9,
                49, 48, 34, 34, 19, 9, 34, 9, 8, 34, 8, 7, 34, 7, 6, 34, 6, 5, 34, 5, 4, 34, 4, 3, 34, 3, 2, 34, 2, 1, 34, 1, 0, 34, 0, 52, 34, 52, 51, 34, 51,
                50, 34, 50, 49,
            ]);
        }
        EyeOff => {
            pen.glyph(1.0, &[
                &[
                    (6.667, 4.126), (7.033, 4.018), (7.407, 3.945), (7.787, 3.906), (8.168, 3.903), (8.548, 3.936), (8.924, 4.004), (9.291, 4.107),
                    (9.648, 4.243), (9.99, 4.412), (10.314, 4.612), (10.619, 4.842), (10.901, 5.099), (11.158, 5.381), (11.388, 5.686), (11.588, 6.01),
                    (11.757, 6.352), (11.893, 6.709), (11.996, 7.076), (12.064, 7.452), (12.097, 7.832), (12.094, 8.213), (12.055, 8.593), (11.982, 8.967),
                    (11.874, 9.333), (13.254, 10.713), (13.755, 10.239), (14.183, 9.766), (14.529, 9.308), (14.785, 8.879), (14.945, 8.491), (15.0, 8.16),
                    (14.851, 7.548), (14.433, 6.807), (13.79, 5.999), (12.964, 5.189), (11.999, 4.439), (10.939, 3.813), (10.743, 3.717), (10.544, 3.628),
                    (10.342, 3.544), (10.138, 3.468), (9.931, 3.398), (9.722, 3.335), (9.511, 3.279), (9.299, 3.23), (9.085, 3.188), (8.869, 3.153),
                    (8.653, 3.125), (8.436, 3.104), (8.218, 3.09), (8.0, 3.083), (7.793, 3.088), (7.587, 3.099), (7.381, 3.117), (7.176, 3.142),
                    (6.971, 3.174), (6.768, 3.212), (6.567, 3.257), (6.366, 3.309), (6.168, 3.367), (5.972, 3.431),
                ],
            ], &[
                1, 0, 61, 1, 61, 60, 1, 60, 59, 1, 59, 58, 1, 58, 57, 1, 57, 56, 1, 56, 55, 1, 55, 54, 1, 54, 53, 1, 53, 52, 1, 52, 51, 1, 51, 50, 1, 50, 49,
                1, 49, 48, 1, 48, 47, 1, 47, 46, 1, 46, 45, 1, 45, 44, 1, 44, 43, 1, 43, 42, 42, 41, 40, 42, 40, 39, 42, 39, 38, 42, 38, 37, 42, 37, 36, 42,
                36, 35, 42, 35, 34, 42, 34, 33, 42, 33, 32, 42, 32, 31, 42, 31, 30, 42, 30, 29, 42, 29, 28, 42, 28, 27, 27, 26, 25, 27, 25, 24, 27, 24, 23, 27,
                23, 22, 27, 22, 21, 27, 21, 20, 27, 20, 19, 27, 19, 18, 27, 18, 17, 27, 17, 16, 27, 16, 15, 27, 15, 14, 2, 1, 42, 42, 27, 14, 42, 14, 13, 42,
                13, 12, 42, 12, 11, 42, 11, 10, 42, 10, 9, 42, 9, 8, 42, 8, 7, 42, 7, 6, 42, 6, 5, 42, 5, 4, 42, 4, 3, 42, 3, 2,
            ]);
            pen.glyph(1.0, &[
                &[
                    (13.733, 13.079), (9.883, 9.228), (9.993, 9.042), (10.084, 8.845), (10.156, 8.641), (10.208, 8.431), (10.24, 8.216), (10.25, 8.0),
                    (10.244, 7.865), (10.229, 7.731), (10.205, 7.598), (10.172, 7.467), (10.057, 7.628), (9.916, 7.767), (9.753, 7.88), (9.574, 7.964),
                    (9.383, 8.016), (9.185, 8.035), (8.98, 8.017), (8.78, 7.964), (8.593, 7.877), (8.424, 7.758), (8.278, 7.613), (8.16, 7.444),
                    (8.072, 7.257), (8.018, 7.058), (8.0, 6.852), (8.02, 6.648), (8.075, 6.451), (8.164, 6.267), (8.284, 6.101), (8.43, 5.958), (8.6, 5.844),
                    (8.452, 5.805), (8.303, 5.776), (8.152, 5.758), (8.0, 5.75), (7.784, 5.761), (7.569, 5.792), (7.359, 5.844), (7.154, 5.917),
                    (6.958, 6.008), (6.772, 6.119), (2.751, 2.098), (2.643, 2.025), (2.515, 2.0), (2.388, 2.025), (2.28, 2.098), (1.431, 2.946),
                    (1.359, 3.054), (1.333, 3.182), (1.359, 3.31), (1.431, 3.418), (3.124, 5.111), (2.514, 5.683), (1.994, 6.261), (1.573, 6.82),
                    (1.261, 7.339), (1.067, 7.793), (1.0, 8.16), (1.194, 8.843), (1.742, 9.686), (2.592, 10.591), (3.692, 11.464), (4.99, 12.206),
                    (6.434, 12.722), (7.972, 12.916), (8.182, 12.91), (8.393, 12.897), (8.603, 12.878), (8.812, 12.853), (9.021, 12.822), (9.228, 12.784),
                    (9.434, 12.739), (9.639, 12.689), (9.842, 12.632), (10.043, 12.569), (10.242, 12.5), (10.439, 12.424), (12.412, 14.398), (12.521, 14.47),
                    (12.648, 14.495), (12.776, 14.47), (12.884, 14.398), (13.733, 13.551), (13.806, 13.442), (13.831, 13.315), (13.806, 13.187),
                ],
                &[
                    (8.0, 12.1), (7.732, 12.091), (7.465, 12.065), (7.2, 12.021), (6.939, 11.96), (6.682, 11.882), (6.431, 11.788), (6.187, 11.677),
                    (5.95, 11.551), (5.722, 11.409), (5.504, 11.253), (5.297, 11.083), (5.101, 10.899), (4.917, 10.703), (4.747, 10.496), (4.591, 10.278),
                    (4.449, 10.05), (4.323, 9.813), (4.212, 9.569), (4.118, 9.318), (4.04, 9.061), (3.979, 8.8), (3.935, 8.535), (3.909, 8.268), (3.9, 8.0),
                    (3.906, 7.803), (3.921, 7.606), (3.947, 7.41), (3.981, 7.215), (4.026, 7.023), (4.079, 6.833), (4.142, 6.645), (4.214, 6.461),
                    (4.294, 6.281), (5.774, 7.761), (5.766, 7.82), (5.759, 7.88), (5.754, 7.94), (5.75, 8.0), (5.761, 8.221), (5.793, 8.439), (5.847, 8.653),
                    (5.921, 8.861), (6.016, 9.061), (6.129, 9.25), (6.261, 9.427), (6.409, 9.591), (6.573, 9.739), (6.75, 9.871), (6.939, 9.984),
                    (7.139, 10.079), (7.347, 10.153), (7.561, 10.207), (7.779, 10.239), (8.0, 10.25), (8.06, 10.247), (8.12, 10.241), (8.18, 10.235),
                    (8.239, 10.226), (9.719, 11.706), (9.539, 11.787), (9.355, 11.859), (9.167, 11.921), (8.977, 11.975), (8.785, 12.019), (8.59, 12.053),
                    (8.394, 12.079), (8.197, 12.094),
                ],
            ], &[
                59, 112, 111, 59, 111, 110, 59, 110, 109, 59, 109, 108, 59, 108, 107, 59, 107, 106, 59, 106, 105, 59, 105, 104, 59, 104, 103, 59, 103, 102, 59,
                102, 101, 59, 101, 100, 59, 100, 99, 59, 99, 98, 59, 98, 97, 146, 145, 144, 146, 144, 143, 146, 143, 142, 146, 142, 141, 146, 141, 140, 146,
                140, 139, 146, 139, 138, 146, 138, 137, 146, 137, 136, 146, 136, 135, 146, 135, 134, 146, 134, 133, 146, 133, 132, 146, 132, 131, 146, 131,
                130, 146, 130, 129, 146, 129, 128, 146, 128, 127, 146, 127, 126, 146, 126, 125, 146, 125, 124, 146, 124, 123, 146, 123, 122, 146, 122, 121,
                113, 112, 59, 113, 59, 58, 113, 58, 57, 113, 57, 56, 113, 56, 55, 113, 55, 54, 113, 54, 53, 53, 52, 51, 53, 51, 50, 53, 50, 49, 53, 49, 48, 53,
                48, 47, 53, 47, 46, 53, 46, 45, 53, 45, 44, 53, 44, 43, 53, 43, 42, 42, 41, 40, 42, 40, 39, 42, 39, 38, 42, 38, 37, 42, 37, 36, 42, 36, 35, 42,
                35, 34, 42, 34, 33, 42, 33, 32, 42, 32, 31, 42, 31, 30, 42, 30, 29, 42, 29, 28, 42, 28, 27, 42, 27, 26, 42, 26, 25, 42, 25, 24, 42, 24, 23, 42,
                23, 22, 42, 22, 21, 12, 11, 10, 12, 10, 9, 12, 9, 8, 12, 8, 7, 12, 7, 6, 12, 6, 5, 12, 5, 4, 12, 4, 3, 12, 3, 2, 12, 2, 1, 1, 0, 87, 1, 87, 86,
                1, 86, 85, 1, 85, 84, 1, 84, 83, 1, 83, 82, 1, 82, 81, 1, 81, 80, 1, 80, 79, 1, 79, 78, 1, 78, 77, 1, 77, 76, 1, 76, 75, 75, 74, 73, 75, 73,
                72, 75, 72, 71, 75, 71, 70, 75, 70, 69, 75, 69, 68, 75, 68, 67, 75, 67, 66, 75, 66, 65, 75, 65, 64, 75, 64, 63, 63, 62, 61, 63, 61, 60, 63, 60,
                59, 63, 59, 97, 63, 97, 96, 63, 96, 95, 63, 95, 94, 63, 94, 93, 63, 93, 92, 63, 92, 91, 63, 91, 90, 114, 113, 53, 53, 42, 21, 53, 21, 20, 13,
                12, 1, 75, 63, 90, 75, 90, 89, 75, 89, 88, 75, 88, 155, 75, 155, 154, 75, 154, 153, 75, 153, 152, 75, 152, 151, 75, 151, 150, 75, 150, 149, 75,
                149, 148, 75, 148, 147, 115, 114, 53, 14, 13, 1, 14, 1, 75, 14, 75, 147, 14, 147, 146, 116, 115, 53, 15, 14, 146, 117, 116, 53, 16, 15, 146,
                118, 117, 53, 17, 16, 146, 119, 118, 53, 18, 17, 146, 120, 119, 53, 19, 18, 146, 19, 146, 121, 121, 120, 53, 121, 53, 20, 121, 20, 19,
            ]);
        }
        Speaker => {
            pen.glyph(1.0, &[
                &[
                    (1.0, 5.5), (1.038, 5.309), (1.146, 5.146), (1.309, 5.038), (1.5, 5.0), (4.5, 5.0), (8.168, 1.74), (8.368, 1.631), (8.596, 1.623),
                    (8.803, 1.716), (8.948, 1.892), (9.0, 2.114), (9.0, 13.886), (8.948, 14.108), (8.803, 14.284), (8.596, 14.377), (8.368, 14.368),
                    (8.168, 14.26), (4.5, 11.0), (1.5, 11.0), (1.309, 10.962), (1.146, 10.854), (1.038, 10.691), (1.0, 10.5),
                ],
            ], &[
                22, 23, 0, 22, 0, 1, 22, 1, 2, 22, 2, 3, 22, 3, 4, 22, 4, 5, 22, 5, 6, 22, 6, 7, 22, 7, 8, 22, 8, 9, 22, 9, 10, 22, 10, 11, 11, 12, 13, 11, 13,
                14, 11, 14, 15, 11, 15, 16, 11, 16, 17, 11, 17, 18, 11, 18, 19, 11, 19, 20, 11, 20, 21, 11, 21, 22,
            ]);
            pen.glyph(1.0, &[
                &[
                    (10.5, 6.27), (10.5, 6.777), (10.513, 6.859), (10.552, 6.932), (10.614, 6.987), (10.691, 7.018), (10.91, 7.087), (11.107, 7.204),
                    (11.272, 7.363), (11.397, 7.556), (11.475, 7.772), (11.501, 8.0), (11.475, 8.228), (11.397, 8.444), (11.272, 8.636), (11.107, 8.795),
                    (10.91, 8.912), (10.691, 8.981), (10.614, 9.013), (10.552, 9.068), (10.513, 9.141), (10.5, 9.222), (10.5, 9.73), (10.524, 9.836),
                    (10.59, 9.922), (10.686, 9.973), (10.794, 9.98), (11.036, 9.929), (11.27, 9.848), (11.492, 9.739), (11.699, 9.603), (11.888, 9.443),
                    (12.055, 9.261), (12.199, 9.059), (12.317, 8.841), (12.406, 8.611), (12.467, 8.371), (12.498, 8.125), (12.498, 7.878), (12.467, 7.632),
                    (12.406, 7.392), (12.317, 7.162), (12.199, 6.944), (12.055, 6.742), (11.888, 6.56), (11.699, 6.4), (11.492, 6.264), (11.27, 6.155),
                    (11.036, 6.074), (10.794, 6.023), (10.686, 6.029), (10.591, 6.08), (10.525, 6.165),
                ],
            ], &[
                1, 0, 52, 1, 52, 51, 1, 51, 50, 1, 50, 49, 1, 49, 48, 1, 48, 47, 1, 47, 46, 1, 46, 45, 1, 45, 44, 1, 44, 43, 1, 43, 42, 1, 42, 41, 1, 41, 40,
                1, 40, 39, 1, 39, 38, 1, 38, 37, 37, 36, 35, 37, 35, 34, 37, 34, 33, 37, 33, 32, 37, 32, 31, 37, 31, 30, 37, 30, 29, 37, 29, 28, 37, 28, 27,
                37, 27, 26, 37, 26, 25, 37, 25, 24, 37, 24, 23, 37, 23, 22, 37, 22, 21, 37, 21, 20, 20, 19, 18, 20, 18, 17, 20, 17, 16, 6, 5, 4, 6, 4, 3, 6, 3,
                2, 6, 2, 1, 37, 20, 16, 37, 16, 15, 37, 15, 14, 37, 14, 13, 37, 13, 12, 37, 12, 11, 37, 11, 10, 37, 10, 9, 37, 9, 8, 37, 8, 7, 7, 6, 1, 7, 1,
                37,
            ]);
            pen.glyph(1.0, &[
                &[
                    (10.5, 3.76), (10.5, 4.261), (10.519, 4.353), (10.571, 4.432), (10.648, 4.486), (10.739, 4.508), (11.174, 4.564), (11.598, 4.675),
                    (12.005, 4.838), (12.388, 5.05), (12.742, 5.309), (13.06, 5.61), (13.339, 5.948), (13.573, 6.318), (13.759, 6.715), (13.894, 7.132),
                    (13.976, 7.562), (14.003, 8.0), (13.976, 8.437), (13.894, 8.867), (13.759, 9.284), (13.573, 9.681), (13.339, 10.051), (13.06, 10.389),
                    (12.742, 10.69), (12.388, 10.949), (12.005, 11.161), (11.598, 11.324), (11.174, 11.435), (10.739, 11.492), (10.648, 11.513),
                    (10.571, 11.567), (10.519, 11.646), (10.5, 11.739), (10.5, 12.239), (10.519, 12.338), (10.576, 12.421), (10.661, 12.475), (10.761, 12.492),
                    (11.324, 12.423), (11.874, 12.285), (12.402, 12.078), (12.9, 11.807), (13.359, 11.475), (13.774, 11.088), (14.136, 10.652),
                    (14.441, 10.174), (14.683, 9.661), (14.859, 9.122), (14.966, 8.565), (15.001, 7.999), (14.966, 7.433), (14.859, 6.876), (14.683, 6.337),
                    (14.441, 5.824), (14.136, 5.346), (13.774, 4.91), (13.359, 4.523), (12.9, 4.191), (12.402, 3.92), (11.874, 3.713), (11.324, 3.575),
                    (10.761, 3.507), (10.661, 3.523), (10.576, 3.577), (10.519, 3.661),
                ],
            ], &[
                1, 0, 65, 1, 65, 64, 1, 64, 63, 1, 63, 62, 1, 62, 61, 1, 61, 60, 1, 60, 59, 1, 59, 58, 1, 58, 57, 1, 57, 56, 1, 56, 55, 55, 54, 53, 55, 53, 52,
                55, 52, 51, 55, 51, 50, 55, 50, 49, 55, 49, 48, 55, 48, 47, 55, 47, 46, 55, 46, 45, 45, 44, 43, 45, 43, 42, 45, 42, 41, 45, 41, 40, 45, 40, 39,
                45, 39, 38, 45, 38, 37, 45, 37, 36, 45, 36, 35, 45, 35, 34, 45, 34, 33, 45, 33, 32, 32, 31, 30, 32, 30, 29, 32, 29, 28, 32, 28, 27, 6, 5, 4, 6,
                4, 3, 6, 3, 2, 6, 2, 1, 45, 32, 27, 45, 27, 26, 45, 26, 25, 45, 25, 24, 45, 24, 23, 45, 23, 22, 45, 22, 21, 45, 21, 20, 45, 20, 19, 45, 19, 18,
                45, 18, 17, 7, 6, 1, 7, 1, 55, 55, 45, 17, 55, 17, 16, 55, 16, 15, 55, 15, 14, 55, 14, 13, 55, 13, 12, 55, 12, 11, 55, 11, 10, 55, 10, 9, 55,
                9, 8, 55, 8, 7,
            ]);
        }
        Mute => {
            pen.glyph(1.0, &[
                &[
                    (5.333, 12.0), (5.338, 11.785), (5.352, 11.57), (5.376, 11.356), (5.409, 11.144), (5.451, 10.933), (5.503, 10.724), (5.564, 10.517),
                    (5.634, 10.314), (5.712, 10.113), (5.8, 9.917), (5.896, 9.724), (6.0, 9.536), (6.113, 9.353), (6.234, 9.174), (6.362, 9.002),
                    (6.498, 8.835), (6.641, 8.674), (6.791, 8.52), (6.948, 8.372), (7.111, 8.232), (7.111, 2.222), (7.063, 2.046), (6.928, 1.924),
                    (6.748, 1.895), (6.58, 1.968), (3.63, 4.991), (3.537, 5.087), (3.448, 5.176), (3.357, 5.253), (3.256, 5.313), (3.138, 5.352),
                    (2.998, 5.366), (0.444, 5.366), (0.274, 5.4), (0.13, 5.496), (0.034, 5.64), (0.0, 5.81), (0.0, 10.22), (0.034, 10.391), (0.13, 10.535),
                    (0.274, 10.631), (0.444, 10.665), (2.998, 10.665), (3.139, 10.679), (3.257, 10.719), (3.359, 10.78), (3.451, 10.857), (3.539, 10.946),
                    (3.63, 11.04), (5.41, 12.864), (5.376, 12.649), (5.352, 12.433), (5.338, 12.217),
                ],
            ], &[
                51, 50, 49, 51, 49, 48, 51, 48, 47, 43, 42, 41, 43, 41, 40, 43, 40, 39, 43, 39, 38, 43, 38, 37, 43, 37, 36, 43, 36, 35, 43, 35, 34, 43, 34, 33,
                43, 33, 32, 43, 32, 31, 43, 31, 30, 43, 30, 29, 43, 29, 28, 43, 28, 27, 43, 27, 26, 43, 26, 25, 43, 25, 24, 43, 24, 23, 43, 23, 22, 43, 22, 21,
                43, 21, 20, 43, 20, 19, 43, 19, 18, 43, 18, 17, 43, 17, 16, 43, 16, 15, 43, 15, 14, 43, 14, 13, 43, 13, 12, 43, 12, 11, 43, 11, 10, 43, 10, 9,
                43, 9, 8, 43, 8, 7, 43, 7, 6, 52, 51, 47, 52, 47, 46, 44, 43, 6, 44, 6, 5, 44, 5, 4, 44, 4, 3, 53, 52, 46, 45, 44, 3, 45, 3, 2, 45, 2, 1, 0,
                53, 46, 46, 45, 1, 46, 1, 0,
            ]);
            pen.glyph(1.0, &[
                &[
                    (10.222, 8.044), (9.451, 8.12), (8.708, 8.346), (8.025, 8.711), (7.425, 9.203), (6.933, 9.802), (6.568, 10.486), (6.343, 11.228),
                    (6.267, 12.0), (6.343, 12.772), (6.568, 13.514), (6.933, 14.198), (7.425, 14.797), (8.025, 15.289), (8.708, 15.654), (9.451, 15.88),
                    (10.222, 15.956), (10.994, 15.88), (11.736, 15.654), (12.42, 15.289), (13.019, 14.797), (13.511, 14.198), (13.877, 13.514),
                    (14.102, 12.772), (14.178, 12.0), (14.169, 11.741), (14.144, 11.484), (14.102, 11.228), (14.043, 10.976), (13.968, 10.729),
                    (13.877, 10.486), (13.77, 10.251), (13.648, 10.022), (13.511, 9.802), (13.36, 9.592), (13.196, 9.392), (13.019, 9.203), (12.83, 9.026),
                    (12.63, 8.862), (12.42, 8.711), (12.2, 8.574), (11.972, 8.452), (11.736, 8.346), (11.494, 8.254), (11.246, 8.179), (10.994, 8.12),
                    (10.739, 8.078), (10.481, 8.053),
                ],
                &[
                    (7.111, 12.0), (7.119, 11.796), (7.139, 11.594), (7.174, 11.393), (7.221, 11.195), (7.281, 11.0), (7.355, 10.81), (7.44, 10.626),
                    (7.537, 10.447), (7.646, 10.275), (7.767, 10.11), (12.112, 14.456), (11.876, 14.622), (11.626, 14.766), (11.364, 14.887), (11.091, 14.982),
                    (10.811, 15.051), (10.526, 15.095), (10.238, 15.111), (9.949, 15.1), (9.663, 15.063), (9.382, 14.999), (9.107, 14.909), (8.842, 14.794),
                    (8.59, 14.655), (8.351, 14.493), (8.128, 14.309), (7.923, 14.106), (7.739, 13.884), (7.575, 13.646), (7.435, 13.394), (7.319, 13.129),
                    (7.227, 12.856), (7.162, 12.574), (7.123, 12.288),
                ],
                &[
                    (12.678, 13.89), (8.332, 9.544), (8.64, 9.333), (8.971, 9.16), (9.32, 9.028), (9.683, 8.94), (10.054, 8.895), (10.427, 8.896),
                    (10.797, 8.941), (11.16, 9.031), (11.509, 9.163), (11.839, 9.337), (12.146, 9.549), (12.426, 9.796), (12.673, 10.076), (12.885, 10.383),
                    (13.059, 10.714), (13.192, 11.062), (13.281, 11.425), (13.327, 11.795), (13.327, 12.169), (13.283, 12.539), (13.194, 12.902),
                    (13.062, 13.251), (12.889, 13.582),
                ],
            ], &[
                85, 84, 5, 85, 5, 4, 85, 4, 3, 85, 3, 2, 85, 2, 1, 85, 1, 0, 85, 0, 47, 85, 47, 46, 46, 45, 44, 46, 44, 43, 46, 43, 42, 46, 42, 41, 46, 41, 40,
                46, 40, 39, 46, 39, 38, 46, 38, 37, 46, 37, 36, 46, 36, 35, 46, 35, 34, 46, 34, 33, 46, 33, 32, 46, 32, 31, 46, 31, 30, 46, 30, 29, 46, 29, 28,
                46, 28, 27, 46, 27, 26, 26, 25, 24, 26, 24, 23, 26, 23, 22, 26, 22, 21, 26, 21, 20, 26, 20, 19, 26, 19, 18, 18, 17, 16, 18, 16, 15, 18, 15, 14,
                18, 14, 13, 18, 13, 12, 12, 11, 10, 12, 10, 9, 12, 9, 8, 8, 48, 82, 8, 82, 81, 8, 81, 80, 8, 80, 79, 8, 79, 78, 8, 78, 77, 8, 77, 76, 49, 48,
                8, 49, 8, 7, 49, 7, 6, 49, 6, 5, 86, 85, 46, 12, 8, 76, 12, 76, 75, 12, 75, 74, 12, 74, 73, 12, 73, 72, 12, 72, 71, 12, 71, 70, 12, 70, 69, 12,
                69, 68, 12, 68, 67, 50, 49, 5, 87, 86, 46, 18, 12, 67, 18, 67, 66, 18, 66, 65, 18, 65, 64, 18, 64, 63, 18, 63, 62, 18, 62, 61, 18, 61, 60, 18,
                60, 59, 51, 50, 5, 88, 87, 46, 52, 51, 5, 89, 88, 46, 53, 52, 5, 90, 89, 46, 54, 53, 5, 91, 90, 46, 55, 54, 5, 92, 91, 46, 56, 55, 5, 93, 92,
                46, 57, 56, 5, 94, 93, 46, 58, 57, 5, 58, 5, 84, 58, 84, 83, 95, 94, 46, 59, 58, 83, 96, 95, 46, 96, 46, 26, 18, 59, 83, 18, 83, 107, 97, 96,
                26, 26, 18, 107, 26, 107, 106, 26, 106, 105, 26, 105, 104, 26, 104, 103, 26, 103, 102, 26, 102, 101, 26, 101, 100, 26, 100, 99, 26, 99, 98, 26,
                98, 97,
            ]);
        }
        Lock => pen.glyph(1.0, &[
            &[
                (12.889, 7.111), (12.444, 7.111), (12.444, 6.222), (12.406, 5.642), (12.293, 5.072), (12.106, 4.521), (11.849, 4.0), (11.526, 3.517),
                (11.143, 3.08), (10.706, 2.696), (10.222, 2.373), (9.701, 2.116), (9.15, 1.929), (8.58, 1.816), (8.0, 1.778), (7.42, 1.816), (6.85, 1.929),
                (6.299, 2.116), (5.778, 2.373), (5.294, 2.696), (4.857, 3.08), (4.474, 3.517), (4.151, 4.0), (3.894, 4.521), (3.707, 5.072), (3.594, 5.642),
                (3.556, 6.222), (3.556, 7.111), (3.111, 7.111), (2.941, 7.145), (2.797, 7.241), (2.7, 7.385), (2.667, 7.556), (2.667, 14.667), (2.7, 14.837),
                (2.797, 14.981), (2.941, 15.077), (3.111, 15.111), (12.889, 15.111), (13.059, 15.077), (13.203, 14.981), (13.3, 14.837), (13.333, 14.667),
                (13.333, 7.556), (13.3, 7.385), (13.203, 7.241), (13.059, 7.145),
            ],
            &[
                (5.333, 6.222), (5.356, 5.874), (5.424, 5.532), (5.536, 5.202), (5.691, 4.889), (5.884, 4.599), (6.114, 4.337), (6.377, 4.107), (6.667, 3.913),
                (6.98, 3.759), (7.31, 3.646), (7.652, 3.578), (8.0, 3.556), (8.348, 3.578), (8.69, 3.646), (9.02, 3.759), (9.333, 3.913), (9.623, 4.107),
                (9.886, 4.337), (10.116, 4.599), (10.309, 4.889), (10.464, 5.202), (10.576, 5.532), (10.644, 5.874), (10.667, 6.222), (10.667, 7.111),
                (5.333, 7.111),
            ],
            &[
                (8.889, 11.654), (8.889, 12.889), (8.855, 13.059), (8.759, 13.203), (8.615, 13.3), (8.444, 13.333), (7.556, 13.333), (7.385, 13.3),
                (7.241, 13.203), (7.145, 13.059), (7.111, 12.889), (7.111, 11.654), (6.931, 11.457), (6.793, 11.227), (6.704, 10.975), (6.668, 10.71),
                (6.685, 10.443), (6.754, 10.185), (6.874, 9.946), (7.04, 9.735), (7.244, 9.562), (7.478, 9.433), (7.734, 9.354), (8.0, 9.327), (8.266, 9.354),
                (8.522, 9.433), (8.756, 9.562), (8.96, 9.735), (9.126, 9.946), (9.246, 10.185), (9.315, 10.443), (9.332, 10.71), (9.296, 10.975),
                (9.207, 11.227), (9.069, 11.457),
            ],
        ], &[
            32, 89, 88, 32, 88, 87, 32, 87, 86, 32, 86, 85, 32, 85, 84, 32, 84, 83, 32, 83, 82, 75, 74, 108, 75, 108, 107, 75, 107, 106, 90, 89, 32, 90, 32,
            31, 90, 31, 30, 90, 30, 29, 90, 29, 28, 90, 28, 27, 90, 27, 26, 26, 47, 73, 48, 47, 26, 48, 26, 25, 48, 25, 24, 48, 24, 23, 48, 23, 22, 48, 22, 21,
            48, 21, 20, 48, 20, 19, 48, 19, 18, 18, 17, 16, 18, 16, 15, 18, 15, 14, 18, 14, 13, 18, 13, 12, 18, 12, 11, 18, 11, 10, 18, 10, 9, 18, 9, 8, 18, 8,
            7, 18, 7, 6, 18, 6, 5, 18, 5, 4, 4, 3, 2, 4, 2, 1, 1, 0, 46, 1, 46, 45, 1, 45, 44, 1, 44, 43, 1, 43, 42, 1, 42, 41, 1, 41, 40, 1, 40, 39, 1, 39,
            38, 38, 37, 36, 38, 36, 35, 38, 35, 34, 38, 34, 33, 33, 32, 82, 33, 82, 81, 33, 81, 80, 33, 80, 79, 33, 79, 78, 76, 75, 106, 91, 90, 26, 91, 26,
            73, 49, 48, 18, 4, 1, 38, 38, 33, 78, 38, 78, 77, 38, 77, 76, 38, 76, 106, 38, 106, 105, 38, 105, 104, 38, 104, 103, 38, 103, 102, 92, 91, 73, 50,
            49, 18, 4, 38, 102, 4, 102, 101, 4, 101, 100, 93, 92, 73, 51, 50, 18, 94, 93, 73, 94, 73, 72, 52, 51, 18, 95, 94, 72, 53, 52, 18, 96, 95, 72, 54,
            53, 18, 97, 96, 72, 55, 54, 18, 98, 97, 72, 56, 55, 18, 99, 98, 72, 57, 56, 18, 100, 99, 72, 58, 57, 18, 4, 100, 72, 4, 72, 71, 4, 71, 70, 4, 70,
            69, 4, 69, 68, 4, 68, 67, 4, 67, 66, 4, 66, 65, 4, 65, 64, 4, 64, 63, 4, 63, 62, 59, 58, 18, 18, 4, 62, 18, 62, 61, 18, 61, 60, 18, 60, 59,
        ]),
        Unlock => pen.glyph(1.0, &[
            &[
                (12.889, 7.111), (5.289, 7.111), (5.289, 4.601), (5.293, 4.388), (5.314, 4.176), (5.351, 3.965), (5.404, 3.759), (5.472, 3.557),
                (5.556, 3.361), (5.654, 3.172), (5.767, 2.991), (5.893, 2.819), (6.032, 2.657), (6.183, 2.506), (6.344, 2.367), (6.516, 2.241), (6.697, 2.128),
                (6.887, 2.03), (7.083, 1.946), (7.285, 1.878), (7.491, 1.825), (7.71, 1.793), (7.931, 1.778), (8.153, 1.782), (8.374, 1.803), (8.592, 1.842),
                (8.806, 1.899), (9.015, 1.973), (9.217, 2.064), (9.411, 2.17), (9.596, 2.293), (9.771, 2.429), (9.933, 2.58), (10.083, 2.743), (10.22, 2.917),
                (10.342, 3.102), (10.448, 3.297), (10.485, 3.384), (10.52, 3.473), (10.552, 3.562), (10.581, 3.652), (10.626, 3.73), (10.698, 3.784),
                (10.786, 3.804), (10.874, 3.789), (12.068, 3.316), (12.139, 3.271), (12.189, 3.204), (12.21, 3.123), (12.201, 3.039), (12.117, 2.807),
                (12.02, 2.58), (11.911, 2.359), (11.789, 2.144), (11.656, 1.936), (11.512, 1.736), (11.356, 1.544), (11.19, 1.361), (11.014, 1.188),
                (10.829, 1.025), (10.635, 0.872), (10.433, 0.731), (10.223, 0.601), (10.006, 0.483), (9.783, 0.377), (9.555, 0.284), (9.322, 0.203),
                (9.084, 0.136), (8.843, 0.082), (8.6, 0.041), (8.354, 0.015), (8.108, 0.001), (7.861, 0.002), (7.615, 0.016), (6.536, 0.264), (5.567, 0.783),
                (4.745, 1.524), (4.11, 2.436), (3.701, 3.47), (3.556, 4.574), (3.556, 7.111), (3.111, 7.111), (2.941, 7.145), (2.797, 7.241), (2.7, 7.385),
                (2.667, 7.556), (2.667, 14.667), (2.7, 14.837), (2.797, 14.981), (2.941, 15.077), (3.111, 15.111), (12.889, 15.111), (13.059, 15.077),
                (13.203, 14.981), (13.3, 14.837), (13.333, 14.667), (13.333, 7.556), (13.3, 7.385), (13.203, 7.241), (13.059, 7.145),
            ],
            &[
                (8.889, 11.654), (8.889, 12.889), (8.855, 13.059), (8.759, 13.203), (8.615, 13.3), (8.444, 13.333), (7.556, 13.333), (7.385, 13.3),
                (7.241, 13.203), (7.145, 13.059), (7.111, 12.889), (7.111, 11.654), (6.931, 11.457), (6.793, 11.227), (6.704, 10.975), (6.668, 10.71),
                (6.685, 10.443), (6.754, 10.185), (6.874, 9.946), (7.04, 9.735), (7.244, 9.562), (7.478, 9.433), (7.734, 9.354), (8.0, 9.327), (8.266, 9.354),
                (8.522, 9.433), (8.756, 9.562), (8.96, 9.735), (9.126, 9.946), (9.246, 10.185), (9.315, 10.443), (9.332, 10.71), (9.296, 10.975),
                (9.207, 11.227), (9.069, 11.457),
            ],
        ], &[
            85, 115, 114, 85, 114, 113, 85, 113, 112, 85, 112, 111, 85, 111, 110, 85, 110, 109, 85, 109, 108, 101, 100, 134, 101, 134, 133, 101, 133, 132, 116,
            115, 85, 116, 85, 84, 116, 84, 83, 116, 83, 82, 116, 82, 81, 116, 81, 80, 116, 80, 79, 116, 79, 78, 78, 77, 76, 78, 76, 75, 78, 75, 74, 78, 74, 73,
            78, 73, 72, 78, 72, 71, 78, 71, 70, 78, 70, 69, 78, 69, 68, 78, 68, 67, 78, 67, 66, 78, 66, 65, 78, 65, 64, 78, 64, 63, 63, 62, 61, 63, 61, 60, 63,
            60, 59, 63, 59, 58, 63, 58, 57, 63, 57, 56, 63, 56, 55, 63, 55, 54, 63, 54, 53, 63, 53, 52, 63, 52, 51, 63, 51, 50, 63, 50, 49, 63, 49, 48, 63, 48,
            47, 63, 47, 46, 63, 46, 45, 63, 45, 44, 63, 44, 43, 63, 43, 42, 63, 42, 41, 63, 41, 40, 63, 40, 39, 63, 39, 38, 63, 38, 37, 63, 37, 36, 63, 36, 35,
            63, 35, 34, 63, 34, 33, 63, 33, 32, 63, 32, 31, 63, 31, 30, 63, 30, 29, 63, 29, 28, 63, 28, 27, 63, 27, 26, 63, 26, 25, 63, 25, 24, 63, 24, 23, 63,
            23, 22, 63, 22, 21, 63, 21, 20, 63, 20, 19, 63, 19, 18, 63, 18, 17, 1, 0, 99, 1, 99, 98, 1, 98, 97, 1, 97, 96, 96, 95, 94, 96, 94, 93, 96, 93, 92,
            96, 92, 91, 91, 90, 89, 91, 89, 88, 91, 88, 87, 91, 87, 86, 86, 85, 108, 86, 108, 107, 86, 107, 106, 86, 106, 105, 86, 105, 104, 102, 101, 132, 78,
            63, 17, 78, 17, 16, 78, 16, 15, 78, 15, 14, 78, 14, 13, 78, 13, 12, 78, 12, 11, 78, 11, 10, 78, 10, 9, 78, 9, 8, 78, 8, 7, 78, 7, 6, 78, 6, 5, 78,
            5, 4, 78, 4, 3, 78, 3, 2, 78, 2, 1, 91, 86, 104, 91, 104, 103, 91, 103, 102, 91, 102, 132, 91, 132, 131, 91, 131, 130, 91, 130, 129, 91, 129, 128,
            116, 78, 1, 96, 91, 128, 96, 128, 127, 96, 127, 126, 96, 126, 125, 96, 125, 124, 96, 124, 123, 96, 123, 122, 96, 122, 121, 117, 116, 1, 1, 96, 121,
            1, 121, 120, 1, 120, 119, 1, 119, 118, 1, 118, 117,
        ]),
        SyncLock => pen.glyph(1.0, &[
            &[
                (2.0, 3.25), (2.0, 6.75), (2.019, 6.846), (2.073, 6.927), (2.154, 6.981), (2.25, 7.0), (9.75, 7.0), (9.846, 6.981), (9.927, 6.927),
                (9.981, 6.846), (10.0, 6.75), (10.0, 6.0), (12.0, 6.0), (12.0, 9.0), (10.0, 9.0), (10.0, 8.25), (9.981, 8.154), (9.927, 8.073), (9.846, 8.019),
                (9.75, 8.0), (2.25, 8.0), (2.154, 8.019), (2.073, 8.073), (2.019, 8.154), (2.0, 8.25), (2.0, 11.75), (2.019, 11.846), (2.073, 11.927),
                (2.154, 11.981), (2.25, 12.0), (9.75, 12.0), (9.846, 11.981), (9.927, 11.927), (9.981, 11.846), (10.0, 11.75), (10.0, 11.0), (13.5, 11.0),
                (13.691, 10.962), (13.854, 10.854), (13.962, 10.691), (14.0, 10.5), (14.0, 4.5), (13.962, 4.309), (13.854, 4.146), (13.691, 4.038),
                (13.5, 4.0), (10.0, 4.0), (10.0, 3.25), (9.981, 3.154), (9.927, 3.073), (9.846, 3.019), (9.75, 3.0), (2.25, 3.0), (2.154, 3.019),
                (2.073, 3.073), (2.019, 3.154),
            ],
            &[
                (3.0, 9.0), (9.0, 9.0), (9.0, 11.0), (3.0, 11.0),
            ],
            &[
                (3.0, 4.0), (9.0, 4.0), (9.0, 6.0), (3.0, 6.0),
            ],
        ], &[
            24, 56, 59, 57, 56, 24, 57, 24, 23, 57, 23, 22, 57, 22, 21, 57, 21, 20, 57, 20, 19, 57, 19, 18, 57, 18, 17, 57, 17, 16, 57, 16, 15, 57, 15, 14, 11,
            10, 9, 11, 9, 8, 11, 8, 7, 11, 7, 6, 11, 6, 5, 11, 5, 4, 11, 4, 3, 11, 3, 2, 11, 2, 1, 1, 0, 60, 1, 60, 63, 1, 63, 62, 61, 60, 0, 61, 0, 55, 61,
            55, 54, 61, 54, 53, 61, 53, 52, 61, 52, 51, 61, 51, 50, 61, 50, 49, 61, 49, 48, 61, 48, 47, 61, 47, 46, 46, 45, 44, 46, 44, 43, 46, 43, 42, 46, 42,
            41, 41, 40, 39, 41, 39, 38, 41, 38, 37, 41, 37, 36, 35, 34, 33, 35, 33, 32, 35, 32, 31, 35, 31, 30, 35, 30, 29, 35, 29, 28, 35, 28, 27, 35, 27, 26,
            35, 26, 25, 25, 24, 59, 25, 59, 58, 58, 57, 14, 58, 14, 13, 11, 1, 62, 11, 62, 61, 11, 61, 46, 11, 46, 41, 35, 25, 58, 35, 58, 13, 12, 11, 41, 12,
            41, 36, 36, 35, 13, 36, 13, 12,
        ]),
        Mic => {
            pen.glyph(1.0, &[
                &[
                    (7.556, 1.956), (6.619, 2.173), (5.992, 2.716), (5.611, 3.422), (5.416, 4.128), (5.344, 4.672), (5.333, 4.889), (5.333, 7.556),
                    (5.344, 7.773), (5.416, 8.316), (5.611, 9.022), (5.992, 9.728), (6.619, 10.272), (7.556, 10.489), (8.487, 10.272), (9.113, 9.728),
                    (9.495, 9.022), (9.692, 8.316), (9.767, 7.773), (9.778, 7.556), (9.778, 4.889), (9.767, 4.672), (9.695, 4.128), (9.5, 3.422),
                    (9.119, 2.716), (8.492, 2.173),
                ],
            ], &[
                1, 0, 25, 1, 25, 24, 1, 24, 23, 1, 23, 22, 1, 22, 21, 1, 21, 20, 1, 20, 19, 1, 19, 18, 1, 18, 17, 1, 17, 16, 1, 16, 15, 1, 15, 14, 1, 14, 13,
                1, 13, 12, 1, 12, 11, 1, 11, 10, 1, 10, 9, 1, 9, 8, 1, 8, 7, 1, 7, 6, 1, 6, 5, 1, 5, 4, 1, 4, 3, 1, 3, 2,
            ]);
            pen.glyph(1.0, &[
                &[
                    (10.667, 8.444), (10.659, 8.661), (10.637, 8.876), (10.599, 9.09), (10.547, 9.3), (10.48, 9.506), (10.4, 9.707), (10.305, 9.902),
                    (10.197, 10.09), (10.076, 10.27), (9.943, 10.441), (9.798, 10.602), (9.643, 10.753), (9.477, 10.893), (9.302, 11.02), (9.119, 11.136),
                    (8.928, 11.238), (8.73, 11.327), (8.527, 11.401), (8.319, 11.462), (8.107, 11.508), (7.893, 11.539), (7.677, 11.555), (7.46, 11.556),
                    (7.248, 11.536), (7.038, 11.503), (6.831, 11.456), (6.627, 11.396), (6.428, 11.321), (6.234, 11.234), (6.046, 11.134), (5.866, 11.022),
                    (5.693, 10.898), (5.529, 10.763), (5.374, 10.618), (5.229, 10.462), (5.095, 10.297), (4.972, 10.124), (4.861, 9.942), (4.762, 9.754),
                    (4.676, 9.56), (4.603, 9.36), (4.544, 9.156), (4.498, 8.948), (4.466, 8.738), (4.448, 8.526), (4.444, 8.314), (4.444, 6.444),
                    (4.428, 6.359), (4.379, 6.287), (4.307, 6.239), (4.222, 6.222), (3.778, 6.222), (3.693, 6.239), (3.621, 6.287), (3.572, 6.359),
                    (3.556, 6.444), (3.556, 8.0), (3.661, 9.185), (3.971, 10.219), (4.479, 11.08), (5.177, 11.749), (6.057, 12.205), (7.111, 12.427),
                    (7.111, 13.333), (4.889, 13.333), (4.719, 13.367), (4.575, 13.464), (4.478, 13.608), (4.444, 13.778), (4.478, 13.948), (4.575, 14.092),
                    (4.719, 14.188), (4.889, 14.222), (10.222, 14.222), (10.392, 14.188), (10.536, 14.092), (10.633, 13.948), (10.667, 13.778),
                    (10.633, 13.608), (10.536, 13.464), (10.392, 13.367), (10.222, 13.333), (8.0, 13.333), (8.0, 12.427), (9.046, 12.204), (9.924, 11.748),
                    (10.623, 11.079), (11.135, 10.218), (11.449, 9.184), (11.556, 8.0), (11.556, 6.444), (11.539, 6.359), (11.49, 6.287), (11.418, 6.239),
                    (11.333, 6.222), (10.889, 6.222), (10.804, 6.239), (10.732, 6.287), (10.684, 6.359), (10.667, 6.444),
                ],
            ], &[
                0, 100, 99, 0, 99, 98, 0, 98, 97, 0, 97, 96, 0, 96, 95, 0, 95, 94, 0, 94, 93, 0, 93, 92, 0, 92, 91, 0, 91, 90, 0, 90, 89, 0, 89, 88, 0, 88, 87,
                87, 86, 85, 87, 85, 84, 83, 82, 81, 83, 81, 80, 83, 80, 79, 83, 79, 78, 83, 78, 77, 83, 77, 76, 83, 76, 75, 83, 75, 74, 83, 74, 73, 83, 73, 72,
                83, 72, 71, 83, 71, 70, 83, 70, 69, 83, 69, 68, 83, 68, 67, 83, 67, 66, 66, 65, 64, 63, 62, 61, 63, 61, 60, 63, 60, 59, 63, 59, 58, 58, 57, 56,
                58, 56, 55, 58, 55, 54, 58, 54, 53, 58, 53, 52, 58, 52, 51, 58, 51, 50, 58, 50, 49, 58, 49, 48, 58, 48, 47, 58, 47, 46, 58, 46, 45, 58, 45, 44,
                58, 44, 43, 58, 43, 42, 58, 42, 41, 58, 41, 40, 58, 40, 39, 58, 39, 38, 58, 38, 37, 58, 37, 36, 58, 36, 35, 58, 35, 34, 58, 34, 33, 1, 0, 87,
                83, 66, 64, 83, 64, 63, 63, 58, 33, 63, 33, 32, 63, 32, 31, 63, 31, 30, 63, 30, 29, 63, 29, 28, 63, 28, 27, 63, 27, 26, 63, 26, 25, 63, 25, 24,
                63, 24, 23, 63, 23, 22, 63, 22, 21, 63, 21, 20, 63, 20, 19, 63, 19, 18, 63, 18, 17, 63, 17, 16, 63, 16, 15, 63, 15, 14, 2, 1, 87, 84, 83, 63,
                84, 63, 14, 84, 14, 13, 84, 13, 12, 84, 12, 11, 3, 2, 87, 87, 84, 11, 87, 11, 10, 87, 10, 9, 87, 9, 8, 87, 8, 7, 87, 7, 6, 87, 6, 5, 87, 5, 4,
                87, 4, 3,
            ]);
        }
        Folder => pen.glyph(1.0, &[
            &[
                (14.667, 3.556), (8.297, 3.559), (6.83, 2.048), (6.694, 1.933), (6.538, 1.848), (6.368, 1.795), (6.191, 1.778), (1.778, 1.778), (1.58, 1.8),
                (1.392, 1.866), (1.224, 1.972), (1.083, 2.112), (0.977, 2.281), (0.911, 2.469), (0.889, 2.667), (0.889, 12.889), (0.923, 13.059),
                (1.019, 13.203), (1.163, 13.3), (1.333, 13.333), (14.667, 13.333), (14.837, 13.3), (14.981, 13.203), (15.077, 13.059), (15.111, 12.889),
                (15.111, 4.0), (15.077, 3.83), (14.981, 3.686), (14.837, 3.589),
            ],
            &[
                (1.778, 2.667), (6.191, 2.667), (7.918, 4.444), (1.778, 4.444),
            ],
        ], &[
            14, 29, 32, 30, 14, 13, 30, 13, 12, 30, 12, 11, 30, 11, 10, 30, 10, 9, 30, 9, 8, 30, 8, 7, 30, 7, 6, 30, 6, 5, 30, 5, 4, 30, 4, 3, 30, 3, 2, 30, 2,
            1, 1, 0, 28, 1, 28, 27, 1, 27, 26, 1, 26, 25, 1, 25, 24, 1, 24, 23, 1, 23, 22, 1, 22, 21, 1, 21, 20, 20, 19, 18, 20, 18, 17, 20, 17, 16, 20, 16,
            15, 15, 14, 32, 15, 32, 31, 31, 30, 1, 31, 1, 20, 31, 20, 15,
        ]),
        Film => pen.glyph(1.0, &[
            &[
                (13.466, 1.0), (2.534, 1.0), (2.335, 1.033), (2.163, 1.14), (2.046, 1.304), (2.0, 1.5), (2.0, 13.5), (2.046, 13.696), (2.163, 13.86),
                (2.335, 13.967), (2.534, 14.0), (13.466, 14.0), (13.665, 13.967), (13.837, 13.86), (13.954, 13.696), (14.0, 13.5), (14.0, 1.5),
                (13.954, 1.304), (13.837, 1.14), (13.665, 1.033),
            ],
            &[
                (10.8, 7.75), (10.777, 7.848), (10.718, 7.93), (10.633, 7.983), (10.534, 8.0), (5.466, 8.0), (5.367, 7.983), (5.282, 7.93), (5.223, 7.848),
                (5.2, 7.75), (5.2, 7.25), (5.223, 7.152), (5.282, 7.07), (5.367, 7.017), (5.466, 7.0), (10.534, 7.0), (10.633, 7.017), (10.718, 7.07),
                (10.777, 7.152), (10.8, 7.25),
            ],
            &[
                (13.0, 12.75), (12.981, 12.846), (12.927, 12.927), (12.846, 12.981), (12.75, 13.0), (12.25, 13.0), (12.154, 12.981), (12.073, 12.927),
                (12.019, 12.846), (12.0, 12.75), (12.0, 12.25), (12.019, 12.154), (12.073, 12.073), (12.154, 12.019), (12.25, 12.0), (12.75, 12.0),
                (12.846, 12.019), (12.927, 12.073), (12.981, 12.154), (13.0, 12.25),
            ],
            &[
                (13.0, 10.75), (12.981, 10.846), (12.927, 10.927), (12.846, 10.981), (12.75, 11.0), (12.25, 11.0), (12.154, 10.981), (12.073, 10.927),
                (12.019, 10.846), (12.0, 10.75), (12.0, 10.25), (12.019, 10.154), (12.073, 10.073), (12.154, 10.019), (12.25, 10.0), (12.75, 10.0),
                (12.846, 10.019), (12.927, 10.073), (12.981, 10.154), (13.0, 10.25),
            ],
            &[
                (13.0, 8.75), (12.981, 8.846), (12.927, 8.927), (12.846, 8.981), (12.75, 9.0), (12.25, 9.0), (12.154, 8.981), (12.073, 8.927), (12.019, 8.846),
                (12.0, 8.75), (12.0, 8.25), (12.019, 8.154), (12.073, 8.073), (12.154, 8.019), (12.25, 8.0), (12.75, 8.0), (12.846, 8.019), (12.927, 8.073),
                (12.981, 8.154), (13.0, 8.25),
            ],
            &[
                (13.0, 6.75), (12.981, 6.846), (12.927, 6.927), (12.846, 6.981), (12.75, 7.0), (12.25, 7.0), (12.154, 6.981), (12.073, 6.927), (12.019, 6.846),
                (12.0, 6.75), (12.0, 6.25), (12.019, 6.154), (12.073, 6.073), (12.154, 6.019), (12.25, 6.0), (12.75, 6.0), (12.846, 6.019), (12.927, 6.073),
                (12.981, 6.154), (13.0, 6.25),
            ],
            &[
                (13.0, 4.75), (12.981, 4.846), (12.927, 4.927), (12.846, 4.981), (12.75, 5.0), (12.25, 5.0), (12.154, 4.981), (12.073, 4.927), (12.019, 4.846),
                (12.0, 4.75), (12.0, 4.25), (12.019, 4.154), (12.073, 4.073), (12.154, 4.019), (12.25, 4.0), (12.75, 4.0), (12.846, 4.019), (12.927, 4.073),
                (12.981, 4.154), (13.0, 4.25),
            ],
            &[
                (13.0, 2.75), (12.981, 2.846), (12.927, 2.927), (12.846, 2.981), (12.75, 3.0), (12.25, 3.0), (12.154, 2.981), (12.073, 2.927), (12.019, 2.846),
                (12.0, 2.75), (12.0, 2.25), (12.019, 2.154), (12.073, 2.073), (12.154, 2.019), (12.25, 2.0), (12.75, 2.0), (12.846, 2.019), (12.927, 2.073),
                (12.981, 2.154), (13.0, 2.25),
            ],
            &[
                (4.0, 12.75), (3.981, 12.846), (3.927, 12.927), (3.846, 12.981), (3.75, 13.0), (3.25, 13.0), (3.154, 12.981), (3.073, 12.927), (3.019, 12.846),
                (3.0, 12.75), (3.0, 12.25), (3.019, 12.154), (3.073, 12.073), (3.154, 12.019), (3.25, 12.0), (3.75, 12.0), (3.846, 12.019), (3.927, 12.073),
                (3.981, 12.154), (4.0, 12.25),
            ],
            &[
                (4.0, 10.75), (3.981, 10.846), (3.927, 10.927), (3.846, 10.981), (3.75, 11.0), (3.25, 11.0), (3.154, 10.981), (3.073, 10.927), (3.019, 10.846),
                (3.0, 10.75), (3.0, 10.25), (3.019, 10.154), (3.073, 10.073), (3.154, 10.019), (3.25, 10.0), (3.75, 10.0), (3.846, 10.019), (3.927, 10.073),
                (3.981, 10.154), (4.0, 10.25),
            ],
            &[
                (4.0, 8.75), (3.981, 8.846), (3.927, 8.927), (3.846, 8.981), (3.75, 9.0), (3.25, 9.0), (3.154, 8.981), (3.073, 8.927), (3.019, 8.846),
                (3.0, 8.75), (3.0, 8.25), (3.019, 8.154), (3.073, 8.073), (3.154, 8.019), (3.25, 8.0), (3.75, 8.0), (3.846, 8.019), (3.927, 8.073),
                (3.981, 8.154), (4.0, 8.25),
            ],
            &[
                (4.0, 6.75), (3.981, 6.846), (3.927, 6.927), (3.846, 6.981), (3.75, 7.0), (3.25, 7.0), (3.154, 6.981), (3.073, 6.927), (3.019, 6.846),
                (3.0, 6.75), (3.0, 6.25), (3.019, 6.154), (3.073, 6.073), (3.154, 6.019), (3.25, 6.0), (3.75, 6.0), (3.846, 6.019), (3.927, 6.073),
                (3.981, 6.154), (4.0, 6.25),
            ],
            &[
                (4.0, 4.75), (3.981, 4.846), (3.927, 4.927), (3.846, 4.981), (3.75, 5.0), (3.25, 5.0), (3.154, 4.981), (3.073, 4.927), (3.019, 4.846),
                (3.0, 4.75), (3.0, 4.25), (3.019, 4.154), (3.073, 4.073), (3.154, 4.019), (3.25, 4.0), (3.75, 4.0), (3.846, 4.019), (3.927, 4.073),
                (3.981, 4.154), (4.0, 4.25),
            ],
            &[
                (4.0, 2.75), (3.981, 2.846), (3.927, 2.927), (3.846, 2.981), (3.75, 3.0), (3.25, 3.0), (3.154, 2.981), (3.073, 2.927), (3.019, 2.846),
                (3.0, 2.75), (3.0, 2.25), (3.019, 2.154), (3.073, 2.073), (3.154, 2.019), (3.25, 2.0), (3.75, 2.0), (3.846, 2.019), (3.927, 2.073),
                (3.981, 2.154), (4.0, 2.25),
            ],
        ], &[
            179, 50, 49, 179, 49, 48, 179, 48, 47, 179, 47, 46, 179, 46, 45, 51, 50, 179, 51, 179, 178, 51, 178, 177, 51, 177, 176, 51, 176, 175, 171, 170, 5,
            5, 190, 189, 180, 199, 70, 180, 70, 69, 180, 69, 68, 180, 68, 67, 180, 67, 66, 180, 66, 65, 71, 70, 199, 71, 199, 198, 71, 198, 197, 71, 197, 196,
            71, 196, 195, 191, 190, 5, 5, 210, 209, 200, 219, 90, 200, 90, 89, 200, 89, 88, 200, 88, 87, 200, 87, 86, 200, 86, 85, 91, 90, 219, 91, 219, 218,
            91, 218, 217, 91, 217, 216, 91, 216, 215, 211, 210, 5, 5, 230, 229, 225, 30, 29, 225, 29, 28, 225, 28, 27, 225, 27, 26, 31, 30, 225, 31, 225, 224,
            31, 224, 223, 31, 223, 222, 31, 222, 221, 31, 221, 220, 31, 220, 239, 239, 110, 109, 239, 109, 108, 239, 108, 107, 239, 107, 106, 239, 106, 105,
            111, 110, 239, 111, 239, 238, 111, 238, 237, 111, 237, 236, 111, 236, 235, 231, 230, 5, 5, 250, 249, 5, 249, 248, 240, 259, 130, 240, 130, 129,
            240, 129, 128, 240, 128, 127, 240, 127, 126, 240, 126, 125, 131, 130, 259, 131, 259, 258, 131, 258, 257, 131, 257, 256, 131, 256, 255, 251, 250, 5,
            5, 270, 269, 5, 269, 268, 5, 268, 267, 260, 279, 150, 260, 150, 149, 260, 149, 148, 260, 148, 147, 260, 147, 146, 260, 146, 145, 151, 150, 279,
            151, 279, 278, 151, 278, 277, 151, 277, 276, 151, 276, 275, 271, 270, 5, 271, 5, 4, 271, 4, 3, 271, 3, 2, 271, 2, 1, 1, 0, 19, 1, 19, 18, 1, 18,
            17, 1, 17, 16, 16, 15, 14, 16, 14, 13, 16, 13, 12, 16, 12, 11, 11, 10, 9, 11, 9, 8, 11, 8, 7, 11, 7, 6, 6, 5, 170, 6, 170, 169, 6, 169, 168, 6,
            168, 167, 6, 167, 166, 6, 166, 165, 6, 165, 164, 6, 164, 163, 160, 179, 45, 52, 51, 175, 172, 171, 5, 172, 5, 189, 172, 189, 188, 172, 188, 187,
            172, 187, 186, 172, 186, 185, 172, 185, 184, 172, 184, 183, 172, 183, 182, 181, 180, 65, 72, 71, 195, 192, 191, 5, 192, 5, 209, 192, 209, 208, 192,
            208, 207, 192, 207, 206, 192, 206, 205, 192, 205, 204, 192, 204, 203, 192, 203, 202, 201, 200, 85, 92, 91, 215, 212, 211, 5, 212, 5, 229, 212, 229,
            228, 212, 228, 227, 212, 227, 226, 212, 226, 225, 32, 31, 239, 112, 111, 235, 232, 231, 5, 232, 5, 248, 232, 248, 247, 232, 247, 246, 232, 246,
            245, 232, 245, 244, 232, 244, 243, 232, 243, 242, 241, 240, 125, 132, 131, 255, 252, 251, 5, 252, 5, 267, 252, 267, 266, 252, 266, 265, 252, 265,
            264, 252, 264, 263, 252, 263, 262, 261, 260, 145, 152, 151, 275, 272, 271, 1, 11, 6, 163, 11, 163, 162, 11, 162, 161, 11, 161, 160, 11, 160, 45,
            11, 45, 44, 11, 44, 43, 11, 43, 42, 11, 42, 41, 11, 41, 40, 11, 40, 59, 11, 59, 58, 53, 52, 175, 173, 172, 182, 182, 181, 65, 73, 72, 195, 193,
            192, 202, 202, 201, 85, 93, 92, 215, 213, 212, 225, 33, 32, 239, 113, 112, 235, 233, 232, 242, 242, 241, 125, 133, 132, 255, 253, 252, 262, 262,
            261, 145, 153, 152, 275, 273, 272, 1, 16, 11, 58, 16, 58, 57, 54, 53, 175, 174, 173, 182, 174, 182, 65, 174, 65, 64, 74, 73, 195, 194, 193, 202,
            194, 202, 85, 194, 85, 84, 214, 213, 225, 214, 225, 26, 34, 33, 239, 114, 113, 235, 234, 233, 242, 234, 242, 125, 234, 125, 124, 134, 133, 255,
            254, 253, 262, 254, 262, 145, 254, 145, 144, 154, 153, 275, 274, 273, 1, 274, 1, 16, 175, 174, 64, 195, 194, 84, 215, 214, 26, 215, 26, 25, 35, 34,
            239, 35, 239, 105, 235, 234, 124, 255, 254, 144, 275, 274, 16, 54, 175, 64, 54, 64, 63, 54, 63, 62, 54, 62, 61, 74, 195, 84, 74, 84, 83, 74, 83,
            82, 74, 82, 81, 93, 215, 25, 93, 25, 24, 93, 24, 23, 93, 23, 22, 93, 22, 21, 93, 21, 20, 93, 20, 39, 93, 39, 38, 93, 38, 37, 36, 35, 105, 36, 105,
            104, 114, 235, 124, 114, 124, 123, 114, 123, 122, 114, 122, 121, 134, 255, 144, 134, 144, 143, 134, 143, 142, 134, 142, 141, 154, 275, 16, 55, 54,
            61, 75, 74, 81, 94, 93, 37, 94, 37, 36, 94, 36, 104, 94, 104, 103, 94, 103, 102, 94, 102, 101, 115, 114, 121, 135, 134, 141, 155, 154, 16, 56, 55,
            61, 56, 61, 60, 76, 75, 81, 76, 81, 80, 95, 94, 101, 116, 115, 121, 116, 121, 120, 136, 135, 141, 136, 141, 140, 156, 155, 16, 57, 56, 60, 77, 76,
            80, 96, 95, 101, 96, 101, 100, 117, 116, 120, 137, 136, 140, 157, 156, 16, 16, 57, 60, 16, 60, 79, 16, 79, 78, 16, 78, 77, 16, 77, 80, 16, 80, 99,
            16, 99, 98, 16, 98, 97, 16, 97, 96, 16, 96, 100, 16, 100, 119, 16, 119, 118, 16, 118, 117, 16, 117, 120, 16, 120, 139, 16, 139, 138, 16, 138, 137,
            16, 137, 140, 16, 140, 159, 16, 159, 158, 16, 158, 157,
        ]),
        Sequence => {
            pen.glyph(1.0, &[
                &[
                    (6.0, 5.5), (3.5, 5.5), (3.309, 5.538), (3.146, 5.646), (3.038, 5.809), (3.0, 6.0), (3.0, 7.0), (3.038, 7.191), (3.146, 7.354),
                    (3.309, 7.462), (3.5, 7.5), (6.0, 7.5),
                ],
            ], &[
                1, 0, 11, 1, 11, 10, 1, 10, 9, 1, 9, 8, 1, 8, 7, 1, 7, 6, 1, 6, 5, 1, 5, 4, 1, 4, 3, 1, 3, 2,
            ]);
            pen.glyph(1.0, &[
                &[
                    (6.0, 8.5), (1.5, 8.5), (1.309, 8.538), (1.146, 8.646), (1.038, 8.809), (1.0, 9.0), (1.0, 10.0), (1.038, 10.191), (1.146, 10.354),
                    (1.309, 10.462), (1.5, 10.5), (6.0, 10.5),
                ],
            ], &[
                1, 0, 11, 1, 11, 10, 1, 10, 9, 1, 9, 8, 1, 8, 7, 1, 7, 6, 1, 6, 5, 1, 5, 4, 1, 4, 3, 1, 3, 2,
            ]);
            pen.glyph(1.0, &[
                &[
                    (6.0, 11.5), (3.0, 11.5), (2.809, 11.538), (2.646, 11.646), (2.538, 11.809), (2.5, 12.0), (2.5, 13.0), (2.538, 13.191), (2.646, 13.354),
                    (2.809, 13.462), (3.0, 13.5), (6.0, 13.5),
                ],
            ], &[
                1, 0, 11, 1, 11, 10, 1, 10, 9, 1, 9, 8, 1, 8, 7, 1, 7, 6, 1, 6, 5, 1, 5, 4, 1, 4, 3, 1, 3, 2,
            ]);
            pen.glyph(1.0, &[
                &[
                    (9.0, 11.5), (9.0, 13.5), (14.5, 13.5), (14.691, 13.462), (14.854, 13.354), (14.962, 13.191), (15.0, 13.0), (15.0, 12.0), (14.962, 11.809),
                    (14.854, 11.646), (14.691, 11.538), (14.5, 11.5),
                ],
            ], &[
                1, 0, 11, 1, 11, 10, 1, 10, 9, 1, 9, 8, 1, 8, 7, 1, 7, 6, 1, 6, 5, 1, 5, 4, 1, 4, 3, 1, 3, 2,
            ]);
            pen.glyph(1.0, &[
                &[
                    (9.0, 8.5), (9.0, 10.5), (11.5, 10.5), (11.691, 10.462), (11.854, 10.354), (11.962, 10.191), (12.0, 10.0), (12.0, 9.0), (11.962, 8.809),
                    (11.854, 8.646), (11.691, 8.538), (11.5, 8.5),
                ],
            ], &[
                1, 0, 11, 1, 11, 10, 1, 10, 9, 1, 9, 8, 1, 8, 7, 1, 7, 6, 1, 6, 5, 1, 5, 4, 1, 4, 3, 1, 3, 2,
            ]);
            pen.glyph(1.0, &[
                &[
                    (9.0, 5.5), (9.0, 7.5), (14.5, 7.5), (14.691, 7.462), (14.854, 7.354), (14.962, 7.191), (15.0, 7.0), (15.0, 6.0), (14.962, 5.809),
                    (14.854, 5.646), (14.691, 5.538), (14.5, 5.5),
                ],
            ], &[
                1, 0, 11, 1, 11, 10, 1, 10, 9, 1, 9, 8, 1, 8, 7, 1, 7, 6, 1, 6, 5, 1, 5, 4, 1, 4, 3, 1, 3, 2,
            ]);
            pen.glyph(0.65, &[
                &[
                    (8.75, 1.0), (6.25, 1.0), (6.154, 1.019), (6.073, 1.073), (6.019, 1.154), (6.0, 1.25), (6.0, 3.5), (6.224, 3.521), (6.438, 3.593),
                    (6.629, 3.712), (6.788, 3.871), (6.907, 4.062), (6.979, 4.276), (7.0, 4.5), (7.0, 14.5), (7.038, 14.691), (7.146, 14.854), (7.309, 14.962),
                    (7.5, 15.0), (7.691, 14.962), (7.854, 14.854), (7.962, 14.691), (8.0, 14.5), (8.0, 4.5), (8.021, 4.276), (8.093, 4.062), (8.212, 3.871),
                    (8.371, 3.712), (8.562, 3.593), (8.776, 3.521), (9.0, 3.5), (9.0, 1.25), (8.981, 1.154), (8.927, 1.073), (8.846, 1.019),
                ],
                &[
                    (8.0, 4.5), (7.0, 4.5), (7.0, 2.5), (8.0, 2.5),
                ],
            ], &[
                38, 37, 5, 38, 5, 4, 38, 4, 3, 38, 3, 2, 38, 2, 1, 38, 1, 0, 38, 0, 34, 38, 34, 33, 38, 33, 32, 38, 32, 31, 38, 31, 30, 38, 30, 29, 38, 29, 28,
                38, 28, 27, 38, 27, 26, 38, 26, 25, 38, 25, 24, 23, 22, 21, 23, 21, 20, 23, 20, 19, 23, 19, 18, 23, 18, 17, 23, 17, 16, 23, 16, 15, 23, 15, 14,
                7, 6, 5, 7, 5, 37, 35, 38, 24, 8, 7, 37, 9, 8, 37, 10, 9, 37, 11, 10, 37, 12, 11, 37, 13, 12, 37, 14, 36, 23,
            ]);
        }
        Audio => {
            pen.glyph(1.0, &[
                &[
                    (1.0, 7.5), (3.292, 7.458), (3.615, 4.514), (3.684, 4.732), (3.872, 5.277), (4.15, 5.986), (4.488, 6.695), (4.86, 7.24), (5.234, 7.458),
                    (5.395, 7.45), (5.556, 7.448), (5.716, 7.45), (5.877, 7.458), (6.321, 7.152), (6.671, 6.389), (6.933, 5.396), (7.113, 4.403),
                    (7.217, 3.639), (7.25, 3.333), (7.323, 3.639), (7.54, 4.403), (7.896, 5.396), (8.387, 6.389), (9.008, 7.152), (9.754, 7.458), (9.81, 7.45),
                    (9.988, 7.45), (10.229, 7.457), (10.474, 7.466), (10.664, 7.474), (10.741, 7.477), (10.963, 4.075), (11.196, 4.778), (11.469, 5.534),
                    (11.787, 6.263), (12.155, 6.884), (12.579, 7.315), (13.063, 7.477), (13.216, 7.488), (13.577, 7.495), (14.042, 7.499), (14.504, 7.5),
                    (14.858, 7.5), (15.0, 7.5), (15.0, 8.5), (13.065, 8.518), (12.463, 8.648), (11.963, 9.002), (11.553, 9.529), (11.22, 10.179),
                    (10.953, 10.901), (10.741, 11.643), (10.741, 8.518), (9.754, 8.5), (9.401, 8.704), (8.898, 9.247), (8.319, 10.021), (7.74, 10.92),
                    (7.237, 11.837), (6.884, 12.667), (6.864, 12.353), (6.804, 11.58), (6.723, 10.578), (6.642, 9.577), (6.578, 8.808), (6.553, 8.5),
                    (5.467, 8.5), (5.151, 8.674), (4.771, 9.111), (4.38, 9.678), (4.034, 10.245), (3.786, 10.681), (3.691, 10.855), (3.292, 8.5), (1.0, 8.5),
                ],
            ], &[
                73, 74, 0, 73, 0, 1, 73, 1, 2, 73, 2, 3, 73, 3, 4, 73, 4, 5, 73, 5, 6, 73, 6, 7, 73, 7, 8, 73, 8, 9, 73, 9, 10, 73, 10, 11, 73, 11, 12, 17, 18,
                19, 17, 19, 20, 17, 20, 21, 24, 25, 26, 24, 26, 27, 24, 27, 28, 24, 28, 29, 24, 29, 30, 30, 31, 32, 30, 32, 33, 30, 33, 34, 30, 34, 35, 30, 35,
                36, 30, 36, 37, 30, 37, 38, 30, 38, 39, 30, 39, 40, 42, 43, 44, 42, 44, 45, 42, 45, 46, 50, 51, 52, 58, 59, 60, 58, 60, 61, 58, 61, 62, 58, 62,
                63, 58, 63, 64, 58, 64, 65, 71, 72, 73, 16, 17, 21, 16, 21, 22, 41, 42, 46, 49, 50, 52, 57, 58, 65, 70, 71, 73, 15, 16, 22, 15, 22, 23, 40, 41,
                46, 40, 46, 47, 48, 49, 52, 56, 57, 65, 69, 70, 73, 69, 73, 12, 14, 15, 23, 14, 23, 24, 30, 40, 47, 30, 47, 48, 30, 48, 52, 30, 52, 53, 30, 53,
                54, 55, 56, 65, 68, 69, 12, 68, 12, 13, 13, 14, 24, 24, 30, 54, 24, 54, 55, 24, 55, 65, 24, 65, 66, 67, 68, 13, 13, 24, 66, 13, 66, 67,
            ]);
            pen.glyph(1.0, &[
                &[
                    (15.334, 2.0), (0.666, 2.0), (0.461, 2.033), (0.275, 2.127), (0.127, 2.275), (0.033, 2.461), (0.0, 2.667), (0.0, 13.334), (0.033, 13.539),
                    (0.127, 13.725), (0.275, 13.873), (0.461, 13.967), (0.666, 14.0), (15.333, 14.0), (15.539, 13.967), (15.725, 13.873), (15.873, 13.725),
                    (15.967, 13.539), (16.0, 13.334), (16.0, 2.667), (15.967, 2.461), (15.873, 2.275), (15.725, 2.127), (15.539, 2.033),
                ],
                &[
                    (15.0, 13.0), (1.0, 13.0), (1.0, 3.0), (15.0, 3.0),
                ],
            ], &[
                6, 26, 25, 27, 26, 6, 27, 6, 5, 27, 5, 4, 27, 4, 3, 27, 3, 2, 27, 2, 1, 27, 1, 0, 27, 0, 23, 27, 23, 22, 27, 22, 21, 27, 21, 20, 27, 20, 19,
                27, 19, 18, 27, 18, 17, 27, 17, 16, 27, 16, 15, 27, 15, 14, 27, 14, 13, 13, 12, 11, 13, 11, 10, 13, 10, 9, 13, 9, 8, 13, 8, 7, 7, 6, 25, 7, 25,
                24, 24, 27, 13, 24, 13, 7,
            ]);
        }
        Image => {
            pen.glyph(1.0, &[
                &[
                    (14.667, 2.667), (1.333, 2.667), (1.163, 2.7), (1.019, 2.797), (0.923, 2.941), (0.889, 3.111), (0.889, 12.889), (0.923, 13.059),
                    (1.019, 13.203), (1.163, 13.3), (1.333, 13.333), (14.667, 13.333), (14.837, 13.3), (14.981, 13.203), (15.077, 13.059), (15.111, 12.889),
                    (15.111, 3.111), (15.077, 2.941), (14.981, 2.797), (14.837, 2.7),
                ],
                &[
                    (14.222, 11.277), (11.879, 8.933), (11.724, 8.809), (11.544, 8.723), (11.35, 8.679), (11.151, 8.679), (10.957, 8.723), (10.778, 8.809),
                    (10.622, 8.933), (9.257, 10.299), (5.899, 6.94), (5.743, 6.816), (5.564, 6.73), (5.37, 6.685), (5.171, 6.685), (4.977, 6.73),
                    (4.797, 6.816), (4.642, 6.94), (1.778, 9.804), (1.778, 3.556), (14.222, 3.556),
                ],
            ], &[
                5, 39, 38, 38, 37, 36, 38, 36, 35, 38, 35, 34, 38, 34, 33, 38, 33, 32, 38, 32, 31, 38, 31, 30, 38, 30, 29, 29, 28, 27, 29, 27, 26, 29, 26, 25,
                29, 25, 24, 29, 24, 23, 29, 23, 22, 29, 22, 21, 29, 21, 20, 40, 39, 5, 40, 5, 4, 40, 4, 3, 40, 3, 2, 40, 2, 1, 40, 1, 0, 40, 0, 19, 40, 19, 18,
                40, 18, 17, 40, 17, 16, 40, 16, 15, 40, 15, 14, 40, 14, 13, 40, 13, 12, 40, 12, 11, 11, 10, 9, 11, 9, 8, 11, 8, 7, 11, 7, 6, 6, 5, 38, 6, 38,
                29, 6, 29, 20, 20, 40, 11, 20, 11, 6,
            ]);
            pen.glyph(1.0, &[
                &[
                    (13.067, 5.911), (13.052, 6.099), (13.008, 6.282), (12.936, 6.456), (12.837, 6.616), (12.715, 6.76), (12.572, 6.882), (12.411, 6.98),
                    (12.237, 7.052), (12.054, 7.096), (11.867, 7.111), (11.679, 7.096), (11.496, 7.052), (11.322, 6.98), (11.161, 6.882), (11.018, 6.76),
                    (10.896, 6.616), (10.797, 6.456), (10.725, 6.282), (10.681, 6.099), (10.667, 5.911), (10.681, 5.723), (10.725, 5.54), (10.797, 5.366),
                    (10.896, 5.206), (11.018, 5.063), (11.161, 4.94), (11.322, 4.842), (11.496, 4.77), (11.679, 4.726), (11.867, 4.711), (12.054, 4.726),
                    (12.237, 4.77), (12.411, 4.842), (12.572, 4.94), (12.715, 5.063), (12.837, 5.206), (12.936, 5.366), (13.008, 5.54), (13.052, 5.723),
                ],
            ], &[
                38, 39, 0, 38, 0, 1, 38, 1, 2, 38, 2, 3, 38, 3, 4, 38, 4, 5, 38, 5, 6, 38, 6, 7, 38, 7, 8, 38, 8, 9, 38, 9, 10, 38, 10, 11, 38, 11, 12, 38, 12,
                13, 38, 13, 14, 38, 14, 15, 38, 15, 16, 38, 16, 17, 38, 17, 18, 38, 18, 19, 38, 19, 20, 38, 20, 21, 38, 21, 22, 38, 22, 23, 38, 23, 24, 38, 24,
                25, 38, 25, 26, 38, 26, 27, 38, 27, 28, 38, 28, 29, 38, 29, 30, 38, 30, 31, 38, 31, 32, 38, 32, 33, 38, 33, 34, 38, 34, 35, 38, 35, 36, 38, 36,
                37,
            ]);
        }
        Search => pen.glyph(1.0, &[
            &[
                (9.5, 2.0), (9.097, 2.018), (8.697, 2.073), (8.303, 2.163), (7.919, 2.288), (7.548, 2.446), (7.192, 2.638), (6.855, 2.86), (6.54, 3.112),
                (6.248, 3.391), (5.982, 3.696), (5.745, 4.022), (5.538, 4.369), (5.363, 4.733), (5.221, 5.111), (5.114, 5.5), (5.042, 5.898), (5.006, 6.3),
                (5.006, 6.704), (5.043, 7.106), (5.115, 7.503), (5.222, 7.892), (5.364, 8.27), (5.54, 8.634), (5.747, 8.98), (2.031, 12.742), (1.888, 12.929),
                (1.799, 13.146), (1.768, 13.379), (1.799, 13.612), (1.888, 13.829), (2.032, 14.015), (2.218, 14.159), (2.435, 14.248), (2.668, 14.279),
                (2.901, 14.248), (3.118, 14.159), (3.304, 14.015), (7.02, 10.253), (7.63, 10.592), (8.286, 10.832), (8.97, 10.968), (9.668, 10.997),
                (10.361, 10.917), (11.034, 10.731), (11.67, 10.444), (12.254, 10.061), (12.772, 9.593), (13.211, 9.05), (13.56, 8.446), (13.813, 7.795),
                (13.961, 7.113), (14.002, 6.416), (13.935, 5.721), (13.761, 5.045), (13.485, 4.404), (13.113, 3.813), (12.654, 3.287), (12.119, 2.839),
                (11.522, 2.478), (10.875, 2.214), (10.196, 2.054),
            ],
            &[
                (9.5, 10.1), (8.798, 10.031), (8.122, 9.826), (7.5, 9.493), (6.954, 9.046), (6.507, 8.5), (6.174, 7.878), (5.969, 7.202), (5.9, 6.5),
                (5.969, 5.798), (6.174, 5.122), (6.507, 4.5), (6.954, 3.954), (7.5, 3.507), (8.122, 3.174), (8.798, 2.969), (9.5, 2.9), (10.202, 2.969),
                (10.878, 3.174), (11.5, 3.507), (12.046, 3.954), (12.493, 4.5), (12.826, 5.122), (13.031, 5.798), (13.1, 6.5), (13.091, 6.746), (13.066, 6.99),
                (13.024, 7.232), (12.965, 7.471), (12.891, 7.705), (12.801, 7.933), (12.695, 8.155), (12.574, 8.369), (12.44, 8.575), (12.291, 8.771),
                (12.13, 8.956), (11.956, 9.13), (11.771, 9.291), (11.575, 9.44), (11.369, 9.574), (11.155, 9.695), (10.933, 9.801), (10.705, 9.891),
                (10.471, 9.965), (10.232, 10.024), (9.99, 10.066), (9.746, 10.091),
            ],
        ], &[
            17, 70, 69, 17, 69, 68, 17, 68, 67, 71, 70, 17, 71, 17, 16, 71, 16, 15, 71, 15, 14, 71, 14, 13, 71, 13, 12, 71, 12, 11, 71, 11, 10, 71, 10, 9, 71,
            9, 8, 71, 8, 7, 7, 6, 5, 7, 5, 4, 7, 4, 3, 7, 3, 2, 7, 2, 1, 7, 1, 0, 7, 0, 61, 7, 61, 60, 7, 60, 59, 7, 59, 58, 58, 57, 56, 58, 56, 55, 58, 55,
            54, 58, 54, 53, 58, 53, 52, 58, 52, 51, 58, 51, 50, 50, 49, 48, 50, 48, 47, 50, 47, 46, 50, 46, 45, 50, 45, 44, 50, 44, 43, 50, 43, 42, 42, 41, 40,
            42, 40, 39, 42, 39, 38, 38, 37, 36, 38, 36, 35, 38, 35, 34, 38, 34, 33, 38, 33, 32, 38, 32, 31, 38, 31, 30, 38, 30, 29, 38, 29, 28, 38, 28, 27, 38,
            27, 26, 38, 26, 25, 38, 25, 24, 38, 24, 23, 38, 23, 22, 38, 22, 21, 38, 21, 20, 38, 20, 19, 38, 19, 18, 38, 18, 17, 38, 17, 67, 38, 67, 66, 38, 66,
            65, 38, 65, 64, 38, 64, 63, 38, 63, 62, 38, 62, 108, 72, 71, 7, 42, 38, 108, 42, 108, 107, 42, 107, 106, 42, 106, 105, 42, 105, 104, 42, 104, 103,
            42, 103, 102, 42, 102, 101, 42, 101, 100, 42, 100, 99, 73, 72, 7, 50, 42, 99, 50, 99, 98, 50, 98, 97, 50, 97, 96, 50, 96, 95, 50, 95, 94, 50, 94,
            93, 50, 93, 92, 50, 92, 91, 50, 91, 90, 50, 90, 89, 50, 89, 88, 50, 88, 87, 50, 87, 86, 50, 86, 85, 50, 85, 84, 74, 73, 7, 58, 50, 84, 58, 84, 83,
            58, 83, 82, 58, 82, 81, 58, 81, 80, 58, 80, 79, 58, 79, 78, 75, 74, 7, 7, 58, 78, 7, 78, 77, 7, 77, 76, 7, 76, 75,
        ]),
        ListView => {
            pen.glyph(1.0, &[
                &[
                    (1.455, 3.636), (1.455, 6.545), (1.482, 6.685), (1.561, 6.803), (1.679, 6.881), (1.818, 6.909), (4.727, 6.909), (4.866, 6.881),
                    (4.984, 6.803), (5.063, 6.685), (5.091, 6.545), (5.091, 3.636), (5.063, 3.497), (4.984, 3.379), (4.866, 3.3), (4.727, 3.273),
                    (1.818, 3.273), (1.679, 3.3), (1.561, 3.379), (1.482, 3.497),
                ],
                &[
                    (2.545, 4.364), (4.0, 4.364), (4.0, 5.818), (2.545, 5.818),
                ],
            ], &[
                0, 20, 23, 21, 20, 0, 21, 0, 19, 21, 19, 18, 21, 18, 17, 21, 17, 16, 21, 16, 15, 21, 15, 14, 21, 14, 13, 21, 13, 12, 21, 12, 11, 21, 11, 10,
                21, 10, 9, 21, 9, 8, 21, 8, 7, 21, 7, 6, 6, 5, 4, 6, 4, 3, 6, 3, 2, 6, 2, 1, 1, 0, 23, 1, 23, 22, 22, 21, 6, 22, 6, 1,
            ]);
            pen.glyph(1.0, &[
                &[
                    (1.455, 8.727), (1.455, 11.636), (1.482, 11.776), (1.561, 11.893), (1.679, 11.972), (1.818, 12.0), (4.727, 12.0), (4.866, 11.972),
                    (4.984, 11.893), (5.063, 11.776), (5.091, 11.636), (5.091, 8.727), (5.063, 8.588), (4.984, 8.47), (4.866, 8.391), (4.727, 8.364),
                    (1.818, 8.364), (1.679, 8.391), (1.561, 8.47), (1.482, 8.588),
                ],
                &[
                    (2.545, 9.455), (4.0, 9.455), (4.0, 10.909), (2.545, 10.909),
                ],
            ], &[
                0, 20, 23, 21, 20, 0, 21, 0, 19, 21, 19, 18, 21, 18, 17, 21, 17, 16, 21, 16, 15, 21, 15, 14, 21, 14, 13, 21, 13, 12, 21, 12, 11, 21, 11, 10,
                21, 10, 9, 21, 9, 8, 21, 8, 7, 21, 7, 6, 6, 5, 4, 6, 4, 3, 6, 3, 2, 6, 2, 1, 1, 0, 23, 1, 23, 22, 22, 21, 6, 22, 6, 1,
            ]);
            pen.glyph(1.0, &[
                &[
                    (14.364, 10.909), (14.433, 10.923), (14.492, 10.962), (14.532, 11.021), (14.545, 11.091), (14.545, 11.818), (14.532, 11.888),
                    (14.492, 11.947), (14.433, 11.986), (14.364, 12.0), (6.727, 12.0), (6.658, 11.986), (6.599, 11.947), (6.559, 11.888), (6.545, 11.818),
                    (6.545, 11.091), (6.559, 11.021), (6.599, 10.962), (6.658, 10.923), (6.727, 10.909),
                ],
            ], &[
                18, 19, 0, 18, 0, 1, 18, 1, 2, 18, 2, 3, 18, 3, 4, 18, 4, 5, 18, 5, 6, 18, 6, 7, 18, 7, 8, 18, 8, 9, 18, 9, 10, 18, 10, 11, 18, 11, 12, 18, 12,
                13, 18, 13, 14, 18, 14, 15, 18, 15, 16, 18, 16, 17,
            ]);
            pen.glyph(1.0, &[
                &[
                    (14.364, 8.364), (14.433, 8.377), (14.492, 8.417), (14.532, 8.476), (14.545, 8.545), (14.545, 9.273), (14.532, 9.342), (14.492, 9.401),
                    (14.433, 9.441), (14.364, 9.455), (6.727, 9.455), (6.658, 9.441), (6.599, 9.401), (6.559, 9.342), (6.545, 9.273), (6.545, 8.545),
                    (6.559, 8.476), (6.599, 8.417), (6.658, 8.377), (6.727, 8.364),
                ],
            ], &[
                18, 19, 0, 18, 0, 1, 18, 1, 2, 18, 2, 3, 18, 3, 4, 18, 4, 5, 18, 5, 6, 18, 6, 7, 18, 7, 8, 18, 8, 9, 18, 9, 10, 18, 10, 11, 18, 11, 12, 18, 12,
                13, 18, 13, 14, 18, 14, 15, 18, 15, 16, 18, 16, 17,
            ]);
            pen.glyph(1.0, &[
                &[
                    (14.364, 5.818), (14.433, 5.832), (14.492, 5.871), (14.532, 5.93), (14.545, 6.0), (14.545, 6.727), (14.532, 6.797), (14.492, 6.856),
                    (14.433, 6.895), (14.364, 6.909), (6.727, 6.909), (6.658, 6.895), (6.599, 6.856), (6.559, 6.797), (6.545, 6.727), (6.545, 6.0),
                    (6.559, 5.93), (6.599, 5.871), (6.658, 5.832), (6.727, 5.818),
                ],
            ], &[
                18, 19, 0, 18, 0, 1, 18, 1, 2, 18, 2, 3, 18, 3, 4, 18, 4, 5, 18, 5, 6, 18, 6, 7, 18, 7, 8, 18, 8, 9, 18, 9, 10, 18, 10, 11, 18, 11, 12, 18, 12,
                13, 18, 13, 14, 18, 14, 15, 18, 15, 16, 18, 16, 17,
            ]);
            pen.glyph(1.0, &[
                &[
                    (14.364, 3.273), (14.433, 3.287), (14.492, 3.326), (14.532, 3.385), (14.545, 3.455), (14.545, 4.182), (14.532, 4.251), (14.492, 4.31),
                    (14.433, 4.35), (14.364, 4.364), (6.727, 4.364), (6.658, 4.35), (6.599, 4.31), (6.559, 4.251), (6.545, 4.182), (6.545, 3.455),
                    (6.559, 3.385), (6.599, 3.326), (6.658, 3.287), (6.727, 3.273),
                ],
            ], &[
                18, 19, 0, 18, 0, 1, 18, 1, 2, 18, 2, 3, 18, 3, 4, 18, 4, 5, 18, 5, 6, 18, 6, 7, 18, 7, 8, 18, 8, 9, 18, 9, 10, 18, 10, 11, 18, 11, 12, 18, 12,
                13, 18, 13, 14, 18, 14, 15, 18, 15, 16, 18, 16, 17,
            ]);
        }
        IconView => {
            pen.glyph(1.0, &[
                &[
                    (2.909, 12.182), (2.909, 12.909), (2.923, 12.979), (2.962, 13.038), (3.021, 13.077), (3.091, 13.091), (12.909, 13.091), (12.979, 13.077),
                    (13.038, 13.038), (13.077, 12.979), (13.091, 12.909), (13.091, 12.182), (13.077, 12.112), (13.038, 12.053), (12.979, 12.014),
                    (12.909, 12.0), (3.091, 12.0), (3.021, 12.014), (2.962, 12.053), (2.923, 12.112),
                ],
            ], &[
                1, 0, 19, 1, 19, 18, 1, 18, 17, 1, 17, 16, 1, 16, 15, 1, 15, 14, 1, 14, 13, 1, 13, 12, 1, 12, 11, 1, 11, 10, 1, 10, 9, 1, 9, 8, 1, 8, 7, 1, 7,
                6, 1, 6, 5, 1, 5, 4, 1, 4, 3, 1, 3, 2,
            ]);
            pen.glyph(1.0, &[
                &[
                    (2.909, 3.273), (2.909, 9.818), (2.937, 9.957), (3.016, 10.075), (3.134, 10.154), (3.273, 10.182), (12.727, 10.182), (12.866, 10.154),
                    (12.984, 10.075), (13.063, 9.957), (13.091, 9.818), (13.091, 3.273), (13.063, 3.134), (12.984, 3.016), (12.866, 2.937), (12.727, 2.909),
                    (3.273, 2.909), (3.134, 2.937), (3.016, 3.016), (2.937, 3.134),
                ],
            ], &[
                1, 0, 19, 1, 19, 18, 1, 18, 17, 1, 17, 16, 1, 16, 15, 1, 15, 14, 1, 14, 13, 1, 13, 12, 1, 12, 11, 1, 11, 10, 1, 10, 9, 1, 9, 8, 1, 8, 7, 1, 7,
                6, 1, 6, 5, 1, 5, 4, 1, 4, 3, 1, 3, 2,
            ]);
        }
        Freeform => {
            pen.rect(2.0, 3.0, 8.0, 8.0);
            pen.rect(7.0, 9.0, 14.0, 13.5);
            pen.rect(10.0, 2.0, 14.0, 6.0);
        }
        NewItem => {
            pen.glyph(1.0, &[
                &[
                    (8.0, 9.818), (8.028, 9.679), (8.107, 9.561), (8.224, 9.482), (8.364, 9.455), (12.727, 9.455), (12.727, 2.182), (12.7, 2.043),
                    (12.621, 1.925), (12.503, 1.846), (12.364, 1.818), (3.273, 1.818), (3.134, 1.846), (3.016, 1.925), (2.937, 2.043), (2.909, 2.182),
                    (2.909, 13.818), (2.937, 13.957), (3.016, 14.075), (3.134, 14.154), (3.273, 14.182), (8.0, 14.182),
                ],
            ], &[
                0, 21, 20, 0, 20, 19, 0, 19, 18, 0, 18, 17, 0, 17, 16, 0, 16, 15, 0, 15, 14, 0, 14, 13, 0, 13, 12, 0, 12, 11, 11, 10, 9, 11, 9, 8, 11, 8, 7,
                11, 7, 6, 11, 6, 5, 11, 5, 4, 11, 4, 3, 11, 3, 2, 11, 2, 1, 11, 1, 0,
            ]);
            pen.glyph(1.0, &[
                &[
                    (9.091, 14.182), (9.091, 10.545), (12.727, 10.545),
                ],
            ], &[
                1, 2, 0,
            ]);
        }
        Trash => pen.glyph(1.0, &[
            &[
                (13.455, 3.273), (10.909, 3.273), (10.909, 2.182), (10.891, 2.02), (10.837, 1.866), (10.75, 1.728), (10.635, 1.613), (10.497, 1.527),
                (10.344, 1.473), (10.182, 1.455), (5.455, 1.455), (5.293, 1.473), (5.139, 1.527), (5.001, 1.613), (4.886, 1.728), (4.799, 1.866),
                (4.746, 2.02), (4.727, 2.182), (4.727, 3.273), (2.182, 3.273), (2.043, 3.3), (1.925, 3.379), (1.846, 3.497), (1.818, 3.636), (1.818, 4.364),
                (1.846, 4.503), (1.925, 4.621), (2.043, 4.7), (2.182, 4.727), (2.727, 4.727), (3.605, 14.201), (3.642, 14.329), (3.723, 14.435),
                (3.837, 14.505), (3.968, 14.529), (11.67, 14.529), (11.801, 14.505), (11.915, 14.435), (11.996, 14.329), (12.033, 14.201), (12.909, 4.727),
                (13.455, 4.727), (13.594, 4.7), (13.712, 4.621), (13.791, 4.503), (13.818, 4.364), (13.818, 3.636), (13.791, 3.497), (13.712, 3.379),
                (13.594, 3.3),
            ],
            &[
                (5.818, 2.545), (9.818, 2.545), (9.818, 3.273), (5.818, 3.273),
            ],
            &[
                (5.709, 12.727), (5.352, 12.727), (5.219, 12.703), (5.105, 12.632), (5.024, 12.524), (4.988, 12.394), (4.475, 6.241), (4.49, 6.1),
                (4.558, 5.975), (4.669, 5.886), (4.805, 5.847), (5.169, 5.815), (5.31, 5.83), (5.434, 5.898), (5.523, 6.009), (5.563, 6.145), (6.072, 12.335),
                (6.053, 12.482), (5.977, 12.61), (5.855, 12.697),
            ],
            &[
                (8.364, 12.364), (8.336, 12.503), (8.257, 12.621), (8.139, 12.7), (8.0, 12.727), (7.636, 12.727), (7.497, 12.7), (7.379, 12.621),
                (7.3, 12.503), (7.273, 12.364), (7.273, 6.182), (7.3, 6.043), (7.379, 5.925), (7.497, 5.846), (7.636, 5.818), (8.0, 5.818), (8.139, 5.846),
                (8.257, 5.925), (8.336, 6.043), (8.364, 6.182),
            ],
            &[
                (10.648, 12.395), (10.612, 12.524), (10.531, 12.632), (10.417, 12.703), (10.284, 12.728), (9.927, 12.728), (9.781, 12.698), (9.659, 12.611),
                (9.582, 12.483), (9.564, 12.335), (10.073, 6.145), (10.112, 6.009), (10.201, 5.898), (10.326, 5.83), (10.467, 5.815), (10.831, 5.847),
                (10.967, 5.886), (11.077, 5.975), (11.145, 6.1), (11.161, 6.241),
            ],
        ], &[
            93, 103, 102, 104, 103, 93, 104, 93, 92, 104, 92, 91, 104, 91, 90, 104, 90, 89, 85, 84, 69, 85, 69, 68, 85, 68, 67, 85, 67, 66, 61, 60, 29, 29, 28,
            27, 29, 27, 26, 29, 26, 25, 29, 25, 24, 29, 24, 23, 29, 23, 22, 29, 22, 21, 29, 21, 20, 29, 20, 19, 29, 19, 18, 18, 17, 50, 18, 50, 53, 51, 50, 17,
            51, 17, 16, 51, 16, 15, 51, 15, 14, 51, 14, 13, 51, 13, 12, 51, 12, 11, 51, 11, 10, 51, 10, 9, 51, 9, 8, 51, 8, 7, 51, 7, 6, 51, 6, 5, 51, 5, 4,
            51, 4, 3, 51, 3, 2, 51, 2, 1, 1, 0, 49, 1, 49, 48, 1, 48, 47, 1, 47, 46, 1, 46, 45, 1, 45, 44, 1, 44, 43, 1, 43, 42, 1, 42, 41, 1, 41, 40, 1, 40,
            39, 1, 39, 38, 1, 38, 37, 37, 36, 35, 37, 35, 34, 37, 34, 33, 37, 33, 32, 37, 32, 31, 37, 31, 30, 30, 29, 60, 30, 60, 59, 30, 59, 58, 30, 58, 57,
            30, 57, 56, 30, 56, 55, 30, 55, 54, 30, 54, 73, 70, 69, 84, 70, 84, 83, 70, 83, 82, 70, 82, 81, 70, 81, 80, 74, 93, 102, 74, 102, 101, 74, 101,
            100, 105, 104, 89, 86, 85, 66, 86, 66, 65, 62, 61, 29, 62, 29, 18, 62, 18, 53, 52, 51, 1, 37, 30, 73, 37, 73, 72, 37, 72, 71, 37, 71, 70, 37, 70,
            80, 37, 80, 79, 37, 79, 78, 37, 78, 77, 37, 77, 76, 37, 76, 75, 37, 75, 74, 37, 74, 100, 37, 100, 99, 37, 99, 98, 37, 98, 97, 37, 97, 96, 37, 96,
            95, 37, 95, 94, 37, 94, 113, 106, 105, 89, 87, 86, 65, 63, 62, 53, 63, 53, 52, 63, 52, 1, 1, 37, 113, 1, 113, 112, 1, 112, 111, 1, 111, 110, 1,
            110, 109, 1, 109, 108, 1, 108, 107, 1, 107, 106, 1, 106, 89, 1, 89, 88, 1, 88, 87, 1, 87, 65, 1, 65, 64, 1, 64, 63,
        ]),
        Home => pen.glyph(1.0, &[
            &[
                (15.703, 9.0), (8.333, 1.63), (8.18, 1.528), (8.0, 1.492), (7.82, 1.528), (7.667, 1.63), (0.297, 9.0), (0.195, 9.153), (0.159, 9.333),
                (0.195, 9.514), (0.297, 9.667), (1.167, 10.537), (1.234, 10.592), (1.311, 10.633), (1.395, 10.658), (1.481, 10.667), (1.778, 10.667),
                (1.778, 14.667), (1.812, 14.837), (1.908, 14.981), (2.052, 15.077), (2.222, 15.111), (5.778, 15.111), (5.948, 15.077), (6.092, 14.981),
                (6.188, 14.837), (6.222, 14.667), (6.222, 10.222), (6.256, 10.052), (6.352, 9.908), (6.497, 9.812), (6.667, 9.778), (9.333, 9.778),
                (9.503, 9.812), (9.648, 9.908), (9.744, 10.052), (9.778, 10.222), (9.778, 14.667), (9.812, 14.837), (9.908, 14.981), (10.052, 15.077),
                (10.222, 15.111), (13.778, 15.111), (13.948, 15.077), (14.092, 14.981), (14.188, 14.837), (14.222, 14.667), (14.222, 10.667), (14.519, 10.667),
                (14.605, 10.658), (14.689, 10.633), (14.766, 10.592), (14.833, 10.536), (15.703, 9.666), (15.805, 9.513), (15.841, 9.333), (15.805, 9.153),
            ],
        ], &[
            1, 0, 56, 1, 56, 55, 1, 55, 54, 1, 54, 53, 1, 53, 52, 1, 52, 51, 1, 51, 50, 1, 50, 49, 1, 49, 48, 1, 48, 47, 1, 47, 46, 1, 46, 45, 1, 45, 44, 1,
            44, 43, 1, 43, 42, 42, 41, 40, 42, 40, 39, 42, 39, 38, 42, 38, 37, 42, 37, 36, 42, 36, 35, 42, 35, 34, 27, 26, 25, 27, 25, 24, 27, 24, 23, 27, 23,
            22, 27, 22, 21, 27, 21, 20, 27, 20, 19, 27, 19, 18, 27, 18, 17, 27, 17, 16, 27, 16, 15, 27, 15, 14, 27, 14, 13, 27, 13, 12, 27, 12, 11, 27, 11, 10,
            27, 10, 9, 27, 9, 8, 27, 8, 7, 27, 7, 6, 27, 6, 5, 27, 5, 4, 4, 3, 2, 4, 2, 1, 4, 1, 42, 4, 42, 34, 4, 34, 33, 4, 33, 32, 4, 32, 31, 4, 31, 30, 4,
            30, 29, 4, 29, 28, 4, 28, 27,
        ]),
        Workspaces => {
            pen.glyph(1.0, &[
                &[
                    (1.333, 2.667), (1.163, 2.7), (1.019, 2.797), (0.923, 2.941), (0.889, 3.111), (0.889, 13.778), (0.923, 13.948), (1.019, 14.092),
                    (1.163, 14.188), (1.333, 14.222), (14.667, 14.222), (14.837, 14.188), (14.981, 14.092), (15.077, 13.948), (15.111, 13.778),
                    (15.111, 3.111), (15.077, 2.941), (14.981, 2.797), (14.837, 2.7), (14.667, 2.667),
                ],
                &[
                    (14.222, 4.444), (14.222, 13.333), (1.778, 13.333), (1.778, 4.444),
                ],
            ], &[
                4, 23, 22, 20, 23, 4, 20, 4, 3, 20, 3, 2, 20, 2, 1, 20, 1, 0, 20, 0, 19, 20, 19, 18, 20, 18, 17, 20, 17, 16, 20, 16, 15, 20, 15, 14, 20, 14,
                13, 20, 13, 12, 20, 12, 11, 20, 11, 10, 10, 9, 8, 10, 8, 7, 10, 7, 6, 10, 6, 5, 5, 4, 22, 5, 22, 21, 21, 20, 10, 21, 10, 5,
            ]);
            pen.glyph(1.0, &[
                &[
                    (3.555, 4.7108), (3.215, 4.7516), (2.927, 4.868), (2.733, 5.0408), (2.667, 5.2448), (2.667, 11.6444), (2.733, 11.8484), (2.927, 12.0212),
                    (3.215, 12.1376), (3.555, 12.1784), (7.111, 12.1784), (7.451, 12.1376), (7.739, 12.0212), (7.933, 11.8484), (7.999, 11.6444),
                    (7.999, 5.2448), (7.933, 5.0408), (7.739, 4.868), (7.451, 4.7516), (7.111, 4.7108),
                ],
                &[
                    (4.445, 5.7776), (6.221, 5.7776), (6.221, 6.8444), (4.445, 6.8444),
                ],
                &[
                    (4.445, 7.9112), (6.221, 7.9112), (6.221, 8.978), (4.445, 8.978),
                ],
                &[
                    (4.445, 10.0448), (6.221, 10.0448), (6.221, 11.1116), (4.445, 11.1116),
                ],
            ], &[
                4, 28, 31, 4, 24, 27, 4, 20, 23, 21, 20, 4, 21, 4, 3, 21, 3, 2, 21, 2, 1, 21, 1, 0, 21, 0, 19, 21, 19, 18, 21, 18, 17, 21, 17, 16, 21, 16, 15,
                21, 15, 14, 21, 14, 13, 21, 13, 12, 21, 12, 11, 21, 11, 10, 10, 9, 8, 10, 8, 7, 10, 7, 6, 10, 6, 5, 5, 4, 31, 5, 31, 30, 28, 4, 27, 28, 27, 26,
                24, 4, 23, 24, 23, 22, 22, 21, 10, 10, 5, 30, 10, 30, 29, 29, 28, 26, 25, 24, 22, 25, 22, 10, 10, 29, 26, 10, 26, 25,
            ]);
        }
        Hamburger => {
            for y in [4.5, 8.0, 11.5] {
                pen.line(&[(3.0, y), (13.0, y)]);
            }
        }
        ChevronDown => pen.glyph(1.0, &[
            &[
                (16.0, 4.8), (15.943, 4.376), (15.781, 3.994), (15.53, 3.67), (15.206, 3.419), (14.824, 3.257), (14.4, 3.2), (14.2, 3.213), (14.003, 3.253),
                (13.81, 3.32), (13.624, 3.413), (13.446, 3.533), (13.28, 3.68), (8.0, 8.96), (2.72, 3.68), (2.376, 3.413), (1.997, 3.253), (1.6, 3.2),
                (1.203, 3.253), (0.824, 3.413), (0.48, 3.68), (0.213, 4.024), (0.053, 4.403), (0.0, 4.8), (0.053, 5.197), (0.213, 5.576), (0.48, 5.92),
                (6.88, 12.32), (7.224, 12.587), (7.603, 12.747), (8.0, 12.8), (8.397, 12.747), (8.776, 12.587), (9.12, 12.32), (15.52, 5.92), (15.667, 5.787),
                (15.787, 5.63), (15.88, 5.45), (15.947, 5.25), (15.987, 5.033),
            ],
        ], &[
            1, 0, 39, 1, 39, 38, 1, 38, 37, 1, 37, 36, 1, 36, 35, 1, 35, 34, 1, 34, 33, 1, 33, 32, 1, 32, 31, 1, 31, 30, 1, 30, 29, 1, 29, 28, 1, 28, 27, 27,
            26, 25, 27, 25, 24, 27, 24, 23, 27, 23, 22, 27, 22, 21, 27, 21, 20, 27, 20, 19, 27, 19, 18, 27, 18, 17, 27, 17, 16, 27, 16, 15, 27, 15, 14, 27, 14,
            13, 27, 13, 12, 27, 12, 11, 27, 11, 10, 27, 10, 9, 27, 9, 8, 27, 8, 7, 27, 7, 6, 27, 6, 5, 27, 5, 4, 27, 4, 3, 27, 3, 2, 27, 2, 1,
        ]),
        ChevronRight => pen.glyph(1.0, &[
            &[
                (10.711, 8.089), (10.704, 7.978), (10.681, 7.868), (10.644, 7.761), (10.593, 7.658), (10.526, 7.559), (10.444, 7.467), (6.889, 3.911),
                (6.713, 3.756), (6.507, 3.651), (6.283, 3.6), (6.056, 3.608), (5.838, 3.681), (5.644, 3.822), (5.49, 3.998), (5.384, 4.204), (5.333, 4.428),
                (5.342, 4.655), (5.414, 4.873), (5.556, 5.067), (5.644, 5.156), (8.578, 8.089), (5.644, 11.022), (5.515, 11.216), (5.439, 11.434),
                (5.417, 11.661), (5.45, 11.885), (5.541, 12.091), (5.689, 12.267), (5.861, 12.396), (6.058, 12.474), (6.267, 12.5), (6.476, 12.474),
                (6.672, 12.396), (6.844, 12.267), (10.4, 8.711), (10.5, 8.619), (10.579, 8.52), (10.639, 8.417), (10.68, 8.309), (10.703, 8.2),
            ],
        ], &[
            1, 0, 40, 1, 40, 39, 1, 39, 38, 1, 38, 37, 1, 37, 36, 1, 36, 35, 1, 35, 34, 1, 34, 33, 1, 33, 32, 1, 32, 31, 1, 31, 30, 1, 30, 29, 1, 29, 28, 1,
            28, 27, 1, 27, 26, 1, 26, 25, 1, 25, 24, 1, 24, 23, 1, 23, 22, 1, 22, 21, 1, 21, 20, 1, 20, 19, 1, 19, 18, 1, 18, 17, 1, 17, 16, 1, 16, 15, 1, 15,
            14, 1, 14, 13, 1, 13, 12, 1, 12, 11, 1, 11, 10, 1, 10, 9, 1, 9, 8, 1, 8, 7, 1, 7, 6, 1, 6, 5, 1, 5, 4, 1, 4, 3, 1, 3, 2,
        ]),
        Magnet => {
            pen.glyph(1.0, &[
                &[
                    (3.111, 7.556), (3.111, 10.222), (5.778, 10.222), (5.778, 7.556), (5.788, 7.338), (5.82, 7.122), (5.873, 6.91), (5.947, 6.705),
                    (6.04, 6.508), (6.152, 6.321), (6.282, 6.146), (6.429, 5.984), (6.59, 5.838), (6.765, 5.708), (6.952, 5.596), (7.15, 5.502),
                    (7.355, 5.429), (7.566, 5.376), (7.782, 5.344), (8.0, 5.333), (8.218, 5.344), (8.434, 5.376), (8.645, 5.429), (8.85, 5.502),
                    (9.048, 5.596), (9.235, 5.708), (9.41, 5.838), (9.571, 5.984), (9.718, 6.146), (9.848, 6.321), (9.96, 6.508), (10.053, 6.705),
                    (10.127, 6.91), (10.18, 7.122), (10.212, 7.338), (10.222, 7.556), (10.222, 10.222), (12.889, 10.222), (12.889, 7.556), (12.878, 7.236),
                    (12.847, 6.917), (12.795, 6.602), (12.722, 6.29), (12.629, 5.984), (12.517, 5.685), (12.385, 5.393), (12.234, 5.111), (12.065, 4.839),
                    (11.879, 4.579), (11.676, 4.332), (11.457, 4.099), (11.223, 3.88), (10.976, 3.677), (10.716, 3.491), (10.444, 3.322), (10.162, 3.171),
                    (9.871, 3.039), (9.571, 2.926), (9.265, 2.833), (8.954, 2.761), (8.638, 2.708), (8.32, 2.677), (8.0, 2.667), (7.68, 2.677), (7.362, 2.708),
                    (7.046, 2.761), (6.735, 2.833), (6.429, 2.926), (6.129, 3.039), (5.838, 3.171), (5.556, 3.322), (5.284, 3.491), (5.024, 3.677),
                    (4.777, 3.88), (4.543, 4.099), (4.324, 4.332), (4.121, 4.579), (3.935, 4.839), (3.766, 5.111), (3.615, 5.393), (3.483, 5.685),
                    (3.371, 5.984), (3.278, 6.29), (3.205, 6.602), (3.153, 6.917), (3.122, 7.236),
                ],
            ], &[
                1, 0, 85, 1, 85, 84, 1, 84, 83, 1, 83, 82, 1, 82, 81, 1, 81, 80, 1, 80, 79, 1, 79, 78, 1, 78, 77, 1, 77, 76, 1, 76, 75, 1, 75, 74, 1, 74, 73,
                1, 73, 72, 1, 72, 71, 1, 71, 70, 1, 70, 69, 1, 69, 68, 1, 68, 67, 1, 67, 66, 1, 66, 65, 1, 65, 64, 1, 64, 63, 1, 63, 62, 1, 62, 61, 1, 61, 60,
                60, 59, 58, 60, 58, 57, 60, 57, 56, 60, 56, 55, 60, 55, 54, 60, 54, 53, 60, 53, 52, 60, 52, 51, 60, 51, 50, 60, 50, 49, 60, 49, 48, 60, 48, 47,
                60, 47, 46, 60, 46, 45, 60, 45, 44, 60, 44, 43, 60, 43, 42, 60, 42, 41, 60, 41, 40, 60, 40, 39, 60, 39, 38, 60, 38, 37, 37, 36, 35, 37, 35, 34,
                37, 34, 33, 37, 33, 32, 37, 32, 31, 37, 31, 30, 37, 30, 29, 37, 29, 28, 3, 2, 1, 60, 37, 28, 60, 28, 27, 60, 27, 26, 60, 26, 25, 60, 25, 24,
                60, 24, 23, 60, 23, 22, 60, 22, 21, 60, 21, 20, 60, 20, 19, 60, 19, 18, 60, 18, 17, 60, 17, 16, 60, 16, 15, 60, 15, 14, 60, 14, 13, 60, 13, 12,
                60, 12, 11, 60, 11, 10, 60, 10, 9, 4, 3, 1, 1, 60, 9, 1, 9, 8, 1, 8, 7, 1, 7, 6, 1, 6, 5, 1, 5, 4,
            ]);
            pen.glyph(0.462, &[
                &[
                    (3.111, 10.222), (5.778, 10.222), (5.778, 12.889), (5.744, 13.059), (5.648, 13.203), (5.503, 13.3), (5.333, 13.333), (3.556, 13.333),
                    (3.385, 13.3), (3.241, 13.203), (3.145, 13.059), (3.111, 12.889),
                ],
            ], &[
                10, 11, 0, 10, 0, 1, 10, 1, 2, 10, 2, 3, 10, 3, 4, 10, 4, 5, 10, 5, 6, 10, 6, 7, 10, 7, 8, 10, 8, 9,
            ]);
            pen.glyph(0.462, &[
                &[
                    (10.222, 10.222), (12.889, 10.222), (12.889, 12.889), (12.855, 13.059), (12.759, 13.203), (12.615, 13.3), (12.444, 13.333),
                    (10.667, 13.333), (10.497, 13.3), (10.352, 13.203), (10.256, 13.059), (10.222, 12.889),
                ],
            ], &[
                10, 11, 0, 10, 0, 1, 10, 1, 2, 10, 2, 3, 10, 3, 4, 10, 4, 5, 10, 5, 6, 10, 6, 7, 10, 7, 8, 10, 8, 9,
            ]);
        }
        Link => {
            pen.glyph(1.0, &[
                &[
                    (7.5, 2.0), (14.5, 2.0), (14.691, 2.038), (14.854, 2.146), (14.962, 2.309), (15.0, 2.5), (15.0, 6.5), (14.962, 6.691), (14.854, 6.854),
                    (14.691, 6.962), (14.5, 7.0), (12.5, 7.0),
                ],
            ], &[
                10, 11, 0, 10, 0, 1, 10, 1, 2, 10, 2, 3, 10, 3, 4, 10, 4, 5, 10, 5, 6, 10, 6, 7, 10, 7, 8, 10, 8, 9,
            ]);
            pen.glyph(1.0, &[
                &[
                    (5.0, 2.0), (1.5, 2.0), (1.309, 2.038), (1.146, 2.146), (1.038, 2.309), (1.0, 2.5), (1.0, 6.5), (1.038, 6.691), (1.146, 6.854),
                    (1.309, 6.962), (1.5, 7.0), (5.0, 7.0),
                ],
            ], &[
                1, 0, 11, 1, 11, 10, 1, 10, 9, 1, 9, 8, 1, 8, 7, 1, 7, 6, 1, 6, 5, 1, 5, 4, 1, 4, 3, 1, 3, 2,
            ]);
            pen.glyph(1.0, &[
                &[
                    (2.0, 12.0), (2.0, 9.0), (5.0, 9.0), (5.0, 8.0), (1.5, 8.0), (1.309, 8.038), (1.146, 8.146), (1.038, 8.309), (1.0, 8.5), (1.0, 12.5),
                    (1.038, 12.691), (1.146, 12.854), (1.309, 12.962), (1.5, 13.0), (5.0, 13.0), (5.0, 12.0),
                ],
            ], &[
                0, 15, 14, 0, 14, 13, 0, 13, 12, 0, 12, 11, 0, 11, 10, 0, 10, 9, 0, 9, 8, 0, 8, 7, 0, 7, 6, 0, 6, 5, 0, 5, 4, 4, 3, 2, 4, 2, 1, 4, 1, 0,
            ]);
            pen.glyph(1.0, &[
                &[
                    (6.0, 2.85), (6.0, 14.975), (6.042, 15.114), (6.154, 15.206), (6.298, 15.22), (6.426, 15.152), (10.172, 11.431), (14.666, 11.444),
                    (14.806, 11.402), (14.898, 11.289), (14.913, 11.145), (14.844, 11.017), (6.478, 2.65), (6.334, 2.573), (6.172, 2.59), (6.046, 2.694),
                ],
            ], &[
                1, 0, 15, 1, 15, 14, 1, 14, 13, 1, 13, 12, 12, 11, 10, 12, 10, 9, 12, 9, 8, 12, 8, 7, 12, 7, 6, 12, 6, 5, 12, 5, 4, 12, 4, 3, 12, 3, 2, 12, 2,
                1,
            ]);
        }
        ExternalLink => {
            pen.glyph(1.0, &[
                &[
                    (9.778, 1.778), (13.778, 1.778), (13.948, 1.812), (14.092, 1.908), (14.188, 2.052), (14.222, 2.222), (14.222, 2.667), (9.778, 2.667),
                ],
            ], &[
                6, 7, 0, 6, 0, 1, 6, 1, 2, 6, 2, 3, 6, 3, 4, 6, 4, 5,
            ]);
            pen.glyph(1.0, &[
                &[
                    (8.889, 3.556), (8.889, 1.778), (2.222, 1.778), (2.052, 1.812), (1.908, 1.908), (1.812, 2.052), (1.778, 2.222), (1.778, 11.708),
                    (2.667, 10.819), (2.667, 2.667), (8.0, 2.667), (8.0, 4.444), (13.333, 4.444), (13.333, 13.333), (5.181, 13.333), (4.292, 14.222),
                    (13.778, 14.222), (13.948, 14.188), (14.092, 14.092), (14.188, 13.948), (14.222, 13.778), (14.222, 3.556),
                ],
            ], &[
                21, 20, 19, 21, 19, 18, 21, 18, 17, 21, 17, 16, 16, 15, 14, 16, 14, 13, 16, 13, 12, 9, 8, 7, 9, 7, 6, 9, 6, 5, 9, 5, 4, 9, 4, 3, 9, 3, 2, 9, 2,
                1, 21, 16, 12, 21, 12, 11, 10, 9, 1, 10, 1, 0, 0, 21, 11, 0, 11, 10,
            ]);
            // the arrow leaves through the frame's lower-left opening
            pen.line(&[(7.5, 6.8), (4.2, 10.8)]);
            pen.fill(&[(1.2, 13.8), (5.7, 12.1), (2.7, 9.5)]);
        }
        Keyframe => {
            pen.glyph(0.231, &[
                &[
                    (8.0, 1.5), (8.0, 14.5), (1.5, 8.0),
                ],
            ], &[
                1, 2, 0,
            ]);
            pen.glyph(1.0, &[
                &[
                    (15.146, 7.646), (8.354, 0.854), (8.191, 0.745), (8.0, 0.707), (7.809, 0.745), (7.647, 0.854), (0.854, 7.646), (0.745, 7.809),
                    (0.707, 8.0), (0.745, 8.191), (0.854, 8.354), (7.647, 15.146), (7.809, 15.255), (8.0, 15.293), (8.191, 15.255), (8.354, 15.146),
                    (15.146, 8.354), (15.255, 8.191), (15.293, 8.0), (15.255, 7.809),
                ],
                &[
                    (8.0, 14.5), (1.5, 8.0), (8.0, 1.5),
                ],
            ], &[
                8, 21, 20, 22, 21, 8, 22, 8, 7, 22, 7, 6, 22, 6, 5, 22, 5, 4, 22, 4, 3, 22, 3, 2, 22, 2, 1, 22, 1, 0, 22, 0, 19, 22, 19, 18, 22, 18, 17, 22,
                17, 16, 22, 16, 15, 22, 15, 14, 14, 13, 12, 14, 12, 11, 14, 11, 10, 14, 10, 9, 14, 9, 8, 14, 8, 20, 14, 20, 22,
            ]);
        }
        Stopwatch => pen.glyph(1.0, &[
            &[
                (9.0, 3.025), (9.0, 2.0), (10.25, 2.0), (10.346, 1.981), (10.427, 1.927), (10.481, 1.846), (10.5, 1.75), (10.5, 1.25), (10.481, 1.154),
                (10.427, 1.073), (10.346, 1.019), (10.25, 1.0), (6.75, 1.0), (6.654, 1.019), (6.573, 1.073), (6.519, 1.154), (6.5, 1.25), (6.5, 1.75),
                (6.519, 1.846), (6.573, 1.927), (6.654, 1.981), (6.75, 2.0), (8.0, 2.0), (8.0, 3.024), (7.77, 3.05), (7.542, 3.086), (7.315, 3.131),
                (7.09, 3.186), (6.868, 3.251), (6.648, 3.324), (6.432, 3.407), (6.22, 3.499), (6.012, 3.6), (5.808, 3.709), (5.609, 3.827), (5.415, 3.953),
                (5.227, 4.087), (4.64, 3.481), (4.84, 3.281), (4.894, 3.2), (4.913, 3.104), (4.894, 3.009), (4.84, 2.927), (4.487, 2.573), (4.406, 2.519),
                (4.311, 2.5), (4.215, 2.519), (4.134, 2.573), (3.073, 3.634), (3.019, 3.715), (3.0, 3.811), (3.019, 3.906), (3.073, 3.987), (3.427, 4.341),
                (3.508, 4.395), (3.604, 4.414), (3.699, 4.395), (3.78, 4.341), (3.933, 4.189), (4.481, 4.755), (3.75, 5.741), (3.256, 6.865), (3.024, 8.071),
                (3.066, 9.297), (3.379, 10.485), (3.947, 11.573), (4.743, 12.507), (5.727, 13.242), (6.849, 13.74), (8.053, 13.977), (9.28, 13.941),
                (10.469, 13.633), (11.559, 13.069), (12.497, 12.277), (13.236, 11.296), (13.739, 10.176), (13.98, 8.973), (13.949, 7.745), (13.646, 6.556),
                (13.086, 5.463), (12.298, 4.522), (11.32, 3.779), (10.203, 3.272),
            ],
            &[
                (8.5, 13.0), (7.788, 12.944), (7.094, 12.777), (6.435, 12.501), (5.828, 12.125), (5.288, 11.658), (4.829, 11.111), (4.462, 10.498),
                (4.197, 9.836), (4.04, 9.139), (3.995, 8.426), (4.063, 7.716), (4.243, 7.025), (4.53, 6.371), (4.917, 5.77), (5.393, 5.239), (5.948, 4.79),
                (6.567, 4.434), (7.235, 4.18), (7.934, 4.035), (8.647, 4.003), (9.357, 4.083), (10.044, 4.275), (10.693, 4.574), (11.287, 4.971),
                (8.111, 8.146), (7.989, 8.335), (7.961, 8.558), (8.034, 8.771), (8.193, 8.93), (8.406, 9.003), (8.629, 8.976), (8.818, 8.854), (12.0, 5.673),
                (12.249, 6.013), (12.466, 6.375), (12.647, 6.756), (12.792, 7.152), (12.899, 7.559), (12.968, 7.976), (12.998, 8.396), (12.988, 8.818),
                (12.938, 9.237), (12.85, 9.649), (12.723, 10.051), (12.559, 10.44), (12.36, 10.811), (12.127, 11.162), (11.862, 11.49), (11.567, 11.792),
                (11.245, 12.065), (10.9, 12.306), (10.533, 12.514), (10.148, 12.687), (9.749, 12.823), (9.339, 12.921), (8.921, 12.98),
            ],
        ], &[
            62, 93, 92, 62, 92, 91, 62, 91, 90, 116, 115, 114, 116, 114, 113, 116, 113, 112, 116, 112, 111, 116, 111, 110, 116, 110, 109, 116, 109, 108, 116,
            108, 107, 94, 93, 62, 94, 62, 61, 94, 61, 60, 94, 60, 59, 58, 57, 56, 58, 56, 55, 58, 55, 54, 58, 54, 53, 58, 53, 52, 58, 52, 51, 58, 51, 50, 58,
            50, 49, 58, 49, 48, 58, 48, 47, 58, 47, 46, 58, 46, 45, 58, 45, 44, 58, 44, 43, 58, 43, 42, 58, 42, 41, 58, 41, 40, 58, 40, 39, 39, 38, 37, 36, 35,
            34, 36, 34, 33, 36, 33, 32, 36, 32, 31, 36, 31, 30, 36, 30, 29, 36, 29, 28, 36, 28, 27, 36, 27, 26, 36, 26, 25, 36, 25, 24, 36, 24, 23, 22, 21, 20,
            22, 20, 19, 22, 19, 18, 22, 18, 17, 22, 17, 16, 22, 16, 15, 22, 15, 14, 22, 14, 13, 22, 13, 12, 22, 12, 11, 22, 11, 10, 22, 10, 9, 22, 9, 8, 22, 8,
            7, 22, 7, 6, 22, 6, 5, 22, 5, 4, 22, 4, 3, 3, 2, 1, 0, 82, 81, 0, 81, 80, 0, 80, 79, 0, 79, 78, 78, 77, 76, 78, 76, 75, 78, 75, 74, 78, 74, 73, 73,
            72, 71, 73, 71, 70, 73, 70, 69, 73, 69, 68, 68, 67, 66, 68, 66, 65, 68, 65, 64, 68, 64, 63, 63, 62, 90, 63, 90, 89, 63, 89, 88, 95, 94, 59, 58, 39,
            37, 58, 37, 36, 22, 3, 1, 22, 1, 0, 68, 63, 88, 68, 88, 87, 68, 87, 86, 68, 86, 85, 68, 85, 84, 68, 84, 83, 68, 83, 139, 68, 139, 138, 68, 138,
            137, 96, 95, 59, 59, 58, 36, 59, 36, 23, 23, 22, 0, 73, 68, 137, 73, 137, 136, 73, 136, 135, 73, 135, 134, 73, 134, 133, 73, 133, 132, 73, 132,
            131, 73, 131, 130, 73, 130, 129, 73, 129, 128, 73, 128, 127, 73, 127, 126, 73, 126, 125, 97, 96, 59, 59, 23, 0, 78, 73, 125, 78, 125, 124, 78, 124,
            123, 78, 123, 122, 78, 122, 121, 78, 121, 120, 78, 120, 119, 78, 119, 118, 78, 118, 117, 78, 117, 116, 78, 116, 107, 98, 97, 59, 0, 78, 107, 0,
            107, 106, 0, 106, 105, 0, 105, 104, 0, 104, 103, 0, 103, 102, 0, 102, 101, 0, 101, 100, 0, 100, 99, 99, 98, 59, 99, 59, 0,
        ]),
        Fx => pen.glyph(1.0, &[
            &[
                (14.692, 6.154), (13.338, 6.154), (13.291, 6.16), (13.249, 6.18), (13.214, 6.211), (13.189, 6.25), (10.454, 8.965), (10.454, 8.946),
                (9.318, 6.215), (9.305, 6.186), (9.281, 6.163), (9.251, 6.152), (9.218, 6.154), (5.553, 6.154), (5.807, 5.051), (6.087, 4.105), (6.433, 3.414),
                (6.825, 2.942), (7.242, 2.65), (7.665, 2.503), (8.073, 2.462), (8.323, 2.506), (8.571, 2.559), (8.818, 2.621), (9.062, 2.691), (9.303, 2.769),
                (9.33, 2.774), (9.354, 2.77), (9.375, 2.757), (9.393, 2.737), (9.407, 2.71), (9.418, 2.676), (9.668, 1.59), (9.669, 1.544), (9.655, 1.501),
                (9.628, 1.465), (9.591, 1.44), (9.296, 1.37), (8.999, 1.315), (8.699, 1.273), (8.398, 1.245), (8.096, 1.231), (7.209, 1.324), (6.414, 1.617),
                (5.715, 2.127), (5.119, 2.873), (4.631, 3.873), (4.256, 5.145), (4.0, 6.154), (2.15, 6.154), (2.108, 6.157), (2.072, 6.177), (2.046, 6.209),
                (2.034, 6.248), (1.65, 7.273), (1.644, 7.31), (1.67, 7.322), (1.693, 7.339), (1.712, 7.36), (1.726, 7.385), (3.553, 7.385), (3.35, 8.225),
                (2.968, 9.7), (2.486, 11.505), (1.985, 13.335), (1.545, 14.885), (1.246, 15.85), (1.233, 15.894), (1.232, 15.931), (1.241, 15.96),
                (1.26, 15.982), (1.288, 15.995), (1.322, 16.0), (1.454, 15.993), (1.674, 15.989), (1.951, 15.988), (2.255, 15.989), (2.555, 15.991),
                (2.818, 15.993), (2.873, 15.987), (2.922, 15.962), (2.958, 15.921), (2.976, 15.87), (3.228, 15.063), (3.562, 13.911), (3.952, 12.491),
                (4.371, 10.878), (4.792, 9.151), (5.188, 7.385), (7.149, 7.385), (7.219, 7.384), (7.374, 7.381), (7.589, 7.376), (7.837, 7.367),
                (8.094, 7.355), (8.334, 7.34), (9.487, 9.639), (8.964, 10.225), (8.413, 10.86), (7.845, 11.52), (7.275, 12.184), (6.715, 12.826),
                (6.178, 13.426), (6.154, 13.456), (6.153, 13.494), (6.175, 13.526), (6.211, 13.538), (7.631, 13.538), (7.811, 13.366), (8.21, 12.92),
                (8.739, 12.306), (9.311, 11.628), (9.838, 10.993), (10.231, 10.506), (10.247, 10.506), (10.37, 10.731), (10.678, 11.293), (11.081, 12.022),
                (11.489, 12.752), (11.814, 13.314), (11.964, 13.538), (13.303, 13.538), (13.344, 13.532), (13.375, 13.505), (13.384, 13.464), (13.369, 13.426),
                (13.124, 12.95), (12.78, 12.332), (12.384, 11.637), (11.979, 10.926), (11.61, 10.26), (11.323, 9.703), (11.842, 9.165), (12.443, 8.566),
                (13.08, 7.943), (13.707, 7.328), (14.276, 6.758), (14.741, 6.266), (14.76, 6.24), (14.769, 6.214), (14.766, 6.191), (14.752, 6.172),
                (14.727, 6.159),
            ],
        ], &[
            1, 0, 142, 1, 142, 141, 1, 141, 140, 1, 140, 139, 1, 139, 138, 1, 138, 137, 1, 137, 136, 1, 136, 135, 1, 135, 134, 1, 134, 133, 1, 133, 132, 1,
            132, 131, 128, 127, 126, 128, 126, 125, 128, 125, 124, 128, 124, 123, 128, 123, 122, 128, 122, 121, 128, 121, 120, 128, 120, 119, 128, 119, 118,
            128, 118, 117, 128, 117, 116, 128, 116, 115, 128, 115, 114, 113, 112, 111, 113, 111, 110, 113, 110, 109, 113, 109, 108, 113, 108, 107, 113, 107,
            106, 113, 106, 105, 113, 105, 104, 113, 104, 103, 113, 103, 102, 113, 102, 101, 113, 101, 100, 113, 100, 99, 113, 99, 98, 113, 98, 97, 113, 97, 96,
            113, 96, 95, 95, 94, 93, 95, 93, 92, 95, 92, 91, 95, 91, 90, 95, 90, 89, 95, 89, 88, 88, 87, 86, 88, 86, 85, 88, 85, 84, 88, 84, 83, 88, 83, 82,
            88, 82, 81, 88, 81, 80, 88, 80, 79, 88, 79, 78, 88, 78, 77, 88, 77, 76, 88, 76, 75, 88, 75, 74, 88, 74, 73, 88, 73, 72, 88, 72, 71, 88, 71, 70, 88,
            70, 69, 88, 69, 68, 88, 68, 67, 88, 67, 66, 88, 66, 65, 88, 65, 64, 88, 64, 63, 88, 63, 62, 88, 62, 61, 88, 61, 60, 60, 59, 58, 60, 58, 57, 60, 57,
            56, 60, 56, 55, 60, 55, 54, 60, 54, 53, 60, 53, 52, 60, 52, 51, 60, 51, 50, 60, 50, 49, 60, 49, 48, 48, 47, 46, 48, 46, 45, 48, 45, 44, 48, 44, 43,
            48, 43, 42, 48, 42, 41, 41, 40, 39, 41, 39, 38, 41, 38, 37, 41, 37, 36, 41, 36, 35, 41, 35, 34, 41, 34, 33, 41, 33, 32, 41, 32, 31, 41, 31, 30, 41,
            30, 29, 41, 29, 28, 41, 28, 27, 41, 27, 26, 41, 26, 25, 41, 25, 24, 41, 24, 23, 41, 23, 22, 41, 22, 21, 41, 21, 20, 41, 20, 19, 41, 19, 18, 41, 18,
            17, 41, 17, 16, 12, 11, 10, 12, 10, 9, 12, 9, 8, 12, 8, 7, 12, 7, 6, 5, 4, 3, 5, 3, 2, 5, 2, 1, 5, 1, 131, 129, 128, 114, 114, 113, 95, 95, 88, 60,
            95, 60, 48, 48, 41, 16, 48, 16, 15, 48, 15, 14, 48, 14, 13, 6, 5, 131, 6, 131, 130, 6, 130, 129, 6, 129, 114, 6, 114, 95, 95, 48, 13, 95, 13, 12,
            95, 12, 6,
        ]),
        Reset => {
            pen.glyph(1.0, &[
                &[
                    (12.275, 12.0), (12.635, 11.65), (13.02, 10.989), (13.388, 10.13), (13.701, 9.188), (13.919, 8.273), (14.0, 7.5), (13.988, 7.14),
                    (13.953, 6.782), (13.894, 6.427), (13.813, 6.076), (13.708, 5.732), (13.581, 5.395), (13.433, 5.067), (13.263, 4.75), (13.073, 4.444),
                    (12.863, 4.152), (12.635, 3.874), (12.389, 3.611), (12.126, 3.365), (11.848, 3.137), (11.556, 2.927), (11.25, 2.737), (10.933, 2.567),
                    (10.605, 2.419), (10.268, 2.292), (9.924, 2.187), (9.573, 2.106), (9.218, 2.047), (8.86, 2.012), (8.5, 2.0), (8.167, 2.012),
                    (7.835, 2.045), (7.505, 2.099), (7.18, 2.173), (6.86, 2.268), (6.547, 2.382), (6.241, 2.516), (5.944, 2.669), (5.658, 2.84),
                    (5.383, 3.028), (5.12, 3.234), (4.87, 3.456), (4.635, 3.693), (4.416, 3.944), (4.212, 4.208), (4.026, 4.485), (3.857, 4.773),
                    (3.707, 5.07), (3.575, 5.377), (3.463, 5.691), (3.371, 6.012), (3.299, 6.338), (3.248, 6.668), (3.218, 7.0), (1.078, 7.0), (0.941, 7.041),
                    (0.849, 7.151), (0.832, 7.293), (0.896, 7.421), (4.26, 11.0), (7.606, 7.421), (7.67, 7.292), (7.653, 7.15), (7.561, 7.041), (7.424, 7.0),
                    (5.215, 7.0), (5.248, 6.757), (5.298, 6.517), (5.366, 6.282), (5.451, 6.052), (5.553, 5.829), (5.671, 5.614), (5.804, 5.409),
                    (5.952, 5.214), (6.115, 5.03), (6.29, 4.859), (6.477, 4.701), (6.676, 4.558), (6.885, 4.43), (7.102, 4.317), (7.328, 4.221), (7.56, 4.141),
                    (7.797, 4.079), (8.038, 4.035), (8.281, 4.008), (8.526, 4.0), (9.525, 4.134), (10.376, 4.473), (11.064, 4.992), (11.574, 5.669),
                    (11.891, 6.48), (12.0, 7.4), (11.941, 8.191), (11.761, 9.104), (11.454, 10.039), (11.013, 10.899), (10.434, 11.585), (9.709, 12.0),
                ],
            ], &[
                1, 0, 98, 1, 98, 97, 1, 97, 96, 1, 96, 95, 1, 95, 94, 1, 94, 93, 1, 93, 92, 1, 92, 91, 66, 65, 64, 66, 64, 63, 66, 63, 62, 66, 62, 61, 66, 61,
                60, 66, 60, 59, 66, 59, 58, 66, 58, 57, 66, 57, 56, 56, 55, 54, 54, 53, 52, 54, 52, 51, 54, 51, 50, 54, 50, 49, 54, 49, 48, 54, 48, 47, 54, 47,
                46, 54, 46, 45, 54, 45, 44, 54, 44, 43, 54, 43, 42, 54, 42, 41, 54, 41, 40, 54, 40, 39, 54, 39, 38, 54, 38, 37, 54, 37, 36, 54, 36, 35, 54, 35,
                34, 54, 34, 33, 54, 33, 32, 54, 32, 31, 54, 31, 30, 54, 30, 29, 54, 29, 28, 54, 28, 27, 54, 27, 26, 26, 25, 24, 26, 24, 23, 26, 23, 22, 26, 22,
                21, 26, 21, 20, 26, 20, 19, 26, 19, 18, 26, 18, 17, 26, 17, 16, 26, 16, 15, 26, 15, 14, 26, 14, 13, 26, 13, 12, 26, 12, 11, 26, 11, 10, 26, 10,
                9, 26, 9, 8, 26, 8, 7, 26, 7, 6, 26, 6, 5, 26, 5, 4, 4, 3, 2, 4, 2, 1, 4, 1, 91, 4, 91, 90, 66, 56, 54, 26, 4, 90, 26, 90, 89, 26, 89, 88, 26,
                88, 87, 26, 87, 86, 26, 86, 85, 26, 85, 84, 26, 84, 83, 26, 83, 82, 26, 82, 81, 26, 81, 80, 26, 80, 79, 26, 79, 78, 26, 78, 77, 67, 66, 54, 54,
                26, 77, 54, 77, 76, 54, 76, 75, 54, 75, 74, 54, 74, 73, 54, 73, 72, 54, 72, 71, 54, 71, 70, 54, 70, 69, 54, 69, 68, 54, 68, 67,
            ]);
            pen.glyph(1.0, &[
                &[
                    (2.25, 13.0), (13.75, 13.0), (13.846, 13.019), (13.927, 13.073), (13.981, 13.154), (14.0, 13.25), (14.0, 13.75), (13.981, 13.846),
                    (13.927, 13.927), (13.846, 13.981), (13.75, 14.0), (2.25, 14.0), (2.154, 13.981), (2.073, 13.927), (2.019, 13.846), (2.0, 13.75),
                    (2.0, 13.25), (2.019, 13.154), (2.073, 13.073), (2.154, 13.019),
                ],
            ], &[
                18, 19, 0, 18, 0, 1, 18, 1, 2, 18, 2, 3, 18, 3, 4, 18, 4, 5, 18, 5, 6, 18, 6, 7, 18, 7, 8, 18, 8, 9, 18, 9, 10, 18, 10, 11, 18, 11, 12, 18, 12,
                13, 18, 13, 14, 18, 14, 15, 18, 15, 16, 18, 16, 17,
            ]);
        }
        Close => pen.glyph(1.0, &[
            &[
                (11.771, 2.972), (8.0, 6.743), (4.229, 2.972), (4.085, 2.875), (3.915, 2.841), (3.745, 2.875), (3.6, 2.972), (2.972, 3.6), (2.875, 3.744),
                (2.841, 3.914), (2.875, 4.084), (2.972, 4.228), (6.743, 8.0), (2.972, 11.771), (2.875, 11.915), (2.841, 12.085), (2.875, 12.255),
                (2.972, 12.4), (3.6, 13.028), (3.744, 13.125), (3.914, 13.159), (4.084, 13.125), (4.228, 13.028), (8.0, 9.257), (11.771, 13.028),
                (11.915, 13.124), (12.085, 13.158), (12.255, 13.124), (12.4, 13.028), (13.028, 12.4), (13.125, 12.256), (13.159, 12.086), (13.125, 11.916),
                (13.028, 11.772), (9.257, 8.0), (13.028, 4.229), (13.124, 4.085), (13.158, 3.915), (13.124, 3.745), (13.028, 3.6), (12.4, 2.972),
                (12.256, 2.875), (12.086, 2.841), (11.915, 2.875),
            ],
        ], &[
            1, 0, 43, 1, 43, 42, 1, 42, 41, 1, 41, 40, 1, 40, 39, 1, 39, 38, 1, 38, 37, 1, 37, 36, 1, 36, 35, 1, 35, 34, 1, 34, 33, 1, 33, 32, 1, 32, 31, 1,
            31, 30, 1, 30, 29, 1, 29, 28, 1, 28, 27, 1, 27, 26, 1, 26, 25, 1, 25, 24, 1, 24, 23, 1, 23, 22, 1, 22, 21, 1, 21, 20, 1, 20, 19, 1, 19, 18, 1, 18,
            17, 1, 17, 16, 1, 16, 15, 1, 15, 14, 14, 13, 12, 12, 11, 10, 12, 10, 9, 12, 9, 8, 12, 8, 7, 12, 7, 6, 12, 6, 5, 12, 5, 4, 12, 4, 3, 12, 3, 2, 12,
            2, 1, 12, 1, 14,
        ]),
        Fullscreen => {
            pen.glyph(1.0, &[
                &[
                    (14.222, 10.889), (14.222, 13.333), (11.778, 13.333), (11.693, 13.35), (11.621, 13.398), (11.572, 13.471), (11.556, 13.556),
                    (11.556, 14.0), (11.572, 14.085), (11.621, 14.157), (11.693, 14.205), (11.778, 14.222), (15.111, 14.222), (15.111, 10.889),
                    (15.094, 10.804), (15.046, 10.732), (14.974, 10.684), (14.889, 10.667), (14.444, 10.667), (14.359, 10.684), (14.287, 10.732),
                    (14.239, 10.804),
                ],
            ], &[
                1, 0, 21, 1, 21, 20, 1, 20, 19, 1, 19, 18, 1, 18, 17, 1, 17, 16, 1, 16, 15, 1, 15, 14, 1, 14, 13, 1, 13, 12, 1, 12, 11, 1, 11, 10, 1, 10, 9, 1,
                9, 8, 1, 8, 7, 1, 7, 6, 1, 6, 5, 1, 5, 4, 1, 4, 3, 1, 3, 2,
            ]);
            pen.glyph(1.0, &[
                &[
                    (1.778, 13.333), (1.778, 10.889), (1.761, 10.804), (1.713, 10.732), (1.641, 10.684), (1.556, 10.667), (1.111, 10.667), (1.026, 10.684),
                    (0.954, 10.732), (0.906, 10.804), (0.889, 10.889), (0.889, 14.222), (4.222, 14.222), (4.307, 14.205), (4.379, 14.157), (4.428, 14.085),
                    (4.444, 14.0), (4.444, 13.556), (4.428, 13.471), (4.379, 13.398), (4.307, 13.35), (4.222, 13.333),
                ],
            ], &[
                0, 21, 20, 0, 20, 19, 0, 19, 18, 0, 18, 17, 0, 17, 16, 0, 16, 15, 0, 15, 14, 0, 14, 13, 0, 13, 12, 0, 12, 11, 0, 11, 10, 0, 10, 9, 0, 9, 8, 0,
                8, 7, 0, 7, 6, 0, 6, 5, 0, 5, 4, 0, 4, 3, 0, 3, 2, 0, 2, 1,
            ]);
            pen.glyph(1.0, &[
                &[
                    (11.556, 2.0), (11.556, 2.444), (11.572, 2.529), (11.621, 2.602), (11.693, 2.65), (11.778, 2.667), (14.222, 2.667), (14.222, 5.111),
                    (14.239, 5.196), (14.287, 5.268), (14.359, 5.316), (14.444, 5.333), (14.889, 5.333), (14.974, 5.316), (15.046, 5.268), (15.094, 5.196),
                    (15.111, 5.111), (15.111, 1.778), (11.778, 1.778), (11.693, 1.795), (11.621, 1.843), (11.572, 1.915),
                ],
            ], &[
                1, 0, 21, 1, 21, 20, 1, 20, 19, 1, 19, 18, 1, 18, 17, 17, 16, 15, 17, 15, 14, 17, 14, 13, 17, 13, 12, 17, 12, 11, 17, 11, 10, 17, 10, 9, 17, 9,
                8, 17, 8, 7, 17, 7, 6, 17, 6, 5, 17, 5, 4, 17, 4, 3, 17, 3, 2, 17, 2, 1,
            ]);
            pen.glyph(1.0, &[
                &[
                    (1.778, 2.667), (4.222, 2.667), (4.307, 2.65), (4.379, 2.602), (4.428, 2.529), (4.444, 2.444), (4.444, 2.0), (4.428, 1.915),
                    (4.379, 1.843), (4.307, 1.795), (4.222, 1.778), (0.889, 1.778), (0.889, 5.111), (0.906, 5.196), (0.954, 5.268), (1.026, 5.316),
                    (1.111, 5.333), (1.556, 5.333), (1.641, 5.316), (1.713, 5.268), (1.761, 5.196), (1.778, 5.111),
                ],
            ], &[
                0, 21, 20, 0, 20, 19, 0, 19, 18, 0, 18, 17, 0, 17, 16, 0, 16, 15, 0, 15, 14, 0, 14, 13, 0, 13, 12, 0, 12, 11, 0, 11, 10, 0, 10, 9, 0, 9, 8, 0,
                8, 7, 0, 7, 6, 0, 6, 5, 0, 5, 4, 0, 4, 3, 0, 3, 2, 0, 2, 1,
            ]);
            pen.glyph(1.0, &[
                &[
                    (12.222, 4.444), (12.307, 4.461), (12.379, 4.51), (12.428, 4.582), (12.444, 4.667), (12.444, 11.333), (12.428, 11.418), (12.379, 11.49),
                    (12.307, 11.539), (12.222, 11.556), (3.778, 11.556), (3.693, 11.539), (3.621, 11.49), (3.572, 11.418), (3.556, 11.333), (3.556, 4.667),
                    (3.572, 4.582), (3.621, 4.51), (3.693, 4.461), (3.778, 4.444),
                ],
            ], &[
                18, 19, 0, 18, 0, 1, 18, 1, 2, 18, 2, 3, 18, 3, 4, 18, 4, 5, 18, 5, 6, 18, 6, 7, 18, 7, 8, 18, 8, 9, 18, 9, 10, 18, 10, 11, 18, 11, 12, 18, 12,
                13, 18, 13, 14, 18, 14, 15, 18, 15, 16, 18, 16, 17,
            ]);
        }
        Export => {
            pen.glyph(1.0, &[
                &[
                    (5.911, 1.956), (5.84, 1.881), (5.754, 1.825), (5.658, 1.79), (5.556, 1.778), (2.222, 1.778), (2.052, 1.812), (1.908, 1.908),
                    (1.812, 2.052), (1.778, 2.222), (1.778, 2.667), (6.4, 2.667),
                ],
            ], &[
                1, 0, 11, 1, 11, 10, 1, 10, 9, 1, 9, 8, 1, 8, 7, 1, 7, 6, 1, 6, 5, 1, 5, 4, 1, 4, 3, 1, 3, 2,
            ]);
            pen.glyph(1.0, &[
                &[
                    (8.0, 9.778), (11.111, 9.778), (11.111, 8.044), (11.147, 7.924), (11.242, 7.842), (11.366, 7.825), (11.48, 7.877), (15.111, 11.111),
                    (11.48, 14.344), (11.366, 14.396), (11.242, 14.379), (11.147, 14.298), (11.111, 14.178), (11.111, 12.444), (8.0, 12.444), (7.83, 12.411),
                    (7.686, 12.314), (7.589, 12.17), (7.556, 12.0), (7.556, 10.222), (7.589, 10.052), (7.686, 9.908), (7.83, 9.812),
                ],
            ], &[
                21, 22, 0, 21, 0, 1, 1, 2, 3, 1, 3, 4, 1, 4, 5, 1, 5, 6, 1, 6, 7, 1, 7, 8, 1, 8, 9, 1, 9, 10, 1, 10, 11, 11, 12, 13, 13, 14, 15, 13, 15, 16,
                13, 16, 17, 13, 17, 18, 13, 18, 19, 13, 19, 20, 13, 20, 21, 13, 21, 1, 13, 1, 11,
            ]);
            pen.glyph(1.0, &[
                &[
                    (6.667, 10.222), (6.683, 10.014), (6.732, 9.81), (6.812, 9.617), (6.921, 9.439), (7.057, 9.279), (7.216, 9.144), (7.395, 9.034),
                    (7.588, 8.954), (7.791, 8.905), (8.0, 8.889), (10.222, 8.889), (10.222, 8.044), (10.244, 7.828), (10.307, 7.619), (10.409, 7.427),
                    (10.548, 7.259), (10.716, 7.121), (10.908, 7.018), (11.117, 6.955), (11.333, 6.933), (11.533, 6.952), (11.727, 7.006), (11.908, 7.094),
                    (12.069, 7.214), (14.222, 9.13), (14.222, 4.0), (14.188, 3.83), (14.092, 3.686), (13.948, 3.589), (13.778, 3.556), (1.778, 3.556),
                    (1.778, 11.111), (1.812, 11.281), (1.908, 11.425), (2.052, 11.522), (2.222, 11.556), (6.667, 11.556),
                ],
            ], &[
                0, 37, 36, 0, 36, 35, 0, 35, 34, 0, 34, 33, 0, 33, 32, 0, 32, 31, 31, 30, 29, 31, 29, 28, 31, 28, 27, 31, 27, 26, 26, 25, 24, 26, 24, 23, 26,
                23, 22, 26, 22, 21, 26, 21, 20, 26, 20, 19, 26, 19, 18, 26, 18, 17, 26, 17, 16, 12, 11, 10, 12, 10, 9, 12, 9, 8, 1, 0, 31, 31, 26, 16, 31, 16,
                15, 31, 15, 14, 31, 14, 13, 31, 13, 12, 31, 12, 8, 31, 8, 7, 31, 7, 6, 31, 6, 5, 31, 5, 4, 31, 4, 3, 31, 3, 2, 31, 2, 1,
            ]);
        }
        Import => {
            pen.glyph(1.0, &[
                &[
                    (5.911, 1.956), (5.84, 1.881), (5.754, 1.825), (5.658, 1.79), (5.556, 1.778), (2.222, 1.778), (2.052, 1.812), (1.908, 1.908),
                    (1.812, 2.052), (1.778, 2.222), (1.778, 2.667), (6.4, 2.667),
                ],
            ], &[
                1, 0, 11, 1, 11, 10, 1, 10, 9, 1, 9, 8, 1, 8, 7, 1, 7, 6, 1, 6, 5, 1, 5, 4, 1, 4, 3, 1, 3, 2,
            ]);
            pen.glyph(1.0, &[
                &[
                    (1.333, 8.889), (4.441, 8.889), (4.441, 7.156), (4.476, 7.035), (4.571, 6.953), (4.695, 6.936), (4.809, 6.988), (8.441, 10.222),
                    (4.809, 13.455), (4.696, 13.508), (4.572, 13.49), (4.477, 13.409), (4.441, 13.289), (4.441, 11.556), (1.333, 11.556), (1.163, 11.522),
                    (1.019, 11.425), (0.923, 11.281), (0.889, 11.111), (0.889, 9.333), (0.923, 9.163), (1.019, 9.019), (1.163, 8.923),
                ],
            ], &[
                21, 22, 0, 21, 0, 1, 1, 2, 3, 1, 3, 4, 1, 4, 5, 1, 5, 6, 1, 6, 7, 1, 7, 8, 1, 8, 9, 1, 9, 10, 1, 10, 11, 11, 12, 13, 13, 14, 15, 13, 15, 16,
                13, 16, 17, 13, 17, 18, 13, 18, 19, 13, 19, 20, 13, 20, 21, 13, 21, 1, 13, 1, 11,
            ]);
            pen.glyph(1.0, &[
                &[
                    (13.778, 3.556), (1.778, 3.556), (1.778, 8.0), (3.552, 8.0), (3.552, 7.156), (3.573, 6.939), (3.637, 6.73), (3.739, 6.538), (3.877, 6.37),
                    (4.046, 6.232), (4.238, 6.129), (4.446, 6.066), (4.663, 6.044), (4.863, 6.063), (5.057, 6.117), (5.237, 6.206), (5.399, 6.325),
                    (9.031, 9.558), (9.778, 10.222), (9.032, 10.886), (8.28, 11.556), (13.778, 11.556), (13.948, 11.522), (14.092, 11.425), (14.188, 11.281),
                    (14.222, 11.111), (14.222, 4.0), (14.188, 3.83), (14.092, 3.686), (13.948, 3.589),
                ],
            ], &[
                1, 0, 29, 1, 29, 28, 1, 28, 27, 1, 27, 26, 1, 26, 25, 1, 25, 24, 1, 24, 23, 1, 23, 22, 1, 22, 21, 21, 20, 19, 21, 19, 18, 21, 18, 17, 21, 17,
                16, 21, 16, 15, 4, 3, 2, 4, 2, 1, 1, 21, 15, 1, 15, 14, 1, 14, 13, 1, 13, 12, 1, 12, 11, 1, 11, 10, 1, 10, 9, 1, 9, 8, 1, 8, 7, 1, 7, 6, 1, 6,
                5, 1, 5, 4,
            ]);
        }
        Gear => pen.glyph(1.0, &[
            &[
                (14.622, 7.019), (13.24, 7.019), (13.196, 6.806), (13.143, 6.594), (13.081, 6.385), (13.012, 6.179), (12.933, 5.975), (12.847, 5.775),
                (12.752, 5.579), (12.649, 5.387), (12.539, 5.199), (12.421, 5.016), (13.401, 4.036), (13.508, 3.877), (13.545, 3.689), (13.508, 3.501),
                (13.401, 3.342), (12.657, 2.599), (12.498, 2.492), (12.311, 2.455), (12.123, 2.492), (11.964, 2.599), (10.984, 3.579), (10.801, 3.461),
                (10.613, 3.35), (10.421, 3.248), (10.225, 3.153), (10.025, 3.066), (9.821, 2.988), (9.615, 2.918), (9.406, 2.857), (9.194, 2.805),
                (8.981, 2.761), (8.981, 1.378), (8.943, 1.19), (8.837, 1.031), (8.677, 0.925), (8.489, 0.889), (7.511, 0.889), (7.324, 0.926), (7.165, 1.032),
                (7.059, 1.191), (7.022, 1.378), (7.022, 2.761), (6.809, 2.805), (6.597, 2.857), (6.388, 2.918), (6.182, 2.988), (5.978, 3.066), (5.778, 3.153),
                (5.582, 3.248), (5.39, 3.35), (5.202, 3.461), (5.019, 3.579), (4.041, 2.599), (3.882, 2.492), (3.695, 2.455), (3.507, 2.492), (3.348, 2.599),
                (2.599, 3.342), (2.492, 3.501), (2.455, 3.689), (2.492, 3.877), (2.599, 4.036), (3.579, 5.016), (3.461, 5.199), (3.351, 5.387), (3.248, 5.579),
                (3.153, 5.775), (3.067, 5.975), (2.988, 6.179), (2.919, 6.385), (2.857, 6.594), (2.804, 6.806), (2.76, 7.019), (1.378, 7.019), (1.19, 7.057),
                (1.031, 7.163), (0.925, 7.323), (0.889, 7.511), (0.889, 8.489), (0.926, 8.676), (1.032, 8.835), (1.191, 8.941), (1.378, 8.978), (2.76, 8.978),
                (2.804, 9.191), (2.857, 9.403), (2.918, 9.612), (2.988, 9.818), (3.066, 10.022), (3.153, 10.222), (3.248, 10.418), (3.35, 10.61),
                (3.461, 10.798), (3.579, 10.981), (2.599, 11.964), (2.492, 12.123), (2.455, 12.311), (2.492, 12.498), (2.599, 12.657), (3.343, 13.401),
                (3.502, 13.508), (3.689, 13.545), (3.877, 13.508), (4.036, 13.401), (5.016, 12.421), (5.199, 12.539), (5.387, 12.65), (5.579, 12.752),
                (5.775, 12.847), (5.975, 12.934), (6.179, 13.012), (6.385, 13.082), (6.594, 13.143), (6.806, 13.195), (7.019, 13.239), (7.019, 14.622),
                (7.057, 14.81), (7.163, 14.969), (7.323, 15.075), (7.511, 15.111), (8.489, 15.111), (8.676, 15.074), (8.835, 14.968), (8.941, 14.809),
                (8.978, 14.622), (8.978, 13.239), (9.191, 13.195), (9.403, 13.143), (9.612, 13.082), (9.818, 13.012), (10.022, 12.934), (10.222, 12.847),
                (10.418, 12.752), (10.61, 12.65), (10.798, 12.539), (10.981, 12.421), (11.961, 13.401), (12.12, 13.508), (12.308, 13.545), (12.495, 13.508),
                (12.654, 13.401), (13.398, 12.657), (13.504, 12.498), (13.542, 12.311), (13.504, 12.123), (13.398, 11.964), (12.418, 10.984), (12.536, 10.801),
                (12.647, 10.613), (12.749, 10.421), (12.844, 10.225), (12.931, 10.025), (13.009, 9.821), (13.079, 9.615), (13.14, 9.406), (13.193, 9.194),
                (13.236, 8.981), (14.622, 8.981), (14.81, 8.943), (14.969, 8.837), (15.075, 8.677), (15.111, 8.489), (15.111, 7.511), (15.075, 7.323),
                (14.969, 7.163), (14.81, 7.057),
            ],
            &[
                (9.962, 8.0), (9.924, 8.383), (9.812, 8.751), (9.631, 9.09), (9.387, 9.387), (9.09, 9.631), (8.751, 9.812), (8.383, 9.924), (8.0, 9.962),
                (7.617, 9.924), (7.249, 9.812), (6.91, 9.631), (6.613, 9.387), (6.369, 9.09), (6.188, 8.751), (6.076, 8.383), (6.038, 8.0), (6.076, 7.617),
                (6.188, 7.249), (6.369, 6.91), (6.613, 6.613), (6.91, 6.369), (7.249, 6.188), (7.617, 6.076), (8.0, 6.038), (8.22, 6.051), (8.437, 6.087),
                (8.648, 6.148), (8.851, 6.232), (9.044, 6.339), (9.223, 6.466), (9.387, 6.613), (9.534, 6.777), (9.661, 6.956), (9.768, 7.149), (9.852, 7.352),
                (9.913, 7.563), (9.949, 7.78),
            ],
        ], &[
            79, 184, 183, 79, 183, 182, 79, 182, 181, 79, 181, 180, 79, 180, 179, 79, 179, 178, 185, 184, 79, 185, 79, 78, 185, 78, 77, 185, 77, 76, 185, 76,
            75, 185, 75, 74, 185, 74, 73, 185, 73, 72, 185, 72, 71, 185, 71, 70, 185, 70, 69, 185, 69, 68, 185, 68, 67, 185, 67, 66, 185, 66, 65, 185, 65, 64,
            64, 63, 62, 64, 62, 61, 64, 61, 60, 64, 60, 59, 64, 59, 58, 64, 58, 57, 64, 57, 56, 64, 56, 55, 64, 55, 54, 64, 54, 53, 64, 53, 52, 64, 52, 51, 64,
            51, 50, 64, 50, 49, 64, 49, 48, 64, 48, 47, 64, 47, 46, 64, 46, 45, 64, 45, 44, 64, 44, 43, 43, 42, 41, 43, 41, 40, 43, 40, 39, 43, 39, 38, 43, 38,
            37, 43, 37, 36, 43, 36, 35, 43, 35, 34, 43, 34, 33, 43, 33, 32, 43, 32, 31, 43, 31, 30, 43, 30, 29, 43, 29, 28, 43, 28, 27, 43, 27, 26, 43, 26, 25,
            43, 25, 24, 43, 24, 23, 43, 23, 22, 22, 21, 20, 22, 20, 19, 22, 19, 18, 22, 18, 17, 22, 17, 16, 22, 16, 15, 22, 15, 14, 22, 14, 13, 22, 13, 12, 22,
            12, 11, 22, 11, 10, 22, 10, 9, 22, 9, 8, 22, 8, 7, 22, 7, 6, 22, 6, 5, 22, 5, 4, 22, 4, 3, 22, 3, 2, 22, 2, 1, 1, 0, 167, 1, 167, 166, 1, 166, 165,
            1, 165, 164, 1, 164, 163, 1, 163, 162, 1, 162, 161, 1, 161, 160, 1, 160, 159, 1, 159, 158, 1, 158, 157, 1, 157, 156, 1, 156, 155, 1, 155, 154, 1,
            154, 153, 1, 153, 152, 1, 152, 151, 1, 151, 150, 1, 150, 149, 1, 149, 148, 148, 147, 146, 148, 146, 145, 148, 145, 144, 148, 144, 143, 148, 143,
            142, 148, 142, 141, 148, 141, 140, 148, 140, 139, 148, 139, 138, 148, 138, 137, 148, 137, 136, 148, 136, 135, 148, 135, 134, 148, 134, 133, 148,
            133, 132, 148, 132, 131, 148, 131, 130, 148, 130, 129, 148, 129, 128, 148, 128, 127, 127, 126, 125, 127, 125, 124, 127, 124, 123, 127, 123, 122,
            127, 122, 121, 127, 121, 120, 127, 120, 119, 127, 119, 118, 127, 118, 117, 127, 117, 116, 127, 116, 115, 127, 115, 114, 127, 114, 113, 127, 113,
            112, 127, 112, 111, 127, 111, 110, 127, 110, 109, 127, 109, 108, 127, 108, 107, 127, 107, 106, 106, 105, 104, 106, 104, 103, 106, 103, 102, 106,
            102, 101, 106, 101, 100, 106, 100, 99, 106, 99, 98, 106, 98, 97, 106, 97, 96, 106, 96, 95, 106, 95, 94, 106, 94, 93, 106, 93, 92, 106, 92, 91, 106,
            91, 90, 106, 90, 89, 106, 89, 88, 106, 88, 87, 106, 87, 86, 106, 86, 85, 85, 84, 83, 85, 83, 82, 85, 82, 81, 85, 81, 80, 85, 80, 79, 85, 79, 178,
            85, 178, 177, 186, 185, 64, 186, 64, 43, 43, 22, 1, 43, 1, 148, 148, 127, 106, 148, 106, 85, 148, 85, 177, 148, 177, 176, 148, 176, 175, 148, 175,
            174, 148, 174, 173, 148, 173, 172, 148, 172, 171, 148, 171, 170, 148, 170, 169, 148, 169, 168, 148, 168, 205, 148, 205, 204, 148, 204, 203, 148,
            203, 202, 148, 202, 201, 187, 186, 43, 43, 148, 201, 43, 201, 200, 43, 200, 199, 43, 199, 198, 43, 198, 197, 43, 197, 196, 43, 196, 195, 43, 195,
            194, 43, 194, 193, 43, 193, 192, 43, 192, 191, 43, 191, 190, 43, 190, 189, 43, 189, 188, 43, 188, 187,
        ]),
        Info => pen.glyph(1.0, &[
            &[
                (8.0, 0.889), (6.613, 1.026), (5.279, 1.43), (4.049, 2.087), (2.972, 2.972), (2.087, 4.049), (1.43, 5.279), (1.026, 6.613), (0.889, 8.0),
                (1.026, 9.387), (1.43, 10.721), (2.087, 11.951), (2.972, 13.028), (4.049, 13.913), (5.279, 14.57), (6.613, 14.974), (8.0, 15.111),
                (9.387, 14.974), (10.721, 14.57), (11.951, 13.913), (13.028, 13.028), (13.913, 11.951), (14.57, 10.721), (14.974, 9.387), (15.111, 8.0),
                (15.096, 7.535), (15.05, 7.072), (14.974, 6.613), (14.869, 6.16), (14.734, 5.714), (14.57, 5.279), (14.378, 4.855), (14.158, 4.444),
                (13.913, 4.049), (13.642, 3.671), (13.346, 3.311), (13.028, 2.972), (12.689, 2.654), (12.329, 2.358), (11.951, 2.087), (11.556, 1.842),
                (11.145, 1.622), (10.721, 1.43), (10.286, 1.266), (9.84, 1.131), (9.387, 1.026), (8.928, 0.95), (8.465, 0.904),
            ],
            &[
                (7.867, 2.8), (8.068, 2.806), (8.265, 2.845), (8.453, 2.916), (8.626, 3.018), (8.78, 3.146), (8.911, 3.299), (9.015, 3.471), (9.089, 3.658),
                (9.131, 3.854), (9.14, 4.055), (9.138, 4.259), (9.101, 4.46), (9.03, 4.652), (8.928, 4.828), (8.796, 4.985), (8.64, 5.116), (8.463, 5.218),
                (8.272, 5.289), (8.071, 5.326), (7.867, 5.328), (7.664, 5.323), (7.464, 5.284), (7.274, 5.212), (7.099, 5.109), (6.944, 4.978), (6.813, 4.822),
                (6.71, 4.647), (6.638, 4.457), (6.599, 4.258), (6.594, 4.055), (6.602, 3.854), (6.644, 3.657), (6.718, 3.47), (6.822, 3.298), (6.953, 3.146),
                (7.107, 3.017), (7.281, 2.916), (7.469, 2.845), (7.666, 2.806),
            ],
            &[
                (9.778, 12.0), (9.744, 12.17), (9.648, 12.314), (9.503, 12.411), (9.333, 12.444), (6.667, 12.444), (6.497, 12.411), (6.352, 12.314),
                (6.256, 12.17), (6.222, 12.0), (6.222, 11.111), (6.256, 10.941), (6.352, 10.797), (6.497, 10.7), (6.667, 10.667), (7.111, 10.667),
                (7.111, 8.0), (6.667, 8.0), (6.497, 7.966), (6.352, 7.87), (6.256, 7.726), (6.222, 7.556), (6.222, 6.667), (6.256, 6.497), (6.352, 6.352),
                (6.497, 6.256), (6.667, 6.222), (8.444, 6.222), (8.615, 6.256), (8.759, 6.352), (8.855, 6.497), (8.889, 6.667), (8.889, 10.667),
                (9.333, 10.667), (9.503, 10.7), (9.648, 10.797), (9.744, 10.941), (9.778, 11.111),
            ],
        ], &[
            6, 78, 77, 6, 77, 76, 6, 76, 75, 6, 75, 74, 6, 74, 73, 6, 73, 72, 6, 72, 71, 6, 71, 70, 6, 70, 69, 6, 69, 68, 79, 78, 6, 79, 6, 5, 79, 5, 4, 79, 4,
            3, 79, 3, 2, 79, 2, 1, 1, 0, 47, 1, 47, 46, 1, 46, 45, 1, 45, 44, 1, 44, 43, 1, 43, 42, 1, 42, 41, 1, 41, 40, 1, 40, 39, 1, 39, 38, 1, 38, 37, 1,
            37, 36, 1, 36, 35, 1, 35, 34, 1, 34, 33, 1, 33, 32, 1, 32, 31, 1, 31, 30, 1, 30, 29, 1, 29, 28, 1, 28, 27, 1, 27, 26, 1, 26, 25, 1, 25, 24, 24, 23,
            22, 24, 22, 21, 24, 21, 20, 24, 20, 19, 24, 19, 18, 24, 18, 17, 24, 17, 16, 24, 16, 15, 15, 14, 13, 15, 13, 12, 15, 12, 11, 15, 11, 10, 15, 10, 9,
            15, 9, 8, 8, 110, 109, 8, 109, 108, 8, 108, 107, 8, 107, 106, 8, 106, 105, 105, 104, 103, 105, 103, 102, 105, 102, 101, 105, 101, 100, 105, 100,
            99, 105, 99, 98, 121, 120, 119, 111, 110, 8, 111, 8, 7, 111, 7, 6, 80, 79, 1, 8, 105, 98, 8, 98, 97, 8, 97, 96, 8, 96, 95, 122, 121, 119, 112, 111,
            6, 112, 6, 68, 81, 80, 1, 15, 8, 95, 15, 95, 94, 15, 94, 93, 15, 93, 92, 15, 92, 91, 15, 91, 90, 123, 122, 119, 113, 112, 68, 113, 68, 67, 113, 67,
            66, 113, 66, 65, 82, 81, 1, 24, 15, 90, 24, 90, 89, 24, 89, 88, 24, 88, 125, 24, 125, 124, 24, 124, 123, 24, 123, 119, 24, 119, 118, 24, 118, 117,
            24, 117, 116, 114, 113, 65, 83, 82, 1, 115, 114, 65, 115, 65, 64, 115, 64, 63, 115, 63, 62, 115, 62, 61, 84, 83, 1, 116, 115, 61, 116, 61, 60, 85,
            84, 1, 24, 116, 60, 24, 60, 59, 24, 59, 58, 24, 58, 57, 24, 57, 56, 24, 56, 55, 24, 55, 54, 24, 54, 53, 24, 53, 52, 86, 85, 1, 1, 24, 52, 1, 52,
            51, 1, 51, 50, 1, 50, 49, 1, 49, 48, 1, 48, 87, 1, 87, 86,
        ]),
        Captions => {
            pen.glyph(1.0, &[
                &[
                    (0.727, 2.909), (0.727, 13.091), (0.755, 13.23), (0.834, 13.348), (0.952, 13.427), (1.091, 13.455), (14.909, 13.455), (15.048, 13.427),
                    (15.166, 13.348), (15.245, 13.23), (15.273, 13.091), (15.273, 2.909), (15.245, 2.77), (15.166, 2.652), (15.048, 2.573), (14.909, 2.545),
                    (1.091, 2.545), (0.952, 2.573), (0.834, 2.652), (0.755, 2.77),
                ],
                &[
                    (14.182, 12.364), (1.818, 12.364), (1.818, 3.636), (14.182, 3.636),
                ],
            ], &[
                0, 22, 21, 23, 22, 0, 23, 0, 19, 23, 19, 18, 23, 18, 17, 23, 17, 16, 23, 16, 15, 23, 15, 14, 23, 14, 13, 23, 13, 12, 23, 12, 11, 23, 11, 10,
                23, 10, 9, 23, 9, 8, 23, 8, 7, 23, 7, 6, 6, 5, 4, 6, 4, 3, 6, 3, 2, 6, 2, 1, 1, 0, 21, 1, 21, 20, 20, 23, 6, 20, 6, 1,
            ]);
            pen.glyph(1.0, &[
                &[
                    (5.636, 9.818), (5.706, 9.832), (5.765, 9.871), (5.804, 9.93), (5.818, 10.0), (5.818, 11.091), (5.804, 11.16), (5.765, 11.219),
                    (5.706, 11.259), (5.636, 11.273), (3.091, 11.273), (3.021, 11.259), (2.962, 11.219), (2.923, 11.16), (2.909, 11.091), (2.909, 10.0),
                    (2.923, 9.93), (2.962, 9.871), (3.021, 9.832), (3.091, 9.818),
                ],
            ], &[
                18, 19, 0, 18, 0, 1, 18, 1, 2, 18, 2, 3, 18, 3, 4, 18, 4, 5, 18, 5, 6, 18, 6, 7, 18, 7, 8, 18, 8, 9, 18, 9, 10, 18, 10, 11, 18, 11, 12, 18, 12,
                13, 18, 13, 14, 18, 14, 15, 18, 15, 16, 18, 16, 17,
            ]);
            pen.glyph(1.0, &[
                &[
                    (8.182, 9.818), (8.251, 9.832), (8.31, 9.871), (8.35, 9.93), (8.364, 10.0), (8.364, 11.091), (8.35, 11.16), (8.31, 11.219),
                    (8.251, 11.259), (8.182, 11.273), (7.091, 11.273), (7.021, 11.259), (6.962, 11.219), (6.923, 11.16), (6.909, 11.091), (6.909, 10.0),
                    (6.923, 9.93), (6.962, 9.871), (7.021, 9.832), (7.091, 9.818),
                ],
            ], &[
                18, 19, 0, 18, 0, 1, 18, 1, 2, 18, 2, 3, 18, 3, 4, 18, 4, 5, 18, 5, 6, 18, 6, 7, 18, 7, 8, 18, 8, 9, 18, 9, 10, 18, 10, 11, 18, 11, 12, 18, 12,
                13, 18, 13, 14, 18, 14, 15, 18, 15, 16, 18, 16, 17,
            ]);
            pen.glyph(1.0, &[
                &[
                    (12.909, 9.818), (12.979, 9.832), (13.038, 9.871), (13.077, 9.93), (13.091, 10.0), (13.091, 11.091), (13.077, 11.16), (13.038, 11.219),
                    (12.979, 11.259), (12.909, 11.273), (11.455, 11.273), (11.385, 11.259), (11.326, 11.219), (11.287, 11.16), (11.273, 11.091),
                    (11.273, 10.0), (11.287, 9.93), (11.326, 9.871), (11.385, 9.832), (11.455, 9.818),
                ],
            ], &[
                18, 19, 0, 18, 0, 1, 18, 1, 2, 18, 2, 3, 18, 3, 4, 18, 4, 5, 18, 5, 6, 18, 6, 7, 18, 7, 8, 18, 8, 9, 18, 9, 10, 18, 10, 11, 18, 11, 12, 18, 12,
                13, 18, 13, 14, 18, 14, 15, 18, 15, 16, 18, 16, 17,
            ]);
            pen.glyph(1.0, &[
                &[
                    (10.0, 9.818), (10.07, 9.832), (10.129, 9.871), (10.168, 9.93), (10.182, 10.0), (10.182, 11.091), (10.168, 11.16), (10.129, 11.219),
                    (10.07, 11.259), (10.0, 11.273), (9.636, 11.273), (9.567, 11.259), (9.508, 11.219), (9.468, 11.16), (9.455, 11.091), (9.455, 10.0),
                    (9.468, 9.93), (9.508, 9.871), (9.567, 9.832), (9.636, 9.818),
                ],
            ], &[
                18, 19, 0, 18, 0, 1, 18, 1, 2, 18, 2, 3, 18, 3, 4, 18, 4, 5, 18, 5, 6, 18, 6, 7, 18, 7, 8, 18, 8, 9, 18, 9, 10, 18, 10, 11, 18, 11, 12, 18, 12,
                13, 18, 13, 14, 18, 14, 15, 18, 15, 16, 18, 16, 17,
            ]);
        }
        Adjust => {
            pen.glyph(1.0, &[
                &[
                    (0.727, 2.909), (0.727, 13.091), (0.755, 13.23), (0.834, 13.348), (0.952, 13.427), (1.091, 13.455), (14.909, 13.455), (15.048, 13.427),
                    (15.166, 13.348), (15.245, 13.23), (15.273, 13.091), (15.273, 2.909), (15.245, 2.77), (15.166, 2.652), (15.048, 2.573), (14.909, 2.545),
                    (1.091, 2.545), (0.952, 2.573), (0.834, 2.652), (0.755, 2.77),
                ],
                &[
                    (14.182, 12.364), (1.818, 12.364), (1.818, 3.636), (14.182, 3.636),
                ],
            ], &[
                0, 22, 21, 23, 22, 0, 23, 0, 19, 23, 19, 18, 23, 18, 17, 23, 17, 16, 23, 16, 15, 23, 15, 14, 23, 14, 13, 23, 13, 12, 23, 12, 11, 23, 11, 10,
                23, 10, 9, 23, 9, 8, 23, 8, 7, 23, 7, 6, 6, 5, 4, 6, 4, 3, 6, 3, 2, 6, 2, 1, 1, 0, 21, 1, 21, 20, 20, 23, 6, 20, 6, 1,
            ]);
            pen.glyph(1.0, &[
                &[
                    (8.0, 4.364), (7.291, 4.434), (6.608, 4.64), (5.98, 4.976), (5.429, 5.429), (4.976, 5.98), (4.64, 6.608), (4.434, 7.291), (4.364, 8.0),
                    (4.434, 8.709), (4.64, 9.392), (4.976, 10.02), (5.429, 10.571), (5.98, 11.024), (6.608, 11.36), (7.291, 11.566), (8.0, 11.636),
                    (8.709, 11.566), (9.392, 11.36), (10.02, 11.024), (10.571, 10.571), (11.024, 10.02), (11.36, 9.392), (11.566, 8.709), (11.636, 8.0),
                    (11.629, 7.762), (11.605, 7.525), (11.566, 7.291), (11.512, 7.059), (11.443, 6.831), (11.36, 6.608), (11.261, 6.392), (11.149, 6.182),
                    (11.024, 5.98), (10.885, 5.786), (10.734, 5.602), (10.571, 5.429), (10.398, 5.266), (10.214, 5.115), (10.02, 4.976), (9.818, 4.851),
                    (9.608, 4.739), (9.392, 4.64), (9.169, 4.557), (8.941, 4.488), (8.709, 4.434), (8.475, 4.395), (8.238, 4.371),
                ],
                &[
                    (5.091, 8.0), (5.105, 7.715), (5.147, 7.433), (5.217, 7.156), (5.313, 6.887), (5.435, 6.63), (5.582, 6.385), (5.752, 6.156),
                    (5.944, 5.944), (6.155, 5.753), (6.384, 5.583), (6.629, 5.436), (6.887, 5.314), (7.156, 5.218), (7.432, 5.149), (7.715, 5.107),
                    (8.0, 5.093), (8.285, 5.107), (8.567, 5.149), (8.844, 5.218), (9.112, 5.314), (9.37, 5.436), (9.615, 5.583), (9.844, 5.752),
                    (10.056, 5.944), (5.944, 10.056), (5.823, 9.928), (5.711, 9.793), (5.606, 9.652), (5.511, 9.505), (5.424, 9.352), (5.347, 9.194),
                    (5.28, 9.031), (5.223, 8.865), (5.175, 8.696), (5.138, 8.524), (5.112, 8.351), (5.096, 8.176),
                ],
            ], &[
                8, 48, 85, 8, 85, 84, 8, 84, 83, 8, 83, 82, 8, 82, 81, 8, 81, 80, 8, 80, 79, 8, 79, 78, 8, 78, 77, 8, 77, 76, 8, 76, 75, 49, 48, 8, 49, 8, 7,
                49, 7, 6, 49, 6, 5, 49, 5, 4, 4, 3, 2, 4, 2, 1, 4, 1, 0, 4, 0, 47, 4, 47, 46, 4, 46, 45, 4, 45, 44, 4, 44, 43, 4, 43, 42, 4, 42, 41, 41, 40,
                39, 41, 39, 38, 41, 38, 37, 41, 37, 36, 41, 36, 35, 41, 35, 34, 41, 34, 33, 41, 33, 32, 41, 32, 31, 41, 31, 30, 41, 30, 29, 41, 29, 28, 41, 28,
                27, 41, 27, 26, 41, 26, 25, 41, 25, 24, 41, 24, 23, 41, 23, 22, 22, 21, 20, 22, 20, 19, 22, 19, 18, 22, 18, 17, 22, 17, 16, 22, 16, 15, 22, 15,
                14, 22, 14, 13, 22, 13, 12, 12, 11, 10, 12, 10, 9, 12, 9, 8, 12, 8, 75, 12, 75, 74, 12, 74, 73, 12, 73, 72, 50, 49, 4, 22, 12, 72, 51, 50, 4,
                41, 22, 72, 41, 72, 71, 41, 71, 70, 41, 70, 69, 41, 69, 68, 41, 68, 67, 41, 67, 66, 41, 66, 65, 41, 65, 64, 41, 64, 63, 41, 63, 62, 52, 51, 4,
                4, 41, 62, 4, 62, 61, 4, 61, 60, 4, 60, 59, 4, 59, 58, 4, 58, 57, 4, 57, 56, 4, 56, 55, 4, 55, 54, 4, 54, 53, 4, 53, 52,
            ]);
        }
        Nest => {
            pen.rect(1.5, 2.5, 14.5, 13.5);
            pen.rect(4.5, 5.5, 11.5, 10.5);
        }
        Undo => pen.glyph(1.0, &[
            &[
                (13.628, 5.574), (13.466, 5.439), (13.297, 5.312), (13.123, 5.193), (12.943, 5.082), (12.758, 4.979), (12.569, 4.884), (12.376, 4.799),
                (12.179, 4.722), (11.979, 4.654), (11.776, 4.596), (11.57, 4.547), (11.363, 4.507), (11.154, 4.477), (10.943, 4.457), (10.732, 4.446),
                (10.521, 4.444), (4.444, 4.444), (4.444, 2.0), (4.407, 1.876), (4.307, 1.794), (4.178, 1.782), (4.064, 1.844), (0.775, 5.177), (0.728, 5.249),
                (0.712, 5.333), (0.728, 5.417), (0.775, 5.488), (4.064, 8.822), (4.178, 8.883), (4.306, 8.872), (4.406, 8.79), (4.444, 8.667), (4.444, 6.222),
                (10.612, 6.222), (10.832, 6.228), (11.051, 6.251), (11.267, 6.292), (11.479, 6.35), (11.685, 6.425), (11.885, 6.516), (12.077, 6.623),
                (12.26, 6.745), (12.432, 6.881), (12.593, 7.031), (12.741, 7.194), (12.875, 7.367), (12.996, 7.551), (13.101, 7.744), (13.19, 7.945),
                (13.263, 8.152), (13.318, 8.365), (13.357, 8.581), (13.378, 8.8), (13.38, 9.017), (13.365, 9.234), (13.331, 9.449), (13.28, 9.66),
                (13.212, 9.867), (13.127, 10.066), (13.025, 10.259), (12.908, 10.442), (12.776, 10.614), (12.63, 10.776), (12.471, 10.924), (12.3, 11.059),
                (12.119, 11.179), (11.929, 11.284), (11.73, 11.372), (11.525, 11.444), (11.315, 11.499), (11.101, 11.536), (10.884, 11.555), (10.667, 11.556),
                (8.444, 11.556), (8.274, 11.589), (8.13, 11.686), (8.034, 11.83), (8.0, 12.0), (8.0, 12.889), (8.034, 13.059), (8.13, 13.203), (8.274, 13.3),
                (8.444, 13.333), (10.456, 13.333), (10.736, 13.33), (11.014, 13.309), (11.291, 13.271), (11.565, 13.217), (11.835, 13.146), (12.101, 13.059),
                (12.36, 12.956), (12.613, 12.837), (12.858, 12.703), (13.095, 12.554), (13.322, 12.392), (13.538, 12.215), (13.744, 12.026), (13.938, 11.825),
                (14.119, 11.613), (14.287, 11.39), (14.441, 11.157), (14.581, 10.915), (14.705, 10.665), (14.815, 10.408), (14.908, 10.144), (14.985, 9.876),
                (15.046, 9.603), (15.09, 9.328), (15.107, 9.108), (15.113, 8.888), (15.108, 8.668), (15.092, 8.449), (15.065, 8.23), (15.027, 8.013),
                (14.979, 7.799), (14.92, 7.587), (14.85, 7.378), (14.77, 7.173), (14.68, 6.972), (14.58, 6.776), (14.471, 6.585), (14.352, 6.4),
                (14.224, 6.221), (14.087, 6.048), (13.942, 5.883), (13.789, 5.724),
            ],
        ], &[
            1, 0, 126, 1, 126, 125, 1, 125, 124, 1, 124, 123, 1, 123, 122, 1, 122, 121, 1, 121, 120, 1, 120, 119, 1, 119, 118, 1, 118, 117, 1, 117, 116, 1,
            116, 115, 1, 115, 114, 1, 114, 113, 1, 113, 112, 1, 112, 111, 1, 111, 110, 1, 110, 109, 1, 109, 108, 1, 108, 107, 1, 107, 106, 1, 106, 105, 1, 105,
            104, 1, 104, 103, 1, 103, 102, 1, 102, 101, 1, 101, 100, 1, 100, 99, 1, 99, 98, 1, 98, 97, 1, 97, 96, 1, 96, 95, 95, 94, 93, 95, 93, 92, 95, 92,
            91, 95, 91, 90, 95, 90, 89, 95, 89, 88, 95, 88, 87, 95, 87, 86, 95, 86, 85, 95, 85, 84, 95, 84, 83, 95, 83, 82, 95, 82, 81, 95, 81, 80, 95, 80, 79,
            95, 79, 78, 95, 78, 77, 95, 77, 76, 95, 76, 75, 95, 75, 74, 95, 74, 73, 95, 73, 72, 95, 72, 71, 95, 71, 70, 95, 70, 69, 95, 69, 68, 95, 68, 67, 95,
            67, 66, 95, 66, 65, 95, 65, 64, 95, 64, 63, 95, 63, 62, 95, 62, 61, 95, 61, 60, 95, 60, 59, 95, 59, 58, 95, 58, 57, 95, 57, 56, 95, 56, 55, 95, 55,
            54, 33, 32, 31, 33, 31, 30, 33, 30, 29, 33, 29, 28, 33, 28, 27, 33, 27, 26, 33, 26, 25, 33, 25, 24, 33, 24, 23, 33, 23, 22, 33, 22, 21, 33, 21, 20,
            33, 20, 19, 19, 18, 17, 17, 16, 15, 17, 15, 14, 17, 14, 13, 17, 13, 12, 17, 12, 11, 17, 11, 10, 17, 10, 9, 17, 9, 8, 17, 8, 7, 17, 7, 6, 17, 6, 5,
            17, 5, 4, 17, 4, 3, 17, 3, 2, 17, 2, 1, 1, 95, 54, 1, 54, 53, 1, 53, 52, 1, 52, 51, 1, 51, 50, 1, 50, 49, 1, 49, 48, 1, 48, 47, 1, 47, 46, 1, 46,
            45, 1, 45, 44, 1, 44, 43, 1, 43, 42, 1, 42, 41, 1, 41, 40, 1, 40, 39, 1, 39, 38, 1, 38, 37, 1, 37, 36, 1, 36, 35, 1, 35, 34, 1, 34, 33, 33, 19, 17,
            33, 17, 1,
        ]),
        Redo => pen.glyph(1.0, &[
            &[
                (2.372, 5.574), (2.534, 5.439), (2.703, 5.312), (2.877, 5.193), (3.057, 5.082), (3.242, 4.979), (3.431, 4.884), (3.624, 4.799), (3.821, 4.722),
                (4.021, 4.654), (4.224, 4.596), (4.43, 4.547), (4.637, 4.507), (4.846, 4.477), (5.057, 4.457), (5.268, 4.446), (5.479, 4.444), (11.556, 4.444),
                (11.556, 2.0), (11.593, 1.876), (11.693, 1.794), (11.822, 1.782), (11.936, 1.844), (15.225, 5.177), (15.272, 5.249), (15.288, 5.333),
                (15.272, 5.417), (15.225, 5.488), (11.936, 8.822), (11.822, 8.883), (11.694, 8.872), (11.594, 8.79), (11.556, 8.667), (11.556, 6.222),
                (5.388, 6.222), (5.168, 6.228), (4.949, 6.251), (4.733, 6.292), (4.521, 6.35), (4.315, 6.425), (4.115, 6.516), (3.923, 6.623), (3.74, 6.745),
                (3.568, 6.881), (3.407, 7.031), (3.259, 7.194), (3.125, 7.367), (3.004, 7.551), (2.899, 7.744), (2.81, 7.945), (2.737, 8.152), (2.682, 8.365),
                (2.643, 8.581), (2.622, 8.8), (2.62, 9.017), (2.635, 9.234), (2.669, 9.449), (2.72, 9.66), (2.788, 9.867), (2.873, 10.066), (2.975, 10.259),
                (3.092, 10.442), (3.224, 10.614), (3.37, 10.776), (3.529, 10.924), (3.7, 11.059), (3.881, 11.179), (4.071, 11.284), (4.27, 11.372),
                (4.475, 11.444), (4.685, 11.499), (4.899, 11.536), (5.116, 11.555), (5.333, 11.556), (7.556, 11.556), (7.726, 11.589), (7.87, 11.686),
                (7.966, 11.83), (8.0, 12.0), (8.0, 12.889), (7.966, 13.059), (7.87, 13.203), (7.726, 13.3), (7.556, 13.333), (5.544, 13.333), (5.264, 13.33),
                (4.986, 13.309), (4.709, 13.271), (4.435, 13.217), (4.165, 13.146), (3.899, 13.059), (3.64, 12.956), (3.387, 12.837), (3.142, 12.703),
                (2.905, 12.554), (2.678, 12.392), (2.462, 12.215), (2.256, 12.026), (2.062, 11.825), (1.881, 11.613), (1.713, 11.39), (1.559, 11.157),
                (1.419, 10.915), (1.295, 10.665), (1.185, 10.408), (1.092, 10.144), (1.015, 9.876), (0.954, 9.603), (0.91, 9.328), (0.893, 9.108),
                (0.887, 8.888), (0.892, 8.668), (0.908, 8.449), (0.935, 8.23), (0.973, 8.013), (1.021, 7.799), (1.08, 7.587), (1.15, 7.378), (1.23, 7.173),
                (1.32, 6.972), (1.42, 6.776), (1.529, 6.585), (1.648, 6.4), (1.776, 6.221), (1.913, 6.048), (2.058, 5.883), (2.211, 5.724),
            ],
        ], &[
            125, 126, 0, 125, 0, 1, 125, 1, 2, 125, 2, 3, 125, 3, 4, 125, 4, 5, 125, 5, 6, 125, 6, 7, 125, 7, 8, 125, 8, 9, 125, 9, 10, 125, 10, 11, 125, 11,
            12, 125, 12, 13, 125, 13, 14, 125, 14, 15, 125, 15, 16, 125, 16, 17, 17, 18, 19, 17, 19, 20, 17, 20, 21, 17, 21, 22, 17, 22, 23, 17, 23, 24, 17,
            24, 25, 17, 25, 26, 17, 26, 27, 17, 27, 28, 17, 28, 29, 17, 29, 30, 17, 30, 31, 31, 32, 33, 73, 74, 75, 73, 75, 76, 73, 76, 77, 73, 77, 78, 73, 78,
            79, 73, 79, 80, 73, 80, 81, 73, 81, 82, 73, 82, 83, 73, 83, 84, 73, 84, 85, 73, 85, 86, 73, 86, 87, 73, 87, 88, 73, 88, 89, 73, 89, 90, 73, 90, 91,
            73, 91, 92, 73, 92, 93, 73, 93, 94, 73, 94, 95, 73, 95, 96, 73, 96, 97, 73, 97, 98, 73, 98, 99, 99, 100, 101, 99, 101, 102, 99, 102, 103, 99, 103,
            104, 99, 104, 105, 99, 105, 106, 99, 106, 107, 99, 107, 108, 99, 108, 109, 99, 109, 110, 99, 110, 111, 99, 111, 112, 99, 112, 113, 99, 113, 114,
            99, 114, 115, 99, 115, 116, 99, 116, 117, 99, 117, 118, 99, 118, 119, 99, 119, 120, 99, 120, 121, 99, 121, 122, 99, 122, 123, 99, 123, 124, 99,
            124, 125, 17, 31, 33, 17, 33, 34, 17, 34, 35, 17, 35, 36, 17, 36, 37, 72, 73, 99, 125, 17, 37, 125, 37, 38, 125, 38, 39, 125, 39, 40, 125, 40, 41,
            125, 41, 42, 125, 42, 43, 125, 43, 44, 125, 44, 45, 125, 45, 46, 125, 46, 47, 125, 47, 48, 125, 48, 49, 125, 49, 50, 125, 50, 51, 125, 51, 52, 125,
            52, 53, 125, 53, 54, 125, 54, 55, 125, 55, 56, 71, 72, 99, 99, 125, 56, 99, 56, 57, 99, 57, 58, 99, 58, 59, 99, 59, 60, 99, 60, 61, 99, 61, 62, 99,
            62, 63, 99, 63, 64, 99, 64, 65, 99, 65, 66, 99, 66, 67, 99, 67, 68, 99, 68, 69, 99, 69, 70, 99, 70, 71,
        ]),
        Bell => {
            pen.line(&[(3.0, 12.0), (13.0, 12.0)]);
            pen.line(&[(4.5, 12.0), (4.5, 7.0)]);
            pen.line(&[(11.5, 12.0), (11.5, 7.0)]);
            pen.arc(8.0, 7.0, 3.5, 180.0, 360.0);
            pen.dot(8.0, 13.8, 1.0);
        }
        Chat => pen.glyph(1.0, &[
            &[
                (2.667, 1.778), (2.452, 1.791), (2.241, 1.829), (2.036, 1.893), (1.84, 1.981), (1.657, 2.092), (1.488, 2.225), (1.336, 2.377), (1.204, 2.546),
                (1.093, 2.729), (1.004, 2.925), (0.941, 3.13), (0.902, 3.341), (0.889, 3.556), (0.889, 9.778), (0.902, 9.992), (0.941, 10.203),
                (1.004, 10.408), (1.093, 10.604), (1.204, 10.788), (1.336, 10.957), (1.488, 11.108), (1.657, 11.241), (1.84, 11.352), (2.036, 11.44),
                (2.241, 11.504), (2.452, 11.543), (2.667, 11.556), (3.556, 11.556), (3.556, 15.467), (3.593, 15.59), (3.693, 15.672), (3.821, 15.685),
                (3.935, 15.624), (8.0, 11.556), (13.333, 11.556), (13.548, 11.543), (13.759, 11.504), (13.964, 11.44), (14.16, 11.352), (14.343, 11.241),
                (14.512, 11.108), (14.664, 10.957), (14.796, 10.788), (14.907, 10.604), (14.996, 10.408), (15.059, 10.203), (15.098, 9.992), (15.111, 9.778),
                (15.111, 3.556), (15.098, 3.341), (15.059, 3.13), (14.996, 2.925), (14.907, 2.729), (14.796, 2.546), (14.664, 2.377), (14.512, 2.225),
                (14.343, 2.092), (14.16, 1.981), (13.964, 1.893), (13.759, 1.829), (13.548, 1.791), (13.333, 1.778),
            ],
        ], &[
            1, 0, 62, 1, 62, 61, 1, 61, 60, 1, 60, 59, 1, 59, 58, 1, 58, 57, 1, 57, 56, 1, 56, 55, 1, 55, 54, 1, 54, 53, 1, 53, 52, 1, 52, 51, 1, 51, 50, 1,
            50, 49, 1, 49, 48, 1, 48, 47, 1, 47, 46, 1, 46, 45, 1, 45, 44, 1, 44, 43, 1, 43, 42, 1, 42, 41, 1, 41, 40, 1, 40, 39, 1, 39, 38, 1, 38, 37, 1, 37,
            36, 1, 36, 35, 1, 35, 34, 34, 33, 32, 34, 32, 31, 34, 31, 30, 34, 30, 29, 34, 29, 28, 28, 27, 26, 28, 26, 25, 28, 25, 24, 28, 24, 23, 28, 23, 22,
            28, 22, 21, 28, 21, 20, 28, 20, 19, 28, 19, 18, 28, 18, 17, 28, 17, 16, 28, 16, 15, 28, 15, 14, 28, 14, 13, 28, 13, 12, 28, 12, 11, 28, 11, 10, 28,
            10, 9, 28, 9, 8, 28, 8, 7, 28, 7, 6, 28, 6, 5, 28, 5, 4, 28, 4, 3, 28, 3, 2, 28, 2, 1, 28, 1, 34,
        ]),
        Globe | Network => pen.glyph(1.0, &[
            &[
                (7.556, 0.0), (6.082, 0.145), (4.664, 0.575), (3.358, 1.273), (2.213, 2.213), (1.273, 3.358), (0.575, 4.664), (0.145, 6.082), (0.0, 7.556),
                (0.145, 9.03), (0.575, 10.447), (1.273, 11.753), (2.213, 12.898), (3.358, 13.838), (4.664, 14.536), (6.082, 14.966), (7.556, 15.111),
                (9.03, 14.966), (10.447, 14.536), (11.753, 13.838), (12.898, 12.898), (13.838, 11.753), (14.536, 10.447), (14.966, 9.03), (15.111, 7.556),
                (15.095, 7.061), (15.046, 6.569), (14.966, 6.082), (14.854, 5.6), (14.71, 5.127), (14.536, 4.664), (14.332, 4.214), (14.099, 3.778),
                (13.838, 3.358), (13.55, 2.956), (13.236, 2.574), (12.898, 2.213), (12.537, 1.875), (12.155, 1.561), (11.753, 1.273), (11.333, 1.012),
                (10.897, 0.779), (10.447, 0.575), (9.984, 0.401), (9.511, 0.257), (9.03, 0.145), (8.542, 0.065), (8.05, 0.016),
            ],
            &[
                (13.666, 7.111), (11.19, 7.111), (11.182, 6.904), (11.171, 6.696), (11.156, 6.489), (11.138, 6.282), (11.116, 6.076), (11.09, 5.87),
                (11.061, 5.664), (11.028, 5.459), (10.992, 5.255), (10.952, 5.051), (10.908, 4.848), (10.861, 4.646), (10.811, 4.444), (12.832, 4.444),
                (12.939, 4.633), (13.04, 4.825), (13.133, 5.021), (13.219, 5.22), (13.299, 5.422), (13.371, 5.626), (13.436, 5.833), (13.493, 6.042),
                (13.543, 6.253), (13.585, 6.466), (13.62, 6.68), (13.647, 6.895),
            ],
            &[
                (12.195, 3.556), (10.533, 3.556), (10.462, 3.363), (10.384, 3.173), (10.3, 2.986), (10.21, 2.801), (10.115, 2.619), (10.014, 2.441),
                (9.908, 2.265), (9.796, 2.093), (9.678, 1.925), (9.556, 1.76), (9.758, 1.834), (9.957, 1.916), (10.153, 2.004), (10.346, 2.099),
                (10.536, 2.201), (10.722, 2.309), (10.904, 2.424), (11.082, 2.545), (11.256, 2.672), (11.425, 2.806), (11.589, 2.945), (11.748, 3.089),
                (11.902, 3.24), (12.051, 3.395),
            ],
            &[
                (7.111, 8.0), (7.111, 10.667), (5.214, 10.667), (5.16, 10.466), (5.11, 10.264), (5.063, 10.061), (5.02, 9.858), (4.981, 9.654), (4.946, 9.449),
                (4.915, 9.243), (4.887, 9.037), (4.864, 8.83), (4.844, 8.623), (4.829, 8.416), (4.817, 8.208), (4.809, 8.0),
            ],
            &[
                (4.809, 7.111), (4.817, 6.903), (4.829, 6.696), (4.844, 6.488), (4.864, 6.281), (4.887, 6.074), (4.915, 5.868), (4.946, 5.663), (4.981, 5.458),
                (5.02, 5.253), (5.063, 5.05), (5.11, 4.847), (5.16, 4.645), (5.214, 4.444), (7.111, 4.444), (7.111, 7.111),
            ],
            &[
                (8.0, 8.0), (10.302, 8.0), (10.294, 8.208), (10.282, 8.416), (10.267, 8.623), (10.247, 8.83), (10.224, 9.037), (10.196, 9.243),
                (10.165, 9.449), (10.13, 9.654), (10.091, 9.858), (10.048, 10.061), (10.002, 10.264), (9.951, 10.466), (9.897, 10.667), (8.0, 10.667),
            ],
            &[
                (8.0, 7.111), (8.0, 4.444), (9.897, 4.444), (9.951, 4.645), (10.002, 4.847), (10.048, 5.05), (10.091, 5.253), (10.13, 5.458), (10.165, 5.663),
                (10.196, 5.868), (10.224, 6.074), (10.247, 6.281), (10.267, 6.488), (10.282, 6.696), (10.294, 6.903), (10.302, 7.111),
            ],
            &[
                (9.586, 3.556), (8.0, 3.556), (8.0, 1.608), (8.289, 1.765), (8.574, 1.993), (8.849, 2.288), (9.113, 2.65), (9.359, 3.073),
            ],
            &[
                (7.111, 1.608), (7.111, 3.556), (5.525, 3.556), (5.752, 3.073), (5.999, 2.65), (6.262, 2.288), (6.537, 1.993), (6.822, 1.765),
            ],
            &[
                (5.556, 1.76), (5.433, 1.925), (5.316, 2.093), (5.204, 2.265), (5.098, 2.441), (4.997, 2.619), (4.902, 2.801), (4.813, 2.986), (4.729, 3.173),
                (4.652, 3.363), (4.58, 3.556), (2.916, 3.556), (3.06, 3.395), (3.209, 3.24), (3.363, 3.089), (3.522, 2.945), (3.687, 2.806), (3.856, 2.672),
                (4.029, 2.545), (4.207, 2.424), (4.389, 2.309), (4.575, 2.201), (4.765, 2.099), (4.958, 2.004), (5.154, 1.916), (5.354, 1.834),
            ],
            &[
                (2.279, 4.444), (4.3, 4.444), (4.25, 4.646), (4.203, 4.848), (4.159, 5.051), (4.12, 5.255), (4.083, 5.459), (4.05, 5.664), (4.021, 5.87),
                (3.996, 6.076), (3.973, 6.282), (3.955, 6.489), (3.94, 6.696), (3.929, 6.904), (3.921, 7.111), (1.445, 7.111), (1.464, 6.895), (1.491, 6.68),
                (1.526, 6.466), (1.568, 6.253), (1.618, 6.042), (1.676, 5.833), (1.74, 5.626), (1.812, 5.422), (1.892, 5.22), (1.978, 5.021), (2.072, 4.825),
                (2.172, 4.633),
            ],
            &[
                (1.445, 8.0), (3.921, 8.0), (3.929, 8.207), (3.94, 8.415), (3.955, 8.622), (3.973, 8.829), (3.996, 9.035), (4.021, 9.241), (4.05, 9.447),
                (4.083, 9.652), (4.12, 9.856), (4.159, 10.06), (4.203, 10.263), (4.25, 10.465), (4.3, 10.667), (2.279, 10.667), (2.172, 10.478),
                (2.072, 10.286), (1.978, 10.09), (1.892, 9.891), (1.812, 9.69), (1.74, 9.485), (1.676, 9.278), (1.618, 9.069), (1.568, 8.858), (1.526, 8.645),
                (1.491, 8.431), (1.464, 8.216),
            ],
            &[
                (2.916, 11.556), (4.58, 11.556), (4.652, 11.748), (4.729, 11.938), (4.813, 12.125), (4.902, 12.31), (4.997, 12.492), (5.098, 12.67),
                (5.204, 12.846), (5.316, 13.018), (5.433, 13.186), (5.556, 13.351), (5.354, 13.277), (5.154, 13.195), (4.958, 13.107), (4.765, 13.012),
                (4.575, 12.91), (4.389, 12.802), (4.207, 12.687), (4.029, 12.566), (3.856, 12.439), (3.687, 12.305), (3.522, 12.166), (3.363, 12.022),
                (3.209, 11.872), (3.06, 11.716),
            ],
            &[
                (5.525, 11.556), (7.111, 11.556), (7.111, 13.503), (6.822, 13.346), (6.537, 13.119), (6.262, 12.823), (5.999, 12.461), (5.752, 12.038),
            ],
            &[
                (8.0, 13.503), (8.0, 11.556), (9.586, 11.556), (9.359, 12.038), (9.113, 12.461), (8.849, 12.823), (8.574, 13.119), (8.289, 13.346),
            ],
            &[
                (9.556, 13.351), (9.678, 13.186), (9.796, 13.018), (9.908, 12.846), (10.014, 12.671), (10.115, 12.492), (10.21, 12.31), (10.3, 12.125),
                (10.384, 11.938), (10.462, 11.748), (10.533, 11.556), (12.197, 11.556), (12.053, 11.716), (11.904, 11.872), (11.75, 12.022), (11.591, 12.167),
                (11.426, 12.306), (11.257, 12.439), (11.083, 12.566), (10.905, 12.687), (10.723, 12.802), (10.537, 12.91), (10.347, 13.012), (10.154, 13.107),
                (9.957, 13.195), (9.758, 13.277),
            ],
            &[
                (12.832, 10.667), (10.811, 10.667), (10.861, 10.465), (10.908, 10.263), (10.952, 10.06), (10.992, 9.856), (11.028, 9.652), (11.061, 9.447),
                (11.09, 9.241), (11.116, 9.035), (11.138, 8.829), (11.156, 8.622), (11.171, 8.415), (11.182, 8.207), (11.19, 8.0), (13.666, 8.0),
                (13.647, 8.216), (13.62, 8.431), (13.585, 8.645), (13.543, 8.858), (13.493, 9.069), (13.436, 9.278), (13.371, 9.485), (13.299, 9.69),
                (13.219, 9.891), (13.133, 10.09), (13.04, 10.286), (12.939, 10.478),
            ],
        ], &[
            346, 345, 344, 346, 344, 343, 346, 343, 342, 346, 342, 341, 346, 341, 340, 346, 340, 339, 346, 339, 338, 346, 338, 337, 346, 337, 336, 346, 336,
            335, 346, 335, 334, 346, 334, 333, 346, 333, 148, 346, 148, 147, 346, 147, 146, 346, 146, 145, 346, 145, 144, 346, 144, 143, 346, 143, 142, 346,
            142, 141, 346, 141, 140, 346, 140, 139, 346, 139, 138, 346, 138, 137, 346, 137, 136, 346, 136, 135, 135, 236, 8, 135, 8, 223, 135, 223, 133, 133,
            132, 151, 133, 151, 150, 153, 152, 62, 153, 62, 61, 153, 61, 60, 153, 60, 59, 153, 59, 58, 153, 58, 57, 153, 57, 56, 153, 56, 55, 153, 55, 54, 153,
            54, 53, 153, 53, 52, 153, 52, 51, 153, 51, 50, 153, 50, 49, 119, 118, 222, 119, 222, 221, 119, 221, 220, 119, 220, 219, 119, 219, 218, 119, 218,
            217, 119, 217, 216, 119, 216, 215, 119, 215, 214, 119, 214, 213, 119, 213, 212, 119, 212, 211, 119, 211, 210, 119, 210, 209, 224, 223, 8, 224, 8,
            7, 224, 7, 6, 6, 193, 175, 87, 86, 85, 87, 85, 84, 87, 84, 83, 87, 83, 82, 87, 82, 81, 87, 81, 80, 87, 80, 79, 87, 79, 78, 87, 78, 77, 177, 176,
            192, 177, 192, 191, 177, 191, 190, 177, 190, 189, 177, 189, 188, 177, 188, 187, 177, 187, 186, 177, 186, 185, 177, 185, 184, 177, 184, 183, 177,
            183, 182, 194, 193, 6, 194, 6, 5, 194, 5, 4, 194, 4, 3, 194, 3, 2, 194, 2, 1, 1, 0, 47, 1, 47, 46, 1, 46, 45, 1, 45, 44, 1, 44, 43, 1, 43, 42, 1,
            42, 41, 1, 41, 40, 1, 40, 39, 1, 39, 38, 1, 38, 37, 1, 37, 36, 1, 36, 35, 1, 35, 34, 1, 34, 33, 1, 33, 32, 32, 31, 30, 32, 30, 29, 32, 29, 28, 32,
            28, 27, 32, 27, 26, 32, 26, 25, 32, 25, 24, 32, 24, 23, 32, 23, 22, 32, 22, 21, 21, 20, 19, 21, 19, 18, 21, 18, 17, 21, 17, 16, 21, 16, 15, 15, 14,
            13, 15, 13, 12, 15, 12, 11, 15, 11, 10, 10, 264, 289, 10, 289, 288, 10, 288, 287, 10, 287, 286, 10, 286, 285, 10, 285, 284, 10, 284, 283, 10, 283,
            282, 10, 282, 281, 275, 274, 273, 275, 273, 272, 275, 272, 271, 275, 271, 270, 275, 270, 269, 275, 269, 268, 275, 268, 267, 275, 267, 266, 275,
            266, 265, 275, 265, 290, 275, 290, 297, 275, 297, 296, 275, 296, 295, 275, 295, 294, 275, 294, 293, 275, 293, 292, 292, 291, 299, 292, 299, 298,
            316, 315, 314, 316, 314, 313, 316, 313, 312, 316, 312, 311, 316, 311, 310, 316, 310, 309, 316, 309, 308, 316, 308, 307, 264, 10, 9, 264, 9, 8, 8,
            236, 263, 8, 263, 262, 8, 262, 261, 8, 261, 260, 8, 260, 259, 8, 259, 258, 8, 258, 257, 8, 257, 256, 8, 256, 255, 8, 255, 254, 8, 254, 253, 8, 253,
            252, 8, 252, 251, 250, 249, 248, 250, 248, 247, 250, 247, 246, 250, 246, 245, 250, 245, 244, 250, 244, 243, 250, 243, 242, 250, 242, 241, 250, 241,
            240, 250, 240, 239, 250, 239, 238, 250, 238, 237, 250, 237, 117, 250, 117, 116, 250, 116, 115, 250, 115, 114, 250, 114, 113, 250, 113, 112, 250,
            112, 111, 250, 111, 110, 250, 110, 109, 250, 109, 108, 250, 108, 107, 250, 107, 106, 250, 106, 105, 250, 105, 104, 103, 102, 134, 103, 134, 149,
            346, 135, 133, 346, 133, 150, 346, 150, 165, 346, 165, 164, 346, 164, 163, 346, 163, 162, 346, 162, 161, 346, 161, 160, 346, 160, 159, 346, 159,
            158, 346, 158, 157, 346, 157, 156, 346, 156, 155, 346, 155, 154, 346, 154, 153, 346, 153, 49, 346, 49, 48, 120, 119, 209, 225, 224, 6, 178, 177,
            182, 195, 194, 1, 15, 10, 281, 15, 281, 280, 15, 280, 279, 15, 279, 278, 15, 278, 277, 15, 277, 276, 15, 276, 275, 15, 275, 292, 15, 292, 298, 15,
            298, 306, 15, 306, 331, 15, 331, 330, 15, 330, 329, 316, 307, 306, 306, 298, 305, 306, 305, 304, 306, 304, 303, 306, 303, 302, 306, 302, 301, 306,
            301, 300, 264, 8, 251, 264, 251, 250, 264, 250, 104, 264, 104, 103, 264, 103, 149, 264, 149, 332, 347, 346, 48, 121, 120, 209, 226, 225, 6, 179,
            178, 182, 196, 195, 1, 21, 15, 329, 21, 329, 328, 21, 328, 327, 21, 327, 326, 21, 326, 325, 21, 325, 324, 21, 324, 323, 21, 323, 322, 21, 322, 321,
            21, 321, 320, 21, 320, 319, 21, 319, 318, 21, 318, 317, 316, 306, 300, 300, 264, 332, 122, 121, 209, 227, 226, 6, 180, 179, 182, 197, 196, 1, 316,
            300, 332, 123, 122, 209, 228, 227, 6, 181, 180, 182, 198, 197, 1, 317, 316, 332, 124, 123, 209, 229, 228, 6, 174, 181, 182, 199, 198, 1, 21, 317,
            332, 21, 332, 359, 21, 359, 358, 21, 358, 357, 21, 357, 356, 21, 356, 355, 21, 355, 354, 21, 354, 353, 21, 353, 352, 21, 352, 351, 21, 351, 350,
            21, 350, 349, 21, 349, 348, 21, 348, 347, 21, 347, 48, 125, 124, 209, 230, 229, 6, 200, 199, 1, 32, 21, 48, 32, 48, 75, 32, 75, 74, 32, 74, 73, 32,
            73, 72, 32, 72, 71, 32, 71, 70, 32, 70, 69, 32, 69, 68, 32, 68, 67, 32, 67, 66, 32, 66, 65, 32, 65, 64, 32, 64, 63, 32, 63, 131, 126, 125, 209,
            231, 230, 6, 201, 200, 1, 127, 126, 209, 232, 231, 6, 202, 201, 1, 128, 127, 209, 233, 232, 6, 203, 202, 1, 129, 128, 209, 234, 233, 6, 204, 203,
            1, 130, 129, 209, 235, 234, 6, 205, 204, 1, 131, 130, 209, 208, 235, 6, 208, 6, 175, 206, 205, 1, 32, 131, 209, 32, 209, 208, 32, 208, 175, 207,
            206, 1, 182, 207, 1, 174, 182, 1, 168, 174, 1, 175, 174, 168, 175, 168, 167, 87, 168, 1, 32, 175, 167, 32, 167, 166, 169, 168, 87, 169, 87, 77, 88,
            87, 1, 170, 169, 77, 89, 88, 1, 171, 170, 77, 90, 89, 1, 90, 1, 32, 172, 171, 77, 91, 90, 32, 173, 172, 77, 92, 91, 32, 166, 173, 77, 93, 92, 32,
            32, 166, 77, 32, 77, 76, 32, 76, 101, 32, 101, 100, 32, 100, 99, 32, 99, 98, 32, 98, 97, 32, 97, 96, 32, 96, 95, 32, 95, 94, 32, 94, 93,
        ]),
        Code => {
            pen.line(&[(5.0, 4.0), (1.5, 8.0), (5.0, 12.0)]);
            pen.line(&[(11.0, 4.0), (14.5, 8.0), (11.0, 12.0)]);
            pen.line(&[(9.5, 2.5), (6.5, 13.5)]);
        }
        Sparkle => {
            pen.fill(&[(8.0, 1.5), (9.5, 6.5), (14.5, 8.0), (9.5, 9.5), (8.0, 14.5), (6.5, 9.5), (1.5, 8.0), (6.5, 6.5)]);
        }
        Grid => {
            pen.glyph(1.0, &[
                &[
                    (6.667, 0.889), (6.837, 0.923), (6.981, 1.019), (7.077, 1.163), (7.111, 1.333), (7.111, 6.667), (7.077, 6.837), (6.981, 6.981),
                    (6.837, 7.077), (6.667, 7.111), (1.333, 7.111), (1.163, 7.077), (1.019, 6.981), (0.923, 6.837), (0.889, 6.667), (0.889, 1.333),
                    (0.923, 1.163), (1.019, 1.019), (1.163, 0.923), (1.333, 0.889),
                ],
            ], &[
                18, 19, 0, 18, 0, 1, 18, 1, 2, 18, 2, 3, 18, 3, 4, 18, 4, 5, 18, 5, 6, 18, 6, 7, 18, 7, 8, 18, 8, 9, 18, 9, 10, 18, 10, 11, 18, 11, 12, 18, 12,
                13, 18, 13, 14, 18, 14, 15, 18, 15, 16, 18, 16, 17,
            ]);
            pen.glyph(1.0, &[
                &[
                    (14.667, 0.889), (14.837, 0.923), (14.981, 1.019), (15.077, 1.163), (15.111, 1.333), (15.111, 6.667), (15.077, 6.837), (14.981, 6.981),
                    (14.837, 7.077), (14.667, 7.111), (9.333, 7.111), (9.163, 7.077), (9.019, 6.981), (8.923, 6.837), (8.889, 6.667), (8.889, 1.333),
                    (8.923, 1.163), (9.019, 1.019), (9.163, 0.923), (9.333, 0.889),
                ],
            ], &[
                18, 19, 0, 18, 0, 1, 18, 1, 2, 18, 2, 3, 18, 3, 4, 18, 4, 5, 18, 5, 6, 18, 6, 7, 18, 7, 8, 18, 8, 9, 18, 9, 10, 18, 10, 11, 18, 11, 12, 18, 12,
                13, 18, 13, 14, 18, 14, 15, 18, 15, 16, 18, 16, 17,
            ]);
            pen.glyph(1.0, &[
                &[
                    (6.667, 8.889), (6.837, 8.923), (6.981, 9.019), (7.077, 9.163), (7.111, 9.333), (7.111, 14.667), (7.077, 14.837), (6.981, 14.981),
                    (6.837, 15.077), (6.667, 15.111), (1.333, 15.111), (1.163, 15.077), (1.019, 14.981), (0.923, 14.837), (0.889, 14.667), (0.889, 9.333),
                    (0.923, 9.163), (1.019, 9.019), (1.163, 8.923), (1.333, 8.889),
                ],
            ], &[
                18, 19, 0, 18, 0, 1, 18, 1, 2, 18, 2, 3, 18, 3, 4, 18, 4, 5, 18, 5, 6, 18, 6, 7, 18, 7, 8, 18, 8, 9, 18, 9, 10, 18, 10, 11, 18, 11, 12, 18, 12,
                13, 18, 13, 14, 18, 14, 15, 18, 15, 16, 18, 16, 17,
            ]);
            pen.glyph(1.0, &[
                &[
                    (14.667, 8.889), (14.837, 8.923), (14.981, 9.019), (15.077, 9.163), (15.111, 9.333), (15.111, 14.667), (15.077, 14.837), (14.981, 14.981),
                    (14.837, 15.077), (14.667, 15.111), (9.333, 15.111), (9.163, 15.077), (9.019, 14.981), (8.923, 14.837), (8.889, 14.667), (8.889, 9.333),
                    (8.923, 9.163), (9.019, 9.019), (9.163, 8.923), (9.333, 8.889),
                ],
            ], &[
                18, 19, 0, 18, 0, 1, 18, 1, 2, 18, 2, 3, 18, 3, 4, 18, 4, 5, 18, 5, 6, 18, 6, 7, 18, 7, 8, 18, 8, 9, 18, 9, 10, 18, 10, 11, 18, 11, 12, 18, 12,
                13, 18, 13, 14, 18, 14, 15, 18, 15, 16, 18, 16, 17,
            ]);
        }
        Square => pen.rect(3.0, 3.0, 13.0, 13.0),
        Proxy => {
            pen.glyph(1.0, &[
                &[
                    (15.273, 10.545), (15.273, 13.818), (15.245, 13.957), (15.166, 14.075), (15.048, 14.154), (14.909, 14.182), (9.818, 14.182),
                    (9.679, 14.154), (9.561, 14.075), (9.482, 13.957), (9.455, 13.818), (9.455, 10.545), (9.482, 10.406), (9.561, 10.288), (9.679, 10.209),
                    (9.818, 10.182), (14.909, 10.182), (15.048, 10.209), (15.166, 10.288), (15.245, 10.406),
                ],
            ], &[
                18, 19, 0, 18, 0, 1, 18, 1, 2, 18, 2, 3, 18, 3, 4, 18, 4, 5, 18, 5, 6, 18, 6, 7, 18, 7, 8, 18, 8, 9, 18, 9, 10, 18, 10, 11, 18, 11, 12, 18, 12,
                13, 18, 13, 14, 18, 14, 15, 18, 15, 16, 18, 16, 17,
            ]);
            pen.glyph(1.0, &[
                &[
                    (8.0, 2.182), (0.364, 2.182), (0.224, 2.209), (0.107, 2.288), (0.028, 2.406), (0.0, 2.545), (0.0, 7.94), (0.031, 7.885), (0.064, 7.832),
                    (0.1, 7.78), (0.138, 7.731), (2.545, 4.841), (4.953, 7.731), (5.055, 7.873), (5.137, 8.028), (5.197, 8.192), (5.233, 8.364), (8.0, 8.364),
                    (8.139, 8.336), (8.257, 8.257), (8.336, 8.139), (8.364, 8.0), (8.364, 2.545), (8.336, 2.406), (8.257, 2.288), (8.139, 2.209),
                ],
            ], &[
                1, 0, 25, 1, 25, 24, 1, 24, 23, 1, 23, 22, 1, 22, 21, 1, 21, 20, 1, 20, 19, 1, 19, 18, 1, 18, 17, 17, 16, 15, 17, 15, 14, 17, 14, 13, 17, 13,
                12, 17, 12, 11, 7, 6, 5, 7, 5, 4, 7, 4, 3, 7, 3, 2, 7, 2, 1, 1, 17, 11, 1, 11, 10, 1, 10, 9, 1, 9, 8, 1, 8, 7,
            ]);
            pen.glyph(1.0, &[
                &[
                    (3.091, 8.727), (3.975, 8.727), (4.073, 8.699), (4.14, 8.623), (4.156, 8.522), (4.115, 8.429), (2.545, 6.545), (0.976, 8.429),
                    (0.935, 8.522), (0.951, 8.623), (1.018, 8.699), (1.115, 8.727), (2.0, 8.727), (2.011, 9.048), (2.043, 9.368), (2.095, 9.685),
                    (2.168, 9.997), (2.262, 10.305), (2.375, 10.605), (2.508, 10.898), (2.659, 11.181), (2.829, 11.453), (3.016, 11.714), (3.22, 11.963),
                    (3.439, 12.197), (3.674, 12.416), (3.922, 12.62), (4.183, 12.807), (4.456, 12.977), (4.739, 13.129), (5.031, 13.261), (5.332, 13.375),
                    (5.639, 13.468), (5.952, 13.541), (6.269, 13.594), (6.588, 13.626), (6.909, 13.636), (6.909, 12.545), (6.659, 12.537), (6.411, 12.512),
                    (6.164, 12.471), (5.921, 12.415), (5.682, 12.342), (5.449, 12.254), (5.221, 12.151), (5.001, 12.033), (4.789, 11.901), (4.586, 11.755),
                    (4.393, 11.597), (4.21, 11.426), (4.04, 11.244), (3.881, 11.051), (3.736, 10.848), (3.604, 10.636), (3.486, 10.415), (3.383, 10.188),
                    (3.294, 9.954), (3.222, 9.715), (3.165, 9.472), (3.124, 9.225), (3.099, 8.977),
                ],
            ], &[
                38, 37, 36, 38, 36, 35, 38, 35, 34, 38, 34, 33, 38, 33, 32, 38, 32, 31, 38, 31, 30, 38, 30, 29, 38, 29, 28, 38, 28, 27, 38, 27, 26, 38, 26, 25,
                25, 24, 23, 25, 23, 22, 25, 22, 21, 25, 21, 20, 25, 20, 19, 25, 19, 18, 25, 18, 17, 25, 17, 16, 25, 16, 15, 25, 15, 14, 25, 14, 13, 25, 13, 12,
                12, 11, 10, 12, 10, 9, 12, 9, 8, 12, 8, 7, 12, 7, 6, 12, 6, 5, 12, 5, 4, 12, 4, 3, 12, 3, 2, 2, 1, 0, 39, 38, 25, 12, 2, 0, 12, 0, 60, 12, 60,
                59, 12, 59, 58, 12, 58, 57, 12, 57, 56, 12, 56, 55, 12, 55, 54, 12, 54, 53, 12, 53, 52, 12, 52, 51, 40, 39, 25, 25, 12, 51, 25, 51, 50, 25, 50,
                49, 25, 49, 48, 25, 48, 47, 25, 47, 46, 25, 46, 45, 25, 45, 44, 25, 44, 43, 25, 43, 42, 25, 42, 41, 25, 41, 40,
            ]);
            pen.glyph(1.0, &[
                &[
                    (15.612, 7.273), (14.727, 7.273), (14.716, 6.952), (14.685, 6.632), (14.632, 6.315), (14.559, 6.003), (14.465, 5.695), (14.352, 5.395),
                    (14.219, 5.102), (14.068, 4.819), (13.898, 4.547), (13.711, 4.286), (13.507, 4.037), (13.288, 3.803), (13.053, 3.584), (12.805, 3.38),
                    (12.544, 3.193), (12.272, 3.023), (11.988, 2.871), (11.696, 2.739), (11.395, 2.625), (11.088, 2.532), (10.776, 2.459), (10.459, 2.406),
                    (10.139, 2.374), (9.818, 2.364), (9.818, 3.455), (10.068, 3.463), (10.316, 3.488), (10.563, 3.529), (10.806, 3.585), (11.045, 3.658),
                    (11.279, 3.746), (11.506, 3.849), (11.727, 3.967), (11.939, 4.099), (12.142, 4.245), (12.335, 4.403), (12.517, 4.574), (12.688, 4.756),
                    (12.846, 4.949), (12.992, 5.152), (13.124, 5.364), (13.242, 5.585), (13.345, 5.812), (13.433, 6.046), (13.506, 6.285), (13.562, 6.528),
                    (13.603, 6.775), (13.628, 7.023), (13.636, 7.273), (12.752, 7.273), (12.654, 7.301), (12.587, 7.377), (12.572, 7.478), (12.612, 7.571),
                    (14.182, 9.455), (15.752, 7.571), (15.792, 7.478), (15.777, 7.377), (15.709, 7.301),
                ],
            ], &[
                1, 0, 60, 1, 60, 59, 1, 59, 58, 1, 58, 57, 1, 57, 56, 1, 56, 55, 1, 55, 54, 1, 54, 53, 1, 53, 52, 52, 51, 50, 27, 26, 25, 27, 25, 24, 27, 24,
                23, 27, 23, 22, 27, 22, 21, 27, 21, 20, 27, 20, 19, 27, 19, 18, 27, 18, 17, 27, 17, 16, 27, 16, 15, 27, 15, 14, 14, 13, 12, 14, 12, 11, 14, 11,
                10, 14, 10, 9, 14, 9, 8, 14, 8, 7, 14, 7, 6, 14, 6, 5, 14, 5, 4, 14, 4, 3, 14, 3, 2, 14, 2, 1, 1, 52, 50, 1, 50, 49, 1, 49, 48, 1, 48, 47, 1,
                47, 46, 1, 46, 45, 1, 45, 44, 1, 44, 43, 1, 43, 42, 1, 42, 41, 1, 41, 40, 28, 27, 14, 14, 1, 40, 14, 40, 39, 14, 39, 38, 14, 38, 37, 14, 37,
                36, 14, 36, 35, 14, 35, 34, 14, 34, 33, 14, 33, 32, 14, 32, 31, 14, 31, 30, 14, 30, 29, 14, 29, 28,
            ]);
        }
        Offline => {
            pen.glyph(1.0, &[
                &[
                    (8.364, 6.182), (8.391, 6.321), (8.47, 6.439), (8.588, 6.518), (8.727, 6.545), (13.091, 6.545), (13.091, 13.818), (13.063, 13.957),
                    (12.984, 14.075), (12.866, 14.154), (12.727, 14.182), (3.636, 14.182), (3.497, 14.154), (3.379, 14.075), (3.3, 13.957), (3.273, 13.818),
                    (3.273, 2.182), (3.3, 2.043), (3.379, 1.925), (3.497, 1.846), (3.636, 1.818), (8.364, 1.818),
                ],
                &[
                    (8.083, 13.427), (7.905, 13.403), (7.738, 13.333), (7.596, 13.223), (7.487, 13.08), (7.418, 12.913), (7.395, 12.735), (7.419, 12.556),
                    (7.488, 12.39), (7.598, 12.247), (7.741, 12.138), (7.907, 12.069), (8.085, 12.045), (8.264, 12.068), (8.43, 12.137), (8.573, 12.246),
                    (8.683, 12.389), (8.753, 12.555), (8.777, 12.733), (8.77, 12.892), (8.725, 13.044), (8.643, 13.181), (8.531, 13.293), (8.394, 13.375),
                    (8.242, 13.42),
                ],
                &[
                    (7.745, 11.475), (7.702, 11.474), (7.66, 11.465), (7.62, 11.448), (7.584, 11.423), (7.482, 11.274), (7.406, 11.109), (7.36, 10.934),
                    (7.344, 10.754), (7.368, 10.538), (7.436, 10.341), (7.543, 10.155), (7.684, 9.974), (7.855, 9.789), (8.049, 9.593), (8.188, 9.451),
                    (8.299, 9.328), (8.384, 9.216), (8.443, 9.11), (8.477, 9.003), (8.489, 8.89), (8.47, 8.747), (8.413, 8.63), (8.318, 8.539), (8.185, 8.474),
                    (8.015, 8.435), (7.807, 8.421), (7.646, 8.427), (7.485, 8.444), (7.326, 8.474), (7.169, 8.515), (7.016, 8.567), (6.868, 8.631),
                    (6.724, 8.705), (6.708, 8.714), (6.661, 8.721), (6.632, 8.721), (6.582, 8.691), (6.559, 8.669), (6.545, 8.606), (6.545, 7.745),
                    (6.546, 7.7), (6.56, 7.657), (6.586, 7.62), (6.622, 7.592), (6.783, 7.515), (6.95, 7.448), (7.12, 7.392), (7.293, 7.346), (7.468, 7.311),
                    (7.646, 7.288), (7.825, 7.275), (8.004, 7.273), (8.489, 7.321), (8.909, 7.459), (9.252, 7.678), (9.509, 7.97), (9.671, 8.327),
                    (9.727, 8.74), (9.718, 8.921), (9.69, 9.101), (9.642, 9.276), (9.574, 9.445), (9.489, 9.605), (9.387, 9.755), (9.269, 9.893),
                    (9.136, 10.017), (9.073, 10.077), (8.925, 10.219), (8.786, 10.357), (8.665, 10.49), (8.57, 10.618), (8.507, 10.739), (8.484, 10.852),
                    (8.494, 10.971), (8.52, 11.087), (8.561, 11.199), (8.617, 11.304), (8.63, 11.331), (8.635, 11.36), (8.633, 11.39), (8.623, 11.417),
                    (8.603, 11.442), (8.578, 11.461), (8.549, 11.472), (8.518, 11.475),
                ],
            ], &[
                16, 28, 27, 29, 28, 16, 16, 87, 86, 16, 86, 85, 81, 80, 79, 81, 79, 78, 81, 78, 77, 81, 77, 76, 81, 76, 75, 81, 75, 74, 81, 74, 73, 81, 73, 72,
                81, 72, 71, 81, 71, 70, 81, 70, 69, 81, 69, 68, 81, 68, 67, 81, 67, 66, 81, 66, 65, 81, 65, 64, 81, 64, 63, 81, 63, 62, 81, 62, 61, 81, 61, 60,
                81, 60, 59, 81, 59, 58, 81, 58, 57, 81, 57, 56, 81, 56, 55, 81, 55, 54, 81, 54, 53, 124, 123, 122, 124, 122, 121, 124, 121, 120, 124, 120, 119,
                124, 119, 118, 124, 118, 117, 124, 117, 116, 124, 116, 115, 124, 115, 114, 124, 114, 113, 124, 113, 112, 124, 112, 111, 124, 111, 110, 124,
                110, 109, 88, 87, 16, 88, 16, 17, 88, 17, 18, 88, 18, 19, 88, 19, 20, 88, 20, 21, 4, 5, 6, 4, 6, 7, 4, 7, 8, 8, 9, 10, 8, 10, 11, 8, 11, 12, 8,
                12, 13, 8, 13, 14, 8, 14, 15, 15, 16, 27, 15, 27, 26, 15, 26, 25, 15, 25, 24, 15, 24, 23, 15, 23, 22, 15, 22, 46, 30, 29, 16, 30, 16, 85, 30,
                85, 84, 30, 84, 83, 30, 83, 82, 30, 82, 81, 30, 81, 53, 30, 53, 52, 30, 52, 51, 30, 51, 50, 30, 50, 49, 30, 49, 48, 30, 48, 47, 125, 124, 109,
                89, 88, 21, 8, 15, 46, 8, 46, 45, 8, 45, 44, 8, 44, 43, 8, 43, 42, 8, 42, 41, 8, 41, 40, 8, 40, 39, 8, 39, 38, 8, 38, 37, 8, 37, 36, 31, 30,
                47, 31, 47, 132, 31, 132, 131, 31, 131, 130, 31, 130, 129, 126, 125, 109, 90, 89, 21, 90, 21, 0, 32, 31, 129, 127, 126, 109, 91, 90, 0, 91, 0,
                1, 91, 1, 2, 91, 2, 3, 91, 3, 4, 33, 32, 129, 128, 127, 109, 92, 91, 4, 34, 33, 129, 93, 92, 4, 35, 34, 129, 35, 129, 128, 94, 93, 4, 36, 35,
                128, 36, 128, 109, 95, 94, 4, 8, 36, 109, 8, 109, 108, 8, 108, 107, 8, 107, 106, 8, 106, 105, 8, 105, 104, 8, 104, 103, 96, 95, 4, 4, 8, 103,
                4, 103, 102, 4, 102, 101, 4, 101, 100, 4, 100, 99, 4, 99, 98, 4, 98, 97, 4, 97, 96,
            ]);
            pen.glyph(1.0, &[
                &[
                    (9.455, 1.818), (9.455, 5.455), (13.091, 5.455),
                ],
            ], &[
                1, 0, 2,
            ]);
        }
        TrackMaskBack => {
            pen.fill(&[(8.0, 3.5), (2.0, 8.0), (8.0, 12.5)]);
            pen.fill(&[(14.0, 3.5), (8.0, 8.0), (14.0, 12.5)]);
        }
        TrackMaskBackFrame => pen.fill(&[(11.0, 3.5), (4.0, 8.0), (11.0, 12.5)]),
        TrackMaskFwdFrame => pen.fill(&[(5.0, 3.5), (12.0, 8.0), (5.0, 12.5)]),
        TrackMaskFwd => {
            pen.fill(&[(2.0, 3.5), (8.0, 8.0), (2.0, 12.5)]);
            pen.fill(&[(8.0, 3.5), (14.0, 8.0), (8.0, 12.5)]);
        }
        SortIcons => {
            pen.glyph(1.0, &[
                &[
                    (13.778, 2.667), (13.948, 2.7), (14.092, 2.797), (14.188, 2.941), (14.222, 3.111), (14.222, 4.0), (14.188, 4.17), (14.092, 4.314),
                    (13.948, 4.411), (13.778, 4.444), (2.222, 4.444), (2.052, 4.411), (1.908, 4.314), (1.812, 4.17), (1.778, 4.0), (1.778, 3.111),
                    (1.812, 2.941), (1.908, 2.797), (2.052, 2.7), (2.222, 2.667),
                ],
            ], &[
                18, 19, 0, 18, 0, 1, 18, 1, 2, 18, 2, 3, 18, 3, 4, 18, 4, 5, 18, 5, 6, 18, 6, 7, 18, 7, 8, 18, 8, 9, 18, 9, 10, 18, 10, 11, 18, 11, 12, 18, 12,
                13, 18, 13, 14, 18, 14, 15, 18, 15, 16, 18, 16, 17,
            ]);
            pen.glyph(1.0, &[
                &[
                    (12.0, 7.111), (12.17, 7.145), (12.314, 7.241), (12.411, 7.385), (12.444, 7.556), (12.444, 8.444), (12.411, 8.615), (12.314, 8.759),
                    (12.17, 8.855), (12.0, 8.889), (2.222, 8.889), (2.052, 8.855), (1.908, 8.759), (1.812, 8.615), (1.778, 8.444), (1.778, 7.556),
                    (1.812, 7.385), (1.908, 7.241), (2.052, 7.145), (2.222, 7.111),
                ],
            ], &[
                18, 19, 0, 18, 0, 1, 18, 1, 2, 18, 2, 3, 18, 3, 4, 18, 4, 5, 18, 5, 6, 18, 6, 7, 18, 7, 8, 18, 8, 9, 18, 9, 10, 18, 10, 11, 18, 11, 12, 18, 12,
                13, 18, 13, 14, 18, 14, 15, 18, 15, 16, 18, 16, 17,
            ]);
            pen.glyph(1.0, &[
                &[
                    (10.222, 11.556), (10.392, 11.589), (10.536, 11.686), (10.633, 11.83), (10.667, 12.0), (10.667, 12.889), (10.633, 13.059),
                    (10.536, 13.203), (10.392, 13.3), (10.222, 13.333), (2.222, 13.333), (2.052, 13.3), (1.908, 13.203), (1.812, 13.059), (1.778, 12.889),
                    (1.778, 12.0), (1.812, 11.83), (1.908, 11.686), (2.052, 11.589), (2.222, 11.556),
                ],
            ], &[
                18, 19, 0, 18, 0, 1, 18, 1, 2, 18, 2, 3, 18, 3, 4, 18, 4, 5, 18, 5, 6, 18, 6, 7, 18, 7, 8, 18, 8, 9, 18, 9, 10, 18, 10, 11, 18, 11, 12, 18, 12,
                13, 18, 13, 14, 18, 14, 15, 18, 15, 16, 18, 16, 17,
            ]);
        }
        Automate => {
            pen.glyph(1.0, &[
                &[
                    (0.727, 3.636), (0.727, 12.0), (0.755, 12.139), (0.834, 12.257), (0.952, 12.336), (1.091, 12.364), (5.091, 12.364), (5.23, 12.336),
                    (5.348, 12.257), (5.427, 12.139), (5.455, 12.0), (5.455, 3.636), (5.427, 3.497), (5.348, 3.379), (5.23, 3.3), (5.091, 3.273),
                    (1.091, 3.273), (0.952, 3.3), (0.834, 3.379), (0.755, 3.497),
                ],
            ], &[
                1, 0, 19, 1, 19, 18, 1, 18, 17, 1, 17, 16, 1, 16, 15, 1, 15, 14, 1, 14, 13, 1, 13, 12, 1, 12, 11, 1, 11, 10, 1, 10, 9, 1, 9, 8, 1, 8, 7, 1, 7,
                6, 1, 6, 5, 1, 5, 4, 1, 4, 3, 1, 3, 2,
            ]);
            pen.glyph(1.0, &[
                &[
                    (6.545, 4.727), (6.545, 12.0), (6.573, 12.139), (6.652, 12.257), (6.77, 12.336), (6.909, 12.364), (10.182, 12.364), (10.321, 12.336),
                    (10.439, 12.257), (10.518, 12.139), (10.545, 12.0), (10.545, 4.727), (10.518, 4.588), (10.439, 4.47), (10.321, 4.391), (10.182, 4.364),
                    (6.909, 4.364), (6.77, 4.391), (6.652, 4.47), (6.573, 4.588),
                ],
            ], &[
                1, 0, 19, 1, 19, 18, 1, 18, 17, 1, 17, 16, 1, 16, 15, 1, 15, 14, 1, 14, 13, 1, 13, 12, 1, 12, 11, 1, 11, 10, 1, 10, 9, 1, 9, 8, 1, 8, 7, 1, 7,
                6, 1, 6, 5, 1, 5, 4, 1, 4, 3, 1, 3, 2,
            ]);
            pen.glyph(1.0, &[
                &[
                    (11.636, 4.727), (11.636, 12.0), (11.664, 12.139), (11.743, 12.257), (11.861, 12.336), (12.0, 12.364), (12.727, 12.364), (12.866, 12.336),
                    (12.984, 12.257), (13.063, 12.139), (13.091, 12.0), (13.091, 4.727), (13.063, 4.588), (12.984, 4.47), (12.866, 4.391), (12.727, 4.364),
                    (12.0, 4.364), (11.861, 4.391), (11.743, 4.47), (11.664, 4.588),
                ],
            ], &[
                1, 0, 19, 1, 19, 18, 1, 18, 17, 1, 17, 16, 1, 16, 15, 1, 15, 14, 1, 14, 13, 1, 13, 12, 1, 12, 11, 1, 11, 10, 1, 10, 9, 1, 9, 8, 1, 8, 7, 1, 7,
                6, 1, 6, 5, 1, 5, 4, 1, 4, 3, 1, 3, 2,
            ]);
            pen.glyph(1.0, &[
                &[
                    (14.182, 5.818), (14.182, 12.0), (14.209, 12.139), (14.288, 12.257), (14.406, 12.336), (14.545, 12.364), (14.909, 12.364),
                    (15.048, 12.336), (15.166, 12.257), (15.245, 12.139), (15.273, 12.0), (15.273, 5.818), (15.245, 5.679), (15.166, 5.561), (15.048, 5.482),
                    (14.909, 5.455), (14.545, 5.455), (14.406, 5.482), (14.288, 5.561), (14.209, 5.679),
                ],
            ], &[
                1, 0, 19, 1, 19, 18, 1, 18, 17, 1, 17, 16, 1, 16, 15, 1, 15, 14, 1, 14, 13, 1, 13, 12, 1, 12, 11, 1, 11, 10, 1, 10, 9, 1, 9, 8, 1, 8, 7, 1, 7,
                6, 1, 6, 5, 1, 5, 4, 1, 4, 3, 1, 3, 2,
            ]);
        }
        Star => pen.glyph(1.0, &[
            &[
                (8.185, 1.23), (9.867, 5.681), (14.617, 5.905), (14.726, 5.947), (14.793, 6.043), (14.795, 6.16), (14.731, 6.258), (11.021, 9.232),
                (12.275, 13.818), (12.269, 13.935), (12.199, 14.028), (12.088, 14.066), (11.975, 14.036), (8.0, 11.424), (4.026, 14.034), (3.913, 14.064),
                (3.802, 14.026), (3.732, 13.932), (3.726, 13.816), (4.979, 9.229), (1.269, 6.255), (1.208, 6.158), (1.21, 6.043), (1.276, 5.948),
                (1.384, 5.905), (6.133, 5.681), (7.815, 1.233), (7.888, 1.142), (7.999, 1.107), (8.111, 1.14),
            ],
        ], &[
            28, 29, 0, 28, 0, 1, 1, 2, 3, 1, 3, 4, 1, 4, 5, 1, 5, 6, 1, 6, 7, 1, 7, 8, 1, 8, 9, 1, 9, 10, 1, 10, 11, 1, 11, 12, 1, 12, 13, 1, 13, 14, 1, 14,
            15, 1, 15, 16, 1, 16, 17, 1, 17, 18, 1, 18, 19, 1, 19, 20, 1, 20, 21, 1, 21, 22, 1, 22, 23, 1, 23, 24, 1, 24, 25, 1, 25, 26, 1, 26, 27, 1, 27, 28,
        ]),
        ChevronLeft => pen.glyph(1.0, &[
            &[
                (5.333, 8.0), (5.351, 8.173), (5.401, 8.339), (5.483, 8.492), (5.593, 8.626), (9.141, 12.173), (9.293, 12.315), (9.473, 12.418),
                (9.672, 12.476), (9.879, 12.487), (10.084, 12.449), (10.273, 12.365), (10.439, 12.24), (10.57, 12.079), (10.661, 11.892), (10.706, 11.69),
                (10.702, 11.482), (10.651, 11.281), (10.554, 11.097), (10.417, 10.941), (10.396, 10.919), (7.475, 8.0), (10.396, 5.08), (10.515, 4.913),
                (10.593, 4.723), (10.625, 4.521), (10.609, 4.316), (10.548, 4.121), (10.443, 3.944), (10.3, 3.797), (10.128, 3.686), (9.934, 3.618),
                (9.73, 3.595), (9.527, 3.62), (9.335, 3.691), (9.164, 3.804), (9.142, 3.826), (5.594, 7.373), (5.483, 7.507), (5.401, 7.661), (5.351, 7.827),
            ],
        ], &[
            1, 0, 40, 1, 40, 39, 1, 39, 38, 1, 38, 37, 1, 37, 36, 1, 36, 35, 1, 35, 34, 1, 34, 33, 1, 33, 32, 1, 32, 31, 1, 31, 30, 1, 30, 29, 1, 29, 28, 1,
            28, 27, 1, 27, 26, 1, 26, 25, 1, 25, 24, 1, 24, 23, 1, 23, 22, 1, 22, 21, 1, 21, 20, 1, 20, 19, 1, 19, 18, 1, 18, 17, 1, 17, 16, 1, 16, 15, 1, 15,
            14, 1, 14, 13, 1, 13, 12, 1, 12, 11, 1, 11, 10, 1, 10, 9, 1, 9, 8, 1, 8, 7, 1, 7, 6, 1, 6, 5, 1, 5, 4, 1, 4, 3, 1, 3, 2,
        ]),
        ArrowUp => pen.glyph(1.0, &[
            &[
                (10.0, 8.0), (10.0, 13.5), (9.962, 13.691), (9.854, 13.854), (9.691, 13.962), (9.5, 14.0), (6.5, 14.0), (6.309, 13.962), (6.146, 13.854),
                (6.038, 13.691), (6.0, 13.5), (6.0, 8.0), (2.503, 8.0), (2.364, 7.958), (2.272, 7.846), (2.258, 7.701), (2.326, 7.573), (8.0, 2.0),
                (13.673, 7.573), (13.742, 7.701), (13.727, 7.846), (13.635, 7.958), (13.496, 8.0),
            ],
        ], &[
            21, 22, 0, 0, 1, 2, 0, 2, 3, 0, 3, 4, 0, 4, 5, 0, 5, 6, 0, 6, 7, 0, 7, 8, 0, 8, 9, 0, 9, 10, 0, 10, 11, 11, 12, 13, 11, 13, 14, 11, 14, 15, 11, 15,
            16, 11, 16, 17, 11, 17, 18, 11, 18, 19, 11, 19, 20, 11, 20, 21, 11, 21, 0,
        ]),
        Drive => pen.glyph(1.0, &[
            &[
                (15.5, 6.0), (0.5, 6.0), (0.309, 6.038), (0.146, 6.146), (0.038, 6.309), (0.0, 6.5), (0.0, 10.5), (0.038, 10.691), (0.146, 10.854),
                (0.309, 10.962), (0.5, 11.0), (1.0, 11.0), (1.0, 11.75), (1.019, 11.846), (1.073, 11.927), (1.154, 11.981), (1.25, 12.0), (1.75, 12.0),
                (1.846, 11.981), (1.927, 11.927), (1.981, 11.846), (2.0, 11.75), (2.0, 11.0), (14.0, 11.0), (14.0, 11.75), (14.019, 11.846), (14.073, 11.927),
                (14.154, 11.981), (14.25, 12.0), (14.75, 12.0), (14.846, 11.981), (14.927, 11.927), (14.981, 11.846), (15.0, 11.75), (15.0, 11.0),
                (15.5, 11.0), (15.691, 10.962), (15.854, 10.854), (15.962, 10.691), (16.0, 10.5), (16.0, 6.5), (15.962, 6.309), (15.854, 6.146),
                (15.691, 6.038),
            ],
            &[
                (2.0, 9.06), (1.753, 9.031), (1.519, 8.944), (1.312, 8.806), (1.142, 8.623), (1.021, 8.406), (0.953, 8.166), (0.943, 7.917), (0.992, 7.672),
                (1.096, 7.446), (1.25, 7.25), (1.446, 7.096), (1.672, 6.992), (1.917, 6.943), (2.166, 6.953), (2.406, 7.021), (2.623, 7.142), (2.806, 7.312),
                (2.944, 7.519), (3.031, 7.753), (3.06, 8.0), (3.033, 8.236), (2.955, 8.46), (2.829, 8.661), (2.661, 8.829), (2.46, 8.955), (2.236, 9.033),
            ],
        ], &[
            5, 51, 50, 5, 50, 49, 52, 51, 5, 52, 5, 4, 52, 4, 3, 52, 3, 2, 52, 2, 1, 1, 0, 43, 1, 43, 42, 1, 42, 41, 1, 41, 40, 1, 40, 39, 1, 39, 38, 1, 38,
            37, 1, 37, 36, 1, 36, 35, 1, 35, 34, 34, 33, 32, 34, 32, 31, 34, 31, 30, 34, 30, 29, 34, 29, 28, 34, 28, 27, 34, 27, 26, 34, 26, 25, 34, 25, 24,
            34, 24, 23, 22, 21, 20, 22, 20, 19, 22, 19, 18, 22, 18, 17, 22, 17, 16, 22, 16, 15, 22, 15, 14, 22, 14, 13, 22, 13, 12, 22, 12, 11, 11, 10, 9, 11,
            9, 8, 11, 8, 7, 11, 7, 6, 11, 6, 5, 11, 5, 49, 11, 49, 48, 11, 48, 47, 11, 47, 46, 11, 46, 45, 11, 45, 44, 11, 44, 70, 11, 70, 69, 11, 69, 68, 11,
            68, 67, 53, 52, 1, 1, 34, 23, 22, 11, 67, 22, 67, 66, 54, 53, 1, 23, 22, 66, 23, 66, 65, 23, 65, 64, 23, 64, 63, 23, 63, 62, 23, 62, 61, 23, 61,
            60, 23, 60, 59, 55, 54, 1, 1, 23, 59, 1, 59, 58, 1, 58, 57, 1, 57, 56, 1, 56, 55,
        ]),
        Clock => {
            pen.circle(8.0, 8.0, 6.0);
            pen.line(&[(8.0, 4.5), (8.0, 8.0), (10.5, 9.5)]);
        }
        Monitor => {
            pen.rect(2.0, 2.5, 14.0, 11.0);
            pen.line(&[(8.0, 11.0), (8.0, 13.5)]);
            pen.line(&[(5.0, 13.5), (11.0, 13.5)]);
        }
        Sun => {
            pen.glyph(1.0, &[
                &[
                    (8.389, 0.09), (7.611, 0.09), (7.571, 0.097), (7.535, 0.117), (7.505, 0.148), (7.481, 0.188), (7.466, 0.236), (7.461, 0.288),
                    (7.461, 2.276), (7.466, 2.328), (7.481, 2.376), (7.505, 2.416), (7.535, 2.447), (7.571, 2.467), (7.611, 2.474), (8.389, 2.474),
                    (8.429, 2.467), (8.465, 2.447), (8.495, 2.416), (8.519, 2.376), (8.534, 2.328), (8.539, 2.276), (8.539, 0.289), (8.534, 0.236),
                    (8.519, 0.188), (8.495, 0.148), (8.465, 0.117), (8.429, 0.097),
                ],
            ], &[
                1, 0, 27, 1, 27, 26, 1, 26, 25, 1, 25, 24, 1, 24, 23, 1, 23, 22, 1, 22, 21, 1, 21, 20, 1, 20, 19, 1, 19, 18, 1, 18, 17, 1, 17, 16, 1, 16, 15,
                1, 15, 14, 1, 14, 13, 1, 13, 12, 1, 12, 11, 1, 11, 10, 1, 10, 9, 1, 9, 8, 1, 8, 7, 1, 7, 6, 1, 6, 5, 1, 5, 4, 1, 4, 3, 1, 3, 2,
            ]);
            pen.glyph(1.0, &[
                &[
                    (8.389, 13.525), (7.611, 13.525), (7.571, 13.532), (7.535, 13.553), (7.505, 13.584), (7.481, 13.624), (7.466, 13.672), (7.461, 13.724),
                    (7.461, 15.711), (7.466, 15.764), (7.481, 15.812), (7.505, 15.852), (7.535, 15.883), (7.571, 15.903), (7.611, 15.91), (8.389, 15.91),
                    (8.429, 15.903), (8.465, 15.883), (8.495, 15.852), (8.519, 15.812), (8.534, 15.764), (8.539, 15.712), (8.539, 13.724), (8.534, 13.672),
                    (8.519, 13.624), (8.495, 13.584), (8.465, 13.553), (8.429, 13.533),
                ],
            ], &[
                1, 0, 27, 1, 27, 26, 1, 26, 25, 1, 25, 24, 1, 24, 23, 1, 23, 22, 1, 22, 21, 1, 21, 20, 1, 20, 19, 1, 19, 18, 1, 18, 17, 1, 17, 16, 1, 16, 15,
                1, 15, 14, 1, 14, 13, 1, 13, 12, 1, 12, 11, 1, 11, 10, 1, 10, 9, 1, 9, 8, 1, 8, 7, 1, 7, 6, 1, 6, 5, 1, 5, 4, 1, 4, 3, 1, 3, 2,
            ]);
            pen.glyph(1.0, &[
                &[
                    (0.09, 7.611), (0.09, 8.39), (0.097, 8.43), (0.117, 8.466), (0.148, 8.496), (0.188, 8.519), (0.236, 8.534), (0.288, 8.54), (2.276, 8.54),
                    (2.328, 8.534), (2.376, 8.519), (2.416, 8.496), (2.447, 8.466), (2.467, 8.43), (2.474, 8.39), (2.474, 7.611), (2.467, 7.571),
                    (2.447, 7.535), (2.416, 7.505), (2.376, 7.482), (2.328, 7.467), (2.276, 7.461), (0.289, 7.461), (0.236, 7.467), (0.188, 7.482),
                    (0.148, 7.505), (0.117, 7.535), (0.097, 7.571),
                ],
            ], &[
                1, 0, 27, 1, 27, 26, 1, 26, 25, 1, 25, 24, 1, 24, 23, 1, 23, 22, 1, 22, 21, 1, 21, 20, 1, 20, 19, 1, 19, 18, 1, 18, 17, 1, 17, 16, 1, 16, 15,
                1, 15, 14, 1, 14, 13, 1, 13, 12, 1, 12, 11, 1, 11, 10, 1, 10, 9, 1, 9, 8, 1, 8, 7, 1, 7, 6, 1, 6, 5, 1, 5, 4, 1, 4, 3, 1, 3, 2,
            ]);
            pen.glyph(1.0, &[
                &[
                    (13.525, 7.611), (13.525, 8.39), (13.532, 8.43), (13.553, 8.466), (13.584, 8.496), (13.624, 8.519), (13.672, 8.534), (13.724, 8.54),
                    (15.711, 8.54), (15.764, 8.534), (15.812, 8.519), (15.852, 8.496), (15.883, 8.466), (15.903, 8.43), (15.91, 8.39), (15.91, 7.611),
                    (15.903, 7.571), (15.883, 7.535), (15.852, 7.505), (15.812, 7.482), (15.764, 7.467), (15.712, 7.461), (13.724, 7.461), (13.672, 7.467),
                    (13.624, 7.482), (13.584, 7.505), (13.553, 7.535), (13.533, 7.571),
                ],
            ], &[
                1, 0, 27, 1, 27, 26, 1, 26, 25, 1, 25, 24, 1, 24, 23, 1, 23, 22, 1, 22, 21, 1, 21, 20, 1, 20, 19, 1, 19, 18, 1, 18, 17, 1, 17, 16, 1, 16, 15,
                1, 15, 14, 1, 14, 13, 1, 13, 12, 1, 12, 11, 1, 11, 10, 1, 10, 9, 1, 9, 8, 1, 8, 7, 1, 7, 6, 1, 6, 5, 1, 5, 4, 1, 4, 3, 1, 3, 2,
            ]);
            pen.glyph(1.0, &[
                &[
                    (2.132, 13.318), (2.683, 13.869), (2.716, 13.892), (2.755, 13.903), (2.799, 13.902), (2.844, 13.891), (2.888, 13.868), (2.929, 13.834),
                    (4.334, 12.429), (4.368, 12.388), (4.391, 12.344), (4.402, 12.299), (4.403, 12.255), (4.392, 12.216), (4.368, 12.183), (3.817, 11.632),
                    (3.784, 11.609), (3.745, 11.597), (3.702, 11.598), (3.657, 11.61), (3.612, 11.633), (3.571, 11.666), (2.167, 13.072), (2.133, 13.113),
                    (2.11, 13.157), (2.098, 13.202), (2.097, 13.245), (2.109, 13.285),
                ],
            ], &[
                1, 0, 27, 1, 27, 26, 1, 26, 25, 1, 25, 24, 1, 24, 23, 1, 23, 22, 1, 22, 21, 1, 21, 20, 1, 20, 19, 1, 19, 18, 1, 18, 17, 1, 17, 16, 1, 16, 15,
                1, 15, 14, 1, 14, 13, 1, 13, 12, 1, 12, 11, 1, 11, 10, 1, 10, 9, 1, 9, 8, 1, 8, 7, 1, 7, 6, 1, 6, 5, 1, 5, 4, 1, 4, 3, 1, 3, 2,
            ]);
            pen.glyph(1.0, &[
                &[
                    (11.632, 3.818), (12.183, 4.369), (12.216, 4.392), (12.255, 4.403), (12.299, 4.402), (12.344, 4.391), (12.388, 4.368), (12.429, 4.334),
                    (13.834, 2.929), (13.867, 2.888), (13.89, 2.844), (13.902, 2.799), (13.903, 2.755), (13.892, 2.716), (13.869, 2.683), (13.318, 2.132),
                    (13.285, 2.109), (13.245, 2.097), (13.202, 2.098), (13.157, 2.11), (13.113, 2.133), (13.072, 2.166), (11.666, 3.571), (11.633, 3.612),
                    (11.61, 3.657), (11.598, 3.702), (11.598, 3.745), (11.609, 3.785),
                ],
            ], &[
                1, 0, 27, 1, 27, 26, 1, 26, 25, 1, 25, 24, 1, 24, 23, 1, 23, 22, 1, 22, 21, 1, 21, 20, 1, 20, 19, 1, 19, 18, 1, 18, 17, 1, 17, 16, 1, 16, 15,
                1, 15, 14, 1, 14, 13, 1, 13, 12, 1, 12, 11, 1, 11, 10, 1, 10, 9, 1, 9, 8, 1, 8, 7, 1, 7, 6, 1, 6, 5, 1, 5, 4, 1, 4, 3, 1, 3, 2,
            ]);
            pen.glyph(1.0, &[
                &[
                    (13.318, 13.869), (13.869, 13.318), (13.892, 13.285), (13.903, 13.246), (13.902, 13.202), (13.891, 13.157), (13.868, 13.113),
                    (13.834, 13.072), (12.429, 11.667), (12.388, 11.633), (12.344, 11.61), (12.299, 11.599), (12.255, 11.598), (12.216, 11.609),
                    (12.183, 11.632), (11.632, 12.184), (11.609, 12.216), (11.597, 12.256), (11.598, 12.299), (11.61, 12.344), (11.633, 12.389),
                    (11.666, 12.43), (13.072, 13.834), (13.113, 13.868), (13.157, 13.891), (13.202, 13.903), (13.245, 13.903), (13.285, 13.892),
                ],
            ], &[
                1, 0, 27, 1, 27, 26, 1, 26, 25, 1, 25, 24, 1, 24, 23, 1, 23, 22, 1, 22, 21, 1, 21, 20, 1, 20, 19, 1, 19, 18, 1, 18, 17, 1, 17, 16, 1, 16, 15,
                1, 15, 14, 1, 14, 13, 1, 13, 12, 1, 12, 11, 1, 11, 10, 1, 10, 9, 1, 9, 8, 1, 8, 7, 1, 7, 6, 1, 6, 5, 1, 5, 4, 1, 4, 3, 1, 3, 2,
            ]);
            pen.glyph(1.0, &[
                &[
                    (3.818, 4.368), (4.369, 3.817), (4.392, 3.784), (4.403, 3.745), (4.402, 3.701), (4.391, 3.656), (4.368, 3.612), (4.334, 3.571),
                    (2.929, 2.166), (2.888, 2.133), (2.844, 2.11), (2.799, 2.098), (2.755, 2.097), (2.716, 2.108), (2.683, 2.131), (2.132, 2.682),
                    (2.109, 2.715), (2.097, 2.755), (2.098, 2.798), (2.11, 2.843), (2.133, 2.887), (2.166, 2.928), (3.571, 4.334), (3.612, 4.367),
                    (3.657, 4.39), (3.702, 4.402), (3.745, 4.402), (3.785, 4.391),
                ],
            ], &[
                1, 0, 27, 1, 27, 26, 1, 26, 25, 1, 25, 24, 1, 24, 23, 1, 23, 22, 1, 22, 21, 1, 21, 20, 1, 20, 19, 1, 19, 18, 1, 18, 17, 1, 17, 16, 1, 16, 15,
                1, 15, 14, 1, 14, 13, 1, 13, 12, 1, 12, 11, 1, 11, 10, 1, 10, 9, 1, 9, 8, 1, 8, 7, 1, 7, 6, 1, 6, 5, 1, 5, 4, 1, 4, 3, 1, 3, 2,
            ]);
            pen.glyph(1.0, &[
                &[
                    (8.0, 3.556), (6.818, 3.714), (5.757, 4.162), (4.857, 4.857), (4.162, 5.757), (3.714, 6.819), (3.556, 8.0), (3.714, 9.181),
                    (4.162, 10.243), (4.857, 11.143), (5.757, 11.838), (6.818, 12.286), (8.0, 12.444), (9.182, 12.286), (10.243, 11.838), (11.143, 11.143),
                    (11.838, 10.243), (12.286, 9.181), (12.444, 8.0), (12.286, 6.819), (11.838, 5.757), (11.143, 4.857), (10.243, 4.162), (9.182, 3.714),
                ],
            ], &[
                1, 0, 23, 1, 23, 22, 1, 22, 21, 1, 21, 20, 1, 20, 19, 1, 19, 18, 1, 18, 17, 1, 17, 16, 1, 16, 15, 1, 15, 14, 1, 14, 13, 1, 13, 12, 1, 12, 11,
                1, 11, 10, 1, 10, 9, 1, 9, 8, 1, 8, 7, 1, 7, 6, 1, 6, 5, 1, 5, 4, 1, 4, 3, 1, 3, 2,
            ]);
        }
        Moon => pen.glyph(1.0, &[
            &[
                (8.0, 0.889), (6.722, 1.003), (5.519, 1.334), (4.411, 1.86), (3.418, 2.561), (2.561, 3.419), (1.86, 4.411), (1.334, 5.519), (1.003, 6.722),
                (0.889, 8.0), (1.003, 9.278), (1.334, 10.481), (1.86, 11.589), (2.561, 12.581), (3.418, 13.439), (4.411, 14.14), (5.519, 14.666),
                (6.722, 14.997), (8.0, 15.111), (9.278, 14.997), (10.481, 14.666), (11.589, 14.14), (12.582, 13.439), (13.439, 12.582), (14.14, 11.589),
                (14.666, 10.481), (14.997, 9.278), (15.111, 8.0), (14.997, 6.722), (14.666, 5.519), (14.14, 4.411), (13.439, 3.418), (12.582, 2.561),
                (11.589, 1.86), (10.481, 1.334), (9.278, 1.003),
            ],
            &[
                (9.882, 13.93), (8.973, 13.222), (8.19, 12.38), (7.55, 11.42), (7.071, 10.359), (6.771, 9.214), (6.667, 8.0), (6.771, 6.786), (7.071, 5.641),
                (7.55, 4.58), (8.19, 3.621), (8.973, 2.778), (9.882, 2.07), (11.076, 2.594), (12.124, 3.346), (12.995, 4.294), (13.656, 5.407),
                (14.075, 6.653), (14.222, 8.0), (14.075, 9.347), (13.656, 10.593), (12.995, 11.706), (12.124, 12.654), (11.076, 13.406),
            ],
        ], &[
            9, 42, 41, 9, 41, 40, 9, 40, 39, 9, 39, 38, 9, 38, 37, 9, 37, 36, 43, 42, 9, 43, 9, 8, 43, 8, 7, 43, 7, 6, 43, 6, 5, 43, 5, 4, 43, 4, 3, 43, 3, 2,
            43, 2, 1, 43, 1, 0, 0, 35, 34, 0, 34, 33, 0, 33, 32, 0, 32, 31, 31, 30, 29, 31, 29, 28, 31, 28, 27, 31, 27, 26, 26, 25, 24, 26, 24, 23, 26, 23, 22,
            26, 22, 21, 21, 20, 19, 21, 19, 18, 21, 18, 17, 21, 17, 16, 21, 16, 15, 21, 15, 14, 14, 13, 12, 14, 12, 11, 14, 11, 10, 14, 10, 9, 14, 9, 36, 44,
            43, 0, 21, 14, 36, 21, 36, 59, 21, 59, 58, 21, 58, 57, 21, 57, 56, 45, 44, 0, 26, 21, 56, 26, 56, 55, 26, 55, 54, 26, 54, 53, 26, 53, 52, 46, 45,
            0, 31, 26, 52, 31, 52, 51, 31, 51, 50, 31, 50, 49, 31, 49, 48, 47, 46, 0, 0, 31, 48, 0, 48, 47,
        ]),
        Eyedropper => {
            // a pipette: bulb at the top right, glass tube running to a tip at the bottom left
            pen.closed(&[(10.0, 4.5), (11.5, 3.0), (13.0, 3.0), (13.0, 4.5), (11.5, 6.0)]);
            pen.line(&[(8.2, 6.2), (10.8, 8.8)]);
            pen.line(&[(9.5, 7.5), (3.5, 12.5)]);
            pen.line(&[(3.5, 12.5), (2.5, 13.5)]);
        }
    }
}

/// An icon button: returns the response; `active` draws the selected state.
pub fn button(ui: &mut egui::Ui, icon: Icon, size: f32, active: bool, t: &crate::theme::Tokens, tooltip: &str) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(vec2(size, size), egui::Sense::click());
    let bg = if resp.is_pointer_button_down_on() {
        Some(t.pressed)
    } else if resp.hovered() {
        Some(t.hover)
    } else {
        None
    };
    if let Some(bg) = bg {
        ui.painter().rect_filled(rect, t.radius_sm, bg);
    }
    let col = if active {
        t.icon_active
    } else if resp.hovered() {
        t.tab_text_active
    } else {
        t.icon
    };
    paint(ui.painter(), rect.shrink(size * 0.2), icon, col);
    if tooltip.is_empty() { resp } else { resp.on_hover_text(tooltip) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use Icon::*;
    use egui::epaint::{ColorMode, Shape};

    const ALL: &[Icon] = &[
        Monitor,
        Sun,
        Moon,
        Selection,
        TrackSelectFwd,
        TrackSelectBack,
        Ripple,
        Rolling,
        RateStretch,
        Remix,
        Razor,
        Slip,
        Slide,
        Pen,
        Rectangle,
        Ellipse,
        Hand,
        Zoom,
        Type,
        Play,
        Pause,
        StepBack,
        StepFwd,
        GoToIn,
        GoToOut,
        PrevEdit,
        NextEdit,
        MarkIn,
        MarkOut,
        Marker,
        Insert,
        Overwrite,
        Lift,
        Extract,
        Camera,
        Loop,
        Wrench,
        Plus,
        Eye,
        EyeOff,
        Speaker,
        Mute,
        Lock,
        Unlock,
        SyncLock,
        Mic,
        Folder,
        Film,
        Sequence,
        Audio,
        Image,
        Search,
        ListView,
        IconView,
        Freeform,
        NewItem,
        Trash,
        Home,
        Workspaces,
        Hamburger,
        ChevronDown,
        ChevronRight,
        Magnet,
        Link,
        ExternalLink,
        Keyframe,
        Stopwatch,
        Fx,
        Reset,
        Close,
        Fullscreen,
        Export,
        Import,
        Gear,
        Info,
        Captions,
        Adjust,
        Nest,
        Undo,
        Redo,
        Bell,
        Chat,
        Globe,
        Code,
        Sparkle,
        Grid,
        Square,
        Proxy,
        Offline,
        TrackMaskBack,
        TrackMaskBackFrame,
        TrackMaskFwdFrame,
        TrackMaskFwd,
        SortIcons,
        Automate,
        Star,
        ChevronLeft,
        ArrowUp,
        Drive,
        Network,
        Clock,
        Eyedropper,
    ];

    /// Paint `icon` at `ppp` device pixels per point into a rect of `size` points whose corner is
    /// off the pixel grid, and return what was drawn.
    fn drawn(icon: Icon, ppp: f32, size: f32) -> Vec<Shape> {
        let ctx = egui::Context::default();
        ctx.set_pixels_per_point(ppp);
        let mut shapes = Vec::new();
        for _ in 0..2 {
            let mut out = ctx.run_ui(egui::RawInput::default(), |ui| {
                paint(ui.painter(), Rect::from_min_size(pos2(10.3, 7.7), vec2(size, size)), icon, Color32::WHITE);
            });
            out.textures_delta.clear();
            shapes = out.shapes.into_iter().map(|c| c.shape).collect();
        }
        fn flat(s: Shape, out: &mut Vec<Shape>) {
            match s {
                Shape::Vec(v) => v.into_iter().for_each(|s| flat(s, out)),
                s => out.push(s),
            }
        }
        let mut out = Vec::new();
        shapes.into_iter().for_each(|s| flat(s, &mut out));
        out
    }

    fn on_grid(v: f32, ppp: f32, width_px: f32) -> bool {
        let want = if width_px.round() as i32 % 2 == 1 { 0.5 } else { 0.0 };
        ((v * ppp).fract().abs() - want).abs() < 1e-3 || ((v * ppp).fract().abs() - (1.0 - want)).abs() < 1e-3
    }

    /// Every straight line of every icon covers whole device pixels at 1x and 2x (no half-pixel
    /// blur): a whole number of pixels wide, its middle where that width needs it.
    #[test]
    fn straight_lines_land_on_whole_pixels() {
        for &ppp in &[1.0, 2.0] {
            for &size in &[16.0, 13.0, 9.6, 20.0] {
                for &icon in ALL {
                    for shape in drawn(icon, ppp, size) {
                        if let Shape::Path(p) = shape {
                            if p.stroke.width == 0.0 || matches!(p.stroke.color, ColorMode::Solid(c) if c == Color32::TRANSPARENT) {
                                continue;
                            }
                            let w = p.stroke.width * ppp;
                            assert!((w - w.round()).abs() < 1e-3 && w >= 1.0, "{icon:?} at {ppp}x size {size}: width {w} px");
                            let n = p.points.len();
                            let segs = if p.closed { n } else { n - 1 };
                            for i in 0..segs {
                                let (a, b) = (p.points[i], p.points[(i + 1) % n]);
                                if (a.y - b.y).abs() < 1e-4 {
                                    assert!(on_grid(a.y, ppp, w), "{icon:?} at {ppp}x size {size}: horizontal line at y {} px", a.y * ppp);
                                }
                                if (a.x - b.x).abs() < 1e-4 {
                                    assert!(on_grid(a.x, ppp, w), "{icon:?} at {ppp}x size {size}: vertical line at x {} px", a.x * ppp);
                                }
                            }
                        } else if let Shape::Mesh(m) = shape {
                            if size < 16.0 {
                                continue;
                            }
                            use std::collections::HashMap;
                            let mut edges: HashMap<(u32, u32), u32> = HashMap::new();
                            for t in m.indices.chunks(3) {
                                if t.len() != 3 || t.iter().any(|&i| m.vertices.get(i as usize).is_none_or(|p| p.color.a() == 0)) {
                                    continue;
                                }
                                for k in 0..3 {
                                    let (a, b) = (t[k], t[(k + 1) % 3]);
                                    *edges.entry((a.min(b), a.max(b))).or_default() += 1;
                                }
                            }
                            for ((a, b), count) in edges {
                                if count != 1 {
                                    continue;
                                }
                                let (pa, pb) = (m.vertices[a as usize].pos, m.vertices[b as usize].pos);
                                if (pa.y - pb.y).abs() < 1e-4 {
                                    assert!(
                                        on_grid(pa.y, ppp, 1.0) || on_grid(pa.y, ppp, 2.0),
                                        "{icon:?} at {ppp}x size {size}: fill edge at y {} px",
                                        pa.y * ppp
                                    );
                                }
                                if (pa.x - pb.x).abs() < 1e-4 {
                                    assert!(
                                        on_grid(pa.x, ppp, 1.0) || on_grid(pa.x, ppp, 2.0),
                                        "{icon:?} at {ppp}x size {size}: fill edge at x {} px",
                                        pa.x * ppp
                                    );
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    /// Every painted point of every icon, in painted points (the test paints each icon into a
    /// 160-point box at `(10.3, 7.7)`, so one grid unit is 10 points).
    fn points(icon: Icon, ppp: f32, size: f32) -> Vec<Pos2> {
        let mut out = Vec::new();
        fn flat(s: Shape, out: &mut Vec<Pos2>) {
            match s {
                Shape::Vec(v) => v.into_iter().for_each(|s| flat(s, out)),
                Shape::Path(p) => out.extend(p.points),
                Shape::Mesh(m) => out.extend(m.vertices.iter().map(|v| v.pos)),
                Shape::Rect(r) => out.extend([r.rect.min, r.rect.max]),
                Shape::Circle(c) => out.extend([c.center - vec2(c.radius, c.radius), c.center + vec2(c.radius, c.radius)]),
                Shape::LineSegment { points, .. } => out.extend(points),
                _ => {}
            }
        }
        for s in drawn(icon, ppp, size) {
            flat(s, &mut out);
        }
        out
    }

    /// Every icon is drawn on its 16×16 grid: nothing runs away from the box (a bad scale would
    /// show up as coordinates in the 20–44 range), the ink is not empty, and it is centred.
    #[test]
    fn every_icon_stays_on_its_grid() {
        for &ppp in &[1.0, 2.0] {
            for &icon in ALL {
                let pts = points(icon, ppp, 160.0);
                assert!(!pts.is_empty(), "{icon:?} at {ppp}x paints nothing");
                let mut min = pos2(f32::MAX, f32::MAX);
                let mut max = pos2(f32::MIN, f32::MIN);
                for p in &pts {
                    assert!(p.x.is_finite() && p.y.is_finite(), "{icon:?} at {ppp}x: non-finite point {p:?}");
                    min.x = min.x.min(p.x);
                    min.y = min.y.min(p.y);
                    max.x = max.x.max(p.x);
                    max.y = max.y.max(p.y);
                }
                // (10.3, 7.7) + 10 points per grid unit; allow one pixel of stroke/feather outside.
                let (gx0, gy0) = ((min.x - 10.3) / 10.0, (min.y - 7.7) / 10.0);
                let (gx1, gy1) = ((max.x - 10.3) / 10.0, (max.y - 7.7) / 10.0);
                let (w, h) = (gx1 - gx0, gy1 - gy0);
                assert!(gx0 >= -1.0 && gy0 >= -1.0 && gx1 <= 17.0 && gy1 <= 17.0, "{icon:?} at {ppp}x: [{gx0:.2}..{gx1:.2}] x [{gy0:.2}..{gy1:.2}]");
                assert!(w >= 3.0 && h >= 3.0, "{icon:?} at {ppp}x: ink only {w:.2} x {h:.2} grid units");
                let (cx, cy) = ((gx0 + gx1) / 2.0, (gy0 + gy1) / 2.0);
                assert!((cx - 8.0).abs() <= 3.0 && (cy - 8.0).abs() <= 3.0, "{icon:?} at {ppp}x: ink centre ({cx:.2}, {cy:.2})");
            }
        }
    }

    /// The Overwrite slot is thinner than one device pixel at 1x; snapping keeps it open.
    #[test]
    fn snapping_keeps_a_thin_slot_open() {
        let slot = [(4.364, 8.727), (10.909, 8.727), (10.909, 9.455), (4.364, 9.455)];
        for &ppp in &[1.0, 2.0] {
            let r = Rect::from_min_size(pos2(0.0, 0.0), vec2(16.0, 16.0));
            let p = snap_fill(&slot, r, ppp);
            let y0 = p.iter().fold(f32::MAX, |a, q| a.min(q.y));
            let y1 = p.iter().fold(f32::MIN, |a, q| a.max(q.y));
            assert!((y1 - y0) * ppp >= 1.0 - 1e-3, "slot height {} px at {ppp}x", (y1 - y0) * ppp);
            assert!((y0 * ppp).fract().abs() < 1e-3 && (y1 * ppp).fract().abs() < 1e-3, "slot edges off the pixel grid at {ppp}x");
        }
    }

    /// Snapping related contours independently must still leave a wall around a hole.
    #[test]
    fn snapping_keeps_rectangle_walls() {
        let outer = [
            (1.455, 2.909),
            (1.455, 13.091),
            (1.482, 13.23),
            (1.561, 13.348),
            (1.679, 13.427),
            (1.818, 13.455),
            (14.182, 13.455),
            (14.321, 13.427),
            (14.439, 13.348),
            (14.518, 13.23),
            (14.545, 13.091),
            (14.545, 2.909),
            (14.518, 2.77),
            (14.439, 2.652),
            (14.321, 2.573),
            (14.182, 2.545),
            (1.818, 2.545),
            (1.679, 2.573),
            (1.561, 2.652),
            (1.482, 2.77),
        ];
        let hole = [(13.455, 12.364), (2.545, 12.364), (2.545, 3.636), (13.455, 3.636)];
        let bounds =
            |p: &[Pos2]| p.iter().fold((f32::MAX, f32::MIN, f32::MAX, f32::MIN), |(x0, x1, y0, y1), q| (x0.min(q.x), x1.max(q.x), y0.min(q.y), y1.max(q.y)));
        for &size in &[13.0, 16.0] {
            for &ppp in &[1.0, 2.0] {
                let r = Rect::from_min_size(pos2(0.0, 0.0), vec2(size, size));
                let mut p = snap_fill(&outer, r, ppp);
                let n = p.len();
                p.extend(snap_fill(&hole, r, ppp));
                let ranges = [(0, n), (n, p.len())];
                keep_walls(&mut p, &ranges, ppp);
                let (ox0, ox1, oy0, oy1) = bounds(&p[0..n]);
                let (hx0, hx1, hy0, hy1) = bounds(&p[n..]);
                for (wall, side) in [(hx0 - ox0, "left"), (ox1 - hx1, "right"), (hy0 - oy0, "top"), (oy1 - hy1, "bottom")] {
                    assert!(wall * ppp >= 1.0 - 1e-3, "{side} wall {wall} px at {size} pt {ppp}x");
                }
            }
        }
    }

    /// Stacked holes keep a separator where their columns overlap (Overwrite at 13 pt).
    #[test]
    fn snapping_keeps_sibling_holes_apart() {
        let outer = [(0.727, 2.909), (14.545, 2.909), (14.545, 11.636), (0.727, 11.636)];
        let upper = [(5.091, 3.636), (10.182, 3.636), (10.182, 8.0), (5.091, 8.0)];
        let slot = [(4.364, 8.727), (10.909, 8.727), (10.909, 9.455), (4.364, 9.455)];
        for &ppp in &[1.0, 2.0] {
            let r = Rect::from_min_size(pos2(10.3, 7.7), vec2(13.0, 13.0));
            let mut p = snap_fill(&outer, r, ppp);
            let n = p.len();
            p.extend(snap_fill(&upper, r, ppp));
            let m = p.len();
            p.extend(snap_fill(&slot, r, ppp));
            let ranges = [(0, n), (n, m), (m, p.len())];
            keep_walls(&mut p, &ranges, ppp);
            keep_holes_apart(&mut p, &ranges, ppp);
            keep_walls(&mut p, &ranges, ppp);
            let (_, _, uy0, uy1) = bounds(&p[n..m]);
            let (_, _, sy0, sy1) = bounds(&p[m..]);
            assert!((sy0 - uy1) * ppp >= 1.0 - 1e-3, "separator {} px at {ppp}x", (sy0 - uy1) * ppp);
            assert!((uy1 - uy0) * ppp >= 1.0 - 1e-3 && (sy1 - sy0) * ppp >= 1.0 - 1e-3, "hole collapsed at {ppp}x");
        }
    }

    /// Diagonal cutouts whose bounding boxes overlap are not stacked holes.
    #[test]
    fn separator_ignores_diagonal_cutouts() {
        let outer = [(6.0, 6.0), (14.0, 6.0), (14.0, 16.0), (6.0, 16.0)];
        let upper = [(10.0, 8.0), (12.0, 10.0), (10.0, 12.0), (8.0, 10.0)];
        let lower = [(10.0, 10.0), (12.0, 12.0), (10.0, 14.0), (8.0, 12.0)];
        let r = Rect::from_min_size(pos2(0.0, 0.0), vec2(16.0, 16.0));
        for &ppp in &[1.0, 2.0] {
            let mut p = snap_fill(&outer, r, ppp);
            let n = p.len();
            p.extend(snap_fill(&upper, r, ppp));
            let m = p.len();
            p.extend(snap_fill(&lower, r, ppp));
            let before = p.clone();
            let end = p.len();
            keep_holes_apart(&mut p, &[(0, n), (n, m), (m, end)], ppp);
            assert_eq!(p, before, "diagonal cutouts moved at {ppp}x");
        }
    }

    /// A half-pixel fallback happens only where a full-pixel wall cannot fit.
    #[test]
    fn half_pixel_fallback_only_for_tight_walls() {
        let outer = [(0.0, 6.0), (2.0, 6.0), (2.0, 10.0), (0.0, 10.0)];
        let hole = [(0.5, 7.0), (1.5, 7.0), (1.5, 9.0), (0.5, 9.0)];
        let r = Rect::from_min_size(pos2(10.3, 7.7), vec2(13.0, 13.0));
        let mut p = snap_fill(&outer, r, 1.0);
        let n = p.len();
        p.extend(snap_fill(&hole, r, 1.0));
        let m = p.len();
        keep_walls(&mut p, &[(0, n), (n, m)], 1.0);
        assert!(p.iter().any(|q| (q.x.fract().abs() - 0.5).abs() < 1e-3 || (q.y.fract().abs() - 0.5).abs() < 1e-3), "expected a half-pixel wall");
    }

    fn boundary_loops(m: &egui::Mesh) -> Vec<Vec<Pos2>> {
        use std::collections::{HashMap, HashSet};
        let mut count: HashMap<(u32, u32), u32> = HashMap::new();
        for t in m.indices.chunks(3) {
            if t.len() != 3 || t.iter().any(|&i| m.vertices[i as usize].color.a() == 0) {
                continue;
            }
            for k in 0..3 {
                let (a, b) = (t[k], t[(k + 1) % 3]);
                *count.entry((a.min(b), a.max(b))).or_default() += 1;
            }
        }
        let mut adj: HashMap<u32, Vec<u32>> = HashMap::new();
        let mut unused: HashSet<(u32, u32)> = HashSet::new();
        for (&(a, b), &n) in &count {
            if n == 1 {
                adj.entry(a).or_default().push(b);
                adj.entry(b).or_default().push(a);
                unused.insert((a, b));
            }
        }
        let mut loops = Vec::new();
        while let Some(&(start, _)) = unused.iter().next() {
            let mut path = vec![start];
            let mut prev = start;
            let mut cur = adj[&start][0];
            while cur != start {
                path.push(cur);
                let next = *adj[&cur].iter().find(|&&n| n != prev).expect("boundary loop");
                prev = cur;
                cur = next;
            }
            for w in path.windows(2) {
                unused.remove(&(w[0].min(w[1]), w[0].max(w[1])));
            }
            unused.remove(&(path[path.len() - 1].min(start), path[path.len() - 1].max(start)));
            loops.push(path.iter().map(|&i| m.vertices[i as usize].pos).collect());
        }
        loops
    }

    /// The Workspaces sidebar, as painted, keeps its three holes open with one-pixel walls and gaps.
    #[test]
    fn workspaces_sidebars_fit_at_16() {
        let shapes = drawn(Icon::Workspaces, 1.0, 16.0);
        let loops =
            shapes.iter().filter_map(|s| if let Shape::Mesh(m) = s { Some(boundary_loops(m)) } else { None }).find(|l| l.len() == 4).expect("sidebar mesh");
        let area = |p: &[Pos2]| {
            let n = p.len();
            (0..n).map(|i| p[i].x * p[(i + 1) % n].y - p[(i + 1) % n].x * p[i].y).sum::<f32>().abs() / 2.0
        };
        let outer = loops.iter().enumerate().max_by(|a, b| area(a.1).total_cmp(&area(b.1))).expect("outer").0;
        let (ox0, ox1, _, _) = bounds(&loops[outer]);
        let mut holes: Vec<_> = loops.iter().enumerate().filter(|(i, _)| *i != outer).map(|(_, l)| bounds(l)).collect();
        holes.sort_by(|a, b| a.2.total_cmp(&b.2));
        for (i, &(hx0, hx1, hy0, hy1)) in holes.iter().enumerate() {
            assert!(hx1 > hx0 && hy1 > hy0, "hole {i} closed");
            assert!(hx0 - ox0 >= 1.0 - 1e-3 && ox1 - hx1 >= 1.0 - 1e-3, "hole {i} wall");
            if i > 0 {
                assert!(hy0 - holes[i - 1].3 >= 1.0 - 1e-3, "hole {i} gap");
            }
        }
    }

    /// The Zoom plus keeps a solid interior at 13 pt, where its bars are under a pixel wide.
    #[test]
    fn zoom_plus_keeps_area_at_13() {
        let shapes = drawn(Icon::Zoom, 1.0, 13.0);
        let mut found = false;
        for shape in shapes {
            if let Shape::Mesh(m) = shape {
                let mut area = 0.0;
                let mut solid = false;
                let mut x0 = f32::MAX;
                let mut x1 = f32::MIN;
                for t in m.indices.chunks(3) {
                    if t.len() != 3 || t.iter().any(|&i| m.vertices[i as usize].color.a() == 0) {
                        continue;
                    }
                    solid = true;
                    for &i in t {
                        x0 = x0.min(m.vertices[i as usize].pos.x);
                        x1 = x1.max(m.vertices[i as usize].pos.x);
                    }
                    let (a, b, c) = (m.vertices[t[0] as usize].pos, m.vertices[t[1] as usize].pos, m.vertices[t[2] as usize].pos);
                    area += ((b.x - a.x) * (c.y - a.y) - (b.y - a.y) * (c.x - a.x)).abs() / 2.0;
                }
                if solid && x1 - x0 < 7.0 {
                    found = true;
                    assert!(area > 0.2, "Zoom plus area {area:.3} at 13 pt");
                }
            }
        }
        assert!(found, "Zoom plus mesh not found");
    }

    /// The glyph-based icons draw triangulated meshes; every index is in range.
    #[test]
    fn glyph_icons_draw_meshes() {
        let mut icons = 0;
        for &icon in ALL {
            let mut tris = 0;
            for s in drawn(icon, 1.0, 160.0) {
                if let Shape::Mesh(m) = s {
                    assert!(m.indices.len() >= 3 && m.indices.len() % 3 == 0, "{icon:?}: bad mesh");
                    assert!(m.indices.iter().all(|&i| (i as usize) < m.vertices.len()), "{icon:?}: index out of range");
                    assert!(m.vertices.iter().all(|v| v.pos.x.is_finite() && v.pos.y.is_finite()), "{icon:?}: bad vertex");
                    tris += m.indices.len() / 3;
                }
            }
            if tris > 0 {
                icons += 1;
            }
        }
        // 86 converted designs; five are hand-drawn or partly hand-drawn.
        assert_eq!(icons, 81, "glyph-mesh icons");
    }

    /// The panel menu mark (≡): three lines, the same two gaps, at every scale.
    #[test]
    fn menu_mark_lines_are_evenly_spaced() {
        for &ppp in &[1.0, 1.5, 2.0] {
            for &size in &[16.0, 13.0, 9.6, 12.0] {
                let ys: Vec<f32> = drawn(Icon::Hamburger, ppp, size)
                    .into_iter()
                    .filter_map(|s| if let Shape::Path(p) = s { Some(p.points[0].y * ppp) } else { None })
                    .collect();
                assert_eq!(ys.len(), 3);
                let (g1, g2) = (ys[1] - ys[0], ys[2] - ys[1]);
                assert!((g1 - g2).abs() < 1e-3 && g1 >= 2.0, "{ppp}x size {size}: gaps {g1} and {g2} px");
            }
        }
    }
}
