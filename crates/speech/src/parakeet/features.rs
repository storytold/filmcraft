//! Parakeet's log-mel front end (NeMo's `AudioToMelSpectrogramPreprocessor` as configured for
//! the FastConformer models), from the published configuration (`processor_config.json`):
//!
//! 16 kHz audio, pre-emphasis 0.97, a 400-sample symmetric Hann window centred in a 512-point
//! FFT, hop 160 (100 frames/s), frames centred on their hop with zero padding of 256 samples at
//! both ends, power spectrum, 128-band Slaney mel filterbank (0–8 kHz, area normalised),
//! `ln(x + 2⁻²⁴)`, then each band normalised to zero mean and unit variance over the clip
//! (unbiased variance, `σ + 10⁻⁵` in the denominator). `n` samples give `n / 160` frames.

use rayon::prelude::*;

pub const N_FFT: usize = 512;
pub const WIN: usize = 400;
pub const HOP: usize = 160;
pub const N_BINS: usize = N_FFT / 2 + 1;
const PREEMPHASIS: f32 = 0.97;
const LOG_GUARD: f32 = 1.0 / (1u32 << 24) as f32;
const EPS: f32 = 1e-5;

/// In-place iterative radix-2 FFT of `re`/`im` (length `N_FFT`).
fn fft(re: &mut [f32; N_FFT], im: &mut [f32; N_FFT], cos: &[f32], sin: &[f32]) {
    let bits = N_FFT.trailing_zeros();
    for i in 0..N_FFT {
        let j = i.reverse_bits() >> (usize::BITS - bits);
        if j > i {
            re.swap(i, j);
            im.swap(i, j);
        }
    }
    let mut len = 2;
    while len <= N_FFT {
        let step = N_FFT / len;
        for start in (0..N_FFT).step_by(len) {
            for k in 0..len / 2 {
                let (c, s) = (cos[k * step], sin[k * step]);
                let (a, b) = (start + k, start + k + len / 2);
                let (tr, ti) = (re[b] * c - im[b] * s, re[b] * s + im[b] * c);
                re[b] = re[a] - tr;
                im[b] = im[a] - ti;
                re[a] += tr;
                im[a] += ti;
            }
        }
        len <<= 1;
    }
}

/// Normalised log-mel features, `frames × n_mels` row-major (time-major, the encoder's layout).
pub fn features(audio: &[f32], n_mels: usize) -> (usize, Vec<f32>) {
    let n = audio.len();
    let frames = n / HOP;
    if frames == 0 || n_mels == 0 {
        return (0, Vec::new());
    }
    let x: Vec<f32> = (0..n).map(|i| if i == 0 { audio[0] } else { audio[i] - PREEMPHASIS * audio[i - 1] }).collect();
    let off = (N_FFT - WIN) / 2;
    let window: Vec<f32> = (0..WIN).map(|i| (0.5 - 0.5 * (2.0 * std::f64::consts::PI * i as f64 / (WIN - 1) as f64).cos()) as f32).collect();
    let cos: Vec<f32> = (0..N_FFT / 2).map(|k| (2.0 * std::f64::consts::PI * k as f64 / N_FFT as f64).cos() as f32).collect();
    let sin: Vec<f32> = (0..N_FFT / 2).map(|k| -(2.0 * std::f64::consts::PI * k as f64 / N_FFT as f64).sin() as f32).collect();
    let filters = crate::mel::mel_filters_for(n_mels, N_FFT);
    let mut out = vec![0f32; frames * n_mels];
    out.par_chunks_mut(n_mels).enumerate().for_each(|(f, row)| {
        let mut re = [0f32; N_FFT];
        let mut im = [0f32; N_FFT];
        // frame f is centred on sample f·HOP: padded index f·HOP + i is sample f·HOP + i − N_FFT/2
        let base = (f * HOP) as isize - (N_FFT / 2) as isize;
        for (i, w) in window.iter().enumerate() {
            let s = base + (off + i) as isize;
            if s >= 0
                && let Some(v) = x.get(s as usize)
            {
                re[off + i] = v * w;
            }
        }
        fft(&mut re, &mut im, &cos, &sin);
        let mut pow = [0f32; N_BINS];
        for (k, p) in pow.iter_mut().enumerate() {
            *p = re[k] * re[k] + im[k] * im[k];
        }
        for (m, r) in row.iter_mut().enumerate() {
            let w = &filters[m * N_BINS..(m + 1) * N_BINS];
            let e: f32 = w.iter().zip(&pow).map(|(a, b)| a * b).sum();
            *r = (e + LOG_GUARD).ln();
        }
    });
    // per-band normalisation over the clip
    let denom = (frames.saturating_sub(1)).max(1) as f32;
    for m in 0..n_mels {
        let mean = (0..frames).map(|f| out[f * n_mels + m]).sum::<f32>() / frames as f32;
        let var = (0..frames).map(|f| (out[f * n_mels + m] - mean).powi(2)).sum::<f32>() / denom;
        let k = 1.0 / (var.sqrt() + EPS);
        for f in 0..frames {
            let v = &mut out[f * n_mels + m];
            *v = (*v - mean) * k;
        }
    }
    (frames, out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fft_matches_a_direct_dft() {
        let cos: Vec<f32> = (0..N_FFT / 2).map(|k| (2.0 * std::f64::consts::PI * k as f64 / N_FFT as f64).cos() as f32).collect();
        let sin: Vec<f32> = (0..N_FFT / 2).map(|k| -(2.0 * std::f64::consts::PI * k as f64 / N_FFT as f64).sin() as f32).collect();
        let sig: Vec<f32> = (0..N_FFT).map(|i| ((i * 7919) % 101) as f32 / 50.0 - 1.0).collect();
        let mut re = [0f32; N_FFT];
        re.copy_from_slice(&sig);
        let mut im = [0f32; N_FFT];
        fft(&mut re, &mut im, &cos, &sin);
        for k in [0, 1, 17, 100, 256] {
            let (mut dr, mut di) = (0f64, 0f64);
            for (i, v) in sig.iter().enumerate() {
                let a = -2.0 * std::f64::consts::PI * (k * i) as f64 / N_FFT as f64;
                dr += *v as f64 * a.cos();
                di += *v as f64 * a.sin();
            }
            assert!((re[k] as f64 - dr).abs() < 1e-2 && (im[k] as f64 - di).abs() < 1e-2, "bin {k}");
        }
    }

    #[test]
    fn frames_are_normalised_per_band() {
        let audio: Vec<f32> = (0..16_000).map(|i| (2.0 * std::f32::consts::PI * 440.0 * i as f32 / 16_000.0).sin() * (i as f32 / 16_000.0)).collect();
        let (frames, f) = features(&audio, 128);
        assert_eq!(frames, 100);
        assert_eq!(f.len(), 100 * 128);
        assert!(f.iter().all(|v| v.is_finite()));
        let mean: f32 = (0..frames).map(|t| f[t * 128 + 10]).sum::<f32>() / frames as f32;
        assert!(mean.abs() < 1e-3);
        // too short for one frame, and silence, never panic or produce NaN
        assert_eq!(features(&[0.0; 100], 128).0, 0);
        assert!(features(&[0.0; 3200], 128).1.iter().all(|v| v.is_finite()));
    }
}
