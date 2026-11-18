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

use std::sync::{OnceLock, PoisonError, RwLock};

use rayon::prelude::*;
use serde::{Deserialize, Serialize};

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

// ---------------------------------------------------------------------------------------------
// Public dense optical-flow field, hardware-accelerator hook (NVOFA) and visualization modes
// ---------------------------------------------------------------------------------------------

/// A dense 2D optical-flow field mapping `prev` onto `curr` (`prev(x) ≈ curr(x + u)`).
///
/// Vectors in [`Self::uv`] are in **full-frame pixel units** (`frame_w` × `frame_h`), stored on a
/// grid of size `w` × `h` (for example, `grid_size = 4` on NVIDIA NVOFA or the CPU working pyramid).
#[derive(Clone, Debug, PartialEq)]
pub struct FlowField {
    /// Flow grid width in cells.
    pub w: usize,
    /// Flow grid height in cells.
    pub h: usize,
    /// Full video frame width in pixels.
    pub frame_w: usize,
    /// Full video frame height in pixels.
    pub frame_h: usize,
    /// Displacement `[dx, dy]` in full-frame pixels per grid cell (`w * h`).
    pub uv: Vec<[f32; 2]>,
    /// Per-cell matching cost (`0` = highest confidence, `255` = lowest confidence / occluded; `w * h`).
    pub cost: Vec<u8>,
    /// `true` when computed by the registered GPU optical-flow engine (e.g. NVIDIA NVOFA).
    pub hardware: bool,
}

impl FlowField {
    /// An empty zero-motion flow field for a frame of size `frame_w` × `frame_h`.
    pub fn zero(frame_w: usize, frame_h: usize) -> Self {
        let w = frame_w.max(1);
        let h = frame_h.max(1);
        let n = w.saturating_mul(h);
        Self { w, h, frame_w: w, frame_h: h, uv: vec![[0.0; 2]; n], cost: vec![0; n], hardware: false }
    }

    #[inline]
    fn cell_at(&self, x: i64, y: i64) -> ([f32; 2], f32) {
        if self.w == 0 || self.h == 0 {
            return ([0.0; 2], 1.0);
        }
        let x = x.clamp(0, self.w as i64 - 1) as usize;
        let y = y.clamp(0, self.h as i64 - 1) as usize;
        let i = y.saturating_mul(self.w).saturating_add(x);
        let uv = self.uv.get(i).copied().unwrap_or([0.0; 2]);
        let c = self.cost.get(i).copied().unwrap_or(0);
        let conf = 1.0 - (f32::from(c) / 255.0).clamp(0.0, 1.0);
        (uv, conf)
    }

    /// Bilinearly sample `([dx, dy], confidence)` at full-frame pixel coordinates `(px, py)`.
    #[inline]
    pub fn sample_px(&self, px: f32, py: f32) -> ([f32; 2], f32) {
        if self.w == 0 || self.h == 0 || self.frame_w == 0 || self.frame_h == 0 {
            return ([0.0; 2], 1.0);
        }
        let sx = self.w as f32 / self.frame_w as f32;
        let sy = self.h as f32 / self.frame_h as f32;
        let gx = (if px.is_finite() { px } else { 0.0 }) * sx - 0.5;
        let gy = (if py.is_finite() { py } else { 0.0 }) * sy - 0.5;
        let (x0, y0) = (gx.floor(), gy.floor());
        let (fx, fy) = (gx - x0, gy - y0);
        let (x0, y0) = (x0 as i64, y0 as i64);
        let (a, ca) = self.cell_at(x0, y0);
        let (b, cb) = self.cell_at(x0 + 1, y0);
        let (c, cc) = self.cell_at(x0, y0 + 1);
        let (d, cd) = self.cell_at(x0 + 1, y0 + 1);
        let lerp = |p: f32, q: f32, t: f32| p + (q - p) * t;
        let dx = lerp(lerp(a[0], b[0], fx), lerp(c[0], d[0], fx), fy);
        let dy = lerp(lerp(a[1], b[1], fx), lerp(c[1], d[1], fx), fy);
        let conf = lerp(lerp(ca, cb, fx), lerp(cc, cd, fx), fy);
        ([dx, dy], conf.clamp(0.0, 1.0))
    }
}

/// Stateful hardware optical-flow session for a fixed frame size (`width` × `height`).
///
/// Keeping one session alive across sequential frames of a clip reuses GPU allocations and enables
/// temporal flow hints on NVIDIA NVOFA hardware.
pub trait FlowSession: Send {
    /// Backend name (e.g. `"NVIDIA NVOFA"`).
    fn backend(&self) -> &'static str;

    /// Estimate dense optical flow from `prev_luma` to `curr_luma` (8-bit luma, `width * height` bytes).
    fn estimate(&mut self, prev_luma: &[u8], curr_luma: &[u8], sequential: bool) -> Result<FlowField, String>;
}

/// Factory registered by `filmcraft-platform` to create hardware optical-flow sessions.
pub type FlowSessionFactory = fn(width: u32, height: u32) -> Result<Box<dyn FlowSession>, String>;

fn hw_factory_slot() -> &'static RwLock<Option<FlowSessionFactory>> {
    static SLOT: OnceLock<RwLock<Option<FlowSessionFactory>>> = OnceLock::new();
    SLOT.get_or_init(|| RwLock::new(None))
}

/// Register the platform's hardware optical-flow session factory (called by `filmcraft_platform::register`).
pub fn register_optical_flow(factory: FlowSessionFactory) {
    *hw_factory_slot().write().unwrap_or_else(PoisonError::into_inner) = Some(factory);
}

/// Whether a hardware optical-flow factory is registered.
pub fn optical_flow_registered() -> bool {
    hw_factory_slot().read().unwrap_or_else(PoisonError::into_inner).is_some()
}

