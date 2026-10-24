//! Essential Sound processors: DeEsser (split-band), DeReverb (spectral late-reverberation
//! suppression), Enhance Speech (a DSP voice-enhancement chain) and Stereo Width (mid/side).
//!
//! All written from textbook descriptions:
//! * de-esser: complementary split (band = 2nd-order high-pass, rest = x − band) with a relative
//!   band-vs-broadband detector driving the band gain;
//! * de-reverb: the statistical late-reverberation model (exponentially decaying diffuse tail,
//!   Polack): the late-reverb PSD at frame t is predicted from the reverberant PSD T_d earlier,
//!   λ_r(t) = e^{−2Δ·T_d}·λ_x(t − T_d), Δ = 3·ln10 / RT60, and removed by spectral subtraction
//!   with over-subtraction and a gain floor (Lebart et al. 2001; Habets 2007);
//! * enhance speech: high-pass, de-mud cut, presence and air boosts (RBJ biquads), then one gain
//!   computer combining a downward expander (noise between words) and a soft-knee compressor.

use super::stft::{Spectral, StftChannel, fft_size, stream};
use crate::biquad::{Biquad, Coeffs, FilterType};
use crate::{AudioEffect, ParamSpec, ParamValues, Smoothed, Unit, block_len, db_to_gain, flush, param_plumbing};

/// One-pole smoothing coefficient for a time constant in ms.
fn coef(ms: f64, sr: f64) -> f64 {
    if ms <= 0.0 { 0.0 } else { (-1.0 / (ms * 0.001 * sr)).exp() }
}

#[inline]
fn pow_db(p: f64) -> f64 {
    10.0 * (p + 1e-20).log10()
}

// ---------------------------------------------------------------------------------------------

/// De-esser: a 2nd-order high-pass sidechain at `frequency` detects sibilance (the band level
/// exceeds the broadband level by more than `threshold`); the band above `frequency` is then cut
/// by a dynamic RBJ high shelf whose gain follows the gain reduction (attack ~1 ms, release
/// ~60 ms, stereo-linked). At 0 dB reduction the shelf is an exact identity, so non-sibilant
/// passages pass unchanged. (A subtractive split `x − HP(x)` is not used: the high-pass phase
/// shift around the crossover makes `x − (1 − g)·HP(x)` boost there instead of cutting.)
pub struct DeEsser {
    pv: ParamValues,
    sr: f64,
    freq: Smoothed,
    coeffs: Coeffs,
    hp: Vec<Biquad>,
    shelf: Vec<Biquad>,
    shelf_coeffs: Coeffs,
    shelf_gr: f64,
    env_band: f64,
    env_full: f64,
    /// Smoothed gain reduction (dB, ≤ 0).
    gr: f64,
    det: f64,
    att: f64,
    rel: f64,
    threshold: f64,
    reduction: f64,
    gr_meter: f32,
}

impl DeEsser {
    pub const PARAMS: &'static [ParamSpec] = &[
        ParamSpec::log("frequency", "Frequency", 2000.0, 12000.0, 6000.0, Unit::Hertz),
        ParamSpec::new("threshold", "Threshold", -40.0, 0.0, -12.0, Unit::Decibels),
        ParamSpec::new("reduction", "Max Reduction", 0.0, 24.0, 8.0, Unit::Decibels),
    ];

    pub fn new(sample_rate: f32, channels: usize) -> Self {
        let sr = sample_rate as f64;
        let mut s = DeEsser {
            pv: ParamValues::new(Self::PARAMS),
            sr,
            freq: Smoothed::with_ms(6000.0, sample_rate, 20.0),
            coeffs: Coeffs::IDENTITY,
            hp: vec![Biquad::new(); channels.max(1)],
            shelf: vec![Biquad::new(); channels.max(1)],
            shelf_coeffs: Coeffs::IDENTITY,
            shelf_gr: 0.0,
            env_band: 0.0,
            env_full: 0.0,
            gr: 0.0,
            det: coef(2.0, sr),
            att: coef(1.0, sr),
            rel: coef(60.0, sr),
            threshold: -12.0,
            reduction: 8.0,
            gr_meter: 0.0,
        };
        s.apply_params(true);
        s
    }

    fn design(&mut self) {
        self.coeffs = Coeffs::design(FilterType::HighPass, self.sr, self.freq.value() as f64, std::f64::consts::FRAC_1_SQRT_2, 0.0);
        self.design_shelf();
    }

    fn design_shelf(&mut self) {
        self.shelf_coeffs = if self.gr == 0.0 {
            Coeffs::IDENTITY
        } else {
            Coeffs::design(FilterType::HighShelf, self.sr, self.freq.value() as f64, std::f64::consts::FRAC_1_SQRT_2, self.gr)
        };
        self.shelf_gr = self.gr;
    }

    fn apply_params(&mut self, snap: bool) {
        self.freq.set(self.pv.v("frequency"));
        self.threshold = self.pv.v("threshold") as f64;
        self.reduction = self.pv.v("reduction") as f64;
        if snap {
            self.freq.snap();
            self.design();
        }
    }

    /// Current band gain reduction in dB (≤ 0), for metering.
    pub fn gain_reduction_db(&self) -> f32 {
        self.gr_meter
    }
}

