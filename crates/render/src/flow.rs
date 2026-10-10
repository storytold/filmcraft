//! Optical Flow time interpolation: the in-between frame of a speed-changed clip synthesised from
//! the motion between its two neighbouring source frames, so a moving object appears once, at its
//! interpolated position, instead of as the double image of Frame Blending.
//!
//! The method (textbook, implemented from the published equations):
//!
//! 1. **Dense flow** — Lucas & Kanade (1981) solved at every pixel: the gradient structure tensor
//!    and the mismatch term summed over a (2·[`WIN`]+1)² window (box filter), Gauss–Newton iterated
//!    with bilinear warping, Tikhonov-damped so flat areas keep the coarser estimate, coarse to fine
//!    over a binomial pyramid of luma at a working width of at most [`WORK_W`] pixels. After each
//!    level, motion is dropped wherever standing still matches the window as well (brightness
//!    changes on static content are not motion), then a 3×3 median removes outliers. Computed both
//!    ways (frame 0 → 1 and 1 → 0).
//! 2. **Flow at the in-between time** — forward splatting (the interpolation of Baker et al., "A
//!    Database and Evaluation Methodology for Optical Flow", IJCV 2011): every pixel of frame 0
//!    lands at `p + t·u`, every pixel of frame 1 at `p + (1−t)·u'`; where several land on one
//!    pixel the one whose flow is forward–backward consistent and photometrically best wins; holes
//!    are filled from their neighbours.
//! 3. **Warp and blend** — each output pixel fetches frame 0 at `x − t·u` and frame 1 at
//!    `x + (1−t)·u`, weighted `(1−t, t)` times how well the fetched pixel's own flow agrees with
//!    `u` (a pixel that moves elsewhere is hidden at time `t`), so background an object uncovers
//!    comes from the frame that shows it. Where neither fetch agrees, or the pixel looks the same in
//!    both frames, it fades to the plain cross-fade (Frame Blending).
//!
//! Everything is deterministic (no randomness, per-pixel parallel work only).

use rayon::prelude::*;

use crate::image::Image;
use crate::track::Gray;

/// Widest luma image the flow is estimated on (larger frames are box-reduced to it first).
pub const WORK_W: usize = 640;
/// Window radius of the per-pixel Lucas–Kanade sums.
pub const WIN: usize = 4;
/// Pyramid levels stop above this size (either dimension).
const MIN_LEVEL: usize = 12;
const MAX_LEVELS: usize = 7;
/// Gauss–Newton iterations per pyramid level.
const ITERS: usize = 6;
/// Gradient noise floor (square-root luma per pixel): texture weaker than this, summed over the
/// window, is not trusted to show motion. Damps the structure tensor (Tikhonov).
const NOISE: f32 = 0.01;
const DAMP: f32 = ((2 * WIN + 1) * (2 * WIN + 1)) as f32 * NOISE * NOISE;
/// Largest flow update per iteration (level pixels).
const MAX_STEP: f32 = 2.0;
/// Weight of the forward–backward error (pixels) against the luma error when splats collide.
const FB_WEIGHT: f32 = 0.05;
/// Flow disagreement (working pixels) at which a fetch counts half.
const AGREE_PX: f32 = 1.0;
/// Luma change (square-root luma) below which a pixel counts as unchanged between the frames;
/// the motion-compensated result fades in over the next `STILL_LUMA`.
const STILL_LUMA: f32 = 0.004;
/// Hole-filling passes after splatting (remaining holes keep zero motion).
const FILL_PASSES: usize = 64;

/// A dense flow field on a working-resolution grid (pixel centres at integers).
#[derive(Clone, Debug, PartialEq)]
struct Flow {
    w: usize,
    h: usize,
    uv: Vec<[f32; 2]>,
}

