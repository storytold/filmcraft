//! Dynamics: compressor, expander/gate, look-ahead true-peak limiter.
//!
//! Detection is stereo-linked (the loudest channel drives one common gain) so the image does
//! not shift. Gain computers work in dB; attack/release are one-pole smoothers on the gain.

use crate::oversample::{Interpolator, PolyphaseBank, true_peak_factor};
use crate::{AudioEffect, ParamSpec, ParamValues, Smoothed, Unit, block_len, db_to_gain, param_plumbing};
use std::collections::VecDeque;

/// One-pole coefficient for a time constant in ms.
fn coef(ms: f32, sr: f32) -> f32 {
    if ms <= 0.0 { 0.0 } else { (-1.0 / (ms * 0.001 * sr)).exp() }
}

#[inline]
fn lin_to_db(x: f32) -> f32 {
    20.0 * (x.max(1e-10)).log10()
}

/// Static compressor curve with a quadratic soft knee (output level in dB for input level `x`).
pub fn compressor_curve(x: f32, threshold: f32, ratio: f32, knee: f32) -> f32 {
    let d = x - threshold;
    if knee > 0.0 && 2.0 * d.abs() <= knee {
        x + (1.0 / ratio - 1.0) * (d + knee / 2.0).powi(2) / (2.0 * knee)
    } else if d > 0.0 {
        threshold + d / ratio
    } else {
        x
    }
}

/// Static downward-expander curve (output level in dB), attenuation limited to `range` dB.
pub fn expander_curve(x: f32, threshold: f32, ratio: f32, range: f32) -> f32 {
    if x >= threshold { x } else { x + ((x - threshold) * (ratio - 1.0)).max(-range) }
}

// ---------------------------------------------------------------------------------------------

/// Feed-forward compressor (peak or RMS detector, soft knee, attack/release, make-up gain).
pub struct Compressor {
    pv: ParamValues,
    sr: f32,
    threshold: f32,
    ratio: f32,
    knee: f32,
    rms: bool,
    att: f32,
    rel: f32,
    rms_coef: f32,
    makeup: Smoothed,
    /// Detector mean square (RMS mode).
    ms: f32,
    /// Smoothed gain reduction (dB, ≤ 0).
    env: f32,
    gr_meter: f32,
}

impl Compressor {
    pub const PARAMS: &'static [ParamSpec] = &[
        ParamSpec::new("threshold", "Threshold", -60.0, 0.0, -20.0, Unit::Decibels),
        ParamSpec::new("ratio", "Ratio", 1.0, 30.0, 4.0, Unit::Ratio),
        ParamSpec::new("knee", "Knee", 0.0, 24.0, 6.0, Unit::Decibels),
        ParamSpec::log("attack", "Attack", 0.01, 300.0, 10.0, Unit::Milliseconds),
        ParamSpec::log("release", "Release", 1.0, 3000.0, 100.0, Unit::Milliseconds),
        ParamSpec::new("makeup", "Make-up Gain", -40.0, 40.0, 0.0, Unit::Decibels),
        ParamSpec::choice("detector", "Detector", &["Peak", "RMS"], 0),
    ];

    pub fn new(sample_rate: f32, _channels: usize) -> Self {
        let mut s = Compressor {
            pv: ParamValues::new(Self::PARAMS),
            sr: sample_rate,
            threshold: 0.0,
            ratio: 1.0,
            knee: 0.0,
            rms: false,
            att: 0.0,
            rel: 0.0,
            rms_coef: coef(10.0, sample_rate),
            makeup: Smoothed::with_ms(1.0, sample_rate, 20.0),
            ms: 0.0,
            env: 0.0,
            gr_meter: 0.0,
        };
        s.apply_params(true);
        s
    }

    fn apply_params(&mut self, snap: bool) {
        self.threshold = self.pv.v("threshold");
        self.ratio = self.pv.v("ratio");
        self.knee = self.pv.v("knee");
        self.att = coef(self.pv.v("attack"), self.sr);
        self.rel = coef(self.pv.v("release"), self.sr);
        self.rms = self.pv.idx("detector") == 1;
        self.makeup.set(db_to_gain(self.pv.v("makeup")));
        if snap {
            self.makeup.snap();
        }
    }

    /// Static curve at the current settings.
    pub fn curve(&self, x_db: f32) -> f32 {
        compressor_curve(x_db, self.threshold, self.ratio, self.knee)
    }

    /// Current gain reduction in dB (≤ 0), for metering.
    pub fn gain_reduction_db(&self) -> f32 {
        self.gr_meter
    }
}

