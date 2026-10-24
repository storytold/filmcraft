//! GPU-vs-CPU parity of the effect stage (`filmcraft_render::gpufx::FxOp::apply` is the
//! reference).
//!
//! Tolerances:
//! - Exact checks ([`gpu_effects_match_cpu_exactly`]): the working image read back as f32 from a
//!   half-float-representable source (so the source draw is lossless) must match the CPU op to
//!   [`EXACT`] per value (f32 arithmetic and the GPU's `pow` / `sqrt` differ in the last bits).
//!   Step functions (Posterize, Color Pass, Leave Color and Change to Color keys, Extract) may
//!   flip a pixel that lands on a threshold; at most [`FLIPS`] of the values may differ by more.
//! - Composited plans ([`gpu_effect_layers_match_cpu_plan`]): the compositor's criterion (p99 of
//!   the per-pixel max 8-bit channel difference ≤ 6, mean < 1.5) away from layer outlines, as for
//!   the blend modes.

use super::blend_tests::{device, interior, ramp_layer, stats8, yuv_frame};
use super::*;
use filmcraft_geom::{Affine, Vec2};
use filmcraft_project::{EffectInstance, ParamValue, find_effect};
use filmcraft_render::effects::FxCtx;
use filmcraft_render::gpufx::FxOp;
use filmcraft_render::plan::{LayerFx, execute_cpu};
use filmcraft_time::Tick;

const EXACT: f32 = 1e-4;
const FLIPS: f64 = 0.01;
/// Normalised budget for `vr_chromatic_aberrations` only (flips and the exact typed==vfx check are unchanged).
/// Measured 22/10/2026 on RADV RENOIR after asin->atan2 in the VR shaders: worst 9.18e-3 (67x41, layout 0,
/// pixel (6,40), red), 5.2e-4 and 6.1e-4 for layouts 1 and 2. The CPU reference is f64 and an f32 emulation of
/// the coordinate chain only accounts for ~6e-4 texel, so the rest is assumed to be driver transcendental
/// precision (inferred, not measured on the GPU). Budget = worst measured x ~1.6.
const VR_CHROMATIC_GPU_BUDGET: f32 = 1.5e-2;

fn parity_budget(id: &str) -> f32 {
    if id == "vr_chromatic_aberrations" { VR_CHROMATIC_GPU_BUDGET } else { EXACT }
}

static CLIP: filmcraft_render::vfx::ClipTiming =
    filmcraft_render::vfx::ClipTiming { clip_seconds: 10.0, clip_offset: 0.0, frame_rate: 24.0, sequence_size: (67, 41), source_size: (67, 41) };

fn cx(t: Tick, px_scale: f32) -> FxCtx<'static> {
    FxCtx {
        t,
        px_scale,
        seconds: 0.0,
        timecode: "",
        clip_name: "",
        project: None,
        env: Some(&CLIP),
        frame_rate: 24.0,
        working: filmcraft_color::WorkingSpace::Rec709,
    }
}

fn required_gpu_device() -> (wgpu::Device, wgpu::Queue) {
    let instance = wgpu::Instance::default();
    let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions::default())).expect("FX parity gate requires a GPU adapter");
    let info = adapter.get_info();
    eprintln!("Required FX parity adapter: {} vendor={:#06x} type={:?} backend={:?}", info.name, info.vendor, info.device_type, info.backend);
    assert_eq!(info.vendor, 0x1002, "FX parity gate requires an AMD adapter (vendor 0x1002); selected {} from {:?}", info.name, info.backend);
    assert!(
        matches!(info.device_type, wgpu::DeviceType::IntegratedGpu | wgpu::DeviceType::DiscreteGpu),
        "FX parity gate requires physical AMD GPU, got {:?} adapter {}",
        info.device_type,
        info.name
    );
    assert_eq!(info.backend, wgpu::Backend::Vulkan, "FX parity gate requires Vulkan on AMD; selected {:?} for {}", info.backend, info.name);
    pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor::default())).expect("FX parity adapter must create a device")
}

fn effect(id: &str, params: &[(&str, ParamValue)]) -> EffectInstance {
    let mut e = find_effect(id).unwrap_or_else(|| panic!("{id}")).instance();
    for (k, v) in params {
        e.params.get_mut(*k).unwrap_or_else(|| panic!("{id}.{k}")).value = v.clone();
    }
    e
}

fn fl(v: f64) -> ParamValue {
    ParamValue::Float(v)
}
fn col(r: f32, g: f32, b: f32) -> ParamValue {
    ParamValue::Color([r, g, b, 1.0])
}
fn ch(c: u32) -> ParamValue {
    ParamValue::Choice(c)
}
fn pt(x: f64, y: f64) -> ParamValue {
    ParamValue::Vec2(Vec2::new(x, y))
}

/// Original `effects.rs` Drop Shadow path, independent from `FxOp::eval/apply`.
fn drop_shadow_oracle(img: &mut filmcraft_render::Image, e: &EffectInstance, cx: &FxCtx<'_>) {
    let c = e.param("color").and_then(|p| p.value_at(cx.t).as_color()).unwrap_or([1.0; 4]);
    let opacity = e.f64_at("opacity", cx.t) as f32 / 100.0;
    let direction = (e.f64_at("direction", cx.t) as f32 as f64).to_radians();
    let distance = e.f64_at("distance", cx.t) as f32 as f64 * cx.px_scale as f64;
    let softness = e.f64_at("softness", cx.t) as f32 * cx.px_scale * 0.5;
    let only = e.param("only").and_then(|p| p.value.as_bool()).unwrap_or(false);
    let (dx, dy) = (direction.sin() * distance, -direction.cos() * distance);
    let dec = |v: f32| filmcraft_color::srgb_to_linear(v.clamp(0.0, 1.0));
    let color = [dec(c[0]), dec(c[1]), dec(c[2])];
    let mut shadow = img.transformed(img.w, img.h, &Affine::translate(dx, dy));
    for p in shadow.px.as_chunks_mut::<4>().0 {
        let alpha = p[3] * opacity;
        p[0] = color[0] * alpha;
        p[1] = color[1] * alpha;
        p[2] = color[2] * alpha;
        p[3] = alpha;
    }
    if softness > 0.3 {
        filmcraft_render::effects::gaussian(&mut shadow, softness, softness, false);
    }
    if !only {
        filmcraft_render::blend::composite(&mut shadow, img, 1.0, filmcraft_render::Blend::Normal);
    }
    *img = shadow;
}

/// Original `effects.rs` Emboss branch, independent from `ProceduralOp::apply`.
fn emboss_oracle(img: &mut filmcraft_render::Image, e: &EffectInstance, cx: &FxCtx<'_>) {
    let direction = (e.f64_at("direction", cx.t) as f32 as f64).to_radians();
    let relief = e.f64_at("relief", cx.t) as f32 * cx.px_scale.max(0.25);
    let contrast = (e.f64_at("contrast", cx.t) as f32) / 100.0;
    let blend = (e.f64_at("blend", cx.t) as f32) / 100.0;
    let (dx, dy) = ((direction.cos() as f32) * relief, (-(direction.sin() as f32)) * relief);
    let source = img.clone();
    for y in 0..img.h {
        for x in 0..img.w {
            let a = source.sample_bilinear_clamped(x as f32 + 0.5 + dx, y as f32 + 0.5 + dy);
            let b = source.sample_bilinear_clamped(x as f32 + 0.5 - dx, y as f32 + 0.5 - dy);
            let luma = |p: [f32; 4]| 0.2126 * p[0] + 0.7152 * p[1] + 0.0722 * p[2];
            let v = filmcraft_color::srgb_to_linear((0.5 + (luma(a) - luma(b)) * contrast * 2.0).clamp(0.0, 1.0));
            let i = (y * img.w + x) * 4;
            let alpha = source.px[i + 3];
            for c in 0..3 {
                img.px[i + c] = v * alpha * (1.0 - blend) + source.px[i + c] * blend;
            }
        }
    }
}

fn half(v: f32) -> f32 {
    super::blend_tests::f16_to_f32(f32_to_f16(v))
}

/// Linear premultiplied test picture, every value representable in half floats: hue sweep along
/// x, lightness along y, a grey ramp, black and white columns, super-whites (up to 1.6), and
/// alpha 1 / partial / 0 bands.
fn picture(w: u32, h: u32) -> Vec<f32> {
    let mut px = Vec::with_capacity((w * h * 4) as usize);
    for y in 0..h {
        for x in 0..w {
            let fx = x as f32 / (w - 1) as f32;
            let fy = y as f32 / (h - 1) as f32;
            let c = match x % 11 {
                0 => [0.0, 0.0, 0.0],
                1 => [1.0, 1.0, 1.0],
                2 => [fy, fy, fy],
                3 => [1.6 * fy, 1.2 * fy, 0.3],
                _ => {
                    let rgb = filmcraft_color::hsl_to_rgb(fx, 0.25 + 0.75 * ((y % 5) as f32 / 4.0), 0.1 + 0.8 * fy);
                    rgb.map(filmcraft_color::srgb_to_linear)
                }
            };
            let a = match y % 7 {
                0 => 0.0,
                1 | 2 => 0.5 + 0.4 * fx,
                3 => 0.25,
                _ => 1.0,
            };
            px.extend_from_slice(&[c[0] * a, c[1] * a, c[2] * a, a].map(half));
        }
    }
    px
}

/// Max |cpu − gpu| (finite values required) and the share of values off by more than 0.02.
fn compare(cpu: &[f32], gpu: &[f32]) -> (f32, f64) {
    assert_eq!(cpu.len(), gpu.len());
    let mut worst = 0f32;
    let mut flips = 0usize;
    for (a, b) in cpu.iter().zip(gpu) {
        assert!(b.is_finite(), "non-finite GPU value");
        let d = (a - b).abs() / (1.0 + a.abs());
        if d > 0.02 {
            flips += 1;
        } else {
            worst = worst.max(d);
        }
    }
    (worst, flips as f64 / cpu.len() as f64)
}

