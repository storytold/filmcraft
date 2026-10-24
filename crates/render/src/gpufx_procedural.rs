//! Evaluated generator and local-stylization effects for the GPU compositor.
//!
//! This module is kept separate so the effect-stage owner can add one dispatch arm without
//! changing the CPU registry or the existing effect contract.

use filmcraft_project::EffectInstance;

use crate::effects::FxCtx;
use crate::image::Image;

#[derive(Clone, Debug, PartialEq)]
pub enum ProceduralOp {
    Checkerboard {
        anchor: [f32; 2],
        size: [f32; 2],
        feather: f32,
        color: [f32; 3],
        opacity: f32,
        mode: u32,
    },
    Grid {
        size: f32,
        border: f32,
        color: [f32; 3],
        opacity: f32,
    },
    Circle {
        center: [f32; 2],
        radius: f32,
        color: [f32; 3],
        opacity: f32,
    },
    Ellipse {
        center: [f32; 2],
        radii: [f32; 2],
        thickness: f32,
        softness: f32,
        inside: [f32; 3],
        outside: [f32; 3],
        composite: bool,
    },
    Ramp {
        start: [f32; 2],
        end: [f32; 2],
        start_color: [f32; 4],
        end_color: [f32; 4],
        radial: bool,
        blend: f32,
    },
    Gradient {
        start: [f32; 2],
        end: [f32; 2],
        start_color: [f32; 4],
        end_color: [f32; 4],
        shape: u32,
        scatter: f32,
        gamma: f32,
        blend: f32,
    },
    FourColorGradient {
        colors: [[f32; 3]; 4],
        blend_exp: f32,
        opacity: f32,
        size: [f32; 2],
    },
    CellPattern {
        pattern: u32,
        invert: bool,
        contrast: f32,
        overflow: u32,
        disperse: f32,
        size: f32,
        offset: [f32; 2],
        evolution: f32,
        seed: u32,
    },
    Emboss {
        direction: [f32; 2],
        relief: f32,
        contrast: f32,
        blend: f32,
        color: bool,
    },
    FindEdges {
        invert: bool,
        blend: f32,
    },
    Strobe {
        color: [f32; 3],
        blend: f32,
        active: bool,
    },
    /// Lens Flare: `t`/`r` are the position along the line through the frame centre and the radius of each
    /// of the first `ghosts` ghosts (their hue follows the index); `k` is brightness times (1 - blend).
    LensFlare {
        c: [f32; 2],
        scale: f32,
        k: f32,
        tint: [f32; 3],
        lens: u32,
        ghosts: u32,
        t: [f32; 8],
        r: [f32; 8],
    },
}

