//! Optional NVIDIA NGX DLAA backend for temporal detail enhancement on Windows.
//! The SDK build uses its public C interface on a Direct3D 11 device. The render crate
//! retains its motion-compensated CPU path when this factory declines or evaluation fails.

#[cfg(filmcraft_ngx_sdk)]
#[allow(unsafe_code)]
mod ffi;
#[cfg(filmcraft_ngx_sdk)]
#[allow(unsafe_code)]
mod session;

#[cfg(all(test, filmcraft_ngx_sdk))]
mod abi_tests;

use filmcraft_render::Image;
use filmcraft_render::flow::{FlowField, NeuralSession};

#[cfg(filmcraft_ngx_sdk)]
pub use self::session::Session;

/// NGX is optional. Builds without the SDK retain the same factory and CPU fallback.
#[cfg(not(filmcraft_ngx_sdk))]
pub struct Session(std::convert::Infallible);

#[cfg(not(filmcraft_ngx_sdk))]
impl Session {
    pub fn new(_: u32, _: u32) -> Result<Self, String> {
        Err("FilmCraft was built without the optional NVIDIA NGX SDK".into())
    }
    pub fn probe() -> Result<(), String> {
        Self::new(64, 64).map(drop)
    }
    pub fn enhance(&mut self, _: &Image, _: &FlowField, _: bool) -> Result<Image, String> {
        match self.0 {}
    }
}

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
        for (output, reference) in [(&r0, &a), (&r1, &b), (&r2, &c), (&viz, &c)] {
            let error = rgb_rmse(output, reference);
            assert!(error < 0.05, "clean translating texture RMSE {error}");
        }
        drop(neural);
        if has_ngx {
            Session::new(w as u32, h as u32).expect("reopen after last feature and NGX shutdown");
        }
    }

    fn rgb_rmse(actual: &Image, reference: &Image) -> f64 {
        assert_eq!((actual.w, actual.h, actual.px.len()), (reference.w, reference.h, reference.px.len()));
        let sum: f64 = actual
            .px
            .as_chunks::<4>()
            .0
            .iter()
            .zip(reference.px.as_chunks::<4>().0)
            .map(|(a, b)| (0..3).map(|k| f64::from(a[k] - b[k]).powi(2)).sum::<f64>())
            .sum();
        (sum / (actual.w * actual.h * 3) as f64).sqrt()
    }

    #[test]
    #[cfg(filmcraft_ngx_sdk)]
    #[ignore = "requires an installed NGX DLSS runtime and an RTX GPU"]
    fn compare_dlaa_and_cpu_on_noisy_translation() {
        use filmcraft_render::flow::fuse_temporal_cpu;
        let (w, h) = (320, 192);
        register_optical_flow(crate::nvof::factory);
        let mut flow = FlowEstimator::new(w, h);
        let mut dlaa = Session::new(w as u32, h as u32).unwrap();
        let mut prev = None;
        let mut seed = 1147u32;
        let mut errors = [0.0f64; 3];
        for i in 0..8 {
            let clean = textured_frame(w, h, i as f32 * 3.0, -(i as f32));
            let mut noisy = clean.clone();
            for px in noisy.px.as_chunks_mut::<4>().0 {
                seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
                let noise = ((seed >> 8) as f32 / 16777216.0 - 0.5) * 0.08;
                for c in &mut px[..3] {
                    *c += noise;
                }
            }
            let field = flow.estimate(prev.as_ref().unwrap_or(&noisy), &noisy);
            let hw = dlaa.enhance(&noisy, &field, prev.is_some()).unwrap();
            let cpu = fuse_temporal_cpu(prev.as_ref(), &noisy, &field);
            if i > 0 {
                let hw = render_flow_viz(&hw, &field, FlowVizMode::DlssEnhance, 16.0);
                let cpu = render_flow_viz(&cpu, &field, FlowVizMode::DlssEnhance, 16.0);
                for (sum, img) in errors.iter_mut().zip([&noisy, &cpu, &hw]) {
                    let rmse = rgb_rmse(img, &clean);
                    assert!(rmse < 0.08, "frame {i}: {rmse}");
                    *sum += rmse;
                }
            }
            prev = Some(noisy);
        }
        let [input, cpu, hw] = errors.map(|v| v / 7.0);
        eprintln!(
            "320x192 translating texture, mean linear RGB RMSE over frames 2-8: input {input:.5}, CPU + detail {cpu:.5}, DLAA + detail {hw:.5}; flow {}",
            flow.backend()
        );
    }
}
