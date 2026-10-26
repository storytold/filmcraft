//! Immersive Video (VR) effects on equirectangular frames.
//!
//! Every effect treats each view (the whole frame, or the two halves of an over/under or
//! side-by-side stereoscopic layout) as a full sphere: x is longitude −180°…180° (wrapping), y is
//! latitude 90°…−90°. Filters are seam-free across the ±180° edge and widen horizontally towards
//! the poles (an equirectangular row at latitude φ covers cos φ of the equator's length).

use std::f64::consts::{PI, TAU};

use filmcraft_project::EffectInstance;
use rayon::prelude::*;

use super::*;
use crate::gpufx_multipass::{MultipassPlan, Pass, Slot};

type V3 = [f64; 3];

#[inline]
fn dir(lon: f64, lat: f64) -> V3 {
    [lat.cos() * lon.sin(), lat.sin(), lat.cos() * lon.cos()]
}
#[inline]
fn lonlat(d: V3) -> (f64, f64) {
    (d[0].atan2(d[2]), d[1].clamp(-1.0, 1.0).asin())
}
#[inline]
fn dot(a: V3, b: V3) -> f64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}
#[inline]
fn cross(a: V3, b: V3) -> V3 {
    [a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0]]
}

type M3 = [[f64; 3]; 3];
fn mmul(a: M3, b: M3) -> M3 {
    let mut o = [[0.0; 3]; 3];
    for i in 0..3 {
        for j in 0..3 {
            o[i][j] = (0..3).map(|k| a[i][k] * b[k][j]).sum();
        }
    }
    o
}
fn mvec(m: &M3, v: V3) -> V3 {
    [dot(m[0], v), dot(m[1], v), dot(m[2], v)]
}
fn transpose(m: M3) -> M3 {
    [[m[0][0], m[1][0], m[2][0]], [m[0][1], m[1][1], m[2][1]], [m[0][2], m[1][2], m[2][2]]]
}
/// Rotation from tilt (about X), pan (about Y) and roll (about Z), degrees: R = Ry · Rx · Rz.
fn rotation(tilt: f64, pan: f64, roll: f64) -> M3 {
    let (sx, cx) = tilt.to_radians().sin_cos();
    let (sy, cy) = pan.to_radians().sin_cos();
    let (sz, cz) = roll.to_radians().sin_cos();
    let rx = [[1.0, 0.0, 0.0], [0.0, cx, -sx], [0.0, sx, cx]];
    let ry = [[cy, 0.0, sy], [0.0, 1.0, 0.0], [-sy, 0.0, cy]];
    let rz = [[cz, -sz, 0.0], [sz, cz, 0.0], [0.0, 0.0, 1.0]];
    mmul(ry, mmul(rx, rz))
}

#[inline]
fn px_to_lonlat(x: f64, y: f64, w: usize, h: usize) -> (f64, f64) {
    ((x / w as f64 - 0.5) * TAU, (0.5 - y / h as f64) * PI)
}

/// Bilinear sample with horizontal wrap (vertical clamp), pixel centres at +0.5.
fn sample_wrap(img: &Image, x: f64, y: f64) -> [f32; 4] {
    let (w, h) = (img.w as isize, img.h as isize);
    let fx = x - 0.5;
    let fy = (y - 0.5).clamp(0.0, (h - 1) as f64);
    let (x0, y0) = (fx.floor(), fy.floor());
    let (tx, ty) = ((fx - x0) as f32, (fy - y0) as f32);
    let (x0, y0) = (x0 as isize, y0 as isize);
    let g = |xx: isize, yy: isize| img.get(xx.rem_euclid(w) as usize, yy.clamp(0, h - 1) as usize);
    let (a, b, c, d) = (g(x0, y0), g(x0 + 1, y0), g(x0, y0 + 1), g(x0 + 1, y0 + 1));
    let mut o = [0.0; 4];
    for k in 0..4 {
        let top = a[k] + (b[k] - a[k]) * tx;
        let bot = c[k] + (d[k] - c[k]) * tx;
        o[k] = top + (bot - top) * ty;
    }
    o
}
fn sample_dir(img: &Image, d: V3) -> [f32; 4] {
    let (lon, lat) = lonlat(d);
    sample_wrap(img, (lon / TAU + 0.5) * img.w as f64, (0.5 - lat / PI) * img.h as f64)
}

