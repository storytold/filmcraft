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

/// Compositor throughput on an already-decoded, cached YUV source; excludes video decoding and
/// GPU readback. GPU waits for each frame's work, rather than timing only command submission.
#[test]
#[ignore = "keying compositor benchmark; use optimized render/frame/GPU crates and --nocapture"]
fn bench_keying_compositor() {
    use std::hint::black_box;
    use std::time::Instant;
    let (dev, q) = device().expect("GPU adapter required for benchmark");
    let mut c = GpuCompositor::new(&dev, &q);
    assert!(c.fx.is_some(), "compute effect stage required");
    for (w, h) in [(1920u32, 1080u32), (3840, 2160)] {
        let frame = yuv_frame(w, h);
        for (id, params) in [
            ("ultra_key", vec![("tolerance", fl(20.0))]),
            ("color_key", vec![("tolerance", fl(80.0)), ("feather", fl(10.0))]),
            ("luma_key", vec![("threshold", fl(80.0)), ("cutoff", fl(20.0))]),
        ] {
            let e = effect(id, &params);
            let op = FxOp::eval(&e, &cx(Tick::ZERO, 1.0), w as usize, h as usize).expect("keying op");
            if id == "ultra_key" {
                let px = frame.to_linear_f32();
                let mut img = filmcraft_render::Image { w: w as usize, h: h as usize, px: px.clone() };
                let mut times = [Vec::new(), Vec::new()];
                for round in 0..5 {
                    for index in [round % 2, 1 - round % 2] {
                        img.px.copy_from_slice(&px);
                        let start = Instant::now();
                        if index == 0 {
                            assert!(filmcraft_render::vfx::apply(&mut img, &e, &cx(Tick::ZERO, 1.0)));
                        } else {
                            op.apply(&mut img);
                        }
                        times[index].push(start.elapsed());
                        black_box(&img.px);
                    }
                }
                for t in &mut times {
                    t.sort();
                }
                eprintln!(
                    "Ultra Key CPU stage {w}x{h}: previous {:?}, fused {:?}, {:.2}x",
                    times[0][2],
                    times[1][2],
                    times[0][2].as_secs_f64() / times[1][2].as_secs_f64()
                );
            }
            let mut layer = PlanLayer::new(frame.clone(), Affine::IDENTITY, 1.0, Blend::Normal);
            layer.fx = Some(Arc::new(LayerFx { size: (w, h), decimation: 1, ops: vec![op] }));
            let plan = FramePlan::Layers { width: w as usize, height: h as usize, layers: vec![layer] };
            black_box(execute_cpu(&plan));
            c.composite(&plan);
            dev.poll(wgpu::PollType::wait_indefinitely()).expect("GPU warm-up");
            let mut times = [Vec::new(), Vec::new()];
            for round in 0..5 {
                for index in [round % 2, 1 - round % 2] {
                    let start = Instant::now();
                    for _ in 0..6 {
                        if index == 0 {
                            black_box(execute_cpu(black_box(&plan)));
                        } else {
                            black_box(c.composite(black_box(&plan)));
                            dev.poll(wgpu::PollType::wait_indefinitely()).expect("GPU frame completion");
                        }
                    }
                    times[index].push(start.elapsed() / 6);
                }
            }
            for t in &mut times {
                t.sort();
            }
            let (cpu, gpu) = (times[0][2], times[1][2]);
            eprintln!("{id} {w}x{h}: CPU {cpu:?}, GPU {gpu:?}, {:.2}x", cpu.as_secs_f64() / gpu.as_secs_f64());
        }
    }
}