impl AudioEffect for Compressor {
    param_plumbing!("compressor");
    fn transfer_db(&self, band: usize, input_db: f32) -> Option<f32> {
        (band == 0).then(|| self.curve(input_db) + self.pv.v("makeup"))
    }
    fn reset(&mut self) {
        self.makeup.snap();
        self.ms = 0.0;
        self.env = 0.0;
        self.gr_meter = 0.0;
    }
    fn process(&mut self, channels: &mut [&mut [f32]]) {
        let n = block_len(channels);
        for i in 0..n {
            let mut peak = 0.0f32;
            let mut sq = 0.0f32;
            for ch in channels.iter() {
                let x = ch[i];
                peak = peak.max(x.abs());
                sq = sq.max(x * x);
            }
            let level_db = if self.rms {
                self.ms = self.rms_coef * self.ms + (1.0 - self.rms_coef) * sq;
                if self.ms < 1e-30 {
                    self.ms = 0.0;
                }
                10.0 * self.ms.max(1e-20).log10()
            } else {
                lin_to_db(peak)
            };
            let target = self.curve(level_db) - level_db;
            let c = if target < self.env { self.att } else { self.rel };
            self.env = c * self.env + (1.0 - c) * target;
            if self.env.abs() < 1e-9 {
                self.env = 0.0;
            }
            let g = db_to_gain(self.env) * self.makeup.tick();
            for ch in channels.iter_mut() {
                ch[i] *= g;
            }
        }
        self.gr_meter = self.env;
    }
}

// ---------------------------------------------------------------------------------------------

/// Downward expander / noise gate with hold (ratio ≥ 10 behaves as a gate).
pub struct Gate {
    pv: ParamValues,
    sr: f32,
    threshold: f32,
    ratio: f32,
    range: f32,
    att: f32,
    rel: f32,
    hold: u32,
    hold_left: u32,
    env: f32,
}

impl Gate {
    pub const PARAMS: &'static [ParamSpec] = &[
        ParamSpec::new("threshold", "Threshold", -90.0, 0.0, -40.0, Unit::Decibels),
        ParamSpec::log("ratio", "Ratio", 1.0, 100.0, 10.0, Unit::Ratio),
        ParamSpec::new("range", "Range", 0.0, 90.0, 60.0, Unit::Decibels),
        ParamSpec::log("attack", "Attack", 0.01, 100.0, 1.0, Unit::Milliseconds),
        ParamSpec::new("hold", "Hold", 0.0, 2000.0, 50.0, Unit::Milliseconds),
        ParamSpec::log("release", "Release", 1.0, 3000.0, 150.0, Unit::Milliseconds),
    ];

    pub fn new(sample_rate: f32, _channels: usize) -> Self {
        let mut s = Gate {
            pv: ParamValues::new(Self::PARAMS),
            sr: sample_rate,
            threshold: 0.0,
            ratio: 1.0,
            range: 0.0,
            att: 0.0,
            rel: 0.0,
            hold: 0,
            hold_left: 0,
            env: 0.0,
        };
        s.apply_params(true);
        s
    }

    fn apply_params(&mut self, _snap: bool) {
        self.threshold = self.pv.v("threshold");
        self.ratio = self.pv.v("ratio");
        self.range = self.pv.v("range");
        self.att = coef(self.pv.v("attack"), self.sr);
        self.rel = coef(self.pv.v("release"), self.sr);
        self.hold = (self.pv.v("hold") * 0.001 * self.sr) as u32;
    }

    pub fn curve(&self, x_db: f32) -> f32 {
        expander_curve(x_db, self.threshold, self.ratio, self.range)
    }
}

