//! Effect masks in the UI: the mask rows of Effect Controls (create ellipse / 4-point polygon /
//! pen masks, per-mask Path / Feather / Opacity / Expansion / Inverted / mode) and on-monitor
//! editing on the Program monitor (drag vertices, Bézier handles, the whole mask, feather,
//! expansion and corner-roundness handles; the pen places a free-draw Bézier mask).
//!
//! Automation ids: `effectControls.<effect>.mask.{ellipse,polygon,pen}`,
//! `effectControls.<effect>.mask<k>` (+ `.mode`, `.inverted`, `.delete`, and the parameter rows
//! `effectControls.<effect>.mask<k>.<param>.*`), `program.mask.vertex.<i>`, `program.mask.in.<i>`,
//! `program.mask.out.<i>`, `program.mask.body`, `program.mask.feather`, `program.mask.expansion`,
//! `program.mask.roundness`, `program.maskPen`.

use egui::{Align2, Color32, Pos2, Rect, Sense, Stroke, pos2, vec2};
use filmcraft_geom::{Affine, Vec2};
use filmcraft_project::{ClipId, EffectInstance, MaskMode, MaskPath, MaskVertex, TrackItem};
use filmcraft_time::Tick;
use serde_json::{Value, json};

use crate::FilmcraftApp;
use crate::icons::{self, Icon};
use crate::theme::Tokens;

const ROW_H: f32 = 22.0;

/// Whether an effect instance takes masks (video effects and Opacity; not Motion or graphic layers).
pub fn maskable(e: &EffectInstance) -> bool {
    let Some(d) = e.def() else { return false };
    d.kind == filmcraft_project::EffectKind::Video && (!d.intrinsic || e.effect == "opacity") && !filmcraft_project::graphic::is_layer(e)
}

/// Mask rows under an effect in Effect Controls.
#[allow(clippy::too_many_arguments)]
pub(crate) fn effect_rows(
    app: &mut FilmcraftApp,
    ui: &mut egui::Ui,
    body: Rect,
    clip: ClipId,
    idx: usize,
    e: &EffectInstance,
    mt: Tick,
    actions: &mut Vec<(String, Value)>,
    lane: &Rect,
    lx: &dyn Fn(Tick) -> f32,
    it: &TrackItem,
    keys: &mut crate::panels::effect_controls::Interaction,
) {
    let t = app.tokens;
    // creation tools: ellipse, 4-point polygon, free-draw Bézier
    let (r, _) = ui.allocate_exact_size(vec2(body.width(), ROW_H), Sense::hover());
    crate::panels::effect_controls::row_line(ui, r, lane, &t);
    let pen_on = app.ui.mask_pen.as_ref().is_some_and(|p| p.clip == clip.0 && p.effect == idx);
    for (i, (icon, kind, tip)) in [
        (Icon::Ellipse, "ellipse", tl!("Create ellipse mask")),
        (Icon::Rectangle, "polygon", tl!("Create 4-point polygon mask")),
        (Icon::Pen, "pen", tl!("Free draw bezier")),
    ]
    .into_iter()
    .enumerate()
    {
        let br = Rect::from_center_size(pos2(r.min.x + 40.0 + i as f32 * 24.0, r.center().y), vec2(20.0, 18.0));
        let resp = ui.interact(br, egui::Id::new(("mask-new", clip.0, idx, kind)), Sense::click()).on_hover_text(tip);
        if resp.hovered() {
            ui.painter().rect_filled(br, 3.0, t.hover);
        }
        let on = kind == "pen" && pen_on;
        icons::paint(ui.painter(), br.shrink(3.0), icon, if on { t.accent } else { t.icon });
        app.auto.add(&format!("effectControls.{}.mask.{kind}", e.effect), br, tip);
        if resp.clicked() {
            if kind == "pen" {
                app.ui.mask_pen = if on { None } else { Some(crate::state::MaskPenDraft { clip: clip.0, effect: idx, points: vec![] }) };
                if !on {
                    app.ui.status =
                        tl!("Click in the Program monitor to place mask points (drag for curves); click the first point or press Enter to close").into();
                }
            } else {
                actions.push(("masks.add".into(), json!({"clip": clip.0, "effect": idx, "shape": kind})));
            }
        }
    }
    let defs = filmcraft_project::mask::mask_param_defs();
    for (k, m) in e.masks.iter().enumerate() {
        let key = format!("mask:{}:{}:{}", clip.0, idx, k);
        let open = !app.ui.collapsed_fx.contains(&key);
        let selected = app.session.state.selected_mask == Some(filmcraft_engine::masks::MaskSel { clip, effect: idx, mask: k });
        let (r, resp) = ui.allocate_exact_size(vec2(body.width(), ROW_H), Sense::click());
        if selected {
            ui.painter().rect_filled(crate::panels::effect_controls::row_fill(r), 0.0, t.row_selected);
        } else if resp.hovered() {
            ui.painter().rect_filled(crate::panels::effect_controls::row_fill(r), 0.0, t.hover);
        }
        crate::panels::effect_controls::row_line(ui, r, lane, &t);
        let tw = Rect::from_center_size(pos2(r.min.x + 26.0, r.center().y), vec2(10.0, 10.0));
        icons::paint(ui.painter(), tw, if open { Icon::ChevronDown } else { Icon::ChevronRight }, t.text_dim);
        let twresp = ui.interact(tw.expand(3.0), egui::Id::new(("mask-twirl", clip.0, idx, k)), Sense::click());
        // shape glyph: ellipse for smooth paths, square for polygons
        let path = m.path_at(mt);
        let glyph = Rect::from_center_size(pos2(r.min.x + 42.0, r.center().y), vec2(12.0, 12.0));
        let gi = if path.vertices.iter().all(|v| v.is_corner()) {
            Icon::Rectangle
        } else if path.len() == 4 {
            Icon::Ellipse
        } else {
            Icon::Pen
        };
        icons::paint(ui.painter(), glyph, gi, if selected { t.accent } else { t.text_dim });
        // (the mode dropdown starts 118 points from the row's right end)
        crate::panels::effect_controls::row_label(
            ui,
            pos2(r.min.x + 54.0, r.center().y),
            &m.name,
            r.max.x - 122.0 - (r.min.x + 54.0),
            Tokens::ui(12.0),
            t.text,
        );
        let base = format!("effectControls.{}.mask{k}", e.effect);
        app.auto.add(&base, r, &m.name);
        // mode
        let mr = Rect::from_min_size(pos2(r.max.x - 118.0, r.min.y + 2.0), vec2(96.0, ROW_H - 4.0));
        let mut mui = ui.new_child(egui::UiBuilder::new().max_rect(mr).layout(egui::Layout::left_to_right(egui::Align::Center)));
        let mut mode = m.mode;
        crate::panels::effect_controls::fit_to_row(&mut mui);
        egui::ComboBox::from_id_salt(("mask-mode", clip.0, idx, k)).selected_text(crate::i18n::t(mode.label())).width(88.0).show_ui(&mut mui, |ui| {
            for md in MaskMode::ALL {
                if ui.selectable_value(&mut mode, md, crate::i18n::t(md.label())).changed() {
                    actions.push(("masks.set".into(), json!({"clip": clip.0, "effect": idx, "mask": k, "mode": md.label()})));
                }
            }
        });
        app.auto.add(&format!("{base}.mode"), mr, "Mask mode");
        if twresp.clicked() {
            if open {
                app.ui.collapsed_fx.push(key.clone());
            } else {
                app.ui.collapsed_fx.retain(|x| *x != key);
            }
        } else if resp.clicked() {
            if selected {
                actions.push(("masks.select".into(), json!({"none": true})));
            } else {
                actions.push(("masks.select".into(), json!({"clip": clip.0, "effect": idx, "mask": k})));
            }
        }
        resp.context_menu(|ui| {
            if ui.button(tl!("Delete Mask")).clicked() {
                actions.push(("masks.remove".into(), json!({"clip": clip.0, "effect": idx, "mask": k})));
                ui.close();
            }
        });
        if !open {
            continue;
        }
        for pd in defs {
            crate::panels::effect_controls::param_row(app, ui, body, clip, idx, e, Some(k), pd, mt, actions, lane, lx, it, keys);
            if pd.id != "path"
                && app.ui.expanded_fx.contains(&crate::panels::effect_controls::graph_key(clip, idx, &format!("mask{k}.{}", pd.id)))
                && let Some(param) = m.param(pd.id)
                && param.is_animated()
            {
                crate::panels::effect_controls::graph_rows(app, ui, body, clip, idx, Some(k), pd, param, lane, it, keys, actions);
            }
        }
        // Inverted
        let (r, _) = ui.allocate_exact_size(vec2(body.width(), ROW_H), Sense::hover());
        crate::panels::effect_controls::row_line(ui, r, lane, &t);
        ui.painter().text(pos2(r.min.x + 40.0, r.center().y), Align2::LEFT_CENTER, tl!("Inverted"), Tokens::ui(12.0), t.text);
        let cr = Rect::from_min_size(pos2(r.min.x + (r.width() * 0.5).max(150.0), r.min.y + 2.0), vec2(20.0, ROW_H - 4.0));
        let mut cui = ui.new_child(egui::UiBuilder::new().max_rect(cr).layout(egui::Layout::left_to_right(egui::Align::Center)));
        crate::panels::effect_controls::fit_to_row(&mut cui);
        let mut inv = m.inverted;
        if cui.checkbox(&mut inv, "").changed() {
            actions.push(("masks.set".into(), json!({"clip": clip.0, "effect": idx, "mask": k, "inverted": inv})));
        }
        app.auto.add(&format!("{base}.inverted"), cr, "Inverted");
    }
}

