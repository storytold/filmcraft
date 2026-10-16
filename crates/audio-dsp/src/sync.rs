//! Synchronising recordings of the same event by their audio (Synchronize, Merge Clips and
//! Create Multi-Camera Source Sequence "Audio").
//!
//! [`find_offset`] returns the lag `L` (in samples) for which `b[n] ≈ g · a[n + L]`: when `b`
//! started recording two seconds after `a`, `L = 2 · sample_rate`. It works in two passes:
//!
//! 1. **Coarse.** Both signals are DC-removed, normalised, low-pass filtered (windowed sinc) and
//!    decimated to ≤ 8 kHz (further for very long recordings, so the transform stays ≤ 4 M points).
//!    The generalised cross-correlation with partial phase transform weighting (GCC-PHAT-β:
//!    `R = A·B* / |A·B*|^β`) is computed with one complex FFT of both signals packed as `a + i·b`.
//!    PHAT weighting whitens the spectra, so the peak is sharp and independent of the microphones'
//!    gains and frequency responses; β < 1 keeps noise-only bins from dominating.
//! 2. **Refine.** Around the coarse peak, the same correlation is computed at the full sample rate
//!    on the loudest common window (≤ 2.7 s at 48 kHz), and the peak is refined by parabolic
//!    interpolation to a fraction of a sample.
//!
//! Two figures say how much to trust the result: `confidence`, the coarse peak's height over the
//! RMS of the correlation, and `distinct`, its height over the highest peak more than 50 ms away
//! (periodic material such as music or a steady hum can match at several lags).
//!
//! References: Knapp & Carter, "The generalized correlation method for estimation of time delay",
//! IEEE Trans. ASSP 24(4), 1976; the β-weighted PHAT variant is standard textbook practice.

use crate::fft::Fft;

/// Tuning of [`find_offset`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SyncOptions {
    /// Largest |lag| considered, in samples (None = anything with enough overlap).
    pub max_lag: Option<i64>,
    /// Upper bound of the coarse search's sample rate (Hz).
    pub coarse_rate: u32,
    /// Most samples (both signals together) the coarse transform may hold; longer recordings are
    /// decimated further.
    pub max_coarse_samples: usize,
    /// PHAT weighting exponent (1 = full whitening, 0 = plain cross-correlation).
    pub phat_beta: f32,
    /// Length of the full-rate refinement window (samples).
    pub refine_window: usize,
}

impl Default for SyncOptions {
    fn default() -> Self {
        Self { max_lag: None, coarse_rate: 8000, max_coarse_samples: 1 << 22, phat_beta: 0.75, refine_window: 1 << 17 }
    }
}

/// The offset found between two recordings.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SyncResult {
    /// `b[n]` lines up with `a[n + lag]` (samples at the input rate).
    pub lag: i64,
    /// The same with sub-sample precision (parabolic interpolation of the refined peak).
    pub lag_fine: f64,
    /// Coarse peak height over the RMS of the correlation.
    pub confidence: f32,
    /// Coarse peak height over the next-highest peak more than 50 ms away.
    pub distinct: f32,
    /// True when the signals correlate negatively (one recording has inverted polarity).
    pub inverted: bool,
}

impl SyncResult {
    /// Whether the match is clear enough to trust.
    pub fn reliable(&self) -> bool {
        self.confidence >= 8.0 && self.distinct >= 1.5
    }
}

/// Average interleaved or planar channels into one mono signal.
pub fn mixdown(channels: &[&[f32]]) -> Vec<f32> {
    let n = channels.iter().map(|c| c.len()).min().unwrap_or(0);
    let k = channels.len().max(1) as f32;
    (0..n).map(|i| channels.iter().map(|c| c[i]).sum::<f32>() / k).collect()
}

