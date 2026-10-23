//! Video De-Noise: noise reduction that looks at the neighbouring frames first and at the
//! neighbouring pixels second (CPU).
//!
//! The design comes from the published work below and from this crate's own conventions.
//!
//! - Tomasi and Manduchi, "Bilateral Filtering for Gray and Color Images", ICCV 1998: a weighted
//!   mean whose weights fall with the difference in value (the range weight), so samples that do
//!   not match the pixel are left out instead of blurred in.
//! - Dubois and Sabri, "Noise Reduction in Image Sequences Using Motion-Compensated Temporal
//!   Filtering", IEEE Trans. Communications, 1984: a pixel is averaged with its counterparts in
//!   the neighbouring frames. This version does not compensate motion, so a pixel that moved
//!   (or sits across a cut) fails the match and keeps its own value: less denoising, never a ghost.
//! - Buades, Coll and Morel, "A Non-Local Algorithm for Image Denoising", CVPR 2005: the spatial
//!   weights compare small patches around two pixels rather than the two pixels alone, which
//!   keeps edges and texture that a single-pixel comparison would take for noise.
//!
//! Weights are computed on display-encoded values split into luma `Y' = 0.2126 R' + 0.7152 G' +
//! 0.0722 B'` and the two colour differences `B' - Y'` and `R' - Y'`. Luma and the colour
//! differences are two groups, each with its own noise level (the Luma and Chroma Noise Level
//! parameters, 100 % = 0.10 in display units) and its own weights, so a neighbour that matches in
//! colour but not in brightness still helps the colour. A level of 0 leaves its group untouched.
//! The sRGB curve is mirrored around 0, so a negative (out-of-gamut) channel keeps its sign.
//!
//! Two samples of the same signal differ by `2 s²` on average (`s` = the noise level), so a patch
//! distance up to that counts as a perfect match; beyond it the weight falls off exponentially.
//!
//! 1. Temporal: each neighbouring frame (up to Temporal Radius on each side, read through
//!    [`FxEnv::frame`], so they carry the effects above this one) is compared patch by patch with
//!    the current frame and averaged in with that weight. The current frame counts 1.
//! 2. Spatial: every pixel is averaged with the pixels around it whose patch looks like its own.
//!    The noise left after the temporal pass is about `s / sqrt(1 + sum of temporal weights)`, so
//!    the spatial pass trusts the match less, and smooths more, where the temporal weights were low.
//!    Preserve Detail narrows what the spatial pass accepts, so fine texture survives.
//!
//! Show Only Noise outputs the removed component around mid grey instead of the result.
//!
//! Everything is deterministic, reads at most four extra frames and a window of at most 7 x 7
//! pixels (sized by `px_scale`), and never indexes with an unchecked number.

use filmcraft_color::{linear_to_srgb, srgb_to_linear};

use super::*;

/// Patch radius compared around two pixels (1 = 3 x 3).
const PATCH: usize = 1;
/// Largest search window radius of the spatial pass.
const MAX_SEARCH: usize = 3;
/// Border replicated around every plane, so a window and its patches never leave the buffer.
const PAD: usize = MAX_SEARCH + PATCH;
/// Most neighbouring frames read on each side.
const MAX_FRAMES: usize = 2;
/// Noise standard deviation, in display units, at a level of 100 %.
const SIGMA_FULL: f32 = 0.10;
/// How fast the weight falls beyond a perfect match; smaller is stricter.
const KAPPA_TEMPORAL: f32 = 1.0;
const KAPPA_SPATIAL: f32 = 2.0;
/// Weights below this count as 0, so a mismatch contributes exactly nothing.
const W_MIN: f32 = 1e-3;
/// `-ln(W_MIN)`: the exponent at which a weight drops under [`W_MIN`].
const LN_W_MIN: f32 = 6.907_755;
/// Pixels with less alpha than this are not filtered and not used as samples.
const ALPHA_MIN: f32 = 1e-4;
/// Smallest noise variance worth filtering for.
const MIN_VARIANCE: f32 = 1e-9;
/// Contrast of the removed component in Show Only Noise.
const NOISE_GAIN: f32 = 4.0;