impl AudioEffect for Gate {
    param_plumbing!("gate");
    fn transfer_db(&self, band: usize, input_db: f32) -> Option<f32> {
        (band == 0).then(|| self.curve(input_db))
    }
    fn reset(&mut self) {
        self.env = 0.0;
        self.hold_left = 0;
    }
    fn process(&mut self, channels: &mut [&mut [f32]]) {
        let n = block_len(channels);
        for i in 0..n {
            let peak = channels.iter().fold(0.0f32, |a, c| a.max(c[i].abs()));
            let level = lin_to_db(peak);
            let target = self.curve(level) - level;
            if target >= self.env {
                // Opening (or staying open): attack.
                self.env = self.att * self.env + (1.0 - self.att) * target;
                if target >= 0.0 {
                    self.hold_left = self.hold;
                }
            } else if self.hold_left > 0 {
                self.hold_left -= 1;
            } else {
                self.env = self.rel * self.env + (1.0 - self.rel) * target;
            }
            let g = db_to_gain(self.env);
            for ch in channels.iter_mut() {
                ch[i] *= g;
            }
        }
    }
}

// ---------------------------------------------------------------------------------------------

/// Look-ahead brick-wall limiter.
///
/// The required gain for each (optionally true-peak) detected sample is held for the look-ahead
/// window with a sliding minimum, released with a one-pole, and then smoothed with a moving
/// average exactly as long as the look-ahead, so the gain has reached its target when the peak
/// leaves the delay line. A final clip at the ceiling guarantees the sample peak never exceeds it.
pub struct Limiter {
    pv: ParamValues,
    sr: f32,
    in_gain: Smoothed,
    ceiling: f32,
    rel: f32,
    true_peak: bool,
    /// Look-ahead (samples) and detector alignment delay.
    look: usize,
    /// Delay-line capacity (samples of look-ahead) allocated at construction for 0…30 ms.
    max_look: usize,
    det_delay: usize,
    bank: PolyphaseBank,
    interp: Vec<Interpolator>,
    /// Per-channel delay line (ring), length `look + det_delay + 1`.
    delay: Vec<Vec<f32>>,
    wpos: usize,
    /// Monotonic deque for the sliding minimum: (sample index, gain).
    minq: VecDeque<(u64, f32)>,
    /// Box filter ring and running sum.
    box_ring: Vec<f32>,
    box_pos: usize,
    box_sum: f64,
    rel_state: f32,
    t: u64,
    gr_meter: f32,
}