/// Effect cases: (effect id, parameters, whether a threshold may flip pixels).
fn cases() -> Vec<(&'static str, Vec<(&'static str, ParamValue)>, bool)> {
    let mut cases = vec![
        ("brightness_contrast", vec![("brightness", fl(30.0)), ("contrast", fl(40.0))], false),
        ("brightness_contrast", vec![("brightness", fl(-100.0)), ("contrast", fl(-100.0))], false),
        ("brightness_contrast", vec![("brightness", fl(100.0)), ("contrast", fl(100.0))], false),
        ("brightness_contrast", vec![("brightness", fl(-20.0)), ("contrast", fl(900.0))], false),
        ("proc_amp", vec![("brightness", fl(10.0)), ("contrast", fl(130.0)), ("hue", fl(200.0)), ("saturation", fl(150.0))], false),
        ("proc_amp", vec![("hue", fl(-400.0)), ("saturation", fl(0.0))], false),
        ("tint", vec![], false),
        ("tint", vec![("black", col(0.2, 0.1, 0.5)), ("white", col(1.0, 0.9, 0.3)), ("amount", fl(60.0))], false),
        ("black_white", vec![], false),
        ("color_balance", vec![("shadow_r", fl(50.0)), ("mid_g", fl(-40.0)), ("hi_b", fl(80.0)), ("shadow_b", fl(-100.0)), ("hi_r", fl(100.0))], false),
        ("color_balance", vec![("mid_r", fl(70.0)), ("hi_g", fl(-60.0)), ("preserve", ParamValue::Bool(true))], false),
        ("leave_color", vec![("amount", fl(80.0)), ("tolerance", fl(20.0)), ("softness", fl(30.0))], true),
        ("leave_color", vec![("amount", fl(100.0)), ("color", col(0.1, 0.8, 0.2)), ("tolerance", fl(0.0)), ("softness", fl(0.0))], true),
        ("change_to_color", vec![], true),
        ("change_to_color", vec![("hue_tol", fl(30.0)), ("softness", fl(10.0)), ("to", col(0.9, 0.9, 0.1))], true),
        ("color_pass", vec![("similarity", fl(30.0))], true),
        ("color_pass", vec![("similarity", fl(45.0)), ("reverse", ParamValue::Bool(true)), ("color", col(0.2, 0.4, 0.9))], true),
        ("color_key", vec![], true),
        ("color_key", vec![("color", col(0.1, 0.7, 0.2)), ("tolerance", fl(60.0)), ("feather", fl(20.0))], true),
        ("luma_key", vec![], true),
        ("luma_key", vec![("threshold", fl(75.0)), ("cutoff", fl(20.0))], true),
        ("cineon_converter", vec![], false),
        (
            "cineon_converter",
            vec![
                ("conversion", ch(1)),
                ("black10", fl(120.0)),
                ("white10", fl(900.0)),
                ("black_internal", fl(12.0)),
                ("white_internal", fl(230.0)),
                ("gamma", fl(2.2)),
                ("rolloff", fl(0.0)),
            ],
            false,
        ),
        ("cineon_converter", vec![("conversion", ch(2)), ("black10", fl(80.0)), ("white10", fl(720.0)), ("rolloff", fl(100.0))], false),
        ("noise", vec![], false),
        ("noise_legacy", vec![], false),
        ("noise", vec![("amount", fl(48.0)), ("color", ParamValue::Bool(false)), ("clip", ParamValue::Bool(true)), ("grain_size", fl(2.5))], false),
        ("noise", vec![("amount", fl(35.0)), ("color", ParamValue::Bool(true)), ("clip", ParamValue::Bool(false)), ("grain_size", fl(1.0))], false),
        ("noise_legacy", vec![("amount", fl(48.0)), ("color", ParamValue::Bool(false)), ("clip", ParamValue::Bool(true))], false),
        ("noise_legacy", vec![("amount", fl(35.0)), ("color", ParamValue::Bool(true)), ("clip", ParamValue::Bool(false))], false),
        ("gamma_correction", vec![("gamma", fl(5.0))], false),
        ("gamma_correction", vec![("gamma", fl(28.0))], false),
        ("gamma_correction", vec![("gamma", fl(0.0))], false),
        ("gamma_correction", vec![("gamma", fl(-3.0))], false),
        ("levels", vec![("in_black", fl(20.0)), ("in_white", fl(230.0)), ("out_black", fl(10.0)), ("out_white", fl(240.0)), ("gamma", fl(150.0))], false),
        ("levels", vec![("in_black", fl(200.0)), ("in_white", fl(0.0)), ("gamma", fl(5.0)), ("out_white", fl(-50.0))], true),
        ("extract", vec![], true),
        ("extract", vec![("invert", ParamValue::Bool(true))], true),
        ("extract", vec![("black", fl(40.0)), ("white", fl(200.0)), ("softness", fl(30.0))], true),
        ("extract", vec![("black", fl(90.0)), ("white", fl(160.0)), ("invert", ParamValue::Bool(true))], true),
        ("extract", vec![("black", fl(90.0)), ("white", fl(160.0)), ("softness", fl(1.0))], true),
        ("extract", vec![("black", fl(40.0)), ("white", fl(200.0)), ("softness", fl(100.0))], true),
        ("extract", vec![("black", fl(160.0)), ("white", fl(90.0))], true),
        ("extract", vec![("black", fl(90.0)), ("white", fl(90.0))], true),
        ("invert", vec![("blend", fl(30.0))], false),
        ("invert", vec![("channel", ParamValue::Choice(1))], false),
        ("invert", vec![("channel", ParamValue::Choice(2)), ("blend", fl(70.0))], false),
        ("invert", vec![("channel", ParamValue::Choice(3))], false),
        ("invert", vec![("channel", ParamValue::Choice(4)), ("blend", fl(20.0))], false),
        ("invert", vec![("channel", ParamValue::Choice(9))], false),
        ("posterize", vec![("levels", fl(2.0))], true),
        ("posterize", vec![("levels", fl(7.0))], true),
        ("posterize", vec![("levels", fl(255.0))], true),
        ("gaussian_blur", vec![("blurriness", fl(0.0))], false),
        ("gaussian_blur", vec![("blurriness", fl(5.0))], false),
        ("gaussian_blur", vec![("blurriness", fl(5.0)), ("repeat_edge", ParamValue::Bool(true))], false),
        ("gaussian_blur", vec![("blurriness", fl(80.0))], false),
        ("gaussian_blur", vec![("blurriness", fl(80.0)), ("repeat_edge", ParamValue::Bool(true))], false),
        ("gaussian_blur", vec![("blurriness", fl(12.0)), ("dimensions", ParamValue::Choice(1))], false),
        ("gaussian_blur", vec![("blurriness", fl(12.0)), ("dimensions", ParamValue::Choice(2)), ("repeat_edge", ParamValue::Bool(true))], false),
        ("gaussian_blur", vec![("blurriness", fl(3000.0)), ("repeat_edge", ParamValue::Bool(true))], false),
        ("gaussian_blur", vec![("blurriness", fl(1e9))], false),
        ("gaussian_blur", vec![("blurriness", fl(-50.0))], false),
        ("camera_blur", vec![("percent", fl(20.0))], false),
        ("lens_flare", vec![], true),
        ("lens_flare", vec![("lens_type", ch(1)), ("ghosts", fl(5.0)), ("size", fl(60.0)), ("center", pt(20.0, 12.0))], true),
        ("lens_flare", vec![("lens_type", ch(2)), ("ghosts", fl(8.0)), ("center", pt(50.0, 30.0)), ("brightness", fl(150.0))], true),
        ("lens_flare", vec![("lens_type", ch(3)), ("ghosts", fl(3.0)), ("blend", fl(30.0)), ("color", col(0.9, 0.5, 0.3))], true),
        ("gaussian_blur_legacy", vec![("blurriness", fl(5.0))], false),
        ("gaussian_blur_legacy", vec![("blurriness", fl(12.0)), ("dimensions", ch(2))], false),
        ("directional_blur_legacy", vec![("length", fl(10.0)), ("direction", fl(30.0))], false),
        ("reduce_interlace_flicker", vec![("softness", fl(2.0))], false),
        ("reduce_interlace_flicker", vec![("softness", fl(7.5))], false),
        ("directional_blur", vec![("length", fl(10.0)), ("direction", fl(30.0))], false),
        ("directional_blur", vec![("length", fl(0.2))], false),
        ("directional_blur", vec![("length", fl(1000.0)), ("direction", fl(-100.0))], false),
        ("sharpen", vec![("amount", fl(80.0))], false),
        ("sharpen", vec![("amount", fl(4000.0))], false),
        ("unsharp_mask", vec![("amount", fl(150.0)), ("radius", fl(3.0)), ("threshold", fl(10.0))], true),
        ("unsharp_mask", vec![("radius", fl(0.1))], false),
        ("unsharp_mask", vec![("amount", fl(300.0)), ("radius", fl(250.0))], false),
        ("channel_blur", vec![("red", fl(5.0)), ("green", fl(9.0)), ("blue", fl(13.0)), ("alpha", fl(4.0))], false),
        ("channel_blur", vec![("red", fl(7.0)), ("blue", fl(11.0)), ("dimensions", ch(1)), ("repeat_edge", ParamValue::Bool(true))], false),
        ("alpha_glow", vec![], false),
        (
            "alpha_glow",
            vec![
                ("glow", fl(18.0)),
                ("brightness", fl(210.0)),
                ("start_color", col(0.2, 0.8, 0.9)),
                ("end_color", col(0.9, 0.2, 0.3)),
                ("use_end", ParamValue::Bool(true)),
            ],
            false,
        ),
        ("alpha_glow", vec![("glow", fl(12.0)), ("brightness", fl(150.0)), ("fade_out", ParamValue::Bool(false))], false),
        ("alpha_glow", vec![("glow", fl(0.2)), ("brightness", fl(255.0)), ("fade_out", ParamValue::Bool(false))], false),
        ("drop_shadow", vec![], false),
        (
            "drop_shadow",
            vec![("color", col(0.2, 0.7, 0.9)), ("opacity", fl(83.0)), ("direction", fl(47.0)), ("distance", fl(12.5)), ("softness", fl(14.0))],
            false,
        ),
        ("drop_shadow", vec![("only", ParamValue::Bool(true)), ("opacity", fl(67.0)), ("distance", fl(3.25)), ("softness", fl(8.0))], false),
        ("crop", vec![("left", fl(10.0)), ("top", fl(5.0)), ("right", fl(20.0)), ("bottom", fl(15.0))], false),
        ("crop", vec![("left", fl(10.0)), ("top", fl(5.0)), ("right", fl(20.0)), ("bottom", fl(15.0)), ("feather", fl(12.0))], false),
        ("crop", vec![("left", fl(12.0)), ("top", fl(5.0)), ("right", fl(20.0)), ("zoom", ParamValue::Bool(true))], false),
        ("crop", vec![("left", fl(70.0)), ("right", fl(70.0)), ("feather", fl(5000.0))], false),
        ("edge_feather", vec![("amount", fl(30.0))], false),
        ("transform", vec![("scale_height", fl(150.0)), ("rotation", fl(30.0)), ("position", pt(30.0, 20.0)), ("opacity", fl(60.0))], false),
        (
            "transform",
            vec![
                ("uniform_scale", ParamValue::Bool(false)),
                ("scale_width", fl(80.0)),
                ("scale_height", fl(120.0)),
                ("skew", fl(20.0)),
                ("skew_axis", fl(45.0)),
                ("anchor", pt(10.0, 30.0)),
            ],
            false,
        ),
        ("horizontal_flip", vec![], false),
        ("vertical_flip", vec![], false),
        ("mirror", vec![("center", pt(20.0, 10.0)), ("angle", fl(30.0))], false),
        ("mirror", vec![("angle", fl(200.0))], false),
        ("offset", vec![("shift", pt(10.0, 30.0)), ("blend", fl(25.0))], false),
        ("offset", vec![("shift", pt(-1e5, 7.25))], false),
        ("asc_cdl", vec![("r_slope", fl(1.4)), ("g_offset", fl(-0.1)), ("b_power", fl(2.2)), ("saturation", fl(1.6))], false),
        ("asc_cdl", vec![("r_power", fl(0.0)), ("g_slope", fl(0.0)), ("saturation", fl(0.0))], false),
        ("channel_mix", vec![("rr", fl(50.0)), ("rg", fl(60.0)), ("gb", fl(-80.0)), ("bc", fl(30.0))], false),
        ("channel_mix", vec![("rb", fl(120.0)), ("monochrome", ParamValue::Bool(true))], false),
        ("color_replace", vec![("similarity", fl(40.0))], true),
        ("color_replace", vec![("similarity", fl(25.0)), ("solid", ParamValue::Bool(true)), ("target", col(0.3, 0.6, 0.2))], true),
        ("color_replace", vec![("similarity", fl(0.0))], true),
        ("alpha_adjust", vec![("opacity", fl(60.0))], false),
        ("alpha_adjust", vec![("ignore", ParamValue::Bool(true)), ("invert", ParamValue::Bool(true))], false),
        ("alpha_adjust", vec![("invert", ParamValue::Bool(true)), ("mask_only", ParamValue::Bool(true)), ("opacity", fl(150.0))], false),
        ("vignette", vec![], false),
        ("vignette", vec![("amount", fl(-100.0)), ("midpoint", fl(50.0)), ("roundness", fl(0.0)), ("feather", fl(50.0))], false),
        (
            "vignette",
            vec![("amount", fl(-30.0)), ("midpoint", fl(20.0)), ("roundness", fl(-100.0)), ("feather", fl(0.0)), ("color", col(0.1, 0.4, 0.7))],
            false,
        ),
        ("vignette", vec![("amount", fl(40.0)), ("midpoint", fl(90.0)), ("roundness", fl(100.0)), ("feather", fl(100.0))], false),
        ("video_limiter", vec![], false),
        ("video_limiter", vec![("axis", ch(0)), ("clip_level", ch(0)), ("compression", ch(0))], false),
        ("video_limiter", vec![("axis", ch(1)), ("clip_level", ch(9)), ("compression", ch(4))], false),
        ("video_limiter", vec![("axis", ch(2)), ("clip_level", ch(5)), ("compression", ch(2))], false),
        ("video_limiter", vec![("axis", ch(3)), ("clip_level", ch(0)), ("compression", ch(1))], false),
        (
            "video_limiter",
            vec![
                ("axis", ch(2)),
                ("clip_level", ch(0)),
                ("compression", ch(0)),
                ("gamut_warning", ParamValue::Bool(true)),
                ("warning_color", col(0.0, 1.0, 0.0)),
            ],
            false,
        ),
        (
            "lumetri",
            vec![
                ("temperature", fl(20.0)),
                ("tint", fl(-10.0)),
                ("exposure", fl(0.5)),
                ("contrast", fl(15.0)),
                ("highlights", fl(-20.0)),
                ("shadows", fl(25.0)),
                ("whites", fl(10.0)),
                ("blacks", fl(-15.0)),
                ("saturation", fl(110.0)),
                ("creative_on", ParamValue::Bool(false)),
                ("vignette_on", ParamValue::Bool(false)),
            ],
            false,
        ),
        (
            "lumetri",
            vec![
                ("basic_on", ParamValue::Bool(false)),
                ("creative_sat", fl(120.0)),
                ("vibrance", fl(30.0)),
                ("faded_film", fl(25.0)),
                ("shadow_tint", col(0.4, 0.45, 0.6)),
                ("highlight_tint", col(0.6, 0.55, 0.4)),
                ("vignette_on", ParamValue::Bool(false)),
            ],
            false,
        ),
        (
            "lumetri",
            vec![
                ("basic_on", ParamValue::Bool(false)),
                ("creative_on", ParamValue::Bool(false)),
                ("vignette_amount", fl(-3.0)),
                ("vignette_midpoint", fl(45.0)),
                ("vignette_roundness", fl(-30.0)),
                ("vignette_feather", fl(60.0)),
            ],
            false,
        ),
        (
            "lumetri",
            vec![
                ("temperature", fl(-15.0)),
                ("tint", fl(10.0)),
                ("exposure", fl(0.3)),
                ("contrast", fl(20.0)),
                ("highlights", fl(-10.0)),
                ("shadows", fl(15.0)),
                ("whites", fl(-5.0)),
                ("blacks", fl(5.0)),
                ("saturation", fl(105.0)),
                ("creative_sat", fl(110.0)),
                ("vibrance", fl(20.0)),
                ("faded_film", fl(15.0)),
                ("shadow_tint", col(0.48, 0.5, 0.55)),
                ("highlight_tint", col(0.52, 0.5, 0.45)),
                ("vignette_amount", fl(2.0)),
                ("vignette_midpoint", fl(50.0)),
                ("vignette_roundness", fl(20.0)),
                ("vignette_feather", fl(50.0)),
            ],
            false,
        ),
        (
            "lumetri",
            vec![("exposure", fl(4.0)), ("contrast", fl(100.0)), ("temperature", fl(100.0)), ("tint", fl(100.0)), ("whites", fl(100.0)), ("blacks", fl(100.0))],
            false,
        ),
        (
            "lumetri",
            vec![
                ("exposure", fl(-4.0)),
                ("contrast", fl(-100.0)),
                ("temperature", fl(-100.0)),
                ("tint", fl(-100.0)),
                ("whites", fl(-100.0)),
                ("blacks", fl(-100.0)),
            ],
            false,
        ),
    ];
    cases.extend(family_cases());
    cases
}

