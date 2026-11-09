//! Parametric (8-band) and simple 3-band equalisers.

use crate::biquad::{Biquad, Coeffs, FilterType};
use crate::{AudioEffect, ParamSpec, ParamValues, Smoothed, Unit, block_len, param_plumbing};

/// Names of [`FilterType::ALL`] for the `type` choice parameters.
pub const FILTER_TYPE_NAMES: &[&str] = &["Peaking", "Low Shelf", "High Shelf", "Low Pass", "High Pass", "Notch", "Band Pass"];

/// Coefficients are recomputed at most every this many samples while parameters glide.
const COEFF_INTERVAL: u32 = 16;
const SMOOTH_MS: f32 = 30.0;
/// Most bands a [`BandBank`] can hold.
pub(crate) const MAX_BANDS: usize = 40;

#[derive(Clone, Copy)]
pub(crate) struct BandSettings {
    enabled: bool,
    kind: FilterType,
    frequency_hz: f32,
    gain_db: f32,
    q: f32,
    snap: bool,
}

impl BandSettings {
    pub(crate) fn new(enabled: bool, kind: FilterType, frequency_hz: f32, gain_db: f32, q: f32, snap: bool) -> Self {
        Self { enabled, kind, frequency_hz, gain_db, q, snap }
    }
}

#[derive(Clone, Debug)]
struct Band {
    kind: FilterType,
    /// log2(Hz).
    freq: Smoothed,
    gain: Smoothed,
    q: Smoothed,
    /// Band enable cross-fade (0 = bypassed, 1 = active).
    mix: Smoothed,
    coeffs: Coeffs,
    dirty: bool,
}

/// A bank of biquad bands shared by both EQs.
#[derive(Clone, Debug)]
pub(crate) struct BandBank {
    sample_rate: f64,
    bands: Vec<Band>,
    /// `state[ch][band]`.
    state: Vec<Vec<Biquad>>,
    counter: u32,
}

impl BandBank {
    pub(crate) fn new(sample_rate: f32, channels: usize, nbands: usize) -> Self {
        assert!(nbands <= MAX_BANDS);
        let band = Band {
            kind: FilterType::Peaking,
            freq: Smoothed::with_ms(1000f32.log2(), sample_rate, SMOOTH_MS),
            gain: Smoothed::with_ms(0.0, sample_rate, SMOOTH_MS),
            q: Smoothed::with_ms(1.0, sample_rate, SMOOTH_MS),
            mix: Smoothed::with_ms(0.0, sample_rate, SMOOTH_MS),
            coeffs: Coeffs::IDENTITY,
            dirty: true,
        };
        BandBank { sample_rate: sample_rate as f64, bands: vec![band; nbands], state: vec![vec![Biquad::new(); nbands]; channels.max(1)], counter: 0 }
    }

    pub(crate) fn set_band(&mut self, i: usize, settings: BandSettings) {
        let BandSettings { enabled, kind, frequency_hz, gain_db, q, snap } = settings;
        let b = &mut self.bands[i];
        if b.kind != kind {
            b.kind = kind;
            b.dirty = true;
        }
        b.freq.set(frequency_hz.max(1.0).log2());
        b.gain.set(gain_db);
        b.q.set(q);
        b.mix.set(if enabled { 1.0 } else { 0.0 });
        if snap {
            b.freq.snap();
            b.gain.snap();
            b.q.snap();
            b.mix.snap();
        }
        b.dirty = true;
    }

    pub(crate) fn update_coeffs(&mut self, force: bool) {
        let sr = self.sample_rate;
        for b in &mut self.bands {
            if b.dirty || force {
                b.coeffs = Coeffs::design(b.kind, sr, 2f64.powf(b.freq.value() as f64), b.q.value() as f64, b.gain.value() as f64);
                b.dirty = b.freq.is_smoothing() || b.gain.is_smoothing() || b.q.is_smoothing();
            }
        }
    }

