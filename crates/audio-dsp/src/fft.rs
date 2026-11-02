//! Minimal in-place radix-2 complex FFT (power-of-two sizes), allocation-free after planning.
//!
//! Kept in-crate (rather than depending on an FFT library) so the crate stays dependency-free;
//! the STFT effects only need a few fixed power-of-two sizes.

use std::f64::consts::PI;

/// A planned FFT of fixed power-of-two size.
#[derive(Clone, Debug)]
pub struct Fft {
    n: usize,
    /// Per-stage twiddles, contiguous: the stage with half-length `h` reads e^{-2πik/(2h)} for
    /// k in 0..h at offset `h - 1` (values taken from the n-point table, so they match it exactly).
    tw_re: Vec<f32>,
    tw_im: Vec<f32>,
    /// `tw_im` negated, for the inverse transform.
    tw_im_inv: Vec<f32>,
    rev: Vec<u32>,
}

impl Fft {
    /// Plan an FFT of size `n` (must be a power of two ≥ 2).
    pub fn new(n: usize) -> Fft {
        assert!(n >= 2 && n.is_power_of_two(), "FFT size must be a power of two");
        let bits = n.trailing_zeros();
        let rev = (0..n as u32).map(|i| i.reverse_bits() >> (32 - bits)).collect();
        let (mut tw_re, mut tw_im) = (Vec::with_capacity(n - 1), Vec::with_capacity(n - 1));
        let mut half = 1;
        while half < n {
            let stride = n / (2 * half);
            for k in 0..half {
                let a = -2.0 * PI * (k * stride) as f64 / n as f64;
                tw_re.push(a.cos() as f32);
                tw_im.push(a.sin() as f32);
            }
            half <<= 1;
        }
        let tw_im_inv = tw_im.iter().map(|w| -w).collect();
        Fft { n, tw_re, tw_im, tw_im_inv, rev }
    }

    pub fn len(&self) -> usize {
        self.n
    }

    pub fn is_empty(&self) -> bool {
        false
    }

    /// Forward transform (e^{-iωt}), unscaled.
    pub fn forward(&self, re: &mut [f32], im: &mut [f32]) {
        self.run(re, im, false);
    }

    /// Inverse transform (e^{+iωt}), unscaled (divide by `n` yourself).
    pub fn inverse(&self, re: &mut [f32], im: &mut [f32]) {
        self.run(re, im, true);
    }

    fn run(&self, re: &mut [f32], im: &mut [f32], inverse: bool) {
        let n = self.n;
        assert!(re.len() >= n && im.len() >= n);
        let (re, im) = (&mut re[..n], &mut im[..n]);
        for i in 0..n {
            let j = self.rev[i] as usize;
            if j > i {
                re.swap(i, j);
                im.swap(i, j);
            }
        }
        let tw_im = if inverse { &self.tw_im_inv } else { &self.tw_im };
        // First stage: the twiddle is 1 + i·(∓0). The multiplications by 1 are exact and dropped;
        // the ones by ∓0 stay so signed zeros come out exactly as in the general butterfly.
        let wi = tw_im[0];
        for (r, i) in re.as_chunks_mut::<2>().0.iter_mut().zip(im.as_chunks_mut::<2>().0) {
            let xr = r[1] - i[1] * wi;
            let xi = r[1] * wi + i[1];
            r[1] = r[0] - xr;
            i[1] = i[0] - xi;
            r[0] += xr;
            i[0] += xi;
        }
        let mut half = 2;
        while half < n {
            let wr = &self.tw_re[half - 1..2 * half - 1];
            let wi = &tw_im[half - 1..2 * half - 1];
            for (r, i) in re.chunks_exact_mut(2 * half).zip(im.chunks_exact_mut(2 * half)) {
                let (ra, rb) = r.split_at_mut(half);
                let (ia, ib) = i.split_at_mut(half);
                for k in 0..half {
                    let xr = rb[k] * wr[k] - ib[k] * wi[k];
                    let xi = rb[k] * wi[k] + ib[k] * wr[k];
                    rb[k] = ra[k] - xr;
                    ib[k] = ia[k] - xi;
                    ra[k] += xr;
                    ia[k] += xi;
                }
            }
            half <<= 1;
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

    /// The previous kernel (strided reads from one n/2-entry twiddle table), kept as reference.
    fn run_reference(n: usize, re: &mut [f32], im: &mut [f32], inverse: bool) {
        let rev: Vec<u32> = (0..n as u32).map(|i| i.reverse_bits() >> (32 - n.trailing_zeros())).collect();
        let (tw_re, tw_im): (Vec<f32>, Vec<f32>) = (0..n / 2)
            .map(|k| {
                let a = -2.0 * PI * k as f64 / n as f64;
                (a.cos() as f32, a.sin() as f32)
            })
            .unzip();
        for i in 0..n {
            let j = rev[i] as usize;
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
                    let wr = tw_re[k * stride];
                    let wi = sign * tw_im[k * stride];
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

    #[test]
    fn bit_identical_to_reference_kernel() {
        let mut rng = crate::testutil::Rng::new(17);
        for bits in 1..=13 {
            let n = 1usize << bits;
            let fft = Fft::new(n);
            for case in 0..4 {
                let mut re: Vec<f32> = (0..n).map(|_| rng.uniform()).collect();
                let mut im: Vec<f32> = match case {
                    0 => vec![0.0; n],
                    1 => vec![-0.0; n],
                    _ => (0..n).map(|_| rng.uniform() * 1e3).collect(),
                };
                if case == 3 {
                    // Exact and signed zeros, subnormals and large values.
                    for (i, (r, m)) in re.iter_mut().zip(im.iter_mut()).enumerate() {
                        match i % 5 {
                            0 => (*r, *m) = (0.0, -0.0),
                            1 => (*r, *m) = (-0.0, 0.0),
                            2 => *r = 1e-40,
                            3 => *m = -3e30,
                            _ => {}
                        }
                    }
                }
                for inverse in [false, true] {
                    let (mut r1, mut i1) = (re.clone(), im.clone());
                    let (mut r2, mut i2) = (re.clone(), im.clone());
                    fft.run(&mut r1, &mut i1, inverse);
                    run_reference(n, &mut r2, &mut i2, inverse);
                    for k in 0..n {
                        assert_eq!(r1[k].to_bits(), r2[k].to_bits(), "n {n} case {case} inverse {inverse} re {k}");
                        assert_eq!(i1[k].to_bits(), i2[k].to_bits(), "n {n} case {case} inverse {inverse} im {k}");
                    }
                }
            }
        }
    }
}
