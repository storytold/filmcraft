//! Colour, keying and matte effects.

use filmcraft_color::{Matrix, hsl_to_rgb, rgb_to_hsl, rgb_to_ycbcr};
use filmcraft_project::EffectInstance;
use filmcraft_project::effect::LIGHT_IDS;
use rayon::prelude::*;

use super::*;

/// Lighting Effects: up to five directional / omni / spot lights, ambience, gloss and a bump map
/// from the layer's own luminance.
pub fn lighting(img: &mut Image, e: &EffectInstance, cx: &FxCtx) {
    struct L {
        kind: u32,
        color: [f32; 3],
        c: Vec2,
        major: f64,
        minor: f64,
        rot: (f64, f64),
        intensity: f32,
        focus: f32,
    }
    let diag = (img.w as f64).hypot(img.h as f64);
    let lights: Vec<L> = LIGHT_IDS
        .iter()
        .map(|ids| L {
            kind: chv(e, ids[0]),
            color: lin(cv(e, ids[1], cx)),
            c: pv(e, ids[2], cx, img),
            major: (fv(e, ids[3], cx) as f64 / 100.0 * diag).max(1.0),
            minor: (fv(e, ids[4], cx) as f64 / 100.0 * diag).max(1.0),
            rot: (fv(e, ids[5], cx) as f64).to_radians().sin_cos(),
            intensity: fv(e, ids[6], cx) / 20.0,
            focus: fv(e, ids[7], cx) / 100.0,
        })
        .filter(|l| l.kind != 0)
        .collect();
    let amb = lin(cv(e, "ambient_color", cx)).map(|v| v * fv(e, "ambient", cx) / 100.0);
    let gloss = fv(e, "gloss", cx) / 100.0;
    let material = (fv(e, "material", cx) / 100.0 + 1.0) / 2.0;
    let exposure = 2f32.powf(fv(e, "exposure", cx) / 100.0 * 2.0);
    let bump = fv(e, "bump_height", cx) / 100.0 * 4.0;
    let white_high = bv(e, "white_high");
    let src = img.clone();
    let lum = |x: isize, y: isize| {
        let p = src.get_clamped(x, y);
        let l = luma(Image::unpremul(p));
        if white_high { l } else { 1.0 - l }
    };
    let w = img.w;
    img.px.par_chunks_mut(w * 4).enumerate().for_each(|(y, row)| {
        for x in 0..w {
            let p = &mut row[x * 4..x * 4 + 4];
            if p[3] <= 1e-6 {
                continue;
            }
            let (px, py) = (x as f64 + 0.5, y as f64 + 0.5);
            let (gx, gy) = if bump != 0.0 {
                (
                    (lum(x as isize + 1, y as isize) - lum(x as isize - 1, y as isize)) * bump,
                    (lum(x as isize, y as isize + 1) - lum(x as isize, y as isize - 1)) * bump,
                )
            } else {
                (0.0, 0.0)
            };
            let mut light = amb;
            let mut spec = [0.0f32; 3];
            for l in &lights {
                let (dx, dy) = (px - l.c.x, py - l.c.y);
                let (s, c) = l.rot;
                let (u, v) = (dx * c + dy * s, -dx * s + dy * c);
                let fall = match l.kind {
                    1 => 1.0,
                    2 => {
                        let d = ((dx * dx + dy * dy).sqrt() / l.major) as f32;
                        (1.0 - d).max(0.0).powi(2)
                    }
                    _ => {
                        let d = ((u / l.major).powi(2) + (v / l.minor).powi(2)).sqrt() as f32;
                        1.0 - smoothstep(l.focus * 0.95, 1.0, d)
                    }
                };
                // bump shading: light direction in the image plane (towards the light), tilted 45°
                let (lx, ly) = if l.kind == 1 { (l.rot.1 as f32, l.rot.0 as f32) } else { (-dx as f32, -dy as f32) };
                let ln = (lx * lx + ly * ly).sqrt().max(1e-6);
                let shade = (1.0 - (gx * lx + gy * ly) / ln).max(0.0);
                for k in 0..3 {
                    light[k] += l.color[k] * l.intensity * fall * shade;
                    spec[k] += l.color[k] * gloss.max(0.0) * fall.powi(8) * l.intensity.max(0.0);
                }
            }
            let c = Image::unpremul([p[0], p[1], p[2], p[3]]);
            for k in 0..3 {
                let s = spec[k] * (1.0 - material) + spec[k] * c[k] * material;
                let v = ((c[k] * light[k] + s) * exposure).max(0.0);
                p[k] = v * p[3];
            }
        }
    });
}

