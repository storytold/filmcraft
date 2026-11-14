//! Blur & Sharpen effects.

use filmcraft_project::EffectInstance;
use rayon::prelude::*;

use super::{Affine, FxCtx, Image, Vec2, bv, chv, enc, fv, luma, over, smoothstep};
use crate::effects::{box_blur, gaussian, gaussian_boxes};
use crate::gpufx::{BlurLevel, FocusArea};

/// Radius of a regular `n`-gon (circumradius 1, rotated by `rot`) in direction `theta`;
/// `n == 0` is a circle.
pub(super) fn poly_radius(n: u32, theta: f32, rot: f32) -> f32 {
    if n == 0 {
        return 1.0;
    }
    let seg = std::f32::consts::TAU / n as f32;
    let a = (theta - rot).rem_euclid(seg) - seg / 2.0;
    (std::f32::consts::PI / n as f32).cos() / a.cos()
}

/// Bokeh Blur (`gpufx::FxOp::Bokeh`, evaluated there): a lens-iris shaped gather blur with
/// highlight boost (bright points bloom into iris-shaped discs), on the image halved `levels`
/// times (large radii: the result is smooth) and scaled back up.
#[allow(clippy::too_many_arguments)]
pub(crate) fn bokeh(img: &mut Image, levels: u32, boost: f32, th: f32, sides: u32, rot: f32, rr: f32, rings: u32) {
    let (fw, fh) = (img.w, img.h);
    let mut work = img.clone();
    for _ in 0..levels {
        work = work.downsample2();
    }
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
    let kernel = bokeh_kernel(sides, rot, rr, rings);
    let src = work.clone();
    let w = work.w;
    work.px.par_chunks_mut(w * 4).enumerate().for_each(|(y, row)| {
        for x in 0..w {
            let mut acc = [0f32; 4];
            for &(dx, dy, wt) in &kernel {
                let p = src.get_clamped((x as isize).saturating_add(dx as isize), (y as isize).saturating_add(dy as isize));
                for k in 0..4 {
                    acc[k] += p[k] * wt;
                }
            }
            row[x * 4..x * 4 + 4].copy_from_slice(&acc);
        }
    });
    *img = if levels > 0 { work.transformed(fw, fh, &Affine::scale(fw as f64 / work.w as f64, fh as f64 / work.h as f64)) } else { work };
}

/// How far (pixels) a Bokeh kernel offset reaches at most: more than any image is wide. A radius
/// can be far larger (a huge amount on an image too small to halve).
pub const KERNEL_REACH: i32 = 1 << 20;

