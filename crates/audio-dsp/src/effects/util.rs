//! Small building blocks shared by the effect implementations: delay lines with fractional
//! reads, LFOs, one-pole filters, envelope followers.

use crate::{flush, flush32};
use std::f64::consts::PI;

/// One-pole smoothing coefficient for a time constant in ms (0 → instant).
#[inline]
pub(crate) fn coef(ms: f32, sr: f32) -> f32 {
    if ms <= 0.0 { 0.0 } else { (-1.0 / (ms * 0.001 * sr)).exp() }
}

/// Linear → dB with a −200 dB floor.
#[inline]
pub(crate) fn lin_db(x: f32) -> f32 {
    20.0 * x.max(1e-10).log10()
}

/// Circular delay line with linear-interpolated fractional reads.
#[derive(Clone, Debug)]
pub(crate) struct DelayLine {
    buf: Vec<f32>,
    w: usize,
}

impl DelayLine {
    /// A line that can delay by up to `max` samples.
    pub(crate) fn new(max: usize) -> Self {
        DelayLine { buf: vec![0.0; max + 3], w: 0 }
    }
    pub(crate) fn reset(&mut self) {
        self.buf.iter_mut().for_each(|v| *v = 0.0);
        self.w = 0;
    }
    /// Longest usable delay in samples.
    pub(crate) fn max_delay(&self) -> f32 {
        (self.buf.len() - 3) as f32
    }
    /// Sample written `d` samples ago (`d` ≥ 0; 0 = the sample written by the last `push`).
    #[inline]
    pub(crate) fn read(&self, d: f32) -> f32 {
        let len = self.buf.len();
        let d = d.clamp(0.0, self.max_delay());
        let di = d.floor();
        let frac = d - di;
        // `w < len` and `di <= len - 3`, so one conditional subtract wraps the index.
        let i0 = self.w + len - 1 - di as usize;
        let i0 = if i0 >= len { i0 - len } else { i0 };
        let i1 = if i0 == 0 { len - 1 } else { i0 - 1 };
        self.buf[i0] + (self.buf[i1] - self.buf[i0]) * frac
    }
    /// Integer read (`d` samples ago, 0 = newest).
    #[inline]
    pub(crate) fn tap(&self, d: usize) -> f32 {
        let len = self.buf.len();
        self.buf[(self.w + len - 1 - d.min(len - 1)) % len]
    }
    #[inline]
    pub(crate) fn push(&mut self, x: f32) {
        self.buf[self.w] = flush32(x);
        self.w += 1;
        if self.w == self.buf.len() {
            self.w = 0;
        }
    }
}

/// Phase accumulator LFO (phase in cycles, 0..1).
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct Lfo {
    phase: f64,
}

impl Lfo {
    pub(crate) fn reset(&mut self) {
        self.phase = 0.0;
    }
    /// Current phase plus `offset` (cycles), wrapped to 0..1.
    #[inline]
    pub(crate) fn phase(&self, offset: f64) -> f64 {
        wrap_phase(self.phase + offset)
    }
    #[inline]
    pub(crate) fn advance(&mut self, hz: f32, sr: f32) {
        self.phase += hz as f64 / sr as f64;
        if self.phase >= 1.0 {
            self.phase -= self.phase.floor();
        }
    }
}

/// `p.rem_euclid(1.0)`, without the `fmod` for phase sums in 0..2 (`p - 1.0` is exact there).
#[inline]
pub(crate) fn wrap_phase(p: f64) -> f64 {
    if (0.0..1.0).contains(&p) {
        p
    } else if (1.0..2.0).contains(&p) {
        p - 1.0
    } else {
        p.rem_euclid(1.0)
    }
}

/// Sine of a phase in cycles, mapped to 0..1.
#[inline]
pub(crate) fn uni_sine(phase: f64) -> f32 {
    (0.5 - 0.5 * (2.0 * PI * phase).cos()) as f32
}

/// Triangle of a phase in cycles, 0..1.
#[inline]
pub(crate) fn uni_tri(phase: f64) -> f32 {
    (if phase < 0.5 { 2.0 * phase } else { 2.0 - 2.0 * phase }) as f32
}

/// One-pole low-pass (`y += a (x − y)`), coefficient from a cutoff frequency.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct OnePole {
    y: f64,
}

impl OnePole {
    /// Coefficient for a −3 dB cutoff at `hz` (matched-z).
    pub(crate) fn alpha(hz: f32, sr: f32) -> f64 {
        1.0 - (-2.0 * PI * (hz.clamp(1.0, sr * 0.49) as f64) / sr as f64).exp()
    }
    #[inline]
    pub(crate) fn lp(&mut self, a: f64, x: f32) -> f32 {
        self.y = flush(self.y + a * (x as f64 - self.y));
        self.y as f32
    }
    /// High-pass as the complement of the low-pass.
    #[inline]
    pub(crate) fn hp(&mut self, a: f64, x: f32) -> f32 {
        x - self.lp(a, x)
    }
    pub(crate) fn reset(&mut self) {
        self.y = 0.0;
    }
}

/// DC blocker (`y = x − x₁ + R·y₁`, R ≈ 0.995 at 48 kHz → ~20 Hz).
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct DcBlock {
    x1: f64,
    y1: f64,
}

