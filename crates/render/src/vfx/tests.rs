//! Tests for the M5.11 video effects: every effect runs, stays finite and in range, and is
//! deterministic; neutral parameters are the identity; each effect has a known-value check; the
//! frame/track environment works through the real compositor.

use std::sync::Arc;

use filmcraft_project::{EffectKind, Interpolation, Keyframe, Param, ParamKind, find_effect};

use super::*;

fn cx() -> FxCtx<'static> {
    FxCtx {
        t: Tick::ZERO,
        px_scale: 1.0,
        seconds: 0.0,
        timecode: "01:00:00:00",
        clip_name: "clip",
        project: None,
        env: None,
        frame_rate: 24.0,
        working: filmcraft_color::WorkingSpace::Rec709,
    }
}

fn inst(id: &str) -> EffectInstance {
    find_effect(id).unwrap_or_else(|| panic!("no effect {id}")).instance()
}

fn set(e: &mut EffectInstance, k: &str, v: ParamValue) {
    e.params.get_mut(k).unwrap_or_else(|| panic!("{}: no param {k}", e.effect)).value = v;
}
fn setf(e: &mut EffectInstance, k: &str, v: f64) {
    set(e, k, ParamValue::Float(v));
}

/// A deterministic textured test picture (straight sRGB-ish values in linear premultiplied form).
fn picture(w: usize, h: usize) -> Image {
    let mut img = Image::new(w, h);
    for y in 0..h {
        for x in 0..w {
            let (u, v) = (x as f32 / w as f32, y as f32 / h as f32);
            let n = noise3(x as f32 / 6.0, y as f32 / 6.0, 0.5, 9) * 0.25;
            let c = [(0.2 + 0.6 * u + n).clamp(0.0, 1.0), (0.3 + 0.5 * v - n).clamp(0.0, 1.0), (0.5 + 0.4 * (u - v) + n * 0.5).clamp(0.0, 1.0)];
            let l = dec(c);
            img.px[(y * w + x) * 4..(y * w + x) * 4 + 4].copy_from_slice(&[l[0], l[1], l[2], 1.0]);
        }
    }
    img
}

fn run(id: &str, e: &EffectInstance, img: &Image, cx: &FxCtx) -> Image {
    let mut o = img.clone();
    crate::effects::apply(&mut o, e, cx);
    let _ = id;
    o
}

fn max_diff(a: &Image, b: &Image) -> f32 {
    a.px.iter().zip(&b.px).map(|(x, y)| (x - y).abs()).fold(0.0, f32::max)
}

/// The ids this milestone added or rebuilt.
const NEW_IDS: &[&str] = &[
    "lighting_effects",
    "bokeh_blur",
    "channel_blur",
    "compound_blur",
    "focus_blur",
    "reduce_interlace_flicker",
    "asc_cdl",
    "video_limiter",
    "vignette",
    "corner_pin",
    "magnify",
    "spherize",
    "turbulent_displace",
    "warp_stabilizer",
    "gradient",
    "channel_mix",
    "color_replace",
    "rounded_crop",
    "vr_blur",
    "vr_chromatic_aberrations",
    "vr_color_gradients",
    "vr_denoise",
    "vr_digital_glitch",
    "vr_fractal_noise",
    "vr_glow",
    "vr_plane_to_sphere",
    "vr_projection",
    "vr_rotate_sphere",
    "vr_sharpen",
    "alpha_adjust",
    "logo_cutout",
    "echo_glow",
    "edge_glow",
    "glint",
    "light_leaks",
    "rgb_split",
    "volumetric_rays",
    "wonder_glow",
    "long_shadow",
    "brush_strokes",
    "color_emboss",
    "roughen_edges",
    "vhs_look",
    "posterize_time",
    "rotate_3d",
    "auto_reframe",
    "camera_shake",
    "grow",
    "shrink",
    "move",
    "spin",
    "spacer",
    "wiggle",
    "auto_align",
    "cineon_converter",
    "clone",
    "stroke",
    "metadata_burnin",
    "echo",
    "lightning",
    "cell_pattern",
    "checkerboard",
    "ellipse",
    "paint_bucket",
    "write_on",
    "alpha_glow",
    "block_dissolve",
    "directional_blur_legacy",
    "gaussian_blur_legacy",
    "gradient_wipe_legacy",
    "linear_wipe_legacy",
    "magnify_legacy",
    "mosaic_legacy",
    "noise_legacy",
    "twirl_legacy",
    // rebuilt
    "ultra_key",
    "lens_flare",
    "track_matte",
    "simple_text",
    "noise",
    "mosaic",
];

#[test]
fn every_new_effect_is_defined_as_video() {
    for id in NEW_IDS {
        let d = find_effect(id).unwrap_or_else(|| panic!("{id}"));
        assert_eq!(d.kind, EffectKind::Video, "{id}");
        assert!(!d.category.is_empty(), "{id}");
        for p in &d.params {
            if let ParamKind::Float { min, max, .. } = p.kind {
                let v = p.default.as_f64().unwrap();
                assert!(v >= min && v <= max, "{id}.{} default {v} outside {min}..{max}", p.id);
            }
        }
    }
}

/// Push every float parameter towards its soft maximum so the code paths run.
fn pushed(id: &str) -> EffectInstance {
    let def = find_effect(id).unwrap();
    let mut e = def.instance();
    for (pid, p) in e.params.iter_mut() {
        if let ParamValue::Float(v) = &mut p.value
            && let Some(ParamKind::Float { soft_max, soft_min, .. }) = def.param(pid).map(|d| &d.kind)
        {
            *v = (*v + (soft_max - soft_min) * 0.3).min(*soft_max);
        }
    }
    e
}

#[test]
fn every_effect_is_finite_in_range_and_deterministic() {
    let img = picture(40, 28);
    let c = FxCtx { seconds: 0.4, ..cx() };
    for id in NEW_IDS {
        for e in [inst(id), pushed(id)] {
            let a = run(id, &e, &img, &c);
            let b = run(id, &e, &img, &c);
            assert_eq!(a.px, b.px, "{id} not deterministic");
            assert_eq!((a.w, a.h), (img.w, img.h), "{id} changed size");
            for p in a.px.as_chunks::<4>().0 {
                assert!(p.iter().all(|v| v.is_finite()), "{id}: {p:?}");
                assert!(p[3] >= -1e-4 && p[3] <= 1.0 + 1e-3, "{id}: alpha {}", p[3]);
                assert!(p[..3].iter().all(|v| *v >= -1e-3), "{id}: negative {p:?}");
            }
        }
    }
}

#[test]
fn every_choice_value_runs() {
    let img = picture(24, 16);
    for id in NEW_IDS {
        let def = find_effect(id).unwrap();
        for p in &def.params {
            if let ParamKind::Choice(opts) = p.kind {
                for i in 0..opts.len() {
                    let mut e = pushed(id);
                    set(&mut e, p.id, ParamValue::Choice(i as u32));
                    let o = run(id, &e, &img, &cx());
                    assert!(o.px.iter().all(|v| v.is_finite()), "{id}.{}={i}", p.id);
                }
            }
        }
    }
}

#[test]
fn neutral_parameters_are_identity() {
    let img = picture(32, 24);
    let cases: Vec<(&str, Vec<(&str, ParamValue)>)> = vec![
        ("asc_cdl", vec![]),
        ("channel_mix", vec![]),
        ("vignette", vec![("amount", ParamValue::Float(0.0))]),
        ("corner_pin", vec![]),
        ("spherize", vec![]),
        ("turbulent_displace", vec![("amount", ParamValue::Float(0.0))]),
        ("rotate_3d", vec![]),
        ("grow", vec![]),
        ("spin", vec![]),
        ("alpha_adjust", vec![]),
        ("rounded_crop", vec![("radius", ParamValue::Float(0.0))]),
        ("block_dissolve", vec![]),
        ("gradient_wipe_legacy", vec![]),
        ("linear_wipe_legacy", vec![]),
        ("channel_blur", vec![]),
        ("bokeh_blur", vec![("amount", ParamValue::Float(0.0))]),
        ("reduce_interlace_flicker", vec![]),
        ("vr_blur", vec![]),
        ("vr_sharpen", vec![]),
        ("vr_denoise", vec![]),
        ("vr_digital_glitch", vec![]),
        ("vr_chromatic_aberrations", vec![]),
        ("vr_rotate_sphere", vec![]),
        ("vr_projection", vec![]),
        ("rgb_split", vec![("amount", ParamValue::Float(0.0))]),
        ("color_replace", vec![("target", ParamValue::Color([0.0, 0.0, 0.0, 1.0])), ("similarity", ParamValue::Float(0.0))]),
        ("clone", vec![("columns", ParamValue::Float(1.0))]),
        ("auto_align", vec![("horizontal", ParamValue::Choice(0)), ("vertical", ParamValue::Choice(0))]),
        ("auto_align", vec![]),
        ("magnify", vec![("magnification", ParamValue::Float(100.0))]),
        ("stroke", vec![("width", ParamValue::Float(0.0))]),
        ("long_shadow", vec![("length", ParamValue::Float(0.0))]),
        ("roughen_edges", vec![("border", ParamValue::Float(0.0))]),
        ("posterize_time", vec![]),
        ("echo", vec![]),
        ("warp_stabilizer", vec![]),
        ("track_matte", vec![]),
        ("noise", vec![]),
        ("noise_legacy", vec![]),
        ("gaussian_blur_legacy", vec![]),
        ("directional_blur_legacy", vec![]),
        ("video_limiter", vec![("compression", ParamValue::Choice(0))]),
        ("cineon_converter", vec![("conversion", ParamValue::Choice(2)), ("black10", ParamValue::Float(95.0)), ("white10", ParamValue::Float(685.0))]),
        ("light_leaks", vec![("intensity", ParamValue::Float(0.0))]),
        ("lens_flare", vec![("brightness", ParamValue::Float(0.0))]),
        ("simple_text", vec![("text", ParamValue::Text(String::new()))]),
    ];
    for (id, params) in cases {
        let mut e = inst(id);
        for (k, v) in params {
            set(&mut e, k, v);
        }
        let o = run(id, &e, &img, &cx());
        let d = max_diff(&o, &img);
        // in-range values pass the limiter's knee untouched; Cineon log→log at the default points
        assert!(d < 2e-3, "{id} not identity: max diff {d}");
    }
}

fn enc_px(img: &Image, x: usize, y: usize) -> [f32; 4] {
    let p = img.get(x, y);
    let c = enc(Image::unpremul(p));
    [c[0], c[1], c[2], p[3]]
}
fn solid(w: usize, h: usize, c: [f32; 3]) -> Image {
    let l = dec(c);
    Image::filled(w, h, [l[0], l[1], l[2], 1.0])
}
fn close(a: f32, b: f32, tol: f32) -> bool {
    (a - b).abs() <= tol
}