/// The planes of a group: 0 = luma, 1 and 2 = the colour differences.
const LUMA: &[usize] = &[0];
const CHROMA: &[usize] = &[1, 2];

/// Luma or the colour differences: the planes filtered together and their noise level.
#[derive(Clone, Copy)]
struct Group {
    planes: &'static [usize],
    /// Noise standard deviation in display units.
    sigma: f32,
}

struct Params {
    luma: f32,
    chroma: f32,
    frames: usize,
    detail: f32,
    show: bool,
}

/// A percentage as 0..1; anything that is not a number counts as 0.
fn unit(percent: f32) -> f32 {
    if percent.is_nan() { 0.0 } else { (percent / 100.0).clamp(0.0, 1.0) }
}

impl Params {
    fn read(e: &EffectInstance, cx: &FxCtx) -> Params {
        let radius = fv(e, "temporal", cx);
        Params {
            luma: unit(fv(e, "luma", cx)),
            chroma: unit(fv(e, "chroma", cx)),
            frames: if radius.is_nan() { 0 } else { radius.round().clamp(0.0, MAX_FRAMES as f32) as usize },
            detail: unit(fv(e, "detail", cx)),
            show: bv(e, "show_noise"),
        }
    }
}

/// Search window radius in working pixels: 2 at full size, scaled with `px_scale`.
fn search_radius(px_scale: f32) -> usize {
    let scale = if px_scale.is_finite() && px_scale > 0.0 { px_scale.min(4.0) } else { 1.0 };
    ((2.0 * scale).round() as usize).clamp(1, MAX_SEARCH)
}

/// Weight of a candidate whose patch distance is `d2` when the noise variance is `s2`.
#[inline]
fn match_weight(d2: f32, s2: f32, kappa: f32) -> f32 {
    let w = (-(d2 - 2.0 * s2).max(0.0) / (kappa * s2)).exp();
    if w >= W_MIN { w } else { 0.0 }
}

// ------------------------------------------------------------------ planes

/// One channel of an image with [`PAD`] replicated pixels around it.
struct Plane {
    stride: usize,
    v: Vec<f32>,
}

impl Plane {
    /// A padded plane whose pixel `i` (row-major, unpadded) is `value(i)`; the border repeats the edge.
    fn build(w: usize, h: usize, value: impl Fn(usize) -> f32 + Sync) -> Plane {
        let stride = w + 2 * PAD;
        let mut v = vec![0.0; stride * (h + 2 * PAD)];
        v.par_chunks_mut(stride).enumerate().for_each(|(r, row)| {
            let y = r.saturating_sub(PAD).min(h.saturating_sub(1));
            for (c, out) in row.iter_mut().enumerate() {
                let x = c.saturating_sub(PAD).min(w.saturating_sub(1));
                *out = value(y * w + x);
            }
        });
        Plane { stride, v }
    }
    /// The value at padded index `base` moved by `offset` (0 outside the buffer, which cannot happen
    /// within [`PAD`] of a real pixel).
    #[inline]
    fn at(&self, base: usize, offset: isize) -> f32 {
        self.v.get(base.wrapping_add_signed(offset)).copied().unwrap_or(0.0)
    }
    /// Unpadded pixel `i` of an image `w` wide.
    fn pixel(&self, w: usize, i: usize) -> f32 {
        let w = w.max(1);
        self.v.get((i / w + PAD) * self.stride + i % w + PAD).copied().unwrap_or(0.0)
    }
}

/// An image as luma and colour-difference planes plus alpha, display-encoded.
struct Frame {
    w: usize,
    h: usize,
    ch: [Plane; 3],
    a: Plane,
}

