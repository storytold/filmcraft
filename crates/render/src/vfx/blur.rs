//! Blur & Sharpen effects.

use filmcraft_project::EffectInstance;
use rayon::prelude::*;

use super::*;
use crate::effects::gaussian;

/// Radius of a regular `n`-gon (circumradius 1, rotated by `rot`) in direction `theta`;
/// `n == 0` is a circle.
fn poly_radius(n: u32, theta: f32, rot: f32) -> f32 {
    if n == 0 {
        return 1.0;
    }
    let seg = std::f32::consts::TAU / n as f32;
    let a = (theta - rot).rem_euclid(seg) - seg / 2.0;
    (std::f32::consts::PI / n as f32).cos() / a.cos()
}

/// Bokeh Blur: a lens-iris shaped gather blur with highlight boost (bright points bloom into
/// iris-shaped discs). Large radii run on a reduced image (the result is smooth).
pub fn bokeh(img: &mut Image, e: &EffectInstance, cx: &FxCtx) {
    let r = fv(e, "amount", cx) * cx.px_scale;
    if r < 0.5 {
        return;
    }
    let sides = [0u32, 3, 4, 5, 6, 8][chv(e, "shape").min(5) as usize];
    let rot = fv(e, "rotation", cx).to_radians();
    let boost = fv(e, "highlight", cx) / 100.0 * 6.0;
    let th = fv(e, "threshold", cx) / 100.0;
    let (fw, fh) = (img.w, img.h);
    let mut work = img.clone();
    let mut down = 1.0f32;
    while r / down > 12.0 && work.w >= 8 && work.h >= 8 {
        work = work.downsample2();
        down *= 2.0;
    }
    let rr = r / down;
    // boost highlights (energy, not weight: bright points stay bright after averaging)
    if boost > 0.0 {
        work.px.par_chunks_mut(4).for_each(|p| {
            if p[3] <= 1e-6 {
                return;
            }
            let l = luma(enc(Image::unpremul([p[0], p[1], p[2], p[3]])));
            let k = 1.0 + boost * smoothstep(th, 1.0, l);
            for v in &mut p[..3] {
                *v *= k;
            }
        });
    }
    let rings = (rr.ceil() as usize).clamp(1, 12);
    let mut taps: Vec<(f32, f32)> = vec![(0.0, 0.0)];
    for ring in 1..=rings {
        let rad = rr * ring as f32 / rings as f32;
        let n = (ring * 8).max(6);
        for i in 0..n {
            let th = std::f32::consts::TAU * (i as f32 + 0.5 * (ring % 2) as f32) / n as f32;
            let pr = rad * poly_radius(sides, th, rot);
            taps.push((th.cos() * pr, th.sin() * pr));
        }
    }
    let inv = 1.0 / taps.len() as f32;
    let src = work.clone();
    let w = work.w;
    work.px.par_chunks_mut(w * 4).enumerate().for_each(|(y, row)| {
        for x in 0..w {
            let mut acc = [0f32; 4];
            for &(dx, dy) in &taps {
                let p = src.sample_bilinear_clamped(x as f32 + 0.5 + dx, y as f32 + 0.5 + dy);
                for k in 0..4 {
                    acc[k] += p[k];
                }
            }
            for k in 0..4 {
                row[x * 4 + k] = acc[k] * inv;
            }
        }
    });
    *img = if down > 1.0 { work.transformed(fw, fh, &Affine::scale(fw as f64 / work.w as f64, fh as f64 / work.h as f64)) } else { work };
}

/// Blur one channel of an image (0..3) with a Gaussian.
fn blur_channel(img: &mut Image, ch: usize, sx: f32, sy: f32, repeat: bool) {
    if sx <= 0.3 && sy <= 0.3 {
        return;
    }
    let mut plane = Image { w: img.w, h: img.h, px: img.px.as_chunks::<4>().0.iter().flat_map(|p| [p[ch], 0.0, 0.0, 0.0]).collect() };
    gaussian(&mut plane, sx, sy, repeat);
    img.px.par_chunks_mut(4).zip(plane.px.par_chunks(4)).for_each(|(p, q)| p[ch] = q[0]);
}

/// Channel Blur: independent Gaussian blur per R, G, B and alpha.
pub fn channel_blur(img: &mut Image, e: &EffectInstance, cx: &FxCtx) {
    let dims = chv(e, "dimensions");
    let repeat = bv(e, "repeat_edge");
    for (ch, id) in ["red", "green", "blue", "alpha"].iter().enumerate() {
        let s = fv(e, id, cx) * cx.px_scale * 0.5;
        blur_channel(img, ch, if dims == 2 { 0.0 } else { s }, if dims == 1 { 0.0 } else { s }, repeat);
    }
}