/// Run `f` on each view of the frame layout.
fn per_view(img: &mut Image, layout: u32, f: impl Fn(&mut Image)) {
    let (w, h) = (img.w, img.h);
    let views: Vec<(usize, usize, usize, usize)> = match layout {
        1 if h >= 2 => vec![(0, 0, w, h / 2), (0, h / 2, w, h - h / 2)],
        2 if w >= 2 => vec![(0, 0, w / 2, h), (w / 2, 0, w - w / 2, h)],
        _ => vec![(0, 0, w, h)],
    };
    if views.len() == 1 {
        f(img);
        return;
    }
    for (x0, y0, vw, vh) in views {
        let mut v = Image::new(vw, vh);
        for y in 0..vh {
            let s = ((y0 + y) * w + x0) * 4;
            v.px[y * vw * 4..(y + 1) * vw * 4].copy_from_slice(&img.px[s..s + vw * 4]);
        }
        f(&mut v);
        for y in 0..vh {
            let s = ((y0 + y) * w + x0) * 4;
            img.px[s..s + vw * 4].copy_from_slice(&v.px[y * vw * 4..(y + 1) * vw * 4]);
        }
    }
}

/// Box radius of row `y` (of a `h`-row view `w` wide) in the three-pass box approximation of a
/// Gaussian of `sigma / cos(latitude)` (capped at a quarter turn). 0: the row is left alone.
///
/// The WGSL kernel `vr_blur_row` in `crates/gpu/src/vr.wgsl` mirrors this function and
/// [`blur_row_wrap`]; both are the spec for it. Rounding is `floor(x + 0.5)` (not `round()`, whose
/// ties differ between Rust and WGSL).
pub(crate) fn row_radius(sigma: f32, w: usize, h: usize, y: usize) -> usize {
    let lat = (0.5 - (y as f64 + 0.5) / h as f64) * PI;
    let s = (sigma as f64 / lat.cos().max(1e-3)).min(w as f64 / 4.0) as f32;
    if s <= 0.3 {
        return 0;
    }
    let r = ((((12.0 * s * s / 3.0 + 1.0).sqrt() - 1.0) / 2.0 + 0.5).floor().max(1.0) as isize).min((w as isize - 1) / 2);
    r.max(0) as usize
}

/// One box pass of radius `r` along one row (`w` RGBA pixels) with wrap-around: `dst` gets the
/// mean of the `2r + 1` pixels centred on each pixel. Sliding window sums, O(w) whatever `r` is.
/// The WGSL kernel `vr_blur_row` is this function.
pub(crate) fn box_row_wrap(src: &[f32], dst: &mut [f32], w: usize, r: usize) {
    if w == 0 {
        return;
    }
    let (wi, r) = (w as isize, r as isize);
    let inv = 1.0 / (2 * r + 1) as f32;
    // sliding window sum with wrap-around
    let mut acc = [0f32; 4];
    for j in -r..=r {
        let i = j.rem_euclid(wi) as usize;
        for k in 0..4 {
            acc[k] += src[i * 4 + k];
        }
    }
    for x in 0..wi {
        for k in 0..4 {
            dst[x as usize * 4 + k] = acc[k] * inv;
        }
        let (out_i, in_i) = ((x - r).rem_euclid(wi) as usize, (x + r + 1).rem_euclid(wi) as usize);
        for k in 0..4 {
            acc[k] += src[in_i * 4 + k] - src[out_i * 4 + k];
        }
    }
}

/// Three box passes of radius `r` along one row (`w` RGBA pixels) with wrap-around.
pub(crate) fn blur_row_wrap(row: &mut [f32], w: usize, r: usize) {
    if r == 0 || w == 0 {
        return;
    }
    let mut tmp = vec![0f32; row.len()];
    for _ in 0..3 {
        box_row_wrap(row, &mut tmp, w, r);
        row.copy_from_slice(&tmp);
    }
}

/// Three-pass box approximation of a Gaussian along each row with wrap-around (see [`row_radius`]).
fn blur_rows_wrap(img: &mut Image, sigma: f32) {
    let (w, h) = (img.w, img.h);
    if w == 0 {
        return;
    }
    img.px.par_chunks_mut(w * 4).enumerate().for_each(|(y, row)| blur_row_wrap(row, w, row_radius(sigma, w, h, y)));
}

fn vr_blur_image(img: &mut Image, sigma: f32) {
    if sigma <= 0.3 {
        return;
    }
    blur_rows_wrap(img, sigma);
    crate::effects::gaussian(img, 0.0, sigma, true);
}

