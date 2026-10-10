//! Stylize, Perspective and generator effects (plus the Legacy and obsolete generators).

use std::collections::VecDeque;

use filmcraft_project::EffectInstance;
use filmcraft_time::Tick;
use rayon::prelude::*;

use super::*;

/// Brush Strokes: a painterly look from short directional strokes (angle jittered per cell).
pub fn brush_strokes(img: &mut Image, e: &EffectInstance, cx: &FxCtx) {
    let base_ang = fv(e, "angle", cx).to_radians();
    let size = (fv(e, "size", cx) * cx.px_scale).max(0.0);
    let len = fv(e, "length", cx) * cx.px_scale;
    if len < 0.5 && size < 0.5 {
        return;
    }
    let density = fv(e, "density", cx).max(0.05);
    let rnd = fv(e, "randomness", cx);
    let surface = chv(e, "surface");
    let blend = fv(e, "blend", cx) / 100.0;
    let cell = (len.max(2.0) / density).max(2.0);
    let steps = (len.ceil() as usize).clamp(2, 24);
    let src = img.clone();
    let w = img.w;
    img.px.par_chunks_mut(w * 4).enumerate().for_each(|(y, row)| {
        for x in 0..w {
            let (px, py) = (x as f32 + 0.5, y as f32 + 0.5);
            let (cxi, cyi) = ((px / cell).floor() as i64, (py / cell).floor() as i64);
            let jit = (hash01(cxi, cyi, 1, 77) - 0.5) * rnd * std::f32::consts::FRAC_PI_2;
            let a = base_ang + jit;
            let (dx, dy) = (a.cos(), -a.sin());
            let (nx, ny) = (-dy, dx);
            let mut acc = [0.0f32; 4];
            let mut n = 0.0;
            for i in 0..steps {
                let t = (i as f32 / (steps - 1) as f32 - 0.5) * len;
                for s in [-0.5f32, 0.0, 0.5] {
                    let q = src.sample_bilinear_clamped(px + dx * t + nx * s * size, py + dy * t + ny * s * size);
                    for k in 0..4 {
                        acc[k] += q[k];
                    }
                    n += 1.0;
                }
            }
            // bristle texture across the stroke
            let perp = px * nx + py * ny;
            let bristle = 1.0 + 0.1 * noise3(perp / size.max(1.0), (px * dx + py * dy) / len.max(1.0), cxi as f32 * 0.37, 5);
            let mut o = acc.map(|v| v / n);
            for v in &mut o[..3] {
                *v *= bristle;
            }
            let cov = (0.75 + 0.25 * hash01(cxi, cyi, 3, 77)).min(1.0);
            let o = match surface {
                1 => o.map(|v| v * cov),
                2 | 3 => {
                    let bg = if surface == 2 { 1.0 } else { 0.0 };
                    let mut b = [bg, bg, bg, 1.0];
                    over(&mut b, o.map(|v| v * cov));
                    b
                }
                _ => o,
            };
            let orig = src.get(x, y);
            for k in 0..4 {
                row[x * 4 + k] = o[k] + (orig[k] - o[k]) * blend;
            }
        }
    });
}

/// Color Emboss: emboss relief that keeps the picture's colours.
pub fn color_emboss(img: &mut Image, e: &EffectInstance, cx: &FxCtx) {
    let dir = (fv(e, "direction", cx) as f64).to_radians();
    let relief = fv(e, "relief", cx) * cx.px_scale.max(0.25);
    let contrast = fv(e, "contrast", cx) / 100.0;
    let blend = fv(e, "blend", cx) / 100.0;
    let (dx, dy) = ((dir.cos() as f32) * relief, (-dir.sin() as f32) * relief);
    let src = img.clone();
    img.map_rgb(|c, x, y| {
        let a = src.sample_bilinear_clamped(x as f32 + 0.5 + dx, y as f32 + 0.5 + dy);
        let b = src.sample_bilinear_clamped(x as f32 + 0.5 - dx, y as f32 + 0.5 - dy);
        let d = luma(Image::unpremul(a)) - luma(Image::unpremul(b));
        let v = enc(c);
        let o = v.map(|q| (q + d * contrast * 2.0).clamp(0.0, 1.0));
        dec(lerp3(o, v, blend))
    });
}

