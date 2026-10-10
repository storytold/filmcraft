//! GPU-vs-CPU parity of the blend modes (`filmcraft_render::blend::composite` is the reference).
//!
//! Tolerances: the composited 8-bit output uses the compositor's parity criterion (99th percentile
//! of the per-pixel max channel difference ≤ 6, mean < 1.5; antialiased edges and half-float
//! textures differ slightly). Exact inputs drawn 1:1 (no resampling) must agree to 2e-3 in linear
//! premultiplied values (half-float accumulator), except Hard Mix, a step function, which may
//! flip a pixel whose blend lands on its 0.5 threshold.

use super::*;
use filmcraft_geom::{Affine, Vec2};
use filmcraft_render::plan::execute_cpu;

pub(crate) fn device() -> Option<(wgpu::Device, wgpu::Queue)> {
    let instance = wgpu::Instance::default();
    let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions::default())).ok()?;
    pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor::default())).ok()
}

pub(crate) fn yuv_frame(w: u32, h: u32) -> Arc<VideoFrame> {
    let (cw, ch) = (w.div_ceil(2), h.div_ceil(2));
    let y: Vec<u8> = (0..w * h).map(|i| (16 + ((i % w) * 219 / w)) as u8).collect();
    let u: Vec<u8> = (0..cw * ch).map(|i| (64 + (i / cw) * 128 / ch) as u8).collect();
    let v: Vec<u8> = (0..cw * ch).map(|i| (200 - (i % cw) * 100 / cw) as u8).collect();
    Arc::new(VideoFrame {
        width: w,
        height: h,
        data: PixelData::Yuv8 { planes: [Arc::new(y), Arc::new(u), Arc::new(v)], chroma: Chroma::C420, alpha: None },
        color: filmcraft_color::ColorInfo::REC709,
        par: (1, 1),
        pts: Default::default(),
    })
}

/// Decode an IEEE half.
pub(crate) fn f16_to_f32(h: u16) -> f32 {
    let s = if h & 0x8000 != 0 { -1.0 } else { 1.0 };
    let e = ((h >> 10) & 0x1f) as i32;
    let m = (h & 0x3ff) as f32;
    match e {
        0 => s * m * 2f32.powi(-24),
        31 => s * f32::INFINITY,
        _ => s * (1.0 + m / 1024.0) * 2f32.powi(e - 15),
    }
}

/// Read the linear premultiplied accumulator back as f32 RGBA (what the CPU plan executor returns).
pub(crate) fn read_accum(c: &GpuCompositor) -> Vec<f32> {
    let (tex, _, (w, h)) = c.accum.as_ref().expect("accumulator");
    let row = (w * 8).div_ceil(256) * 256;
    let buf = c.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("accum-readback"),
        size: (row * h) as u64,
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    let mut enc = c.device.create_command_encoder(&Default::default());
    enc.copy_texture_to_buffer(
        wgpu::TexelCopyTextureInfo { texture: tex, mip_level: 0, origin: wgpu::Origin3d::ZERO, aspect: wgpu::TextureAspect::All },
        wgpu::TexelCopyBufferInfo { buffer: &buf, layout: wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(row), rows_per_image: Some(*h) } },
        wgpu::Extent3d { width: *w, height: *h, depth_or_array_layers: 1 },
    );
    c.queue.submit([enc.finish()]);
    let slice = buf.slice(..);
    slice.map_async(wgpu::MapMode::Read, |_| {});
    let _ = c.device.poll(wgpu::PollType::wait_indefinitely());
    let data = slice.get_mapped_range().expect("mapped");
    let mut out = Vec::with_capacity((w * h * 4) as usize);
    for y in 0..*h {
        let r = &data[(y * row) as usize..(y * row + w * 8) as usize];
        out.extend(r.as_chunks::<2>().0.iter().map(|b| f16_to_f32(u16::from_le_bytes(*b))));
    }
    out
}

/// Per-pixel max channel difference of two RGBA8 images over the pixels `keep` selects: (p99,
/// mean, max).
pub(crate) fn stats8(a: &[u8], b: &[u8], keep: &[bool]) -> (u32, f64, u32) {
    let mut d: Vec<u32> = a
        .chunks(4)
        .zip(b.chunks(4))
        .zip(keep)
        .filter(|(_, k)| **k)
        .map(|((a, b), _)| (0..3).map(|k| (a[k] as i32 - b[k] as i32).unsigned_abs()).max().unwrap_or(0))
        .collect();
    d.sort_unstable();
    (d[d.len() * 99 / 100], d.iter().sum::<u32>() as f64 / d.len() as f64, d[d.len() - 1])
}