#[test]
fn asc_cdl_known_values() {
    // slope 2, offset 0.1, power 2 on 0.2 → (0.5)² = 0.25; saturation 0 → luma
    let out = color::cdl([0.2, 0.2, 0.2], [2.0; 3], [0.1; 3], [2.0; 3], 1.0);
    assert!(out.iter().all(|v| close(*v, 0.25, 1e-6)), "{out:?}");
    let grey = color::cdl([1.0, 0.0, 0.0], [1.0; 3], [0.0; 3], [1.0; 3], 0.0);
    assert!(grey.iter().all(|v| close(*v, 0.2126, 1e-5)), "{grey:?}");
    let mut e = inst("asc_cdl");
    setf(&mut e, "r_slope", 2.0);
    let o = run("asc_cdl", &e, &solid(4, 4, [0.25, 0.25, 0.25]), &cx());
    let p = enc_px(&o, 1, 1);
    assert!(close(p[0], 0.5, 2e-3) && close(p[1], 0.25, 2e-3), "{p:?}");
}

#[test]
fn channel_mix_swaps_channels() {
    let mut e = inst("channel_mix");
    for (k, v) in [("rr", 0.0), ("rb", 100.0), ("bb", 0.0), ("br", 100.0)] {
        setf(&mut e, k, v);
    }
    let o = run("channel_mix", &e, &solid(4, 4, [0.8, 0.4, 0.1]), &cx());
    let p = enc_px(&o, 0, 0);
    assert!(close(p[0], 0.1, 2e-3) && close(p[1], 0.4, 2e-3) && close(p[2], 0.8, 2e-3), "{p:?}");
}

#[test]
fn video_limiter_clips_super_whites() {
    let img = Image::filled(4, 4, [3.0, 3.0, 3.0, 1.0]);
    let o = run("video_limiter", &inst("video_limiter"), &img, &cx());
    let p = o.get(0, 0);
    assert!(p[0] <= 1.0 + 1e-4 && p[0] > 0.9, "{p:?}");
    // 105 IRE allows a little more
    let mut e = inst("video_limiter");
    set(&mut e, "clip_level", ParamValue::Choice(5));
    set(&mut e, "compression", ParamValue::Choice(0));
    let o = run("video_limiter", &e, &img, &cx());
    assert!(o.get(0, 0)[0] > 1.0, "{:?}", o.get(0, 0));
    // gamut warning paints the clipped pixels
    let mut e = inst("video_limiter");
    set(&mut e, "gamut_warning", ParamValue::Bool(true));
    let o = run("video_limiter", &e, &img, &cx());
    let p = o.get(0, 0);
    assert!(p[0] > 0.9 && p[1] < 0.01, "{p:?}");
    // the limiter keeps legal R′G′B′ and the luma knee
    let v = color::limit([1.2, 0.2, -0.1], 1.0, 0.0, 2);
    assert!(v.iter().all(|c| *c >= 0.0 && *c <= 1.0), "{v:?}");
}

#[test]
fn vignette_darkens_corners_only() {
    let img = solid(64, 36, [0.6, 0.6, 0.6]);
    let o = run("vignette", &inst("vignette"), &img, &cx());
    assert!(enc_px(&o, 0, 0)[0] < 0.45, "corner {:?}", enc_px(&o, 0, 0));
    assert!(close(enc_px(&o, 32, 18)[0], 0.6, 2e-3), "centre {:?}", enc_px(&o, 32, 18));
}

#[test]
fn corner_pin_maps_corners() {
    let img = picture(40, 30);
    let mut e = inst("corner_pin");
    // shrink the right half to the middle: UR (40,0)→(20,0), LR (40,30)→(20,30)
    set(&mut e, "upper_left", ParamValue::Vec2(Vec2::new(0.0, 0.0)));
    set(&mut e, "lower_left", ParamValue::Vec2(Vec2::new(0.0, 30.0)));
    set(&mut e, "upper_right", ParamValue::Vec2(Vec2::new(20.0, 0.0)));
    set(&mut e, "lower_right", ParamValue::Vec2(Vec2::new(20.0, 30.0)));
    let o = run("corner_pin", &e, &img, &cx());
    assert_eq!(o.get(30, 15)[3], 0.0, "outside the pinned quad is clear");
    // output x maps to source 2x
    let a = o.get(5, 10);
    let b = img.sample_bilinear(11.0, 10.5);
    for k in 0..4 {
        assert!(close(a[k], b[k], 1e-3), "{a:?} {b:?}");
    }
    // the homography helpers invert exactly
    let q = [Vec2::new(3.0, 1.0), Vec2::new(30.0, 5.0), Vec2::new(28.0, 25.0), Vec2::new(1.0, 20.0)];
    let h = distort::square_to_quad(q);
    for (i, (u, v)) in [(0.0, 0.0), (1.0, 0.0), (1.0, 1.0), (0.0, 1.0)].iter().enumerate() {
        let (x, y) = distort::h_apply(&h, *u, *v).unwrap();
        assert!((x - q[i].x).abs() < 1e-9 && (y - q[i].y).abs() < 1e-9);
    }
}

#[test]
fn magnify_and_spherize_enlarge_the_centre() {
    let img = picture(60, 60);
    let o = run("magnify", &inst("magnify"), &img, &cx());
    // 200 %: the pixel 10 px right of centre shows the source 5 px right of centre
    let a = o.get(40, 30);
    let b = img.sample_bilinear(35.25, 30.5);
    for k in 0..3 {
        assert!(close(a[k], b[k], 0.02), "{a:?} {b:?}");
    }
    let mut e = inst("spherize");
    setf(&mut e, "radius", 25.0);
    let o = run("spherize", &e, &img, &cx());
    assert!(max_diff(&o, &img) > 0.01);
    // outside the radius nothing moves
    assert_eq!(o.get(1, 1), img.get(1, 1));
}

#[test]
fn turbulent_displace_moves_pixels_and_pins_edges() {
    let img = picture(48, 48);
    let o = run("turbulent_displace", &inst("turbulent_displace"), &img, &cx());
    assert!(max_diff(&o, &img) > 0.01);
    // Pin All: the corners stay
    for (x, y) in [(0, 0), (47, 0), (0, 47), (47, 47)] {
        let (a, b) = (o.get(x, y), img.get(x, y));
        assert!((0..4).all(|k| close(a[k], b[k], 0.03)), "{x},{y}: {a:?} {b:?}");
    }
    // evolution changes the pattern; seed too
    let mut e = inst("turbulent_displace");
    setf(&mut e, "evolution", 90.0);
    assert!(max_diff(&run("t", &e, &img, &cx()), &o) > 1e-3);
}

#[test]
fn rotate_3d_z_matches_a_2d_rotation() {
    let img = picture(41, 41);
    let mut e = inst("rotate_3d");
    setf(&mut e, "rot_z", 90.0);
    let o = run("rotate_3d", &e, &img, &cx());
    // rotating 90° clockwise about the centre: output (x,y) samples source (y, 41−x)
    let a = o.get(30, 10);
    let b = img.sample_bilinear(10.5, 41.0 - 30.5);
    for k in 0..3 {
        assert!(close(a[k], b[k], 0.02), "{a:?} {b:?}");
    }
    // a Y rotation past 90° shows the back face; Hide Back Face clears it
    setf(&mut e, "rot_z", 0.0);
    setf(&mut e, "rot_y", 180.0);
    set(&mut e, "hide_back", ParamValue::Bool(true));
    assert!(run("r", &e, &img, &cx()).px.iter().all(|v| *v == 0.0));
}

#[test]
fn track_matte_uses_the_other_tracks_alpha_and_luma() {
    struct Env(Image);
    impl FxEnv for Env {
        fn frame(&self, _: f64, _: &EffectInstance) -> Option<Image> {
            None
        }
        fn source_frame(&self, _: f64, _: f32) -> Option<Image> {
            None
        }
        fn track(&self, i: usize) -> Option<Image> {
            (i == 1).then(|| self.0.clone())
        }
        fn layer_to_output(&self) -> Affine {
            Affine::IDENTITY
        }
        fn clip_seconds(&self) -> f64 {
            1.0
        }
        fn clip_offset(&self) -> f64 {
            0.0
        }
        fn frame_rate(&self) -> f64 {
            24.0
        }
        fn media_timecode(&self) -> String {
            String::new()
        }
        fn file_name(&self) -> String {
            String::new()
        }
        fn sequence_name(&self) -> String {
            String::new()
        }
        fn sequence_size(&self) -> (u32, u32) {
            (8, 4)
        }
        fn source_size(&self) -> (u32, u32) {
            (8, 4)
        }
        fn clip_key(&self) -> u64 {
            1
        }
    }
    // matte: left half opaque white, right half transparent
    let mut matte = Image::new(8, 4);
    for y in 0..4 {
        for x in 0..4 {
            matte.px[(y * 8 + x) * 4..(y * 8 + x) * 4 + 4].copy_from_slice(&[1.0, 1.0, 1.0, 1.0]);
        }
    }
    let env = Env(matte);
    let c = FxCtx { env: Some(&env), ..cx() };
    let img = solid(8, 4, [0.5, 0.5, 0.5]);
    let mut e = inst("track_matte");
    set(&mut e, "matte", ParamValue::Choice(2));
    let o = run("track_matte", &e, &img, &c);
    assert!(close(o.get(1, 1)[3], 1.0, 1e-4) && close(o.get(6, 1)[3], 0.0, 1e-4));
    set(&mut e, "reverse", ParamValue::Bool(true));
    let o = run("track_matte", &e, &img, &c);
    assert!(close(o.get(1, 1)[3], 0.0, 1e-4) && close(o.get(6, 1)[3], 1.0, 1e-4));
    set(&mut e, "reverse", ParamValue::Bool(false));
    set(&mut e, "composite", ParamValue::Choice(1));
    let o = run("track_matte", &e, &img, &c);
    assert!(close(o.get(1, 1)[3], 1.0, 1e-3) && close(o.get(6, 1)[3], 0.0, 1e-4));
}

#[test]
fn keyers_known_values() {
    // Ultra Key: pure green screen → transparent, grey foreground → opaque
    let mut img = solid(8, 2, [0.0, 0.8, 0.2]);
    let grey = dec([0.5, 0.45, 0.42]);
    for x in 4..8 {
        for y in 0..2 {
            img.px[(y * 8 + x) * 4..(y * 8 + x) * 4 + 4].copy_from_slice(&[grey[0], grey[1], grey[2], 1.0]);
        }
    }
    let o = run("ultra_key", &inst("ultra_key"), &img, &cx());
    assert!(o.get(1, 0)[3] < 0.05, "screen {:?}", o.get(1, 0));
    assert!(o.get(6, 0)[3] > 0.95, "fg {:?}", o.get(6, 0));
    let mut e = inst("ultra_key");
    set(&mut e, "output", ParamValue::Choice(1));
    let m = run("ultra_key", &e, &img, &cx());
    assert!(m.get(1, 0)[0] < 0.05 && m.get(6, 0)[0] > 0.9 && m.get(1, 0)[3] == 1.0);
    let mut aggressive = inst("ultra_key");
    set(&mut aggressive, "setting", ParamValue::Choice(2));
    let ag = run("ultra_key", &aggressive, &img, &cx());
    let def = run("ultra_key", &inst("ultra_key"), &img, &cx());
    let delta = (0..img.w * img.h).map(|i| (ag.px[i * 4 + 3] - def.px[i * 4 + 3]).abs()).fold(0.0f32, f32::max);
    assert!(delta > 1e-4, "Aggressive Setting left the Default matte unchanged");
    // Alpha Adjust: invert and opacity
    let mut a = Image::filled(2, 2, [0.1, 0.1, 0.1, 0.25]);
    let mut e = inst("alpha_adjust");
    set(&mut e, "invert", ParamValue::Bool(true));
    setf(&mut e, "opacity", 50.0);
    crate::effects::apply(&mut a, &e, &cx());
    assert!(close(a.get(0, 0)[3], 0.375, 1e-5), "{:?}", a.get(0, 0));
    // Logo Cutout: the white background goes, a black logo stays black and opaque
    let mut img = solid(4, 1, [1.0, 1.0, 1.0]);
    img.px[0..4].copy_from_slice(&[0.0, 0.0, 0.0, 1.0]);
    let o = run("logo_cutout", &inst("logo_cutout"), &img, &cx());
    assert_eq!(o.get(2, 0)[3], 0.0);
    assert!(close(o.get(0, 0)[3], 1.0, 1e-4) && o.get(0, 0)[0] < 1e-4);
    // 50 % grey over white un-multiplies to black at alpha 0.5
    let mut e = inst("logo_cutout");
    setf(&mut e, "threshold", 0.0);
    setf(&mut e, "softness", 100.0);
    let o = run("logo_cutout", &e, &solid(1, 1, [0.5, 0.5, 0.5]), &cx());
    let p = o.get(0, 0);
    assert!(close(p[3], 0.5, 1e-3) && p[0] < 1e-3, "{p:?}");
}

