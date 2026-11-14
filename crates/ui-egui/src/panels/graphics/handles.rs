use super::canvas::{anchor_screen, screen_scale, sp, unscale};
use super::text_edit::text_of;
use super::{ClipId, LayerContent, LayerView, Pos2, Value, json};

#[derive(Clone, Copy, Debug)]
pub(super) enum DragKind {
    Move,
    /// Handle `n` of the layer's box (see [`handle_points`] and `HandleDrag`).
    Handle(usize),
    /// The layer's anchor point: it moves, the layer stays where it is.
    Anchor,
    /// Corner-radius handle `n` of a rectangle (see [`corner_handles`]).
    Corner(usize),
    NewShape,
    /// The Type tool dragging out the box of a new paragraph-text layer.
    NewTextBox,
    TextSelect,
}

#[derive(Clone, Copy, Debug)]
pub(super) struct DragState {
    pub(super) kind: DragKind,
    pub(super) clip: ClipId,
    pub(super) layer: usize,
    pub(super) start: Pos2,
}

/// The eight handles of a layer's box `quad` (TL, TR, BR, BL): the corners (0–3), then the middles
/// of the top, right, bottom and left edges (4–7).
pub(crate) fn handle_points(quad: &[Pos2; 4]) -> [Pos2; 8] {
    let mid = |i: usize| quad[i] + (quad[(i + 1) % 4] - quad[i]) * 0.5;
    [quad[0], quad[1], quad[2], quad[3], mid(0), mid(1), mid(2), mid(3)]
}

/// Resize cursor for a selected graphics-box handle, following the handle's screen-space axis.
pub(super) fn resize_handle_cursor(v: &LayerView, at: Pos2) -> Option<egui::CursorIcon> {
    let quad = v.quad();
    let centre = quad[0] + (quad[2] - quad[0]) * 0.5;
    handle_points(&quad).into_iter().find(|point| (*point - at).length() <= 7.0).map(|point| crate::panels::transform_handles::resize_cursor(point - centre))
}

/// Dragging handle `n` of `quad` from `start` to `cur`: the box grows towards the pointer while
/// the opposite corner / edge stays put. A corner scales each axis by the pointer's travel along
/// it (both alike with `keep_ratio`, Shift); an edge scales only its own.
pub(super) struct HandleScale {
    /// The handle opposite the dragged one, which does not move.
    pin: Pos2,
    /// Unit vectors along the box's width and height on screen.
    ux: egui::Vec2,
    uy: egui::Vec2,
    fx: f32,
    fy: f32,
    /// Width and height scale apart (an edge, or a corner without Shift).
    apart: bool,
}

impl HandleScale {
    fn new(quad: &[Pos2; 4], n: usize, start: Pos2, cur: Pos2, keep_ratio: bool) -> Self {
        let pts = handle_points(quad);
        let n = n.min(7);
        let pin = if n < 4 { pts[(n + 2) % 4] } else { pts[4 + (n - 4 + 2) % 4] };
        let (ux, uy) = ((quad[1] - quad[0]).normalized(), (quad[3] - quad[0]).normalized());
        // the factor along `dir`: how far the pointer is from the pinned side, relative to the start
        let along = |dir: egui::Vec2| {
            let from = (start - pin).dot(dir);
            if from.abs() < 1.0 { 1.0 } else { ((cur - pin).dot(dir) / from).max(0.01) }
        };
        let (fx, fy) = match n {
            0..=3 if keep_ratio => {
                let f = along((start - pin).normalized());
                (f, f)
            }
            0..=3 => (along(ux), along(uy)),
            4 | 6 => (1.0, along(uy)),
            _ => (along(ux), 1.0),
        };
        Self { pin, ux, uy, fx, fy, apart: n >= 4 || !keep_ratio }
    }

    /// Where a screen point of the layer ends up.
    fn apply(&self, p: Pos2) -> Pos2 {
        let w = p - self.pin;
        self.pin + self.ux * (w.dot(self.ux) * self.fx) + self.uy * (w.dot(self.uy) * self.fy)
    }
}