/// The sRGB curve mirrored around 0, so an out-of-gamut (negative) channel keeps its sign.
fn to_display(v: f32) -> f32 {
    linear_to_srgb(v.abs()).copysign(v)
}
fn to_linear(v: f32) -> f32 {
    srgb_to_linear(v.abs()).copysign(v)
}
/// Straight display-encoded `(Y', B' - Y', R' - Y')` and alpha of a premultiplied linear pixel.
/// Pixels that are transparent or not numbers come out with alpha 0 and are left alone.
fn analyse(px: &[f32]) -> [f32; 4] {
    let &[r, g, b, a] = px else { return [0.0; 4] };
    if !(a > ALPHA_MIN && a.is_finite() && r.is_finite() && g.is_finite() && b.is_finite()) {
        return [0.0; 4];
    }
    let [r, g, b] = [r / a, g / a, b / a].map(to_display);
    let y = luma([r, g, b]);
    [y, b - y, r - y, a.min(1.0)]
}
/// Display-encoded `(R', G', B')` of `(Y', B' - Y', R' - Y')`.
fn synthesise([y, u, v]: [f32; 3]) -> [f32; 3] {
    let (r, b) = (y + v, y + u);
    [r, (y - 0.2126 * r - 0.0722 * b) / 0.7152, b]
}

impl Frame {
    /// The planes of `img`, or `None` when its buffer does not match its size.
    fn load(img: &Image) -> Option<Frame> {
        let n = img.w.checked_mul(img.h)?;
        if n == 0 || img.px.len() != n.checked_mul(4)? {
            return None;
        }
        let (padded_w, padded_h) = (img.w.checked_add(2 * PAD)?, img.h.checked_add(2 * PAD)?);
        padded_w.checked_mul(padded_h)?;
        let analysed: Vec<[f32; 4]> = img.px.par_chunks_exact(4).map(analyse).collect();
        let plane = |k: usize| Plane::build(img.w, img.h, |i| analysed.get(i).map_or(0.0, |p| p.get(k).copied().unwrap_or(0.0)));
        Some(Frame { w: img.w, h: img.h, ch: [plane(0), plane(1), plane(2)], a: plane(3) })
    }
    /// Like [`Frame::load`], but only for an image of the same size as `like`.
    fn load_like(img: &Image, like: &Frame) -> Option<Frame> {
        if img.w != like.w || img.h != like.h { None } else { Frame::load(img) }
    }
}

/// Offsets of a 3 x 3 patch in a plane `stride` wide.
fn patch_offsets(stride: usize) -> [isize; 9] {
    let s = stride as isize;
    [-s - 1, -s, -s + 1, -1, 0, 1, s - 1, s, s + 1]
}

// ------------------------------------------------------------------ temporal pass

/// The neighbouring frames to read, as seconds from now: up to `frames` on each side, nearest
/// first, skipping those that fall outside the clip.
fn neighbour_times(env: &dyn FxEnv, cx: &FxCtx, frames: usize) -> Vec<f64> {
    let Some(fps) = [env.frame_rate(), cx.frame_rate].into_iter().find(|f| f.is_finite() && *f > 1e-3) else { return Vec::new() };
    let (now, length) = (env.clip_offset(), env.clip_seconds());
    let known = now.is_finite() && length.is_finite() && length > 0.0;
    let half = 0.5 / fps;
    let mut times = Vec::new();
    for k in 1..=frames {
        for sign in [-1.0, 1.0] {
            let dt = sign * k as f64 / fps;
            let t = now + dt;
            if !known || (t >= -half && t < length - half) {
                times.push(dt);
            }
        }
    }
    times
}

