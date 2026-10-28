//! Graphic clips: text and shape layers drawn as vectors straight into the output (no resampling),
//! with fill, up to two strokes (outer / centre / inner), a background box and a drop shadow.
//! Each layer is rasterised into a tight premultiplied linear-light image that is cached while
//! the layer and its transform stay the same.
//!
//! [`item_layer_specs`] evaluates a graphic clip's layers the way they are shown: keyframes read
//! through Responsive Design – Time, per-character style runs, responsive pins resolved (pinned
//! layers move, pinned shapes resize), and the roll / crawl offset applied to every layer.

use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::sync::{Arc, Mutex, OnceLock};

use filmcraft_geom::{Affine, Vec2};
use filmcraft_project::graphic::{LayerContent, LayerSpec, LinearGradient, ShapeProps, TextProps, eval_layer};
use filmcraft_project::graphic_design::{GraphicMeta, PinNode, remap_time, resolve_pins, roll_offset};
use filmcraft_project::{EffectInstance, TrackItem};
use filmcraft_text::raster::{Xform, apply, fill_into, xform_scale};
use filmcraft_text::{Align, Caps, Layout, Mask, ParagraphStyle, StrokeKind, StyleRun, TextStyle, layout_rich, mask, render};
use filmcraft_time::Tick;
use rayon::prelude::*;

use crate::graphics::{premul_linear, premul_linear_lerp};
use crate::image::Image;

/// f64 affine → the text crate's f32 transform.
pub fn xform(a: &Affine) -> Xform {
    [a.a as f32, a.b as f32, a.c as f32, a.d as f32, a.e as f32, a.f as f32]
}

/// Layer pixels → graphic canvas pixels (position / anchor / scale / rotation).
pub fn layer_matrix(spec: &LayerSpec) -> Affine {
    let t = &spec.transform;
    Affine::motion(t.position, t.scale, t.rotation, t.anchor)
}

/// Text style + paragraph style of a text layer.
pub fn text_styles(t: &TextProps) -> (TextStyle, ParagraphStyle) {
    (
        TextStyle {
            family: if t.font.is_empty() { filmcraft_text::fonts::DEFAULT_FAMILY.into() } else { t.font.clone() },
            style: if t.style.is_empty() { "Regular".into() } else { t.style.clone() },
            size: t.size,
            tracking: t.tracking,
            kerning: t.kerning,
            ligatures: t.ligatures,
            baseline_shift: t.baseline_shift,
            faux_bold: t.faux_bold,
            faux_italic: t.faux_italic,
            caps: caps_of(t.caps),
            underline: t.underline,
        },
        ParagraphStyle {
            align: match t.align {
                1 => Align::Center,
                2 => Align::Right,
                3 => Align::Justify,
                _ => Align::Left,
            },
            leading: t.leading,
            width: (t.box_width > 0.0 && !t.vertical).then_some(t.box_width),
            height: (t.box_width > 0.0 && t.box_height > 0.0 && !t.vertical).then_some(t.box_height),
            rtl: None,
            vertical: t.vertical,
        },
    )
}

fn caps_of(c: u32) -> Caps {
    match c {
        1 => Caps::All,
        2 => Caps::Small,
        _ => Caps::Normal,
    }
}

/// The per-character style runs of a text layer as text-engine runs (overrides applied to the
/// layer's style).
pub fn text_runs(t: &TextProps, base: &TextStyle) -> Vec<StyleRun> {
    t.runs
        .iter()
        .map(|(r, c)| StyleRun {
            range: r.clone(),
            style: TextStyle {
                family: c.font.clone().filter(|f| !f.is_empty()).unwrap_or_else(|| base.family.clone()),
                style: c.font_style.clone().filter(|f| !f.is_empty()).unwrap_or_else(|| base.style.clone()),
                size: c.size.map_or(base.size, |s| s.max(0.1)),
                tracking: c.tracking.unwrap_or(base.tracking),
                baseline_shift: c.baseline_shift.unwrap_or(base.baseline_shift),
                faux_bold: c.faux_bold.unwrap_or(base.faux_bold),
                faux_italic: c.faux_italic.unwrap_or(base.faux_italic),
                caps: c.caps.map_or(base.caps, caps_of),
                underline: c.underline.unwrap_or(base.underline),
                ..base.clone()
            },
        })
        .collect()
}