/// ASC CDL (v1.2): clamp(in × slope + offset)^power per channel, then saturation (Rec. 709 luma),
/// on display-encoded values (`gpufx::FxOp::AscCdl`).
pub(crate) fn cdl(c: [f32; 3], s: [f32; 3], o: [f32; 3], p: [f32; 3], sat: f32) -> [f32; 3] {
    let mut v = [0.0; 3];
    for k in 0..3 {
        v[k] = (c[k] * s[k] + o[k]).clamp(0.0, 1.0).powf(p[k].max(0.0));
    }
    let l = luma(v);
    v.map(|q| (l + sat * (q - l)).clamp(0.0, 1.0))
}

#[allow(unused_imports)]
pub(crate) use crate::gpufx::limit;

/// Logo Cutout: removes a flat white/black/custom background by un-multiplying it — the alpha of
/// a pixel is how far it is from the background towards the gamut edge, and the colour is
/// recovered as if it had been composited over that background.
pub fn logo_cutout(img: &mut Image, e: &EffectInstance, cx: &FxCtx) {
    let bg = match chv(e, "background") {
        0 => [1.0; 3],
        1 => [0.0; 3],
        _ => {
            let c = cv(e, "color", cx);
            [c[0], c[1], c[2]]
        }
    };
    let th = fv(e, "threshold", cx) / 100.0;
    let soft = (fv(e, "softness", cx) / 100.0).max(1e-3);
    let unmult = bv(e, "unmultiply");
    let invert = bv(e, "invert");
    img.px.par_chunks_mut(4).for_each(|p| {
        if p[3] <= 1e-6 {
            return;
        }
        let v = enc(Image::unpremul([p[0], p[1], p[2], p[3]]));
        let mut a0 = 0.0f32;
        for k in 0..3 {
            let d = v[k] - bg[k];
            let room = if d >= 0.0 { 1.0 - bg[k] } else { bg[k] };
            if room > 1e-6 {
                a0 = a0.max(d.abs() / room);
            }
        }
        let a0 = a0.clamp(0.0, 1.0);
        let mut a = ((a0 - th) / ((1.0 - th).max(1e-3) * soft)).clamp(0.0, 1.0);
        if invert {
            a = 1.0 - a;
        }
        let col = if unmult && a0 > 1e-4 && !invert { [0, 1, 2].map(|k| (bg[k] + (v[k] - bg[k]) / a0).clamp(0.0, 1.0)) } else { v };
        let l = dec(col);
        let na = a * p[3];
        p.copy_from_slice(&[l[0] * na, l[1] * na, l[2] * na, na]);
    });
}

