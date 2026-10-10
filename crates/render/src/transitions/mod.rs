//! Video transitions: combine the outgoing (A) and incoming (B) layers at progress `p` ∈ [0,1].
//!
//! Every transition is procedural maths written for FilmCraft (no third-party artwork, no masks
//! or textures from other products). The folders follow Premiere 26's Effects panel (see
//! `filmcraft_project::vtransition`). Shared building blocks:
//!
//! - [`Tx`]: the inputs plus typed parameter access (defaults come from the definition, so older
//!   projects that lack a newer parameter still render).
//! - [`paint`]: evaluate a per-pixel function in parallel.
//! - [`wipe::field_wipe`]: shape wipes from a scalar field (iris, clock, star, linear…) with
//!   feather, border width/colour and anti-aliasing.
//! - [`cards`]: textured 3D quads under a perspective camera (flips, spins, cubes, folds).
//!
//! Contract (tested for every transition): `p = 0` is exactly the outgoing frame, `p = 1` exactly
//! the incoming frame, the output is finite and deterministic, and it changes continuously near
//! both ends. There is no GPU transition path: the GPU plan pre-renders transition layers on the
//! CPU (see `plan.rs`), so this module is the only implementation.
//!
//! Immersive (VR) transitions are flat approximations on the equirectangular frame: sampling wraps
//! horizontally and the iris uses great-circle distance, but there is no sphere re-projection.
//! Morph Cut morphs with dense optical flow ([`crate::flow`]) but has no face tracking, and the
//! Smart Tools / Text transitions are procedural stand-ins (no content analysis, no typed text).

use filmcraft_project::{EffectInstance, ParamValue};
use filmcraft_time::Tick;
use rayon::prelude::*;

use crate::image::Image;

pub mod cards;
mod dissolve;
mod grunge;
mod legacy;
mod lights;
mod motion;
mod special;
pub mod wipe;

#[cfg(test)]
mod tests;

/// Render transition `e` at progress `p` (full-resolution parameters).
pub fn apply(e: &EffectInstance, a: &Image, b: &Image, p: f32) -> Image {
    apply_scaled(e, a, b, p, 1.0)
}

/// Render transition `e` at progress `p`; `scale` converts pixel parameters (border width, blur
/// length, centre) from sequence pixels to working-image pixels (½ for half-resolution playback).
pub fn apply_scaled(e: &EffectInstance, a: &Image, b: &Image, p: f32, scale: f32) -> Image {
    let p = if p.is_finite() { p.clamp(0.0, 1.0) } else { 0.0 };
    if a.w == 0 || a.h == 0 || (a.w, a.h) != (b.w, b.h) {
        return if p < 0.5 { a.clone() } else { b.clone() };
    }
    if p <= 0.0 {
        return a.clone();
    }
    if p >= 1.0 {
        return b.clone();
    }
    if let Some(img) = legacy::apply(e, a, b, p) {
        return img;
    }
    let t = Tx { e, a, b, p, w: a.w, h: a.h, wf: a.w as f32, hf: a.h as f32, scale: if scale.is_finite() && scale > 0.0 { scale } else { 1.0 } };
    let id = e.effect.as_str();
    dissolve::apply(id, &t)
        .or_else(|| wipe::apply(id, &t))
        .or_else(|| motion::apply(id, &t))
        .or_else(|| lights::apply(id, &t))
        .or_else(|| grunge::apply(id, &t))
        .or_else(|| special::apply(id, &t))
        .unwrap_or_else(|| paint(t.w, t.h, |x, y| mix(t.sa(x, y), t.sb(x, y), p)))
}

/// Audio crossfade gains (out, in) for progress p.
pub fn audio_gains(kind: &str, p: f32) -> (f32, f32) {
    match kind {
        "constant_gain" => (1.0 - p, p),
        "exponential_fade" => ((1.0 - p).powi(3), 1.0 - (1.0 - p).powi(3)),
        _ => ((p * std::f32::consts::FRAC_PI_2).cos(), (p * std::f32::consts::FRAC_PI_2).sin()),
    }
}

// ---------------------------------------------------------------------------------------------
// Inputs and parameters

/// One transition evaluation: the effect, both layers, progress and the pixel scale.
pub(crate) struct Tx<'a> {
    pub e: &'a EffectInstance,
    pub a: &'a Image,
    pub b: &'a Image,
    pub p: f32,
    pub w: usize,
    pub h: usize,
    pub wf: f32,
    pub hf: f32,
    pub scale: f32,
}