impl AudioEffect for DeEsser {
    param_plumbing!("deesser");
    fn reset(&mut self) {
        self.freq.snap();
        self.design();
        self.hp.iter_mut().for_each(Biquad::reset);
        self.shelf.iter_mut().for_each(Biquad::reset);
        self.env_band = 0.0;
        self.env_full = 0.0;
        self.gr = 0.0;
        self.gr_meter = 0.0;
        self.design_shelf();
    }
    fn process(&mut self, channels: &mut [&mut [f32]]) {
        let n = block_len(channels);
        let nch = channels.len().min(self.hp.len());
        for i in 0..n {
            if self.freq.is_smoothing() {
                self.freq.tick();
                self.design();
            }
            let (mut pb, mut pf) = (0.0f64, 0.0f64);
            for ch in 0..nch {
                let x = channels[ch][i] as f64;
                let b = self.hp[ch].tick(&self.coeffs, x);
                pb += b * b;
                pf += x * x;
            }
            self.env_band = flush(self.det * self.env_band + (1.0 - self.det) * pb);
            self.env_full = flush(self.det * self.env_full + (1.0 - self.det) * pf);
            let bdb = pow_db(self.env_band);
            let excess = bdb - pow_db(self.env_full) - self.threshold;
            let target = if bdb > -70.0 && excess > 0.0 { -(excess * 2.0).min(self.reduction) } else { 0.0 };
            let c = if target < self.gr { self.att } else { self.rel };
            self.gr = flush(c * self.gr + (1.0 - c) * target);
            if target == 0.0 && self.gr > -1e-4 {
                self.gr = 0.0;
            }
            if (self.gr - self.shelf_gr).abs() > 0.05 || (self.gr == 0.0) != (self.shelf_gr == 0.0) {
                self.design_shelf();
            }
            for ch in 0..nch {
                let x = channels[ch][i] as f64;
                channels[ch][i] = self.shelf[ch].tick(&self.shelf_coeffs, x) as f32;
            }
        }
        self.gr_meter = self.gr as f32;
    }
}

// ---------------------------------------------------------------------------------------------

/// Late-reverberation delay T_d (s): reflections later than this count as reverb.
const DEREVERB_TD: f64 = 0.05;
/// Time constant (s) of the PSD smoothing.
const DEREVERB_PSD_TAU: f64 = 0.02;

#[derive(Clone, Debug)]
struct ReverbState {
    psd: Vec<f32>,
    /// Ring of past smoothed PSDs, `delay_frames` deep.
    hist: Vec<f32>,
    pos: usize,
    gains: Vec<f32>,
    frames: usize,
}

impl ReverbState {
    fn new(bins: usize, delay: usize) -> Self {
        ReverbState { psd: vec![0.0; bins], hist: vec![0.0; bins * delay], pos: 0, gains: vec![1.0; bins], frames: 0 }
    }
    fn reset(&mut self) {
        self.psd.iter_mut().for_each(|v| *v = 0.0);
        self.hist.iter_mut().for_each(|v| *v = 0.0);
        self.gains.iter_mut().for_each(|v| *v = 1.0);
        self.pos = 0;
        self.frames = 0;
    }
}

/// Spectral de-reverberation: suppresses the late reverberant tail predicted by the exponential
/// decay model for the assumed `rt60`. `amount` sets the over-subtraction and the gain floor
/// (0 % = identity apart from the STFT delay, 100 % ≈ −18 dB floor).
pub struct DeReverb {
    pv: ParamValues,
    sr: f32,
    n: usize,
    hop: usize,
    delay: usize,
    spec: Spectral,
    stft: Vec<StftChannel>,
    state: Vec<ReverbState>,
    smoothed_gains: Vec<f32>,
    /// e^{−2Δ·T_d} (power decay over the delay).
    decay: f32,
    beta: f32,
    floor: f32,
    alpha: f32,
}