#[test]
fn colour_replace_and_cineon() {
    let o = run("color_replace", &inst("color_replace"), &solid(2, 2, [1.0, 0.0, 0.0]), &cx());
    let p = enc_px(&o, 0, 0);
    assert!(p[2] > 0.9 && p[0] < 0.1, "{p:?}");
    // Cineon: code 685 → white, 95 → black; log→lin→log round trip
    let (b, w) = (95.0, 685.0);
    assert!(close(color::cineon_value(685.0 / 1023.0, 0, b, w, 0.0, 1.0, 1.7, 0.0), 1.0, 1e-4));
    assert!(close(color::cineon_value(95.0 / 1023.0, 0, b, w, 0.0, 1.0, 1.7, 0.0), 0.0, 1e-4));
    for v in [0.2f32, 0.4, 0.6] {
        let l = color::cineon_value(v, 0, b, w, 0.0, 1.0, 1.7, 0.0);
        let back = color::cineon_value(l, 1, b, w, 0.0, 1.0, 1.7, 0.0);
        assert!(close(back, v, 2e-3), "{v} → {l} → {back}");
    }
}

#[test]
fn lights_add_light_around_highlights() {
    let mut img = solid(48, 48, [0.05, 0.05, 0.05]);
    for y in 22..26 {
        for x in 22..26 {
            img.px[(y * 48 + x) * 4..(y * 48 + x) * 4 + 4].copy_from_slice(&[1.0, 1.0, 1.0, 1.0]);
        }
    }
    let total = |i: &Image| i.px.as_chunks::<4>().0.iter().map(|p| p[0] as f64).sum::<f64>();
    for id in ["echo_glow", "glint", "wonder_glow", "volumetric_rays", "edge_glow"] {
        let o = run(id, &inst(id), &img, &cx());
        let added = total(&o) - total(&img);
        assert!(added > 0.5, "{id}: added only {added}");
        assert!(o.get(2, 2)[0] < 0.2, "{id}: lit the far corner {:?}", o.get(2, 2));
    }
    // Glint with 4 rays at 0°: light along the horizontal, not the diagonal
    let mut e = inst("glint");
    setf(&mut e, "rotation", 0.0);
    let o = run("glint", &e, &img, &cx());
    assert!(o.get(38, 24)[0] > o.get(34, 34)[0], "{:?} {:?}", o.get(38, 24), o.get(34, 34));
    // RGB Split: red sampled from the left
    let mut s = Image::new(9, 1);
    s.px[4 * 4..4 * 4 + 4].copy_from_slice(&[1.0, 1.0, 1.0, 1.0]);
    let mut e = inst("rgb_split");
    setf(&mut e, "amount", 2.0);
    let o = run("rgb_split", &e, &s, &cx());
    assert!(close(o.get(6, 0)[0], 1.0, 1e-4) && close(o.get(2, 0)[2], 1.0, 1e-4) && close(o.get(4, 0)[1], 1.0, 1e-4));
}

#[test]
fn long_shadow_and_stroke_geometry() {
    // a 4×4 opaque square at (4,4) on a transparent 24×24 layer
    let mut img = Image::new(24, 24);
    for y in 4..8 {
        for x in 4..8 {
            img.px[(y * 24 + x) * 4..(y * 24 + x) * 4 + 4].copy_from_slice(&[1.0, 1.0, 1.0, 1.0]);
        }
    }
    let mut e = inst("long_shadow");
    setf(&mut e, "angle", 90.0); // to the right
    setf(&mut e, "length", 10.0);
    set(&mut e, "fade", ParamValue::Bool(false));
    setf(&mut e, "opacity", 100.0);
    let o = run("long_shadow", &e, &img, &cx());
    assert!(o.get(14, 5)[3] > 0.8, "shadow to the right {:?}", o.get(14, 5));
    assert!(o.get(20, 5)[3] < 0.2, "ends after Length {:?}", o.get(20, 5));
    assert!(o.get(5, 14)[3] < 0.05, "not below {:?}", o.get(5, 14));
    // Stroke outside 2 px
    let mut e = inst("stroke");
    setf(&mut e, "width", 2.0);
    set(&mut e, "color", ParamValue::Color([1.0, 0.0, 0.0, 1.0]));
    let o = run("stroke", &e, &img, &cx());
    assert!(o.get(9, 5)[0] > 0.9 && o.get(9, 5)[1] < 0.05, "stroke {:?}", o.get(9, 5));
    assert!(o.get(12, 5)[3] < 0.01, "beyond the stroke {:?}", o.get(12, 5));
    assert_eq!(o.get(5, 5), [1.0, 1.0, 1.0, 1.0], "inside untouched");
    // exact distance transform
    let mut inside = vec![false; 7 * 5];
    inside[2 * 7 + 3] = true;
    let d = stylize::distance_to(&inside, 7, 5);
    assert!(close(d[2 * 7 + 3], 0.0, 1e-6) && close(d[0], (9.0f32 + 4.0).sqrt(), 1e-5));
}

#[test]
fn generators_known_values() {
    let img = solid(40, 20, [0.5, 0.5, 0.5]);
    // Gradient: black at the top centre, white at the bottom centre
    let mut e = inst("gradient");
    set(&mut e, "start", ParamValue::Vec2(Vec2::new(20.0, 0.0)));
    set(&mut e, "end", ParamValue::Vec2(Vec2::new(20.0, 20.0)));
    let o = run("gradient", &e, &img, &cx());
    assert!(enc_px(&o, 20, 0)[0] < 0.05 && enc_px(&o, 20, 19)[0] > 0.95 && close(enc_px(&o, 20, 10)[0], 0.525, 0.03));
    // Checkerboard: alternating cells
    let mut e = inst("checkerboard");
    setf(&mut e, "width", 10.0);
    set(&mut e, "anchor", ParamValue::Vec2(Vec2::new(0.0, 0.0)));
    let o = run("checkerboard", &e, &img, &cx());
    assert!(o.get(5, 5)[3] < 0.01 && o.get(15, 5)[3] > 0.99 && o.get(15, 15)[3] < 0.01);
    // Ellipse ring: on the ring opaque, at the centre clear
    let mut e = inst("ellipse");
    setf(&mut e, "width", 16.0);
    setf(&mut e, "height", 16.0);
    setf(&mut e, "thickness", 2.0);
    let o = run("ellipse", &e, &img, &cx());
    assert!(o.get(28, 10)[3] > 0.8 && o.get(20, 10)[3] < 0.01, "{:?} {:?}", o.get(28, 10), o.get(20, 10));
    // Cell Pattern is opaque grey in 0..1
    let o = run("cell_pattern", &inst("cell_pattern"), &img, &cx());
    assert!(o.px.as_chunks::<4>().0.iter().all(|p| p[3] == 1.0 && p[0] >= 0.0 && p[0] <= 1.0 && p[0] == p[1]));
    // Lightning: deterministic per seed, different across seeds and strikes
    let a = run("lightning", &inst("lightning"), &img, &cx());
    let mut e = inst("lightning");
    setf(&mut e, "seed", 5.0);
    assert!(max_diff(&a, &run("lightning", &e, &img, &cx())) > 0.01);
    let later = FxCtx { seconds: 2.0, ..cx() };
    assert!(max_diff(&a, &run("lightning", &inst("lightning"), &img, &later)) > 0.01);
}

#[test]
fn paint_bucket_fills_the_connected_region() {
    // left half red, right half blue; fill at the left
    let mut img = solid(10, 4, [1.0, 0.0, 0.0]);
    let blue = dec([0.0, 0.0, 1.0]);
    for y in 0..4 {
        for x in 5..10 {
            img.px[(y * 10 + x) * 4..(y * 10 + x) * 4 + 4].copy_from_slice(&[blue[0], blue[1], blue[2], 1.0]);
        }
    }
    let mut e = inst("paint_bucket");
    set(&mut e, "point", ParamValue::Vec2(Vec2::new(1.0, 1.0)));
    set(&mut e, "color", ParamValue::Color([0.0, 1.0, 0.0, 1.0]));
    let o = run("paint_bucket", &e, &img, &cx());
    assert!(enc_px(&o, 1, 1)[1] > 0.95 && enc_px(&o, 1, 1)[0] < 0.05);
    assert!(enc_px(&o, 8, 1)[2] > 0.95 && enc_px(&o, 8, 1)[1] < 0.05);
}

#[test]
fn write_on_follows_the_animated_brush() {
    use filmcraft_project::{Interpolation, Keyframe};
    let img = solid(40, 20, [0.0, 0.0, 0.0]);
    let mut e = inst("write_on");
    let p = e.params.get_mut("brush").unwrap();
    p.keyframes = vec![
        Keyframe {
            time: Tick::ZERO,
            value: ParamValue::Vec2(Vec2::new(5.0, 10.0)),
            interp: Interpolation::Linear,
            out_influence: 1.0 / 3.0,
            in_influence: 1.0 / 3.0,
        },
        Keyframe {
            time: Tick::from_seconds_f64(1.0),
            value: ParamValue::Vec2(Vec2::new(35.0, 10.0)),
            interp: Interpolation::Linear,
            out_influence: 1.0 / 3.0,
            in_influence: 1.0 / 3.0,
        },
    ];
    // half way: painted from x=5 to x=20, not beyond
    let c = FxCtx { t: Tick::from_seconds_f64(0.5), seconds: 0.5, ..cx() };
    let o = run("write_on", &e, &img, &c);
    assert!(enc_px(&o, 12, 10)[0] > 0.9, "{:?}", enc_px(&o, 12, 10));
    assert!(enc_px(&o, 30, 10)[0] < 0.05, "{:?}", enc_px(&o, 30, 10));
}