/// Only the left button drags on the monitor; a middle or right drag leaves the mask alone.
const LEFT: egui::PointerButton = egui::PointerButton::Primary;

/// Clip pixels → screen points for the clip's current Motion and the monitor picture rect.
fn clip_to_screen(app: &FilmcraftApp, it: &TrackItem, mt: Tick, pic: Rect, frame: (u32, u32)) -> Option<Affine> {
    let seq = app.session.active_sequence()?;
    let size = filmcraft_render::source_size(&app.session.project, it.item).unwrap_or(frame);
    let motion = filmcraft_render::motion_matrix(seq, it, size, filmcraft_render::source_par(&app.session.project, it.item), mt);
    let view = Affine::translate(pic.min.x as f64, pic.min.y as f64)
        .then_apply(&Affine::scale(pic.width() as f64 / frame.0 as f64, pic.height() as f64 / frame.1 as f64));
    Some(view.then_apply(&motion))
}

fn sp(m: &Affine, v: Vec2) -> Pos2 {
    let q = m.apply(v);
    pos2(q.x as f32, q.y as f32)
}

/// Outward offset of a flattened (clip-space) polygon by `d` along averaged vertex normals.
fn offset(pts: &[Vec2], d: f64) -> Vec<Vec2> {
    let n = pts.len();
    if n < 3 || d == 0.0 {
        return pts.to_vec();
    }
    // orientation: positive shoelace area in y-down = clockwise on screen
    let area: f64 = (0..n).map(|i| pts[i].x * pts[(i + 1) % n].y - pts[(i + 1) % n].x * pts[i].y).sum();
    let s = if area > 0.0 { 1.0 } else { -1.0 };
    (0..n)
        .map(|i| {
            let a = pts[(i + n - 1) % n];
            let b = pts[(i + 1) % n];
            let t = b - a;
            let len = t.length().max(1e-9);
            // right-hand normal of a clockwise polygon points outward
            let nrm = Vec2::new(t.y / len, -t.x / len) * s;
            pts[i] + nrm * d
        })
        .collect()
}