fn cx(t: Tick, px_scale: f32) -> FxCtx<'static> {
    FxCtx { t, px_scale, seconds: 0.0, timecode: "", clip_name: "", project: None, env: None, working: filmcraft_color::WorkingSpace::Rec709 }
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
    vec![
        ("color_key", vec![], false),
        ("color_key", vec![("color", col(0.0, 0.8, 0.2)), ("tolerance", fl(80.0)), ("feather", fl(15.0))], false),
        ("color_key", vec![("color", col(1.0, 0.1, 0.0)), ("tolerance", fl(255.0)), ("feather", fl(50.0))], false),
        ("ultra_key", vec![], false),
        ("ultra_key", vec![("setting", ch(1)), ("contrast", fl(40.0)), ("mid_point", fl(35.0))], false),
        ("ultra_key", vec![("setting", ch(2)), ("cc_saturation", fl(140.0)), ("cc_hue", fl(-45.0)), ("cc_luminance", fl(80.0))], false),
        ("ultra_key", vec![("key_color", col(0.1, 0.2, 0.9)), ("spill", fl(100.0)), ("output", ch(1))], false),
        ("ultra_key", vec![("key_color", col(0.9, 0.2, 0.1)), ("spill", fl(100.0)), ("output", ch(2))], false),
        ("ultra_key", vec![("pedestal", fl(100.0)), ("tolerance", fl(0.0)), ("transparency", fl(0.0))], false),
        ("luma_key", vec![], false),
        ("luma_key", vec![("threshold", fl(80.0)), ("cutoff", fl(20.0))], false),
        ("luma_key", vec![("threshold", fl(20.0)), ("cutoff", fl(80.0))], false),
        ("luma_key", vec![("threshold", fl(50.0)), ("cutoff", fl(50.0))], false),
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
        ("gamma_correction", vec![("gamma", fl(5.0))], false),
        ("gamma_correction", vec![("gamma", fl(28.0))], false),
        ("gamma_correction", vec![("gamma", fl(0.0))], false),
        ("gamma_correction", vec![("gamma", fl(-3.0))], false),
        ("levels", vec![("in_black", fl(20.0)), ("in_white", fl(230.0)), ("out_black", fl(10.0)), ("out_white", fl(240.0)), ("gamma", fl(150.0))], false),
        ("levels", vec![("in_black", fl(200.0)), ("in_white", fl(0.0)), ("gamma", fl(5.0)), ("out_white", fl(-50.0))], true),
        ("extract", vec![("black", fl(40.0)), ("white", fl(200.0)), ("softness", fl(30.0))], true),
        ("extract", vec![("black", fl(90.0)), ("white", fl(160.0)), ("invert", ParamValue::Bool(true))], true),
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
        ("directional_blur", vec![("length", fl(10.0)), ("direction", fl(30.0))], false),
        ("directional_blur", vec![("length", fl(0.2))], false),
        ("directional_blur", vec![("length", fl(1000.0)), ("direction", fl(-100.0))], false),
        ("sharpen", vec![("amount", fl(80.0))], false),
        ("sharpen", vec![("amount", fl(4000.0))], false),
        ("unsharp_mask", vec![("amount", fl(150.0)), ("radius", fl(3.0)), ("threshold", fl(10.0))], true),
        ("unsharp_mask", vec![("radius", fl(0.1))], false),
        ("unsharp_mask", vec![("amount", fl(300.0)), ("radius", fl(250.0))], false),
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
    ]
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
        op.apply(&mut cpu);
        let fx = LayerFx { size: (w, h), decimation: 1, ops: vec![op] };
        let (_, _, gpu) = c.effect_image(&frame, &fx).expect("effect image");
        let (worst, flips) = compare(&cpu.px, &gpu);
        eprintln!("{id} {params:?}: max rel diff {worst:.2e}, {:.3}% flipped", flips * 100.0);
        if worst > EXACT || flips > if steps { FLIPS } else { 0.0 } {
            failures.push(format!("{id} {params:?}: {worst:.2e}, flips {flips:.4}"));
        }
    }
    assert!(failures.is_empty(), "GPU effects differ from the CPU:\n{}", failures.join("\n"));
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
            effect("brightness_contrast", &[("brightness", fl(10.0))]),
            effect("ultra_key", &[("tolerance", fl(20.0))]),
            effect("gaussian_blur", &[("blurriness", fl(3.0)), ("repeat_edge", ParamValue::Bool(true))]),
            effect("luma_key", &[("threshold", fl(80.0)), ("cutoff", fl(10.0))]),
        ],
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
fn keyframed_keying_follows_time() {
    let Some((dev, q)) = device() else {
        eprintln!("no GPU adapter; skipping");
        return;
    };
    let mut c = GpuCompositor::new(&dev, &q);
    let (w, h) = (67u32, 41u32);
    let px = picture(w, h);
    let frame = VideoFrame::rgba_f32(w, h, px.clone());
    for (id, parameter, from, to) in [("color_key", "tolerance", 0.0, 180.0), ("ultra_key", "tolerance", 0.0, 100.0), ("luma_key", "threshold", 20.0, 90.0)] {
        let mut e = effect(id, &[]);
        let p = e.params.get_mut(parameter).expect("animated parameter");
        p.put_keyframe(Tick::ZERO, fl(from));
        p.put_keyframe(Tick::from_seconds_f64(1.0), fl(to));
        let mut last = None;
        for s in [0.0, 0.25, 0.7, 1.0] {
            let op = FxOp::eval(&e, &cx(Tick::from_seconds_f64(s), 1.0), w as usize, h as usize).expect("keying op");
            assert!(op.gpu_ok());
            let mut cpu = filmcraft_render::Image { w: w as usize, h: h as usize, px: px.clone() };
            op.apply(&mut cpu);
            let (_, _, gpu) = c.effect_image(&frame, &LayerFx { size: (w, h), decimation: 1, ops: vec![op] }).expect("keying image");
            let (worst, flips) = compare(&cpu.px, &gpu);
            assert!(worst < EXACT && flips == 0.0, "{id}, t={s}: {worst}, {flips}");
            assert_ne!(last.as_ref(), Some(&gpu), "{id}: animated picture at {s}");
            last = Some(gpu);
        }
    }
}

