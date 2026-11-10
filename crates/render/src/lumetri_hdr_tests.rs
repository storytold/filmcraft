//! Lumetri in HDR working spaces (known cd/m² values through PQ and HLG) and HSL Secondary ▸
//! Refine (Denoise, Blur of the key).

use filmcraft_color::{GradeSpace, REFERENCE_WHITE_NITS, WorkingSpace};
use filmcraft_project::{EffectInstance, ParamValue, find_effect};
use filmcraft_time::Tick;

use crate::Image;
use crate::effects::{FxCtx, apply};

const RW: f32 = REFERENCE_WHITE_NITS as f32;

fn cx(working: WorkingSpace) -> FxCtx<'static> {
    FxCtx { t: Tick::ZERO, px_scale: 1.0, seconds: 0.0, timecode: "", clip_name: "", project: None, env: None, frame_rate: 24.0, working }
}

fn lumetri(params: &[(&str, ParamValue)]) -> EffectInstance {
    let mut e = find_effect("lumetri").unwrap().instance();
    for (k, v) in params {
        e.params.get_mut(*k).unwrap_or_else(|| panic!("no param {k}")).value = v.clone();
    }
    e
}

/// A row of grey pixels at these cd/m², graded; returns their cd/m² afterwards.
fn grade(working: WorkingSpace, nits: &[f32], e: &EffectInstance) -> Vec<f32> {
    let mut img = Image::new(nits.len(), 1);
    for (i, n) in nits.iter().enumerate() {
        img.px[i * 4..i * 4 + 4].copy_from_slice(&[n / RW, n / RW, n / RW, 1.0]);
    }
    apply(&mut img, e, &cx(working));
    (0..nits.len()).map(|i| img.px[i * 4 + 1] * RW).collect()
}

fn close(got: &[f32], want: &[f32], rel: f32) {
    for (g, w) in got.iter().zip(want) {
        assert!((g - w).abs() <= w.abs() * rel + 0.02, "got {got:?}, want {want:?}");
    }
}

const F: fn(f64) -> ParamValue = ParamValue::Float;
const LEVELS: [f32; 6] = [1.0, 26.0, 100.0, 203.0, 1000.0, 4000.0];

#[test]
fn identity_keeps_hdr_highlights_in_pq_and_hlg() {
    let e = lumetri(&[]);
    close(&grade(WorkingSpace::Rec2100Pq, &LEVELS, &e), &LEVELS, 2e-3);
    // HLG signal tops out around 12× its peak's scene light: 4000 cd/m² is still inside
    close(&grade(WorkingSpace::Rec2100Hlg, &LEVELS, &e), &LEVELS, 2e-3);
    // the same grade in a Rec. 709 sequence clips at SDR white (1.0 = 203 cd/m² here)
    let sdr = grade(WorkingSpace::Rec709, &[1000.0], &e);
    assert!((sdr[0] - RW).abs() < 0.5, "{sdr:?}");
}

#[test]
fn exposure_is_stops_of_light_in_hdr() {
    let e = lumetri(&[("exposure", F(1.0))]);
    let want: Vec<f32> = LEVELS.iter().map(|n| n * 2.0).collect();
    close(&grade(WorkingSpace::Rec2100Pq, &LEVELS, &e), &want, 3e-3);
    close(&grade(WorkingSpace::Rec2100Hlg, &LEVELS[..5], &e), &want[..5], 3e-3);
    let e = lumetri(&[("exposure", F(-2.0))]);
    close(&grade(WorkingSpace::Rec2100Pq, &[400.0, 4000.0], &e), &[100.0, 1000.0], 3e-3);
}