impl ProceduralOp {
    pub fn eval(e: &EffectInstance, cx: &FxCtx<'_>, w: usize, h: usize) -> Option<Self> {
        if !e.enabled || w == 0 || h == 0 {
            return None;
        }
        use crate::effects::{color as effect_color, point as effect_point};
        use crate::vfx::{bv, chv, cv, fv, offv, pv};
        let img = Image { w, h, px: Vec::new() };
        let scale = cx.px_scale;
        Some(match e.effect.as_str() {
            "checkerboard" => {
                let a = pv(e, "anchor", cx, &img);
                let cw = (fv(e, "width", cx) * scale).max(1.0);
                let ch = if bv(e, "square") { cw } else { (fv(e, "height", cx) * scale).max(1.0) };
                let c = crate::vfx::lin(cv(e, "color", cx));
                Self::Checkerboard {
                    anchor: [a.x as f32, a.y as f32],
                    size: [cw, ch],
                    feather: fv(e, "feather", cx) * scale,
                    color: c,
                    opacity: fv(e, "opacity", cx) / 100.0,
                    mode: chv(e, "mode"),
                }
            }
            "grid" => Self::Grid {
                size: (fv(e, "size", cx) * scale).max(1.0),
                border: fv(e, "border", cx) * scale,
                color: {
                    let c = effect_color(e, "color", cx);
                    crate::vfx::dec([c[0], c[1], c[2]])
                },
                opacity: fv(e, "opacity", cx) / 100.0,
            },
            "circle" => {
                let p = effect_point(e, "center", cx, &img);
                Self::Circle {
                    center: [p.x as f32, p.y as f32],
                    radius: fv(e, "radius", cx) * scale,
                    color: {
                        let c = effect_color(e, "color", cx);
                        crate::vfx::dec([c[0], c[1], c[2]])
                    },
                    opacity: fv(e, "opacity", cx) / 100.0,
                }
            }
            "ellipse" => {
                let p = pv(e, "center", cx, &img);
                Self::Ellipse {
                    center: [p.x as f32, p.y as f32],
                    radii: [(fv(e, "width", cx) * scale / 2.0).max(0.5), (fv(e, "height", cx) * scale / 2.0).max(0.5)],
                    thickness: (fv(e, "thickness", cx) * scale).max(0.5),
                    softness: fv(e, "softness", cx) / 100.0,
                    inside: crate::vfx::lin(cv(e, "inside", cx)),
                    outside: crate::vfx::lin(cv(e, "outside", cx)),
                    composite: bv(e, "composite"),
                }
            }
            "ramp" => {
                let s = effect_point(e, "start", cx, &img);
                let p = effect_point(e, "end", cx, &img);
                let sc = effect_color(e, "start_color", cx);
                let ec = effect_color(e, "end_color", cx);
                Self::Ramp {
                    start: [s.x as f32, s.y as f32],
                    end: [p.x as f32, p.y as f32],
                    start_color: sc,
                    end_color: ec,
                    radial: chv(e, "shape") == 1,
                    blend: fv(e, "blend", cx) / 100.0,
                }
            }
            "gradient" => {
                let s = pv(e, "start", cx, &img);
                let p = pv(e, "end", cx, &img);
                let mid = (fv(e, "midpoint", cx) / 100.0).clamp(0.01, 0.99);
                Self::Gradient {
                    start: [s.x as f32, s.y as f32],
                    end: [p.x as f32, p.y as f32],
                    start_color: cv(e, "start_color", cx),
                    end_color: cv(e, "end_color", cx),
                    shape: chv(e, "shape"),
                    scatter: fv(e, "scatter", cx) / 100.0,
                    gamma: (0.5f32).ln() / mid.ln(),
                    blend: fv(e, "blend", cx) / 100.0,
                }
            }
            "four_color_gradient" => Self::FourColorGradient {
                colors: ["c1", "c2", "c3", "c4"].map(|id| {
                    let c = effect_color(e, id, cx);
                    [c[0], c[1], c[2]]
                }),
                blend_exp: 1.0 + fv(e, "blend", cx) / 100.0,
                opacity: fv(e, "opacity", cx) / 100.0,
                size: [w as f32, h as f32],
            },
            "cell_pattern" => Self::CellPattern {
                pattern: chv(e, "pattern"),
                invert: bv(e, "invert"),
                contrast: fv(e, "contrast", cx) / 100.0,
                overflow: chv(e, "overflow"),
                disperse: fv(e, "disperse", cx).clamp(0.0, 1.5),
                size: (fv(e, "size", cx) * scale).max(1.0),
                offset: {
                    let p = offv(e, "offset", cx);
                    [p.x as f32, p.y as f32]
                },
                evolution: if chv(e, "pattern") == 3 { 0.0 } else { fv(e, "evolution", cx).to_radians() },
                seed: fv(e, "seed", cx).max(0.0) as u32,
            },
            "emboss" | "color_emboss" => {
                let a = (fv(e, "direction", cx) as f64).to_radians();
                Self::Emboss {
                    direction: [a.cos() as f32, -(a.sin() as f32)],
                    relief: fv(e, "relief", cx) * scale.max(0.25),
                    contrast: fv(e, "contrast", cx) / 100.0,
                    blend: fv(e, "blend", cx) / 100.0,
                    color: e.effect == "color_emboss",
                }
            }
            "find_edges" => Self::FindEdges { invert: bv(e, "invert"), blend: fv(e, "blend", cx) / 100.0 },
            "lens_flare" => {
                let f = crate::vfx::lens_flare_params(e, cx, w, h);
                let (mut t, mut r) = ([0.0f32; 8], [0.0f32; 8]);
                for (i, (gt, gr, _)) in f.ghosts.iter().take(8).enumerate() {
                    (t[i], r[i]) = (*gt as f32, *gr as f32);
                }
                Self::LensFlare {
                    c: [f.c.x as f32, f.c.y as f32],
                    scale: f.scale as f32,
                    k: f.k,
                    tint: f.tint,
                    lens: f.lens,
                    ghosts: f.ghosts.len().min(8) as u32,
                    t,
                    r,
                }
            }
            "strobe" => {
                let period = fv(e, "period", cx).max(0.001) as f64;
                let active = cx.seconds % period < fv(e, "duration", cx) as f64;
                Self::Strobe { color: crate::vfx::lin(cv(e, "color", cx)), blend: fv(e, "blend", cx) / 100.0, active }
            }
            _ => return None,
        })
    }