/// Bokeh Blur's iris gather as whole-pixel weights (`dx`, `dy`, weight; sorted by row, then
/// column; the weights sum to 1): the mean of bilinear taps (`Image::sample_bilinear_clamped`) on
/// `rings` rings out to radius `rr` around each pixel, the shape a regular `sides`-gon rotated by
/// `rot` (`sides` 0: a circle).
///
/// A pixel's centre is a whole number and a half, so where a tap's four bilinear weights fall
/// relative to the pixel, and how much each gets, is the same for every pixel; so is clamping at
/// the image's edge, which moves a weight onto the edge pixel either way. Summing the weights per
/// offset once gives each pixel about a fifth of the reads: 441 taps at radius 10 read 1764
/// pixels, the kernel about 330. At most 4 entries a tap (2500 at 12 rings), however large `rr`;
/// offsets stay within ±[`KERNEL_REACH`], so adding one to a pixel position can't overflow.
pub fn bokeh_kernel(sides: u32, rot: f32, rr: f32, rings: u32) -> Vec<(i32, i32, f32)> {
    let rings = rings.min(12) as usize;
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
    let mut cells: Vec<(i32, i32, f32)> = Vec::with_capacity(taps.len() * 4);
    for (dx, dy) in taps {
        // a tap off at a non-finite offset reads the pixel itself
        let (dx, dy) = if dx.is_finite() && dy.is_finite() { (dx, dy) } else { (0.0, 0.0) };
        let (fx, fy) = (dx.floor(), dy.floor());
        let (tx, ty) = (dx - fx, dy - fy);
        // farther than any image is wide reads its edge pixel either way
        let reach = KERNEL_REACH as f32;
        let (x0, y0) = (fx.clamp(-reach, reach) as i32, fy.clamp(-reach, reach) as i32);
        let (x1, y1) = ((x0 + 1).min(KERNEL_REACH), (y0 + 1).min(KERNEL_REACH));
        cells.extend([(x0, y0, (1.0 - tx) * (1.0 - ty) * inv), (x1, y0, tx * (1.0 - ty) * inv), (x0, y1, (1.0 - tx) * ty * inv), (x1, y1, tx * ty * inv)]);
    }
    cells.sort_by_key(|c| (c.1, c.0));
    let mut kernel: Vec<(i32, i32, f32)> = Vec::with_capacity(cells.len());
    for (x, y, wt) in cells {
        match kernel.last_mut() {
            Some(last) if (last.0, last.1) == (x, y) => last.2 += wt,
            _ => kernel.push((x, y, wt)),
        }
    }
    kernel.retain(|c| c.2 != 0.0);
    kernel
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

/// The blur levels of Focus Blur and Compound Blur, as fractions of the maximum blur: a pixel
/// mixes the two levels around its blur amount (0..1) linearly.
pub(crate) const BLUR_LEVELS: [f32; 5] = [0.0, 0.125, 0.25, 0.5, 1.0];

/// How much level `k` of [`BLUR_LEVELS`] contributes to a pixel of blur amount `a` (0..1): the
/// level below `a` gets 1 − t, the one above t, the others 0. `fx.wgsl` `focus_weight` computes
/// the same.
pub(crate) fn level_weight(k: u32, a: f32) -> f32 {
    // the level above `a`, 1..=4 (a = 1 falls in the last span, 0.5 to 1, at t = 1)
    let i = 1 + usize::from(a >= BLUR_LEVELS[1]) + usize::from(a >= BLUR_LEVELS[2]) + usize::from(a >= BLUR_LEVELS[3]);
    let (l0, l1) = (BLUR_LEVELS[i - 1], BLUR_LEVELS[i]);
    let t = ((a - l0) / (l1 - l0)).clamp(0.0, 1.0);
    match k as usize {
        k if k == i => t,
        k if k + 1 == i => 1.0 - t,
        _ => 0.0,
    }
}

/// A level is blurred on a halved picture (halved again and again) while its blur stays at least
/// this many pixels wide there: a Gaussian that wide is smooth enough that scaling it back up
/// bilinearly looks the same as blurring at full size, at a quarter of the work per halving.
const MIN_HALVED_SIGMA: f32 = 3.0;

/// The levels of [`BLUR_LEVELS`] a pixel whose blur amount lies in `lo..=hi` can take, each with
/// how often the `w`×`h` picture is halved before it is blurred and the box radii per axis of
/// its Gaussian at that size (none for level 0, the picture itself). Empty when nothing blurs: a
/// maximum blur (`max_sigma`) of 0.3 px or less, or every amount 0. A range that is not a number
/// takes every level.
pub(crate) fn blur_levels(w: usize, h: usize, max_sigma: f32, lo: f32, hi: f32) -> Vec<BlurLevel> {
    if max_sigma.is_nan() || max_sigma <= 0.3 {
        return Vec::new();
    }
    let (lo, hi) = if lo.is_nan() || hi.is_nan() { (0.0, 1.0) } else { (lo, hi) };
    if hi <= 0.0 {
        return Vec::new();
    }
    // past a few picture sizes every blur gives the same flat picture
    let cap = 2.0 * w.max(h).max(8) as f32;
    (0..BLUR_LEVELS.len())
        .filter(|&k| {
            // a level weighs something strictly between its neighbours
            let below = k.checked_sub(1).and_then(|j| BLUR_LEVELS.get(j)).copied().unwrap_or(f32::NEG_INFINITY);
            let above = BLUR_LEVELS.get(k + 1).copied().unwrap_or(f32::INFINITY);
            lo < above && hi > below
        })
        .map(|k| {
            let s = (max_sigma * BLUR_LEVELS.get(k).copied().unwrap_or(0.0)).min(cap);
            let (mut lw, mut lh, mut down) = (w, h, 0u32);
            while down < 20 && s / (1u32 << (down + 1)) as f32 >= MIN_HALVED_SIGMA && lw >= 16 && lh >= 16 {
                (lw, lh, down) = (lw / 2, lh / 2, down + 1);
            }
            // the halvings (a box b px wide) and the scale back up (a tent 2b wide) blur too: the
            // Gaussian on the halved picture adds the rest
            let b = (1u32 << down) as f32;
            let extra = if down == 0 { 0.0 } else { (b * b - 1.0) / 12.0 + b * b / 6.0 };
            let s = (s * s - extra).max(0.0).sqrt() / b;
            let (rx, ry) = gaussian_boxes(lw, lh, s, s);
            BlurLevel { k: k as u32, down, rx, ry }
        })
        .collect()
}

/// One level of [`blend_levels`], ready to read.
enum Level {
    /// The picture itself (level 0, or a blur too narrow to do anything).
    Picture,
    /// No pixel weighs it.
    Unused,
    /// Blurred: at full size, the part from `at` on; halved, all of it, read scaled by `scale`.
    Blurred { img: Image, at: (usize, usize), scale: Option<(f32, f32)> },
}

/// Where each level `want`s to know it weighs anything (`level_weight` above 0 at `amount`):
/// the first and last column and row, or None.
fn level_bounds(w: usize, h: usize, amount: &(impl Fn(usize, usize) -> f32 + Sync), levels: &[BlurLevel], want: &[bool]) -> Vec<Option<[usize; 4]>> {
    let merge = |a: Vec<Option<[usize; 4]>>, b: Vec<Option<[usize; 4]>>| -> Vec<Option<[usize; 4]>> {
        a.into_iter()
            .zip(b)
            .map(|(a, b)| match (a, b) {
                (Some(a), Some(b)) => Some([a[0].min(b[0]), a[1].max(b[1]), a[2].min(b[2]), a[3].max(b[3])]),
                (a, b) => a.or(b),
            })
            .collect()
    };
    (0..h)
        .into_par_iter()
        .map(|y| {
            let mut rows: Vec<Option<[usize; 4]>> = vec![None; levels.len()];
            for x in 0..w {
                let a = amount(x, y);
                for ((lv, want), r) in levels.iter().zip(want).zip(&mut rows) {
                    if *want && level_weight(lv.k, a) != 0.0 {
                        r.get_or_insert([x, x, y, y])[1] = x;
                    }
                }
            }
            rows
        })
        .reduce(|| vec![None; levels.len()], merge)
}

/// Blur `img` by a blur amount per pixel (`amount(x, y)`, 0..1) through `levels`
/// ([`blur_levels`]): each pixel becomes the sum of the levels, each weighted by
/// [`level_weight`] (levels of weight 0 left out). Each level is blurred at its size, from the
/// picture halved as often as it says; a pixel then reads the halved ones bilinearly
/// (`Image::sample_bilinear_clamped` at its centre scaled to their size), all in one pass. A
/// level at full size is blurred only around the pixels that weigh it, as far out as its blur
/// reaches, which gives the same values there (to rounding). The CPU reference of
/// `FxOp::FocusBlur`: the GPU adds the same levels in the same order.
pub(crate) fn blend_levels(img: &mut Image, amount: impl Fn(usize, usize) -> f32 + Sync, levels: &[BlurLevel]) {
    let (w, h) = (img.w, img.h);
    if levels.is_empty() || w == 0 || h == 0 || img.px.len() != w * h * 4 {
        return;
    }
    // the picture halved once, twice… as deep as a level goes
    let deepest = levels.iter().map(|l| l.down).max().unwrap_or(0);
    let mut halves: Vec<Image> = Vec::new();
    for _ in 0..deepest {
        let next = halves.last().unwrap_or(img).downsample2();
        halves.push(next);
    }
    let blurs = |lv: &BlurLevel| !(lv.rx.is_empty() && lv.ry.is_empty());
    let full: Vec<bool> = levels.iter().map(|lv| lv.down == 0 && blurs(lv)).collect();
    let bounds = if full.contains(&true) { level_bounds(w, h, &amount, levels, &full) } else { vec![None; levels.len()] };
    let src = &*img;
    let ready: Vec<Level> = levels
        .par_iter()
        .zip(&bounds)
        .map(|(lv, bound)| {
            if lv.down > 0 {
                let Some(half) = halves.get(lv.down as usize - 1) else { return Level::Unused };
                let mut b = half.clone();
                box_blur(&mut b, &lv.rx, &lv.ry, true);
                let scale = Some((b.w as f32 / w as f32, b.h as f32 / h as f32));
                return Level::Blurred { img: b, at: (0, 0), scale };
            }
            if !blurs(lv) {
                return Level::Picture;
            }
            let Some([x0, x1, y0, y1]) = *bound else { return Level::Unused };
            // the pixels that weigh it and as far around as its passes read
            let reach = |r: &[u32]| r.iter().fold(0usize, |s, r| s.saturating_add(*r as usize));
            let (mx, my) = (reach(&lv.rx), reach(&lv.ry));
            let (cx0, cy0) = (x0.saturating_sub(mx), y0.saturating_sub(my));
            let (cx1, cy1) = (x1.saturating_add(mx).min(w - 1), y1.saturating_add(my).min(h - 1));
            let (cw, ch) = (cx1 + 1 - cx0, cy1 + 1 - cy0);
            let mut b = Image::new(cw, ch);
            b.px.par_chunks_mut(cw * 4).enumerate().for_each(|(y, row)| {
                let at = ((cy0 + y) * w + cx0) * 4;
                if let Some(from) = src.px.get(at..at + cw * 4) {
                    row.copy_from_slice(from);
                }
            });
            box_blur(&mut b, &lv.rx, &lv.ry, true);
            Level::Blurred { img: b, at: (cx0, cy0), scale: None }
        })
        .collect();
    let reads: Vec<(u32, &Level)> = levels.iter().map(|lv| lv.k).zip(&ready).collect();
    img.px.par_chunks_mut(w * 4).enumerate().for_each(|(y, row)| {
        for (x, p) in row.chunks_exact_mut(4).enumerate() {
            let a = amount(x, y);
            let orig = [p[0], p[1], p[2], p[3]];
            let mut sum: Option<[f32; 4]> = None;
            for &(k, level) in &reads {
                let wt = level_weight(k, a);
                if wt == 0.0 {
                    continue;
                }
                let v = match level {
                    Level::Picture => orig,
                    Level::Unused => continue,
                    Level::Blurred { img: b, scale: Some((sx, sy)), .. } => b.sample_bilinear_clamped((x as f32 + 0.5) * sx, (y as f32 + 0.5) * sy),
                    Level::Blurred { img: b, at: (ox, oy), scale: None } => match (x.checked_sub(*ox), y.checked_sub(*oy)) {
                        (Some(bx), Some(by)) if bx < b.w && by < b.h => b.get(bx, by),
                        _ => continue,
                    },
                };
                sum = Some(match sum {
                    None => v.map(|c| wt * c),
                    Some(s) => [s[0] + wt * v[0], s[1] + wt * v[1], s[2] + wt * v[2], s[3] + wt * v[3]],
                });
            }
            if let Some(s) = sum {
                p.copy_from_slice(&s);
            }
        }
    });
}

/// The blur amount `amount(x, y)` of every pixel of a `w`×`h` picture, row by row, clamped to
/// 0..1 (not a number: 0, sharp).
fn amount_map(w: usize, h: usize, amount: impl Fn(usize, usize) -> f32 + Sync) -> Vec<f32> {
    let mut map = vec![0f32; w * h];
    if w == 0 {
        return map;
    }
    map.par_chunks_mut(w).enumerate().for_each(|(y, row)| {
        for (x, v) in row.iter_mut().enumerate() {
            let a = amount(x, y);
            *v = if a.is_nan() { 0.0 } else { a.clamp(0.0, 1.0) };
        }
    });
    map
}

/// Mix between increasingly blurred copies (the levels of [`blur_levels`] some pixel uses) by a
/// per-pixel amount 0..1.
pub(super) fn variable_blur(img: &mut Image, max_sigma: f32, amount: impl Fn(usize, usize) -> f32 + Sync) {
    if max_sigma.is_nan() || max_sigma <= 0.3 || img.w == 0 || img.h == 0 {
        return;
    }
    let map = amount_map(img.w, img.h, amount);
    let (lo, hi) = map
        .par_iter()
        .fold(|| (f32::INFINITY, f32::NEG_INFINITY), |(lo, hi), &a| (lo.min(a), hi.max(a)))
        .reduce(|| (f32::INFINITY, f32::NEG_INFINITY), |a, b| (a.0.min(b.0), a.1.max(b.1)));
    let levels = blur_levels(img.w, img.h, max_sigma, lo, hi);
    let w = img.w;
    blend_levels(img, |x, y| map.get(y * w + x).copied().unwrap_or(0.0), &levels);
}

/// Focus Blur (`gpufx::FxOp::FocusBlur`, evaluated there): sharp inside the focus `area`,
/// blurring smoothly outside it through `levels`; `show` tints the area red.
pub(crate) fn focus(img: &mut Image, area: &FocusArea, levels: &[BlurLevel], show: bool) {
    let w = img.w;
    if w == 0 || img.h == 0 {
        return;
    }
    blend_levels(img, |x, y| area.amount(x, y), levels);
    if show {
        img.px.par_chunks_mut(w * 4).enumerate().for_each(|(y, row)| {
            for (x, p) in row.chunks_exact_mut(4).enumerate() {
                let k = (1.0 - area.amount(x, y)) * 0.35;
                over(p, [k, 0.0, 0.0, k]);
            }
        });
    }
}

/// Compound Blur: blur amount per pixel from a blur layer's luminance (another track, or this
/// layer's own luminance when the Blur Layer is None).
pub fn compound(img: &mut Image, e: &EffectInstance, cx: &FxCtx) {
    let max = fv(e, "max", cx) * cx.px_scale * 0.5;
    let invert = bv(e, "invert");
    let layer = chv(e, "layer") as usize;
    let track = if layer > 0 { cx.env.and_then(|env| env.track(layer - 1).map(|t| (t, env.layer_to_output()))) } else { None };
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
}

/// Reduce Interlace Flicker: a vertical-only softening across fields.
pub fn interlace_flicker(img: &mut Image, e: &EffectInstance, cx: &FxCtx) {
    let s = fv(e, "softness", cx) * cx.px_scale.max(0.25);
    gaussian(img, 0.0, s, true);
}