impl DcBlock {
    pub(crate) fn r(sr: f32) -> f64 {
        1.0 - 2.0 * PI * 20.0 / sr as f64
    }
    #[inline]
    pub(crate) fn tick(&mut self, r: f64, x: f32) -> f32 {
        let y = x as f64 - self.x1 + r * self.y1;
        self.x1 = x as f64;
        self.y1 = flush(y);
        y as f32
    }
    pub(crate) fn reset(&mut self) {
        *self = DcBlock::default();
    }
}

/// First-order all-pass section `H(z) = (a + z⁻¹) / (1 + a z⁻¹)`.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct AllPass1 {
    x1: f64,
    y1: f64,
}

impl AllPass1 {
    /// Coefficient placing the 90° phase point at `hz`.
    pub(crate) fn coef(hz: f32, sr: f32) -> f64 {
        let t = (PI * (hz.clamp(1.0, sr * 0.49) as f64) / sr as f64).tan();
        (t - 1.0) / (t + 1.0)
    }
    #[inline]
    pub(crate) fn tick(&mut self, a: f64, x: f64) -> f64 {
        let y = a * x + self.x1 - a * self.y1;
        self.x1 = x;
        self.y1 = flush(y);
        y
    }
    pub(crate) fn reset(&mut self) {
        *self = AllPass1::default();
    }
}

/// Peak envelope follower with separate attack / release coefficients (linear domain).
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct Envelope {
    pub(crate) env: f32,
}

impl Envelope {
    #[inline]
    pub(crate) fn tick(&mut self, att: f32, rel: f32, x: f32) -> f32 {
        let c = if x > self.env { att } else { rel };
        self.env = c * self.env + (1.0 - c) * x;
        if self.env < 1e-20 {
            self.env = 0.0;
        }
        self.env
    }
}

/// Tiny deterministic PRNG (xorshift64*) for generated impulse responses.
#[derive(Clone, Debug)]
pub(crate) struct Prng(u64);

impl Prng {
    pub(crate) fn new(seed: u64) -> Self {
        Prng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1)
    }
    pub(crate) fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
    /// Uniform in [-1, 1).
    pub(crate) fn uniform(&mut self) -> f32 {
        ((self.next_u64() >> 40) as f32 / (1u64 << 24) as f32) * 2.0 - 1.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn delay_line_reads_integer_and_fractional() {
        let mut d = DelayLine::new(8);
        for i in 0..5 {
            d.push(i as f32);
        }
        assert_eq!(d.read(0.0), 4.0);
        assert_eq!(d.tap(2), 2.0);
        assert!((d.read(1.5) - 2.5).abs() < 1e-6);
    }

    #[test]
    fn delay_read_matches_modulo_indexing() {
        let reference = |line: &DelayLine, d: f32| {
            let len = line.buf.len();
            let d = d.clamp(0.0, line.max_delay());
            let di = d.floor();
            let frac = d - di;
            let i0 = (line.w + len - 1 - di as usize) % len;
            let i1 = (i0 + len - 1) % len;
            line.buf[i0] + (line.buf[i1] - line.buf[i0]) * frac
        };
        for max in [0, 1, 2, 7] {
            let mut line = DelayLine::new(max);
            for k in 0..3 * (max + 3) {
                line.push(k as f32 * 0.37 - 1.0);
                for d in (-8..=4 * max as i32 + 12).map(|i| i as f32 * 0.25).chain([f32::NAN, f32::INFINITY, -f32::INFINITY, 1e30]) {
                    assert_eq!(line.read(d).to_bits(), reference(&line, d).to_bits(), "max {max} w {} d {d}", line.w);
                }
            }
        }
    }

    #[test]
    fn wrap_phase_matches_rem_euclid() {
        let edges = [0.0, -0.0, 1.0, 2.0, 3.0, -1.0, 0.5, 1.5, 1e300, f64::MIN_POSITIVE, f64::INFINITY, f64::NEG_INFINITY, f64::NAN];
        let near = edges.iter().flat_map(|&p: &f64| [p, f64::from_bits(p.to_bits().wrapping_add(1)), f64::from_bits(p.to_bits().wrapping_sub(1))]);
        let mut r = Prng::new(7);
        let sums = (0..100_000).map(|_| {
            let mut lfo = Lfo::default();
            lfo.advance((r.uniform() + 1.0) * 24000.0, 48000.0);
            lfo.phase + (r.uniform() as f64 + 1.0) * 1.5
        });
        for p in near.chain(sums) {
            assert_eq!(wrap_phase(p).to_bits(), p.rem_euclid(1.0).to_bits(), "{p:e}");
        }
    }

    #[test]
    fn allpass_is_unit_magnitude() {
        let a = AllPass1::coef(1000.0, 48000.0);
        let mut ap = AllPass1::default();
        let mut e_in = 0.0;
        let mut e_out = 0.0;
        let mut r = Prng::new(3);
        for _ in 0..48000 {
            let x = r.uniform() as f64;
            let y = ap.tick(a, x);
            e_in += x * x;
            e_out += y * y;
        }
        assert!((e_out / e_in - 1.0).abs() < 0.01);
    }
}