/// Pixels at least 1.5 px away from every layer's quad outline (the CPU resampler fades a layer's
/// edge over a pixel beyond it, the GPU rasterises the quad: compared separately).
pub(crate) fn interior(plan: &FramePlan) -> Vec<bool> {
    let FramePlan::Layers { width, height, layers } = plan else { return Vec::new() };
    let inv: Vec<(Affine, f64, f64)> = layers.iter().filter_map(|l| Some((l.matrix.inverse()?, l.size().0 as f64, l.size().1 as f64))).collect();
    let mut keep = vec![true; width * height];
    for (i, k) in keep.iter_mut().enumerate() {
        let (x, y) = ((i % width) as f64 + 0.5, (i / width) as f64 + 0.5);
        *k = inv.iter().all(|(m, fw, fh)| {
            let inside = |dx: f64, dy: f64| {
                let p = m.apply(Vec2::new(x + dx, y + dy));
                p.x >= 0.0 && p.y >= 0.0 && p.x <= *fw && p.y <= *fh
            };
            let c = [inside(-1.5, -1.5), inside(1.5, -1.5), inside(-1.5, 1.5), inside(1.5, 1.5)];
            c.iter().all(|v| *v == c[0])
        });
    }
    keep
}

/// Linear premultiplied RGBA f32 test frame: a colour ramp with black and white bands; alpha 0 in
/// the left tenth, 1 in the top third, partial below.
pub(crate) fn ramp_layer(w: u32, h: u32, phase: u32) -> Arc<VideoFrame> {
    let mut px = Vec::with_capacity((w * h * 4) as usize);
    for y in 0..h {
        for x in 0..w {
            let fx = x as f32 / (w - 1) as f32;
            let fy = y as f32 / (h - 1) as f32;
            let c = match (x + phase) / 8 % 6 {
                0 => [0.0, 0.0, 0.0],
                1 => [1.0, 1.0, 1.0],
                _ => [fx, (fy * 1.3 + phase as f32 * 0.1).fract(), ((fx + fy) * 0.7).fract()],
            };
            let a = if x < w / 10 {
                0.0
            } else if y < h / 3 {
                1.0
            } else {
                0.25 + 0.75 * fy
            };
            px.extend_from_slice(&[c[0] * a, c[1] * a, c[2] * a, a]);
        }
    }
    Arc::new(VideoFrame::rgba_f32(w, h, px))
}

/// Every blend mode over a backdrop with opaque, partially transparent and empty regions, on two
/// overlapping transformed layers (rotated, scaled, partly off the output) with partial opacity
/// and partial alpha: the GPU matches the CPU plan executor within the compositor's tolerance,
/// away from the layers' outlines (which are reported: the CPU fades them over an extra pixel).
#[test]
fn gpu_blend_modes_match_cpu() {
    let Some((dev, q)) = device() else {
        eprintln!("no GPU adapter; skipping");
        return;
    };
    let mut c = GpuCompositor::new(&dev, &q);
    let (w, h) = (320usize, 180usize);
    let pixels = filmcraft_media::generators::render(
        &filmcraft_media::Generator::Demo(filmcraft_media::DemoScene::Dunes),
        320,
        180,
        1.0,
        24,
        filmcraft_time::FrameRate::FPS_24,
    );
    let demo = Arc::new(VideoFrame::rgba8(320, 180, pixels));
    let base = ramp_layer(200, 180, 3);
    let top = ramp_layer(160, 120, 0);
    for mode in Blend::ALL {
        let plan = FramePlan::Layers {
            width: w,
            height: h,
            layers: vec![
                // opaque YUV on the left, a partially transparent ramp on the right; the top-right
                // corner stays empty (destination alpha 0)
                PlanLayer { frame: yuv_frame(640, 360), matrix: Affine::scale(0.35, 0.5), opacity: 1.0, blend: Blend::Normal, fx: None },
                PlanLayer { frame: base.clone(), matrix: Affine::translate(200.0, 40.0), opacity: 0.9, blend: Blend::Normal, fx: None },
                PlanLayer {
                    frame: top.clone(),
                    matrix: Affine::motion(Vec2::new(170.0, 95.0), Vec2::new(1.1, 1.1), 17.0, Vec2::new(80.0, 60.0)),
                    opacity: 0.8,
                    blend: mode,
                    fx: None,
                },
                PlanLayer {
                    frame: demo.clone(),
                    matrix: Affine::motion(Vec2::new(250.0, 70.0), Vec2::new(0.45, 0.45), -9.0, Vec2::new(160.0, 90.0)),
                    opacity: 0.6,
                    blend: mode,
                    fx: None,
                },
            ],
        };
        let cpu_img = execute_cpu(&plan).unwrap();
        let cpu = cpu_img.over_black_rgba8();
        c.composite(&plan).unwrap();
        let (_, _, gpu) = c.read_output().expect("readback");
        let keep = interior(&plan);
        let (p99, mean, max) = stats8(&cpu, &gpu, &keep);
        let all = vec![true; keep.len()];
        let (ap99, amean, _) = stats8(&cpu, &gpu, &all);
        // linear premultiplied accumulator, alpha included
        let acc = read_accum(&c);
        let mut fd: Vec<f32> =
            cpu_img.px.chunks(4).zip(acc.chunks(4)).zip(&keep).filter(|(_, k)| **k).flat_map(|((a, b), _)| (0..4).map(|k| (a[k] - b[k]).abs())).collect();
        fd.sort_unstable_by(f32::total_cmp);
        let (fp99, fmax) = (fd[fd.len() * 99 / 100], fd[fd.len() - 1]);
        eprintln!("{mode:?}: interior 8-bit p99 {p99} mean {mean:.3} max {max}; linear p99 {fp99:.2e} max {fmax:.2e}; with edges p99 {ap99} mean {amean:.3}");
        assert!(p99 <= 6 && mean < 1.5, "{mode:?}: p99 {p99}, mean {mean}");
    }
    assert!(c.backdrop.is_some(), "blend modes use the backdrop copy");
}