/// The laid-out text of a text layer (cached by the text engine).
pub fn text_layout(t: &TextProps) -> Arc<Layout> {
    let (st, ps) = text_styles(t);
    let runs = text_runs(t, &st);
    layout_rich(&t.text, &st, &runs, &ps)
}

/// A shape layer's outline in layer pixels (centred on the layer origin).
pub fn shape_path(s: &ShapeProps) -> filmcraft_text::Path {
    let (w, h) = (s.size.0 / 2.0, s.size.1 / 2.0);
    match s.shape {
        1 => filmcraft_text::Path::ellipse(0.0, 0.0, w, h),
        2 => {
            let n = s.sides.max(3);
            let pts: Vec<(f32, f32)> = (0..n)
                .map(|i| {
                    let a = std::f32::consts::TAU * i as f32 / n as f32 - std::f32::consts::FRAC_PI_2;
                    (a.cos() * w, a.sin() * h)
                })
                .collect();
            filmcraft_text::Path::polygon(&pts)
        }
        3 if s.points.len() >= 3 => filmcraft_text::Path::polygon(&s.points.iter().map(|p| (p[0], p[1])).collect::<Vec<_>>()),
        _ => filmcraft_text::Path::round_rect(-w, -h, w, h, s.corner_radius),
    }
}

/// Content bounds `[x0, y0, x1, y1]` of a layer in layer pixels (text box or shape box).
pub fn layer_local_bounds(spec: &LayerSpec) -> [f32; 4] {
    match &spec.content {
        LayerContent::Text(t) => text_layout(t).bounds,
        LayerContent::Shape(s) => match shape_path(s).bounds() {
            Some(b) => [b.0, b.1, b.2, b.3],
            None => [0.0; 4],
        },
    }
}

/// The four corners of a layer's content box in graphic canvas pixels (selection boxes, hit
/// testing, alignment), clockwise from the top-left.
pub fn layer_quad(spec: &LayerSpec) -> [Vec2; 4] {
    let b = layer_local_bounds(spec);
    let m = layer_matrix(spec);
    [Vec2::new(b[0] as f64, b[1] as f64), Vec2::new(b[2] as f64, b[1] as f64), Vec2::new(b[2] as f64, b[3] as f64), Vec2::new(b[0] as f64, b[3] as f64)]
        .map(|p| m.apply(p))
}

/// A rendered layer: premultiplied linear RGBA at device position (`x`, `y`).
#[derive(Debug)]
pub struct LayerRaster {
    pub x: i32,
    pub y: i32,
    pub img: Image,
}

fn stroke_kind(k: u32) -> StrokeKind {
    match k {
        1 => StrokeKind::Center,
        2 => StrokeKind::Inner,
        _ => StrokeKind::Outer,
    }
}

fn bbox_of(m: &Xform, b: [f32; 4], pad: f32) -> [f32; 4] {
    let pts = [(b[0] - pad, b[1] - pad), (b[2] + pad, b[1] - pad), (b[0] - pad, b[3] + pad), (b[2] + pad, b[3] + pad)];
    let mut o = [f32::MAX, f32::MAX, f32::MIN, f32::MIN];
    for (x, y) in pts {
        let (a, c) = apply(m, x, y);
        o = [o[0].min(a), o[1].min(c), o[2].max(a), o[3].max(c)];
    }
    o
}

fn union(a: [f32; 4], b: [f32; 4]) -> [f32; 4] {
    [a[0].min(b[0]), a[1].min(b[1]), a[2].max(b[2]), a[3].max(b[3])]
}

