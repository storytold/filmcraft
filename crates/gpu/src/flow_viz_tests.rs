//! The GPU optical-flow overlay against the CPU reference (`encode_flow_viz_cpu`).

use super::blend_tests::device;
use super::flow_viz::GpuFlowViz;
use filmcraft_render::Image;
use filmcraft_render::flow::{FlowField, FlowVizMode, FlowVizRenderer, encode_flow_viz_cpu};

const MODES: [FlowVizMode; 4] = [FlowVizMode::HsvWheel, FlowVizMode::MotionVectors, FlowVizMode::KineticGlow, FlowVizMode::DlssEnhance];

/// A frame with smooth colour, values above 1 and below 0 (as effects can leave them).
fn frame(w: usize, h: usize) -> Image {
    let mut img = Image::new(w, h);
    for y in 0..h {
        for x in 0..w {
            let (u, v) = (x as f32 / w as f32, y as f32 / h as f32);
            let i = (y * w + x) * 4;
            img.px[i..i + 4].copy_from_slice(&[u * 1.4 - 0.1, v, (u * 9.0 + v * 5.0).sin() * 0.5 + 0.5, 1.0]);
        }
    }
    img
}

/// A grid of `grid` px cells whose vectors sweep every direction and speed (still, sub-pixel,
/// past the saturation), with varied cost.
fn field(frame_w: usize, frame_h: usize, grid: usize) -> FlowField {
    let (w, h) = (frame_w.div_ceil(grid), frame_h.div_ceil(grid));
    let mut uv = Vec::with_capacity(w * h);
    let mut cost = Vec::with_capacity(w * h);
    for y in 0..h {
        for x in 0..w {
            let a = (x as f32 * 0.37 + y as f32 * 0.11) * std::f32::consts::TAU / 5.0;
            let speed = ((x * 7 + y * 3) % 23) as f32 * 1.6 - 2.0;
            uv.push([a.cos() * speed, a.sin() * speed]);
            cost.push(((x * 31 + y * 17) % 256) as u8);
        }
    }
    FlowField { w, h, frame_w, frame_h, uv, cost, hardware: false }
}

fn max_diff(a: &[f32], b: &[f32]) -> (f32, usize) {
    assert_eq!(a.len(), b.len());
    a.iter().zip(b).enumerate().fold((0.0, 0), |(m, at), (i, (p, q))| if (p - q).abs() > m { ((p - q).abs(), i) } else { (m, at) })
}

/// A quarter of a 12-bit code: well below anything the 10/12-bit ProRes encoder keeps.
const TOLERANCE: f32 = 1.0 / 4096.0 / 4.0;

#[test]
fn gpu_overlay_matches_the_cpu_in_every_mode() {
    let Some((dev, q)) = device() else {
        eprintln!("no GPU adapter; skipping");
        return;
    };
    let mut gpu = GpuFlowViz::with_device(&dev, &q, "test".into()).unwrap();
    // sizes that are not multiples of the 16×16 workgroup, one after another on the same
    // renderer (buffers are reallocated for the new size)
    for (w, h, grid) in [(101usize, 37usize, 4usize), (64, 48, 4), (96, 60, 8)] {
        let (img, f) = (frame(w, h), field(w, h, grid));
        for mode in MODES {
            for max_motion in [16.0f32, 3.0] {
                let want = encode_flow_viz_cpu(&img, &f, mode, max_motion);
                let got = gpu.encode(&img, &f, mode, max_motion).unwrap();
                let (d, at) = max_diff(&got, &want);
                assert!(d <= TOLERANCE, "{mode:?} {w}x{h} max {max_motion}: GPU {} vs CPU {} at value {at} (diff {d})", got[at], want[at]);
            }
        }
    }
}

#[test]
fn gpu_motion_vectors_keep_the_neutral_code_exact() {
    let Some((dev, q)) = device() else {
        eprintln!("no GPU adapter; skipping");
        return;
    };
    let mut gpu = GpuFlowViz::with_device(&dev, &q, "test".into()).unwrap();
    let (w, h) = (40, 20);
    let still = FlowField { w: 10, h: 5, frame_w: w, frame_h: h, uv: vec![[0.0; 2]; 50], cost: vec![0; 50], hardware: true };
    let got = gpu.encode(&frame(w, h), &still, FlowVizMode::MotionVectors, 16.0).unwrap();
    assert!(got.iter().all(|&v| v == 0.5), "no motion must encode as exactly 0.5");
}

#[test]
fn gpu_overlay_handles_odd_fields_like_the_cpu() {
    let Some((dev, q)) = device() else {
        eprintln!("no GPU adapter; skipping");
        return;
    };
    let mut gpu = GpuFlowViz::with_device(&dev, &q, "test".into()).unwrap();
    let img = frame(50, 30);
    let empty = FlowField { w: 0, h: 0, frame_w: 0, frame_h: 0, uv: Vec::new(), cost: Vec::new(), hardware: false };
    // fewer vectors than cells: the missing ones are still
    let short = FlowField { uv: vec![[9.0, -4.0]; 20], cost: vec![10; 20], ..field(50, 30, 4) };
    // a field computed for another frame size is sampled in its own pixels
    let other = field(100, 60, 4);
    for f in [&empty, &short, &other] {
        for mode in MODES {
            let want = encode_flow_viz_cpu(&img, f, mode, 16.0);
            let got = gpu.encode(&img, f, mode, 16.0).unwrap();
            let (d, at) = max_diff(&got, &want);
            assert!(d <= TOLERANCE, "{mode:?} {}x{} grid: diff {d} at {at}", f.w, f.h);
        }
    }
}

#[test]
fn a_frame_beyond_the_gpu_limits_is_an_error_not_a_crash() {
    let Some((dev, q)) = device() else {
        eprintln!("no GPU adapter; skipping");
        return;
    };
    let mut gpu = GpuFlowViz::with_device(&dev, &q, "test".into()).unwrap();
    let wide = dev.limits().max_texture_dimension_2d as usize + 1;
    let img = Image::new(wide, 1);
    let f = field(wide, 1, 4);
    assert!(gpu.encode(&img, &f, FlowVizMode::KineticGlow, 16.0).is_err());
    // and the renderer still works afterwards
    let (img, f) = (frame(32, 32), field(32, 32, 4));
    assert!(gpu.encode(&img, &f, FlowVizMode::HsvWheel, 16.0).is_ok());
}

#[test]
fn linear_detail_keeps_hdr_before_encoding() {
    let Some((dev, q)) = device() else { return };
    let mut gpu = GpuFlowViz::with_device(&dev, &q, "test".into()).unwrap();
    let img = Image::filled(32, 16, [4.0, 2.0, 0.5, 0.5]);
    let rgb = gpu.linear_detail(&img, &field(32, 16, 4), 16.0).unwrap();
    for p in rgb.as_chunks::<3>().0 {
        for (value, expected) in p.iter().zip([4.0, 2.0, 0.5]) {
            assert!((value - expected).abs() < TOLERANCE);
        }
    }
}