/// Convert a linear-light [`Image`] into an 8-bit perceptual luma buffer (`w * h` bytes) suitable
/// for hardware optical-flow estimation (NVOFA `NV_OF_BUFFER_FORMAT_GRAYSCALE8`).
pub fn image_to_luma8(img: &Image) -> Vec<u8> {
    let n = img.w.saturating_mul(img.h);
    if n == 0 || img.px.len() < n.saturating_mul(4) {
        return vec![0; n];
    }
    let mut out = vec![0u8; n];
    out.par_chunks_mut(img.w.max(1)).enumerate().for_each(|(y, row)| {
        for (x, dst) in row.iter_mut().enumerate() {
            let i = (y * img.w + x) * 4;
            if let Some(p) = img.px.get(i..i + 4) {
                let y_lin = 0.2126 * p[0] + 0.7152 * p[1] + 0.0722 * p[2];
                let srgb = if y_lin.is_finite() { y_lin.clamp(0.0, 1.0).sqrt() } else { 0.0 };
                *dst = (srgb * 255.0 + 0.5) as u8;
            }
        }
    });
    out
}

/// Estimate dense optical flow on the CPU using our coarse-to-fine Lucas–Kanade pyramid (the
/// reference fallback when hardware optical flow is unavailable or declines a frame size).
pub fn estimate_cpu(prev: &Image, curr: &Image) -> FlowField {
    let (w, h) = (prev.w, prev.h);
    let n = w.saturating_mul(h).saturating_mul(4);
    if (w, h) != (curr.w, curr.h) || w == 0 || h == 0 || prev.px.len() != n || curr.px.len() != n {
        return FlowField::zero(w, h);
    }
    let s = w.div_ceil(WORK_W).max(1);
    let (pa, pb) = (pyramid(work_luma(prev, s)), pyramid(work_luma(curr, s)));
    let (Some(ga), Some(gb), Some(f01)) = (pa.first(), pb.first(), dense_flow(&pa, &pb)) else {
        return FlowField::zero(w, h);
    };
    let sf = s as f32;
    let cells = f01.w.saturating_mul(f01.h);
    let mut uv = Vec::with_capacity(cells);
    let mut cost = Vec::with_capacity(cells);
    for (i, &u) in f01.uv.iter().enumerate() {
        let (x, y) = ((i % f01.w) as f32, (i / f01.w) as f32);
        let photo = (ga.px.get(i).copied().unwrap_or(0.0) - g_sample(gb, x + u[0], y + u[1])).abs();
        let c = (photo * 512.0).clamp(0.0, 255.0) as u8;
        uv.push([u[0] * sf, u[1] * sf]);
        cost.push(c);
    }
    FlowField { w: f01.w, h: f01.h, frame_w: w, frame_h: h, uv, cost, hardware: false }
}

/// Stateful optical-flow estimator that prefers the registered hardware engine (NVIDIA NVOFA) and
/// transparently falls back to [`estimate_cpu`] if hardware is absent or declines the frame size.
pub struct FlowEstimator {
    width: usize,
    height: usize,
    hw: Option<Box<dyn FlowSession>>,
    /// Luma of the `curr` frame of the last successful hardware call: the next call is a
    /// continuation (temporal hints on) only when its `prev` is this very picture.
    last_curr_luma: Option<Vec<u8>>,
    /// Why the hardware engine is not (or no longer) used, when a factory is registered.
    fallback_reason: Option<String>,
}

/// Name [`FlowEstimator::backend`] reports for the CPU fallback.
pub const CPU_FLOW_BACKEND: &str = "CPU Lucas-Kanade";

impl FlowEstimator {
    /// Open an estimator for frames of `width` × `height`.
    pub fn new(width: usize, height: usize) -> Self {
        let factory = *hw_factory_slot().read().unwrap_or_else(PoisonError::into_inner);
        let (hw, fallback_reason) = match (factory, u32::try_from(width), u32::try_from(height)) {
            (None, _, _) => (None, None),
            (Some(f), Ok(w), Ok(h)) => match f(w, h) {
                Ok(s) => (Some(s), None),
                Err(e) => (None, Some(e)),
            },
            (Some(_), _, _) => (None, Some(format!("frame size {width}x{height} is too large for hardware optical flow"))),
        };
        Self { width, height, hw, last_curr_luma: None, fallback_reason }
    }

    /// Whether this estimator is backed by a hardware optical-flow session.
    pub fn is_hardware(&self) -> bool {
        self.hw.is_some()
    }

    /// Name of the active backend (`"NVIDIA NVOFA"` or [`CPU_FLOW_BACKEND`]).
    pub fn backend(&self) -> &'static str {
        self.hw.as_ref().map_or(CPU_FLOW_BACKEND, |s| s.backend())
    }

    /// Why a registered hardware engine is not used (it declined the frame size, failed to open,
    /// or failed on a frame and was dropped), if it is not.
    pub fn fallback_reason(&self) -> Option<&str> {
        self.fallback_reason.as_deref()
    }

    /// Estimate optical flow between `prev` and `curr`. The result's [`FlowField::hardware`] says
    /// which backend computed it: a hardware failure drops the session and this frame (and every
    /// later one) is computed on the CPU.
    pub fn estimate(&mut self, prev: &Image, curr: &Image) -> FlowField {
        if prev.w != self.width || prev.h != self.height || curr.w != self.width || curr.h != self.height {
            self.last_curr_luma = None;
            return estimate_cpu(prev, curr);
        }
        if let Some(hw) = self.hw.as_mut() {
            // The luma of `prev` is always taken from `prev` itself: callers may pass any pair, and
            // reusing the last `curr` blindly would compare a frame with itself (zero motion).
            let p_luma = image_to_luma8(prev);
            let c_luma = image_to_luma8(curr);
            let sequential = self.last_curr_luma.as_deref() == Some(p_luma.as_slice());
            match hw.estimate(&p_luma, &c_luma, sequential) {
                Ok(field) => {
                    self.last_curr_luma = Some(c_luma);
                    return field;
                }
                Err(why) => {
                    self.fallback_reason = Some(format!("{} failed: {why}", hw.backend()));
                    self.hw = None;
                    self.last_curr_luma = None;
                }
            }
        }
        estimate_cpu(prev, curr)
    }
}