impl Flow {
    fn zero(w: usize, h: usize) -> Self {
        Self { w, h, uv: vec![[0.0; 2]; w.saturating_mul(h)] }
    }
    #[inline]
    fn at(&self, x: i64, y: i64) -> [f32; 2] {
        if self.w == 0 || self.h == 0 {
            return [0.0; 2];
        }
        let x = x.clamp(0, self.w as i64 - 1) as usize;
        let y = y.clamp(0, self.h as i64 - 1) as usize;
        self.uv.get(y * self.w + x).copied().unwrap_or([0.0; 2])
    }
    /// Bilinear sample, clamped at the edges.
    #[inline]
    fn sample(&self, x: f32, y: f32) -> [f32; 2] {
        let (x, y) = (if x.is_finite() { x } else { 0.0 }, if y.is_finite() { y } else { 0.0 });
        let (x0, y0) = (x.floor(), y.floor());
        let (fx, fy) = (x - x0, y - y0);
        let (x0, y0) = (x0 as i64, y0 as i64);
        let (a, b, c, d) = (self.at(x0, y0), self.at(x0 + 1, y0), self.at(x0, y0 + 1), self.at(x0 + 1, y0 + 1));
        let mut o = [0.0; 2];
        for k in 0..2 {
            let top = a[k] + (b[k] - a[k]) * fx;
            let bot = c[k] + (d[k] - c[k]) * fx;
            o[k] = top + (bot - top) * fy;
        }
        o
    }
}

/// Clamped fetch from a luma image.
#[inline]
fn g_at(g: &Gray, x: i64, y: i64) -> f32 {
    if g.w == 0 || g.h == 0 {
        return 0.0;
    }
    let x = x.clamp(0, g.w as i64 - 1) as usize;
    let y = y.clamp(0, g.h as i64 - 1) as usize;
    g.px.get(y * g.w + x).copied().unwrap_or(0.0)
}

/// Bilinear luma sample (pixel centres at integers), clamped; non-finite coordinates read the origin.
#[inline]
fn g_sample(g: &Gray, x: f32, y: f32) -> f32 {
    let (x, y) = (if x.is_finite() { x } else { 0.0 }, if y.is_finite() { y } else { 0.0 });
    let (x0, y0) = (x.floor(), y.floor());
    let (fx, fy) = (x - x0, y - y0);
    let (x0, y0) = (x0 as i64, y0 as i64);
    let top = g_at(g, x0, y0) + (g_at(g, x0 + 1, y0) - g_at(g, x0, y0)) * fx;
    let bot = g_at(g, x0, y0 + 1) + (g_at(g, x0 + 1, y0 + 1) - g_at(g, x0, y0 + 1)) * fx;
    top + (bot - top) * fy
}

/// Perceptual-ish luma of a premultiplied linear pixel (square root lifts the shadows so their
/// edges carry gradient; HDR highlights are capped).
#[inline]
fn luma(p: [f32; 4]) -> f32 {
    let l = 0.2126 * p[0] + 0.7152 * p[1] + 0.0722 * p[2];
    if l.is_finite() { l.clamp(0.0, 16.0).sqrt() } else { 0.0 }
}

/// Luma of `img` box-reduced by `s` (≥ 1) in each direction.
fn work_luma(img: &Image, s: usize) -> Gray {
    let s = s.max(1);
    let (w, h) = (img.w.div_ceil(s).max(1), img.h.div_ceil(s).max(1));
    let mut px = vec![0.0f32; w * h];
    px.par_chunks_mut(w).enumerate().for_each(|(y, row)| {
        for (x, o) in row.iter_mut().enumerate() {
            let (mut sum, mut n) = (0.0f32, 0u32);
            for yy in y * s..((y + 1) * s).min(img.h) {
                for xx in x * s..((x + 1) * s).min(img.w) {
                    let i = (yy * img.w + xx) * 4;
                    if let Some(p) = img.px.get(i..i + 4) {
                        sum += luma([p[0], p[1], p[2], p[3]]);
                        n += 1;
                    }
                }
            }
            *o = if n > 0 { sum / n as f32 } else { 0.0 };
        }
    });
    Gray { w, h, px }
}

/// Binomial pyramid, finest first.
fn pyramid(g: Gray) -> Vec<Gray> {
    let mut v = vec![g];
    while v.len() < MAX_LEVELS {
        let Some(l) = v.last() else { break };
        if l.w < 2 * MIN_LEVEL || l.h < 2 * MIN_LEVEL {
            break;
        }
        let n = l.down();
        v.push(n);
    }
    v
}

