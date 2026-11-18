//! NVIDIA NGX Direct3D 11 (`_nvngx.dll` + OTA `nvngx_dlss.dll` Preset K Transformer) hardware
//! neural reconstruction backend on Windows.
//!
//! Plugs into [`filmcraft_render::flow::register_neural_enhancer`]. When no NVIDIA RTX GPU, NGX
//! driver runtime, or signed DLSS model binary is present, or when a frame size is outside the
//! engine's bounds, [`factory`] declines and [`filmcraft_render::flow::NeuralEnhancer`]
//! transparently falls back to the pure-Rust motion-compensated temporal reconstruction path.
//!
//! `unsafe` is confined to `session` (driver loading, D3D11 textures, and NGX evaluation); `ffi`
//! is plain `repr(C)` data transcribed from NVIDIA's MIT-licensed Streamline SDK, and this module
//! is safe code.

mod ffi;
#[allow(unsafe_code)]
mod session;

#[cfg(test)]
mod abi_tests;

use filmcraft_render::Image;
use filmcraft_render::flow::{FlowField, NeuralSession};

pub use self::session::Session;

/// Adapter implementing [`filmcraft_render::flow::NeuralSession`] over an NGX DLSS [`Session`].
pub struct NgxNeuralSession {
    inner: Session,
}

impl NeuralSession for NgxNeuralSession {
    fn backend(&self) -> &'static str {
        "NVIDIA NGX DLSS (Preset K)"
    }

    fn enhance(&mut self, curr: &Image, field: &FlowField, sequential: bool) -> Result<Image, String> {
        self.inner.enhance(curr, field, sequential)
    }
}

/// Factory registered with [`filmcraft_render::flow::register_neural_enhancer`].
pub fn factory(width: u32, height: u32) -> Result<Box<dyn NeuralSession>, String> {
    let inner = Session::new(width, height)?;
    Ok(Box::new(NgxNeuralSession { inner }))
}

/// Whether the NVIDIA NGX DLSS runtime and hardware are available on this machine.
pub fn available() -> bool {
    Session::probe().is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use filmcraft_render::flow::{FlowEstimator, FlowVizMode, NeuralEnhancer, register_neural_enhancer, register_optical_flow, render_flow_viz};

    fn textured_frame(w: usize, h: usize, shift_x: f32, shift_y: f32) -> Image {
        let mut img = Image::new(w, h);
        for y in 0..h {
            for x in 0..w {
                let (u, v) = (x as f32 + 0.5 - shift_x, y as f32 + 0.5 - shift_y);
                let l =
                    (0.4 + 0.25 * (u * 0.09).sin() * (v * 0.08).cos() + 0.15 * ((u + v) * 0.05).sin() + 0.10 * (u * 0.23 - v * 0.19).cos()).clamp(0.05, 0.95);
                let i = (y * w + x) * 4;
                img.px[i..i + 4].copy_from_slice(&[l, l * 0.9, l * 0.8, 1.0]);
            }
        }
        img
    }

    #[test]
    fn ngx_dlss_with_nvofa_motion_vectors_reconstructs_frames_cleanly() {
        register_optical_flow(crate::nvof::factory);
        register_neural_enhancer(factory);

        let (w, h) = (320usize, 192usize);
        let a = textured_frame(w, h, 0.0, 0.0);
        let b = textured_frame(w, h, 6.0, -2.0);
        let c = textured_frame(w, h, 12.0, -4.0);

        let mut flow_est = FlowEstimator::new(w, h);
        let mut neural = NeuralEnhancer::new(w, h);
        let has_ngx = available();
        eprintln!("NGX neural backend: {} (hardware: {has_ngx}; fallback: {:?})", neural.backend(), neural.fallback_reason());
        if has_ngx {
            assert!(neural.is_hardware());
            assert_eq!(neural.backend(), "NVIDIA NGX DLSS (Preset K)");
        }

        let f0 = flow_est.estimate(&a, &b);
        let (r0, hw0) = neural.enhance(None, &a, &f0);
        assert_eq!(hw0, has_ngx);
        assert_eq!((r0.w, r0.h), (w, h));

        let f1 = flow_est.estimate(&a, &b);
        let (r1, hw1) = neural.enhance(Some(&a), &b, &f1);
        assert_eq!(hw1, has_ngx);
        let f2 = flow_est.estimate(&b, &c);
        let (r2, hw2) = neural.enhance(Some(&b), &c, &f2);
        assert_eq!(hw2, has_ngx);

        let viz = render_flow_viz(&r2, &f2, FlowVizMode::DlssEnhance, 16.0);
        let center = viz.get(160, 96);
        let ref_center = c.get(160, 96);
        eprintln!("DLSS Preset K center={center:?} vs source={ref_center:?} (r1 center={:?})", r1.get(160, 96));
        assert!((center[0] - ref_center[0]).abs() < 0.25 && center[3] == 1.0);
    }
}