/// Draws optical-flow overlay frames on a GPU: the counterpart of [`encode_flow_viz_cpu`]
/// ([`render_flow_viz`] then [`image_to_encoded_rgb_f32`] in one step), registered by the app from
/// `filmcraft-gpu`.
pub trait FlowVizRenderer: Send {
    /// Backend name for messages (e.g. `"GPU (NVIDIA GeForce …)"`).
    fn backend(&self) -> &str;

    /// The encoded `R'G'B'` floats (3 per pixel, `curr.w × curr.h`) of `field` drawn in `mode`.
    fn encode(&mut self, curr: &Image, field: &FlowField, mode: FlowVizMode, max_motion_px: f32) -> Result<Vec<f32>, String>;

    /// Premultiplied linear RGB before the track's output colour transform.
    fn linear_detail(&mut self, _: &Image, _: &FlowField, _: f32) -> Result<Vec<f32>, String> {
        Err("linear detail output unavailable".into())
    }
}

/// Factory registered by the app to open a [`FlowVizRenderer`].
pub type FlowVizFactory = fn() -> Result<Box<dyn FlowVizRenderer>, String>;

fn viz_factory_slot() -> &'static RwLock<Option<FlowVizFactory>> {
    static SLOT: OnceLock<RwLock<Option<FlowVizFactory>>> = OnceLock::new();
    SLOT.get_or_init(|| RwLock::new(None))
}

/// Register the GPU overlay drawing factory (the app calls this at startup with `filmcraft-gpu`'s).
pub fn register_flow_viz(factory: FlowVizFactory) {
    *viz_factory_slot().write().unwrap_or_else(PoisonError::into_inner) = Some(factory);
}

/// Name [`FlowVizEncoder::backend`] reports when the CPU draws the overlay.
pub const CPU_VIZ_BACKEND: &str = "CPU";

/// The encoded `R'G'B'` floats of an overlay frame drawn on the CPU: the reference the GPU path
/// is tested against.
pub fn encode_flow_viz_cpu(curr: &Image, field: &FlowField, mode: FlowVizMode, max_motion_px: f32) -> Vec<f32> {
    image_to_encoded_rgb_f32(&render_flow_viz(curr, field, mode, max_motion_px))
}

/// Draws overlay frames on the registered GPU renderer and falls back to [`encode_flow_viz_cpu`]
/// when there is none, it cannot open, or it fails on a frame (that frame and every later one are
/// then drawn on the CPU).
pub struct FlowVizEncoder {
    gpu: Option<Box<dyn FlowVizRenderer>>,
    /// Why a registered GPU renderer is not (or no longer) used.
    fallback_reason: Option<String>,
}

impl FlowVizEncoder {
    /// Open the registered GPU renderer, if there is one.
    pub fn new() -> Self {
        let factory = *viz_factory_slot().read().unwrap_or_else(PoisonError::into_inner);
        let (gpu, fallback_reason) = match factory.map(|f| f()) {
            None => (None, None),
            Some(Ok(r)) => (Some(r), None),
            Some(Err(e)) => (None, Some(e)),
        };
        Self { gpu, fallback_reason }
    }

    /// Whether frames are drawn on the GPU.
    pub fn is_gpu(&self) -> bool {
        self.gpu.is_some()
    }

    /// The active backend's name ([`CPU_VIZ_BACKEND`] on the CPU).
    pub fn backend(&self) -> String {
        self.gpu.as_ref().map_or_else(|| CPU_VIZ_BACKEND.to_string(), |g| g.backend().to_string())
    }

    /// Why a registered GPU renderer is not used, if it is not.
    pub fn fallback_reason(&self) -> Option<&str> {
        self.fallback_reason.as_deref()
    }

    /// The encoded `R'G'B'` floats of one overlay frame, and whether the GPU drew it.
    pub fn encode(&mut self, curr: &Image, field: &FlowField, mode: FlowVizMode, max_motion_px: f32) -> (Vec<f32>, bool) {
        if let Some(rgb) = self.draw_on_gpu(curr, |gpu| gpu.encode(curr, field, mode, max_motion_px)) {
            return (rgb, true);
        }
        (encode_flow_viz_cpu(curr, field, mode, max_motion_px), false)
    }

    /// Draw temporal detail in working light, preserving highlights for PQ / HLG output.
    pub fn linear_detail(&mut self, curr: &Image, field: &FlowField, max_motion_px: f32) -> (Vec<f32>, bool) {
        if let Some(rgb) = self.draw_on_gpu(curr, |gpu| gpu.linear_detail(curr, field, max_motion_px)) {
            return (rgb, true);
        }
        let frame = render_flow_viz(curr, field, FlowVizMode::DlssEnhance, max_motion_px);
        (frame.px.as_chunks::<4>().0.iter().flat_map(|p| [p[0], p[1], p[2]]).collect(), false)
    }

    fn draw_on_gpu(&mut self, curr: &Image, draw: impl FnOnce(&mut dyn FlowVizRenderer) -> Result<Vec<f32>, String>) -> Option<Vec<f32>> {
        if let Some(gpu) = self.gpu.as_mut() {
            let want = curr.w.saturating_mul(curr.h).saturating_mul(3);
            match draw(gpu.as_mut()) {
                Ok(v) if v.len() == want => return Some(v),
                Ok(v) => self.fallback_reason = Some(format!("{} returned {} values for a {}x{} frame", gpu.backend(), v.len(), curr.w, curr.h)),
                Err(why) => self.fallback_reason = Some(format!("{} failed: {why}", gpu.backend())),
            }
            self.gpu = None;
        }
        None
    }
}

impl Default for FlowVizEncoder {
    fn default() -> Self {
        Self::new()
    }
}

/// Stateful hardware neural frame-reconstruction session (e.g. NVIDIA NGX DLSS Preset K
/// Transformer on Direct3D 11) for a fixed `(width, height)` frame size.
pub trait NeuralSession: Send {
    /// Backend name (e.g. `"NVIDIA NGX DLSS (Preset K)"`).
    fn backend(&self) -> &'static str;

