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
fn paint_mixed_gradient(
    px: &mut [f32],
    cover: &Mask,
    grad: &LinearGradient,
    lb: [f32; 4],
    inv: &Xform,
    origin: (i32, i32),
    layout: &Layout,
    m: &Xform,
    run_fills: &[Option<[f32; 4]>],
) {
    let mut grad_mask = Mask::new(cover.w, cover.h);
    render::draw_run(layout, m, &mut grad_mask, origin, Some(0));
    let mut solids: Vec<([f32; 4], Vec<u16>)> = Vec::new();
    for (i, fill) in run_fills.iter().enumerate() {
        let run = (i + 1) as u16;
        if let Some(col) = fill {
            match solids.iter_mut().find(|g| g.0 == *col) {
                Some(g) => g.1.push(run),
                None => solids.push((*col, vec![run])),
            }
        } else {
            render::draw_run(layout, m, &mut grad_mask, origin, Some(run));
        }
    }
    over_linear_gradient(px, &grad_mask, grad, lb, inv, origin);
    for (col, runs) in solids {
        let mut m_run = Mask::new(cover.w, cover.h);
        for r in runs {
            render::draw_run(layout, m, &mut m_run, origin, Some(r));
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
    let strokes: Vec<(Mask, [f32; 4])> = ap.strokes.iter().map(|(c, sw, k)| (mask::stroke(&cover, sw * s, stroke_kind(*k)), *c)).collect();
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
    let outer = |k: u32| k != 1 && k != 2;
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
            paint_mixed_gradient(&mut px, &cover, grad, lb, &inv, (x0, y0), layout, m, &run_fills);
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
mod tests {
    use super::*;
    use filmcraft_project::graphic::{new_shape_layer, new_text_layer};
    use filmcraft_project::{ParamValue, find_effect};

    fn set(e: &mut EffectInstance, k: &str, v: ParamValue) {
        e.params.get_mut(k).unwrap_or_else(|| panic!("{k}")).value = v;
    }

    /// A 5 s graphic clip holding `layers` (canvas 400×300 at 25 fps).
    fn graphic_item(layers: Vec<EffectInstance>) -> TrackItem {
        let mut p = filmcraft_project::Project::new("t");
        let rate = filmcraft_time::FrameRate::FPS_25;
        let g = p.add_item("G", filmcraft_project::Label::Rose, filmcraft_project::ItemKind::Graphic { width: 400, height: 300, rate }, None);
        let mut it = p
            .make_track_item(g, filmcraft_project::TrackKind::Video, Tick::ZERO, filmcraft_time::TimeRange::new(Tick::ZERO, Tick::from_seconds_f64(5.0)), rate)
            .unwrap();
        it.effects.extend(layers);
        it
    }

    fn ink_box(img: &Image) -> Option<[usize; 4]> {
        let mut b: Option<[usize; 4]> = None;
        for y in 0..img.h {
            for x in 0..img.w {
                if img.get(x, y)[3] > 0.5 {
                    b = Some(match b {
                        Some(a) => [a[0].min(x), a[1].min(y), a[2].max(x), a[3].max(y)],
                        None => [x, y, x, y],
                    });
                }
            }
        }
        b
    }

    #[test]
    fn per_character_fill_and_size() {
        use filmcraft_project::graphic_design::{CharStyle, LayerExtra, StyleRun};
        let mut e = new_text_layer("AAAA", Vec2::new(20.0, 100.0), 60.0);
        set(&mut e, "fill_color", ParamValue::Color([1.0, 1.0, 1.0, 1.0]));
        let red = CharStyle { fill: Some([1.0, 0.0, 0.0, 1.0]), ..Default::default() };
        e.layer = Some(Box::new(LayerExtra { runs: vec![StyleRun { start: 2, end: 4, style: red }], ..Default::default() }));
        let mut img = Image::new(400, 200);
        render_graphic_layers(&[e.clone()], Tick::ZERO, (400, 200), &Affine::IDENTITY, &mut img);
        let l = text_layout(match &eval_layer(&e, Tick::ZERO, (400, 200)).unwrap().content {
            LayerContent::Text(t) => t,
            _ => unreachable!(),
        });
        let split = 20.0 + l.caret(2).0;
        let (mut white, mut red_px) = (0, 0);
        for y in 0..200 {
            for x in 0..400 {
                let p = img.get(x, y);
                if p[3] > 0.9 {
                    if p[1] > 0.9 {
                        white += 1;
                        assert!((x as f32) < split + 1.0, "white only in the first half at {x}");
                    } else if p[0] > 0.9 && p[1] < 0.05 {
                        red_px += 1;
                        assert!((x as f32) > split - 1.0, "red only in the second half at {x}");
                    }
                }
            }
        }
        assert!(white > 200 && red_px > 200, "{white} {red_px}");
        // a bigger run is taller and sits on the same baseline
        let big = CharStyle { size: Some(120.0), ..Default::default() };
        e.layer = Some(Box::new(LayerExtra { runs: vec![StyleRun { start: 3, end: 4, style: big }], ..Default::default() }));
        let mut img2 = Image::new(400, 200);
        render_graphic_layers(&[e], Tick::ZERO, (400, 200), &Affine::IDENTITY, &mut img2);
        let (b1, b2) = (ink_box(&img).unwrap(), ink_box(&img2).unwrap());
        assert!(b2[1] + 30 < b1[1], "the big letter rises higher: {b1:?} {b2:?}");
        assert!(b2[3].abs_diff(b1[3]) <= 1, "same baseline: {b1:?} {b2:?}");
    }

    #[test]
    fn pinned_box_follows_text_growth() {
        use filmcraft_project::graphic_design::{LayerExtra, Pin, PinTarget};
        let mut t = new_text_layer("Hi", Vec2::new(50.0, 100.0), 40.0);
        t.layer = Some(Box::new(LayerExtra { uid: 1, ..Default::default() }));
        let mut bx = new_shape_layer(0, Vec2::new(60.0, 90.0), Vec2::new(40.0, 40.0), vec![]);
        let tb = layer_bounds(&eval_layer(&t, Tick::ZERO, (400, 300)).unwrap());
        let padded = [tb[0] - 10.0, tb[1] - 10.0, tb[2] + 10.0, tb[3] + 10.0];
        let pin = Pin::new(PinTarget::Layer(1), [true; 4], padded, tb);
        bx.layer = Some(Box::new(LayerExtra { uid: 2, pin: Some(pin), ..Default::default() }));
        for text in ["Hi", "Hello there, world"] {
            set(&mut t, "text", ParamValue::Text(text.into()));
            let specs = graphic_specs(&[bx.clone(), t.clone()], Tick::ZERO, (400, 300));
            let (b, tt) = (layer_bounds(&specs[0]), layer_bounds(&specs[1]));
            for k in 0..4 {
                let want = tt[k] + if k < 2 { -10.0 } else { 10.0 };
                assert!((b[k] - want).abs() < 1e-3, "{text}: edge {k}: {b:?} vs {tt:?}");
            }
        }
    }

    #[test]
    fn roll_moves_the_whole_graphic() {
        use filmcraft_project::graphic_design::{GraphicMeta, Roll, RollMode};
        let e = new_shape_layer(0, Vec2::new(200.0, 150.0), Vec2::new(100.0, 100.0), vec![]);
        let mut item = graphic_item(vec![e]);
        item.graphic = Some(Box::new(GraphicMeta {
            roll: Roll { mode: RollMode::Roll, start_off_screen: true, end_off_screen: true, ..Default::default() },
            ..Default::default()
        }));
        let at = |item: &TrackItem, s: f64| {
            let sp = item_layer_specs(item, Tick::from_seconds_f64(s), (400, 300));
            layer_bounds(&sp[0].1)
        };
        // the box (y 100..200) starts with its top at the frame bottom and ends above the frame
        assert!((at(&item, 0.0)[1] - 300.0).abs() < 1e-6);
        assert!(at(&item, 5.0)[3].abs() < 1e-6);
        // half way: offset (200 + (-200)) / 2 = 0 → back where it was laid out
        assert!((at(&item, 2.5)[1] - 100.0).abs() < 1e-6, "{:?}", at(&item, 2.5));
        let mut img = Image::new(400, 300);
        render_graphic(&item, Tick::from_seconds_f64(2.5), (400, 300), &Affine::IDENTITY, &mut img);
        assert!(img.get(200, 150)[3] > 0.9, "centre at mid-roll");
        let mut img0 = Image::new(400, 300);
        render_graphic(&item, Tick::ZERO, (400, 300), &Affine::IDENTITY, &mut img0);
        assert!(ink_box(&img0).is_none(), "off screen at the start");
    }

    #[test]
    fn text_layer_renders_white_text_at_position() {
        let e = new_text_layer("HELLO", Vec2::new(100.0, 150.0), 60.0);
        let mut img = Image::new(400, 200);
        render_graphic_layers(&[e], Tick::ZERO, (400, 200), &Affine::IDENTITY, &mut img);
        let ink: Vec<(usize, usize)> = (0..200).flat_map(|y| (0..400).map(move |x| (x, y))).filter(|&(x, y)| img.get(x, y)[3] > 0.5).collect();
        assert!(ink.len() > 500);
        let minx = ink.iter().map(|p| p.0).min().unwrap();
        let maxy = ink.iter().map(|p| p.1).max().unwrap();
        assert!((98..=106).contains(&minx), "starts at the position: {minx}");
        assert!((145..=152).contains(&maxy), "sits on the baseline: {maxy}");
        assert!(img.get(ink[0].0, ink[0].1)[0] > 0.4, "white");
    }

    #[test]
    fn shape_appearance_layers() {
        let mut e = new_shape_layer(0, Vec2::new(100.0, 100.0), Vec2::new(80.0, 40.0), vec![]);
        set(&mut e, "fill_color", ParamValue::Color([0.0, 0.0, 1.0, 1.0]));
        set(&mut e, "stroke", ParamValue::Bool(true));
        set(&mut e, "stroke_width", ParamValue::Float(6.0));
        set(&mut e, "stroke_color", ParamValue::Color([1.0, 0.0, 0.0, 1.0]));
        set(&mut e, "shadow", ParamValue::Bool(true));
        set(&mut e, "shadow_distance", ParamValue::Float(20.0));
        set(&mut e, "shadow_blur", ParamValue::Float(0.0));
        let mut img = Image::new(200, 200);
        render_graphic_layers(&[e], Tick::ZERO, (200, 200), &Affine::IDENTITY, &mut img);
        let p = img.get(100, 100);
        assert!(p[2] > 0.9 && p[0] < 0.05, "blue fill {p:?}");
        let s = img.get(100, 78);
        assert!(s[0] > 0.9 && s[2] < 0.05, "red outer stroke above the box {s:?}");
        // shadow: below-right of the box, outside the stroke
        let sh = img.get(150, 135);
        assert!(sh[3] > 0.5 && sh[0] < 0.05 && sh[2] < 0.05, "black shadow {sh:?}");
        assert_eq!(img.get(20, 20)[3], 0.0);
    }

    #[test]
    fn linear_gradient_runs_left_to_right_on_a_shape_and_on_text() {
        let mut shape = new_shape_layer(0, Vec2::new(100.0, 100.0), Vec2::new(80.0, 40.0), vec![]);
        set(&mut shape, "fill_kind", ParamValue::Choice(1));
        set(&mut shape, "gradient_start", ParamValue::Color([1.0, 0.0, 0.0, 1.0]));
        set(&mut shape, "gradient_end", ParamValue::Color([0.0, 0.0, 1.0, 1.0]));
        set(&mut shape, "gradient_angle", ParamValue::Float(0.0));
        let mut img = Image::new(200, 200);
        render_graphic_layers(&[shape], Tick::ZERO, (200, 200), &Affine::IDENTITY, &mut img);
        let left = img.get(70, 100);
        let right = img.get(130, 100);
        assert!(left[3] > 0.9 && left[0] > left[2], "redder on the left {left:?}");
        assert!(right[3] > 0.9 && right[2] > right[0], "bluer on the right {right:?}");

        let mut text = new_text_layer("HHHHHHHH", Vec2::new(16.0, 90.0), 64.0);
        set(&mut text, "fill_kind", ParamValue::Choice(1));
        set(&mut text, "gradient_start", ParamValue::Color([1.0, 0.0, 0.0, 1.0]));
        set(&mut text, "gradient_end", ParamValue::Color([0.0, 0.0, 1.0, 1.0]));
        let mut img = Image::new(500, 180);
        render_graphic_layers(&[text], Tick::ZERO, (500, 180), &Affine::IDENTITY, &mut img);
        let ink: Vec<(usize, usize)> = (0..img.h).flat_map(|y| (0..img.w).map(move |x| (x, y))).filter(|&(x, y)| img.get(x, y)[3] > 0.8).collect();
        let minx = ink.iter().map(|p| p.0).min().unwrap();
        let maxx = ink.iter().map(|p| p.0).max().unwrap();
        let span = (maxx - minx).max(1);
        let (mut ln, mut rn, mut ls, mut rs) = (0.0_f32, 0.0, 0.0, 0.0);
        for &(x, y) in &ink {
            let p = img.get(x, y)[0];
            if x < minx + span / 3 {
                ls += p;
                ln += 1.0;
            } else if x > maxx - span / 3 {
                rs += p;
                rn += 1.0;
            }
        }
        assert!(ln > 30.0 && rn > 30.0, "ink on both ends of the title: {ln} {rn} x {minx}..{maxx}");
        assert!(ls / ln > rs / rn + 0.15, "text ramps from red to blue: {} {}", ls / ln, rs / rn);
    }

    #[test]
    fn linear_gradient_blends_in_linear_light_and_turns_with_the_layer() {
        let mut shape = new_shape_layer(0, Vec2::new(100.0, 100.0), Vec2::new(80.0, 40.0), vec![]);
        set(&mut shape, "fill_kind", ParamValue::Choice(1));
        set(&mut shape, "gradient_start", ParamValue::Color([0.0, 0.0, 0.0, 1.0]));
        set(&mut shape, "gradient_end", ParamValue::Color([1.0, 1.0, 1.0, 1.0]));
        let mut img = Image::new(200, 200);
        render_graphic_layers(&[shape.clone()], Tick::ZERO, (200, 200), &Affine::IDENTITY, &mut img);
        let mid = img.get(100, 100)[0];
        // Linear-light midpoint is ~0.5. An sRGB midpoint converted to linear is ~0.21.
        assert!(mid > 0.4 && mid < 0.6, "centre of black→white is linear 0.5, got {mid}");
        assert!(img.get(70, 100)[0] < mid && mid < img.get(130, 100)[0]);

        set(&mut shape, "gradient_start", ParamValue::Color([1.0, 0.0, 0.0, 1.0]));
        set(&mut shape, "gradient_end", ParamValue::Color([0.0, 0.0, 1.0, 1.0]));
        set(&mut shape, "rotation", ParamValue::Float(90.0));
        let mut turned = Image::new(200, 200);
        render_graphic_layers(&[shape], Tick::ZERO, (200, 200), &Affine::IDENTITY, &mut turned);
        let top = turned.get(100, 70);
        let bot = turned.get(100, 130);
        assert!(top[3] > 0.9 && bot[3] > 0.9, "the rotated ramp is inside the shape: {top:?} {bot:?}");
        assert!(top[0] > top[2], "clockwise 90° carries the start colour to the top {top:?}");
        assert!(bot[2] > bot[0], "and the end colour to the bottom {bot:?}");
    }

    #[test]
    fn linear_gradient_keeps_a_per_character_fill_solid() {
        use filmcraft_project::graphic_design::{CharStyle, LayerExtra, StyleRun};
        let mut e = new_text_layer("AAAA", Vec2::new(20.0, 100.0), 60.0);
        set(&mut e, "fill_kind", ParamValue::Choice(1));
        set(&mut e, "gradient_start", ParamValue::Color([0.0, 1.0, 0.0, 1.0]));
        set(&mut e, "gradient_end", ParamValue::Color([0.0, 0.0, 1.0, 1.0]));
        let red = CharStyle { fill: Some([1.0, 0.0, 0.0, 1.0]), ..Default::default() };
        e.layer = Some(Box::new(LayerExtra { runs: vec![StyleRun { start: 2, end: 4, style: red }], ..Default::default() }));
        let mut img = Image::new(400, 200);
        render_graphic_layers(&[e.clone()], Tick::ZERO, (400, 200), &Affine::IDENTITY, &mut img);
        let l = text_layout(match &eval_layer(&e, Tick::ZERO, (400, 200)).unwrap().content {
            LayerContent::Text(t) => t,
            _ => unreachable!(),
        });
        let split = 20.0 + l.caret(2).0;
        let (mut green, mut red_px) = (0, 0);
        for y in 0..200 {
            for x in 0..400 {
                let p = img.get(x, y);
                if p[3] < 0.8 {
                    continue;
                }
                if (x as f32) < split - 1.0 && p[1] > p[0] && p[1] > p[2] {
                    green += 1;
                } else if (x as f32) > split + 1.0 && p[0] > 0.9 && p[1] < 0.05 {
                    red_px += 1;
                }
            }
        }
        assert!(green > 80 && red_px > 80, "gradient on the plain letters, solid red on the styled ones: {green} {red_px}");
    }

    #[test]
    fn inner_and_centre_strokes() {
        for (kind, inside, outside) in [(2u32, true, false), (1, true, true)] {
            let mut e = new_shape_layer(2, Vec2::new(100.0, 100.0), Vec2::new(120.0, 120.0), vec![]);
            set(&mut e, "sides", ParamValue::Float(4.0));
            set(&mut e, "stroke", ParamValue::Bool(true));
            set(&mut e, "stroke_width", ParamValue::Float(6.0));
            set(&mut e, "stroke_type", ParamValue::Choice(kind));
            set(&mut e, "stroke_color", ParamValue::Color([0.0, 1.0, 0.0, 1.0]));
            let mut img = Image::new(200, 200);
            render_graphic_layers(&[e], Tick::ZERO, (200, 200), &Affine::IDENTITY, &mut img);
            // diamond: top vertex at y = 40; probe straight below/above the right vertex (160, 100)
            let green = |x: usize| {
                let p = img.get(x, 100);
                p[1] > 0.9 && p[0] < 0.1
            };
            assert_eq!(green(157), inside, "kind {kind} inside");
            assert_eq!(green(161), outside, "kind {kind} outside");
            assert!(!green(120), "fill in the middle");
        }
    }

    #[test]
    fn rotation_scale_and_opacity() {
        let mut e = new_shape_layer(0, Vec2::new(100.0, 100.0), Vec2::new(100.0, 10.0), vec![]);
        set(&mut e, "rotation", ParamValue::Float(90.0));
        set(&mut e, "opacity", ParamValue::Float(50.0));
        let mut img = Image::new(200, 200);
        render_graphic_layers(&[e.clone()], Tick::ZERO, (200, 200), &Affine::IDENTITY, &mut img);
        assert!((img.get(100, 60)[3] - 0.5).abs() < 0.02, "vertical after 90°");
        assert_eq!(img.get(60, 100)[3], 0.0);
        // a 2× output transform (e.g. Motion scale) draws vectors crisply at the larger size
        let mut big = Image::new(400, 400);
        render_graphic_layers(&[e], Tick::ZERO, (200, 200), &Affine::scale(2.0, 2.0), &mut big);
        assert!((big.get(200, 110)[3] - 0.5).abs() < 0.02);
        let q = layer_quad(&eval_layer(&find_effect("graphic_shape").unwrap().instance(), Tick::ZERO, (200, 200)).unwrap());
        assert!((q[0].x - (100.0 - 200.0)).abs() < 1e-6 && (q[2].y - (100.0 + 100.0)).abs() < 1e-6);
    }

    #[test]
    fn tight_image_matches_full_render() {
        let a = new_text_layer("Tight", Vec2::new(50.0, 80.0), 40.0);
        let b = new_shape_layer(1, Vec2::new(150.0, 120.0), Vec2::new(60.0, 60.0), vec![]);
        let fx = vec![b, a];
        let mut full = Image::new(220, 160);
        let item = graphic_item(fx);
        render_graphic(&item, Tick::ZERO, (220, 160), &Affine::IDENTITY, &mut full);
        let (tight, x, y) = render_graphic_tight(&item, Tick::ZERO, (220, 160), &Affine::IDENTITY, 220, 160).unwrap();
        assert!(tight.w < 220 && tight.h < 160);
        for yy in 0..tight.h {
            for xx in 0..tight.w {
                let (p, q) = (tight.get(xx, yy), full.get(xx + x as usize, yy + y as usize));
                assert!((p[3] - q[3]).abs() < 1e-5);
            }
        }
    }
}
