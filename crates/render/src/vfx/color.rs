//! Colour, keying and matte effects.

use filmcraft_project::EffectInstance;
use filmcraft_project::effect::LIGHT_IDS;
use rayon::prelude::*;

use super::{Affine, FxCtx, Image, Vec2, bv, chv, cv, dec, enc, fv, lin, luma, pv, smoothstep};

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
    let cin = CineonParams { mode, black, white, ib, iw, gamma, roll };
    img.map_rgb(|c, _, _| dec(enc(c).map(|v| cineon_value(v, &cin))));
}

/// Cineon conversion settings: direction (`mode`), 10-bit black and white points, the internal
/// black and white, film gamma and highlight rolloff.
#[derive(Clone, Copy)]
pub(crate) struct CineonParams {
    pub mode: u32,
    pub black: f32,
    pub white: f32,
    pub ib: f32,
    pub iw: f32,
    pub gamma: f32,
    pub roll: f32,
}

pub(crate) fn cineon_value(v: f32, p: &CineonParams) -> f32 {
    let CineonParams { mode, black, white, ib, iw, gamma, roll } = *p;
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
