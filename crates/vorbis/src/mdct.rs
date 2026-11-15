//! Forward MDCT of a windowed block of `n` samples to `n/2` coefficients:
//! `X[k] = Σ x[j] cos(2π/n · (j + 1/2 + n/4)(k + 1/2))`, the transform the Vorbis decoder inverts
//! (Vorbis I §1.3.2, §4.3.9). Computed as a DCT-IV of the folded block through an `n/8`-point
//! complex FFT.

use std::f64::consts::PI;

#[derive(Clone, Copy, Default, Debug)]
struct Cpx {
    re: f64,
    im: f64,
}

impl Cpx {
    fn mul(self, o: Cpx) -> Cpx {
        Cpx { re: self.re * o.re - self.im * o.im, im: self.re * o.im + self.im * o.re }
    }
}

/// A forward MDCT for one block size (a power of two, at least 16).
#[derive(Clone, Debug)]
pub struct Mdct {
    n: usize,
    twiddle: Vec<Cpx>,
    post: Vec<Cpx>,
    fft_tw: Vec<Cpx>,
    rev: Vec<usize>,
}

impl Mdct {
    pub fn new(n: usize) -> Mdct {
        let m = n / 2; // DCT-IV length
        let h = m / 2; // FFT length
        let twiddle = (0..h).map(|j| cis(-PI * (4.0 * j as f64 + 1.0) / (4.0 * m as f64))).collect();
        let post = (0..h).map(|k| cis(-PI * k as f64 / m as f64)).collect();
        let fft_tw = (0..h / 2).map(|j| cis(-2.0 * PI * j as f64 / h as f64)).collect();
        let bits = h.trailing_zeros();
        let rev = (0..h).map(|i| if bits == 0 { 0 } else { i.reverse_bits() >> (usize::BITS - bits) }).collect();
        Mdct { n, twiddle, post, fft_tw, rev }
    }

    /// `x`: `n` windowed samples; `out`: `n/2` coefficients (scaled by `scale`).
    pub fn forward(&self, x: &[f32], out: &mut [f32], scale: f64) {
        let n = self.n;
        let m = n / 2;
        let q = n / 4;
        let at = |i: usize| f64::from(x.get(i).copied().unwrap_or(0.0));
        // fold (a, b, c, d) to the DCT-IV input (-c_r - d, a - b_r)
        let mut v = vec![0f64; m];
        for (i, s) in v.iter_mut().enumerate() {
            *s = if i < q { -at(3 * q - 1 - i) - at(3 * q + i) } else { at(i - q) - at(3 * q - 1 - i) };
        }
        // DCT-IV through a half-length complex FFT
        let h = m / 2;
        let mut z: Vec<Cpx> = (0..h).map(|j| Cpx { re: v[2 * j], im: v[m - 1 - 2 * j] }.mul(self.twiddle[j])).collect();
        self.fft(&mut z);
        for (k, c) in z.iter().enumerate() {
            let d = c.mul(self.post[k]);
            if let Some(o) = out.get_mut(2 * k) {
                *o = (d.re * scale) as f32;
            }
            if let Some(o) = out.get_mut(m - 1 - 2 * k) {
                *o = (-d.im * scale) as f32;
            }
        }
    }

    /// In-place radix-2 FFT (`e^{-2πi jk/h}`).
    fn fft(&self, a: &mut [Cpx]) {
        let h = a.len();
        for i in 0..h {
            let j = self.rev[i];
            if i < j {
                a.swap(i, j);
            }
        }
        let mut len = 2;
        while len <= h {
            let step = h / len;
            for start in (0..h).step_by(len) {
                for k in 0..len / 2 {
                    let w = self.fft_tw[k * step];
                    let u = a[start + k];
                    let t = a[start + k + len / 2].mul(w);
                    a[start + k] = Cpx { re: u.re + t.re, im: u.im + t.im };
                    a[start + k + len / 2] = Cpx { re: u.re - t.re, im: u.im - t.im };
                }
            }
            len *= 2;
        }
    }
}

fn cis(t: f64) -> Cpx {
    Cpx { re: t.cos(), im: t.sin() }
}

/// The Vorbis window (§4.3.1) for a block of `n` between blocks of the same size.
pub fn window(n: usize) -> Vec<f32> {
    (0..n)
        .map(|i| {
            let s = (PI * (i as f64 + 0.5) / n as f64).sin();
            (PI / 2.0 * s * s).sin() as f32
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_the_direct_formula() {
        for n in [16usize, 64, 256, 2048] {
            let x: Vec<f32> = (0..n).map(|i| ((i * 7919 % 211) as f32 - 105.0) / 50.0).collect();
            let mut fast = vec![0f32; n / 2];
            Mdct::new(n).forward(&x, &mut fast, 1.0);
            for k in [0usize, 1, 2, n / 4, n / 2 - 1] {
                let direct: f64 = (0..n).map(|j| f64::from(x[j]) * (2.0 * PI / n as f64 * (j as f64 + 0.5 + n as f64 / 4.0) * (k as f64 + 0.5)).cos()).sum();
                assert!((direct - f64::from(fast[k])).abs() < 1e-3 * (1.0 + direct.abs()), "n={n} k={k}: {direct} vs {}", fast[k]);
            }
        }
    }

    #[test]
    fn the_window_is_power_complementary() {
        let w = window(2048);
        for i in 0..1024 {
            let s = w[i] * w[i] + w[i + 1024] * w[i + 1024];
            assert!((s - 1.0).abs() < 1e-5, "{i}: {s}");
        }
    }
}
