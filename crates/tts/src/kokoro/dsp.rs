//! The small DSP pieces of the vocoder: a 20-point STFT / inverse STFT (hop 5, periodic Hann
//! window, centred with reflection padding) and a seeded random source, so synthesis is
//! deterministic.

/// FFT size, hop and the number of one-sided bins.
pub(crate) const N_FFT: usize = 20;
pub(crate) const HOP: usize = 5;
pub(crate) const BINS: usize = N_FFT / 2 + 1;

fn hann() -> [f64; N_FFT] {
    let mut w = [0f64; N_FFT];
    for (i, v) in w.iter_mut().enumerate() {
        *v = 0.5 - 0.5 * (std::f64::consts::TAU * i as f64 / N_FFT as f64).cos();
    }
    w
}

/// Reflect index `i` (may be negative or past the end) into `0..n`.
fn reflect(i: isize, n: usize) -> usize {
    if n <= 1 {
        return 0;
    }
    let period = 2 * (n as isize - 1);
    let mut j = i.rem_euclid(period);
    if j >= n as isize {
        j = period - j;
    }
    j as usize
}

/// Magnitude and phase, bin-major: `mag[b * frames + f]`. Frames = `len / HOP + 1`.
pub(crate) fn stft(x: &[f32]) -> (Vec<f32>, Vec<f32>) {
    let n = x.len();
    let frames = n / HOP + 1;
    let w = hann();
    let half = (N_FFT / 2) as isize;
    let mut mag = vec![0f32; BINS * frames];
    let mut ph = vec![0f32; BINS * frames];
    let mut seg = [0f64; N_FFT];
    for f in 0..frames {
        for (k, s) in seg.iter_mut().enumerate() {
            let i = (f * HOP) as isize + k as isize - half;
            *s = f64::from(x.get(reflect(i, n)).copied().unwrap_or(0.0)) * w[k];
        }
        for b in 0..BINS {
            let (mut re, mut im) = (0f64, 0f64);
            for (k, s) in seg.iter().enumerate() {
                let a = -std::f64::consts::TAU * (b * k) as f64 / N_FFT as f64;
                re += s * a.cos();
                im += s * a.sin();
            }
            if let Some(m) = mag.get_mut(b * frames + f) {
                *m = re.hypot(im) as f32;
            }
            if let Some(p) = ph.get_mut(b * frames + f) {
                *p = im.atan2(re) as f32;
            }
        }
    }
    (mag, ph)
}

/// Inverse of [`stft`] from magnitude and phase (bin-major); output length `HOP · (frames − 1)`.
pub(crate) fn istft(mag: &[f32], phase: &[f32]) -> Vec<f32> {
    let frames = mag.len() / BINS;
    if frames == 0 || phase.len() < mag.len() {
        return Vec::new();
    }
    let w = hann();
    let total = N_FFT + HOP * (frames - 1);
    let mut out = vec![0f64; total];
    let mut env = vec![0f64; total];
    let mut frame = [0f64; N_FFT];
    for f in 0..frames {
        // inverse real DFT of the one-sided spectrum
        for (k, v) in frame.iter_mut().enumerate() {
            let mut acc = 0f64;
            for b in 0..BINS {
                let m = f64::from(mag.get(b * frames + f).copied().unwrap_or(0.0));
                let p = f64::from(phase.get(b * frames + f).copied().unwrap_or(0.0));
                let (re, im) = (m * p.cos(), m * p.sin());
                let a = std::f64::consts::TAU * (b * k) as f64 / N_FFT as f64;
                let c = if b == 0 || b == N_FFT / 2 { 1.0 } else { 2.0 };
                acc += c * (re * a.cos() - im * a.sin());
            }
            *v = acc / N_FFT as f64;
        }
        for k in 0..N_FFT {
            let at = f * HOP + k;
            if let (Some(o), Some(e)) = (out.get_mut(at), env.get_mut(at)) {
                *o += frame[k] * w[k];
                *e += w[k] * w[k];
            }
        }
    }
    let start = N_FFT / 2;
    let len = HOP * (frames - 1);
    (start..start + len)
        .map(|i| match (out.get(i), env.get(i)) {
            (Some(o), Some(e)) if *e > 1e-11 => (o / e) as f32,
            (Some(o), _) => *o as f32,
            _ => 0.0,
        })
        .collect()
}

/// Deterministic random numbers (SplitMix64).
pub(crate) struct Rng(u64);

impl Rng {
    pub(crate) fn new(seed: u64) -> Rng {
        Rng(seed ^ 0x9E37_79B9_7F4A_7C15)
    }
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }
    /// Uniform in [0, 1).
    pub(crate) fn uniform(&mut self) -> f64 {
        (self.next() >> 11) as f64 / (1u64 << 53) as f64
    }
    /// Standard normal (Box–Muller).
    pub(crate) fn normal(&mut self) -> f64 {
        let u1 = self.uniform().max(1e-12);
        let u2 = self.uniform();
        (-2.0 * u1.ln()).sqrt() * (std::f64::consts::TAU * u2).cos()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn istft_inverts_stft() {
        let x: Vec<f32> = (0..600).map(|i| ((i as f32) * 0.07).sin() * 0.5 + ((i as f32) * 0.31).cos() * 0.2).collect();
        let (m, p) = stft(&x);
        assert_eq!(m.len(), BINS * (600 / HOP + 1));
        let y = istft(&m, &p);
        assert_eq!(y.len(), 600);
        let err = x.iter().zip(&y).map(|(a, b)| (a - b).abs()).fold(0f32, f32::max);
        assert!(err < 1e-4, "max error {err}");
    }

    #[test]
    fn rng_is_deterministic_and_normalish() {
        let mut a = Rng::new(7);
        let mut b = Rng::new(7);
        let xs: Vec<f64> = (0..20000).map(|_| a.normal()).collect();
        assert!((0..20000).all(|i| xs[i] == b.normal()));
        let mean = xs.iter().sum::<f64>() / xs.len() as f64;
        let var = xs.iter().map(|x| (x - mean).powi(2)).sum::<f64>() / xs.len() as f64;
        assert!(mean.abs() < 0.03 && (var - 1.0).abs() < 0.05, "{mean} {var}");
    }

    #[test]
    fn reflect_pads_like_numpy() {
        assert_eq!((-3..8).map(|i| reflect(i, 5)).collect::<Vec<_>>(), vec![3, 2, 1, 0, 1, 2, 3, 4, 3, 2, 1]);
    }
}