/// Low-pass filter (Blackman-windowed sinc, cutoff at 0.42 × the new sample rate) and keep every
/// `d`-th sample.
pub fn decimate(x: &[f32], d: usize) -> Vec<f32> {
    // a factor beyond the signal length gains nothing (find_offset rejects such signals): cap it
    // so the filter taps stay bounded
    let d = d.min(x.len());
    if d <= 1 {
        return x.to_vec();
    }
    let half = 8 * d;
    let fc = 0.42 / d as f64; // cycles per input sample
    let taps: Vec<f32> = {
        let n = 2 * half + 1;
        let mut h: Vec<f64> = (0..n)
            .map(|k| {
                let t = k as f64 - half as f64;
                let sinc = if t == 0.0 { 2.0 * fc } else { (2.0 * std::f64::consts::PI * fc * t).sin() / (std::f64::consts::PI * t) };
                let w = 0.42 - 0.5 * (2.0 * std::f64::consts::PI * k as f64 / (n - 1) as f64).cos()
                    + 0.08 * (4.0 * std::f64::consts::PI * k as f64 / (n - 1) as f64).cos();
                sinc * w
            })
            .collect();
        let s: f64 = h.iter().sum();
        h.iter_mut().for_each(|v| *v /= s);
        h.into_iter().map(|v| v as f32).collect()
    };
    let out_len = x.len().div_ceil(d);
    let mut out = Vec::with_capacity(out_len);
    for i in 0..out_len {
        let c = (i * d) as isize;
        let lo = (c - half as isize).max(0) as usize;
        let hi = ((c + half as isize + 1) as usize).min(x.len());
        let k0 = (lo as isize - (c - half as isize)) as usize;
        let mut acc = 0.0f32;
        for (j, &v) in x[lo..hi].iter().enumerate() {
            acc += v * taps[k0 + j];
        }
        out.push(acc);
    }
    out
}

/// Remove the mean and scale to unit RMS (silence stays silence).
fn normalise(x: &mut [f32]) {
    if x.is_empty() {
        return;
    }
    let mean = (x.iter().map(|&v| v as f64).sum::<f64>() / x.len() as f64) as f32;
    x.iter_mut().for_each(|v| *v -= mean);
    let rms = (x.iter().map(|&v| (v as f64) * (v as f64)).sum::<f64>() / x.len() as f64).sqrt();
    if rms > 1e-12 {
        let k = (1.0 / rms) as f32;
        x.iter_mut().for_each(|v| *v *= k);
    }
}

/// GCC-PHAT-β of `a` and `b` (zero-padded to `n`, a power of two ≥ `a.len() + b.len()`):
/// `r[L mod n] = Σ a[i + L]·b[i]` with whitened spectra. Only bins whose frequency (cycles per
/// sample) lies in `band` contribute.
fn gcc(a: &[f32], b: &[f32], n: usize, beta: f32, band: (f32, f32)) -> Vec<f32> {
    let fft = Fft::new(n);
    let mut re = vec![0.0f32; n];
    let mut im = vec![0.0f32; n];
    re[..a.len()].copy_from_slice(a);
    im[..b.len()].copy_from_slice(b);
    fft.forward(&mut re, &mut im);
    // X = A + iB for real a, b: A(k) = (X(k) + X*(n-k)) / 2, B(k) = (X(k) - X*(n-k)) / 2i.
    let weigh = |k: usize, xr: f32, xi: f32, yr: f32, yi: f32| -> (f32, f32) {
        // y = X*(n-k) given as (yr, yi) already conjugated
        let (ar, ai) = (0.5 * (xr + yr), 0.5 * (xi + yi));
        let (dr, di) = (xr - yr, xi - yi);
        let (br, bi) = (0.5 * di, -0.5 * dr);
        // R = A·conj(B)
        let rr = ar * br + ai * bi;
        let ri = ai * br - ar * bi;
        let f = k.min(n - k) as f32 / n as f32;
        if f < band.0 || f > band.1 {
            return (0.0, 0.0);
        }
        let mag = (rr * rr + ri * ri).sqrt();
        if mag <= 1e-30 {
            return (0.0, 0.0);
        }
        let w = mag.powf(-beta);
        (rr * w, ri * w)
    };
    for k in 0..=n / 2 {
        let j = (n - k) % n;
        let (xkr, xki, xjr, xji) = (re[k], im[k], re[j], im[j]);
        let rk = weigh(k, xkr, xki, xjr, -xji);
        let rj = weigh(j, xjr, xji, xkr, -xki);
        re[k] = rk.0;
        im[k] = rk.1;
        re[j] = rj.0;
        im[j] = rj.1;
    }
    fft.inverse(&mut re, &mut im);
    re
}

fn corr_at(r: &[f32], lag: i64) -> f32 {
    let n = r.len() as i64;
    r[lag.rem_euclid(n) as usize]
}

