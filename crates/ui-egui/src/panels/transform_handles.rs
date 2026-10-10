//! On-screen transform handles on the Program monitor (#639). Selecting Motion in Effect Controls
//! (its header or one of its properties), or double-clicking a clip's picture in the Program
//! monitor, draws a box around the clip's picture with eight handles and the anchor point. The
//! Transform effect gets the same box for its own properties while it is the selected effect.
//!
//! Dragging inside the box changes Position. A handle changes Scale about the anchor point; with
//! Uniform Scale off a side handle changes only the width and a top or bottom handle only the
//! height. Dragging just outside the box changes Rotation. The anchor point moves without moving
//! the picture: Position changes by the same amount. The values go through `effects.setParam` at
//! the playhead while the drag goes on, as dragging them in Effect Controls does, so a property
//! with its stopwatch on gets a keyframe there, and one drag is one undo step.
//!
//! Automation ids: `program.transform.box`, `program.transform.handle.<n>` (0–3 the corners TL,
//! TR, BR, BL, 4–7 the middles of the top, right, bottom and left edges, as for graphic layers),
//! `program.transform.rotate.<n>` (just outside corner `n`), `program.transform.anchor`.

use egui::{Color32, CursorIcon, Pos2, Rect, Sense, Stroke, StrokeKind, pos2, vec2};
use filmcraft_geom::{Affine, Vec2};
use filmcraft_project::{ClipId, EffectDef, EffectInstance, TrackItem};
use filmcraft_time::Tick;
use serde_json::{Value, json};

use crate::FilmcraftApp;
use crate::panels::graphics::{frame_to_screen, handle_points, unscale};
use crate::state::Tool;

/// How close (in points) the pointer must be to a handle or the anchor point to grab it: well
/// beyond the drawn squares, so the small handles stay easy to take hold of.
const GRAB: f32 = 8.0;
/// The drawn handles, a little smaller than the graphic layers' (7 and 5 points) for precision:
/// the side of a corner square and of an edge square.
const CORNER: f32 = 5.0;
const EDGE: f32 = 4.0;
/// The anchor point, Premiere's ⊕: a circle of this radius with the cross inside it, in thin lines
/// whose crossing marks the exact point; and how close the pointer must be to take hold of it.
const ANCHOR_RADIUS: f32 = 7.0;
const ANCHOR_ARM: f32 = ANCHOR_RADIUS;
const ANCHOR_GRAB: f32 = 11.0;
/// The small anchor point beside the pointer while it is over the anchor point.
const BADGE_RADIUS: f32 = 3.0;
const BADGE_ARM: f32 = 5.0;
/// How far outside the box (from its edges and corners) the pointer rotates the picture.
const ROTATE_REACH: f32 = 24.0;
/// The `merge` key of a drag's changes: one undo step, whichever parameters the drag changes.
const MERGE: &str = "programTransform";

/// What a press on the box takes hold of.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Grab {
    Move,
    /// Handle `n` (see [`handle_points`]).
    Handle(usize),
    Rotate,
    Anchor,
}

/// The selected effect's box on the monitor and the values it is drawn from.
#[derive(Clone, Copy, Debug)]
struct Placement {
    /// Where the effect's Position is measured → screen: the sequence frame for Motion, the clip
    /// (through Motion) for Transform.
    outer: Affine,
    /// Clip pixels → screen.
    to_screen: Affine,
    /// The picture's box: TL, TR, BR, BL.
    quad: [Pos2; 4],
    anchor_at: Pos2,
    position: Vec2,
    anchor: Vec2,
    /// Scale (Motion) or Scale Height (Transform), and Scale Width, in percent.
    height: f64,
    width: f64,
    rotation: f64,
    uniform: bool,
}

/// A clip and the index of one of its effects.
type Target = (ClipId, usize);

#[derive(Clone, Copy, Debug)]
struct Drag {
    sel: Target,
    grab: Grab,
    /// Where the button went down, and the box then: every frame of the drag sets the values the
    /// pointer's travel from there gives.
    start: Pos2,
    from: Placement,
    /// Rotate: the pointer's angle about the anchor point last frame and the turn so far, in
    /// degrees (a drag can go round more than once).
    angle: f64,
    turned: f64,
}

