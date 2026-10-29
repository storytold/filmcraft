//! Filter and EQ effects: graphic equalisers (10/20/30 bands), the full parametric equaliser
//! (pass filters, shelves, five peaking bands), the six-notch filter, the Scientific Filter
//! (Bessel / Butterworth / Chebyshev / elliptic designs) and the FFT Filter (frequency-sampled
//! gain curve applied by STFT overlap-add).

use super::eq::{BandBank, BandSettings, MAX_BANDS};
use super::stft::{Spectral, StftChannel, fft_size};
use crate::biquad::{Biquad, FilterType};
use crate::design::{self, Band, Family, MAX_SECTIONS, Prototype, Sections};
use crate::{AudioEffect, ParamSpec, ParamValues, Smoothed, Unit, block_len, db_to_gain, param_plumbing};

const SMOOTH_MS: f32 = 30.0;

/// Q of a peaking band `octaves` wide (between the −3 dB points of the analog prototype).
pub fn q_for_octaves(octaves: f64) -> f64 {
    let p = 2f64.powf(octaves);
    p.sqrt() / (p - 1.0)
}

/// Copy `N` parameter-spec arrays into one flat array (const-evaluable).
const fn flatten<const K: usize, const N: usize, const M: usize>(groups: [[ParamSpec; K]; N]) -> [ParamSpec; M] {
    let mut out = [ParamSpec::new("", "", 0.0, 0.0, 0.0, Unit::None); M];
    let mut g = 0;
    while g < N {
        let mut k = 0;
        while k < K {
            out[g * K + k] = groups[g][k];
            k += 1;
        }
        g += 1;
    }
    out
}

/// `a` followed by `b` (const-evaluable).
const fn join<const A: usize, const B: usize, const M: usize>(a: [ParamSpec; A], b: [ParamSpec; B]) -> [ParamSpec; M] {
    let mut out = [ParamSpec::new("", "", 0.0, 0.0, 0.0, Unit::None); M];
    let mut i = 0;
    while i < A {
        out[i] = a[i];
        i += 1;
    }
    let mut j = 0;
    while j < B {
        out[A + j] = b[j];
        j += 1;
    }
    out
}

// ------------------------------------------------------------------------------ graphic EQ

/// Centre frequencies of the 10-band graphic equaliser.
pub const GEQ10_FREQS: [f32; 10] = [31.5, 63.0, 125.0, 250.0, 500.0, 1000.0, 2000.0, 4000.0, 8000.0, 16000.0];
const GEQ10_PARAMS: &[ParamSpec] = &[
    ParamSpec::new("b1", "31.5 Hz", -24.0, 24.0, 0.0, Unit::Decibels),
    ParamSpec::new("b2", "63 Hz", -24.0, 24.0, 0.0, Unit::Decibels),
    ParamSpec::new("b3", "125 Hz", -24.0, 24.0, 0.0, Unit::Decibels),
    ParamSpec::new("b4", "250 Hz", -24.0, 24.0, 0.0, Unit::Decibels),
    ParamSpec::new("b5", "500 Hz", -24.0, 24.0, 0.0, Unit::Decibels),
    ParamSpec::new("b6", "1 kHz", -24.0, 24.0, 0.0, Unit::Decibels),
    ParamSpec::new("b7", "2 kHz", -24.0, 24.0, 0.0, Unit::Decibels),
    ParamSpec::new("b8", "4 kHz", -24.0, 24.0, 0.0, Unit::Decibels),
    ParamSpec::new("b9", "8 kHz", -24.0, 24.0, 0.0, Unit::Decibels),
    ParamSpec::new("b10", "16 kHz", -24.0, 24.0, 0.0, Unit::Decibels),
    ParamSpec::new("gain", "Master Gain", -24.0, 24.0, 0.0, Unit::Decibels),
];
/// Centre frequencies of the 20-band graphic equaliser.
pub const GEQ20_FREQS: [f32; 20] =
    [31.5, 44.0, 63.0, 88.0, 125.0, 177.0, 250.0, 355.0, 500.0, 710.0, 1000.0, 1400.0, 2000.0, 2800.0, 4000.0, 5600.0, 8000.0, 11200.0, 16000.0, 22400.0];