/// Roughen Edges: fractal-eroded alpha edges (Roughen, Cut, Spiky, Rusty, Photocopy…).
pub fn roughen_edges(img: &mut Image, e: &EffectInstance, cx: &FxCtx) {
    let kind = chv(e, "edge_type");
    let border = fv(e, "border", cx) * cx.px_scale;
    if border < 0.5 {
        return;
    }
    let sharp = fv(e, "sharpness", cx).max(0.0);
    let infl = fv(e, "influence", cx).clamp(0.0, 1.0);
    let scale = (fv(e, "scale", cx) / 100.0 * 30.0 * cx.px_scale).max(0.5);
    let stretch = 2f32.powf(fv(e, "stretch", cx));
    let off = offv(e, "offset", cx);
    let cplx = fv(e, "complexity", cx).clamp(1.0, 10.0);
    let evo = fv(e, "evolution", cx) / 360.0;
    let seed = fv(e, "seed", cx).max(0.0) as u64;
    let ec = lin(cv(e, "edge_color", cx));
    let colored = matches!(kind, 1 | 5 | 7);
    let a = alpha_of(img);
    let depth = blur_plane(&a, img.w, img.h, border * 0.5);
    let w = img.w;
    img.px.par_chunks_mut(w * 4).enumerate().for_each(|(y, row)| {
        for x in 0..w {
            let i = y * w + x;
            let p = &mut row[x * 4..x * 4 + 4];
            if p[3] <= 1e-6 {
                continue;
            }
            let (u, v) = ((x as f32 + off.x as f32) / (scale * stretch), (y as f32 + off.y as f32) * stretch / scale);
            let mut n = fbm(u, v, evo, cplx, seed);
            if kind == 3 {
                n = 1.0 - 2.0 * n.abs();
            }
            let d = depth[i];
            let mut k = ((d - 0.5 + n * infl * 0.5) * (1.0 + sharp * 4.0) + 0.5).clamp(0.0, 1.0);
            if kind == 2 {
                k = if k > 0.5 { 1.0 } else { 0.0 };
            }
            if matches!(kind, 4..=7) {
                // rust / photocopy: holes eaten into the border region
                let hole = fbm(u * 2.3 + 11.0, v * 2.3, evo, cplx, seed + 1);
                if d < 0.97 && hole > 0.25 {
                    k *= 1.0 - smoothstep(0.25, 0.4, hole) * (1.0 - d);
                }
            }
            if colored && d < 0.97 {
                let t = (1.0 - d) * 1.5;
                let c = Image::unpremul([p[0], p[1], p[2], p[3]]);
                let col = lerp3(c, ec, t.min(1.0));
                for c in 0..3 {
                    p[c] = col[c] * p[3];
                }
            }
            if matches!(kind, 6 | 7) {
                let c = Image::unpremul([p[0], p[1], p[2], p[3]]);
                let l = if luma(enc(c)) > 0.5 { 1.0 } else { 0.0 };
                if kind == 6 {
                    for c in 0..3 {
                        p[c] = l * p[3];
                    }
                }
            }
            for v in p.iter_mut() {
                *v *= k;
            }
        }
    });
}

/// Long Shadow: an extruded, optionally fading shadow behind the layer's alpha. The per-pixel
/// step distance to the shape along the shadow direction is found by doubling (min-plus).
pub fn long_shadow(img: &mut Image, e: &EffectInstance, cx: &FxCtx) {
    let ang = (fv(e, "angle", cx) as f64).to_radians();
    let len = fv(e, "length", cx) * cx.px_scale;
    if len < 0.5 {
        return;
    }
    let col = lin(cv(e, "color", cx));
    let op = fv(e, "opacity", cx) / 100.0;
    let fade = bv(e, "fade");
    let only = bv(e, "only");
    let (dx, dy) = (ang.sin() as f32, -ang.cos() as f32);
    let (w, h) = (img.w, img.h);
    let inf = f32::INFINITY;
    let mut d: Vec<f32> = img.px.par_chunks(4).map(|p| if p[3] > 0.5 { 0.0 } else { inf }).collect();
    let mut s = 1.0f32;
    while s <= len * 2.0 {
        let prev = d.clone();
        d.par_chunks_mut(w).enumerate().for_each(|(y, row)| {
            for (x, v) in row.iter_mut().enumerate() {
                let (sx, sy) = ((x as f32 - dx * s).round(), (y as f32 - dy * s).round());
                if sx >= 0.0 && sy >= 0.0 && (sx as usize) < w && (sy as usize) < h {
                    let q = prev[sy as usize * w + sx as usize] + s;
                    if q < *v {
                        *v = q;
                    }
                }
            }
        });
        s *= 2.0;
    }
    let mut sh: Vec<f32> = d.par_iter().map(|&v| if v <= len { (if fade { 1.0 - v / len } else { 1.0 }) * op } else { 0.0 }).collect();
    sh = blur_plane(&sh, w, h, 0.6);
    img.px.par_chunks_mut(4).zip(sh.par_iter()).for_each(|(p, &a)| {
        let mut o = [col[0] * a, col[1] * a, col[2] * a, a];
        if !only {
            over(&mut o, [p[0], p[1], p[2], p[3]]);
        }
        p.copy_from_slice(&o);
    });
}

