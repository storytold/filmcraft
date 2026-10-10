//! Lumetri ▸ Color Wheels & Match ▸ **Apply Match**: grade the current clip so its colour matches
//! a reference frame. A documented, non-ML method:
//!
//! 1. Both frames are downsampled (≤ 96 px wide) and converted to **Oklab** (Björn Ottosson, 2020),
//!    a perceptual space where L is lightness and (a, b) the colour axes.
//! 2. Each pixel is weighted into three **tonal ranges** by its L — shadows `smoothstep(0.55, 0.15,
//!    L)`, highlights `smoothstep(0.45, 0.85, L)`, midtones the rest — and per range we take the
//!    mean L, a and b. A global **chroma** statistic (mean √(a² + b²)) captures saturation.
//! 3. **Skin-tone protection** ("Face Detection"): pixels near the skin-tone line (Oklab hue
//!    20°–75°, chroma 0.02–0.2, L 0.25–0.92) are down-weighted (×0.2) in the colour statistics, so
//!    faces do not drive the cast; afterwards the solved cast is scaled down until the mean hue of
//!    the current frame's skin pixels moves by at most 8°.
//! 4. The match is solved **in Lumetri's own parameters** — the three colour wheels (x, y), their
//!    lightness sliders and Basic saturation (10 unknowns) — by damped Gauss–Newton
//!    (Levenberg–Marquardt) with numerical derivatives through the real Lumetri implementation
//!    ([`crate::effects::apply`]), so the result is an ordinary, editable grade. A small penalty
//!    on the parameters keeps the grade minimal when the statistics are already close.

use filmcraft_geom::Vec2;
use filmcraft_project::{EffectInstance, ParamValue};
use filmcraft_time::Tick;

use crate::Image;
use crate::effects::{FxCtx, apply};

/// sRGB-primaries linear RGB → Oklab.
pub fn oklab(c: [f32; 3]) -> [f32; 3] {
    let [r, g, b] = c.map(|v| v.max(0.0));
    let l = 0.412_221_47 * r + 0.536_332_55 * g + 0.051_445_995 * b;
    let m = 0.211_903_5 * r + 0.680_699_5 * g + 0.107_396_96 * b;
    let s = 0.088_302_46 * r + 0.281_718_85 * g + 0.629_978_7 * b;
    let (l, m, s) = (l.cbrt(), m.cbrt(), s.cbrt());
    [
        0.210_454_26 * l + 0.793_617_8 * m - 0.004_072_047 * s,
        1.977_998_5 * l - 2.428_592_2 * m + 0.450_593_7 * s,
        0.025_904_037 * l + 0.782_771_77 * m - 0.808_675_77 * s,
    ]
}