    /// Reconstruct / temporally anti-alias `curr` using the dense optical-flow field `field`
    /// (`prev -> curr`) and temporal history when `sequential` is `true`.
    fn enhance(&mut self, curr: &Image, field: &FlowField, sequential: bool) -> Result<Image, String>;
}

/// Factory registered by `filmcraft-platform` to open hardware neural enhancement sessions.
pub type NeuralSessionFactory = fn(width: u32, height: u32) -> Result<Box<dyn NeuralSession>, String>;

fn neural_factory_slot() -> &'static RwLock<Option<NeuralSessionFactory>> {
    static SLOT: OnceLock<RwLock<Option<NeuralSessionFactory>>> = OnceLock::new();
    SLOT.get_or_init(|| RwLock::new(None))
}

/// Register the platform's hardware neural-enhancement session factory (called by `filmcraft_platform::register`).
pub fn register_neural_enhancer(factory: NeuralSessionFactory) {
    *neural_factory_slot().write().unwrap_or_else(PoisonError::into_inner) = Some(factory);
}

/// Whether a hardware neural-enhancement factory is registered.
pub fn neural_enhancer_registered() -> bool {
    neural_factory_slot().read().unwrap_or_else(PoisonError::into_inner).is_some()
}

/// Name [`NeuralEnhancer::backend`] reports for the motion-compensated CPU temporal fallback.
pub const CPU_NEURAL_BACKEND: &str = "Motion-Compensated Temporal";

/// Motion-compensated temporal reconstruction on the CPU (the reference fallback when hardware
/// NGX DLSS is absent or declines the frame size): warps `prev` along `field` (`prev -> curr`)
/// weighted by flow confidence and photometric agreement so temporal grain is suppressed without
/// ghosting across disocclusions or brightness changes.
pub fn fuse_temporal_cpu(prev: Option<&Image>, curr: &Image, field: &FlowField) -> Image {
    let (w, h) = (curr.w, curr.h);
    let n = w.saturating_mul(h).saturating_mul(4);
    let Some(p) = prev.filter(|p| (p.w, p.h) == (w, h) && p.px.len() == n && curr.px.len() == n && w > 0 && h > 0) else {
        return curr.clone();
    };
    let mut out = Image::new(w, h);
    out.px.par_chunks_mut(w * 4).enumerate().for_each(|(y, row)| {
        for (x, dst) in row.as_chunks_mut::<4>().0.iter_mut().enumerate() {
            let i = (y * w + x) * 4;
            let Some(c) = curr.px.get(i..i + 4) else { continue };
            let (px, py) = (x as f32 + 0.5, y as f32 + 0.5);
            let ([dx, dy], conf) = field.sample_px(px, py);
            let (dx, dy) = (if dx.is_finite() { dx } else { 0.0 }, if dy.is_finite() { dy } else { 0.0 });
            let warped = p.sample_bilinear_clamped(px - dx, py - dy);
            let lc = luma([c[0], c[1], c[2], c[3]]);
            let lw = luma(warped);
            let diff = (lc - lw).abs();
            let photo_agree = if diff.is_finite() { 1.0 / (1.0 + (diff / 0.06).powi(2)) } else { 0.0 };
            let blend = (0.28 * conf.clamp(0.0, 1.0) * photo_agree).clamp(0.0, 0.35);
            for k in 0..3 {
                let ck = if c[k].is_finite() { c[k] } else { 0.0 };
                let wk = if warped[k].is_finite() { warped[k] } else { ck };
                dst[k] = ck + (wk - ck) * blend;
            }
            dst[3] = if c[3].is_finite() { c[3] } else { 1.0 };
        }
    });
    out
}

/// Stateful neural frame enhancer that prefers the registered hardware engine (NVIDIA NGX DLSS
/// Preset K Transformer) and transparently falls back to [`fuse_temporal_cpu`] if hardware is
/// absent, declines the frame size, or fails mid-clip.
pub struct NeuralEnhancer {
    width: usize,
    height: usize,
    hw: Option<Box<dyn NeuralSession>>,
    has_history: bool,
    fallback_reason: Option<String>,
}

impl NeuralEnhancer {
    /// Open a neural enhancer for frames of `width` × `height`.
    pub fn new(width: usize, height: usize) -> Self {
        let factory = *neural_factory_slot().read().unwrap_or_else(PoisonError::into_inner);
        let (hw, fallback_reason) = match (factory, u32::try_from(width), u32::try_from(height)) {
            (None, _, _) => (None, None),
            (Some(f), Ok(w), Ok(h)) => match f(w, h) {
                Ok(s) => (Some(s), None),
                Err(e) => (None, Some(e)),
            },
            (Some(_), _, _) => (None, Some(format!("frame size {width}x{height} is too large for hardware neural enhancement"))),
        };
        Self { width, height, hw, has_history: false, fallback_reason }
    }

    /// Whether this enhancer is backed by a hardware neural session.
    pub fn is_hardware(&self) -> bool {
        self.hw.is_some()
    }

    /// Name of the active backend (`"NVIDIA NGX DLSS (Preset K)"` or [`CPU_NEURAL_BACKEND`]).
    pub fn backend(&self) -> &'static str {
        self.hw.as_ref().map_or(CPU_NEURAL_BACKEND, |s| s.backend())
    }

    /// Why a registered hardware engine is not used, if it is not.
    pub fn fallback_reason(&self) -> Option<&str> {
        self.fallback_reason.as_deref()
    }

    /// Reconstruct `curr` with `prev` and `field`, returning `(reconstructed_image, used_hardware)`.
    pub fn enhance(&mut self, prev: Option<&Image>, curr: &Image, field: &FlowField) -> (Image, bool) {
        if curr.w != self.width || curr.h != self.height {
            self.has_history = false;
            return (fuse_temporal_cpu(prev, curr, field), false);
        }
        if let Some(hw) = self.hw.as_mut() {
            let sequential = self.has_history && prev.is_some_and(|p| (p.w, p.h) == (self.width, self.height));
            match hw.enhance(curr, field, sequential) {
                Ok(img) if (img.w, img.h) == (self.width, self.height) => {
                    self.has_history = true;
                    return (img, true);
                }
                Ok(img) => {
                    self.fallback_reason = Some(format!("{} returned {}x{} for a {}x{} frame", hw.backend(), img.w, img.h, self.width, self.height));
                }
                Err(why) => {
                    self.fallback_reason = Some(format!("{} failed: {why}", hw.backend()));
                }
            }
            self.hw = None;
            self.has_history = false;
        }
        (fuse_temporal_cpu(prev, curr, field), false)
    }
}

