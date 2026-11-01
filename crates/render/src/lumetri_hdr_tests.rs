//! Lumetri in HDR working spaces (known cd/m² values through PQ and HLG), HSL Secondary ▸
//! Refine (Denoise, Blur of the key), Invert Mask and the key eyedroppers (Set / Add / Remove color).

use filmcraft_color::{GradeSpace, REFERENCE_WHITE_NITS, WorkingSpace};
use filmcraft_project::{EffectInstance, ParamValue, find_effect};
use filmcraft_time::Tick;

use crate::Image;
use crate::effects::{FxCtx, HslKeyPick, apply, lumetri_hsl_key_pick};

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

#[test]
fn hsl_invert_mask_flips_the_key() {
    let m = |img: &Image, x: usize, y: usize| filmcraft_color::linear_to_srgb(img.get(x, y)[1]);
    let mut img = keyed_image();
    let e = lumetri(&[
        ("hsl_on", ParamValue::Bool(true)),
        ("hsl_hue", F(0.0)),
        ("hsl_hue_range", F(40.0)),
        ("hsl_show_mask", ParamValue::Choice(3)),
        ("hsl_invert", ParamValue::Bool(true)),
    ]);
    apply(&mut img, &e, &cx(WorkingSpace::Rec709));
    assert!(m(&img, 20, 20) < 0.01 && m(&img, 8, 20) > 0.99, "the red square is out, the grey is in");
    // the correction follows the inverted key: the grey around is warmed, the red square left alone
    let mut img = keyed_image();
    let e = lumetri(&[("hsl_on", ParamValue::Bool(true)), ("hsl_hue_range", F(40.0)), ("hsl_invert", ParamValue::Bool(true)), ("hsl_temp", F(100.0))]);
    apply(&mut img, &e, &cx(WorkingSpace::Rec709));
    let (grey, red) = (img.get(8, 20), img.get(20, 20));
    assert!(grey[0] > grey[2] + 0.01, "the grey is warmed: {grey:?}");
    assert!((red[0] - 0.8).abs() < 1e-3 && (red[1] - 0.02).abs() < 1e-3, "the red is untouched: {red:?}");
}

/// The HSL Secondary key (White/Black mask view) of each linear colour under `e`.
fn key_of(e: &EffectInstance, working: WorkingSpace, colours: &[[f32; 3]]) -> Vec<f32> {
    let mut e = e.clone();
    for (k, v) in [("hsl_on", ParamValue::Bool(true)), ("hsl_show_mask", ParamValue::Choice(3))] {
        e.params.get_mut(k).unwrap().value = v;
    }
    let mut img = Image::new(colours.len(), 1);
    for (i, c) in colours.iter().enumerate() {
        img.px[i * 4..i * 4 + 4].copy_from_slice(&[c[0], c[1], c[2], 1.0]);
    }
    apply(&mut img, &e, &cx(working));
    // the mask view is a display image in the working signal: grey levels 0…1 decode to themselves at 0 and 1
    let gs = GradeSpace::new(working, e.f64_at("curves_hdr_range", Tick::ZERO) as f32);
    (0..colours.len()).map(|i| gs.encode([img.px[i * 4]; 3])[0]).collect()
}

/// Pick `c` with `mode` and write the resulting sliders into `e`.
fn pick(e: &mut EffectInstance, working: WorkingSpace, c: [f32; 3], mode: HslKeyPick) {
    for (k, v) in lumetri_hsl_key_pick(e, Tick::ZERO, working, c, mode).unwrap() {
        assert!(v.is_finite(), "{k} = {v}");
        e.params.get_mut(k).unwrap().value = F(f64::from(v));
    }
}