impl DeReverb {
    pub const PARAMS: &'static [ParamSpec] =
        &[ParamSpec::new("amount", "Amount", 0.0, 100.0, 50.0, Unit::Percent), ParamSpec::log("rt60", "Decay (RT60)", 0.1, 5.0, 0.8, Unit::Seconds)];

    pub fn new(sample_rate: f32, channels: usize) -> Self {
        let n = fft_size(sample_rate);
        let hop = n / 4;
        let bins = n / 2 + 1;
        let delay = ((DEREVERB_TD * sample_rate as f64 / hop as f64).round() as usize).max(1);
        let ch = channels.max(1);
        let mut s = DeReverb {
            pv: ParamValues::new(Self::PARAMS),
            sr: sample_rate,
            n,
            hop,
            delay,
            spec: Spectral::new(n, true),
            stft: vec![StftChannel::new(n); ch],
            state: vec![ReverbState::new(bins, delay); ch],
            smoothed_gains: vec![1.0; bins],
            decay: 1.0,
            beta: 1.0,
            floor: 1.0,
            alpha: (-(hop as f64) / (sample_rate as f64 * DEREVERB_PSD_TAU)).exp() as f32,
        };
        s.apply_params(true);
        s
    }

    fn apply_params(&mut self, _snap: bool) {
        let amount = self.pv.v("amount") / 100.0;
        let rt60 = self.pv.v("rt60") as f64;
        let delta = 3.0 * std::f64::consts::LN_10 / rt60;
        let td = self.delay as f64 * self.hop as f64 / self.sr as f64;
        self.decay = (-2.0 * delta * td).exp() as f32;
        self.beta = 1.0 + amount;
        self.floor = db_to_gain(-18.0 * amount);
    }
}

impl AudioEffect for DeReverb {
    param_plumbing!("dereverb");
    fn latency(&self) -> usize {
        self.n
    }
    fn reset(&mut self) {
        self.stft.iter_mut().for_each(StftChannel::reset);
        self.state.iter_mut().for_each(ReverbState::reset);
    }
    fn process(&mut self, channels: &mut [&mut [f32]]) {
        let len = block_len(channels);
        let (n, hop, delay) = (self.n, self.hop, self.delay);
        let (decay, beta, floor, alpha) = (self.decay, self.beta, self.floor, self.alpha);
        let scale = 1.0 / (n as f32 * 2.0);
        let bins = n / 2 + 1;
        let (states, sg) = (&mut self.state, &mut self.smoothed_gains);
        stream(&mut self.stft, &mut self.spec, channels, len, hop, scale, &mut |ch, _, re, im| {
            let st = &mut states[ch];
            let warm = st.frames >= delay;
            let base = st.pos * bins;
            for k in 0..bins {
                let p = re[k] * re[k] + im[k] * im[k];
                let psd = alpha * st.psd[k] + (1.0 - alpha) * p;
                // Range test: tiny values and NaN/inf (one bad input sample) clear the state
                // instead of poisoning the recursion and its history ring.
                st.psd[k] = if (1e-30..=f32::MAX).contains(&psd) { psd } else { 0.0 };
                // Late reverb predicted from the PSD `delay` frames ago (the ring slot we are
                // about to overwrite holds exactly that frame).
                let late = if warm { decay * st.hist[base + k] } else { 0.0 };
                st.hist[base + k] = st.psd[k];
                let target = if st.psd[k] > 0.0 { (1.0 - beta * late / st.psd[k]).max(floor) } else { 1.0 };
                let g = &mut st.gains[k];
                // fast recovery on onsets, slower suppression (less musical noise)
                let c = if target > *g { 0.8 } else { 0.5 };
                *g += (target - *g) * c;
            }
            st.pos = (st.pos + 1) % delay;
            st.frames = st.frames.saturating_add(1);
            for k in 0..bins {
                let a = st.gains[k.saturating_sub(1)];
                let c = st.gains[(k + 1).min(bins - 1)];
                sg[k] = 0.25 * a + 0.5 * st.gains[k] + 0.25 * c;
            }
            for k in 0..bins {
                re[k] *= sg[k];
                im[k] *= sg[k];
            }
        });
    }
}

// ---------------------------------------------------------------------------------------------

const ENH_STAGES: usize = 4;
/// Expander: below this level (dBFS) the signal is pushed down (noise between words).
const ENH_EXP_THRESHOLD: f64 = -50.0;
const ENH_EXP_RATIO: f64 = 3.0;
const ENH_EXP_RANGE: f64 = 18.0;
/// Compressor above this level (dBFS).
const ENH_COMP_THRESHOLD: f64 = -24.0;
const ENH_COMP_RATIO: f64 = 3.0;
const ENH_COMP_KNEE: f64 = 6.0;
/// Make-up gain: puts conversational speech (≈ −20 dBFS RMS) back near its input loudness.
const ENH_MAKEUP: f64 = 3.0;