const GEQ20_PARAMS: &[ParamSpec] = &[
    ParamSpec::new("b1", "31.5 Hz", -24.0, 24.0, 0.0, Unit::Decibels),
    ParamSpec::new("b2", "44 Hz", -24.0, 24.0, 0.0, Unit::Decibels),
    ParamSpec::new("b3", "63 Hz", -24.0, 24.0, 0.0, Unit::Decibels),
    ParamSpec::new("b4", "88 Hz", -24.0, 24.0, 0.0, Unit::Decibels),
    ParamSpec::new("b5", "125 Hz", -24.0, 24.0, 0.0, Unit::Decibels),
    ParamSpec::new("b6", "177 Hz", -24.0, 24.0, 0.0, Unit::Decibels),
    ParamSpec::new("b7", "250 Hz", -24.0, 24.0, 0.0, Unit::Decibels),
    ParamSpec::new("b8", "355 Hz", -24.0, 24.0, 0.0, Unit::Decibels),
    ParamSpec::new("b9", "500 Hz", -24.0, 24.0, 0.0, Unit::Decibels),
    ParamSpec::new("b10", "710 Hz", -24.0, 24.0, 0.0, Unit::Decibels),
    ParamSpec::new("b11", "1 kHz", -24.0, 24.0, 0.0, Unit::Decibels),
    ParamSpec::new("b12", "1.4 kHz", -24.0, 24.0, 0.0, Unit::Decibels),
    ParamSpec::new("b13", "2 kHz", -24.0, 24.0, 0.0, Unit::Decibels),
    ParamSpec::new("b14", "2.8 kHz", -24.0, 24.0, 0.0, Unit::Decibels),
    ParamSpec::new("b15", "4 kHz", -24.0, 24.0, 0.0, Unit::Decibels),
    ParamSpec::new("b16", "5.6 kHz", -24.0, 24.0, 0.0, Unit::Decibels),
    ParamSpec::new("b17", "8 kHz", -24.0, 24.0, 0.0, Unit::Decibels),
    ParamSpec::new("b18", "11.2 kHz", -24.0, 24.0, 0.0, Unit::Decibels),
    ParamSpec::new("b19", "16 kHz", -24.0, 24.0, 0.0, Unit::Decibels),
    ParamSpec::new("b20", "22.4 kHz", -24.0, 24.0, 0.0, Unit::Decibels),
    ParamSpec::new("gain", "Master Gain", -24.0, 24.0, 0.0, Unit::Decibels),
];
/// Centre frequencies of the 30-band graphic equaliser.
pub const GEQ30_FREQS: [f32; 30] = [
    25.0, 31.5, 40.0, 50.0, 63.0, 80.0, 100.0, 125.0, 160.0, 200.0, 250.0, 315.0, 400.0, 500.0, 630.0, 800.0, 1000.0, 1250.0, 1600.0, 2000.0, 2500.0, 3150.0,
    4000.0, 5000.0, 6300.0, 8000.0, 10000.0, 12500.0, 16000.0, 20000.0,
];
const GEQ30_PARAMS: &[ParamSpec] = &[
    ParamSpec::new("b1", "25 Hz", -24.0, 24.0, 0.0, Unit::Decibels),
    ParamSpec::new("b2", "31.5 Hz", -24.0, 24.0, 0.0, Unit::Decibels),
    ParamSpec::new("b3", "40 Hz", -24.0, 24.0, 0.0, Unit::Decibels),
    ParamSpec::new("b4", "50 Hz", -24.0, 24.0, 0.0, Unit::Decibels),
    ParamSpec::new("b5", "63 Hz", -24.0, 24.0, 0.0, Unit::Decibels),
    ParamSpec::new("b6", "80 Hz", -24.0, 24.0, 0.0, Unit::Decibels),
    ParamSpec::new("b7", "100 Hz", -24.0, 24.0, 0.0, Unit::Decibels),
    ParamSpec::new("b8", "125 Hz", -24.0, 24.0, 0.0, Unit::Decibels),
    ParamSpec::new("b9", "160 Hz", -24.0, 24.0, 0.0, Unit::Decibels),
    ParamSpec::new("b10", "200 Hz", -24.0, 24.0, 0.0, Unit::Decibels),
    ParamSpec::new("b11", "250 Hz", -24.0, 24.0, 0.0, Unit::Decibels),
    ParamSpec::new("b12", "315 Hz", -24.0, 24.0, 0.0, Unit::Decibels),
    ParamSpec::new("b13", "400 Hz", -24.0, 24.0, 0.0, Unit::Decibels),
    ParamSpec::new("b14", "500 Hz", -24.0, 24.0, 0.0, Unit::Decibels),
    ParamSpec::new("b15", "630 Hz", -24.0, 24.0, 0.0, Unit::Decibels),
    ParamSpec::new("b16", "800 Hz", -24.0, 24.0, 0.0, Unit::Decibels),
    ParamSpec::new("b17", "1 kHz", -24.0, 24.0, 0.0, Unit::Decibels),
    ParamSpec::new("b18", "1.25 kHz", -24.0, 24.0, 0.0, Unit::Decibels),
    ParamSpec::new("b19", "1.6 kHz", -24.0, 24.0, 0.0, Unit::Decibels),
    ParamSpec::new("b20", "2 kHz", -24.0, 24.0, 0.0, Unit::Decibels),
    ParamSpec::new("b21", "2.5 kHz", -24.0, 24.0, 0.0, Unit::Decibels),
    ParamSpec::new("b22", "3.15 kHz", -24.0, 24.0, 0.0, Unit::Decibels),
    ParamSpec::new("b23", "4 kHz", -24.0, 24.0, 0.0, Unit::Decibels),
    ParamSpec::new("b24", "5 kHz", -24.0, 24.0, 0.0, Unit::Decibels),
    ParamSpec::new("b25", "6.3 kHz", -24.0, 24.0, 0.0, Unit::Decibels),
    ParamSpec::new("b26", "8 kHz", -24.0, 24.0, 0.0, Unit::Decibels),
    ParamSpec::new("b27", "10 kHz", -24.0, 24.0, 0.0, Unit::Decibels),
    ParamSpec::new("b28", "12.5 kHz", -24.0, 24.0, 0.0, Unit::Decibels),
    ParamSpec::new("b29", "16 kHz", -24.0, 24.0, 0.0, Unit::Decibels),
    ParamSpec::new("b30", "20 kHz", -24.0, 24.0, 0.0, Unit::Decibels),
    ParamSpec::new("gain", "Master Gain", -24.0, 24.0, 0.0, Unit::Decibels),
];