/// Over-composite `c` (premultiplied) through `m` into `px` (premultiplied RGBA, same size).
fn over_mask(px: &mut [f32], m: &Mask, c: [f32; 4]) {
    if c[3] <= 0.0 || m.w == 0 {
        return;
    }
    px.par_chunks_mut(4 * m.w).zip(m.a.par_chunks(m.w)).for_each(|(row, mrow)| {
        for (p, &a) in row.as_chunks_mut::<4>().0.iter_mut().zip(mrow) {
            if a > 0.0 {
                let k = 1.0 - c[3] * a;
                for i in 0..4 {
                    p[i] = c[i] * a + p[i] * k;
                }
            }
        }
    });
}

fn invert_xform(m: &Xform) -> Option<Xform> {
    let det = m[0] * m[3] - m[1] * m[2];
    if det.abs() < 1e-8 {
        return None;
    }
    let inv = 1.0 / det;
    let a = m[3] * inv;
    let b = -m[1] * inv;
    let c = -m[2] * inv;
    let d = m[0] * inv;
    Some([a, b, c, d, -(a * m[4] + c * m[5]), -(b * m[4] + d * m[5])])
}

/// Paint `grad` through `msk`. 0° runs left to right across `lb` in layer space; the angle is
/// clockwise on screen and turns with `inv` (the device→layer transform).
fn over_linear_gradient(px: &mut [f32], msk: &Mask, grad: &LinearGradient, lb: [f32; 4], inv: &Xform, origin: (i32, i32)) {
    if msk.w == 0 {
        return;
    }
    let (cx, cy) = ((lb[0] + lb[2]) * 0.5, (lb[1] + lb[3]) * 0.5);
    let (sin, cos) = grad.angle.to_radians().sin_cos();
    let (ax, ay) = (cos, sin);
    let corners = [(lb[0], lb[1]), (lb[2], lb[1]), (lb[0], lb[3]), (lb[2], lb[3])];
    let half = corners.iter().map(|(x, y)| ((x - cx) * ax + (y - cy) * ay).abs()).fold(1e-3_f32, f32::max);
    let w = msk.w;
    let (start, end) = (grad.start, grad.end);
    px.par_chunks_mut(4 * w).enumerate().for_each(|(row, dest)| {
        let py = origin.1 as f32 + row as f32 + 0.5;
        let mrow = &msk.a[row * w..(row + 1) * w];
        for (col, (p, &cov)) in dest.as_chunks_mut::<4>().0.iter_mut().zip(mrow).enumerate() {
            if cov <= 0.0 {
                continue;
            }
            let (lx, ly) = apply(inv, origin.0 as f32 + col as f32 + 0.5, py);
            let t = (((((lx - cx) * ax + (ly - cy) * ay) / half) + 1.0) * 0.5).clamp(0.0, 1.0);
            let c = premul_linear_lerp(start, end, t);
            let k = 1.0 - c[3] * cov;
            for i in 0..4 {
                p[i] = c[i] * cov + p[i] * k;
            }
        }
    });
}

/// Solid fill, one mask per distinct colour when text runs carry their own.
fn paint_solid(px: &mut [f32], cover: &Mask, color: [f32; 4], text: Option<&Layout>, m: &Xform, run_fills: &[Option<[f32; 4]>], origin: (i32, i32)) {
    let Some(layout) = text.filter(|_| run_fills.iter().any(Option::is_some)) else {
        over_mask(px, cover, premul_linear(color));
        return;
    };
    let mut groups: Vec<([f32; 4], Vec<u16>)> = Vec::new();
    for r in 0..=run_fills.len() {
        let col = if r == 0 { color } else { run_fills[r - 1].unwrap_or(color) };
        match groups.iter_mut().find(|g| g.0 == col) {
            Some(g) => g.1.push(r as u16),
            None => groups.push((col, vec![r as u16])),
        }
    }
    for (col, runs) in groups {
        let mut m_run = Mask::new(cover.w, cover.h);
        for r in runs {
            render::draw_run(layout, m, &mut m_run, origin, Some(r));
        }
        over_mask(px, &m_run, premul_linear(col));
    }
}