pub fn apply(img: &mut Image, e: &EffectInstance, cx: &FxCtx) {
    let layout = chv(e, "frame_layout");
    match e.effect.as_str() {
        "vr_blur" => {
            let s = fv(e, "blurriness", cx) * cx.px_scale * 0.5;
            per_view(img, layout, |v| vr_blur_image(v, s));
        }
        "vr_sharpen" => {
            let amt = fv(e, "amount", cx) / 100.0;
            if amt <= 0.0 {
                return;
            }
            let s = cx.px_scale.max(0.35) * 1.2;
            per_view(img, layout, |v| {
                let mut b = v.clone();
                vr_blur_image(&mut b, s);
                v.px.par_chunks_mut(4).zip(b.px.par_chunks(4)).for_each(|(p, q)| {
                    for k in 0..3 {
                        p[k] = (p[k] + (p[k] - q[k]) * amt).max(0.0);
                    }
                });
            });
        }
        "vr_denoise" => {
            let level = fv(e, "amount", cx) / 100.0;
            if level <= 0.0 {
                return;
            }
            let slow = chv(e, "noise_type") == 1;
            let show = bv(e, "show_noise");
            let s = (level * if slow { 3.0 } else { 1.8 }) * cx.px_scale.max(0.35);
            let thr = 0.02 + level * 0.12;
            per_view(img, layout, |v| {
                let mut b = v.clone();
                vr_blur_image(&mut b, s);
                v.px.par_chunks_mut(4).zip(b.px.par_chunks(4)).for_each(|(p, q)| {
                    let d = (0..3).map(|k| (p[k] - q[k]).abs()).fold(0.0, f32::max);
                    let k = (-(d * d) / (thr * thr)).exp();
                    let o: Vec<f32> = (0..4).map(|c| p[c] + (q[c] - p[c]) * k).collect();
                    if show {
                        for c in 0..3 {
                            p[c] = ((p[c] - o[c]) * 4.0 + 0.18).max(0.0) * p[3];
                        }
                    } else {
                        p.copy_from_slice(&o);
                    }
                });
            });
        }
        "vr_glow" => {
            let th = fv(e, "threshold", cx) / 100.0;
            let r = fv(e, "radius", cx) * cx.px_scale * 0.5;
            let br = fv(e, "brightness", cx);
            let sat = fv(e, "saturation", cx);
            let tint = bv(e, "use_tint").then(|| lin(cv(e, "tint", cx)));
            per_view(img, layout, |v| {
                let mut hi = highlights(v, th, 0.1);
                vr_blur_image(&mut hi, r);
                v.px.par_chunks_mut(4).zip(hi.px.par_chunks(4)).for_each(|(p, q)| {
                    let l = luma([q[0], q[1], q[2]]);
                    let mut g = [0, 1, 2].map(|k| (l + (q[k] - l) * sat).max(0.0));
                    if let Some(t) = tint {
                        g = t.map(|c| c * l);
                    }
                    add_light(p, g.map(|c| c * br));
                });
            });
        }
        "vr_chromatic_aberrations" => {
            let ab = [fv(e, "red", cx), fv(e, "green", cx), fv(e, "blue", cx)];
            if ab.iter().all(|a| a.abs() < 1e-4) {
                return;
            }
            let center = dir((fv(e, "center_x", cx) as f64).to_radians(), (fv(e, "center_y", cx) as f64).to_radians());
            let p = 0.5 + fv(e, "falloff", cx) as f64 / 50.0;
            let inverse = bv(e, "inverse");
            per_view(img, layout, |v| {
                let src = v.clone();
                let (w, h) = (v.w, v.h);
                v.px.par_chunks_mut(w * 4).enumerate().for_each(|(y, row)| {
                    for x in 0..w {
                        let (lon, lat) = px_to_lonlat(x as f64 + 0.5, y as f64 + 0.5, w, h);
                        let d = dir(lon, lat);
                        let th = dot(d, center).clamp(-1.0, 1.0).acos();
                        let mut f = (th / PI).powf(p);
                        if inverse {
                            f = 1.0 - f;
                        }
                        let axis = cross(center, d);
                        let al = dot(axis, axis).sqrt();
                        if al > 1e-9 {
                            let k = [axis[0] / al, axis[1] / al, axis[2] / al];
                            let px = &mut row[x * 4..x * 4 + 3];
                            for (c, v) in px.iter_mut().enumerate() {
                                let ang = ab[c] as f64 / 100.0 * 0.15 * f;
                                // rotate d about k by −ang (Rodrigues)
                                let (s, co) = (-ang).sin_cos();
                                let kd = cross(k, d);
                                let kdot = dot(k, d);
                                let r = [0, 1, 2].map(|i| d[i] * co + kd[i] * s + k[i] * kdot * (1.0 - co));
                                *v = sample_dir(&src, r)[c];
                            }
                        }
                    }
                });
            });
        }
        "vr_color_gradients" => {
            let pts: Vec<(V3, [f32; 3])> = (1..=3)
                .map(|i| {
                    let lon = fv(e, ["p1_lon", "p2_lon", "p3_lon"][i - 1], cx) as f64;
                    let lat = fv(e, ["p1_lat", "p2_lat", "p3_lat"][i - 1], cx) as f64;
                    (dir(lon.to_radians(), lat.to_radians()), lin(cv(e, ["c1", "c2", "c3"][i - 1], cx)))
                })
                .collect();
            let expo = 1.0 + fv(e, "blend", cx) as f64 / 25.0;
            let op = fv(e, "opacity", cx) / 100.0;
            let mode = chv(e, "mode");
            per_view(img, layout, |v| {
                let (w, h) = (v.w, v.h);
                v.px.par_chunks_mut(w * 4).enumerate().for_each(|(y, row)| {
                    for x in 0..w {
                        let (lon, lat) = px_to_lonlat(x as f64 + 0.5, y as f64 + 0.5, w, h);
                        let d = dir(lon, lat);
                        let mut acc = [0.0f64; 3];
                        let mut ws = 0.0;
                        for (pd, c) in &pts {
                            let ang = dot(d, *pd).clamp(-1.0, 1.0).acos().max(1e-3);
                            let wgt = 1.0 / ang.powf(expo);
                            for k in 0..3 {
                                acc[k] += c[k] as f64 * wgt;
                            }
                            ws += wgt;
                        }
                        let g = acc.map(|v| (v / ws) as f32);
                        let p = &mut row[x * 4..x * 4 + 4];
                        let base = Image::unpremul([p[0], p[1], p[2], p[3]]);
                        let o = lerp3(base, blend_simple(mode, base, g), op);
                        let a = p[3] + (1.0 - p[3]) * op;
                        p.copy_from_slice(&[o[0] * a, o[1] * a, o[2] * a, a]);
                    }
                });
            });
        }
        "vr_digital_glitch" => {
            let amp = fv(e, "amplitude", cx) / 100.0;
            if amp <= 0.0 {
                return;
            }
            let cplx = fv(e, "distortion", cx).max(1.0);
            let rate = fv(e, "rate", cx) as f64;
            let colour = fv(e, "color", cx) / 100.0;
            let scan = fv(e, "scanlines", cx) / 100.0;
            let seed = fv(e, "seed", cx).max(0.0) as u64;
            let tick = (cx.seconds * rate).floor() as i64;
            per_view(img, layout, |v| {
                let src = v.clone();
                let (w, h) = (v.w, v.h);
                let band = (h as f32 / cplx).max(1.0);
                v.px.par_chunks_mut(w * 4).enumerate().for_each(|(y, row)| {
                    let b = (y as f32 / band) as i64;
                    let r = hash01(b, tick, 0, seed);
                    let active = r < amp;
                    let shift = if active { (hash01(b, tick, 1, seed) - 0.5) * w as f32 * 0.3 * amp } else { 0.0 };
                    let cs = colour * amp * 12.0;
                    for x in 0..w {
                        let (fx, fy) = (x as f64 + 0.5 + shift as f64, y as f64 + 0.5);
                        let mut o = sample_wrap(&src, fx, fy);
                        if active && cs > 0.0 {
                            o[0] = sample_wrap(&src, fx + cs as f64, fy)[0];
                            o[2] = sample_wrap(&src, fx - cs as f64, fy)[2];
                        }
                        if scan > 0.0 && y % 2 == 1 {
                            for c in &mut o[..3] {
                                *c *= 1.0 - scan * 0.6;
                            }
                        }
                        row[x * 4..x * 4 + 4].copy_from_slice(&o);
                    }
                });
            });
        }
        "vr_fractal_noise" => {
            let kind = chv(e, "fractal_type");
            let invert = bv(e, "invert");
            let contrast = fv(e, "contrast", cx) / 100.0;
            let bright = fv(e, "brightness", cx) / 100.0;
            let scale = (fv(e, "scale", cx) / 100.0).max(0.01);
            let cplx = fv(e, "complexity", cx);
            let evo = fv(e, "evolution", cx) / 360.0;
            let op = fv(e, "opacity", cx) / 100.0;
            let mode = chv(e, "mode");
            let f = 4.0 / scale;
            per_view(img, layout, |v| {
                let (w, h) = (v.w, v.h);
                v.px.par_chunks_mut(w * 4).enumerate().for_each(|(y, row)| {
                    for x in 0..w {
                        let (lon, lat) = px_to_lonlat(x as f64 + 0.5, y as f64 + 0.5, w, h);
                        let d = dir(lon, lat);
                        let mut n = fbm(d[0] as f32 * f + evo * 0.31, d[1] as f32 * f + evo * 0.17, d[2] as f32 * f + evo * 0.11, cplx, 3);
                        n = match kind {
                            1 => n.abs().sqrt(),
                            2 => n.abs(),
                            _ => n * 0.5 + 0.5,
                        };
                        let mut g = ((n - 0.5) * contrast + 0.5 + bright).clamp(0.0, 1.0);
                        if invert {
                            g = 1.0 - g;
                        }
                        let gl = filmcraft_color::srgb_to_linear(g);
                        let p = &mut row[x * 4..x * 4 + 4];
                        let base = Image::unpremul([p[0], p[1], p[2], p[3]]);
                        let o = lerp3(base, blend_simple(mode, base, [gl; 3]), op);
                        let a = p[3] + (1.0 - p[3]) * op;
                        p.copy_from_slice(&[o[0] * a, o[1] * a, o[2] * a, a]);
                    }
                });
            });
        }
        "vr_plane_to_sphere" => {
            let fov = (fv(e, "scale", cx) as f64).to_radians().clamp(1e-3, PI * 0.999);
            let r = transpose(rotation(fv(e, "tilt", cx) as f64, fv(e, "pan", cx) as f64, fv(e, "roll", cx) as f64));
            let feather = fv(e, "feather", cx) / 100.0;
            per_view(img, layout, |v| {
                let src = v.clone();
                let (w, h) = (v.w, v.h);
                let tx = (fov / 2.0).tan();
                let ty = tx * h as f64 / w as f64;
                v.px.par_chunks_mut(w * 4).enumerate().for_each(|(y, row)| {
                    for x in 0..w {
                        let (lon, lat) = px_to_lonlat(x as f64 + 0.5, y as f64 + 0.5, w, h);
                        let d = mvec(&r, dir(lon, lat));
                        let mut o = [0.0f32; 4];
                        if d[2] > 1e-6 {
                            let (u, vv) = (d[0] / d[2] / tx, -d[1] / d[2] / ty);
                            if u.abs() <= 1.0 && vv.abs() <= 1.0 {
                                o = src.sample_bilinear_clamped(((u + 1.0) / 2.0 * w as f64) as f32, ((vv + 1.0) / 2.0 * h as f64) as f32);
                                if feather > 0.0 {
                                    let edge = (1.0 - u.abs().max(vv.abs())) as f32;
                                    let k = smoothstep(0.0, feather, edge);
                                    o = o.map(|c| c * k);
                                }
                            }
                        }
                        row[x * 4..x * 4 + 4].copy_from_slice(&o);
                    }
                });
            });
        }
        "vr_projection" | "vr_rotate_sphere" => {
            let r = rotation(fv(e, "tilt", cx) as f64, fv(e, "pan", cx) as f64, fv(e, "roll", cx) as f64);
            let fov = if e.effect == "vr_projection" { fv(e, "fov", cx) as f64 } else { 360.0 };
            let stretch = e.effect == "vr_projection" && bv(e, "stretch");
            let ident = (0..3).all(|i| (0..3).all(|j| (r[i][j] - if i == j { 1.0 } else { 0.0 }).abs() < 1e-12));
            if ident && (fov - 360.0).abs() < 1e-9 {
                return;
            }
            let rt = transpose(r);
            per_view(img, layout, |v| {
                let src = v.clone();
                let (w, h) = (v.w, v.h);
                let (kx, ky) = (fov / 360.0, if stretch { (fov / 180.0).min(1.0) } else { 1.0 });
                v.px.par_chunks_mut(w * 4).enumerate().for_each(|(y, row)| {
                    for x in 0..w {
                        let (lon, lat) = px_to_lonlat(x as f64 + 0.5, y as f64 + 0.5, w, h);
                        let d = mvec(&rt, dir(lon * kx, lat * ky));
                        row[x * 4..x * 4 + 4].copy_from_slice(&sample_dir(&src, d));
                    }
                });
            });
        }
        _ => {}
    }
}

