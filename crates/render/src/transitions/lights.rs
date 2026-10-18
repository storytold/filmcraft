//! Lights & Blurs: light effects that peak at the midpoint while the clips swap underneath, and
//! blur-through transitions.

use std::f32::consts::PI;

use super::dissolve::perceptual_luma;
use super::wipe::angle_dir;
use super::{Tx, add_light, bell, blurred, disc_blur, ease_in, ease_out, hash, line_blur, luma, mid_mix, mix, paint, smoothstep, spin_blur, taps, zoom_blur};
use crate::image::Image;

/// Soft round light blob.
#[inline]
fn blob(x: f32, y: f32, cx: f32, cy: f32, r: f32) -> f32 {
    let d2 = ((x - cx).powi(2) + (y - cy).powi(2)) / (r * r).max(1e-6);
    (-d2).exp()
}

/// The midpoint mix of A and B (unwarped).
fn base(t: &Tx, x: f32, y: f32, k: f32) -> [f32; 4] {
    if k <= 0.0 {
        t.pa(x, y)
    } else if k >= 1.0 {
        t.pb(x, y)
    } else {
        mix(t.pa(x, y), t.pb(x, y), k)
    }
}

pub(super) fn apply(id: &str, t: &Tx) -> Option<Image> {
    let p = t.p;
    let (w, h) = (t.wf, t.hf);
    let k = mid_mix(p);
    let env = bell(p);
    Some(match id {
        "burn_chroma" => {
            let amt = t.frac("amount");
            let hue = p * PI * 2.0;
            let tint = [1.0 + 0.5 * hue.cos(), 1.0 + 0.5 * (hue + 2.1).cos(), 1.0 + 0.5 * (hue + 4.2).cos(), 1.0];
            paint(t.w, t.h, |x, y| {
                let c = base(t, x, y, k);
                let g = amt * env * 1.8;
                let l = luma(c);
                [c[0] * (1.0 + g * tint[0]) + l * g * 0.3, c[1] * (1.0 + g * tint[1]) + l * g * 0.2, c[2] * (1.0 + g * tint[2]) + l * g * 0.1, c[3]]
            })
        }
        "chroma_leak" | "light_leak" => {
            let amt = t.frac("amount");
            let s = t.seed();
            let leak = id == "light_leak";
            let user = t.color("color");
            let cols: [[f32; 4]; 3] = if leak {
                [user, [user[0], user[1] * 0.8, user[2] * 0.5, 1.0], [1.0, 0.9, 0.7, 1.0]]
            } else {
                [[1.0, 0.15, 0.6, 1.0], [0.1, 0.8, 1.0, 1.0], [1.0, 0.85, 0.1, 1.0]]
            };
            let blobs: Vec<(f32, f32, f32, [f32; 4])> = (0..3)
                .map(|i| {
                    let r0 = hash(i, 1, s);
                    let r1 = hash(i, 2, s);
                    let side = if leak { 0.0 } else { r0 };
                    let cx = w * (side + (r1 - 0.5) * 0.4 + (p - 0.5) * (0.8 + r0));
                    let cy = h * (0.2 + 0.6 * hash(i, 3, s) + (p - 0.5) * (r1 - 0.5));
                    (cx, cy, w.max(h) * (0.25 + 0.25 * hash(i, 4, s)), cols[i as usize])
                })
                .collect();
            paint(t.w, t.h, |x, y| {
                let mut c = base(t, x, y, k);
                for &(cx, cy, r, col) in &blobs {
                    c = add_light(c, col, blob(x, y, cx, cy, r) * amt * env * 0.7);
                }
                c
            })
        }
        "cross_zoom" | "zoom_blur" => {
            let (cx, cy) = t.center();
            let st = t.frac("strength");
            let zooms = id == "cross_zoom";
            let za = if zooms { 1.0 + 3.0 * st * ease_in((p / 0.5).min(1.0)) } else { 1.0 + 0.1 * st * ease_in(p) };
            let zb = if zooms { 1.0 + 3.0 * st * (1.0 - ease_out(((p - 0.5) / 0.5).clamp(0.0, 1.0))) } else { 1.0 + 0.1 * st * (1.0 - ease_out(p)) };
            let blur = st * env * 0.6;
            let n = taps(blur * w.max(h) * 0.5);
            paint(t.w, t.h, |x, y| {
                let s = |img: &Image, z: f32| zoom_blur(img, (x - cx) / z + cx, (y - cy) / z + cy, cx, cy, blur, n);
                let a = if k < 1.0 { s(t.a, za) } else { [0.0; 4] };
                let b = if k > 0.0 { s(t.b, zb) } else { [0.0; 4] };
                mix(a, b, k)
            })
        }
        "directional_blur_transition" => {
            let (dx, dy) = angle_dir(t.num("angle"));
            let len = t.px("blur") * env;
            let n = taps(len * 2.0);
            let shift = len * 0.5;
            paint(t.w, t.h, |x, y| {
                let a = if k < 1.0 { line_blur(t.a, x - dx * shift * p, y - dy * shift * p, dx * len, dy * len, n) } else { [0.0; 4] };
                let b = if k > 0.0 { line_blur(t.b, x + dx * shift * (1.0 - p), y + dy * shift * (1.0 - p), dx * len, dy * len, n) } else { [0.0; 4] };
                mix(a, b, k)
            })
        }
        "flare" => {
            let col = t.color("color");
            let amt = t.frac("amount");
            let (dx, dy) = angle_dir(t.num("angle") + 90.0);
            let diag = (w * w + h * h).sqrt();
            let (fx, fy) = (w / 2.0 + dx * diag * (p - 0.5) * 0.9, h / 2.0 + dy * diag * (p - 0.5) * 0.9);
            let unit = h.max(1.0);
            paint(t.w, t.h, |x, y| {
                let c = base(t, x, y, k);
                let (rx, ry) = (x - fx, y - fy);
                // core, wide halo, anamorphic streak along the flare's axis, two ghosts mirrored through the centre
                let along = rx * dx + ry * dy;
                let across = -rx * dy + ry * dx;
                let core = blob(x, y, fx, fy, unit * 0.06) * 3.0 + blob(x, y, fx, fy, unit * 0.35) * 0.6;
                let streak = (-(across.abs() / (unit * 0.01))).exp() * (-(along.abs() / (diag * 0.35))).exp() * 1.5;
                let (gx, gy) = (w - fx, h - fy);
                let ghost = blob(x, y, gx, gy, unit * 0.08) * 0.4 + blob(x, y, (gx + fx) / 2.0, (gy + fy) / 2.0, unit * 0.05) * 0.3;
                add_light(c, col, (core + streak + ghost) * amt * env)
            })
        }
        "flash" => {
            let col = t.color("color");
            let inten = t.frac("intensity").clamp(0.0, 1.0);
            let cut = smoothstep(0.45, 0.55, p);
            // a short flash centred on the cut
            let f = inten * (-((p - 0.5) / 0.12).powi(2)).exp() * smoothstep(0.0, 0.2, env);
            paint(t.w, t.h, |x, y| mix(base(t, x, y, cut), col, f))
        }
        "glow" => {
            let r = t.px("radius").max(1.0);
            let inten = t.frac("intensity");
            let b = t.a.clone().lerp(t.b, k);
            let g = blurred(&b, r * 0.5);
            let gain = inten * env * 2.0;
            paint(t.w, t.h, |x, y| {
                let c = b.get(x as usize, y as usize);
                let gl = g.get(x as usize, y as usize);
                [c[0] + gl[0] * gain, c[1] + gl[1] * gain, c[2] + gl[2] * gain, c[3].max(gl[3] * gain.min(1.0))]
            })
        }
        "lens_blur" => {
            let r = t.px("radius");
            let ra = r * smoothstep(0.0, 0.55, p);
            let rb = r * (1.0 - smoothstep(0.45, 1.0, p));
            let n = ((r * 0.8) as usize).clamp(8, 48);
            paint(t.w, t.h, |x, y| {
                let a = if k < 1.0 { disc_blur(t.a, x, y, ra, n) } else { [0.0; 4] };
                let b = if k > 0.0 { disc_blur(t.b, x, y, rb, n) } else { [0.0; 4] };
                mix(a, b, k)
            })
        }
        "light_sweep" => {
            let (dx, dy) = angle_dir(t.num("angle"));
            let bw = t.px("width").max(2.0);
            let col = t.color("color");
            let amt = t.frac("amount");
            let ends = [(0.0, 0.0), (w, 0.0), (0.0, h), (w, h)].map(|(cx, cy): (f32, f32)| cx * dx + cy * dy);
            let lo = ends.iter().copied().fold(f32::INFINITY, f32::min);
            let hi = ends.iter().copied().fold(f32::NEG_INFINITY, f32::max);
            let e = lo - bw + p * (hi - lo + 2.0 * bw);
            paint(t.w, t.h, |x, y| {
                let d = x * dx + y * dy - e;
                let c = mix(t.pa(x, y), t.pb(x, y), 1.0 - smoothstep(-bw * 0.3, bw * 0.3, d));
                add_light(c, col, (-(d / (bw * 0.35)).powi(2)).exp() * amt * 2.0)
            })
        }
        "phosphor" => {
            let col = t.color("color");
            let decay = t.frac("decay").clamp(0.0, 1.0);
            let kk = smoothstep(0.15, 0.85, p);
            let persist = (1.0 - p).powf(0.5 + 3.0 * decay) * smoothstep(0.0, 0.15, p);
            paint(t.w, t.h, |x, y| {
                let a = t.pa(x, y);
                let c = mix(a, t.pb(x, y), kk);
                let l = perceptual_luma(a).powi(2);
                add_light(c, col, l * persist * 1.5)
            })
        }
        "radial_blur" => {
            let (cx, cy) = t.center();
            let max = t.num("angle").to_radians();
            let ang = max * env;
            let n = taps(ang * w.max(h) * 0.5);
            let (ra, rb) = (max * 0.25 * ease_in(p), -max * 0.25 * (1.0 - ease_out(p)));
            paint(t.w, t.h, |x, y| {
                let rot = |r: f32| {
                    let (s, c) = (-r).sin_cos();
                    let (ux, uy) = (x - cx, y - cy);
                    (cx + ux * c - uy * s, cy + ux * s + uy * c)
                };
                let a = if k < 1.0 {
                    let (sx, sy) = rot(ra);
                    spin_blur(t.a, sx, sy, cx, cy, ang, n)
                } else {
                    [0.0; 4]
                };
                let b = if k > 0.0 {
                    let (sx, sy) = rot(rb);
                    spin_blur(t.b, sx, sy, cx, cy, ang, n)
                } else {
                    [0.0; 4]
                };
                mix(a, b, k)
            })
        }
        "ray" => {
            let (cx, cy) = t.center();
            let amt = t.frac("amount");
            let len = t.frac("length").clamp(0.0, 1.0);
            let b0 = t.a.clone().lerp(t.b, k);
            let bright = {
                let src = &b0;
                paint(t.w, t.h, |x, y| {
                    let c = src.get(x as usize, y as usize);
                    let l = (perceptual_luma(c) - 0.55).max(0.0) * 2.2;
                    [c[0] * l, c[1] * l, c[2] * l, c[3] * l]
                })
            };
            let n = 24;
            paint(t.w, t.h, |x, y| {
                let c = b0.get(x as usize, y as usize);
                let r = zoom_blur(&bright, x, y, cx, cy, len * 0.9, n);
                let g = amt * env * 3.0;
                [c[0] + r[0] * g, c[1] + r[1] * g, c[2] + r[2] * g, c[3].max(r[3] * g.min(1.0))]
            })
        }
        "solarize" => {
            let amt = t.frac("amount").clamp(0.0, 1.0) * env;
            paint(t.w, t.h, |x, y| {
                let c = base(t, x, y, k);
                let sol = |v: f32| {
                    let g = v.max(0.0).powf(1.0 / 2.2).min(1.0);
                    let s = if g > 0.5 { 1.0 - g } else { g } * 2.0;
                    s.min(1.0).powf(2.2)
                };
                let a = c[3];
                let s = [sol(c[0]) * a, sol(c[1]) * a, sol(c[2]) * a, a];
                mix(c, s, amt)
            })
        }
        "stripe" => {
            let n = t.num("stripes").round().clamp(1.0, 64.0);
            let (dx, dy) = angle_dir(t.num("angle") + 90.0);
            let (px_, py_) = (-dy, dx);
            let col = t.color("color");
            let ends = [(0.0, 0.0), (w, 0.0), (0.0, h), (w, h)];
            let rng = |f: &dyn Fn(f32, f32) -> f32| {
                let v = ends.map(|(a, b)| f(a, b));
                (v.iter().copied().fold(f32::INFINITY, f32::min), v.iter().copied().fold(f32::NEG_INFINITY, f32::max))
            };
            let (alo, ahi) = rng(&|x, y| x * dx + y * dy);
            let (clo, chi) = rng(&|x, y| x * px_ + y * py_);
            let span = (ahi - alo).max(1.0);
            let bar = span * 0.08;
            paint(t.w, t.h, |x, y| {
                let i = (((x * px_ + y * py_) - clo) / (chi - clo).max(1.0) * n).floor().clamp(0.0, n - 1.0);
                let qi = ((p - 0.4 * i / n) / 0.6).clamp(0.0, 1.0);
                let e = alo - bar + qi * (span + 2.0 * bar);
                let d = x * dx + y * dy - e;
                let c = mix(t.pa(x, y), t.pb(x, y), 1.0 - smoothstep(-1.0, 1.0, d));
                let light = if d >= 0.0 && d < bar { (1.0 - d / bar).powi(2) * smoothstep(0.0, 0.1, qi) * (1.0 - smoothstep(0.9, 1.0, qi)) } else { 0.0 };
                add_light(c, col, light * 1.5)
            })
        }
        _ => return None,
    })
}
