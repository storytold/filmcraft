//! Pitch shifter: phase vocoder with peak-region shifting and identity phase locking.
//!
//! Every STFT frame (8× overlap) is analysed for spectral peaks; each peak's region of influence
//! (down to the magnitude minima between peaks) is moved as a block so that the peak lands on
//! `k·ratio`, preserving the window's main-lobe shape. All bins of a region receive the same phase
//! rotation, accumulated per frame from the peak's instantaneous frequency so the partial's
//! frequency is scaled exactly by `ratio` while the within-lobe phase relations stay coherent
//! (low phasiness). Duration is unchanged. At ratio 1 the output is the input delayed by the
//! frame size.

use super::stft::{Spectral, StftChannel, fft_size};
use crate::{AudioEffect, ParamSpec, ParamValues, Unit, block_len, param_plumbing};
use std::f32::consts::PI;

const OVERLAP: usize = 8;

#[derive(Clone, Debug)]
struct PvState {
    /// Previous frame's analysis spectrum (bins 0..=n/2); its phase is only needed at peaks.
    last_re: Vec<f32>,
    last_im: Vec<f32>,
    /// Accumulated phase rotation per output bin (from the previous frame).
    rot: Vec<f32>,
    new_rot: Vec<f32>,
}

/// Frame scratch shared by all channels.
#[derive(Clone, Debug)]
struct Scratch {
    mag: Vec<f32>,
    out_re: Vec<f32>,
    out_im: Vec<f32>,
    peaks: Vec<usize>,
}

/// Semitone pitch shifter (±12 st, fractional allowed).
pub struct PitchShifter {
    pv: ParamValues,
    n: usize,
    hop: usize,
    spec: Spectral,
    stft: Vec<StftChannel>,
    state: Vec<PvState>,
    scratch: Scratch,
    target_ratio: f32,
    /// Ratio used by the current frame (glides per frame towards the target).
    ratio: f32,
}

/// Wrap a phase to (−π, π].
#[inline]
fn wrap(p: f32) -> f32 {
    p - 2.0 * PI * ((p + PI) / (2.0 * PI)).floor()
}

impl PitchShifter {
    pub const PARAMS: &'static [ParamSpec] = &[ParamSpec::new("semitones", "Pitch", -12.0, 12.0, 0.0, Unit::Semitones)];

    pub fn new(sample_rate: f32, channels: usize) -> Self {
        let n = fft_size(sample_rate);
        let bins = n / 2 + 1;
        let ch = channels.max(1);
        let mut s = PitchShifter {
            pv: ParamValues::new(Self::PARAMS),
            n,
            hop: n / OVERLAP,
            spec: Spectral::new(n, false),
            stft: vec![StftChannel::new(n); ch],
            state: vec![PvState { last_re: vec![0.0; bins], last_im: vec![0.0; bins], rot: vec![0.0; bins], new_rot: vec![0.0; bins] }; ch],
            scratch: Scratch { mag: vec![0.0; bins], out_re: vec![0.0; bins], out_im: vec![0.0; bins], peaks: Vec::with_capacity(bins) },
            target_ratio: 1.0,
            ratio: 1.0,
        };
        s.apply_params(true);
        s
    }

    fn apply_params(&mut self, snap: bool) {
        self.target_ratio = 2f32.powf(self.pv.v("semitones") / 12.0);
        if snap {
            self.ratio = self.target_ratio;
        }
    }
}