/// A handle closer than this (in points) to the anchor point scales point text as if it were this
/// much further away, so that a handle on the anchor does not scale without bound. Measured in
/// Premiere Pro 26 with the Program monitor at Fit.
const NEAR_ANCHOR: f32 = 60.0;

/// Box `from` (`[x0, y0, x1, y1]`) with handle `n` moved by (`dx`, `dy`): that side or corner
/// follows, the opposite one stays, and the box keeps at least `min` across instead of flipping.
fn drag_sides(from: [f32; 4], n: usize, dx: f32, dy: f32, min: f32) -> [f32; 4] {
    let [mut x0, mut y0, mut x1, mut y1] = from;
    if matches!(n, 0 | 3 | 7) {
        x0 = (x0 + dx).min(x1 - min);
    }
    if matches!(n, 1 | 2 | 5) {
        x1 = (x1 + dx).max(x0 + min);
    }
    if matches!(n, 0 | 1 | 4) {
        y0 = (y0 + dy).min(y1 - min);
    }
    if matches!(n, 2 | 3 | 6) {
        y1 = (y1 + dy).max(y0 + min);
    }
    [x0, y0, x1, y1]
}

/// What dragging handle `n` of a layer from `start` to `cur` does, as Premiere Pro does it: point
/// text scales about its anchor point, a paragraph-text box is resized (its text re-wraps at the
/// same size), a shape's Size follows the handle (Scale untouched), and a path stretches away from
/// its opposite side.
pub(super) enum HandleDrag {
    Stretch(HandleScale),
    /// Scale both axes by `f` about the anchor point, which is at `anchor` on screen.
    Scale {
        anchor: Pos2,
        f: f32,
    },
    /// The text box becomes `rect` (`[x0, y0, x1, y1]` in the layer's pixels; it was `from`).
    /// `tall`: the drag sets the box's height too.
    Resize {
        rect: [f32; 4],
        from: [f32; 4],
        tall: bool,
    },
    /// The shape's box becomes `rect` (in the layer's pixels; it was `from`, with Size `size`).
    Size {
        rect: [f32; 4],
        from: [f32; 4],
        size: (f32, f32),
    },
}

impl HandleDrag {
    pub(super) fn new(v: &LayerView, n: usize, start: Pos2, cur: Pos2, keep_ratio: bool) -> Self {
        let n = n.min(7);
        let quad = v.quad();
        let (dx, dy) = match (v.to_local(start), v.to_local(cur)) {
            (Some(a), Some(b)) => (b.0 - a.0, b.1 - a.1),
            _ => (0.0, 0.0),
        };
        let Some(t) = text_of(&v.spec) else {
            let LayerContent::Shape(s) = &v.spec.content else { return Self::Stretch(HandleScale::new(&quad, n, start, cur, keep_ratio)) };
            // a path is its points, so it stretches; the other shapes are drawn from their Size
            if s.shape == 3 {
                return Self::Stretch(HandleScale::new(&quad, n, start, cur, keep_ratio));
            }
            let from = v.local;
            let mut rect = drag_sides(from, n, dx, dy, 1.0);
            let (w, h) = (from[2] - from[0], from[3] - from[1]);
            if keep_ratio && n < 4 && w > 0.0 && h > 0.0 {
                // Shift: the side that moved further sets the scale of both; the opposite corner stays
                let (fw, fh) = ((rect[2] - rect[0]) / w, (rect[3] - rect[1]) / h);
                let f = if (fw - 1.0).abs() >= (fh - 1.0).abs() { fw } else { fh };
                let f = f.max(1.0 / w).max(1.0 / h);
                if matches!(n, 0 | 3) {
                    rect[0] = rect[2] - w * f;
                } else {
                    rect[2] = rect[0] + w * f;
                }
                if matches!(n, 0 | 1) {
                    rect[1] = rect[3] - h * f;
                } else {
                    rect[3] = rect[1] + h * f;
                }
            }
            return Self::Size { rect, from, size: s.size };
        };
        if is_paragraph(t) {
            // a box without a height is as tall as its text
            let from = if t.box_height > 0.0 { [0.0, 0.0, t.box_width, t.box_height] } else { [0.0, v.local[1], t.box_width, v.local[3]] };
            let rect = drag_sides(from, n, dx, dy, (t.size * 0.5).max(1.0));
            return Self::Resize { rect, from, tall: t.box_height > 0.0 || !matches!(n, 5 | 7) };
        }
        // Every handle scales both axes alike. Only the pointer's travel along one side of the box
        // counts: along its width for the two side handles, along its height for all the others.
        let anchor = anchor_screen(v);
        let axis = if matches!(n, 5 | 7) { (quad[1] - quad[0]).normalized() } else { (quad[3] - quad[0]).normalized() };
        let handle = handle_points(&quad)[n];
        let from = (handle - anchor).dot(axis);
        // Away from the anchor grows. A handle sitting on the anchor counts as just inside the box
        // (where a letter's side bearing puts it in Premiere), so dragging it into the box grows.
        let centre = quad[0] + (quad[2] - quad[0]) * 0.5;
        let away = if from.abs() > 0.5 {
            from.signum()
        } else if (centre - handle).dot(axis) < 0.0 {
            -1.0
        } else {
            1.0
        };
        let reach = from.abs() + if from.abs() < NEAR_ANCHOR { NEAR_ANCHOR } else { 0.0 };
        let f = 1.0 + (cur - start).dot(axis) * away / reach;
        Self::Scale { anchor, f: if f.is_finite() { f.max(0.01) } else { 1.0 } }
    }