#[test]
fn transform_presets_animate_over_the_clip() {
    struct Env;
    impl FxEnv for Env {
        fn frame(&self, _: f64, _: &EffectInstance) -> Option<Image> {
            None
        }
        fn source_frame(&self, _: f64, _: f32) -> Option<Image> {
            None
        }
        fn track(&self, _: usize) -> Option<Image> {
            None
        }
        fn layer_to_output(&self) -> Affine {
            Affine::IDENTITY
        }
        fn clip_seconds(&self) -> f64 {
            2.0
        }
        fn clip_offset(&self) -> f64 {
            0.0
        }
        fn frame_rate(&self) -> f64 {
            24.0
        }
        fn media_timecode(&self) -> String {
            "00:00:10:00".into()
        }
        fn file_name(&self) -> String {
            "A001.mov".into()
        }
        fn sequence_name(&self) -> String {
            "Seq".into()
        }
        fn sequence_size(&self) -> (u32, u32) {
            (40, 40)
        }
        fn source_size(&self) -> (u32, u32) {
            (40, 40)
        }
        fn clip_key(&self) -> u64 {
            2
        }
    }
    let env = Env;
    let img = picture(40, 40);
    let end = FxCtx { seconds: 2.0, env: Some(&env), ..cx() };
    // Grow ends at 120 %
    let o = run("grow", &inst("grow"), &img, &end);
    let a = o.get(30, 20);
    let b = img.sample_bilinear(20.0 + 10.5 / 1.2, 20.0 + 0.5 / 1.2);
    assert!((0..3).all(|k| close(a[k], b[k], 0.02)), "{a:?} {b:?}");
    // Spin ends at 360° (identity again); half way it is upside down
    let o = run("spin", &inst("spin"), &img, &end);
    assert!(max_diff(&o, &img) < 0.02);
    let mid = FxCtx { seconds: 1.0, env: Some(&env), ..cx() };
    let o = run("spin", &inst("spin"), &img, &mid);
    let (a, b) = (o.get(10, 10), img.get(29, 29));
    assert!((0..3).all(|k| close(a[k], b[k], 0.03)), "{a:?} {b:?}");
    // Move from (−100, 0) to (0, 0): at the end identity, at the start shifted out
    assert!(max_diff(&run("move", &inst("move"), &img, &end), &img) < 1e-4);
    // burn-in text sources
    let e = inst("metadata_burnin");
    assert_eq!(text::burnin_text(&e, &end), "01:00:00:00");
    let mut e2 = e.clone();
    set(&mut e2, "source", ParamValue::Choice(1));
    assert_eq!(text::burnin_text(&e2, &end), "00:00:10:00");
    set(&mut e2, "source", ParamValue::Choice(3));
    set(&mut e2, "prefix", ParamValue::Text("FILE".into()));
    assert_eq!(text::burnin_text(&e2, &end), "FILE A001.mov");
    set(&mut e2, "source", ParamValue::Choice(4));
    assert_eq!(text::burnin_text(&e2, &mid), "FILE 24");
}

#[test]
fn transform_shutter_override_blurs_moving_position() {
    let mut e = inst("transform");
    set(&mut e, "shutter_override", ParamValue::Bool(true));
    setf(&mut e, "shutter_angle", 180.0);
    let a = Keyframe::new(Tick::ZERO, ParamValue::Vec2(filmcraft_geom::Vec2::new(8.0, 16.0)));
    let mut b = Keyframe::new(Tick::from_seconds_f64(1.0), ParamValue::Vec2(filmcraft_geom::Vec2::new(40.0, 16.0)));
    b.interp = Interpolation::Linear;
    e.params.insert("position".into(), Param { value: ParamValue::Vec2(filmcraft_geom::Vec2::new(8.0, 16.0)), keyframes: vec![a, b] });
    let img = picture(32, 32);
    let cx = FxCtx { t: Tick::from_seconds_f64(0.5), seconds: 0.5, ..cx() };
    let sharp = {
        let mut off = e.clone();
        set(&mut off, "shutter_override", ParamValue::Bool(false));
        run("transform", &off, &img, &cx)
    };
    let blurred = run("transform", &e, &img, &cx);
    let delta = max_diff(&sharp, &blurred);
    assert!(delta > 1e-3, "shutter override left the Transform picture unchanged ({delta})");
}

#[test]
fn clone_auto_align_and_rounded_crop() {
    let img = picture(40, 20);
    let mut e = inst("clone");
    setf(&mut e, "columns", 2.0);
    let o = run("clone", &e, &img, &cx());
    // two half-size copies side by side, letterboxed vertically
    assert_eq!(o.get(10, 1)[3], 0.0);
    let (a, b) = (o.get(10, 10), o.get(30, 10));
    assert!((0..4).all(|k| close(a[k], b[k], 1e-5)));
    // Auto Align left: content moves to x = 0
    let mut layer = Image::new(20, 10);
    for y in 3..6 {
        for x in 10..14 {
            layer.px[(y * 20 + x) * 4..(y * 20 + x) * 4 + 4].copy_from_slice(&[1.0, 1.0, 1.0, 1.0]);
        }
    }
    let mut e = inst("auto_align");
    set(&mut e, "horizontal", ParamValue::Choice(1));
    set(&mut e, "vertical", ParamValue::Choice(1));
    let o = run("auto_align", &e, &layer, &cx());
    assert_eq!(o.get(0, 0)[3], 1.0);
    assert_eq!(o.get(4, 0)[3], 0.0);
    // Rounded Crop: corners are cut, the centre stays
    let o = run("rounded_crop", &inst("rounded_crop"), &picture(100, 100), &cx());
    assert_eq!(o.get(0, 0)[3], 0.0);
    assert_eq!(o.get(50, 50)[3], 1.0);
}

#[test]
fn vr_rotate_sphere_pans_exactly() {
    let img = picture(64, 32);
    let mut e = inst("vr_rotate_sphere");
    setf(&mut e, "pan", 360.0);
    assert!(max_diff(&run("vr", &e, &img, &cx()), &img) < 1e-4);
    setf(&mut e, "pan", 90.0);
    let o = run("vr", &e, &img, &cx());
    // a quarter turn is a 16-pixel horizontal shift (with wrap)
    let mut moved = 0.0f32;
    for y in 4..28 {
        for x in 0..64 {
            let a = o.get(x, y);
            let b = img.get((x + 64 - 16) % 64, y);
            let c = img.get((x + 16) % 64, y);
            moved = moved.max((0..3).map(|k| (a[k] - b[k]).abs()).fold(0.0, f32::max).min((0..3).map(|k| (a[k] - c[k]).abs()).fold(0.0, f32::max)));
        }
    }
    assert!(moved < 2e-3, "{moved}");
    // VR Blur is seamless across the ±180° edge
    let mut e = inst("vr_blur");
    setf(&mut e, "blurriness", 8.0);
    let o = run("vr_blur", &e, &img, &cx());
    let seam = |i: &Image| (0..3).map(|k| (i.get(0, 16)[k] - i.get(63, 16)[k]).abs()).fold(0.0, f32::max);
    assert!(seam(&o) < seam(&img) * 0.3, "{} vs {}", seam(&o), seam(&img));
    // Plane to Sphere: the flat picture lands in front (lon 0), nothing behind
    let o = run("vr_plane_to_sphere", &inst("vr_plane_to_sphere"), &img, &cx());
    assert!(o.get(32, 16)[3] > 0.99 && o.get(0, 16)[3] == 0.0);
    // stereoscopic layouts process each half
    let mut e = inst("vr_rotate_sphere");
    setf(&mut e, "pan", 90.0);
    set(&mut e, "frame_layout", ParamValue::Choice(1));
    let o = run("vr", &e, &img, &cx());
    assert_eq!((o.w, o.h), (64, 32));
}

#[test]
fn bokeh_and_focus_blur() {
    let mut img = solid(48, 48, [0.0, 0.0, 0.0]);
    img.px[(24 * 48 + 24) * 4..(24 * 48 + 24) * 4 + 4].copy_from_slice(&[1.0, 1.0, 1.0, 1.0]);
    let mut e = inst("bokeh_blur");
    setf(&mut e, "amount", 8.0);
    set(&mut e, "shape", ParamValue::Choice(2)); // square
    setf(&mut e, "rotation", 45.0); // axis-aligned, half-side 8/√2
    let o = run("bokeh", &e, &img, &cx());
    // the bright point becomes a square: lit at (+5, +5), dark at (+7, 0) where a disc would be lit
    assert!(o.get(29, 29)[0] > 1e-4 && o.get(31, 24)[0] < 1e-6, "{:?} {:?}", o.get(29, 29), o.get(31, 24));
    set(&mut e, "shape", ParamValue::Choice(0));
    let o = run("bokeh", &e, &img, &cx());
    assert!(o.get(31, 24)[0] > 1e-4);
    // Focus Blur: the focus point stays sharp
    let pic = picture(64, 64);
    let mut e = inst("focus_blur");
    setf(&mut e, "size", 20.0);
    setf(&mut e, "feather", 10.0);
    setf(&mut e, "amount", 20.0);
    let o = run("focus", &e, &pic, &cx());
    assert!(max_diff(&o, &pic) > 0.01);
    let (a, b) = (o.get(32, 32), pic.get(32, 32));
    assert!((0..4).all(|k| close(a[k], b[k], 1e-4)));
}

#[test]
fn echo_combines_operators() {
    let a = Image::filled(1, 1, [0.2, 0.4, 0.1, 1.0]);
    let b = Image::filled(1, 1, [0.6, 0.1, 0.3, 1.0]);
    let layers = vec![(a.clone(), 1.0), (b.clone(), 0.5)];
    let add = temporal::combine_echoes(&layers, 0);
    assert!(close(add.get(0, 0)[0], 0.5, 1e-6) && close(add.get(0, 0)[3], 1.0, 1e-6));
    let max = temporal::combine_echoes(&layers, 1);
    assert!(close(max.get(0, 0)[0], 0.3, 1e-6) && close(max.get(0, 0)[1], 0.4, 1e-6));
    let back = temporal::combine_echoes(&layers, 4);
    assert_eq!(back.get(0, 0), a.get(0, 0), "the current frame stays in front");
}

/// A clip environment over a synthetic moving picture (for the analysis effects).
struct ClipEnv {
    w: usize,
    h: usize,
    fps: f64,
    secs: f64,
    now: f64,
    key: u64,
    seq: (u32, u32),
    pic: fn(f64, f64, f64) -> [f32; 3],
}
impl ClipEnv {
    fn render(&self, t: f64, scale: f32) -> Image {
        let (w, h) = (((self.w as f32 * scale).round() as usize).max(1), ((self.h as f32 * scale).round() as usize).max(1));
        let k = self.w as f64 / w as f64;
        let mut img = Image::new(w, h);
        img.px.par_chunks_mut(w * 4).enumerate().for_each(|(y, row)| {
            for x in 0..w {
                let c = (self.pic)((x as f64 + 0.5) * k, (y as f64 + 0.5) * k, t);
                row[x * 4..x * 4 + 4].copy_from_slice(&[c[0], c[1], c[2], 1.0]);
            }
        });
        img
    }
}
impl FxEnv for ClipEnv {
    fn frame(&self, dt: f64, _: &EffectInstance) -> Option<Image> {
        Some(self.render(((self.now + dt) * self.fps).round() / self.fps, 1.0))
    }
    fn source_frame(&self, dt: f64, scale: f32) -> Option<Image> {
        Some(self.render(((self.now + dt) * self.fps).round() / self.fps, scale))
    }
    fn track(&self, _: usize) -> Option<Image> {
        None
    }
    fn layer_to_output(&self) -> Affine {
        Affine::IDENTITY
    }
    fn clip_seconds(&self) -> f64 {
        self.secs
    }
    fn clip_offset(&self) -> f64 {
        self.now
    }
    fn frame_rate(&self) -> f64 {
        self.fps
    }
    fn media_timecode(&self) -> String {
        String::new()
    }
    fn file_name(&self) -> String {
        String::new()
    }
    fn sequence_name(&self) -> String {
        String::new()
    }
    fn sequence_size(&self) -> (u32, u32) {
        self.seq
    }
    fn source_size(&self) -> (u32, u32) {
        (self.w as u32, self.h as u32)
    }
    fn clip_key(&self) -> u64 {
        self.key
    }
}

