//! Streaming STFT overlap-add framework shared by the spectral effects.

use crate::fft::Fft;
use std::f64::consts::PI;

/// Per-channel streaming buffers. Latency is exactly `n` samples.
#[derive(Clone, Debug)]
pub(crate) struct StftChannel {
    input: Vec<f32>,
    output: Vec<f32>,
    pos: usize,
}

impl StftChannel {
    pub(crate) fn new(n: usize) -> Self {
        StftChannel { input: vec![0.0; n], output: vec![0.0; n], pos: 0 }
    }
    pub(crate) fn reset(&mut self) {
        self.input.iter_mut().for_each(|v| *v = 0.0);
        self.output.iter_mut().for_each(|v| *v = 0.0);
        self.pos = 0;
    }
    /// Push one sample and pop one output sample. Every `hop` samples `frame` is called with the
    /// last `n` input samples and must write an `n`-sample (windowed) frame that is overlap-added.
    #[inline]
    pub(crate) fn tick(&mut self, x: f32, hop: usize, frame: &mut impl FnMut(&[f32], &mut [f32])) -> f32 {
        let n = self.input.len();
        self.input[n - hop + self.pos] = x;
        let y = self.output[self.pos];
        self.pos += 1;
        if self.pos == hop {
            self.pos = 0;
            self.output.copy_within(hop.., 0);
            self.output[n - hop..].iter_mut().for_each(|v| *v = 0.0);
            frame(&self.input, &mut self.output);
            self.input.copy_within(hop.., 0);
        }
        y
    }

    /// `tick` for two channels that frame on the same samples: `frame` gets both inputs and both
    /// outputs.
    #[inline]
    fn tick_pair(
        a: &mut StftChannel,
        b: &mut StftChannel,
        x: (f32, f32),
        hop: usize,
        frame: &mut impl FnMut(&[f32], &[f32], &mut [f32], &mut [f32]),
    ) -> (f32, f32) {
        debug_assert_eq!(a.pos, b.pos);
        let n = a.input.len();
        a.input[n - hop + a.pos] = x.0;
        b.input[n - hop + a.pos] = x.1;
        let y = (a.output[a.pos], b.output[a.pos]);
        a.pos += 1;
        b.pos = a.pos;
        if a.pos == hop {
            a.pos = 0;
            b.pos = 0;
            for c in [&mut *a, &mut *b] {
                c.output.copy_within(hop.., 0);
                c.output[n - hop..].iter_mut().for_each(|v| *v = 0.0);
            }
            frame(&a.input, &b.input, &mut a.output, &mut b.output);
            a.input.copy_within(hop.., 0);
            b.input.copy_within(hop.., 0);
        }
        y
    }
}

/// Hop, gain and FFT pairing shared by every frame of one STFT effect run.
#[derive(Clone, Copy)]
pub(crate) struct Plan {
    pub hop: usize,
    pub scale: f32,
    pub pairing: Pairing,
}

