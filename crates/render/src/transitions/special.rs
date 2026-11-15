//! Immersive Video (flat equirectangular approximations), Smart Tools shape transitions and the
//! Text folder's cell-based reveals.

use std::f32::consts::TAU;

use super::wipe::{WipeStyle, field_wipe, shape_dist, wipe_px};
use super::{Tx, add_light, bell, blurred, ease_in, ease_in_out, ease_out, hash, mid_mix, mix, over, paint, sample_wrap, scale4, smoothstep};
use crate::image::Image;

/// Horizontal distance on a wrapping (360°) frame.
#[inline]
fn wrap_dx(x: f32, cx: f32, w: f32) -> f32 {
    (x - cx + w / 2.0).rem_euclid(w) - w / 2.0
}

fn rot(x: f32, y: f32, deg: f32) -> (f32, f32) {
    let (s, c) = deg.to_radians().sin_cos();
    (x * c + y * s, -x * s + y * c)
}

/// Pixel bounds `(x0, y0, x1, y1)` of everything not fully transparent in either frame.
fn content_bounds(a: &Image, b: &Image) -> Option<(f32, f32, f32, f32)> {
    let (mut x0, mut y0, mut x1, mut y1) = (usize::MAX, usize::MAX, 0, 0);
    for (y, (ra, rb)) in a.px.chunks(a.w * 4).zip(b.px.chunks(b.w * 4)).enumerate() {
        for (x, (pa, pb)) in ra.chunks(4).zip(rb.chunks(4)).enumerate() {
            if pa.get(3).is_some_and(|v| *v > 0.0) || pb.get(3).is_some_and(|v| *v > 0.0) {
                (x0, y0, x1, y1) = (x0.min(x), y0.min(y), x1.max(x + 1), y1.max(y + 1));
            }
        }
    }
    (x0 < x1 && y0 < y1).then_some((x0 as f32, y0 as f32, x1 as f32, y1 as f32))
}