fn family_cases() -> Vec<(&'static str, Vec<(&'static str, ParamValue)>, bool)> {
    let mut out: Vec<(&'static str, Vec<(&'static str, ParamValue)>, bool)> = Vec::new();
    let params: &[(&str, &[(&str, ParamValue)])] = &[
        ("mosaic", &[("horizontal", fl(3.0)), ("vertical", fl(2.0)), ("softness", fl(35.0))]),
        ("mosaic_legacy", &[("horizontal", fl(3.0)), ("vertical", fl(2.0)), ("sharp", ParamValue::Bool(true))]),
        ("replicate", &[("count", fl(4.0))]),
        ("magnify", &[("center", pt(33.0, 20.0)), ("magnification", fl(250.0)), ("size", fl(30.0)), ("feather", fl(5.0)), ("opacity", fl(80.0))]),
        ("magnify_legacy", &[("center", pt(33.0, 20.0)), ("magnification", fl(250.0)), ("size", fl(30.0)), ("feather", fl(5.0)), ("opacity", fl(80.0))]),
        ("twirl", &[("angle", fl(110.0)), ("radius", fl(45.0))]),
        ("twirl_legacy", &[("angle", fl(110.0)), ("radius", fl(45.0))]),
        ("spherize", &[("radius", fl(18.0))]),
        ("corner_pin", &[("upper_left", pt(2.0, 1.0)), ("upper_right", pt(60.0, 2.0)), ("lower_right", pt(63.0, 39.0)), ("lower_left", pt(5.0, 40.0))]),
        (
            "turbulent_displace",
            &[
                ("displacement", ch(8)),
                ("amount", fl(8.0)),
                ("size", fl(8.5)),
                ("offset", pt(7.0, 3.0)),
                ("complexity", fl(3.5)),
                ("evolution", fl(725.0)),
                ("seed", fl(9999.0)),
                ("pinning", ch(3)),
                ("cycle", ParamValue::Bool(true)),
                ("cycle_revs", fl(2.5)),
            ],
        ),
        ("lens_distortion", &[("curvature", fl(32.0)), ("h_decentering", fl(8.0))]),
        ("wave_warp", &[("height", fl(18.0)), ("width", fl(13.0)), ("direction", fl(35.0)), ("speed", fl(2.0))]),
        (
            "rounded_crop",
            &[("left", fl(3.0)), ("top", fl(2.0)), ("right", fl(5.0)), ("bottom", fl(4.0)), ("radius", fl(10.0)), ("feather", fl(2.0)), ("border", fl(3.0))],
        ),
        ("rgb_split", &[("mode", ch(1)), ("amount", fl(18.0)), ("angle", fl(25.0)), ("blend", fl(20.0))]),
        ("checkerboard", &[("width", fl(9.0)), ("height", fl(7.0)), ("feather", fl(1.0)), ("opacity", fl(70.0)), ("mode", ch(1))]),
        ("grid", &[("size", fl(7.0)), ("border", fl(2.0)), ("opacity", fl(65.0))]),
        ("circle", &[("radius", fl(10.0)), ("opacity", fl(60.0))]),
        ("ellipse", &[("width", fl(16.0)), ("height", fl(9.0)), ("thickness", fl(2.0)), ("softness", fl(10.0)), ("composite", ParamValue::Bool(true))]),
        ("ramp", &[("shape", ch(1)), ("blend", fl(25.0))]),
        ("gradient", &[("shape", ch(2)), ("scatter", fl(20.0)), ("midpoint", fl(30.0)), ("blend", fl(20.0))]),
        ("four_color_gradient", &[("blend", fl(130.0)), ("opacity", fl(70.0))]),
        ("cell_pattern", &[("pattern", ch(1)), ("contrast", fl(150.0)), ("disperse", fl(1.0)), ("size", fl(7.0)), ("evolution", fl(30.0)), ("seed", fl(12.0))]),
        ("emboss", &[("direction", fl(90.0)), ("relief", fl(3.0)), ("contrast", fl(150.0)), ("blend", fl(20.0))]),
        ("color_emboss", &[("direction", fl(90.0)), ("relief", fl(3.0)), ("contrast", fl(150.0)), ("blend", fl(20.0))]),
        ("find_edges", &[("invert", ParamValue::Bool(true)), ("blend", fl(25.0))]),
        ("strobe", &[("duration", fl(0.2)), ("period", fl(0.5)), ("blend", fl(50.0))]),
        ("median", &[("radius", fl(1.0))]),
        ("median", &[("radius", fl(0.0))]),
        ("grow", &[("from", fl(70.0)), ("to", fl(125.0))]),
        ("shrink", &[("from", fl(110.0)), ("to", fl(75.0))]),
        ("move", &[("from", pt(-5.0, 2.0)), ("to", pt(8.0, -4.0))]),
        ("spin", &[("amount", fl(95.0)), ("scale", fl(90.0))]),
        ("wiggle", &[("amount", fl(30.0)), ("rotation", fl(6.0)), ("scale", fl(12.0)), ("frequency", fl(3.0)), ("seed", fl(7.0))]),
        ("wiggle", &[("amount", fl(20.0)), ("dimensions", ch(1)), ("seed", fl(3.0))]),
        ("camera_shake", &[("amount", fl(25.0)), ("rotation", fl(4.0)), ("zoom", fl(8.0)), ("frequency", fl(5.0)), ("complexity", fl(4.0)), ("seed", fl(9.0))]),
        ("camera_shake", &[]),
        ("rotate_3d", &[]),
        ("rotate_3d", &[("rot_x", fl(25.0)), ("rot_y", fl(-30.0)), ("rot_z", fl(12.0)), ("perspective", fl(60.0))]),
        ("rotate_3d", &[("rot_x", fl(70.0)), ("z", fl(40.0)), ("perspective", fl(90.0))]),
        ("rotate_3d", &[("rot_y", fl(160.0)), ("perspective", fl(40.0))]),
        ("rotate_3d", &[("rot_y", fl(160.0)), ("hide_back", ParamValue::Bool(true))]),
        ("rotate_3d", &[("rot_z", fl(45.0)), ("perspective", fl(10.0))]),
        ("basic_3d", &[]),
        ("basic_3d", &[("swivel", fl(40.0)), ("tilt", fl(-20.0)), ("distance", fl(15.0))]),
        ("basic_3d", &[("swivel", fl(85.0)), ("tilt", fl(10.0))]),
        ("basic_3d", &[("swivel", fl(-120.0)), ("tilt", fl(35.0)), ("distance", fl(-30.0))]),
        ("bevel_alpha", &[("thickness", fl(2.5)), ("angle", fl(40.0)), ("intensity", fl(65.0))]),
        ("block_dissolve", &[("completion", fl(47.0)), ("block_w", fl(2.3)), ("block_h", fl(1.7)), ("feather", fl(60.0)), ("soft", ParamValue::Bool(true))]),
        ("linear_wipe_legacy", &[("completion", fl(47.0)), ("angle", fl(38.0)), ("feather", fl(2.2))]),
        ("logo_cutout", &[("background", ch(2)), ("color", col(0.75, 0.6, 0.35)), ("threshold", fl(12.0)), ("softness", fl(45.0))]),
        (
            "logo_cutout",
            &[
                ("background", ch(1)),
                ("threshold", fl(0.0)),
                ("softness", fl(80.0)),
                ("unmultiply", ParamValue::Bool(false)),
                ("invert", ParamValue::Bool(true)),
            ],
        ),
        (
            "vr_chromatic_aberrations",
            &[
                ("red", fl(31.0)),
                ("green", fl(-23.0)),
                ("blue", fl(47.0)),
                ("center_x", fl(37.0)),
                ("center_y", fl(-19.0)),
                ("falloff", fl(73.0)),
                ("inverse", ParamValue::Bool(true)),
                ("frame_layout", ch(0)),
            ],
        ),
        (
            "vr_chromatic_aberrations",
            &[
                ("red", fl(31.0)),
                ("green", fl(-23.0)),
                ("blue", fl(47.0)),
                ("center_x", fl(37.0)),
                ("center_y", fl(-19.0)),
                ("falloff", fl(73.0)),
                ("inverse", ParamValue::Bool(true)),
                ("frame_layout", ch(1)),
            ],
        ),
        (
            "vr_chromatic_aberrations",
            &[
                ("red", fl(31.0)),
                ("green", fl(-23.0)),
                ("blue", fl(47.0)),
                ("center_x", fl(37.0)),
                ("center_y", fl(-19.0)),
                ("falloff", fl(73.0)),
                ("inverse", ParamValue::Bool(true)),
                ("frame_layout", ch(2)),
            ],
        ),
    ];
    for (id, custom) in params {
        out.push((*id, vec![], false));
        out.push((*id, custom.to_vec(), false));
    }
    for layout in 0..=2 {
        for mode in 0..=4 {
            out.push((
                "vr_color_gradients",
                vec![
                    ("c1", col(0.85, 0.2, 0.1)),
                    ("p1_lon", fl(-115.0)),
                    ("p1_lat", fl(25.0)),
                    ("c2", col(0.1, 0.8, 0.2)),
                    ("p2_lon", fl(8.0)),
                    ("p2_lat", fl(-33.0)),
                    ("c3", col(0.15, 0.3, 0.9)),
                    ("p3_lon", fl(126.0)),
                    ("p3_lat", fl(18.0)),
                    ("blend", fl(73.0)),
                    ("opacity", fl(61.0)),
                    ("mode", ch(mode)),
                    ("frame_layout", ch(layout)),
                ],
                false,
            ));
        }
    }
    out.push(("vr_chromatic_aberrations", vec![], false));
    out.push(("vr_color_gradients", vec![], false));
    out.push(("vr_projection", vec![], false));
    out.push(("vr_rotate_sphere", vec![], false));
    out.push(("vr_plane_to_sphere", vec![], false));
    for layout in 0..=2 {
        for stretch in [false, true] {
            out.push((
                "vr_projection",
                vec![
                    ("fov", fl(235.0)),
                    ("tilt", fl(-23.0)),
                    ("pan", fl(41.0)),
                    ("roll", fl(17.0)),
                    ("stretch", ParamValue::Bool(stretch)),
                    ("frame_layout", ch(layout)),
                ],
                false,
            ));
        }
        out.push(("vr_rotate_sphere", vec![("tilt", fl(-23.0)), ("pan", fl(41.0)), ("roll", fl(17.0)), ("frame_layout", ch(layout))], false));
        for feather in [0.0, 35.0] {
            out.push((
                "vr_plane_to_sphere",
                vec![("scale", fl(105.0)), ("tilt", fl(19.0)), ("pan", fl(-31.0)), ("roll", fl(12.0)), ("feather", fl(feather)), ("frame_layout", ch(layout))],
                false,
            ));
        }
    }
    out
}