/// Stream `len` samples of every channel through its `StftChannel`, sample-major. At a frame
/// boundary `bins(ch, first, re, im)` edits that channel's half spectrum (bins 0..=n/2);
/// `first` is set for the first channel that frames on this sample. Neighbouring channels that
/// frame on the same samples share one complex FFT (`a + i·b`), except for frames with a
/// non-finite sample, which run per channel so the bad channel cannot contaminate its neighbour.
/// `pairing` says whether the forward FFT is shared too or only the inverse one.
pub(crate) fn stream(
    stft: &mut [StftChannel],
    spec: &mut Spectral,
    channels: &mut [&mut [f32]],
    len: usize,
    plan: Plan,
    bins: &mut impl FnMut(usize, bool, &mut [f32], &mut [f32]),
) {
    let Plan { hop, scale, pairing } = plan;
    let nch = channels.len().min(stft.len());
    let half = spec.re2.len();
    for i in 0..len {
        let mut first = true;
        let mut ch = 0;
        while ch < nch {
            if ch + 1 < nch && stft[ch].pos == stft[ch + 1].pos {
                let (lo, hi) = stft.split_at_mut(ch + 1);
                let x = (channels[ch][i], channels[ch + 1][i]);
                let (ya, yb) = StftChannel::tick_pair(&mut lo[ch], &mut hi[0], x, hop, &mut |a, b, out_a, out_b| {
                    if all_finite(a) && all_finite(b) {
                        match pairing {
                            Pairing::Full => {
                                spec.analyse_pair(a, b);
                                bins(ch, first, &mut spec.re[..half], &mut spec.im[..half]);
                                bins(ch + 1, false, &mut spec.re2, &mut spec.im2);
                            }
                            Pairing::SynthesisOnly => {
                                // Each channel's spectrum is bit-identical to its mono analysis;
                                // the edited half spectra then share one inverse FFT.
                                spec.analyse(a);
                                bins(ch, first, &mut spec.re[..half], &mut spec.im[..half]);
                                spec.re2.copy_from_slice(&spec.re[..half]);
                                spec.im2.copy_from_slice(&spec.im[..half]);
                                spec.analyse(b);
                                bins(ch + 1, false, &mut spec.re[..half], &mut spec.im[..half]);
                                spec.re[..half].swap_with_slice(&mut spec.re2);
                                spec.im[..half].swap_with_slice(&mut spec.im2);
                            }
                        }
                        if spec.pair_finite(half) {
                            spec.synthesise_add_pair(out_a, out_b, scale);
                        } else {
                            // Poisoned per-channel state can leave one edited spectrum
                            // non-finite: synthesise separately so it cannot leak either.
                            spec.synthesise_add_split(out_a, out_b, scale);
                        }
                    } else {
                        // A non-finite sample would leak into the other channel through the
                        // packed FFT: keep the channels isolated for this frame.
                        spec.analyse(a);
                        bins(ch, first, &mut spec.re[..half], &mut spec.im[..half]);
                        spec.synthesise_add(out_a, scale);
                        spec.analyse(b);
                        bins(ch + 1, false, &mut spec.re[..half], &mut spec.im[..half]);
                        spec.synthesise_add(out_b, scale);
                    }
                    first = false;
                });
                channels[ch][i] = ya;
                channels[ch + 1][i] = yb;
                ch += 2;
            } else {
                channels[ch][i] = stft[ch].tick(channels[ch][i], hop, &mut |input, out| {
                    spec.analyse(input);
                    bins(ch, first, &mut spec.re[..half], &mut spec.im[..half]);
                    spec.synthesise_add(out, scale);
                    first = false;
                });
                ch += 1;
            }
        }
    }
}

/// How much of the STFT two paired channels share.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Pairing {
    /// Forward and inverse FFT (`a + i·b`). The analysed spectra differ from the mono ones at
    /// rounding level, so only for effects whose bin edit is continuous in the spectrum.
    Full,
    /// Only the inverse FFT. For effects whose bin edit makes discrete decisions on the spectrum
    /// (peak picking, thresholds): a rounding-level change could flip one, so they get the exact
    /// mono analysis.
    SynthesisOnly,
}

/// Branch-free scan (vectorises, unlike a short-circuiting `all`).
#[inline]
fn all_finite(x: &[f32]) -> bool {
    x.iter().fold(true, |ok, v| ok & v.is_finite())
}

/// Periodic Hann window.
pub(crate) fn hann(n: usize) -> Vec<f32> {
    (0..n).map(|i| (0.5 - 0.5 * (2.0 * PI * i as f64 / n as f64).cos()) as f32).collect()
}

/// FFT size for spectral effects at a sample rate (~43 ms at 48 kHz).
pub(crate) fn fft_size(sample_rate: f32) -> usize {
    if sample_rate <= 50_000.0 {
        2048
    } else if sample_rate <= 100_000.0 {
        4096
    } else {
        8192
    }
}