fn inside(pts: &[Vec2], p: Vec2) -> bool {
    let n = pts.len();
    let mut w = 0;
    for i in 0..n {
        let (a, b) = (pts[i], pts[(i + 1) % n]);
        if (a.y <= p.y) != (b.y <= p.y) && a.x + (b.x - a.x) * (p.y - a.y) / (b.y - a.y) < p.x {
            w += if b.y > a.y { 1 } else { -1 };
        }
    }
    w != 0
}

/// Mask editing on the Program monitor (selected mask, while its clip is selected, as for the
/// transform box) and the pen tool.
pub fn monitor_overlay(app: &mut FilmcraftApp, ui: &mut egui::Ui, pic: Rect, frame: (u32, u32)) {
    if app.ui.mask_pen.is_some() {
        pen_overlay(app, ui, pic, frame);
        return;
    }
    let Some(sel) = app.session.state.selected_mask else { return };
    // (the mask stays selected: its handles come back with its clip, #1341)
    if !app.session.state.selection.contains(&sel.clip) {
        return;
    }
    let Some(seq) = app.session.active_sequence() else { return };
    let ph = app.session.playhead();
    let Some((_, it)) = seq.find_item(sel.clip) else { return };
    if ph < it.start || ph >= it.end() {
        return;
    }
    let it = it.clone();
    let Some(e) = it.effects.get(sel.effect) else { return };
    let Some(m) = e.masks.get(sel.mask) else { return };
    let mt = it.source_time_at(ph);
    let Some(to_screen) = clip_to_screen(app, &it, mt, pic, frame) else { return };
    let Some(to_clip) = to_screen.inverse() else { return };
    let lin_inv = Affine { e: 0.0, f: 0.0, ..to_clip };
    let path = m.path_at(mt);
    let feather = m.feather.f64_at(mt).max(0.0);
    let expansion = m.expansion.f64_at(mt);
    let t = app.tokens;
    let col = Color32::from_rgb(0x5a, 0xb4, 0xff);
    let painter = ui.painter().with_clip_rect(pic.expand(16.0));
    let flat = path.flatten(0.5);
    let screen: Vec<Pos2> = flat.iter().map(|v| sp(&to_screen, *v)).collect();
    painter.add(egui::Shape::closed_line(screen.clone(), Stroke::new(1.5, col)));
    // expansion + feather contours (dashed)
    let exp_pts = offset(&flat, expansion);
    let fea_pts = offset(&flat, expansion + feather / 2.0);
    let dashed = |pts: &[Vec2], c: Color32| {
        let mut v: Vec<Pos2> = pts.iter().map(|q| sp(&to_screen, *q)).collect();
        if let Some(f) = v.first().copied() {
            v.push(f);
        }
        painter.extend(egui::Shape::dashed_line(&v, Stroke::new(1.0, c), 4.0, 3.0));
    };
    if expansion.abs() > 1e-6 {
        dashed(&exp_pts, col.gamma_multiply(0.8));
    }
    if feather > 1e-6 {
        dashed(&fea_pts, col.gamma_multiply(0.6));
    }
    let mut actions: Vec<(String, Value)> = Vec::new();
    let target = json!({"clip": sel.clip.0, "effect": sel.effect, "mask": sel.mask});
    let with = |extra: Value| -> Value {
        let mut v = target.clone();
        if let (Some(o), Some(x)) = (v.as_object_mut(), extra.as_object()) {
            for (k, val) in x {
                o.insert(k.clone(), val.clone());
            }
        }
        v
    };
    let gesture_id = egui::Id::new("mask-gesture");
    // one gesture (undo step) per pointer press
    let mut gesture: u64 = ui.data(|d| d.get_temp(gesture_id)).unwrap_or(0);
    if ui.input(|i| i.pointer.any_pressed()) {
        gesture += 1;
        ui.data_mut(|d| d.insert_temp(gesture_id, gesture));
    }
    let clip_delta = |d: egui::Vec2| -> [f64; 2] {
        let v = lin_inv.apply(Vec2::new(d.x as f64, d.y as f64));
        [v.x, v.y]
    };
    // ---- whole mask (drag inside)
    let bb = Rect::from_points(&screen).expand(2.0).intersect(pic);
    let body = ui.interact(bb, egui::Id::new("mask-body"), Sense::click_and_drag());
    app.auto.add("program.mask.body", bb, &m.name);
    let ptr_in = |p: Option<Pos2>| p.is_some_and(|p| inside(&flat, to_clip.apply(Vec2::new(p.x as f64, p.y as f64))));
    if body.drag_started_by(LEFT) {
        let ok = ptr_in(body.interact_pointer_pos());
        ui.data_mut(|d| d.insert_temp(gesture_id.with("body"), ok));
    }
    if body.dragged_by(LEFT) && ui.data(|d| d.get_temp::<bool>(gesture_id.with("body"))).unwrap_or(false) && body.drag_delta() != egui::Vec2::ZERO {
        let d = drag_step(ui, egui::Id::new(("mask-body-drag", gesture)), body.drag_delta());
        if d != egui::Vec2::ZERO {
            actions.push(("masks.translate".into(), with(json!({"delta": clip_delta(d), "merge": format!("body-{gesture}")}))));
        }
    }
    if body.clicked() && !ptr_in(body.interact_pointer_pos()) {
        actions.push(("masks.select".into(), json!({"none": true})));
    }
    if body.hovered() && ptr_in(body.hover_pos()) {
        ui.ctx().set_cursor_icon(egui::CursorIcon::Move);
    }
    // ---- feather (top) and expansion (bottom) handles
    let top = (0..flat.len()).min_by(|a, b| screen[*a].y.total_cmp(&screen[*b].y));
    let bottom = (0..flat.len()).max_by(|a, b| screen[*a].y.total_cmp(&screen[*b].y));
    if let (Some(ti), Some(bi)) = (top, bottom) {
        let unit_n = |i: usize| {
            let o = offset(&flat, 1.0)[i] - flat[i];
            o * (1.0 / o.length().max(1e-9))
        };
        for (which, i, dist) in [("feather", ti, expansion + feather / 2.0), ("expansion", bi, expansion)] {
            let n = unit_n(i);
            let anchor = sp(&to_screen, flat[i]);
            let hp = sp(&to_screen, flat[i] + n * dist);
            // keep the handle visible off the contour: a short stem outward
            let stem = egui::Vec2::new((sp(&to_screen, flat[i] + n) - anchor).x, (sp(&to_screen, flat[i] + n) - anchor).y);
            let stem = if stem.length() > 1e-6 { stem.normalized() } else { egui::Vec2::new(0.0, -1.0) };
            let hp = hp + stem * 14.0;
            painter.line_segment([sp(&to_screen, flat[i] + n * dist), hp], Stroke::new(1.0, col));
            let hr = Rect::from_center_size(hp, vec2(9.0, 9.0));
            if which == "feather" {
                painter.circle_filled(hp, 4.0, col);
            } else {
                painter.add(egui::Shape::convex_polygon(
                    vec![hp + vec2(0.0, -4.5), hp + vec2(4.5, 0.0), hp + vec2(0.0, 4.5), hp + vec2(-4.5, 0.0)],
                    col,
                    Stroke::NONE,
                ));
            }
            let r = ui.interact(hr.expand(3.0), egui::Id::new(("mask-handle", which)), Sense::drag());
            app.auto.add(&format!("program.mask.{which}"), hr, if which == "feather" { "Mask Feather" } else { "Mask Expansion" });
            if r.dragged_by(LEFT) && r.drag_delta() != egui::Vec2::ZERO {
                let d = clip_delta(r.drag_delta());
                let along = d[0] * n.x + d[1] * n.y;
                let (key, v) = if which == "feather" { ("feather", (feather + 2.0 * along).max(0.0)) } else { ("expansion", expansion + along) };
                actions.push(("masks.set".into(), with(json!({key: v, "merge": format!("{which}-{gesture}")}))));
                painter.text(hp + vec2(10.0, -10.0), Align2::LEFT_BOTTOM, format!("{v:.1}"), Tokens::ui(10.5), t.hot_text);
            }
        }
    }
    // ---- corner roundness (masks with sharp corners: rectangles, polygons)
    actions.extend(roundness_handle(app, ui, &painter, &path, &to_screen, &target));
    // ---- tangent handles and vertices
    let sel_v_id = egui::Id::new(("mask-sel-vertex", sel.clip.0, sel.effect, sel.mask));
    let sel_v: Option<usize> = ui.data(|d| d.get_temp(sel_v_id));
    let alt = ui.input(|i| i.modifiers.alt);
    let cmd = ui.input(|i| i.modifiers.command);
    for (i, v) in path.vertices.iter().enumerate() {
        if v.is_corner() {
            continue;
        }
        let p = sp(&to_screen, v.p);
        for (which, tan) in [("in", v.t_in), ("out", v.t_out)] {
            let h = sp(&to_screen, v.p + tan);
            painter.line_segment([p, h], Stroke::new(1.0, col.gamma_multiply(0.8)));
            painter.circle_filled(h, 3.5, Color32::WHITE);
            painter.circle_stroke(h, 3.5, Stroke::new(1.0, col));
            let hr = Rect::from_center_size(h, vec2(8.0, 8.0));
            let r = ui.interact(hr.expand(2.0), egui::Id::new(("mask-tan", which, i)), Sense::drag());
            app.auto.add(&format!("program.mask.{which}.{i}"), hr, "Bezier handle");
            if r.dragged_by(LEFT) && r.drag_delta() != egui::Vec2::ZERO {
                actions.push((
                    "masks.moveVertex".into(),
                    with(json!({"vertex": i, "handle": which, "delta": clip_delta(r.drag_delta()), "breakHandles": alt, "merge": format!("tan-{gesture}")})),
                ));
            }
        }
    }
    for (i, v) in path.vertices.iter().enumerate() {
        let p = sp(&to_screen, v.p);
        let vr = Rect::from_center_size(p, vec2(7.0, 7.0));
        let selected = sel_v == Some(i);
        painter.rect_filled(vr, 0.0, if selected { col } else { Color32::WHITE });
        painter.rect_stroke(vr, 0.0, Stroke::new(1.0, col), egui::StrokeKind::Middle);
        let r = ui.interact(vr.expand(3.0), egui::Id::new(("mask-vertex", i)), Sense::click_and_drag()).on_hover_text(if cfg!(target_os = "macos") {
            tl!("Drag: move · Alt-click: smooth/corner · Cmd-click: delete")
        } else {
            tl!("Drag: move · Alt-click: smooth/corner · Ctrl-click: delete")
        });
        app.auto.add(&format!("program.mask.vertex.{i}"), vr, "Mask vertex");
        if r.drag_started_by(LEFT) {
            ui.data_mut(|d| d.insert_temp(sel_v_id, i));
        }
        if r.dragged_by(LEFT) && r.drag_delta() != egui::Vec2::ZERO {
            let d = drag_step(ui, egui::Id::new(("mask-vertex-drag", gesture, i)), r.drag_delta());
            if d != egui::Vec2::ZERO {
                actions.push(("masks.moveVertex".into(), with(json!({"vertex": i, "delta": clip_delta(d), "merge": format!("vertex-{gesture}")}))));
            }
        }
        if r.clicked() {
            if alt {
                actions.push(("masks.toggleVertexSmooth".into(), with(json!({"vertex": i}))));
            } else if cmd {
                actions.push(("masks.removeVertex".into(), with(json!({"vertex": i}))));
            } else {
                ui.data_mut(|d| d.insert_temp(sel_v_id, i));
            }
        }
    }
    // Cmd-click on an edge adds a vertex there
    if cmd
        && body.clicked()
        && let Some(pp) = body.interact_pointer_pos()
    {
        let n = path.vertices.len();
        let best = (0..n)
            .map(|i| {
                let seg = MaskPath { vertices: vec![path.vertices[i], path.vertices[(i + 1) % n]], closed: false }.flatten(0.5);
                let d = seg.windows(2).map(|w| seg_dist(pp, sp(&to_screen, w[0]), sp(&to_screen, w[1]))).fold(f32::INFINITY, f32::min);
                (i, d)
            })
            .min_by(|a, b| a.1.total_cmp(&b.1));
        if let Some((i, d)) = best
            && d < 8.0
        {
            let c = to_clip.apply(Vec2::new(pp.x as f64, pp.y as f64));
            actions.retain(|(c, _)| c != "masks.select");
            actions.push(("masks.addVertex".into(), with(json!({"after": i, "at": [c.x, c.y]}))));
        }
    }
    if ui.input(|i| i.key_pressed(egui::Key::Escape)) && ui.ctx().memory(|m| m.focused().is_none()) {
        actions.push(("masks.select".into(), json!({"none": true})));
    }
    for (c, p) in actions {
        if let Err(e) = app.session.execute(&c, p) {
            app.ui.status = e.to_string();
        }
    }
}