/// Ultra Key: a colour-difference keyer (for a key colour one channel leads, a green or blue screen;
/// otherwise a Y′CbCr chroma distance), with matte generation, cleanup, spill suppression and colour
/// correction stages.
pub fn ultra_key(img: &mut Image, e: &EffectInstance, cx: &FxCtx) {
    let kc = cv(e, "key_color", cx);
    let k = rgb_to_ycbcr(kc[0], kc[1], kc[2], Matrix::Bt709);
    let kmag2 = (k[1] * k[1] + k[2] * k[2]).max(1e-6);
    let setting = chv(e, "setting");
    let preset = filmcraft_project::effect::ultra_key_setting(setting);
    let pf = |id: &str, fallback: f32| preset.and_then(|rows| rows.iter().find(|(k, _)| *k == id).map(|(_, v)| *v as f32)).unwrap_or(fallback);
    let transparency = pf("transparency", fv(e, "transparency", cx)) / 100.0;
    let highlight = pf("highlight", fv(e, "highlight", cx)) / 100.0;
    let shadow = pf("shadow", fv(e, "shadow", cx)) / 100.0;
    let tol = 0.08 + pf("tolerance", fv(e, "tolerance", cx)) / 100.0 * 0.5;
    let pedestal = (pf("pedestal", fv(e, "pedestal", cx)) / 100.0 * 0.3).min(0.9);
    let gain = 1.0 + 2.0 * transparency;
    let output = chv(e, "output");
    let dom = if kc[1] >= kc[0] && kc[1] >= kc[2] {
        1
    } else if kc[2] >= kc[0] {
        2
    } else {
        0
    };
    // A key that one channel clearly leads (a green or blue screen) is keyed by colour difference: the
    // lead of that channel over the other two in a pixel, against the key colour's own lead, is the
    // share of the key colour the pixel is made of. Fire, skin or smoke over a green screen have no
    // green lead, so they stay opaque, and a pixel that is part screen and part foreground gets the
    // alpha of its foreground share (a hue/chroma distance in Y′CbCr cannot tell yellow flames from
    // the olive their edges blend with, and keyed or kept both together). Other keys (cyan, magenta,
    // yellow, grey…) have no single leading channel and keep the chroma-distance matte.
    //
    // Tolerance above its default of 50 also lowers how much of a pixel's red counts against its green,
    // down to 60 % at 100: the edge of a red or orange foreground is red mixed with the screen's green,
    // and reads as part screen instead of as an olive fringe. Up to the default all of the red counts,
    // since a pure yellow (red equal to green) cannot be told from that edge by its colour alone and
    // must stay foreground, flames and yellow clothes among them.
    let tolerance = (pf("tolerance", fv(e, "tolerance", cx)) / 100.0).clamp(0.0, 1.0);
    let (lead1, lead2) = ((dom + 1) % 3, (dom + 2) % 3);
    let red_weight = 1.0 - 0.4 * ((tolerance - 0.5) * 2.0).clamp(0.0, 1.0);
    let weight = [if dom == 0 { 1.0 } else { red_weight }, 1.0, 1.0];
    let other = |v: [f32; 3]| (v[lead1] * weight[lead1]).max(v[lead2] * weight[lead2]);
    let key_lead = kc[dom] - other([kc[0], kc[1], kc[2]]);
    let by_difference = key_lead >= 0.12;
    let lead_share = |v: [f32; 3]| ((v[dom] - other(v)) / key_lead).clamp(0.0, 1.0);
    // The share of the key colour at which a pixel is all background; Tolerance lowers it, 95 % at the
    // default. Measured against Premiere Pro on fire and smoke shot on green, this keeps the long tail of
    // thin smoke (a third of the screen's lead still leaves it about half opaque) instead of cutting it.
    let full_share = (1.15 - 0.4 * tolerance).min(1.0);
    // matte
    let n = img.w * img.h;
    let mut alpha = vec![1.0f32; n];
    alpha.par_iter_mut().zip(img.px.par_chunks(4)).for_each(|(a, p)| {
        if p[3] <= 1e-6 {
            *a = 0.0;
            return;
        }
        let v = enc(Image::unpremul([p[0], p[1], p[2], p[3]]));
        let y = rgb_to_ycbcr(v[0], v[1], v[2], Matrix::Bt709);
        let (keyness, mut m) = if by_difference {
            // Transparency pulls the partly transparent pixels towards transparent
            let keyness = (lead_share(v) / full_share).clamp(0.0, 1.0);
            (keyness, (1.0 - keyness).powf(1.0 + transparency))
        } else {
            let proj = (y[1] * k[1] + y[2] * k[2]) / kmag2;
            let perp = ((y[1] - proj * k[1]).powi(2) + (y[2] - proj * k[2]).powi(2)).sqrt() / kmag2.sqrt();
            let keyness = proj.clamp(0.0, 1.5) * (1.0 - smoothstep(0.0, tol, perp));
            (keyness, (1.0 - keyness * gain).clamp(0.0, 1.0))
        };
        m += (y[0] - k[0]).max(0.0) * highlight * 2.0 * keyness.min(1.0);
        m += (k[0] - y[0]).max(0.0) * shadow * 2.0 * keyness.min(1.0);
        *a = ((m.clamp(0.0, 1.0) - pedestal) / (1.0 - pedestal)).clamp(0.0, 1.0);
    });
    // the soft edge of a foreground of one steady colour, solved against that colour
    let key_rgb = [kc[0], kc[1], kc[2]];
    let edge_fg = if by_difference { solve_edges(&img.px, img.w, img.h, &mut alpha, key_rgb, &lead_share, cx.px_scale) } else { vec![None; n] };
    // cleanup
    let choke = pf("choke", fv(e, "choke", cx)) / 100.0;
    let soften = pf("soften", fv(e, "soften", cx)) / 100.0;
    let contrast = pf("contrast", fv(e, "contrast", cx)) / 100.0;
    let mid = pf("mid_point", fv(e, "mid_point", cx)) / 100.0;
    if choke > 0.0 {
        let r = (choke * 4.0 * cx.px_scale).max(0.5);
        let b = blur_plane(&alpha, img.w, img.h, r);
        let c = choke * 0.5;
        alpha.par_iter_mut().zip(b.par_iter()).for_each(|(a, &bb)| *a = a.min(((bb - c) / (1.0 - c)).clamp(0.0, 1.0)));
    }
    if soften > 0.0 {
        alpha = blur_plane(&alpha, img.w, img.h, soften * 10.0 * cx.px_scale);
    }
    if contrast > 0.0 {
        let g = 1.0 + contrast * 4.0;
        alpha.par_iter_mut().for_each(|a| *a = ((*a - mid) * g + mid).clamp(0.0, 1.0));
    }
    // spill suppression + colour correction on the foreground
    let spill = pf("spill", fv(e, "spill", cx)) / 100.0;
    let desat = pf("desaturate", fv(e, "desaturate", cx)) / 100.0;
    let range = pf("range", fv(e, "range", cx)) / 100.0;
    let sluma = pf("spill_luma", fv(e, "spill_luma", cx)) / 100.0;
    let (cc_s, cc_h, cc_l) = (fv(e, "cc_saturation", cx) / 100.0, fv(e, "cc_hue", cx) / 360.0, fv(e, "cc_luminance", cx) / 100.0);
    let cc = (cc_s - 1.0).abs() > 1e-4 || cc_h.abs() > 1e-6 || (cc_l - 1.0).abs() > 1e-4;
    img.px.par_chunks_mut(4).zip(alpha.par_iter()).zip(edge_fg.par_iter()).for_each(|((p, &a), edge)| {
        let src_a = p[3];
        let mut v = enc(Image::unpremul([p[0], p[1], p[2], p[3]]));
        // A partly transparent pixel keeps its own colour with the green spill taken out below, as in
        // Premiere Pro: thin smoke turns a soft grey and the dense parts stay golden. Taking the whole
        // key colour back out of the mix instead left an olive cast on everything thin.
        if let Some(f) = edge {
            v = *f;
        }
        let (o1, o2) = (v[(dom + 1) % 3], v[(dom + 2) % 3]);
        let limit = o1.max(o2) * (1.0 - range) + (o1 + o2) * 0.5 * range;
        let excess = (v[dom] - limit).max(0.0);
        if excess > 0.0 && spill > 0.0 {
            let l0 = luma(v);
            // Spill Suppression at its default of 50 already takes all of the excess out
            v[dom] -= excess * (spill * 2.0).min(1.0);
            let l1 = luma(v);
            let lm = luma(v);
            v = v.map(|q| lm + (q - lm) * (1.0 - desat * (excess * 4.0).min(1.0)));
            let comp = (l0 - l1) * sluma;
            v = v.map(|q| q + comp);
        }
        if cc {
            let mut h = rgb_to_hsl(v[0].clamp(0.0, 1.0), v[1].clamp(0.0, 1.0), v[2].clamp(0.0, 1.0));
            h[0] = (h[0] + cc_h).rem_euclid(1.0);
            h[1] = (h[1] * cc_s).clamp(0.0, 1.0);
            h[2] = (h[2] * cc_l).clamp(0.0, 1.0);
            v = hsl_to_rgb(h[0], h[1], h[2]);
        }
        let l = dec(v);
        match output {
            1 => {
                let g = filmcraft_color::srgb_to_linear(a);
                p.copy_from_slice(&[g * src_a, g * src_a, g * src_a, src_a]);
            }
            2 => p.copy_from_slice(&[l[0] * src_a, l[1] * src_a, l[2] * src_a, src_a]),
            _ => {
                let na = a * src_a;
                p.copy_from_slice(&[l[0] * na, l[1] * na, l[2] * na, na]);
            }
        }
    });
}

