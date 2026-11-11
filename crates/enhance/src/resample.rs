//! Streaming windowed-sinc sample-rate conversion between the sequence rate and a model's rate.
//!
//! Output sample `k` is the input signal at time `k × from / to − delay` (in input samples), where
//! `delay` is the filter's half width plus an optional fractional extra. The position is an exact
//! rational, so the output does not depend on how the stream is cut into blocks, and the filter
//! never reads ahead of the samples pushed so far. Converting down, the cut-off follows the output
//! rate (anti-aliasing).

use std::collections::VecDeque;

/// Zero crossings of the sinc on each side of the centre, at the cut-off rate.
const ZEROS: f64 = 16.0;
/// Longest filter half width, input samples.
const MAX_HALF: i64 = 1024;

#[derive(Debug, Clone)]
pub(crate) struct Sinc {
    from: u64,
    to: u64,
    cutoff: f64,
    /// Half width of the filter in input samples.
    half: i64,
    delay: f64,
    /// Input samples `[base, base + buf.len())`; earlier ones read as silence.
    buf: VecDeque<f32>,
    base: i64,
    /// Index of the next output sample.
    k: u64,
}

impl Sinc {
    /// `extra` (≥ 0, input samples) is added to the filter's own delay.
    pub(crate) fn new(from: u32, to: u32, extra: f64) -> Self {
        let (from, to) = (u64::from(from.max(1)), u64::from(to.max(1)));
        let cutoff = (to as f64 / from as f64).min(1.0);
        // the width is capped so an absurd ratio cannot ask for an unbounded filter
        let half = ((ZEROS / cutoff).ceil() as i64).min(MAX_HALF);
        let extra = if extra.is_finite() { extra.max(0.0) } else { 0.0 };
        Sinc { from, to, cutoff, half, delay: half as f64 + extra, buf: VecDeque::new(), base: 0, k: 0 }
    }

    /// The filter's own delay in input samples (without `extra`).
    pub(crate) fn half(&self) -> usize {
        self.half as usize
    }

    pub(crate) fn reset(&mut self) {
        self.buf.clear();
        self.base = 0;
        self.k = 0;
    }

    /// The input sample at or just before output `k`, and the fraction beyond it (0 ≤ f < 1).
    fn centre(&self, k: u64) -> (i64, f64) {
        let p = u128::from(k) * u128::from(self.from);
        let to = u128::from(self.to);
        let t = (p % to) as f64 / self.to as f64 - self.delay;
        let fl = t.floor();
        ((p / to) as i64 + fl as i64, t - fl)
    }

    fn kernel(&self, x: f64) -> f64 {
        let h = self.half as f64;
        if x.abs() >= h {
            return 0.0;
        }
        let a = std::f64::consts::PI * self.cutoff * x;
        let sinc = if a.abs() < 1e-9 { 1.0 } else { a.sin() / a };
        sinc * (0.5 + 0.5 * (std::f64::consts::PI * x / h).cos())
    }

    /// Feed `input`; every output sample it completes is appended to `out`.
    pub(crate) fn push(&mut self, input: &[f32], out: &mut impl Extend<f32>) {
        self.buf.extend(input.iter().copied());
        let end = self.base + self.buf.len() as i64;
        loop {
            let (c, frac) = self.centre(self.k);
            if c + self.half >= end {
                // drop what no later output reads
                let keep_from = c - self.half + 1;
                while self.base < keep_from && self.buf.pop_front().is_some() {
                    self.base += 1;
                }
                return;
            }
            let (mut acc, mut norm) = (0.0f64, 0.0f64);
            for j in (c - self.half + 1)..=(c + self.half) {
                let w = self.kernel((j - c) as f64 - frac);
                norm += w;
                if j >= self.base
                    && let Some(&s) = self.buf.get((j - self.base) as usize)
                {
                    acc += f64::from(s) * w;
                }
            }
            out.extend(std::iter::once(if norm.abs() > 1e-9 { (acc / norm) as f32 } else { acc as f32 }));
            self.k += 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sine(rate: u32, hz: f64, n: usize) -> Vec<f32> {
        (0..n).map(|i| (std::f64::consts::TAU * hz * i as f64 / f64::from(rate)).sin() as f32).collect()
    }

    fn convert(from: u32, to: u32, x: &[f32], block: usize) -> Vec<f32> {
        let mut r = Sinc::new(from, to, 0.0);
        let mut y = Vec::new();
        for b in x.chunks(block.max(1)) {
            r.push(b, &mut y);
        }
        y
    }

    #[test]
    fn a_tone_keeps_its_level_and_frequency_and_is_delayed_by_the_half_width() {
        for (from, to) in [(44_100, 48_000), (48_000, 44_100), (96_000, 48_000), (48_000, 96_000), (32_000, 48_000), (48_000, 48_000)] {
            let x = sine(from, 1000.0, from as usize / 2);
            let y = convert(from, to, &x, 333);
            let half = Sinc::new(from, to, 0.0).half() as f64;
            let mut worst = 0f64;
            // skip the filter's warm-up
            for (k, v) in y.iter().enumerate().skip(to as usize / 10) {
                let t = k as f64 * f64::from(from) / f64::from(to) - half;
                let want = (std::f64::consts::TAU * 1000.0 * t / f64::from(from)).sin();
                worst = worst.max((f64::from(*v) - want).abs());
            }
            assert!(worst < 2e-3, "{from} → {to}: error {worst}");
            let expected = (x.len() as f64 * f64::from(to) / f64::from(from)) as i64;
            assert!((y.len() as i64 - expected).abs() <= 2, "{from} → {to}: {} samples, expected about {expected}", y.len());
        }
    }

    #[test]
    fn converting_down_removes_what_the_new_rate_cannot_hold() {
        // 30 kHz at 96 kHz would alias to 18 kHz at 48 kHz
        let x = sine(96_000, 30_000.0, 48_000);
        let y = convert(96_000, 48_000, &x, 512);
        let peak = y.iter().skip(4800).fold(0f32, |m, v| m.max(v.abs()));
        assert!(peak < 0.01, "alias level {peak}");
    }

    #[test]
    fn the_output_does_not_depend_on_the_block_size() {
        let x = sine(44_100, 440.0, 20_000);
        let whole = convert(44_100, 48_000, &x, x.len());
        for block in [1, 7, 256, 4095] {
            assert_eq!(convert(44_100, 48_000, &x, block), whole, "block {block}");
        }
    }

    #[test]
    fn hostile_rates_and_extras_do_not_panic() {
        for (from, to, extra) in [(0, 0, f64::NAN), (1, 384_000, -3.0), (384_000, 8_000, f64::INFINITY), (48_000, 1, 0.5)] {
            let mut r = Sinc::new(from, to, extra);
            let mut y = Vec::new();
            r.push(&[0.5; 8], &mut y);
            r.reset();
            r.push(&[], &mut y);
            assert!(y.iter().all(|v| v.is_finite()));
        }
    }
}
