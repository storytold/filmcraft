//! Lumetri ▸ Basic Correction ▸ **Auto**: a one-click starting grade for a shot, solved in
//! Lumetri's own Basic Correction sliders so the result is an ordinary, editable grade.
//!
//! The method is deliberately simple and documented, no ML:
//!
//! 1. The frame (as Lumetri receives it, after the Input LUT) is downsampled to ≤ 192 px wide.
//! 2. **White balance** (optional): grey world over the *near-neutral* pixels only (saturation
//!    (max − min) / max below 0.25 in linear light, luminance 2 … 90 %, not clipped) — the
//!    surfaces that should be grey. A shot
//!    where fewer than 10 % of the pixels are near-neutral (a sunset, an aurora, neon) is a
//!    coloured scene, not a cast, and keeps its balance. Temperature evens out red and blue, Tint
//!    brings green to their mean, both through Lumetri's white-balance gains, capped at ±40.
//! 3. **Exposure** moves the log-average luminance towards 18 % grey, at 50 % strength and within
//!    ±1.5 stops, so a low-key shot is lifted but not turned into a noon scene.
//! 4. **Whites / Blacks** stretch the tonal range: the 0.5 % and 99.5 % luminance percentiles of
//!    the exposed frame (in the grading signal) go to 2 % and 98 %, each slider within ±50. They
//!    only *expand* a flat picture — a frame that already spans black to white is not
//!    compressed. In HDR working spaces Whites is left alone (highlights above HDR White are
//!    speculars, not a range to fill).
//!
//! Contrast, Highlights and Shadows are reset to 0, as Auto is a fresh start; Saturation, the
//! other sections and the Input LUT are kept.

use filmcraft_color::{GradeSpace, WorkingSpace};
use filmcraft_project::{EffectInstance, Project};
use filmcraft_time::Tick;

use crate::Image;
use crate::color_match::shrink;

/// The solved Basic Correction values, in Lumetri's slider units.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct AutoTone {
    pub temperature: f32,
    pub tint: f32,
    /// Stops.
    pub exposure: f32,
    pub whites: f32,
    pub blacks: f32,
}

/// The largest white-balance correction Auto applies (slider units).
pub const MAX_WB: f32 = 40.0;
/// How much of the way to 18 % grey Auto moves the exposure.
pub const EXPOSURE_STRENGTH: f32 = 0.5;
/// The largest exposure change Auto applies (stops).
pub const MAX_EXPOSURE: f32 = 1.5;
/// The largest Whites / Blacks Auto applies (slider units).
pub const MAX_RANGE: f32 = 50.0;
/// Pixels below this saturation ((max − min) / max, linear) count as near-neutral for the white
/// balance.
const NEUTRAL_SAT: f32 = 0.25;

const LUMA: [f32; 3] = [0.2126, 0.7152, 0.0722];

fn luma(c: [f32; 3]) -> f32 {
    LUMA[0] * c[0] + LUMA[1] * c[1] + LUMA[2] * c[2]
}

fn percentile(sorted: &[f32], q: f32) -> f32 {
    let i = ((sorted.len() - 1) as f32 * q).round() as usize;
    sorted[i.min(sorted.len() - 1)]
}