/// Adds the neighbouring frame `nb` to the group's accumulator `acc`: per pixel the weighted sum of
/// each plane followed by the sum of weights (`planes + 1` values), row-major and unpadded.
fn temporal(cur: &Frame, nb: &Frame, g: &Group, acc: &mut [f32]) {
    let m = g.planes.len();
    let s2 = g.sigma * g.sigma;
    let stride = cur.a.stride;
    // squared difference summed over the group's planes, border included
    let mut d2 = vec![0.0f32; cur.a.v.len()];
    for &k in g.planes {
        let (Some(a), Some(b)) = (cur.ch.get(k), nb.ch.get(k)) else { continue };
        d2.par_iter_mut().zip(a.v.par_iter()).zip(b.v.par_iter()).for_each(|((d, &x), &y)| *d += (x - y) * (x - y));
    }
    let patch = patch_offsets(stride);
    let den = (patch.len() * m) as f32;
    acc.par_chunks_mut(cur.w * (m + 1)).enumerate().for_each(|(y, row)| {
        for (x, cell) in row.chunks_exact_mut(m + 1).enumerate() {
            let base = (y + PAD) * stride + x + PAD;
            let (here, there) = (cur.a.at(base, 0), nb.a.at(base, 0));
            if here <= ALPHA_MIN || there <= ALPHA_MIN {
                continue;
            }
            let sum: f32 = patch.iter().map(|&o| d2.get(base.wrapping_add_signed(o)).copied().unwrap_or(0.0)).sum();
            let w = match_weight(sum / den, s2, KAPPA_TEMPORAL) * there;
            if w <= 0.0 {
                continue;
            }
            let Some((total, planes)) = cell.split_last_mut() else { continue };
            for (slot, &k) in planes.iter_mut().zip(g.planes) {
                *slot += w * nb.ch.get(k).map_or(0.0, |p| p.at(base, 0));
            }
            *total += w;
        }
    });
}

// ------------------------------------------------------------------ spatial pass

/// The group's planes after the temporal average of `acc`, then the spatial pass: per pixel the
/// filtered value of each plane, row-major and unpadded. `tolerance` scales the noise level the
/// pass accepts (Preserve Detail).
fn spatial(cur: &Frame, g: &Group, acc: &[f32], radius: usize, tolerance: f32) -> Vec<f32> {
    let (w, h) = (cur.w, cur.h);
    let m = g.planes.len().min(2);
    if m == 0 {
        return Vec::new();
    }
    let stride = cur.a.stride;
    let averaged: Vec<Plane> = g
        .planes
        .iter()
        .take(m)
        .enumerate()
        .map(|(j, &k)| {
            Plane::build(w, h, |i| {
                let own = cur.ch.get(k).map_or(0.0, |p| p.pixel(w, i));
                let sum = acc.get(i * (m + 1) + j).copied().unwrap_or(0.0);
                let weights = acc.get(i * (m + 1) + m).copied().unwrap_or(0.0);
                (own + sum) / (1.0 + weights)
            })
        })
        .collect();
    // the window: offset in a padded plane and distance falloff of every tap but the centre
    let r = radius.min(MAX_SEARCH) as isize;
    let mut taps: Vec<(isize, f32)> = Vec::new();
    for dy in -r..=r {
        for dx in -r..=r {
            if (dx, dy) != (0, 0) {
                taps.push((dy * stride as isize + dx, (-((dx * dx + dy * dy) as f32) / (2.0 * (r * r) as f32)).exp()));
            }
        }
    }
    let patch = patch_offsets(stride);
    let den = (patch.len() * m) as f32;
    let mut out = vec![0.0f32; w * h * m];
    out.par_chunks_mut(w * m).enumerate().for_each(|(y, row)| {
        for (x, o) in row.chunks_exact_mut(m).enumerate() {
            let i = y * w + x;
            let base = (y + PAD) * stride + x + PAD;
            let mut centre = [0.0f32; 2];
            for (c, p) in centre.iter_mut().zip(&averaged) {
                *c = p.at(base, 0);
            }
            // the noise left after the temporal pass shrinks with the weight it gathered
            let gathered = acc.get(i * (m + 1) + m).copied().unwrap_or(0.0);
            let s2 = (g.sigma * tolerance).powi(2) / (1.0 + gathered);
            if !(s2.is_finite() && s2 > MIN_VARIANCE) || cur.a.at(base, 0) <= ALPHA_MIN {
                for (v, c) in o.iter_mut().zip(centre) {
                    *v = c;
                }
                continue;
            }
            let mut patches = [[0.0f32; 9]; 2];
            for (pc, p) in patches.iter_mut().zip(&averaged) {
                for (v, &off) in pc.iter_mut().zip(&patch) {
                    *v = p.at(base, off);
                }
            }
            // beyond this summed distance the weight is under W_MIN
            let cut = (2.0 + LN_W_MIN * KAPPA_SPATIAL) * s2 * den;
            let (mut sum, mut total) = ([0.0f32; 2], 0.0f32);
            for &(tap, falloff) in &taps {
                let alpha = cur.a.at(base, tap);
                if alpha <= ALPHA_MIN {
                    continue;
                }
                let mut dist = 0.0f32;
                for (pc, p) in patches.iter().zip(&averaged) {
                    for (&v, &off) in pc.iter().zip(&patch) {
                        let d = v - p.at(base, tap + off);
                        dist += d * d;
                    }
                }
                if dist > cut {
                    continue;
                }
                let weight = match_weight(dist / den, s2, KAPPA_SPATIAL) * falloff * alpha;
                if weight <= 0.0 {
                    continue;
                }
                for (s, p) in sum.iter_mut().zip(&averaged) {
                    *s += weight * p.at(base, tap);
                }
                total += weight;
            }
            for ((v, c), s) in o.iter_mut().zip(centre).zip(sum) {
                *v = (c + s) / (1.0 + total);
            }
        }
    });
    out
}