fn seg_dist(p: Pos2, a: Pos2, b: Pos2) -> f32 {
    let ab = b - a;
    let l2 = ab.length_sq();
    let t = if l2 > 0.0 { ((p - a).dot(ab) / l2).clamp(0.0, 1.0) } else { 0.0 };
    (p - (a + ab * t)).length()
}

/// A sharp corner `masks.roundCorners` can fillet: its point, the unit bisector pointing to the
/// fillet's centre, `sin(θ/2)` of its opening angle θ, and the largest radius it takes (the fillet
/// never uses more than half of either edge).
struct RoundableCorner {
    p: Vec2,
    bisector: Vec2,
    sin_half: f64,
    max_radius: f64,
}

/// The corners of `path` that rounding changes (degenerate ones — straight, reversing, zero-length
/// edges — are left out).
fn roundable_corners(path: &MaskPath) -> Vec<RoundableCorner> {
    let n = path.vertices.len();
    path.roundable_corners()
        .into_iter()
        .filter_map(|i| {
            let v = path.vertices.get(i)?.p;
            let prev = path.vertices.get((i + n - 1) % n)?.p;
            let next = path.vertices.get((i + 1) % n)?.p;
            let (a, b) = (prev - v, next - v);
            let (la, lb) = (a.length(), b.length());
            if !(la > 1e-6 && lb > 1e-6) {
                return None;
            }
            let (u, w) = (a * (1.0 / la), b * (1.0 / lb));
            let theta = (u.x * w.x + u.y * w.y).clamp(-1.0, 1.0).acos();
            if !(1e-6..std::f64::consts::PI - 1e-6).contains(&theta) {
                return None;
            }
            let s = u + w;
            let sl = s.length();
            if !sl.is_finite() || sl <= 1e-9 {
                return None;
            }
            let half = theta / 2.0;
            let max_radius = la.min(lb) / 2.0 * half.tan();
            max_radius.is_finite().then_some(RoundableCorner { p: v, bisector: s * (1.0 / sl), sin_half: half.sin(), max_radius })
        })
        .collect()
}