/// Shared spectral scratch.
#[derive(Clone, Debug)]
pub(crate) struct Spectral {
    pub fft: Fft,
    pub window: Vec<f32>,
    pub re: Vec<f32>,
    pub im: Vec<f32>,
    /// Second channel's half spectrum (bins 0..=n/2) after `analyse_pair`.
    pub re2: Vec<f32>,
    pub im2: Vec<f32>,
}

impl Spectral {
    /// `sqrt_window`: use √Hann for analysis and synthesis (sums to a constant at 75 % overlap);
    /// otherwise Hann for both.
    pub(crate) fn new(n: usize, sqrt_window: bool) -> Self {
        let mut window = hann(n);
        if sqrt_window {
            window.iter_mut().for_each(|w| *w = w.sqrt());
        }
        Spectral { fft: Fft::new(n), window, re: vec![0.0; n], im: vec![0.0; n], re2: vec![0.0; n / 2 + 1], im2: vec![0.0; n / 2 + 1] }
    }
    /// Window `input` into re/im and run the forward FFT.
    pub(crate) fn analyse(&mut self, input: &[f32]) {
        for i in 0..input.len() {
            self.re[i] = input[i] * self.window[i];
            self.im[i] = 0.0;
        }
        self.fft.forward(&mut self.re, &mut self.im);
    }
    /// Enforce Hermitian symmetry from bins 0..=n/2, inverse FFT, window, scale and add to `out`.
    pub(crate) fn synthesise_add(&mut self, out: &mut [f32], scale: f32) {
        let n = self.re.len();
        self.im[0] = 0.0;
        self.im[n / 2] = 0.0;
        for k in 1..n / 2 {
            self.re[n - k] = self.re[k];
            self.im[n - k] = -self.im[k];
        }
        self.fft.inverse(&mut self.re, &mut self.im);
        for i in 0..n {
            out[i] += self.re[i] * self.window[i] * scale;
        }
    }
    /// Window `a` and `b` into one complex FFT (`a + i·b`), then split it: bins 0..=n/2 of
    /// re/im hold the spectrum of `a`, re2/im2 that of `b`.
    pub(crate) fn analyse_pair(&mut self, a: &[f32], b: &[f32]) {
        let n = self.re.len();
        for i in 0..n {
            self.re[i] = a[i] * self.window[i];
            self.im[i] = b[i] * self.window[i];
        }
        self.fft.forward(&mut self.re, &mut self.im);
        // A[k] = (Z[k] + conj Z[n−k]) / 2, B[k] = (Z[k] − conj Z[n−k]) / 2i. Bins above n/2 are
        // only read, so the lower half can be overwritten in place.
        for k in 0..=n / 2 {
            let m = (n - k) % n;
            let (zr, zi, wr, wi) = (self.re[k], self.im[k], self.re[m], self.im[m]);
            self.re[k] = 0.5 * (zr + wr);
            self.im[k] = 0.5 * (zi - wi);
            self.re2[k] = 0.5 * (zi + wi);
            self.im2[k] = 0.5 * (wr - zr);
        }
    }
    /// Whether both half spectra of `analyse_pair` (bins 0..half) are finite.
    pub(crate) fn pair_finite(&self, half: usize) -> bool {
        [&self.re[..half], &self.im[..half], &self.re2[..half], &self.im2[..half]].iter().all(|s| s.iter().fold(true, |ok, v| ok & v.is_finite()))
    }
    /// `synthesise_add_pair` without sharing the inverse FFT: the two spectra stay isolated.
    pub(crate) fn synthesise_add_split(&mut self, out_a: &mut [f32], out_b: &mut [f32], scale: f32) {
        self.synthesise_add(out_a, scale);
        let half = self.re2.len();
        self.re[..half].copy_from_slice(&self.re2);
        self.im[..half].copy_from_slice(&self.im2);
        self.synthesise_add(out_b, scale);
    }
    /// `synthesise_add` for both spectra of `analyse_pair`: one inverse FFT of
    /// Z = Y_a + i·Y_b, whose real part goes to `out_a` and imaginary part to `out_b`.
    pub(crate) fn synthesise_add_pair(&mut self, out_a: &mut [f32], out_b: &mut [f32], scale: f32) {
        let n = self.re.len();
        let h = n / 2;
        self.im[0] = 0.0;
        self.im[h] = 0.0;
        self.im2[0] = 0.0;
        self.im2[h] = 0.0;
        for k in 1..h {
            // Z[n−k] = conj A[k] + i·conj B[k]
            self.re[n - k] = self.re[k] + self.im2[k];
            self.im[n - k] = self.re2[k] - self.im[k];
        }
        for k in 0..=h {
            self.re[k] -= self.im2[k];
            self.im[k] += self.re2[k];
        }
        self.fft.inverse(&mut self.re, &mut self.im);
        for i in 0..n {
            out_a[i] += self.re[i] * self.window[i] * scale;
            out_b[i] += self.im[i] * self.window[i] * scale;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::Rng;

    #[test]
    fn paired_channels_match_separate_ones_to_rounding() {
        let (n, len) = (1024, 20000);
        let hop = n / 4;
        let mut rng = Rng::new(11);
        let src: Vec<Vec<f32>> = (0..3).map(|_| (0..len).map(|_| rng.uniform()).collect()).collect();
        let mut gain = |ch: usize, _: bool, re: &mut [f32], im: &mut [f32]| {
            for k in 0..re.len() {
                let g = 1.0 / (1.0 + (k * (ch + 1)) as f32 / 64.0);
                re[k] *= g;
                im[k] *= g;
            }
        };
        let scale = 1.0 / (n as f32 * 2.0);
        // Three channels: 0 and 1 share an FFT, 2 runs alone.
        let mut stft = vec![StftChannel::new(n); 3];
        let mut spec = Spectral::new(n, true);
        let mut joint = src.clone();
        {
            let mut chans: Vec<&mut [f32]> = joint.iter_mut().map(|c| c.as_mut_slice()).collect();
            stream(&mut stft, &mut spec, &mut chans, len, Plan { hop, scale, pairing: Pairing::Full }, &mut gain);
        }
        for ch in 0..3 {
            let mut single = vec![StftChannel::new(n)];
            let mut y = src[ch].clone();
            stream(&mut single, &mut spec, &mut [&mut y], len, Plan { hop, scale, pairing: Pairing::Full }, &mut |_, first, re, im| gain(ch, first, re, im));
            let peak = y.iter().fold(0.0f32, |a, v| a.max(v.abs()));
            let dev = y.iter().zip(&joint[ch]).fold(0.0f32, |a, (p, q)| a.max((p - q).abs()));
            assert!(peak > 0.1 && dev < 1e-5 * peak, "channel {ch}: deviation {dev}");
            if ch == 2 {
                assert!(y.iter().zip(&joint[ch]).all(|(p, q)| p.to_bits() == q.to_bits()));
            }
        }
    }

    /// Every STFT effect gives each channel of a paired (shared complex FFT) run the output it
    /// would have as a mono signal, to rounding, on finite tone + transient material. Covers
    /// identical channels (the pitch-shifter repro: three channels, +3 semitones, 256-sample
    /// blocks) and channels with different content and level.
    #[test]
    fn paired_effects_match_mono_on_transients() {
        use crate::AudioEffect;
        use crate::effects::{DeNoise, DeReverb, FftFilter, PitchShifter};
        const SR: f32 = 48_000.0;
        let len = 48_000;
        let material = |f: f64, amp: f32, phase: f64, click_every: usize, click_at: usize, noise: f32, seed: u64| -> Vec<f32> {
            let mut rng = Rng::new(seed);
            let mut x: Vec<f32> = crate::testutil::sine(f, amp as f64, SR as f64, len, phase).iter().map(|v| v + noise * rng.uniform()).collect();
            for (i, v) in x.iter_mut().enumerate() {
                if i % click_every == click_at {
                    *v += 0.8 * amp / 0.3;
                }
            }
            x
        };
        let tone = material(440.0, 0.3, 0.0, 6000, 1500, 0.0, 1);
        let identical = vec![tone.clone(), tone.clone(), tone];
        let different =
            vec![material(440.0, 0.3, 0.0, 6000, 1500, 0.0, 1), material(1250.0, 0.05, 0.7, 4410, 333, 0.01, 2), material(97.0, 0.2, 1.3, 9000, 7000, 0.0, 3)];
        let make = |name: &str, ch: usize| -> Box<dyn AudioEffect> {
            let mut fx: Box<dyn AudioEffect> = match name {
                "DeReverb" => Box::new(DeReverb::new(SR, ch)),
                "FftFilter" => Box::new(FftFilter::new(SR, ch)),
                "DeNoise" => Box::new(DeNoise::new(SR, ch)),
                _ => Box::new(PitchShifter::new(SR, ch)),
            };
            if name == "PitchShifter" {
                fx.set_param("semitones", 3.0);
            }
            fx
        };
        let run = |name: &str, src: &[Vec<f32>]| -> Vec<Vec<f32>> {
            let mut fx = make(name, src.len());
            let mut y = src.to_vec();
            for start in (0..len).step_by(256) {
                let end = (start + 256).min(len);
                let mut blk: Vec<&mut [f32]> = y.iter_mut().map(|c| &mut c[start..end]).collect();
                fx.process(&mut blk);
            }
            y
        };
        let mut failures = Vec::new();
        for name in ["PitchShifter", "DeNoise", "DeReverb", "FftFilter"] {
            for (label, src) in [("identical", &identical), ("different", &different)] {
                let joint = run(name, src);
                for (ch, chan) in src.iter().enumerate() {
                    let want = run(name, std::slice::from_ref(chan)).remove(0);
                    let peak = want.iter().fold(0.0f32, |a, v| a.max(v.abs()));
                    let dev = want.iter().zip(&joint[ch]).fold(0.0f32, |a, (p, q)| a.max((p - q).abs()));
                    assert!(joint[ch].iter().all(|v| v.is_finite()) && peak > 0.01, "{name} {label} ch{ch}: peak {peak}");
                    if dev > 1e-5 * peak {
                        failures.push(format!("{name} {label} ch{ch}: max diff {dev} at peak {peak}"));
                    }
                }
            }
        }
        assert!(failures.is_empty(), "{failures:#?}");
    }

    /// DeNoise's noise gate is a hard threshold on the smoothed PSD: on stationary noise many bins
    /// sit near it. With a shared forward FFT a rounding-level change flipped some, and the output
    /// moved by up to ~4e-4 of the peak here. Paired channels must match mono to rounding.
    #[test]
    fn paired_denoise_matches_mono_near_its_threshold() {
        use crate::AudioEffect;
        use crate::effects::DeNoise;
        const SR: f32 = 48_000.0;
        let len = 512 * 940;
        let mut rng = Rng::new(9);
        let a: Vec<f32> = (0..len).map(|_| 0.1 * rng.uniform()).collect();
        let t = crate::testutil::sine(700.0, 0.2, SR as f64, len, 0.0);
        let b: Vec<f32> = (0..len).map(|i| t[i] + 0.03 * rng.uniform()).collect();
        for (thr, red) in [(0.5, 20.0), (2.0, 30.0)] {
            let run = |src: &[&Vec<f32>]| -> Vec<Vec<f32>> {
                let mut fx = DeNoise::new(SR, src.len());
                fx.set_param("threshold", thr);
                fx.set_param("reduction", red);
                let mut y: Vec<Vec<f32>> = src.iter().map(|c| c.to_vec()).collect();
                for s in (0..len).step_by(512) {
                    let mut blk: Vec<&mut [f32]> = y.iter_mut().map(|c| &mut c[s..s + 512]).collect();
                    fx.process(&mut blk);
                }
                y
            };
            let joint = run(&[&a, &b]);
            for (ch, c) in [&a, &b].into_iter().enumerate() {
                let want = run(&[c]).remove(0);
                let peak = want.iter().fold(0.0f32, |m, v| m.max(v.abs()));
                let dev = want.iter().zip(&joint[ch]).fold(0.0f32, |m, (p, q)| m.max((p - q).abs()));
                assert!(peak > 0.05 && dev <= 1e-5 * peak, "threshold {thr} ch{ch}: deviation {dev} (peak {peak})");
            }
        }
    }

    /// A non-finite sample in one channel must not reach its partner through the packed FFT: the
    /// clean channel stays finite and equals what it would be as a mono signal.
    #[test]
    fn non_finite_sample_stays_in_its_channel() {
        use crate::AudioEffect;
        use crate::effects::{DeNoise, DeReverb, FftFilter, PitchShifter};
        const SR: f32 = 48_000.0;
        let len = 24_000;
        let mut rng = Rng::new(5);
        let clean_a: Vec<f32> = crate::testutil::sine(440.0, 0.4, SR as f64, len, 0.0).iter().map(|v| v + 0.05 * rng.uniform()).collect();
        let clean_b: Vec<f32> = crate::testutil::sine(1250.0, 0.4, SR as f64, len, 0.3).iter().map(|v| v + 0.05 * rng.uniform()).collect();
        let make: [(&str, fn() -> Box<dyn AudioEffect>, usize); 4] = [
            ("DeReverb", || Box::new(DeReverb::new(SR, 2)), 1),
            ("FftFilter", || Box::new(FftFilter::new(SR, 2)), 1),
            ("DeNoise", || Box::new(DeNoise::new(SR, 2)), 1),
            ("PitchShifter", || Box::new(PitchShifter::new(SR, 2)), 1),
        ];
        let mono = |name: &str, src: &[f32]| -> Vec<f32> {
            let mut fx: Box<dyn AudioEffect> = match name {
                "DeReverb" => Box::new(DeReverb::new(SR, 1)),
                "FftFilter" => Box::new(FftFilter::new(SR, 1)),
                "DeNoise" => Box::new(DeNoise::new(SR, 1)),
                _ => Box::new(PitchShifter::new(SR, 1)),
            };
            if name == "PitchShifter" {
                fx.set_param("semitones", 3.0);
            }
            let mut y = src.to_vec();
            for blk in y.chunks_mut(512) {
                fx.process(&mut [blk]);
            }
            y
        };
        for (name, ctor, _) in make {
            for bad_left in [true, false] {
                let (mut l, mut r) = (clean_a.clone(), clean_b.clone());
                for (k, bad) in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY].into_iter().enumerate() {
                    if bad_left {
                        l[4000 + 977 * k] = bad;
                    } else {
                        r[4000 + 977 * k] = bad;
                    }
                }
                let mut fx = ctor();
                if name == "PitchShifter" {
                    fx.set_param("semitones", 3.0);
                }
                for (bl, br) in l.chunks_mut(512).zip(r.chunks_mut(512)) {
                    fx.process(&mut [bl, br]);
                }
                let (good, src) = if bad_left { (&r, &clean_b) } else { (&l, &clean_a) };
                let want = mono(name, src);
                let peak = want.iter().fold(0.0f32, |a, v| a.max(v.abs()));
                assert!(
                    good.iter().all(|v| v.is_finite()),
                    "{name} bad_left={bad_left}: clean channel not finite at {:?}",
                    good.iter().position(|v| !v.is_finite())
                );
                let dev = good.iter().zip(&want).fold(0.0f32, |a, (p, q)| a.max((p - q).abs()));
                assert!(peak > 0.05 && dev < 1e-5 * peak, "{name} bad_left={bad_left}: deviation {dev} (peak {peak})");
            }
        }
    }
}
