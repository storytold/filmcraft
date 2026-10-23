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
    "video_denoise",
    "long_shadow",
    "brush_strokes",
    "color_emboss",
    "roughen_edges",
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

// ------------------------------------------------------------------ Video De-Noise

mod video_denoise {
    use std::ops::Range;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;

    /// Half-width of the uniform noise added to every channel and pixel (standard deviation ≈ 0.035).
    const GRAIN: f32 = 0.06;
    const BRIGHT: [f32; 3] = [0.85, 0.80, 0.30];
    const DARK: [f32; 3] = [0.25, 0.30, 0.45];

    // Pictures are display-encoded colours; `dec` turns them into the linear values the compositor holds.

    /// Two tinted halves meeting at x = 48.
    fn flat(x: f64) -> [f32; 3] {
        if x < 48.0 { [0.30, 0.40, 0.55] } else { [0.65, 0.45, 0.35] }
    }
    /// A bar 14 px wide moving 4 px a frame over a flat background.
    fn bar(x: f64, t: f64) -> [f32; 3] {
        let left = 20.0 + 96.0 * t;
        if x >= left && x < left + 14.0 { BRIGHT } else { DARK }
    }
    /// A hard cut at 0.49 s: the bright half switches sides.
    fn shots(x: f64, t: f64) -> [f32; 3] {
        if (x < 48.0) == (t < 0.49) { BRIGHT } else { DARK }
    }
    /// Five colours that share nothing in brightness (0.20 to 0.80) or in colour, one per frame in
    /// turn: no frame within four of another looks like it.
    fn unrelated(x: f64, t: f64) -> [f32; 3] {
        const PALETTE: [[f32; 3]; 5] = [[0.10, 0.204, 0.45], [0.60, 0.296, 0.15], [0.25, 0.584, 0.40], [0.95, 0.556, 0.70], [0.75, 0.845, 0.50]];
        let f = (t * 24.0).round() as i64;
        let side = if x < 48.0 { 0 } else { 2 };
        PALETTE[(f + side).rem_euclid(5) as usize]
    }
    /// A static fine texture as faint as the noise itself.
    fn texture_static(x: f64, y: f64) -> [f32; 3] {
        std::array::from_fn(|k| 0.5 + (hash01(x.floor() as i64, y.floor() as i64, k as i64, 5) - 0.5) * 0.09)
    }
    /// Noise that changes with the frame, the pixel and the channel.
    fn grained(c: [f32; 3], x: f64, y: f64, t: f64) -> [f32; 3] {
        let f = (t * 24.0).round() as i64;
        std::array::from_fn(|k| c[k] + (hash01(x.floor() as i64, y.floor() as i64, f * 3 + k as i64, 41) - 0.5) * 2.0 * GRAIN)
    }

    fn flat_clean(x: f64, _: f64, _: f64) -> [f32; 3] {
        dec(flat(x))
    }
    fn flat_noisy(x: f64, y: f64, t: f64) -> [f32; 3] {
        dec(grained(flat(x), x, y, t))
    }
    fn bar_clean(x: f64, _: f64, t: f64) -> [f32; 3] {
        dec(bar(x, t))
    }
    fn bar_noisy(x: f64, y: f64, t: f64) -> [f32; 3] {
        dec(grained(bar(x, t), x, y, t))
    }
    fn shots_clean(x: f64, _: f64, t: f64) -> [f32; 3] {
        dec(shots(x, t))
    }
    fn shots_noisy(x: f64, y: f64, t: f64) -> [f32; 3] {
        dec(grained(shots(x, t), x, y, t))
    }
    fn unrelated_noisy(x: f64, y: f64, t: f64) -> [f32; 3] {
        dec(grained(unrelated(x, t), x, y, t))
    }
    fn texture_noisy(x: f64, y: f64, t: f64) -> [f32; 3] {
        dec(grained(texture_static(x, y), x, y, t))
    }
    /// Two tinted halves meeting at x = 48; the left one has a negative red, as an out-of-gamut colour does.
    fn gamut(x: f64) -> [f32; 3] {
        if x < 48.0 { [-0.30, 0.40, 0.55] } else { [0.65, 0.45, 0.35] }
    }
    /// Like `dec`, with the curve mirrored around 0 so that a negative channel stays negative.
    fn dec_mirrored(c: [f32; 3]) -> [f32; 3] {
        let linear = dec(c.map(f32::abs));
        std::array::from_fn(|k| linear[k].copysign(c[k]))
    }
    fn gamut_clean(x: f64, _: f64, _: f64) -> [f32; 3] {
        dec_mirrored(gamut(x))
    }
    fn gamut_noisy(x: f64, y: f64, t: f64) -> [f32; 3] {
        dec_mirrored(grained(gamut(x), x, y, t))
    }
    /// Two saturated colours meeting at x = 48. Each has one channel on the gamut boundary: its
    /// display value is below 0, so `dec` makes it exactly 0 under any grain.
    fn deep(x: f64) -> [f32; 3] {
        if x < 48.0 { [-0.15, 0.30, 0.30] } else { [0.30, 0.30, -0.15] }
    }
    /// `deep` under the usual grain; `dec` keeps every linear channel in 0..=1.
    fn deep_noisy(x: f64, y: f64, t: f64) -> [f32; 3] {
        dec(grained(deep(x), x, y, t))
    }