/// Gradient for runs that have no fill of their own; runs with a fill stay that solid colour.
struct MixedGradient<'a> {
    px: &'a mut [f32],
    cover: &'a Mask,
    grad: &'a LinearGradient,
    bounds: [f32; 4],
    inverse: &'a Xform,
    origin: (i32, i32),
    layout: &'a Layout,
    transform: &'a Xform,
    run_fills: &'a [Option<[f32; 4]>],
}

fn paint_mixed_gradient(input: MixedGradient<'_>) {
    let MixedGradient { px, cover, grad, bounds, inverse, origin, layout, transform, run_fills } = input;
    let mut grad_mask = Mask::new(cover.w, cover.h);
    render::draw_run(layout, transform, &mut grad_mask, origin, Some(0));
    let mut solids: Vec<([f32; 4], Vec<u16>)> = Vec::new();
    for (i, fill) in run_fills.iter().enumerate() {
        let run = (i + 1) as u16;
        if let Some(col) = fill {
            match solids.iter_mut().find(|g| g.0 == *col) {
                Some(g) => g.1.push(run),
                None => solids.push((*col, vec![run])),
            }
        } else {
            render::draw_run(layout, transform, &mut grad_mask, origin, Some(run));
        }
    }
    over_linear_gradient(px, &grad_mask, grad, bounds, inverse, origin);
    for (col, runs) in solids {
        let mut m_run = Mask::new(cover.w, cover.h);
        for r in runs {
            render::draw_run(layout, transform, &mut m_run, origin, Some(r));
        }
        over_mask(px, &m_run, premul_linear(col));
    }
}