/// Visual representation mode for an optical-flow overlay track.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum FlowVizMode {
    /// Directional HSV color wheel (angle → hue, speed → neon brightness; static areas black).
    /// Designed to be stacked on V2 with `Screen` or `Add` blend mode.
    #[default]
    HsvWheel,
    /// Raw 2D velocity pass (`R = 0.5 + dx / (2·range)`, `G = 0.5 + dy / (2·range)`, `B = confidence`).
    MotionVectors,
    /// Kinetic edge & motion aura: illuminates moving parts of the source frame with directional neon energy.
    KineticGlow,
    /// Temporal reconstruction followed by edge-preserving local contrast enhancement.
    DlssEnhance,
}

impl FlowVizMode {
    pub fn from_name(s: &str) -> Option<Self> {
        match s {
            "hsv" | "hsvWheel" | "wheel" => Some(Self::HsvWheel),
            "uv" | "motionVectors" | "vectors" => Some(Self::MotionVectors),
            "glow" | "kineticGlow" | "kinetic" => Some(Self::KineticGlow),
            "dlss" | "dlssEnhance" | "neural" | "neuralEnhance" => Some(Self::DlssEnhance),
            _ => None,
        }
    }

    pub fn id(self) -> &'static str {
        match self {
            Self::HsvWheel => "hsvWheel",
            Self::MotionVectors => "motionVectors",
            Self::KineticGlow => "kineticGlow",
            Self::DlssEnhance => "dlssEnhance",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::HsvWheel => "HSV Neon Flow Wheel",
            Self::MotionVectors => "Motion Vectors (UV Pass)",
            Self::KineticGlow => "Kinetic Motion Glow",
            Self::DlssEnhance => "Temporal Detail Enhancement",
        }
    }
}

/// Clamped linear RGB fetch from `img` with non-finite values sanitized to `0.0` (matching the GPU
/// upload in `filmcraft_gpu::flow_viz`).
#[inline]
fn src_rgb_clamped(img: &Image, x: isize, y: isize) -> [f32; 3] {
    if img.w == 0 || img.h == 0 {
        return [0.0; 3];
    }
    let cx = x.clamp(0, img.w as isize - 1) as usize;
    let cy = y.clamp(0, img.h as isize - 1) as usize;
    let i = cy.saturating_mul(img.w).saturating_add(cx).saturating_mul(4);
    let Some(p) = img.px.get(i..i + 3) else {
        return [0.0; 3];
    };
    let fin = |v: f32| if v.is_finite() { v } else { 0.0 };
    [fin(p[0]), fin(p[1]), fin(p[2])]
}

/// Edge-preserving bilateral local structure & micro-contrast enhancement for `FlowVizMode::DlssEnhance`
/// (reproduced step for step in `flow_viz.wgsl`).
#[inline]
fn local_contrast_rgb(curr: &Image, x: isize, y: isize, conf: f32, active: f32) -> [f32; 3] {
    let c = src_rgb_clamped(curr, x, y);
    let luma_c = 0.2126 * c[0] + 0.7152 * c[1] + 0.0722 * c[2];
    let mut sum = [c[0] * 0.25, c[1] * 0.25, c[2] * 0.25];
    let mut w_sum = 0.25f32;
    for oy in -1isize..=1 {
        for ox in -1isize..=1 {
            if ox == 0 && oy == 0 {
                continue;
            }
            let ws = if ox == 0 || oy == 0 { 0.15f32 } else { 0.10f32 };
            let n = src_rgb_clamped(curr, x + ox, y + oy);
            let luma_n = 0.2126 * n[0] + 0.7152 * n[1] + 0.0722 * n[2];
            let dl = luma_n - luma_c;
            let w = ws / (1.0 + dl * dl * 36.0);
            sum[0] += n[0] * w;
            sum[1] += n[1] * w;
            sum[2] += n[2] * w;
            w_sum += w;
        }
    }
    let inv = 1.0 / w_sum;
    let clarity = 0.45 + 0.40 * conf * (1.0 - 0.35 * active);
    [(c[0] + (c[0] - sum[0] * inv) * clarity).max(0.0), (c[1] + (c[1] - sum[1] * inv) * clarity).max(0.0), (c[2] + (c[2] - sum[2] * inv) * clarity).max(0.0)]
}

/// Convert HSV (`h` in radians `-π..π`, `s` in `0..1`, `v` in `0..1`) to linear RGB.
#[inline]
fn hsv_to_linear_rgb(angle_rad: f32, s: f32, v: f32) -> [f32; 3] {
    let deg = angle_rad.to_degrees().rem_euclid(360.0);
    let c = v * s;
    let h6 = deg / 60.0;
    let x = c * (1.0 - (h6.rem_euclid(2.0) - 1.0).abs());
    let m = v - c;
    let (r, g, b) = if h6 < 1.0 {
        (c, x, 0.0)
    } else if h6 < 2.0 {
        (x, c, 0.0)
    } else if h6 < 3.0 {
        (0.0, c, x)
    } else if h6 < 4.0 {
        (0.0, x, c)
    } else if h6 < 5.0 {
        (x, 0.0, c)
    } else {
        (c, 0.0, x)
    };
    // Convert perceptual HSV ramp into linear light so the compositor displays vivid colours
    let to_lin = |u: f32| {
        let u = (u + m).clamp(0.0, 1.0);
        u * u
    };
    [to_lin(r), to_lin(g), to_lin(b)]
}