/// Constant-Q graphic equaliser with 10 (octave), 20 (half-octave) or 30 (third-octave) bands.
pub struct GraphicEq<const N: usize> {
    pv: ParamValues,
    bank: BandBank,
    out: Smoothed,
}

impl<const N: usize> GraphicEq<N> {
    pub const PARAMS: &'static [ParamSpec] = match N {
        10 => GEQ10_PARAMS,
        20 => GEQ20_PARAMS,
        _ => GEQ30_PARAMS,
    };
    const ID: &'static str = match N {
        10 => "graphic_eq_10",
        20 => "graphic_eq_20",
        _ => "graphic_eq_30",
    };

    /// Band centre frequencies.
    pub fn freqs() -> &'static [f32] {
        match N {
            10 => &GEQ10_FREQS,
            20 => &GEQ20_FREQS,
            _ => &GEQ30_FREQS,
        }
    }

    /// Band Q (octave, half-octave or third-octave bandwidth).
    pub fn q() -> f32 {
        q_for_octaves(match N {
            10 => 1.0,
            20 => 0.5,
            _ => 1.0 / 3.0,
        }) as f32
    }

    pub fn new(sample_rate: f32, channels: usize) -> Self {
        assert!(N <= MAX_BANDS);
        let mut s = GraphicEq {
            pv: ParamValues::new(Self::PARAMS),
            bank: BandBank::new(sample_rate, channels, N),
            out: Smoothed::with_ms(1.0, sample_rate, SMOOTH_MS),
        };
        s.apply_params(true);
        s
    }

    fn apply_params(&mut self, snap: bool) {
        let q = Self::q();
        for (i, &f) in Self::freqs().iter().enumerate() {
            let g = self.pv.v(Self::PARAMS[i].id);
            self.bank.set_band(i, BandSettings::new(true, FilterType::Peaking, f, g, q, snap));
        }
        self.out.set(db_to_gain(self.pv.v("gain")));
        if snap {
            self.out.snap();
            self.bank.update_coeffs(true);
        }
    }
}

impl<const N: usize> AudioEffect for GraphicEq<N> {
    param_plumbing!(Self::ID);
    fn response_db(&self, freq: f64) -> Option<f64> {
        Some(self.bank.response_db(freq) + self.pv.v("gain") as f64)
    }
    fn reset(&mut self) {
        self.out.snap();
        self.bank.reset();
    }
    fn process(&mut self, channels: &mut [&mut [f32]]) {
        self.bank.process(channels, &mut self.out);
    }
}

// ----------------------------------------------------------------------- parametric EQ