/// 1-D squared Euclidean distance transform: lower envelope of parabolas (Felzenszwalb &
/// Huttenlocher, "Distance Transforms of Sampled Functions", 2012).
fn edt_1d(f: &[f64], out: &mut [f64]) {
    let n = f.len();
    if n == 0 {
        return;
    }
    let mut v = vec![0usize; n];
    let mut z = vec![0f64; n + 1];
    let mut k = 0usize;
    z[0] = f64::NEG_INFINITY;
    z[1] = f64::INFINITY;
    let isect = |q: usize, p: usize| ((f[q] + (q * q) as f64) - (f[p] + (p * p) as f64)) / (2.0 * q as f64 - 2.0 * p as f64);
    for q in 1..n {
        let mut s = isect(q, v[k]);
        while s <= z[k] {
            k -= 1;
            s = isect(q, v[k]);
        }
        k += 1;
        v[k] = q;
        z[k] = s;
        z[k + 1] = f64::INFINITY;
    }
    k = 0;
    for (q, o) in out.iter_mut().enumerate() {
        while z[k + 1] < q as f64 {
            k += 1;
        }
        let p = v[k];
        *o = (q as f64 - p as f64).powi(2) + f[p];
    }
}

/// Euclidean distance from every pixel to the nearest pixel where `inside` is true.
pub(crate) fn distance_to(inside: &[bool], w: usize, h: usize) -> Vec<f32> {
    let big = 1e12f64;
    let mut g: Vec<f64> = inside.iter().map(|&b| if b { 0.0 } else { big }).collect();
    g.par_chunks_mut(w).for_each(|row| {
        let f = row.to_vec();
        edt_1d(&f, row);
    });
    let cols: Vec<Vec<f64>> = (0..w)
        .into_par_iter()
        .map(|x| {
            let f: Vec<f64> = (0..h).map(|y| g[y * w + x]).collect();
            let mut o = vec![0f64; h];
            edt_1d(&f, &mut o);
            o
        })
        .collect();
    let mut out = vec![0f32; w * h];
    for (x, c) in cols.iter().enumerate() {
        for y in 0..h {
            out[y * w + x] = c[y].sqrt() as f32;
        }
    }
    out
}

/// Stroke: a solid outline around the layer's alpha (outside, centred or inside the edge).
pub fn stroke(img: &mut Image, e: &EffectInstance, cx: &FxCtx) {
    let width = fv(e, "width", cx) * cx.px_scale;
    if width <= 0.0 {
        return;
    }
    let col = lin(cv(e, "color", cx));
    let op = fv(e, "opacity", cx) / 100.0;
    let pos = chv(e, "position");
    let only = bv(e, "only");
    // pad by one pixel so the frame border counts as an edge
    let (w, h) = (img.w + 2, img.h + 2);
    let mut inside = vec![false; w * h];
    for y in 0..img.h {
        for x in 0..img.w {
            inside[(y + 1) * w + x + 1] = img.px[(y * img.w + x) * 4 + 3] > 0.5;
        }
    }
    let outside: Vec<bool> = inside.iter().map(|b| !b).collect();
    let d_out = distance_to(&inside, w, h);
    let d_in = distance_to(&outside, w, h);
    let iw = img.w;
    img.px.par_chunks_mut(iw * 4).enumerate().for_each(|(y, row)| {
        for x in 0..iw {
            let i = (y + 1) * w + x + 1;
            // signed distance to the edge, positive outside (pixel centres sit ±0.5 from it)
            let s = if inside[i] { -(d_in[i] - 0.5) } else { d_out[i] - 0.5 };
            let (lo, hi) = match pos {
                0 => (0.0, width),
                1 => (-width / 2.0, width / 2.0),
                _ => (-width, 0.0),
            };
            let k = ((s - lo + 0.5).clamp(0.0, 1.0)).min((hi - s + 0.5).clamp(0.0, 1.0)) * op;
            let p = &mut row[x * 4..x * 4 + 4];
            if only {
                p.copy_from_slice(&[col[0] * k, col[1] * k, col[2] * k, k]);
            } else if k > 0.0 {
                over(p, [col[0] * k, col[1] * k, col[2] * k, k]);
            }
        }
    });
}

/// Gradient (26.2): linear / radial / reflected / diamond ramp with midpoint and scatter.
pub fn gradient(img: &mut Image, e: &EffectInstance, cx: &FxCtx) {
    let s = pv(e, "start", cx, img);
    let en = pv(e, "end", cx, img);
    let sc = cv(e, "start_color", cx);
    let ec = cv(e, "end_color", cx);
    let shape = chv(e, "shape");
    let scatter = fv(e, "scatter", cx) / 100.0;
    let mid = (fv(e, "midpoint", cx) / 100.0).clamp(0.01, 0.99);
    let gamma = (0.5f32).ln() / mid.ln();
    let blend = fv(e, "blend", cx) / 100.0;
    let (vx, vy) = ((en.x - s.x) as f32, (en.y - s.y) as f32);
    let l2 = (vx * vx + vy * vy).max(1e-6);
    let l = l2.sqrt();
    generate(img, |x, y, p| {
        let (dx, dy) = (x - s.x as f32, y - s.y as f32);
        let along = (dx * vx + dy * vy) / l2;
        let across = (-dx * vy + dy * vx) / l2;
        let mut t = match shape {
            1 => (dx * dx + dy * dy).sqrt() / l,
            2 => along.abs(),
            3 => along.abs() + across.abs(),
            _ => along,
        };
        if scatter > 0.0 {
            t += (hash01(x as i64, y as i64, 0, 13) - 0.5) * scatter * 0.1;
        }
        let t = t.clamp(0.0, 1.0).powf(gamma);
        let c = dec(lerp3([sc[0], sc[1], sc[2]], [ec[0], ec[1], ec[2]], t));
        let a = sc[3] + (ec[3] - sc[3]) * t;
        let g = [c[0] * a, c[1] * a, c[2] * a, a];
        [0, 1, 2, 3].map(|k| g[k] + (p[k] - g[k]) * blend)
    });
}