/// One immersive blur-family route: the evaluated numbers the GPU plan needs (see [`vr_plan`]).
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum VrKind {
    /// `vr_blur`: row blur then vertical box Gaussian, per view.
    Blur { sigma: f32 },
    /// `vr_sharpen`: the blur, then `p + (p - blur) * amount` on RGB.
    Sharpen { sigma: f32, amount: f32 },
    /// `vr_denoise`: the blur, then the edge-aware blend with threshold `threshold`.
    Denoise { sigma: f32, threshold: f32, show: bool },
}

/// Kernel names of the immersive plan (entry points in `crates/gpu/src/vr.wgsl`).
pub const VR_KERNEL_ROW: &str = "vr_blur_row";
pub const VR_KERNEL_COL: &str = "vr_box_col";
pub const VR_KERNEL_COPY: &str = "vr_copy";
pub const VR_KERNEL_SHARPEN: &str = "vr_sharpen_combine";
pub const VR_KERNEL_DENOISE: &str = "vr_denoise_combine";

/// The views of a frame layout, as `per_view` splits them: (x0, y0, w, h).
pub(crate) fn views_of(w: usize, h: usize, layout: u32) -> Vec<(usize, usize, usize, usize)> {
    match layout {
        1 if h >= 2 => vec![(0, 0, w, h / 2), (0, h / 2, w, h - h / 2)],
        2 if w >= 2 => vec![(0, 0, w / 2, h), (w / 2, 0, w - w / 2, h)],
        _ => vec![(0, 0, w, h)],
    }
}