/// Every effect case on the GPU against its CPU reference, on the working image read back as f32.
#[test]
fn gpu_effects_match_cpu_exactly() {
    let Some((dev, q)) = device() else {
        eprintln!("no GPU adapter; skipping");
        return;
    };
    let mut c = GpuCompositor::new(&dev, &q);
    let (w, h) = (67u32, 41u32);
    let px = picture(w, h);
    let frame = VideoFrame::rgba_f32(w, h, px.clone());
    let mut failures = Vec::new();
    for (id, params, steps) in cases() {
        let e = effect(id, &params);
        let op = FxOp::eval(&e, &cx(Tick::ZERO, 1.0), w as usize, h as usize).unwrap_or_else(|| panic!("{id} has no GPU op"));
        assert!(op.gpu_ok(), "{id} {params:?}: not GPU-capable");
        let mut cpu = filmcraft_render::Image { w: w as usize, h: h as usize, px: px.clone() };
        if id == "drop_shadow" {
            drop_shadow_oracle(&mut cpu, &e, &cx(Tick::ZERO, 1.0));
        } else if matches!(
            id,
            "noise"
                | "noise_legacy"
                | "gaussian_blur_legacy"
                | "lens_flare"
                | "directional_blur_legacy"
                | "reduce_interlace_flicker"
                | "mosaic"
                | "mosaic_legacy"
                | "replicate"
                | "magnify"
                | "magnify_legacy"
                | "twirl"
                | "twirl_legacy"
                | "spherize"
                | "corner_pin"
                | "turbulent_displace"
                | "lens_distortion"
                | "wave_warp"
                | "rounded_crop"
                | "rgb_split"
                | "vr_chromatic_aberrations"
                | "vr_color_gradients"
                | "vr_projection"
                | "vr_rotate_sphere"
                | "vr_plane_to_sphere"
                | "checkerboard"
                | "grid"
                | "circle"
                | "ellipse"
                | "ramp"
                | "gradient"
                | "four_color_gradient"
                | "cell_pattern"
                | "emboss"
                | "color_emboss"
                | "find_edges"
                | "strobe"
                | "median"
                | "grow"
                | "shrink"
                | "move"
                | "spin"
                | "wiggle"
                | "camera_shake"
                | "rotate_3d"
                | "basic_3d"
                | "bevel_alpha"
                | "block_dissolve"
                | "linear_wipe_legacy"
                | "logo_cutout"
                | "alpha_glow"
        ) {
            filmcraft_render::effects::apply(&mut cpu, &e, &cx(Tick::ZERO, 1.0));
        } else {
            op.apply(&mut cpu);
        }
        let fx = LayerFx { size: (w, h), decimation: 1, ops: vec![op] };
        let (_, _, gpu) = c.effect_image(&frame, &fx).expect("effect image");
        let (worst, flips) = compare(&cpu.px, &gpu);
        eprintln!("{id} {params:?}: max rel diff {worst:.2e}, {:.3}% flipped", flips * 100.0);
        if !(worst <= parity_budget(id) && flips <= if steps { FLIPS } else { 0.0 }) {
            failures.push(format!("{id} {params:?}: {worst:.2e}, flips {flips:.4}"));
        }
    }
    assert!(failures.is_empty(), "GPU effects differ from the CPU:\n{}", failures.join("\n"));
}

#[test]
fn channel_blur_typed_cpu_matches_original_for_dimensions_and_edge_modes() {
    let (w, h) = (13u32, 9u32);
    let pixels = picture(w, h);
    for params in [
        vec![("red", fl(5.0)), ("green", fl(9.0)), ("blue", fl(13.0)), ("alpha", fl(4.0))],
        vec![("red", fl(7.0)), ("blue", fl(11.0)), ("dimensions", ch(1)), ("repeat_edge", ParamValue::Bool(true))],
    ] {
        let e = effect("channel_blur", &params);
        let ctx = cx(Tick::ZERO, 1.0);
        let op = FxOp::eval(&e, &ctx, w as usize, h as usize).expect("channel blur route");
        assert!(op.gpu_ok());
        let mut expected = filmcraft_render::Image { w: w as usize, h: h as usize, px: pixels.clone() };
        let mut actual = expected.clone();
        assert!(filmcraft_render::vfx::apply(&mut expected, &e, &ctx), "missing original channel blur");
        op.apply(&mut actual);
        for (i, (a, b)) in actual.px.iter().zip(expected.px.iter()).enumerate() {
            assert!((a - b).abs() <= EXACT, "channel_blur mismatch at {i}: {a} != {b}");
        }
    }
}

#[test]
fn alpha_glow_typed_cpu_matches_original_and_small_radius_is_noop() {
    let (w, h) = (17u32, 11u32);
    let pixels = picture(w, h);
    for params in [
        vec![],
        vec![
            ("glow", fl(18.0)),
            ("brightness", fl(210.0)),
            ("start_color", col(0.2, 0.8, 0.9)),
            ("end_color", col(0.9, 0.2, 0.3)),
            ("use_end", ParamValue::Bool(true)),
        ],
        vec![("glow", fl(12.0)), ("brightness", fl(150.0)), ("fade_out", ParamValue::Bool(false))],
        vec![("glow", fl(0.2)), ("brightness", fl(255.0)), ("fade_out", ParamValue::Bool(false))],
    ] {
        let e = effect("alpha_glow", &params);
        let ctx = cx(Tick::ZERO, 1.0);
        let op = FxOp::eval(&e, &ctx, w as usize, h as usize).expect("alpha glow route");
        assert!(op.gpu_ok());
        let mut expected = filmcraft_render::Image { w: w as usize, h: h as usize, px: pixels.clone() };
        let mut actual = expected.clone();
        assert!(filmcraft_render::vfx::apply(&mut expected, &e, &ctx), "missing original alpha glow");
        op.apply(&mut actual);
        for (i, (a, b)) in actual.px.iter().zip(expected.px.iter()).enumerate() {
            assert!((a - b).abs() <= EXACT, "alpha_glow mismatch at {i}: {a} != {b}");
        }
    }
}

#[test]
fn gpu_channel_blur_large_radius_preserves_unselected_channels() {
    let Some((dev, q)) = device() else {
        eprintln!("no GPU adapter; skipping");
        return;
    };
    let (w, h) = (67u32, 41u32);
    let pixels = picture(w, h);
    let frame = VideoFrame::rgba_f32(w, h, pixels.clone());
    let e = effect("channel_blur", &[("red", fl(268.0)), ("dimensions", ch(1)), ("repeat_edge", ParamValue::Bool(true))]);
    let ctx = cx(Tick::ZERO, 1.0);
    let op = FxOp::eval(&e, &ctx, w as usize, h as usize).expect("channel blur route");
    let FxOp::ChannelBlur { rx, .. } = &op else { panic!("expected typed channel blur") };
    assert!(rx[0].iter().any(|r| *r > 32), "case must dispatch the running-sum blur path");
    let mut expected = filmcraft_render::Image { w: w as usize, h: h as usize, px: pixels.clone() };
    assert!(filmcraft_render::vfx::apply(&mut expected, &e, &ctx), "missing original channel blur");
    let (_, _, gpu) = GpuCompositor::new(&dev, &q).effect_image(&frame, &LayerFx { size: (w, h), decimation: 1, ops: vec![op] }).expect("GPU channel blur");
    let (worst, flips) = compare(&expected.px, &gpu);
    assert!(worst <= EXACT && flips == 0.0, "large channel blur mismatch: {worst:.2e}, flips {flips:.4}");
    for (i, (src, out)) in pixels.as_chunks::<4>().0.iter().zip(gpu.as_chunks::<4>().0.iter()).enumerate() {
        assert_eq!(&out[1..], &src[1..], "large red blur changed G/B/A at pixel {i}");
    }
}

