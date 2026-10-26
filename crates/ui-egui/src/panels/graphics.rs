//! Graphics UI (M10.2): the Program-monitor overlay for graphic clips and the graphic sections of
//! the Properties / Essential Graphics panels.
//!
//! Monitor overlay:
//! - **Type tool (T):** click on empty picture → a new point-text layer (in the selected graphic
//!   clip under the playhead, else a new graphic clip) with a caret; drag on empty picture → a new
//!   paragraph-text layer whose box is the dragged rectangle; click on a text layer → caret at
//!   the click. Typing edits the text (one undo step per typing session); arrows, Home/End,
//!   Shift-selection, ⌘A, ⌘C/⌘X/⌘V, Return (new line) and Esc (stop editing) work as in a text
//!   field. Double-clicking a text layer with the Selection tool also edits it.
//! - **Selection tool:** click selects a layer (bounding box with handles and anchor point); drag
//!   moves it; drag the anchor point to move that alone. What a handle does depends on the layer
//!   (see `HandleDrag`): point text scales about its anchor point, a paragraph-text box is
//!   resized and its text re-wraps, a shape stretches away from its opposite side. A rectangle
//!   also has four small circles just inside its corners: dragging one rounds all four corners.
//! - **Rectangle / Ellipse tools:** drag out a shape. **Pen tool:** click points; click the first
//!   point again (or press Return) to close the path; Esc cancels.
//!
//! All edits go through `graphics.*` commands. Automation ids: `program.layer.<clip>.<layer>`,
//! `program.layer.<clip>.<layer>.handle.<n>`, `program.layer.<clip>.<layer>.anchor`,
//! `program.layer.<clip>.<layer>.corner.<n>`,
//! `program.textEdit`, `graphics.*` in the panels.

use crate::widgets::CenteredDialog as _;
use egui::ime_context::ImeTarget;
use egui::{Align2, Color32, Pos2, Rect, Sense, Stroke, StrokeKind, pos2, vec2};
use filmcraft_engine::Session;
use filmcraft_geom::{Affine, Vec2};
use filmcraft_project::graphic::{self, LayerContent, LayerSpec, eval_layer, layer_display_name, layer_indices};
use filmcraft_project::{ClipId, EffectInstance, ItemKind, ParamValue, TrackItem};
use filmcraft_render::graphic_clip::{layer_local_bounds, layer_matrix, text_layout};
use filmcraft_time::Tick;
use serde_json::{Value, json};

use crate::FilmcraftApp;
use crate::icons::{self, Icon};
use crate::state::{GfxEdit, TextPropsDialog, Tool};
use crate::theme::Tokens;
use crate::widgets::HotNumberOptions;

/// A graphic layer visible at the playhead, with its layer→screen transform.
pub struct LayerView {
    pub clip: ClipId,
    pub layer: usize,
    pub spec: LayerSpec,
    pub to_screen: Affine,
    /// Graphic canvas → screen (the clip's Motion and the monitor fit).
    pub canvas_to_screen: Affine,
    pub local: [f32; 4],
}

/// The Program monitor editor drawn in one pass, remembered so the end-of-pass IME check can run
/// after that pass's control requests have been drained. It holds only geometry, never a copy of
/// the layer's text.
#[derive(Clone, Copy, Debug)]
pub(crate) struct DrawnImeEditor {
    pub viewport: egui::ViewportId,
    pub pass_nr: u64,
    pub document_epoch: u64,
    pub sequence: Option<u64>,
    pub target: ImeTarget,
    pub clip: u64,
    pub layer: usize,
    /// Identity of the layer drawn at that index; a reorder after the draw is detected here even
    /// when the numeric index (and therefore a uid-less owner key) stayed the same.
    pub fingerprint: u64,
    /// Identity of the committed text drawn in this pass. A control request that changes the
    /// text after the draw must not publish the old cursor geometry with the new text; the next
    /// pass draws and publishes it. A caret/selection-only change keeps the text, so the IME
    /// context stays.
    pub text_stamp: u64,
    /// Every layout-affecting layer property drawn in this pass, evaluated at the playhead. A
    /// same-pass control request can change the font, style, alignment, spacing, box or transform
    /// without changing the committed text or the layer identity; the cursor rectangle from the
    /// old layout must then not be published with the new one.
    pub layout_fingerprint: u64,
}

/// The most marked text drawn; an input method's composition is a few words.
const MAX_PREEDIT_BYTES: usize = 1024;

/// What [`draw_preedit`] drew, for automation: the plate with the marked text, and the active
/// clause's rectangle and text.
struct DrawnPreedit {
    plate: Rect,
    text: String,
    clause: Option<(Rect, String)>,
}

/// Draw the marked text from the caret line `top`–`bottom` until it commits: on a dark plate,
/// underlined, with a heavier underline under the clause being converted, or a caret where an
/// empty active range puts it.
fn draw_preedit(painter: &egui::Painter, top: Pos2, bottom: Pos2, preedit: craft_ui::ime::Preedit<'_>) -> DrawnPreedit {
    let text = preedit.text.get(..char_boundary_floor(preedit.text, MAX_PREEDIT_BYTES)).unwrap_or_default();
    let chars = text.chars().count();
    let active = preedit.active.map(|r| {
        let start = r.start.min(chars);
        start..r.end.clamp(start, chars)
    });
    let height = (bottom - top).length();
    let size = if height.is_finite() { (height * 0.8).clamp(8.0, 256.0) } else { 12.0 };
    let galley = painter.layout_no_wrap(text.to_string(), egui::FontId::proportional(size), Color32::WHITE);
    // centred on the caret line, where the committed text will be
    let rise = bottom.y - top.y;
    let at = top + vec2(0.0, if rise.is_finite() { (rise - galley.size().y) / 2.0 } else { 0.0 });
    let plate = Rect::from_min_size(at, galley.size());
    painter.rect_filled(plate.expand(2.0), 2.0, Color32::from_black_alpha(160));
    let x = |i: usize| at.x + galley.pos_from_cursor(egui::text::CCursor::new(i)).min.x;
    let y = plate.bottom();
    painter.line_segment([pos2(plate.left(), y), pos2(plate.right(), y)], Stroke::new(1.0, Color32::WHITE));
    let mut clause = None;
    match active {
        Some(r) if r.start < r.end => {
            let (from, to) = (x(r.start), x(r.end));
            painter.line_segment([pos2(from, y), pos2(to, y)], Stroke::new(3.0, Color32::WHITE));
            clause = Some((Rect::from_min_max(pos2(from, plate.top()), pos2(to, y)), text.chars().skip(r.start).take(r.len()).collect()));
        }
        Some(r) => {
            let caret = x(r.start);
            painter.line_segment([pos2(caret, plate.top()), pos2(caret, y)], Stroke::new(1.5, Color32::WHITE));
        }
        None => {}
    }
    painter.galley(at, galley, Color32::WHITE);
    DrawnPreedit { plate, text: text.to_string(), clause }
}

/// Identity of committed text alone (no caret or selection).
fn ime_text_stamp(text: &str) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for &b in text.as_bytes() {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h ^ (text.len() as u64).wrapping_mul(0x9e37_79b9_7f4a_7c15)
}

impl LayerView {
    pub fn quad(&self) -> [Pos2; 4] {
        let b = self.local;
        [(b[0], b[1]), (b[2], b[1]), (b[2], b[3]), (b[0], b[3])].map(|(x, y)| sp(&self.to_screen, x, y))
    }
    fn to_local(&self, p: Pos2) -> Option<(f32, f32)> {
        let v = self.to_screen.inverse()?.apply(Vec2::new(p.x as f64, p.y as f64));
        Some((v.x as f32, v.y as f32))
    }
    pub(crate) fn hit(&self, p: Pos2) -> bool {
        let pad = 4.0 / screen_scale(&self.to_screen).max(1e-6);
        self.to_local(p).is_some_and(|(x, y)| x >= self.local[0] - pad && x <= self.local[2] + pad && y >= self.local[1] - pad && y <= self.local[3] + pad)
    }
    pub(crate) fn is_text(&self) -> bool {
        matches!(self.spec.content, LayerContent::Text(_))
    }
}

fn sp(a: &Affine, x: f32, y: f32) -> Pos2 {
    let v = a.apply(Vec2::new(x as f64, y as f64));
    pos2(v.x as f32, v.y as f32)
}

fn screen_scale(a: &Affine) -> f32 {
    (a.a * a.d - a.b * a.c).abs().sqrt() as f32
}

/// Whether a clip id is a graphic clip in the active sequence.
pub fn is_graphic_clip(app: &FilmcraftApp, c: ClipId) -> bool {
    app.session
        .active_sequence()
        .and_then(|q| q.find_item(c))
        .and_then(|(_, it)| app.session.project.item(it.item))
        .is_some_and(|p| matches!(p.kind, ItemKind::Graphic { .. }))
}

/// The selected graphic clip, if any.
pub fn selected_graphic(app: &FilmcraftApp) -> Option<(ClipId, TrackItem)> {
    let q = app.session.active_sequence()?;
    app.session.state.selection.iter().copied().find(|c| is_graphic_clip(app, *c)).and_then(|c| q.find_item(c).map(|(_, it)| (c, it.clone())))
}

/// Layers of the graphic clips visible at the playhead, front-most first. A layer whose
/// visibility is off is not in the picture, so it has no box and cannot be clicked either.
/// Sequence pixels → screen points on the monitor picture `pic`. The picture shows the frame at its
/// display aspect, so with non-square sequence pixels the two axes scale differently.
pub(crate) fn frame_to_screen(pic: Rect, frame: (u32, u32)) -> Affine {
    let (kx, ky) = (pic.width() as f64 / frame.0.max(1) as f64, pic.height() as f64 / frame.1.max(1) as f64);
    Affine::translate(pic.min.x as f64, pic.min.y as f64).then_apply(&Affine::scale(kx, ky))
}

pub fn visible_layers(app: &FilmcraftApp, pic: Rect, frame: (u32, u32)) -> Vec<LayerView> {
    let Some(seq) = app.session.active_sequence() else { return Vec::new() };
    let t = app.session.playhead();
    let screen = frame_to_screen(pic, frame);
    let mut out = Vec::new();
    for tr in seq.video_tracks.iter().rev() {
        if !tr.enabled {
            continue;
        }
        let Some(it) = tr.item_at(t) else { continue };
        if !it.enabled {
            continue;
        }
        let Some(ItemKind::Graphic { width, height, .. }) = app.session.project.item(it.item).map(|p| &p.kind) else { continue };
        let size = (*width, *height);
        let mt = it.source_time_at(t);
        let base = screen.then_apply(&filmcraft_render::motion_matrix(seq, it, size, None, mt));
        let idx = layer_indices(&it.effects);
        for (li, &ei) in idx.iter().enumerate().rev() {
            if let Some(spec) = eval_layer(&it.effects[ei], mt, size).filter(|s| s.enabled) {
                let local = layer_local_bounds(&spec);
                out.push(LayerView { clip: it.id, layer: li, to_screen: base.then_apply(&layer_matrix(&spec)), canvas_to_screen: base, spec, local });
            }
        }
    }
    out
}

/// Screen → graphic-canvas mapping for new layers: the selected graphic clip under the playhead
/// (its Motion applies), else the sequence frame.
fn canvas_target(app: &FilmcraftApp, pic: Rect, frame: (u32, u32)) -> (Option<ClipId>, Affine) {
    let screen = frame_to_screen(pic, frame);
    let t = app.session.playhead();
    if let Some((c, it)) = selected_graphic(app)
        && it.range().contains(t)
        && let Some(seq) = app.session.active_sequence()
        && let Some(ItemKind::Graphic { width, height, .. }) = app.session.project.item(it.item).map(|p| &p.kind)
    {
        let m = screen.then_apply(&filmcraft_render::motion_matrix(seq, &it, (*width, *height), None, it.source_time_at(t)));
        return (Some(c), m);
    }
    (None, screen)
}

fn to_canvas(m: &Affine, p: Pos2) -> Vec2 {
    m.inverse().map(|i| i.apply(Vec2::new(p.x as f64, p.y as f64))).unwrap_or_default()
}

fn text_of(spec: &LayerSpec) -> Option<&filmcraft_project::graphic::TextProps> {
    match &spec.content {
        LayerContent::Text(t) => Some(t),
        _ => None,
    }
}

fn prev_boundary(s: &str, i: usize) -> usize {
    s[..i.min(s.len())].char_indices().next_back().map_or(0, |(b, _)| b)
}
fn next_boundary(s: &str, i: usize) -> usize {
    let i = i.min(s.len());
    s[i..].chars().next().map_or(i, |c| i + c.len_utf8())
}

