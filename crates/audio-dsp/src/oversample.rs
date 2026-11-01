//! Polyphase FIR interpolator used for true-peak estimation (BS.1770-4 Annex 2 style).
//!
//! The prototype is a Kaiser-windowed sinc low-pass (cut-off at the original Nyquist) split into
//! `factor` phases of [`TAPS_PER_PHASE`] taps; each phase is normalised to unity DC gain.

use std::f64::consts::PI;

/// Taps per polyphase branch.
pub const TAPS_PER_PHASE: usize = 16;

/// Oversampling factor giving an effective rate of at least 176.4 kHz (4× at 44.1/48 kHz,
/// 2× at 88.2/96 kHz, 1× above), as recommended by BS.1770-4 for true-peak metering.
pub fn true_peak_factor(sample_rate: f64) -> usize {
    if sample_rate < 88_200.0 {
        4
    } else if sample_rate < 176_400.0 {
        2
    } else {
        1
    }
}

fn bessel_i0(x: f64) -> f64 {
    let mut sum = 1.0;
    let mut term = 1.0;
    let q = x * x / 4.0;
    for k in 1..50 {
        term *= q / (k * k) as f64;
        sum += term;
        if term < sum * 1e-17 {
            break;
        }
    }
    sum
}

/// Polyphase coefficient bank shared by all channels.
#[derive(Clone, Debug)]
pub struct PolyphaseBank {
    factor: usize,
    /// `coeffs[p * TAPS_PER_PHASE + j]` multiplies `x[n - j]` for output phase `p`.
    coeffs: Vec<f32>,
}

impl PolyphaseBank {
    pub fn new(factor: usize) -> Self {
        let factor = factor.max(1);
        let len = factor * TAPS_PER_PHASE;
        let centre = (len - 1) as f64 / 2.0;
        let beta = 8.0;
        let i0b = bessel_i0(beta);
        let proto: Vec<f64> = (0..len)
            .map(|m| {
                let t = (m as f64 - centre) / factor as f64;
                let sinc = if t.abs() < 1e-12 { 1.0 } else { (PI * t).sin() / (PI * t) };
                let r = (m as f64 - centre) / (centre + 1.0);
                let w = bessel_i0(beta * (1.0 - r * r).max(0.0).sqrt()) / i0b;
                sinc * w
            })
            .collect();
        let mut coeffs = vec![0.0f32; len];
        for p in 0..factor {
            let sum: f64 = (0..TAPS_PER_PHASE).map(|j| proto[p + factor * j]).sum();
            for j in 0..TAPS_PER_PHASE {
                coeffs[p * TAPS_PER_PHASE + j] = (proto[p + factor * j] / sum) as f32;
            }
        }
        PolyphaseBank { factor, coeffs }
    }
    pub fn factor(&self) -> usize {
        self.factor
    }
    /// Approximate delay (in input samples) between an input sample and the interpolated
    /// values around it.
    pub fn delay(&self) -> usize {
        TAPS_PER_PHASE / 2
    }
}

/// Per-channel interpolator history.
#[derive(Clone, Debug)]
pub struct Interpolator {
    /// Double-written ring so a contiguous window of TAPS_PER_PHASE is always available.
    hist: [f32; 2 * TAPS_PER_PHASE],
    pos: usize,
}

impl Default for Interpolator {
    fn default() -> Self {
        Interpolator { hist: [0.0; 2 * TAPS_PER_PHASE], pos: 0 }
    }
}

impl Interpolator {
    pub fn reset(&mut self) {
        *self = Self::default();
    }

    /// Push one input sample; returns the maximum absolute value of the `factor` interpolated
    /// output samples produced for it.
    #[inline]
    pub fn push_peak(&mut self, bank: &PolyphaseBank, x: f32) -> f32 {
        if bank.factor == 1 {
            return x.abs();
        }
        // Newest sample at window[TAPS-1-0]... store so that window[j] = x[n - j].
        self.pos = if self.pos == 0 { TAPS_PER_PHASE - 1 } else { self.pos - 1 };
        self.hist[self.pos] = x;
        self.hist[self.pos + TAPS_PER_PHASE] = x;
        let win = &self.hist[self.pos..self.pos + TAPS_PER_PHASE];
        let mut peak = 0.0f32;
        for c in bank.coeffs.as_chunks::<TAPS_PER_PHASE>().0 {
            peak = peak.max(dot(c, win).abs());
        }
        peak
    }
}