/// Scale's parameter: Motion's `scale` is the height when Uniform Scale is off.
fn height_param(effect: &str) -> &'static str {
    if effect == "transform" { "scale_height" } else { "scale" }
}

fn sp(m: &Affine, v: Vec2) -> Pos2 {
    let q = m.apply(v);
    pos2(q.x as f32, q.y as f32)
}

/// The box of a clip `size` pixels large that `m` places on the screen.
fn quad_of(m: &Affine, size: (u32, u32)) -> [Pos2; 4] {
    let (w, h) = (f64::from(size.0), f64::from(size.1));
    [(0.0, 0.0), (w, 0.0), (w, h), (0.0, h)].map(|(x, y)| sp(m, Vec2::new(x, y)))
}

fn inside(quad: &[Pos2; 4], p: Pos2) -> bool {
    let sides = quad.iter().zip(quad.iter().cycle().skip(1)).map(|(a, b)| (*b - *a).x * (p - *a).y - (*b - *a).y * (p - *a).x);
    let (mut pos, mut neg) = (false, false);
    for s in sides {
        pos |= s > 0.0;
        neg |= s < 0.0;
    }
    pos != neg
}

/// The box of effect `e` (Motion or Transform) of clip `it` at media time `mt`, on the monitor
/// picture `pic` showing a `frame`-sized sequence.
fn placement(app: &FilmcraftApp, it: &TrackItem, e: &EffectInstance, mt: Tick, pic: Rect, frame: (u32, u32)) -> Option<Placement> {
    let seq = app.session.active_sequence()?;
    let project = &app.session.project;
    let size = filmcraft_render::source_size(project, it.item).unwrap_or(frame);
    let view = frame_to_screen(pic, frame);
    let motion = view.then_apply(&filmcraft_render::motion_matrix(seq, it, size, filmcraft_render::source_par(project, it.item), mt));
    // "auto" points (NaN) are where the render puts them
    let point = |id: &str, auto: Vec2| {
        let v = e.param(id).map_or(auto, |p| p.vec2_at(mt));
        Vec2::new(if v.x.is_nan() { auto.x } else { v.x }, if v.y.is_nan() { auto.y } else { v.y })
    };
    let centre = Vec2::new(f64::from(size.0) / 2.0, f64::from(size.1) / 2.0);
    let uniform = e.param("uniform_scale").and_then(|p| p.value.as_bool()).unwrap_or(true);
    let (height, width, rotation) = (e.f64_at(height_param(&e.effect), mt), e.f64_at("scale_width", mt), e.f64_at("rotation", mt));
    let (outer, to_screen, position, anchor) = if e.effect == "transform" {
        let (anchor, position) = (point("anchor", centre), point("position", centre));
        let (sh, sw) = (height / 100.0, if uniform { height } else { width } / 100.0);
        // as the render places it (`filmcraft_render::gpufx`, "transform")
        let axis = e.f64_at("skew_axis", mt);
        let shear = Affine { c: e.f64_at("skew", mt).to_radians().tan(), ..Affine::IDENTITY };
        let skew = Affine::rotate_deg(axis).then_apply(&shear).then_apply(&Affine::rotate_deg(-axis));
        let own = Affine::translate(position.x, position.y)
            .then_apply(&Affine::rotate_deg(rotation))
            .then_apply(&skew)
            .then_apply(&Affine::scale(sw, sh))
            .then_apply(&Affine::translate(-anchor.x, -anchor.y));
        (motion, motion.then_apply(&own), position, anchor)
    } else {
        let frame_centre = Vec2::new(f64::from(seq.settings.width) / 2.0, f64::from(seq.settings.height) / 2.0);
        (view, motion, point("position", frame_centre), point("anchor", centre))
    };
    let quad = quad_of(&to_screen, size);
    let anchor_at = sp(&to_screen, anchor);
    quad.iter().chain([&anchor_at]).all(|p| p.x.is_finite() && p.y.is_finite()).then_some(Placement {
        outer,
        to_screen,
        quad,
        anchor_at,
        position,
        anchor,
        height,
        width,
        rotation,
        uniform,
    })
}