#[test]
fn whites_and_contrast_scale_with_hdr_white() {
    // Whites +100 stretches the signal so 85 % of HDR White becomes HDR White
    for white in [1000.0f32, 4000.0] {
        let gs = GradeSpace::new(WorkingSpace::Rec2100Pq, white);
        let e = lumetri(&[("whites", F(100.0)), ("hdr_white", F(white as f64))]);
        let src = gs.nits_of_signal(0.85);
        let got = grade(WorkingSpace::Rec2100Pq, &[0.0, src], &e);
        assert!(got[0].abs() < 1e-3, "black stays black");
        assert!((got[1] - white).abs() < white * 3e-3, "HDR White {white}: {src} cd/m² → {} (want {white})", got[1]);
    }
    // Contrast pivots on the middle of the HDR White range and leaves speculars above it alone
    let gs = GradeSpace::new(WorkingSpace::Rec2100Pq, 1000.0);
    let (mid, dark, bright) = (gs.nits_of_signal(0.5), gs.nits_of_signal(0.3), gs.nits_of_signal(0.7));
    let e = lumetri(&[("contrast", F(60.0))]);
    let got = grade(WorkingSpace::Rec2100Pq, &[mid, dark, bright, 4000.0], &e);
    assert!((got[0] - mid).abs() < mid * 3e-3, "pivot {mid} → {}", got[0]);
    assert!(got[1] < dark && got[2] > bright, "{got:?}");
    assert!((got[3] - 4000.0).abs() < 4000.0 * 3e-3, "specular kept: {}", got[3]);
    // the same in HLG: reference white is 75 % of a 1000 cd/m² display's signal
    let hlg = GradeSpace::new(WorkingSpace::Rec2100Hlg, 1000.0);
    assert!((hlg.signal_of_nits(203.0) - 0.75).abs() < 2e-3);
    let e = lumetri(&[("whites", F(100.0))]);
    let got = grade(WorkingSpace::Rec2100Hlg, &[hlg.nits_of_signal(0.85)], &e);
    assert!((got[0] - 1000.0).abs() < 4.0, "{got:?}");
}

#[test]
fn hdr_specular_sets_highlights_above_hdr_white() {
    let e = lumetri(&[("hdr_specular", F(-100.0))]);
    let got = grade(WorkingSpace::Rec2100Pq, &[203.0, 1000.0, 4000.0], &e);
    close(&got, &[203.0, 1000.0, 1000.0], 3e-3);
    let gs = GradeSpace::new(WorkingSpace::Rec2100Pq, 1000.0);
    let e = lumetri(&[("hdr_specular", F(50.0))]);
    let got = grade(WorkingSpace::Rec2100Pq, &[1000.0, 4000.0], &e);
    let want = gs.nits_of_signal(1.0 + (gs.signal_of_nits(4000.0) - 1.0) * 1.5);
    close(&got, &[1000.0, want], 3e-3);
    // no effect in SDR (the slider is HDR-only)
    let sdr = grade(WorkingSpace::Rec709, &[150.0], &lumetri(&[("hdr_specular", F(-100.0))]));
    assert!((sdr[0] - 150.0).abs() < 0.2);
}

#[test]
fn curves_span_the_hdr_range() {
    // a luma curve lifting the middle: 0.5 → 0.6 of the HDR Range
    let curve = ParamValue::Curve(vec![[0.0, 0.0], [0.5, 0.6], [1.0, 1.0]]);
    for range in [1000.0f32, 2000.0] {
        let gs = GradeSpace::new(WorkingSpace::Rec2100Pq, range);
        let e = lumetri(&[("curve_luma", curve.clone()), ("curves_hdr_range", F(range as f64))]);
        let got = grade(WorkingSpace::Rec2100Pq, &[gs.nits_of_signal(0.5), range, 6000.0], &e);
        close(&got, &[gs.nits_of_signal(0.6), range, 6000.0], 4e-3);
    }
}

/// A 40×40 image: grey background, a red 16×16 square, and isolated red speckles.
fn keyed_image() -> Image {
    let mut img = Image::filled(40, 40, [0.2, 0.2, 0.2, 1.0]);
    let red = [0.8, 0.02, 0.02, 1.0];
    for y in 12..28 {
        for x in 12..28 {
            img.px[(y * 40 + x) * 4..][..4].copy_from_slice(&red);
        }
    }
    for (x, y) in [(3, 3), (35, 6), (5, 33), (33, 34), (20, 4)] {
        img.px[(y * 40 + x) * 4..][..4].copy_from_slice(&red);
    }
    img
}

fn key(denoise: f64, blur: f64) -> Image {
    let mut img = keyed_image();
    let e = lumetri(&[
        ("hsl_on", ParamValue::Bool(true)),
        ("hsl_hue", F(0.0)),
        ("hsl_hue_range", F(40.0)),
        ("hsl_show_mask", ParamValue::Choice(3)),
        ("hsl_denoise", F(denoise)),
        ("hsl_blur", F(blur)),
    ]);
    apply(&mut img, &e, &cx(WorkingSpace::Rec709));
    img
}

