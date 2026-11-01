//! Second-order IIR sections: RBJ "Audio EQ Cookbook" designs, transposed direct form II.

use crate::flush;
use std::f64::consts::PI;

/// Filter shape.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum FilterType {
    Peaking,
    LowShelf,
    HighShelf,
    LowPass,
    HighPass,
    Notch,
    BandPass,
    AllPass,
    /// Constant-bandwidth cut: `1 + (g − 1)·BP(s)` — the −3 dB width of the band-pass stays
    /// `f/Q` however deep the cut (`gain_db` ≤ 0), unlike the RBJ peaking filter whose cut
    /// widens with depth. Used by the Notch Filter.
    Cut,
}

impl FilterType {
    /// Order used by the parametric EQ's `type` choice parameter.
    pub const ALL: [FilterType; 7] =
        [FilterType::Peaking, FilterType::LowShelf, FilterType::HighShelf, FilterType::LowPass, FilterType::HighPass, FilterType::Notch, FilterType::BandPass];
    pub fn from_index(i: usize) -> FilterType {
        Self::ALL[i.min(Self::ALL.len() - 1)]
    }
}

/// Normalised biquad coefficients (`a0 == 1`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Coeffs {
    pub b0: f64,
    pub b1: f64,
    pub b2: f64,
    pub a1: f64,
    pub a2: f64,
}

impl Coeffs {
    /// Pass-through.
    pub const IDENTITY: Coeffs = Coeffs { b0: 1.0, b1: 0.0, b2: 0.0, a1: 0.0, a2: 0.0 };

    fn norm(b0: f64, b1: f64, b2: f64, a0: f64, a1: f64, a2: f64) -> Coeffs {
        Coeffs { b0: b0 / a0, b1: b1 / a0, b2: b2 / a0, a1: a1 / a0, a2: a2 / a0 }
    }

    /// RBJ cookbook design. `freq` in Hz (clamped below Nyquist), `q` > 0, `gain_db` used by
    /// peaking and shelving types (shelves use `q` as the shelf "Q", S=1 ≙ Q=1/√2).
    pub fn design(kind: FilterType, sample_rate: f64, freq: f64, q: f64, gain_db: f64) -> Coeffs {
        let f = freq.clamp(1.0, sample_rate * 0.49);
        let q = q.max(1e-3);
        let w0 = 2.0 * PI * f / sample_rate;
        let (sn, cs) = w0.sin_cos();
        let alpha = sn / (2.0 * q);
        let a = 10f64.powf(gain_db / 40.0);
        match kind {
            FilterType::LowPass => Self::norm((1.0 - cs) / 2.0, 1.0 - cs, (1.0 - cs) / 2.0, 1.0 + alpha, -2.0 * cs, 1.0 - alpha),
            FilterType::HighPass => Self::norm((1.0 + cs) / 2.0, -(1.0 + cs), (1.0 + cs) / 2.0, 1.0 + alpha, -2.0 * cs, 1.0 - alpha),
            FilterType::BandPass => Self::norm(alpha, 0.0, -alpha, 1.0 + alpha, -2.0 * cs, 1.0 - alpha),
            FilterType::Notch => Self::norm(1.0, -2.0 * cs, 1.0, 1.0 + alpha, -2.0 * cs, 1.0 - alpha),
            FilterType::AllPass => Self::norm(1.0 - alpha, -2.0 * cs, 1.0 + alpha, 1.0 + alpha, -2.0 * cs, 1.0 - alpha),
            FilterType::Cut => {
                let g = 10f64.powf(gain_db / 20.0);
                Self::norm(1.0 + g * alpha, -2.0 * cs, 1.0 - g * alpha, 1.0 + alpha, -2.0 * cs, 1.0 - alpha)
            }
            FilterType::Peaking => Self::norm(1.0 + alpha * a, -2.0 * cs, 1.0 - alpha * a, 1.0 + alpha / a, -2.0 * cs, 1.0 - alpha / a),
            FilterType::LowShelf => {
                let k = 2.0 * a.sqrt() * alpha;
                Self::norm(
                    a * ((a + 1.0) - (a - 1.0) * cs + k),
                    2.0 * a * ((a - 1.0) - (a + 1.0) * cs),
                    a * ((a + 1.0) - (a - 1.0) * cs - k),
                    (a + 1.0) + (a - 1.0) * cs + k,
                    -2.0 * ((a - 1.0) + (a + 1.0) * cs),
                    (a + 1.0) + (a - 1.0) * cs - k,
                )
            }
            FilterType::HighShelf => {
                let k = 2.0 * a.sqrt() * alpha;
                Self::norm(
                    a * ((a + 1.0) + (a - 1.0) * cs + k),
                    -2.0 * a * ((a - 1.0) + (a + 1.0) * cs),
                    a * ((a + 1.0) + (a - 1.0) * cs - k),
                    (a + 1.0) - (a - 1.0) * cs + k,
                    2.0 * ((a - 1.0) - (a + 1.0) * cs),
                    (a + 1.0) - (a - 1.0) * cs - k,
                )
            }
        }
    }