#[derive(Clone, Copy, Debug)]
enum DragKind {
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
struct DragState {
    kind: DragKind,
    clip: ClipId,
    layer: usize,
    start: Pos2,
}

/// The eight handles of a layer's box `quad` (TL, TR, BR, BL): the corners (0–3), then the middles
/// of the top, right, bottom and left edges (4–7).
pub(crate) fn handle_points(quad: &[Pos2; 4]) -> [Pos2; 8] {
    let mid = |i: usize| quad[i] + (quad[(i + 1) % 4] - quad[i]) * 0.5;
    [quad[0], quad[1], quad[2], quad[3], mid(0), mid(1), mid(2), mid(3)]
}

/// Resize cursor for a selected graphics-box handle, following the handle's screen-space axis.
fn resize_handle_cursor(v: &LayerView, at: Pos2) -> Option<egui::CursorIcon> {
    let quad = v.quad();
    let centre = quad[0] + (quad[2] - quad[0]) * 0.5;
    handle_points(&quad).into_iter().find(|point| (*point - at).length() <= 7.0).map(|point| super::transform_handles::resize_cursor(point - centre))
}

/// Dragging handle `n` of `quad` from `start` to `cur`: the box grows towards the pointer while
/// the opposite corner / edge stays put. A corner scales each axis by the pointer's travel along
/// it (both alike with `keep_ratio`, Shift); an edge scales only its own.
struct HandleScale {
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
enum HandleDrag {
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
    fn new(v: &LayerView, n: usize, start: Pos2, cur: Pos2, keep_ratio: bool) -> Self {
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
    fn quad(&self, v: &LayerView) -> [Pos2; 4] {
        match self {
            Self::Stretch(h) => v.quad().map(|q| h.apply(q)),
            Self::Scale { anchor, f } => v.quad().map(|q| *anchor + (q - *anchor) * *f),
            Self::Resize { rect: b, .. } | Self::Size { rect: b, .. } => {
                [(b[0], b[1]), (b[2], b[1]), (b[2], b[3]), (b[0], b[3])].map(|(x, y)| sp(&v.to_screen, x, y))
            }
        }
    }

    /// The `graphics.set` properties that carry out the drag.
    fn props(&self, v: &LayerView) -> Value {
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
fn is_paragraph(t: &filmcraft_project::graphic::TextProps) -> bool {
    t.box_width > 0.0 && !t.vertical
}

/// Corner-radius handles sit at least this far (in points) inside a rectangle's corners, and show
/// only on a rectangle at least four times as wide and tall on screen.
const CORNER_INSET: f32 = 12.0;

/// A rectangle layer's corner radius as it is drawn (at most half its shorter side) and its four
/// corner-radius handles on screen (TL, TR, BR, BL); `None` for other layers and for a rectangle
/// too small on screen to hold them.
fn corner_handles(v: &LayerView) -> Option<(f32, [Pos2; 4])> {
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
fn corner_points(v: &LayerView, r: f32) -> [Pos2; 4] {
    let b = v.local;
    let d = r.max(CORNER_INSET / screen_scale(&v.to_screen).max(1e-6)).min((b[2] - b[0]) / 2.0).min((b[3] - b[1]) / 2.0);
    [(b[0] + d, b[1] + d), (b[2] - d, b[1] + d), (b[2] - d, b[3] - d), (b[0] + d, b[3] - d)].map(|(x, y)| sp(&v.to_screen, x, y))
}

/// The corner radius after dragging handle `n` of rectangle `v` (radius `r0`) from `start` to
/// `cur`: the pointer's travel into the shape along the corner's diagonal, up to half the shorter
/// side.
fn corner_drag(v: &LayerView, r0: f32, n: usize, start: Pos2, cur: Pos2) -> f32 {
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
fn round_rect_outline(v: &LayerView, r: f32) -> Vec<Pos2> {
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

/// The layer's anchor point on screen.
fn anchor_screen(v: &LayerView) -> Pos2 {
    sp(&v.to_screen, v.spec.transform.anchor.x as f32, v.spec.transform.anchor.y as f32)
}

/// A screen offset in the units `m` maps to the screen (its translation left out).
pub(crate) fn unscale(m: &Affine, off: egui::Vec2) -> Vec2 {
    Affine { e: 0.0, f: 0.0, ..*m }.inverse().map(|i| i.apply(Vec2::new(off.x as f64, off.y as f64))).unwrap_or_default()
}

/// `graphics.newText` for the Type tool: point text at `at`, or paragraph text in the box from
/// `at` to `corner`.
fn new_text_action(app: &FilmcraftApp, pic: Rect, frame: (u32, u32), at: Pos2, corner: Option<Pos2>) -> (String, Value) {
    let (into, m) = canvas_target(app, pic, frame);
    let a = to_canvas(&m, at);
    let size = 100.0 * frame.1 as f64 / 1080.0;
    let mut prm = json!({"text": "", "position": [a.x, a.y], "size": size.round()});
    if let Some(b) = corner.map(|c| to_canvas(&m, c)) {
        let (w, h) = ((b.x - a.x).abs(), (b.y - a.y).abs());
        // a box too small to hold a letter was meant as a click
        if w >= 4.0 && h >= 4.0 {
            prm["position"] = json!([a.x.min(b.x), a.y.min(b.y)]);
            prm["box"] = json!([w, h]);
        }
    }
    if let Some(cl) = into {
        prm["clip"] = json!(cl.0);
    }
    ("graphics.newText#edit".into(), prm)
}

pub(crate) const TEXT_EDIT_ID: &str = "gfx-text-edit";

/// End text editing: type the marked text still being composed, clear `gfx_edit`, invalidate any
/// in-flight IME composition, surrender focus and forget the panel tag so a late commit cannot
/// reach the next editor.
fn end_edit(app: &mut FilmcraftApp, ui: &egui::Ui) {
    commit_marked_text(app, ui.ctx());
    app.ui.gfx_edit = None;
    // Any composition that was in flight belongs to the editor that just ended; a late commit
    // must not be inserted into whatever is edited next.
    app.invalidate_ime_composition();
    ui.memory_mut(|m| m.surrender_focus(egui::Id::unique(TEXT_EDIT_ID)));
    ui.data_mut(|d| d.remove::<crate::dock::PanelKind>(egui::Id::unique(TEXT_EDIT_ID).with("panel")));
}

/// End the composer's composition. Its marked text, when it is the edit's live composition, goes
/// into the layer's text at the caret, over the selection, as one `graphics.setText`: Korean
/// commits a syllable only when the next one starts, so the last one would be lost (#2445). The
/// composer drops the input method's late commit of that text.
fn commit_marked_text(app: &mut FilmcraftApp, ctx: &egui::Context) {
    let target = app.ui.gfx_edit.as_ref().and_then(|ed| current_graphics_ime_target(app, ed.clip, ed.layer));
    let live = target.is_some_and(|target| composing_live(app, ctx, target));
    let Some(marked) = app.ime_composer.end(ctx).filter(|_| live) else { return };
    let Some(ed) = app.ui.gfx_edit.as_ref() else { return };
    let Some(mut text) = current_layer_text(&app.session, ed.clip, ed.layer).map(str::to_string) else { return };
    let marked: String = marked.chars().filter(|c| !c.is_control()).collect();
    let (a, b) = (char_boundary_floor(&text, ed.caret.min(ed.anchor)), char_boundary_floor(&text, ed.caret.max(ed.anchor)));
    text.replace_range(a..b, &marked);
    let params = json!({"clip": ed.clip, "layer": ed.layer, "text": text, "merge": true});
    if let Err(e) = app.session.execute("graphics.setText", params) {
        app.ui.status = e.to_string();
    }
}

/// Whether the composer's marked text is `target`'s composition in `ctx`'s viewport, not one a
/// command, a control request or a caret move retired.
fn composing_live(app: &FilmcraftApp, ctx: &egui::Context, target: ImeTarget) -> bool {
    app.ime_composer_target == Some(target)
        && app.ime_composition == Some(crate::ImeComposition::Active(target))
        && app.ime_composition_viewport == Some(ctx.viewport_id())
}

/// A click on the Program monitor's empty space (the picture's, or around it): end the text edit
/// and let go of the selected graphic layers and mask (#683).
pub(crate) fn deselect_on_monitor(app: &mut FilmcraftApp, ui: &egui::Ui) {
    end_edit(app, ui);
    app.session.state.graphic_layers.clear();
    if app.session.state.selected_mask.is_some()
        && let Err(e) = app.session.execute("masks.select", json!({"none": true}))
    {
        app.ui.status = e.to_string();
    }
}

/// Draw the overlay and handle graphics tools on the Program monitor picture `pic`.
pub fn monitor_overlay(app: &mut FilmcraftApp, ui: &mut egui::Ui, pic: Rect, frame: (u32, u32)) {
    // A project command may have reordered or deleted layers since the last pass. Follow the
    // edited layer (or end the edit) before anything reads `gfx_edit`.
    reconcile_gfx_edit(app);
    let t = app.tokens;
    let views = visible_layers(app, pic, frame);
    let tool = app.ui.tool;
    let accent = t.accent;
    let painter = ui.painter().with_clip_rect(pic.expand(12.0));
    for v in &views {
        let q = v.quad();
        let r = Rect::from_points(&q);
        let name = layer_display_name_view(app, v);
        app.auto.add(&format!("program.layer.{}.{}", v.clip.0, v.layer), r, &name);
    }
    let sel_clip = selected_graphic(app).map(|(c, _)| c);
    let sel_layers = app.session.state.graphic_layers.clone();
    let editing = app.ui.gfx_edit.clone();
    // ---- boxes and handles
    let graphics_tool = matches!(tool, Tool::Selection | Tool::Type | Tool::VerticalType | Tool::Rectangle | Tool::Ellipse | Tool::Pen);
    if graphics_tool {
        for v in views.iter().filter(|v| Some(v.clip) == sel_clip) {
            let q = v.quad();
            let selected = sel_layers.contains(&v.layer);
            let is_edit = editing.as_ref().is_some_and(|e| e.clip == v.clip.0 && e.layer == v.layer);
            // only the selected layers have a box; that of the text being typed into is red, as in Premiere
            if !(selected || is_edit) {
                continue;
            }
            painter.add(egui::Shape::closed_line(q.to_vec(), Stroke::new(1.0, if is_edit { t.danger } else { accent })));
            if selected && !is_edit {
                let paragraph = text_of(&v.spec).filter(|tp| is_paragraph(tp));
                let label = if paragraph.is_some() { "resize handle" } else { "scale handle" };
                // text the box hides: its bottom-right handle turns into a red plus
                let overflow = paragraph.is_some_and(|tp| text_layout(tp).overflow);
                for (n, c) in q.iter().enumerate() {
                    let hr = Rect::from_center_size(*c, vec2(7.0, 7.0));
                    if overflow && n == 2 {
                        painter.rect_filled(hr.expand(1.0), 0.0, t.danger);
                        painter.line_segment([*c - vec2(3.0, 0.0), *c + vec2(3.0, 0.0)], Stroke::new(1.0, Color32::WHITE));
                        painter.line_segment([*c - vec2(0.0, 3.0), *c + vec2(0.0, 3.0)], Stroke::new(1.0, Color32::WHITE));
                    } else {
                        painter.rect_filled(hr, 0.0, Color32::WHITE);
                        painter.rect_stroke(hr, 0.0, Stroke::new(1.0, accent), StrokeKind::Middle);
                    }
                    app.auto.add(&format!("program.layer.{}.{}.handle.{n}", v.clip.0, v.layer), hr.expand(3.0), label);
                }
                for (n, m) in handle_points(&q).iter().enumerate().skip(4) {
                    let hr = Rect::from_center_size(*m, vec2(5.0, 5.0));
                    painter.rect_filled(hr, 0.0, Color32::WHITE);
                    app.auto.add(&format!("program.layer.{}.{}.handle.{n}", v.clip.0, v.layer), hr.expand(4.0), label);
                }
                // a rectangle's corner-radius handles, just inside its corners
                for (n, c) in corner_handles(v).map(|(_, pts)| pts).iter().flatten().enumerate() {
                    painter.circle_filled(*c, 3.0, Color32::WHITE);
                    painter.circle_stroke(*c, 3.0, Stroke::new(1.0, accent));
                    app.auto.add(&format!("program.layer.{}.{}.corner.{n}", v.clip.0, v.layer), Rect::from_center_size(*c, vec2(12.0, 12.0)), "corner radius");
                }
                // anchor point
                let a = anchor_screen(v);
                painter.circle_stroke(a, 4.0, Stroke::new(1.0, accent));
                painter.line_segment([a - vec2(6.0, 0.0), a + vec2(6.0, 0.0)], Stroke::new(1.0, accent));
                painter.line_segment([a - vec2(0.0, 6.0), a + vec2(0.0, 6.0)], Stroke::new(1.0, accent));
                app.auto.add(&format!("program.layer.{}.{}.anchor", v.clip.0, v.layer), Rect::from_center_size(a, vec2(12.0, 12.0)), "anchor point");
            }
        }
    }
    let id = egui::Id::unique("gfx-overlay");
    let resp = ui.interact(pic, id, if graphics_tool { Sense::click_and_drag() } else { Sense::hover() });
    let hover = resp.hover_pos();
    let mut actions: Vec<(String, Value)> = Vec::new();
    let drag_id = id.with("drag");
    let mut drag: Option<DragState> = ui.data(|d| d.get_temp(drag_id));

    // cursors
    if let Some(p) = hover {
        match tool {
            Tool::Type | Tool::VerticalType => ui.ctx().set_cursor_icon(egui::CursorIcon::Text),
            Tool::Rectangle | Tool::Ellipse | Tool::Pen => ui.ctx().set_cursor_icon(egui::CursorIcon::Crosshair),
            Tool::Selection => {
                let selected = || views.iter().filter(|v| Some(v.clip) == sel_clip && sel_layers.contains(&v.layer));
                let handle = selected().find_map(|v| resize_handle_cursor(v, p));
                let radius_handle = selected().find_map(|v| {
                    let centre = v.quad()[0] + (v.quad()[2] - v.quad()[0]) * 0.5;
                    corner_handles(v)?.1.iter().find(|c| (**c - p).length() <= 6.0).map(|c| super::transform_handles::resize_cursor(*c - centre))
                });
                if editing.as_ref().is_some_and(|e| views.iter().any(|v| v.clip.0 == e.clip && v.layer == e.layer && v.hit(p))) {
                    ui.ctx().set_cursor_icon(egui::CursorIcon::Text);
                } else if let Some(icon) = handle {
                    ui.ctx().set_cursor_icon(icon);
                } else if let Some(icon) = radius_handle {
                    ui.ctx().set_cursor_icon(icon);
                } else if views.iter().any(|v| v.hit(p)) {
                    ui.ctx().set_cursor_icon(egui::CursorIcon::Move);
                }
            }
            _ => {}
        }
    }

    // ---- text editing
    if let Some(ed) = editing.clone() {
        match views.iter().find(|v| v.clip.0 == ed.clip && v.layer == ed.layer && v.is_text()) {
            Some(v) => text_edit(app, ui, v, ed, &resp),
            None => end_edit(app, ui),
        }
    }
    let editing = app.ui.gfx_edit.clone();
    let edit_view = editing.as_ref().and_then(|e| views.iter().find(|v| v.clip.0 == e.clip && v.layer == e.layer));

    // ---- presses (the left button only: a middle or right drag leaves the layers alone)
    if graphics_tool && resp.drag_started_by(egui::PointerButton::Primary) {
        // where the button went down (a drag starts only after the pointer has moved a little)
        let p = ui.input(|i| i.pointer.press_origin()).or(resp.interact_pointer_pos()).unwrap_or(pic.center());
        let shift = ui.input(|i| i.modifiers.shift);
        let in_edit = edit_view.is_some_and(|v| v.hit(p));
        drag = None;
        if in_edit {
            if let (Some(v), Some(mut ed)) = (edit_view, editing.clone())
                && let Some(tp) = text_of(&v.spec)
            {
                let l = text_layout(tp);
                if let Some((x, y)) = v.to_local(p) {
                    let previous_selection = (ed.caret, ed.anchor);
                    ed.caret = l.hit(x, y);
                    if !shift {
                        ed.anchor = ed.caret;
                    }
                    if previous_selection != (ed.caret, ed.anchor) {
                        app.invalidate_ime_composition();
                    }
                    app.ui.gfx_edit = Some(ed);
                }
                drag = Some(DragState { kind: DragKind::TextSelect, clip: v.clip, layer: v.layer, start: p });
            }
        } else {
            match tool {
                Tool::Selection | Tool::Type | Tool::VerticalType => {
                    // of the selected layers: the anchor point first (it can sit on a handle), then the handles
                    let selected = || views.iter().filter(|v| Some(v.clip) == sel_clip && sel_layers.contains(&v.layer));
                    let anchor = selected().find(|v| tool == Tool::Selection && (anchor_screen(v) - p).length() <= 7.0);
                    let handle = selected().find_map(|v| handle_points(&v.quad()).iter().position(|c| (*c - p).length() <= 7.0).map(|n| (v.clip, v.layer, n)));
                    let corner = selected()
                        .find_map(|v| corner_handles(v).and_then(|(_, pts)| pts.iter().position(|c| (*c - p).length() <= 6.0)).map(|n| (v.clip, v.layer, n)));
                    if let Some(v) = anchor {
                        drag = Some(DragState { kind: DragKind::Anchor, clip: v.clip, layer: v.layer, start: p });
                    } else if let Some((c, l, n)) = handle {
                        drag = Some(DragState { kind: DragKind::Handle(n), clip: c, layer: l, start: p });
                    } else if let Some((c, l, n)) = corner {
                        drag = Some(DragState { kind: DragKind::Corner(n), clip: c, layer: l, start: p });
                    } else if tool == Tool::Type && !views.iter().any(|v| v.is_text() && v.hit(p)) {
                        drag = Some(DragState { kind: DragKind::NewTextBox, clip: ClipId(0), layer: 0, start: p });
                    } else if tool == Tool::Selection
                        && let Some(v) = views.iter().find(|v| v.hit(p))
                    {
                        if editing.is_some() {
                            end_edit(app, ui);
                        }
                        if !(Some(v.clip) == sel_clip && sel_layers.contains(&v.layer)) {
                            actions.push(("graphics.selectLayer".into(), json!({"clip": v.clip.0, "layers": [v.layer]})));
                        }
                        drag = Some(DragState { kind: DragKind::Move, clip: v.clip, layer: v.layer, start: p });
                    }
                }
                Tool::Rectangle | Tool::Ellipse => {
                    drag = Some(DragState { kind: DragKind::NewShape, clip: ClipId(0), layer: 0, start: p });
                }
                _ => {}
            }
        }
        ui.data_mut(|dd| {
            if let Some(d) = drag {
                dd.insert_temp(drag_id, d);
            } else {
                dd.remove::<DragState>(drag_id);
            }
        });
    }
    // ---- drag feedback / commit
    if let Some(d) = drag {
        let cur = resp.interact_pointer_pos().or(hover).unwrap_or(d.start);
        let v = views.iter().find(|v| v.clip == d.clip && v.layer == d.layer);
        // Snap in Program Monitor (hold ⌘/Ctrl to move freely)
        let snap_off = match (d.kind, v) {
            (DragKind::Move, Some(v)) if !ui.input(|i| i.modifiers.command) => {
                crate::panels::monitor_view::snap_move(app, pic, frame, Rect::from_points(&v.quad()), cur - d.start)
            }
            _ => (cur - d.start, Vec::new()),
        };
        match d.kind {
            DragKind::Move => {
                if let Some(v) = v {
                    let off = snap_off.0;
                    painter.add(egui::Shape::closed_line(v.quad().iter().map(|q| *q + off).collect(), Stroke::new(1.0, Color32::WHITE)));
                    crate::panels::monitor_view::draw_snap_lines(&painter, pic, &snap_off.1);
                }
            }
            DragKind::Handle(n) => {
                if let Some(v) = v {
                    let h = HandleDrag::new(v, n, d.start, cur, ui.input(|i| i.modifiers.shift));
                    painter.add(egui::Shape::closed_line(h.quad(v).to_vec(), Stroke::new(1.0, Color32::WHITE)));
                }
            }
            DragKind::Anchor => {
                if let Some(v) = v {
                    let a = anchor_screen(v) + (cur - d.start);
                    painter.circle_stroke(a, 4.0, Stroke::new(1.0, Color32::WHITE));
                    painter.line_segment([a - vec2(6.0, 0.0), a + vec2(6.0, 0.0)], Stroke::new(1.0, Color32::WHITE));
                    painter.line_segment([a - vec2(0.0, 6.0), a + vec2(0.0, 6.0)], Stroke::new(1.0, Color32::WHITE));
                }
            }
            DragKind::Corner(n) => {
                if let Some(v) = v
                    && let Some((r0, _)) = corner_handles(v)
                {
                    let r = corner_drag(v, r0, n, d.start, cur);
                    painter.add(egui::Shape::closed_line(round_rect_outline(v, r), Stroke::new(1.0, Color32::WHITE)));
                    for c in corner_points(v, r) {
                        painter.circle_stroke(c, 3.0, Stroke::new(1.0, Color32::WHITE));
                    }
                }
            }
            DragKind::NewTextBox => {
                painter.rect_stroke(Rect::from_two_pos(d.start, cur), 0.0, Stroke::new(1.0, t.danger), StrokeKind::Middle);
            }
            DragKind::NewShape => {
                let r = Rect::from_two_pos(d.start, cur);
                if tool == Tool::Ellipse {
                    painter.add(egui::Shape::ellipse_stroke(r.center(), r.size() / 2.0, Stroke::new(1.0, Color32::WHITE)));
                } else {
                    painter.rect_stroke(r, 0.0, Stroke::new(1.0, Color32::WHITE), StrokeKind::Middle);
                }
            }
            DragKind::TextSelect => {
                if let (Some(v), Some(mut ed)) = (v, app.ui.gfx_edit.clone())
                    && let Some(tp) = text_of(&v.spec)
                {
                    let l = text_layout(tp);
                    if let Some((x, y)) = v.to_local(cur) {
                        let previous_selection = (ed.caret, ed.anchor);
                        ed.caret = l.hit(x, y);
                        if previous_selection != (ed.caret, ed.anchor) {
                            app.invalidate_ime_composition();
                        }
                        app.ui.gfx_edit = Some(ed);
                    }
                }
            }
        }
        if resp.drag_stopped() {
            ui.data_mut(|dd| dd.remove::<DragState>(drag_id));
            // egui ends a drag on Escape: that stop abandons it (#580)
            let moved = (cur - d.start).length() > 2.0 && !ui.input(|i| i.key_pressed(egui::Key::Escape));
            match d.kind {
                DragKind::Move if moved => {
                    if let Some(v) = v {
                        let lin = Affine { e: 0.0, f: 0.0, ..v.canvas_to_screen };
                        let off = snap_off.0;
                        let delta = lin.inverse().map(|i| i.apply(Vec2::new(off.x as f64, off.y as f64))).unwrap_or_default();
                        let p0 = v.spec.transform.position;
                        actions.push((
                            "graphics.set".into(),
                            json!({"clip": v.clip.0, "layer": v.layer, "props": {"position": [p0.x + delta.x, p0.y + delta.y]}}),
                        ));
                    }
                }
                DragKind::Handle(n) if moved => {
                    if let Some(v) = v {
                        let props = HandleDrag::new(v, n, d.start, cur, ui.input(|i| i.modifiers.shift)).props(v);
                        actions.push(("graphics.set".into(), json!({"clip": v.clip.0, "layer": v.layer, "props": props})));
                    }
                }
                DragKind::Anchor if moved => {
                    if let Some(v) = v {
                        // the anchor moves over the layer; the position follows it, so the layer stays
                        let (canvas, local) = (unscale(&v.canvas_to_screen, cur - d.start), unscale(&v.to_screen, cur - d.start));
                        let (p0, a0) = (v.spec.transform.position, v.spec.transform.anchor);
                        actions.push((
                            "graphics.set".into(),
                            json!({"clip": v.clip.0, "layer": v.layer, "props": {"position": [p0.x + canvas.x, p0.y + canvas.y], "anchor": [a0.x + local.x, a0.y + local.y]}}),
                        ));
                    }
                }
                DragKind::Corner(n) if moved => {
                    if let Some(v) = v
                        && let Some((r0, _)) = corner_handles(v)
                    {
                        let r = corner_drag(v, r0, n, d.start, cur);
                        actions.push(("graphics.set".into(), json!({"clip": v.clip.0, "layer": v.layer, "props": {"corner_radius": r}})));
                    }
                }
                DragKind::NewTextBox if moved => {
                    end_edit(app, ui);
                    actions.push(new_text_action(app, pic, frame, d.start, Some(cur)));
                }
                DragKind::NewShape if moved => {
                    let (into, m) = canvas_target(app, pic, frame);
                    let (a, b) = (to_canvas(&m, d.start), to_canvas(&m, cur));
                    let c = Vec2::new((a.x + b.x) / 2.0, (a.y + b.y) / 2.0);
                    let mut p = json!({
                        "shape": if tool == Tool::Ellipse { "ellipse" } else { "rectangle" },
                        "position": [c.x, c.y],
                        "size": [(b.x - a.x).abs(), (b.y - a.y).abs()],
                    });
                    if let Some(cl) = into {
                        p["clip"] = json!(cl.0);
                    }
                    actions.push(("graphics.newShape".into(), p));
                }
                _ => {}
            }
        }
    }
    // ---- clicks
    if graphics_tool && resp.clicked() && drag.is_none_or(|d| !matches!(d.kind, DragKind::TextSelect)) {
        let p = resp.interact_pointer_pos().unwrap_or(pic.center());
        let in_edit = edit_view.is_some_and(|v| v.hit(p));
        match tool {
            // a click in the text being edited puts the caret there (Shift extends the selection),
            // as a press that becomes a drag already does (#2430)
            _ if in_edit => {
                if let (Some(v), Some(mut ed)) = (edit_view, editing.clone())
                    && let Some(tp) = text_of(&v.spec)
                    && let Some((x, y)) = v.to_local(p)
                {
                    let previous_selection = (ed.caret, ed.anchor);
                    ed.caret = text_layout(tp).hit(x, y);
                    if !ui.input(|i| i.modifiers.shift) {
                        ed.anchor = ed.caret;
                    }
                    if previous_selection != (ed.caret, ed.anchor) {
                        app.invalidate_ime_composition();
                    }
                    app.ui.gfx_edit = Some(ed);
                }
            }
            Tool::Type | Tool::VerticalType => {
                if let Some(v) = views.iter().find(|v| v.is_text() && v.hit(p)) {
                    start_edit_at(app, ui, v, p, &mut actions);
                } else {
                    end_edit(app, ui);
                    let (command, prm) = new_text_action(app, pic, frame, p, None);
                    // vertical text is always point text: a click, never a drawn box
                    let command = if tool == Tool::VerticalType { "graphics.newVerticalText#edit".into() } else { command };
                    actions.push((command, prm));
                }
            }
            Tool::Selection => {
                if resp.double_clicked() {
                    if let Some(v) = views.iter().find(|v| v.is_text() && v.hit(p)) {
                        start_edit_at(app, ui, v, p, &mut actions);
                        // double-click selects all the text so typing replaces it, as in Premiere
                        if let (Some(tp), Some(e)) = (text_of(&v.spec), app.ui.gfx_edit.as_mut()) {
                            (e.anchor, e.caret) = (0, tp.text.len());
                        }
                    }
                } else if let Some(v) = views.iter().find(|v| v.hit(p)) {
                    if app.ui.gfx_edit.is_some() {
                        end_edit(app, ui);
                    }
                    actions.push(("graphics.selectLayer".into(), json!({"clip": v.clip.0, "layers": [v.layer]})));
                } else {
                    deselect_on_monitor(app, ui);
                }
            }
            Tool::Pen => {
                let (_, m) = canvas_target(app, pic, frame);
                let c = to_canvas(&m, p);
                let close = app.ui.pen_points.len() >= 3
                    && app.ui.pen_points.first().is_some_and(|f| {
                        let fs = m.apply(Vec2::new(f[0], f[1]));
                        (pos2(fs.x as f32, fs.y as f32) - p).length() <= 8.0
                    });
                if close {
                    finish_pen(app, pic, frame, &mut actions);
                } else {
                    app.ui.pen_points.push([c.x, c.y]);
                }
            }
            _ => {}
        }
    }
    // ---- pen path in progress
    if tool == Tool::Pen && !app.ui.pen_points.is_empty() {
        let (_, m) = canvas_target(app, pic, frame);
        let pts: Vec<Pos2> = app.ui.pen_points.iter().map(|q| m.apply(Vec2::new(q[0], q[1]))).map(|v| pos2(v.x as f32, v.y as f32)).collect();
        painter.add(egui::Shape::line(pts.clone(), Stroke::new(1.0, accent)));
        if let (Some(last), Some(h)) = (pts.last(), hover) {
            painter.line_segment([*last, h], Stroke::new(1.0, accent.gamma_multiply(0.5)));
        }
        for (i, q) in pts.iter().enumerate() {
            painter.rect_filled(Rect::from_center_size(*q, vec2(6.0, 6.0)), 0.0, if i == 0 { accent } else { Color32::WHITE });
        }
        let (enter, esc) = ui.input(|i| (i.key_pressed(egui::Key::Enter), i.key_pressed(egui::Key::Escape)));
        if enter && app.ui.pen_points.len() >= 3 {
            finish_pen(app, pic, frame, &mut actions);
        } else if esc {
            app.ui.pen_points.clear();
        }
    } else if tool != Tool::Pen && !app.ui.pen_points.is_empty() {
        app.ui.pen_points.clear();
    }
    // ---- run accepted actions before drawing the caret
    for (cmd, p) in actions {
        let (cmd, then_edit) = match cmd.strip_suffix("#edit") {
            Some(c) => (c.to_string(), true),
            None => (cmd, false),
        };
        match app.session.execute(&cmd, p) {
            Ok(r) => {
                if then_edit && let (Some(c), Some(l)) = (r["clip"].as_u64(), r["layer"].as_u64()) {
                    app.restart_ime_target();
                    let uid = current_layer_uid(&app.session, c, l as usize).unwrap_or(0);
                    app.ui.gfx_edit = Some(GfxEdit { clip: c, layer: l as usize, caret: 0, anchor: 0, uid, fingerprint: 0 });
                    app.ui.focused = crate::dock::PanelKind::Program;
                }
            }
            Err(e) => app.ui.status = e.to_string(),
        }
    }
    // The model may have changed above; draw the post-action caret and committed text.
    let views = visible_layers(app, pic, frame);
    if let Some(ed) = app.ui.gfx_edit.clone()
        && let Some(v) = views.iter().find(|v| v.clip.0 == ed.clip && v.layer == ed.layer)
        && let Some(tp) = text_of(&v.spec)
    {
        let l = text_layout(tp);
        let caret = char_boundary_floor(&tp.text, ed.caret);
        let anchor = char_boundary_floor(&tp.text, ed.anchor);
        for r in l.selection_rects(anchor, caret) {
            let q = [(r[0], r[1]), (r[2], r[1]), (r[2], r[3]), (r[0], r[3])].map(|(x, y)| sp(&v.to_screen, x, y));
            painter.add(egui::Shape::convex_polygon(q.to_vec(), Color32::from_rgba_unmultiplied(80, 140, 255, 90), Stroke::NONE));
        }
        let (x, base, li) = l.caret(caret);
        let (ascent, descent) = l.lines.get(li.min(l.lines.len().saturating_sub(1))).map_or((0.0, 0.0), |line| (line.ascent, line.descent));
        let (a, b) = (sp(&v.to_screen, x, base - ascent), sp(&v.to_screen, x, base + descent));
        let cursor_rect = Rect::from_two_pos(a, b);
        let sequence = app.session.state.active_sequence.map_or(0, |sequence| sequence.0);
        let target = graphics_ime_target(sequence, v.clip.0, v.layer, v.spec.uid, app.ime_replacement_epoch);
        let live = composing_live(app, ui.ctx(), target);
        match app.ime_composer.preedit().filter(|_| live).map(|preedit| draw_preedit(&painter, a, b, preedit)) {
            Some(drawn) => {
                app.auto.add("program.textEdit.preedit", drawn.plate, &drawn.text);
                if let Some((rect, clause)) = drawn.clause {
                    app.auto.add("program.textEdit.preedit.clause", rect, &clause);
                }
            }
            None => {
                painter.line_segment([a, b], Stroke::new(1.5, Color32::WHITE));
                painter.line_segment([a + vec2(1.0, 0.0), b + vec2(1.0, 0.0)], Stroke::new(0.5, Color32::BLACK));
            }
        }
        let er = Rect::from_points(&v.quad()).union(cursor_rect);
        app.auto.add("program.textEdit", er, &tp.text);
        // `drain_ui_requests` later in this pass may run a command or replace `gfxEdit`:
        // `finish_graphics_ime` checks the post-drain state against this one.
        note_graphics_ime_editor(app, ui, v, cursor_rect);
    }
}

/// Remember the focused Program monitor editor drawn in this pass, for [`finish_graphics_ime`]
/// after the pass's control requests have run, and put the candidate window at its caret.
pub(crate) fn note_graphics_ime_editor(app: &mut FilmcraftApp, ui: &egui::Ui, v: &LayerView, cursor_rect: Rect) {
    let ctx = ui.ctx();
    app.ime_composer.set_caret(ctx, cursor_rect);
    let viewport = ctx.viewport_id();
    let pass_nr = ctx.cumulative_pass_nr_for(viewport);
    let sequence = app.session.state.active_sequence.map(|sequence| sequence.0);
    let target = graphics_ime_target(sequence.unwrap_or(0), v.clip.0, v.layer, v.spec.uid, app.ime_replacement_epoch);
    let fingerprint = current_layer_fingerprint(&app.session, v.clip.0, v.layer).unwrap_or(0);
    let layout_fingerprint = current_layer_layout_fingerprint(&app.session, v.clip.0, v.layer).unwrap_or(0);
    let text_stamp = text_of(&v.spec).map_or(0, |tp| ime_text_stamp(&tp.text));
    app.ime_drawn_editor = Some(DrawnImeEditor {
        viewport,
        pass_nr,
        document_epoch: app.session.document_epoch,
        sequence,
        target,
        clip: v.clip.0,
        layer: v.layer,
        fingerprint,
        text_stamp,
        layout_fingerprint,
    });
}

/// The end-of-pass check of the editor drawn in `ctx`'s viewport, after the pass's commands
/// ran. Returns whether the editor drawn there in this pass keeps the composer's descriptor.
/// Otherwise the composer turns the IME off: when the editor was drawn there, or, with `last`
/// (the main window's pass, which ends after every native window's), when it was drawn nowhere.
///
/// The composer's descriptor goes only with the editor's current target: a document, sequence,
/// target, text or layout replaced since the overlay drew turns it off for this pass instead of
/// pairing the old caret with the new model.
pub(crate) fn finish_graphics_ime(app: &mut FilmcraftApp, ctx: &egui::Context, last: bool) -> bool {
    let viewport = ctx.viewport_id();
    let drawn = match app.ime_drawn_editor {
        Some(drawn) if drawn.viewport != viewport => return false,
        None if !last => return false,
        drawn => drawn.filter(|drawn| drawn.pass_nr == ctx.cumulative_pass_nr_for(viewport)),
    };
    let eligible = drawn.is_some_and(|drawn| graphics_ime_eligible(app, ctx, &drawn));
    if !eligible {
        // The editor is gone or replaced: the marked text goes with it.
        let _ = app.ime_composer.update(ctx, false);
        return false;
    }
    app.note_ime_viewport(viewport);
    // In the main window the composer wrote its descriptor itself; a native window's goes out
    // after its last body.
    let owner = app.ime_composer_target.map(|target| target.owner);
    if let Some(output) = ctx.output(|o| o.ime.as_ref().filter(|ime| Some(ime.owner) == owner).map(crate::ime_reset::OutputContext::of)) {
        app.ime_reset.produced(output);
    }
    true
}

/// Whether the editor drawn in this pass is still the one being edited, unchanged since it drew.
fn graphics_ime_eligible(app: &mut FilmcraftApp, ctx: &egui::Context, drawn: &DrawnImeEditor) -> bool {
    // A picker or in-app modal opened after the overlay drew owns input for the rest of this
    // pass, so this editor cannot attach an already queued reset under the modal. Core may still
    // carry that un-taken request to a later stock descriptor (the accepted attachment
    // limitation); the next eligible pass rearms this guard explicitly.
    if app.ime_modal_blocks_input() {
        return false;
    }
    if drawn.document_epoch != app.session.document_epoch || drawn.sequence != app.session.state.active_sequence.map(|sequence| sequence.0) {
        return false;
    }
    if ctx.memory(|m| m.focused() != Some(egui::Id::unique(TEXT_EDIT_ID))) {
        return false;
    }
    let Some(ed) = app.ui.gfx_edit.clone() else { return false };
    if ed.clip != drawn.clip || ed.layer != drawn.layer {
        return false;
    }
    let (Some(sequence), Some(uid)) = (app.session.state.active_sequence, current_layer_uid(&app.session, ed.clip, ed.layer)) else {
        return false;
    };
    let target = graphics_ime_target(sequence.0, ed.clip, ed.layer, uid, app.ime_replacement_epoch);
    if target != drawn.target || current_layer_fingerprint(&app.session, ed.clip, ed.layer).is_none_or(|fingerprint| fingerprint != drawn.fingerprint) {
        // A control request replaced the edit target, reordered the layers, or changed the
        // document after the overlay drew.
        return false;
    }
    let Some(text) = current_layer_text(&app.session, ed.clip, ed.layer) else { return false };
    if ime_text_stamp(text) != drawn.text_stamp {
        // A same-pass control request changed the text after the overlay drew. The caret
        // belongs to the old text, and so does the composition: its next delete, preedit or
        // commit must not be inserted into the new text.
        app.invalidate_ime_composition();
        return false;
    }
    // A same-pass control request that changed a layout-affecting property (font, style,
    // alignment, spacing, box, transform, per-character style or pin) moved the caret: the next
    // pass draws and publishes the new one. A caret or selection move alone keeps the
    // descriptor, with a caret one pass old.
    current_layer_layout_fingerprint(&app.session, ed.clip, ed.layer).is_some_and(|fingerprint| fingerprint == drawn.layout_fingerprint)
}

fn layer_display_name_view(app: &FilmcraftApp, v: &LayerView) -> String {
    app.session
        .active_sequence()
        .and_then(|q| q.find_item(v.clip))
        .and_then(|(_, it)| layer_indices(&it.effects).get(v.layer).map(|&e| layer_display_name(&it.effects[e], v.layer)))
        .unwrap_or_default()
}

fn finish_pen(app: &mut FilmcraftApp, pic: Rect, frame: (u32, u32), actions: &mut Vec<(String, Value)>) {
    let pts = std::mem::take(&mut app.ui.pen_points);
    if pts.len() < 3 {
        return;
    }
    let n = pts.len() as f64;
    let (cx, cy) = (pts.iter().map(|p| p[0]).sum::<f64>() / n, pts.iter().map(|p| p[1]).sum::<f64>() / n);
    let (into, _) = canvas_target(app, pic, frame);
    let mut p = json!({
        "shape": "path",
        "position": [cx, cy],
        "points": pts.iter().map(|q| [q[0] - cx, q[1] - cy]).collect::<Vec<_>>(),
    });
    if let Some(c) = into {
        p["clip"] = json!(c.0);
    }
    actions.push(("graphics.newShape".into(), p));
}

/// Start editing a text layer at screen point `p` (caret at the text hit, else the end), restart the IME target, record clip/layer/uid and select the layer.
fn start_edit_at(app: &mut FilmcraftApp, ui: &egui::Ui, v: &LayerView, p: Pos2, actions: &mut Vec<(String, Value)>) {
    let Some(tp) = text_of(&v.spec) else { return };
    let l = text_layout(tp);
    let caret = v.to_local(p).map_or(tp.text.len(), |(x, y)| l.hit(x, y));
    // the marked text still being composed goes into the layer it was typed into
    commit_marked_text(app, ui.ctx());
    app.restart_ime_target();
    app.ui.gfx_edit = Some(GfxEdit { clip: v.clip.0, layer: v.layer, caret, anchor: caret, uid: v.spec.uid, fingerprint: 0 });
    app.ui.focused = crate::dock::PanelKind::Program;
    actions.push(("graphics.selectLayer".into(), json!({"clip": v.clip.0, "layers": [v.layer]})));
}

/// Edit ▸ Select All while text is edited on the monitor selects all of that text, not the
/// timeline clips: on macOS the native menu takes Cmd+A before the text sees the key (#1102).
/// False when no text is being edited.
pub(crate) fn select_all_edited_text(app: &mut FilmcraftApp) -> bool {
    let Some((clip, layer)) = app.ui.gfx_edit.as_ref().map(|e| (e.clip, e.layer)) else { return false };
    let t = app.session.playhead();
    let Some((_, it)) = app.session.active_sequence().and_then(|q| q.find_item(ClipId(clip))) else { return false };
    let Some(ItemKind::Graphic { width, height, .. }) = app.session.project.item(it.item).map(|p| &p.kind) else { return false };
    let Some(effect) = layer_indices(&it.effects).get(layer).and_then(|&i| it.effects.get(i)) else { return false };
    let Some(len) = eval_layer(effect, it.source_time_at(t), (*width, *height)).as_ref().and_then(text_of).map(|tp| tp.text.len()) else { return false };
    let Some(ed) = app.ui.gfx_edit.as_mut() else { return false };
    let moved = (ed.anchor, ed.caret) != (0, len);
    (ed.anchor, ed.caret) = (0, len);
    if moved {
        app.invalidate_ime_composition();
    }
    true
}

/// The effect instance of layer `layer` of `clip` when it is enabled and on screen.
///
/// This is the read-only path for the IME producer: it never evaluates a `LayerSpec` (which
/// clones the layer's full text) just to read an identity or a committed-text window.
fn current_layer_effect(session: &Session, clip: u64, layer: usize) -> Option<&EffectInstance> {
    let seq = session.active_sequence()?;
    let (_, it) = seq.find_item(ClipId(clip))?;
    if !it.enabled {
        return None;
    }
    let &effect_index = layer_indices(&it.effects).get(layer)?;
    let effect = it.effects.get(effect_index)?;
    effect.enabled.then_some(effect)
}

/// Borrowed committed text of the edited layer at the playhead (no `TextProps`/full-text copy).
fn current_layer_text(session: &Session, clip: u64, layer: usize) -> Option<&str> {
    let seq = session.active_sequence()?;
    let t = session.playhead();
    let (_, it) = seq.find_item(ClipId(clip))?;
    if !it.enabled {
        return None;
    }
    let &effect_index = layer_indices(&it.effects).get(layer)?;
    let effect = it.effects.get(effect_index)?;
    if !effect.enabled || effect.effect != graphic::TEXT_LAYER {
        return None;
    }
    effect.params.get("text")?.text_ref_at(it.source_time_at(t))
}

/// Stable uid of the enabled layer at index `layer`, or 0 when that layer has no uid; `None`
/// when the clip, item or layer is missing or disabled.
fn current_layer_uid(session: &Session, clip: u64, layer: usize) -> Option<u64> {
    current_layer_effect(session, clip, layer).map(|effect| effect.layer.as_ref().map_or(0, |x| x.uid))
}

/// A compact identity for a layer that survives reorder/delete/undo when the layer has no uid.
///
/// It uses static parameter values only, so a keyframed or animating position at the playhead
/// does not make the target look new, and it deliberately excludes the editable text, so typing
/// does not either.
fn layer_fingerprint(effect: &EffectInstance) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    let mut hash = |bytes: &[u8]| {
        for &b in bytes {
            h ^= u64::from(b);
            h = h.wrapping_mul(0x0000_0100_0000_01b3);
        }
    };
    hash(effect.effect.as_bytes());
    hash(&[u8::from(effect.enabled)]);
    hash(&effect.layer.as_ref().map_or(0, |x| x.uid).to_le_bytes());
    if let Some(name) = effect.params.get("name").and_then(|p| match &p.value {
        ParamValue::Text(s) => Some(s.as_str()),
        _ => None,
    }) {
        hash(name.as_bytes());
    }
    for id in ["position", "size", "box_width", "box_height", "shape", "sides", "corner_radius", "vertical"] {
        let Some(value) = effect.params.get(id).map(|p| &p.value) else { continue };
        match value {
            ParamValue::Float(v) => hash(&v.to_bits().to_le_bytes()),
            ParamValue::Vec2(v) => {
                hash(&v.x.to_bits().to_le_bytes());
                hash(&v.y.to_bits().to_le_bytes());
            }
            ParamValue::Choice(v) => hash(&u64::from(*v).to_le_bytes()),
            ParamValue::Bool(v) => hash(&[u8::from(*v)]),
            ParamValue::Text(s) => hash(s.as_bytes()),
            _ => {}
        }
    }
    h
}

/// Static-identity fingerprint of the enabled layer at `layer`, or `None` when it is not
/// present. Editable text is excluded by [`layer_fingerprint`], so typing does not change it.
fn current_layer_fingerprint(session: &Session, clip: u64, layer: usize) -> Option<u64> {
    current_layer_effect(session, clip, layer).map(layer_fingerprint)
}

/// Stable uid and identity fingerprint of the layer currently at `layer`.
///
/// A control request that explicitly names a clip and numeric layer means that index now, not
/// whichever layer an older edit session happened to own at that index.
pub(crate) fn current_layer_identity(session: &Session, clip: u64, layer: usize) -> Option<(u64, u64)> {
    current_layer_effect(session, clip, layer).map(|effect| (effect.layer.as_ref().map_or(0, |extra| extra.uid), layer_fingerprint(effect)))
}

/// Properties that change a text or shape layer's layout, and therefore the caret and candidate
/// rectangle, without necessarily changing its committed text or stable identity. Both static
/// values and keyframes at the playhead are included.
const LAYOUT_PARAMS: &[&str] = &[
    // Text layout.
    "font",
    "font_style",
    "size",
    "align",
    "tracking",
    "kerning",
    "ligatures",
    "leading",
    "baseline_shift",
    "faux_bold",
    "faux_italic",
    "caps",
    "underline",
    "box_width",
    "box_height",
    "vertical",
    // Shape geometry.
    "shape",
    "sides",
    "corner_radius",
    "points",
    // Transform.
    "position",
    "anchor",
    "scale",
    "scale_width",
    "uniform_scale",
    "rotation",
];

/// Fold `bytes` into the FNV-1a-style hash accumulator `h`, allowing callers to chain fields.
fn hash_bytes(h: &mut u64, bytes: &[u8]) {
    for &b in bytes {
        *h ^= u64::from(b);
        *h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
}

/// Hash the bits of a finite `f64`; all non-finite values share one representation so NaN
/// payloads or evaluation details cannot change a layout identity.
fn hash_f64(h: &mut u64, v: f64) {
    // Normalise non-finite values: NaN is "auto" for point parameters and must not make the
    // fingerprint depend on payload bits or an evaluation detail.
    if v.is_finite() {
        hash_bytes(h, &v.to_bits().to_le_bytes());
    } else {
        hash_bytes(h, b"non-finite");
    }
}

/// Hash an `f32` widened to `f64`, keeping the non-finite normalization of [`hash_f64`].
fn hash_f32(h: &mut u64, v: f32) {
    hash_f64(h, f64::from(v));
}

/// Hash an optional value with an explicit presence byte, followed by the inner value only
/// when it is present. This keeps `None` distinct from any hashed `Some` payload.
fn hash_option<T>(h: &mut u64, value: Option<T>, hash: impl FnOnce(&mut u64, T)) {
    match value {
        Some(v) => {
            hash_bytes(h, &[1]);
            hash(h, v);
        }
        None => hash_bytes(h, &[0]),
    }
}

/// Hash a layout parameter by kind and value. Non-finite floats normalize through
/// [`hash_f64`]; path payloads contribute only a tag because layout parameters do not use
/// mask paths.
fn hash_param_value(h: &mut u64, value: &ParamValue) {
    match value {
        ParamValue::Float(v) => {
            hash_bytes(h, b"f");
            hash_f64(h, *v);
        }
        ParamValue::Vec2(v) => {
            hash_bytes(h, b"v");
            hash_f64(h, v.x);
            hash_f64(h, v.y);
        }
        ParamValue::Color(c) => {
            hash_bytes(h, b"c");
            for v in c {
                hash_f32(h, *v);
            }
        }
        ParamValue::Bool(v) => {
            hash_bytes(h, b"b");
            hash_bytes(h, &[u8::from(*v)]);
        }
        ParamValue::Choice(v) => {
            hash_bytes(h, b"o");
            hash_bytes(h, &u64::from(*v).to_le_bytes());
        }
        ParamValue::Text(v) => {
            hash_bytes(h, b"t");
            hash_bytes(h, v.as_bytes());
        }
        ParamValue::Curve(points) => {
            hash_bytes(h, b"u");
            hash_bytes(h, &(points.len() as u64).to_le_bytes());
            for p in points {
                hash_f32(h, p[0]);
                hash_f32(h, p[1]);
            }
        }
        // Mask paths are not used by layer layout parameters.
        ParamValue::Path(_) => hash_bytes(h, b"p"),
    }
}

/// Fingerprint every layout-affecting value of `effect` at tick `t`: the layout parameters
/// evaluated at the playhead, character runs and pin data. Non-finite floats normalize, and
/// the committed text bytes are not hashed directly, so caret/selection changes do not churn
/// the fingerprint; a value or run change that can move the caret does.
fn layout_fingerprint(effect: &EffectInstance, t: Tick) -> u64 {
    let mut h: u64 = 0x9e37_79b9_7f4a_7c15;
    hash_bytes(&mut h, effect.effect.as_bytes());
    hash_bytes(&mut h, &[u8::from(effect.enabled)]);
    for id in LAYOUT_PARAMS {
        let Some(param) = effect.params.get(*id) else { continue };
        hash_bytes(&mut h, id.as_bytes());
        hash_param_value(&mut h, &param.value_at(t));
    }
    if let Some(extra) = &effect.layer {
        hash_bytes(&mut h, b"runs");
        hash_bytes(&mut h, &(extra.runs.len() as u64).to_le_bytes());
        for run in &extra.runs {
            hash_bytes(&mut h, &(run.start as u64).to_le_bytes());
            hash_bytes(&mut h, &(run.end as u64).to_le_bytes());
            hash_option(&mut h, run.style.font.as_ref(), |h, v| hash_bytes(h, v.as_bytes()));
            hash_option(&mut h, run.style.font_style.as_ref(), |h, v| hash_bytes(h, v.as_bytes()));
            hash_option(&mut h, run.style.size, hash_f32);
            hash_option(&mut h, run.style.fill, |h, v| {
                for c in v {
                    hash_f32(h, c);
                }
            });
            hash_option(&mut h, run.style.faux_bold, |h, v| hash_bytes(h, &[u8::from(v)]));
            hash_option(&mut h, run.style.faux_italic, |h, v| hash_bytes(h, &[u8::from(v)]));
            hash_option(&mut h, run.style.underline, |h, v| hash_bytes(h, &[u8::from(v)]));
            hash_option(&mut h, run.style.tracking, hash_f32);
            hash_option(&mut h, run.style.baseline_shift, hash_f32);
            hash_option(&mut h, run.style.caps, |h, v| hash_bytes(h, &u64::from(v).to_le_bytes()));
        }
        hash_bytes(&mut h, b"pin");
        match &extra.pin {
            Some(pin) => {
                hash_bytes(&mut h, &[1]);
                match pin.to {
                    filmcraft_project::graphic_design::PinTarget::Frame => hash_bytes(&mut h, b"frame"),
                    filmcraft_project::graphic_design::PinTarget::Layer(uid) => {
                        hash_bytes(&mut h, b"layer");
                        hash_bytes(&mut h, &uid.to_le_bytes());
                    }
                }
                hash_bytes(&mut h, &[u8::from(pin.left), u8::from(pin.top), u8::from(pin.right), u8::from(pin.bottom)]);
                for offset in pin.offsets {
                    hash_f64(&mut h, offset);
                }
            }
            None => hash_bytes(&mut h, &[0]),
        }
    }
    h
}

/// Layout fingerprint of the layer at `layer` evaluated at the playhead's source time;
/// `None` when the sequence, clip or layer is missing.
fn current_layer_layout_fingerprint(session: &Session, clip: u64, layer: usize) -> Option<u64> {
    let sequence = session.active_sequence()?;
    let t = session.playhead();
    let (_, item) = sequence.find_item(ClipId(clip))?;
    let &effect_index = layer_indices(&item.effects).get(layer)?;
    let effect = item.effects.get(effect_index)?;
    Some(layout_fingerprint(effect, item.source_time_at(t)))
}

/// Follow the edited layer through a real project edit (reorder, delete, undo/redo).
///
/// A stable uid wins. For a uid-less layer the compact fingerprint finds the same layer at its
/// new index; with no candidate the layer was deleted (or replaced), so the edit is ended rather
/// than silently retargeted to whatever now occupies the old index.
fn resolve_gfx_edit(session: &Session, ed: &GfxEdit) -> Option<(usize, u64, u64)> {
    let seq = session.active_sequence()?;
    let (_, it) = seq.find_item(ClipId(ed.clip))?;
    let indices = layer_indices(&it.effects);
    let resolved = if ed.uid != 0 {
        indices.iter().position(|&ei| it.effects.get(ei).and_then(|e| e.layer.as_ref()).is_some_and(|x| x.uid == ed.uid))
    } else {
        let at = |i: usize| it.effects.get(indices[i]).map(layer_fingerprint);
        let mut found = indices.get(ed.layer).and_then(|_| at(ed.layer)).filter(|fp| ed.fingerprint == 0 || *fp == ed.fingerprint).map(|_| ed.layer);
        if found.is_none() && ed.fingerprint != 0 {
            let mut best: Option<(usize, usize)> = None;
            for i in 0..indices.len() {
                if at(i) == Some(ed.fingerprint) {
                    let distance = i.abs_diff(ed.layer);
                    if best.is_none_or(|(d, _)| distance < d) {
                        best = Some((distance, i));
                    }
                }
            }
            found = best.map(|(_, i)| i);
        }
        found
    }?;
    let effect = it.effects.get(*indices.get(resolved)?)?;
    let uid = effect.layer.as_ref().map_or(0, |x| x.uid);
    Some((resolved, uid, layer_fingerprint(effect)))
}

/// Keep `gfx_edit` on the layer it started on. Called before the overlay draws, so a command run
/// between two passes (arrange/delete/undo or a control edit) cannot shift the caret into another
/// layer.
fn reconcile_gfx_edit(app: &mut FilmcraftApp) {
    let Some(ed) = app.ui.gfx_edit.clone() else { return };
    match resolve_gfx_edit(&app.session, &ed) {
        Some((layer, uid, fingerprint)) => {
            // A uid-less owner key includes the index, so an index change is a new context. A
            // stable uid layer keeps the same owner and its composition may continue.
            if layer != ed.layer && ed.uid == 0 {
                app.invalidate_ime_composition();
            }
            app.ui.gfx_edit = Some(GfxEdit { layer, uid, fingerprint, ..ed });
        }
        None => {
            app.ui.gfx_edit = None;
            app.invalidate_ime_composition();
        }
    }
}

/// Owner for the graphics text editor: sequence, clip and stable layer uid, falling back to
/// the numeric layer index only when `uid` is 0. The replacement epoch is explicit. Caret,
/// selection, text, revision, geometry, visual order and pointers are excluded so those
/// changes do not churn the IME owner.
fn graphics_ime_target(sequence: u64, clip: u64, layer: usize, uid: u64, replacement_epoch: u64) -> ImeTarget {
    // The owner excludes caret, selection, text, revision, geometry, visual order and pointers.
    // `uid` is the stable layer component id; zero means "none", where the layer index is the
    // only component identity available.
    let component_uid = (uid != 0).then_some(uid);
    let component_index = (uid == 0).then_some(layer as u64);
    ImeTarget::new(egui::Id::unique(("filmcraft.graphics.ime", sequence, clip, component_uid, component_index)), replacement_epoch)
}

/// Current editor owner for the layer, using its live uid and the app's replacement epoch;
/// `None` when there is no active sequence or the layer is unavailable.
fn current_graphics_ime_target(app: &FilmcraftApp, clip: u64, layer: usize) -> Option<ImeTarget> {
    let sequence = app.session.state.active_sequence?.0;
    let uid = current_layer_uid(&app.session, clip, layer)?;
    Some(graphics_ime_target(sequence, clip, layer, uid, app.ime_replacement_epoch))
}

/// Outside the keyboard reducer the actual engine/UI edit state is authoritative.
pub(crate) fn current_reset_destination(app: &FilmcraftApp) -> Option<crate::ime_reset::Destination> {
    let ed = app.ui.gfx_edit.as_ref()?;
    let target = current_graphics_ime_target(app, ed.clip, ed.layer)?;
    let text = current_layer_text(&app.session, ed.clip, ed.layer)?;
    Some(local_reset_destination(app, ed, text, target))
}

/// Build the admission destination from the local edit and current document epoch, stamping
/// the committed `text` while carrying the caret and anchor separately.
fn local_reset_destination(app: &FilmcraftApp, ed: &GfxEdit, text: &str, target: ImeTarget) -> crate::ime_reset::Destination {
    crate::ime_reset::Destination { document: app.session.document_epoch, target, caret: ed.caret, anchor: ed.anchor, text: ime_text_stamp(text) }
}

/// Is this IME commit/delete left over from a composition whose target or caret/focus was
/// interrupted? A terminal-pending composition still owns its pending commit/delete after an empty
/// preedit. `None` (no preedit was seen) still accepts a direct commit.
fn ime_event_is_stale(app: &FilmcraftApp, target: ImeTarget) -> bool {
    match app.ime_composition {
        Some(crate::ImeComposition::Stale) => true,
        Some(crate::ImeComposition::Active(active) | crate::ImeComposition::TerminalPending(active)) => active != target,
        None => false,
    }
}

/// Whether an event was fully handled or must skip the per-event IME destination update, which
/// mirrors the original loop's `continue` paths.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum EventFlow {
    Applied,
    Skip,
}

/// Mutable state for one batch of text-editor events: the committed text and caret/selection.
struct TextEditState {
    ed: GfxEdit,
    text: String,
    changed: bool,
    stop: bool,
    custom_mutations: bool,
}

impl TextEditState {
    /// The caret/anchor pair sorted into a forward `(start, end)` selection range.
    fn selection(&self) -> (usize, usize) {
        (self.ed.caret.min(self.ed.anchor), self.ed.caret.max(self.ed.anchor))
    }

    /// Delete the sorted selection in place, collapse caret and anchor to its start and report
    /// whether anything was removed. This does not set the batch's changed flags; callers do.
    fn delete_selection(&mut self) -> bool {
        let (a, b) = self.selection();
        if a == b {
            return false;
        }
        self.text.replace_range(a..b, "");
        self.ed.caret = a;
        self.ed.anchor = a;
        true
    }
}

/// The per-event prologue: sync the live destination, consume IME-reset completions, and reject
/// events while a reset blocks IME input. `None` is the original loop's `continue`.
fn begin_text_event(app: &mut FilmcraftApp, ui: &egui::Ui, state: &TextEditState, target: ImeTarget, event: &egui::Event) -> Option<(bool, bool)> {
    let destination = local_reset_destination(app, &state.ed, &state.text, target);
    if state.custom_mutations {
        app.sync_live_ime_destination(ui.ctx(), destination);
    }
    if let egui::Event::ImeResetCompleted { request_id } = event {
        if state.custom_mutations && app.ime_reset.complete(ui.ctx(), *request_id, destination) && app.ime_composition_viewport == Some(ui.ctx().viewport_id())
        {
            app.ime_composition = None;
            app.ime_composition_viewport = None;
        }
        return None;
    }
    if !state.custom_mutations || (app.ime_reset.blocked(ui.ctx().viewport_id()) && matches!(event, egui::Event::Ime(_))) {
        return None;
    }
    let from_ime = matches!(event, egui::Event::Ime(_));
    let terminal = matches!(event, egui::Event::Ime(egui::ImeEvent::Commit(_)));
    Some((from_ime, terminal))
}

/// The per-step epilogue: remember or terminal-acknowledge an IME step's destination, or keep
/// the live destination in sync for ordinary application edits.
fn finish_text_event(app: &mut FilmcraftApp, ui: &egui::Ui, state: &TextEditState, target: ImeTarget, from_ime: bool, terminal: bool) {
    let current = local_reset_destination(app, &state.ed, &state.text, target);
    if from_ime {
        if terminal {
            app.ime_reset.terminal(ui.ctx().viewport_id());
        } else {
            app.ime_reset.remember(ui.ctx().viewport_id(), current);
        }
    } else if state.custom_mutations {
        app.sync_live_ime_destination(ui.ctx(), current);
    }
}

/// Track this viewport's IME preedit target. Empty preedit becomes `TerminalPending` so a
/// following empty commit still belongs to the same composition; non-empty becomes `Active`.
fn apply_preedit(app: &mut FilmcraftApp, viewport: egui::ViewportId, target: ImeTarget, text: &str) {
    app.ime_composition_viewport = Some(viewport);
    if text.is_empty() {
        app.ime_composition = Some(crate::ImeComposition::TerminalPending(target));
    } else {
        app.ime_composition = Some(crate::ImeComposition::Active(target));
    }
}

/// Apply a UTF-8 byte-count surrounding deletion after the staleness check. Only whole
/// characters fully inside the reported byte ranges are removed, the selection is preserved
/// and a stale event skips.
fn apply_delete_surrounding_bytes(app: &FilmcraftApp, state: &mut TextEditState, target: ImeTarget, before_bytes: usize, after_bytes: usize) -> EventFlow {
    if ime_event_is_stale(app, target) {
        return EventFlow::Skip;
    }
    let (a, b) = state.selection();
    let (before, after) = surrounding_byte_deletion_ranges(&state.text, a, b, before_bytes, after_bytes);
    if !before.is_empty() || !after.is_empty() {
        apply_surrounding_deletion(&mut state.text, &mut state.ed, before, after);
        state.changed = true;
    }
    EventFlow::Applied
}

/// Apply a character-count surrounding deletion after the staleness check. The selection is
/// preserved, deletion stops at either text end, and a stale event skips.
fn apply_delete_surrounding_chars(app: &FilmcraftApp, state: &mut TextEditState, target: ImeTarget, before_chars: usize, after_chars: usize) -> EventFlow {
    if ime_event_is_stale(app, target) {
        return EventFlow::Skip;
    }
    let (a, b) = state.selection();
    let before = prev_n_chars(&state.text, a, before_chars)..a;
    let after = b..next_n_chars(&state.text, b, after_chars);
    if !before.is_empty() || !after.is_empty() {
        apply_surrounding_deletion(&mut state.text, &mut state.ed, before, after);
        state.changed = true;
    }
    EventFlow::Applied
}

/// Insert application-origin typing or paste: strip control characters except newline,
/// replace the selection and move the caret to the end. Input that sanitizes to nothing is
/// skipped.
fn apply_text_input(state: &mut TextEditState, input: &str) -> EventFlow {
    let s: String = input.chars().filter(|c| !c.is_control() || *c == '\n').collect();
    if s.is_empty() {
        return EventFlow::Skip;
    }
    state.delete_selection();
    let at = state.ed.caret;
    state.text.insert_str(at, &s);
    state.ed.caret += s.len();
    state.ed.anchor = state.ed.caret;
    state.changed = true;
    EventFlow::Applied
}

/// Apply an IME commit after the staleness check: clear the composition, insert sanitized
/// committed text, or delete the selection when the commit is empty; a stale commit skips.
fn apply_ime_commit(app: &mut FilmcraftApp, state: &mut TextEditState, target: ImeTarget, input: &str) -> EventFlow {
    if ime_event_is_stale(app, target) {
        return EventFlow::Skip;
    }
    app.ime_composition = None;
    app.ime_composition_viewport = None;
    let s: String = input.chars().filter(|c| !c.is_control() || *c == '\n').collect();
    if s.is_empty() {
        // A commit replaces the selection even when the input method produced no text
        // (for example DeleteSurroundingBytes followed by an empty commit). An empty
        // commit with no selection stays a no-op.
        if state.delete_selection() {
            state.changed = true;
        }
    } else {
        state.delete_selection();
        let at = state.ed.caret;
        state.text.insert_str(at, &s);
        state.ed.caret += s.len();
        state.ed.anchor = state.ed.caret;
        state.changed = true;
    }
    EventFlow::Applied
}

/// Copy the sorted selection, and for Cut delete it. An empty selection is a no-op.
fn apply_copy_cut(ui: &egui::Ui, state: &mut TextEditState, cut: bool) -> EventFlow {
    let (a, b) = state.selection();
    if a < b {
        ui.ctx().copy_text(state.text[a..b].to_string());
        if cut && state.delete_selection() {
            state.changed = true;
        }
    }
    EventFlow::Applied
}

/// Caret destination for Left: with no Shift collapse a selection to its start, otherwise
/// move to the line start (Command), previous word (Alt) or previous character boundary.
fn left_target(state: &TextEditState, layout: &filmcraft_text::Layout, shift: bool, command: bool, alt: bool) -> usize {
    let (a, _) = state.selection();
    if !shift && state.ed.caret != state.ed.anchor {
        a
    } else if command {
        layout.lines[layout.caret(state.ed.caret).2].range.start
    } else if alt {
        word_left(&state.text, state.ed.caret)
    } else {
        prev_boundary(&state.text, state.ed.caret)
    }
}

/// Caret destination for Right: with no Shift collapse a selection to its end, otherwise
/// move to the line end (Command), next word (Alt) or next character boundary.
fn right_target(state: &TextEditState, layout: &filmcraft_text::Layout, shift: bool, command: bool, alt: bool) -> usize {
    let (_, b) = state.selection();
    if !shift && state.ed.caret != state.ed.anchor {
        b
    } else if command {
        layout.lines[layout.caret(state.ed.caret).2].range.end
    } else if alt {
        word_right(&state.text, state.ed.caret)
    } else {
        next_boundary(&state.text, state.ed.caret)
    }
}

/// Caret destination for Up/Down: hit-test the same x on the adjacent line's baseline,
/// clamping to the text start/end when there is no line in that direction.
fn vertical_target(state: &TextEditState, layout: &filmcraft_text::Layout, up: bool) -> usize {
    let (x, _, line) = layout.caret(state.ed.caret);
    let target = if up { line.checked_sub(1) } else { Some(line + 1).filter(|next| *next < layout.lines.len()) };
    match target {
        Some(next) => layout.hit(x, layout.lines[next].baseline),
        None if up => 0,
        None => state.text.len(),
    }
}

/// Apply a layout-derived caret destination after re-clamping it to a UTF-8 boundary in the
/// current (possibly already edited) text. Without Shift the anchor collapses to the caret;
/// any actual selection change invalidates the IME composition.
fn apply_moved_caret(app: &mut FilmcraftApp, state: &mut TextEditState, moved: Option<usize>, shift: bool) {
    if let Some(m) = moved {
        let previous_selection = (state.ed.caret, state.ed.anchor);
        // `m` comes from the layout of the text at the start of this batch, but an
        // earlier Text/Paste/IME event may already have changed `text`; clamp to a
        // UTF-8 boundary in the current text before a later event slices or inserts.
        state.ed.caret = char_boundary_floor(&state.text, m);
        if !shift {
            state.ed.anchor = state.ed.caret;
        }
        if previous_selection != (state.ed.caret, state.ed.anchor) {
            app.invalidate_ime_composition();
        }
    }
}

/// Apply one non-IME key press to the local edit: Enter/Backspace/Delete mutate text,
/// Command+A selects all and arrows/Home/End move the caret. Text edits mark an application
/// mutation; Escape stops editing and suspends IME input. Only Escape returns `Skip`.
fn apply_key_event(
    app: &mut FilmcraftApp,
    ui: &egui::Ui,
    state: &mut TextEditState,
    layout: &filmcraft_text::Layout,
    key: egui::Key,
    modifiers: egui::Modifiers,
) -> EventFlow {
    let shift = modifiers.shift;
    let mut moved = None;
    match key {
        egui::Key::Escape => {
            // Escape cancels the edit now: a commit later in the same raw batch must
            // not still be inserted after the user cancelled.
            state.stop = true;
            state.custom_mutations = false;
            app.suspend_ime_input(ui.ctx());
            return EventFlow::Skip;
        }
        egui::Key::Enter => {
            state.delete_selection();
            let at = state.ed.caret;
            state.text.insert(at, '\n');
            state.ed.caret += 1;
            state.ed.anchor = state.ed.caret;
            state.changed = true;
        }
        egui::Key::Backspace => {
            let mut did = state.delete_selection();
            if !did && state.ed.caret > 0 {
                let p = if modifiers.alt || modifiers.command { word_left(&state.text, state.ed.caret) } else { prev_boundary(&state.text, state.ed.caret) };
                let end = state.ed.caret;
                state.text.replace_range(p..end, "");
                state.ed.caret = p;
                state.ed.anchor = p;
                did = true;
            }
            if did {
                state.changed = true;
            }
        }
        egui::Key::Delete => {
            let mut did = state.delete_selection();
            if !did && state.ed.caret < state.text.len() {
                let n = next_boundary(&state.text, state.ed.caret);
                let at = state.ed.caret;
                state.text.replace_range(at..n, "");
                did = true;
            }
            if did {
                state.changed = true;
            }
        }
        egui::Key::A if modifiers.command => {
            let moved = (state.ed.anchor, state.ed.caret) != (0, state.text.len());
            state.ed.anchor = 0;
            state.ed.caret = state.text.len();
            if moved {
                app.invalidate_ime_composition();
            }
        }
        egui::Key::ArrowLeft => {
            moved = Some(left_target(state, layout, shift, modifiers.command, modifiers.alt));
        }
        egui::Key::ArrowRight => {
            moved = Some(right_target(state, layout, shift, modifiers.command, modifiers.alt));
        }
        egui::Key::ArrowUp | egui::Key::ArrowDown => {
            moved = Some(vertical_target(state, layout, key == egui::Key::ArrowUp));
        }
        egui::Key::Home => moved = Some(layout.lines[layout.caret(state.ed.caret).2].range.start),
        egui::Key::End => moved = Some(layout.lines[layout.caret(state.ed.caret).2].range.end),
        _ => {}
    }
    apply_moved_caret(app, state, moved, shift);
    EventFlow::Applied
}

/// One step for the editor, in the order the platform sent its events: an edit from
/// [`craft_ui::ime::Composer`], or an empty input-method commit with nothing composing, which
/// the composer drops but which replaces the selection here.
#[derive(Clone, Debug, PartialEq, Eq)]
enum TextStep {
    Edit(craft_ui::ime::Edit),
    EmptyCommit,
}

/// Run the admitted events through the composer. Returns the steps and whether the composer
/// ended a composition the platform input method may still hold.
fn compose(composer: &mut craft_ui::ime::Composer, events: &[egui::Event], time: f64) -> (Vec<TextStep>, bool) {
    let mut steps = Vec::new();
    let mut interrupted = false;
    let mut start = 0;
    for (i, event) in events.iter().enumerate() {
        if !matches!(event, egui::Event::Ime(egui::ImeEvent::Commit(text)) if text.is_empty()) {
            continue;
        }
        steps.extend(composer.process(events.get(start..i).unwrap_or_default(), time).edits.into_iter().map(TextStep::Edit));
        interrupted |= composer.interrupted();
        if composer.is_composing() {
            start = i;
        } else {
            let _ = composer.process(std::slice::from_ref(event), time);
            steps.push(TextStep::EmptyCommit);
            start = i.saturating_add(1);
        }
    }
    steps.extend(composer.process(events.get(start..).unwrap_or_default(), time).edits.into_iter().map(TextStep::Edit));
    (steps, interrupted || composer.interrupted())
}

/// Whether a step is the input method's (`from_ime`) and ends its composition (`terminal`). An
/// insert is the input method's when it is one of the batch's committed texts, which it takes.
fn step_origin(step: &TextStep, commits: &mut Vec<String>) -> (bool, bool) {
    use craft_ui::ime::Edit;
    match step {
        TextStep::EmptyCommit => (true, true),
        TextStep::Edit(Edit::Insert(text)) => match commits.iter().position(|commit| commit == text) {
            Some(i) => {
                commits.remove(i);
                (true, true)
            }
            None => (false, false),
        },
        TextStep::Edit(Edit::Preedit { .. } | Edit::DeleteSurrounding { .. } | Edit::DeleteSurroundingBytes { .. }) => (true, false),
        TextStep::Edit(_) => (false, false),
    }
}

/// Apply one step. `Skip` is a stale or empty input-method or text step, which leaves the IME
/// destination as it was.
fn apply_text_step(
    app: &mut FilmcraftApp,
    ui: &egui::Ui,
    state: &mut TextEditState,
    target: ImeTarget,
    layout: &filmcraft_text::Layout,
    step: TextStep,
    from_ime: bool,
) -> EventFlow {
    use craft_ui::ime::Edit;
    let viewport = ui.ctx().viewport_id();
    let edit = match step {
        TextStep::EmptyCommit => return apply_ime_commit(app, state, target, ""),
        TextStep::Edit(edit) => edit,
    };
    match edit {
        Edit::Preedit { text, .. } => {
            apply_preedit(app, viewport, target, &text);
            EventFlow::Applied
        }
        // text-input-v3/Wayland delete-surrounding arrives in UTF-8 bytes. Only whole
        // characters fully inside the reported range are deleted: a byte count that ends
        // inside a character leaves that character (the fork's contract), so a combining
        // accent or the letter next to an emoji is preserved.
        Edit::DeleteSurroundingBytes { before_bytes, after_bytes } => apply_delete_surrounding_bytes(app, state, target, before_bytes, after_bytes),
        Edit::DeleteSurrounding { before_chars, after_chars } => apply_delete_surrounding_chars(app, state, target, before_chars, after_chars),
        // an input method (Chinese, Japanese, Korean) hands over its finished text as a commit (#1083)
        Edit::Insert(text) if from_ime => apply_ime_commit(app, state, target, &text),
        // ordinary typing, paste and marked text the composer kept as typed are application-origin
        Edit::Insert(text) | Edit::Paste(text) => apply_text_input(state, &text),
        Edit::Copy => apply_copy_cut(ui, state, false),
        Edit::Cut => apply_copy_cut(ui, state, true),
        Edit::Key { key, modifiers, .. } => apply_key_event(app, ui, state, layout, key, modifiers),
        Edit::LineBreak { modifiers } => apply_key_event(app, ui, state, layout, egui::Key::Enter, modifiers),
    }
}

/// Keyboard handling while a text layer is being edited.
fn text_edit(app: &mut FilmcraftApp, ui: &mut egui::Ui, v: &LayerView, ed: GfxEdit, resp: &egui::Response) {
    let _ = resp;
    let window_focused = ui.input(|input| input.viewport().focused.unwrap_or(input.focused));
    if !window_focused || app.ime_modal_blocks_input() || !ui.is_enabled() {
        ime_off(app, ui);
        return;
    }
    // moving to another panel ends the edit; Properties and Essential Graphics style the text
    // being edited, so working there keeps it (#683). Only a move counts: an edit started with
    // another panel focused (an agent's `ui.set`) goes on until the focus moves.
    use crate::dock::PanelKind;
    let panel_id = egui::Id::unique(TEXT_EDIT_ID).with("panel");
    let before = ui.data(|d| d.get_temp::<PanelKind>(panel_id));
    ui.data_mut(|d| d.insert_temp(panel_id, app.ui.focused));
    if before.is_some_and(|b| b != app.ui.focused) && !matches!(app.ui.focused, PanelKind::Program | PanelKind::Properties | PanelKind::EssentialGraphics) {
        end_edit(app, ui);
        ime_off(app, ui);
        return;
    }
    let eid = egui::Id::unique(TEXT_EDIT_ID);
    // a real (non-clickable) widget holds keyboard focus so shortcuts pause while typing
    let fr = ui.interact(Rect::from_points(&v.quad()), eid, Sense::focusable_noninteractive());
    fr.widget_info(|| egui::WidgetInfo::labeled(egui::Role::TextInput, true, "Text layer"));
    // while another field has the keyboard (a value typed in Properties) the keys are its own;
    // the text takes the keyboard back once nothing has it (#683)
    if ui.memory(|m| m.focused().is_some_and(|f| f != eid)) {
        ime_off(app, ui);
        return;
    }
    ui.memory_mut(|m| {
        m.request_focus(eid);
        m.set_focus_lock_filter(eid, egui::EventFilter { tab: true, horizontal_arrows: true, vertical_arrows: true, escape: true });
    });
    let Some(tp) = text_of(&v.spec) else {
        ime_off(app, ui);
        return;
    };
    let mut state = TextEditState { ed, text: tp.text.clone(), changed: false, stop: false, custom_mutations: true };
    // `ui.set.gfxEdit` accepts arbitrary byte offsets. Clamp both ends to UTF-8 boundaries here,
    // before any slice/replace_range/insert_str, so hostile offsets can never panic the editor.
    clamp_edit_offsets(&state.text, &mut state.ed);
    let l = text_layout(tp);
    let (events, time) = ui.input(|i| (i.events.clone(), i.time));
    let sequence = app.session.state.active_sequence.map_or(0, |sequence| sequence.0);
    let composition_target = graphics_ime_target(sequence, state.ed.clip, state.ed.layer, v.spec.uid, app.ime_replacement_epoch);
    let viewport = ui.ctx().viewport_id();
    if app.ime_composer_target != Some(composition_target) {
        app.ime_composer.discard(ui.ctx());
        app.ime_composer = craft_ui::ime::Composer::new().with_owner(composition_target.owner);
        app.ime_composer_target = Some(composition_target);
    }
    // A command, a control request or a caret move ended the composition: the composer drops
    // its copy of the marked text, and a late commit of it.
    if app.ime_composer.is_composing()
        && !(app.ime_composition == Some(crate::ImeComposition::Active(composition_target)) && app.ime_composition_viewport == Some(viewport))
    {
        app.ime_composer.discard(ui.ctx());
    }
    // The composer turns the IME on and publishes its descriptor; the editor runs the events
    // through it below, between the fence's per-event checks.
    let held = ui.ctx().input_mut(|i| std::mem::take(&mut i.events));
    let _ = app.ime_composer.update(ui.ctx(), true);
    ui.ctx().input_mut(|i| i.events = held);
    // Reset completions and the fence's admission run per raw event. A pointer press is
    // handled later in this pass (caret placement, another layer, or ending the edit): the
    // composer sees it, so a composition keeps its marked text, and the events after it
    // belong to the new target.
    let mut admitted = Vec::new();
    let mut pressed = false;
    for ev in events {
        if begin_text_event(app, ui, &state, composition_target, &ev).is_none() {
            continue;
        }
        pressed = matches!(ev, egui::Event::PointerButton { pressed: true, .. });
        admitted.push(ev);
        if pressed {
            break;
        }
    }
    let mut commits: Vec<String> = admitted
        .iter()
        .filter_map(|ev| match ev {
            egui::Event::Ime(egui::ImeEvent::Commit(text)) if !text.is_empty() => Some(text.clone()),
            _ => None,
        })
        .collect();
    let (steps, interrupted) = compose(&mut app.ime_composer, &admitted, time);
    for step in steps {
        if !state.custom_mutations {
            break;
        }
        let (from_ime, terminal) = step_origin(&step, &mut commits);
        app.sync_live_ime_destination(ui.ctx(), local_reset_destination(app, &state.ed, &state.text, composition_target));
        if from_ime && app.ime_reset.blocked(viewport) {
            continue;
        }
        if apply_text_step(app, ui, &mut state, composition_target, &l, step, from_ime) == EventFlow::Skip {
            continue;
        }
        finish_text_event(app, ui, &state, composition_target, from_ime, terminal);
    }
    if pressed {
        state.custom_mutations = false;
        app.suspend_ime_input(ui.ctx());
    } else if interrupted && matches!(app.ime_composition, Some(crate::ImeComposition::Active(_) | crate::ImeComposition::TerminalPending(_))) {
        // The composer kept the marked text as typed (a bare line break confirmed it): the
        // platform input method drops its copy.
        app.invalidate_ime_composition();
    }
    // Written now, not queued with the pass's other actions: an edit that ends later in this pass
    // (Escape, a click elsewhere, another layer) types the marked text into this text.
    if state.changed && state.text != tp.text {
        let text = std::mem::take(&mut state.text);
        if let Err(e) = app.session.execute("graphics.setText", json!({"clip": state.ed.clip, "layer": state.ed.layer, "text": text, "merge": true})) {
            app.ui.status = e.to_string();
        }
    }
    app.ui.gfx_edit = Some(state.ed);
    if state.stop {
        end_edit(app, ui);
    }
}

/// The editor doesn't take the keyboard in this pass: the composer turns the IME off, and the
/// fence's controls stay inert.
fn ime_off(app: &mut FilmcraftApp, ui: &egui::Ui) {
    let _ = app.ime_composer.update(ui.ctx(), false);
    app.suspended_ime_controls(ui.ctx());
}

/// Clamp an edit caret/anchor to valid UTF-8 boundaries (hostile `ui.set.gfxEdit` offsets).
fn clamp_edit_offsets(text: &str, ed: &mut GfxEdit) {
    ed.caret = char_boundary_floor(text, ed.caret);
    ed.anchor = char_boundary_floor(text, ed.anchor);
}

/// The two ranges `DeleteSurroundingBytes` deletes: whole characters fully contained in the
/// bytes before and after the sorted selection. The selection itself and any pre-edit (absent
/// from FilmCraft's committed model) are excluded. A byte count that ends inside a character
/// leaves that character whole.
fn surrounding_byte_deletion_ranges(
    text: &str,
    a: usize,
    b: usize,
    before_bytes: usize,
    after_bytes: usize,
) -> (core::ops::Range<usize>, core::ops::Range<usize>) {
    let a = char_boundary_floor(text, a);
    let b = char_boundary_ceil(text, b).max(a);
    let from = a.saturating_sub(before_bytes);
    let before_chars = text[..a].char_indices().rev().take_while(|(index, _)| *index >= from).count();
    let after_chars = text[b..].char_indices().take_while(|(index, c)| index + c.len_utf8() <= after_bytes).count();
    (prev_n_chars(text, a, before_chars)..a, b..next_n_chars(text, b, after_chars))
}

/// Apply the two delete-surrounding ranges while preserving the selection's direction.
///
/// Delete after the selection first, then before it, so the selection remains intact and both
/// endpoints shift only by the preceding deletion. This mirrors the stock `TextEdit`: a
/// surrounding deletion does not consume the selection, and the commit that usually follows
/// replaces it.
fn apply_surrounding_deletion(text: &mut String, ed: &mut GfxEdit, before: core::ops::Range<usize>, after: core::ops::Range<usize>) {
    let before_end = before.end;
    let before_len = before.len();
    if !after.is_empty() {
        text.replace_range(after, "");
    }
    if !before.is_empty() {
        text.replace_range(before, "");
    }
    let shift = |offset: usize| {
        let offset = if offset >= before_end { offset - before_len } else { offset };
        offset.min(text.len())
    };
    ed.caret = shift(ed.caret);
    ed.anchor = shift(ed.anchor);
}

/// Largest char boundary not after `i`.
fn char_boundary_floor(s: &str, mut i: usize) -> usize {
    i = i.min(s.len());
    while i > 0 && !s.is_char_boundary(i) {
        i -= 1;
    }
    i
}

/// Smallest char boundary not before `i`.
fn char_boundary_ceil(s: &str, mut i: usize) -> usize {
    i = i.min(s.len());
    while i < s.len() && !s.is_char_boundary(i) {
        i += 1;
    }
    i
}

/// Move `i` to a character boundary and then back up to `n` whole characters, stopping at
/// the start. Hostile offsets and counts are clamped instead of slicing inside a character.
fn prev_n_chars(s: &str, mut i: usize, n: usize) -> usize {
    i = char_boundary_floor(s, i);
    for _ in 0..n {
        if i == 0 {
            break;
        }
        i = match s[..i].char_indices().next_back() {
            Some((j, _)) => j,
            None => 0,
        };
    }
    i
}

/// Move `i` to a character boundary and then forward up to `n` whole characters, stopping at
/// the end. Hostile offsets and counts are clamped instead of slicing inside a character.
fn next_n_chars(s: &str, mut i: usize, n: usize) -> usize {
    i = char_boundary_ceil(s, i);
    for _ in 0..n {
        if i >= s.len() {
            break;
        }
        i = match s[i..].char_indices().nth(1) {
            Some((j, _)) => i + j,
            None => s.len(),
        };
    }
    i
}

fn word_left(s: &str, i: usize) -> usize {
    let mut j = i;
    let b: Vec<(usize, char)> = s[..i].char_indices().collect();
    let mut k = b.len();
    while k > 0 && b[k - 1].1.is_whitespace() {
        k -= 1;
    }
    while k > 0 && !b[k - 1].1.is_whitespace() {
        k -= 1;
    }
    if k < b.len() {
        j = b[k].0;
    }
    if k == 0 { 0 } else { j }
}

fn word_right(s: &str, i: usize) -> usize {
    let mut it = s[i..].char_indices().peekable();
    while it.peek().is_some_and(|(_, c)| c.is_whitespace()) {
        it.next();
    }
    while it.peek().is_some_and(|(_, c)| !c.is_whitespace()) {
        it.next();
    }
    it.peek().map_or(s.len(), |(b, _)| i + b)
}

// ---------------------------------------------------------------------------------------------
// Properties / Essential Graphics
// ---------------------------------------------------------------------------------------------

const ROW: f32 = 28.0;

fn pv(e: &EffectInstance, id: &str, mt: Tick) -> ParamValue {
    e.params.get(id).map(|p| p.value_at(mt)).or_else(|| e.def().and_then(|d| d.param(id)).map(|d| d.default.clone())).unwrap_or(ParamValue::Float(0.0))
}

fn color32(c: [f32; 4]) -> Color32 {
    let q = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
    Color32::from_rgba_unmultiplied(q(c[0]), q(c[1]), q(c[2]), q(c[3]))
}

/// The formatting knobs of an inline graphics number field.
struct NumberField<'a> {
    id: &'a str,
    speed: f64,
    range: (f64, f64),
    decimals: usize,
    unit: &'a str,
}

struct Ctx<'a> {
    clip: ClipId,
    layer: usize,
    /// The layer's index in the clip's effect list (what `effects.toggleAnimation` addresses).
    eidx: usize,
    e: &'a EffectInstance,
    mt: Tick,
    t: Tokens,
    actions: Vec<(String, Value)>,
    autos: Vec<(String, Rect, String)>,
    /// Characters selected with the Type tool in this layer: character properties apply to them.
    range: Option<(usize, usize)>,
}

/// Character properties a Type-tool selection can carry, with their `graphics.setCharStyle` names.
fn char_style_key(id: &str) -> Option<&'static str> {
    Some(match id {
        "font" => "font",
        "font_style" => "fontStyle",
        "size" => "size",
        "fill_color" => "color",
        "faux_bold" => "bold",
        "faux_italic" => "italic",
        "underline" => "underline",
        "tracking" => "tracking",
        "baseline_shift" => "baselineShift",
        "caps" => "caps",
        _ => return None,
    })
}

impl Ctx<'_> {
    fn set(&mut self, id: &str, v: Value) {
        if let (Some((a, b)), Some(k)) = (self.range, char_style_key(id)) {
            self.actions.push(("graphics.setCharStyle".into(), json!({"clip": self.clip.0, "layer": self.layer, "start": a, "end": b, "style": {k: v}})));
            return;
        }
        self.actions.push(("graphics.set".into(), json!({"clip": self.clip.0, "layer": self.layer, "props": {id: v}})));
    }
    fn auto(&mut self, id: &str, r: Rect, label: &str) {
        self.autos.push((format!("graphics.prop.{id}"), r, label.to_string()));
    }
    fn row(&self, ui: &mut egui::Ui, label: &str) -> (Rect, egui::Ui) {
        let (r, _) = ui.allocate_exact_size(vec2(ui.available_width(), ROW), Sense::hover());
        ui.painter().text(pos2(r.min.x + 18.0, r.center().y), Align2::LEFT_CENTER, label, Tokens::ui(12.0), self.t.text_dim);
        let v = ui.new_child(
            egui::UiBuilder::new()
                .max_rect(Rect::from_min_max(pos2(r.min.x + 130.0, r.min.y + 3.0), pos2(r.max.x - 6.0, r.max.y - 3.0)))
                .layout(egui::Layout::left_to_right(egui::Align::Center)),
        );
        (r, v)
    }
    /// The stopwatch in front of an animatable property's label (#2358): turns keyframing on or
    /// off at the playhead, like the one in Effect Controls. While a property animates, changing
    /// it here adds or updates a keyframe at the playhead instead of changing it everywhere. A
    /// Type-tool selection edits the selected characters, which have no keyframes: no stopwatch.
    fn stopwatch(&mut self, ui: &egui::Ui, row: Rect, id: &str) {
        let animatable = self.e.def().and_then(|d| d.param(id)).is_some_and(|d| d.animatable);
        if !animatable || (self.range.is_some() && char_style_key(id).is_some()) {
            return;
        }
        let animated = self.e.params.get(id).is_some_and(|p| p.is_animated());
        let sw = Rect::from_center_size(pos2(row.min.x + 8.0, row.center().y), vec2(12.0, 12.0));
        let resp = ui.interact(sw.expand(2.0), egui::Id::unique(("gfxsw", self.clip.0, self.layer, id)), Sense::click()).on_hover_text(tl!("Toggle animation"));
        let tint = if animated {
            self.t.accent
        } else if resp.hovered() {
            self.t.text
        } else {
            self.t.text_faint
        };
        icons::paint(ui.painter(), sw, Icon::Stopwatch, tint);
        self.autos.push((format!("graphics.prop.{id}.stopwatch"), sw, "Toggle animation".to_string()));
        if resp.clicked() {
            self.actions.push(("effects.toggleAnimation".into(), json!({"clip": self.clip.0, "effect": self.eidx, "param": id})));
        }
    }
    /// Draw a labeled hot number for this clip/layer/`id`, register it for automation and emit the changed value through `Ctx::set`; `range` bounds typed and
    /// dragged input.
    fn number(&mut self, ui: &mut egui::Ui, label: &str, id: &str, speed: f64, range: (f64, f64), dec: usize, unit: &str) {
        let v = pv(self.e, id, self.mt).as_f64().unwrap_or(0.0);
        let (r, mut vui) = self.row(ui, label);
        let (resp, nv) = crate::widgets::hot_number(
            &mut vui,
            label,
            egui::Id::unique(("gfxp", self.clip.0, self.layer, id)),
            v,
            HotNumberOptions::new(speed, range, dec, unit, &self.t),
        );
        self.auto(id, resp.rect, label);
        self.stopwatch(ui, r, id);
        if let Some(nv) = nv {
            self.set(id, json!(nv));
        }
    }
    /// Draw an inline numeric field without a label row or stopwatch, register it for automation
    /// and emit the edited value through `Ctx::set` when it changes.
    fn number_in(&mut self, vui: &mut egui::Ui, label: &str, field: NumberField<'_>) {
        let NumberField { id, speed, range, decimals, unit } = field;
        let v = pv(self.e, id, self.mt).as_f64().unwrap_or(0.0);
        let (resp, nv) = crate::widgets::hot_number(
            vui,
            label,
            egui::Id::unique(("gfxp", self.clip.0, self.layer, id)),
            v,
            HotNumberOptions::new(speed, range, decimals, unit, &self.t),
        );
        self.auto(id, resp.rect, id);
        if let Some(nv) = nv {
            self.set(id, json!(nv));
        }
    }
    /// Draw X and Y hot numbers for this clip/layer/`id` with independent ids and ±100,000 bounds, register both and emit both components when either changes.
    fn point(&mut self, ui: &mut egui::Ui, label: &str, id: &str) {
        let v = pv(self.e, id, self.mt).as_vec2().unwrap_or_default();
        let (r, mut vui) = self.row(ui, label);
        self.stopwatch(ui, r, id);
        let (rx, nx) = crate::widgets::hot_number(
            &mut vui,
            &format!("{label} X"),
            egui::Id::unique(("gfxp", self.clip.0, self.layer, id, "x")),
            v.x,
            HotNumberOptions::new(1.0, (-100_000.0, 100_000.0), 1, " X", &self.t),
        );
        vui.add_space(10.0);
        let (ry, ny) = crate::widgets::hot_number(
            &mut vui,
            &format!("{label} Y"),
            egui::Id::unique(("gfxp", self.clip.0, self.layer, id, "y")),
            v.y,
            HotNumberOptions::new(1.0, (-100_000.0, 100_000.0), 1, " Y", &self.t),
        );
        self.auto(&format!("{id}.x"), rx.rect, label);
        self.auto(&format!("{id}.y"), ry.rect, label);
        if nx.is_some() || ny.is_some() {
            self.set(id, json!([nx.unwrap_or(v.x), ny.unwrap_or(v.y)]));
        }
    }
    fn check(&mut self, vui: &mut egui::Ui, id: &str, label: &str) -> bool {
        let mut b = pv(self.e, id, self.mt).as_bool().unwrap_or(false);
        let r = vui.checkbox(&mut b, label);
        self.auto(id, r.rect, label);
        if r.changed() {
            self.set(id, json!(b));
        }
        b
    }
    fn color(&mut self, vui: &mut egui::Ui, id: &str) {
        let c = pv(self.e, id, self.mt).as_color().unwrap_or([1.0; 4]);
        let mut c32 = color32(c);
        let r = egui::color_picker::color_edit_button_srgba(vui, &mut c32, egui::color_picker::Alpha::Opaque);
        self.auto(id, r.rect, id);
        if r.changed() {
            let [r8, g8, b8, _] = c32.to_srgba_unmultiplied();
            self.set(id, json!(format!("#{r8:02x}{g8:02x}{b8:02x}")));
        }
    }
    /// Draw a combo box for a choice parameter, register it for automation and emit the selected
    /// index through `Ctx::set` when it changes. An out-of-range current value displays as an
    /// empty selection.
    fn choice(&mut self, vui: &mut egui::Ui, id: &str, label: &str, opts: &[&str], width: f32) {
        let cur = match pv(self.e, id, self.mt) {
            ParamValue::Choice(c) => c as usize,
            _ => 0,
        };
        let mut sel = cur;
        let r = egui::ComboBox::from_id_salt(("gfxc", self.clip.0, self.layer, id))
            .selected_text(opts.get(cur).map_or("", |o| crate::i18n::t(o)))
            .width(width)
            .show_ui(vui, |ui| {
                for (i, o) in opts.iter().enumerate() {
                    ui.selectable_value(&mut sel, i, crate::i18n::t(o));
                }
            });
        let response = crate::widgets::accessible_name(r.response, label);
        self.auto(id, response.rect, id);
        if sel != cur {
            self.set(id, json!(sel));
        }
    }
}

fn section(ui: &mut egui::Ui, app: &mut FilmcraftApp, name: &str, t: &Tokens) -> bool {
    section_header(ui, app, name, t).0
}

/// A collapsible section's header row: whether the section is open, and the row.
fn section_header(ui: &mut egui::Ui, app: &mut FilmcraftApp, name: &str, t: &Tokens) -> (bool, Rect) {
    ui.add_space(4.0);
    let key = format!("gfx:{name}");
    let open = !app.ui.collapsed_fx.contains(&key);
    let (r, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 30.0), Sense::click());
    ui.painter().line_segment([r.left_top(), r.right_top()], Stroke::new(1.0, t.separator));
    icons::paint(
        ui.painter(),
        Rect::from_center_size(pos2(r.min.x + 6.0, r.center().y), vec2(10.0, 10.0)),
        if open { Icon::ChevronDown } else { Icon::ChevronRight },
        t.text_dim,
    );
    ui.painter().text(pos2(r.min.x + 18.0, r.center().y), Align2::LEFT_CENTER, crate::i18n::t(name), Tokens::semibold(13.0), t.text);
    app.auto.add(&format!("graphics.section.{}", name.replace(' ', "")), r, name);
    if resp.clicked() {
        if open {
            app.ui.collapsed_fx.push(key);
        } else {
            app.ui.collapsed_fx.retain(|k| *k != key);
        }
    }
    (open, r)
}

/// A small code-drawn alignment glyph (original artwork: bars against an edge line).
fn align_glyph(p: &egui::Painter, r: Rect, how: &str, c: Color32) {
    let s = Stroke::new(1.2, c);
    let (x0, x1, y0, y1) = (r.min.x + 2.0, r.max.x - 2.0, r.min.y + 2.0, r.max.y - 2.0);
    let (cx, cy) = ((x0 + x1) / 2.0, (y0 + y1) / 2.0);
    let bar = |a: Pos2, b: Pos2| p.rect_filled(Rect::from_two_pos(a, b), 0.0, c);
    match how {
        "left" => {
            p.line_segment([pos2(x0, y0), pos2(x0, y1)], s);
            bar(pos2(x0 + 2.0, y0 + 2.0), pos2(x1, y0 + 5.0));
            bar(pos2(x0 + 2.0, y1 - 5.0), pos2(cx + 1.0, y1 - 2.0));
        }
        "right" => {
            p.line_segment([pos2(x1, y0), pos2(x1, y1)], s);
            bar(pos2(x0, y0 + 2.0), pos2(x1 - 2.0, y0 + 5.0));
            bar(pos2(cx - 1.0, y1 - 5.0), pos2(x1 - 2.0, y1 - 2.0));
        }
        "hcenter" => {
            p.line_segment([pos2(cx, y0), pos2(cx, y1)], s);
            bar(pos2(x0 + 1.0, y0 + 2.0), pos2(x1 - 1.0, y0 + 5.0));
            bar(pos2(cx - 3.0, y1 - 5.0), pos2(cx + 3.0, y1 - 2.0));
        }
        "top" => {
            p.line_segment([pos2(x0, y0), pos2(x1, y0)], s);
            bar(pos2(x0 + 2.0, y0 + 2.0), pos2(x0 + 5.0, y1));
            bar(pos2(x1 - 5.0, y0 + 2.0), pos2(x1 - 2.0, cy + 1.0));
        }
        "bottom" => {
            p.line_segment([pos2(x0, y1), pos2(x1, y1)], s);
            bar(pos2(x0 + 2.0, y0), pos2(x0 + 5.0, y1 - 2.0));
            bar(pos2(x1 - 5.0, cy - 1.0), pos2(x1 - 2.0, y1 - 2.0));
        }
        "vcenter" => {
            p.line_segment([pos2(x0, cy), pos2(x1, cy)], s);
            bar(pos2(x0 + 2.0, y0 + 1.0), pos2(x0 + 5.0, y1 - 1.0));
            bar(pos2(x1 - 5.0, cy - 3.0), pos2(x1 - 2.0, cy + 3.0));
        }
        "dh" => {
            for x in [x0 + 1.0, cx - 1.5, x1 - 4.0] {
                bar(pos2(x, y0 + 3.0), pos2(x + 3.0, y1 - 3.0));
            }
        }
        "dv" => {
            for y in [y0 + 1.0, cy - 1.5, y1 - 4.0] {
                bar(pos2(x0 + 3.0, y), pos2(x1 - 3.0, y + 3.0));
            }
        }
        // paragraph alignment: four text lines
        a @ ("pl" | "pc" | "pr" | "pj") => {
            for (i, w) in [1.0f32, 0.6, 0.85, 0.5].iter().enumerate() {
                let w = if a == "pj" && i < 3 { 1.0 } else { *w } * (x1 - x0);
                let y = y0 + 1.0 + i as f32 * 3.2;
                let xa = match a {
                    "pc" => cx - w / 2.0,
                    "pr" => x1 - w,
                    _ => x0,
                };
                p.line_segment([pos2(xa, y), pos2(xa + w, y)], s);
            }
        }
        _ => {}
    }
}

fn glyph_button(ui: &mut egui::Ui, how: &str, tip: &str, on: bool, t: &Tokens) -> egui::Response {
    let (r, resp) = ui.allocate_exact_size(vec2(24.0, 22.0), Sense::click());
    if on {
        ui.painter().rect_filled(r, 3.0, Color32::from_rgb(0x4b, 0x4b, 0x4b));
    } else if resp.hovered() {
        ui.painter().rect_filled(r, 3.0, t.hover);
    }
    align_glyph(ui.painter(), Rect::from_center_size(r.center(), vec2(16.0, 14.0)), how, if resp.hovered() || on { t.text } else { t.text_dim });
    resp.on_hover_text(tip)
}

fn letter_button(ui: &mut egui::Ui, text: &str, tip: &str, on: bool, t: &Tokens, font: egui::FontId) -> egui::Response {
    let (r, resp) = ui.allocate_exact_size(vec2(26.0, 22.0), Sense::click());
    if on {
        ui.painter().rect_filled(r, 3.0, Color32::from_rgb(0x4b, 0x4b, 0x4b));
    } else if resp.hovered() {
        ui.painter().rect_filled(r, 3.0, t.hover);
    }
    ui.painter().text(r.center(), Align2::CENTER_CENTER, text, font, if on || resp.hovered() { t.text } else { t.text_dim });
    resp.on_hover_text(tip)
}

/// Whether the Properties panel should show the graphic editor.
pub fn graphic_selected(app: &FilmcraftApp) -> bool {
    selected_graphic(app).is_some()
}

/// The graphic editor (Properties panel / Essential Graphics ▸ Edit).
pub fn properties(app: &mut FilmcraftApp, ui: &mut egui::Ui, rect: Rect) {
    let t = app.tokens;
    let Some((clip, it)) = selected_graphic(app) else {
        crate::dock::placeholder(ui, rect, &t, tl!("Select a graphic clip, or use the Type tool (T) on the Program monitor"));
        let br = Rect::from_center_size(rect.center() + vec2(0.0, 30.0), vec2(150.0, 26.0));
        let resp = ui.put(br, egui::Button::new(tl!("Create new graphic")));
        app.auto.add("graphics.createNew", br, "Create new graphic");
        if resp.clicked()
            && let Err(e) = app.session.execute("graphics.newText", json!({"text": "New Text"}))
        {
            app.ui.status = e.to_string();
        }
        return;
    };
    let ph = app.session.playhead();
    let mt = it.source_time_at(ph.clamp(it.start, it.end() - Tick(1)));
    let idx = layer_indices(&it.effects);
    let sel = app.session.state.graphic_layers.first().copied().filter(|l| *l < idx.len());
    let mut actions: Vec<(String, Value)> = Vec::new();
    let mut autos: Vec<(String, Rect, String)> = Vec::new();
    let mut bui = ui.new_child(egui::UiBuilder::new().max_rect(rect.shrink2(vec2(12.0, 6.0))).id_salt("gfx-props"));
    bui.set_clip_rect(rect);
    egui::ScrollArea::vertical().id_salt("gfx-props-scroll").auto_shrink([false, false]).show(&mut bui, |ui| {
        // header
        let (hr, _) = ui.allocate_exact_size(vec2(ui.available_width(), 30.0), Sense::hover());
        let lc = app.session.prefs.labels.rgb(it.label);
        ui.painter().rect_filled(Rect::from_center_size(pos2(hr.min.x + 8.0, hr.center().y), vec2(12.0, 12.0)), 2.0, Color32::from_rgb(lc[0], lc[1], lc[2]));
        ui.painter().text(pos2(hr.min.x + 22.0, hr.center().y), Align2::LEFT_CENTER, &it.name, Tokens::semibold(12.5), t.text);
        // ---- layers (front first)
        if section(ui, app, "Layers", &t) {
            ui.horizontal(|ui| {
                ui.add_space(14.0);
                for (label, key, tip, cmd, p) in [
                    ("T", "NewTextLayer", tl!("New Text Layer"), "graphics.newText", json!({"clip": clip.0, "text": tl!("New Text")})),
                    ("", "NewRectangle", tl!("New Rectangle"), "graphics.newShape", json!({"clip": clip.0, "shape": "rectangle"})),
                    ("", "NewEllipse", tl!("New Ellipse"), "graphics.newShape", json!({"clip": clip.0, "shape": "ellipse", "size": [200, 200]})),
                ] {
                    let r = letter_button(ui, label, tip, false, &t, Tokens::semibold(13.0));
                    if label.is_empty() {
                        let ir = Rect::from_center_size(r.rect.center(), vec2(14.0, 14.0));
                        icons::paint(ui.painter(), ir, if key == "NewRectangle" { Icon::Rectangle } else { Icon::Ellipse }, t.text_dim);
                    }
                    autos.push((format!("graphics.{key}"), r.rect, tip.into()));
                    if r.clicked() {
                        actions.push((cmd.into(), p));
                    }
                }
                ui.add_space(8.0);
                if let Some(l) = sel {
                    for (label, tip, to) in [("↑", tl!("Bring Forward"), "forward"), ("↓", tl!("Send Backward"), "backward")] {
                        let r = letter_button(ui, label, tip, false, &t, Tokens::ui(13.0));
                        autos.push((format!("graphics.arrange.{to}"), r.rect, tip.into()));
                        if r.clicked() {
                            actions.push(("graphics.arrangeLayer".into(), json!({"clip": clip.0, "layer": l, "to": to})));
                        }
                    }
                    let (dr, dresp) = ui.allocate_exact_size(vec2(24.0, 22.0), Sense::click());
                    icons::paint(ui.painter(), dr.shrink(4.0), Icon::Trash, if dresp.hovered() { t.text } else { t.text_dim });
                    autos.push(("graphics.deleteLayer".into(), dr, "Delete Layer".into()));
                    if dresp.on_hover_text(tl!("Delete Layer")).clicked() {
                        actions.push(("graphics.deleteLayer".into(), json!({"clip": clip.0, "layer": l})));
                    }
                }
            });
            for (li, &ei) in idx.iter().enumerate().rev() {
                let e = &it.effects[ei];
                let (r, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 26.0), Sense::click());
                let on = sel == Some(li);
                if on {
                    ui.painter().rect_filled(r, 2.0, Color32::from_rgb(0x2e, 0x3d, 0x55));
                } else if resp.hovered() {
                    ui.painter().rect_filled(r, 2.0, t.hover);
                }
                let kind = if e.effect == graphic::TEXT_LAYER { "T" } else { "◆" };
                ui.painter().text(pos2(r.min.x + 22.0, r.center().y), Align2::CENTER_CENTER, kind, Tokens::semibold(12.0), t.text_dim);
                ui.painter().text(pos2(r.min.x + 38.0, r.center().y), Align2::LEFT_CENTER, layer_display_name(e, li), Tokens::ui(12.0), t.text);
                // visibility
                let er = Rect::from_center_size(pos2(r.max.x - 14.0, r.center().y), vec2(16.0, 16.0));
                let eresp = ui.interact(er, egui::Id::unique(("gfx-eye", clip.0, li)), Sense::click());
                icons::paint(ui.painter(), er.shrink(2.0), if e.enabled { Icon::Eye } else { Icon::EyeOff }, t.text_dim);
                autos.push((format!("graphics.layers.{li}.visible"), er, "Toggle visibility".into()));
                autos.push((format!("graphics.layers.{li}"), r, layer_display_name(e, li)));
                if eresp.clicked() {
                    actions.push(("graphics.set".into(), json!({"clip": clip.0, "layer": li, "props": {"enabled": !e.enabled}})));
                } else if resp.clicked() {
                    actions.push(("graphics.selectLayer".into(), json!({"clip": clip.0, "layers": [li]})));
                }
            }
        }
        // ---- template properties (a graphic made from a graphics template)
        if it.graphic.as_ref().is_some_and(|m| m.template.is_some()) && section(ui, app, "Template Properties", &t) {
            crate::panels::graphics_templates::template_controls(app, ui, clip, &it, mt, &mut autos, &mut actions);
        }
        let Some(l) = sel else {
            if section(ui, app, "Responsive Design - Time", &t) {
                crate::panels::graphics_templates::responsive_time(app, ui, clip, &it, &mut autos, &mut actions);
            }
            ui.add_space(12.0);
            ui.label(egui::RichText::new(tl!("Select a layer to edit its properties.")).color(t.text_faint));
            return;
        };
        let e = &it.effects[idx[l]];
        // a Type-tool selection in this layer: character properties apply to the selected characters
        let range = app.ui.gfx_edit.as_ref().filter(|ed| ed.clip == clip.0 && ed.layer == l && ed.caret != ed.anchor).and_then(|ed| {
            let text = match pv(e, "text", mt) {
                ParamValue::Text(s) => s,
                _ => return None,
            };
            let ch = |b: usize| text.get(..b.min(text.len())).map_or(0, |s| s.chars().count());
            Some((ch(ed.caret.min(ed.anchor)), ch(ed.caret.max(ed.anchor))))
        });
        let mut cx = Ctx { clip, layer: l, eidx: idx[l], e, mt, t, actions: Vec::new(), autos: Vec::new(), range };
        if section(ui, app, "Responsive Design - Position", &t) {
            crate::panels::graphics_templates::responsive_position(app, ui, clip, &it, l, &mut cx.autos, &mut cx.actions);
        }
        // ---- align and transform
        if section(ui, app, "Align and Transform", &t) {
            ui.horizontal(|ui| {
                ui.add_space(14.0);
                for (how, tip) in [
                    ("left", tl!("Align Left")),
                    ("hcenter", tl!("Align Center Horizontally")),
                    ("right", tl!("Align Right")),
                    ("top", tl!("Align Top")),
                    ("vcenter", tl!("Align Center Vertically")),
                    ("bottom", tl!("Align Bottom")),
                ] {
                    let r = glyph_button(ui, how, tip, false, &t);
                    cx.autos.push((format!("graphics.align.{how}"), r.rect, tip.into()));
                    if r.clicked() {
                        cx.actions.push(("graphics.align".into(), json!({"clip": clip.0, "align": how})));
                    }
                }
                ui.add_space(6.0);
                for (how, axis, tip) in [("dh", "horizontal", tl!("Distribute Horizontally")), ("dv", "vertical", tl!("Distribute Vertically"))] {
                    let r = glyph_button(ui, how, tip, false, &t);
                    cx.autos.push((format!("graphics.distribute.{axis}"), r.rect, tip.into()));
                    if r.clicked() {
                        cx.actions.push(("graphics.distribute".into(), json!({"clip": clip.0, "axis": axis, "layers": (0..idx.len()).collect::<Vec<_>>()})));
                    }
                }
            });
            cx.point(ui, tl!("Position"), "position");
            cx.point(ui, tl!("Anchor point"), "anchor");
            cx.number(ui, tl!("Scale"), "scale", 0.5, (0.0, 100_000.0), 0, " %");
            cx.number(ui, tl!("Rotation"), "rotation", 0.5, (-36_000.0, 36_000.0), 1, " °");
            cx.number(ui, tl!("Opacity"), "opacity", 0.5, (0.0, 100.0), 0, " %");
        }
        // ---- text
        let text_open = e.effect == graphic::TEXT_LAYER && {
            let (open, head) = section_header(ui, app, "Text", &t);
            // Text Properties: the layer type (point / paragraph) and text styling
            let wr = Rect::from_center_size(pos2(head.max.x - 16.0, head.center().y), vec2(22.0, 22.0));
            let wresp = ui.interact(wr, egui::Id::unique(("gfx-text-props", clip.0, l)), Sense::click());
            icons::paint(ui.painter(), wr.shrink(4.0), Icon::Wrench, if wresp.hovered() { t.text } else { t.text_dim });
            cx.autos.push(("graphics.textProperties".into(), wr, "Text Properties".into()));
            if wresp.on_hover_text(tl!("Text Properties")).clicked() {
                let flag = |id: &str| pv(e, id, mt).as_bool().unwrap_or(false);
                let ligatures = flag("ligatures");
                app.ui.text_props_dialog = Some(TextPropsDialog {
                    clip: clip.0,
                    layer: l,
                    paragraph: pv(e, "box_width", mt).as_f64().unwrap_or(0.0) > 0.0 && !flag("vertical"),
                    vertical: flag("vertical"),
                    ligatures,
                    ligatures_was: ligatures,
                });
            }
            open
        };
        if text_open {
            // source text (multi-line), committed as it is typed
            let mut s = match pv(e, "text", mt) {
                ParamValue::Text(s) => s,
                _ => String::new(),
            };
            let before = s.clone();
            ui.horizontal(|ui| {
                ui.add_space(14.0);
                let r = crate::widgets::accessible_name(
                    ui.add(egui::TextEdit::multiline(&mut s).desired_rows(2).desired_width(ui.available_width() - 8.0).font(Tokens::ui(12.0))),
                    "Source Text",
                );
                cx.autos.push(("graphics.sourceText".into(), r.rect, "Source Text".into()));
            });
            if s != before {
                cx.actions.push(("graphics.setText".into(), json!({"clip": clip.0, "layer": l, "text": s, "merge": true})));
            }
            let family = match pv(e, "font", mt) {
                ParamValue::Text(f) => f,
                _ => String::new(),
            };
            let style = match pv(e, "font_style", mt) {
                ParamValue::Text(f) => f,
                _ => String::new(),
            };
            let fams = filmcraft_text::families();
            let (_, mut vui) = cx.row(ui, tl!("Font"));
            let r = egui::ComboBox::from_id_salt(("gfx-font", clip.0, l)).selected_text(&family).width(170.0).height(400.0).show_ui(&mut vui, |ui| {
                if !filmcraft_text::fonts::system_scanned() {
                    filmcraft_text::fonts::scan_system();
                }
                for (f, _) in &fams {
                    if ui.selectable_label(*f == family, f).clicked() {
                        cx.set("font", json!(f));
                    }
                }
            });
            let response = crate::widgets::accessible_name(r.response, "Font");
            cx.auto("font", response.rect, "Font");
            let styles: Vec<String> = fams.iter().find(|(f, _)| f.eq_ignore_ascii_case(&family)).map(|(_, s)| s.clone()).unwrap_or_default();
            let (_, mut vui) = cx.row(ui, tl!("Font Style"));
            let r = egui::ComboBox::from_id_salt(("gfx-style", clip.0, l)).selected_text(&style).width(170.0).show_ui(&mut vui, |ui| {
                for st in styles.iter().map(String::as_str).chain(["Bold", "Italic", "Bold Italic"]).filter({
                    let mut seen = Vec::new();
                    move |x: &&str| {
                        if seen.contains(x) {
                            false
                        } else {
                            seen.push(*x);
                            true
                        }
                    }
                }) {
                    if ui.selectable_label(st == style, st).clicked() {
                        cx.set("font_style", json!(st));
                    }
                }
            });
            let response = crate::widgets::accessible_name(r.response, "Font Style");
            cx.auto("font_style", response.rect, "Font Style");
            cx.number(ui, tl!("Font Size"), "size", 0.5, (1.0, 2000.0), 0, "");
            let align = match pv(e, "align", mt) {
                ParamValue::Choice(c) => c,
                _ => 0,
            };
            let (_, mut vui) = cx.row(ui, tl!("Alignment"));
            for (i, (how, tip)) in
                [("pl", tl!("Left Align Text")), ("pc", tl!("Center Align Text")), ("pr", tl!("Right Align Text")), ("pj", tl!("Justify"))].iter().enumerate()
            {
                let r = glyph_button(&mut vui, how, tip, align == i as u32, &t);
                cx.auto(&format!("align.{i}"), r.rect, tip);
                if r.clicked() {
                    cx.set("align", json!(i));
                }
            }
            cx.number(ui, tl!("Tracking"), "tracking", 1.0, (-1000.0, 10_000.0), 0, "");
            cx.number(ui, tl!("Leading"), "leading", 0.5, (-5000.0, 5000.0), 0, "");
            cx.number(ui, tl!("Baseline Shift"), "baseline_shift", 0.5, (-5000.0, 5000.0), 0, "");
            let vertical = pv(e, "vertical", mt).as_bool().unwrap_or(false);
            let (_, mut vui) = cx.row(ui, tl!("Orientation"));
            let mut value = vertical;
            let response = vui.checkbox(&mut value, tl!("Vertical Text"));
            cx.auto("vertical", response.rect, "Vertical Text");
            if response.changed() {
                cx.set("vertical", json!(value));
            }
            cx.number(ui, tl!("Text Box Width"), "box_width", 2.0, (0.0, 100_000.0), 0, "");
            cx.number(ui, tl!("Text Box Height"), "box_height", 2.0, (0.0, 100_000.0), 0, "");
            let (_, mut vui) = cx.row(ui, tl!("Style"));
            let flag = |id: &str| pv(e, id, mt).as_bool().unwrap_or(false);
            let caps = match pv(e, "caps", mt) {
                ParamValue::Choice(c) => c,
                _ => 0,
            };
            for (id, label, tip, on, font) in [
                ("faux_bold", "T", tl!("Faux Bold"), flag("faux_bold"), Tokens::semibold(14.0)),
                ("faux_italic", "T", tl!("Faux Italic"), flag("faux_italic"), egui::FontId::new(14.0, egui::FontFamily::Proportional)),
                ("all_caps", "TT", tl!("All Caps"), caps == 1, Tokens::ui(12.0)),
                ("small_caps", "Tt", tl!("Small Caps"), caps == 2, Tokens::ui(12.0)),
                ("underline", "U", tl!("Underline"), flag("underline"), Tokens::ui(13.0)),
            ] {
                let r = letter_button(&mut vui, label, tip, on, &t, font);
                if id == "faux_italic" {
                    // slanted stroke to read as italic
                    let c = r.rect.center();
                    vui.painter().line_segment([c + vec2(-5.0, 6.0), c + vec2(5.0, -6.0)], Stroke::new(0.8, t.text_faint));
                }
                cx.auto(id, r.rect, tip);
                if r.clicked() {
                    match id {
                        "all_caps" => cx.set("caps", json!(if caps == 1 { 0 } else { 1 })),
                        "small_caps" => cx.set("caps", json!(if caps == 2 { 0 } else { 2 })),
                        _ => cx.set(id, json!(!on)),
                    }
                }
            }
            let (_, mut vui) = cx.row(ui, tl!("OpenType"));
            cx.check(&mut vui, "kerning", tl!("Kerning"));
            cx.check(&mut vui, "ligatures", tl!("Ligatures"));
        }
        if e.effect == graphic::SHAPE_LAYER && section(ui, app, "Shape", &t) {
            let (_, mut vui) = cx.row(ui, tl!("Shape"));
            cx.choice(&mut vui, "shape", tl!("Shape"), graphic::SHAPE_OPTS, 120.0);
            cx.point(ui, tl!("Size"), "size");
            cx.number(ui, tl!("Corner Radius"), "corner_radius", 0.5, (0.0, 10_000.0), 0, "");
            cx.number(ui, tl!("Polygon Sides"), "sides", 0.05, (3.0, 64.0), 0, "");
        }
        // ---- appearance
        if section(ui, app, "Appearance", &t) {
            let (_, mut vui) = cx.row(ui, tl!("Fill"));
            let fill_on = cx.check(&mut vui, "fill", "");
            cx.color(&mut vui, "fill_color");
            if fill_on {
                vui.add_space(6.0);
                cx.choice(&mut vui, "fill_kind", tl!("Fill Type"), graphic::FILL_KIND_OPTS, 130.0);
            }
            let fill_kind = match pv(e, "fill_kind", mt) {
                ParamValue::Choice(c) => c,
                _ => 0,
            };
            if fill_on && fill_kind == 1 {
                let (_, mut vui) = cx.row(ui, tl!("Gradient"));
                cx.color(&mut vui, "gradient_start");
                cx.color(&mut vui, "gradient_end");
                cx.number(ui, tl!("   Angle"), "gradient_angle", 0.5, (-3600.0, 3600.0), 0, " °");
            }
            for (on, col, w, kind, label, width_label, type_label) in [
                ("stroke", "stroke_color", "stroke_width", "stroke_type", tl!("Stroke"), tl!("Stroke Width"), tl!("Stroke Type")),
                ("stroke2", "stroke2_color", "stroke2_width", "stroke2_type", tl!("Stroke 2"), tl!("Stroke 2 Width"), tl!("Stroke 2 Type")),
            ] {
                let (_, mut vui) = cx.row(ui, label);
                cx.check(&mut vui, on, "");
                cx.color(&mut vui, col);
                vui.add_space(6.0);
                cx.number_in(&mut vui, width_label, NumberField { id: w, speed: 0.2, range: (0.0, 1000.0), decimals: 1, unit: "" });
                vui.add_space(6.0);
                cx.choice(&mut vui, kind, type_label, graphic::STROKE_OPTS, 70.0);
            }
            let (_, mut vui) = cx.row(ui, tl!("Background"));
            let bg = cx.check(&mut vui, "background", "");
            cx.color(&mut vui, "background_color");
            if bg {
                cx.number(ui, tl!("   Opacity"), "background_opacity", 0.5, (0.0, 100.0), 0, " %");
                cx.number(ui, tl!("   Size"), "background_size", 0.5, (0.0, 1000.0), 0, "");
                cx.number(ui, tl!("   Corner Radius"), "background_radius", 0.5, (0.0, 1000.0), 0, "");
            }
            let (_, mut vui) = cx.row(ui, tl!("Shadow"));
            let sh = cx.check(&mut vui, "shadow", "");
            cx.color(&mut vui, "shadow_color");
            if sh {
                cx.number(ui, tl!("   Opacity"), "shadow_opacity", 0.5, (0.0, 100.0), 0, " %");
                cx.number(ui, tl!("   Angle"), "shadow_angle", 0.5, (-36_000.0, 36_000.0), 0, " °");
                cx.number(ui, tl!("   Distance"), "shadow_distance", 0.5, (0.0, 1000.0), 0, "");
                cx.number(ui, tl!("   Size"), "shadow_size", 0.5, (0.0, 1000.0), 0, "");
                cx.number(ui, tl!("   Blur"), "shadow_blur", 0.5, (0.0, 1000.0), 0, "");
            }
        }
        actions.append(&mut cx.actions);
        autos.append(&mut cx.autos);
    });
    for (id, r, label) in autos {
        app.auto.add(&id, r, &label);
    }
    let (down, press) = ui.ctx().input(|i| (i.pointer.any_down(), i.pointer.press_start_time()));
    let key = egui::Id::unique("gfx-props-drag-step");
    let begun = ui.ctx().data(|d| d.get_temp::<Option<f64>>(key)).flatten();
    let mut wrote = false;
    for (cmd, mut p) in actions {
        // a drag of a property is one undo step: the first change of each press begins a new one,
        // the rest fold into it (like Effect Controls, #201 / #887)
        if matches!(cmd.as_str(), "graphics.set" | "graphics.setCharStyle") && down {
            p["merge"] = json!(true);
            p["begin"] = json!(begun != press);
            wrote = true;
        }
        if let Err(e) = app.session.execute(&cmd, p) {
            app.ui.status = e.to_string();
        }
    }
    if down && (wrote || begun.is_some()) {
        ui.ctx().data_mut(|d| d.insert_temp(key, press));
    } else if !down {
        ui.ctx().data_mut(|d| d.remove::<Option<f64>>(key));
    }
}

/// Text Properties (the wrench in the Text section): Text Layer Type and Text Styling.
pub fn dialogs(app: &mut FilmcraftApp, ctx: &egui::Context) {
    let Some(mut d) = app.ui.text_props_dialog.clone() else { return };
    let mut elems: Vec<(String, Rect, &str)> = Vec::new();
    let mut close = crate::widgets::escape_closes(ctx);
    let mut ok = false;
    let name = |paragraph: bool| if paragraph { tl!("Paragraph Text") } else { tl!("Point Text") };
    let id = egui::Id::unique("gfx-text-properties");
    egui::Window::new(tl!("Text Properties")).id(id).collapsible(false).resizable(false).centered(ctx).show(ctx, |ui| {
        ui.label(tl!("Text Layer Type"));
        let r = egui::ComboBox::from_id_salt("gfx-text-type").selected_text(name(d.paragraph)).width(190.0).show_ui(ui, |ui| {
            for paragraph in [false, true] {
                // vertical text has no box to wrap in
                let r = ui.add_enabled_ui(!(paragraph && d.vertical), |ui| ui.selectable_label(d.paragraph == paragraph, name(paragraph))).inner;
                elems.push((format!("graphics.textProperties.type.{}", if paragraph { "paragraph" } else { "point" }), r.rect, name(paragraph)));
                if r.clicked() {
                    d.paragraph = paragraph;
                }
            }
        });
        elems.push(("graphics.textProperties.type".into(), r.response.rect, "Text Layer Type"));
        ui.add_space(6.0);
        ui.label(tl!("Text Styling"));
        let r = ui.checkbox(&mut d.ligatures, tl!("Ligatures"));
        elems.push(("graphics.textProperties.ligatures".into(), r.rect, "Ligatures"));
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            let r = ui.button(tl!("Cancel"));
            elems.push(("graphics.textProperties.cancel".into(), r.rect, "Cancel"));
            close |= r.clicked();
            let r = ui.button(tl!("OK"));
            elems.push(("graphics.textProperties.ok".into(), r.rect, "OK"));
            ok |= r.clicked();
        });
    });
    for (id, r, l) in elems {
        app.auto.add(&id, r, l);
    }
    if ok {
        let target = json!({"clip": d.clip, "layer": d.layer});
        let mut run = |cmd: &str, extra: Value| {
            let mut p = target.clone();
            if let (Some(p), Some(x)) = (p.as_object_mut(), extra.as_object()) {
                p.extend(x.clone());
            }
            if let Err(e) = app.session.execute(cmd, p) {
                app.ui.status = e.to_string();
            }
        };
        // a layer already of that type is left alone (no undo step)
        run("graphics.setTextType", json!({"type": if d.paragraph { "paragraph" } else { "point" }}));
        if d.ligatures != d.ligatures_was {
            run("graphics.set", json!({"props": {"ligatures": d.ligatures}}));
        }
        // the text being typed into may have been rewritten
        app.ui.gfx_edit = None;
    }
    app.ui.text_props_dialog = if ok || close { None } else { Some(d) };
}