    /// A clip environment that hands back neighbouring frames at the working scale and counts how
    /// many it was asked for.
    struct Probe {
        inner: ClipEnv,
        scale: f32,
        asked: AtomicUsize,
    }
    impl Probe {
        fn new(pic: fn(f64, f64, f64) -> [f32; 3], now: f64, scale: f32) -> Self {
            Probe { inner: ClipEnv { w: 96, h: 64, fps: 24.0, secs: 2.0, now, key: 0xDE01, seq: (96, 64), pic }, scale, asked: AtomicUsize::new(0) }
        }
        fn asked(&self) -> usize {
            self.asked.load(Ordering::Relaxed)
        }
    }
    impl FxEnv for Probe {
        fn frame(&self, dt: f64, _: &EffectInstance) -> Option<Image> {
            self.asked.fetch_add(1, Ordering::Relaxed);
            Some(self.inner.render(((self.inner.now + dt) * self.inner.fps).round() / self.inner.fps, self.scale))
        }
        fn source_frame(&self, dt: f64, scale: f32) -> Option<Image> {
            self.inner.source_frame(dt, scale)
        }
        fn track(&self, index: usize) -> Option<Image> {
            self.inner.track(index)
        }
        fn layer_to_output(&self) -> Affine {
            self.inner.layer_to_output()
        }
        fn clip_seconds(&self) -> f64 {
            self.inner.clip_seconds()
        }
        fn clip_offset(&self) -> f64 {
            self.inner.clip_offset()
        }
        fn frame_rate(&self) -> f64 {
            self.inner.frame_rate()
        }
        fn media_timecode(&self) -> String {
            self.inner.media_timecode()
        }
        fn file_name(&self) -> String {
            self.inner.file_name()
        }
        fn sequence_name(&self) -> String {
            self.inner.sequence_name()
        }
        fn sequence_size(&self) -> (u32, u32) {
            self.inner.sequence_size()
        }
        fn source_size(&self) -> (u32, u32) {
            self.inner.source_size()
        }
        fn clip_key(&self) -> u64 {
            self.inner.clip_key()
        }
    }

    fn denoise(luma: f64, chroma: f64, temporal: f64, detail: f64) -> EffectInstance {
        let mut e = inst("video_denoise");
        setf(&mut e, "luma", luma);
        setf(&mut e, "chroma", chroma);
        setf(&mut e, "temporal", temporal);
        setf(&mut e, "detail", detail);
        e
    }
    /// The effect applied to the clip's picture at `now`, rendered at `scale` of full size.
    fn run_on(env: &Probe, e: &EffectInstance, scale: f32) -> Image {
        let t = env.inner.now;
        let mut img = env.inner.render(t, scale);
        crate::effects::apply(&mut img, e, &FxCtx { seconds: t, px_scale: scale, env: Some(env), ..cx() });
        img
    }
    fn render_of(pic: fn(f64, f64, f64) -> [f32; 3], now: f64, scale: f32) -> Image {
        Probe::new(pic, now, scale).inner.render(now, scale)
    }

    type Area = (Range<usize>, Range<usize>);