/// Block Dissolve (Legacy): random blocks disappear as Transition Completion rises.
pub fn block_dissolve(img: &mut Image, e: &EffectInstance, cx: &FxCtx) {
    let c = fv(e, "completion", cx) / 100.0;
    if c <= 0.0 {
        return;
    }
    let bw = (fv(e, "block_w", cx) * cx.px_scale).max(1.0);
    let bh = (fv(e, "block_h", cx) * cx.px_scale).max(1.0);
    let fe = fv(e, "feather", cx) / 100.0 * 0.5;
    let soft = bv(e, "soft");
    let w = img.w;
    img.px.par_chunks_mut(w * 4).enumerate().for_each(|(y, row)| {
        for x in 0..w {
            let r = hash01((x as f32 / bw) as i64, (y as f32 / bh) as i64, 0, 41);
            let a = if soft && fe > 0.0 {
                smoothstep(c - fe, c + fe, r)
            } else if r >= c {
                1.0
            } else {
                0.0
            };
            if c >= 1.0 || a < 1.0 {
                let a = if c >= 1.0 { 0.0 } else { a };
                for v in &mut row[x * 4..x * 4 + 4] {
                    *v *= a;
                }
            }
        }
    });
}

/// Alpha from a wipe source g (0..1): pixels whose g is below the completion vanish first.
#[inline]
fn wipe_alpha(g: f32, c: f32, soft: f32) -> f32 {
    if soft <= 1e-4 {
        return if c <= 0.0 || g >= c { 1.0 } else { 0.0 };
    }
    let t = c * (1.0 + soft);
    ((g - t + soft) / soft).clamp(0.0, 1.0)
}

/// Gradient Wipe (Legacy): reveals by the luminance of a gradient layer (another track, or this
/// layer's own luminance).
pub fn gradient_wipe(img: &mut Image, e: &EffectInstance, cx: &FxCtx) -> crate::Result<()> {
    let c = fv(e, "completion", cx) / 100.0;
    if c <= 0.0 {
        return Ok(());
    }
    let soft = fv(e, "softness", cx) / 100.0;
    let invert = bv(e, "invert");
    let layer = chv(e, "layer") as usize;
    let placement = chv(e, "placement");
    let track = if layer > 0 {
        match cx.env {
            Some(env) => env.track(layer - 1)?.map(|t| (t, env.layer_to_output())),
            None => None,
        }
    } else {
        None
    };
    let own = img.clone();
    let w = img.w;
    let (iw, ih) = (img.w as f32, img.h as f32);
    img.px.par_chunks_mut(w * 4).enumerate().for_each(|(y, row)| {
        for x in 0..w {
            let (px, py) = (x as f32 + 0.5, y as f32 + 0.5);
            let s = match &track {
                Some((t, m)) => match placement {
                    0 => t.get(x % t.w, y % t.h),
                    1 => t.sample_bilinear(px - iw / 2.0 + t.w as f32 / 2.0, py - ih / 2.0 + t.h as f32 / 2.0),
                    _ => {
                        let o = m.apply(Vec2::new(px as f64, py as f64));
                        t.sample_bilinear_clamped(o.x as f32, o.y as f32)
                    }
                },
                None => own.get(x, y),
            };
            let mut g = filmcraft_color::linear_to_srgb(luma([s[0], s[1], s[2]]).clamp(0.0, 1.0));
            if invert {
                g = 1.0 - g;
            }
            let a = wipe_alpha(g, c, soft);
            for v in &mut row[x * 4..x * 4 + 4] {
                *v *= a;
            }
        }
    });
    Ok(())
}