#[cfg(test)]
mod ime_byte_delete_tests {
    use super::{
        GfxEdit, apply_surrounding_deletion, char_boundary_ceil, char_boundary_floor, clamp_edit_offsets, graphics_ime_target, next_n_chars, prev_n_chars,
        surrounding_byte_deletion_ranges,
    };

    /// Floor/ceil byte offsets clamp to UTF-8 character boundaries, including offsets past the
    /// end and offsets inside a multi-byte character.
    #[test]
    fn byte_boundaries_never_split_a_code_point() {
        let s = "aé😀z"; // 1 + 2 + 4 + 1 bytes
        assert_eq!(char_boundary_floor(s, 2), 1);
        assert_eq!(char_boundary_ceil(s, 2), 3);
        assert_eq!(char_boundary_floor(s, 4), 3);
        assert_eq!(char_boundary_ceil(s, 4), 7);
        assert_eq!(char_boundary_floor(s, 999), s.len());
    }

    /// Byte-count surrounding deletion removes only whole characters: a partial é or emoji byte
    /// count deletes nothing, and the selection itself is always excluded.
    #[test]
    fn deleting_surrounding_bytes_leaves_partial_characters() {
        let s = "aé😀z"; // a=1, é=1..3, 😀=3..7, z=7..8
        // A cursor after "aé" (byte 3): one byte of "é" is not a whole character, so nothing is
        // deleted; two bytes are exactly "é".
        assert_eq!(surrounding_byte_deletion_ranges(s, 3, 3, 1, 0), (3..3, 3..3));
        assert_eq!(surrounding_byte_deletion_ranges(s, 3, 3, 2, 0), (1..3, 3..3));
        assert_eq!(&s[1..3], "é");
        // After the cursor: the 4-byte emoji is only deleted when all four bytes are reported.
        for partial in 1..4 {
            assert_eq!(surrounding_byte_deletion_ranges(s, 3, 3, 0, partial), (3..3, 3..3), "after_bytes={partial}");
        }
        assert_eq!(surrounding_byte_deletion_ranges(s, 3, 3, 0, 4), (3..3, 3..7));
        // The selection itself is excluded; only its surroundings are deleted.
        assert_eq!(surrounding_byte_deletion_ranges(s, 1, 3, 0, 0), (1..1, 3..3));
        assert_eq!(surrounding_byte_deletion_ranges(s, 1, 3, 1, 4), (0..1, 3..7));
    }