    /// |H(e^{jω})| at `freq` Hz.
    pub fn magnitude(&self, freq: f64, sample_rate: f64) -> f64 {
        let w = 2.0 * PI * freq / sample_rate;
        let (s1, c1) = w.sin_cos();
        let (s2, c2) = (2.0 * w).sin_cos();
        // H = (b0 + b1 z^-1 + b2 z^-2) / (1 + a1 z^-1 + a2 z^-2), z^-1 = e^{-jω}.
        let nr = self.b0 + self.b1 * c1 + self.b2 * c2;
        let ni = -(self.b1 * s1 + self.b2 * s2);
        let dr = 1.0 + self.a1 * c1 + self.a2 * c2;
        let di = -(self.a1 * s1 + self.a2 * s2);
        ((nr * nr + ni * ni) / (dr * dr + di * di)).sqrt()
    }

    /// Magnitude in dB.
    pub fn magnitude_db(&self, freq: f64, sample_rate: f64) -> f64 {
        20.0 * self.magnitude(freq, sample_rate).log10()
    }
}

/// One biquad section's state (TDF-II), f64 internally.
#[derive(Clone, Copy, Debug, Default)]
pub struct Biquad {
    s1: f64,
    s2: f64,
}

impl Biquad {
    pub const fn new() -> Self {
        Biquad { s1: 0.0, s2: 0.0 }
    }
    #[inline(always)]
    pub fn tick(&mut self, c: &Coeffs, x: f64) -> f64 {
        let y = c.b0 * x + self.s1;
        self.s1 = flush(c.b1 * x - c.a1 * y + self.s2);
        self.s2 = flush(c.b2 * x - c.a2 * y);
        y
    }
    pub fn reset(&mut self) {
        *self = Biquad::new();
    }
    /// Exact zero state, including the sign of zero, for identity-filter fast paths.
    pub(crate) fn is_clear(&self) -> bool {
        self.s1.to_bits() == 0 && self.s2.to_bits() == 0
    }
    /// Filter a buffer in place.
    pub fn process(&mut self, c: &Coeffs, buf: &mut [f32]) {
        for v in buf {
            *v = self.tick(c, *v as f64) as f32;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::*;

    const SR: f64 = 48000.0;

    fn measured_db(c: &Coeffs, freq: f64) -> f64 {
        let n = SR as usize;
        let mut x = sine(freq, 0.5, SR, n, 0.0);
        let mut bq = Biquad::new();
        bq.process(c, &mut x);
        // Skip the transient; measure over an integer number of periods-ish long tail.
        let tail = &x[n / 2..];
        db(tone_amplitude(tail, freq, SR) / 0.5)
    }

    #[test]
    fn peaking_hits_gain_at_centre() {
        let c = Coeffs::design(FilterType::Peaking, SR, 1000.0, 1.0, 6.0);
        assert!((c.magnitude_db(1000.0, SR) - 6.0).abs() < 1e-9);
        assert!(c.magnitude_db(20.0, SR).abs() < 0.05);
        assert!((measured_db(&c, 1000.0) - 6.0).abs() < 0.05);
    }

    #[test]
    fn lowpass_highpass_minus_3db_at_cutoff() {
        let q = std::f64::consts::FRAC_1_SQRT_2;
        let lp = Coeffs::design(FilterType::LowPass, SR, 2000.0, q, 0.0);
        let hp = Coeffs::design(FilterType::HighPass, SR, 2000.0, q, 0.0);
        assert!((lp.magnitude_db(2000.0, SR) + 3.0103).abs() < 0.01);
        assert!((hp.magnitude_db(2000.0, SR) + 3.0103).abs() < 0.01);
        assert!(lp.magnitude_db(100.0, SR).abs() < 0.01);
        assert!(hp.magnitude_db(15000.0, SR).abs() < 0.1);
        // 12 dB/oct slope well past cutoff.
        assert!(lp.magnitude_db(8000.0, SR) < -22.0);
        assert!((measured_db(&lp, 2000.0) + 3.0103).abs() < 0.05);
        assert!((measured_db(&hp, 500.0) - hp.magnitude_db(500.0, SR)).abs() < 0.05);
    }

    #[test]
    fn notch_is_deep_at_centre() {
        let c = Coeffs::design(FilterType::Notch, SR, 60.0, 10.0, 0.0);
        assert!(c.magnitude(60.0, SR) < 1e-6);
        assert!(c.magnitude_db(1000.0, SR).abs() < 0.01);
        assert!(measured_db(&c, 60.0) < -40.0);
    }

    #[test]
    fn shelves_reach_their_gain() {
        let q = std::f64::consts::FRAC_1_SQRT_2;
        let ls = Coeffs::design(FilterType::LowShelf, SR, 200.0, q, 9.0);
        let hs = Coeffs::design(FilterType::HighShelf, SR, 5000.0, q, -9.0);
        assert!((ls.magnitude_db(10.0, SR) - 9.0).abs() < 0.05);
        assert!((ls.magnitude_db(200.0, SR) - 4.5).abs() < 0.01);
        assert!(ls.magnitude_db(10000.0, SR).abs() < 0.05);
        assert!((hs.magnitude_db(23000.0, SR) + 9.0).abs() < 0.1);
        assert!((hs.magnitude_db(5000.0, SR) + 4.5).abs() < 0.01);
        assert!((measured_db(&ls, 40.0) - ls.magnitude_db(40.0, SR)).abs() < 0.05);
        assert!((measured_db(&hs, 12000.0) - hs.magnitude_db(12000.0, SR)).abs() < 0.05);
    }

    #[test]
    fn bandpass_and_allpass() {
        let bp = Coeffs::design(FilterType::BandPass, SR, 1000.0, 2.0, 0.0);
        assert!(bp.magnitude_db(1000.0, SR).abs() < 1e-9);
        let ap = Coeffs::design(FilterType::AllPass, SR, 1000.0, 2.0, 0.0);
        for f in [50.0, 1000.0, 9000.0] {
            assert!(ap.magnitude_db(f, SR).abs() < 1e-9);
        }
    }

    #[test]
    fn state_flushes_to_zero_no_denormals() {
        let c = Coeffs::design(FilterType::LowPass, SR, 100.0, 0.7, 0.0);
        let mut bq = Biquad::new();
        bq.tick(&c, 1.0);
        let mut last = 1.0;
        for _ in 0..2_000_000 {
            last = bq.tick(&c, 0.0);
            assert!(!(last as f32).is_subnormal());
        }
        assert_eq!(last, 0.0);
    }

    // ----------------------------------------------------------------- non-finite input

    fn flush_reference(x: f64) -> f64 {
        if x.abs() < 1e-30 { 0.0 } else { x }
    }

    fn flush32_reference(x: f32) -> f32 {
        if x.abs() < 1e-25 { 0.0 } else { x }
    }

    #[test]
    fn flush_is_unchanged_for_finite_values() {
        let mut rng = Rng::new(21);
        let mut xs = vec![0.0, -0.0, 1e-30, -1e-30, 1e-300, f64::MIN_POSITIVE, 5e-324, f64::MAX, f64::MIN, 1.0, -1.0];
        xs.extend([1e-30f64.next_down(), 1e-30f64.next_up(), (-1e-30f64).next_down(), (-1e-30f64).next_up()]);
        for _ in 0..100_000 {
            let m = rng.uniform() as f64;
            let e = (rng.uniform() * 300.0) as i32;
            xs.push(m * 10f64.powi(e));
        }
        for &x in &xs {
            assert_eq!(crate::flush(x).to_bits(), flush_reference(x).to_bits(), "{x:e}");
            let y = x as f32;
            if y.is_finite() {
                assert_eq!(crate::flush32(y).to_bits(), flush32_reference(y).to_bits(), "{y:e}");
            }
        }
        for y in [1e-25f32, 1e-25f32.next_down(), -1e-25f32, f32::MAX, f32::MIN, 1e-45] {
            assert_eq!(crate::flush32(y).to_bits(), flush32_reference(y).to_bits(), "{y:e}");
        }
        for x in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            assert_eq!(crate::flush(x), 0.0);
            assert_eq!(crate::flush32(x as f32), 0.0);
        }
    }

    /// Run `tick` over noise with one non-finite sample at index 100 and return the output.
    fn run_with_bad_sample(bad: f32, mut tick: impl FnMut(f32) -> f32) -> Vec<f32> {
        let mut rng = Rng::new(4);
        (0..4800)
            .map(|i| {
                let x = if i == 100 { bad } else { 0.5 * rng.uniform() };
                tick(x)
            })
            .collect()
    }

    #[test]
    fn one_non_finite_sample_does_not_poison_filter_state() {
        use crate::effects::util::{AllPass1, DcBlock, OnePole};
        for bad in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
            let c = Coeffs::design(FilterType::Peaking, SR, 1000.0, 1.0, 6.0);
            let mut bq = Biquad::new();
            let mut lp = OnePole::default();
            let mut hp = OnePole::default();
            let a = OnePole::alpha(500.0, SR as f32);
            let mut ap = AllPass1::default();
            let apc = AllPass1::coef(1000.0, SR as f32);
            let mut dc = DcBlock::default();
            let r = DcBlock::r(SR as f32);
            let outputs = [
                ("Biquad", run_with_bad_sample(bad, |x| bq.tick(&c, x as f64) as f32)),
                ("OnePole lp", run_with_bad_sample(bad, |x| lp.lp(a, x))),
                ("OnePole hp", run_with_bad_sample(bad, |x| hp.hp(a, x))),
                ("AllPass1", run_with_bad_sample(bad, |x| ap.tick(apc, x as f64) as f32)),
                ("DcBlock", run_with_bad_sample(bad, |x| dc.tick(r, x))),
            ];
            for (name, y) in outputs {
                assert!(y[..100].iter().all(|v| v.is_finite()), "{name} {bad}");
                assert!(y[102..].iter().all(|v| v.is_finite()), "{name} stays non-finite after {bad}");
            }
        }
    }

    #[test]
    fn effects_recover_after_one_nan_sample() {
        for id in ["parametric_eq", "simple_eq", "graphic_eq_10", "notch_filter"] {
            let mut fx = crate::create_effect(id, SR as f32, 2).unwrap_or_else(|| panic!("{id}"));
            for p in fx.params() {
                if p.unit == crate::Unit::Decibels && p.id != "output" {
                    fx.set_param(p.id, 6.0);
                }
            }
            let mut rng = Rng::new(8);
            let mut l: Vec<f32> = (0..4800).map(|_| 0.5 * rng.uniform()).collect();
            let mut r: Vec<f32> = (0..4800).map(|_| 0.5 * rng.uniform()).collect();
            l[100] = f32::NAN;
            fx.process(&mut [&mut l, &mut r]);
            let mut l: Vec<f32> = (0..4800).map(|_| 0.5 * rng.uniform()).collect();
            let mut r: Vec<f32> = (0..4800).map(|_| 0.5 * rng.uniform()).collect();
            fx.process(&mut [&mut l, &mut r]);
            assert!(l.iter().chain(&r).all(|v| v.is_finite()), "{id} stays NaN after one NaN input sample");
        }
    }
}