/// Linear Wipe (Legacy): a straight edge sweeps across at Wipe Angle.
pub fn linear_wipe(img: &mut Image, e: &EffectInstance, cx: &FxCtx) {
    let c = fv(e, "completion", cx) / 100.0;
    if c <= 0.0 {
        return;
    }
    let ang = fv(e, "angle", cx).to_radians();
    let (dx, dy) = (ang.sin(), -ang.cos());
    let (w, h) = (img.w as f32, img.h as f32);
    // extent of the frame along the wipe direction
    let corners = [(0.0, 0.0), (w, 0.0), (0.0, h), (w, h)].map(|(x, y)| x * dx + y * dy);
    let (lo, hi) = (corners.iter().copied().fold(f32::INFINITY, f32::min), corners.iter().copied().fold(f32::NEG_INFINITY, f32::max));
    let ext = (hi - lo).max(1.0);
    let soft = fv(e, "feather", cx) * cx.px_scale / ext;
    let wi = img.w;
    img.px.par_chunks_mut(wi * 4).enumerate().for_each(|(y, row)| {
        for x in 0..wi {
            let g = ((x as f32 + 0.5) * dx + (y as f32 + 0.5) * dy - lo) / ext;
            let a = wipe_alpha(g, c, soft);
            for v in &mut row[x * 4..x * 4 + 4] {
                *v *= a;
            }
        }
    });
}

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> f32 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        ((z ^ (z >> 31)) >> 40) as f32 / (1u64 << 24) as f32
    }
    fn signed(&mut self) -> f32 {
        self.next() * 2.0 - 1.0
    }
}

/// A bolt between two points: midpoint displacement, `levels` of detail.
fn bolt(a: (f32, f32), b: (f32, f32), segs: usize, amp: f32, levels: usize, detail_amp: f32, rng: &mut Rng) -> Vec<(f32, f32)> {
    let mut pts = vec![a];
    let (dx, dy) = (b.0 - a.0, b.1 - a.1);
    let len = (dx * dx + dy * dy).sqrt().max(1e-3);
    let (nx, ny) = (-dy / len, dx / len);
    for i in 1..segs {
        let t = i as f32 / segs as f32;
        let o = rng.signed() * amp;
        pts.push((a.0 + dx * t + nx * o, a.1 + dy * t + ny * o));
    }
    pts.push(b);
    let mut a2 = amp / segs as f32 * 2.0 * detail_amp.max(0.05) * 4.0;
    for _ in 0..levels {
        let mut np = Vec::with_capacity(pts.len() * 2);
        for w in pts.windows(2) {
            let (p, q) = (w[0], w[1]);
            let (ex, ey) = (q.0 - p.0, q.1 - p.1);
            let l = (ex * ex + ey * ey).sqrt().max(1e-3);
            let o = rng.signed() * a2.min(l * 0.5);
            np.push(p);
            np.push(((p.0 + q.0) / 2.0 - ey / l * o, (p.1 + q.1) / 2.0 + ex / l * o));
        }
        if let Some(&last) = pts.last() {
            np.push(last);
        }
        pts = np;
        a2 *= 0.5;
    }
    pts
}

/// Lightning (obsolete): a seeded, re-striking bolt with branches, glow and core colours.
pub fn lightning(img: &mut Image, e: &EffectInstance, cx: &FxCtx) {
    let s = pv(e, "start", cx, img);
    let en = pv(e, "end", cx, img);
    let segs = fv(e, "segments", cx).clamp(1.0, 100.0) as usize;
    let dist = ((en.x - s.x).hypot(en.y - s.y)) as f32;
    let amp = fv(e, "amplitude", cx) / 100.0 * dist * 0.5;
    let levels = fv(e, "detail", cx).clamp(0.0, 6.0) as usize;
    let damp = fv(e, "detail_amplitude", cx);
    let branching = fv(e, "branching", cx);
    let speed = fv(e, "speed", cx) as f64;
    let width = (fv(e, "width", cx) * cx.px_scale).max(0.5);
    let core = fv(e, "core", cx);
    let outc = lin(cv(e, "outside", cx));
    let inc = lin(cv(e, "inside", cx));
    let mode = chv(e, "mode");
    let strike = (cx.seconds * speed / 10.0).floor() as u64;
    let mut rng = Rng(fv(e, "seed", cx).max(0.0) as u64 * 7919 + strike);
    let main = bolt((s.x as f32, s.y as f32), (en.x as f32, en.y as f32), segs, amp, levels, damp, &mut rng);
    let mut lines: Vec<(Vec<(f32, f32)>, f32)> = vec![(main.clone(), width)];
    for i in 1..main.len().saturating_sub(1) {
        if rng.next() < branching * 0.25 {
            let p = main[i];
            let (dx, dy) = (main[i + 1].0 - main[i - 1].0, main[i + 1].1 - main[i - 1].1);
            let a = dy.atan2(dx) + rng.signed() * 0.6;
            let l = dist * (0.15 + rng.next() * 0.25);
            let q = (p.0 + a.cos() * l, p.1 + a.sin() * l);
            lines.push((bolt(p, q, 4, amp * 0.4, levels.min(3), damp, &mut rng), width * 0.6));
        }
    }
    let (w, h) = (img.w, img.h);
    let mut dmap = vec![f32::INFINITY; w * h];
    for (pts, lw) in &lines {
        for seg in pts.windows(2) {
            let (a, b) = (seg[0], seg[1]);
            let r = lw * 3.0;
            let (x0, x1) = ((a.0.min(b.0) - r).floor().max(0.0) as usize, ((a.0.max(b.0) + r).ceil().max(0.0) as usize).min(w));
            let (y0, y1) = ((a.1.min(b.1) - r).floor().max(0.0) as usize, ((a.1.max(b.1) + r).ceil().max(0.0) as usize).min(h));
            let (ex, ey) = (b.0 - a.0, b.1 - a.1);
            let l2 = (ex * ex + ey * ey).max(1e-6);
            for y in y0..y1 {
                for x in x0..x1 {
                    let (px, py) = (x as f32 + 0.5, y as f32 + 0.5);
                    let t = (((px - a.0) * ex + (py - a.1) * ey) / l2).clamp(0.0, 1.0);
                    let d = ((px - a.0 - ex * t).powi(2) + (py - a.1 - ey * t).powi(2)).sqrt() / lw;
                    let i = y * w + x;
                    if d < dmap[i] {
                        dmap[i] = d;
                    }
                }
            }
        }
    }
    img.px.par_chunks_mut(4).zip(dmap.par_iter()).for_each(|(p, &d)| {
        if !d.is_finite() {
            return;
        }
        let glow = (-(d * 1.6)).exp() * 0.9;
        let c = 1.0 - smoothstep(core * 0.5, core * 0.5 + 0.2, d);
        let col = lerp3(outc.map(|v| v * glow), inc, c);
        let a = glow.max(c).min(1.0);
        let base = Image::unpremul([p[0], p[1], p[2], p[3]]);
        let o = match mode {
            0 => {
                let mut q = [p[0], p[1], p[2], p[3]];
                over(&mut q, [col[0] * a, col[1] * a, col[2] * a, a]);
                p.copy_from_slice(&q);
                return;
            }
            m => blend_simple(m, base, col),
        };
        let na = p[3] + (1.0 - p[3]) * a;
        let mixed = lerp3(base, o, a);
        p.copy_from_slice(&[mixed[0] * na, mixed[1] * na, mixed[2] * na, na]);
    });
}

