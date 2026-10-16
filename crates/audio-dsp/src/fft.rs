//! Minimal in-place radix-2 complex FFT (power-of-two sizes), allocation-free after planning.
//!
//! Kept in-crate (rather than depending on an FFT library) so the crate stays dependency-free;
//! the STFT effects only need a few fixed power-of-two sizes.

use std::f64::consts::PI;

/// A planned FFT of fixed power-of-two size.
#[derive(Clone, Debug)]
pub struct Fft {
    n: usize,
    /// e^{-2πik/n} for k in 0..n/2.
    tw_re: Vec<f32>,
    tw_im: Vec<f32>,
    rev: Vec<u32>,
}

impl Fft {
    /// Plan an FFT of size `n`. A size that is not a power of two ≥ 2 (at most 2^26) is rounded up
    /// to the next one in that range, so a hostile length can neither panic nor allocate wildly.
    pub fn new(n: usize) -> Fft {
        let n = n.clamp(2, 1 << 26).next_power_of_two();
        let bits = n.trailing_zeros();
        let rev = (0..n as u32).map(|i| i.reverse_bits() >> (32 - bits)).collect();
        let (tw_re, tw_im) = (0..n / 2)
            .map(|k| {
                let a = -2.0 * PI * k as f64 / n as f64;
                (a.cos() as f32, a.sin() as f32)
            })
            .unzip();
        Fft { n, tw_re, tw_im, rev }
    }

    pub fn len(&self) -> usize {
        self.n
    }

    pub fn is_empty(&self) -> bool {
        false
    }

    /// Forward transform (e^{-iωt}), unscaled. Buffers shorter than [`Fft::len`] are left untouched.
    pub fn forward(&self, re: &mut [f32], im: &mut [f32]) {
        self.run(re, im, false);
    }

    /// Inverse transform (e^{+iωt}), unscaled (divide by `n` yourself).
    pub fn inverse(&self, re: &mut [f32], im: &mut [f32]) {
        self.run(re, im, true);
    }

    fn run(&self, re: &mut [f32], im: &mut [f32], inverse: bool) {
        let n = self.n;
        if re.len() < n || im.len() < n {
            return;
        }
        for i in 0..n {
            let j = self.rev[i] as usize;
            if j > i {
                re.swap(i, j);
                im.swap(i, j);
            }
        }
        let sign = if inverse { -1.0 } else { 1.0 };
        let mut len = 2;
        while len <= n {
            let half = len / 2;
            let stride = n / len;
            let mut start = 0;
            while start < n {
                for k in 0..half {
                    let wr = self.tw_re[k * stride];
                    let wi = sign * self.tw_im[k * stride];
                    let a = start + k;
                    let b = a + half;
                    let xr = re[b] * wr - im[b] * wi;
                    let xi = re[b] * wi + im[b] * wr;
                    re[b] = re[a] - xr;
                    im[b] = im[a] - xi;
                    re[a] += xr;
                    im[a] += xi;
                }
                start += len;
            }
            len <<= 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_naive_dft_and_round_trips() {
        let n = 64;
        let fft = Fft::new(n);
        let x: Vec<f32> = (0..n).map(|i| ((i * 7 % 13) as f32 - 6.0) / 6.0).collect();
        let mut re = x.clone();
        let mut im = vec![0.0; n];
        fft.forward(&mut re, &mut im);
        for k in 0..n {
            let (mut r, mut i) = (0.0f64, 0.0f64);
            for (t, &v) in x.iter().enumerate() {
                let a = -2.0 * PI * (k * t) as f64 / n as f64;
                r += v as f64 * a.cos();
                i += v as f64 * a.sin();
            }
            assert!((re[k] as f64 - r).abs() < 1e-4, "re {k}");
            assert!((im[k] as f64 - i).abs() < 1e-4, "im {k}");
        }
        fft.inverse(&mut re, &mut im);
        for t in 0..n {
            assert!((re[t] / n as f32 - x[t]).abs() < 1e-5);
            assert!((im[t] / n as f32).abs() < 1e-5);
        }
    }
}
