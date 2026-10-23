//! Minimal in-place radix-2 complex FFT (power-of-two sizes), allocation-free after planning.
//!
//! Kept in-crate (rather than depending on an FFT library) so the crate stays dependency-free;
//! the STFT effects only need a few fixed power-of-two sizes.

use std::f64::consts::PI;
use std::sync::atomic::{AtomicBool, Ordering};

/// Largest transform `Fft::new` plans (2^26 points). Callers that derive a size from file data should
/// check it against this and give up, rather than rely on the clamp in [`Fft::new`].
pub const MAX_LEN: usize = 1 << 26;

/// Set once a short buffer has been reported, so a misused FFT in an audio callback logs once, not per block.
static SHORT_BUFFER_REPORTED: AtomicBool = AtomicBool::new(false);

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
    /// Plan an FFT of size `n`, which must be a power of two in `2..=`[`MAX_LEN`]. FilmCraft never
    /// panics on bad input, so any other size is not rejected with an assertion: it is logged as an
    /// error (it is a bug in the caller, which must check sizes that come from file data first, as
    /// `sync` and `remix` do) and rounded up to a power of two and clamped into that range so the allocation stays bounded.
    pub fn new(n: usize) -> Fft {
        let planned = n.clamp(2, MAX_LEN).next_power_of_two();
        if planned != n {
            log::error!("FFT size {n} is not a power of two in 2..={MAX_LEN}; planning {planned} points instead");
        }
        let n = planned;
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

    /// Forward transform (e^{-iωt}), unscaled. Buffers shorter than [`Fft::len`] are left untouched
    /// (and the first time that happens is logged as an error: the caller sized them wrongly).
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
            if !SHORT_BUFFER_REPORTED.swap(true, Ordering::Relaxed) {
                log::error!("FFT of {n} points given buffers of {} and {} samples; left untouched", re.len(), im.len());
            }
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

    #[test]
    fn valid_sizes_are_planned_as_asked() {
        for n in [2usize, 4, 256, 2048, 1 << 16] {
            assert_eq!(Fft::new(n).len(), n);
        }
    }

    #[test]
    fn invalid_sizes_are_rounded_into_range_instead_of_panicking() {
        assert_eq!(Fft::new(0).len(), 2);
        assert_eq!(Fft::new(1).len(), 2);
        assert_eq!(Fft::new(1000).len(), 1024);
        assert_eq!(Fft::new(usize::MAX).len(), MAX_LEN);
    }

    #[test]
    fn short_buffers_are_left_untouched() {
        let fft = Fft::new(8);
        let (mut re, mut im) = (vec![1.0f32; 4], vec![2.0f32; 8]);
        fft.forward(&mut re, &mut im);
        assert_eq!((re, im), (vec![1.0; 4], vec![2.0; 8]));
    }
}