/// Rasterise one layer with layer→device transform `m`, clipped to a `cw`×`ch` canvas.
pub fn raster_layer(spec: &LayerSpec, m: &Xform, cw: usize, ch: usize) -> Option<LayerRaster> {
    if !spec.enabled || spec.transform.opacity <= 0.0 {
        return None;
    }
    let s = xform_scale(m).max(1e-6);
    let ap = &spec.appearance;
    let text = match &spec.content {
        LayerContent::Text(t) => {
            let l = text_layout(t);
            if l.glyphs.is_empty() && l.underlines.is_empty() && ap.background.is_none() {
                return None;
            }
            Some(l)
        }
        LayerContent::Shape(_) => None,
    };
    let lb = match &text {
        Some(l) => l.bounds,
        None => layer_local_bounds(spec),
    };
    let shape = match &spec.content {
        LayerContent::Shape(sh) => Some(shape_path(sh).transformed(m)),
        _ => None,
    };
    let stroke_out = ap
        .strokes
        .iter()
        .map(|(_, w, k)| {
            if *k == 2 {
                0.0
            } else if *k == 1 {
                w / 2.0
            } else {
                *w
            }
        })
        .fold(0.0f32, f32::max);
    let overhang = text.as_ref().map_or(0.0, |l| l.glyphs.iter().map(|g| g.size).fold(0.0f32, f32::max) * 0.35);
    let mut bb = bbox_of(m, lb, overhang + stroke_out + 2.0 / s);
    if let Some((_, pad, _)) = ap.background {
        bb = union(bb, bbox_of(m, lb, pad + 1.0 / s));
    }
    let mut shadow_dev = (0.0f32, 0.0f32);
    if let Some(sh) = &ap.shadow {
        // the offset is in layer space, so it turns and scales with the layer
        shadow_dev = (m[0] * sh.offset.0 + m[2] * sh.offset.1, m[1] * sh.offset.0 + m[3] * sh.offset.1);
        let grow = (sh.size + sh.blur) * s + 2.0;
        bb = union(bb, [bb[0] + shadow_dev.0 - grow, bb[1] + shadow_dev.1 - grow, bb[2] + shadow_dev.0 + grow, bb[3] + shadow_dev.1 + grow]);
    }
    // clip to the canvas, keeping enough margin for blur to read content just outside it
    let margin = ap.shadow.as_ref().map_or(2.0, |sh| (sh.blur + sh.size) * s + shadow_dev.0.abs().max(shadow_dev.1.abs()) + 2.0);
    let x0 = bb[0].floor().max(-margin) as i32;
    let y0 = bb[1].floor().max(-margin) as i32;
    let x1 = bb[2].ceil().min(cw as f32 + margin) as i32;
    let y1 = bb[3].ceil().min(ch as f32 + margin) as i32;
    if x1 <= x0 || y1 <= y0 || x1 <= 0 || y1 <= 0 || x0 >= cw as i32 || y0 >= ch as i32 {
        return None;
    }
    let (w, h) = ((x1 - x0) as usize, (y1 - y0) as usize);
    if w * h > 64 << 20 {
        return None;
    }
    let mut cover = Mask::new(w, h);
    if let Some(l) = &text {
        render::draw(l, m, &mut cover, (x0, y0));
    }
    if let Some(p) = &shape {
        fill_into(p, &mut cover, -x0 as f32, -y0 as f32);
    }
    let mut strokes: Vec<(Mask, [f32; 4])> = ap.strokes.iter().map(|(c, sw, k)| (mask::stroke(&cover, sw * s, stroke_kind(*k)), *c)).collect();
    let outer = |k: u32| k != 1 && k != 2;
    // an outer stroke mask includes the shape so the fill sits on it without a seam; with the
    // fill off only the ring outside the edge shows (as in Premiere)
    if ap.fill.is_none() {
        for ((sm, _), (_, _, k)) in strokes.iter_mut().zip(&ap.strokes) {
            if outer(*k) {
                sm.a.iter_mut().zip(&cover.a).for_each(|(v, c)| *v = v.min(1.0 - c));
            }
        }
    }
    let bg = ap.background.map(|(c, pad, r)| {
        let p = filmcraft_text::Path::round_rect(lb[0] - pad, lb[1] - pad, lb[2] + pad, lb[3] + pad, r).transformed(m);
        let mut bm = Mask::new(w, h);
        fill_into(&p, &mut bm, -x0 as f32, -y0 as f32);
        (bm, c)
    });
    let mut px = vec![0.0f32; w * h * 4];
    if let Some(sh) = &ap.shadow {
        let mut u = if ap.fill.is_some() { cover.clone() } else { Mask::new(w, h) };
        for (sm, _) in &strokes {
            u.max_with(sm);
        }
        if let Some((bm, _)) = &bg {
            u.max_with(bm);
        }
        if sh.size > 0.0 {
            u = mask::stroke(&u, sh.size * s, StrokeKind::Outer);
        }
        let u = mask::offset(&u, shadow_dev.0.round() as i32, shadow_dev.1.round() as i32);
        let u = mask::blur(&u, sh.blur * s / 3.0);
        over_mask(&mut px, &u, premul_linear(sh.color));
    }
    if let Some((bm, c)) = &bg {
        over_mask(&mut px, bm, premul_linear(*c));
    }
    // outer strokes sit under the fill; centre and inner strokes are drawn over it
    for ((sm, c), (_, _, k)) in strokes.iter().zip(&ap.strokes).rev() {
        if outer(*k) {
            over_mask(&mut px, sm, premul_linear(*c));
        }
    }
    let run_fills: Vec<Option<[f32; 4]>> = match &spec.content {
        LayerContent::Text(t) => t.runs.iter().map(|(_, s)| s.fill).collect(),
        _ => Vec::new(),
    };
    if let Some(grad) = &ap.gradient
        && let Some(inv) = invert_xform(m)
    {
        if run_fills.iter().any(Option::is_some)
            && let Some(layout) = &text
        {
            paint_mixed_gradient(MixedGradient {
                px: &mut px,
                cover: &cover,
                grad,
                bounds: lb,
                inverse: &inv,
                origin: (x0, y0),
                layout,
                transform: m,
                run_fills: &run_fills,
            });
        } else {
            over_linear_gradient(&mut px, &cover, grad, lb, &inv, (x0, y0));
        }
    } else if let Some(c) = ap.fill {
        paint_solid(&mut px, &cover, c, text.as_deref(), m, &run_fills, (x0, y0));
    }
    for ((sm, c), (_, _, k)) in strokes.iter().zip(&ap.strokes).rev() {
        if !outer(*k) {
            over_mask(&mut px, sm, premul_linear(*c));
        }
    }
    let op = spec.transform.opacity;
    if op < 1.0 {
        px.iter_mut().for_each(|v| *v *= op);
    }
    Some(LayerRaster { x: x0, y: y0, img: Image { w, h, px } })
}

