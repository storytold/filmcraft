//! Log-mel spectrogram front end (Whisper's), computed from the published recipe:
//! 16 kHz audio, 400-sample periodic Hann window, hop 160 (100 frames/s), reflect padding of
//! 200 samples at both ends (centred frames), power spectrum, Slaney-style mel filterbank with
//! Slaney area normalisation (0–8 kHz), `log10(max(x, 1e-10))`, floor at `max − 8`, then
//! `(x + 4) / 4`. The last STFT frame is dropped, so `n` samples give `n / 160` frames.
//!
//! The filterbank is computed here, not loaded from a file.

/// FFT size (25 ms).
pub const N_FFT: usize = 400;
/// Hop length (10 ms).
pub const HOP: usize = 160;
/// Frequency bins of the one-sided spectrum.
pub const N_BINS: usize = N_FFT / 2 + 1;

fn hz_to_mel(f: f64) -> f64 {
    // Slaney: linear below 1 kHz, logarithmic above
    let f_sp = 200.0 / 3.0;
    let min_log_hz = 1000.0;
    let min_log_mel = min_log_hz / f_sp;
    let logstep = (6.4f64).ln() / 27.0;
    if f >= min_log_hz { min_log_mel + (f / min_log_hz).ln() / logstep } else { f / f_sp }
}

fn mel_to_hz(m: f64) -> f64 {
    let f_sp = 200.0 / 3.0;
    let min_log_hz = 1000.0;
    let min_log_mel = min_log_hz / f_sp;
    let logstep = (6.4f64).ln() / 27.0;
    if m >= min_log_mel { min_log_hz * (logstep * (m - min_log_mel)).exp() } else { f_sp * m }
}

/// Mel filterbank, `n_mels × N_BINS` row-major, for 16 kHz audio and a 400-point FFT.
pub fn mel_filters(n_mels: usize) -> Vec<f32> {
    mel_filters_for(n_mels, N_FFT)
}

/// Mel filterbank for an `n_fft`-point FFT of 16 kHz audio, `n_mels × (n_fft / 2 + 1)` row-major
/// (Slaney scale and area normalisation, 0–8 kHz).
pub fn mel_filters_for(n_mels: usize, n_fft: usize) -> Vec<f32> {
    let sr = 16_000.0;
    let n_bins = n_fft / 2 + 1;
    let fftfreqs: Vec<f64> = (0..n_bins).map(|i| i as f64 * sr / n_fft.max(1) as f64).collect();
    let (lo, hi) = (hz_to_mel(0.0), hz_to_mel(sr / 2.0));
    let mel_f: Vec<f64> = (0..n_mels + 2).map(|i| mel_to_hz(lo + (hi - lo) * i as f64 / (n_mels + 1) as f64)).collect();
    let mut w = vec![0f32; n_mels * n_bins];
    for m in 0..n_mels {
        let enorm = 2.0 / (mel_f[m + 2] - mel_f[m]);
        for (k, &f) in fftfreqs.iter().enumerate() {
            let lower = (f - mel_f[m]) / (mel_f[m + 1] - mel_f[m]);
            let upper = (mel_f[m + 2] - f) / (mel_f[m + 2] - mel_f[m + 1]);
            w[m * n_bins + k] = (lower.min(upper).max(0.0) * enorm) as f32;
        }
    }
    w
}

/// Power spectrum frames (`frames × N_BINS`) of `audio`, Whisper framing (see module docs).
pub fn power_spectrum(audio: &[f32]) -> (usize, Vec<f32>) {
    let pad = N_FFT / 2;
    let n = audio.len();
    // reflect padding (numpy "reflect": the edge sample is not repeated)
    let at = |i: isize| -> f32 {
        if n == 0 {
            return 0.0;
        }
        if n == 1 {
            return audio[0];
        }
        let p = 2 * (n as isize - 1);
        let mut j = i.rem_euclid(p);
        if j >= n as isize {
            j = p - j;
        }
        audio[j as usize]
    };
    let frames = n / HOP; // the last centred frame (n / HOP + 1) is dropped
    let window: Vec<f32> = (0..N_FFT).map(|i| (0.5 - 0.5 * (2.0 * std::f64::consts::PI * i as f64 / N_FFT as f64).cos()) as f32).collect();
    let (cos, sin) = twiddles();
    let mut out = vec![0f32; frames * N_BINS];
    let work = |f: usize, row: &mut [f32]| {
        let mut x = [0f32; N_FFT];
        let base = (f * HOP) as isize - pad as isize;
        for (i, v) in x.iter_mut().enumerate() {
            *v = at(base + i as isize) * window[i];
        }
        for (k, r) in row.iter_mut().enumerate() {
            let (mut re, mut im) = (0f32, 0f32);
            let (c, s) = (&cos[k * N_FFT..(k + 1) * N_FFT], &sin[k * N_FFT..(k + 1) * N_FFT]);
            for i in 0..N_FFT {
                re += x[i] * c[i];
                im += x[i] * s[i];
            }
            *r = re * re + im * im;
        }
    };
    {
        use rayon::prelude::*;
        out.par_chunks_mut(N_BINS).enumerate().for_each(|(f, row)| work(f, row));
    }
    (frames, out)
}