/// Mean of the (2r+1)² square around each pixel of a `w`×`h` plane, edges replicated: a running
/// mean along the rows, then along the columns (as rows of the transposed plane, so both passes run
/// in parallel).
fn box_mean(src: &[f32], w: usize, h: usize, r: usize) -> Vec<f32> {
    if r == 0 || w == 0 || h == 0 || src.len() < w * h {
        return src.to_vec();
    }
    let rows = box_rows(src, w, h, r);
    let cols = box_rows(&transpose(&rows, w, h), h, w, r);
    transpose(&cols, h, w)
}

/// Running (2r+1)-wide mean along each row of a `w`×`h` plane, edges replicated.
fn box_rows(src: &[f32], w: usize, h: usize, r: usize) -> Vec<f32> {
    let k = (2 * r + 1) as f64;
    let last = w.saturating_sub(1) as isize;
    let mut out = vec![0.0f32; w * h];
    out.par_chunks_mut(w).zip(src.par_chunks(w)).for_each(|(out, row)| {
        let at = |x: isize| row[x.clamp(0, last) as usize] as f64;
        let mut acc: f64 = (-(r as isize)..=r as isize).map(at).sum();
        out[0] = (acc / k) as f32;
        for x in 1..w {
            acc += at(x as isize + r as isize) - at(x as isize - r as isize - 1);
            out[x] = (acc / k) as f32;
        }
    });
    out
}