    pub fn matches_id(&self, id: &str) -> bool {
        matches!(
            (self, id),
            (Self::Checkerboard { .. }, "checkerboard")
                | (Self::Grid { .. }, "grid")
                | (Self::Circle { .. }, "circle")
                | (Self::Ellipse { .. }, "ellipse")
                | (Self::Ramp { .. }, "ramp")
                | (Self::Gradient { .. }, "gradient")
                | (Self::FourColorGradient { .. }, "four_color_gradient")
                | (Self::CellPattern { .. }, "cell_pattern")
                | (Self::Emboss { color: false, .. }, "emboss")
                | (Self::Emboss { color: true, .. }, "color_emboss")
                | (Self::FindEdges { .. }, "find_edges")
                | (Self::Strobe { .. }, "strobe")
                | (Self::LensFlare { .. }, "lens_flare")
        )
    }

    pub fn gpu_ok(&self) -> bool {
        let fin = |a: &[f32]| a.iter().all(|x| x.is_finite());
        match self {
            Self::Checkerboard { anchor, size, feather, color, opacity, .. } => {
                fin(anchor) && fin(size) && fin(&[*feather, *opacity]) && fin(color) && size.iter().all(|x| *x >= 1.0)
            }
            Self::Grid { size, border, color, opacity } => fin(&[*size, *border, *opacity]) && fin(color) && *size >= 1.0,
            Self::Circle { center, radius, color, opacity } => fin(center) && fin(&[*radius, *opacity]) && fin(color),
            Self::Ellipse { center, radii, thickness, softness, inside, outside, .. } => {
                fin(center) && fin(radii) && fin(&[*thickness, *softness]) && fin(inside) && fin(outside)
            }
            Self::Ramp { start, end, start_color, end_color, blend, .. } => fin(start) && fin(end) && fin(start_color) && fin(end_color) && blend.is_finite(),
            Self::Gradient { start, end, start_color, end_color, scatter, gamma, blend, .. } => {
                fin(start) && fin(end) && fin(start_color) && fin(end_color) && fin(&[*scatter, *gamma, *blend])
            }
            Self::FourColorGradient { colors, blend_exp, opacity, size } => colors.iter().all(|c| fin(c)) && fin(&[*blend_exp, *opacity]) && fin(size),
            Self::CellPattern { disperse, size, offset, evolution, .. } => fin(&[*disperse, *size, *evolution]) && fin(offset) && *size >= 1.0,
            Self::Emboss { direction, relief, contrast, blend, .. } => fin(direction) && fin(&[*relief, *contrast, *blend]),
            Self::FindEdges { blend, .. } => blend.is_finite(),
            Self::Strobe { color, blend, .. } => fin(color) && blend.is_finite(),
            Self::LensFlare { c, scale, k, tint, ghosts, t, r, .. } => {
                fin(c) && fin(tint) && fin(t) && fin(r) && fin(&[*scale, *k]) && *scale > 0.0 && *ghosts <= 8
            }
        }
    }