    /// Mean absolute and root-mean-square display-space error of `img` against `clean` over `area`.
    fn error(img: &Image, clean: &Image, (xs, ys): &Area) -> (f32, f32) {
        let (mut abs, mut sq, mut n) = (0.0f64, 0.0f64, 0usize);
        for y in ys.clone() {
            for x in xs.clone() {
                let (a, b) = (enc_px(img, x, y), enc_px(clean, x, y));
                for k in 0..3 {
                    let d = f64::from(a[k] - b[k]);
                    abs += d.abs();
                    sq += d * d;
                    n += 1;
                }
            }
        }
        ((abs / n as f64) as f32, (sq / n as f64).sqrt() as f32)
    }
    /// Display-space luma and the two colour differences of one pixel.
    fn yuv(img: &Image, x: usize, y: usize) -> [f32; 3] {
        let p = enc_px(img, x, y);
        let l = luma([p[0], p[1], p[2]]);
        [l, p[2] - l, p[0] - l]
    }
    /// Root-mean-square error of one of the `yuv` planes against `clean` over `area`.
    fn plane_error(img: &Image, clean: &Image, plane: usize, (xs, ys): &Area) -> f32 {
        let (mut sq, mut n) = (0.0f64, 0usize);
        for y in ys.clone() {
            for x in xs.clone() {
                let d = f64::from(yuv(img, x, y)[plane] - yuv(clean, x, y)[plane]);
                sq += d * d;
                n += 1;
            }
        }
        (sq / n as f64).sqrt() as f32
    }
    /// The mean of the clip's frame before, this one and the next: plain temporal averaging.
    fn plain_average(env: &Probe) -> Image {
        let frames: Vec<Image> = [-1.0, 0.0, 1.0].iter().filter_map(|k| env.frame(k / 24.0, &inst("video_denoise"))).collect();
        let mut out = Image::new(frames[0].w, frames[0].h);
        for f in &frames {
            for (o, v) in out.px.iter_mut().zip(&f.px) {
                *o += v / frames.len() as f32;
            }
        }
        out
    }

    const LEFT: fn() -> Area = || (8..40, 8..56);
    const RIGHT: fn() -> Area = || (56..88, 8..56);

    #[test]
    fn static_noise_drops_and_the_static_edge_survives() {
        let env = Probe::new(flat_noisy, 0.5, 1.0);
        let clean = render_of(flat_clean, 0.5, 1.0);
        let input = env.inner.render(0.5, 1.0);
        let out = run_on(&env, &denoise(30.0, 50.0, 1.0, 50.0), 1.0);
        for area in [LEFT(), RIGHT()] {
            let (before, after) = (error(&input, &clean, &area).1, error(&out, &clean, &area).1);
            assert!(after < before / 3.0, "noise {before} -> {after}");
        }
        // every neighbouring frame that matches takes more of it away
        let left_noise = |radius: f64| error(&run_on(&env, &denoise(30.0, 50.0, radius, 50.0), 1.0), &clean, &LEFT()).1;
        let (r0, r1, r2) = (left_noise(0.0), left_noise(1.0), left_noise(2.0));
        assert!(r1 < r0 * 0.85 && r2 < r1 * 0.85, "noise left with radius 0, 1, 2: {r0} {r1} {r2}");
        // the colour edge at x = 48 is neither smeared nor lost: next to it the noise falls just as far
        let edge = (44..52, 8..56);
        let (before, after) = (error(&input, &clean, &edge).0, error(&out, &clean, &edge).0);
        assert!(after < before * 0.5, "edge error {before} -> {after}");
    }

    #[test]
    fn a_moving_edge_does_not_ghost() {
        let env = Probe::new(bar_noisy, 0.25, 1.0); // the bar spans x = 44..58, 4 px further each frame
        let clean = render_of(bar_clean, 0.25, 1.0);
        let out = run_on(&env, &denoise(30.0, 50.0, 1.0, 50.0), 1.0);
        let average = plain_average(&env);
        let near_edges = (36..70, 8..56);
        let (input, ours, plain) =
            (error(&env.inner.render(0.25, 1.0), &clean, &near_edges).0, error(&out, &clean, &near_edges).0, error(&average, &clean, &near_edges).0);
        assert!(ours < plain * 0.5, "error at the moving edges: {ours} against {plain} for a plain 3-frame average");
        // ... without giving up the denoising there: it is still well below the noisy picture's own error
        assert!(ours < input * 0.5, "error at the moving edges: {ours} against {input} before");
    }