// ------------------------------------------------------------------ the effect

pub fn video_denoise(img: &mut Image, e: &EffectInstance, cx: &FxCtx) {
    let prm = Params::read(e, cx);
    let groups: Vec<Group> = [Group { planes: LUMA, sigma: prm.luma * SIGMA_FULL }, Group { planes: CHROMA, sigma: prm.chroma * SIGMA_FULL }]
        .into_iter()
        .filter(|g| g.sigma * g.sigma > MIN_VARIANCE)
        .collect();
    // with no group to filter, Show Only Noise still shows what was removed: nothing, a flat grey
    if groups.is_empty() && !prm.show {
        return;
    }
    let Some(cur) = Frame::load(img) else { return };
    let mut acc: Vec<Vec<f32>> = groups.iter().map(|g| vec![0.0; cur.w * cur.h * (g.planes.len() + 1)]).collect();
    if let Some(env) = cx.env.filter(|_| !groups.is_empty()) {
        for dt in neighbour_times(env, cx, prm.frames) {
            let Some(nb) = env.frame(dt, e).and_then(|im| Frame::load_like(&im, &cur)) else { continue };
            for (g, a) in groups.iter().zip(acc.iter_mut()) {
                temporal(&cur, &nb, g, a);
            }
        }
    }
    let radius = search_radius(cx.px_scale);
    let tolerance = 1.0 - 0.75 * prm.detail;
    let filtered: Vec<Vec<f32>> = groups.iter().zip(&acc).map(|(g, a)| spatial(&cur, g, a, radius, tolerance)).collect();
    drop(acc);
    // back to premultiplied linear; alpha is never touched
    let w = cur.w;
    img.px.par_chunks_mut(4).enumerate().for_each(|(i, px)| {
        let &mut [ref mut r, ref mut g, ref mut b, a] = px else { return };
        if cur.a.pixel(w, i) <= ALPHA_MIN {
            return;
        }
        let original = cur.ch.each_ref().map(|p| p.pixel(w, i));
        let mut result = original;
        for (grp, values) in groups.iter().zip(&filtered) {
            let m = grp.planes.len();
            for (j, &k) in grp.planes.iter().enumerate() {
                if let (Some(slot), Some(&v)) = (result.get_mut(k), values.get(i * m + j)) {
                    *slot = v;
                }
            }
        }
        if !prm.show && result == original {
            return;
        }
        let shown: [f32; 3] = if prm.show {
            let (before, after) = (synthesise(original), synthesise(result));
            std::array::from_fn(|c| (0.5 + NOISE_GAIN * (before.get(c).copied().unwrap_or(0.0) - after.get(c).copied().unwrap_or(0.0))).clamp(0.0, 1.0))
        } else {
            synthesise(result)
        };
        // Luma and colour differences are averaged apart, so their sum can undershoot 0 where the
        // input did not: only a channel that was already negative keeps a negative result.
        for (slot, v) in [r, g, b].into_iter().zip(shown) {
            let linear = to_linear(v) * a;
            *slot = if *slot < 0.0 { linear } else { linear.max(0.0) };
        }
    });
}
