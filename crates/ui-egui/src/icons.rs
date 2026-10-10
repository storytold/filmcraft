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
    Keyframe,
    Stopwatch,
    Fx,
    Reset,
    Close,
    Fullscreen,
    Export,
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
                let mut nb = Vec::new();
                if i > 0 || closed {
                    nb.push(seg((i + n - 1) % n, i));
                }
                if i + 1 < n || closed {
                    nb.push(seg(i, (i + 1) % n));
                }
                let q = q[i];
                let vert = nb.iter().any(|s| s.0);
                let horiz = nb.iter().any(|s| s.1);
                if !vert && !horiz {
                    return q;
                }
                // across a straight segment: the stroke's middle; along it: a pixel edge, where it ends
                self.snap_pos(q, vert, horiz)
            })
            .collect()
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
}

/// Paint `icon` centred in `rect`.
pub fn paint(painter: &Painter, rect: Rect, icon: Icon, color: Color32) {
    let pen = Pen16 { painter, rect, color, width: 1.25 };
    use Icon::*;
    match icon {
        Selection => pen.fill(&[(4.0, 2.0), (4.0, 13.0), (6.8, 10.4), (8.8, 14.6), (10.4, 13.9), (8.5, 9.8), (12.2, 9.6)]),
        TrackSelectFwd => {
            pen.fill(&[(2.0, 3.0), (2.0, 11.0), (4.0, 9.2), (5.4, 12.2), (6.6, 11.6), (5.3, 8.7), (7.8, 8.5)]);
            pen.fill(&[(8.5, 4.5), (11.0, 7.0), (8.5, 9.5)]);
            pen.fill(&[(11.5, 4.5), (14.0, 7.0), (11.5, 9.5)]);
        }
        TrackSelectBack => {
            pen.fill(&[(14.0, 3.0), (14.0, 11.0), (12.0, 9.2), (10.6, 12.2), (9.4, 11.6), (10.7, 8.7), (8.2, 8.5)]);
            pen.fill(&[(7.5, 4.5), (5.0, 7.0), (7.5, 9.5)]);
            pen.fill(&[(4.5, 4.5), (2.0, 7.0), (4.5, 9.5)]);
        }
        Ripple => {
            pen.line(&[(9.0, 2.5), (6.5, 2.5), (6.5, 13.5), (9.0, 13.5)]);
            pen.fill(&[(10.0, 5.0), (13.5, 8.0), (10.0, 11.0)]);
            pen.line(&[(2.0, 8.0), (6.5, 8.0)]);
        }
        Rolling => {
            pen.line(&[(6.0, 2.5), (8.0, 2.5), (8.0, 13.5), (6.0, 13.5)]);
            pen.line(&[(10.0, 2.5), (8.0, 2.5)]);
            pen.line(&[(10.0, 13.5), (8.0, 13.5)]);
            pen.fill(&[(5.0, 5.5), (2.0, 8.0), (5.0, 10.5)]);
            pen.fill(&[(11.0, 5.5), (14.0, 8.0), (11.0, 10.5)]);
        }
        RateStretch => {
            pen.line(&[(2.5, 3.0), (2.5, 13.0)]);
            pen.line(&[(13.5, 3.0), (13.5, 13.0)]);
            pen.line(&[(4.5, 8.0), (11.5, 8.0)]);
            pen.fill(&[(4.0, 8.0), (6.5, 6.0), (6.5, 10.0)]);
            pen.fill(&[(12.0, 8.0), (9.5, 6.0), (9.5, 10.0)]);
            pen.circle(8.0, 4.0, 1.4);
            pen.circle(8.0, 12.0, 1.4);
        }
        Remix => {
            // original: a waveform cut in three blocks that swap places (two arrows over a gap)
            pen.line(&[(2.0, 8.0), (3.0, 5.5), (4.0, 10.5), (5.0, 7.0)]);
            pen.line(&[(11.0, 7.0), (12.0, 10.5), (13.0, 5.5), (14.0, 8.0)]);
            pen.line(&[(7.0, 3.0), (7.0, 13.0)]);
            pen.line(&[(9.0, 3.0), (9.0, 13.0)]);
            pen.line(&[(4.0, 2.5), (12.0, 2.5)]);
            pen.fill(&[(12.5, 2.5), (10.5, 1.0), (10.5, 4.0)]);
            pen.line(&[(12.0, 13.5), (4.0, 13.5)]);
            pen.fill(&[(3.5, 13.5), (5.5, 12.0), (5.5, 14.8)]);
        }
        Razor => {
            pen.closed(&[(2.5, 5.0), (13.5, 5.0), (13.5, 11.0), (2.5, 11.0)]);
            pen.circle(8.0, 8.0, 1.3);
            pen.line(&[(5.0, 8.0), (6.5, 8.0)]);
            pen.line(&[(9.5, 8.0), (11.0, 8.0)]);
        }
        Slip => {
            pen.line(&[(2.5, 4.0), (2.5, 12.0)]);
            pen.line(&[(13.5, 4.0), (13.5, 12.0)]);
            pen.fill(&[(4.5, 8.0), (7.0, 6.0), (7.0, 10.0)]);
            pen.fill(&[(11.5, 8.0), (9.0, 6.0), (9.0, 10.0)]);
            pen.line(&[(5.5, 3.0), (10.5, 3.0)]);
            pen.line(&[(5.5, 13.0), (10.5, 13.0)]);
        }
        Slide => {
            pen.line(&[(5.5, 3.0), (5.5, 13.0)]);
            pen.line(&[(10.5, 3.0), (10.5, 13.0)]);
            pen.fill(&[(1.5, 8.0), (4.0, 6.0), (4.0, 10.0)]);
            pen.fill(&[(14.5, 8.0), (12.0, 6.0), (12.0, 10.0)]);
        }
        Pen => {
            pen.closed(&[(8.0, 2.0), (12.0, 9.0), (9.5, 13.5), (6.5, 13.5), (4.0, 9.0)]);
            pen.line(&[(8.0, 2.0), (8.0, 8.0)]);
            pen.dot(8.0, 9.0, 1.0);
        }
        Rectangle => pen.rect(2.5, 4.0, 13.5, 12.0),
        Ellipse => pen.circle(8.0, 8.0, 5.5),
        Hand => {
            pen.line(&[(5.0, 8.5), (5.0, 4.0), (6.5, 3.2), (7.3, 4.0), (7.3, 7.5)]);
            pen.line(&[(7.3, 4.0), (7.3, 2.8), (8.8, 2.2), (9.6, 3.0), (9.6, 7.5)]);
            pen.line(&[(9.6, 3.6), (11.0, 3.2), (11.9, 4.0), (11.9, 8.0)]);
            pen.line(&[(11.9, 5.5), (13.0, 5.3), (13.6, 6.0), (13.6, 10.0), (11.5, 14.0), (6.5, 14.0), (3.0, 10.0), (2.5, 8.5), (3.5, 7.8), (5.0, 8.5)]);
        }
        Zoom => {
            pen.circle(6.8, 6.8, 4.3);
            pen.line(&[(10.0, 10.0), (14.0, 14.0)]);
            pen.line(&[(4.8, 6.8), (8.8, 6.8)]);
            pen.line(&[(6.8, 4.8), (6.8, 8.8)]);
        }
        Type => {
            pen.line(&[(3.0, 4.5), (3.0, 2.8), (13.0, 2.8), (13.0, 4.5)]);
            pen.line(&[(8.0, 2.8), (8.0, 13.5)]);
            pen.line(&[(6.0, 13.5), (10.0, 13.5)]);
        }
        Play => pen.fill(&[(4.5, 2.8), (13.0, 8.0), (4.5, 13.2)]),
        Pause => {
            pen.rect_fill(4.0, 3.0, 6.8, 13.0);
            pen.rect_fill(9.2, 3.0, 12.0, 13.0);
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
        MarkIn => pen.line(&[
            (10.5, 2.5),
            (8.5, 2.5),
            (7.5, 3.0),
            (7.0, 4.0),
            (7.0, 6.0),
            (6.5, 7.0),
            (5.0, 8.0),
            (6.5, 9.0),
            (7.0, 10.0),
            (7.0, 12.0),
            (7.5, 13.0),
            (8.5, 13.5),
            (10.5, 13.5),
        ]),
        MarkOut => pen.line(&[
            (5.5, 2.5),
            (7.5, 2.5),
            (8.5, 3.0),
            (9.0, 4.0),
            (9.0, 6.0),
            (9.5, 7.0),
            (11.0, 8.0),
            (9.5, 9.0),
            (9.0, 10.0),
            (9.0, 12.0),
            (8.5, 13.0),
            (7.5, 13.5),
            (5.5, 13.5),
        ]),
        Marker => pen.fill(&[(4.0, 2.5), (12.0, 2.5), (12.0, 9.5), (8.0, 13.5), (4.0, 9.5)]),
        Insert => {
            pen.rect(2.0, 5.0, 14.0, 13.0);
            pen.fill(&[(5.5, 1.5), (10.5, 1.5), (8.0, 5.5)]);
            pen.line(&[(8.0, 5.0), (8.0, 13.0)]);
        }
        Overwrite => {
            pen.rect(2.0, 7.0, 14.0, 13.0);
            pen.rect_fill(5.0, 8.5, 11.0, 11.5);
            pen.fill(&[(5.5, 1.5), (10.5, 1.5), (8.0, 5.5)]);
        }
        Lift => {
            pen.rect(2.0, 8.0, 14.0, 13.5);
            pen.line(&[(8.0, 11.0), (8.0, 2.5)]);
            pen.line(&[(5.0, 5.5), (8.0, 2.5), (11.0, 5.5)]);
        }
        Extract => {
            pen.rect(2.0, 8.0, 14.0, 13.5);
            pen.line(&[(8.0, 11.0), (8.0, 2.5)]);
            pen.line(&[(5.0, 5.5), (8.0, 2.5), (11.0, 5.5)]);
            pen.line(&[(5.5, 10.75), (6.5, 10.75)]);
            pen.line(&[(9.5, 10.75), (10.5, 10.75)]);
        }
        Camera => {
            pen.closed(&[(2.0, 5.0), (5.0, 5.0), (6.0, 3.5), (10.0, 3.5), (11.0, 5.0), (14.0, 5.0), (14.0, 12.5), (2.0, 12.5)]);
            pen.circle(8.0, 8.7, 2.3);
        }
        Loop => {
            pen.arc(8.0, 8.0, 5.0, 200.0, 520.0);
            pen.fill(&[(1.8, 5.5), (5.2, 5.5), (3.2, 8.5)]);
        }
        Wrench => {
            pen.line(&[(3.0, 13.0), (9.0, 7.0)]);
            pen.arc(10.5, 5.5, 3.0, 110.0, 400.0);
        }
        Plus => {
            pen.line(&[(8.0, 3.0), (8.0, 13.0)]);
            pen.line(&[(3.0, 8.0), (13.0, 8.0)]);
        }
        Eye | EyeOff => {
            pen.line(&[(1.5, 8.0), (4.0, 5.0), (8.0, 3.8), (12.0, 5.0), (14.5, 8.0), (12.0, 11.0), (8.0, 12.2), (4.0, 11.0), (1.5, 8.0)]);
            pen.circle(8.0, 8.0, 2.0);
            if icon == EyeOff {
                pen.line(&[(2.5, 13.5), (13.5, 2.5)]);
            }
        }
        Speaker | Mute => {
            pen.fill(&[(2.0, 6.0), (5.0, 6.0), (9.0, 2.5), (9.0, 13.5), (5.0, 10.0), (2.0, 10.0)]);
            if icon == Mute {
                pen.line(&[(10.5, 6.0), (14.0, 10.0)]);
                pen.line(&[(14.0, 6.0), (10.5, 10.0)]);
            } else {
                pen.arc(9.0, 8.0, 3.0, -50.0, 50.0);
                pen.arc(9.0, 8.0, 5.5, -50.0, 50.0);
            }
        }
        Lock | Unlock => {
            pen.rect(3.5, 7.0, 12.5, 14.0);
            if icon == Lock {
                pen.arc(8.0, 7.0, 3.0, 180.0, 360.0);
            } else {
                pen.arc(8.0, 5.0, 3.0, 180.0, 330.0);
            }
            pen.dot(8.0, 10.5, 1.0);
        }
        SyncLock => {
            pen.rect(3.0, 7.0, 13.0, 14.0);
            pen.arc(8.0, 7.0, 3.0, 180.0, 360.0);
            pen.line(&[(5.5, 10.5), (7.0, 12.0), (10.5, 9.0)]);
        }
        Mic => {
            pen.rect(6.0, 2.0, 10.0, 10.0);
            pen.arc(8.0, 8.0, 4.5, 0.0, 180.0);
            pen.line(&[(8.0, 12.5), (8.0, 14.5)]);
        }
        Folder => pen.closed(&[(1.5, 4.0), (6.0, 4.0), (7.5, 5.5), (14.5, 5.5), (14.5, 13.0), (1.5, 13.0)]),
        Film => {
            pen.rect(2.0, 3.0, 14.0, 13.0);
            for y in [5.0, 8.0, 11.0] {
                pen.dot(3.8, y, 0.6);
                pen.dot(12.2, y, 0.6);
            }
            pen.line(&[(5.5, 3.0), (5.5, 13.0)]);
            pen.line(&[(10.5, 3.0), (10.5, 13.0)]);
        }
        Sequence => {
            pen.rect(1.5, 3.5, 14.5, 12.5);
            pen.rect_fill(3.0, 5.5, 9.0, 7.5);
            pen.rect_fill(6.0, 9.0, 13.0, 11.0);
        }
        Audio => {
            for (i, h) in [3.0, 6.0, 9.0, 5.0, 7.0, 3.0].iter().enumerate() {
                let x = 2.5 + i as f32 * 2.2;
                pen.line(&[(x, 8.0 - h / 2.0), (x, 8.0 + h / 2.0)]);
            }
        }
        Image => {
            pen.rect(2.0, 3.0, 14.0, 13.0);
            pen.line(&[(2.5, 12.0), (6.0, 8.0), (9.0, 11.0), (11.0, 9.0), (13.5, 12.0)]);
            pen.dot(10.5, 6.0, 1.1);
        }
        Search => {
            pen.circle(6.8, 6.8, 4.3);
            pen.line(&[(10.0, 10.0), (14.0, 14.0)]);
        }
        ListView => {
            for y in [4.0, 8.0, 12.0] {
                pen.dot(3.0, y, 0.8);
                pen.line(&[(5.5, y), (14.0, y)]);
            }
        }
        IconView => {
            pen.rect(2.5, 2.5, 7.0, 7.0);
            pen.rect(9.0, 2.5, 13.5, 7.0);
            pen.rect(2.5, 9.0, 7.0, 13.5);
            pen.rect(9.0, 9.0, 13.5, 13.5);
        }
        Freeform => {
            pen.rect(2.0, 3.0, 8.0, 8.0);
            pen.rect(7.0, 9.0, 14.0, 13.5);
            pen.rect(10.0, 2.0, 14.0, 6.0);
        }
        NewItem => {
            pen.closed(&[(3.0, 2.0), (10.0, 2.0), (13.0, 5.0), (13.0, 14.0), (3.0, 14.0)]);
            pen.line(&[(8.0, 6.0), (8.0, 11.0)]);
            pen.line(&[(5.5, 8.5), (10.5, 8.5)]);
        }
        Trash => {
            pen.line(&[(2.5, 4.0), (13.5, 4.0)]);
            pen.line(&[(6.0, 4.0), (6.5, 2.5), (9.5, 2.5), (10.0, 4.0)]);
            pen.closed(&[(3.8, 4.0), (12.2, 4.0), (11.4, 14.0), (4.6, 14.0)]);
        }
        Home => {
            pen.line(&[(1.5, 8.0), (8.0, 2.0), (14.5, 8.0)]);
            pen.line(&[(3.5, 6.5), (3.5, 14.0), (12.5, 14.0), (12.5, 6.5)]);
            pen.line(&[(6.5, 14.0), (6.5, 10.0), (9.5, 10.0), (9.5, 14.0)]);
        }
        Workspaces => {
            pen.rect(1.5, 2.5, 14.5, 13.5);
            pen.line(&[(6.0, 2.5), (6.0, 13.5)]);
            pen.line(&[(6.0, 8.0), (14.5, 8.0)]);
        }
        Hamburger => {
            for y in [4.5, 8.0, 11.5] {
                pen.line(&[(3.0, y), (13.0, y)]);
            }
        }
        ChevronDown => pen.line(&[(4.0, 6.0), (8.0, 10.0), (12.0, 6.0)]),
        ChevronRight => pen.line(&[(6.0, 4.0), (10.0, 8.0), (6.0, 12.0)]),
        Magnet => {
            pen.arc(8.0, 8.0, 4.5, 0.0, 180.0);
            pen.line(&[(3.5, 8.0), (3.5, 2.5)]);
            pen.line(&[(12.5, 8.0), (12.5, 2.5)]);
            pen.rect_fill(2.5, 2.5, 4.8, 4.8);
            pen.rect_fill(11.2, 2.5, 13.5, 4.8);
        }
        Link => {
            pen.closed(&[(2.0, 6.0), (8.0, 6.0), (8.0, 10.0), (2.0, 10.0)]);
            pen.closed(&[(8.0, 6.0), (14.0, 6.0), (14.0, 10.0), (8.0, 10.0)]);
        }
        Keyframe => pen.fill(&[(8.0, 3.0), (13.0, 8.0), (8.0, 13.0), (3.0, 8.0)]),
        Stopwatch => {
            pen.circle(8.0, 9.0, 5.0);
            pen.line(&[(8.0, 9.0), (8.0, 6.0)]);
            pen.line(&[(6.5, 2.0), (9.5, 2.0)]);
            pen.line(&[(8.0, 2.0), (8.0, 4.0)]);
        }
        Fx => {
            pen.line(&[(7.5, 2.5), (6.0, 2.5), (5.0, 4.0), (4.0, 13.5)]);
            pen.line(&[(2.5, 6.5), (7.0, 6.5)]);
            pen.line(&[(8.5, 7.0), (13.5, 13.5)]);
            pen.line(&[(13.5, 7.0), (8.5, 13.5)]);
        }
        Reset => {
            pen.arc(8.0, 8.5, 5.0, 200.0, 500.0);
            pen.fill(&[(1.5, 6.0), (5.5, 5.5), (3.0, 9.0)]);
        }
        Close => {
            pen.line(&[(4.0, 4.0), (12.0, 12.0)]);
            pen.line(&[(12.0, 4.0), (4.0, 12.0)]);
        }
        Fullscreen => {
            pen.line(&[(2.5, 6.0), (2.5, 2.5), (6.0, 2.5)]);
            pen.line(&[(10.0, 2.5), (13.5, 2.5), (13.5, 6.0)]);
            pen.line(&[(13.5, 10.0), (13.5, 13.5), (10.0, 13.5)]);
            pen.line(&[(6.0, 13.5), (2.5, 13.5), (2.5, 10.0)]);
        }
        Export => {
            pen.line(&[(8.0, 10.0), (8.0, 2.0)]);
            pen.line(&[(5.0, 5.0), (8.0, 2.0), (11.0, 5.0)]);
            pen.line(&[(3.0, 8.0), (3.0, 14.0), (13.0, 14.0), (13.0, 8.0)]);
        }
        Gear => {
            pen.circle(8.0, 8.0, 2.2);
            for i in 0..8 {
                let a = (i as f32 * 45.0).to_radians();
                pen.line(&[(8.0 + 4.0 * a.cos(), 8.0 + 4.0 * a.sin()), (8.0 + 6.0 * a.cos(), 8.0 + 6.0 * a.sin())]);
            }
            pen.circle(8.0, 8.0, 4.2);
        }
        Info => {
            pen.circle(8.0, 8.0, 6.0);
            pen.line(&[(8.0, 7.0), (8.0, 11.5)]);
            pen.dot(8.0, 4.8, 0.9);
        }
        Captions => {
            pen.rect(1.5, 3.0, 14.5, 13.0);
            pen.line(&[(4.0, 9.0), (7.0, 9.0)]);
            pen.line(&[(9.0, 9.0), (12.0, 9.0)]);
            pen.line(&[(4.0, 11.0), (12.0, 11.0)]);
        }
        Adjust => {
            for (y, x) in [(4.0, 10.0), (8.0, 5.0), (12.0, 9.0)] {
                pen.line(&[(2.0, y), (14.0, y)]);
                pen.dot(x, y, 1.5);
            }
        }
        Nest => {
            pen.rect(1.5, 2.5, 14.5, 13.5);
            pen.rect(4.5, 5.5, 11.5, 10.5);
        }
        Undo => {
            pen.arc(9.0, 9.0, 4.5, 180.0, 450.0);
            pen.fill(&[(2.0, 9.0), (4.5, 6.0), (7.0, 9.0)]);
        }
        Redo => {
            pen.arc(7.0, 9.0, 4.5, 90.0, 360.0);
            pen.fill(&[(14.0, 9.0), (11.5, 6.0), (9.0, 9.0)]);
        }
        Bell => {
            pen.line(&[(3.0, 12.0), (13.0, 12.0)]);
            pen.line(&[(4.5, 12.0), (4.5, 7.0)]);
            pen.line(&[(11.5, 12.0), (11.5, 7.0)]);
            pen.arc(8.0, 7.0, 3.5, 180.0, 360.0);
            pen.dot(8.0, 13.8, 1.0);
        }
        Chat => {
            pen.line(&[
                (3.5, 3.0),
                (12.5, 3.0),
                (14.0, 4.5),
                (14.0, 9.5),
                (12.5, 11.0),
                (7.0, 11.0),
                (4.0, 14.0),
                (4.5, 11.0),
                (3.5, 11.0),
                (2.0, 9.5),
                (2.0, 4.5),
                (3.5, 3.0),
            ]);
            pen.dot(5.5, 7.0, 0.9);
            pen.dot(8.0, 7.0, 0.9);
            pen.dot(10.5, 7.0, 0.9);
        }
        Globe => {
            pen.circle(8.0, 8.0, 6.0);
            pen.line(&[(2.0, 8.0), (14.0, 8.0)]);
            pen.line(&[(3.0, 5.0), (13.0, 5.0)]);
            pen.line(&[(3.0, 11.0), (13.0, 11.0)]);
            pen.line(&[(8.0, 2.0), (6.0, 5.0), (5.5, 8.0), (6.0, 11.0), (8.0, 14.0)]);
            pen.line(&[(8.0, 2.0), (10.0, 5.0), (10.5, 8.0), (10.0, 11.0), (8.0, 14.0)]);
        }
        Code => {
            pen.line(&[(5.0, 4.0), (1.5, 8.0), (5.0, 12.0)]);
            pen.line(&[(11.0, 4.0), (14.5, 8.0), (11.0, 12.0)]);
            pen.line(&[(9.5, 2.5), (6.5, 13.5)]);
        }
        Sparkle => {
            pen.fill(&[(8.0, 1.5), (9.5, 6.5), (14.5, 8.0), (9.5, 9.5), (8.0, 14.5), (6.5, 9.5), (1.5, 8.0), (6.5, 6.5)]);
        }
        Grid => {
            pen.rect(2.0, 2.0, 14.0, 14.0);
            pen.line(&[(6.0, 2.0), (6.0, 14.0)]);
            pen.line(&[(10.0, 2.0), (10.0, 14.0)]);
            pen.line(&[(2.0, 6.0), (14.0, 6.0)]);
            pen.line(&[(2.0, 10.0), (14.0, 10.0)]);
        }
        Square => pen.rect(3.0, 3.0, 13.0, 13.0),
        Proxy => {
            pen.rect(1.5, 3.0, 14.5, 13.0);
            pen.rect_fill(3.5, 8.0, 8.5, 11.5);
            pen.line(&[(9.5, 7.0), (12.5, 4.5)]);
            pen.line(&[(10.5, 4.5), (12.5, 4.5), (12.5, 6.5)]);
        }
        Offline => {
            pen.line(&[(7.0, 4.0), (4.5, 4.0), (2.5, 6.0), (2.5, 7.5), (4.0, 9.0)]);
            pen.line(&[(9.0, 12.0), (11.5, 12.0), (13.5, 10.0), (13.5, 8.5), (12.0, 7.0)]);
            pen.line(&[(5.5, 2.0), (6.5, 0.8)]);
            pen.line(&[(10.5, 14.0), (9.5, 15.2)]);
            pen.line(&[(3.0, 1.5), (3.5, 2.8)]);
            pen.line(&[(13.0, 14.5), (12.5, 13.2)]);
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
            for (i, w) in [12.0, 9.5, 7.0, 4.5].iter().enumerate() {
                let y = 3.5 + i as f32 * 3.0;
                pen.line(&[(2.0, y), (2.0 + w, y)]);
            }
        }
        Automate => {
            pen.rect_fill(1.5, 5.0, 4.5, 11.0);
            pen.rect_fill(5.5, 5.0, 8.5, 11.0);
            pen.rect_fill(9.5, 5.0, 12.0, 11.0);
            pen.line(&[(13.5, 5.0), (13.5, 11.0)]);
        }
        Star => {
            let pts: Vec<(f32, f32)> = (0..10)
                .map(|i| {
                    let a = (-90.0 + i as f32 * 36.0f32).to_radians();
                    let r = if i % 2 == 0 { 6.5 } else { 2.7 };
                    (8.0 + r * a.cos(), 8.5 + r * a.sin())
                })
                .collect();
            pen.closed(&pts);
        }
        ChevronLeft => pen.line(&[(10.0, 4.0), (6.0, 8.0), (10.0, 12.0)]),
        ArrowUp => {
            pen.line(&[(8.0, 13.5), (8.0, 3.0)]);
            pen.line(&[(4.0, 7.0), (8.0, 3.0), (12.0, 7.0)]);
        }
        Drive => {
            pen.rect(1.5, 5.0, 14.5, 11.5);
            pen.dot(12.0, 8.25, 0.8);
            pen.line(&[(3.5, 8.25), (8.0, 8.25)]);
        }
        Network => {
            pen.circle(8.0, 8.0, 6.0);
            pen.line(&[(2.0, 8.0), (14.0, 8.0)]);
            pen.arc(8.0, 8.0, 6.0, -90.0, 90.0);
            pen.line(&[(8.0, 2.0), (8.0, 14.0)]);
        }
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
            pen.circle(8.0, 8.0, 2.8);
            for i in 0..8 {
                let a = (i as f32 * 45.0).to_radians();
                let (c, s) = (a.cos(), a.sin());
                pen.line(&[(8.0 + 4.6 * c, 8.0 + 4.6 * s), (8.0 + 6.2 * c, 8.0 + 6.2 * s)]);
            }
        }
        Moon => {
            // The outer circle's arc through the bottom left, back along a smaller circle that
            // bites the top right out of it.
            let arc = |cx: f32, cy: f32, r: f32, a0: f32, a1: f32| {
                (0..=12).map(move |i| {
                    let a = (a0 + (a1 - a0) * i as f32 / 12.0).to_radians();
                    (cx + r * a.cos(), cy + r * a.sin())
                })
            };
            let pts: Vec<(f32, f32)> = arc(8.0, 8.5, 5.5, 8.56, 261.44).chain(arc(11.2, 5.3, 4.6, 209.12, 60.88).skip(1)).collect();
            pen.closed(&pts);
        }
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
        Keyframe,
        Stopwatch,
        Fx,
        Reset,
        Close,
        Fullscreen,
        Export,
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
                        }
                    }
                }
            }
        }
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