/// Mix between a stack of increasingly blurred copies by a per-pixel amount 0..1.
fn variable_blur(img: &mut Image, max_sigma: f32, amount: impl Fn(usize, usize) -> f32 + Sync) {
    if max_sigma <= 0.3 {
        return;
    }
    let levels = [0.0f32, 0.125, 0.25, 0.5, 1.0];
    let stack: Vec<Image> = levels
        .par_iter()
        .map(|&k| {
            let mut c = img.clone();
            if k > 0.0 {
                gaussian(&mut c, max_sigma * k, max_sigma * k, true);
            }
            c
        })
        .collect();
    let w = img.w;
    img.px.par_chunks_mut(w * 4).enumerate().for_each(|(y, row)| {
        for x in 0..w {
            let a = amount(x, y).clamp(0.0, 1.0);
            let i = levels.partition_point(|&l| l <= a).clamp(1, levels.len() - 1);
            let (l0, l1) = (levels[i - 1], levels[i]);
            let t = ((a - l0) / (l1 - l0)).clamp(0.0, 1.0);
            let (p0, p1) = (stack[i - 1].get(x, y), stack[i].get(x, y));
            for k in 0..4 {
                row[x * 4 + k] = p0[k] + (p1[k] - p0[k]) * t;
            }
        }
    });
}

/// Compound Blur: blur amount per pixel from a blur layer's luminance (another track, or this
/// layer's own luminance when the Blur Layer is None).
pub fn compound(img: &mut Image, e: &EffectInstance, cx: &FxCtx) -> crate::Result<()> {
    let max = fv(e, "max", cx) * cx.px_scale * 0.5;
    let invert = bv(e, "invert");
    let layer = chv(e, "layer") as usize;
    let track = if layer > 0 {
        match cx.env {
            Some(env) => env.track(layer - 1)?.map(|t| (t, env.layer_to_output())),
            None => None,
        }
    } else {
        None
    };
    let stretch = bv(e, "stretch");
    let own = img.clone();
    let (iw, ih) = (img.w as f64, img.h as f64);
    variable_blur(img, max, |x, y| {
        let l = match &track {
            Some((t, m)) => {
                let p = if stretch {
                    m.apply(Vec2::new(x as f64 + 0.5, y as f64 + 0.5))
                } else {
                    Vec2::new((x as f64 + 0.5) / iw * t.w as f64, (y as f64 + 0.5) / ih * t.h as f64)
                };
                let s = t.sample_bilinear_clamped(p.x as f32, p.y as f32);
                filmcraft_color::linear_to_srgb(luma([s[0], s[1], s[2]]).clamp(0.0, 1.0))
            }
            None => {
                let s = own.get(x, y);
                filmcraft_color::linear_to_srgb(luma([s[0], s[1], s[2]]).clamp(0.0, 1.0))
            }
        };
        if invert { 1.0 - l } else { l }
    });
    Ok(())
}

/// Focus Blur: sharp inside a radial or linear focus area, blurring smoothly outside it.
pub fn focus(img: &mut Image, e: &EffectInstance, cx: &FxCtx) {
    let amount = fv(e, "amount", cx) * cx.px_scale * 0.5;
    let c = pv(e, "center", cx, img);
    let linear = chv(e, "shape") == 1;
    let (s, co) = (fv(e, "angle", cx) as f64).to_radians().sin_cos();
    let size = fv(e, "size", cx) as f64 * cx.px_scale as f64 * 0.5;
    let feather = (fv(e, "feather", cx) as f64 * cx.px_scale as f64).max(1.0);
    let show = bv(e, "show");
    let dist = |x: usize, y: usize| -> f32 {
        let (dx, dy) = (x as f64 + 0.5 - c.x, y as f64 + 0.5 - c.y);
        let d = if linear { (-dx * s + dy * co).abs() } else { dx.hypot(dy) };
        (((d - size) / feather).clamp(0.0, 1.0)) as f32
    };
    variable_blur(img, amount, dist);
    if show {
        let w = img.w;
        img.px.par_chunks_mut(w * 4).enumerate().for_each(|(y, row)| {
            for x in 0..w {
                let k = (1.0 - dist(x, y)) * 0.35;
                let p = &mut row[x * 4..x * 4 + 4];
                over(p, [k, 0.0, 0.0, k]);
            }
        });
    }
}

/// Reduce Interlace Flicker: a vertical-only softening across fields.
pub fn interlace_flicker(img: &mut Image, e: &EffectInstance, cx: &FxCtx) {
    let s = fv(e, "softness", cx) * cx.px_scale.max(0.25);
    gaussian(img, 0.0, s, true);
}