pub(super) fn apply(id: &str, t: &Tx) -> Option<Image> {
    let p = t.p;
    let (w, h) = (t.wf, t.hf);
    let env = bell(p);
    let k = mid_mix(p);
    let s = t.seed();
    Some(match id {
        // ---------------- Immersive Video ----------------
        "vr_chroma_leaks" | "vr_light_leaks" => {
            let amt = t.frac("amount");
            let chroma = id == "vr_chroma_leaks";
            let cols: [[f32; 4]; 4] = if chroma {
                [[1.0, 0.1, 0.55, 1.0], [0.1, 0.75, 1.0, 1.0], [1.0, 0.85, 0.1, 1.0], [0.5, 0.2, 1.0, 1.0]]
            } else {
                [[1.0, 0.6, 0.25, 1.0], [1.0, 0.45, 0.15, 1.0], [1.0, 0.85, 0.6, 1.0], [1.0, 0.7, 0.4, 1.0]]
            };
            let blobs: Vec<(f32, f32, f32, [f32; 4])> = (0..4)
                .map(|i| {
                    let cx = w * (hash(i, 1, s) + (p - 0.5) * (0.5 + hash(i, 2, s)));
                    let cy = h * (0.2 + 0.6 * hash(i, 3, s));
                    (cx, cy, h * (0.3 + 0.3 * hash(i, 4, s)), cols[i as usize])
                })
                .collect();
            paint(t.w, t.h, |x, y| {
                let mut c = mix(t.pa(x, y), t.pb(x, y), k);
                for &(cx, cy, r, col) in &blobs {
                    let d2 = (wrap_dx(x, cx, w).powi(2) + (y - cy).powi(2)) / (r * r);
                    c = add_light(c, col, (-d2).exp() * amt * env * 0.7);
                }
                c
            })
        }
        "vr_gradient_wipe" => {
            let soft = t.frac("softness").clamp(0.005, 1.0) * 0.5;
            let inv = t.flag("invert");
            let thr = -soft + p * (1.0 + 2.0 * soft);
            paint(t.w, t.h, |x, y| {
                let lon = x / w * TAU;
                let g0 = (y / h + 0.04 * (lon * 2.0).sin()).clamp(0.0, 1.0);
                let g = if inv { 1.0 - g0 } else { g0 };
                mix(t.pa(x, y), t.pb(x, y), smoothstep(-soft, soft, thr - g))
            })
        }
        "vr_light_rays" => {
            let (cx, cy) = t.center();
            let amt = t.frac("amount");
            paint(t.w, t.h, |x, y| {
                let c = mix(t.pa(x, y), t.pb(x, y), k);
                let (rx, ry) = (wrap_dx(x, cx, w), y - cy);
                let a = ry.atan2(rx);
                let r = (rx * rx + ry * ry).sqrt() / h;
                let rays = (0.5 + 0.5 * (a * 23.0 + p * 3.0).sin() * (a * 7.0 - p * 2.0).cos()).powi(6);
                let fall = (-r * 1.5).exp();
                add_light(c, [1.0, 0.97, 0.9, 1.0], (rays * fall + (-r * 6.0).exp() * 0.8) * amt * env * 2.0)
            })
        }
        "vr_mobius_zoom" => {
            let zoom = t.frac("zoom");
            let twist = t.num("twist").to_radians();
            let (za, zb) = (1.0 + zoom * 3.0 * ease_in((p / 0.5).min(1.0)), 1.0 + zoom * 3.0 * (1.0 - ease_out(((p - 0.5) / 0.5).clamp(0.0, 1.0))));
            let rmax = (w * w + h * h).sqrt() / 2.0;
            paint(t.w, t.h, |x, y| {
                let sample = |img: &Image, z: f32, tw: f32| {
                    let (rx, ry) = (x - w / 2.0, y - h / 2.0);
                    let r = (rx * rx + ry * ry).sqrt();
                    let a = ry.atan2(rx) + tw * (1.0 - r / rmax).max(0.0);
                    let r2 = r / z;
                    sample_wrap(img, w / 2.0 + r2 * a.cos(), h / 2.0 + r2 * a.sin())
                };
                let a = if k < 1.0 { sample(t.a, za, twist * env) } else { [0.0; 4] };
                let b = if k > 0.0 { sample(t.b, zb, -twist * env) } else { [0.0; 4] };
                mix(a, b, k)
            })
        }
        "vr_random_blocks" => {
            let bs = t.px("block_size").max(2.0);
            let soft = t.frac("softness").clamp(0.0, 0.45).max(0.01);
            paint(t.w, t.h, |x, y| {
                let (bx, by) = ((x / bs).floor() as i32, (y / bs).floor() as i32);
                // wrap the block grid horizontally so the seam column matches
                let cols = (w / bs).ceil().max(1.0) as i32;
                let tau = soft + (1.0 - 2.0 * soft) * hash(bx.rem_euclid(cols), by, s);
                mix(t.pa(x, y), t.pb(x, y), smoothstep(tau - soft, tau + soft, p))
            })
        }
        "vr_spherical_blur" => {
            let sigma = t.px("blur") * 0.5 * env;
            let m = t.a.clone().lerp(t.b, k);
            blurred(&m, sigma)
        }
        // ---------------- Smart Tools ----------------
        "shape_dissolve" => {
            let shape = t.choice("shape");
            let size = t.px("size").max(4.0);
            let rot_deg = t.num("rotation");
            let order = t.choice("order");
            let st = WipeStyle { feather: t.px("softness"), border: 0.0, border_color: [0.0; 4], aa: 1.0 };
            let (cols, rows) = ((w / size).ceil().max(1.0), (h / size).ceil().max(1.0));
            let diag = ((cols / 2.0).powi(2) + (rows / 2.0).powi(2)).sqrt().max(1.0);
            field_wipe(t, st, move |x, y| {
                let (cx, cy) = ((x / size).floor(), (y / size).floor());
                let (lx, ly) = (x - (cx + 0.5) * size, y - (cy + 0.5) * size);
                let (u, v) = rot(lx / (size * 0.5), ly / (size * 0.5), rot_deg);
                let delay = match order {
                    1 => cx / cols,
                    2 => (((cx + 0.5) - cols / 2.0).powi(2) + ((cy + 0.5) - rows / 2.0).powi(2)).sqrt() / diag,
                    _ => hash(cx as i32, cy as i32, s),
                };
                (shape_dist(shape, u, v) + delay * 1.5) * size * 0.5
            })
        }
        "shape_flow" => {
            let shape = t.choice("shape");
            let (dx, dy) = t.dir();
            let st = WipeStyle { feather: t.px("softness"), border: 0.0, border_color: [0.0; 4], aa: 1.0 };
            let c1 = (w / 2.0, h / 2.0);
            let c0 = (c1.0 - dx * w * 0.75, c1.1 - dy * h * 0.75);
            let cover = [(0.0, 0.0), (w, 0.0), (0.0, h), (w, h)].iter().map(|&(x, y)| shape_dist(shape, x - c1.0, y - c1.1)).fold(0.0f32, f32::max);
            let rmax = cover * 1.05 + st.feather + 4.0;
            let e = ease_out(p);
            let (cx, cy) = (c0.0 + (c1.0 - c0.0) * e, c0.1 + (c1.1 - c0.1) * e);
            let r = rmax * ease_in_out(p);
            paint(t.w, t.h, |x, y| wipe_px(&st, t.pa(x, y), t.pb(x, y), shape_dist(shape, x - cx, y - cy) - r))
        }
        // ---------------- Text ----------------
        "text_animator" => {
            let cols = t.num("columns").round().clamp(2.0, 128.0);
            let rows = t.num("rows").round().clamp(1.0, 64.0);
            let (cw, ch) = (w / cols, h / rows);
            paint(t.w, t.h, |x, y| {
                let (ci, ri) = ((x / cw).floor().min(cols - 1.0), (y / ch).floor().min(rows - 1.0));
                let delay = 0.6 * (0.7 * ci / cols + 0.3 * ri / rows);
                let q = ((p - delay) / 0.4).clamp(0.0, 1.0);
                let (top, bot) = (ri * ch, (ri + 1.0) * ch);
                let ya = y + ch * ease_in(q);
                let yb = y - ch * (1.0 - ease_out(q));
                let a = if ya < bot { t.sac(x, ya) } else { [0.0; 4] };
                let b = if yb >= top { t.sbc(x, yb) } else { [0.0; 4] };
                mix(a, b, smoothstep(0.0, 1.0, q))
            })
        }
        "typewriter" => {
            let cols = t.num("columns").round().clamp(2.0, 128.0);
            let rows = t.num("rows").round().clamp(1.0, 64.0);
            let cursor = t.flag("cursor");
            let n = cols * rows;
            let cur = (p * n).floor();
            // lay the grid over the visible content (a text layer is mostly transparent)
            let Some((bx0, by0, bx1, by1)) = content_bounds(t.a, t.b) else {
                return Some(t.a.clone().lerp(t.b, p));
            };
            let (cw, ch) = ((bx1 - bx0) / cols, (by1 - by0) / rows);
            paint(t.w, t.h, |x, y| {
                if x < bx0 || x >= bx1 || y < by0 || y >= by1 {
                    return if p < 0.5 { t.pa(x, y) } else { t.pb(x, y) };
                }
                let (ci, ri) = (((x - bx0) / cw).floor().min(cols - 1.0), ((y - by0) / ch).floor().min(rows - 1.0));
                let i = ri * cols + ci;
                if i < cur {
                    t.pb(x, y)
                } else if i == cur && cursor {
                    // a caret block in the lower part of the cell
                    let ly = (y - by0 - ri * ch) / ch;
                    if ly > 0.15 { over(t.pa(x, y), scale4([1.0, 1.0, 1.0, 1.0], 0.75)) } else { t.pa(x, y) }
                } else {
                    t.pa(x, y)
                }
            })
        }
        _ => return None,
    })
}