#[test]
fn spatial_ultra_key_stays_on_cpu() {
    for id in ["choke", "soften"] {
        let e = effect("ultra_key", &[(id, fl(10.0))]);
        assert!(FxOp::eval(&e, &cx(Tick::ZERO, 1.0), 67, 41).is_none(), "{id}: spatial cleanup requires CPU fallback");
    }
}

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
        effect("color_key", &[("color", ParamValue::Color([f32::NAN, 0.0, 1.0, 1.0]))]),
        effect("ultra_key", &[("spill", fl(f64::INFINITY))]),
        effect("luma_key", &[("threshold", fl(f64::NAN))]),
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
                },
                PlanLayer {
                    frame: ramp.clone(),
                    matrix: Affine::motion(Vec2::new(170.0, 95.0), Vec2::new(1.1, 1.1), 17.0, Vec2::new(80.0, 60.0)),
                    opacity: 0.8,
                    blend: mode,
                    fx: Some(Arc::new(LayerFx { size: (160, 120), decimation: 1, ops: ops(&chain_a, 160, 120, 1.0) })),
                },
                PlanLayer::new(ramp.clone(), Affine::translate(10.0, 20.0), 0.5, Blend::Normal),
                PlanLayer {
                    frame: yuv_frame(320, 180),
                    matrix: Affine::motion(Vec2::new(250.0, 70.0), Vec2::new(0.45, 0.45), -9.0, Vec2::new(160.0, 90.0)),
                    opacity: 0.7,
                    blend: mode,
                    fx: Some(Arc::new(LayerFx { size: (320, 180), decimation: 1, ops: ops(&chain_b, 320, 180, 1.0) })),
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

#[test]
fn advanced_lumetri_and_fused_masked_spatial_chains_match_cpu() {
    use filmcraft_project::MaskMode;
    use filmcraft_render::mask::FlatMask;
    let Some((dev, q)) = device() else {
        eprintln!("no GPU adapter; skipping");
        return;
    };
    let mut c = GpuCompositor::new(&dev, &q);
    let (w, h) = (67, 41);
    let px = picture(w, h);
    let frame = VideoFrame::rgba_f32(w, h, px.clone());
    let curve = ParamValue::Curve(vec![[0.0, 0.1], [0.3, 0.2], [0.8, 0.95], [1.0, 0.9]]);
    let mut params = vec![
        ("wheel_shadows", pt(0.4, 0.2)),
        ("wheel_midtones_l", fl(15.0)),
        ("wheel_highlights", pt(-0.2, 0.3)),
        ("look", ch(1)),
        ("look_intensity", fl(70.0)),
    ];
    for id in ["curve_luma", "curve_red", "curve_green", "curve_blue", "hue_vs_sat", "hue_vs_hue", "hue_vs_luma", "luma_vs_sat", "sat_vs_sat"] {
        params.push((id, curve.clone()));
    }
    let mut chains = Vec::new();
    for look in 0..=8 {
        let mut params = params.clone();
        params.push(("look", ch(look)));
        chains.push(vec![FxOp::eval(&effect("lumetri", &params), &cx(Tick::ZERO, 1.0), w as usize, h as usize).unwrap()]);
    }
    for output in 0..=3 {
        chains.push(vec![
            FxOp::eval(
                &effect(
                    "lumetri",
                    &[
                        ("hsl_on", ParamValue::Bool(true)),
                        ("hsl_show_mask", ch(output)),
                        ("hsl_denoise", fl(60.0)),
                        ("hsl_blur", fl(45.0)),
                        ("hsl_temp", fl(50.0)),
                        ("hsl_tint", fl(-30.0)),
                        ("hsl_hue_shift", fl(45.0)),
                        ("sharpen", fl(20.0)),
                    ],
                ),
                &cx(Tick::ZERO, 1.0),
                w as usize,
                h as usize,
            )
            .unwrap(),
        ]);
    }
    chains.push(vec![
        FxOp::eval(
            &effect(
                "lumetri",
                &[("input_lut", ParamValue::Text("builtin:look-teal-orange".into())), ("look_lut", ParamValue::Text("builtin:look-teal-orange".into()))],
            ),
            &cx(Tick::ZERO, 1.0),
            w as usize,
            h as usize,
        )
        .unwrap(),
    ]);
    // Non-unit domains and a 1D shaper before a non-affine cube exercise the actual LUT layout.
    let mut lut = filmcraft_color::Lut::from_cube(filmcraft_color::Lut3d::from_fn(5, |v| [v[0] * v[1], v[1] * v[1], v[2].sqrt()]));
    lut.shaper = Some(filmcraft_color::Lut1d::identity(17));
    if let Some(s) = &mut lut.shaper {
        s.domain_min = [-0.1; 3];
        s.domain_max = [1.1; 3];
    }
    chains.push(vec![FxOp::Lut { lut: Arc::new(lut) }]);
    for mode in [MaskMode::Add, MaskMode::Subtract, MaskMode::Intersect, MaskMode::Lighten, MaskMode::Darken, MaskMode::Difference] {
        let masks = vec![
            FlatMask { pts: vec![[5.0, 4.0], [58.0, 7.0], [45.0, 35.0], [10.0, 30.0]], feather: 5.0, expansion: -1.0, opacity: 0.7, inverted: false, mode },
            FlatMask {
                pts: vec![[20.0, 10.0], [48.0, 10.0], [35.0, 32.0]],
                feather: 1.0,
                expansion: 2.0,
                opacity: 0.5,
                inverted: true,
                mode: MaskMode::Difference,
            },
        ];
        let sharp = FxOp::eval(&effect("unsharp_mask", &[("radius", fl(4.0)), ("amount", fl(40.0))]), &cx(Tick::ZERO, 1.0), w as usize, h as usize).unwrap();
        let masked = FxOp::Masked { op: Box::new(sharp), masks: masks.clone() };
        chains.push(vec![
            FxOp::BrightnessContrast { br: 0.1, co: 1.2 },
            masked,
            FxOp::OpacityMask { masks },
            FxOp::Tint { black: [0.1; 3], white: [0.9; 3], amount: 0.3 },
        ]);
    }
    // More than sixteen point operations exercise bounded batch splitting.
    chains.push(vec![FxOp::BrightnessContrast { br: 0.01, co: 0.98 }; 37]);
    for (index, ops) in chains.into_iter().enumerate() {
        assert!(ops.iter().all(FxOp::gpu_ok));
        let mut cpu = filmcraft_render::Image { w: w as usize, h: h as usize, px: px.clone() };
        for op in &ops {
            op.apply(&mut cpu);
        }
        let (_, _, gpu) = c.effect_image(&frame, &LayerFx { size: (w, h), decimation: 1, ops }).expect("effect result");
        let (worst, flips) = compare(&cpu.px, &gpu);
        assert!(worst < 2e-4 && flips == 0.0, "chain {index}: {worst}, flips {flips}");
    }
}

#[test]
#[ignore = "GPU fusion throughput benchmark; optimized crates, real GPU, --nocapture"]
fn bench_fused_color_chain() {
    let (dev, q) = device().expect("GPU adapter");
    let mut c = GpuCompositor::new(&dev, &q);
    let ids = [
        ("brightness_contrast", vec![("brightness", fl(10.0))]),
        ("tint", vec![("amount", fl(20.0))]),
        ("lumetri", vec![("exposure", fl(0.2))]),
        ("color_balance", vec![("mid_r", fl(10.0))]),
        ("luma_key", vec![("threshold", fl(60.0)), ("cutoff", fl(10.0))]),
    ];
    for (w, h) in [(1920, 1080), (3840, 2160)] {
        let frame = yuv_frame(w, h);
        let ops = ids.iter().map(|(id, p)| FxOp::eval(&effect(id, p), &cx(Tick::ZERO, 1.0), w as usize, h as usize).unwrap()).collect();
        let mut layer = filmcraft_render::plan::PlanLayer::new(frame, Affine::IDENTITY, 1.0, Blend::Normal);
        layer.fx = Some(Arc::new(LayerFx { size: (w, h), decimation: 1, ops }));
        let plan = FramePlan::Layers { width: w as usize, height: h as usize, layers: vec![layer] };
        let prep = prepare(&plan);
        let mut times = [Vec::new(), Vec::new()];
        for round in 0..5 {
            for index in [round % 2, 1 - round % 2] {
                c.fx.as_mut().unwrap().set_fusion(index == 1);
                for _ in 0..2 {
                    c.composite_prepared(&plan, Some(&prep));
                    dev.poll(wgpu::PollType::wait_indefinitely()).unwrap();
                }
                let start = std::time::Instant::now();
                for _ in 0..6 {
                    std::hint::black_box(c.composite_prepared(&plan, Some(&prep)));
                    dev.poll(wgpu::PollType::wait_indefinitely()).unwrap();
                }
                times[index].push(start.elapsed().as_secs_f64() * 1000.0 / 6.0);
            }
        }
        for t in &mut times {
            t.sort_by(f64::total_cmp);
        }
        eprintln!("fusion {w}x{h}: separate {:.3}ms -> fused {:.3}ms ({:.2}x)", times[0][2], times[1][2], times[0][2] / times[1][2]);
    }
}

#[test]
fn tiled_blur_and_mask_cache_preserve_edges_and_invalidation() {
    use filmcraft_project::MaskMode;
    use filmcraft_render::mask::FlatMask;
    let Some((dev, q)) = device() else { return };
    let mut c = GpuCompositor::new(&dev, &q);
    for (w, h) in [(259, 73), (65, 257)] {
        let px = picture(w, h);
        let frame = VideoFrame::rgba_f32(w, h, px.clone());
        for repeat in [false, true] {
            for r in [1, 8, 32, 33, 64, 65, 127] {
                let op = FxOp::Gaussian { rx: vec![r; 3], ry: vec![r; 3], repeat };
                let mut cpu = filmcraft_render::Image { w: w as usize, h: h as usize, px: px.clone() };
                op.apply(&mut cpu);
                let (_, _, gpu) = c.effect_image(&frame, &LayerFx { size: (w, h), decimation: 1, ops: vec![op] }).unwrap();
                let (error, flips) = compare(&cpu.px, &gpu);
                assert!(error < 2e-4 && flips == 0.0, "blur {w}x{h} r{r} repeat{repeat}: {error}");
            }
        }
        for x in [0.0, 11.0, 0.0] {
            let mask = FlatMask {
                pts: vec![[x, 0.0], [w as f32 * 0.8, 5.0], [w as f32 * 0.5, h as f32], [x, h as f32 * 0.7]],
                feather: 7.0,
                expansion: -2.0,
                opacity: 0.8,
                inverted: false,
                mode: MaskMode::Add,
            };
            let op = FxOp::Masked { op: Box::new(FxOp::Tint { black: [0.1; 3], white: [0.8; 3], amount: 0.5 }), masks: vec![mask] };
            let mut cpu = filmcraft_render::Image { w: w as usize, h: h as usize, px: px.clone() };
            op.apply(&mut cpu);
            for _ in 0..2 {
                let (_, _, gpu) = c.effect_image(&frame, &LayerFx { size: (w, h), decimation: 1, ops: vec![op.clone()] }).unwrap();
                let (error, flips) = compare(&cpu.px, &gpu);
                assert!(error < 2e-4 && flips == 0.0, "mask x{x}: {error}");
            }
        }
    }
}

#[test]
#[ignore = "Metal optimisation throughput; actual GPU and --nocapture"]
fn bench_metal_optimisations() {
    use filmcraft_project::MaskMode;
    use filmcraft_render::mask::FlatMask;
    let (dev, q) = device().expect("GPU adapter");
    let mut c = GpuCompositor::new(&dev, &q);
    for (w, h) in [(1920, 1080), (3840, 2160)] {
        let color = vec![
            FxOp::BrightnessContrast { br: 0.1, co: 1.1 },
            FxOp::Tint { black: [0.05; 3], white: [0.9; 3], amount: 0.2 },
            FxOp::Gamma { g: 0.9 },
            FxOp::ColorBalance { sh: [1.1, 1.0, 0.9], md: [1.0; 3], hi: [0.9, 1.0, 1.1], preserve: true },
            FxOp::LumaKey { threshold: 0.6, cutoff: 0.1 },
        ];
        let mask = FlatMask {
            pts: (0..64)
                .map(|i| {
                    let a = i as f32 * std::f32::consts::TAU / 64.0;
                    [w as f32 * (0.5 + 0.4 * a.cos()), h as f32 * (0.5 + 0.4 * a.sin())]
                })
                .collect(),
            feather: 20.0,
            expansion: 2.0,
            opacity: 1.0,
            inverted: false,
            mode: MaskMode::Add,
        };
        let cases = [
            ("specialised color", color),
            ("cached 64-edge mask", vec![FxOp::Masked { op: Box::new(FxOp::BrightnessContrast { br: 0.1, co: 1.2 }), masks: vec![mask] }]),
            ("tiled six-pass blur", vec![FxOp::Gaussian { rx: vec![32; 3], ry: vec![32; 3], repeat: true }]),
        ];
        for (name, ops) in cases {
            let mut layer = PlanLayer::new(yuv_frame(w, h), Affine::IDENTITY, 1.0, Blend::Normal);
            layer.fx = Some(Arc::new(LayerFx { size: (w, h), decimation: 1, ops }));
            let plan = FramePlan::Layers { width: w as usize, height: h as usize, layers: vec![layer] };
            let prep = prepare(&plan);
            let mut times = [Vec::new(), Vec::new()];
            for round in 0..5 {
                for index in [round % 2, 1 - round % 2] {
                    c.fx.as_mut().unwrap().set_optimise(index == 1);
                    for _ in 0..2 {
                        c.composite_prepared(&plan, Some(&prep));
                        dev.poll(wgpu::PollType::wait_indefinitely()).unwrap();
                    }
                    let start = std::time::Instant::now();
                    for _ in 0..6 {
                        c.composite_prepared(&plan, Some(&prep));
                        dev.poll(wgpu::PollType::wait_indefinitely()).unwrap();
                    }
                    times[index].push(start.elapsed().as_secs_f64() * 1000.0 / 6.0);
                }
            }
            for t in &mut times {
                t.sort_by(f64::total_cmp);
            }
            eprintln!("Metal {name} {w}x{h}: {:.3}ms -> {:.3}ms ({:.2}x)", times[0][2], times[1][2], times[0][2] / times[1][2]);
        }
    }
}

#[test]
#[cfg(not(target_arch = "wasm32"))]
fn specialised_variants_compile_without_blocking_and_reuse_keyframed_parameters() {
    let Some((dev, q)) = device() else { return };
    let mut c = GpuCompositor::new(&dev, &q);
    let (w, h) = (67, 41);
    let px = picture(w, h);
    let frame = VideoFrame::rgba_f32(w, h, px.clone());
    for br in [0.1, -0.05, 0.3] {
        let ops = vec![FxOp::BrightnessContrast { br, co: 1.1 }, FxOp::Tint { black: [0.05; 3], white: [0.9; 3], amount: 0.2 }, FxOp::Gamma { g: 0.9 }];
        let mut cpu = filmcraft_render::Image { w: w as usize, h: h as usize, px: px.clone() };
        for op in &ops {
            op.apply(&mut cpu);
        }
        let start = std::time::Instant::now();
        loop {
            let (_, _, gpu) = c.effect_image(&frame, &LayerFx { size: (w, h), decimation: 1, ops: ops.clone() }).unwrap();
            let (error, flips) = compare(&cpu.px, &gpu);
            assert!(error < 2e-4 && flips == 0.0, "brightness {br}: {error}");
            if c.fx.as_ref().unwrap().compiled_variants() > 0 {
                break;
            }
            assert!(start.elapsed() < std::time::Duration::from_secs(10), "background shader compilation completed");
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert_eq!(c.fx.as_ref().unwrap().compiled_variants(), 1, "changing parameters reuses the same operation variant");
    }
}

#[test]
#[ignore = "Select tiled blur crossover on actual GPU"]
fn bench_blur_kernel_thresholds() {
    let (dev, q) = device().expect("GPU adapter");
    let mut c = GpuCompositor::new(&dev, &q);
    let (w, h) = (1920, 1080);
    let frame = yuv_frame(w, h);
    for r in [1, 4, 8, 16, 32, 33, 64] {
        let mut layer = PlanLayer::new(frame.clone(), Affine::IDENTITY, 1.0, Blend::Normal);
        layer.fx = Some(Arc::new(LayerFx { size: (w, h), decimation: 1, ops: vec![FxOp::Gaussian { rx: vec![r], ry: vec![r], repeat: true }] }));
        let plan = FramePlan::Layers { width: w as usize, height: h as usize, layers: vec![layer] };
        let prep = prepare(&plan);
        let mut times = [Vec::new(), Vec::new()];
        for round in 0..5 {
            for index in [round % 2, 1 - round % 2] {
                c.fx.as_mut().unwrap().set_optimise(index == 1);
                for _ in 0..2 {
                    c.composite_prepared(&plan, Some(&prep));
                    dev.poll(wgpu::PollType::wait_indefinitely()).unwrap();
                }
                let start = std::time::Instant::now();
                for _ in 0..6 {
                    c.composite_prepared(&plan, Some(&prep));
                    dev.poll(wgpu::PollType::wait_indefinitely()).unwrap();
                }
                times[index].push(start.elapsed().as_secs_f64() * 1000.0 / 6.0);
            }
        }
        for t in &mut times {
            t.sort_by(f64::total_cmp);
        }
        eprintln!("blur radius {r}: {:.3}ms -> {:.3}ms ({:.2}x)", times[0][2], times[1][2], times[0][2] / times[1][2]);
    }
}