/// Radius of a roundness drag that has moved `dist` clip pixels along the corner's bisector: the
/// handle follows the fillet's centre, so it sits `r / sin(θ/2)` from the corner.
fn drag_radius(dist: f64, c: &RoundableCorner, max_radius: f64) -> f64 {
    let r = dist * c.sin_half;
    if r.is_finite() { r.clamp(0.0, max_radius.max(0.0)) } else { 0.0 }
}

/// The corner-roundness handle of the selected mask, just inside its top-left sharp corner.
/// Dragging it into the shape previews the rounded corners live; releasing rounds every sharp
/// corner with one `masks.roundCorners` (one undo step per drag). Masks without a sharp corner
/// between straight edges (ellipses, pen masks, corners already rounded) show no handle.
fn roundness_handle(
    app: &mut FilmcraftApp,
    ui: &mut egui::Ui,
    painter: &egui::Painter,
    path: &MaskPath,
    to_screen: &Affine,
    target: &Value,
) -> Option<(String, Value)> {
    let corners = roundable_corners(path);
    let to_clip = to_screen.inverse()?;
    let lin_inv = Affine { e: 0.0, f: 0.0, ..to_clip };
    let c = corners.iter().min_by(|a, b| {
        let (pa, pb) = (sp(to_screen, a.p), sp(to_screen, b.p));
        (pa.x + pa.y).total_cmp(&(pb.x + pb.y))
    })?;
    let max_radius = corners.iter().map(|c| c.max_radius).fold(0.0, f64::max);
    let id = egui::Id::new("mask-roundness");
    let dist_id = id.with("dist");
    let dragging = ui.ctx().is_being_dragged(id);
    let dist: f64 = if dragging { ui.data(|d| d.get_temp(dist_id)).unwrap_or(0.0) } else { 0.0 };
    let radius = drag_radius(dist, c, max_radius);
    let anchor = sp(to_screen, c.p);
    let dir = sp(to_screen, c.p + c.bisector) - anchor;
    let dir = if dir.length() > 1e-6 { dir.normalized() } else { egui::Vec2::new(1.0, 1.0).normalized() };
    let hp = sp(to_screen, c.p + c.bisector * (radius / c.sin_half.max(1e-9))) + dir * 18.0;
    let hr = Rect::from_center_size(hp, vec2(9.0, 9.0));
    let r = ui.interact(hr.expand(3.0), id, Sense::drag()).on_hover_text(tl!("Drag inward to round the corners"));
    app.auto.add("program.mask.roundness", hr, "Mask corner roundness");
    let col = Color32::from_rgb(0x5a, 0xb4, 0xff);
    if dragging && radius > 0.0 {
        let preview = path.with_rounded_corners(radius, &path.roundable_corners()).flatten(0.5);
        painter.add(egui::Shape::closed_line(preview.iter().map(|v| sp(to_screen, *v)).collect(), Stroke::new(1.5, Color32::WHITE)));
        painter.text(hp + vec2(10.0, -10.0), Align2::LEFT_BOTTOM, format!("{radius:.1}"), Tokens::ui(10.5), app.tokens.hot_text);
    }
    painter.line_segment([anchor, hp], Stroke::new(1.0, col.gamma_multiply(0.6)));
    painter.circle_filled(hp, 4.0, Color32::WHITE);
    painter.circle_stroke(hp, 4.0, Stroke::new(1.5, col));
    let primary = egui::PointerButton::Primary;
    if r.drag_started_by(primary) {
        ui.data_mut(|d| d.insert_temp(dist_id, 0.0_f64));
    }
    if r.dragged_by(primary) && r.drag_delta() != egui::Vec2::ZERO {
        let d = lin_inv.apply(Vec2::new(r.drag_delta().x as f64, r.drag_delta().y as f64));
        let along = d.x * c.bisector.x + d.y * c.bisector.y;
        // accumulate the raw distance: past the limit the handle waits for the pointer to come back
        let total = ui.data(|m| m.get_temp::<f64>(dist_id)).unwrap_or(0.0) + along;
        ui.data_mut(|m| m.insert_temp(dist_id, if total.is_finite() { total } else { 0.0 }));
    }
    if r.drag_stopped_by(primary) {
        let dist: f64 = ui.data_mut(|d| d.remove_temp(dist_id)).unwrap_or(0.0);
        let radius = (drag_radius(dist, c, max_radius) * 10.0).round() / 10.0;
        if radius > 0.0 {
            let mut p = target.clone();
            if let Some(o) = p.as_object_mut() {
                o.insert("radius".into(), json!(radius));
            }
            return Some(("masks.roundCorners".into(), p));
        }
    }
    None
}