/// The selected effect (its clip and index) and its box, while it is Motion or Transform of a
/// selected clip under the playhead.
fn selected_placement(app: &FilmcraftApp, pic: Rect, frame: (u32, u32)) -> Option<(Target, &'static EffectDef, Placement)> {
    let sel = app.session.state.selected_effect.as_ref()?;
    if !app.session.state.selection.contains(&sel.clip) {
        return None;
    }
    let seq = app.session.active_sequence()?;
    let ph = app.session.playhead();
    let (_, it) = seq.find_item(sel.clip)?;
    if !it.range().contains(ph) {
        return None;
    }
    let index = sel.index(&it.effects)?;
    let e = it.effects.get(index).filter(|e| e.enabled)?;
    let def = e.def().filter(|d| d.id == "motion" || d.id == "transform")?;
    let pl = placement(app, it, e, it.source_time_at(ph), pic, frame)?;
    Some(((sel.clip, index), def, pl))
}

fn grab_at(pl: &Placement, p: Pos2) -> Option<Grab> {
    if (pl.anchor_at - p).length() <= ANCHOR_GRAB {
        return Some(Grab::Anchor);
    }
    if let Some(n) = handle_points(&pl.quad).iter().position(|h| (*h - p).length() <= GRAB) {
        return Some(Grab::Handle(n));
    }
    if inside(&pl.quad, p) {
        return Some(Grab::Move);
    }
    // anywhere just outside the box rotates, beside an edge as beside a corner, as in Premiere
    let n = pl.quad.len();
    let near = (0..n).filter_map(|i| Some(seg_dist(p, *pl.quad.get(i)?, *pl.quad.get((i + 1) % n)?))).any(|d| d <= ROTATE_REACH);
    near.then_some(Grab::Rotate)
}

/// The distance from `p` to the segment `a`–`b`.
fn seg_dist(p: Pos2, a: Pos2, b: Pos2) -> f32 {
    let ab = b - a;
    let l2 = ab.length_sq();
    let t = if l2 > 0.0 { ((p - a).dot(ab) / l2).clamp(0.0, 1.0) } else { 0.0 };
    (p - (a + ab * t)).length()
}

/// The scale factors (width, height) dragging handle `n` from `start` to `cur` gives: the
/// pointer's distance from the anchor point along the box's sides, relative to where it started.
fn scale_factors(pl: &Placement, n: usize, start: Pos2, cur: Pos2) -> (f64, f64) {
    let [tl, tr, _, bl] = pl.quad;
    let (ux, uy) = ((tr - tl).normalized(), (bl - tl).normalized());
    let (from, to) = (start - pl.anchor_at, cur - pl.anchor_at);
    let along = |dir: egui::Vec2| {
        let a = from.dot(dir);
        if a.abs() < 1.0 { 1.0 } else { f64::from((to.dot(dir) / a).max(0.0)) }
    };
    match (n, pl.uniform) {
        (0..=3, true) => {
            let f = along(from.normalized());
            (f, f)
        }
        (0..=3, false) => (along(ux), along(uy)),
        (4 | 6, true) => (along(uy), along(uy)),
        (4 | 6, false) => (1.0, along(uy)),
        (_, true) => (along(ux), along(ux)),
        (_, false) => (along(ux), 1.0),
    }
}