type Cache = Mutex<HashMap<u64, (u64, Option<Arc<LayerRaster>>)>>;

fn raster_cache() -> &'static Cache {
    static C: OnceLock<Cache> = OnceLock::new();
    C.get_or_init(Default::default)
}

/// [`raster_layer`] through a small cache (static titles re-composite without re-rasterising).
pub fn raster_layer_cached(spec: &LayerSpec, m: &Xform, cw: usize, ch: usize) -> Option<Arc<LayerRaster>> {
    let mut hs = std::collections::hash_map::DefaultHasher::new();
    format!("{spec:?}").hash(&mut hs);
    m.iter().for_each(|v| v.to_bits().hash(&mut hs));
    (cw, ch).hash(&mut hs);
    let key = hs.finish();
    static CLOCK: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let now = CLOCK.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    if let Some(e) = raster_cache().lock().unwrap_or_else(|e| e.into_inner()).get_mut(&key) {
        e.0 = now;
        return e.1.clone();
    }
    let r = raster_layer(spec, m, cw, ch).map(Arc::new);
    let mut c = raster_cache().lock().unwrap_or_else(|e| e.into_inner());
    if c.len() >= 96
        && let Some(k) = c.iter().min_by_key(|(_, v)| v.0).map(|(k, _)| *k)
    {
        c.remove(&k);
    }
    c.insert(key, (now, r.clone()));
    r
}

/// Over-composite a raster at (`r.x` + `dx`, `r.y` + `dy`) onto `img`.
pub fn composite_raster(img: &mut Image, r: &LayerRaster, dx: i32, dy: i32) {
    let (rx, ry) = (r.x + dx, r.y + dy);
    let (w, h) = (img.w as i32, img.h as i32);
    let ys = ry.max(0)..(ry + r.img.h as i32).min(h);
    let xs = rx.max(0)..(rx + r.img.w as i32).min(w);
    if ys.is_empty() || xs.is_empty() {
        return;
    }
    let iw = img.w;
    img.px[ys.start as usize * iw * 4..ys.end as usize * iw * 4].par_chunks_mut(iw * 4).enumerate().for_each(|(row_i, row)| {
        let sy = (ys.start + row_i as i32 - ry) as usize;
        for x in xs.clone() {
            let si = (sy * r.img.w + (x - rx) as usize) * 4;
            let s = &r.img.px[si..si + 4];
            if s[3] <= 0.0 {
                continue;
            }
            let d = &mut row[x as usize * 4..x as usize * 4 + 4];
            let k = 1.0 - s[3];
            for c in 0..4 {
                d[c] = s[c] + d[c] * k;
            }
        }
    });
}

/// Axis-aligned bounds `[x0, y0, x1, y1]` of a layer's content box in canvas pixels.
pub fn layer_bounds(spec: &LayerSpec) -> [f64; 4] {
    let q = layer_quad(spec);
    let mut b = [f64::MAX, f64::MAX, f64::MIN, f64::MIN];
    for p in q {
        b = [b[0].min(p.x), b[1].min(p.y), b[2].max(p.x), b[3].max(p.y)];
    }
    b
}