/// The encoded `[R, G, B]` code values of the Motion Vectors pass: `R = 0.5 + dx / (2·range)`,
/// `G = 0.5 + dy / (2·range)` (both clamped to `0..=1`, so `0.5` is no motion), `B` = confidence
/// where something moves and `0.5` where nothing does.
#[inline]
fn motion_vector_code(dx: f32, dy: f32, conf: f32, active: f32, scale: f32) -> [f32; 3] {
    let u = (0.5 + 0.5 * (dx / scale).clamp(-1.0, 1.0)).clamp(0.0, 1.0);
    let v = (0.5 + 0.5 * (dy / scale).clamp(-1.0, 1.0)).clamp(0.0, 1.0);
    let b = if active > 0.0 { conf.clamp(0.0, 1.0) } else { 0.5 };
    [u, v, b]
}

/// Encode a linear-light [`Image`] as sRGB-encoded `R'G'B'` floats (3 per pixel, alpha dropped,
/// premultiplied colour composited over black), without the 8-bit rounding of
/// [`Image::over_black_with_alpha_rgba8`]: the input of a 10/12-bit encoder.
pub fn image_to_encoded_rgb_f32(img: &Image) -> Vec<f32> {
    let n = img.w.saturating_mul(img.h);
    let mut out = vec![0.0f32; n.saturating_mul(3)];
    if img.px.len() < n.saturating_mul(4) {
        return out;
    }
    out.par_chunks_mut(3).zip(img.px.par_chunks(4)).for_each(|(o, p)| {
        for (dst, &c) in o.iter_mut().zip(p.iter()) {
            *dst = if c.is_finite() { filmcraft_color::linear_to_srgb(c.clamp(0.0, 1.0)) } else { 0.0 };
        }
    });
    out
}