/// The parameter values a drag sets with the pointer at `cur`.
fn changes(d: &Drag, effect: &str, cur: Pos2) -> Vec<(&'static str, Value)> {
    let pl = &d.from;
    let moved = cur - d.start;
    // the ranges Effect Controls' fields take
    let point = |v: Vec2| (v.x.is_finite() && v.y.is_finite()).then(|| json!([v.x.clamp(-100_000.0, 100_000.0), v.y.clamp(-100_000.0, 100_000.0)]));
    let number = |v: f64, max: f64| v.is_finite().then(|| json!(v.clamp(-max, max)));
    let scale = |v: f64| v.is_finite().then(|| json!(v.clamp(0.0, 10_000.0)));
    let out = match d.grab {
        Grab::Move => vec![("position", point(pl.position + unscale(&pl.outer, moved)))],
        Grab::Anchor => vec![("anchor", point(pl.anchor + unscale(&pl.to_screen, moved))), ("position", point(pl.position + unscale(&pl.outer, moved)))],
        Grab::Handle(n) => {
            let (fx, fy) = scale_factors(pl, n, d.start, cur);
            let (height, width) = (height_param(effect), "scale_width");
            match n {
                _ if pl.uniform => vec![(height, scale(pl.height * fy))],
                4 | 6 => vec![(height, scale(pl.height * fy))],
                5 | 7 => vec![(width, scale(pl.width * fx))],
                _ => vec![(height, scale(pl.height * fy)), (width, scale(pl.width * fx))],
            }
        }
        Grab::Rotate => vec![("rotation", number(pl.rotation + d.turned, 36_000.0))],
    };
    out.into_iter().filter_map(|(param, v)| Some((param, v?))).collect()
}

/// The resize cursor for a handle in direction `v` from the box's centre.
fn resize_cursor(v: egui::Vec2) -> CursorIcon {
    let deg = v.y.atan2(v.x).to_degrees().rem_euclid(180.0);
    if !(22.5..157.5).contains(&deg) {
        CursorIcon::ResizeHorizontal
    } else if deg < 67.5 {
        CursorIcon::ResizeNwSe
    } else if deg < 112.5 {
        CursorIcon::ResizeVertical
    } else {
        CursorIcon::ResizeNeSw
    }
}

fn centre(quad: &[Pos2; 4]) -> Pos2 {
    let [tl, _, br, _] = *quad;
    tl + (br - tl) * 0.5
}

fn angle_deg(v: egui::Vec2) -> f64 {
    f64::from(v.y.atan2(v.x).to_degrees())
}

fn draw(app: &mut FilmcraftApp, ui: &egui::Ui, pl: &Placement, pic: Rect, name: &str) {
    let accent = app.tokens.accent;
    let painter = ui.painter().with_clip_rect(pic.expand(12.0));
    painter.add(egui::Shape::closed_line(pl.quad.to_vec(), Stroke::new(1.0, accent)));
    app.auto.add("program.transform.box", Rect::from_points(&pl.quad), name);
    let mid = centre(&pl.quad);
    // automation rects: the square inside the circle a press grabs in
    let grab = vec2(GRAB, GRAB) * std::f32::consts::SQRT_2;
    for (n, c) in pl.quad.iter().enumerate() {
        let hr = Rect::from_center_size(*c, vec2(CORNER, CORNER));
        painter.rect_filled(hr, 0.0, Color32::WHITE);
        painter.rect_stroke(hr, 0.0, Stroke::new(1.0, accent), StrokeKind::Middle);
        app.auto.add(&format!("program.transform.handle.{n}"), Rect::from_center_size(*c, grab), "scale handle");
        // just outside the corner, away from the box's centre
        let out = (*c - mid).normalized();
        app.auto.add(&format!("program.transform.rotate.{n}"), Rect::from_center_size(*c + out * 14.0, vec2(10.0, 10.0)), "rotate");
    }
    for (n, m) in handle_points(&pl.quad).iter().enumerate().skip(4) {
        painter.rect_filled(Rect::from_center_size(*m, vec2(EDGE, EDGE)), 0.0, Color32::WHITE);
        app.auto.add(&format!("program.transform.handle.{n}"), Rect::from_center_size(*m, grab), "scale handle");
    }
    let a = pl.anchor_at;
    painter.circle_stroke(a, ANCHOR_RADIUS, Stroke::new(1.0, accent));
    painter.line_segment([a - vec2(ANCHOR_ARM, 0.0), a + vec2(ANCHOR_ARM, 0.0)], Stroke::new(1.0, accent));
    painter.line_segment([a - vec2(0.0, ANCHOR_ARM), a + vec2(0.0, ANCHOR_ARM)], Stroke::new(1.0, accent));
    app.auto.add("program.transform.anchor", Rect::from_center_size(a, vec2(ANCHOR_GRAB, ANCHOR_GRAB) * std::f32::consts::SQRT_2), "anchor point");
}