    #[test]
    fn a_cut_gives_the_temporal_radius_zero_result() {
        // nothing in the neighbouring frames resembles this one: radius 2 must equal radius 0
        let env = Probe::new(unrelated_noisy, 0.5, 1.0);
        let with = run_on(&env, &denoise(30.0, 50.0, 2.0, 50.0), 1.0);
        let without = run_on(&env, &denoise(30.0, 50.0, 0.0, 50.0), 1.0);
        assert_eq!(env.asked(), 4);
        assert_eq!(with.px, without.px);
        // the first frame of a new shot takes nothing from the shot before it, only from the next frame
        let env = Probe::new(shots_noisy, 0.5, 1.0);
        let clean = render_of(shots_clean, 0.5, 1.0);
        let out = run_on(&env, &denoise(30.0, 50.0, 1.0, 50.0), 1.0);
        let average = plain_average(&env);
        let away_from_the_edge = (4..44, 8..56);
        let (ours, plain) = (error(&out, &clean, &away_from_the_edge).0, error(&average, &clean, &away_from_the_edge).0);
        assert!(ours < 0.02 && plain > 0.1, "previous shot leaks: {ours} against {plain} for a plain average");
    }

    #[test]
    fn half_resolution_looks_like_full_resolution() {
        let measure = |scale: f32| {
            let env = Probe::new(flat_noisy, 0.5, scale);
            let (clean, input) = (render_of(flat_clean, 0.5, scale), env.inner.render(0.5, scale));
            let out = run_on(&env, &denoise(30.0, 50.0, 1.0, 50.0), scale);
            let s = |v: usize| (v as f32 * scale).round() as usize;
            let (flat, edge) = ((s(8)..s(40), s(8)..s(56)), (s(44)..s(52), s(8)..s(56)));
            (error(&out, &clean, &flat).1 / error(&input, &clean, &flat).1, error(&out, &clean, &edge).0 / error(&input, &clean, &edge).0)
        };
        let ((noise_full, edge_full), (noise_half, edge_half)) = (measure(1.0), measure(0.5));
        // the half-size picture has a quarter of the pixels to average over, so it keeps a little more noise
        assert!(noise_full < 0.35 && noise_half < 0.35, "noise left: {noise_full} at full size, {noise_half} at half");
        assert!((noise_full - noise_half).abs() < 0.15, "noise left: {noise_full} at full size, {noise_half} at half");
        assert!(edge_full < 0.5 && edge_half < 0.5, "edge error left: {edge_full} at full size, {edge_half} at half");
        assert!((edge_full - edge_half).abs() < 0.2, "edge error left: {edge_full} at full size, {edge_half} at half");
    }

    #[test]
    fn luma_and_chroma_levels_act_separately() {
        let env = Probe::new(flat_noisy, 0.5, 1.0);
        let (clean, input) = (render_of(flat_clean, 0.5, 1.0), env.inner.render(0.5, 1.0));
        let area = LEFT();
        let ratio = |out: &Image, plane: usize| plane_error(out, &clean, plane, &area) / plane_error(&input, &clean, plane, &area);
        let chroma_only = run_on(&env, &denoise(0.0, 100.0, 1.0, 50.0), 1.0);
        assert!((ratio(&chroma_only, 0) - 1.0).abs() < 0.01, "luma moved with luma at 0: {}", ratio(&chroma_only, 0));
        assert!(ratio(&chroma_only, 1) < 0.4 && ratio(&chroma_only, 2) < 0.4, "chroma noise left: {} {}", ratio(&chroma_only, 1), ratio(&chroma_only, 2));
        let luma_only = run_on(&env, &denoise(100.0, 0.0, 1.0, 50.0), 1.0);
        assert!((ratio(&luma_only, 1) - 1.0).abs() < 0.01 && (ratio(&luma_only, 2) - 1.0).abs() < 0.01, "colour moved with chroma at 0");
        assert!(ratio(&luma_only, 0) < 0.4, "luma noise left: {}", ratio(&luma_only, 0));
        // both at 0 is off: not one value changes
        assert_eq!(run_on(&env, &denoise(0.0, 0.0, 2.0, 0.0), 1.0).px, input.px);
    }