impl Tx<'_> {
    fn value(&self, id: &str) -> Option<ParamValue> {
        self.e.param(id).map(|p| p.value.clone()).or_else(|| self.e.def().and_then(|d| d.param(id)).map(|p| p.default.clone()))
    }
    /// A float parameter (definition default if the instance lacks it).
    pub fn num(&self, id: &str) -> f32 {
        let v = self.e.f64_at(id, Tick::ZERO) as f32;
        if v.is_finite() { v } else { 0.0 }
    }
    /// A percentage parameter as 0..1.
    pub fn frac(&self, id: &str) -> f32 {
        self.num(id) / 100.0
    }
    /// A pixel-size parameter in working-image pixels.
    pub fn px(&self, id: &str) -> f32 {
        self.num(id).max(0.0) * self.scale
    }
    pub fn choice(&self, id: &str) -> u32 {
        match self.value(id) {
            Some(ParamValue::Choice(c)) => c,
            _ => 0,
        }
    }
    pub fn flag(&self, id: &str) -> bool {
        matches!(self.value(id), Some(ParamValue::Bool(true)))
    }
    /// A colour parameter as opaque linear-light RGBA (colour params are stored display-encoded).
    pub fn color(&self, id: &str) -> [f32; 4] {
        let c = match self.value(id) {
            Some(ParamValue::Color(c)) => c,
            _ => [1.0; 4],
        };
        let l = |v: f32| filmcraft_color::srgb_to_linear(v.clamp(0.0, 1.0));
        [l(c[0]), l(c[1]), l(c[2]), 1.0]
    }
    /// The `center` point in working pixels (NaN components = frame centre).
    pub fn center(&self) -> (f32, f32) {
        let v = match self.value("center") {
            Some(ParamValue::Vec2(v)) => v,
            _ => filmcraft_geom::Vec2::new(f64::NAN, f64::NAN),
        };
        let x = if v.x.is_finite() { v.x as f32 * self.scale } else { self.wf / 2.0 };
        let y = if v.y.is_finite() { v.y as f32 * self.scale } else { self.hf / 2.0 };
        (x, y)
    }
    pub fn seed(&self) -> u32 {
        self.num("seed").max(0.0) as u32
    }
    /// Unit motion vector of the `direction` param: the incoming clip enters *from* that side, so
    /// "From West" moves left → right (+x).
    pub fn dir(&self) -> (f32, f32) {
        match self.choice("direction") {
            0 => (0.0, 1.0),
            1 => (-1.0, 0.0),
            2 => (0.0, -1.0),
            _ => (1.0, 0.0),
        }
    }
    /// Frame extent along the direction of travel.
    pub fn travel(&self) -> f32 {
        let (dx, dy) = self.dir();
        (dx.abs() * self.wf + dy.abs() * self.hf).max(1.0)
    }
    /// Reference size for resolution-independent defaults (1 at 1080p height).
    pub fn unit(&self) -> f32 {
        self.hf / 1080.0
    }
    #[inline]
    pub fn sa(&self, x: f32, y: f32) -> [f32; 4] {
        self.a.sample_bilinear(x, y)
    }
    #[inline]
    pub fn sb(&self, x: f32, y: f32) -> [f32; 4] {
        self.b.sample_bilinear(x, y)
    }
    #[inline]
    pub fn sac(&self, x: f32, y: f32) -> [f32; 4] {
        self.a.sample_bilinear_clamped(x, y)
    }
    #[inline]
    pub fn sbc(&self, x: f32, y: f32) -> [f32; 4] {
        self.b.sample_bilinear_clamped(x, y)
    }
    /// Pixel of A / B at integer coordinates (fast path for unwarped reads).
    #[inline]
    pub fn pa(&self, x: f32, y: f32) -> [f32; 4] {
        self.a.get_clamped(x as isize, y as isize)
    }
    #[inline]
    pub fn pb(&self, x: f32, y: f32) -> [f32; 4] {
        self.b.get_clamped(x as isize, y as isize)
    }
}

// ---------------------------------------------------------------------------------------------
// Pixel helpers

/// Evaluate `f(x, y)` at every pixel centre (x, y in pixels, +0.5 centres), in parallel.
pub(crate) fn paint(w: usize, h: usize, f: impl Fn(f32, f32) -> [f32; 4] + Sync) -> Image {
    let mut out = Image::new(w, h);
    out.px.par_chunks_mut(w * 4).enumerate().for_each(|(y, row)| {
        let fy = y as f32 + 0.5;
        for x in 0..w {
            let c = f(x as f32 + 0.5, fy);
            row[x * 4..x * 4 + 4].copy_from_slice(&c);
        }
    });
    out
}