/// Sum of `src` over a (2r+1)² window at every pixel (truncated at the borders).
fn box_sum<const N: usize>(w: usize, h: usize, src: &[[f32; N]], r: usize) -> Vec<[f32; N]> {
    if w == 0 || h == 0 || src.len() != w * h {
        return vec![[0.0; N]; src.len()];
    }
    // horizontal pass: prefix sums per row (f64, so differences of large sums stay exact)
    let mut tmp = vec![[0.0f64; N]; w * h];
    tmp.par_chunks_mut(w).zip(src.par_chunks(w)).for_each(|(o, s)| {
        let mut pre = vec![[0.0f64; N]; w + 1];
        for x in 0..w {
            for k in 0..N {
                pre[x + 1][k] = pre[x][k] + s[x][k] as f64;
            }
        }
        for (x, ox) in o.iter_mut().enumerate() {
            let (lo, hi) = (x.saturating_sub(r), (x + r + 1).min(w));
            for k in 0..N {
                ox[k] = pre[hi][k] - pre[lo][k];
            }
        }
    });
    // vertical pass: column prefix sums, then one output row per task
    let mut pre = vec![[0.0f64; N]; w * (h + 1)];
    for y in 0..h {
        for x in 0..w {
            for k in 0..N {
                pre[(y + 1) * w + x][k] = pre[y * w + x][k] + tmp[y * w + x][k];
            }
        }
    }
    let mut out = vec![[0.0f32; N]; w * h];
    out.par_chunks_mut(w).enumerate().for_each(|(y, row)| {
        let (lo, hi) = (y.saturating_sub(r), (y + r + 1).min(h));
        for (x, o) in row.iter_mut().enumerate() {
            for k in 0..N {
                o[k] = (pre[hi * w + x][k] - pre[lo * w + x][k]) as f32;
            }
        }
    });
    out
}

/// Gauss–Newton refinement of `flow` (mapping `a` onto `b`: `a(x) ≈ b(x + u)`) at one level.
fn refine(a: &Gray, b: &Gray, flow: &mut Flow) {
    let (w, h) = (a.w, a.h);
    if w == 0 || h == 0 || a.px.len() != w * h || flow.uv.len() != w * h || (flow.w, flow.h) != (w, h) {
        return;
    }
    let grad: Vec<[f32; 2]> = (0..w * h)
        .into_par_iter()
        .map(|i| {
            let (x, y) = ((i % w) as i64, (i / w) as i64);
            [(g_at(a, x + 1, y) - g_at(a, x - 1, y)) * 0.5, (g_at(a, x, y + 1) - g_at(a, x, y - 1)) * 0.5]
        })
        .collect();
    let products: Vec<[f32; 3]> = grad.iter().map(|g| [g[0] * g[0], g[0] * g[1], g[1] * g[1]]).collect();
    let tensor = box_sum(w, h, &products, WIN);
    for _ in 0..ITERS {
        let mismatch: Vec<[f32; 2]> = (0..w * h)
            .into_par_iter()
            .map(|i| {
                let (x, y) = ((i % w) as f32, (i / w) as f32);
                let u = flow.uv[i];
                let e = g_sample(b, x + u[0], y + u[1]) - a.px[i];
                [grad[i][0] * e, grad[i][1] * e]
            })
            .collect();
        let bsum = box_sum(w, h, &mismatch, WIN);
        flow.uv.par_iter_mut().zip(tensor.par_iter().zip(bsum.par_iter())).for_each(|(f, (g, bv))| {
            let (gxx, gxy, gyy) = (g[0] + DAMP, g[1], g[2] + DAMP);
            let det = gxx * gyy - gxy * gxy;
            if det > 1e-12 {
                let dx = -(gyy * bv[0] - gxy * bv[1]) / det;
                let dy = -(gxx * bv[1] - gxy * bv[0]) / det;
                if dx.is_finite() && dy.is_finite() {
                    f[0] += dx.clamp(-MAX_STEP, MAX_STEP);
                    f[1] += dy.clamp(-MAX_STEP, MAX_STEP);
                }
            }
        });
    }
}

/// Where standing still explains the window at least as well as the estimated motion, the
/// motion is dropped: brightness changes (flicker, lights switching, noise) on static content
/// otherwise read as motion and would drag static detail around.
fn prefer_still(a: &Gray, b: &Gray, flow: &mut Flow) {
    let (w, h) = (a.w, a.h);
    if w == 0 || h == 0 || a.px.len() != w * h || b.px.len() != w * h || flow.uv.len() != w * h {
        return;
    }
    let errs: Vec<[f32; 2]> = (0..w * h)
        .into_par_iter()
        .map(|i| {
            let (x, y) = ((i % w) as f32, (i / w) as f32);
            let u = flow.uv[i];
            let moved = g_sample(b, x + u[0], y + u[1]) - a.px[i];
            let still = b.px[i] - a.px[i];
            [moved * moved, still * still]
        })
        .collect();
    let sums = box_sum(w, h, &errs, WIN);
    let slack = ((2 * WIN + 1) * (2 * WIN + 1)) as f32 * NOISE * NOISE * 0.25;
    flow.uv.par_iter_mut().zip(sums.par_iter()).for_each(|(f, e)| {
        if e[1] <= e[0] + slack {
            *f = [0.0; 2];
        }
    });
}