    /// Applying before/after surrounding deletions preserves a reversed selection's direction
    /// while shifting both endpoints by the preceding deletion.
    #[test]
    fn surrounding_deletion_keeps_reversed_selection_direction() {
        let mut text = "aé😀z".to_string();
        let mut ed = GfxEdit { clip: 1, layer: 0, caret: 3, anchor: 1, ..Default::default() };
        let (before, after) = surrounding_byte_deletion_ranges(&text, 1, 3, 1, 4);
        apply_surrounding_deletion(&mut text, &mut ed, before, after);
        assert_eq!(text, "éz");
        assert_eq!((ed.caret, ed.anchor), (2, 0), "primary caret stays after the anchor");
    }

    /// Hostile `gfxEdit` caret/anchor offsets inside multi-byte characters clamp to boundaries
    /// without panicking.
    #[test]
    fn hostile_edit_offsets_are_clamped_to_char_boundaries() {
        let s = "é😀";
        let mut ed = GfxEdit { clip: 1, layer: 0, caret: 1, anchor: 3, ..Default::default() };
        clamp_edit_offsets(s, &mut ed);
        assert_eq!((ed.caret, ed.anchor), (0, 2), "offsets inside the two characters move to boundaries");
        assert_eq!(s.get(ed.caret..ed.anchor), Some("é"));
    }