    pub fn pack(&self) -> (u32, [u32; 4], [f32; 24]) {
        let mut i = [0; 4];
        let mut p = [0.0; 24];
        macro_rules! put {
            ($n:expr,$v:expr) => {
                p[$n] = $v
            };
        }
        let op = match self {
            Self::Checkerboard { anchor, size, feather, color, opacity, mode } => {
                i[0] = *mode;
                p[0..2].copy_from_slice(anchor);
                p[2..4].copy_from_slice(size);
                put!(4, *feather);
                p[5..8].copy_from_slice(color);
                put!(8, *opacity);
                96
            }
            Self::Grid { size, border, color, opacity } => {
                put!(0, *size);
                put!(1, *border);
                p[2..5].copy_from_slice(color);
                put!(5, *opacity);
                97
            }
            Self::Circle { center, radius, color, opacity } => {
                p[0..2].copy_from_slice(center);
                put!(2, *radius);
                p[3..6].copy_from_slice(color);
                put!(6, *opacity);
                98
            }
            Self::Ellipse { center, radii, thickness, softness, inside, outside, composite } => {
                i[0] = u32::from(*composite);
                p[0..2].copy_from_slice(center);
                p[2..4].copy_from_slice(radii);
                put!(4, *thickness);
                put!(5, *softness);
                p[6..9].copy_from_slice(inside);
                p[9..12].copy_from_slice(outside);
                99
            }
            Self::Ramp { start, end, start_color, end_color, radial, blend } => {
                i[0] = u32::from(*radial);
                p[0..2].copy_from_slice(start);
                p[2..4].copy_from_slice(end);
                p[4..8].copy_from_slice(start_color);
                p[8..12].copy_from_slice(end_color);
                put!(12, *blend);
                100
            }
            Self::Gradient { start, end, start_color, end_color, shape, scatter, gamma, blend } => {
                i[0] = *shape;
                p[0..2].copy_from_slice(start);
                p[2..4].copy_from_slice(end);
                p[4..8].copy_from_slice(start_color);
                p[8..12].copy_from_slice(end_color);
                put!(12, *scatter);
                put!(13, *gamma);
                put!(14, *blend);
                101
            }
            Self::FourColorGradient { colors, blend_exp, opacity, size } => {
                for (n, c) in colors.iter().enumerate() {
                    p[n * 3..n * 3 + 3].copy_from_slice(c);
                }
                put!(12, *blend_exp);
                put!(13, *opacity);
                p[14..16].copy_from_slice(size);
                102
            }
            Self::CellPattern { pattern, invert, contrast, overflow, disperse, size, offset, evolution, seed } => {
                i = [*pattern, u32::from(*invert), *overflow, *seed];
                put!(0, *contrast);
                put!(1, *disperse);
                put!(2, *size);
                p[3..5].copy_from_slice(offset);
                put!(5, *evolution);
                103
            }
            Self::Emboss { direction, relief, contrast, blend, color } => {
                i[0] = u32::from(*color);
                p[0..2].copy_from_slice(direction);
                put!(2, *relief);
                put!(3, *contrast);
                put!(4, *blend);
                104
            }
            Self::FindEdges { invert, blend } => {
                i[0] = u32::from(*invert);
                put!(0, *blend);
                105
            }
            Self::Strobe { color, blend, active } => {
                i[0] = u32::from(*active);
                p[0..3].copy_from_slice(color);
                put!(3, *blend);
                106
            }
            Self::LensFlare { c, scale, k, tint, lens, ghosts, t, r } => {
                i[0] = *lens;
                i[1] = *ghosts;
                p[0..2].copy_from_slice(c);
                put!(2, *scale);
                put!(3, *k);
                p[4..7].copy_from_slice(tint);
                p[8..16].copy_from_slice(t);
                p[16..24].copy_from_slice(r);
                109
            }
        };
        (op, i, p)
    }