/// Formula edge cases on exact (half-float representable) values, drawn 1:1 so no resampling is
/// involved: black / white / mid inputs, Divide by zero, Color Dodge and Burn at 0 and 1, zero and
/// partial alpha on either side.
#[test]
fn gpu_blend_edge_cases_match_cpu() {
    let Some((dev, q)) = device() else {
        eprintln!("no GPU adapter; skipping");
        return;
    };
    let vals = [0.0f32, 1.0, 0.5, 0.25, 0.75, 0.125, 0.875, 0.0625];
    let alphas = [1.0f32, 0.5, 0.0, 0.25];
    let n = vals.len() * alphas.len();
    // backdrop varies along x, source along y: every (b, s, alpha) combination; channels are
    // shifted along the other axis so greys, primaries and mixed colours all occur
    let make = |along_x: bool| {
        let mut px = Vec::with_capacity(n * n * 4);
        for y in 0..n {
            for x in 0..n {
                let (i, o) = if along_x { (x, y) } else { (y, x) };
                let (v, a) = (i % vals.len(), alphas[i / vals.len()]);
                let j = o % 3;
                let c = [vals[v], vals[(v + j) % vals.len()], vals[(v + 2 * j) % vals.len()]];
                px.extend_from_slice(&[c[0] * a, c[1] * a, c[2] * a, a]);
            }
        }
        Arc::new(VideoFrame::rgba_f32(n as u32, n as u32, px))
    };
    let (back, src) = (make(true), make(false));
    let mut c = GpuCompositor::new(&dev, &q);
    for mode in Blend::ALL {
        for opacity in [1.0f32, 0.5] {
            let plan = FramePlan::Layers {
                width: n,
                height: n,
                layers: vec![
                    PlanLayer { frame: back.clone(), matrix: Affine::IDENTITY, opacity: 1.0, blend: Blend::Normal, fx: None },
                    PlanLayer { frame: src.clone(), matrix: Affine::IDENTITY, opacity, blend: mode, fx: None },
                ],
            };
            let cpu = execute_cpu(&plan).unwrap();
            c.composite(&plan).unwrap();
            let gpu = read_accum(&c);
            let mut worst = (0f32, 0usize);
            let mut steps = 0;
            for (i, (a, b)) in cpu.px.iter().zip(&gpu).enumerate() {
                assert!(b.is_finite(), "{mode:?}: non-finite GPU value at {i}");
                let d = (a - b).abs();
                if d > 0.05 {
                    steps += 1;
                }
                if d > worst.0 {
                    worst = (d, i);
                }
            }
            eprintln!("{mode:?} @ {opacity}: max |cpu − gpu| = {:.2e} (value {} of {}), {steps} step flips", worst.0, worst.1, gpu.len());
            assert!(worst.0 < 2e-3 || mode == Blend::HardMix && steps <= 2, "{mode:?} @ {opacity}: {worst:?}, {steps}");
        }
    }
}