/// Draft vertices → a mask path.
pub fn draft_path(points: &[[f64; 4]]) -> MaskPath {
    MaskPath {
        vertices: points.iter().map(|q| MaskVertex { p: Vec2::new(q[0], q[1]), t_in: Vec2::new(-q[2], -q[3]), t_out: Vec2::new(q[2], q[3]) }).collect(),
        closed: true,
    }
}

/// Delete / Backspace and Cmd+Z take back the last pen point while a mask is drawn (#1345). The
/// draft is outside the undo history: run before the shortcuts, so the keys reach neither Clear
/// (of the clip being masked) nor Undo (of the previous project edit). Taking back the last point
/// drops the draft.
pub(crate) fn pen_take_back(app: &mut FilmcraftApp, ctx: &egui::Context, text_focused: bool) {
    let Some(d) = app.ui.mask_pen.as_mut() else { return };
    let n = ctx.input_mut(|i| {
        // Shift+Cmd+Z is Redo: consume_key ignores an extra Shift
        let redo = i.events.iter().any(|e| matches!(e, egui::Event::Key { key: egui::Key::Z, pressed: true, modifiers: m, .. } if m.shift));
        let undo = if redo { 0 } else { i.count_and_consume_key(egui::Modifiers::COMMAND, egui::Key::Z) };
        let clear = if text_focused {
            0
        } else {
            i.count_and_consume_key(egui::Modifiers::NONE, egui::Key::Backspace) + i.count_and_consume_key(egui::Modifiers::NONE, egui::Key::Delete)
        };
        undo + clear
    });
    if n == 0 {
        return;
    }
    let left = d.points.len().saturating_sub(n);
    d.points.truncate(left);
    if left == 0 {
        app.ui.mask_pen = None;
    }
}