/// 3×3 median of each flow component (removes isolated outliers, keeps motion edges).
fn median3(f: &Flow) -> Flow {
    let (w, h) = (f.w, f.h);
    let uv = (0..w * h)
        .into_par_iter()
        .map(|i| {
            let (x, y) = ((i % w) as i64, (i / w) as i64);
            let mut xs = [0.0f32; 9];
            let mut ys = [0.0f32; 9];
            let mut k = 0;
            for dy in -1..=1 {
                for dx in -1..=1 {
                    let v = f.at(x + dx, y + dy);
                    xs[k] = v[0];
                    ys[k] = v[1];
                    k += 1;
                }
            }
            xs.sort_by(f32::total_cmp);
            ys.sort_by(f32::total_cmp);
            [xs[4], ys[4]]
        })
        .collect();
    Flow { w, h, uv }
}

/// `f` on a grid twice as fine (`w`×`h`), its vectors doubled.
fn upsample(f: &Flow, w: usize, h: usize) -> Flow {
    let uv = (0..w * h)
        .into_par_iter()
        .map(|i| {
            let v = f.sample((i % w) as f32 * 0.5, (i / w) as f32 * 0.5);
            [v[0] * 2.0, v[1] * 2.0]
        })
        .collect();
    Flow { w, h, uv }
}

/// Coarse-to-fine dense flow from `pa[0]` to `pb[0]` (pyramids of equal-sized images).
fn dense_flow(pa: &[Gray], pb: &[Gray]) -> Option<Flow> {
    let levels = pa.len().min(pb.len());
    let mut flow: Option<Flow> = None;
    for l in (0..levels).rev() {
        let (a, b) = (pa.get(l)?, pb.get(l)?);
        if (a.w, a.h) != (b.w, b.h) {
            return None;
        }
        let mut f = match &flow {
            Some(c) => upsample(c, a.w, a.h),
            None => Flow::zero(a.w, a.h),
        };
        refine(a, b, &mut f);
        prefer_still(a, b, &mut f);
        flow = Some(median3(&f));
    }
    flow
}

/// The flow (in the 0 → 1 direction) of the surface seen at each pixel at time `t`, by forward
/// splatting both flows (see the module docs).
fn flow_at(a: &Gray, b: &Gray, f01: &Flow, f10: &Flow, t: f32) -> Flow {
    let (w, h) = (a.w, a.h);
    let n = w * h;
    if n == 0 || [b.px.len(), f01.uv.len(), f10.uv.len(), a.px.len()].iter().any(|&l| l != n) {
        return Flow::zero(w, h);
    }
    let mut best = vec![f32::INFINITY; n];
    let mut out = vec![[0.0f32; 2]; n];
    // frame 0 moves forward by t·u01, frame 1 backward by (1−t)·u10 (= forward flow −u10)
    for (src, dst, f, back, tt, sign) in [(a, b, f01, f10, t, 1.0f32), (b, a, f10, f01, 1.0 - t, -1.0f32)] {
        for y in 0..h {
            for x in 0..w {
                let i = y * w + x;
                let u = f.uv[i];
                let (qx, qy) = (x as f32 + u[0], y as f32 + u[1]);
                let photo = (src.px[i] - g_sample(dst, qx, qy)).abs();
                let r = back.sample(qx, qy);
                let fb = ((u[0] + r[0]).powi(2) + (u[1] + r[1]).powi(2)).sqrt().min(100.0);
                let score = photo + FB_WEIGHT * fb;
                if !score.is_finite() {
                    continue;
                }
                let (tx, ty) = (x as f32 + tt * u[0], y as f32 + tt * u[1]);
                if !(tx > -1.0 && ty > -1.0 && tx < w as f32 && ty < h as f32) {
                    continue;
                }
                let (x0, y0) = (tx.floor() as i64, ty.floor() as i64);
                for (sx, sy) in [(x0, y0), (x0 + 1, y0), (x0, y0 + 1), (x0 + 1, y0 + 1)] {
                    if sx < 0 || sy < 0 || sx >= w as i64 || sy >= h as i64 {
                        continue;
                    }
                    let j = sy as usize * w + sx as usize;
                    if score < best[j] {
                        best[j] = score;
                        out[j] = [sign * u[0], sign * u[1]];
                    }
                }
            }
        }
    }
    // fill holes from their filled neighbours
    let mut valid: Vec<bool> = best.iter().map(|s| s.is_finite()).collect();
    for _ in 0..FILL_PASSES {
        let (prev, prev_valid) = (out.clone(), valid.clone());
        let mut changed = false;
        for y in 0..h {
            for x in 0..w {
                let i = y * w + x;
                if prev_valid[i] {
                    continue;
                }
                let (mut sum, mut k) = ([0.0f32; 2], 0u32);
                for (dx, dy) in [(-1i64, -1i64), (0, -1), (1, -1), (-1, 0), (1, 0), (-1, 1), (0, 1), (1, 1)] {
                    let (nx, ny) = (x as i64 + dx, y as i64 + dy);
                    if nx < 0 || ny < 0 || nx >= w as i64 || ny >= h as i64 {
                        continue;
                    }
                    let j = ny as usize * w + nx as usize;
                    if prev_valid[j] {
                        sum = [sum[0] + prev[j][0], sum[1] + prev[j][1]];
                        k += 1;
                    }
                }
                if k > 0 {
                    out[i] = [sum[0] / k as f32, sum[1] / k as f32];
                    valid[i] = true;
                    changed = true;
                }
            }
        }
        if !changed {
            break;
        }
    }
    Flow { w, h, uv: out }
}