#[test]
fn gpu_vr_effects_report_pixel_deltas_against_independent_and_typed_cpu() {
    let (dev, q) = required_gpu_device();
    let mut compositor = GpuCompositor::new(&dev, &q);
    let (w, h) = (67u32, 41u32);
    let pixels = picture(w, h);
    let frame = VideoFrame::rgba_f32(w, h, pixels.clone());
    let mut cases: Vec<(&str, Vec<(&str, ParamValue)>)> = Vec::new();
    for layout in 0..=2 {
        cases.push((
            "vr_chromatic_aberrations",
            vec![
                ("red", fl(31.0)),
                ("green", fl(-23.0)),
                ("blue", fl(47.0)),
                ("center_x", fl(37.0)),
                ("center_y", fl(-19.0)),
                ("falloff", fl(73.0)),
                ("inverse", ParamValue::Bool(true)),
                ("frame_layout", ch(layout)),
            ],
        ));
        for stretch in [false, true] {
            cases.push((
                "vr_projection",
                vec![
                    ("fov", fl(235.0)),
                    ("tilt", fl(-23.0)),
                    ("pan", fl(41.0)),
                    ("roll", fl(17.0)),
                    ("stretch", ParamValue::Bool(stretch)),
                    ("frame_layout", ch(layout)),
                ],
            ));
        }
        cases.push(("vr_rotate_sphere", vec![("tilt", fl(-23.0)), ("pan", fl(41.0)), ("roll", fl(17.0)), ("frame_layout", ch(layout))]));
    }

    let summarize = |reference: &[f32], actual: &[f32]| {
        let first = reference.iter().zip(actual).position(|(a, b)| (a - b).abs() > EXACT);
        let (max_index, max_delta) = reference.iter().zip(actual).enumerate().fold((0usize, 0.0f32), |best, (i, (a, b))| {
            let delta = (a - b).abs();
            if delta > best.1 { (i, delta) } else { best }
        });
        let detail = |index: Option<usize>| {
            index.map_or_else(
                || "none".to_owned(),
                |i| {
                    format!(
                        "index={i} xy=({}, {}) channel={} ref={} got={} abs={}",
                        (i / 4) % w as usize,
                        (i / 4) / w as usize,
                        i % 4,
                        reference[i],
                        actual[i],
                        (reference[i] - actual[i]).abs()
                    )
                },
            )
        };
        (max_delta, detail(first), detail(Some(max_index)))
    };

    let mut failures = Vec::new();
    for (id, params) in cases {
        let e = effect(id, &params);
        let ctx = cx(Tick::ZERO, 1.0);
        let op = FxOp::eval(&e, &ctx, w as usize, h as usize).unwrap_or_else(|| panic!("{id}: missing typed route"));
        assert!(op.gpu_ok(), "{id} {params:?}: ineligible operation");
        let mut oracle = filmcraft_render::Image { w: w as usize, h: h as usize, px: pixels.clone() };
        assert!(filmcraft_render::vfx::apply(&mut oracle, &e, &ctx), "{id}: missing independent CPU oracle");
        let mut typed = filmcraft_render::Image { w: w as usize, h: h as usize, px: pixels.clone() };
        op.apply(&mut typed);
        let (_, _, gpu) = compositor
            .effect_image(&frame, &LayerFx { size: (w, h), decimation: 1, ops: vec![op] })
            .unwrap_or_else(|| panic!("{id}: GPU effect pass unavailable"));
        let (typed_max, typed_first, typed_worst) = summarize(&oracle.px, &typed.px);
        let (gpu_max, gpu_first, gpu_worst) = summarize(&oracle.px, &gpu);
        let (typed_gpu_max, typed_gpu_first, typed_gpu_worst) = summarize(&typed.px, &gpu);
        eprintln!(
            "VRDIAG {id} {params:?}: typed-vfx max_abs={typed_max} first={typed_first} worst={typed_worst}; gpu-vfx max_abs={gpu_max} first={gpu_first} worst={gpu_worst}; gpu-typed max_abs={typed_gpu_max} first={typed_gpu_first} worst={typed_gpu_worst}"
        );
        let (normalized_max, flips) = compare(&oracle.px, &gpu);
        if normalized_max > parity_budget(id) || flips > 0.0 {
            failures.push(format!(
                "{id} {params:?}: max_rel={normalized_max:.6e}, flips={flips:.6}, gpu-vfx max_abs={gpu_max}, first={gpu_first}, worst={gpu_worst}"
            ));
        }
    }
    assert!(failures.is_empty(), "focused VR GPU parity failures:\n{}", failures.join("\n"));
}

#[test]
fn gpu_effect_catalog_family_parity_requires_adapter() {
    let (dev, q) = required_gpu_device();
    let mut compositor = GpuCompositor::new(&dev, &q);
    let (w, h) = (67u32, 41u32);
    let pixels = picture(w, h);
    let frame = VideoFrame::rgba_f32(w, h, pixels.clone());
    let routed = [
        "alpha_glow",
        "cineon_converter",
        "noise",
        "noise_legacy",
        "mosaic",
        "mosaic_legacy",
        "replicate",
        "magnify",
        "magnify_legacy",
        "twirl",
        "twirl_legacy",
        "spherize",
        "corner_pin",
        "turbulent_displace",
        "lens_distortion",
        "wave_warp",
        "rounded_crop",
        "rgb_split",
        "vr_chromatic_aberrations",
        "vr_color_gradients",
        "vr_projection",
        "vr_rotate_sphere",
        "vr_plane_to_sphere",
        "checkerboard",
        "grid",
        "circle",
        "ellipse",
        "ramp",
        "gradient",
        "four_color_gradient",
        "cell_pattern",
        "emboss",
        "color_emboss",
        "find_edges",
        "strobe",
        "median",
        "channel_blur",
        "grow",
        "shrink",
        "move",
        "spin",
        "wiggle",
        "camera_shake",
        "bevel_alpha",
        "block_dissolve",
        "linear_wipe_legacy",
        "logo_cutout",
        "drop_shadow",
    ];
    let cases: Vec<_> = cases().into_iter().filter(|(id, _, _)| routed.contains(id)).collect();
    let mut failures = Vec::new();
    for (id, params, steps) in cases {
        let e = effect(id, &params);
        let ctx = cx(Tick::ZERO, 1.0);
        let op = FxOp::eval(&e, &ctx, w as usize, h as usize).unwrap_or_else(|| panic!("{id}: missing typed route"));
        assert!(op.gpu_ok(), "{id} {params:?}: ineligible route");
        let mut cpu = filmcraft_render::Image { w: w as usize, h: h as usize, px: pixels.clone() };
        match id {
            "drop_shadow" => drop_shadow_oracle(&mut cpu, &e, &ctx),
            // These 12 routes have separately checked legacy equivalence in render's legacy_cpu_oracle tests.
            "replicate"
            | "twirl"
            | "twirl_legacy"
            | "lens_distortion"
            | "wave_warp"
            | "grid"
            | "circle"
            | "ramp"
            | "four_color_gradient"
            | "emboss"
            | "find_edges"
            | "strobe" => op.apply(&mut cpu),
            // Render verifies these typed CPU references independently: sorted median taps and bevel's direct alpha-gradient formula.
            "median" | "bevel_alpha" => op.apply(&mut cpu),
            _ => assert!(filmcraft_render::vfx::apply(&mut cpu, &e, &ctx), "{id}: missing independent CPU oracle"),
        }
        let (_, _, gpu) = compositor
            .effect_image(&frame, &LayerFx { size: (w, h), decimation: 1, ops: vec![op] })
            .unwrap_or_else(|| panic!("GPU family effect unavailable: {}", compositor.fx_unavailable_reason.as_deref().unwrap_or("unknown reason")));
        let cpu_invalid = cpu.px.iter().enumerate().find(|(_, value)| !value.is_finite());
        let gpu_invalid = gpu.iter().enumerate().find(|(_, value)| !value.is_finite());
        if let Some((index, value)) = cpu_invalid {
            failures.push(format!("{id} {params:?}: non-finite CPU component index={index} value={value:?}"));
        }
        if let Some((index, value)) = gpu_invalid {
            failures.push(format!("{id} {params:?}: non-finite GPU component index={index} value={value:?}"));
        }
        if cpu_invalid.is_some() || gpu_invalid.is_some() {
            continue;
        }
        let (worst, flips) = compare(&cpu.px, &gpu);
        if !(worst <= parity_budget(id) && flips <= if steps { FLIPS } else { 0.0 }) {
            failures.push(format!("{id} {params:?}: worst={worst:.2e}, flips={flips:.4}"));
        }
    }
    assert!(failures.is_empty(), "GPU family parity failures:\n{}", failures.join("\n"));
}

#[test]
fn gpu_block_dissolve_matches_cpu_hash_oracle() {
    let (dev, q) = required_gpu_device();
    let (w, h) = (67u32, 41u32);
    let pixels = picture(w, h);
    let frame = VideoFrame::rgba_f32(w, h, pixels.clone());
    let ctx = cx(Tick::ZERO, 1.0);
    let e = effect(
        "block_dissolve",
        &[("completion", fl(47.0)), ("block_w", fl(2.3)), ("block_h", fl(1.7)), ("feather", fl(60.0)), ("soft", ParamValue::Bool(true))],
    );
    let mut expected = filmcraft_render::Image { w: w as usize, h: h as usize, px: pixels };
    assert!(filmcraft_render::vfx::apply(&mut expected, &e, &ctx));
    let op = FxOp::eval(&e, &ctx, w as usize, h as usize).expect("Block Dissolve GPU route");
    let (_, _, actual) =
        GpuCompositor::new(&dev, &q).effect_image(&frame, &LayerFx { size: (w, h), decimation: 1, ops: vec![op] }).expect("Block Dissolve GPU pass");
    for (i, (gpu, cpu)) in actual.iter().zip(expected.px.iter()).enumerate() {
        assert!((gpu - cpu).abs() <= EXACT, "Block Dissolve GPU/CPU mismatch at {i}: {gpu} != {cpu}");
    }
}

#[test]
fn typed_cpu_family_ops_match_independent_legacy_oracles_diagnostic() {
    let (w, h) = (67usize, 41usize);
    let pixels = picture(w as u32, h as u32);
    let ctx = cx(Tick::ZERO, 1.0);
    let targets = ["mosaic", "ellipse", "emboss", "block_dissolve", "vr_chromatic_aberrations"];
    let mut selected =
        vec![("mosaic", Vec::new(), false), ("ellipse", Vec::new(), false), ("emboss", Vec::new(), false), ("block_dissolve", Vec::new(), false)];
    selected.extend(family_cases());
    let mut failures = Vec::new();
    let mut tested_ids = Vec::new();
    let mut case_count = 0usize;
    for (id, params, _) in selected.into_iter().filter(|(id, _, _)| targets.contains(id)) {
        case_count += 1;
        if !tested_ids.contains(&id) {
            tested_ids.push(id);
        }
        let e = effect(id, &params);
        let op = FxOp::eval(&e, &ctx, w, h).unwrap_or_else(|| panic!("{id}: missing typed operation"));
        let mut expected = filmcraft_render::Image { w, h, px: pixels.clone() };
        let mut typed = expected.clone();
        match id {
            "emboss" => emboss_oracle(&mut expected, &e, &ctx),
            _ => assert!(filmcraft_render::vfx::apply(&mut expected, &e, &ctx), "{id}: missing independent legacy oracle"),
        }
        op.apply(&mut typed);
        let expected_invalid = expected.px.iter().enumerate().find(|(_, value)| !value.is_finite());
        let typed_invalid = typed.px.iter().enumerate().find(|(_, value)| !value.is_finite());
        if let Some((index, value)) = expected_invalid {
            failures.push(format!("{id} {params:?}: non-finite legacy CPU component {index}={value:?}"));
        }
        if let Some((index, value)) = typed_invalid {
            failures.push(format!("{id} {params:?}: non-finite typed CPU component {index}={value:?}"));
        }
        if expected_invalid.is_some() || typed_invalid.is_some() {
            continue;
        }
        let first = typed.px.iter().zip(&expected.px).enumerate().find(|(_, (actual, reference))| (**actual - **reference).abs() > EXACT);
        let (max_abs, max_index) = typed.px.iter().zip(&expected.px).enumerate().fold((0.0f32, 0usize), |best, (i, (a, b))| {
            let d = (a - b).abs();
            if d > best.0 { (d, i) } else { best }
        });
        if let Some((index, (actual, reference))) = first {
            failures.push(format!("{id} {params:?}: first component {index}, typed={actual}, legacy={reference}, max_abs={max_abs} at {max_index}"));
        }
    }
    assert!(case_count > 0, "CPU oracle diagnostic selected no cases");
    assert!(
        targets.iter().all(|id| tested_ids.contains(id)),
        "CPU oracle diagnostic missed IDs: {:?}",
        targets.iter().filter(|id| !tested_ids.contains(id)).collect::<Vec<_>>()
    );
    assert!(failures.is_empty(), "typed CPU ops differ from independent legacy CPU oracles:\n{}", failures.join("\n"));
}