/// Little-endian u32 words of a parameter block, zero-padded to the 16-byte multiple the plan needs.
fn words(v: &[u32]) -> Vec<u8> {
    let mut out: Vec<u8> = v.iter().flat_map(|w| w.to_le_bytes()).collect();
    out.resize(out.len().next_multiple_of(16), 0);
    out
}

/// The multi-pass plan of a VR blur-family route on a `w`×`h` frame with frame `layout`. Slot 0
/// is the source; slots 1 and 2 ping-pong; slot 3 receives each view's result (the result slot).
/// Each view runs its own chain, so its latitude and vertical Gaussian use the view's size.
/// None when the frame is empty or a dimension does not fit the plan's `u32` parameters.
pub fn vr_plan(kind: VrKind, layout: u32, w: usize, h: usize) -> Option<MultipassPlan> {
    let (w32, h32) = (u32::try_from(w).ok()?, u32::try_from(h).ok()?);
    if w32 == 0 || h32 == 0 {
        return None;
    }
    let sigma = match kind {
        VrKind::Blur { sigma } | VrKind::Sharpen { sigma, .. } | VrKind::Denoise { sigma, .. } => sigma,
    };
    let mut passes = Vec::new();
    for (x0, y0, vw, vh) in views_of(w, h, layout) {
        let rect = [x0, y0, vw, vh].map(|v| u32::try_from(v).unwrap_or(u32::MAX));
        let mut cur = Slot::SOURCE;
        // three row box passes, ping-pong between slots 1 and 2
        for k in 0..3u8 {
            let out = Slot(1 + k % 2);
            passes.push(Pass { kernel: VR_KERNEL_ROW, inputs: vec![cur], output: out, params: words(&[rect[0], rect[1], rect[2], rect[3], sigma.to_bits()]) });
            cur = out;
        }
        // vertical box passes of the view's Gaussian (repeat edge), same ping-pong
        let (_, ry) = crate::effects::gaussian_boxes(vw, vh, 0.0, sigma);
        for r in ry.into_iter().filter(|r| *r > 0) {
            let out = Slot(if cur == Slot(1) { 2 } else { 1 });
            passes.push(Pass { kernel: VR_KERNEL_COL, inputs: vec![cur], output: out, params: words(&[rect[0], rect[1], rect[2], rect[3], r]) });
            cur = out;
        }
        match kind {
            VrKind::Blur { .. } => {
                passes.push(Pass { kernel: VR_KERNEL_COPY, inputs: vec![cur], output: Slot(3), params: words(&rect) });
            }
            VrKind::Sharpen { amount, .. } => {
                let p = [rect[0], rect[1], rect[2], rect[3], amount.to_bits(), 0, 0];
                passes.push(Pass { kernel: VR_KERNEL_SHARPEN, inputs: vec![Slot::SOURCE, cur], output: Slot(3), params: words(&p) });
            }
            VrKind::Denoise { threshold, show, .. } => {
                let p = [rect[0], rect[1], rect[2], rect[3], 0, u32::from(show), threshold.to_bits()];
                passes.push(Pass { kernel: VR_KERNEL_DENOISE, inputs: vec![Slot::SOURCE, cur], output: Slot(3), params: words(&p) });
            }
        }
    }
    Some(MultipassPlan { passes, result: Slot(3) })
}

