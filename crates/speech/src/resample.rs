//! Streaming conversion of captured audio to the 16 kHz every recogniser takes.
//!
//! A windowed-sinc interpolator: each output sample is the input around its position weighted by
//! a sinc low-pass (cut off a little below the lower of the two Nyquist frequencies, so nothing
//! above 8 kHz folds back into the speech band) under a Hann window, normalised to unit gain.
//! Input arrives in blocks of any size; the output is the same as converting the whole signal at
//! once. Memory is bounded by the filter's length, whatever the length of the stream.

use std::collections::VecDeque;

use crate::SAMPLE_RATE;

/// Zero crossings of the sinc on either side of a sample, at the output rate.
const ZERO_CROSSINGS: f64 = 8.0;
/// The cut-off as a share of the lower Nyquist frequency.
const CUTOFF: f64 = 0.9;
/// Input rates accepted (a rate outside is treated as the nearest end, so a damaged rate cannot
/// make the output many times longer than the input).
const MIN_RATE: u32 = 8_000;
const MAX_RATE: u32 = 768_000;

/// Converts a mono stream at one rate to [`SAMPLE_RATE`].
pub struct Resampler {
    /// Input samples per output sample.
    step: f64,
    /// The cut-off in cycles per input sample, times two (1.0 = the input's Nyquist frequency).
    cutoff: f64,
    /// Input samples on either side of an output sample's position that carry weight.
    half: i64,
    /// Input samples from absolute index `start` on.
    buf: VecDeque<f32>,
    start: i64,
    /// Absolute input position of the next output sample.
    next: f64,
    passthrough: bool,
}

impl Resampler {
    /// A converter from `from` Hz (0 is taken as 16 kHz).
    pub fn new(from: u32) -> Self {
        let from = if from == 0 { SAMPLE_RATE } else { from.clamp(MIN_RATE, MAX_RATE) };
        let step = f64::from(from) / f64::from(SAMPLE_RATE);
        let cutoff = CUTOFF * (1.0 / step).min(1.0);
        let half = (ZERO_CROSSINGS / cutoff).ceil() as i64;
        Self { step, cutoff, half, buf: VecDeque::new(), start: 0, next: 0.0, passthrough: from == SAMPLE_RATE }
    }

    /// Take `input` and return the output samples it completes.
    pub fn push(&mut self, input: &[f32]) -> Vec<f32> {
        let clean = input.iter().map(|v| if v.is_finite() { *v } else { 0.0 });
        if self.passthrough {
            return clean.collect();
        }
        self.buf.extend(clean);
        let end = self.start + self.buf.len() as i64;
        let mut out = Vec::with_capacity((input.len() as f64 / self.step) as usize + 1);
        // an output needs the input up to `half` samples past its position
        while (self.next.floor() as i64) + self.half < end {
            out.push(self.sample_at(self.next));
            self.next += self.step;
        }
        // keep what the next output still needs
        let keep_from = (self.next.floor() as i64 - self.half).max(self.start);
        let drop = (keep_from - self.start).clamp(0, self.buf.len() as i64) as usize;
        self.buf.drain(..drop);
        self.start += drop as i64;
        out
    }

    fn sample_at(&self, pos: f64) -> f32 {
        let centre = pos.floor() as i64;
        let (mut acc, mut norm) = (0.0f64, 0.0f64);
        for k in centre - self.half + 1..=centre + self.half {
            let x = pos - k as f64;
            let w = self.kernel(x);
            norm += w;
            if k >= self.start
                && let Some(v) = self.buf.get((k - self.start) as usize)
            {
                acc += f64::from(*v) * w;
            }
        }
        if norm.abs() < 1e-12 { 0.0 } else { (acc / norm) as f32 }
    }

    fn kernel(&self, x: f64) -> f64 {
        let span = self.half as f64;
        if x.abs() >= span {
            return 0.0;
        }
        let window = 0.5 + 0.5 * (std::f64::consts::PI * x / span).cos();
        let a = std::f64::consts::PI * self.cutoff * x;
        let sinc = if a.abs() < 1e-9 { 1.0 } else { a.sin() / a };
        self.cutoff * sinc * window
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tone(rate: u32, hz: f64, seconds: f64) -> Vec<f32> {
        (0..(f64::from(rate) * seconds) as usize).map(|i| (0.5 * (2.0 * std::f64::consts::PI * hz * i as f64 / f64::from(rate)).sin()) as f32).collect()
    }

    fn rms(x: &[f32]) -> f32 {
        (x.iter().map(|v| v * v).sum::<f32>() / x.len().max(1) as f32).sqrt()
    }

    fn crossings(x: &[f32]) -> usize {
        x.windows(2).filter(|w| (w[0] < 0.0) != (w[1] < 0.0)).count()
    }

    #[test]
    fn speech_band_tones_keep_their_pitch_and_level_and_tones_above_it_are_removed() {
        let mut r = Resampler::new(48_000);
        let input = tone(48_000, 1000.0, 1.0);
        let out = r.push(&input);
        let len = out.len();
        assert!((15_900..=16_000).contains(&len), "a second in gives about a second out: {len}");
        let body = &out[200..len - 200];
        let hz = crossings(body) as f64 / 2.0 / (body.len() as f64 / 16_000.0);
        assert!((hz - 1000.0).abs() < 10.0, "the pitch is kept: {hz}");
        assert!((rms(body) - rms(&input)).abs() < 0.02, "and the level");
        let mut r = Resampler::new(48_000);
        let high = r.push(&tone(48_000, 12_000.0, 0.5));
        assert!(rms(&high[200..]) < 0.02, "a tone above 8 kHz does not fold back into speech: {}", rms(&high[200..]));
    }

    #[test]
    fn blocks_of_any_size_give_the_same_output_as_the_whole() {
        let input = tone(44_100, 440.0, 0.5);
        let whole = Resampler::new(44_100).push(&input);
        let mut r = Resampler::new(44_100);
        let mut parts = Vec::new();
        for c in input.chunks(97) {
            parts.extend(r.push(c));
        }
        assert_eq!(parts.len(), whole.len());
        assert!(parts.iter().zip(&whole).all(|(a, b)| (a - b).abs() < 1e-6));
        assert!(r.buf.len() < 200, "memory stays bounded by the filter: {}", r.buf.len());
    }

    #[test]
    fn hostile_input_neither_panics_nor_leaks_non_finite_samples() {
        for rate in [0, 1, 7, 8_000, 16_000, 22_050, u32::MAX] {
            let mut r = Resampler::new(rate);
            let out = r.push(&[f32::NAN, f32::INFINITY, -1.0, 1.0, f32::NEG_INFINITY]);
            let more = r.push(&vec![0.25; 4096]);
            assert!(out.iter().chain(&more).all(|v| v.is_finite()), "rate {rate}");
        }
    }
}