/// The pointer for what it would take hold of at `at` (or has hold of). There is no system
/// cursor for rotating, so a code-drawn one replaces it there.
fn cursor(ui: &egui::Ui, pl: &Placement, grab: Grab, at: Pos2, pic: Rect) {
    let icon = match grab {
        Grab::Move => CursorIcon::Move,
        // the plain arrow with a small anchor point beside it, as Premiere shows, so it's clear the
        // anchor point is under the pointer and not the picture
        Grab::Anchor => {
            let painter = ui.painter().with_clip_rect(pic.expand(24.0));
            let b = at + vec2(14.0, 16.0);
            for (col, w) in [(Color32::BLACK, 3.0), (Color32::WHITE, 1.0)] {
                painter.circle_stroke(b, BADGE_RADIUS, Stroke::new(w, col));
                painter.line_segment([b - vec2(BADGE_ARM, 0.0), b + vec2(BADGE_ARM, 0.0)], Stroke::new(w, col));
                painter.line_segment([b - vec2(0.0, BADGE_ARM), b + vec2(0.0, BADGE_ARM)], Stroke::new(w, col));
            }
            CursorIcon::Default
        }
        Grab::Handle(n) => handle_points(&pl.quad).get(n).map_or(CursorIcon::Move, |h| resize_cursor(*h - centre(&pl.quad))),
        Grab::Rotate => {
            let out = (at - centre(&pl.quad)).normalized();
            rotate_pointer(&ui.painter().with_clip_rect(pic.expand(ROTATE_REACH + 16.0)), at, if out.is_finite() { out } else { vec2(0.0, -1.0) });
            CursorIcon::None
        }
    };
    ui.ctx().set_cursor_icon(icon);
}

/// Premiere's rotate pointer: a curved double arrow at `at`, bulging along `out` (away from the
/// box), so it turns to face outward wherever the pointer is around the box.
fn rotate_pointer(painter: &egui::Painter, at: Pos2, out: egui::Vec2) {
    const R: f32 = 9.0;
    const SWEEP: f32 = 1.6;
    let c = at - out * R;
    let base = out.y.atan2(out.x);
    let on_arc = |a: f32| c + vec2(a.cos(), a.sin()) * R;
    let arc: Vec<Pos2> = (0..=12).map(|i| on_arc(base + (i as f32 / 12.0 - 0.5) * SWEEP)).collect();
    // an arrowhead at each end, pointing on along the arc
    let head = |a: f32, dir: f32| {
        let (tip, along, across) = (on_arc(a), vec2(-a.sin(), a.cos()) * dir, vec2(a.cos(), a.sin()));
        vec![tip + along * 4.5, tip - across * 3.5, tip + across * 3.5]
    };
    let heads = [head(base + SWEEP / 2.0, 1.0), head(base - SWEEP / 2.0, -1.0)];
    painter.add(egui::Shape::line(arc.clone(), Stroke::new(3.5, Color32::BLACK)));
    painter.add(egui::Shape::line(arc, Stroke::new(1.5, Color32::WHITE)));
    for h in heads {
        painter.add(egui::Shape::convex_polygon(h, Color32::WHITE, Stroke::new(1.0, Color32::BLACK)));
    }
}

/// The top-most clip at the playhead whose picture is under `p`.
fn clip_under(app: &FilmcraftApp, pic: Rect, frame: (u32, u32), p: Pos2) -> Option<ClipId> {
    let seq = app.session.active_sequence()?;
    let t = app.session.playhead();
    let view = frame_to_screen(pic, frame);
    let project = &app.session.project;
    seq.video_tracks.iter().rev().filter(|tr| tr.enabled).filter_map(|tr| tr.item_at(t)).filter(|it| it.enabled).find_map(|it| {
        let size = filmcraft_render::source_size(project, it.item).unwrap_or(frame);
        let m = view.then_apply(&filmcraft_render::motion_matrix(seq, it, size, filmcraft_render::source_par(project, it.item), it.source_time_at(t)));
        inside(&quad_of(&m, size), p).then_some(it.id)
    })
}