#[inline]
pub(crate) fn mix(a: [f32; 4], b: [f32; 4], k: f32) -> [f32; 4] {
    [a[0] + (b[0] - a[0]) * k, a[1] + (b[1] - a[1]) * k, a[2] + (b[2] - a[2]) * k, a[3] + (b[3] - a[3]) * k]
}
/// Premultiplied `src` over `dst`.
#[inline]
pub(crate) fn over(dst: [f32; 4], src: [f32; 4]) -> [f32; 4] {
    let k = 1.0 - src[3];
    [src[0] + dst[0] * k, src[1] + dst[1] * k, src[2] + dst[2] * k, src[3] + dst[3] * k]
}
#[inline]
pub(crate) fn scale4(c: [f32; 4], k: f32) -> [f32; 4] {
    [c[0] * k, c[1] * k, c[2] * k, c[3] * k]
}
/// Add light `rgb × k` (keeps alpha at least as opaque as the light).
#[inline]
pub(crate) fn add_light(c: [f32; 4], rgb: [f32; 4], k: f32) -> [f32; 4] {
    [c[0] + rgb[0] * k, c[1] + rgb[1] * k, c[2] + rgb[2] * k, (c[3] + k.max(0.0)).min(1.0)]
}
#[inline]
pub(crate) fn luma(c: [f32; 4]) -> f32 {
    0.2126 * c[0] + 0.7152 * c[1] + 0.0722 * c[2]
}
#[inline]
pub(crate) fn smoothstep(e0: f32, e1: f32, x: f32) -> f32 {
    if e1 <= e0 {
        return if x < e0 { 0.0 } else { 1.0 };
    }
    let t = ((x - e0) / (e1 - e0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}
/// 0 at both ends, 1 at the midpoint (smooth).
#[inline]
pub(crate) fn bell(p: f32) -> f32 {
    let t = 4.0 * p * (1.0 - p);
    t * t * (3.0 - 2.0 * t)
}
/// The A→B mix weight for "distort out, distort in" transitions: switches around the midpoint.
#[inline]
pub(crate) fn mid_mix(p: f32) -> f32 {
    smoothstep(0.3, 0.7, p)
}
#[inline]
pub(crate) fn ease_in_out(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    if t < 0.5 { 4.0 * t * t * t } else { 1.0 - (-2.0 * t + 2.0).powi(3) / 2.0 }
}
#[inline]
pub(crate) fn ease_out(t: f32) -> f32 {
    1.0 - (1.0 - t.clamp(0.0, 1.0)).powi(3)
}
#[inline]
pub(crate) fn ease_in(t: f32) -> f32 {
    t.clamp(0.0, 1.0).powi(3)
}

/// Integer hash → [0, 1).
#[inline]
pub(crate) fn hash(x: i32, y: i32, s: u32) -> f32 {
    let mut h = (x as u32).wrapping_mul(0x8DA6_B343) ^ (y as u32).wrapping_mul(0xD816_3841) ^ s.wrapping_mul(0xCB1A_B31F) ^ 0x9E37_79B9;
    h ^= h >> 15;
    h = h.wrapping_mul(0x2C1B_3C6D);
    h ^= h >> 12;
    h = h.wrapping_mul(0x297A_2D39);
    h ^= h >> 15;
    (h >> 8) as f32 / (1u32 << 24) as f32
}
/// Smooth value noise in [0, 1) with unit cell size.
pub(crate) fn vnoise(x: f32, y: f32, s: u32) -> f32 {
    let (xi, yi) = (x.floor(), y.floor());
    let (fx, fy) = (x - xi, y - yi);
    let (ux, uy) = (fx * fx * (3.0 - 2.0 * fx), fy * fy * (3.0 - 2.0 * fy));
    let (i, j) = (xi as i32, yi as i32);
    let a = hash(i, j, s);
    let b = hash(i + 1, j, s);
    let c = hash(i, j + 1, s);
    let d = hash(i + 1, j + 1, s);
    let top = a + (b - a) * ux;
    let bot = c + (d - c) * ux;
    top + (bot - top) * uy
}
/// Fractal (4-octave) value noise in ~[0, 1).
pub(crate) fn fbm(x: f32, y: f32, s: u32) -> f32 {
    let (mut v, mut amp, mut f, mut norm) = (0.0, 0.5, 1.0, 0.0);
    for o in 0..4 {
        v += vnoise(x * f, y * f, s.wrapping_add(o * 101)) * amp;
        norm += amp;
        amp *= 0.5;
        f *= 2.03;
    }
    v / norm
}

/// Average of `n` samples of `img` along the segment centred on (x, y) with half-extent (dx, dy).
pub(crate) fn line_blur(img: &Image, x: f32, y: f32, dx: f32, dy: f32, n: usize) -> [f32; 4] {
    if n <= 1 || (dx.abs() + dy.abs()) < 0.5 {
        return img.sample_bilinear_clamped(x, y);
    }
    let mut acc = [0.0; 4];
    for i in 0..n {
        let t = i as f32 / (n - 1) as f32 * 2.0 - 1.0;
        let s = img.sample_bilinear_clamped(x + dx * t, y + dy * t);
        for k in 0..4 {
            acc[k] += s[k];
        }
    }
    scale4(acc, 1.0 / n as f32)
}
/// Zoom (radial streak) blur towards (cx, cy): samples between the pixel and `k` of the way to
/// the centre.
pub(crate) fn zoom_blur(img: &Image, x: f32, y: f32, cx: f32, cy: f32, k: f32, n: usize) -> [f32; 4] {
    let (dx, dy) = ((x - cx) * k * 0.5, (y - cy) * k * 0.5);
    line_blur(img, x - dx, y - dy, dx, dy, n)
}
/// Spin blur about (cx, cy) over `ang` radians (centred on the pixel).
pub(crate) fn spin_blur(img: &Image, x: f32, y: f32, cx: f32, cy: f32, ang: f32, n: usize) -> [f32; 4] {
    if n <= 1 || ang.abs() < 1e-4 {
        return img.sample_bilinear_clamped(x, y);
    }
    let (rx, ry) = (x - cx, y - cy);
    let mut acc = [0.0; 4];
    for i in 0..n {
        let a = (i as f32 / (n - 1) as f32 - 0.5) * ang;
        let (s, c) = a.sin_cos();
        let p = img.sample_bilinear_clamped(cx + rx * c - ry * s, cy + rx * s + ry * c);
        for k in 0..4 {
            acc[k] += p[k];
        }
    }
    scale4(acc, 1.0 / n as f32)
}
/// Disc (bokeh-like) blur of radius `r` with `n` golden-angle samples.
pub(crate) fn disc_blur(img: &Image, x: f32, y: f32, r: f32, n: usize) -> [f32; 4] {
    if r < 0.5 || n <= 1 {
        return img.sample_bilinear_clamped(x, y);
    }
    let mut acc = [0.0; 4];
    for i in 0..n {
        let rr = r * ((i as f32 + 0.5) / n as f32).sqrt();
        let a = i as f32 * 2.399_963;
        let p = img.sample_bilinear_clamped(x + rr * a.cos(), y + rr * a.sin());
        for k in 0..4 {
            acc[k] += p[k];
        }
    }
    scale4(acc, 1.0 / n as f32)
}
/// Sample count for a blur of extent `len` pixels.
#[inline]
pub(crate) fn taps(len: f32) -> usize {
    ((len.abs() * 0.5).ceil() as usize).clamp(1, 40)
}
/// Gaussian-blurred copy (σ in pixels).
pub(crate) fn blurred(img: &Image, sigma: f32) -> Image {
    let mut out = img.clone();
    if sigma > 0.3 {
        crate::effects::gaussian(&mut out, sigma, sigma, true);
    }
    out
}
/// Horizontal-wrap bilinear sample (equirectangular frames).
pub(crate) fn sample_wrap(img: &Image, x: f32, y: f32) -> [f32; 4] {
    let w = img.w as f32;
    let xw = (x - 0.5).rem_euclid(w);
    let x0 = xw.floor();
    let tx = xw - x0;
    let x0 = x0 as usize % img.w;
    let x1 = (x0 + 1) % img.w;
    let fy = (y - 0.5).clamp(0.0, img.h as f32 - 1.0);
    let y0 = fy.floor() as usize;
    let y1 = (y0 + 1).min(img.h - 1);
    let ty = fy - y0 as f32;
    let (a, b, c, d) = (img.get(x0, y0), img.get(x1, y0), img.get(x0, y1), img.get(x1, y1));
    let mut o = [0.0; 4];
    for k in 0..4 {
        let top = a[k] + (b[k] - a[k]) * tx;
        let bot = c[k] + (d[k] - c[k]) * tx;
        o[k] = top + (bot - top) * ty;
    }
    o
}

/// Average a per-pixel scene over `n` time samples spread over `shutter` (in progress units)
/// around `p` (motion blur). `n = 1` or `shutter = 0` evaluates once at `p`.
pub(crate) fn temporal(x: f32, y: f32, p: f32, shutter: f32, n: usize, f: &(impl Fn(f32, f32, f32) -> [f32; 4] + Sync)) -> [f32; 4] {
    if n <= 1 || shutter <= 1e-5 {
        return f(x, y, p);
    }
    let mut acc = [0.0; 4];
    for i in 0..n {
        let q = (p + (i as f32 / (n - 1) as f32 - 0.5) * shutter).clamp(0.0, 1.0);
        let c = f(x, y, q);
        for k in 0..4 {
            acc[k] += c[k];
        }
    }
    scale4(acc, 1.0 / n as f32)
}
/// Motion-blur settings from the `motion_blur` param: (shutter in progress units, samples).
pub(crate) fn shutter(t: &Tx, max: f32) -> (f32, usize) {
    let mb = t.frac("motion_blur").clamp(0.0, 1.0);
    if mb <= 0.0 { (0.0, 1) } else { (max * mb, ((mb * 12.0).ceil() as usize).clamp(2, 12)) }
}
