//! Restoration: DeHum (harmonic notch comb) and DeNoise (STFT spectral gating).

use super::stft::{Spectral, StftChannel, fft_size, stream};
use crate::biquad::{Biquad, Coeffs, FilterType};
use crate::{AudioEffect, ParamSpec, ParamValues, Smoothed, Unit, block_len, db_to_gain, param_plumbing};

const MAX_HARMONICS: usize = 10;

/// Removes mains hum: a cascade of notches at the fundamental (50/60 Hz) and its harmonics.
pub struct DeHum {
    pv: ParamValues,
    sr: f32,
    coeffs: [Coeffs; MAX_HARMONICS],
    /// Per-stage enable cross-fade (so changing the harmonic count is click-free).
    stage_mix: [Smoothed; MAX_HARMONICS],
    state: Vec<[Biquad; MAX_HARMONICS]>,
    amount: Smoothed,
}

impl DeHum {
    pub const PARAMS: &'static [ParamSpec] = &[
        ParamSpec::choice("frequency", "Frequency", &["50 Hz", "60 Hz"], 0),
        ParamSpec::new("harmonics", "Harmonics", 1.0, MAX_HARMONICS as f32, 5.0, Unit::None),
        ParamSpec::log("q", "Notch Q", 1.0, 100.0, 30.0, Unit::Q),
        ParamSpec::new("amount", "Amount", 0.0, 100.0, 100.0, Unit::Percent),
    ];

    pub fn new(sample_rate: f32, channels: usize) -> Self {
        let mut s = DeHum {
            pv: ParamValues::new(Self::PARAMS),
            sr: sample_rate,
            coeffs: [Coeffs::IDENTITY; MAX_HARMONICS],
            stage_mix: [Smoothed::with_ms(0.0, sample_rate, 20.0); MAX_HARMONICS],
            state: vec![[Biquad::new(); MAX_HARMONICS]; channels.max(1)],
            amount: Smoothed::with_ms(1.0, sample_rate, 20.0),
        };
        s.apply_params(true);
        s
    }

    fn apply_params(&mut self, snap: bool) {
        let f0 = if self.pv.idx("frequency") == 1 { 60.0 } else { 50.0 };
        let h = self.pv.v("harmonics").round() as usize;
        let q = self.pv.v("q") as f64;
        for k in 0..MAX_HARMONICS {
            let f = f0 * (k + 1) as f64;
            let active = k < h && f < self.sr as f64 * 0.45;
            self.coeffs[k] = Coeffs::design(FilterType::Notch, self.sr as f64, f, q, 0.0);
            self.stage_mix[k].set(if active { 1.0 } else { 0.0 });
            if snap {
                self.stage_mix[k].snap();
            }
        }
        self.amount.set(self.pv.v("amount") / 100.0);
        if snap {
            self.amount.snap();
        }
    }
}

impl AudioEffect for DeHum {
    param_plumbing!("dehum");
    fn reset(&mut self) {
        self.state.iter_mut().for_each(|s| s.iter_mut().for_each(Biquad::reset));
        self.stage_mix.iter_mut().for_each(Smoothed::snap);
        self.amount.snap();
    }
    fn process(&mut self, channels: &mut [&mut [f32]]) {
        let n = block_len(channels);
        let nch = channels.len().min(self.state.len());
        for i in 0..n {
            let mut mix = [0.0f64; MAX_HARMONICS];
            for (m, s) in mix.iter_mut().zip(self.stage_mix.iter_mut()) {
                *m = s.tick() as f64;
            }
            let amt = self.amount.tick() as f64;
            for ch in 0..nch {
                let dry = channels[ch][i] as f64;
                let mut x = dry;
                for k in 0..MAX_HARMONICS {
                    let st = &mut self.state[ch][k];
                    if mix[k] > 0.0 {
                        let y = st.tick(&self.coeffs[k], x);
                        x += mix[k] * (y - x);
                    } else {
                        st.reset();
                    }
                }
                channels[ch][i] = (dry + amt * (x - dry)) as f32;
            }
        }
    }
}

// ---------------------------------------------------------------------------------------------

/// Minimum-statistics sub-windows.
const MS_SUBWINDOWS: usize = 8;
/// Length of the minimum-statistics search window (seconds).
const MS_WINDOW_S: f32 = 1.5;
/// Bias compensation of the minimum of smoothed periodograms (≈ +4 dB).
const MS_BIAS: f32 = 2.5;
const PSD_SMOOTH: f32 = 0.8;

#[derive(Clone, Debug)]
struct NoiseState {
    psd: Vec<f32>,
    cur_min: Vec<f32>,
    ring: Vec<f32>,
    ring_pos: usize,
    frames_in_sub: usize,
    gains: Vec<f32>,
    started: bool,
}