    #[test]
    fn show_noise_shows_what_was_removed() {
        let env = Probe::new(flat_noisy, 0.5, 1.0);
        let input = env.inner.render(0.5, 1.0);
        let cleaned = run_on(&env, &denoise(30.0, 50.0, 1.0, 50.0), 1.0);
        let mut e = denoise(30.0, 50.0, 1.0, 50.0);
        set(&mut e, "show_noise", ParamValue::Bool(true));
        let shown = run_on(&env, &e, 1.0);
        let (mut a, mut b) = (Vec::new(), Vec::new());
        for y in 8..56 {
            for x in 8..40 {
                let (s, i, c) = (enc_px(&shown, x, y), enc_px(&input, x, y), enc_px(&cleaned, x, y));
                for k in 0..3 {
                    a.push(f64::from(s[k] - 0.5));
                    b.push(f64::from(i[k] - c[k]));
                }
            }
        }
        let n = a.len() as f64;
        let (ma, mb) = (a.iter().sum::<f64>() / n, b.iter().sum::<f64>() / n);
        let cov: f64 = a.iter().zip(&b).map(|(x, y)| (x - ma) * (y - mb)).sum();
        let (va, vb): (f64, f64) = (a.iter().map(|x| (x - ma).powi(2)).sum(), b.iter().map(|y| (y - mb).powi(2)).sum());
        let corr = cov / (va * vb).sqrt();
        assert!(corr > 0.99, "the picture shown does not follow input - result: correlation {corr}");
        assert!(ma.abs() < 0.01, "noise is shown around mid grey: mean offset {ma}");
        assert!((va / n).sqrt() > 0.03, "the noise is shown, not a blank picture: {}", (va / n).sqrt());
    }

    #[test]
    fn show_noise_with_nothing_to_remove_shows_flat_grey_not_the_source() {
        let env = Probe::new(flat_noisy, 0.5, 1.0);
        let mut input = env.inner.render(0.5, 1.0);
        let at = |x: usize, y: usize| (y * input.w + x) * 4;
        // a half-transparent pixel (premultiplied), a transparent one that still holds a colour, and one that is not a number
        let (half, clear, broken) = (at(20, 20), at(30, 30), at(31, 30));
        input.px[half..half + 3].iter_mut().for_each(|v| *v *= 0.5);
        input.px[half + 3] = 0.5;
        input.px[clear..clear + 4].copy_from_slice(&[0.7, 0.2, 0.1, 0.0]);
        input.px[broken] = f32::NAN;
        let show = |level: f64| {
            let mut e = denoise(level, level, 1.0, 50.0);
            set(&mut e, "show_noise", ParamValue::Bool(true));
            let mut img = input.clone();
            crate::effects::apply(&mut img, &e, &FxCtx { seconds: 0.5, env: Some(&env), ..cx() });
            img
        };
        let off = show(0.0);
        assert_eq!(env.asked(), 0, "with nothing to filter no neighbouring frame is read");
        // 0.05 % is just above the level the filter acts on; it matches no sample, so it removes next to nothing
        let barely = show(0.05);
        assert!(env.asked() > 0, "the comparison level must run the filter");
        let pixels = off.px.as_chunks::<4>().0.iter().zip(barely.px.as_chunks::<4>().0).zip(input.px.as_chunks::<4>().0);
        for (i, ((o, b), s)) in pixels.enumerate() {
            let (x, y) = (i % input.w, i / input.w);
            if [clear, broken].contains(&(i * 4)) {
                assert!(o.iter().zip(s).all(|(a, b)| a.to_bits() == b.to_bits()), "({x},{y}) must be left alone: {o:?} for {s:?}");
                continue;
            }
            assert_eq!(o[3], s[3], "alpha changed at ({x},{y})");
            assert!(o[..3] != s[..3], "the source picture is shown at ({x},{y}): {o:?}");
            assert!(o.iter().zip(b).all(|(a, b)| close(*a, *b, 1e-4)), "({x},{y}) is {o:?} with nothing removed but {b:?} with next to nothing removed");
        }
    }