/// Process one analysed frame (spectrum in `spec.re/im`) into the shifted spectrum.
fn shift_frame(spec: &mut Spectral, st: &mut PvState, sc: &mut Scratch, ratio: f32, expect: f32) {
    let bins = sc.mag.len();
    let mut max_mag = 0.0f32;
    for k in 0..bins {
        let (re, im) = (spec.re[k], spec.im[k]);
        sc.mag[k] = (re * re + im * im).sqrt();
        max_mag = max_mag.max(sc.mag[k]);
        sc.out_re[k] = 0.0;
        sc.out_im[k] = 0.0;
        st.new_rot[k] = 0.0;
    }
    // Peaks: local maxima over ±2 bins, above −100 dB of the frame maximum.
    sc.peaks.clear();
    let floor = max_mag * 1e-5;
    for k in 0..bins {
        let m = sc.mag[k];
        if m <= floor {
            continue;
        }
        let lo = k.saturating_sub(2);
        let hi = (k + 2).min(bins - 1);
        if (lo..k).all(|j| sc.mag[j] < m) && (k + 1..=hi).all(|j| sc.mag[j] <= m) {
            sc.peaks.push(k);
        }
    }
    let np = sc.peaks.len();
    let mut start = 0usize;
    for i in 0..np {
        let p = sc.peaks[i];
        // Region end: magnitude minimum between this peak and the next.
        let end = if i + 1 < np {
            let q = sc.peaks[i + 1];
            let mut mi = p + 1;
            for j in p + 1..q {
                if sc.mag[j] < sc.mag[mi] {
                    mi = j;
                }
            }
            mi
        } else {
            bins
        };
        // Instantaneous frequency of the peak (bins).
        let phase = spec.im[p].atan2(spec.re[p]);
        let last_phase = st.last_im[p].atan2(st.last_re[p]);
        let d = wrap(phase - last_phase - p as f32 * expect);
        let f = p as f32 + d / expect;
        let target = (p as f32 * ratio).round() as isize;
        let delta = target - p as isize;
        let prev_rot = if (0..bins as isize).contains(&target) { st.rot[target as usize] } else { 0.0 };
        let rot = wrap(prev_rot + (ratio - 1.0) * f * expect);
        // mag·e^{i(φ+rot)} = X·e^{i·rot}: one sin_cos per region instead of one per bin.
        let (s, c) = rot.sin_cos();
        for k in start..end {
            let t = k as isize + delta;
            if t < 0 || t >= bins as isize {
                continue;
            }
            let t = t as usize;
            let (re, im) = (spec.re[k], spec.im[k]);
            sc.out_re[t] += re * c - im * s;
            sc.out_im[t] += re * s + im * c;
            st.new_rot[t] = rot;
        }
        start = end;
    }
    for k in 0..bins {
        st.last_re[k] = spec.re[k];
        st.last_im[k] = spec.im[k];
        spec.re[k] = sc.out_re[k];
        spec.im[k] = sc.out_im[k];
    }
    std::mem::swap(&mut st.rot, &mut st.new_rot);
}

