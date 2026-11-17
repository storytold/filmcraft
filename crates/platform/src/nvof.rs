//! NVIDIA Optical Flow Accelerator (`NVOFA`) hardware backend: dense 2D motion estimation on the
//! GPU's dedicated optical-flow engine through `nvofapi64.dll` (Windows) or
//! `libnvidia-opticalflow.so.1` (64-bit Linux).
//!
//! Plugs into [`filmcraft_render::flow::register_optical_flow`]. When no NVIDIA GPU or driver is
//! present, or when a frame size is outside the hardware engine's bounds (for example, tiny test
//! pictures below `160x64`), [`factory`] declines and [`filmcraft_render::flow::FlowEstimator`]
//! transparently falls back to the pure-Rust CPU Lucas–Kanade pyramid.
//!
//! `unsafe` is confined to `session` (driver loading and calls); `ffi` is plain `repr(C)` data
//! without an unsafe allowance, and this module is safe code.

mod ffi;
#[allow(unsafe_code)]
mod session;

#[cfg(test)]
mod abi_tests;

use filmcraft_render::flow::{FlowField, FlowSession};

pub use self::session::Session;

/// Adapter implementing [`filmcraft_render::flow::FlowSession`] over an NVOFA hardware [`Session`].
pub struct NvofFlowSession {
    inner: Session,
    width: usize,
    height: usize,
}

impl FlowSession for NvofFlowSession {
    fn backend(&self) -> &'static str {
        "NVIDIA NVOFA"
    }

    fn estimate(&mut self, prev_luma: &[u8], curr_luma: &[u8], sequential: bool) -> Result<FlowField, String> {
        let (uv, cost) = self.inner.execute(prev_luma, curr_luma, sequential)?;
        let (gw, gh, _) = self.inner.grid_dimensions();
        Ok(FlowField { w: gw, h: gh, frame_w: self.width, frame_h: self.height, uv, cost, hardware: true })
    }
}

/// Factory registered with [`filmcraft_render::flow::register_optical_flow`].
pub fn factory(width: u32, height: u32) -> Result<Box<dyn FlowSession>, String> {
    let inner = Session::new(width, height)?;
    Ok(Box::new(NvofFlowSession { inner, width: width as usize, height: height as usize }))
}

/// Whether the NVIDIA Optical Flow driver and hardware are available on this machine.
pub fn available() -> bool {
    Session::probe().is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use filmcraft_render::Image;
    use filmcraft_render::flow::{FlowEstimator, FlowVizMode, register_optical_flow, render_flow_viz};

    fn textured_frame(w: usize, h: usize, shift_x: f32, shift_y: f32) -> Image {
        let mut img = Image::new(w, h);
        for y in 0..h {
            for x in 0..w {
                let (u, v) = (x as f32 + 0.5 - shift_x, y as f32 + 0.5 - shift_y);
                let l =
                    (0.4 + 0.25 * (u * 0.09).sin() * (v * 0.08).cos() + 0.15 * ((u + v) * 0.05).sin() + 0.10 * (u * 0.23 - v * 0.19).cos()).clamp(0.02, 0.98);
                let i = (y * w + x) * 4;
                img.px[i..i + 4].copy_from_slice(&[l, l * 0.9, l * 0.8, 1.0]);
            }
        }
        img
    }

    #[test]
    fn nvof_hardware_or_fallback_estimates_motion_cleanly() {
        register_optical_flow(factory);
        // 320x192 is above NVOFA's 160x64 minimum frame size
        let (w, h) = (320usize, 192usize);
        let a = textured_frame(w, h, 0.0, 0.0);
        let b = textured_frame(w, h, 8.0, -4.0);
        let mut est = FlowEstimator::new(w, h);
        let has_hw = available();
        if has_hw {
            assert!(est.is_hardware(), "expected NVIDIA NVOFA hardware session on 320x192 frame");
            assert_eq!(est.backend(), "NVIDIA NVOFA");
        }
        let field = est.estimate(&a, &b);
        assert_eq!(field.hardware, has_hw);
        assert_eq!((field.frame_w, field.frame_h), (w, h));

        // Sample interior motion around the center of the frame
        let ([dx, dy], _conf) = field.sample_px(160.0, 96.0);
        assert!(dx.abs() > 2.0, "expected horizontal motion on 320x192 frame, got dx={dx}, dy={dy} (hw={has_hw})");

        let viz = render_flow_viz(&b, &field, FlowVizMode::HsvWheel, 16.0);
        assert_eq!((viz.w, viz.h), (w, h));
        let center_px = viz.get(160, 96);
        assert!(center_px[0] + center_px[1] + center_px[2] > 0.02, "expected non-black HSV wheel output: {center_px:?}");
    }

    /// The overlay command's call order on real hardware (when present): frame 0 looks ahead
    /// (a → b), frame 1 asks for a → b again, frame 2 continues from b (only c is uploaded, with
    /// temporal hints). Every one must see the motion; frame 1 once saw none.
    #[test]
    fn nvof_sequence_keeps_seeing_motion_on_every_frame() {
        register_optical_flow(factory);
        let (w, h) = (320usize, 192usize);
        let (a, b, c) = (textured_frame(w, h, 0.0, 0.0), textured_frame(w, h, 6.0, 0.0), textured_frame(w, h, 12.0, 0.0));
        let mut est = FlowEstimator::new(w, h);
        eprintln!("optical flow backend: {} (hardware: {}; {:?})", est.backend(), est.is_hardware(), est.fallback_reason());
        for (i, (p, q)) in [(&a, &b), (&a, &b), (&b, &c)].into_iter().enumerate() {
            let field = est.estimate(p, q);
            let ([dx, dy], _) = field.sample_px(160.0, 96.0);
            eprintln!("call {i}: dx={dx:.2} dy={dy:.2} hardware={}", field.hardware);
            assert!(dx > 3.0 && dy.abs() < 2.0, "call {i}: expected about +6 px horizontal motion, got ({dx}, {dy})");
        }
    }
}