    /// Character-count deletion stops at both text ends and never splits a character, even for
    /// counts larger than the text.
    #[test]
    fn character_deletion_is_bounded() {
        let s = "aé😀z".to_string();
        assert_eq!(prev_n_chars(&s, s.len(), 99), 0);
        assert_eq!(next_n_chars(&s, 0, 2), "aé".len());
        assert_eq!(prev_n_chars(&s, 5, 1), 1);
        assert_eq!(next_n_chars(&s, 1, 1), 3);
    }

    /// The graphics IME owner prefers stable uid identity, falls back to the layer index only
    /// without a uid, and separates sequence and replacement epochs.
    #[test]
    fn ime_owner_uses_stable_component_identity_and_explicit_replacement() {
        let a = graphics_ime_target(7, 11, 3, 42, 5);
        let b = graphics_ime_target(7, 11, 9, 42, 5);
        assert_eq!(a, b, "the stable layer uid, not the mutable layer index, is the component");
        assert_ne!(a, graphics_ime_target(7, 11, 3, 42, 6), "replacement epoch is explicit");
        assert_ne!(a, graphics_ime_target(7, 11, 3, 43, 5), "a different component is a different owner");
        assert_ne!(a, graphics_ime_target(8, 11, 3, 42, 5), "sequence identity is part of the owner");
        let no_uid = graphics_ime_target(7, 11, 3, 0, 5);
        assert_ne!(no_uid, graphics_ime_target(7, 11, 4, 0, 5), "without a layer uid, the index is the identity");
    }