/// Cell Pattern (obsolete): Worley (cellular) noise patterns.
pub fn cell_pattern(img: &mut Image, e: &EffectInstance, cx: &FxCtx) {
    let pat = chv(e, "pattern");
    let invert = bv(e, "invert");
    let contrast = fv(e, "contrast", cx) / 100.0;
    let overflow = chv(e, "overflow");
    let disperse = fv(e, "disperse", cx).clamp(0.0, 1.5);
    let size = (fv(e, "size", cx) * cx.px_scale).max(1.0);
    let off = offv(e, "offset", cx);
    let evo = if pat == 3 { 0.0 } else { fv(e, "evolution", cx).to_radians() };
    let seed = fv(e, "seed", cx).max(0.0) as u64;
    generate(img, |x, y, _| {
        let (u, v) = ((x + off.x as f32) / size, (y + off.y as f32) / size);
        let (ci, cj) = (u.floor() as i64, v.floor() as i64);
        let (mut f1, mut f2, mut id) = (f32::INFINITY, f32::INFINITY, 0.0f32);
        for dj in -1..=1 {
            for di in -1..=1 {
                let (gi, gj) = (ci + di, cj + dj);
                let ph = hash01(gi, gj, 9, seed) * std::f32::consts::TAU;
                let jx = 0.5 + (hash01(gi, gj, 0, seed) - 0.5) * disperse + (evo + ph).cos() * 0.15 * disperse;
                let jy = 0.5 + (hash01(gi, gj, 1, seed) - 0.5) * disperse + (evo + ph).sin() * 0.15 * disperse;
                let d = ((gi as f32 + jx - u).powi(2) + (gj as f32 + jy - v).powi(2)).sqrt();
                if d < f1 {
                    f2 = f1;
                    f1 = d;
                    id = hash01(gi, gj, 2, seed);
                } else if d < f2 {
                    f2 = d;
                }
            }
        }
        let mut val = match pat {
            0 => (1.0 - f1 * 1.4).max(0.0),
            1 => (f2 - f1) * 2.0,
            2 | 3 => id,
            4 => id * smoothstep(0.0, 0.12, f2 - f1),
            5 => (f2 - f1) / (f2 + f1).max(1e-4) * 2.0,
            6 => ((f2 - f1) * 2.0 + id) * 0.5,
            _ => 1.0 - smoothstep(0.0, 0.12, (f1 - 0.35).abs()),
        };
        val = (val - 0.5) * contrast + 0.5;
        val = match overflow {
            1 => 1.0 / (1.0 + (-(val - 0.5) * 4.0).exp()),
            2 => {
                let m = val.rem_euclid(2.0);
                if m > 1.0 { 2.0 - m } else { m }
            }
            _ => val.clamp(0.0, 1.0),
        };
        if invert {
            val = 1.0 - val;
        }
        let g = filmcraft_color::srgb_to_linear(val);
        [g, g, g, 1.0]
    });
}

