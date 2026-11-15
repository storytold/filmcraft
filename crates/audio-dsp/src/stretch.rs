//! Time stretch that keeps the pitch: WSOLA (waveform-similarity overlap-add).
//!
//! The output is made of grains two hops long, overlapped by one hop with a raised-cosine window
//! (the falling half of one grain plus the rising half of the next always sums to one). Grain `k`
//! is read from the input near its nominal position (where the speed puts output hop `k`), shifted
//! by at most the tolerance to the position whose first hop best matches the previous grain's
//! natural continuation: the input that would have followed that grain at speed 1. The two halves
//! that are crossfaded are then in phase, so a steady tone keeps its level and its pitch.
//!
//! [`Wsola`] holds the sizes and the window; the caller keeps the chain of grain positions (each
//! one follows from the one before, see [`Wsola::align`]) and reads the input however it is
//! stored. Nothing here allocates per call or keeps state between calls.

/// WSOLA sizes and window for one sample rate.
#[derive(Clone, Debug)]
pub struct Wsola {
    hop: usize,
    tolerance: usize,
    /// The window's rising half, `hop` samples; the falling half is `1 - rise`.
    rise: Vec<f32>,
}

impl Wsola {
    /// Sizes for `sample_rate`: 15 ms hops (30 ms grains) and an 8 ms search either side, which
    /// covers a period of every pitch above 62 Hz.
    pub fn new(sample_rate: u32) -> Self {
        let sr = f64::from(sample_rate.clamp(1_000, 768_000));
        let hop = ((sr * 0.015).round() as usize).max(16);
        let tolerance = ((sr * 0.008).round() as usize).max(4);
        let rise = (0..hop)
            .map(|t| {
                let s = (std::f64::consts::FRAC_PI_2 * (t as f64 + 0.5) / hop as f64).sin();
                (s * s) as f32
            })
            .collect();
        Wsola { hop, tolerance, rise }
    }

    /// Output samples between grains (half a grain).
    pub fn hop(&self) -> usize {
        self.hop
    }

    /// How far (samples) a grain may move from its nominal position.
    pub fn tolerance(&self) -> usize {
        self.tolerance
    }

    /// Where to read the next grain: the position within the tolerance of `nominal` whose first
    /// hop of `guide` best matches the hop at `continuation` (the previous grain's position plus
    /// one hop). Positions index `guide` (a mono mix of the input); candidates whose hop would
    /// leave it are skipped, and `nominal` is returned when there is nothing to match (the
    /// continuation is outside `guide`, or silent). Ties go to the candidate closest to `nominal`.
    pub fn align(&self, guide: &[f32], continuation: i64, nominal: i64) -> i64 {
        let h = self.hop;
        let len = guide.len() as i64;
        let window = |p: i64| -> Option<&[f32]> {
            if p < 0 || p.checked_add(h as i64)? > len {
                return None;
            }
            guide.get(p as usize..p as usize + h)
        };
        let Some(template) = window(continuation) else { return nominal };
        if template.iter().all(|x| x.abs() < 1e-9) {
            return nominal;
        }
        // normalized cross-correlation (the template's own energy is the same for every candidate)
        let score = |p: i64, step: usize| -> Option<f64> {
            let cand = window(p)?;
            let (mut xy, mut yy) = (0.0f64, 0.0f64);
            for (x, y) in template.iter().zip(cand).step_by(step) {
                xy += f64::from(*x) * f64::from(*y);
                yy += f64::from(*y) * f64::from(*y);
            }
            Some(if yy > 1e-18 { xy / yy.sqrt() } else { 0.0 })
        };
        let tol = self.tolerance as i64;
        // coarse pass on every other offset and sample, nearest offsets first so ties keep them;
        // then the neighbours of the best at full resolution
        let consider = |d: i64, step: usize, best: &mut Option<(i64, f64)>| {
            if let Some(s) = score(nominal.saturating_add(d), step)
                && best.is_none_or(|(_, b)| s > b)
            {
                *best = Some((d, s));
            }
        };
        let mut best = None;
        let mut d = 0;
        while d <= tol {
            consider(d, 2, &mut best);
            if d > 0 {
                consider(-d, 2, &mut best);
            }
            d += 2;
        }
        let Some((coarse, _)) = best else { return nominal };
        let mut fine = None;
        for d in [coarse, coarse - 1, coarse + 1] {
            if d.abs() <= tol {
                consider(d, 1, &mut fine);
            }
        }
        nominal.saturating_add(fine.map_or(coarse, |(d, _)| d))
    }

    /// One output hop: the previous grain's falling half (`prev`, the samples one hop into it)
    /// crossfaded into the next grain's rising half (`cur`). Writes `out.len()` samples, at most a
    /// hop; missing input reads as silence.
    pub fn overlap(&self, prev: &[f32], cur: &[f32], out: &mut [f32]) {
        for (t, o) in out.iter_mut().enumerate().take(self.hop) {
            let r = self.rise(t);
            let a = prev.get(t).copied().unwrap_or(0.0);
            let b = cur.get(t).copied().unwrap_or(0.0);
            *o = a * (1.0 - r) + b * r;
        }
    }