/// Render a [`FlowField`] as a premultiplied linear-light [`Image`] at `(curr.w, curr.h)`.
///
/// * `max_motion_px`: motion distance (in full-frame pixels) that saturates the visualization
///   (default `16.0` px; clamped to `>= 0.5`).
pub fn render_flow_viz(curr: &Image, field: &FlowField, mode: FlowVizMode, max_motion_px: f32) -> Image {
    let (w, h) = (curr.w, curr.h);
    if w == 0 || h == 0 {
        return Image::new(w, h);
    }
    let scale = if max_motion_px.is_finite() { max_motion_px.clamp(0.5, 512.0) } else { 16.0 };
    let deadzone = 0.25f32; // sub-pixel noise floor so static camera grain stays pure black
    let mut out = Image::new(w, h);
    out.px.par_chunks_mut(w * 4).enumerate().for_each(|(y, row)| {
        for (x, dst) in row.as_chunks_mut::<4>().0.iter_mut().enumerate() {
            let (px, py) = (x as f32 + 0.5, y as f32 + 0.5);
            let ([dx, dy], conf) = field.sample_px(px, py);
            let (dx, dy) = (if dx.is_finite() { dx } else { 0.0 }, if dy.is_finite() { dy } else { 0.0 });
            let mag = (dx * dx + dy * dy).sqrt();
            let active = ((mag - deadzone) / scale).clamp(0.0, 1.0);
            match mode {
                FlowVizMode::HsvWheel => {
                    if active <= 0.0 {
                        *dst = [0.0, 0.0, 0.0, 1.0];
                    } else {
                        let angle = dy.atan2(dx);
                        // Smooth knee so medium motion is already bright and fast motion saturates cleanly
                        let v = (active * (2.0 - active) * (0.35 + 0.65 * conf)).clamp(0.0, 1.0);
                        let rgb = hsv_to_linear_rgb(angle, 0.92, v);
                        *dst = [rgb[0], rgb[1], rgb[2], 1.0];
                    }
                }
                FlowVizMode::MotionVectors => {
                    let [u, v, b] = motion_vector_code(dx, dy, conf, active, scale);
                    // The codes are the encoded (sRGB) values; the exact sRGB curve makes the
                    // encoder's linear → sRGB step give them back, so 0.5 stays exactly 0.5.
                    let to_lin = filmcraft_color::srgb_to_linear;
                    *dst = [to_lin(u), to_lin(v), to_lin(b), 1.0];
                }
                FlowVizMode::KineticGlow => {
                    if active <= 0.0 {
                        *dst = [0.0, 0.0, 0.0, 1.0];
                    } else {
                        let angle = dy.atan2(dx);
                        let neon = hsv_to_linear_rgb(angle, 0.85, 1.0);
                        let src = src_rgb_clamped(curr, x as isize, y as isize);
                        let src_luma = (0.2126 * src[0] + 0.7152 * src[1] + 0.0722 * src[2]).clamp(0.0, 2.0);
                        let energy = (active.sqrt() * (0.25 + 0.75 * src_luma)).clamp(0.0, 1.0);
                        *dst = [
                            (src[0] * 0.35 + neon[0] * 0.95) * energy,
                            (src[1] * 0.35 + neon[1] * 0.95) * energy,
                            (src[2] * 0.35 + neon[2] * 0.95) * energy,
                            1.0,
                        ];
                    }
                }
                FlowVizMode::DlssEnhance => {
                    let rgb = local_contrast_rgb(curr, x as isize, y as isize, conf, active);
                    let alpha = curr.px.get((y * w + x) * 4 + 3).copied().filter(|a| a.is_finite()).unwrap_or(0.0).clamp(0.0, 1.0);
                    if alpha > 0.0 {
                        *dst = [rgb[0], rgb[1], rgb[2], alpha];
                    }
                }
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

    /// What the fake hardware session was given: (first luma byte of `prev`, of `curr`, sequential).
    static FAKE_CALLS: std::sync::Mutex<Vec<(u8, u8, bool)>> = std::sync::Mutex::new(Vec::new());
    /// Only this frame size opens the fake session, so the other tests keep the CPU path.
    const FAKE_SIZE: (u32, u32) = (333, 77);
    /// The next fake session fails its second call.
    static FAKE_FAILS_ON_SECOND_CALL: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

    struct FakeSession {
        fail_on_call: Option<usize>,
        calls: usize,
    }

    impl FlowSession for FakeSession {
        fn backend(&self) -> &'static str {
            "Fake"
        }
        fn estimate(&mut self, prev_luma: &[u8], curr_luma: &[u8], sequential: bool) -> Result<FlowField, String> {
            self.calls += 1;
            if self.fail_on_call == Some(self.calls) {
                return Err("device lost".into());
            }
            FAKE_CALLS.lock().unwrap_or_else(PoisonError::into_inner).push((prev_luma[0], curr_luma[0], sequential));
            let mut f = FlowField::zero(FAKE_SIZE.0 as usize, FAKE_SIZE.1 as usize);
            f.hardware = true;
            Ok(f)
        }
    }

    fn fake_factory(w: u32, h: u32) -> Result<Box<dyn FlowSession>, String> {
        if (w, h) != FAKE_SIZE {
            return Err("fake declines this size".into());
        }
        let fail = FAKE_FAILS_ON_SECOND_CALL.swap(false, std::sync::atomic::Ordering::SeqCst);
        Ok(Box::new(FakeSession { fail_on_call: fail.then_some(2), calls: 0 }))
    }

    fn flat(level: f32) -> Image {
        let (w, h) = (FAKE_SIZE.0 as usize, FAKE_SIZE.1 as usize);
        let mut img = Image::new(w, h);
        for p in img.px.chunks_mut(4) {
            p.copy_from_slice(&[level, level, level, 1.0]);
        }
        img
    }

    #[test]
    fn hardware_flow_gets_the_frames_it_is_asked_for_and_falls_back_on_failure() {
        register_optical_flow(fake_factory);
        let (a, b, c) = (flat(0.04), flat(0.25), flat(0.64));
        let (la, lb, lc) = (image_to_luma8(&a)[0], image_to_luma8(&b)[0], image_to_luma8(&c)[0]);
        assert!(la != lb && lb != lc && la != lc);

        // The overlay command's order: frame 0 looks ahead (a → b), frame 1 asks for a → b again,
        // frame 2 continues from b. Frame 1 used to reach the hardware as b → b (no motion).
        FAKE_CALLS.lock().unwrap_or_else(PoisonError::into_inner).clear();
        let mut est = FlowEstimator::new(FAKE_SIZE.0 as usize, FAKE_SIZE.1 as usize);
        assert!(est.is_hardware());
        for (p, q) in [(&a, &b), (&a, &b), (&b, &c)] {
            assert!(est.estimate(p, q).hardware);
        }
        assert_eq!(*FAKE_CALLS.lock().unwrap_or_else(PoisonError::into_inner), vec![(la, lb, false), (la, lb, false), (lb, lc, true)]);
        assert_eq!(est.backend(), "Fake");

        // A session that fails mid-clip: that frame and the rest go to the CPU, and the estimator
        // says why.
        FAKE_FAILS_ON_SECOND_CALL.store(true, std::sync::atomic::Ordering::SeqCst);
        let mut est2 = FlowEstimator::new(FAKE_SIZE.0 as usize, FAKE_SIZE.1 as usize);
        assert!(est2.estimate(&a, &b).hardware);
        assert!(!est2.estimate(&b, &c).hardware, "failed hardware frame is computed on the CPU");
        assert!(!est2.is_hardware());
        assert_eq!(est2.backend(), CPU_FLOW_BACKEND);
        assert!(est2.fallback_reason().is_some_and(|r| r.contains("device lost")), "{:?}", est2.fallback_reason());
    }

    #[test]
    fn motion_vector_pass_keeps_the_neutral_code_exact() {
        let still = FlowField::zero(8, 8);
        let img = Image::new(8, 8);
        let mv = render_flow_viz(&img, &still, FlowVizMode::MotionVectors, 16.0);
        let enc = image_to_encoded_rgb_f32(&mv);
        assert_eq!(enc.len(), 8 * 8 * 3);
        for v in enc {
            assert!((v - 0.5).abs() < 1e-4, "no motion must encode as 0.5, got {v}");
        }
    }

    #[test]
    fn flow_field_and_visualizations_reflect_motion_and_stay_black_when_still() {
        let a = texture(96, 64, 0.0, 0.0);
        let b = texture(96, 64, 4.0, 0.0);
        let mut est = FlowEstimator::new(96, 64);
        let still_field = est.estimate(&a, &a);
        let still_hsv = render_flow_viz(&a, &still_field, FlowVizMode::HsvWheel, 16.0);
        assert!(still_hsv.get(48, 32)[0] < 1e-5 && still_hsv.get(48, 32)[1] < 1e-5, "static frame must render pure black in HSV mode");

        let move_field = est.estimate(&a, &b);
        let ([dx, dy], conf) = move_field.sample_px(48.0, 32.0);
        assert!(dx.abs() > 2.5, "expected horizontal motion magnitude, got dx={dx}, dy={dy}");
        assert!(conf > 0.5, "expected high confidence on smooth texture, got {conf}");

        let hsv = render_flow_viz(&b, &move_field, FlowVizMode::HsvWheel, 16.0);
        let p = hsv.get(48, 32);
        assert!(p[0] + p[1] + p[2] > 0.05, "moving pixel should light up in HSV wheel: {p:?}");

        let uv = render_flow_viz(&b, &move_field, FlowVizMode::MotionVectors, 16.0);
        let uvp = uv.get(48, 32);
        assert!((uvp[0] - 0.5f32.powf(2.2)).abs() > 0.02, "UV red channel should deviate from neutral 0.5: {uvp:?}");

        let glow = render_flow_viz(&b, &move_field, FlowVizMode::KineticGlow, 16.0);
        let gp = glow.get(48, 32);
        assert!(gp[0] + gp[1] + gp[2] > 0.02, "kinetic glow should illuminate moving region: {gp:?}");
    }

    /// A GPU overlay renderer that answers with `FAKE_VIZ_REPLY` (or fails when it is empty) and
    /// counts its calls.
    struct FakeViz;
    static FAKE_VIZ_CALLS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    static FAKE_VIZ_REPLY: std::sync::Mutex<Option<Vec<f32>>> = std::sync::Mutex::new(None);

    impl FlowVizRenderer for FakeViz {
        fn backend(&self) -> &str {
            "fake GPU"
        }

        fn encode(&mut self, _: &Image, _: &FlowField, _: FlowVizMode, _: f32) -> Result<Vec<f32>, String> {
            FAKE_VIZ_CALLS.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            FAKE_VIZ_REPLY.lock().unwrap().clone().ok_or_else(|| "device lost".to_string())
        }
    }

    /// One test for every case: the factory slot is global to this test binary.
    #[test]
    fn overlay_drawing_uses_the_gpu_and_falls_back_to_the_cpu() {
        use std::sync::atomic::Ordering::SeqCst;
        let (img, field) =
            (texture(12, 8, 0.0, 0.0), FlowField { w: 3, h: 2, frame_w: 12, frame_h: 8, uv: vec![[3.0, -1.0]; 6], cost: vec![20; 6], hardware: false });
        let cpu = encode_flow_viz_cpu(&img, &field, FlowVizMode::HsvWheel, 16.0);

        // nothing registered: the CPU, with no reason to report
        let mut none = FlowVizEncoder::new();
        assert!(!none.is_gpu() && none.fallback_reason().is_none());
        assert_eq!(none.encode(&img, &field, FlowVizMode::HsvWheel, 16.0), (cpu.clone(), false));

        register_flow_viz(|| Ok(Box::new(FakeViz)));
        let gpu_answer = vec![0.25f32; 12 * 8 * 3];
        *FAKE_VIZ_REPLY.lock().unwrap() = Some(gpu_answer.clone());
        let mut enc = FlowVizEncoder::new();
        assert!(enc.is_gpu());
        assert_eq!(enc.backend(), "fake GPU");
        assert_eq!(enc.encode(&img, &field, FlowVizMode::HsvWheel, 16.0), (gpu_answer, true));

        // a reply of the wrong size is not passed on: the CPU draws that frame and the rest
        *FAKE_VIZ_REPLY.lock().unwrap() = Some(vec![0.0; 5]);
        assert_eq!(enc.encode(&img, &field, FlowVizMode::HsvWheel, 16.0), (cpu.clone(), false));
        assert!(enc.fallback_reason().is_some_and(|r| r.contains("5 values")), "{:?}", enc.fallback_reason());
        let calls = FAKE_VIZ_CALLS.load(SeqCst);
        assert_eq!(enc.encode(&img, &field, FlowVizMode::HsvWheel, 16.0), (cpu.clone(), false));
        assert_eq!(FAKE_VIZ_CALLS.load(SeqCst), calls, "a failed GPU renderer is not asked again");
        assert_eq!(enc.backend(), CPU_VIZ_BACKEND);

        // a GPU error: the same
        *FAKE_VIZ_REPLY.lock().unwrap() = None;
        let mut enc = FlowVizEncoder::new();
        assert_eq!(enc.encode(&img, &field, FlowVizMode::HsvWheel, 16.0), (cpu.clone(), false));
        assert!(enc.fallback_reason().is_some_and(|r| r.contains("device lost")));

        // a GPU that cannot open
        register_flow_viz(|| Err("no usable GPU".into()));
        let mut enc = FlowVizEncoder::new();
        assert!(!enc.is_gpu());
        assert_eq!(enc.fallback_reason(), Some("no usable GPU"));
        assert_eq!(enc.encode(&img, &field, FlowVizMode::HsvWheel, 16.0), (cpu, false));
    }

    #[test]
    fn temporal_detail_keeps_linear_highlights_and_coverage() {
        let color = [3.0, 1.5, 0.75, 0.5];
        let image = Image::filled(3, 3, color);
        let field = FlowField { w: 1, h: 1, frame_w: 3, frame_h: 3, uv: vec![[0.0; 2]], cost: vec![0], hardware: false };
        let result = render_flow_viz(&image, &field, FlowVizMode::DlssEnhance, 16.0);
        for (actual, expected) in result.get(1, 1).into_iter().zip(color) {
            assert!((actual - expected).abs() < 0.0001, "{actual} vs {expected}");
        }
    }

    #[test]
    fn dlss_enhance_and_neural_enhancer_preserve_colour_and_fall_back_cleanly() {
        let a = texture(48, 32, 0.0, 0.0);
        let b = texture(48, 32, 3.0, -1.0);
        let field = estimate_cpu(&a, &b);

        let fused = fuse_temporal_cpu(Some(&a), &b, &field);
        assert_eq!((fused.w, fused.h), (48, 32));
        assert!(interior_diff(&fused, &b, 4) < 0.05);

        let enhanced = render_flow_viz(&fused, &field, FlowVizMode::DlssEnhance, 16.0);
        assert_eq!((enhanced.w, enhanced.h), (48, 32));
        let p = enhanced.get(24, 16);
        assert!(p[0] > 0.05 && p[3] == 1.0, "DlssEnhance must produce valid opaque colour: {p:?}");

        struct FakeNeural {
            calls: usize,
        }
        impl NeuralSession for FakeNeural {
            fn backend(&self) -> &'static str {
                "Fake NGX DLSS"
            }
            fn enhance(&mut self, curr: &Image, _field: &FlowField, _sequential: bool) -> Result<Image, String> {
                self.calls += 1;
                if self.calls == 2 {
                    return Err("ngx lost".into());
                }
                Ok(curr.clone())
            }
        }
        register_neural_enhancer(|w, h| if (w, h) == (48, 32) { Ok(Box::new(FakeNeural { calls: 0 })) } else { Err("unsupported size".into()) });
        let mut ne = NeuralEnhancer::new(48, 32);
        assert!(ne.is_hardware());
        assert_eq!(ne.backend(), "Fake NGX DLSS");
        let (_, on_hw1) = ne.enhance(None, &a, &field);
        assert!(on_hw1);
        let (_, on_hw2) = ne.enhance(Some(&a), &b, &field);
        assert!(!on_hw2, "second call fails and falls back to CPU temporal fusion");
        assert!(!ne.is_hardware());
        assert_eq!(ne.backend(), CPU_NEURAL_BACKEND);
        assert!(ne.fallback_reason().is_some_and(|r| r.contains("ngx lost")));
    }
}