/// Resolve responsive pins of evaluated layers in place: pinned layers move, shapes pinned on
/// opposite edges resize.
pub fn apply_pins(specs: &mut [LayerSpec], canvas: (u32, u32)) {
    if !specs.iter().any(|s| s.pin.is_some()) {
        return;
    }
    let nodes: Vec<PinNode> = specs
        .iter()
        .map(|s| PinNode { uid: s.uid, pin: s.pin.clone(), bounds: layer_bounds(s), resizable: matches!(s.content, LayerContent::Shape(_)) })
        .collect();
    let out = resolve_pins(&nodes, [0.0, 0.0, canvas.0 as f64, canvas.1 as f64]);
    for ((s, n), b) in specs.iter_mut().zip(&nodes).zip(out) {
        if b == n.bounds {
            continue;
        }
        let (ow, oh) = (n.bounds[2] - n.bounds[0], n.bounds[3] - n.bounds[1]);
        let (nw, nh) = (b[2] - b[0], b[3] - b[1]);
        if let LayerContent::Shape(sh) = &mut s.content {
            let kx = if ow > 1e-9 { nw / ow } else { 1.0 };
            let ky = if oh > 1e-9 { nh / oh } else { 1.0 };
            if (kx - 1.0).abs() > 1e-12 || (ky - 1.0).abs() > 1e-12 {
                sh.size = ((sh.size.0 as f64 * kx) as f32, (sh.size.1 as f64 * ky) as f32);
                sh.points.iter_mut().for_each(|p| *p = [(p[0] as f64 * kx) as f32, (p[1] as f64 * ky) as f32]);
            }
        }
        let now = layer_bounds(s);
        s.transform.position = Vec2::new(s.transform.position.x + b[0] - now[0], s.transform.position.y + b[1] - now[1]);
    }
}

/// Evaluated layers of a graphic clip's effect list at clip time `t`, in paint order, with pins
/// resolved.
pub fn graphic_specs(effects: &[EffectInstance], t: Tick, canvas: (u32, u32)) -> Vec<LayerSpec> {
    let mut v: Vec<LayerSpec> = effects.iter().filter(|e| filmcraft_project::graphic::is_layer(e)).filter_map(|e| eval_layer(e, t, canvas)).collect();
    apply_pins(&mut v, canvas);
    v
}

/// The media time at which a graphic clip's layers are read at media time `mt` (Responsive Design
/// – Time), and the clip-relative time and clip length (media time) for rolls.
pub fn graphic_time(item: &TrackItem, mt: Tick) -> (Tick, Tick, Tick) {
    let u = mt - item.source_in;
    let d = Tick((item.duration.0 as f64 * item.speed.abs().max(1e-9)).round() as i64);
    let t = match item.graphic.as_deref() {
        Some(m) if m.has_responsive_time() => remap_time(m, u, d),
        _ => mt,
    };
    (t, u, d)
}

/// Union of the layers' bounds (with background padding) for rolls and crawls.
fn content_bounds(specs: &[LayerSpec]) -> Option<[f64; 4]> {
    let mut b: Option<[f64; 4]> = None;
    for s in specs.iter().filter(|s| s.enabled) {
        let mut lb = layer_bounds(s);
        if let Some((_, pad, _)) = s.appearance.background {
            let k = s.transform.scale.x.abs().max(s.transform.scale.y.abs());
            let p = pad as f64 * k;
            lb = [lb[0] - p, lb[1] - p, lb[2] + p, lb[3] + p];
        }
        b = Some(match b {
            Some(a) => [a[0].min(lb[0]), a[1].min(lb[1]), a[2].max(lb[2]), a[3].max(lb[3])],
            None => lb,
        });
    }
    b
}