/// Enhance Speech: a DSP voice-enhancement chain (no machine learning) — 80 Hz high-pass,
/// de-mud cut, presence and air boosts, then a combined downward expander + soft-knee
/// compressor with make-up gain. `tone` picks the cut/boost frequencies for lower or higher
/// voices; `mix` blends with the dry signal.
pub struct SpeechEnhance {
    pv: ParamValues,
    sr: f64,
    coeffs: [Coeffs; ENH_STAGES],
    filters: Vec<[Biquad; ENH_STAGES]>,
    mix: Smoothed,
    /// Detector mean square.
    ms: f64,
    /// Smoothed expander gain (dB, ≤ 0): opens fast, closes slowly.
    env_exp: f64,
    /// Smoothed compressor gain (dB, ≤ 0): fast attack, slow release.
    env_comp: f64,
    det: f64,
    fast: f64,
    exp_close: f64,
    comp_rel: f64,
}

impl SpeechEnhance {
    pub const PARAMS: &'static [ParamSpec] =
        &[ParamSpec::new("mix", "Mix", 0.0, 100.0, 100.0, Unit::Percent), ParamSpec::choice("tone", "Tone", &["Low Tone", "High Tone"], 0)];

    pub fn new(sample_rate: f32, channels: usize) -> Self {
        let sr = sample_rate as f64;
        let mut s = SpeechEnhance {
            pv: ParamValues::new(Self::PARAMS),
            sr,
            coeffs: [Coeffs::IDENTITY; ENH_STAGES],
            filters: vec![[Biquad::new(); ENH_STAGES]; channels.max(1)],
            mix: Smoothed::with_ms(1.0, sample_rate, 20.0),
            ms: 0.0,
            env_exp: 0.0,
            env_comp: 0.0,
            det: coef(5.0, sr),
            fast: coef(3.0, sr),
            exp_close: coef(30.0, sr),
            comp_rel: coef(120.0, sr),
        };
        s.apply_params(true);
        s
    }

    fn apply_params(&mut self, snap: bool) {
        let high = self.pv.idx("tone") == 1;
        let (mud, presence) = if high { (350.0, 4000.0) } else { (250.0, 2500.0) };
        let sr = self.sr;
        self.coeffs = [
            Coeffs::design(FilterType::HighPass, sr, 80.0, std::f64::consts::FRAC_1_SQRT_2, 0.0),
            Coeffs::design(FilterType::Peaking, sr, mud, 1.0, -3.0),
            Coeffs::design(FilterType::Peaking, sr, presence, 1.0, 4.0),
            Coeffs::design(FilterType::HighShelf, sr, 10000.0, std::f64::consts::FRAC_1_SQRT_2, 2.0),
        ];
        self.mix.set(self.pv.v("mix") / 100.0);
        if snap {
            self.mix.snap();
        }
    }

    /// Static downward-expander gain (dB) for a detector level (dBFS).
    fn expander_db(level: f64) -> f64 {
        if level < ENH_EXP_THRESHOLD { ((level - ENH_EXP_THRESHOLD) * (ENH_EXP_RATIO - 1.0)).max(-ENH_EXP_RANGE) } else { 0.0 }
    }

    /// Static soft-knee compressor gain (dB) for a detector level (dBFS).
    fn compressor_db(level: f64) -> f64 {
        let d = level - ENH_COMP_THRESHOLD;
        if 2.0 * d.abs() <= ENH_COMP_KNEE {
            (1.0 / ENH_COMP_RATIO - 1.0) * (d + ENH_COMP_KNEE / 2.0).powi(2) / (2.0 * ENH_COMP_KNEE)
        } else if d > 0.0 {
            d / ENH_COMP_RATIO - d
        } else {
            0.0
        }
    }
}