/// Dot product of one phase with the window. Eight partial sums (instead of one sequential sum)
/// let the compiler vectorise it; the result differs from the sequential sum only by rounding.
#[inline(always)]
fn dot(c: &[f32; TAPS_PER_PHASE], win: &[f32]) -> f32 {
    let mut acc = [0.0f32; 8];
    for (cc, ww) in c.as_chunks::<8>().0.iter().zip(win.as_chunks::<8>().0) {
        for ((a, &ci), &wi) in acc.iter_mut().zip(cc).zip(ww) {
            *a += ci * wi;
        }
    }
    (acc[0] + acc[4]) + (acc[1] + acc[5]) + (acc[2] + acc[6]) + (acc[3] + acc[7])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn phases_have_unity_dc_and_interpolate_sines() {
        let bank = PolyphaseBank::new(4);
        for p in 0..4 {
            let s: f32 = bank.coeffs[p * TAPS_PER_PHASE..(p + 1) * TAPS_PER_PHASE].iter().sum();
            assert!((s - 1.0).abs() < 1e-5);
        }
        // Sine at fs/4 with 45° phase: samples at ±0.707, true peak 1.0.
        let mut ip = Interpolator::default();
        let mut peak = 0.0f32;
        for n in 0..400 {
            let x = (std::f64::consts::FRAC_PI_2 * n as f64 + std::f64::consts::FRAC_PI_4).sin() as f32;
            let p = ip.push_peak(&bank, x);
            if n > 40 {
                peak = peak.max(p);
            }
        }
        assert!((peak - 1.0).abs() < 0.02, "peak {peak}");
    }

    /// The previous sequential-sum interpolator.
    fn push_peak_reference(ip: &mut Interpolator, bank: &PolyphaseBank, x: f32) -> f32 {
        ip.pos = if ip.pos == 0 { TAPS_PER_PHASE - 1 } else { ip.pos - 1 };
        ip.hist[ip.pos] = x;
        ip.hist[ip.pos + TAPS_PER_PHASE] = x;
        let win = &ip.hist[ip.pos..ip.pos + TAPS_PER_PHASE];
        let mut peak = 0.0f32;
        for p in 0..bank.factor {
            let c = &bank.coeffs[p * TAPS_PER_PHASE..(p + 1) * TAPS_PER_PHASE];
            let y: f32 = c.iter().zip(win).map(|(a, b)| a * b).sum();
            peak = peak.max(y.abs());
        }
        peak
    }

    #[test]
    fn partial_sums_match_sequential_sum_to_rounding() {
        let mut rng = crate::testutil::Rng::new(9);
        let mut signals: Vec<Vec<f32>> = vec![(0..48_000).map(|_| rng.uniform()).collect(), (0..48_000).map(|_| 1e-4 * rng.uniform()).collect()];
        for f in [997.0, 11_025.0, 19_000.0, 23_900.0] {
            signals.push(crate::testutil::sine(f, 0.99, 48_000.0, 48_000, 0.3));
        }
        let (mut max_abs, mut max_rel, mut max_db) = (0.0f32, 0.0f32, 0.0f64);
        for factor in [2, 4] {
            let bank = PolyphaseBank::new(factor);
            for x in &signals {
                let (mut a, mut b) = (Interpolator::default(), Interpolator::default());
                let (mut pa, mut pb) = (0.0f32, 0.0f32);
                for &v in x {
                    let (ya, yb) = (a.push_peak(&bank, v), push_peak_reference(&mut b, &bank, v));
                    max_abs = max_abs.max((ya - yb).abs());
                    if yb > 0.0 {
                        max_rel = max_rel.max((ya - yb).abs() / yb);
                    }
                    pa = pa.max(ya);
                    pb = pb.max(yb);
                }
                max_db = max_db.max((20.0 * (pa as f64 / pb as f64).log10()).abs());
            }
        }
        println!("true-peak interpolator vs sequential sum: max abs {max_abs:e}, max rel {max_rel:e}, max peak {max_db:e} dB");
        assert!(max_abs < 1e-6, "abs {max_abs}");
        assert!(max_db < 1e-5, "dB {max_db}");
    }
}