impl Limiter {
    pub const PARAMS: &'static [ParamSpec] = &[
        ParamSpec::new("input_gain", "Input Boost", 0.0, 24.0, 0.0, Unit::Decibels),
        ParamSpec::new("ceiling", "Ceiling", -24.0, 0.0, -1.0, Unit::Decibels),
        ParamSpec::log("release", "Release", 1.0, 1000.0, 60.0, Unit::Milliseconds),
        ParamSpec::new("lookahead", "Look-Ahead", 0.0, 30.0, 5.0, Unit::Milliseconds),
        ParamSpec::toggle("true_peak", "True Peak", true),
    ];
    /// Default look-ahead when the parameter is left at its DSP default.
    pub const LOOKAHEAD_MS: f32 = 5.0;
    /// Longest look-ahead the delay line is sized for (Hard Limiter's 0…30 ms range).
    pub const MAX_LOOKAHEAD_MS: f32 = 30.0;

    pub fn new(sample_rate: f32, channels: usize) -> Self {
        let channels = channels.max(1);
        let bank = PolyphaseBank::new(true_peak_factor(sample_rate as f64).max(2));
        let max_look = ((Self::MAX_LOOKAHEAD_MS * 0.001 * sample_rate).round() as usize).max(1);
        let look = ((Self::LOOKAHEAD_MS * 0.001 * sample_rate).round() as usize).max(1).min(max_look);
        let det_delay = bank.delay();
        let dlen = max_look + det_delay + 1;
        let mut s = Limiter {
            pv: ParamValues::new(Self::PARAMS),
            sr: sample_rate,
            in_gain: Smoothed::with_ms(1.0, sample_rate, 20.0),
            ceiling: 1.0,
            rel: 0.0,
            true_peak: true,
            look,
            max_look,
            det_delay,
            bank,
            interp: vec![Interpolator::default(); channels],
            delay: vec![vec![0.0; dlen]; channels],
            wpos: 0,
            minq: VecDeque::with_capacity(look + 2),
            box_ring: vec![1.0; look + 1],
            box_pos: 0,
            box_sum: (look + 1) as f64,
            rel_state: 1.0,
            t: 0,
            gr_meter: 0.0,
        };
        s.apply_params(true);
        s
    }

    fn apply_params(&mut self, snap: bool) {
        self.in_gain.set(db_to_gain(self.pv.v("input_gain")));
        self.ceiling = db_to_gain(self.pv.v("ceiling"));
        self.rel = coef(self.pv.v("release"), self.sr);
        self.true_peak = self.pv.on("true_peak");
        let look = ((self.pv.v("lookahead") * 0.001 * self.sr).round() as usize).max(1).min(self.max_look);
        if look != self.look {
            self.look = look;
            self.minq.clear();
            self.box_ring = vec![1.0; look + 1];
            self.box_pos = 0;
            self.box_sum = (look + 1) as f64;
            self.rel_state = 1.0;
        }
        if snap {
            self.in_gain.snap();
        }
    }

    /// Current gain reduction in dB (≤ 0).
    pub fn gain_reduction_db(&self) -> f32 {
        self.gr_meter
    }
}