fn smoothstep(e0: f32, e1: f32, x: f32) -> f32 {
    let t = ((x - e0) / (e1 - e0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// Tonal-range weights (shadows, midtones, highlights) of an Oklab lightness.
pub fn tonal_weights(l: f32) -> [f32; 3] {
    let s = smoothstep(0.55, 0.15, l);
    let h = smoothstep(0.45, 0.85, l);
    [s, (1.0 - s - h).max(0.0), h]
}

/// How much a pixel looks like skin (0..1).
pub fn skin_weight(lab: [f32; 3]) -> f32 {
    let c = (lab[1] * lab[1] + lab[2] * lab[2]).sqrt();
    if !(0.02..=0.2).contains(&c) || !(0.25..=0.92).contains(&lab[0]) {
        return 0.0;
    }
    let h = lab[2].atan2(lab[1]).to_degrees();
    let d = if h < 20.0 {
        20.0 - h
    } else if h > 75.0 {
        h - 75.0
    } else {
        0.0
    };
    (1.0 - d / 10.0).clamp(0.0, 1.0)
}

/// Statistics of a frame: per tonal range (weight, mean L, mean a, mean b), mean chroma, and
/// the mean (a, b) of its skin pixels with their total weight.
#[derive(Clone, Debug, PartialEq)]
pub struct Stats {
    pub bands: [[f32; 4]; 3],
    pub chroma: f32,
    pub skin: [f32; 3],
}

/// Downsample to at most `max_w` pixels wide (box filter).
pub fn shrink(img: &Image, max_w: usize) -> Image {
    let mut out = img.clone();
    while out.w > max_w {
        out = out.downsample2();
    }
    out
}

pub fn stats(img: &Image, skin_protect: bool) -> Stats {
    let mut acc = [[0f64; 4]; 3];
    let mut chroma = (0f64, 0f64);
    let mut skin = [0f64; 3];
    for p in img.px.as_chunks::<4>().0 {
        if p[3] <= 1e-3 {
            continue;
        }
        let lab = oklab([p[0] / p[3], p[1] / p[3], p[2] / p[3]]);
        let sk = skin_weight(lab);
        let cw = if skin_protect { 1.0 - 0.8 * sk } else { 1.0 } as f64;
        let w = tonal_weights(lab[0]);
        for k in 0..3 {
            let wk = w[k] as f64;
            acc[k][0] += wk;
            acc[k][1] += wk * lab[0] as f64;
            // colour means use the skin-protected weight (stored relative to wk)
            acc[k][2] += wk * cw * lab[1] as f64;
            acc[k][3] += wk * cw * lab[2] as f64;
        }
        chroma.0 += cw * ((lab[1] * lab[1] + lab[2] * lab[2]).sqrt()) as f64;
        chroma.1 += cw;
        skin[0] += sk as f64 * lab[1] as f64;
        skin[1] += sk as f64 * lab[2] as f64;
        skin[2] += sk as f64;
    }
    // colour weights per band for normalisation
    let mut cwsum = [0f64; 3];
    for p in img.px.as_chunks::<4>().0 {
        if p[3] <= 1e-3 {
            continue;
        }
        let lab = oklab([p[0] / p[3], p[1] / p[3], p[2] / p[3]]);
        let cw = if skin_protect { 1.0 - 0.8 * skin_weight(lab) } else { 1.0 } as f64;
        let w = tonal_weights(lab[0]);
        for k in 0..3 {
            cwsum[k] += w[k] as f64 * cw;
        }
    }
    let n = img.px.len().max(4) as f64 / 4.0;
    let bands = std::array::from_fn(|k| {
        let w = acc[k][0].max(1e-9);
        let cw = cwsum[k].max(1e-9);
        [(acc[k][0] / n) as f32, (acc[k][1] / w) as f32, (acc[k][2] / cw) as f32, (acc[k][3] / cw) as f32]
    });
    let sw = skin[2].max(1e-9);
    Stats { bands, chroma: (chroma.0 / chroma.1.max(1e-9)) as f32, skin: [(skin[0] / sw) as f32, (skin[1] / sw) as f32, (skin[2] / n) as f32] }
}

/// Residuals between graded statistics and the reference's (bands weighted by how much of both
/// frames they cover).
fn residuals(cur: &Stats, refs: &Stats) -> Vec<f32> {
    let mut r = Vec::with_capacity(10);
    for k in 0..3 {
        let w = (cur.bands[k][0].min(refs.bands[k][0]) * 3.0).clamp(0.0, 1.0).sqrt();
        r.push((cur.bands[k][1] - refs.bands[k][1]) * w);
        r.push((cur.bands[k][2] - refs.bands[k][2]) * w * 2.0);
        r.push((cur.bands[k][3] - refs.bands[k][3]) * w * 2.0);
    }
    r.push((cur.chroma - refs.chroma) * 2.0);
    r
}

/// The solved grade.
#[derive(Clone, Debug, PartialEq)]
pub struct Match {
    pub shadows: [f32; 2],
    pub midtones: [f32; 2],
    pub highlights: [f32; 2],
    /// Lightness sliders (−100..100).
    pub lightness: [f32; 3],
    /// Basic Correction saturation (0..200).
    pub saturation: f32,
    /// Remaining residual norm (statistics distance) before and after.
    pub before: f32,
    pub after: f32,
}

// unknowns: [sx, sy, mx, my, hx, hy, sl, ml, hl, sat] (wheel offsets in −1..1, lightness/100, sat−1)
fn set_params(base: &EffectInstance, x: &[f32; 10]) -> EffectInstance {
    let mut e = base.clone();
    let mut put = |id: &str, v: ParamValue| {
        let prm = e.params.entry(id.to_string()).or_insert_with(|| filmcraft_project::Param::new(v.clone()));
        prm.value = v;
    };
    put("wheel_shadows", ParamValue::Vec2(Vec2::new(x[0] as f64, x[1] as f64)));
    put("wheel_midtones", ParamValue::Vec2(Vec2::new(x[2] as f64, x[3] as f64)));
    put("wheel_highlights", ParamValue::Vec2(Vec2::new(x[4] as f64, x[5] as f64)));
    put("wheel_shadows_l", ParamValue::Float(x[6] as f64 * 100.0));
    put("wheel_midtones_l", ParamValue::Float(x[7] as f64 * 100.0));
    put("wheel_highlights_l", ParamValue::Float(x[8] as f64 * 100.0));
    put("saturation", ParamValue::Float((1.0 + x[9] as f64) * 100.0));
    put("wheels_on", ParamValue::Bool(true));
    put("basic_on", ParamValue::Bool(true));
    e.enabled = true;
    e
}

fn graded_stats(img: &Image, base: &EffectInstance, x: &[f32; 10], skin_protect: bool) -> crate::Result<Stats> {
    let mut g = img.clone();
    let cx = FxCtx {
        t: Tick::ZERO,
        px_scale: g.w as f32 / 1920.0,
        seconds: 0.0,
        timecode: "",
        clip_name: "",
        project: None,
        env: None,
        working: filmcraft_color::WorkingSpace::Rec709,
    };
    apply(&mut g, &set_params(base, x), &cx)?;
    Ok(stats(&g, skin_protect))
}

/// Solve the grade that matches `current` (the clip before its wheels) to `reference`. `base` is
/// the clip's Lumetri instance (its other settings are kept; wheels and saturation are solved).
pub fn solve(current: &Image, reference: &Image, base: &EffectInstance, skin_protect: bool) -> crate::Result<Match> {
    let cur = shrink(current, 96);
    let refs = stats(&shrink(reference, 96), skin_protect);
    let mut x = [0f32; 10];
    let lambda_reg = 0.02f32;
    let cost = |x: &[f32; 10]| -> crate::Result<(Vec<f32>, f32)> {
        let r = residuals(&graded_stats(&cur, base, x, skin_protect)?, &refs);
        let c = r.iter().map(|v| v * v).sum::<f32>() + lambda_reg * lambda_reg * x.iter().map(|v| v * v).sum::<f32>();
        Ok((r, c))
    };
    let (mut r, mut c) = cost(&x)?;
    let before = c.sqrt();
    let mut mu = 1e-3f32;
    for _ in 0..12 {
        // numerical Jacobian (forward differences)
        let h = 1e-2f32;
        let mut jac = vec![[0f32; 10]; r.len()];
        for j in 0..10 {
            let mut xp = x;
            xp[j] += h;
            let (rp, _) = cost(&xp)?;
            for (i, row) in jac.iter_mut().enumerate() {
                row[j] = (rp[i] - r[i]) / h;
            }
        }
        // normal equations with Tikhonov regularisation and LM damping
        let mut a = [[0f64; 10]; 10];
        let mut g = [0f64; 10];
        for (i, row) in jac.iter().enumerate() {
            for p in 0..10 {
                g[p] += (row[p] * r[i]) as f64;
                for q in 0..10 {
                    a[p][q] += (row[p] * row[q]) as f64;
                }
            }
        }
        for p in 0..10 {
            g[p] += (lambda_reg * lambda_reg * x[p]) as f64;
            a[p][p] += (lambda_reg * lambda_reg) as f64 + mu as f64 * (1.0 + a[p][p]);
        }
        let Some(dx) = solve_linear(a, g.map(|v| -v)) else { break };
        let mut xn = x;
        for p in 0..10 {
            xn[p] = (x[p] + dx[p] as f32).clamp(-1.0, 1.0);
        }
        let (rn, cn) = cost(&xn)?;
        if cn < c {
            x = xn;
            r = rn;
            let improved = c - cn;
            c = cn;
            mu = (mu * 0.3).max(1e-6);
            if improved < 1e-7 {
                break;
            }
        } else {
            mu *= 10.0;
            if mu > 1e4 {
                break;
            }
        }
    }
    if skin_protect {
        // limit the hue shift of the current frame's skin tones to 8°
        let s0 = stats(&cur, true);
        if s0.skin[2] > 0.002 {
            let hue = |s: &Stats| s.skin[1].atan2(s.skin[0]).to_degrees();
            let h0 = hue(&s0);
            let mut k = 1.0f32;
            for _ in 0..8 {
                let xs: [f32; 10] = std::array::from_fn(|i| if i < 6 { x[i] * k } else { x[i] });
                let s1 = graded_stats(&cur, base, &xs, true)?;
                let mut d = (hue(&s1) - h0).abs();
                if d > 180.0 {
                    d = 360.0 - d;
                }
                if d <= 8.0 {
                    break;
                }
                k *= 0.7;
            }
            for v in x.iter_mut().take(6) {
                *v *= k;
            }
            c = cost(&x)?.1;
        }
    }
    Ok(Match {
        shadows: [x[0], x[1]],
        midtones: [x[2], x[3]],
        highlights: [x[4], x[5]],
        lightness: [x[6] * 100.0, x[7] * 100.0, x[8] * 100.0],
        saturation: ((1.0 + x[9]) * 100.0).clamp(0.0, 200.0),
        before,
        after: c.sqrt(),
    })
}

/// Gaussian elimination with partial pivoting.
fn solve_linear(mut a: [[f64; 10]; 10], mut b: [f64; 10]) -> Option<[f64; 10]> {
    for col in 0..10 {
        let piv = (col..10).max_by(|&i, &j| a[i][col].abs().total_cmp(&a[j][col].abs()))?;
        if a[piv][col].abs() < 1e-14 {
            return None;
        }
        a.swap(col, piv);
        b.swap(col, piv);
        for row in col + 1..10 {
            let f = a[row][col] / a[col][col];
            for k in col..10 {
                a[row][k] -= f * a[col][k];
            }
            b[row] -= f * b[col];
        }
    }
    let mut x = [0f64; 10];
    for i in (0..10).rev() {
        let s: f64 = (i + 1..10).map(|k| a[i][k] * x[k]).sum();
        x[i] = (b[i] - s) / a[i][i];
    }
    Some(x)
}

#[cfg(test)]
mod tests {
    use super::*;
    use filmcraft_project::find_effect;

    /// A synthetic frame: a lightness ramp with colour patches and a skin-tone block.
    fn frame(skin: bool) -> Image {
        let (w, h) = (192, 108);
        let mut img = Image::new(w, h);
        for y in 0..h {
            for x in 0..w {
                let t = x as f32 / w as f32;
                let mut c = [t * t, t * t * 0.9 + 0.02, t * 0.8 * t + 0.03];
                if y < h / 3 {
                    c = [c[0] * 0.6, c[1] * 1.1, c[2] * 0.7]; // greenish band
                } else if y > 2 * h / 3 {
                    c = [c[0] * 0.8, c[1] * 0.85, c[2] * 1.2]; // bluish band
                }
                if skin && (60..120).contains(&x) && (40..70).contains(&y) {
                    c = [0.55, 0.32, 0.22]; // linear skin tone
                }
                let i = (y * w + x) * 4;
                img.px[i..i + 4].copy_from_slice(&[c[0], c[1], c[2], 1.0]);
            }
        }
        img
    }

    fn graded(img: &Image, base: &EffectInstance, x: [f32; 10]) -> Image {
        let mut g = img.clone();
        let cx = FxCtx {
            t: Tick::ZERO,
            px_scale: 0.1,
            seconds: 0.0,
            timecode: "",
            clip_name: "",
            project: None,
            env: None,
            working: filmcraft_color::WorkingSpace::Rec709,
        };
        apply(&mut g, &set_params(base, &x), &cx).unwrap();
        g
    }

    #[test]
    fn oklab_reference_values() {
        // Ottosson's published table: white → (1, 0, 0); sRGB red → (0.6279, 0.2249, 0.1258)
        let w = oklab([1.0, 1.0, 1.0]);
        assert!((w[0] - 1.0).abs() < 1e-3 && w[1].abs() < 1e-3 && w[2].abs() < 1e-3, "{w:?}");
        let r = oklab([1.0, 0.0, 0.0]);
        assert!((r[0] - 0.6279).abs() < 2e-3 && (r[1] - 0.2249).abs() < 2e-3 && (r[2] - 0.1258).abs() < 2e-3, "{r:?}");
        let w = tonal_weights(0.1);
        assert!(w[0] > 0.99 && w[2] == 0.0);
        assert!(skin_weight(oklab([0.55, 0.32, 0.22])) > 0.9);
        assert_eq!(skin_weight(oklab([0.2, 0.3, 0.8])), 0.0);
    }

    #[test]
    fn matching_a_frame_to_itself_changes_nothing() {
        let base = find_effect("lumetri").unwrap().instance();
        let f = frame(false);
        let m = solve(&f, &f, &base, false).unwrap();
        assert!(m.before < 1e-4, "{m:?}");
        for v in m.shadows.iter().chain(&m.midtones).chain(&m.highlights) {
            assert!(v.abs() < 0.02, "{m:?}");
        }
        assert!(m.lightness.iter().all(|v| v.abs() < 2.0) && (m.saturation - 100.0).abs() < 2.0, "{m:?}");
    }

    #[test]
    fn recovers_a_known_grade() {
        let base = find_effect("lumetri").unwrap().instance();
        let f = frame(false);
        // reference = the frame with a warm-highlights / cool-shadows grade and lifted shadows
        let truth = [-0.3, -0.2, 0.0, 0.1, 0.35, 0.15, 0.1, 0.0, -0.05, 0.2];
        let reference = graded(&f, &base, truth);
        let m = solve(&f, &reference, &base, false).unwrap();
        eprintln!("known grade: {m:?}");
        assert!(m.after < m.before * 0.15, "{m:?}");
        // the matched render's statistics are close to the reference's
        let x = [
            m.shadows[0],
            m.shadows[1],
            m.midtones[0],
            m.midtones[1],
            m.highlights[0],
            m.highlights[1],
            m.lightness[0] / 100.0,
            m.lightness[1] / 100.0,
            m.lightness[2] / 100.0,
            m.saturation / 100.0 - 1.0,
        ];
        let out = stats(&graded(&f, &base, x), false);
        let want = stats(&reference, false);
        for k in 0..3 {
            for c in 1..4 {
                assert!((out.bands[k][c] - want.bands[k][c]).abs() < 0.02, "band {k} channel {c}: {:?} vs {:?}", out.bands[k], want.bands[k]);
            }
        }
    }

    #[test]
    fn skin_protection_limits_the_skin_hue_shift() {
        let base = find_effect("lumetri").unwrap().instance();
        let f = frame(true);
        // a strong green/teal cast reference
        let reference = graded(&f, &base, [-0.6, 0.5, -0.6, 0.5, -0.5, 0.4, 0.0, 0.0, 0.0, 0.0]);
        let hue = |img: &Image| {
            let s = stats(img, true);
            s.skin[1].atan2(s.skin[0]).to_degrees()
        };
        let h0 = hue(&f);
        let shift = |m: &Match| {
            let x = [
                m.shadows[0],
                m.shadows[1],
                m.midtones[0],
                m.midtones[1],
                m.highlights[0],
                m.highlights[1],
                m.lightness[0] / 100.0,
                m.lightness[1] / 100.0,
                m.lightness[2] / 100.0,
                m.saturation / 100.0 - 1.0,
            ];
            let d = (hue(&graded(&f, &base, x)) - h0).abs();
            if d > 180.0 { 360.0 - d } else { d }
        };
        let free = solve(&f, &reference, &base, false).unwrap();
        let protected = solve(&f, &reference, &base, true).unwrap();
        let (a, b) = (shift(&free), shift(&protected));
        eprintln!("skin hue shift: free {a:.1}°, protected {b:.1}°");
        assert!(b <= 8.5, "protected skin hue shift {b}° (unprotected {a}°)");
        assert!(a > b, "protection reduces the skin shift: {a}° vs {b}°");
    }
}