fn twiddles() -> (Vec<f32>, Vec<f32>) {
    let mut c = vec![0f32; N_BINS * N_FFT];
    let mut s = vec![0f32; N_BINS * N_FFT];
    for k in 0..N_BINS {
        for i in 0..N_FFT {
            // index the angle table by (k·i mod N) so the f64 argument stays small and exact
            let a = 2.0 * std::f64::consts::PI * ((k * i) % N_FFT) as f64 / N_FFT as f64;
            c[k * N_FFT + i] = a.cos() as f32;
            s[k * N_FFT + i] = -a.sin() as f32;
        }
    }
    (c, s)
}

/// Mel energies `n_mels × frames` (row-major by mel band, like Whisper's input) before the log.
pub fn mel_energies(audio: &[f32], n_mels: usize) -> (usize, Vec<f32>) {
    let (frames, pow) = power_spectrum(audio);
    let filters = mel_filters(n_mels);
    let mut out = vec![0f32; n_mels * frames];
    for f in 0..frames {
        let p = &pow[f * N_BINS..(f + 1) * N_BINS];
        for m in 0..n_mels {
            let w = &filters[m * N_BINS..(m + 1) * N_BINS];
            out[m * frames + f] = w.iter().zip(p).map(|(a, b)| a * b).sum();
        }
    }
    (frames, out)
}

/// Whisper's normalised log-mel spectrogram, `n_mels × frames` row-major.
pub fn log_mel(audio: &[f32], n_mels: usize) -> (usize, Vec<f32>) {
    let (frames, mut m) = mel_energies(audio, n_mels);
    for v in &mut m {
        *v = v.max(1e-10).log10();
    }
    let max = m.iter().copied().fold(f32::NEG_INFINITY, f32::max);
    for v in &mut m {
        *v = (v.max(max - 8.0) + 4.0) / 4.0;
    }
    (frames, m)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filterbank_shape_and_coverage() {
        let w = mel_filters(80);
        assert_eq!(w.len(), 80 * N_BINS);
        // every band has weight, nothing is negative
        for m in 0..80 {
            let row = &w[m * N_BINS..(m + 1) * N_BINS];
            assert!(row.iter().all(|v| *v >= 0.0));
            assert!(row.iter().sum::<f32>() > 0.0, "band {m}");
        }
        // Slaney scale is linear below 1 kHz
        assert!((hz_to_mel(500.0) - 7.5).abs() < 1e-9);
        assert!((mel_to_hz(hz_to_mel(3000.0)) - 3000.0).abs() < 1e-6);
    }

    #[test]
    fn a_tone_lands_in_its_band() {
        let audio: Vec<f32> = (0..16_000).map(|i| (2.0 * std::f32::consts::PI * 1000.0 * i as f32 / 16_000.0).sin()).collect();
        let (frames, m) = log_mel(&audio, 80);
        assert_eq!(frames, 100);
        let f = 50;
        let col: Vec<f32> = (0..80).map(|b| m[b * frames + f]).collect();
        let peak = col.iter().enumerate().max_by(|a, b| a.1.total_cmp(b.1)).unwrap().0;
        let w = mel_filters(80);
        let k = 1000 * N_FFT / 16_000; // bin 25
        let best = (0..80).max_by(|a, b| w[a * N_BINS + k].total_cmp(&w[b * N_BINS + k])).unwrap();
        assert_eq!(peak, best);
    }
}