impl AudioEffect for SpeechEnhance {
    param_plumbing!("speech_enhance");
    fn reset(&mut self) {
        self.filters.iter_mut().for_each(|f| f.iter_mut().for_each(Biquad::reset));
        self.mix.snap();
        self.ms = 0.0;
        self.env_exp = 0.0;
        self.env_comp = 0.0;
    }
    fn process(&mut self, channels: &mut [&mut [f32]]) {
        let n = block_len(channels);
        let nch = channels.len().min(self.filters.len()).min(8);
        let mut wet = [0.0f64; 8];
        for i in 0..n {
            let mut sq = 0.0f64;
            for ch in 0..nch {
                let mut x = channels[ch][i] as f64;
                for (st, c) in self.filters[ch].iter_mut().zip(self.coeffs.iter()) {
                    x = st.tick(c, x);
                }
                wet[ch] = x;
                sq = sq.max(x * x);
            }
            self.ms = flush(self.det * self.ms + (1.0 - self.det) * sq);
            let level = pow_db(self.ms);
            let te = Self::expander_db(level);
            let c = if te > self.env_exp { self.fast } else { self.exp_close };
            self.env_exp = c * self.env_exp + (1.0 - c) * te;
            let tc = Self::compressor_db(level);
            let c = if tc < self.env_comp { self.fast } else { self.comp_rel };
            self.env_comp = c * self.env_comp + (1.0 - c) * tc;
            if self.env_exp.abs() < 1e-9 {
                self.env_exp = 0.0;
            }
            if self.env_comp.abs() < 1e-9 {
                self.env_comp = 0.0;
            }
            let g = 10f64.powf((self.env_exp + self.env_comp + ENH_MAKEUP) / 20.0);
            let m = self.mix.tick() as f64;
            for (ch, w) in wet.iter().enumerate().take(nch) {
                let dry = channels[ch][i] as f64;
                channels[ch][i] = (dry + m * (w * g - dry)) as f32;
            }
        }
    }
}

// ---------------------------------------------------------------------------------------------

/// Stereo width by mid/side scaling: 0 % = mono, 100 % = unchanged, 200 % = side doubled.
/// Only the first two channels are processed; mono input passes through.
pub struct StereoWidth {
    pv: ParamValues,
    width: Smoothed,
}

impl StereoWidth {
    pub const PARAMS: &'static [ParamSpec] = &[ParamSpec::new("width", "Width", 0.0, 200.0, 100.0, Unit::Percent)];

    pub fn new(sample_rate: f32, _channels: usize) -> Self {
        let mut s = StereoWidth { pv: ParamValues::new(Self::PARAMS), width: Smoothed::with_ms(1.0, sample_rate, 20.0) };
        s.apply_params(true);
        s
    }
    fn apply_params(&mut self, snap: bool) {
        self.width.set(self.pv.v("width") / 100.0);
        if snap {
            self.width.snap();
        }
    }
}