    #[test]
    fn preserve_detail_keeps_texture_the_filter_would_flatten() {
        let env = Probe::new(texture_noisy, 0.5, 1.0);
        let input = env.inner.render(0.5, 1.0);
        let spread = |img: &Image| {
            let (mut s, mut s2, mut n) = (0.0f64, 0.0f64, 0usize);
            for y in 8..56 {
                for x in 8..88 {
                    for k in 0..3 {
                        let v = f64::from(enc_px(img, x, y)[k]);
                        s += v;
                        s2 += v * v;
                        n += 1;
                    }
                }
            }
            (s2 / n as f64 - (s / n as f64).powi(2)).sqrt()
        };
        let at = |detail: f64| spread(&run_on(&env, &denoise(30.0, 50.0, 0.0, detail), 1.0));
        let (open, half, kept) = (at(0.0), at(50.0), at(100.0));
        assert!(kept > open * 1.5, "texture spread: {open} with no detail kept, {kept} with all of it");
        assert!(open < half && half < kept, "texture spread at 0, 50, 100 % detail: {open} {half} {kept}");
        assert!(kept > spread(&input) * 0.9, "all detail kept should leave the picture as it was");
    }

    #[test]
    fn negative_channels_keep_their_sign() {
        // the left half is out of gamut: a negative linear red under the usual grain, alpha 1
        let env = Probe::new(gamut_noisy, 0.5, 1.0);
        let clean = render_of(gamut_clean, 0.5, 1.0);
        let input = env.inner.render(0.5, 1.0);
        let out = run_on(&env, &denoise(30.0, 50.0, 1.0, 50.0), 1.0);
        let (xs, ys) = LEFT();
        let (mut n, mut changed, mut sign_lost) = (0usize, 0usize, 0usize);
        let (mut before, mut after) = (0.0f64, 0.0f64);
        for y in ys {
            for x in xs.clone() {
                let (i, o, c) = (input.get(x, y), out.get(x, y), clean.get(x, y));
                n += 1;
                changed += usize::from(o[0] != i[0]);
                sign_lost += usize::from(o[0] >= 0.0);
                before += f64::from((i[0] - c[0]).abs());
                after += f64::from((o[0] - c[0]).abs());
            }
        }
        // the red channel really went through the filter, so a pass-through cannot satisfy the rest
        assert!(changed * 2 > n, "the filter changed the red of only {changed} of {n} pixels");
        let clean_red = clean.get(xs.start, 8)[0];
        assert_eq!(sign_lost, 0, "{sign_lost} of {n} pixels lost the sign of their red (clean value {clean_red})");
        let (before, after) = (before / n as f64, after / n as f64);
        assert!(after < before, "red error {before} -> {after} (clean value {clean_red})");
    }

    #[test]
    fn in_gamut_input_never_comes_out_negative() {
        let env = Probe::new(deep_noisy, 0.5, 1.0);
        let input = env.inner.render(0.5, 1.0);
        assert!(input.px.iter().all(|v| *v >= 0.0), "the scene must be in gamut");
        let out = run_on(&env, &denoise(30.0, 50.0, 1.0, 50.0), 1.0);
        let changed = out.px.iter().zip(&input.px).filter(|(o, i)| o != i).count();
        assert!(changed * 2 > out.px.len(), "the filter changed only {changed} of {} values", out.px.len());
        // luma and the colour differences are averaged apart, so their sum can undershoot 0 by itself
        let below: Vec<(usize, f32)> = out.px.iter().copied().enumerate().filter(|(_, v)| *v < 0.0).collect();
        assert!(below.is_empty(), "{} of {} channels came out below 0, first (index, value): {:?}", below.len(), out.px.len(), below.first());
    }

    #[test]
    fn temporal_radius_bounds_the_frames_read() {
        let asked = |now: f64, radius: f64| {
            let env = Probe::new(flat_noisy, now, 1.0);
            run_on(&env, &denoise(30.0, 50.0, radius, 50.0), 1.0);
            env.asked()
        };
        assert_eq!(asked(0.5, 0.0), 0, "radius 0 is spatial only");
        assert_eq!(asked(0.5, 1.0), 2);
        assert_eq!(asked(0.5, 2.0), 4);
        assert_eq!(asked(0.5, 1e9), 4, "a huge radius is capped at the parameter's range");
        assert_eq!(asked(0.0, 2.0), 2, "no frames from before the clip starts");
        assert_eq!(asked(2.0 - 1.0 / 24.0, 2.0), 2, "no frames from after the clip ends");
    }