#[cfg(test)]
mod row_tests {
    use super::*;

    #[test]
    fn row_radius_is_zero_for_small_sigma_and_bounded_by_the_row() {
        assert_eq!(row_radius(0.2, 64, 32, 16), 0);
        assert_eq!(row_radius(0.0, 0, 0, 0), 0);
        for y in 0..32 {
            assert!(row_radius(1e6, 9, 32, y) <= 4);
        }
    }

    #[test]
    fn blur_row_wrap_keeps_a_constant_row_and_wraps_the_seam() {
        let (w, r) = (8, 2);
        let mut flat = vec![0.5f32; w * 4];
        blur_row_wrap(&mut flat, w, r);
        assert!(flat.iter().all(|v| (v - 0.5).abs() < 1e-6));

        // a single bright pixel at x = 0 spreads onto x = w-1 across the seam
        let mut row = vec![0.0f32; w * 4];
        row[0..4].copy_from_slice(&[1.0; 4]);
        blur_row_wrap(&mut row, w, r);
        assert!(row[(w - 1) * 4] > 0.0 && row[4] > 0.0);
    }

    #[test]
    fn zero_width_image_is_a_no_op() {
        let mut img = Image { w: 0, h: 4, px: Vec::new() };
        blur_rows_wrap(&mut img, 5.0);
        assert!(img.px.is_empty());
    }