/// A `w`×`h` plane as `h`×`w`.
fn transpose(src: &[f32], w: usize, h: usize) -> Vec<f32> {
    let mut out = vec![0.0f32; w * h];
    out.par_chunks_mut(h).enumerate().for_each(|(x, col)| {
        for (y, o) in col.iter_mut().enumerate() {
            *o = src[y * w + x];
        }
    });
    out
}

/// The soft edge of a keyed foreground, solved against the foreground's own colour.
///
/// The edge pixels of a red shirt on a green screen are red mixed with green: a colour (olive, yellow)
/// that a pure yellow shares, which no per-pixel rule can tell from that mix. The pixels a few steps
/// inside the edge are known foreground, though: their average colour is the foreground the edge is
/// made of, so an edge pixel's share of foreground is where it falls on the line from the key colour
/// to that foreground, and its colour is the foreground's (not the mix, with its green fringe).
///
/// Only where the foreground colour is steady and the soft zone is a thin edge: wide hazes of partly
/// transparent flames or smoke, whose foreground colour varies, are left to the matte. Lowers `alpha`
/// where it solves and returns, per pixel, the foreground colour to use (sRGB-encoded) or `None`.
fn solve_edges(
    px: &[f32],
    w: usize,
    h: usize,
    alpha: &mut [f32],
    key: [f32; 3],
    lead_share: &(dyn Fn([f32; 3]) -> f32 + Sync),
    px_scale: f32,
) -> Vec<Option<[f32; 3]>> {
    let n = w * h;
    if n == 0 || px.len() < n * 4 || alpha.len() < n {
        return vec![None; n];
    }
    // widths are in pixels of a full-size frame; a smaller render has proportionally thinner edges
    let erode = ((3.0 * px_scale).round() as usize).clamp(1, 6);
    let radius = ((12.0 * px_scale).round() as usize).clamp(4, 24);
    // the wide averages below are smooth, so they are taken on a grid of `step`×`step` blocks and
    // read back bilinearly: a 4K frame costs little more than a small one
    let step = (radius / 3).clamp(1, 4);
    let (lw, lh) = (w.div_ceil(step), h.div_ceil(step));
    let low_radius = (radius / step).max(1);
    // each pixel's colour as the key sees it (sRGB-encoded, straight), worked out once
    let colours: Vec<[f32; 3]> =
        px.par_chunks(4).take(n).map(|p| if p[3] <= 1e-6 { [0.0; 3] } else { enc(Image::unpremul([p[0], p[1], p[2], p[3]])) }).collect();
    let alpha_in: &[f32] = alpha;
    // steady foreground (opaque, not a trace of the key colour in it), and its core: at least
    // `erode` pixels from anything that is not steady foreground
    let core: Vec<f32> = {
        let sure: Vec<f32> =
            (0..n).into_par_iter().map(|i| if px[i * 4 + 3] > 1e-6 && alpha_in[i] > 0.98 && lead_share(colours[i]) <= 0.0 { 1.0 } else { 0.0 }).collect();
        box_mean(&sure, w, h, erode).par_iter().map(|&m| if m > 0.9999 { 1.0 } else { 0.0 }).collect()
    };
    // per block, as shares of its pixels: the core, the sums of the core's colours and of their
    // squares (how much that colour varies: a steady colour such as a shirt or skin, or a mix of
    // things such as the corner of two flat areas, whose average is no foreground colour), and the
    // partly transparent pixels outside the core (few for an edge, many for a haze)
    const PLANES: usize = 6;
    let blocks: Vec<[f32; PLANES]> = (0..lw * lh)
        .into_par_iter()
        .map(|b| {
            let (bx, by) = (b % lw, b / lw);
            let mut acc = [0.0f32; PLANES];
            let mut count = 0.0f32;
            for y in by * step..((by + 1) * step).min(h) {
                for x in bx * step..((bx + 1) * step).min(w) {
                    let i = y * w + x;
                    count += 1.0;
                    if core[i] > 0.5 {
                        let c = colours[i];
                        acc[0] += 1.0;
                        acc[1] += c[0];
                        acc[2] += c[1];
                        acc[3] += c[2];
                        acc[4] += c[0] * c[0] + c[1] * c[1] + c[2] * c[2];
                    } else if alpha_in[i] > 0.02 && alpha_in[i] < 0.98 {
                        acc[5] += 1.0;
                    }
                }
            }
            acc.map(|v| v / count.max(1.0))
        })
        .collect();
    let planes: Vec<Vec<f32>> = (0..PLANES)
        .into_par_iter()
        .map(|c| {
            let plane: Vec<f32> = blocks.iter().map(|b| b[c]).collect();
            box_mean(&plane, lw, lh, low_radius)
        })
        .collect();
    let sample = |x: usize, y: usize| -> [f32; PLANES] {
        let fx = ((x as f32 + 0.5) / step as f32 - 0.5).clamp(0.0, (lw - 1) as f32);
        let fy = ((y as f32 + 0.5) / step as f32 - 0.5).clamp(0.0, (lh - 1) as f32);
        let (x0, y0) = (fx as usize, fy as usize);
        let (x1, y1) = ((x0 + 1).min(lw - 1), (y0 + 1).min(lh - 1));
        let (tx, ty) = (fx - x0 as f32, fy - y0 as f32);
        let mut out = [0.0f32; PLANES];
        for (o, p) in out.iter_mut().zip(&planes) {
            let top = p[y0 * lw + x0] * (1.0 - tx) + p[y0 * lw + x1] * tx;
            let bottom = p[y1 * lw + x0] * (1.0 - tx) + p[y1 * lw + x1] * tx;
            *o = top * (1.0 - ty) + bottom * ty;
        }
        out
    };
    alpha
        .par_iter_mut()
        .enumerate()
        .map(|(i, a)| {
            if core[i] > 0.5 || px[i * 4 + 3] <= 1e-6 {
                return None;
            }
            let s = sample(i % w, i / w);
            let wsum = s[0];
            if wsum <= 0.002 || s[5] >= 0.17 {
                return None;
            }
            let f = [s[1] / wsum, s[2] / wsum, s[3] / wsum];
            let spread = (s[4] / wsum - (f[0] * f[0] + f[1] * f[1] + f[2] * f[2])).max(0.0).sqrt();
            if spread > 0.16 {
                return None;
            }
            let d = [f[0] - key[0], f[1] - key[1], f[2] - key[2]];
            let dd = d[0] * d[0] + d[1] * d[1] + d[2] * d[2];
            if dd <= 0.05 {
                return None;
            }
            let v = colours[i];
            let u = [v[0] - key[0], v[1] - key[1], v[2] - key[2]];
            let t = ((u[0] * d[0] + u[1] * d[1] + u[2] * d[2]) / dd).clamp(0.0, 1.0);
            // it has to lie near the line from the key to the foreground, or it is something else
            let off = (0..3).map(|c| (u[c] - t * d[c]).powi(2)).sum::<f32>().sqrt();
            if off >= 0.6 {
                return None;
            }
            *a = a.min(t);
            Some(f.map(|x| x.clamp(0.0, 1.0)))
        })
        .collect()
}