/// Find the lag `L` with `b[n] ≈ g·a[n + L]` (see the module docs). `None` when either signal is
/// too short or silent, or no lag leaves enough overlap.
pub fn find_offset(a: &[f32], b: &[f32], sample_rate: u32, opts: &SyncOptions) -> Option<SyncResult> {
    let sr = sample_rate.max(1) as usize;
    let min_len = (sr / 10).max(64);
    if a.len() < min_len || b.len() < min_len {
        return None;
    }
    let energy = |x: &[f32]| x.iter().map(|&v| (v as f64) * (v as f64)).sum::<f64>();
    if energy(a) < 1e-12 || energy(b) < 1e-12 {
        return None;
    }
    // ---- coarse
    let d = (sr.div_ceil(opts.coarse_rate.max(500) as usize)).max((a.len() + b.len()).div_ceil(opts.max_coarse_samples.max(1 << 12))).max(1);
    // too short for this decimation factor (huge `max_coarse_samples`/rate mismatch): no estimate
    if d > a.len().min(b.len()) {
        return None;
    }
    let mut ca = a.to_vec();
    let mut cb = b.to_vec();
    normalise(&mut ca);
    normalise(&mut cb);
    let (ca, cb) = (decimate(&ca, d), decimate(&cb, d));
    let (la, lb) = (ca.len() as i64, cb.len() as i64);
    let n = ((la + lb) as usize).next_power_of_two();
    let rate_c = sr as f32 / d as f32;
    let band = ((40.0 / rate_c).min(0.05), if d > 1 { 0.40 } else { 0.5 });
    let r = gcc(&ca, &cb, n, opts.phat_beta, band);
    // valid lags: b[0..lb) against a[L..L+lb) must overlap by a minimum amount
    let min_overlap = ((rate_c as i64) / 2).min(la.min(lb) / 4).max(8);
    let max_lag_c = opts.max_lag.map(|m| (m.saturating_abs() / d as i64).saturating_add(1));
    let lo = -(lb - min_overlap);
    let hi = la - min_overlap;
    if hi < lo {
        return None;
    }
    let mut best = (0i64, 0.0f32);
    let mut sum2 = 0.0f64;
    let mut count = 0usize;
    for l in lo..=hi {
        if max_lag_c.is_some_and(|m| l.abs() > m) {
            continue;
        }
        let v = corr_at(&r, l);
        sum2 += (v as f64) * (v as f64);
        count += 1;
        if v.abs() > best.1.abs() {
            best = (l, v);
        }
    }
    if count == 0 || best.1 == 0.0 {
        return None;
    }
    let rms = (sum2 / count as f64).sqrt().max(1e-30) as f32;
    let confidence = best.1.abs() / rms;
    let guard = ((rate_c * 0.05) as i64).max(2);
    let mut second = 0.0f32;
    for l in lo..=hi {
        if (l - best.0).abs() <= guard || max_lag_c.is_some_and(|m| l.abs() > m) {
            continue;
        }
        second = second.max(corr_at(&r, l).abs());
    }
    let distinct = if second > 0.0 { best.1.abs() / second } else { f32::INFINITY };
    let inverted = best.1 < 0.0;
    let coarse = best.0 * d as i64;
    // ---- refine at the full rate
    let margin = (2 * d + 4) as i64;
    let (fa, fb) = (a.len() as i64, b.len() as i64);
    let ov0 = (-coarse).max(0);
    let ov1 = fb.min(fa - coarse);
    if ov1 - ov0 < 16 {
        return Some(SyncResult { lag: coarse, lag_fine: coarse as f64, confidence, distinct, inverted });
    }
    let w = ((ov1 - ov0) as usize).min(opts.refine_window.max(256)) as i64;
    // the window of b with the most energy (both sides: a lag-shifted)
    let start = {
        let hop = (w / 4).max(1);
        let pre = |x: &[f32]| {
            let mut p = Vec::with_capacity(x.len() + 1);
            p.push(0.0f64);
            let mut s = 0.0;
            for &v in x {
                s += (v as f64) * (v as f64);
                p.push(s);
            }
            p
        };
        let (pa, pb) = (pre(a), pre(b));
        let mut s = ov0;
        let mut best_s = ov0;
        let mut best_e = -1.0f64;
        while s + w <= ov1 {
            let eb = pb[(s + w) as usize] - pb[s as usize];
            let ea = pa[(s + coarse + w) as usize] - pa[(s + coarse) as usize];
            let e = eb.min(ea);
            if e > best_e {
                best_e = e;
                best_s = s;
            }
            s += hop;
        }
        best_s
    };
    let bw: Vec<f32> = b[start as usize..(start + w) as usize].to_vec();
    let aw: Vec<f32> = (start + coarse - margin..start + coarse + w + margin).map(|i| if i >= 0 && i < fa { a[i as usize] } else { 0.0 }).collect();
    let mut aw = aw;
    let mut bw = bw;
    normalise(&mut aw);
    normalise(&mut bw);
    let n2 = (aw.len() + bw.len()).next_power_of_two();
    let r2 = gcc(&aw, &bw, n2, opts.phat_beta, (20.0 / sr as f32, 0.5));
    // r2[L'] = Σ aw[i + L']·bw[i]; L' = margin + δ
    let sign = if inverted { -1.0 } else { 1.0 };
    let mut bl = 0i64;
    let mut bv = f32::MIN;
    for l in 0..=2 * margin {
        let v = corr_at(&r2, l) * sign;
        if v > bv {
            bv = v;
            bl = l;
        }
    }
    let (y0, y1, y2) = (corr_at(&r2, bl - 1) * sign, bv, corr_at(&r2, bl + 1) * sign);
    let den = y0 - 2.0 * y1 + y2;
    let frac = if den.abs() > 1e-20 { (0.5 * (y0 - y2) / den).clamp(-0.5, 0.5) } else { 0.0 };
    let delta = bl - margin;
    let lag = coarse + delta;
    Some(SyncResult { lag, lag_fine: lag as f64 + frac as f64, confidence, distinct, inverted })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::Rng;

    /// Speech-like test signal: voiced syllables (harmonics of a gliding pitch under an envelope),
    /// noise bursts (consonants) and pauses.
    fn speechy(sr: f64, secs: f64, seed: u64) -> Vec<f32> {
        let n = (sr * secs) as usize;
        let mut rng = Rng::new(seed);
        let mut out = vec![0.0f32; n];
        let mut i = 0usize;
        while i < n {
            let len = ((0.08 + 0.25 * (rng.uniform() * 0.5 + 0.5) as f64) * sr) as usize;
            let kind = rng.uniform();
            let f0 = 110.0 + 120.0 * (rng.uniform() as f64 * 0.5 + 0.5);
            let glide = 1.0 + 0.3 * rng.uniform() as f64;
            let amp = 0.2 + 0.3 * (rng.uniform() * 0.5 + 0.5);
            for k in 0..len.min(n - i) {
                let t = k as f64 / sr;
                let env = (std::f64::consts::PI * k as f64 / len as f64).sin() as f32;
                let v = if kind > 0.3 {
                    let f = f0 * (1.0 + (glide - 1.0) * k as f64 / len as f64);
                    (1..8).map(|h| ((2.0 * std::f64::consts::PI * f * h as f64 * t).sin() / h as f64) as f32).sum::<f32>()
                } else if kind > -0.4 {
                    rng.uniform()
                } else {
                    0.0
                };
                out[i + k] = v * env * amp;
            }
            i += len + ((rng.uniform() * 0.5 + 0.5) * 0.15 * sr as f32) as usize;
        }
        out
    }

    /// `x` delayed by `delay` samples (fractional: windowed-sinc interpolation), scaled by `gain`,
    /// optionally coloured by a zero-phase low-pass (`[k, 1-2k, k]`, so the delay stays exact), with
    /// white noise `snr_db` below the signal.
    fn mic(x: &[f32], delay: f64, gain: f32, lowpass: Option<f32>, snr_db: f32, len: usize, seed: u64) -> Vec<f32> {
        let mut out = vec![0.0f32; len];
        let di = delay.floor() as i64;
        let fr = delay - di as f64;
        for (n, o) in out.iter_mut().enumerate() {
            let src = n as i64 - di;
            if fr == 0.0 {
                if src >= 0 && (src as usize) < x.len() {
                    *o = x[src as usize];
                }
            } else {
                let mut acc = 0.0f64;
                for k in -16i64..=16 {
                    let j = src - k;
                    if j < 0 || j as usize >= x.len() {
                        continue;
                    }
                    let t = k as f64 - fr;
                    let s = (std::f64::consts::PI * t).sin() / (std::f64::consts::PI * t);
                    let w = 0.5 + 0.5 * (std::f64::consts::PI * t / 17.0).cos();
                    acc += x[j as usize] as f64 * s * w;
                }
                *o = acc as f32;
            }
        }
        if let Some(k) = lowpass {
            let src = out.clone();
            for i in 1..len.saturating_sub(1) {
                out[i] = k * src[i - 1] + (1.0 - 2.0 * k) * src[i] + k * src[i + 1];
            }
        }
        let rms = crate::testutil::rms(&out) as f32;
        let nrms = rms * 10f32.powf(-snr_db / 20.0) * 3f32.sqrt();
        let mut rng = Rng::new(seed);
        for v in &mut out {
            *v = *v * gain + rng.uniform() * nrms * gain;
        }
        out
    }

    #[test]
    fn decimate_keeps_low_frequencies_and_removes_high_ones() {
        let sr = 48_000.0;
        let low = crate::testutil::sine(300.0, 1.0, sr, 48_000, 0.0);
        let high = crate::testutil::sine(7_000.0, 1.0, sr, 48_000, 0.0);
        let dl = decimate(&low, 6);
        let dh = decimate(&high, 6);
        assert_eq!(dl.len(), 8000);
        let rl = crate::testutil::rms(&dl[100..7900]);
        let rh = crate::testutil::rms(&dh[100..7900]);
        assert!((rl - std::f64::consts::FRAC_1_SQRT_2).abs() < 0.01, "{rl}");
        assert!(rh < 0.01, "{rh}");
    }

    #[test]
    fn pure_delay_exact_both_signs() {
        let sr = 48_000;
        let s = speechy(sr as f64, 12.0, 1);
        for delay in [0i64, 1, 7, 12_345, 96_001, -4_321, -50_000] {
            // b = s shifted: b[n] = s[n + delay]
            let b: Vec<f32> = (0..400_000i64).map(|n| if n + delay >= 0 && ((n + delay) as usize) < s.len() { s[(n + delay) as usize] } else { 0.0 }).collect();
            let r = find_offset(&s, &b, sr, &SyncOptions::default()).unwrap();
            assert_eq!(r.lag, delay, "{r:?}");
            assert!(r.reliable(), "{r:?}");
            assert!(!r.inverted);
        }
    }

    /// Three microphones recording one talker: different gains (−20…+10 dB), noise (SNR 25, 10 and
    /// 3 dB), one coloured by a low-pass, fractional delays. Every offset within one sample.
    #[test]
    fn multi_mic_offsets_within_one_sample() {
        let sr = 48_000u32;
        let src = speechy(sr as f64, 30.0, 7);
        let len = 30 * sr as usize;
        // mic k records src delayed by d_k: mic_k[n] = src[n - d_k]
        let cases: [(f64, f32, Option<f32>, f32); 4] =
            [(0.0, 1.0, None, 40.0), (2_345.0, 0.1, None, 25.0), (-31_337.4, 3.0, Some(0.3), 10.0), (150_000.6, 0.5, None, 3.0)];
        let mics: Vec<Vec<f32>> = cases.iter().enumerate().map(|(i, c)| mic(&src, c.0, c.1, c.2, c.3, len, 100 + i as u64)).collect();
        let mut worst = 0.0f64;
        let mut worst_fine = 0.0f64;
        for (k, c) in cases.iter().enumerate().skip(1) {
            // b = mic_k: b[n] = src[n - d_k] = a[n - d_k + d_0] → lag = d_0 - d_k
            let expect = cases[0].0 - c.0;
            let r = find_offset(&mics[0], &mics[k], sr, &SyncOptions::default()).unwrap();
            let err = (r.lag as f64 - expect).abs();
            let err_fine = (r.lag_fine - expect).abs();
            worst = worst.max(err);
            worst_fine = worst_fine.max(err_fine);
            assert!(err <= 1.0, "mic {k}: lag {} vs {expect} ({r:?})", r.lag);
            assert!(err_fine < 0.75, "mic {k}: fine {} vs {expect}", r.lag_fine);
            assert!(r.reliable(), "{r:?}");
        }
        eprintln!("multi-mic worst error: {worst} samples (integer lag), {worst_fine:.3} samples (sub-sample estimate)");
    }

    /// A room: the far microphone hears the direct sound plus a strong reflection 23 ms later, at
    /// 0 dB SNR. The direct path wins.
    #[test]
    fn echo_and_heavy_noise() {
        let sr = 48_000u32;
        let src = speechy(sr as f64, 20.0, 21);
        let len = src.len();
        let near = mic(&src, 0.0, 1.0, None, 35.0, len, 1);
        let direct = mic(&src, 777.0, 1.0, None, 60.0, len, 2);
        let echo = mic(&src, 777.0 + 0.023 * sr as f64, 0.6, None, 60.0, len, 3);
        let mut rng = Rng::new(9);
        let room: Vec<f32> = direct.iter().zip(&echo).map(|(a, b)| a + b).collect();
        let nrms = crate::testutil::rms(&room) as f32 * 3f32.sqrt();
        let far: Vec<f32> = room.iter().map(|v| (v + rng.uniform() * nrms) * 0.3).collect();
        let r = find_offset(&near, &far, sr, &SyncOptions::default()).unwrap();
        assert!((r.lag + 777).abs() <= 1, "{r:?}");
        assert!(r.reliable(), "{r:?}");
    }

    /// Two 10-minute recordings (run with `--ignored --nocapture` in release for the timing).
    #[test]
    #[ignore]
    fn long_recordings_timing() {
        let sr = 48_000u32;
        let src = speechy(sr as f64, 600.0, 31);
        let a = mic(&src, 0.0, 1.0, None, 20.0, src.len(), 1);
        let b = mic(&src, -1_234_567.0, 0.5, None, 10.0, src.len() - 2_000_000, 2);
        let t = std::time::Instant::now();
        let r = find_offset(&a, &b, sr, &SyncOptions::default()).unwrap();
        eprintln!("10 min + 9.3 min at 48 kHz: {:?} in {:.2} s", r, t.elapsed().as_secs_f64());
        assert!((r.lag - 1_234_567).abs() <= 1);
    }

    #[test]
    fn inverted_polarity_is_found() {
        let sr = 48_000;
        let s = speechy(sr as f64, 8.0, 3);
        let b: Vec<f32> = (0..300_000usize).map(|n| if n + 1000 < s.len() { -0.5 * s[n + 1000] } else { 0.0 }).collect();
        let r = find_offset(&s, &b, sr, &SyncOptions::default()).unwrap();
        assert_eq!(r.lag, 1000);
        assert!(r.inverted);
    }

    #[test]
    fn unrelated_signals_are_not_reliable() {
        let sr = 48_000;
        let a = speechy(sr as f64, 10.0, 11);
        let b = speechy(sr as f64, 10.0, 12);
        let r = find_offset(&a, &b, sr, &SyncOptions::default()).unwrap();
        assert!(!r.reliable(), "{r:?}");
        assert!(find_offset(&a, &[0.0; 48_000], sr, &SyncOptions::default()).is_none());
        assert!(find_offset(&a[..100], &a, sr, &SyncOptions::default()).is_none());
    }

    #[test]
    fn too_short_for_the_decimation_factor_gives_none() {
        // the long signal forces d = 300_064 / 4096 = 74 > the 64-sample one: no estimate
        let long: Vec<f32> = (0..300_000).map(|i| (i as f32 * 0.05).sin()).collect();
        let short: Vec<f32> = (0..64).map(|i| (i as f32 * 0.05).sin()).collect();
        assert!(find_offset(&long, &short, 640, &SyncOptions { max_coarse_samples: 1, ..Default::default() }).is_none());
    }

    #[test]
    fn max_lag_limits_the_search() {
        let sr = 48_000;
        let s = speechy(sr as f64, 10.0, 5);
        let b: Vec<f32> = s[48_000..].to_vec();
        let r = find_offset(&s, &b, sr, &SyncOptions { max_lag: Some(60_000), ..Default::default() }).unwrap();
        assert_eq!(r.lag, 48_000);
        let r = find_offset(&s, &b, sr, &SyncOptions { max_lag: Some(10_000), ..Default::default() }).unwrap();
        assert!(!r.reliable() || r.lag != 48_000);
    }
}