    /// A pointer-down starts a new target, so later Text, Paste and IME events in the same raw
    /// batch must not be applied to the layer that was being edited before the click. The old loop
    /// cleared `custom_mutations` here; the extracted helper once fell through to `Applied` and
    /// typed every later event into the old layer.
    #[test]
    fn pointer_press_rejects_text_paste_and_ime_later_in_the_same_raw_batch() {
        use filmcraft_geom::{Affine, Vec2};
        use filmcraft_project::ClipId;
        use filmcraft_project::graphic::{Appearance, LayerContent, LayerSpec, LayerTransform, TextProps};

        let mut app = crate::FilmcraftApp::new(filmcraft_engine::Session::default());
        let ed = GfxEdit { clip: 1, layer: 0, caret: 3, anchor: 3, ..Default::default() };
        app.ui.gfx_edit = Some(ed.clone());
        let props = TextProps {
            text: "old".into(),
            font: String::new(),
            style: String::new(),
            size: 72.0,
            align: 0,
            tracking: 0.0,
            kerning: true,
            ligatures: true,
            leading: 0.0,
            baseline_shift: 0.0,
            faux_bold: false,
            faux_italic: false,
            caps: 0,
            underline: false,
            box_width: 0.0,
            box_height: 0.0,
            vertical: false,
            runs: Vec::new(),
        };
        let view = super::LayerView {
            clip: ClipId(1),
            layer: 0,
            spec: LayerSpec {
                name: "Text".into(),
                enabled: true,
                uid: 0,
                pin: None,
                content: LayerContent::Text(props),
                appearance: Appearance { fill: None, gradient: None, strokes: Vec::new(), background: None, shadow: None },
                transform: LayerTransform { position: Vec2::ZERO, anchor: Vec2::ZERO, scale: Vec2::new(1.0, 1.0), rotation: 0.0, opacity: 1.0 },
            },
            to_screen: Affine::IDENTITY,
            canvas_to_screen: Affine::IDENTITY,
            local: [-50.0, -25.0, 50.0, 25.0],
        };

        let ctx = egui::Context::default();
        let raw = egui::RawInput {
            focused: true,
            events: vec![
                egui::Event::PointerButton {
                    pos: egui::pos2(20.0, 20.0),
                    button: egui::PointerButton::Primary,
                    pressed: true,
                    modifiers: egui::Modifiers::NONE,
                },
                egui::Event::Text("typed".into()),
                egui::Event::Paste("pasted".into()),
                egui::Event::Ime(egui::ImeEvent::Commit("committed".into())),
            ],
            ..Default::default()
        };
        let mut focused = false;
        let mut out = ctx.run_ui(raw, |ui| {
            let response = ui.allocate_response(egui::vec2(100.0, 100.0), egui::Sense::click());
            super::text_edit(&mut app, ui, &view, ed.clone(), &response);
            focused = ui.memory(|m| m.has_focus(egui::Id::unique(super::TEXT_EDIT_ID)));
        });
        out.textures_delta.clear();

        assert!(focused, "the text editor must own input for this batch");
        // the session has no such clip: a `graphics.setText` would have failed into the status
        assert!(app.ui.status.is_empty(), "the pointer press must stop Text, Paste and IME editing the old layer: {}", app.ui.status);
    }
}