    /// Executes a [`vr_plan`] with the kernels' semantics (`crates/gpu/src/vr.wgsl`), on the CPU.
    fn run_plan(plan: &MultipassPlan, src: &Image) -> Image {
        let (w, h) = (src.w, src.h);
        let mut slots: Vec<Vec<f32>> = (0..4).map(|i| if i == 0 { src.px.clone() } else { vec![0.0; w * h * 4] }).collect();
        let at = |x: usize, y: usize| (y * w + x) * 4;
        for pass in &plan.passes {
            let p: Vec<u32> = pass.params.chunks(4).map(|c| u32::from_le_bytes([c[0], c[1], c[2], c[3]])).collect();
            let (x0, y0, vw, vh) = (p[0] as usize, p[1] as usize, p[2] as usize, p[3] as usize);
            let inp = slots[usize::from(pass.inputs[0].0)].clone();
            let other = pass.inputs.get(1).map(|s| slots[usize::from(s.0)].clone());
            let out = &mut slots[usize::from(pass.output.0)];
            match pass.kernel {
                VR_KERNEL_ROW => {
                    let sigma = f32::from_bits(p[4]);
                    for y in 0..vh {
                        let b = at(x0, y0 + y);
                        // vr_blur_image's early return: no blur at all for sigma <= 0.3
                        let r = if sigma > 0.3 { row_radius(sigma, vw, vh, y) } else { 0 };
                        if r == 0 {
                            out[b..b + vw * 4].copy_from_slice(&inp[b..b + vw * 4]);
                        } else {
                            box_row_wrap(&inp[b..b + vw * 4], &mut out[b..b + vw * 4], vw, r);
                        }
                    }
                }
                VR_KERNEL_COL => {
                    let r = p[4] as usize;
                    let inv = 1.0 / (2 * r + 1) as f32;
                    for x in 0..vw {
                        for y in 0..vh {
                            let mut acc = [0f32; 4];
                            for j in 0..=2 * r {
                                let yy = (y + j).saturating_sub(r).min(vh - 1);
                                for k in 0..4 {
                                    acc[k] += inp[at(x0 + x, y0 + yy) + k];
                                }
                            }
                            let b = at(x0 + x, y0 + y);
                            for k in 0..4 {
                                out[b + k] = acc[k] * inv;
                            }
                        }
                    }
                }
                VR_KERNEL_COPY => {
                    for y in 0..vh {
                        let b = at(x0, y0 + y);
                        out[b..b + vw * 4].copy_from_slice(&inp[b..b + vw * 4]);
                    }
                }
                VR_KERNEL_SHARPEN | VR_KERNEL_DENOISE => {
                    let q_img = other.unwrap_or_default();
                    for y in 0..vh {
                        for x in 0..vw {
                            let b = at(x0 + x, y0 + y);
                            let pp = [inp[b], inp[b + 1], inp[b + 2], inp[b + 3]];
                            let q = [q_img[b], q_img[b + 1], q_img[b + 2], q_img[b + 3]];
                            if pass.kernel == VR_KERNEL_SHARPEN {
                                let amt = f32::from_bits(p[4]);
                                for k in 0..3 {
                                    out[b + k] = (pp[k] + (pp[k] - q[k]) * amt).max(0.0);
                                }
                                out[b + 3] = pp[3];
                            } else {
                                let thr = f32::from_bits(p[6]);
                                let d = (0..3).map(|k| (pp[k] - q[k]).abs()).fold(0.0, f32::max);
                                let kk = (-(d * d) / (thr * thr)).exp();
                                let o: Vec<f32> = (0..4).map(|c| pp[c] + (q[c] - pp[c]) * kk).collect();
                                for c in 0..4 {
                                    out[b + c] = if p[5] == 1 { if c < 3 { ((pp[c] - o[c]) * 4.0 + 0.18).max(0.0) * pp[3] } else { pp[3] } } else { o[c] };
                                }
                            }
                        }
                    }
                }
                unknown => panic!("unknown kernel {unknown}"),
            }
        }
        Image { w, h, px: slots.swap_remove(3) }
    }

