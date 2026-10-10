//! The NeMo log-mel front end (`AudioToMelSpectrogramPreprocessor` at inference), from the
//! model's `model_config.yaml`:
//!
//! 1. pre-emphasis `y[n] = x[n] − 0.97·x[n−1]` (`y[0] = x[0]`); no dither at inference;
//! 2. STFT with `n_fft` 512, hop 160 (10 ms), a symmetric Hann window of 400 samples (25 ms)
//!    centred in the 512-sample frame, frames centred on `t·hop` with zero padding of `n_fft / 2`;
//! 3. power spectrum, 128-band Slaney mel filterbank (0–8 kHz, Slaney area normalisation);
//! 4. `ln(mel + 2⁻²⁴)`;
//! 5. per-feature normalisation over the clip's valid frames: `(x − mean) / (std + 1e-5)` with the
//!    unbiased standard deviation.
//!
//! `n` samples give `n / hop` valid frames. The filterbank and window stored in the checkpoint
//! (`preprocessor.featurizer.fb`, `.window`) are used when present; otherwise they are computed.

use rayon::prelude::*;

/// Front-end settings and tables.
#[derive(Clone, Debug)]
pub struct Frontend {
    pub n_fft: usize,
    pub hop: usize,
    pub n_mels: usize,
    pub preemph: Option<f32>,
    /// Added before the log.
    pub log_guard: f32,
    /// `n_fft` samples: the window, zero outside its centred span.
    window: Vec<f64>,
    /// `n_mels × (n_fft / 2 + 1)` row-major.
    filters: Vec<f32>,
    fft: Fft,
}

fn hz_to_mel(f: f64) -> f64 {
    let f_sp = 200.0 / 3.0;
    let min_log_hz = 1000.0;
    let logstep = (6.4f64).ln() / 27.0;
    if f >= min_log_hz { min_log_hz / f_sp + (f / min_log_hz).ln() / logstep } else { f / f_sp }
}

fn mel_to_hz(m: f64) -> f64 {
    let f_sp = 200.0 / 3.0;
    let min_log_hz = 1000.0;
    let min_log_mel = min_log_hz / f_sp;
    let logstep = (6.4f64).ln() / 27.0;
    if m >= min_log_mel { min_log_hz * (logstep * (m - min_log_mel)).exp() } else { f_sp * m }
}

/// Slaney-scale, Slaney-normalised mel filterbank (`n_mels × (n_fft/2 + 1)`), `fmin`..`fmax` Hz.
pub fn mel_filters(sample_rate: f64, n_fft: usize, n_mels: usize, fmin: f64, fmax: f64) -> Vec<f32> {
    let bins = n_fft / 2 + 1;
    let freqs: Vec<f64> = (0..bins).map(|i| i as f64 * sample_rate / n_fft as f64).collect();
    let (lo, hi) = (hz_to_mel(fmin), hz_to_mel(fmax));
    let mel_f: Vec<f64> = (0..n_mels + 2).map(|i| mel_to_hz(lo + (hi - lo) * i as f64 / (n_mels + 1) as f64)).collect();
    let mut w = vec![0f32; n_mels * bins];
    for m in 0..n_mels {
        let (a, b, c) = (mel_f[m], mel_f[m + 1], mel_f[m + 2]);
        let enorm = 2.0 / (c - a);
        for (k, &f) in freqs.iter().enumerate() {
            let lower = (f - a) / (b - a);
            let upper = (c - f) / (c - b);
            w[m * bins + k] = (lower.min(upper).max(0.0) * enorm) as f32;
        }
    }
    w
}

/// Symmetric ("periodic=False") Hann window.
pub fn hann(n: usize) -> Vec<f32> {
    if n < 2 {
        return vec![1.0; n];
    }
    (0..n).map(|i| (0.5 - 0.5 * (2.0 * std::f64::consts::PI * i as f64 / (n - 1) as f64).cos()) as f32).collect()
}

/// Radix-2 complex FFT of a fixed power-of-two size.
#[derive(Clone, Debug)]
struct Fft {
    n: usize,
    rev: Vec<usize>,
    /// `exp(−2πik/n)` for k < n/2.
    tw: Vec<(f64, f64)>,
}

impl Fft {
    fn new(n: usize) -> Self {
        let bits = n.trailing_zeros();
        let rev = (0..n).map(|i| if bits == 0 { 0 } else { i.reverse_bits() >> (usize::BITS - bits) }).collect();
        let tw = (0..n / 2)
            .map(|k| {
                let a = -2.0 * std::f64::consts::PI * k as f64 / n as f64;
                (a.cos(), a.sin())
            })
            .collect();
        Self { n, rev, tw }
    }

    /// In-place transform of `re`/`im` (length `n`).
    fn run(&self, re: &mut [f64], im: &mut [f64]) {
        let n = self.n;
        for i in 0..n {
            let j = self.rev[i];
            if j > i {
                re.swap(i, j);
                im.swap(i, j);
            }
        }
        let mut len = 2;
        while len <= n {
            let step = n / len;
            for start in (0..n).step_by(len) {
                for k in 0..len / 2 {
                    let (wr, wi) = self.tw[k * step];
                    let (a, b) = (start + k, start + k + len / 2);
                    let (xr, xi) = (re[b] * wr - im[b] * wi, re[b] * wi + im[b] * wr);
                    re[b] = re[a] - xr;
                    im[b] = im[a] - xi;
                    re[a] += xr;
                    im[a] += xi;
                }
            }
            len <<= 1;
        }
    }
}