    pub(crate) fn reset(&mut self) {
        for b in &mut self.bands {
            b.freq.snap();
            b.gain.snap();
            b.q.snap();
            b.mix.snap();
            b.dirty = true;
        }
        for ch in &mut self.state {
            ch.iter_mut().for_each(Biquad::reset);
        }
        self.counter = 0;
        self.update_coeffs(true);
    }

    /// Analytic magnitude (dB) of the whole bank at its current (target-reached) settings.
    pub(crate) fn response_db(&self, freq: f64) -> f64 {
        let mut db = 0.0;
        for b in &self.bands {
            if b.mix.target() >= 0.5 {
                let c = Coeffs::design(b.kind, self.sample_rate, 2f64.powf(b.freq.target() as f64), b.q.target() as f64, b.gain.target() as f64);
                db += c.magnitude_db(freq, self.sample_rate);
            }
        }
        db
    }

    /// Process; `out_gain` is applied per sample.
    pub(crate) fn process(&mut self, channels: &mut [&mut [f32]], out_gain: &mut Smoothed) {
        let n = block_len(channels);
        let nch = channels.len().min(self.state.len());
        if n == 0 {
            return;
        }
        if !out_gain.is_smoothing()
            && self.bands.iter().all(|b| !b.dirty && !b.freq.is_smoothing() && !b.gain.is_smoothing() && !b.q.is_smoothing() && !b.mix.is_smoothing())
        {
            // Keep the exact per-sample arithmetic and recursive state; only the settled
            // parameter/smoother bookkeeping is hoisted out of the hot loop.
            let g = out_gain.value() as f64;
            let bands = &self.bands;
            // Channels are filtered in pairs so two independent recurrences share one loop.
            let mut held: Option<(&mut [f32], &mut [Biquad])> = None;
            for (channel, state) in channels.iter_mut().take(nch).zip(&mut self.state) {
                for (b, st) in bands.iter().zip(state.iter_mut()) {
                    if b.mix.value() <= 0.0 {
                        st.reset();
                    }
                }
                // A settled unit transfer with zero state is exactly transparent for finite
                // f32 input. Do not discard a tail or bypass a non-finite sample. Bounded
                // coefficients also ensure intermediate f64 products cannot overflow.
                let transparent = bands.iter().zip(state.iter()).all(|(b, st)| {
                    b.mix.value() <= 0.0
                        || (st.is_clear()
                            && b.coeffs.b0 == 1.0
                            && b.coeffs.b1 == b.coeffs.a1
                            && b.coeffs.b2 == b.coeffs.a2
                            && b.coeffs.b1.abs() <= 4.0
                            && b.coeffs.b2.abs() <= 4.0)
                });
                if transparent && channel.iter().take(n).all(|x| x.is_finite()) {
                    let active = bands.iter().any(|b| b.mix.value() > 0.0);
                    for sample in channel.iter_mut().take(n) {
                        let x = *sample as f64;
                        // An active identity biquad adds +0, which normalizes negative zero.
                        *sample = ((if active { x + 0.0 } else { x }) * g) as f32;
                    }
                    continue;
                }
                let (channel, state) = (&mut channel[..n], state.as_mut_slice());
                match held.take() {
                    Some((first, first_state)) => filter_pair(bands, first, first_state, channel, state, g),
                    None => held = Some((channel, state)),
                }
            }
            if let Some((channel, state)) = held {
                for sample in channel.iter_mut() {
                    let mut x = *sample as f64;
                    for (b, st) in bands.iter().zip(state.iter_mut()) {
                        let m = b.mix.value() as f64;
                        if m > 0.0 {
                            let y = st.tick(&b.coeffs, x);
                            x += m * (y - x);
                        }
                    }
                    *sample = (x * g) as f32;
                }
            }
            self.counter = (self.counter + (n % COEFF_INTERVAL as usize) as u32) % COEFF_INTERVAL;
            return;
        }
        for i in 0..n {
            if self.counter == 0 {
                self.update_coeffs(false);
            }
            self.counter = (self.counter + 1) % COEFF_INTERVAL;
            for b in &mut self.bands {
                b.freq.tick();
                b.gain.tick();
                b.q.tick();
                if b.freq.is_smoothing() || b.gain.is_smoothing() || b.q.is_smoothing() {
                    b.dirty = true;
                }
            }
            let g = out_gain.tick();
            // Tick the band mixes once per sample (shared by all channels).
            let mut mixes = [0.0f32; MAX_BANDS];
            for (m, b) in mixes.iter_mut().zip(self.bands.iter_mut()) {
                *m = b.mix.tick();
            }
            for ch in 0..nch {
                let mut x = channels[ch][i] as f64;
                for (k, b) in self.bands.iter().enumerate() {
                    let m = mixes[k] as f64;
                    let st = &mut self.state[ch][k];
                    if m > 0.0 {
                        let y = st.tick(&b.coeffs, x);
                        x += m * (y - x);
                    } else {
                        st.reset();
                    }
                }
                channels[ch][i] = (x * g as f64) as f32;
            }
        }
    }
}