/// Solve Auto for `img` (the clip's picture before its Lumetri, working linear light). `base` is
/// the clip's Lumetri instance (Input LUT and HDR White are read from it).
pub fn solve(img: &Image, base: &EffectInstance, working: WorkingSpace, project: Option<&Project>, white_balance: bool) -> Option<AutoTone> {
    let img = shrink(img, 192);
    let white = if base.param("hdr_white").is_some() { base.f64_at("hdr_white", Tick::ZERO) as f32 } else { filmcraft_color::grade::DEFAULT_HDR_WHITE_NITS };
    let gs = GradeSpace::new(working, white);
    let hdr = gs.is_hdr();
    let lut = crate::luts::resolve(project, crate::effects::text(base, "input_lut"));
    let px: Vec<[f32; 3]> = img
        .px
        .as_chunks::<4>()
        .0
        .iter()
        .filter(|p| p[3] > 1e-3)
        .map(|p| {
            let c = [p[0] / p[3], p[1] / p[3], p[2] / p[3]];
            match &lut {
                Some(l) if hdr => {
                    let v = gs.encode(c);
                    let m = v[0].max(v[1]).max(v[2]).max(1.0);
                    gs.decode(l.apply(v.map(|q| (q / m).max(0.0))).map(|q| q * m))
                }
                Some(l) => gs.decode(l.apply(gs.encode(c).map(|q| q.clamp(0.0, 1.0)))),
                None => c,
            }
        })
        .collect();
    // too little picture to judge: no correction at all, rather than a "correction" of zeros
    if px.len() < 16 {
        return None;
    }
    let mut out = AutoTone::default();

    // 1. white balance: grey world over pixels that are neither clipped nor near black
    let mut gains = [1.0f32; 3];
    if white_balance {
        let peak = if hdr { f32::INFINITY } else { 0.95 };
        let (mut s, mut n) = ([0f64; 3], 0usize);
        for c in &px {
            let (mx, mn) = (c[0].max(c[1]).max(c[2]), c[0].min(c[1]).min(c[2]));
            let neutral = mx > 1e-4 && (mx - mn) / mx < NEUTRAL_SAT && (0.02..=0.9).contains(&luma(*c));
            if neutral && mx < peak {
                for k in 0..3 {
                    s[k] += c[k] as f64;
                }
                n += 1;
            }
        }
        if n >= 16 && n * 10 >= px.len() {
            let [r, g, b] = s.map(|v| (v / n as f64) as f32);
            if r > 1e-4 && g > 1e-4 && b > 1e-4 {
                // gains [1 + 0.35 t, 1 − 0.3 ti, 1 − 0.35 t] (t, ti = slider / 100)
                let t = ((b - r) / (0.35 * (r + b))).clamp(-MAX_WB / 100.0, MAX_WB / 100.0);
                let m = 0.5 * (r * (1.0 + 0.35 * t) + b * (1.0 - 0.35 * t));
                let ti = ((1.0 - m / g) / 0.3).clamp(-MAX_WB / 100.0, MAX_WB / 100.0);
                out.temperature = t * 100.0;
                out.tint = ti * 100.0;
                gains = [1.0 + 0.35 * t, 1.0 - 0.3 * ti, 1.0 - 0.35 * t];
            }
        }
    }

    // 2. exposure: the log-average luminance towards 18 % grey
    let (mut lsum, mut ln) = (0f64, 0usize);
    for c in &px {
        let l = luma([c[0] * gains[0], c[1] * gains[1], c[2] * gains[2]]);
        if l > 1e-4 {
            lsum += (l as f64).ln();
            ln += 1;
        }
    }
    if ln > 0 {
        let lavg = (lsum / ln as f64).exp() as f32;
        out.exposure = ((0.18 / lavg).log2() * EXPOSURE_STRENGTH).clamp(-MAX_EXPOSURE, MAX_EXPOSURE);
    }
    let k = 2f32.powf(out.exposure);

    // 3. whites / blacks: stretch the 0.5 % … 99.5 % range of the grading signal to 2 % … 98 %
    let mut ls: Vec<f32> = px.iter().map(|c| luma(gs.encode([c[0] * gains[0] * k, c[1] * gains[1] * k, c[2] * gains[2] * k]))).collect();
    ls.sort_by(f32::total_cmp);
    let (lo, hi) = (percentile(&ls, 0.005), percentile(&ls, 0.995).min(1.0));
    if hi - lo > 0.02 {
        let (lo_t, hi_t) = (0.02, 0.98);
        let r = (hi - lo) / (hi_t - lo_t);
        // Lumetri maps v → (v − b0) / (w0 − b0) with b0 = −blacks·0.15, w0 = 1 − whites·0.15
        let span = 0.15 * MAX_RANGE / 100.0;
        let b0 = (lo - lo_t * r).clamp(0.0, span);
        let w0 = if hdr { 1.0 } else { (b0 + r).clamp(1.0 - span, 1.0) };
        out.blacks = -b0 / 0.15 * 100.0;
        out.whites = (1.0 - w0) / 0.15 * 100.0;
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::effects::{FxCtx, apply};
    use filmcraft_project::ParamValue;

    fn lumetri() -> EffectInstance {
        filmcraft_project::find_effect("lumetri").expect("lumetri").instance()
    }

    fn graded(img: &Image, a: &AutoTone) -> Image {
        let mut e = lumetri();
        for (k, v) in [("temperature", a.temperature), ("tint", a.tint), ("exposure", a.exposure), ("whites", a.whites), ("blacks", a.blacks)] {
            e.params.get_mut(k).expect(k).value = ParamValue::Float(v as f64);
        }
        let mut g = img.clone();
        let cx = FxCtx {
            t: Tick::ZERO,
            px_scale: 1.0,
            seconds: 0.0,
            timecode: "",
            clip_name: "",
            project: None,
            env: None,
            frame_rate: 24.0,
            working: WorkingSpace::Rec709,
        };
        apply(&mut g, &e, &cx);
        g
    }

    /// A dim, flat, blue-cast horizontal ramp (working linear light).
    fn dull_blue_ramp() -> Image {
        let (w, h) = (128, 32);
        let mut img = Image::new(w, h);
        for y in 0..h {
            for x in 0..w {
                // sRGB 0.25 … 0.55, then darkened and tinted blue
                let v = filmcraft_color::srgb_to_linear(0.25 + 0.3 * x as f32 / (w - 1) as f32) * 0.5;
                let i = (y * w + x) * 4;
                img.px[i..i + 4].copy_from_slice(&[v * 0.9, v, v * 1.15, 1.0]);
            }
        }
        img
    }

    fn means(img: &Image) -> [f32; 3] {
        let mut s = [0f64; 3];
        for p in img.px.as_chunks::<4>().0 {
            for k in 0..3 {
                s[k] += p[k] as f64;
            }
        }
        s.map(|v| (v / (img.w * img.h) as f64) as f32)
    }

    fn log_avg(img: &Image) -> f32 {
        let mut s = 0f64;
        for p in img.px.as_chunks::<4>().0 {
            s += (luma([p[0], p[1], p[2]]).max(1e-4) as f64).ln();
        }
        (s / (img.w * img.h) as f64).exp() as f32
    }

    fn signal_range(img: &Image) -> (f32, f32) {
        let mut ls: Vec<f32> = img.px.as_chunks::<4>().0.iter().map(|p| filmcraft_color::linear_to_srgb(luma([p[0], p[1], p[2]]))).collect();
        ls.sort_by(f32::total_cmp);
        (percentile(&ls, 0.005), percentile(&ls, 0.995))
    }

    #[test]
    fn auto_neutralises_the_cast_lifts_the_exposure_and_stretches_the_range() {
        let img = dull_blue_ramp();
        let a = solve(&img, &lumetri(), WorkingSpace::Rec709, None, true).unwrap();
        assert!(a.temperature > 5.0, "a blue cast is warmed: {a:?}");
        assert!(a.exposure > 0.3, "a dim shot is brightened: {a:?}");
        assert!(a.blacks < 0.0 && a.whites > 0.0, "a flat shot is stretched: {a:?}");
        let g = graded(&img, &a);
        let [r0, _, b0] = means(&img);
        let [r1, _, b1] = means(&g);
        assert!((b1 / r1 - 1.0).abs() < (b0 / r0 - 1.0).abs() * 0.5, "blue/red {} → {}", b0 / r0, b1 / r1);
        assert!((log_avg(&g) / 0.18).log2().abs() < (log_avg(&img) / 0.18).log2().abs(), "closer to 18 % grey");
        let (lo0, hi0) = signal_range(&img);
        let (lo1, hi1) = signal_range(&g);
        assert!(hi1 - lo1 > (hi0 - lo0) * 1.2, "range {lo0}..{hi0} → {lo1}..{hi1}");
    }

    #[test]
    fn white_balance_can_be_left_alone() {
        let a = solve(&dull_blue_ramp(), &lumetri(), WorkingSpace::Rec709, None, false).unwrap();
        assert_eq!((a.temperature, a.tint), (0.0, 0.0));
        assert!(a.exposure > 0.3);
    }

    #[test]
    fn a_strong_cast_is_only_partly_removed() {
        let mut img = dull_blue_ramp();
        for p in img.px.as_chunks_mut::<4>().0 {
            p[0] *= 0.2;
        }
        let a = solve(&img, &lumetri(), WorkingSpace::Rec709, None, true).unwrap();
        assert!(a.temperature.abs() <= MAX_WB && a.tint.abs() <= MAX_WB, "{a:?}");
    }

    #[test]
    fn a_coloured_scene_keeps_its_balance() {
        // a saturated orange sunset gradient: nothing in it is meant to be grey
        let (w, h) = (128, 16);
        let mut img = Image::new(w, h);
        for y in 0..h {
            for x in 0..w {
                let v = 0.05 + 0.6 * x as f32 / (w - 1) as f32;
                let i = (y * w + x) * 4;
                img.px[i..i + 4].copy_from_slice(&[v, v * 0.35, v * 0.08, 1.0]);
            }
        }
        let a = solve(&img, &lumetri(), WorkingSpace::Rec709, None, true).unwrap();
        assert_eq!((a.temperature, a.tint), (0.0, 0.0), "{a:?}");
    }

    #[test]
    fn a_full_range_neutral_frame_is_not_compressed() {
        let (w, h) = (128, 8);
        let mut img = Image::new(w, h);
        for y in 0..h {
            for x in 0..w {
                let v = filmcraft_color::srgb_to_linear(x as f32 / (w - 1) as f32);
                let i = (y * w + x) * 4;
                img.px[i..i + 4].copy_from_slice(&[v, v, v, 1.0]);
            }
        }
        let a = solve(&img, &lumetri(), WorkingSpace::Rec709, None, true).unwrap();
        assert!(a.blacks <= 0.0 && a.whites >= 0.0, "never compresses: {a:?}");
        assert!(a.temperature.abs() < 1.0 && a.tint.abs() < 1.0, "a neutral frame keeps its balance: {a:?}");
    }

    /// #2378 review: too few pixels to judge give no correction, not a "correction" of zeros.
    #[test]
    fn too_little_picture_solves_nothing() {
        let img = Image::new(3, 3);
        assert!(solve(&img, &lumetri(), WorkingSpace::Rec709, None, true).is_none());
    }
}