#[test]
fn drop_shadow_typed_cpu_matches_original_oracle() {
    let (w, h) = (67usize, 41usize);
    let pixels = picture(w as u32, h as u32);
    for params in [
        vec![],
        vec![("color", col(0.2, 0.7, 0.9)), ("opacity", fl(83.0)), ("direction", fl(47.0)), ("distance", fl(12.5)), ("softness", fl(14.0))],
        vec![("only", ParamValue::Bool(true)), ("opacity", fl(67.0)), ("distance", fl(3.25)), ("softness", fl(8.0))],
    ] {
        let e = effect("drop_shadow", &params);
        let ctx = cx(Tick::ZERO, 0.75);
        let op = FxOp::eval(&e, &ctx, w, h).expect("drop shadow route");
        let mut expected = filmcraft_render::Image { w, h, px: pixels.clone() };
        let mut actual = expected.clone();
        drop_shadow_oracle(&mut expected, &e, &ctx);
        op.apply(&mut actual);
        for (i, (a, b)) in actual.px.iter().zip(expected.px.iter()).enumerate() {
            assert!((a - b).abs() <= EXACT, "drop_shadow typed/oracle mismatch at {i}: {a} != {b}");
        }
    }
}

#[test]
fn gpu_extract_zero_softness_has_no_fractional_edge() {
    let Some((dev, q)) = device() else {
        eprintln!("no GPU adapter; skipping");
        return;
    };
    let mut c = GpuCompositor::new(&dev, &q);
    let (w, h) = (67, 41);
    let px = picture(w, h);
    let frame = VideoFrame::rgba_f32(w, h, px.clone());
    let mut masks = Vec::new();
    for invert in [false, true] {
        let e = effect("extract", &[("black", fl(90.0)), ("white", fl(160.0)), ("invert", ParamValue::Bool(invert))]);
        let op = FxOp::eval(&e, &cx(Tick::ZERO, 1.0), w as usize, h as usize).unwrap();
        let (_, _, gpu) = c.effect_image(&frame, &LayerFx { size: (w, h), decimation: 1, ops: vec![op] }).unwrap();
        for (src, out) in px.as_chunks::<4>().0.iter().zip(gpu.as_chunks::<4>().0.iter()) {
            assert_eq!(out[3], src[3], "Extract must preserve alpha");
            assert!(out[0] == 0.0 || out[0] == out[3], "zero softness produced a fractional edge: {out:?}");
            assert_eq!(out[0], out[1]);
            assert_eq!(out[0], out[2]);
        }
        masks.push(gpu);
    }
    for ((normal, inverted), src) in masks[0].as_chunks::<4>().0.iter().zip(masks[1].as_chunks::<4>().0.iter()).zip(px.as_chunks::<4>().0.iter()) {
        assert_eq!(normal[0] + inverted[0], src[3], "inversion must complement the premultiplied mask");
    }
}

#[test]
fn gpu_keys_keep_alpha_and_apply_expected_cutoffs() {
    let Some((dev, q)) = device() else {
        eprintln!("no GPU adapter; skipping");
        return;
    };
    let mut c = GpuCompositor::new(&dev, &q);
    let (w, h) = (4u32, 1u32);
    let px = vec![
        0.0, 0.0, 0.0, 0.5, // black, partial alpha
        0.5, 0.5, 0.5, 0.5, // mid-grey, partial alpha
        0.0, 0.0, 0.5, 0.5, // blue key color
        0.125, 0.25, 0.375, 0.0, // transparent with hidden RGB
    ];
    let frame = VideoFrame::rgba_f32(w, h, px.clone());
    for (id, expected_alpha) in [("luma_key", [0.0, 0.5, 0.5, 0.0]), ("color_key", [0.5, 0.5, 0.0, 0.0])] {
        let e = effect(id, &[]);
        let op = FxOp::eval(&e, &cx(Tick::ZERO, 1.0), w as usize, h as usize).unwrap();
        assert!(op.gpu_ok());
        let (_, _, gpu) = c.effect_image(&frame, &LayerFx { size: (w, h), decimation: 1, ops: vec![op] }).unwrap();
        for (i, alpha) in expected_alpha.into_iter().enumerate() {
            assert!((gpu[i * 4 + 3] - alpha).abs() <= EXACT, "{id} pixel {i}: {:?}", &gpu[i * 4..i * 4 + 4]);
        }
        assert_eq!(&gpu[12..16], &px[12..16], "{id} must preserve zero-alpha pixels");
    }
}

#[test]
fn gpu_noise_matches_oracle_at_exact_positive_and_negative_ticks() {
    let Some((dev, q)) = device() else {
        eprintln!("no GPU adapter; skipping");
        return;
    };
    let mut compositor = GpuCompositor::new(&dev, &q);
    let (w, h) = (19u32, 11u32);
    let pixels = picture(w, h);
    let frame = VideoFrame::rgba_f32(w, h, pixels.clone());
    for id in ["noise", "noise_legacy"] {
        let mut outputs = Vec::new();
        for tick in [Tick(-7), Tick(123_456)] {
            let mut e = effect(id, &[("amount", fl(64.0)), ("color", ParamValue::Bool(true)), ("clip", ParamValue::Bool(false))]);
            if id == "noise" {
                e.params.get_mut("grain_size").unwrap().value = fl(2.5);
            }
            let ctx = cx(tick, 0.5);
            let op = FxOp::eval(&e, &ctx, w as usize, h as usize).unwrap();
            assert!(op.gpu_ok());
            let (_, _, gpu) = compositor.effect_image(&frame, &LayerFx { size: (w, h), decimation: 1, ops: vec![op] }).unwrap();
            let mut cpu = filmcraft_render::Image { w: w as usize, h: h as usize, px: pixels.clone() };
            assert!(filmcraft_render::vfx::apply(&mut cpu, &e, &ctx));
            let (worst, flips) = compare(&cpu.px, &gpu);
            assert!(worst <= EXACT && flips == 0.0, "{id} at tick {}: {worst} / {flips}", tick.0);
            outputs.push(gpu);
        }
        assert_ne!(outputs[0], outputs[1], "{id} should use exact media ticks as deterministic seeds");
    }
}

#[test]
fn gpu_wave_warp_and_strobe_follow_clip_seconds() {
    let Some((dev, q)) = device() else {
        eprintln!("no GPU adapter; skipping");
        return;
    };
    let mut compositor = GpuCompositor::new(&dev, &q);
    let (w, h) = (17u32, 11u32);
    let pixels = picture(w, h);
    let frame = VideoFrame::rgba_f32(w, h, pixels.clone());
    for (id, params) in
        [("wave_warp", vec![("height", fl(12.0)), ("width", fl(9.0)), ("speed", fl(3.0))]), ("strobe", vec![("duration", fl(0.05)), ("period", fl(0.5))])]
    {
        let mut outputs = Vec::new();
        for seconds in [0.0, 0.1] {
            let e = effect(id, &params);
            let t = Tick::from_seconds_f64(seconds);
            let ctx = FxCtx { seconds, ..cx(t, 1.0) };
            let op = FxOp::eval(&e, &ctx, w as usize, h as usize).unwrap();
            assert!(op.gpu_ok());
            let (_, _, gpu) = compositor.effect_image(&frame, &LayerFx { size: (w, h), decimation: 1, ops: vec![op] }).unwrap();
            let mut cpu = filmcraft_render::Image { w: w as usize, h: h as usize, px: pixels.clone() };
            filmcraft_render::effects::apply(&mut cpu, &e, &ctx);
            let (worst, flips) = compare(&cpu.px, &gpu);
            assert!(worst <= EXACT && flips == 0.0, "{id} at {seconds}: {worst} / {flips}");
            outputs.push(gpu);
        }
        assert_ne!(outputs[0], outputs[1], "{id} should follow clip seconds");
    }
}

#[test]
fn gpu_turbulent_displace_matches_oracle_for_all_modes_pinning_and_cycles() {
    let (dev, q) = required_gpu_device();
    let mut compositor = GpuCompositor::new(&dev, &q);
    let (w, h) = (17u32, 11u32);
    let pixels = picture(w, h);
    let frame = VideoFrame::rgba_f32(w, h, pixels.clone());
    for kind in 0..9 {
        for pin in 0..4 {
            for cycle in [false, true] {
                let mut e = effect("turbulent_displace", &[]);
                for (key, value) in [
                    ("displacement", ch(kind)),
                    ("pinning", ch(pin)),
                    ("cycle", ParamValue::Bool(cycle)),
                    ("seed", fl(9999.0)),
                    ("size", fl(8.5)),
                    ("complexity", fl(3.5)),
                    ("evolution", fl(725.0)),
                    ("amount", fl(8.0)),
                    ("cycle_revs", fl(2.5)),
                    ("offset", pt(7.0, 3.0)),
                ] {
                    e.params.get_mut(key).unwrap_or_else(|| panic!("turbulent_displace missing parameter {key}")).value = value;
                }
                let ctx = cx(Tick::ZERO, 1.0);
                let op = FxOp::eval(&e, &ctx, w as usize, h as usize).expect("turbulent operation");
                assert!(op.gpu_ok(), "mode {kind}, pin {pin}, cycle {cycle}");
                let (_, _, gpu) = compositor.effect_image(&frame, &LayerFx { size: (w, h), decimation: 1, ops: vec![op] }).expect("turbulent GPU pass");
                let mut cpu = filmcraft_render::Image { w: w as usize, h: h as usize, px: pixels.clone() };
                assert!(filmcraft_render::vfx::apply(&mut cpu, &e, &ctx));
                let (worst, flips) = compare(&cpu.px, &gpu);
                assert!(worst <= EXACT && flips == 0.0, "mode {kind}, pin {pin}, cycle {cycle}: {worst} / {flips}");
            }
        }
    }
}