/// The settled band cascade for two channels, with the same per-channel arithmetic as one.
fn filter_pair(bands: &[Band], l: &mut [f32], l_state: &mut [Biquad], r: &mut [f32], r_state: &mut [Biquad], g: f64) {
    for (a, b) in l.iter_mut().zip(r.iter_mut()) {
        let (mut x, mut y) = (*a as f64, *b as f64);
        for ((band, sl), sr) in bands.iter().zip(l_state.iter_mut()).zip(r_state.iter_mut()) {
            let m = band.mix.value() as f64;
            if m > 0.0 {
                let yl = sl.tick(&band.coeffs, x);
                x += m * (yl - x);
                let yr = sr.tick(&band.coeffs, y);
                y += m * (yr - y);
            }
        }
        *a = (x * g) as f32;
        *b = (y * g) as f32;
    }
}

macro_rules! eq_params {
    ($(($n:literal, $on:expr, $ty:expr, $f:expr, $q:expr)),* $(,)?) => {
        &[
            $(
                ParamSpec::toggle(concat!("b", $n, ".on"), concat!("Band ", $n, " On"), $on),
                ParamSpec::choice(concat!("b", $n, ".type"), concat!("Band ", $n, " Type"), FILTER_TYPE_NAMES, $ty),
                ParamSpec::log(concat!("b", $n, ".freq"), concat!("Band ", $n, " Frequency"), 20.0, 20000.0, $f, Unit::Hertz),
                ParamSpec::new(concat!("b", $n, ".gain"), concat!("Band ", $n, " Gain"), -30.0, 30.0, 0.0, Unit::Decibels),
                ParamSpec::log(concat!("b", $n, ".q"), concat!("Band ", $n, " Q"), 0.1, 30.0, $q, Unit::Q),
            )*
            ParamSpec::new("output", "Output Gain", -30.0, 30.0, 0.0, Unit::Decibels),
        ]
    };
}

const BAND_IDS: [[&str; 5]; 8] = [
    ["b1.on", "b1.type", "b1.freq", "b1.gain", "b1.q"],
    ["b2.on", "b2.type", "b2.freq", "b2.gain", "b2.q"],
    ["b3.on", "b3.type", "b3.freq", "b3.gain", "b3.q"],
    ["b4.on", "b4.type", "b4.freq", "b4.gain", "b4.q"],
    ["b5.on", "b5.type", "b5.freq", "b5.gain", "b5.q"],
    ["b6.on", "b6.type", "b6.freq", "b6.gain", "b6.q"],
    ["b7.on", "b7.type", "b7.freq", "b7.gain", "b7.q"],
    ["b8.on", "b8.type", "b8.freq", "b8.gain", "b8.q"],
];

/// 8-band parametric EQ. Every band can be any [`FilterType`]; defaults are
/// HP · low shelf · 4 × peaking · high shelf · LP with the pass filters disabled.
pub struct ParametricEq {
    pv: ParamValues,
    bank: BandBank,
    out: Smoothed,
}