/// Track Matte Key: multiplies the layer by another video track's alpha or luma (sampled where
/// the layer lands in the frame).
pub fn track_matte(img: &mut Image, e: &EffectInstance, cx: &FxCtx) {
    let idx = chv(e, "matte") as usize;
    if idx == 0 {
        return;
    }
    let Some(env) = cx.env else { return };
    let Some(matte) = env.track(idx - 1) else { return };
    apply_matte(img, &matte, &env.layer_to_output(), chv(e, "composite") == 1, bv(e, "reverse"));
}

/// Multiply `img` (layer pixels) by `matte` (output pixels) mapped through `m`.
pub(crate) fn apply_matte(img: &mut Image, matte: &Image, m: &Affine, use_luma: bool, reverse: bool) {
    let w = img.w;
    img.px.par_chunks_mut(w * 4).enumerate().for_each(|(y, row)| {
        for x in 0..w {
            let o = m.apply(Vec2::new(x as f64 + 0.5, y as f64 + 0.5));
            let s = matte.sample_bilinear(o.x as f32, o.y as f32);
            let mut v = if use_luma { filmcraft_color::linear_to_srgb(luma([s[0], s[1], s[2]]).clamp(0.0, 1.0)) } else { s[3] };
            if reverse {
                v = 1.0 - v;
            }
            for c in &mut row[x * 4..x * 4 + 4] {
                *c *= v.clamp(0.0, 1.0);
            }
        }
    });
}