fn texture(x: f64, y: f64) -> [f32; 3] {
    let v = 0.5 + 0.45 * fbm(x as f32 / 9.0, y as f32 / 9.0, 0.3, 4.0, 11);
    let l = filmcraft_color::srgb_to_linear(v.clamp(0.0, 1.0));
    [l, l, l]
}
/// Camera jitter at time t: a few pixels, changing every frame.
fn jitter(t: f64) -> (f64, f64) {
    let f = (t * 24.0).round() as i64;
    ((hash01(f, 0, 0, 3) as f64 - 0.5) * 8.0, (hash01(f, 1, 0, 3) as f64 - 0.5) * 8.0)
}
fn shaky(x: f64, y: f64, t: f64) -> [f32; 3] {
    let (dx, dy) = jitter(t);
    texture(x + dx, y + dy)
}

#[test]
fn posterize_time_and_echo_read_other_frames() {
    fn moving(x: f64, _y: f64, t: f64) -> [f32; 3] {
        let v = if (x - t * 100.0).abs() < 4.0 { 1.0 } else { 0.0 };
        [v, v, v]
    }
    let mk = |now: f64| ClipEnv { w: 64, h: 8, fps: 24.0, secs: 1.0, now, key: 77, seq: (64, 8), pic: moving };
    // 12 fps: frames 0.0 and 1/24 show the same picture; 2/24 is new
    let e = inst("posterize_time");
    let at = |now: f64| {
        let env = mk(now);
        let mut img = env.render(now, 1.0);
        crate::effects::apply(&mut img, &e, &FxCtx { seconds: now, env: Some(&env), ..cx() });
        img
    };
    assert_eq!(at(0.0).px, at(1.0 / 24.0).px);
    assert_ne!(at(0.0).px, at(2.0 / 24.0).px);
    // Echo (one echo a frame back, Add): two bars
    let env = mk(0.5);
    let mut img = env.render(0.5, 1.0);
    let mut e = inst("echo");
    setf(&mut e, "time", -0.1);
    setf(&mut e, "decay", 0.5);
    crate::effects::apply(&mut img, &e, &FxCtx { seconds: 0.5, env: Some(&env), ..cx() });
    assert!(close(img.get(50, 4)[0], 1.0, 1e-5), "current bar {:?}", img.get(50, 4));
    assert!(close(img.get(40, 4)[0], 0.5, 1e-5), "echo bar {:?}", img.get(40, 4));
}

#[test]
fn warp_stabilizer_removes_camera_jitter() {
    let (w, h) = (192, 128);
    let mk = |now: f64| ClipEnv { w, h, fps: 24.0, secs: 1.0, now, key: 0xC0FFEE, seq: (w as u32, h as u32), pic: shaky };
    let mut e = inst("warp_stabilizer");
    set(&mut e, "result", ParamValue::Choice(1)); // No Motion
    set(&mut e, "framing", ParamValue::Choice(0)); // Stabilize Only
    clear_stabilizer_cache();
    let frames: Vec<(Image, Image)> = [3usize, 9, 15, 21]
        .iter()
        .map(|&f| {
            let now = f as f64 / 24.0;
            let env = mk(now);
            let raw = env.render(now, 1.0);
            let mut st = raw.clone();
            crate::effects::apply(&mut st, &e, &FxCtx { seconds: now, env: Some(&env), ..cx() });
            (raw, st)
        })
        .collect();
    // compare the interior of every pair of frames: stabilised frames agree, raw ones don't
    let interior = |a: &Image, b: &Image| {
        let mut s = 0.0f64;
        let mut n = 0;
        for y in 24..h - 24 {
            for x in 24..w - 24 {
                s += (a.get(x, y)[0] - b.get(x, y)[0]).abs() as f64;
                n += 1;
            }
        }
        s / n as f64
    };
    let (mut raw_err, mut st_err) = (0.0, 0.0);
    for i in 1..frames.len() {
        raw_err += interior(&frames[0].0, &frames[i].0);
        st_err += interior(&frames[0].1, &frames[i].1);
    }
    assert!(st_err < raw_err * 0.25, "stabilised {st_err} vs raw {raw_err}");
    // Auto-scale framing hides the borders
    set(&mut e, "framing", ParamValue::Choice(2));
    let env = mk(9.0 / 24.0);
    let mut st = env.render(9.0 / 24.0, 1.0);
    crate::effects::apply(&mut st, &e, &FxCtx { seconds: 9.0 / 24.0, env: Some(&env), ..cx() });
    assert!(st.px.as_chunks::<4>().0.iter().all(|p| p[3] > 0.99), "auto-scale leaves no border");
    // the path cache is reused
    assert!(stabilizer_path(&env, 0.0, filmcraft_project::TrackMethod::Position, false).is_some());
}

#[test]
fn auto_reframe_follows_the_subject() {
    fn subject(x: f64, y: f64, _t: f64) -> [f32; 3] {
        // a textured blob on the right third of a flat frame
        let d = ((x - 150.0).powi(2) + (y - 45.0).powi(2)).sqrt();
        if d < 20.0 { texture(x, y) } else { [0.2, 0.2, 0.2] }
    }
    // 16:9 source in a 9:16 sequence → zoom to fill the height and pan to the subject
    let env = ClipEnv { w: 160, h: 90, fps: 24.0, secs: 1.0, now: 0.5, key: 0xBEEF, seq: (90, 160), pic: subject };
    let mut img = env.render(0.5, 1.0);
    crate::effects::apply(&mut img, &inst("auto_reframe"), &FxCtx { seconds: 0.5, env: Some(&env), ..cx() });
    // the layer centre now shows the subject side of the frame (texture, not the flat grey)
    let c = img.get(80, 45);
    assert!((c[0] - 0.2).abs() > 1e-3 || (img.get(85, 40)[0] - 0.2).abs() > 1e-3, "{c:?}");
    // same aspect → nothing to do
    let env = ClipEnv { seq: (160, 90), key: 0xBEF0, ..env };
    let raw = env.render(0.5, 1.0);
    let mut img = raw.clone();
    crate::effects::apply(&mut img, &inst("auto_reframe"), &FxCtx { seconds: 0.5, env: Some(&env), ..cx() });
    assert_eq!(img.px, raw.px);
}

// ------------------------------------------------------------------ through the compositor

mod pipeline {
    use super::*;
    use crate::{RenderOptions, SourceMap, render_sequence};
    use filmcraft_media::generators::GeneratorSource;
    use filmcraft_media::{DemoScene, Generator, MediaSource, SharedSource};
    use filmcraft_project::{ItemId, ItemKind, Label, MediaClip, MediaRef, SequenceSettings, TrackKind};
    use filmcraft_time::{FrameRate, TICKS_PER_SECOND, TimeRange};

    fn add(p: &mut Project, map: &mut SourceMap, g: GeneratorSource) -> ItemId {
        let info = g.info().clone();
        let generator = g.generator.clone();
        let id = p.add_item(
            &info.name.clone(),
            Label::Iris,
            ItemKind::Media(MediaClip {
                media: MediaRef::Generator(generator),
                info,
                interpret: Default::default(),
                mark_in: None,
                mark_out: None,
                markers: vec![],
                offline: false,
                proxy: None,
                identity: None,
            }),
            None,
        );
        map.0.insert(id, Arc::new(g) as SharedSource);
        id
    }

    fn place(p: &mut Project, seq: ItemId, track: usize, item: ItemId, fx: Vec<EffectInstance>) {
        let r = FrameRate::FPS_24;
        let mut ti = p.make_track_item(item, TrackKind::Video, Tick::ZERO, TimeRange::new(Tick::ZERO, r.tick_of(48)), r).unwrap();
        ti.scale_to_frame = true;
        for (i, e) in fx.into_iter().enumerate() {
            ti.effects.insert(i, e);
        }
        p.sequence_mut(seq).unwrap().video_tracks[track].items.push(ti);
    }

    #[test]
    fn temporal_and_track_effects_render_through_the_sequence() {
        let mut p = Project::new("vfx");
        let mut map = SourceMap::default();
        let ocean = add(&mut p, &mut map, GeneratorSource::demo(DemoScene::OceanSunset));
        let white = add(
            &mut p,
            &mut map,
            GeneratorSource::new(Generator::ColorMatte { color: [1.0, 1.0, 1.0, 1.0] }, 64, 36, FrameRate::FPS_24, Tick(10 * TICKS_PER_SECOND)),
        );
        let seq = p.new_sequence("s", SequenceSettings { width: 64, height: 36, frame_rate: FrameRate::FPS_24, ..Default::default() }, 2, 0, None);
        let mut pt = inst("posterize_time");
        setf(&mut pt, "rate", 6.0);
        place(&mut p, seq, 0, ocean, vec![pt]);
        let r = FrameRate::FPS_24;
        let a = render_sequence(&p, seq, r.tick_of(1), RenderOptions::default(), &map);
        let b = render_sequence(&p, seq, r.tick_of(3), RenderOptions::default(), &map);
        assert_eq!(a.px, b.px, "frames 1 and 3 share the 6 fps frame");
        let c0 = render_sequence(&p, seq, r.tick_of(0), RenderOptions::default(), &map);
        assert_eq!(a.px, c0.px);
        // Track Matte Key on V2 (white) using V1's luma
        let mut tm = inst("track_matte");
        set(&mut tm, "matte", ParamValue::Choice(1));
        set(&mut tm, "composite", ParamValue::Choice(1));
        place(&mut p, seq, 1, white, vec![tm]);
        let img = render_sequence(&p, seq, r.tick_of(0), RenderOptions::default(), &map);
        assert!(img.px.iter().all(|v| v.is_finite()));
        assert!(max_diff(&img, &c0) > 1e-3, "the matted white layer shows over V1");
        // Echo through the compositor stays finite and differs from the plain frame
        let mut p2 = Project::new("echo");
        let mut map2 = SourceMap::default();
        let ocean2 = add(&mut p2, &mut map2, GeneratorSource::demo(DemoScene::OceanSunset));
        let seq2 = p2.new_sequence("s", SequenceSettings { width: 64, height: 36, frame_rate: FrameRate::FPS_24, ..Default::default() }, 1, 0, None);
        let mut echo = inst("echo");
        setf(&mut echo, "time", -0.5);
        setf(&mut echo, "count", 2.0);
        set(&mut echo, "operator", ParamValue::Choice(6));
        place(&mut p2, seq2, 0, ocean2, vec![echo]);
        let e = render_sequence(&p2, seq2, r.tick_of(30), RenderOptions::default(), &map2);
        assert!(e.px.iter().all(|v| v.is_finite()));
    }
}