impl ParametricEq {
    pub const PARAMS: &'static [ParamSpec] = eq_params![
        ("1", false, 4, 30.0, 0.707),
        ("2", true, 1, 100.0, 0.707),
        ("3", true, 0, 250.0, 1.0),
        ("4", true, 0, 700.0, 1.0),
        ("5", true, 0, 2000.0, 1.0),
        ("6", true, 0, 5000.0, 1.0),
        ("7", true, 2, 10000.0, 0.707),
        ("8", false, 3, 18000.0, 0.707),
    ];

    pub fn new(sample_rate: f32, channels: usize) -> Self {
        let mut s = ParametricEq {
            pv: ParamValues::new(Self::PARAMS),
            bank: BandBank::new(sample_rate, channels, 8),
            out: Smoothed::with_ms(1.0, sample_rate, SMOOTH_MS),
        };
        s.apply_params(true);
        s
    }

    fn apply_params(&mut self, snap: bool) {
        for (i, ids) in BAND_IDS.iter().enumerate() {
            self.bank.set_band(
                i,
                BandSettings::new(
                    self.pv.on(ids[0]),
                    FilterType::from_index(self.pv.idx(ids[1])),
                    self.pv.v(ids[2]),
                    self.pv.v(ids[3]),
                    self.pv.v(ids[4]),
                    snap,
                ),
            );
        }
        self.out.set(crate::db_to_gain(self.pv.v("output")));
        if snap {
            self.out.snap();
            self.bank.update_coeffs(true);
        }
    }

    /// Analytic magnitude response (dB) at the target settings, including output gain — for
    /// drawing the EQ curve.
    pub fn response_db(&self, freq: f64) -> f64 {
        self.bank.response_db(freq) + self.pv.v("output") as f64
    }
}

impl AudioEffect for ParametricEq {
    param_plumbing!("parametric_eq");
    fn response_db(&self, freq: f64) -> Option<f64> {
        Some(ParametricEq::response_db(self, freq))
    }
    fn reset(&mut self) {
        self.out.snap();
        self.bank.reset();
    }
    fn process(&mut self, channels: &mut [&mut [f32]]) {
        self.bank.process(channels, &mut self.out);
    }
}

/// Simple 3-band EQ: low shelf, mid peak, high shelf.
pub struct SimpleEq {
    pv: ParamValues,
    bank: BandBank,
    out: Smoothed,
}

impl SimpleEq {
    pub const PARAMS: &'static [ParamSpec] = &[
        ParamSpec::new("low", "Low", -24.0, 24.0, 0.0, Unit::Decibels),
        ParamSpec::log("low_freq", "Low Frequency", 20.0, 1000.0, 200.0, Unit::Hertz),
        ParamSpec::new("mid", "Mid", -24.0, 24.0, 0.0, Unit::Decibels),
        ParamSpec::log("mid_freq", "Mid Frequency", 100.0, 10000.0, 1000.0, Unit::Hertz),
        ParamSpec::log("mid_q", "Mid Q", 0.1, 10.0, 0.7, Unit::Q),
        ParamSpec::new("high", "High", -24.0, 24.0, 0.0, Unit::Decibels),
        ParamSpec::log("high_freq", "High Frequency", 1000.0, 20000.0, 5000.0, Unit::Hertz),
        ParamSpec::new("output", "Output Gain", -24.0, 24.0, 0.0, Unit::Decibels),
    ];

    pub fn new(sample_rate: f32, channels: usize) -> Self {
        let mut s =
            SimpleEq { pv: ParamValues::new(Self::PARAMS), bank: BandBank::new(sample_rate, channels, 3), out: Smoothed::with_ms(1.0, sample_rate, SMOOTH_MS) };
        s.apply_params(true);
        s
    }

    fn apply_params(&mut self, snap: bool) {
        let sq = std::f32::consts::FRAC_1_SQRT_2;
        let p = &self.pv;
        let (lg, lf, mg, mf, mq, hg, hf) = (p.v("low"), p.v("low_freq"), p.v("mid"), p.v("mid_freq"), p.v("mid_q"), p.v("high"), p.v("high_freq"));
        self.bank.set_band(0, BandSettings::new(true, FilterType::LowShelf, lf, lg, sq, snap));
        self.bank.set_band(1, BandSettings::new(true, FilterType::Peaking, mf, mg, mq, snap));
        self.bank.set_band(2, BandSettings::new(true, FilterType::HighShelf, hf, hg, sq, snap));
        self.out.set(crate::db_to_gain(self.pv.v("output")));
        if snap {
            self.out.snap();
            self.bank.update_coeffs(true);
        }
    }

    pub fn response_db(&self, freq: f64) -> f64 {
        self.bank.response_db(freq) + self.pv.v("output") as f64
    }
}