/// Dissolve on the GPU reproduces the CPU's pixel pattern exactly (opaque layer at partial opacity).
#[test]
fn gpu_dissolve_pattern_is_exact() {
    let Some((dev, q)) = device() else {
        eprintln!("no GPU adapter; skipping");
        return;
    };
    let (w, h) = (257usize, 131usize);
    let white = Arc::new(VideoFrame::rgba_f32(1, 1, vec![1.0; 4]));
    let red = Arc::new(VideoFrame::rgba_f32(1, 1, vec![1.0, 0.0, 0.0, 1.0]));
    let mut c = GpuCompositor::new(&dev, &q);
    for op in [0.1f32, 0.5, 0.9] {
        let plan = FramePlan::Layers {
            width: w,
            height: h,
            layers: vec![
                PlanLayer { frame: red.clone(), matrix: Affine::scale(w as f64, h as f64), opacity: 1.0, blend: Blend::Normal, fx: None },
                PlanLayer { frame: white.clone(), matrix: Affine::scale(w as f64, h as f64), opacity: op, blend: Blend::Dissolve, fx: None },
            ],
        };
        let cpu = execute_cpu(&plan).unwrap().over_black_rgba8();
        c.composite(&plan).unwrap();
        let (_, _, gpu) = c.read_output().expect("readback");
        let differ = cpu.chunks(4).zip(gpu.chunks(4)).filter(|(a, b)| a[..3] != b[..3]).count();
        let kept = gpu.chunks(4).filter(|p| p[1] > 128).count() as f32 / (w * h) as f32;
        eprintln!("Dissolve @ {op}: {differ} differing pixels, {kept:.3} kept");
        assert_eq!(differ, 0, "opacity {op}");
        assert!((kept - op).abs() < 0.02);
    }
    // fixed-function only: no backdrop copy
    assert!(c.backdrop.is_none());
}

/// Normal-only plans keep the single fixed-function pass (no backdrop texture), and blend-mode
/// layers entirely off the output draw nothing.
#[test]
fn normal_fast_path_and_blend_layers_off_output() {
    let Some((dev, q)) = device() else {
        eprintln!("no GPU adapter; skipping");
        return;
    };
    let mut c = GpuCompositor::new(&dev, &q);
    let grey = Arc::new(VideoFrame::rgba_f32(4, 4, vec![0.2; 64]));
    let layer = |m: Affine, blend: Blend| PlanLayer { frame: grey.clone(), matrix: m, opacity: 1.0, blend, fx: None };
    let normal = FramePlan::Layers { width: 32, height: 16, layers: vec![layer(Affine::scale(8.0, 4.0), Blend::Normal)] };
    c.composite(&normal).unwrap();
    assert!(c.backdrop.is_none());
    let (_, _, before) = c.read_output().expect("readback");
    let off = FramePlan::Layers {
        width: 32,
        height: 16,
        layers: vec![
            layer(Affine::scale(8.0, 4.0), Blend::Normal),
            layer(Affine::translate(100.0, 0.0), Blend::Multiply),
            layer(Affine::translate(-10.0, -10.0), Blend::Screen),
        ],
    };
    c.composite(&off).unwrap();
    let (_, _, after) = c.read_output().expect("readback");
    assert_eq!(before, after);
    // only blend-mode layers first: the accumulator is still cleared
    let only = FramePlan::Layers { width: 32, height: 16, layers: vec![layer(Affine::translate(100.0, 0.0), Blend::Multiply)] };
    c.composite(&only).unwrap();
    let (_, _, empty) = c.read_output().expect("readback");
    assert!(empty.chunks(4).all(|p| p[..3] == [0, 0, 0]));
}

#[test]
fn quad_bounds_clamps_and_handles_non_finite() {
    let l = |m: Affine| PlanLayer { frame: Arc::new(VideoFrame::rgba_f32(10, 10, vec![0.0; 400])), matrix: m, opacity: 1.0, blend: Blend::Screen, fx: None };
    assert_eq!(quad_bounds(&l(Affine::translate(5.0, 5.0)), 100, 100), Some((4, 4, 12, 12)));
    assert_eq!(quad_bounds(&l(Affine::translate(-5.0, 95.0)), 100, 100), Some((0, 94, 6, 6)));
    assert_eq!(quad_bounds(&l(Affine::translate(f64::NAN, 0.0)), 100, 100), Some((0, 0, 100, 100)));
    assert_eq!(quad_bounds(&l(Affine::scale(f64::INFINITY, 1.0)), 100, 100), Some((0, 0, 100, 100)));
    assert_eq!(quad_bounds(&l(Affine::translate(-50.0, 0.0)), 100, 100), None);
}