impl AudioEffect for Limiter {
    param_plumbing!("limiter");
    fn latency(&self) -> usize {
        self.look + self.det_delay
    }
    fn reset(&mut self) {
        self.in_gain.snap();
        self.interp.iter_mut().for_each(Interpolator::reset);
        self.delay.iter_mut().for_each(|d| d.iter_mut().for_each(|v| *v = 0.0));
        self.wpos = 0;
        self.minq.clear();
        self.box_ring.iter_mut().for_each(|v| *v = 1.0);
        self.box_pos = 0;
        self.box_sum = self.box_ring.len() as f64;
        self.rel_state = 1.0;
        self.t = 0;
        self.gr_meter = 0.0;
    }
    fn process(&mut self, channels: &mut [&mut [f32]]) {
        let n = block_len(channels);
        let nch = channels.len().min(self.delay.len());
        let dlen = self.delay[0].len();
        let window = (self.look + 1) as u64;
        for i in 0..n {
            let g_in = self.in_gain.tick();
            let det_pos = (self.wpos + dlen - self.det_delay) % dlen;
            let out_pos = (self.wpos + dlen - (self.look + self.det_delay)) % dlen;
            let mut det = 0.0f32;
            for ch in 0..nch {
                let x = channels[ch][i] * g_in;
                self.delay[ch][self.wpos] = x;
                let aligned = self.delay[ch][det_pos].abs();
                let tp = if self.true_peak { self.interp[ch].push_peak(&self.bank, x) } else { 0.0 };
                det = det.max(aligned.max(tp));
            }
            let greq = if det > self.ceiling { self.ceiling / det } else { 1.0 };
            // Sliding minimum over the last `window` values.
            while let Some(&(_, v)) = self.minq.back() {
                if v >= greq {
                    self.minq.pop_back();
                } else {
                    break;
                }
            }
            self.minq.push_back((self.t, greq));
            while let Some(&(idx, _)) = self.minq.front() {
                if idx + window <= self.t {
                    self.minq.pop_front();
                } else {
                    break;
                }
            }
            let m = self.minq.front().map_or(1.0, |&(_, v)| v);
            // Release.
            self.rel_state = if m < self.rel_state { m } else { self.rel_state + (m - self.rel_state) * (1.0 - self.rel) };
            // Moving average over look+1 samples.
            self.box_sum += self.rel_state as f64 - self.box_ring[self.box_pos] as f64;
            self.box_ring[self.box_pos] = self.rel_state;
            self.box_pos = (self.box_pos + 1) % self.box_ring.len();
            let g = (self.box_sum / self.box_ring.len() as f64) as f32;
            for ch in 0..nch {
                let y = self.delay[ch][out_pos] * g;
                channels[ch][i] = y.clamp(-self.ceiling, self.ceiling);
            }
            self.wpos = (self.wpos + 1) % dlen;
            self.t += 1;
            // Periodically re-sum to cancel floating-point drift.
            if self.t.is_multiple_of(65536) {
                self.box_sum = self.box_ring.iter().map(|&v| v as f64).sum();
            }
            self.gr_meter = lin_to_db(g);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::loudness::LoudnessMeter;
    use crate::testutil::*;

    const SR: f32 = 48000.0;

    #[test]
    fn compressor_static_curve() {
        let c = |x| compressor_curve(x, -20.0, 4.0, 0.0);
        assert_eq!(c(-30.0), -30.0);
        assert_eq!(c(-20.0), -20.0);
        assert_eq!(c(0.0), -15.0);
        // Soft knee: continuous at both knee edges, midpoint below the hard curve.
        let k = |x| compressor_curve(x, -20.0, 4.0, 10.0);
        assert!((k(-25.0) + 25.0).abs() < 1e-5);
        assert!((k(-15.0) - c(-15.0)).abs() < 1e-5);
        assert!(k(-20.0) < -20.0);
        assert!((k(-20.0) - (-20.0 + (0.25 - 1.0) * 25.0 / 20.0)).abs() < 1e-5);
        // Monotonic.
        let mut prev = f32::MIN;
        for i in 0..800 {
            let y = k(-80.0 + i as f32 * 0.1);
            assert!(y >= prev);
            prev = y;
        }
    }

    #[test]
    fn compressor_steady_state_matches_curve() {
        for detector in [0.0, 1.0] {
            let mut c = Compressor::new(SR, 2);
            c.set_param("threshold", -20.0);
            c.set_param("ratio", 4.0);
            c.set_param("knee", 0.0);
            c.set_param("makeup", 3.0);
            c.set_param("detector", detector);
            // Constant (DC) level: peak and RMS detectors agree.
            for level_db in [-40.0f32, -10.0, -2.0] {
                c.reset();
                let v = db_to_gain(level_db);
                let mut l = vec![v; 48000];
                let mut r = vec![-v; 48000];
                c.process(&mut [&mut l, &mut r]);
                let out_db = lin_to_db(l[47999]);
                let expect = compressor_curve(level_db, -20.0, 4.0, 0.0) + 3.0;
                assert!((out_db - expect).abs() < 0.05, "det {detector} in {level_db}: {out_db} vs {expect}");
                assert_eq!(l[47999], -r[47999]);
            }
        }
    }

    #[test]
    fn compressor_attack_time() {
        let mut c = Compressor::new(SR, 1);
        c.set_param("threshold", -20.0);
        c.set_param("ratio", 20.0);
        c.set_param("knee", 0.0);
        c.set_param("attack", 10.0);
        let mut x = vec![1.0f32; 4800];
        c.process(&mut [&mut x]);
        // After one time constant ~63% of the final reduction (in dB) is applied.
        let final_db = compressor_curve(0.0, -20.0, 20.0, 0.0);
        let at_tau = lin_to_db(x[480]);
        assert!((at_tau / final_db - 0.632).abs() < 0.02, "{at_tau} / {final_db}");
    }

    #[test]
    fn gate_attenuates_below_threshold_and_holds() {
        let mut g = Gate::new(SR, 1);
        g.set_param("threshold", -30.0);
        g.set_param("ratio", 100.0);
        g.set_param("range", 40.0);
        g.set_param("hold", 100.0);
        g.set_param("release", 10.0);
        let loud = db_to_gain(-10.0);
        let quiet = db_to_gain(-50.0);
        let mut x: Vec<f32> = (0..48000).map(|i| if i < 24000 { loud } else { quiet }).collect();
        g.process(&mut [&mut x]);
        assert!((lin_to_db(x[23999]) + 10.0).abs() < 0.01);
        // During hold (100 ms) the gate stays open.
        assert!((lin_to_db(x[24000 + 2400]) + 50.0).abs() < 0.01);
        // Well after hold + release: attenuated by the full range.
        assert!((lin_to_db(x[47999]) + 90.0).abs() < 0.1, "{}", lin_to_db(x[47999]));
        assert!((expander_curve(-40.0, -30.0, 2.0, 60.0) + 50.0).abs() < 1e-6);
    }

    #[test]
    fn limiter_never_exceeds_ceiling() {
        let mut rng = Rng::new(7);
        for tp in [0.0, 1.0] {
            let mut lim = Limiter::new(SR, 2);
            lim.set_param("ceiling", -1.0);
            lim.set_param("input_gain", 12.0);
            lim.set_param("true_peak", tp);
            let ceiling = db_to_gain(-1.0);
            let mut all_l = Vec::new();
            let mut all_r = Vec::new();
            for block in 0..200 {
                let len = 1 + (rng.next_u64() % 700) as usize;
                let mut l: Vec<f32> = (0..len).map(|_| rng.uniform() * 0.8).collect();
                let mut r: Vec<f32> = (0..len).map(|_| rng.uniform() * 0.3).collect();
                if block % 7 == 0 {
                    l[len / 2] = 4.0; // impulse
                    r[0] = -3.0;
                }
                lim.process(&mut [&mut l, &mut r]);
                assert!(l.iter().chain(&r).all(|v| v.abs() <= ceiling + 1e-7));
                all_l.extend_from_slice(&l);
                all_r.extend_from_slice(&r);
            }
            if tp == 1.0 {
                // With true-peak detection the reconstructed peak stays close to the ceiling.
                let mut m = LoudnessMeter::new(SR as f64, 2);
                m.process(&[&all_l, &all_r]);
                assert!(m.true_peak_dbtp() < -1.0 + 0.6, "TP {}", m.true_peak_dbtp());
            }
        }
    }

    #[test]
    fn limiter_is_transparent_below_ceiling_with_reported_latency() {
        let mut lim = Limiter::new(SR, 1);
        let lat = lim.latency();
        let x = sine(1000.0, 0.3, SR as f64, 4800, 0.0);
        let mut y = x.clone();
        lim.process(&mut [&mut y]);
        for i in lat..4800 {
            assert!((y[i] - x[i - lat]).abs() < 1e-6);
        }
    }

    #[test]
    fn limiter_reduces_loud_sine_to_ceiling_smoothly() {
        let mut lim = Limiter::new(SR, 1);
        lim.set_param("ceiling", -6.0);
        let mut x = sine(200.0, 1.0, SR as f64, 48000, 0.0);
        lim.process(&mut [&mut x]);
        let tail = &x[24000..];
        let peak = tail.iter().fold(0.0f32, |a, v| a.max(v.abs()));
        assert!((lin_to_db(peak) + 6.0).abs() < 0.3, "{}", lin_to_db(peak));
    }

    #[test]
    fn limiter_lookahead_changes_latency_and_transient_shape() {
        let mut a = Limiter::new(SR, 1);
        let mut b = Limiter::new(SR, 1);
        a.set_param("lookahead", 0.0);
        b.set_param("lookahead", 30.0);
        a.set_param("ceiling", -6.0);
        b.set_param("ceiling", -6.0);
        assert!(b.latency() > a.latency(), "{} vs {}", b.latency(), a.latency());
        let mut xa: Vec<f32> = (0..4800).map(|i| 0.08 * (2.0 * std::f32::consts::PI * 997.0 * i as f32 / SR).sin()).collect();
        for s in &mut xa[1200..1248] {
            *s = 0.9;
        }
        let mut xb = xa.clone();
        a.process(&mut [&mut xa]);
        b.process(&mut [&mut xb]);
        let max = xa.iter().zip(&xb).map(|(x, y)| (x - y).abs()).fold(0.0f32, f32::max);
        assert!(max > 1e-4, "look-ahead of 0 ms and 30 ms produced the same waveform (max Δ {max})");
    }
}