impl AudioEffect for StereoWidth {
    param_plumbing!("stereo_width");
    fn reset(&mut self) {
        self.width.snap();
    }
    fn process(&mut self, channels: &mut [&mut [f32]]) {
        let n = block_len(channels);
        if channels.len() < 2 {
            // keep the smoother in step with time
            for _ in 0..n {
                self.width.tick();
            }
            return;
        }
        let (l, rest) = channels.split_at_mut(1);
        let (l, r) = (&mut l[0], &mut rest[0]);
        for i in 0..n {
            let w = self.width.tick();
            let m = 0.5 * (l[i] + r[i]);
            let s = 0.5 * (l[i] - r[i]) * w;
            l[i] = m + s;
            r[i] = m - s;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::*;

    const SR: f32 = 48000.0;

    fn energy(x: &[f32]) -> f64 {
        x.iter().map(|&v| (v as f64) * (v as f64)).sum::<f64>()
    }

    /// Band-limited energy via a 4th-order band-pass (two cascaded RBJ band-passes at the
    /// geometric centre) — good enough for relative before/after comparisons.
    fn band_energy(x: &[f32], lo: f64, hi: f64) -> f64 {
        let fc = (lo * hi).sqrt();
        let q = fc / (hi - lo);
        let c = Coeffs::design(FilterType::BandPass, SR as f64, fc, q, 0.0);
        let (mut a, mut b) = (Biquad::new(), Biquad::new());
        x.iter().map(|&v| b.tick(&c, a.tick(&c, v as f64))).map(|v| v * v).sum::<f64>()
    }

    fn harmonic_voice(n: usize, f0: f64, amp: f64) -> Vec<f32> {
        let mut v = vec![0.0f32; n];
        let mut k = 1;
        while f0 * k as f64 <= 1500.0 {
            let s = sine(f0 * k as f64, amp / k as f64, SR as f64, n, k as f64 * 0.7);
            for (o, x) in v.iter_mut().zip(&s) {
                *o += x;
            }
            k += 1;
        }
        v
    }

    fn bandpassed_noise(seed: u64, n: usize, lo: f64, hi: f64, amp: f32) -> Vec<f32> {
        let mut r = Rng::new(seed);
        let fc = (lo * hi).sqrt();
        let c = Coeffs::design(FilterType::BandPass, SR as f64, fc, fc / (hi - lo), 0.0);
        let (mut a, mut b) = (Biquad::new(), Biquad::new());
        (0..n).map(|_| (b.tick(&c, a.tick(&c, (r.uniform() * amp) as f64)) * 2.0) as f32).collect()
    }

    #[test]
    fn deesser_zero_reduction_is_identity() {
        let mut d = DeEsser::new(SR, 1);
        d.set_param("reduction", 0.0);
        d.reset();
        let mut r = Rng::new(3);
        let x: Vec<f32> = (0..20000).map(|_| r.uniform() * 0.5).collect();
        let mut y = x.clone();
        d.process(&mut [&mut y]);
        for (a, b) in x.iter().zip(&y) {
            assert!((a - b).abs() < 1e-6);
        }
    }

    #[test]
    fn deesser_reduces_sibilance_keeps_voice() {
        let n = SR as usize * 4;
        let voice = harmonic_voice(n, 150.0, 0.15);
        let sib = bandpassed_noise(11, n, 5000.0, 10000.0, 0.6);
        // sibilant bursts: 150 ms every 500 ms (from 250 ms)
        let burst = |i: usize| {
            let p = i % (SR as usize / 2);
            p >= SR as usize / 4 && p < SR as usize / 4 + (0.15 * SR) as usize
        };
        let x: Vec<f32> = (0..n).map(|i| voice[i] + if burst(i) { sib[i] } else { 0.0 }).collect();
        for (reduction, need) in [(8.0f32, 5.0f64), (16.0, 6.0)] {
            let mut d = DeEsser::new(SR, 1);
            d.set_param("reduction", reduction);
            d.reset();
            let mut y = x.clone();
            d.process(&mut [&mut y]);
            let pick = |v: &[f32], on: bool| -> Vec<f32> { v.iter().enumerate().skip(SR as usize).filter(|(i, _)| burst(*i) == on).map(|(_, s)| *s).collect() };
            let (xb, yb) = (pick(&x, true), pick(&y, true));
            let sib_drop = 10.0 * (band_energy(&yb, 5000.0, 10000.0) / band_energy(&xb, 5000.0, 10000.0)).log10();
            let low_change = 10.0 * (band_energy(&y, 150.0, 1500.0) / band_energy(&x, 150.0, 1500.0)).log10();
            let (xq, yq) = (pick(&x, false), pick(&y, false));
            let quiet_change = 10.0 * (energy(&yq) / energy(&xq)).log10();
            println!("deesser reduction {reduction} dB: sibilance {sib_drop:.2} dB, voice band {low_change:.3} dB, non-sibilant passages {quiet_change:.3} dB");
            assert!(sib_drop < -need, "sibilance only {sib_drop} dB");
            assert!(low_change.abs() < 1.0, "voice band changed {low_change} dB");
            assert!(quiet_change.abs() < 0.5, "non-sibilant passages changed {quiet_change} dB");
        }
    }

    /// Voice bursts (0.1 s every 0.5 s) with a statistical late-reverb tail (DRR about 0 dB).
    fn reverberant_bursts(n: usize) -> (Vec<f32>, usize, usize) {
        let sr = SR as f64;
        let period = (0.5 * sr) as usize;
        let on = (0.1 * sr) as usize;
        let dry_src = harmonic_voice(n, 180.0, 0.25);
        let dry: Vec<f32> = (0..n).map(|i| if i % period < on { dry_src[i] } else { 0.0 }).collect();
        // Statistical late reverb (Polack): white noise shaped by the energy envelope of the dry
        // signal convolved with an exponential decay — a one-pole filter on the energy.
        let rt60 = 0.8;
        let a = (-6.0 * std::f64::consts::LN_10 / (rt60 * sr)).exp(); // power decay per sample
        let mut e = 0.0f64;
        let mut rng = Rng::new(77);
        let wet_gain = 1.0 - a; // tail energy ≈ direct energy (DRR ≈ 0 dB)
        let x: Vec<f32> = dry
            .iter()
            .map(|&d| {
                e = a * e + wet_gain * (d as f64) * (d as f64);
                d + (rng.uniform() as f64 * 3f64.sqrt() * e.sqrt()) as f32
            })
            .collect();
        (x, period, on)
    }

    /// After one NaN or infinite input sample DeReverb's gain reduction comes back: the tail
    /// suppression matches a run without the bad sample (not just finite output).
    #[test]
    fn dereverb_gain_reduction_recovers_after_non_finite_sample() {
        let n = SR as usize * 6;
        let (x, period, on) = reverberant_bursts(n);
        let run = |bad: Option<f32>| {
            let mut fx = DeReverb::new(SR, 1);
            fx.set_param("amount", 100.0);
            fx.reset();
            let mut y = x.clone();
            if let Some(b) = bad {
                y[SR as usize * 2 + 777] = b;
            }
            for blk in y.chunks_mut(512) {
                fx.process(&mut [blk]);
            }
            y.split_off(DeReverb::new(SR, 1).latency())
        };
        // Tail-to-burst energy ratio (dB) over the last two seconds (latency already removed).
        let tail_ratio = |v: &[f32]| {
            let (mut b, mut g) = (0.0f64, 0.0f64);
            for (i, s) in v.iter().enumerate().skip(SR as usize * 4) {
                let p = (*s as f64) * (*s as f64);
                if i % period < on { b += p } else { g += p }
            }
            10.0 * (g / b).log10()
        };
        let clean = run(None);
        let x = &x[..clean.len()];
        let reduction = tail_ratio(x) - tail_ratio(&clean);
        assert!(reduction >= 4.0, "clean run reduces the tail by only {reduction} dB");
        for bad in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
            let y = run(Some(bad));
            assert!(y[SR as usize * 4..].iter().all(|v| v.is_finite()), "{bad}: output not finite");
            let got = tail_ratio(x) - tail_ratio(&y);
            assert!((got - reduction).abs() < 0.5, "{bad}: tail reduction {got} dB after the bad sample, {reduction} dB without");
        }
    }

    #[test]
    fn dereverb_suppresses_tail() {
        let n = SR as usize * 6;
        let (x, period, on) = reverberant_bursts(n);
        let mut fx = DeReverb::new(SR, 1);
        fx.set_param("amount", 100.0);
        fx.reset();
        let lat = fx.latency();
        let mut y = x.clone();
        fx.process(&mut [&mut y]);
        let y = &y[lat..];
        let start = SR as usize; // after warm-up
        let split = |v: &[f32]| {
            let (mut b, mut g) = (0.0, 0.0);
            for (i, s) in v.iter().enumerate().take(n - lat).skip(start) {
                let p = (*s as f64) * (*s as f64);
                if i % period < on { b += p } else { g += p }
            }
            (b, g)
        };
        let (xb, xg) = split(&x);
        let (yb, yg) = split(y);
        let before = 10.0 * (xg / xb).log10();
        let after = 10.0 * (yg / yb).log10();
        let burst_loss = 10.0 * (yb / xb).log10();
        println!("dereverb: tail/burst {before:.2} dB → {after:.2} dB (improvement {:.2} dB), burst energy {burst_loss:.2} dB", before - after);
        assert!(before - after >= 4.0, "tail improvement only {} dB", before - after);
        assert!(burst_loss > -3.0, "burst energy lost {burst_loss} dB");
    }

    #[test]
    fn dereverb_zero_amount_is_delayed_identity() {
        let mut d = DeReverb::new(SR, 1);
        d.set_param("amount", 0.0);
        let lat = d.latency();
        let mut rng = Rng::new(9);
        let x: Vec<f32> = (0..20000).map(|_| rng.uniform() * 0.5).collect();
        let mut y = x.clone();
        d.process(&mut [&mut y]);
        for i in lat..x.len() {
            assert!((y[i] - x[i - lat]).abs() < 1e-4);
        }
    }

    #[test]
    fn speech_enhance_presence_noise_and_level() {
        let sr = SR as usize;
        let n = sr * 4;
        let voice = harmonic_voice(n, 140.0, 0.08);
        let fric = bandpassed_noise(5, n, 1500.0, 6000.0, 0.05);
        let mut rng = Rng::new(42);
        // syllables: 200 ms on, 200 ms off; −60 dBFS-ish background noise throughout
        let syl = |i: usize| (i / (sr / 5)).is_multiple_of(2);
        let x: Vec<f32> = (0..n).map(|i| rng.uniform() * 0.0017 + if syl(i) { voice[i] + fric[i] } else { 0.0 }).collect();
        let mut fx = SpeechEnhance::new(SR, 1);
        let mut y = x.clone();
        fx.process(&mut [&mut y]);
        let pick = |v: &[f32], on: bool| -> Vec<f32> {
            v.iter()
                .enumerate()
                .skip(sr)
                .filter(|(i, _)| {
                    let p = i % (2 * sr / 5);
                    // skip 50 ms after each edge
                    syl(*i) == on && p % (sr / 5) > sr / 20
                })
                .map(|(_, s)| *s)
                .collect()
        };
        let (xs, ys) = (pick(&x, true), pick(&y, true));
        let tilt_in = 10.0 * (band_energy(&xs, 2000.0, 5000.0) / band_energy(&xs, 200.0, 400.0)).log10();
        let tilt_out = 10.0 * (band_energy(&ys, 2000.0, 5000.0) / band_energy(&ys, 200.0, 400.0)).log10();
        let noise = 10.0 * (energy(&pick(&y, false)) / energy(&pick(&x, false))).log10();
        let level = 10.0 * (energy(&y[sr..]) / energy(&x[sr..])).log10();
        println!(
            "enhance speech: presence/mud tilt {tilt_in:.2} → {tilt_out:.2} dB (+{:.2}), noise between syllables {noise:.2} dB, level {level:.2} dB",
            tilt_out - tilt_in
        );
        assert!(tilt_out - tilt_in > 3.0);
        assert!(noise <= -3.0, "noise floor only {noise} dB");
        assert!(level.abs() <= 3.0, "level changed {level} dB");
        // mix 0 is identity
        let mut fx = SpeechEnhance::new(SR, 1);
        fx.set_param("mix", 0.0);
        fx.reset();
        let mut z = x.clone();
        fx.process(&mut [&mut z]);
        assert!(z.iter().zip(&x).all(|(a, b)| (a - b).abs() < 1e-6));
    }

    #[test]
    fn stereo_width_laws() {
        let mut rng = Rng::new(1);
        let l0: Vec<f32> = (0..4000).map(|_| rng.uniform() * 0.5).collect();
        let r0: Vec<f32> = (0..4000).map(|_| rng.uniform() * 0.5).collect();
        let run = |w: f32| {
            let mut fx = StereoWidth::new(SR, 2);
            fx.set_param("width", w);
            fx.reset();
            let (mut l, mut r) = (l0.clone(), r0.clone());
            fx.process(&mut [&mut l, &mut r]);
            (l, r)
        };
        let (l, r) = run(0.0);
        assert!(l.iter().zip(&r).all(|(a, b)| (a - b).abs() < 1e-7));
        let (l, r) = run(100.0);
        assert!(l.iter().zip(&l0).chain(r.iter().zip(&r0)).all(|(a, b)| (a - b).abs() < 1e-6));
        let (l, r) = run(200.0);
        for i in 0..l.len() {
            let (m, s) = (0.5 * (l0[i] + r0[i]), 0.5 * (l0[i] - r0[i]));
            assert!((0.5 * (l[i] + r[i]) - m).abs() < 1e-6);
            assert!((0.5 * (l[i] - r[i]) - 2.0 * s).abs() < 1e-6);
        }
        // mono passes through
        let mut fx = StereoWidth::new(SR, 2);
        fx.set_param("width", 0.0);
        let mut m = l0.clone();
        fx.process(&mut [&mut m]);
        assert_eq!(m, l0);
    }

    /// The Essential Sound "Dialogue" chain on 60 s of stereo 48 kHz.
    /// `cargo test --release -p filmcraft-audio-dsp dialogue_chain_realtime -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn dialogue_chain_realtime() {
        let sr = SR as usize;
        let n = sr * 60;
        let mut rng = Rng::new(3);
        let voice = harmonic_voice(n, 140.0, 0.1);
        let mut l: Vec<f32> = (0..n).map(|i| voice[i] + rng.uniform() * 0.01).collect();
        let mut r: Vec<f32> = l.iter().map(|v| v * 0.9 + rng.uniform() * 0.01).collect();
        let mut chain: Vec<Box<dyn AudioEffect>> =
            ["denoise", "parametric_eq", "dehum", "deesser", "dereverb", "compressor", "simple_eq", "speech_enhance", "reverb"]
                .iter()
                .map(|id| crate::create_effect(id, SR, 2).unwrap())
                .collect();
        chain[1].set_param("b1.on", 1.0);
        chain[1].set_param("b2.on", 1.0);
        chain[1].set_param("b2.type", 4.0);
        chain[1].set_param("b2.freq", 80.0);
        let t0 = std::time::Instant::now();
        for (lb, rb) in l.chunks_mut(1024).zip(r.chunks_mut(1024)) {
            for fx in chain.iter_mut() {
                fx.process(&mut [&mut *lb, &mut *rb]);
            }
        }
        let secs = t0.elapsed().as_secs_f64();
        println!("dialogue chain: 60 s stereo in {secs:.3} s = {:.1}× realtime (one core)", 60.0 / secs);
        assert!(l.iter().chain(&r).all(|v| v.is_finite()));
    }
}