impl Frontend {
    /// `win`: the analysis window (≤ `n_fft` samples, centred); `filters`: `n_mels × (n_fft/2+1)`.
    pub fn new(n_fft: usize, hop: usize, win: &[f32], filters: Vec<f32>, n_mels: usize, preemph: Option<f32>, log_guard: f32) -> Option<Self> {
        if !n_fft.is_power_of_two()
            || !(16..=1 << 16).contains(&n_fft)
            || hop == 0
            || win.len() > n_fft
            || filters.len() != n_mels * (n_fft / 2 + 1)
            || n_mels == 0
        {
            return None;
        }
        let mut window = vec![0f64; n_fft];
        let left = (n_fft - win.len()) / 2;
        for (i, w) in win.iter().enumerate() {
            window[left + i] = f64::from(*w);
        }
        Some(Self { n_fft, hop, n_mels, preemph, log_guard, window, filters, fft: Fft::new(n_fft) })
    }

    /// Normalised log-mel features of `audio`: `(frames, n_mels × frames row-major by band)`.
    /// Fewer than two frames give no features (the normalisation needs two).
    pub fn features(&self, audio: &[f32]) -> (usize, Vec<f32>) {
        let frames = audio.len() / self.hop;
        if frames < 2 {
            return (0, Vec::new());
        }
        let y: Vec<f32> = match self.preemph {
            Some(k) => std::iter::once(audio[0]).chain(audio.windows(2).map(|w| w[1] - k * w[0])).collect(),
            None => audio.to_vec(),
        };
        let bins = self.n_fft / 2 + 1;
        let pad = (self.n_fft / 2) as isize;
        let n = y.len() as isize;
        // log-mel per frame (frame-major), then transposed
        let mut fm = vec![0f32; frames * self.n_mels];
        fm.par_chunks_mut(self.n_mels).enumerate().for_each(|(t, out)| {
            let mut re = vec![0f64; self.n_fft];
            let mut im = vec![0f64; self.n_fft];
            let base = (t * self.hop) as isize - pad;
            for (i, r) in re.iter_mut().enumerate() {
                let j = base + i as isize;
                let w = self.window[i];
                if w != 0.0 && j >= 0 && j < n {
                    *r = f64::from(y[j as usize]) * w;
                }
            }
            self.fft.run(&mut re, &mut im);
            let pow: Vec<f32> = (0..bins).map(|k| (re[k] * re[k] + im[k] * im[k]) as f32).collect();
            for (m, o) in out.iter_mut().enumerate() {
                let f = &self.filters[m * bins..(m + 1) * bins];
                let e: f32 = f.iter().zip(&pow).map(|(a, b)| a * b).sum();
                *o = (e + self.log_guard).ln();
            }
        });
        let mut out = vec![0f32; self.n_mels * frames];
        out.par_chunks_mut(frames).enumerate().for_each(|(m, row)| {
            for (t, v) in row.iter_mut().enumerate() {
                *v = fm[t * self.n_mels + m];
            }
            let mean = row.iter().map(|&v| f64::from(v)).sum::<f64>() / frames as f64;
            let var = row.iter().map(|&v| (f64::from(v) - mean).powi(2)).sum::<f64>() / (frames - 1) as f64;
            let std = var.sqrt() + 1e-5;
            for v in row.iter_mut() {
                *v = ((f64::from(*v) - mean) / std) as f32;
            }
        });
        (frames, out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fft_matches_a_direct_dft() {
        let n = 64;
        let f = Fft::new(n);
        let x: Vec<f64> = (0..n).map(|i| ((i * 7 % 13) as f64 - 6.0) / 3.0).collect();
        let (mut re, mut im) = (x.clone(), vec![0.0; n]);
        f.run(&mut re, &mut im);
        for k in 0..n {
            let (mut r, mut i) = (0.0, 0.0);
            for (t, v) in x.iter().enumerate() {
                let a = -2.0 * std::f64::consts::PI * (k * t) as f64 / n as f64;
                r += v * a.cos();
                i += v * a.sin();
            }
            assert!((re[k] - r).abs() < 1e-9 && (im[k] - i).abs() < 1e-9, "bin {k}");
        }
    }

    #[test]
    fn tone_lands_in_its_band_and_is_normalised() {
        let fb = mel_filters(16_000.0, 512, 128, 0.0, 8_000.0);
        let fe = Frontend::new(512, 160, &hann(400), fb.clone(), 128, Some(0.97), 2f32.powi(-24)).unwrap();
        let audio: Vec<f32> = (0..16_000).map(|i| 0.5 * (2.0 * std::f32::consts::PI * 1000.0 * i as f32 / 16_000.0).sin()).collect();
        let (frames, m) = fe.features(&audio);
        assert_eq!(frames, 100);
        // every band has zero mean after normalisation
        for b in 0..128 {
            let mean: f32 = m[b * frames..(b + 1) * frames].iter().sum::<f32>() / frames as f32;
            assert!(mean.abs() < 1e-3, "band {b}: {mean}");
        }
        // the 1 kHz bin (16) peaks in the band whose filter weighs it most
        let peak = (0..128).max_by(|a, b| fb[a * 257 + 16].total_cmp(&fb[b * 257 + 16])).unwrap();
        assert!(fb[peak * 257 + 16] > 0.0);
        assert_eq!(fe.features(&audio[..200]).0, 0);
        assert!(Frontend::new(500, 160, &hann(400), fb, 128, None, 1e-6).is_none());
    }
}