    #[test]
    fn hostile_input_neither_panics_nor_returns_garbage() {
        let env = Probe::new(flat_noisy, 0.5, 1.0);
        let src = env.inner.render(0.5, 1.0);
        let sane = |img: &Image, src: &Image, what: &str| {
            assert_eq!((img.w, img.h, img.px.len()), (src.w, src.h, src.px.len()), "{what}: size");
            for (o, i) in img.px.as_chunks::<4>().0.iter().zip(src.px.as_chunks::<4>().0) {
                assert!(o.iter().all(|v| v.is_finite()), "{what}: {o:?}");
                assert_eq!(o[3], i[3], "{what}: alpha");
                assert!(o[..3].iter().all(|v| (0.0..=2.0).contains(v)), "{what}: {o:?}");
            }
        };
        // levels, radius and detail: NaN, infinities, negative and absurd values
        for id in ["luma", "chroma", "temporal", "detail"] {
            for v in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY, -50.0, 1e30, 1e300] {
                for show in [false, true] {
                    let mut e = denoise(30.0, 50.0, 1.0, 50.0);
                    setf(&mut e, id, v);
                    set(&mut e, "show_noise", ParamValue::Bool(show));
                    sane(&run_on(&env, &e, 1.0), &src, &format!("{id} = {v}"));
                }
            }
        }
        // a working scale that is not a number
        for px_scale in [f32::NAN, f32::INFINITY, 0.0, -1.0, 1e9] {
            let mut img = src.clone();
            crate::effects::apply(&mut img, &denoise(30.0, 50.0, 1.0, 50.0), &FxCtx { px_scale, env: Some(&env), ..cx() });
            sane(&img, &src, &format!("px_scale {px_scale}"));
        }
        // tiny, transparent, half-transparent and broken images, with neighbours of another size or none at all
        let mut e = denoise(30.0, 50.0, 2.0, 50.0);
        let mut images = vec![Image::filled(1, 1, [0.2, 0.4, 0.6, 1.0]), Image::filled(1, 7, [0.2, 0.4, 0.6, 1.0]), Image::filled(7, 1, [0.2, 0.4, 0.6, 1.0])];
        images.push(Image::filled(5, 5, [0.1, 0.2, 0.3, 0.2]));
        images.push(Image::filled(9, 9, [0.7, f32::NAN, 5.0, 0.0])); // transparent with garbage colour
        for show in [false, true] {
            set(&mut e, "show_noise", ParamValue::Bool(show));
            for img in &images {
                for with_env in [false, true] {
                    let mut out = img.clone();
                    let env = with_env.then_some(&env as &dyn FxEnv);
                    crate::effects::apply(&mut out, &e, &FxCtx { env, ..cx() });
                    if img.px.iter().all(|v| v.is_finite()) {
                        sane(&out, img, &format!("{}x{} env {with_env} show {show}", img.w, img.h));
                    }
                    if img.px.as_chunks::<4>().0.iter().all(|p| p[3] == 0.0) {
                        assert!(out.px.iter().zip(&img.px).all(|(a, b)| a.to_bits() == b.to_bits()), "transparent pixels must stay as they were");
                    }
                }
            }
        }
        set(&mut e, "show_noise", ParamValue::Bool(false));
        // a pixel that is not a number is left as it was and does not spread to its neighbours
        let mut img = Image::filled(8, 8, [0.2, 0.4, 0.6, 1.0]);
        img.px[..3].fill(f32::NAN);
        crate::effects::apply(&mut img, &e, &FxCtx { env: Some(&env), ..cx() });
        assert!(img.px[..3].iter().all(|v| v.is_nan()) && img.px[4..].iter().all(|v| v.is_finite()));
        // a buffer that does not match its size is refused, not indexed
        let mut broken = Image { w: 4, h: 4, px: vec![0.5; 10] };
        crate::effects::apply(&mut broken, &e, &FxCtx { env: Some(&env), ..cx() });
        assert_eq!(broken.px, vec![0.5; 10]);
        // no environment: spatial only, the same as radius 0
        let mut alone = src.clone();
        crate::effects::apply(&mut alone, &e, &FxCtx { env: None, ..cx() });
        let mut spatial = src.clone();
        crate::effects::apply(&mut spatial, &denoise(30.0, 50.0, 0.0, 50.0), &FxCtx { env: None, ..cx() });
        assert_eq!(alone.px, spatial.px);
        // neighbours of another size are ignored
        let wrong = Probe::new(flat_noisy, 0.5, 0.5);
        assert_eq!(run_on(&wrong, &denoise(30.0, 50.0, 2.0, 50.0), 1.0).px, spatial.px);
    }
}