/// Pass-filter slopes.
pub const SLOPES: &[&str] = &["12 dB/oct", "24 dB/oct", "36 dB/oct", "48 dB/oct"];

macro_rules! peq_band {
    ($p:literal, $name:literal, $f:expr, $q:expr) => {
        [
            ParamSpec::toggle(concat!($p, "_on"), concat!($name, " On"), true),
            ParamSpec::log(concat!($p, "_freq"), concat!($name, " Frequency"), 20.0, 20000.0, $f, Unit::Hertz),
            ParamSpec::new(concat!($p, "_gain"), concat!($name, " Gain"), -30.0, 30.0, 0.0, Unit::Decibels),
            ParamSpec::log(concat!($p, "_q"), concat!($name, " Q"), 0.1, 30.0, $q, Unit::Q),
        ]
    };
}

/// The bands of the full parametric equaliser between its pass filters, in signal order:
/// (parameter prefix, filter type).
pub const PEQ_BANDS: [(&str, FilterType); 7] = [
    ("low", FilterType::LowShelf),
    ("b1", FilterType::Peaking),
    ("b2", FilterType::Peaking),
    ("mid", FilterType::Peaking),
    ("b4", FilterType::Peaking),
    ("b5", FilterType::Peaking),
    ("high", FilterType::HighShelf),
];

const PEQ_BAND_IDS: [[&str; 4]; 7] = [
    ["low_on", "low_freq", "low_gain", "low_q"],
    ["b1_on", "b1_freq", "b1_gain", "b1_q"],
    ["b2_on", "b2_freq", "b2_gain", "b2_q"],
    ["mid_on", "mid_freq", "mid_gain", "mid_q"],
    ["b4_on", "b4_freq", "b4_gain", "b4_q"],
    ["b5_on", "b5_freq", "b5_gain", "b5_q"],
    ["high_on", "high_freq", "high_gain", "high_q"],
];

const PEQ_HEAD: [ParamSpec; 4] = [
    ParamSpec::new("master_gain", "Master Gain", -30.0, 30.0, 0.0, Unit::Decibels),
    ParamSpec::toggle("hp_on", "High Pass On", false),
    ParamSpec::log("hp_freq", "High Pass Frequency", 10.0, 20000.0, 30.0, Unit::Hertz),
    ParamSpec::choice("hp_slope", "High Pass Slope", SLOPES, 0),
];
const PEQ_TAIL: [ParamSpec; 3] = [
    ParamSpec::toggle("lp_on", "Low Pass On", false),
    ParamSpec::log("lp_freq", "Low Pass Frequency", 20.0, 22000.0, 18000.0, Unit::Hertz),
    ParamSpec::choice("lp_slope", "Low Pass Slope", SLOPES, 0),
];
const PEQ_MID: [ParamSpec; 28] = flatten([
    peq_band!("low", "Low Shelf", 100.0, 0.707),
    peq_band!("b1", "Band 1", 200.0, 1.0),
    peq_band!("b2", "Band 2", 500.0, 1.0),
    peq_band!("mid", "Band 3", 1000.0, 1.0),
    peq_band!("b4", "Band 4", 2000.0, 1.0),
    peq_band!("b5", "Band 5", 5000.0, 1.0),
    peq_band!("high", "High Shelf", 8000.0, 0.707),
]);
const PEQ_HEAD_MID: [ParamSpec; 32] = join(PEQ_HEAD, PEQ_MID);
const PEQ_PARAMS: [ParamSpec; 35] = join(PEQ_HEAD_MID, PEQ_TAIL);

/// Butterworth Q of section `k` (0-based) of an order-`2m` cascade.
pub fn butterworth_q(m: usize, k: usize) -> f32 {
    let n = 2 * m;
    (1.0 / (2.0 * (std::f64::consts::PI * (2 * k + 1) as f64 / (2 * n) as f64).cos())) as f32
}

/// Premiere-style parametric equaliser: master gain, high-pass (12–48 dB/oct), low shelf,
/// five peaking bands, high shelf and low-pass (12–48 dB/oct), all RBJ biquads.
pub struct FullParametricEq {
    pv: ParamValues,
    bank: BandBank,
    out: Smoothed,
}

impl FullParametricEq {
    pub const PARAMS: &'static [ParamSpec] = &PEQ_PARAMS;
    /// Bank layout: 4 high-pass slots, 7 bands, 4 low-pass slots.
    const SLOTS: usize = 4 + 7 + 4;