#[test]
fn hsl_refine_denoise_removes_speckles() {
    let m = |img: &Image, x: usize, y: usize| filmcraft_color::linear_to_srgb(img.get(x, y)[1]);
    let raw = key(0.0, 0.0);
    assert!(m(&raw, 3, 3) > 0.9 && m(&raw, 20, 20) > 0.9 && m(&raw, 8, 20) < 0.01, "the key: speckles and square");
    let clean = key(100.0, 0.0);
    for (x, y) in [(3, 3), (35, 6), (5, 33), (33, 34), (20, 4)] {
        assert!(m(&clean, x, y) < 0.05, "speckle at {x},{y}: {}", m(&clean, x, y));
    }
    assert!(m(&clean, 20, 20) > 0.95, "the square stays keyed");
    // half denoise: half way
    let half = key(50.0, 0.0);
    assert!((0.3..0.7).contains(&m(&half, 3, 3)), "{}", m(&half, 3, 3));
}

#[test]
fn hsl_refine_blur_softens_the_key() {
    let soft = |img: &Image| (0..40).filter(|&x| (0.05..0.95).contains(&img.get(x, 20)[1])).count();
    let hard = key(0.0, 0.0);
    assert_eq!(soft(&hard), 0, "a hard key");
    let blurred = key(0.0, 25.0);
    let n = soft(&blurred);
    assert!(n >= 6, "soft edge pixels: {n}");
    // the key's total is kept (blur only moves it around)
    let total = |img: &Image| (0..40 * 40).map(|i| filmcraft_color::linear_to_srgb(img.px[i * 4 + 1])).sum::<f32>();
    assert!((total(&blurred) - total(&hard)).abs() / total(&hard) < 0.05);
    // the correction follows the refined key: a hue shift inside, soft at the edge
    let mut img = keyed_image();
    let e =
        lumetri(&[("hsl_on", ParamValue::Bool(true)), ("hsl_hue", F(0.0)), ("hsl_hue_range", F(40.0)), ("hsl_hue_shift", F(120.0)), ("hsl_denoise", F(100.0))]);
    apply(&mut img, &e, &cx(WorkingSpace::Rec709));
    let inside = img.get(20, 20);
    assert!(inside[1] > inside[0], "red turned green inside: {inside:?}");
    let speck = img.get(3, 3);
    assert!(speck[0] > speck[1], "speckle left alone after denoise: {speck:?}");
}

/// 40×20: a textured, mid-grey-ish red left half (keyed by hue 0) and the same texture in blue on
/// the right.
fn textured_red_blue() -> Image {
    let mut img = Image::new(40, 20);
    for y in 0..20 {
        for x in 0..40 {
            let t = if (x + y) % 2 == 0 { 0.08 } else { -0.08 };
            let c = if x < 20 { [0.62 + t, 0.42 + t, 0.38 + t] } else { [0.2 + t, 0.3 + t, 0.75 + t] };
            let l = c.map(filmcraft_color::srgb_to_linear);
            img.px[(y * 40 + x) * 4..][..4].copy_from_slice(&[l[0], l[1], l[2], 1.0]);
        }
    }
    img
}

/// Spread (standard deviation, display units) of the green channel over the inner 12×12 of a half.
fn spread(img: &Image, x0: usize) -> f32 {
    let v: Vec<f32> = (4..16).flat_map(|y| (x0 + 4..x0 + 16).map(move |x| (x, y))).map(|(x, y)| filmcraft_color::linear_to_srgb(img.get(x, y)[1])).collect();
    let m = v.iter().sum::<f32>() / v.len() as f32;
    (v.iter().map(|q| (q - m) * (q - m)).sum::<f32>() / v.len() as f32).sqrt()
}

/// [`textured_red_blue`] through Lumetri with HSL Secondary keying the red half (hard key) and
/// these float parameters.
fn hsl_correct(params: &[(&str, f64)]) -> Image {
    let mut img = textured_red_blue();
    let mut p = vec![("hsl_on", ParamValue::Bool(true)), ("hsl_hue", F(0.0)), ("hsl_hue_range", F(40.0)), ("hsl_soft", F(0.0))];
    p.extend(params.iter().map(|(k, v)| (*k, F(*v))));
    apply(&mut img, &lumetri(&p), &cx(WorkingSpace::Rec709));
    img
}