    /// The window's weight for the next grain at sample `t` of a hop (the previous grain gets one
    /// minus this).
    pub fn rise(&self, t: usize) -> f32 {
        self.rise.get(t).copied().unwrap_or(1.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `input` stretched to play at `speed` (2.0 = twice as fast) the way a caller chains grains:
    /// hop `k` reads grain `k` near `k * hop * speed`, aligned to the previous grain.
    fn stretch(w: &Wsola, input: &[f32], speed: f64) -> Vec<f32> {
        let h = w.hop() as i64;
        let out_len = (input.len() as f64 / speed) as usize;
        let hops = out_len as i64 / h;
        let mut pos = vec![0i64];
        for k in 1..=hops {
            let nominal = (k as f64 * h as f64 * speed).round() as i64;
            let prev = *pos.last().unwrap();
            pos.push(w.align(input, prev + h, nominal));
        }
        let mut out = vec![0.0f32; (hops as usize) * w.hop()];
        let slice = |p: i64| input.get(p.max(0) as usize..).unwrap_or(&[]);
        for k in 1..=hops as usize {
            let (prev, cur) = (pos[k - 1] + h, pos[k]);
            let o = &mut out[(k - 1) * w.hop()..k * w.hop()];
            w.overlap(slice(prev), slice(cur), o);
        }
        out
    }

    fn sine(freq: f64, sr: f64, n: usize) -> Vec<f32> {
        (0..n).map(|i| (2.0 * std::f64::consts::PI * freq * i as f64 / sr).sin() as f32 * 0.8).collect()
    }

    /// Positive-going zero crossings per second.
    fn frequency(x: &[f32], sr: f64) -> f64 {
        let n = x.windows(2).filter(|w| w[0] < 0.0 && w[1] >= 0.0).count();
        n as f64 * sr / x.len() as f64
    }

    fn rms(x: &[f32]) -> f64 {
        (x.iter().map(|v| f64::from(*v) * f64::from(*v)).sum::<f64>() / x.len().max(1) as f64).sqrt()
    }

    #[test]
    fn the_window_halves_sum_to_one() {
        for sr in [8_000, 44_100, 48_000, 96_000] {
            let w = Wsola::new(sr);
            assert!(w.hop() >= 16 && w.tolerance() >= 4);
            let (ones, mut out) = (vec![1.0; w.hop()], vec![0.0; w.hop()]);
            w.overlap(&ones, &ones, &mut out);
            assert!(out.iter().all(|v| (v - 1.0).abs() < 1e-6), "{sr}");
            assert!(w.rise(0) < 0.01 && w.rise(w.hop() - 1) > 0.99);
        }
    }

    #[test]
    fn a_tone_keeps_its_pitch_and_level_faster_and_slower() {
        let sr = 48_000.0;
        let w = Wsola::new(sr as u32);
        for freq in [110.0, 440.0, 1000.0] {
            let input = sine(freq, sr, 48_000 * 2);
            for speed in [0.5, 0.8, 1.25, 2.0, 3.0] {
                let out = stretch(&w, &input, speed);
                let expect = (input.len() as f64 / speed) as usize;
                assert!(out.len() + w.hop() >= expect && out.len() <= expect, "{freq} Hz × {speed}: {} vs {expect}", out.len());
                let f = frequency(&out[w.hop()..], sr);
                assert!((f - freq).abs() < freq * 0.02 + 2.0, "{freq} Hz × {speed}: {f} Hz");
                // in-phase crossfades: no dips in level, no clicks (a step bigger than the sine's own slope)
                let level = rms(&out[w.hop()..]);
                assert!((level - 0.8 / 2f64.sqrt()).abs() < 0.03, "{freq} Hz × {speed}: rms {level}");
                let slope = 0.8 * 2.0 * std::f64::consts::PI * freq / sr;
                let step = out[w.hop()..].windows(2).map(|p| f64::from((p[1] - p[0]).abs())).fold(0.0, f64::max);
                assert!(step < slope * 1.3, "{freq} Hz × {speed}: step {step} vs slope {slope}");
            }
        }
    }

    #[test]
    fn silence_and_out_of_range_positions_keep_the_nominal_position() {
        let w = Wsola::new(48_000);
        let quiet = vec![0.0f32; 10_000];
        assert_eq!(w.align(&quiet, 2_000, 3_000), 3_000);
        let tone = sine(440.0, 48_000.0, 10_000);
        // the continuation is outside the guide: nothing to match
        for c in [-5, -(w.hop() as i64), 9_999, i64::MAX, i64::MIN] {
            assert_eq!(w.align(&tone, c, 3_000), 3_000);
        }
        // candidates outside the guide are skipped; nothing fits at all: nominal
        assert_eq!(w.align(&tone, 100, i64::MAX), i64::MAX);
        assert_eq!(w.align(&tone, 100, i64::MIN), i64::MIN);
        let p = w.align(&tone, 100, 2);
        assert!((0..=2 + w.tolerance() as i64).contains(&p), "{p}");
        // empty and tiny guides never panic
        assert_eq!(w.align(&[], 0, 7), 7);
        assert_eq!(w.align(&[0.5; 3], 0, 1), 1);
        let mut out = vec![9.0; 4];
        w.overlap(&[], &[], &mut out);
        assert_eq!(out, vec![0.0; 4]);
    }

    #[test]
    fn the_aligned_grain_stays_within_the_tolerance_of_its_nominal_position() {
        let w = Wsola::new(44_100);
        let tone = sine(97.0, 44_100.0, 44_100);
        for nominal in (1_000..30_000).step_by(997) {
            let p = w.align(&tone, nominal - 500, nominal);
            assert!((p - nominal).abs() <= w.tolerance() as i64, "{nominal} → {p}");
        }
    }
}