    pub fn new(sample_rate: f32, channels: usize) -> Self {
        let mut s = FullParametricEq {
            pv: ParamValues::new(Self::PARAMS),
            bank: BandBank::new(sample_rate, channels, Self::SLOTS),
            out: Smoothed::with_ms(1.0, sample_rate, SMOOTH_MS),
        };
        s.apply_params(true);
        s
    }

    fn apply_params(&mut self, snap: bool) {
        let p = &self.pv;
        let pass = |on: bool, slope: usize, slot: usize| -> (bool, f32) {
            let m = slope + 1;
            if on && slot < m { (true, butterworth_q(m, slot)) } else { (false, std::f32::consts::FRAC_1_SQRT_2) }
        };
        let (hp_on, hp_f, hp_s) = (p.on("hp_on"), p.v("hp_freq"), p.idx("hp_slope"));
        let (lp_on, lp_f, lp_s) = (p.on("lp_on"), p.v("lp_freq"), p.idx("lp_slope"));
        let mut bands = [(false, FilterType::Peaking, 1000.0, 0.0, 1.0); 7];
        for (i, ids) in PEQ_BAND_IDS.iter().enumerate() {
            bands[i] = (p.on(ids[0]), PEQ_BANDS[i].1, p.v(ids[1]), p.v(ids[2]), p.v(ids[3]));
        }
        for slot in 0..4 {
            let (on, q) = pass(hp_on, hp_s, slot);
            self.bank.set_band(slot, BandSettings::new(on, FilterType::HighPass, hp_f, 0.0, q, snap));
            let (on, q) = pass(lp_on, lp_s, slot);
            self.bank.set_band(11 + slot, BandSettings::new(on, FilterType::LowPass, lp_f, 0.0, q, snap));
        }
        for (i, (on, kind, f, g, q)) in bands.into_iter().enumerate() {
            self.bank.set_band(4 + i, BandSettings::new(on, kind, f, g, q, snap));
        }
        self.out.set(db_to_gain(self.pv.v("master_gain")));
        if snap {
            self.out.snap();
            self.bank.update_coeffs(true);
        }
    }
}

impl AudioEffect for FullParametricEq {
    param_plumbing!("parametric_eq_full");
    fn response_db(&self, freq: f64) -> Option<f64> {
        Some(self.bank.response_db(freq) + self.pv.v("master_gain") as f64)
    }
    fn reset(&mut self) {
        self.out.snap();
        self.bank.reset();
    }
    fn process(&mut self, channels: &mut [&mut [f32]]) {
        self.bank.process(channels, &mut self.out);
    }
}

// ---------------------------------------------------------------------------- notch filter

/// Notch widths.
pub const NOTCH_WIDTHS: &[&str] = &["Narrow", "Very Narrow", "Super Narrow"];
const NOTCH_Q: [f32; 3] = [8.0, 20.0, 50.0];

macro_rules! notch {
    ($n:literal, $f:expr) => {
        [
            ParamSpec::toggle(concat!("n", $n, "_on"), concat!("Notch ", $n, " On"), false),
            ParamSpec::log(concat!("n", $n, "_freq"), concat!("Notch ", $n, " Frequency"), 20.0, 20000.0, $f, Unit::Hertz),
            ParamSpec::new(concat!("n", $n, "_gain"), concat!("Notch ", $n, " Gain"), -90.0, 0.0, -30.0, Unit::Decibels),
        ]
    };
}

const NOTCH_BANDS: [ParamSpec; 18] =
    flatten([notch!("1", 60.0), notch!("2", 120.0), notch!("3", 180.0), notch!("4", 240.0), notch!("5", 300.0), notch!("6", 360.0)]);
const NOTCH_PARAMS: [ParamSpec; 19] = join(NOTCH_BANDS, [ParamSpec::choice("width", "Notch Width", NOTCH_WIDTHS, 0)]);

const NOTCH_IDS: [[&str; 3]; 6] = [
    ["n1_on", "n1_freq", "n1_gain"],
    ["n2_on", "n2_freq", "n2_gain"],
    ["n3_on", "n3_freq", "n3_gain"],
    ["n4_on", "n4_freq", "n4_gain"],
    ["n5_on", "n5_freq", "n5_gain"],
    ["n6_on", "n6_freq", "n6_gain"],
];

/// Up to six narrow cuts (constant-bandwidth [`FilterType::Cut`] biquads) of selectable width.
pub struct NotchFilter {
    pv: ParamValues,
    bank: BandBank,
    out: Smoothed,
}