/// Cineon Converter (Kodak Cineon printing-density model, film gamma 0.6, 0.002 density / code).
pub fn cineon(img: &mut Image, e: &EffectInstance, cx: &FxCtx) {
    let mode = chv(e, "conversion");
    let black = fv(e, "black10", cx);
    let white = fv(e, "white10", cx).max(black + 1.0);
    let ib = fv(e, "black_internal", cx) / 255.0;
    let iw = fv(e, "white_internal", cx) / 255.0;
    let gamma = fv(e, "gamma", cx).max(0.1);
    let roll = fv(e, "rolloff", cx) / 255.0;
    img.map_rgb(|c, _, _| dec(enc(c).map(|v| cineon_value(v, mode, black, white, ib, iw, gamma, roll))));
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn cineon_value(v: f32, mode: u32, black: f32, white: f32, ib: f32, iw: f32, gamma: f32, roll: f32) -> f32 {
    let k = 0.002 / 0.6;
    let offset = 10f32.powf((black - white) * k);
    match mode {
        0 => {
            let code = v * 1023.0;
            let linv = ((10f32.powf((code - white) * k) - offset) / (1.0 - offset)).max(0.0);
            let mut o = linv.powf(1.0 / gamma);
            o = ib + o * (iw - ib);
            let r0 = 1.0 - roll;
            if roll > 1e-4 && o > r0 {
                o = r0 + roll * (1.0 - (-(o - r0) / roll).exp());
            }
            o.clamp(0.0, 1.0)
        }
        1 => {
            let d = ((v - ib) / (iw - ib).max(1e-4)).clamp(0.0, 1.0);
            let linv = d.powf(gamma);
            let code = white + (linv * (1.0 - offset) + offset).max(1e-6).log10() / k;
            (code / 1023.0).clamp(0.0, 1.0)
        }
        _ => {
            let code = v * 1023.0;
            (((code - black) / (white - black)) * (685.0 - 95.0) + 95.0) / 1023.0
        }
    }
}

/// Noise (and Noise (Legacy)): per-frame random noise added in display space, with grain size.
pub fn noise(img: &mut Image, e: &EffectInstance, cx: &FxCtx) {
    let amt = fv(e, "amount", cx) / 100.0;
    if amt <= 0.0 {
        return;
    }
    let colored = bv(e, "color");
    let clip = bv(e, "clip");
    let grain = (fv(e, "grain_size", cx).max(1.0) * cx.px_scale).max(1.0);
    let frame = crate::effects::noise_frame_seed(cx);
    let w = img.w;
    img.px.par_chunks_mut(w * 4).enumerate().for_each(|(y, row)| {
        for x in 0..w {
            let p = &mut row[x * 4..x * 4 + 4];
            let a = p[3];
            if a <= 0.0 {
                continue;
            }
            let (gx, gy) = ((x as f32 / grain) as usize, (y as f32 / grain) as usize);
            let h = |z: u64| crate::effects::hash3(gx, gy, z) - 0.5;
            let n0 = h(frame);
            let ns = if colored { [n0, h(frame + 7777), h(frame + 99_991)] } else { [n0; 3] };
            let c = enc([p[0] / a, p[1] / a, p[2] / a]);
            let mut o = [c[0] + ns[0] * amt, c[1] + ns[1] * amt, c[2] + ns[2] * amt];
            if clip {
                o = o.map(|v| v.clamp(0.0, 1.0));
            }
            let o = dec(o);
            p[0] = o[0] * a;
            p[1] = o[1] * a;
            p[2] = o[2] * a;
        }
    });
}

#[cfg(test)]
mod box_mean_tests {
    use super::box_mean;

    /// The parallel row/transposed-row passes give the plain mean over the square, edges replicated.
    #[test]
    fn box_mean_matches_a_direct_sum_with_replicated_edges() {
        let (w, h, r) = (7usize, 5usize, 2isize);
        let src: Vec<f32> = (0..w * h).map(|i| ((i * 37) % 11) as f32).collect();
        let got = box_mean(&src, w, h, r as usize);
        let at = |x: isize, y: isize| src[(y.clamp(0, h as isize - 1) as usize) * w + x.clamp(0, w as isize - 1) as usize] as f64;
        for (i, g) in got.iter().enumerate() {
            let (x, y) = ((i % w) as isize, (i / w) as isize);
            let sum: f64 = (-r..=r).flat_map(|dy| (-r..=r).map(move |dx| (dx, dy))).map(|(dx, dy)| at(x + dx, y + dy)).sum();
            let want = sum / ((2 * r + 1) * (2 * r + 1)) as f64;
            assert!((*g as f64 - want).abs() < 1e-4, "({x}, {y}): {g} against {want}");
        }
    }
}