/// Chains run in order (and Unsharp keeps its original while blurring, between other blurs).
#[test]
fn gpu_effect_chains_match_cpu() {
    let Some((dev, q)) = device() else {
        eprintln!("no GPU adapter; skipping");
        return;
    };
    let mut c = GpuCompositor::new(&dev, &q);
    let (w, h) = (96u32, 54u32);
    let px = picture(w, h);
    let frame = VideoFrame::rgba_f32(w, h, px.clone());
    let chains: Vec<Vec<EffectInstance>> = vec![
        vec![
            effect("brightness_contrast", &[("brightness", fl(20.0)), ("contrast", fl(30.0))]),
            effect("gaussian_blur", &[("blurriness", fl(8.0))]),
            effect("tint", &[("amount", fl(50.0))]),
        ],
        vec![
            effect("gaussian_blur", &[("blurriness", fl(90.0)), ("repeat_edge", ParamValue::Bool(true))]),
            effect("unsharp_mask", &[("amount", fl(200.0)), ("radius", fl(4.0))]),
            effect("gaussian_blur", &[("blurriness", fl(3.0)), ("dimensions", ParamValue::Choice(1))]),
            effect("sharpen", &[("amount", fl(50.0))]),
        ],
        vec![
            effect("crop", &[("left", fl(10.0)), ("bottom", fl(20.0)), ("feather", fl(4.0))]),
            effect("transform", &[("rotation", fl(-15.0)), ("scale_height", fl(110.0))]),
            effect("horizontal_flip", &[]),
            effect("levels", &[("in_black", fl(15.0)), ("gamma", fl(80.0))]),
            effect("offset", &[("shift", pt(70.0, 10.0))]),
            effect("invert", &[("channel", ParamValue::Choice(4))]),
            effect("directional_blur", &[("length", fl(6.0)), ("direction", fl(90.0))]),
        ],
    ];
    for chain in chains {
        let ops: Vec<FxOp> = chain.iter().filter_map(|e| FxOp::eval(e, &cx(Tick::ZERO, 1.0), w as usize, h as usize)).collect();
        assert!(ops.iter().all(FxOp::gpu_ok));
        let mut cpu = filmcraft_render::Image { w: w as usize, h: h as usize, px: px.clone() };
        for op in &ops {
            op.apply(&mut cpu);
        }
        let names: Vec<&str> = chain.iter().map(|e| e.effect.as_str()).collect();
        let (_, _, gpu) = c.effect_image(&frame, &LayerFx { size: (w, h), decimation: 1, ops }).expect("effect image");
        let (worst, flips) = compare(&cpu.px, &gpu);
        eprintln!("{names:?}: max rel diff {worst:.2e}, {:.3}% flipped", flips * 100.0);
        assert!(worst < EXACT * 2.0 && flips == 0.0, "{names:?}: {worst}, {flips}");
    }
}

/// Keyframed parameters are evaluated at the layer's time on the CPU: the GPU result follows them.
#[test]
fn keyframed_effects_follow_time() {
    let Some((dev, q)) = device() else {
        eprintln!("no GPU adapter; skipping");
        return;
    };
    let mut c = GpuCompositor::new(&dev, &q);
    let (w, h) = (48u32, 32u32);
    let px = picture(w, h);
    let frame = VideoFrame::rgba_f32(w, h, px.clone());
    let mut blur = effect("gaussian_blur", &[]);
    let mut bc = effect("brightness_contrast", &[]);
    let p = blur.params.get_mut("blurriness").expect("blurriness");
    p.put_keyframe(Tick::ZERO, fl(0.0));
    p.put_keyframe(Tick::from_seconds_f64(1.0), fl(30.0));
    let p = bc.params.get_mut("brightness").expect("brightness");
    p.put_keyframe(Tick::ZERO, fl(-50.0));
    p.put_keyframe(Tick::from_seconds_f64(1.0), fl(60.0));
    let mut last: Option<Vec<f32>> = None;
    for s in [0.0, 0.3, 0.75, 1.0] {
        let cx = cx(Tick::from_seconds_f64(s), 1.0);
        let ops: Vec<FxOp> = [&bc, &blur].iter().filter_map(|e| FxOp::eval(e, &cx, w as usize, h as usize)).collect();
        let mut cpu = filmcraft_render::Image { w: w as usize, h: h as usize, px: px.clone() };
        for op in &ops {
            op.apply(&mut cpu);
        }
        let (_, _, gpu) = c.effect_image(&frame, &LayerFx { size: (w, h), decimation: 1, ops }).expect("effect image");
        let (worst, flips) = compare(&cpu.px, &gpu);
        assert!(worst < EXACT && flips == 0.0, "t = {s}: {worst}, {flips}");
        assert_ne!(last.as_ref(), Some(&gpu), "t = {s}: the keyframes change the picture");
        last = Some(gpu);
    }
}

/// Hostile values: non-finite parameters stay on the CPU (`gpu_ok` is false), huge radii and
/// sizes are capped as on the CPU and run in bounded time on the GPU.
#[test]
fn hostile_parameters_are_bounded() {
    let (w, h) = (40usize, 24usize);
    let nan = [
        effect("brightness_contrast", &[("brightness", fl(f64::NAN))]),
        effect("gaussian_blur", &[("blurriness", fl(f64::INFINITY))]),
        effect("directional_blur", &[("length", fl(f64::NAN))]),
        effect("transform", &[("rotation", fl(f64::NAN))]),
        effect("crop", &[("left", fl(f64::NAN))]),
        effect("offset", &[("shift", pt(f64::INFINITY, 0.0))]),
        effect("levels", &[("in_black", fl(f64::NEG_INFINITY))]),
        effect("transform", &[("scale_height", fl(1e300))]),
        // minifying resamples (the CPU's mip path) stay on the CPU
        effect("transform", &[("scale_height", fl(20.0))]),
        effect("crop", &[("left", fl(-300.0)), ("zoom", ParamValue::Bool(true))]),
        effect("vignette", &[("amount", fl(f64::NAN))]),
        effect("vignette", &[("midpoint", fl(f64::INFINITY))]),
        effect("vignette", &[("roundness", fl(f64::NAN))]),
        effect("vignette", &[("feather", fl(f64::NAN))]),
        effect("video_limiter", &[("clip_level", fl(f64::NAN))]),
        effect("video_limiter", &[("compression", fl(f64::INFINITY))]),
        effect("video_limiter", &[("axis", fl(f64::NAN))]),
        effect("video_limiter", &[("warning_color", ParamValue::Color([f32::NAN, 0.0, 0.0, 1.0]))]),
        effect("lumetri", &[("exposure", fl(f64::NAN))]),
        effect("lumetri", &[("temperature", fl(f64::INFINITY))]),
        effect("lumetri", &[("contrast", fl(f64::NAN))]),
        effect("lumetri", &[("vignette_amount", fl(f64::NAN))]),
    ];
    for e in &nan {
        let op = FxOp::eval(e, &cx(Tick::ZERO, 1.0), w, h);
        // gaussian with an infinite radius is capped (no blur), which the GPU runs
        if e.effect == "gaussian_blur" {
            assert!(op.as_ref().is_some_and(FxOp::gpu_ok));
            continue;
        }
        assert!(op.as_ref().is_some_and(|o| !o.gpu_ok()), "{}: {op:?}", e.effect);
        // and the CPU reference copes
        let mut img = filmcraft_render::Image::filled(w, h, [0.3, 0.2, 0.1, 1.0]);
        if let Some(op) = op {
            op.apply(&mut img);
        }
    }
    // huge radii: box radii capped by the image size
    let op = FxOp::eval(&effect("gaussian_blur", &[("blurriness", fl(1e30))]), &cx(Tick::ZERO, 1.0), w, h);
    let Some(FxOp::Gaussian { rx, ry, .. }) = op else { panic!("{op:?}") };
    assert!(rx.iter().chain(&ry).all(|r| *r as usize <= 4 * w.max(h).max(8)), "{rx:?} {ry:?}");
    let Some((dev, q)) = device() else {
        eprintln!("no GPU adapter; skipping");
        return;
    };
    let mut c = GpuCompositor::new(&dev, &q);
    let frame = VideoFrame::rgba_f32(w as u32, h as u32, picture(w as u32, h as u32));
    let ops = vec![
        FxOp::eval(&effect("gaussian_blur", &[("blurriness", fl(1e30))]), &cx(Tick::ZERO, 1.0), w, h).expect("op"),
        FxOp::eval(&effect("unsharp_mask", &[("radius", fl(1e30)), ("amount", fl(1e30))]), &cx(Tick::ZERO, 1.0), w, h).expect("op"),
        FxOp::eval(&effect("directional_blur", &[("length", fl(1e30))]), &cx(Tick::ZERO, 1.0), w, h).expect("op"),
    ];
    let t0 = std::time::Instant::now();
    let (_, _, gpu) = c.effect_image(&frame, &LayerFx { size: (w as u32, h as u32), decimation: 1, ops }).expect("effect image");
    assert!(t0.elapsed().as_secs() < 30);
    assert_eq!(gpu.len(), w * h * 4);
    // a working image larger than any texture is refused, not attempted
    assert!(c.effect_image(&frame, &LayerFx { size: (1 << 20, 4), decimation: 1, ops: vec![FxOp::BlackWhite] }).is_none());
}

#[test]
fn lumetri_unsupported_sections_fall_back() {
    let (w, h) = (40usize, 24usize);
    // sharpen > 0 and input LUT (explicitly required by brief)
    let e = effect("lumetri", &[("sharpen", fl(10.0)), ("input_lut", ParamValue::Text("custom.cube".into()))]);
    let op = FxOp::eval(&e, &cx(Tick::ZERO, 1.0), w, h);
    assert!(op.as_ref().is_some_and(|o| !o.gpu_ok()), "sharpen > 0 and input_lut must fall back to CPU");

    // sharpen alone
    let e = effect("lumetri", &[("sharpen", fl(20.0))]);
    let op = FxOp::eval(&e, &cx(Tick::ZERO, 1.0), w, h);
    assert!(op.as_ref().is_some_and(|o| !o.gpu_ok()));

    // input_lut alone
    let e = effect("lumetri", &[("input_lut", ParamValue::Text("test.cube".into()))]);
    let op = FxOp::eval(&e, &cx(Tick::ZERO, 1.0), w, h);
    assert!(op.as_ref().is_some_and(|o| !o.gpu_ok()));

    // look_lut
    let e = effect("lumetri", &[("look_lut", ParamValue::Text("look.cube".into()))]);
    let op = FxOp::eval(&e, &cx(Tick::ZERO, 1.0), w, h);
    assert!(op.as_ref().is_some_and(|o| !o.gpu_ok()));

    // look > 0
    let e = effect("lumetri", &[("look", ch(1))]);
    let op = FxOp::eval(&e, &cx(Tick::ZERO, 1.0), w, h);
    assert!(op.as_ref().is_some_and(|o| !o.gpu_ok()));

    // curve_luma modified
    let e = effect("lumetri", &[("curve_luma", ParamValue::Curve(vec![[0.0, 0.1], [1.0, 0.9]]))]);
    let op = FxOp::eval(&e, &cx(Tick::ZERO, 1.0), w, h);
    assert!(op.as_ref().is_some_and(|o| !o.gpu_ok()));

    // hue_vs_sat non-empty
    let e = effect("lumetri", &[("hue_vs_sat", ParamValue::Curve(vec![[0.5, 0.5]]))]);
    let op = FxOp::eval(&e, &cx(Tick::ZERO, 1.0), w, h);
    assert!(op.as_ref().is_some_and(|o| !o.gpu_ok()));

    // wheel offset
    let e = effect("lumetri", &[("wheel_shadows", pt(0.5, 0.0))]);
    let op = FxOp::eval(&e, &cx(Tick::ZERO, 1.0), w, h);
    assert!(op.as_ref().is_some_and(|o| !o.gpu_ok()));

    // wheel lightness
    let e = effect("lumetri", &[("wheel_midtones_l", fl(20.0))]);
    let op = FxOp::eval(&e, &cx(Tick::ZERO, 1.0), w, h);
    assert!(op.as_ref().is_some_and(|o| !o.gpu_ok()));

    // HSL secondary on
    let e = effect("lumetri", &[("hsl_on", ParamValue::Bool(true))]);
    let op = FxOp::eval(&e, &cx(Tick::ZERO, 1.0), w, h);
    assert!(op.as_ref().is_some_and(|o| !o.gpu_ok()));

    // HDR working space
    let e = effect("lumetri", &[]);
    let mut cx_hdr = cx(Tick::ZERO, 1.0);
    cx_hdr.working = filmcraft_color::WorkingSpace::Rec2100Pq;
    let op = FxOp::eval(&e, &cx_hdr, w, h);
    assert!(op.as_ref().is_some_and(|o| !o.gpu_ok()));
}