#[inline]
fn agree(d: f32) -> f32 {
    let q = d / AGREE_PX;
    1.0 / (1.0 + q * q)
}

/// The frame between `a` (time 0) and `b` (time 1) at `t` (0..1), motion compensated. Frames of
/// different sizes (or empty ones) fall back to the plain cross-fade.
pub fn interpolate(a: &Image, b: &Image, t: f32) -> Image {
    let t = if t.is_finite() { t.clamp(0.0, 1.0) } else { 0.0 };
    let n = a.w.saturating_mul(a.h).saturating_mul(4);
    if (a.w, a.h) != (b.w, b.h) || a.w == 0 || a.h == 0 || a.px.len() != n || b.px.len() != n {
        return a.clone().lerp(b, t);
    }
    if t <= 0.0 {
        return a.clone();
    }
    if t >= 1.0 {
        return b.clone();
    }
    let s = a.w.div_ceil(WORK_W).max(1);
    let (pa, pb) = (pyramid(work_luma(a, s)), pyramid(work_luma(b, s)));
    let (Some(ga), Some(gb), Some(f01), Some(f10)) = (pa.first(), pb.first(), dense_flow(&pa, &pb), dense_flow(&pb, &pa)) else {
        return a.clone().lerp(b, t);
    };
    let ft = flow_at(ga, gb, &f01, &f10, t);
    let sf = s as f32;
    let to_work = |v: f32| v / sf - 0.5;
    let mut out = Image::new(a.w, a.h);
    out.px.par_chunks_mut(a.w * 4).enumerate().for_each(|(y, row)| {
        for (x, o) in row.as_chunks_mut::<4>().0.iter_mut().enumerate() {
            let (px, py) = (x as f32 + 0.5, y as f32 + 0.5);
            let u = ft.sample(to_work(px), to_work(py));
            let (ux, uy) = (u[0] * sf, u[1] * sf);
            let (x0, y0) = (px - t * ux, py - t * uy);
            let (x1, y1) = (px + (1.0 - t) * ux, py + (1.0 - t) * uy);
            // does the fetched pixel itself move along this trajectory?
            let ua = f01.sample(to_work(x0), to_work(y0));
            let ub = f10.sample(to_work(x1), to_work(y1));
            let ka = agree(((ua[0] - u[0]).powi(2) + (ua[1] - u[1]).powi(2)).sqrt());
            let kb = agree(((ub[0] + u[0]).powi(2) + (ub[1] + u[1]).powi(2)).sqrt());
            let (wa, wb) = ((1.0 - t) * ka, t * kb);
            let ca = a.sample_bilinear_clamped(x0, y0);
            let cb = b.sample_bilinear_clamped(x1, y1);
            let i = (y * a.w + x) * 4;
            let (Some(pa), Some(pb)) = (a.px.get(i..i + 4), b.px.get(i..i + 4)) else { continue };
            let sum = wa + wb;
            // a pixel that looks the same in both frames needs no motion compensation (static
            // content, flat interiors): the cross-fade is right there and immune to flow errors
            let changed = (luma([pa[0], pa[1], pa[2], 0.0]) - luma([pb[0], pb[1], pb[2], 0.0])).abs().max((pa[3] - pb[3]).abs());
            let changed = if changed.is_finite() { ((changed - STILL_LUMA) / STILL_LUMA).clamp(0.0, 1.0) } else { 0.0 };
            let conf = ka.max(kb) * changed;
            for k in 0..4 {
                let warped = if sum > 1e-6 { (ca[k] * wa + cb[k] * wb) / sum } else { ca[k] * (1.0 - t) + cb[k] * t };
                let plain = pa[k] + (pb[k] - pa[k]) * t;
                o[k] = plain + (warped - plain) * conf;
            }
        }
    });
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A smooth, textured, premultiplied opaque picture sampled with the content offset by `(dx, dy)`.
    fn texture(w: usize, h: usize, dx: f32, dy: f32) -> Image {
        let mut img = Image::new(w, h);
        for y in 0..h {
            for x in 0..w {
                let (u, v) = (x as f32 + 0.5 - dx, y as f32 + 0.5 - dy);
                let l = 0.35 + 0.15 * (u * 0.21).sin() * (v * 0.17).cos() + 0.1 * ((u + v) * 0.11).sin() + 0.05 * (u * 0.53 - v * 0.31).cos();
                let i = (y * w + x) * 4;
                img.px[i..i + 4].copy_from_slice(&[l, l * 0.9, l * 0.8, 1.0]);
            }
        }
        img
    }

    /// A flat grey background with a white square of side `side` whose left/top edge is at `(x0, y0)`.
    fn square(w: usize, h: usize, x0: usize, y0: usize, side: usize) -> Image {
        let mut img = Image::filled(w, h, [0.1, 0.1, 0.1, 1.0]);
        for y in y0..(y0 + side).min(h) {
            for x in x0..(x0 + side).min(w) {
                let i = (y * w + x) * 4;
                img.px[i..i + 4].copy_from_slice(&[1.0, 1.0, 1.0, 1.0]);
            }
        }
        img
    }

    /// Mean absolute difference over the pixels at least `margin` from every edge.
    fn interior_diff(a: &Image, b: &Image, margin: usize) -> f32 {
        let (mut s, mut n) = (0.0f32, 0usize);
        for y in margin..a.h - margin {
            for x in margin..a.w - margin {
                let (p, q) = (a.get(x, y), b.get(x, y));
                s += (0..3).map(|k| (p[k] - q[k]).abs()).sum::<f32>();
                n += 3;
            }
        }
        s / n as f32
    }

    #[test]
    fn uniform_motion_lands_half_way() {
        let (a, b) = (texture(160, 96, 0.0, 0.0), texture(160, 96, 6.0, -3.0));
        let want = texture(160, 96, 3.0, -1.5);
        let flow = interpolate(&a, &b, 0.5);
        let blend = a.clone().lerp(&b, 0.5);
        let (e_flow, e_blend) = (interior_diff(&flow, &want, 12), interior_diff(&blend, &want, 12));
        assert!(e_flow < 0.006, "optical flow off the half-way picture: {e_flow}");
        assert!(e_flow * 5.0 < e_blend, "optical flow {e_flow} should beat frame blending {e_blend}");
    }

    #[test]
    fn quarter_way_follows_the_time() {
        let (a, b) = (texture(160, 96, 0.0, 0.0), texture(160, 96, 8.0, 4.0));
        let want = texture(160, 96, 2.0, 1.0);
        let e = interior_diff(&interpolate(&a, &b, 0.25), &want, 12);
        assert!(e < 0.006, "{e}");
    }

    #[test]
    fn a_moving_square_appears_once_at_its_interpolated_place() {
        // a 24 px square moving 10 px right: Frame Blending shows two half-bright copies
        let (a, b) = (square(128, 96, 40, 36, 24), square(128, 96, 50, 36, 24));
        let want = square(128, 96, 45, 36, 24);
        let flow = interpolate(&a, &b, 0.5);
        let blend = a.clone().lerp(&b, 0.5);
        // the leading and trailing bands are where blending leaves ghosts
        for x in [42usize, 71] {
            let (f, bl, w) = (flow.get(x, 48)[0], blend.get(x, 48)[0], want.get(x, 48)[0]);
            assert!((f - w).abs() < 0.1, "x={x}: flow {f}, want {w} (blend {bl})");
            assert!((bl - w).abs() > 0.3, "x={x}: the blend should ghost here");
        }
        assert!(flow.get(57, 48)[0] > 0.95, "the square's centre stays solid");
        let (e_flow, e_blend) = (interior_diff(&flow, &want, 4), interior_diff(&blend, &want, 4));
        assert!(e_flow * 4.0 < e_blend, "flow {e_flow} vs blend {e_blend}");
    }

    #[test]
    fn flicker_on_static_content_is_not_dragged_around() {
        // static rows of lit windows; between the frames some switch on and some grow a pixel:
        // brightness change, not motion, so the result is the plain cross-fade
        let (w, h) = (160usize, 96usize);
        let mut a = Image::filled(w, h, [0.004, 0.004, 0.006, 1.0]);
        for row in 0..8 {
            for col in 0..10 {
                let (x0, y0) = (6 + col * 15, 6 + row * 11);
                for x in x0..x0 + 8 {
                    for y in y0..y0 + 2 {
                        let i = (y * w + x) * 4;
                        a.px[i..i + 4].copy_from_slice(&[0.6, 0.45, 0.1, 1.0]);
                    }
                }
            }
        }
        let mut b = a.clone();
        for (k, i) in (0..w * h).step_by(97).enumerate() {
            let lit = if k % 2 == 0 { [0.6, 0.45, 0.1, 1.0] } else { [0.004, 0.004, 0.006, 1.0] };
            b.px[i * 4..i * 4 + 4].copy_from_slice(&lit);
        }
        let flow = interpolate(&a, &b, 0.5);
        let blend = a.clone().lerp(&b, 0.5);
        let e = interior_diff(&flow, &blend, 0);
        assert!(e < 1e-3, "static detail moved: {e}");
    }

    #[test]
    fn identical_frames_are_returned_unchanged() {
        let a = texture(64, 48, 0.0, 0.0);
        let out = interpolate(&a, &a.clone(), 0.37);
        assert!(interior_diff(&out, &a, 0) < 1e-5);
    }

    #[test]
    fn large_frames_estimate_flow_at_the_working_size() {
        // wider than WORK_W: the flow is estimated on a reduced copy and scaled back up
        let (a, b) = (texture(1400, 120, 0.0, 0.0), texture(1400, 120, 12.0, 0.0));
        let want = texture(1400, 120, 6.0, 0.0);
        let e = interior_diff(&interpolate(&a, &b, 0.5), &want, 24);
        assert!(e < 0.01, "{e}");
    }

    #[test]
    fn hostile_inputs_do_not_panic() {
        let a = texture(32, 24, 0.0, 0.0);
        // size mismatch and empty frames fall back to the cross-fade
        let small = texture(16, 24, 0.0, 0.0);
        assert_eq!(interpolate(&a, &small, 0.5), a.clone().lerp(&small, 0.5));
        let empty = Image::new(0, 0);
        assert_eq!(interpolate(&empty, &empty, 0.5).px.len(), 0);
        // a pixel buffer that does not match its size
        let broken = Image { w: 32, h: 24, px: vec![0.0; 7] };
        assert_eq!(interpolate(&broken, &a, 0.5).w, 32);
        // end points and non-finite weights
        assert_eq!(interpolate(&a, &small.clone(), f32::NAN).w, 32);
        let b = texture(32, 24, 2.0, 1.0);
        assert_eq!(interpolate(&a, &b, 0.0), a);
        assert_eq!(interpolate(&a, &b, 7.0), b);
        assert_eq!(interpolate(&a, &b, f32::NAN), a);
        // NaN / infinite pixels and tiny frames
        let mut nan = b.clone();
        nan.px[40] = f32::NAN;
        nan.px[41] = f32::INFINITY;
        nan.px[45] = -f32::INFINITY;
        assert_eq!(interpolate(&a, &nan, 0.5).px.len(), a.px.len());
        for (w, h) in [(1, 1), (2, 1), (1, 3), (3, 2)] {
            let (p, q) = (texture(w, h, 0.0, 0.0), texture(w, h, 1.0, 0.0));
            assert_eq!(interpolate(&p, &q, 0.5).px.len(), w * h * 4);
        }
    }
}