impl NotchFilter {
    pub const PARAMS: &'static [ParamSpec] = &NOTCH_PARAMS;

    pub fn new(sample_rate: f32, channels: usize) -> Self {
        let mut s = NotchFilter { pv: ParamValues::new(Self::PARAMS), bank: BandBank::new(sample_rate, channels, 6), out: Smoothed::new(1.0, 0) };
        s.apply_params(true);
        s
    }

    fn apply_params(&mut self, snap: bool) {
        let q = NOTCH_Q[self.pv.idx("width").min(2)];
        for (i, ids) in NOTCH_IDS.iter().enumerate() {
            self.bank.set_band(i, BandSettings::new(self.pv.on(ids[0]), FilterType::Cut, self.pv.v(ids[1]), self.pv.v(ids[2]), q, snap));
        }
        if snap {
            self.bank.update_coeffs(true);
        }
    }
}

impl AudioEffect for NotchFilter {
    param_plumbing!("notch_filter");
    fn response_db(&self, freq: f64) -> Option<f64> {
        Some(self.bank.response_db(freq))
    }
    fn reset(&mut self) {
        self.bank.reset();
    }
    fn process(&mut self, channels: &mut [&mut [f32]]) {
        self.bank.process(channels, &mut self.out);
    }
}

// ----------------------------------------------------------------------- scientific filter

pub const SCI_TYPES: &[&str] = &["Bessel", "Butterworth", "Chebyshev", "Elliptical"];
pub const SCI_MODES: &[&str] = &["Low Pass", "High Pass", "Band Pass", "Band Stop"];

/// Classical IIR designs (see [`crate::design`]) as a cascade of biquads. Cutoff changes glide
/// (redesigned every 32 samples while moving); type/order/ripple changes switch immediately.
pub struct ScientificFilter {
    pv: ParamValues,
    sr: f32,
    proto_key: (usize, usize, f32, f32),
    proto: Prototype,
    band: Band,
    /// log2 cutoffs (smoothed so automation glides).
    f1: Smoothed,
    f2: Smoothed,
    sections: Sections,
    state: Vec<[Biquad; MAX_SECTIONS]>,
    gain: Smoothed,
    counter: u32,
    dirty: bool,
}

impl ScientificFilter {
    pub const PARAMS: &'static [ParamSpec] = &[
        ParamSpec::choice("type", "Type", SCI_TYPES, 1),
        ParamSpec::choice("mode", "Mode", SCI_MODES, 0),
        ParamSpec::new("order", "Order", 1.0, design::MAX_ORDER as f32, 6.0, Unit::None),
        ParamSpec::log("cutoff", "Cutoff", 20.0, 20000.0, 1000.0, Unit::Hertz),
        ParamSpec::log("high_cutoff", "High Cutoff", 20.0, 20000.0, 4000.0, Unit::Hertz),
        ParamSpec::new("ripple", "Passband Ripple", 0.01, 6.0, 1.0, Unit::Decibels),
        ParamSpec::new("stop_atten", "Stopband Attenuation", 20.0, 120.0, 60.0, Unit::Decibels),
        ParamSpec::new("gain", "Master Gain", -30.0, 30.0, 0.0, Unit::Decibels),
    ];

    pub fn new(sample_rate: f32, channels: usize) -> Self {
        let proto = design::prototype(Family::Butterworth, 1, 1.0, 60.0);
        let mut s = ScientificFilter {
            pv: ParamValues::new(Self::PARAMS),
            sr: sample_rate,
            proto_key: (usize::MAX, 0, 0.0, 0.0),
            proto,
            band: Band::LowPass,
            f1: Smoothed::with_ms(10.0, sample_rate, SMOOTH_MS),
            f2: Smoothed::with_ms(12.0, sample_rate, SMOOTH_MS),
            sections: design::realize(&proto, Band::LowPass, 1000.0, 2000.0, sample_rate as f64),
            state: vec![[Biquad::new(); MAX_SECTIONS]; channels.max(1)],
            gain: Smoothed::with_ms(1.0, sample_rate, SMOOTH_MS),
            counter: 0,
            dirty: true,
        };
        s.apply_params(true);
        s
    }

    fn family(&self) -> Family {
        [Family::Bessel, Family::Butterworth, Family::Chebyshev, Family::Elliptic][self.pv.idx("type").min(3)]
    }

    fn apply_params(&mut self, snap: bool) {
        let key = (self.pv.idx("type"), self.pv.v("order").round() as usize, self.pv.v("ripple"), self.pv.v("stop_atten"));
        if key != self.proto_key {
            self.proto_key = key;
            self.proto = design::prototype(self.family(), key.1, key.2 as f64, key.3 as f64);
        }
        self.band = [Band::LowPass, Band::HighPass, Band::BandPass, Band::BandStop][self.pv.idx("mode").min(3)];
        self.f1.set(self.pv.v("cutoff").log2());
        self.f2.set(self.pv.v("high_cutoff").log2());
        self.gain.set(db_to_gain(self.pv.v("gain")));
        self.dirty = true;
        if snap {
            self.f1.snap();
            self.f2.snap();
            self.gain.snap();
            self.redesign();
        }
    }

    fn redesign(&mut self) {
        let (f1, f2) = (2f64.powf(self.f1.value() as f64), 2f64.powf(self.f2.value() as f64));
        self.sections = design::realize(&self.proto, self.band, f1, f2, self.sr as f64);
        self.dirty = self.f1.is_smoothing() || self.f2.is_smoothing();
    }

    /// The designed cascade at the target settings.
    pub fn target_sections(&self) -> Sections {
        design::realize(&self.proto, self.band, self.pv.v("cutoff") as f64, self.pv.v("high_cutoff") as f64, self.sr as f64)
    }
}