/// A graphic clip's layers at media time `mt` as shown: `(effect index, layer)` in paint order,
/// with Responsive Design – Time, pins and the roll / crawl offset applied. Includes hidden
/// layers (their `enabled` is false).
pub fn item_layer_specs(item: &TrackItem, mt: Tick, canvas: (u32, u32)) -> Vec<(usize, LayerSpec)> {
    let (t, u, d) = graphic_time(item, mt);
    let mut idx = Vec::new();
    let mut specs = Vec::new();
    for (i, e) in item.effects.iter().enumerate() {
        if filmcraft_project::graphic::is_layer(e)
            && let Some(s) = eval_layer(e, t, canvas)
        {
            idx.push(i);
            specs.push(s);
        }
    }
    apply_pins(&mut specs, canvas);
    if let Some(meta) = item.graphic.as_deref() {
        apply_roll(meta, &mut specs, u, d, canvas);
    }
    idx.into_iter().zip(specs).collect()
}

/// Move every layer by the roll / crawl offset at clip time `u` of a clip `d` long.
pub fn apply_roll(meta: &GraphicMeta, specs: &mut [LayerSpec], u: Tick, d: Tick, canvas: (u32, u32)) {
    if meta.roll.mode == filmcraft_project::RollMode::Off {
        return;
    }
    let Some(cb) = content_bounds(specs) else { return };
    let (dx, dy) = roll_offset(&meta.roll, u, d, canvas, cb);
    for s in specs.iter_mut() {
        s.transform.position = Vec2::new(s.transform.position.x + dx, s.transform.position.y + dy);
    }
}

/// Draw all layers of a graphic clip at media time `mt` onto `out`; `m` maps graphic canvas
/// pixels to `out` pixels.
pub fn render_graphic(item: &TrackItem, mt: Tick, canvas: (u32, u32), m: &Affine, out: &mut Image) {
    let specs: Vec<LayerSpec> = item_layer_specs(item, mt, canvas).into_iter().map(|(_, s)| s).collect();
    draw_specs(&specs, m, out);
}

/// Draw a graphic's layers (an effect list, without clip settings) at time `t` onto `out`.
pub fn render_graphic_layers(effects: &[EffectInstance], t: Tick, canvas: (u32, u32), m: &Affine, out: &mut Image) {
    draw_specs(&graphic_specs(effects, t, canvas), m, out);
}

fn draw_specs(specs: &[LayerSpec], m: &Affine, out: &mut Image) {
    for spec in specs {
        let lm = xform(&m.then_apply(&layer_matrix(spec)));
        if let Some(r) = raster_layer_cached(spec, &lm, out.w, out.h) {
            composite_raster(out, &r, 0, 0);
        }
    }
}

/// All layers composed into one tight image (for the GPU plan): `(image, x, y)` in output pixels.
pub fn render_graphic_tight(item: &TrackItem, mt: Tick, canvas: (u32, u32), m: &Affine, w: usize, h: usize) -> Option<(Image, i32, i32)> {
    let rasters: Vec<Arc<LayerRaster>> =
        item_layer_specs(item, mt, canvas).iter().filter_map(|(_, spec)| raster_layer_cached(spec, &xform(&m.then_apply(&layer_matrix(spec))), w, h)).collect();
    let x0 = rasters.iter().map(|r| r.x).min()?.max(0);
    let y0 = rasters.iter().map(|r| r.y).min()?.max(0);
    let x1 = rasters.iter().map(|r| r.x + r.img.w as i32).max()?.min(w as i32);
    let y1 = rasters.iter().map(|r| r.y + r.img.h as i32).max()?.min(h as i32);
    if x1 <= x0 || y1 <= y0 {
        return None;
    }
    let mut img = Image::new((x1 - x0) as usize, (y1 - y0) as usize);
    for r in &rasters {
        composite_raster(&mut img, r, -x0, -y0);
    }
    Some((img, x0, y0))
}

#[cfg(test)]
mod tests;