    /// The layer's box on screen as the drag leaves it.
    pub(super) fn quad(&self, v: &LayerView) -> [Pos2; 4] {
        match self {
            Self::Stretch(h) => v.quad().map(|q| h.apply(q)),
            Self::Scale { anchor, f } => v.quad().map(|q| *anchor + (q - *anchor) * *f),
            Self::Resize { rect: b, .. } | Self::Size { rect: b, .. } => {
                [(b[0], b[1]), (b[2], b[1]), (b[2], b[3]), (b[0], b[3])].map(|(x, y)| sp(&v.to_screen, x, y))
            }
        }
    }

    /// The `graphics.set` properties that carry out the drag.
    pub(super) fn props(&self, v: &LayerView) -> Value {
        let tr = &v.spec.transform;
        match self {
            Self::Stretch(h) => {
                // the layer scales about its anchor, so it also moves to keep the pinned side still
                let a = anchor_screen(v);
                let delta = unscale(&v.canvas_to_screen, h.apply(a) - a);
                let mut props = json!({
                    "scale": tr.scale.y * 100.0 * h.fy as f64,
                    "scale_width": tr.scale.x * 100.0 * h.fx as f64,
                    "position": [tr.position.x + delta.x, tr.position.y + delta.y],
                });
                if h.apart {
                    props["uniform_scale"] = json!(false);
                }
                props
            }
            Self::Scale { f, .. } => json!({"scale": tr.scale.y * 100.0 * *f as f64, "scale_width": tr.scale.x * 100.0 * *f as f64}),
            Self::Resize { rect, from, tall } => {
                // the box's top-left corner is the layer's origin: when it moves, the anchor point
                // is renumbered so that it (and the position) stay on the same spot
                let mut props = json!({
                    "box_width": rect[2] - rect[0],
                    "anchor": [tr.anchor.x - (rect[0] - from[0]) as f64, tr.anchor.y - (rect[1] - from[1]) as f64],
                });
                if *tall {
                    props["box_height"] = json!(rect[3] - rect[1]);
                }
                props
            }
            Self::Size { rect, from, size } => {
                // The box scales with Size about the layer's origin, so the origin moves to `o` (in
                // the old layer pixels) to put the box at `rect`. The anchor point is renumbered so
                // that it, the Position and the side that was not dragged stay where they are.
                let f = |a: f32, b: f32| if b > 0.0 { a / b } else { 1.0 };
                let (fx, fy) = (f(rect[2] - rect[0], from[2] - from[0]), f(rect[3] - rect[1], from[3] - from[1]));
                let o = (rect[0] - from[0] * fx, rect[1] - from[1] * fy);
                json!({
                    "size": [size.0 * fx, size.1 * fy],
                    "anchor": [tr.anchor.x - o.0 as f64, tr.anchor.y - o.1 as f64],
                })
            }
        }
    }
}