impl AudioEffect for ScientificFilter {
    param_plumbing!("scientific_filter");
    fn response_db(&self, freq: f64) -> Option<f64> {
        Some(self.target_sections().magnitude_db(freq, self.sr as f64) + self.pv.v("gain") as f64)
    }
    fn reset(&mut self) {
        self.f1.snap();
        self.f2.snap();
        self.gain.snap();
        self.state.iter_mut().for_each(|s| s.iter_mut().for_each(Biquad::reset));
        self.counter = 0;
        self.redesign();
    }
    fn process(&mut self, channels: &mut [&mut [f32]]) {
        let n = block_len(channels);
        let nch = channels.len().min(self.state.len());
        for i in 0..n {
            if self.counter == 0 && self.dirty {
                self.redesign();
            }
            self.counter = (self.counter + 1) % 32;
            self.f1.tick();
            self.f2.tick();
            if self.f1.is_smoothing() || self.f2.is_smoothing() {
                self.dirty = true;
            }
            let g = self.gain.tick() as f64;
            let secs = self.sections.as_slice();
            for ch in 0..nch {
                let mut x = channels[ch][i] as f64;
                for (c, st) in secs.iter().zip(self.state[ch].iter_mut()) {
                    x = st.tick(c, x);
                }
                channels[ch][i] = (x * g) as f32;
            }
        }
    }
}

// ------------------------------------------------------------------------------ FFT filter

/// Number of FFT Filter curve points.
pub const FFT_POINTS: usize = 8;
const FFT_POINT_IDS: [[&str; 2]; FFT_POINTS] = [
    ["p1_freq", "p1_gain"],
    ["p2_freq", "p2_gain"],
    ["p3_freq", "p3_gain"],
    ["p4_freq", "p4_gain"],
    ["p5_freq", "p5_gain"],
    ["p6_freq", "p6_gain"],
    ["p7_freq", "p7_gain"],
    ["p8_freq", "p8_gain"],
];

macro_rules! fft_point {
    ($n:literal, $f:expr) => {
        [
            ParamSpec::log(concat!("p", $n, "_freq"), concat!("Point ", $n, " Frequency"), 20.0, 20000.0, $f, Unit::Hertz),
            ParamSpec::new(concat!("p", $n, "_gain"), concat!("Point ", $n, " Gain"), -60.0, 20.0, 0.0, Unit::Decibels),
        ]
    };
}

const FFT_POINT_PARAMS: [ParamSpec; 16] = flatten([
    fft_point!("1", 40.0),
    fft_point!("2", 100.0),
    fft_point!("3", 250.0),
    fft_point!("4", 600.0),
    fft_point!("5", 1500.0),
    fft_point!("6", 4000.0),
    fft_point!("7", 9000.0),
    fft_point!("8", 16000.0),
]);
const FFT_PARAMS: [ParamSpec; 17] = join(FFT_POINT_PARAMS, [ParamSpec::choice("interp", "Interpolation", &["Linear", "Smooth"], 1)]);