/// HSL Secondary ▸ Correction ▸ Contrast and Sharpen change the keyed colour only: Contrast
/// spreads or flattens its tones, negative Sharpen softens its texture and positive sharpens it.
#[test]
fn hsl_contrast_and_sharpen_act_inside_the_key() {
    let src = textured_red_blue();
    let plain = hsl_correct(&[]);
    for (x, y) in [(5, 5), (30, 5)] {
        let (a, b) = (plain.get(x, y), src.get(x, y));
        assert!((0..3).all(|k| (a[k] - b[k]).abs() < 1e-5), "neutral correction at {x},{y}: {a:?} vs {b:?}");
    }
    let base = spread(&plain, 0);
    for (params, grows) in [
        (vec![("hsl_contrast", 100.0)], true),
        (vec![("hsl_contrast", -100.0)], false),
        (vec![("hsl_sharpen", 100.0)], true),
        (vec![("hsl_sharpen", -100.0)], false),
    ] {
        let img = hsl_correct(&params);
        let s = spread(&img, 0);
        if grows {
            assert!(s > base * 1.2, "{params:?}: keyed texture {base} -> {s}");
        } else {
            assert!(s < base * 0.8, "{params:?}: keyed texture {base} -> {s}");
        }
        // blue is outside the key: untouched, right up to the key's edge (the hard key is 0 there,
        // so Sharpen's blur of the red next to it is mixed in by nothing)
        let blue = (20..40)
            .flat_map(|x| (0..20).map(move |y| (x, y)))
            .map(|(x, y)| (0..3).map(|k| (img.get(x, y)[k] - src.get(x, y)[k]).abs()).fold(0.0, f32::max))
            .fold(0.0, f32::max);
        assert!(blue < 1e-5, "{params:?}: blue changed by {blue}");
    }
    // Show Mask views stay the key, without the correction
    let mut shown = textured_red_blue();
    let e = lumetri(&[
        ("hsl_on", ParamValue::Bool(true)),
        ("hsl_hue", F(0.0)),
        ("hsl_hue_range", F(40.0)),
        ("hsl_soft", F(0.0)),
        ("hsl_sharpen", F(-100.0)),
        ("hsl_contrast", F(100.0)),
        ("hsl_show_mask", ParamValue::Choice(3)),
    ]);
    apply(&mut shown, &e, &cx(WorkingSpace::Rec709));
    assert!(shown.get(5, 5)[1] > 0.9 && shown.get(30, 5)[1] < 0.01);
}

/// Hostile Contrast and Sharpen values: no panic, finite output; a value that is not a number
/// is neutral (0) and an out-of-range one is clamped to ±100.
#[test]
fn hsl_contrast_and_sharpen_survive_hostile_values() {
    let neutral = hsl_correct(&[]);
    for k in ["hsl_contrast", "hsl_sharpen"] {
        for (v, want) in [(f64::NAN, None), (f64::INFINITY, None), (f64::NEG_INFINITY, None), (1e30, Some(100.0)), (-1e30, Some(-100.0))] {
            let img = hsl_correct(&[(k, v)]);
            assert!(img.px.iter().all(|q| q.is_finite()), "{k}={v}");
            let want = want.map_or_else(|| neutral.clone(), |w| hsl_correct(&[(k, w)]));
            assert_eq!(img.px, want.px, "{k}={v}");
        }
    }
}