#[test]
fn transform_shutter_uses_the_sequence_frame_rate_without_env() {
    // An adjustment layer has no env: the shutter must still span one 60 fps frame at 360°, so
    // the frames just before the motion starts and just after it ends stay sharp (#1088).
    let mut e = inst("transform");
    set(&mut e, "shutter_override", ParamValue::Bool(true));
    setf(&mut e, "shutter_angle", 360.0);
    let mut a = Keyframe::new(Tick::from_seconds_f64(1.0), ParamValue::Vec2(filmcraft_geom::Vec2::new(8.0, 16.0)));
    a.interp = Interpolation::Linear;
    let mut b = Keyframe::new(Tick::from_seconds_f64(2.0), ParamValue::Vec2(filmcraft_geom::Vec2::new(40.0, 16.0)));
    b.interp = Interpolation::Linear;
    e.params.insert("position".into(), Param { value: ParamValue::Vec2(filmcraft_geom::Vec2::new(8.0, 16.0)), keyframes: vec![a, b] });
    let img = picture(32, 32);
    let mut off = e.clone();
    set(&mut off, "shutter_override", ParamValue::Bool(false));
    let delta = |frame: i32| {
        let s = f64::from(frame) / 60.0;
        let cx = FxCtx { t: Tick::from_seconds_f64(s), seconds: s, frame_rate: 60.0, ..cx() };
        max_diff(&run("transform", &off, &img, &cx), &run("transform", &e, &img, &cx))
    };
    for frame in [59, 121] {
        assert!(delta(frame) < 1e-4, "frame {frame} is blurred outside the motion ({})", delta(frame));
    }
    for frame in [60, 90, 120] {
        assert!(delta(frame) > 1e-3, "frame {frame} moves but is sharp");
    }
}

// ------------------------------------------------------------------ VHS Look

mod vhs_look {
    use std::f32::consts::TAU;

    use filmcraft_color::{Matrix, rgb_to_ycbcr, ycbcr_to_rgb};

    use super::*;

    /// A picture from display-encoded colours (`dec` turns them into the linear values the compositor holds).
    fn paint(w: usize, h: usize, f: impl Fn(usize, usize) -> [f32; 3]) -> Image {
        let mut img = Image::new(w, h);
        for y in 0..h {
            for x in 0..w {
                let l = dec(f(x, y));
                img.px[(y * w + x) * 4..(y * w + x) * 4 + 4].copy_from_slice(&[l[0], l[1], l[2], 1.0]);
            }
        }
        img
    }
    /// Y'CbCr (BT.601) of a pixel as the eye gets it: display-encoded, straight alpha.
    fn ycc(img: &Image, x: usize, y: usize) -> [f32; 3] {
        let c = enc(Image::unpremul(img.get(x, y)));
        rgb_to_ycbcr(c[0], c[1], c[2], Matrix::Bt601)
    }
    /// One row of one plane: 0 = Y', 1 = Cb, 2 = Cr.
    fn plane(img: &Image, y: usize, p: usize) -> Vec<f32> {
        (0..img.w).map(|x| ycc(img, x, y)[p]).collect()
    }