fn pen_overlay(app: &mut FilmcraftApp, ui: &mut egui::Ui, pic: Rect, frame: (u32, u32)) {
    let Some(draft) = app.ui.mask_pen.clone() else { return };
    let ph = app.session.playhead();
    let clip = ClipId(draft.clip);
    let Some(it) = app.session.active_sequence().and_then(|q| q.find_item(clip)).map(|(_, i)| i.clone()) else {
        app.ui.mask_pen = None;
        return;
    };
    let mt = it.source_time_at(ph.clamp(it.start, it.end() - Tick(1)));
    let Some(to_screen) = clip_to_screen(app, &it, mt, pic, frame) else { return };
    let Some(to_clip) = to_screen.inverse() else { return };
    let col = Color32::from_rgb(0x5a, 0xb4, 0xff);
    let resp = ui.interact(pic, egui::Id::new("mask-pen"), Sense::click_and_drag());
    app.auto.add("program.maskPen", pic, "Pen mask");
    if resp.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::Crosshair);
    }
    let painter = ui.painter().with_clip_rect(pic);
    let to_c = |p: Pos2| to_clip.apply(Vec2::new(p.x as f64, p.y as f64));
    let mut d = draft.clone();
    // Shift constrains the segment from the previous point (and a dragged-out handle) to 45° steps
    let shift = ui.input(|i| i.modifiers.shift);
    let snap = |from: Option<&[f64; 4]>, p: Pos2| match from {
        Some(q) if shift => constrain_45(sp(&to_screen, Vec2::new(q[0], q[1])), p),
        _ => p,
    };
    let mut commit = false;
    let pressed = resp.drag_started_by(LEFT) || (resp.clicked() && !ui.data(|m| m.get_temp::<bool>(egui::Id::new("mask-pen-placed")).unwrap_or(false)));
    if pressed && let Some(p) = resp.interact_pointer_pos() {
        let near_first = d.points.first().is_some_and(|f| (sp(&to_screen, Vec2::new(f[0], f[1])) - p).length() < 8.0);
        if d.points.len() >= 3 && near_first {
            commit = true;
        } else {
            let c = to_c(snap(d.points.last(), p));
            d.points.push([c.x, c.y, 0.0, 0.0]);
            // read the response before data_mut: it locks the context that drag_started_by reads
            let dragging = resp.drag_started_by(LEFT);
            ui.data_mut(|m| m.insert_temp(egui::Id::new("mask-pen-placed"), dragging));
        }
    }
    if resp.clicked() || resp.drag_stopped() {
        ui.data_mut(|m| m.insert_temp(egui::Id::new("mask-pen-placed"), false));
    }
    if resp.dragged_by(LEFT)
        && !commit
        && let (Some(p), Some(last)) = (resp.interact_pointer_pos(), d.points.last_mut())
    {
        let c = to_c(snap(Some(last), p));
        let tan = Vec2::new(c.x - last[0], c.y - last[1]);
        if tan.length() > 2.0 {
            last[2] = tan.x;
            last[3] = tan.y;
        }
    }
    if ui.input(|i| i.key_pressed(egui::Key::Enter)) && d.points.len() >= 3 {
        commit = true;
    }
    if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
        app.ui.mask_pen = None;
        return;
    }
    // preview
    let open = MaskPath { closed: false, ..draft_path(&d.points) };
    let pts = open_polyline(&open, &to_screen);
    if pts.len() >= 2 {
        painter.add(egui::Shape::line(pts, Stroke::new(1.5, col)));
    }
    if let (Some(h), Some(l)) = (resp.hover_pos(), d.points.last()) {
        painter.line_segment([sp(&to_screen, Vec2::new(l[0], l[1])), snap(Some(l), h)], Stroke::new(1.0, col.gamma_multiply(0.5)));
    }
    for (i, q) in d.points.iter().enumerate() {
        let p = sp(&to_screen, Vec2::new(q[0], q[1]));
        let r = Rect::from_center_size(p, vec2(7.0, 7.0));
        painter.rect_filled(r, 0.0, if i == 0 { col } else { Color32::WHITE });
        painter.rect_stroke(r, 0.0, Stroke::new(1.0, col), egui::StrokeKind::Middle);
        if q[2] != 0.0 || q[3] != 0.0 {
            for s in [1.0, -1.0] {
                let h = sp(&to_screen, Vec2::new(q[0] + q[2] * s, q[1] + q[3] * s));
                painter.line_segment([p, h], Stroke::new(1.0, col));
                painter.circle_filled(h, 3.0, Color32::WHITE);
            }
        }
    }
    if commit {
        let path = draft_path(&d.points);
        app.ui.mask_pen = None;
        let r = app
            .session
            .execute("masks.add", json!({"clip": draft.clip, "effect": draft.effect, "shape": "bezier", "path": filmcraft_engine::masks::path_to_json(&path)}));
        if let Err(e) = r {
            app.ui.status = e.to_string();
        } else {
            app.ui.status.clear();
        }
    } else {
        app.ui.mask_pen = Some(d);
    }
}

/// Screen polyline of an open path's segments (preview while drawing).
fn open_polyline(path: &MaskPath, to_screen: &Affine) -> Vec<Pos2> {
    let mut out = Vec::new();
    if let Some(v) = path.vertices.first() {
        out.push(sp(to_screen, v.p));
    }
    for [p0, c1, c2, p3] in path.segments() {
        for k in 1..=16 {
            out.push(sp(to_screen, filmcraft_project::mask::cubic(p0, c1, c2, p3, k as f64 / 16.0)));
        }
    }
    out
}

/// This frame's drag delta for a whole-mask or vertex drag. With Shift held the movement since the
/// press is constrained to 45° steps (horizontal, vertical, diagonal); the state (raw and applied
/// totals) lives under `id`, which includes the pointer-press counter so each drag starts fresh.
fn drag_step(ui: &egui::Ui, id: egui::Id, delta: egui::Vec2) -> egui::Vec2 {
    let shift = ui.input(|i| i.modifiers.shift);
    let (raw, applied) = ui.data(|d| d.get_temp::<(egui::Vec2, egui::Vec2)>(id)).unwrap_or_default();
    let (raw, applied, step) = shift_drag(raw, applied, delta, shift);
    ui.data_mut(|d| d.insert_temp(id, (raw, applied)));
    step
}

/// Pure core of [`drag_step`]: `(raw total, applied total, step to apply now)`.
fn shift_drag(raw: egui::Vec2, applied: egui::Vec2, delta: egui::Vec2, shift: bool) -> (egui::Vec2, egui::Vec2, egui::Vec2) {
    if !(delta.x.is_finite() && delta.y.is_finite()) {
        return (raw, applied, egui::Vec2::ZERO);
    }
    let raw = raw + delta;
    let target = if shift { constrain_45(Pos2::ZERO, raw.to_pos2()).to_vec2() } else { applied + delta };
    (raw, target, target - applied)
}

/// `to` moved onto the nearest 45° ray from `from` (horizontal, vertical or diagonal on screen),
/// keeping its distance along that ray; `to` itself when it coincides with `from` or is not finite.
fn constrain_45(from: Pos2, to: Pos2) -> Pos2 {
    let v = to - from;
    if !(v.x.is_finite() && v.y.is_finite()) || v.length_sq() == 0.0 {
        return to;
    }
    let step = std::f32::consts::FRAC_PI_4;
    let a = (v.y.atan2(v.x) / step).round() * step;
    let dir = egui::Vec2::new(a.cos(), a.sin());
    from + dir * v.dot(dir)
}