/// HDR: positive Contrast leaves keyed values that Temperature lifted above 1 where they are (as
/// Basic Correction leaves highlights above HDR White) instead of clipping them to 1; negative
/// Contrast lowers them with the top of its curve but keeps them above it.
#[test]
fn hsl_contrast_keeps_values_above_one() {
    let red = |contrast: f64| {
        let mut img = Image::new(1, 1);
        // a bright red highlight, about 400 cd/m², at the top of its pixel's normalised signal
        img.px.copy_from_slice(&[2.0, 0.9, 0.8, 1.0]);
        let e = lumetri(&[
            ("hsl_on", ParamValue::Bool(true)),
            ("hsl_hue", F(0.0)),
            ("hsl_hue_range", F(40.0)),
            ("hsl_soft", F(0.0)),
            ("hsl_temp", F(100.0)),
            ("hsl_contrast", F(contrast)),
        ]);
        apply(&mut img, &e, &cx(WorkingSpace::Rec2100Pq));
        img.px[0]
    };
    let warm = red(0.0);
    assert!(warm > 2.0 * 1.05, "Temperature warms the red highlight: {warm}");
    for c in [50.0, 100.0] {
        assert!((red(c) - warm).abs() <= warm * 1e-4, "contrast {c}: {} vs {warm}", red(c));
    }
    // negative: lowered, but still above a keyed pixel that ends exactly at the curve's top
    let (soft, top) = (red(-50.0), {
        let mut img = Image::new(1, 1);
        img.px.copy_from_slice(&[1.0, 0.45, 0.4, 1.0]);
        let e =
            lumetri(&[("hsl_on", ParamValue::Bool(true)), ("hsl_hue", F(0.0)), ("hsl_hue_range", F(40.0)), ("hsl_soft", F(0.0)), ("hsl_contrast", F(-50.0))]);
        apply(&mut img, &e, &cx(WorkingSpace::Rec2100Pq));
        img.px[0]
    });
    assert!(soft < warm && soft > top, "contrast -50: {soft}, between {top} and {warm}");
}

/// HDR: across a gradient that Temperature lifts through 1, negative Contrast stays continuous
/// (no edge where values cross 1) and keeps the detail above it.
#[test]
fn hsl_negative_contrast_is_continuous_across_one() {
    let n = 64;
    let mut img = Image::new(n, 1);
    for i in 0..n {
        let r = 1.0 + 1.6 * i as f32 / (n - 1) as f32;
        img.px[i * 4..i * 4 + 4].copy_from_slice(&[r, 0.45 * r, 0.4 * r, 1.0]);
    }
    let e = lumetri(&[
        ("hsl_on", ParamValue::Bool(true)),
        ("hsl_hue", F(0.0)),
        ("hsl_hue_range", F(40.0)),
        ("hsl_soft", F(0.0)),
        ("hsl_temp", F(100.0)),
        ("hsl_contrast", F(-100.0)),
    ]);
    apply(&mut img, &e, &cx(WorkingSpace::Rec2100Pq));
    let red: Vec<f32> = (0..n).map(|i| img.px[i * 4]).collect();
    let steps: Vec<f32> = red.windows(2).map(|w| w[1] - w[0]).collect();
    let mean = steps.iter().sum::<f32>() / steps.len() as f32;
    assert!(mean > 0.0, "the gradient keeps rising: {red:?}");
    for (i, d) in steps.iter().enumerate() {
        assert!(*d >= -1e-4 && *d <= 4.0 * mean, "step {i}: {d} (mean {mean}): {red:?}");
    }
}

/// Contrast and Sharpen leave fully transparent pixels empty, even where Refine Blur spreads the
/// key onto them (negative Contrast would otherwise turn their black into mid grey).
#[test]
fn hsl_sharpen_leaves_transparent_pixels_empty() {
    for (k, v) in [("hsl_sharpen", -100.0), ("hsl_sharpen", 100.0), ("hsl_contrast", -100.0), ("hsl_contrast", 100.0)] {
        let mut img = textured_red_blue();
        for y in 0..20 {
            for x in 8..12 {
                img.px[(y * 40 + x) * 4..][..4].copy_from_slice(&[0.0; 4]);
            }
        }
        let e = lumetri(&[("hsl_on", ParamValue::Bool(true)), ("hsl_hue", F(0.0)), ("hsl_hue_range", F(40.0)), ("hsl_blur", F(100.0)), (k, F(v))]);
        apply(&mut img, &e, &cx(WorkingSpace::Rec709));
        for y in 0..20 {
            for x in 8..12 {
                assert_eq!(img.get(x, y), [0.0; 4], "{k} {v} at {x},{y}");
            }
        }
    }
}