impl AudioEffect for PitchShifter {
    param_plumbing!("pitch_shifter");
    fn latency(&self) -> usize {
        self.n
    }
    fn reset(&mut self) {
        self.stft.iter_mut().for_each(StftChannel::reset);
        for s in &mut self.state {
            s.last_re.iter_mut().for_each(|v| *v = 0.0);
            s.last_im.iter_mut().for_each(|v| *v = 0.0);
            s.rot.iter_mut().for_each(|v| *v = 0.0);
        }
        self.ratio = self.target_ratio;
    }
    fn process(&mut self, channels: &mut [&mut [f32]]) {
        let len = block_len(channels);
        let nch = channels.len().min(self.stft.len());
        let (n, hop) = (self.n, self.hop);
        // Hann² summed over 8× overlap = 3/8 · 8 = 3.
        let scale = 1.0 / (n as f32 * 3.0 / 8.0 * OVERLAP as f32);
        let expect = 2.0 * PI * hop as f32 / n as f32;
        // Sample-major so every channel hits its frame boundary on the same sample and the ratio
        // glides identically for all channels (once per frame).
        for i in 0..len {
            let mut frame_done = false;
            for ch in 0..nch {
                let st = &mut self.state[ch];
                let spec = &mut self.spec;
                let sc = &mut self.scratch;
                let ratio = self.ratio;
                let mut frame = |input: &[f32], out: &mut [f32]| {
                    spec.analyse(input);
                    shift_frame(spec, st, sc, ratio, expect);
                    spec.synthesise_add(out, scale);
                    frame_done = true;
                };
                channels[ch][i] = self.stft[ch].tick(channels[ch][i], hop, &mut frame);
            }
            if frame_done {
                self.ratio += (self.target_ratio - self.ratio) * 0.5;
                if (self.ratio - self.target_ratio).abs() < 1e-5 {
                    self.ratio = self.target_ratio;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::*;

    const SR: f32 = 48000.0;

    fn shifted(semis: f32, f_in: f64) -> (Vec<f32>, usize) {
        let mut p = PitchShifter::new(SR, 2);
        p.set_param("semitones", semis);
        p.reset();
        let n = SR as usize * 2;
        let mut l = sine(f_in, 0.5, SR as f64, n, 0.0);
        let mut r = l.clone();
        p.process(&mut [&mut l, &mut r]);
        assert_eq!(l, r);
        (l, p.latency())
    }

    #[test]
    fn octave_up_and_down() {
        for (semis, f_in, f_out) in [(12.0, 440.0, 880.0), (-12.0, 440.0, 220.0), (7.0, 300.0, 300.0 * 1.498_307)] {
            let (y, lat) = shifted(semis, f_in);
            let tail = &y[lat + 4096..];
            let a_out = tone_amplitude(tail, f_out, SR as f64);
            let a_in = tone_amplitude(tail, f_in, SR as f64);
            assert!(db(a_out / 0.5).abs() < 3.0, "{semis}: level {} dB", db(a_out / 0.5));
            assert!(db(a_in / a_out) < -30.0, "{semis}: residual {} dB", db(a_in / a_out));
            // Output is a clean tone: almost all energy at the target frequency.
            let r = rms(tail);
            assert!((db(a_out / std::f64::consts::SQRT_2 / r)).abs() < 1.0);
        }
    }

    /// The previous per-bin version of `shift_frame` (atan2 and sin_cos for every bin).
    fn shift_frame_reference(re: &mut [f32], im: &mut [f32], last_phase: &mut [f32], rot: &mut [f32], ratio: f32, expect: f32) {
        let bins = last_phase.len();
        let mag: Vec<f32> = (0..bins).map(|k| (re[k] * re[k] + im[k] * im[k]).sqrt()).collect();
        let phase: Vec<f32> = (0..bins).map(|k| im[k].atan2(re[k])).collect();
        let max_mag = mag.iter().fold(0.0f32, |a, &m| a.max(m));
        let (mut out_re, mut out_im, mut new_rot) = (vec![0.0f32; bins], vec![0.0f32; bins], vec![0.0f32; bins]);
        let floor = max_mag * 1e-5;
        let peaks: Vec<usize> = (0..bins)
            .filter(|&k| {
                let m = mag[k];
                m > floor && (k.saturating_sub(2)..k).all(|j| mag[j] < m) && (k + 1..=(k + 2).min(bins - 1)).all(|j| mag[j] <= m)
            })
            .collect();
        let mut start = 0usize;
        for (i, &p) in peaks.iter().enumerate() {
            let end = match peaks.get(i + 1) {
                Some(&q) => (p + 1..q).fold(p + 1, |mi, j| if mag[j] < mag[mi] { j } else { mi }),
                None => bins,
            };
            let d = wrap(phase[p] - last_phase[p] - p as f32 * expect);
            let f = p as f32 + d / expect;
            let target = (p as f32 * ratio).round() as isize;
            let delta = target - p as isize;
            let prev_rot = if (0..bins as isize).contains(&target) { rot[target as usize] } else { 0.0 };
            let r = wrap(prev_rot + (ratio - 1.0) * f * expect);
            for k in start..end {
                let t = k as isize + delta;
                if t < 0 || t >= bins as isize {
                    continue;
                }
                let t = t as usize;
                let (s, c) = (phase[k] + r).sin_cos();
                out_re[t] += mag[k] * c;
                out_im[t] += mag[k] * s;
                new_rot[t] = r;
            }
            start = end;
        }
        last_phase.copy_from_slice(&phase);
        re[..bins].copy_from_slice(&out_re);
        im[..bins].copy_from_slice(&out_im);
        rot.copy_from_slice(&new_rot);
    }

    #[test]
    fn matches_per_bin_reference_to_rounding() {
        let n = fft_size(SR);
        let (bins, hop) = (n / 2 + 1, n / OVERLAP);
        let expect = 2.0 * PI * hop as f32 / n as f32;
        let mut rng = Rng::new(3);
        let tones = [sine(220.0, 0.3, SR as f64, 40 * hop + n, 0.0), sine(1234.5, 0.2, SR as f64, 40 * hop + n, 1.0)];
        let x: Vec<f32> = (0..40 * hop + n).map(|i| tones[0][i] + tones[1][i] + 0.05 * rng.uniform()).collect();
        let mut spec = Spectral::new(n, false);
        let mut st = PvState { last_re: vec![0.0; bins], last_im: vec![0.0; bins], rot: vec![0.0; bins], new_rot: vec![0.0; bins] };
        let mut sc = Scratch { mag: vec![0.0; bins], out_re: vec![0.0; bins], out_im: vec![0.0; bins], peaks: Vec::new() };
        let (mut last_phase, mut rot) = (vec![0.0f32; bins], vec![0.0f32; bins]);
        let (mut ratio, mut worst) = (1.0f32, 0.0f32);
        for f in 0..40 {
            let target = if f < 20 { 2f32.powf(7.0 / 12.0) } else { 2f32.powf(-5.0 / 12.0) };
            ratio += (target - ratio) * 0.5;
            spec.analyse(&x[f * hop..f * hop + n]);
            let (mut re, mut im) = (spec.re.clone(), spec.im.clone());
            shift_frame(&mut spec, &mut st, &mut sc, ratio, expect);
            shift_frame_reference(&mut re, &mut im, &mut last_phase, &mut rot, ratio, expect);
            assert!(st.rot.iter().zip(&rot).all(|(a, b)| a.to_bits() == b.to_bits()), "frame {f}: rotations differ");
            let peak = (0..bins).map(|k| re[k].hypot(im[k])).fold(0.0f32, f32::max);
            for k in 0..bins {
                worst = worst.max((spec.re[k] - re[k]).hypot(spec.im[k] - im[k]) / peak);
            }
        }
        assert!(worst < 1e-5, "relative deviation {worst}");
    }

    #[test]
    fn zero_shift_is_delayed_identity() {
        let (y, lat) = shifted(0.0, 440.0);
        let x = sine(440.0, 0.5, SR as f64, y.len(), 0.0);
        for i in lat + 2048..y.len() {
            assert!((y[i] - x[i - lat]).abs() < 2e-3, "i {i}");
        }
    }
}