/// Gain (dB) of the FFT Filter curve at `freq` for points `(freq, gain_db)` (any order):
/// constant beyond the end points, interpolated in log-frequency between them (linearly or with
/// a smoothstep).
pub fn fft_curve_db(points: &[(f32, f32)], freq: f32, smooth: bool) -> f32 {
    let mut pts = [(0.0f32, 0.0f32); FFT_POINTS];
    let n = points.len().min(FFT_POINTS);
    pts[..n].copy_from_slice(&points[..n]);
    pts[..n].sort_by(|a, b| a.0.total_cmp(&b.0));
    let pts = &pts[..n];
    if pts.is_empty() {
        return 0.0;
    }
    if freq <= pts[0].0 {
        return pts[0].1;
    }
    for w in pts.windows(2) {
        if freq <= w[1].0 {
            let span = (w[1].0 / w[0].0).log2();
            let mut t = if span > 0.0 { (freq / w[0].0).log2() / span } else { 1.0 };
            if smooth {
                t = t * t * (3.0 - 2.0 * t);
            }
            return w[0].1 + (w[1].1 - w[0].1) * t;
        }
    }
    pts[n - 1].1
}

/// Zero-phase frequency-domain gain curve (8 points), applied with a √Hann STFT at 75 %
/// overlap (latency = FFT size). Curve changes glide over a few frames.
pub struct FftFilter {
    pv: ParamValues,
    sr: f32,
    n: usize,
    hop: usize,
    spec: Spectral,
    stft: Vec<StftChannel>,
    /// Target and current per-bin linear gains.
    target: Vec<f32>,
    gains: Vec<f32>,
}

impl FftFilter {
    pub const PARAMS: &'static [ParamSpec] = &FFT_PARAMS;

    pub fn new(sample_rate: f32, channels: usize) -> Self {
        let n = fft_size(sample_rate);
        let bins = n / 2 + 1;
        let mut s = FftFilter {
            pv: ParamValues::new(Self::PARAMS),
            sr: sample_rate,
            n,
            hop: n / 4,
            spec: Spectral::new(n, true),
            stft: vec![StftChannel::new(n); channels.max(1)],
            target: vec![1.0; bins],
            gains: vec![1.0; bins],
        };
        s.apply_params(true);
        s
    }

    fn points(&self) -> [(f32, f32); FFT_POINTS] {
        let mut p = [(0.0, 0.0); FFT_POINTS];
        for (i, ids) in FFT_POINT_IDS.iter().enumerate() {
            p[i] = (self.pv.v(ids[0]), self.pv.v(ids[1]));
        }
        p
    }

    fn apply_params(&mut self, snap: bool) {
        let pts = self.points();
        let smooth = self.pv.idx("interp") == 1;
        let bins = self.n / 2 + 1;
        for k in 0..bins {
            let f = (k as f32 * self.sr / self.n as f32).max(1.0);
            self.target[k] = db_to_gain(fft_curve_db(&pts, f, smooth));
        }
        if snap {
            self.gains.copy_from_slice(&self.target);
        }
    }
}

impl AudioEffect for FftFilter {
    param_plumbing!("fft_filter");
    fn latency(&self) -> usize {
        self.n
    }
    fn response_db(&self, freq: f64) -> Option<f64> {
        Some(fft_curve_db(&self.points(), freq as f32, self.pv.idx("interp") == 1) as f64)
    }
    fn reset(&mut self) {
        self.stft.iter_mut().for_each(StftChannel::reset);
        self.gains.copy_from_slice(&self.target);
    }
    fn process(&mut self, channels: &mut [&mut [f32]]) {
        let len = block_len(channels);
        let nch = channels.len().min(self.stft.len());
        let (n, hop) = (self.n, self.hop);
        let scale = 1.0 / (n as f32 * 2.0);
        let bins = n / 2 + 1;
        for i in 0..len {
            for ch in 0..nch {
                let spec = &mut self.spec;
                let gains = &mut self.gains;
                let target = &self.target;
                // Every channel frames at the same samples; the gains glide once per frame.
                let first = ch == 0;
                let mut frame = |input: &[f32], out: &mut [f32]| {
                    if first {
                        for k in 0..bins {
                            gains[k] += (target[k] - gains[k]) * 0.5;
                        }
                    }
                    spec.analyse(input);
                    for k in 0..bins {
                        spec.re[k] *= gains[k];
                        spec.im[k] *= gains[k];
                    }
                    spec.synthesise_add(out, scale);
                };
                channels[ch][i] = self.stft[ch].tick(channels[ch][i], hop, &mut frame);
            }
        }
    }
}