/// A soft key mixes Sharpen in by its value: where Refine Blur leaves the key partial (near the
/// red half's edge) a pixel changes by that fraction of what a full key changes it by.
#[test]
fn hsl_sharpen_mixes_by_a_partial_key() {
    let base = [("hsl_on", ParamValue::Bool(true)), ("hsl_hue", F(0.0)), ("hsl_hue_range", F(40.0)), ("hsl_soft", F(0.0))];
    let run = |extra: &[(&str, ParamValue)]| {
        let mut img = textured_red_blue();
        let mut p = base.to_vec();
        p.extend_from_slice(extra);
        apply(&mut img, &lumetri(&p), &cx(WorkingSpace::Rec709));
        img
    };
    let src = run(&[]);
    let hard = run(&[("hsl_sharpen", F(100.0))]);
    // the blurred key, read through Show Mask ▸ Mask (an sRGB grey of the key's value)
    let key = run(&[("hsl_blur", F(10.0)), ("hsl_show_mask", ParamValue::Choice(3))]);
    let soft_plain = run(&[("hsl_blur", F(10.0))]);
    let soft = run(&[("hsl_blur", F(10.0)), ("hsl_sharpen", F(100.0))]);
    let mut partial = 0;
    for y in 2..18 {
        for x in 12..20 {
            let m = filmcraft_color::linear_to_srgb(key.get(x, y)[1]);
            let (full, got) = (hard.get(x, y)[1] - src.get(x, y)[1], soft.get(x, y)[1] - soft_plain.get(x, y)[1]);
            if m > 0.05 && m < 0.95 && full.abs() > 1e-3 {
                partial += 1;
                assert!((got - m * full).abs() <= 1e-3 + full.abs() * 0.02, "{x},{y}: key {m}, full change {full}, got {got}");
            }
        }
    }
    assert!(partial > 8, "the blurred key is partial near the edge ({partial} pixels)");
}

/// Sharpen works on straight colour: a keyed red of one colour whose alpha ramps down (a soft edge)
/// keeps its colour, with no bright or dark halo along the ramp.
#[test]
fn hsl_sharpen_puts_no_halo_on_a_soft_alpha_edge() {
    let (w, h) = (24usize, 6usize);
    let l = [0.62f32, 0.42, 0.38].map(filmcraft_color::srgb_to_linear);
    for amount in [100.0, -100.0] {
        let mut img = Image::new(w, h);
        for y in 0..h {
            for x in 0..w {
                let a = (1.0 - x as f32 / (w - 1) as f32 * 0.8).clamp(0.2, 1.0);
                img.px[(y * w + x) * 4..][..4].copy_from_slice(&[l[0] * a, l[1] * a, l[2] * a, a]);
            }
        }
        let e =
            lumetri(&[("hsl_on", ParamValue::Bool(true)), ("hsl_hue", F(0.0)), ("hsl_hue_range", F(40.0)), ("hsl_soft", F(0.0)), ("hsl_sharpen", F(amount))]);
        apply(&mut img, &e, &cx(WorkingSpace::Rec709));
        for x in 0..w {
            let p = img.get(x, 3);
            for k in 0..3 {
                assert!((p[k] / p[3] - l[k]).abs() < 1e-4, "sharpen {amount} at x {x}: straight {:?} vs {l:?}", [p[0] / p[3], p[1] / p[3], p[2] / p[3]]);
            }
        }
    }
}

/// Sharpen and soften change keyed texture at full and at quarter-resolution playback (where the
/// blur's σ is under half a pixel).
#[test]
fn hsl_sharpen_works_at_full_and_quarter_resolution() {
    for scale in [1.0f32, 0.25] {
        let run = |amount: f64| {
            let mut img = textured_red_blue();
            let e = lumetri(&[
                ("hsl_on", ParamValue::Bool(true)),
                ("hsl_hue", F(0.0)),
                ("hsl_hue_range", F(40.0)),
                ("hsl_soft", F(0.0)),
                ("hsl_sharpen", F(amount)),
            ]);
            apply(&mut img, &e, &FxCtx { px_scale: scale, ..cx(WorkingSpace::Rec709) });
            spread(&img, 0)
        };
        let base = run(0.0);
        assert!(run(100.0) > base * 1.2, "scale {scale}: sharpen {} vs {base}", run(100.0));
        assert!(run(-100.0) < base * 0.8, "scale {scale}: soften {} vs {base}", run(-100.0));
    }
}