/// Checkerboard (obsolete): a checkerboard of Color and transparency.
pub fn checkerboard(img: &mut Image, e: &EffectInstance, cx: &FxCtx) {
    let a = pv(e, "anchor", cx, img);
    let cw = (fv(e, "width", cx) * cx.px_scale).max(1.0);
    let ch = if bv(e, "square") { cw } else { (fv(e, "height", cx) * cx.px_scale).max(1.0) };
    let fe = fv(e, "feather", cx) * cx.px_scale;
    let col = lin(cv(e, "color", cx));
    let op = fv(e, "opacity", cx) / 100.0;
    let mode = chv(e, "mode");
    generate(img, |x, y, p| {
        let (u, v) = ((x - a.x as f32) / cw, (y - a.y as f32) / ch);
        let par = ((u.floor() as i64 + v.floor() as i64).rem_euclid(2)) as f32;
        let edge = {
            let du = (u - u.round()).abs() * cw;
            let dv = (v - v.round()).abs() * ch;
            if fe > 0.0 { smoothstep(0.0, fe, du.min(dv)) } else { 1.0 }
        };
        let k = par * edge * op;
        match mode {
            0 => [col[0] * k, col[1] * k, col[2] * k, k],
            1 => {
                let mut q = p;
                over(&mut q, [col[0] * k, col[1] * k, col[2] * k, k]);
                q
            }
            m => {
                let base = Image::unpremul(p);
                let bm = [0u32, 0, 1, 3, 2, 4][m.min(5) as usize];
                let o = lerp3(base, blend_simple(bm, base, col), k);
                [o[0] * p[3], o[1] * p[3], o[2] * p[3], p[3]]
            }
        }
    });
}

/// Ellipse (obsolete): an anti-aliased elliptical ring, Inside Color at its core.
pub fn ellipse(img: &mut Image, e: &EffectInstance, cx: &FxCtx) {
    let c = pv(e, "center", cx, img);
    let (rx, ry) = ((fv(e, "width", cx) * cx.px_scale / 2.0).max(0.5), (fv(e, "height", cx) * cx.px_scale / 2.0).max(0.5));
    let th = (fv(e, "thickness", cx) * cx.px_scale).max(0.5);
    let soft = fv(e, "softness", cx) / 100.0;
    let inc = lin(cv(e, "inside", cx));
    let outc = lin(cv(e, "outside", cx));
    let comp = bv(e, "composite");
    let rmin = rx.min(ry);
    generate(img, |x, y, p| {
        let (dx, dy) = ((x - c.x as f32) / rx, (y - c.y as f32) / ry);
        let d = ((dx * dx + dy * dy).sqrt() - 1.0).abs() * rmin;
        let half = th / 2.0;
        let a = 1.0 - smoothstep(half * (1.0 - soft) - 0.5, half + 0.5, d);
        let inner = 1.0 - smoothstep(0.0, half, d);
        let col = lerp3(outc, inc, inner);
        let s = [col[0] * a, col[1] * a, col[2] * a, a];
        if comp {
            let mut q = p;
            over(&mut q, s);
            q
        } else {
            s
        }
    });
}

/// Paint Bucket (obsolete): flood fill from Fill Point within Tolerance.
pub fn paint_bucket(img: &mut Image, e: &EffectInstance, cx: &FxCtx) {
    let pt = pv(e, "point", cx, img);
    let (w, h) = (img.w, img.h);
    let (sx, sy) = (pt.x.floor() as isize, pt.y.floor() as isize);
    if sx < 0 || sy < 0 || sx as usize >= w || sy as usize >= h {
        return;
    }
    let sel = chv(e, "selector");
    let tol = fv(e, "tolerance", cx) / 255.0;
    let col = lin(cv(e, "color", cx));
    let op = fv(e, "opacity", cx) / 100.0;
    let mode = chv(e, "mode");
    let px = |i: usize| -> [f32; 4] {
        let p = &img.px[i * 4..i * 4 + 4];
        let c = enc(Image::unpremul([p[0], p[1], p[2], p[3]]));
        [c[0], c[1], c[2], p[3]]
    };
    let seed = px(sy as usize * w + sx as usize);
    let similar = |q: [f32; 4]| -> bool {
        match sel {
            0 => (0..4).all(|k| (q[k] - seed[k]).abs() <= tol),
            1 => (0..3).all(|k| (q[k] - seed[k]).abs() <= tol),
            2 => q[3] <= tol.max(1e-3),
            _ => (q[3] - seed[3]).abs() <= tol,
        }
    };
    let mut fill = vec![0f32; w * h];
    let mut queue = VecDeque::new();
    let start = sy as usize * w + sx as usize;
    if similar(seed) {
        fill[start] = 1.0;
        queue.push_back(start);
    }
    while let Some(i) = queue.pop_front() {
        let (x, y) = (i % w, i / w);
        let mut visit = |j: usize| {
            if fill[j] == 0.0 && similar(px(j)) {
                fill[j] = 1.0;
                queue.push_back(j);
            }
        };
        if x > 0 {
            visit(i - 1);
        }
        if x + 1 < w {
            visit(i + 1);
        }
        if y > 0 {
            visit(i - w);
        }
        if y + 1 < h {
            visit(i + w);
        }
    }
    if bv(e, "invert") {
        fill.iter_mut().for_each(|v| *v = 1.0 - *v);
    }
    let fill = blur_plane(&fill, w, h, 0.5);
    img.px.par_chunks_mut(4).zip(fill.par_iter()).for_each(|(p, &f)| {
        let k = f * op;
        if k <= 0.0 {
            return;
        }
        let s = [col[0] * k, col[1] * k, col[2] * k, k];
        match mode {
            1 => {
                let mut q = s;
                over(&mut q, [p[0], p[1], p[2], p[3]]);
                p.copy_from_slice(&q);
            }
            2..=4 => {
                let base = Image::unpremul([p[0], p[1], p[2], p[3]]);
                let bm = [0u32, 0, 1, 3, 2][mode as usize];
                let o = lerp3(base, blend_simple(bm, base, col), k);
                for c in 0..3 {
                    p[c] = o[c] * p[3];
                }
            }
            _ => over(p, s),
        }
    });
}

