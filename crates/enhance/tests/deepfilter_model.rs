//! The effect end-to-end with real DeepFilterNet weights, when they are on this machine.
//!
//! Never downloads. Looks for the models in `$FILMCRAFT_MODELS_DIR/<id>/`, then
//! `<repo>/target/models/<id>/` (put a model there by hand, or let the app download it into the
//! data directory and point `FILMCRAFT_MODELS_DIR` at `<data dir>/models`). Without the weights
//! the tests print SKIPPED.
#![cfg(feature = "deepfilter")]

use std::path::{Path, PathBuf};

use filmcraft_audio_dsp::AudioEffect;
use filmcraft_enhance::{NeuralEnhance, catalog};

fn models_root() -> Option<PathBuf> {
    let repo = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let mut roots: Vec<PathBuf> = std::env::var_os("FILMCRAFT_MODELS_DIR").map(PathBuf::from).into_iter().collect();
    roots.push(repo.join("target/models"));
    let default = catalog::find(catalog::DEFAULT_MODEL)?;
    roots.into_iter().find(|r| catalog::installed(r, default))
}

/// Deterministic white noise in −1…1.
struct Noise(u64);

impl Noise {
    fn next(&mut self) -> f32 {
        self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        ((self.0 >> 40) as f32 / (1u64 << 23) as f32) - 1.0
    }
}

fn rms_db(x: &[f32]) -> f32 {
    let e = x.iter().map(|v| f64::from(*v) * f64::from(*v)).sum::<f64>() / x.len().max(1) as f64;
    (10.0 * (e + 1e-20).log10()) as f32
}

/// Steady broadband noise at about −26 dBFS, stereo, `seconds` long at `rate`.
fn noise(rate: u32, seconds: usize) -> (Vec<f32>, Vec<f32>) {
    let (mut a, mut b) = (Noise(1), Noise(2));
    let n = rate as usize * seconds;
    ((0..n).map(|_| a.next() * 0.087).collect(), (0..n).map(|_| b.next() * 0.087).collect())
}

/// Run stereo audio through the effect in playback-sized blocks; the result is latency-compensated.
fn run(fx: &mut NeuralEnhance, l: &[f32], r: &[f32]) -> (Vec<f32>, Vec<f32>) {
    let lat = fx.latency();
    let (mut l, mut r) = (l.to_vec(), r.to_vec());
    let n = l.len();
    l.resize(n + lat, 0.0);
    r.resize(n + lat, 0.0);
    for (a, b) in l.chunks_mut(1024).zip(r.chunks_mut(1024)) {
        fx.process(&mut [a, b]);
    }
    (l.split_off(lat), r.split_off(lat))
}

#[test]
fn noise_is_removed_and_the_attenuation_limit_is_honoured() {
    let Some(root) = models_root() else {
        eprintln!("SKIPPED: no DeepFilterNet weights (see the top of this file)");
        return;
    };
    catalog::set_models_dir(Some(root.clone()));
    for (index, model) in catalog::catalogue().iter().enumerate() {
        if !catalog::installed(&root, model) {
            eprintln!("SKIPPED {}: not installed", model.id);
            continue;
        }
        for rate in [48_000u32, 44_100] {
            let (l, r) = noise(rate, 4);
            let tail = rate as usize * 2..;
            let before = rms_db(&l[tail.clone()]);

            let mut fx = NeuralEnhance::new(rate as f32, 2, index);
            assert_eq!(fx.bypass(), None, "{} at {rate} Hz", model.id);
            assert!(fx.latency() > 0);
            fx.set_param("attenuation", 100.0);
            let (el, er) = run(&mut fx, &l, &r);
            assert!(!fx.failed(), "{} at {rate} Hz", model.id);
            assert!(el.iter().chain(&er).all(|v| v.is_finite()));
            let full = before - rms_db(&el[tail.clone()]);
            assert!(full > 20.0, "{} at {rate} Hz removed only {full:.1} dB of steady noise", model.id);

            // 12 dB limit: noise goes down by about that much and no further
            let mut fx = NeuralEnhance::new(rate as f32, 2, index);
            fx.set_param("attenuation", 12.0);
            let (el, _) = run(&mut fx, &l, &r);
            let limited = before - rms_db(&el[tail.clone()]);
            assert!((9.0..13.5).contains(&limited), "{} at {rate} Hz: a 12 dB limit removed {limited:.1} dB", model.id);

            // mix 0 %: the dry signal, delayed by the latency that `run` took off again
            let mut fx = NeuralEnhance::new(rate as f32, 2, index);
            fx.set_param("mix", 0.0);
            fx.process(&mut [&mut vec![0.0; 256], &mut vec![0.0; 256]]);
            fx.reset();
            let (dl, _) = run(&mut fx, &l, &r);
            assert_eq!(&dl[..l.len() - 4096], &l[..l.len() - 4096], "{} at {rate} Hz", model.id);
            eprintln!("{} at {rate} Hz: −{full:.1} dB unlimited, −{limited:.1} dB at a 12 dB limit, latency {} samples", model.id, fx.latency());
        }
    }
}