impl AudioEffect for SimpleEq {
    param_plumbing!("simple_eq");
    fn response_db(&self, freq: f64) -> Option<f64> {
        Some(SimpleEq::response_db(self, freq))
    }
    fn reset(&mut self) {
        self.out.snap();
        self.bank.reset();
    }
    fn process(&mut self, channels: &mut [&mut [f32]]) {
        self.bank.process(channels, &mut self.out);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::*;

    const SR: f32 = 48000.0;

    fn measure(fx: &mut dyn AudioEffect, freq: f64) -> f64 {
        fx.reset();
        let n = SR as usize;
        let mut l = sine(freq, 0.25, SR as f64, n, 0.0);
        let mut r = l.clone();
        fx.process(&mut [&mut l, &mut r]);
        let a = tone_amplitude(&l[n / 2..], freq, SR as f64);
        let b = tone_amplitude(&r[n / 2..], freq, SR as f64);
        assert!((a - b).abs() < 1e-6);
        db(a / 0.25)
    }

    #[test]
    fn default_parametric_is_flat() {
        let mut eq = ParametricEq::new(SR, 2);
        for f in [30.0, 250.0, 1000.0, 8000.0, 16000.0] {
            assert!(eq.response_db(f).abs() < 1e-9);
            assert!(measure(&mut eq, f).abs() < 0.01);
        }
    }

    #[test]
    fn parametric_bands_match_analytic_response() {
        let mut eq = ParametricEq::new(SR, 2);
        eq.set_param("b4.gain", 9.0);
        eq.set_param("b4.freq", 1000.0);
        eq.set_param("b4.q", 2.0);
        eq.set_param("b2.gain", -6.0);
        eq.set_param("b1.on", 1.0);
        eq.set_param("b1.freq", 40.0);
        eq.set_param("b8.on", 1.0);
        eq.set_param("b8.type", 5.0); // notch
        eq.set_param("b8.freq", 12000.0);
        eq.set_param("b8.q", 5.0);
        eq.set_param("output", 1.5);
        assert!((eq.response_db(1000.0) - (9.0 + 1.5 + eq.bank.response_db(1000.0) - 9.0)).abs() < 1e-9);
        for f in [30.0, 80.0, 400.0, 1000.0, 3000.0, 12000.0 * 1.2] {
            let m = measure(&mut eq, f);
            assert!((m - eq.response_db(f)).abs() < 0.05, "f {f}: measured {m} analytic {}", eq.response_db(f));
        }
        assert!(measure(&mut eq, 12000.0) < -30.0);
    }

    #[test]
    fn simple_eq_gains() {
        let mut eq = SimpleEq::new(SR, 2);
        eq.set_param("low", 6.0);
        eq.set_param("high", -6.0);
        eq.set_param("mid", 3.0);
        assert!((measure(&mut eq, 30.0) - eq.response_db(30.0)).abs() < 0.05);
        assert!((eq.response_db(20.0) - 6.0).abs() < 0.3);
        assert!((eq.response_db(20000.0) + 6.0).abs() < 0.5);
        assert!((measure(&mut eq, 1000.0) - eq.response_db(1000.0)).abs() < 0.05);
    }

    #[test]
    fn gain_change_glides_without_jump() {
        let mut eq = ParametricEq::new(SR, 1);
        let mut x = vec![0.5f32; 4800];
        eq.process(&mut [&mut x[..2400]]);
        eq.set_param("output", 12.0);
        eq.process(&mut [&mut x[2400..]]);
        let max_step = x.windows(2).map(|w| (w[1] - w[0]).abs()).fold(0.0, f32::max);
        assert!(max_step < 0.01, "step {max_step}");
        assert!((x[4799] - 0.5 * crate::db_to_gain(12.0)).abs() < 1e-4);
    }
}

#[cfg(test)]
impl BandBank {
    fn process_reference(&mut self, channels: &mut [&mut [f32]], out_gain: &mut Smoothed) {
        let n = block_len(channels);
        let nch = channels.len().min(self.state.len());
        for i in 0..n {
            if self.counter == 0 {
                self.update_coeffs(false);
            }
            self.counter = (self.counter + 1) % COEFF_INTERVAL;
            for b in &mut self.bands {
                b.freq.tick();
                b.gain.tick();
                b.q.tick();
                if b.freq.is_smoothing() || b.gain.is_smoothing() || b.q.is_smoothing() {
                    b.dirty = true;
                }
            }
            let g = out_gain.tick();
            // Tick the band mixes once per sample (shared by all channels).
            let mut mixes = [0.0f32; MAX_BANDS];
            for (m, b) in mixes.iter_mut().zip(self.bands.iter_mut()) {
                *m = b.mix.tick();
            }
            for ch in 0..nch {
                let mut x = channels[ch][i] as f64;
                for (k, b) in self.bands.iter().enumerate() {
                    let m = mixes[k] as f64;
                    let st = &mut self.state[ch][k];
                    if m > 0.0 {
                        let y = st.tick(&b.coeffs, x);
                        x += m * (y - x);
                    } else {
                        st.reset();
                    }
                }
                channels[ch][i] = (x * g as f64) as f32;
            }
        }
    }
}

#[cfg(test)]
mod throughput_tests {
    use super::*;
    #[test]
    fn flat_eq_fast_path_preserves_samples_and_filter_tails() {
        let mut fast = BandBank::new(48000.0, 2, 1);
        fast.set_band(0, BandSettings::new(true, FilterType::Peaking, 1000.0, 0.0, 0.7, true));
        fast.reset();
        let mut reference = fast.clone();
        let mut gain = Smoothed::new(1.0, 0);
        let mut ref_gain = gain;
        for input in
            [vec![-0.0, 0.0, f32::MIN_POSITIVE, f32::from_bits(1), f32::MAX, -f32::MAX, 0.7], vec![0.0; 100], vec![f32::INFINITY, f32::NEG_INFINITY, f32::NAN]]
        {
            let mut a = [input.clone(), input];
            let mut b = a.clone();
            let [al, ar] = &mut a;
            let [bl, br] = &mut b;
            fast.process(&mut [al, ar], &mut gain);
            reference.process_reference(&mut [bl, br], &mut ref_gain);
            for (a, b) in a.iter().flatten().zip(b.iter().flatten()) {
                assert_eq!(a.to_bits(), b.to_bits());
            }
        }
        // A newly flat filter must drain its old state, not drop the tail.
        fast.reset();
        fast.set_band(0, BandSettings::new(true, FilterType::Peaking, 1000.0, 12.0, 0.7, true));
        fast.update_coeffs(true);
        fast.process(&mut [&mut [0.5], &mut [0.5]], &mut gain);
        fast.set_band(0, BandSettings::new(true, FilterType::Peaking, 1000.0, 0.0, 0.7, true));
        fast.update_coeffs(true);
        reference = fast.clone();
        let mut a = [vec![0.0; 4096], vec![0.0; 4096]];
        let mut b = a.clone();
        let [al, ar] = &mut a;
        let [bl, br] = &mut b;
        fast.process(&mut [al, ar], &mut gain);
        reference.process_reference(&mut [bl, br], &mut ref_gain);
        assert_eq!(a, b);
        assert!(a[0].iter().any(|x| *x != 0.0));
    }
    #[test]
    fn settled_and_automated_eq_matches_original_loop() {
        let mut fast = BandBank::new(48000.0, 2, 3);
        fast.reset();
        let mut reference = fast.clone();
        let mut gain = Smoothed::with_ms(1.0, 48000.0, 20.0);
        let mut ref_gain = gain;
        for block in 0..500 {
            if matches!(block, 2 | 90 | 180 | 270 | 360) {
                let on = block != 180;
                let db = if block == 270 { 0.0 } else { 9.0 };
                for bank in [&mut fast, &mut reference] {
                    bank.set_band(0, BandSettings::new(on, FilterType::Peaking, 1733.0, db, 0.7, false));
                }
                gain.set(if block == 360 { 1.0 } else { 0.8 });
                ref_gain.set(gain.target());
            }
            let n = [0, 1, 17, 64, 513][block % 5];
            let input: Vec<f32> = (0..n).map(|i| if block > 400 { 0.0 } else { ((i + block * 13) as f32 * 0.17).sin() * 0.3 }).collect();
            let right: Vec<f32> = input.iter().enumerate().map(|(i, x)| if block > 450 { 0.0 } else { 0.5 * x + (i as f32 * 0.031).cos() * 0.2 }).collect();
            let mut a = [input, right];
            let mut b = a.clone();
            let [al, ar] = &mut a;
            let [bl, br] = &mut b;
            fast.process(&mut [al, ar], &mut gain);
            reference.process_reference(&mut [bl, br], &mut ref_gain);
            for (a, b) in a.iter().flatten().zip(b.iter().flatten()) {
                assert_eq!(a.to_bits(), b.to_bits(), "block {block}");
            }
            assert_eq!(fast.counter, reference.counter);
        }
    }
    #[test]
    fn settled_eq_pairs_channels_like_original_loop() {
        let kinds = [FilterType::HighPass, FilterType::LowShelf, FilterType::Peaking, FilterType::Notch, FilterType::HighShelf, FilterType::LowPass];
        for (flat, nch) in [(false, 1), (false, 2), (false, 3), (false, 5), (true, 3)] {
            let mut fast = BandBank::new(48000.0, nch, kinds.len());
            for (i, kind) in kinds.iter().enumerate() {
                let gain = if flat { 0.0 } else { 3.0 * i as f32 - 7.0 };
                let kind = if flat { FilterType::Peaking } else { *kind };
                fast.set_band(i, BandSettings::new(i != 2, kind, 40.0 * 3.1f32.powi(i as i32), gain, 0.5 + 0.4 * i as f32, true));
            }
            fast.reset();
            let mut reference = fast.clone();
            let mut gain = Smoothed::new(0.9, 0);
            let mut ref_gain = gain;
            for block in 0..40 {
                let n = [512, 1, 17, 0, 64][block % 5];
                let mut a: Vec<Vec<f32>> = (0..nch)
                    .map(|ch| {
                        (0..n)
                            .map(|i| match (ch, block) {
                                (1, 10..=12) if flat => f32::NAN,
                                (2, 20..) => 0.0,
                                _ => ((i + block * 31) as f32 * (0.05 + 0.07 * ch as f32)).sin() * 0.4,
                            })
                            .collect()
                    })
                    .collect();
                let mut b = a.clone();
                let mut ar: Vec<&mut [f32]> = a.iter_mut().map(|c| c.as_mut_slice()).collect();
                let mut br: Vec<&mut [f32]> = b.iter_mut().map(|c| c.as_mut_slice()).collect();
                fast.process(&mut ar, &mut gain);
                reference.process_reference(&mut br, &mut ref_gain);
                for (x, y) in a.iter().flatten().zip(b.iter().flatten()) {
                    assert_eq!(x.to_bits(), y.to_bits(), "nch {nch} flat {flat} block {block}");
                }
            }
        }
    }
}