    /// The CPU reference of each route, applied per view as `apply` does.
    fn reference(src: &Image, layout: u32, kind: VrKind) -> Image {
        let mut out = src.clone();
        per_view(&mut out, layout, |v| match kind {
            VrKind::Blur { sigma } => vr_blur_image(v, sigma),
            VrKind::Sharpen { sigma, amount } => {
                let mut b = v.clone();
                vr_blur_image(&mut b, sigma);
                v.px.par_chunks_mut(4).zip(b.px.par_chunks(4)).for_each(|(p, q)| {
                    for k in 0..3 {
                        p[k] = (p[k] + (p[k] - q[k]) * amount).max(0.0);
                    }
                });
            }
            VrKind::Denoise { sigma, threshold, show } => {
                let mut b = v.clone();
                vr_blur_image(&mut b, sigma);
                v.px.par_chunks_mut(4).zip(b.px.par_chunks(4)).for_each(|(p, q)| {
                    let d = (0..3).map(|k| (p[k] - q[k]).abs()).fold(0.0, f32::max);
                    let k = (-(d * d) / (threshold * threshold)).exp();
                    let o: Vec<f32> = (0..4).map(|c| p[c] + (q[c] - p[c]) * k).collect();
                    if show {
                        for c in 0..3 {
                            p[c] = ((p[c] - o[c]) * 4.0 + 0.18).max(0.0) * p[3];
                        }
                    } else {
                        p.copy_from_slice(&o);
                    }
                });
            }
        });
        out
    }

    fn picture(w: usize, h: usize) -> Image {
        let mut img = Image::new(w, h);
        for (i, v) in img.px.iter_mut().enumerate() {
            *v = ((i * 37 + i / 7) % 101) as f32 / 100.0;
        }
        for a in img.px.chunks_mut(4) {
            a[3] = 1.0;
        }
        img
    }

    #[test]
    fn vr_plans_match_the_cpu_reference_for_every_layout() {
        let kinds = [
            VrKind::Blur { sigma: 0.2 },
            VrKind::Blur { sigma: 2.5 },
            VrKind::Sharpen { sigma: 1.2, amount: 0.6 },
            VrKind::Denoise { sigma: 1.8, threshold: 0.1, show: false },
            VrKind::Denoise { sigma: 1.8, threshold: 0.1, show: true },
        ];
        for (w, h) in [(16, 8), (9, 7)] {
            let src = picture(w, h);
            for layout in 0..3 {
                for kind in kinds {
                    let plan = vr_plan(kind, layout, w, h).expect("plan");
                    assert!(plan.validate().is_ok(), "{kind:?} layout {layout}: {:?}", plan.validate());
                    let got = run_plan(&plan, &src);
                    let want = reference(&src, layout, kind);
                    let worst = got.px.iter().zip(&want.px).map(|(a, b)| (a - b).abs()).fold(0.0f32, f32::max);
                    assert!(worst < 2e-4, "{kind:?} layout {layout} {w}x{h}: max diff {worst}");
                }
            }
        }
    }

    #[test]
    fn vr_plan_refuses_empty_frames() {
        assert!(vr_plan(VrKind::Blur { sigma: 1.0 }, 0, 0, 4).is_none());
    }
}