/// Paragraph text: wrapped in a box (vertical text never is).
pub(super) fn is_paragraph(t: &filmcraft_project::graphic::TextProps) -> bool {
    t.box_width > 0.0 && !t.vertical
}

/// Corner-radius handles sit at least this far (in points) inside a rectangle's corners, and show
/// only on a rectangle at least four times as wide and tall on screen.
const CORNER_INSET: f32 = 12.0;

/// A rectangle layer's corner radius as it is drawn (at most half its shorter side) and its four
/// corner-radius handles on screen (TL, TR, BR, BL); `None` for other layers and for a rectangle
/// too small on screen to hold them.
pub(super) fn corner_handles(v: &LayerView) -> Option<(f32, [Pos2; 4])> {
    let LayerContent::Shape(s) = &v.spec.content else { return None };
    let q = v.quad();
    if s.shape != 0 || (q[1] - q[0]).length() < 4.0 * CORNER_INSET || (q[3] - q[0]).length() < 4.0 * CORNER_INSET {
        return None;
    }
    let r = s.corner_radius.min(s.size.0 / 2.0).min(s.size.1 / 2.0).max(0.0);
    Some((r, corner_points(v, r)))
}

/// Where the corner-radius handles of rectangle `v` sit for radius `r`: on each corner's diagonal,
/// where the rounded corner's arc is centred, but at least [`CORNER_INSET`] points in.
pub(super) fn corner_points(v: &LayerView, r: f32) -> [Pos2; 4] {
    let b = v.local;
    let d = r.max(CORNER_INSET / screen_scale(&v.to_screen).max(1e-6)).min((b[2] - b[0]) / 2.0).min((b[3] - b[1]) / 2.0);
    [(b[0] + d, b[1] + d), (b[2] - d, b[1] + d), (b[2] - d, b[3] - d), (b[0] + d, b[3] - d)].map(|(x, y)| sp(&v.to_screen, x, y))
}

/// The corner radius after dragging handle `n` of rectangle `v` (radius `r0`) from `start` to
/// `cur`: the pointer's travel into the shape along the corner's diagonal, up to half the shorter
/// side.
pub(super) fn corner_drag(v: &LayerView, r0: f32, n: usize, start: Pos2, cur: Pos2) -> f32 {
    let (Some(a), Some(b)) = (v.to_local(start), v.to_local(cur)) else { return r0 };
    let (sx, sy) = match n {
        0 => (1.0, 1.0),
        1 => (-1.0, 1.0),
        2 => (-1.0, -1.0),
        _ => (1.0, -1.0),
    };
    let inward = ((b.0 - a.0) * sx + (b.1 - a.1) * sy) / 2.0;
    let half = ((v.local[2] - v.local[0]).min(v.local[3] - v.local[1]) / 2.0).max(0.0);
    (r0 + inward).clamp(0.0, half)
}

/// The outline of rectangle `v` with corner radius `r`, on screen.
pub(super) fn round_rect_outline(v: &LayerView, r: f32) -> Vec<Pos2> {
    let b = v.local;
    let mut pts = Vec::new();
    // each corner's arc, clockwise from the top-right
    for (cx, cy, from) in [(b[2] - r, b[1] + r, -90.0_f32), (b[2] - r, b[3] - r, 0.0), (b[0] + r, b[3] - r, 90.0), (b[0] + r, b[1] + r, 180.0)] {
        for i in 0..=8 {
            let a = (from + 90.0 * i as f32 / 8.0).to_radians();
            pts.push(sp(&v.to_screen, cx + r * a.cos(), cy + r * a.sin()));
        }
    }
    pts
}