impl NoiseState {
    fn new(bins: usize) -> Self {
        NoiseState {
            psd: vec![0.0; bins],
            cur_min: vec![f32::INFINITY; bins],
            ring: vec![f32::INFINITY; bins * MS_SUBWINDOWS],
            ring_pos: 0,
            frames_in_sub: 0,
            gains: vec![1.0; bins],
            started: false,
        }
    }
    fn reset(&mut self) {
        self.psd.iter_mut().for_each(|v| *v = 0.0);
        self.cur_min.iter_mut().for_each(|v| *v = f32::INFINITY);
        self.ring.iter_mut().for_each(|v| *v = f32::INFINITY);
        self.gains.iter_mut().for_each(|v| *v = 1.0);
        self.ring_pos = 0;
        self.frames_in_sub = 0;
        self.started = false;
    }
}

/// Broadband noise reduction by spectral gating: the noise floor of every STFT bin is tracked
/// continuously with minimum statistics (so no "learn" step is needed), and bins that do not
/// rise above `floor × threshold` are attenuated by `reduction`, with per-bin attack/release
/// and light frequency smoothing to avoid musical noise.
pub struct DeNoise {
    pv: ParamValues,
    n: usize,
    hop: usize,
    sub_frames: usize,
    spec: Spectral,
    stft: Vec<StftChannel>,
    noise: Vec<NoiseState>,
    smoothed_gains: Vec<f32>,
    floor: f32,
    thr: f32,
    release: f32,
}

impl DeNoise {
    pub const PARAMS: &'static [ParamSpec] = &[
        ParamSpec::new("reduction", "Reduction", 0.0, 40.0, 12.0, Unit::Decibels),
        ParamSpec::new("threshold", "Threshold", 0.0, 20.0, 6.0, Unit::Decibels),
        ParamSpec::new("smoothing", "Smoothing", 0.0, 100.0, 50.0, Unit::Percent),
    ];

    pub fn new(sample_rate: f32, channels: usize) -> Self {
        let n = fft_size(sample_rate);
        let hop = n / 4;
        let bins = n / 2 + 1;
        let sub_frames = ((MS_WINDOW_S * sample_rate / hop as f32 / MS_SUBWINDOWS as f32).round() as usize).max(1);
        let ch = channels.max(1);
        let mut s = DeNoise {
            pv: ParamValues::new(Self::PARAMS),
            n,
            hop,
            sub_frames,
            spec: Spectral::new(n, true),
            stft: vec![StftChannel::new(n); ch],
            noise: vec![NoiseState::new(bins); ch],
            smoothed_gains: vec![1.0; bins],
            floor: 1.0,
            thr: 1.0,
            release: 0.5,
        };
        s.apply_params(true);
        s
    }

    fn apply_params(&mut self, _snap: bool) {
        self.floor = db_to_gain(-self.pv.v("reduction"));
        self.thr = 10f32.powf(self.pv.v("threshold") / 10.0);
        // 0 % → fast (0.6 per frame), 100 % → slow (0.03 per frame).
        self.release = 0.6 - 0.57 * self.pv.v("smoothing") / 100.0;
    }
}