/// The selected effect's box on the Program monitor picture `pic`, and double-clicking a clip's
/// picture to select its Motion.
pub fn monitor_overlay(app: &mut FilmcraftApp, ui: &mut egui::Ui, pic: Rect, frame: (u32, u32)) {
    if app.ui.tool != Tool::Selection || app.ui.mask_pen.is_some() {
        return;
    }
    let mut actions: Vec<(String, Value)> = Vec::new();
    let id = egui::Id::new("program-transform");
    let drag_id = id.with("drag");
    let double = ui.input(|i| i.pointer.button_double_clicked(egui::PointerButton::Primary));
    if let Some((sel, def, pl)) = selected_placement(app, pic, frame) {
        draw(app, ui, &pl, pic, def.name);
        // (as far again beyond the rotate zone, where a click lets go of the box)
        let resp = ui.interact(Rect::from_points(&pl.quad).expand(ROTATE_REACH * 2.0), id, Sense::click_and_drag());
        // the left button only: a middle or right drag leaves the picture alone
        let left = egui::PointerButton::Primary;
        if resp.drag_started_by(left) {
            // where the button went down (a drag starts only after the pointer has moved a little)
            let p = ui.input(|i| i.pointer.press_origin()).or(resp.interact_pointer_pos()).unwrap_or(pl.anchor_at);
            match grab_at(&pl, p) {
                Some(grab) => {
                    let angle = angle_deg(p - pl.anchor_at);
                    ui.data_mut(|d| d.insert_temp(drag_id, Drag { sel, grab, start: p, from: pl, angle, turned: 0.0 }));
                }
                None => ui.data_mut(|d| d.remove::<Drag>(drag_id)),
            }
        }
        let drag: Option<Drag> = ui.data(|d| d.get_temp(drag_id)).filter(|d: &Drag| d.sel == sel);
        if let Some(mut d) = drag
            && resp.dragged_by(left)
        {
            let cur = resp.interact_pointer_pos().unwrap_or(d.start);
            let off = cur - d.from.anchor_at;
            if d.grab == Grab::Rotate && off.length() >= 1.0 {
                let a = angle_deg(off);
                d.turned += (a - d.angle + 180.0).rem_euclid(360.0) - 180.0;
                d.angle = a;
                ui.data_mut(|m| m.insert_temp(drag_id, d));
            }
            if resp.drag_started_by(left) || resp.drag_delta() != egui::Vec2::ZERO {
                for (param, value) in changes(&d, def.id, cur) {
                    actions.push(("effects.setParam".into(), json!({"clip": sel.0.0, "effect": sel.1, "param": param, "value": value, "merge": MERGE})));
                }
            }
            cursor(ui, &pl, d.grab, cur, pic);
        } else if let Some(p) = resp.hover_pos()
            && let Some(grab) = grab_at(&pl, p)
        {
            cursor(ui, &pl, grab, p, pic);
        }
        if resp.drag_stopped() {
            ui.data_mut(|d| d.remove::<Drag>(drag_id));
        }
        // a click beside the box (not on it, a handle or a corner's rotate zone) lets go of it
        if resp.clicked() && !double && resp.interact_pointer_pos().is_some_and(|p| grab_at(&pl, p).is_none()) {
            actions.push(("effects.select".into(), json!({"none": true})));
        }
    }
    if double
        && app.ui.gfx_edit.is_none()
        && let Some(p) = ui.input(|i| i.pointer.interact_pos()).filter(|p| pic.contains(*p) && ui.rect_contains_pointer(pic))
        && let Some(clip) = clip_under(app, pic, frame, p)
    {
        if !app.session.state.selection.contains(&clip) {
            actions.push(("timeline.select".into(), json!({"clips": [clip.0]})));
        }
        actions.push(("effects.select".into(), json!({"clip": clip.0, "effect": "motion"})));
    }
    crate::panels::effect_controls::run(app, ui.ctx(), actions);
}