/// Write-on (obsolete): paints the animated Brush Position's path up to now.
pub fn write_on(img: &mut Image, e: &EffectInstance, cx: &FxCtx) {
    let Some(param) = e.param("brush") else { return };
    let col = lin(cv(e, "color", cx));
    let r = (fv(e, "size", cx) * cx.px_scale / 2.0).max(0.5);
    let hard = fv(e, "hardness", cx) / 100.0;
    let op = fv(e, "opacity", cx) / 100.0;
    let stroke_len = fv(e, "stroke_length", cx) as f64;
    let spacing = (fv(e, "spacing", cx) as f64).max(0.001);
    let style = chv(e, "style");
    let elapsed = cx.seconds.max(0.0);
    let n = ((elapsed / spacing).floor() as usize).min(20_000);
    let first = if stroke_len > 0.0 { ((elapsed - stroke_len) / spacing).ceil().max(0.0) as usize } else { 0 };
    let to_px = |t: f64| {
        let mt = cx.t - Tick::from_seconds_f64(elapsed - t);
        let v = param.vec2_at(mt);
        let (fx, fy) = (
            if v.x.is_nan() { img.w as f64 / 2.0 } else { v.x * cx.px_scale as f64 },
            if v.y.is_nan() { img.h as f64 / 2.0 } else { v.y * cx.px_scale as f64 },
        );
        (fx as f32, fy as f32)
    };
    let mut stamps: Vec<(f32, f32)> = Vec::new();
    let mut prev: Option<(f32, f32)> = None;
    for i in first..=n {
        let p = to_px(i as f64 * spacing);
        if let Some(q) = prev {
            let d = ((p.0 - q.0).powi(2) + (p.1 - q.1).powi(2)).sqrt();
            let steps = (d / (r * 0.5)).ceil() as usize;
            for s in 1..steps.min(2000) {
                let t = s as f32 / steps as f32;
                stamps.push((q.0 + (p.0 - q.0) * t, q.1 + (p.1 - q.1) * t));
            }
        }
        stamps.push(p);
        prev = Some(p);
    }
    let (w, h) = (img.w, img.h);
    let mut cov = vec![0f32; w * h];
    for (sx, sy) in stamps {
        let (x0, x1) = ((sx - r - 1.0).floor().max(0.0) as usize, ((sx + r + 1.0).ceil().max(0.0) as usize).min(w));
        let (y0, y1) = ((sy - r - 1.0).floor().max(0.0) as usize, ((sy + r + 1.0).ceil().max(0.0) as usize).min(h));
        for y in y0..y1 {
            for x in x0..x1 {
                let d = ((x as f32 + 0.5 - sx).powi(2) + (y as f32 + 0.5 - sy).powi(2)).sqrt();
                let a = 1.0 - smoothstep(r * hard - 0.5, r + 0.5, d);
                let i = y * w + x;
                if a > cov[i] {
                    cov[i] = a;
                }
            }
        }
    }
    img.px.par_chunks_mut(4).zip(cov.par_iter()).for_each(|(p, &c)| {
        let k = c * op;
        match style {
            1 => p.copy_from_slice(&[col[0] * k, col[1] * k, col[2] * k, k]),
            2 => {
                for v in p.iter_mut() {
                    *v *= k;
                }
            }
            _ => over(p, [col[0] * k, col[1] * k, col[2] * k, k]),
        }
    });
}