    pub fn apply(&self, img: &mut Image) {
        if img.w == 0 || img.h == 0 {
            return;
        }
        let src = img.clone();
        let (w, h) = (img.w, img.h);
        for y in 0..h {
            for x in 0..w {
                let pos = [x as f32 + 0.5, y as f32 + 0.5];
                let k = y * w + x;
                let p = src.get(x, y);
                let out = match self {
                    Self::Checkerboard { anchor, size, feather, color, opacity, mode } => {
                        let uv = [(pos[0] - anchor[0]) / size[0], (pos[1] - anchor[1]) / size[1]];
                        let parity = (uv[0].floor() as i64 + uv[1].floor() as i64).rem_euclid(2) as f32;
                        let du = (uv[0] - uv[0].round()).abs() * size[0];
                        let dv = (uv[1] - uv[1].round()).abs() * size[1];
                        let edge = if *feather > 0.0 { crate::vfx::smoothstep(0.0, *feather, du.min(dv)) } else { 1.0 };
                        let a = parity * edge * *opacity;
                        let s = [color[0] * a, color[1] * a, color[2] * a, a];
                        match *mode {
                            0 => s,
                            1 => over_pixel(p, s),
                            _ => blend_pixel(p, *color, a, *mode),
                        }
                    }
                    Self::Grid { size, border, color, opacity } => {
                        let dx = (x as f32 - w as f32 / 2.0).rem_euclid(*size);
                        let dy = (y as f32 - h as f32 / 2.0).rem_euclid(*size);
                        let d = dx.min(*size - dx).min(dy.min(*size - dy));
                        let a = ((*border * 0.5 - d + 0.5).clamp(0.0, 1.0)) * *opacity;
                        over_pixel(p, [color[0] * a, color[1] * a, color[2] * a, a])
                    }
                    Self::Circle { center, radius, color, opacity } => {
                        let d = ((pos[0] - center[0]).powi(2) + (pos[1] - center[1]).powi(2)).sqrt();
                        let a = (*radius - d + 0.5).clamp(0.0, 1.0) * *opacity;
                        over_pixel(p, [color[0] * a, color[1] * a, color[2] * a, a])
                    }
                    Self::Ellipse { center, radii, thickness, softness, inside, outside, composite } => {
                        let dx = (pos[0] - center[0]) / radii[0];
                        let dy = (pos[1] - center[1]) / radii[1];
                        let d = ((dx * dx + dy * dy).sqrt() - 1.0).abs() * radii[0].min(radii[1]);
                        let half = *thickness / 2.0;
                        let a = 1.0 - crate::vfx::smoothstep(half * (1.0 - *softness) - 0.5, half + 0.5, d);
                        let t = 1.0 - crate::vfx::smoothstep(0.0, half, d);
                        let c = crate::vfx::lerp3(*outside, *inside, t);
                        let s = [c[0] * a, c[1] * a, c[2] * a, a];
                        if *composite { over_pixel(p, s) } else { s }
                    }
                    Self::Ramp { start, end, start_color, end_color, radial, blend } => {
                        let vx = end[0] - start[0];
                        let vy = end[1] - start[1];
                        let len2 = (vx * vx + vy * vy).max(1e-9);
                        let t = if *radial {
                            ((pos[0] - start[0]).hypot(pos[1] - start[1]) / len2.sqrt()).clamp(0.0, 1.0)
                        } else {
                            (((pos[0] - start[0]) * vx + (pos[1] - start[1]) * vy) / len2).clamp(0.0, 1.0)
                        };
                        let c = crate::vfx::dec([
                            start_color[0] + (end_color[0] - start_color[0]) * t,
                            start_color[1] + (end_color[1] - start_color[1]) * t,
                            start_color[2] + (end_color[2] - start_color[2]) * t,
                        ]);
                        let a = 1.0 - *blend;
                        [c[0] * a + p[0] * *blend, c[1] * a + p[1] * *blend, c[2] * a + p[2] * *blend, a + p[3] * *blend]
                    }
                    Self::Gradient { start, end, start_color, end_color, shape, scatter, gamma, blend } => {
                        let vx = end[0] - start[0];
                        let vy = end[1] - start[1];
                        let l2 = (vx * vx + vy * vy).max(1e-6);
                        let dx = pos[0] - start[0];
                        let dy = pos[1] - start[1];
                        let along = (dx * vx + dy * vy) / l2;
                        let across = (-dx * vy + dy * vx) / l2;
                        let mut t = match *shape {
                            1 => (dx * dx + dy * dy).sqrt() / l2.sqrt(),
                            2 => along.abs(),
                            3 => along.abs() + across.abs(),
                            _ => along,
                        };
                        if *scatter > 0.0 {
                            t += (crate::vfx::hash01(x as i64, y as i64, 0, 13) - 0.5) * *scatter * 0.1;
                        }
                        let t = t.clamp(0.0, 1.0).powf(*gamma);
                        let c =
                            crate::vfx::dec(crate::vfx::lerp3([start_color[0], start_color[1], start_color[2]], [end_color[0], end_color[1], end_color[2]], t));
                        let a = start_color[3] + (end_color[3] - start_color[3]) * t;
                        let q = [c[0] * a, c[1] * a, c[2] * a, a];
                        let b = *blend;
                        [0, 1, 2, 3].map(|i| q[i] + (p[i] - q[i]) * b)
                    }
                    Self::FourColorGradient { colors, blend_exp, opacity, size } => {
                        let pts = [
                            [0.25 * size[0], 0.25 * size[1]],
                            [0.75 * size[0], 0.25 * size[1]],
                            [0.25 * size[0], 0.75 * size[1]],
                            [0.75 * size[0], 0.75 * size[1]],
                        ];
                        let mut acc = [0.0; 3];
                        let mut sum = 0.0;
                        for j in 0..4 {
                            let d = ((pos[0] - pts[j][0]).powi(2) + (pos[1] - pts[j][1]).powi(2)).sqrt().max(1.0);
                            let q = 1.0 / d.powf(*blend_exp);
                            for c in 0..3 {
                                acc[c] += colors[j][c] * q;
                            }
                            sum += q;
                        }
                        let c = crate::vfx::dec(acc.map(|v| v / sum));
                        let a = *opacity;
                        over_pixel(p, [c[0] * a, c[1] * a, c[2] * a, a])
                    }
                    Self::CellPattern { pattern, invert, contrast, overflow, disperse, size, offset, evolution, seed } => {
                        let u = (pos[0] + offset[0]) / *size;
                        let v = (pos[1] + offset[1]) / *size;
                        let ci = u.floor() as i64;
                        let cj = v.floor() as i64;
                        let (mut f1, mut f2, mut id) = (f32::INFINITY, f32::INFINITY, 0.0);
                        for dj in -1..=1 {
                            for di in -1..=1 {
                                let (gi, gj) = (ci + di, cj + dj);
                                let ph = crate::vfx::hash01(gi, gj, 9, *seed as u64) * std::f32::consts::TAU;
                                let jx = 0.5 + (crate::vfx::hash01(gi, gj, 0, *seed as u64) - 0.5) * *disperse + (*evolution + ph).cos() * 0.15 * *disperse;
                                let jy = 0.5 + (crate::vfx::hash01(gi, gj, 1, *seed as u64) - 0.5) * *disperse + (*evolution + ph).sin() * 0.15 * *disperse;
                                let d = ((gi as f32 + jx - u).powi(2) + (gj as f32 + jy - v).powi(2)).sqrt();
                                if d < f1 {
                                    f2 = f1;
                                    f1 = d;
                                    id = crate::vfx::hash01(gi, gj, 2, *seed as u64)
                                } else if d < f2 {
                                    f2 = d
                                }
                            }
                        }
                        let mut val = match *pattern {
                            0 => (1.0 - f1 * 1.4).max(0.0),
                            1 => (f2 - f1) * 2.0,
                            2 | 3 => id,
                            4 => id * crate::vfx::smoothstep(0.0, 0.12, f2 - f1),
                            5 => (f2 - f1) / (f2 + f1).max(1e-4) * 2.0,
                            6 => ((f2 - f1) * 2.0 + id) * 0.5,
                            _ => 1.0 - crate::vfx::smoothstep(0.0, 0.12, (f1 - 0.35).abs()),
                        };
                        val = (val - 0.5) * *contrast + 0.5;
                        val = match *overflow {
                            1 => 1.0 / (1.0 + (-(val - 0.5) * 4.0).exp()),
                            2 => {
                                let m = val.rem_euclid(2.0);
                                if m > 1.0 { 2.0 - m } else { m }
                            }
                            _ => val.clamp(0.0, 1.0),
                        };
                        if *invert {
                            val = 1.0 - val;
                        }
                        let g = filmcraft_color::srgb_to_linear(val);
                        [g, g, g, 1.0]
                    }
                    Self::Emboss { direction, relief, contrast, blend, color } => {
                        let a = src.sample_bilinear_clamped(pos[0] + direction[0] * relief, pos[1] + direction[1] * relief);
                        let b = src.sample_bilinear_clamped(pos[0] - direction[0] * relief, pos[1] - direction[1] * relief);
                        let lum = |q: [f32; 4]| crate::vfx::luma(if *color { Image::unpremul(q) } else { [q[0], q[1], q[2]] });
                        let d = lum(a) - lum(b);
                        let rgb = if *color {
                            let v = crate::vfx::enc(Image::unpremul(p));
                            let straight = crate::vfx::dec(crate::vfx::lerp3(v.map(|x| (x + d * *contrast * 2.0).clamp(0.0, 1.0)), v, *blend));
                            [straight[0] * p[3], straight[1] * p[3], straight[2] * p[3]]
                        } else {
                            let v = filmcraft_color::srgb_to_linear((0.5 + d * *contrast * 2.0).clamp(0.0, 1.0));
                            [v * p[3] * (1.0 - *blend) + p[0] * *blend, v * p[3] * (1.0 - *blend) + p[1] * *blend, v * p[3] * (1.0 - *blend) + p[2] * *blend]
                        };
                        [rgb[0], rgb[1], rgb[2], p[3]]
                    }
                    Self::FindEdges { invert, blend } => {
                        let l = |dx: isize, dy: isize| {
                            let q = src.get_clamped(x as isize + dx, y as isize + dy);
                            crate::vfx::luma([q[0], q[1], q[2]])
                        };
                        let gx = l(1, -1) + 2.0 * l(1, 0) + l(1, 1) - l(-1, -1) - 2.0 * l(-1, 0) - l(-1, 1);
                        let gy = l(-1, 1) + 2.0 * l(0, 1) + l(1, 1) - l(-1, -1) - 2.0 * l(0, -1) - l(1, -1);
                        let mut m = (gx * gx + gy * gy).sqrt().min(1.0);
                        if !*invert {
                            m = 1.0 - m;
                        }
                        let g = m * p[3] * (1.0 - *blend);
                        [g + p[0] * *blend, g + p[1] * *blend, g + p[2] * *blend, p[3]]
                    }
                    Self::LensFlare { c, scale, k, tint, lens, ghosts, t, r } => {
                        let hues = [[0.4, 0.6, 1.0], [1.0, 0.5, 0.3], [0.4, 1.0, 0.5], [0.9, 0.4, 1.0]];
                        let params = crate::vfx::LensFlareParams {
                            c: filmcraft_geom::Vec2::new(f64::from(c[0]), f64::from(c[1])),
                            scale: f64::from(*scale),
                            core_k: [18.0, 26.0, 14.0, 22.0][(*lens).min(3) as usize],
                            lens: *lens,
                            mid: filmcraft_geom::Vec2::new(w as f64 / 2.0, h as f64 / 2.0),
                            ghosts: (0..*ghosts as usize).map(|n| (f64::from(t[n]), f64::from(r[n]), hues[n % 4])).collect(),
                            tint: *tint,
                            k: *k,
                        };
                        let mut o = p;
                        crate::vfx::add_light(&mut o, crate::vfx::lens_flare_add(&params, f64::from(pos[0]), f64::from(pos[1])));
                        o
                    }
                    Self::Strobe { color, blend, active } => {
                        if !*active {
                            p
                        } else {
                            let orig = Image::unpremul(p);
                            let c = crate::vfx::lerp3(*color, orig, *blend);
                            let a = p[3];
                            [c[0] * a, c[1] * a, c[2] * a, a]
                        }
                    }
                };
                img.px[k * 4..k * 4 + 4].copy_from_slice(&out);
            }
        }
    }
}