/// The Mask Path row's value area: mask tracking (backward continuously / one frame, forward one
/// frame / continuously; clicking while a track runs stops it) and the tracking-method menu.
pub fn path_value(app: &mut FilmcraftApp, ui: &mut egui::Ui, clip: ClipId, effect: usize, mask: Option<usize>, actions: &mut Vec<(String, Value)>) {
    let Some(k) = mask else { return };
    let t = app.tokens;
    let Some(it) = app.session.active_sequence().and_then(|q| q.find_item(clip)).map(|(_, i)| i.clone()) else { return };
    let Some(e) = it.effects.get(effect) else { return };
    let Some(m) = e.masks.get(k) else { return };
    let base = format!("effectControls.{}.mask{k}", e.effect);
    let target = filmcraft_engine::masks::MaskSel { clip, effect, mask: k };
    let running = app.session.mask_jobs.iter().find(|j| j.target == target).map(|j| j.job);
    for (icon, dir, frames, id, tip) in [
        (Icon::TrackMaskBack, "backward", None, "back", tl!("Track selected mask backward")),
        (Icon::TrackMaskBackFrame, "backward", Some(1), "backFrame", tl!("Track selected mask backward 1 frame")),
        (Icon::TrackMaskFwdFrame, "forward", Some(1), "fwdFrame", tl!("Track selected mask forward 1 frame")),
        (Icon::TrackMaskFwd, "forward", None, "fwd", tl!("Track selected mask forward")),
    ] {
        let (r, resp) = ui.allocate_exact_size(vec2(20.0, 18.0), Sense::click());
        let resp = resp.on_hover_text(if running.is_some() { tl!("Stop tracking") } else { tip });
        if resp.hovered() {
            ui.painter().rect_filled(r, 3.0, t.hover);
        }
        icons::paint(ui.painter(), r.shrink(3.0), icon, if running.is_some() { t.accent } else { t.icon });
        app.auto.add(&format!("{base}.track.{id}"), r, tip);
        if resp.clicked() {
            match running {
                Some(job) => actions.push(("jobs.cancel".into(), json!({"job": job}))),
                None => {
                    let mut p = json!({"clip": clip.0, "effect": effect, "mask": k, "direction": dir});
                    if let Some(n) = frames {
                        p["frames"] = json!(n);
                    }
                    actions.push(("masks.select".into(), json!({"clip": clip.0, "effect": effect, "mask": k})));
                    actions.push(("masks.track".into(), p));
                }
            }
        }
    }
    let (r, resp) = ui.allocate_exact_size(vec2(20.0, 18.0), Sense::click());
    icons::paint(ui.painter(), r.shrink(3.0), Icon::Wrench, if resp.hovered() { t.tab_text_active } else { t.icon });
    app.auto.add(&format!("{base}.trackMethod"), r, "Tracking method");
    let resp = resp.on_hover_text(tlf!("Tracking method: {method}", method = crate::i18n::t(m.track_method.label())));
    egui::Popup::menu(&resp).show(|ui| {
        for tm in filmcraft_project::TrackMethod::ALL {
            if ui.selectable_label(tm == m.track_method, crate::i18n::t(tm.label())).clicked() {
                actions.push(("masks.set".into(), json!({"clip": clip.0, "effect": effect, "mask": k, "trackMethod": tm.label()})));
            }
        }
    });
    if let Some(job) = running
        && let Some(j) = app.session.jobs.iter().find(|j| j.id == job)
    {
        let d = j.progress.done.load(std::sync::atomic::Ordering::Relaxed);
        let n = j.progress.total.load(std::sync::atomic::Ordering::Relaxed);
        ui.label(egui::RichText::new(format!("{d}/{n}")).size(10.5).color(t.text_dim));
        ui.ctx().request_repaint();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shift_drag_constrains_total_movement_to_45_degree_steps() {
        use egui::vec2;
        let (mut raw, mut applied) = (egui::Vec2::ZERO, egui::Vec2::ZERO);
        let mut sum = egui::Vec2::ZERO;
        // a mostly horizontal drag stays on the x axis
        for d in [vec2(10.0, 2.0), vec2(10.0, 3.0), vec2(10.0, 1.0)] {
            let (r, a, s) = shift_drag(raw, applied, d, true);
            (raw, applied) = (r, a);
            sum += s;
        }
        assert!(sum.y.abs() < 1e-4 && (sum.x - 30.0).abs() < 1.0, "{sum:?}");
        // without shift the delta passes through unchanged
        let (_, _, s) = shift_drag(egui::Vec2::ZERO, egui::Vec2::ZERO, vec2(3.0, 4.0), false);
        assert_eq!(s, vec2(3.0, 4.0));
        // a non-finite delta is ignored
        let (_, _, s) = shift_drag(egui::Vec2::ZERO, egui::Vec2::ZERO, vec2(f32::NAN, 1.0), true);
        assert_eq!(s, egui::Vec2::ZERO);
    }

    #[test]
    fn constrain_45_snaps_to_the_nearest_eighth_turn() {
        let o = pos2(10.0, 20.0);
        let near = |a: Pos2, b: Pos2| (a - b).length() < 1e-3;
        // nearly horizontal / vertical: the off-axis component goes
        assert!(near(constrain_45(o, pos2(110.0, 30.0)), pos2(110.0, 20.0)));
        assert!(near(constrain_45(o, pos2(5.0, -80.0)), pos2(10.0, -80.0)));
        // nearly diagonal: projected onto the 45° ray
        let d = constrain_45(o, pos2(60.0, 66.0));
        assert!(((d.x - o.x) - (d.y - o.y)).abs() < 1e-3, "{d:?}");
        // degenerate input comes back unchanged
        assert_eq!(constrain_45(o, o), o);
        let nan = pos2(f32::NAN, 1.0);
        assert!(constrain_45(o, nan).x.is_nan());
    }
}