    /// The effect with every damage control at zero (only the two low-pass filters act), then `settings`.
    fn vhs(settings: &[(&str, f64)]) -> EffectInstance {
        let mut e = inst("vhs_look");
        for id in ["chroma_shift", "jitter", "noise", "head_switch", "dropouts"] {
            setf(&mut e, id, 0.0);
        }
        for (id, v) in settings {
            setf(&mut e, id, *v);
        }
        e
    }
    fn frame_cx(frame: i64) -> FxCtx<'static> {
        let t = Tick::from_seconds_f64(frame as f64 / 24.0);
        FxCtx { t, seconds: t.seconds(), ..cx() }
    }
    /// `e` applied to `img` at frame `frame` of a 24 fps clip, with the working image at `px_scale` of full size.
    fn at_scaled(img: &Image, e: &EffectInstance, frame: i64, px_scale: f32) -> Image {
        let mut out = img.clone();
        crate::effects::apply(&mut out, e, &FxCtx { px_scale, ..frame_cx(frame) });
        out
    }
    fn at(img: &Image, e: &EffectInstance, frame: i64) -> Image {
        at_scaled(img, e, frame, 1.0)
    }

    /// Where a profile first crosses `level`, in pixels (linear interpolation).
    fn crossing(profile: &[f32], level: f32) -> f32 {
        for (i, pair) in profile.windows(2).enumerate() {
            let (a, b) = (pair[0] - level, pair[1] - level);
            if a == 0.0 {
                return i as f32;
            }
            if a * b < 0.0 {
                return i as f32 + a / (a - b);
            }
        }
        f32::NAN
    }
    /// How soft an edge is: the distance from 10 % to 30 % of the way between the first and the last
    /// value. Only the side the profile starts on is read: past the edge a colour that bleeds onto black
    /// is clipped by the display's gamut, which says nothing about the filter.
    fn softness(profile: &[f32]) -> f32 {
        let (a, b) = (profile[0], profile[profile.len() - 1]);
        (crossing(profile, a + 0.3 * (b - a)) - crossing(profile, a + 0.1 * (b - a))).abs()
    }
    fn red_to_black(w: usize) -> Image {
        paint(w, 4, |x, _| if x < w / 2 { [1.0, 0.0, 0.0] } else { [0.0; 3] })
    }
    /// A step in brightness and colour at once, the two in opposite directions so that no mix of the
    /// brightness of one side with the colour of the other leaves the gamut.
    fn two_tone(w: usize) -> Image {
        let (left, right) = (ycbcr_to_rgb(0.7, 0.0, 0.2, Matrix::Bt601), ycbcr_to_rgb(0.3, 0.0, -0.2, Matrix::Bt601));
        paint(w, 4, |x, _| if x < w / 2 { left } else { right })
    }

    /// Amplitude of the sinusoid of `cycles` per picture width in the middle half of a row.
    fn amplitude(row: &[f32], cycles: f32) -> f32 {
        let w = row.len();
        let mid = &row[w / 4..3 * w / 4];
        let mean = mid.iter().sum::<f32>() / mid.len() as f32;
        let (mut s, mut c) = (0.0, 0.0);
        for (i, v) in mid.iter().enumerate() {
            let phase = TAU * cycles * ((w / 4 + i) as f32 + 0.5) / w as f32;
            s += (v - mean) * phase.sin();
            c += (v - mean) * phase.cos();
        }
        2.0 * s.hypot(c) / mid.len() as f32
    }
    /// How much of a grating of `cycles` per picture width the effect lets through (output over input amplitude).
    fn gain(w: usize, cycles: f32, e: &EffectInstance, chroma: bool) -> f32 {
        let src = paint(w, 3, |x, _| {
            let s = 0.2 * (TAU * cycles * (x as f32 + 0.5) / w as f32).sin();
            if chroma { ycbcr_to_rgb(0.5, s, 0.0, Matrix::Bt601) } else { [0.5 + s; 3] }
        });
        let p = usize::from(chroma);
        amplitude(&plane(&at(&src, e, 0), 1, p), cycles) / amplitude(&plane(&src, 1, p), cycles)
    }

    #[test]
    fn chroma_edge_is_wider_than_luma_edge() {
        // VHS keeps colour in a far narrower band than brightness, so on a red/black step the colour
        // edge is much softer than the brightness edge.
        let out = at(&red_to_black(480), &vhs(&[]), 0);
        let (luma, chroma) = (softness(&plane(&out, 2, 0)), softness(&plane(&out, 2, 2)));
        assert!(luma > 0.05, "luma edge {luma}");
        assert!(chroma > 4.0 * luma, "luma edge {luma} px, chroma edge {chroma} px");
    }

    #[test]
    fn colour_bleeds_and_shifts_right_of_an_edge() {
        // Chroma Shift delays the colour: its edge moves right of the brightness edge for a positive
        // value, left for a negative one, and stays put at 0.
        let src = two_tone(480);
        for shift in [0.0, 6.0, -6.0] {
            let out = at(&src, &vhs(&[("chroma_shift", shift)]), 0);
            let offset = crossing(&plane(&out, 2, 2), 0.0) - crossing(&plane(&out, 2, 0), 0.5);
            assert!((offset - shift as f32).abs() < 1.0, "shift {shift}: the colour edge is {offset} px from the brightness edge");
        }
        // on red against black, eight pixels past the edge the picture is dark, yet the colour is still red
        let out = at(&red_to_black(480), &vhs(&[("chroma_shift", 6.0)]), 0);
        let p = enc(Image::unpremul(out.get(248, 2)));
        assert!(p[0] > 0.1 && p[1] < 0.02, "past the edge: {p:?}");
    }

    #[test]
    fn gratings_below_the_cutoff_pass_and_above_it_fade_at_any_resolution() {
        // The cutoff is a number of cycles per picture width, so the same grating keeps its contrast
        // at 256 and at 512 pixels: the look does not depend on the resolution.
        let e = vhs(&[("luma_bw", 0.5)]);
        let mut low = Vec::new();
        let mut high = Vec::new();
        for w in [256, 512] {
            low.push(gain(w, 6.0, &e, false));
            high.push(gain(w, 64.0, &e, false));
        }
        assert!(low.iter().all(|g| *g > 0.85), "a grating below the cutoff is kept: {low:?}");
        assert!(high.iter().all(|g| *g < 0.3), "a grating well above the cutoff fades: {high:?}");
        assert!((low[0] - low[1]).abs() < 0.1 && (high[0] - high[1]).abs() < 0.1, "256 vs 512 px: {low:?} {high:?}");
    }

    #[test]
    fn the_cutoff_sits_where_the_megahertz_setting_says() {
        // 0.8 MHz across the 52.6 µs active line is 42 cycles per picture width, where a low-pass
        // is 3 dB down; 1.6 MHz moves that point out of the way.
        let g = gain(512, 42.0, &vhs(&[("luma_bw", 0.8)]), false);
        assert!((0.6..0.8).contains(&g), "gain at the cutoff {g}");
        assert!(gain(512, 42.0, &vhs(&[("luma_bw", 1.6)]), false) > 0.85);
        // and chroma has a band of its own: 0.2 MHz is 10.5 cycles per width, so 22 is twice that
        let c = gain(512, 22.0, &vhs(&[("chroma_bw", 0.2)]), true);
        assert!(c < 0.45, "a chroma grating at twice its cutoff: {c}");
        assert!(gain(512, 22.0, &vhs(&[("luma_bw", 5.0)]), false) > 0.9, "the same luma grating passes");
    }

    #[test]
    fn same_frame_same_noise_new_frame_new_noise() {
        // Renders repeat bit for bit, but each frame gets its own noise, jitter and dropouts, and
        // each clip and each Seed its own pattern (#906 and #956 for Noise).
        let src = picture(480, 8);
        let damaged = vhs(&[("noise", 60.0), ("jitter", 4.0), ("dropouts", 60.0), ("head_switch", 6.0)]);
        let a = at(&src, &damaged, 10);
        assert_eq!(a.px, at(&src, &damaged, 10).px, "the same frame twice");
        assert!(max_diff(&a, &at(&src, &damaged, 11)) > 0.02, "the next frame is a copy");
        let grain = vhs(&[("noise", 60.0)]);
        assert!(max_diff(&at(&src, &grain, 10), &at(&src, &grain, 11)) > 0.02, "noise repeats from frame to frame");
        // Seed
        let mut reseeded = grain.clone();
        setf(&mut reseeded, "seed", 7.0);
        assert!(max_diff(&at(&src, &grain, 10), &at(&src, &reseeded, 10)) > 0.02, "Seed does nothing");
        // another clip, same picture and time
        let clip = |key: u64| {
            let env = ClipEnv { w: 480, h: 8, fps: 24.0, secs: 10.0, now: 0.0, key, seq: (480, 8), pic: |_, _, _| [0.2; 3] };
            let mut out = src.clone();
            crate::effects::apply(&mut out, &grain, &FxCtx { env: Some(&env), ..frame_cx(10) });
            out
        };
        assert_eq!(clip(1).px, clip(1).px);
        assert!(max_diff(&clip(1), &clip(2)) > 0.02, "two clips share their noise");
        // nothing time-varying switched on: every frame is the same
        let calm = vhs(&[("chroma_shift", 3.0)]);
        assert_eq!(at(&src, &calm, 10).px, at(&src, &calm, 11).px);
    }

    #[test]
    fn head_switching_stays_in_the_bottom_band() {
        // Head Switching damages the last N lines of the 480-line raster and nothing above them,
        // however much other damage is on.
        let (w, h) = (240, 240);
        let src = picture(w, h);
        let base = vhs(&[("noise", 30.0), ("jitter", 2.0), ("dropouts", 30.0)]);
        let clean = at(&src, &base, 3);
        let changed = |lines: f64| -> Vec<usize> {
            let mut e = base.clone();
            setf(&mut e, "head_switch", lines);
            let out = at(&src, &e, 3);
            (0..h).filter(|y| out.px[y * w * 4..(y + 1) * w * 4] != clean.px[y * w * 4..(y + 1) * w * 4]).collect()
        };
        // 240 rows: two lines of the raster per row
        let wide = changed(24.0);
        let narrow = changed(8.0);
        assert_eq!(wide.last(), Some(&(h - 1)));
        assert_eq!(narrow.last(), Some(&(h - 1)));
        assert!(wide.first().is_some_and(|y| (h - 12 - 2..=h - 12 + 1).contains(y)), "24 lines start at row {:?}", wide.first());
        assert!(narrow.first().is_some_and(|y| (h - 4 - 2..=h - 4 + 1).contains(y)), "8 lines start at row {:?}", narrow.first());
        // the damage is real at the bottom of the band
        let mut e = base.clone();
        setf(&mut e, "head_switch", 24.0);
        let out = at(&src, &e, 3);
        let last = (h - 1) * w * 4..h * w * 4;
        let d = out.px[last.clone()].iter().zip(&clean.px[last]).map(|(a, b)| (a - b).abs()).sum::<f32>() / (w * 4) as f32;
        assert!(d > 0.02, "last row changed by {d} on average");
        // on a flat picture a sideways shear changes nothing, so what shows is the noise of the band
        let flat = paint(w, h, |_, _| [0.4; 3]);
        let out = at(&flat, &vhs(&[("head_switch", 24.0)]), 3);
        let spread = |y: usize| {
            let row = plane(&out, y, 0);
            let mean = row.iter().sum::<f32>() / row.len() as f32;
            (row.iter().map(|v| (v - mean) * (v - mean)).sum::<f32>() / row.len() as f32).sqrt()
        };
        assert!(spread(h - 1) > 0.05, "no noise in the last row of the band: {}", spread(h - 1));
        assert!(max_diff(&out, &flat) > 0.05 && (0..h - 14).all(|y| spread(y) < 1e-4), "the picture above the band must stay flat");
        // the lines of the band are also torn sideways, by up to about 5 % of the width at 24 lines. The
        // colour edge shows it cleanly: the band's noise is in the brightness only
        let (left, right) = (ycbcr_to_rgb(0.5, 0.0, 0.2, Matrix::Bt601), ycbcr_to_rgb(0.5, 0.0, -0.2, Matrix::Bt601));
        let wide = paint(480, h, |x, _| if x < 240 { left } else { right });
        let out = at(&wide, &vhs(&[("head_switch", 24.0)]), 3);
        let edge = |y: usize| crossing(&plane(&out, y, 2), 0.0) - 239.5;
        assert!((0..h - 14).all(|y| edge(y).abs() < 0.05), "the picture above the band moved");
        assert!((12.0..36.0).contains(&edge(h - 1)), "the last line moved {} px of 480", edge(h - 1));
    }

    #[test]
    fn line_jitter_moves_whole_lines_within_the_set_amount() {
        // Every line is displaced sideways by up to Line Jitter pixels, and by none at 0.
        let (w, h) = (480, 960);
        let src = paint(w, h, |x, _| if x < w / 2 { [0.1; 3] } else { [0.9; 3] });
        let edges = |jitter: f64, px_scale: f32| -> Vec<f32> {
            let out = at_scaled(&src, &vhs(&[("jitter", jitter)]), 4, px_scale);
            (0..h).map(|y| crossing(&plane(&out, y, 0), 0.5)).collect()
        };
        const EDGE: f32 = 239.5; // between pixels 239 and 240
        assert!(edges(0.0, 1.0).iter().all(|x| (x - EDGE).abs() < 0.01), "at 0 the edge must not move");
        let moved = edges(6.0, 1.0);
        let spread = moved.iter().map(|x| (x - EDGE).abs()).fold(0.0, f32::max);
        assert!(spread > 3.0, "the lines barely move: {spread}");
        assert!(spread < 6.5, "a line moved {spread} px, more than the 6 asked for");
        // not every line goes the same way
        assert!(moved.iter().any(|x| *x < EDGE - 1.0) && moved.iter().any(|x| *x > EDGE + 1.0));
        // on a half-size working image the same setting is half as many pixels
        let spread_half = edges(6.0, 0.5).iter().map(|x| (x - EDGE).abs()).fold(0.0, f32::max);
        assert!(spread_half < 3.5, "a proxy render moved a line {spread_half} px");
    }

    #[test]
    fn dropouts_are_short_bright_streaks_scaled_by_the_setting() {
        // Dropouts add short bright streaks to some lines: none at 0 %, more at 100 % than at 10 %,
        // and never a whole line.
        let src = paint(480, 480, |_, _| [0.55, 0.25, 0.25]);
        let hit = |pct: f64| -> (usize, usize) {
            let out = at(&src, &vhs(&[("dropouts", pct)]), 5);
            let widest = |y: usize| (0..480).filter(|x| ycc(&out, *x, y)[0] > 0.7).count();
            let rows: Vec<usize> = (0..480).map(widest).filter(|n| *n > 0).collect();
            (rows.len(), rows.into_iter().max().unwrap_or(0))
        };
        assert_eq!(hit(0.0).0, 0);
        let (few, _) = hit(10.0);
        let (many, widest) = hit(100.0);
        assert!(many >= 5, "{many} lines hit at 100 %");
        assert!(few < many, "{few} lines at 10 %, {many} at 100 %");
        assert!(widest > 8 && widest < 480 * 2 / 5, "the widest streak is {widest} px of 480");
        // the streak is bright snow, not a brighter shade of the picture's red
        let out = at(&src, &vhs(&[("dropouts", 100.0)]), 5);
        let snow: Vec<[f32; 3]> = (0..480 * 480).map(|i| ycc(&out, i % 480, i / 480)).filter(|p| p[0] > 0.8).collect();
        assert!(
            !snow.is_empty() && snow.iter().all(|p| p[2].abs() < 0.05),
            "snow keeps the picture's colour: {:?}",
            snow.iter().map(|p| p[2]).fold(0.0, f32::max)
        );
        assert!(ycc(&src, 0, 0)[2] > 0.1, "the picture itself must be red enough to tell");
    }

    #[test]
    fn noise_grain_follows_the_bandwidth_not_the_pixel_count() {
        // Tape noise passes through the same narrow band as the picture: a lower luma bandwidth
        // gives coarser grain, and the grain is the same fraction of the picture width at any
        // resolution.
        let grain = |w: usize, h: usize, bw: f64| -> f32 {
            let flat = paint(w, h, |_, _| [0.4; 3]);
            let out = at_scaled(&flat, &vhs(&[("noise", 50.0), ("luma_bw", bw)]), 2, w as f32 / 960.0);
            let rows: Vec<Vec<f32>> = (0..h).map(|y| plane(&out, y, 0)).collect();
            let mean = rows.iter().flatten().sum::<f32>() / (w * h) as f32;
            let var: f32 = rows.iter().flatten().map(|v| (v - mean) * (v - mean)).sum();
            let mut before = 1.0;
            for lag in 1..w / 2 {
                let r: f32 = rows.iter().map(|row| (0..w - lag).map(|x| (row[x] - mean) * (row[x + lag] - mean)).sum::<f32>()).sum::<f32>() / var;
                if r < 0.5 {
                    return (lag as f32 - 1.0 + (before - 0.5) / (before - r)) / w as f32;
                }
                before = r;
            }
            f32::NAN
        };
        let (full, half) = (grain(960, 64, 1.0), grain(480, 32, 1.0));
        assert!((full / half - 1.0).abs() < 0.3, "grain {full} of the width at 960 px, {half} at 480 px");
        let (coarse, fine) = (grain(960, 64, 0.5), grain(960, 64, 5.0));
        assert!(coarse > 3.0 * fine, "grain at 0.5 MHz {coarse}, at 5 MHz {fine}");
        // and the level follows the setting: none at 0, more at 80 % than at 20 %
        let flat = paint(480, 32, |_, _| [0.4; 3]);
        let spread = |pct: f64| {
            let out = at(&flat, &vhs(&[("noise", pct)]), 2);
            let v: Vec<f32> = out.px.chunks(4).map(|p| p[0]).collect();
            let mean = v.iter().sum::<f32>() / v.len() as f32;
            (v.iter().map(|x| (x - mean) * (x - mean)).sum::<f32>() / v.len() as f32).sqrt()
        };
        assert!(spread(0.0) < 1e-4);
        assert!(spread(80.0) > 2.0 * spread(20.0), "noise 20 % {} vs 80 % {}", spread(20.0), spread(80.0));
    }

    #[test]
    fn half_resolution_looks_like_full_resolution_scaled_down() {
        // Shift, jitter and the head-switching band are lengths: on a proxy render (px_scale 0.5)
        // they must come out half as many pixels, so the picture matches the full-size one shrunk.
        fn scene(x: f64, y: f64, _t: f64) -> [f32; 3] {
            let (u, v) = (x / 960.0, y / 540.0);
            let tone = (0.8 + 0.2 * (v * 6.0).sin()) as f32;
            let c = if (0.3..0.5).contains(&u) {
                [0.9, 0.15, 0.1]
            } else if (0.62..0.8).contains(&u) {
                [0.1, 0.3, 0.9]
            } else {
                [0.3 + 0.4 * u as f32; 3]
            };
            dec([c[0] * tone, c[1] * tone, c[2] * tone])
        }
        let env = ClipEnv { w: 960, h: 540, fps: 24.0, secs: 2.0, now: 0.0, key: 5, seq: (960, 540), pic: scene };
        let e = vhs(&[("chroma_shift", 12.0), ("jitter", 8.0), ("head_switch", 20.0)]);
        let (full, half) = (env.render(0.0, 1.0), env.render(0.0, 0.5));
        let want = at(&full, &e, 6).downsample2();
        let mean_error = |a: &Image, b: &Image| a.px.iter().zip(&b.px).map(|(x, y)| (x - y).abs()).sum::<f32>() / a.px.len() as f32;
        let right = mean_error(&want, &at_scaled(&half, &e, 6, 0.5));
        let wrong = mean_error(&want, &at_scaled(&half, &e, 6, 1.0));
        assert!(right < 0.03, "half resolution is off by {right}");
        assert!(right < 0.5 * wrong, "scaling the lengths changed nothing: {right} vs {wrong} without it");
    }

    #[test]
    fn neutral_settings_keep_the_picture() {
        // Bandwidths opened above what this small picture can hold and every damage control at 0:
        // what is left is the Y'CbCr round trip, which must lose nothing.
        let src = picture(64, 24);
        let out = at(&src, &vhs(&[("luma_bw", 5.0), ("chroma_bw", 1.5)]), 0);
        let d = max_diff(&out, &src);
        assert!(d < 1e-4, "round trip changed the picture by {d}");
    }

    #[test]
    fn alpha_is_untouched_and_transparent_pixels_stay_out_of_the_picture() {
        // Alpha is never changed, a transparent pixel keeps whatever it held, and the colour hidden
        // under it does not bleed into the pixels beside it.
        let (w, h) = (96, 6);
        let flat = [0.2, 0.5, 0.8];
        let build = |hidden: [f32; 3]| {
            let mut img = Image::new(w, h);
            for (i, p) in img.px.chunks_mut(4).enumerate() {
                // the transparent part holds a colour anyway: premultiplied data may carry one
                let (c, a) = match i % w {
                    0..32 => (hidden, 0.0),
                    32..48 => (flat, 0.5),
                    _ => (flat, 1.0),
                };
                let k = if a == 0.0 { 1.0 } else { a };
                p.copy_from_slice(&[c[0] * k, c[1] * k, c[2] * k, a]);
            }
            img
        };
        let e = vhs(&[("luma_bw", 0.5), ("chroma_bw", 0.05), ("chroma_shift", 4.0), ("jitter", 3.0)]);
        let (a, b) = (build([5.0, 0.0, 0.9]), build([0.0, 0.7, 0.0]));
        let (oa, ob) = (at(&a, &e, 8), at(&b, &e, 8));
        for (out, src) in [(&oa, &a), (&ob, &b)] {
            for i in 0..w * h {
                assert_eq!(out.px[i * 4 + 3].to_bits(), src.px[i * 4 + 3].to_bits(), "alpha changed at {i}");
                if i % w < 32 {
                    assert!(
                        out.px[i * 4..i * 4 + 3].iter().zip(&src.px[i * 4..i * 4 + 3]).all(|(p, q)| p.to_bits() == q.to_bits()),
                        "transparent pixel {i} changed"
                    );
                } else {
                    let (o, s) = (Image::unpremul(out.get(i % w, i / w)), Image::unpremul(src.get(i % w, i / w)));
                    assert!((0..3).all(|k| (o[k] - s[k]).abs() < 2e-3), "pixel {i}: {o:?} for {s:?}");
                }
            }
        }
        for i in 32 * 4..w * 4 {
            assert_eq!(oa.px[i].to_bits(), ob.px[i].to_bits(), "the colour under the transparent area reached the picture");
        }
        // half transparent all over, with no clear pixel in sight: the colours must not fade with the alpha
        let veil = Image::filled(w, h, [flat[0] * 0.5, flat[1] * 0.5, flat[2] * 0.5, 0.5]);
        let out = at(&veil, &e, 8);
        for i in 0..w * h {
            let o = Image::unpremul(out.get(i % w, i / w));
            assert!((0..3).all(|k| (o[k] - flat[k]).abs() < 2e-3), "pixel {i} of the veil: {o:?} for {flat:?}");
            assert_eq!(out.px[i * 4 + 3], 0.5, "alpha of the veil at {i}");
        }
    }

    #[test]
    fn a_value_beyond_the_range_acts_as_the_end_of_the_range() {
        // A project file or script can hold any number. Line Jitter 1e6 must not shear the picture a
        // million pixels and Noise -40 % must not turn the grain upside down: each control stops at
        // the ends of the range it is declared with.
        let src = picture(96, 40);
        let def = find_effect("vhs_look").unwrap();
        let damaged = |id: &str, v: f64| {
            let mut e = vhs(&[("noise", 40.0), ("jitter", 3.0), ("head_switch", 6.0), ("dropouts", 40.0), ("chroma_shift", 2.0)]);
            setf(&mut e, id, v);
            at(&src, &e, 3)
        };
        for p in &def.params {
            let ParamKind::Float { min, max, .. } = p.kind else { continue };
            let reach = 1000.0 * (max - min);
            assert_eq!(damaged(p.id, max + reach).px, damaged(p.id, max).px, "{} above its range", p.id);
            assert_eq!(damaged(p.id, min - reach).px, damaged(p.id, min).px, "{} below its range", p.id);
        }
    }

    #[test]
    fn hostile_input_neither_panics_nor_returns_garbage() {
        let src = picture(48, 20);
        let sane = |img: &Image, src: &Image, what: &str| {
            assert_eq!((img.w, img.h, img.px.len()), (src.w, src.h, src.px.len()), "{what}: size");
            for (o, i) in img.px.as_chunks::<4>().0.iter().zip(src.px.as_chunks::<4>().0) {
                assert!(o.iter().all(|v| v.is_finite()), "{what}: {o:?}");
                assert_eq!(o[3], i[3], "{what}: alpha");
                assert!(o[..3].iter().all(|v| (0.0..=1.0 + 1e-4).contains(v)), "{what}: {o:?}");
            }
        };
        // every control: NaN, infinities, negative and absurd values, with all the damage on
        for id in ["luma_bw", "chroma_bw", "chroma_shift", "jitter", "noise", "head_switch", "dropouts", "seed"] {
            for v in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY, -50.0, 0.0, 1e30, 1e300] {
                let mut e = vhs(&[("noise", 40.0), ("jitter", 3.0), ("head_switch", 6.0), ("dropouts", 40.0), ("chroma_shift", 2.0)]);
                setf(&mut e, id, v);
                sane(&at(&src, &e, 3), &src, &format!("{id} = {v}"));
            }
        }
        // a working scale and a time that are not sane
        let e = vhs(&[("noise", 40.0), ("jitter", 3.0), ("head_switch", 6.0), ("dropouts", 40.0), ("chroma_shift", 2.0)]);
        for px_scale in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY, 0.0, -1.0, 1e9] {
            sane(&at_scaled(&src, &e, 3, px_scale), &src, &format!("px_scale {px_scale}"));
        }
        // a working scale that is not a positive number is taken as full size, not as "no shift at all"
        let full = at_scaled(&src, &e, 3, 1.0);
        for px_scale in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY, 0.0, -1.0] {
            assert_eq!(at_scaled(&src, &e, 3, px_scale).px, full.px, "px_scale {px_scale}");
        }
        for t in [i64::MIN, -1, i64::MAX] {
            let mut out = src.clone();
            crate::effects::apply(&mut out, &e, &FxCtx { t: Tick(t), ..cx() });
            sane(&out, &src, &format!("time {t}"));
        }
        // tiny, transparent, half-transparent and odd-sized images
        let sizes = [(1, 1), (1, 9), (9, 1), (2, 2), (3, 7), (479, 3), (4, 481)];
        let mut images: Vec<Image> = sizes.iter().map(|(w, h)| Image::filled(*w, *h, [0.2, 0.4, 0.6, 1.0])).collect();
        images.push(Image::filled(5, 5, [0.1, 0.2, 0.3, 0.2]));
        images.push(Image::filled(9, 9, [0.7, f32::NAN, 5.0, 0.0])); // transparent, garbage colour
        images.push(Image::filled(9, 9, [0.0; 4]));
        for img in &images {
            let mut out = img.clone();
            crate::effects::apply(&mut out, &e, &frame_cx(3));
            if img.px.iter().all(|v| v.is_finite()) {
                sane(&out, img, &format!("{}x{}", img.w, img.h));
            }
            if img.px.as_chunks::<4>().0.iter().all(|p| p[3] == 0.0) {
                assert!(out.px.iter().zip(&img.px).all(|(a, b)| a.to_bits() == b.to_bits()), "transparent pixels must stay as they were");
            }
        }
        // a pixel that is not a number (or whose colour overflows when divided by its tiny alpha) is left
        // as it was and is not used as a sample: the rest of its row is processed as if it were not there
        let mut clean = Image::new(64, 4);
        for (i, p) in clean.px.chunks_mut(4).enumerate() {
            p.copy_from_slice(&if i % 64 < 40 { [0.2, 0.4, 0.6, 1.0] } else { [0.6, 0.4, 0.2, 1.0] });
        }
        let mut poisoned = clean.clone();
        poisoned.px[..3].fill(f32::NAN);
        poisoned.px[10 * 4..11 * 4].copy_from_slice(&[0.2, 0.4, 0.6, f32::INFINITY]);
        poisoned.px[20 * 4..21 * 4].copy_from_slice(&[1e34, 1e34, 1e34, 1e-5]);
        let shifted = vhs(&[("luma_bw", 0.5), ("chroma_bw", 0.05), ("chroma_shift", 4.0)]);
        let (good, bad) = (at(&clean, &shifted, 3), at(&poisoned, &shifted, 3));
        assert!(max_diff(&good, &clean) > 0.02, "the effect must show on this picture for the check to mean anything");
        for x in 0..64 {
            let (got, want_good, own) = (&bad.px[x * 4..x * 4 + 4], &good.px[x * 4..x * 4 + 4], &poisoned.px[x * 4..x * 4 + 4]);
            if [0, 10, 20].contains(&x) {
                assert!(got.iter().zip(own).all(|(a, b)| a.to_bits() == b.to_bits()), "broken pixel {x} was changed: {got:?}");
            } else {
                assert!(got.iter().zip(want_good).all(|(a, b)| (a - b).abs() < 2e-3), "pixel {x} of a row with broken ones: {got:?} for {want_good:?}");
            }
        }
        // empty images and buffers that do not match their size are refused, not indexed
        let mut empty = Image { w: 0, h: 0, px: Vec::new() };
        crate::effects::apply(&mut empty, &e, &frame_cx(3));
        let mut broken = Image { w: 4, h: 4, px: vec![0.5; 10] };
        crate::effects::apply(&mut broken, &e, &frame_cx(3));
        assert_eq!(broken.px, vec![0.5; 10]);
    }
}