fn over_pixel(mut dst: [f32; 4], src: [f32; 4]) -> [f32; 4] {
    let a = src[3];
    for c in 0..3 {
        dst[c] = src[c] + dst[c] * (1.0 - a);
    }
    dst[3] = a + dst[3] * (1.0 - a);
    dst
}
fn blend_pixel(p: [f32; 4], color: [f32; 3], amount: f32, mode: u32) -> [f32; 4] {
    let a = p[3];
    if a <= 1e-6 {
        return p;
    }
    let b = [p[0] / a, p[1] / a, p[2] / a];
    let c = match mode {
        2 => [b[0] + color[0], b[1] + color[1], b[2] + color[2]],
        3 => [b[0] * color[0], b[1] * color[1], b[2] * color[2]],
        4 => [b[0] + color[0] - b[0] * color[0], b[1] + color[1] - b[1] * color[1], b[2] + color[2] - b[2] * color[2]],
        _ => [b[0] * color[0] * 2.0, b[1] * color[1] * 2.0, b[2] * color[2] * 2.0],
    };
    let c = crate::vfx::lerp3(b, c, amount);
    [c[0] * a, c[1] * a, c[2] * a, a]
}

#[cfg(test)]
mod tests {
    use super::ProceduralOp as P;

    #[test]
    fn procedural_ids_have_stable_shader_opcodes() {
        let ops = [
            (P::Checkerboard { anchor: [0.0, 0.0], size: [4.0, 4.0], feather: 0.0, color: [1.0; 3], opacity: 1.0, mode: 0 }, "checkerboard", 96),
            (P::Grid { size: 4.0, border: 1.0, color: [1.0; 3], opacity: 1.0 }, "grid", 97),
            (P::Circle { center: [2.0, 2.0], radius: 1.0, color: [1.0; 3], opacity: 1.0 }, "circle", 98),
            (
                P::Ellipse { center: [2.0, 2.0], radii: [1.0, 1.0], thickness: 1.0, softness: 0.0, inside: [1.0; 3], outside: [0.0; 3], composite: false },
                "ellipse",
                99,
            ),
            (
                P::Ramp { start: [0.0, 0.0], end: [4.0, 4.0], start_color: [0.0, 0.0, 0.0, 1.0], end_color: [1.0, 1.0, 1.0, 1.0], radial: false, blend: 0.0 },
                "ramp",
                100,
            ),
            (
                P::Gradient {
                    start: [0.0, 0.0],
                    end: [4.0, 4.0],
                    start_color: [0.0, 0.0, 0.0, 1.0],
                    end_color: [1.0, 1.0, 1.0, 1.0],
                    shape: 0,
                    scatter: 0.0,
                    gamma: 1.0,
                    blend: 0.0,
                },
                "gradient",
                101,
            ),
            (P::FourColorGradient { colors: [[1.0; 3]; 4], blend_exp: 2.0, opacity: 1.0, size: [4.0, 4.0] }, "four_color_gradient", 102),
            (
                P::CellPattern { pattern: 0, invert: false, contrast: 1.0, overflow: 0, disperse: 1.0, size: 4.0, offset: [0.0; 2], evolution: 0.0, seed: 7 },
                "cell_pattern",
                103,
            ),
            (P::Emboss { direction: [1.0, 0.0], relief: 1.0, contrast: 1.0, blend: 0.0, color: false }, "emboss", 104),
            (P::Emboss { direction: [1.0, 0.0], relief: 1.0, contrast: 1.0, blend: 0.0, color: true }, "color_emboss", 104),
            (P::FindEdges { invert: false, blend: 0.0 }, "find_edges", 105),
            (P::Strobe { color: [1.0; 3], blend: 0.0, active: true }, "strobe", 106),
        ];
        for (op, id, opcode) in ops {
            assert!(op.matches_id(id));
            assert_eq!(op.pack().0, opcode);
            assert!(op.gpu_ok(), "{id}");
        }
    }

    #[test]
    fn generators_preserve_cpu_alpha_contracts() {
        let mut image = crate::image::Image { w: 1, h: 1, px: vec![0.1, 0.05, 0.02, 0.5] };
        P::Ramp { start: [0.0, 0.0], end: [1.0, 1.0], start_color: [1.0, 0.0, 0.0, 0.0], end_color: [1.0, 0.0, 0.0, 0.0], radial: false, blend: 0.0 }
            .apply(&mut image);
        assert_eq!(image.px, [1.0, 0.0, 0.0, 1.0]);
        let mut image = crate::image::Image { w: 1, h: 1, px: vec![0.1, 0.05, 0.02, 0.5] };
        P::Strobe { color: [0.8, 0.2, 0.1], blend: 0.0, active: false }.apply(&mut image);
        assert_eq!(image.px, [0.1, 0.05, 0.02, 0.5]);
    }
}