/// Layers with effect chains in a composited plan — YUV and RGBA sources, a decimated working
/// image, rotated / scaled placement, partial opacity and alpha, blend modes — match the CPU plan
/// executor.
#[test]
fn gpu_effect_layers_match_cpu_plan() {
    let Some((dev, q)) = device() else {
        eprintln!("no GPU adapter; skipping");
        return;
    };
    let mut c = GpuCompositor::new(&dev, &q);
    let (w, h) = (320usize, 180usize);
    let ops = |list: &[EffectInstance], lw: u32, lh: u32, px_scale: f32| -> Vec<FxOp> {
        list.iter().filter_map(|e| FxOp::eval(e, &cx(Tick::ZERO, px_scale), lw as usize, lh as usize)).collect()
    };
    let chain_a = [
        effect("brightness_contrast", &[("brightness", fl(15.0)), ("contrast", fl(25.0))]),
        effect("gaussian_blur", &[("blurriness", fl(6.0))]),
        effect("tint", &[("amount", fl(40.0))]),
    ];
    let chain_b = [
        effect("crop", &[("left", fl(8.0)), ("right", fl(12.0)), ("feather", fl(6.0))]),
        effect("unsharp_mask", &[("amount", fl(120.0)), ("radius", fl(2.0))]),
        effect("color_balance", &[("hi_b", fl(60.0)), ("shadow_r", fl(40.0))]),
    ];
    let chain_c = [effect("mirror", &[("angle", fl(90.0))]), effect("gaussian_blur", &[("blurriness", fl(50.0)), ("dimensions", ParamValue::Choice(1))])];
    let ramp = ramp_layer(160, 120, 0);
    for mode in [Blend::Normal, Blend::Screen, Blend::Multiply, Blend::Overlay] {
        let plan = FramePlan::Layers {
            width: w,
            height: h,
            layers: vec![
                // YUV source decoded at half size (decimation 2) with effects, filling the output
                PlanLayer {
                    frame: yuv_frame(640, 360),
                    matrix: Affine::IDENTITY,
                    opacity: 1.0,
                    blend: Blend::Normal,
                    fx: Some(Arc::new(LayerFx { size: (320, 180), decimation: 2, ops: ops(&chain_c, 320, 180, 0.5) })),
                    input: None,
                },
                PlanLayer {
                    frame: ramp.clone(),
                    matrix: Affine::motion(Vec2::new(170.0, 95.0), Vec2::new(1.1, 1.1), 17.0, Vec2::new(80.0, 60.0)),
                    opacity: 0.8,
                    blend: mode,
                    fx: Some(Arc::new(LayerFx { size: (160, 120), decimation: 1, ops: ops(&chain_a, 160, 120, 1.0) })),
                    input: None,
                },
                PlanLayer::new(ramp.clone(), Affine::translate(10.0, 20.0), 0.5, Blend::Normal),
                PlanLayer {
                    frame: yuv_frame(320, 180),
                    matrix: Affine::motion(Vec2::new(250.0, 70.0), Vec2::new(0.45, 0.45), -9.0, Vec2::new(160.0, 90.0)),
                    opacity: 0.7,
                    blend: mode,
                    fx: Some(Arc::new(LayerFx { size: (320, 180), decimation: 1, ops: ops(&chain_b, 320, 180, 1.0) })),
                    input: None,
                },
            ],
        };
        let cpu = execute_cpu(&plan).over_black_rgba8();
        c.composite(&plan);
        let (_, _, gpu) = c.read_output().expect("readback");
        let keep = interior(&plan);
        let (p99, mean, max) = stats8(&cpu, &gpu, &keep);
        let (ap99, amean, _) = stats8(&cpu, &gpu, &vec![true; keep.len()]);
        eprintln!("{mode:?}: interior 8-bit p99 {p99} mean {mean:.3} max {max}; with edges p99 {ap99} mean {amean:.3}");
        assert!(p99 <= 6 && mean < 1.5, "{mode:?}: p99 {p99}, mean {mean}");
    }
}

/// Plans without effects never create working textures.
#[test]
fn plain_layers_skip_the_effect_stage() {
    let Some((dev, q)) = device() else {
        eprintln!("no GPU adapter; skipping");
        return;
    };
    let mut c = GpuCompositor::new(&dev, &q);
    let plan = FramePlan::Layers { width: 64, height: 36, layers: vec![PlanLayer::new(yuv_frame(64, 36), Affine::IDENTITY, 1.0, Blend::Normal)] };
    c.composite(&plan);
    assert!(c.fx.as_ref().is_some_and(fx::FxStage::is_idle));
    let fx = LayerFx { size: (64, 36), decimation: 1, ops: vec![FxOp::BlackWhite] };
    let with = FramePlan::Layers {
        width: 64,
        height: 36,
        layers: vec![PlanLayer { fx: Some(Arc::new(fx)), ..PlanLayer::new(yuv_frame(64, 36), Affine::IDENTITY, 1.0, Blend::Normal) }],
    };
    c.composite(&with);
    assert!(!c.fx.as_ref().is_some_and(fx::FxStage::is_idle));
    // and they are released once no layer needs them
    c.composite(&plan);
    assert!(c.fx.as_ref().is_some_and(fx::FxStage::is_idle));
}

/// Without an effect stage (devices without compute shaders) the compositor renders a layer's
/// effects on the CPU itself: the picture is the same.
#[test]
fn layers_with_effects_fall_back_to_the_cpu_without_a_stage() {
    let Some((dev, q)) = device() else {
        eprintln!("no GPU adapter; skipping");
        return;
    };
    let ops: Vec<FxOp> = [effect("tint", &[("amount", fl(70.0))]), effect("gaussian_blur", &[("blurriness", fl(10.0))])]
        .iter()
        .filter_map(|e| FxOp::eval(e, &cx(Tick::ZERO, 1.0), 160, 120))
        .collect();
    let plan = FramePlan::Layers {
        width: 200,
        height: 150,
        layers: vec![
            PlanLayer::new(yuv_frame(200, 150), Affine::IDENTITY, 1.0, Blend::Normal),
            PlanLayer {
                fx: Some(Arc::new(LayerFx { size: (160, 120), decimation: 1, ops })),
                ..PlanLayer::new(
                    ramp_layer(160, 120, 2),
                    Affine::motion(Vec2::new(100.0, 75.0), Vec2::new(0.9, 0.9), 8.0, Vec2::new(80.0, 60.0)),
                    0.8,
                    Blend::Screen,
                )
            },
        ],
    };
    let cpu = execute_cpu(&plan).over_black_rgba8();
    for stage in [true, false] {
        let mut c = GpuCompositor::new(&dev, &q);
        if !stage {
            c.fx = None;
        }
        c.composite(&plan);
        let (_, _, gpu) = c.read_output().expect("readback");
        let (p99, mean, _) = stats8(&cpu, &gpu, &interior(&plan));
        eprintln!("effect stage {stage}: interior p99 {p99}, mean {mean:.3}");
        assert!(p99 <= 6 && mean < 1.5, "stage {stage}: p99 {p99}, mean {mean}");
    }
}

/// The half-float upload is lossless for this input, so a 1:1 effect source draw must be too.
/// Odd dimensions expose interpolation drift that can invent opacity before any effect runs.
#[test]
fn effect_source_identity_preserves_odd_sized_half_float_texels() {
    let Some((dev, q)) = device() else {
        eprintln!("no GPU adapter; skipping");
        return;
    };
    let mut c = GpuCompositor::new(&dev, &q);
    for (w, h) in [(67u32, 41u32), (96, 54), (64, 32)] {
        let px = picture(w, h);
        let frame = VideoFrame::rgba_f32(w, h, px.clone());
        for n in [0, 1] {
            let (ow, oh, gpu) = c.effect_image(&frame, &LayerFx { size: (w, h), decimation: n, ops: Vec::new() }).expect("source image");
            assert_eq!((ow, oh, gpu.len()), (w, h, px.len()));
            for (i, (expected, actual)) in px.iter().zip(&gpu).enumerate() {
                assert_eq!(actual.to_bits(), expected.to_bits(), "{w}x{h} n{n} source component{i}: {actual} != {expected}");
            }
        }
    }
}

#[test]
fn alpha_invert_does_not_amplify_invented_opacity_at_odd_source_pixels() {
    let Some((dev, q)) = device() else {
        eprintln!("no GPU adapter; skipping");
        return;
    };
    let mut c = GpuCompositor::new(&dev, &q);
    let (w, h) = (67, 41);
    let px = picture(w, h);
    let frame = VideoFrame::rgba_f32(w, h, px.clone());
    let op = FxOp::Invert { channel: 4, blend: 0.2 };
    let mut cpu = filmcraft_render::Image { w: w as usize, h: h as usize, px: px.clone() };
    op.apply(&mut cpu);
    let (_, _, gpu) = c.effect_image(&frame, &LayerFx { size: (w, h), decimation: 1, ops: vec![op] }).expect("alpha invert image");
    assert_eq!(gpu.len(), px.len());
    let (worst, flips) = compare(&cpu.px, &gpu);
    assert!(worst <= EXACT && flips == 0.0, "alpha invert: {worst}, {flips}");
    let mut transparent = 0;
    for (source, result) in px.as_chunks::<4>().0.iter().zip(gpu.as_chunks::<4>().0.iter()) {
        if source[3] == 0.0 {
            transparent += 1;
            assert_eq!(&result[..3], &[0.0; 3], "a transparent source must not acquire neighboring RGB");
            assert!((result[3] - 0.8).abs() <= f32::EPSILON);
        }
    }
    assert_eq!(transparent, 402);
}

/// The exact source grid keeps the existing box-minification behavior at non-unit factors.
#[test]
fn effect_source_integer_decimation_preserves_cpu_box_oracle() {
    let Some((dev, q)) = device() else {
        eprintln!("no GPU adapter; skipping");
        return;
    };
    let mut c = GpuCompositor::new(&dev, &q);
    for (w, h) in [(67u32, 41u32), (96, 54)] {
        let frame = VideoFrame::rgba_f32(w, h, picture(w, h));
        for n in [2, 4, 8] {
            let (ow, oh, cpu) = frame.to_linear_f32_decimated(n as usize);
            let (gw, gh, gpu) =
                c.effect_image(&frame, &LayerFx { size: (ow as u32, oh as u32), decimation: n, ops: Vec::new() }).expect("decimated source image");
            assert_eq!((gw as usize, gh as usize, gpu.len()), (ow, oh, cpu.len()));
            let (worst, flips) = compare(&cpu, &gpu);
            assert!(worst <= EXACT && flips == 0.0, "{w}x{h} n{n} source box: {worst}, {flips}");
        }
    }
}