#[test]
fn hsl_key_eyedroppers_set_add_and_remove_colours() {
    let (orange, yellow, blue, grey) = ([0.8, 0.15, 0.02], [0.5, 0.6, 0.03], [0.04, 0.08, 0.7], [0.2, 0.2, 0.2]);
    let all = [orange, yellow, blue, grey];
    let w = WorkingSpace::Rec709;
    let mut e = lumetri(&[]);
    pick(&mut e, w, orange, HslKeyPick::Set);
    let k = key_of(&e, w, &all);
    assert!(k[0] > 0.99 && k[1] < 0.01 && k[2] < 0.01 && k[3] < 0.01, "Set keys the orange alone: {k:?}");
    pick(&mut e, w, yellow, HslKeyPick::Add);
    let k = key_of(&e, w, &all);
    assert!(k[0] > 0.99 && k[1] > 0.99 && k[2] < 0.01 && k[3] < 0.01, "Add takes the yellow in: {k:?}");
    // adding a colour already inside changes nothing
    let again = lumetri_hsl_key_pick(&e, Tick::ZERO, w, orange, HslKeyPick::Add).unwrap();
    let now: Vec<f32> = again.iter().map(|(k, _)| e.f64_at(k, Tick::ZERO) as f32).collect();
    assert!(again.iter().zip(&now).all(|((_, a), b)| (a - b).abs() < 1e-3), "{again:?} vs {now:?}");
    pick(&mut e, w, yellow, HslKeyPick::Remove);
    let k = key_of(&e, w, &all);
    assert!(k[1] < 0.01 && k[0] > 0.99, "Remove drops the yellow and keeps the orange: {k:?}");
    // removing a colour outside the key changes nothing
    let kept = lumetri_hsl_key_pick(&e, Tick::ZERO, w, blue, HslKeyPick::Remove).unwrap();
    assert!(kept.iter().all(|(k, v)| (e.f64_at(k, Tick::ZERO) as f32 - v).abs() < 1e-3), "{kept:?}");
    // colours that are not numbers pick nothing
    for c in [[f32::NAN, 0.2, 0.2], [f32::INFINITY, 0.0, 0.0]] {
        assert!(lumetri_hsl_key_pick(&e, Tick::ZERO, w, c, HslKeyPick::Set).is_none());
    }
    // negative and huge values are clamped like the key clamps them
    assert!(lumetri_hsl_key_pick(&e, Tick::ZERO, w, [-1.0, 1e30, 0.2], HslKeyPick::Add).is_some_and(|v| v.iter().all(|(_, x)| x.is_finite())));
}

#[test]
fn hsl_key_eyedroppers_wrap_around_red_and_stop_at_180_degrees() {
    let w = WorkingSpace::Rec709;
    let hue = |deg: f32| {
        let c = filmcraft_color::hsl_to_rgb(deg / 360.0, 0.8, 0.5);
        c.map(filmcraft_color::srgb_to_linear)
    };
    let mut e = lumetri(&[]);
    pick(&mut e, w, hue(350.0), HslKeyPick::Set);
    pick(&mut e, w, hue(20.0), HslKeyPick::Add);
    let k = key_of(&e, w, &[hue(350.0), hue(5.0), hue(20.0), hue(180.0)]);
    assert!(k[0] > 0.99 && k[1] > 0.99 && k[2] > 0.99 && k[3] < 0.01, "the arc runs through red: {k:?}");
    let range = e.f64_at("hsl_hue_range", Tick::ZERO);
    assert!((44.0..47.0).contains(&range), "350° … 20° plus the half range on the far side: {range}");
    // an opposite hue would need more than 180°: the arc stops there with the colour at its edge
    pick(&mut e, w, hue(200.0), HslKeyPick::Add);
    assert!((e.f64_at("hsl_hue_range", Tick::ZERO) - 180.0).abs() < 1e-3);
    assert!(key_of(&e, w, &[hue(200.0)])[0] > 0.99);
    // Remove on the hue axis pulls the near edge in
    let mut e = lumetri(&[]);
    pick(&mut e, w, hue(120.0), HslKeyPick::Set);
    pick(&mut e, w, hue(131.0), HslKeyPick::Remove);
    let k = key_of(&e, w, &[hue(131.0), hue(107.0)]);
    assert!(k[0] < 0.01 && k[1] > 0.99, "{k:?}");
}

#[test]
fn hsl_key_eyedroppers_read_hdr_colours_like_the_key() {
    // a bright red highlight above HDR White in a PQ sequence: Set keys it, not the grey beside it
    let w = WorkingSpace::Rec2100Pq;
    let red = [2000.0 / RW, 300.0 / RW, 200.0 / RW];
    let grey = [100.0 / RW; 3];
    let mut e = lumetri(&[]);
    pick(&mut e, w, red, HslKeyPick::Set);
    let k = key_of(&e, w, &[red, grey]);
    assert!(k[0] > 0.99 && k[1] < 0.01, "{k:?}");
}