impl AudioEffect for DeNoise {
    param_plumbing!("denoise");
    fn latency(&self) -> usize {
        self.n
    }
    fn reset(&mut self) {
        self.stft.iter_mut().for_each(StftChannel::reset);
        self.noise.iter_mut().for_each(NoiseState::reset);
    }
    fn process(&mut self, channels: &mut [&mut [f32]]) {
        let len = block_len(channels);
        let (n, hop, sub_frames) = (self.n, self.hop, self.sub_frames);
        let (floor, thr, release) = (self.floor, self.thr, self.release);
        let scale = 1.0 / (n as f32 * 2.0);
        let (states, sg) = (&mut self.noise, &mut self.smoothed_gains);
        stream(&mut self.stft, &mut self.spec, channels, len, hop, scale, &mut |ch, _, re, im| {
            let st = &mut states[ch];
            let bins = n / 2 + 1;
            for k in 0..bins {
                let p = re[k] * re[k] + im[k] * im[k];
                st.psd[k] = if st.started { PSD_SMOOTH * st.psd[k] + (1.0 - PSD_SMOOTH) * p } else { p };
                st.cur_min[k] = st.cur_min[k].min(st.psd[k]);
            }
            st.started = true;
            st.frames_in_sub += 1;
            let rotate = st.frames_in_sub == sub_frames;
            for k in 0..bins {
                let mut m = st.cur_min[k];
                for u in 0..MS_SUBWINDOWS {
                    m = m.min(st.ring[u * bins + k]);
                }
                let noise = m * MS_BIAS;
                let target = if st.psd[k] > noise * thr { 1.0 } else { floor };
                let g = &mut st.gains[k];
                let c = if target > *g { 0.7 } else { release };
                *g += (target - *g) * c;
                if rotate {
                    st.ring[st.ring_pos * bins + k] = st.cur_min[k];
                    st.cur_min[k] = f32::INFINITY;
                }
            }
            if rotate {
                st.ring_pos = (st.ring_pos + 1) % MS_SUBWINDOWS;
                st.frames_in_sub = 0;
            }
            // Frequency smoothing (3-tap) to reduce musical noise.
            for k in 0..bins {
                let a = st.gains[k.saturating_sub(1)];
                let b = st.gains[k];
                let c = st.gains[(k + 1).min(bins - 1)];
                sg[k] = 0.25 * a + 0.5 * b + 0.25 * c;
            }
            for k in 0..bins {
                re[k] *= sg[k];
                im[k] *= sg[k];
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::*;

    const SR: f32 = 48000.0;

    #[test]
    fn dehum_removes_hum_keeps_program() {
        let mut d = DeHum::new(SR, 1);
        d.set_param("frequency", 0.0);
        d.set_param("harmonics", 4.0);
        let n = SR as usize * 2;
        let hum = |f: f64| sine(f, 0.2, SR as f64, n, 0.1);
        let (h1, h3, p) = (hum(50.0), hum(150.0), sine(1000.0, 0.2, SR as f64, n, 0.0));
        let mut x: Vec<f32> = (0..n).map(|i| h1[i] + h3[i] + p[i]).collect();
        d.process(&mut [&mut x]);
        let tail = &x[n / 2..];
        assert!(db(tone_amplitude(tail, 50.0, SR as f64) / 0.2) < -30.0);
        assert!(db(tone_amplitude(tail, 150.0, SR as f64) / 0.2) < -30.0);
        assert!(db(tone_amplitude(tail, 1000.0, SR as f64) / 0.2).abs() < 0.2);
        // 60 Hz mode leaves 50 Hz mostly alone.
        let mut d60 = DeHum::new(SR, 1);
        d60.set_param("frequency", 1.0);
        d60.reset();
        let mut y = hum(50.0);
        d60.process(&mut [&mut y]);
        assert!(db(tone_amplitude(&y[n / 2..], 50.0, SR as f64) / 0.2) > -3.0);
    }

    #[test]
    fn denoise_zero_reduction_is_delayed_identity() {
        let mut d = DeNoise::new(SR, 1);
        d.set_param("reduction", 0.0);
        let lat = d.latency();
        let mut rng = Rng::new(9);
        let x: Vec<f32> = (0..20000).map(|_| rng.uniform() * 0.5).collect();
        let mut y = x.clone();
        d.process(&mut [&mut y]);
        for i in lat..x.len() {
            assert!((y[i] - x[i - lat]).abs() < 1e-4, "i {i}: {} vs {}", y[i], x[i - lat]);
        }
    }

    #[test]
    fn denoise_reduces_noise_and_keeps_bursts() {
        let mut d = DeNoise::new(SR, 1);
        d.set_param("reduction", 20.0);
        let lat = d.latency();
        let n = SR as usize * 8;
        let mut rng = Rng::new(21);
        let noise: Vec<f32> = (0..n).map(|_| rng.uniform() * 0.02).collect();
        let tone = sine(1000.0, 0.3, SR as f64, n, 0.0);
        // Tone bursts: on for 0.5 s, off for 0.5 s.
        let on = |i: usize| (i / (SR as usize / 2)) % 2 == 1;
        let x: Vec<f32> = (0..n).map(|i| noise[i] + if on(i) { tone[i] } else { 0.0 }).collect();
        let mut y = x.clone();
        d.process(&mut [&mut y]);
        let half = SR as usize / 2;
        // Evaluate the last seconds (after warm-up), aligned for latency, away from edges.
        let seg = |k: usize| {
            let s = k * half + lat + half / 4;
            s..s + half / 2
        };
        let (off, onn) = (seg(14), seg(15));
        let red = db(rms(&y[off.clone()]) / rms(&x[off.start - lat..off.end - lat]));
        assert!(red < -10.0, "noise reduction only {red} dB");
        let kept = db(tone_amplitude(&y[onn.clone()], 1000.0, SR as f64) / 0.3);
        assert!(kept.abs() < 1.0, "tone changed by {kept} dB");
    }
}
