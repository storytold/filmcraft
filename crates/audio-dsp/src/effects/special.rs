//! Special and stereo effects: Channel Mixer, Distortion, GuitarSuite, Mastering, Vocal
//! Enhancer, Stereo Expander, Binauralizer, Ambisonics Panner, Loudness Meter and Mute.

use super::multiband::soft_clip;
use super::time::Reverb;
use super::util::{DcBlock, DelayLine, OnePole, coef, lin_db};
use crate::biquad::{Biquad, Coeffs, FilterType};
use crate::loudness::LoudnessMeter;
use crate::{AudioEffect, ParamSpec, ParamValues, Smoothed, Unit, block_len, db_to_gain, flush32, param_plumbing};
use std::f32::consts::{FRAC_PI_2, PI};

const SM: f32 = 20.0;
const CHUNK: usize = 256;

fn sm(v: f32, sr: f32) -> Smoothed {
    Smoothed::with_ms(v, sr, SM)
}

// ------------------------------------------------------------------------------ channel mixer

/// 2×2 channel matrix with per-output polarity inversion.
pub struct ChannelMixer {
    pv: ParamValues,
    g: [Smoothed; 4],
}

impl ChannelMixer {
    pub const PARAMS: &'static [ParamSpec] = &[
        ParamSpec::new("l_from_l", "Left - Left", -100.0, 100.0, 100.0, Unit::Percent),
        ParamSpec::new("l_from_r", "Left - Right", -100.0, 100.0, 0.0, Unit::Percent),
        ParamSpec::new("r_from_l", "Right - Left", -100.0, 100.0, 0.0, Unit::Percent),
        ParamSpec::new("r_from_r", "Right - Right", -100.0, 100.0, 100.0, Unit::Percent),
        ParamSpec::toggle("invert_l", "Invert Left", false),
        ParamSpec::toggle("invert_r", "Invert Right", false),
    ];

    pub fn new(sample_rate: f32, _channels: usize) -> Self {
        let mut s =
            ChannelMixer { pv: ParamValues::new(Self::PARAMS), g: [sm(1.0, sample_rate), sm(0.0, sample_rate), sm(0.0, sample_rate), sm(1.0, sample_rate)] };
        s.apply_params(true);
        s
    }

    fn apply_params(&mut self, snap: bool) {
        let il = if self.pv.on("invert_l") { -1.0 } else { 1.0 };
        let ir = if self.pv.on("invert_r") { -1.0 } else { 1.0 };
        let v =
            [il * self.pv.v("l_from_l") / 100.0, il * self.pv.v("l_from_r") / 100.0, ir * self.pv.v("r_from_l") / 100.0, ir * self.pv.v("r_from_r") / 100.0];
        for (s, v) in self.g.iter_mut().zip(v) {
            s.set(v);
            if snap {
                s.snap();
            }
        }
    }
}

impl AudioEffect for ChannelMixer {
    param_plumbing!("channel_mixer");
    fn reset(&mut self) {
        self.g.iter_mut().for_each(Smoothed::snap);
    }
    fn process(&mut self, channels: &mut [&mut [f32]]) {
        let n = block_len(channels);
        if channels.len() < 2 {
            for i in 0..n {
                let g: Vec4 = self.tick();
                channels[0][i] *= g[0] + g[1];
            }
            return;
        }
        let (a, b) = channels.split_at_mut(1);
        let (l, r) = (&mut a[0], &mut b[0]);
        for i in 0..n {
            let g = self.tick();
            let (x, y) = (l[i], r[i]);
            l[i] = g[0] * x + g[1] * y;
            r[i] = g[2] * x + g[3] * y;
        }
    }
}

type Vec4 = [f32; 4];

impl ChannelMixer {
    #[inline]
    fn tick(&mut self) -> Vec4 {
        [self.g[0].tick(), self.g[1].tick(), self.g[2].tick(), self.g[3].tick()]
    }
}

// --------------------------------------------------------------------------------- distortion

pub const DIST_CURVES: &[&str] = &["Soft Clip", "Hard Clip", "Tube", "Foldback"];

/// Memoryless transfer curves (unity small-signal slope).
#[inline]
pub fn shape(kind: usize, v: f32) -> f32 {
    match kind {
        0 => v.tanh(),
        1 => v.clamp(-1.0, 1.0),
        2 => {
            if v >= 0.0 {
                v.tanh()
            } else {
                (1.6 * v).tanh() / 1.6
            }
        }
        _ => v.sin(),
    }
}

/// Waveshaping distortion: drive, curve, asymmetry (bias, DC-blocked), tone low-pass, output,
/// dry/wet mix.
pub struct Distortion {
    pv: ParamValues,
    sr: f32,
    dc: Vec<DcBlock>,
    lp: Vec<OnePole>,
    drive: Smoothed,
    bias: Smoothed,
    out: Smoothed,
    mix: Smoothed,
}

impl Distortion {
    pub const PARAMS: &'static [ParamSpec] = &[
        ParamSpec::new("drive", "Drive", 0.0, 48.0, 12.0, Unit::Decibels),
        ParamSpec::choice("curve", "Curve", DIST_CURVES, 0),
        ParamSpec::new("symmetry", "Asymmetry", -100.0, 100.0, 0.0, Unit::Percent),
        ParamSpec::log("tone", "Tone", 500.0, 20000.0, 20000.0, Unit::Hertz),
        ParamSpec::new("output", "Output Gain", -48.0, 12.0, 0.0, Unit::Decibels),
        ParamSpec::new("mix", "Mix", 0.0, 100.0, 100.0, Unit::Percent),
    ];

    pub fn new(sample_rate: f32, channels: usize) -> Self {
        let ch = channels.max(1);
        let mut s = Distortion {
            pv: ParamValues::new(Self::PARAMS),
            sr: sample_rate,
            dc: vec![DcBlock::default(); ch],
            lp: vec![OnePole::default(); ch],
            drive: sm(1.0, sample_rate),
            bias: sm(0.0, sample_rate),
            out: sm(1.0, sample_rate),
            mix: sm(1.0, sample_rate),
        };
        s.apply_params(true);
        s
    }

    fn apply_params(&mut self, snap: bool) {
        self.drive.set(db_to_gain(self.pv.v("drive")));
        self.bias.set(0.5 * self.pv.v("symmetry") / 100.0);
        self.out.set(db_to_gain(self.pv.v("output")));
        self.mix.set(self.pv.v("mix") / 100.0);
        if snap {
            for s in [&mut self.drive, &mut self.bias, &mut self.out, &mut self.mix] {
                s.snap();
            }
        }
    }
}

impl AudioEffect for Distortion {
    param_plumbing!("distortion");
    fn reset(&mut self) {
        self.dc.iter_mut().for_each(DcBlock::reset);
        self.lp.iter_mut().for_each(OnePole::reset);
        for s in [&mut self.drive, &mut self.bias, &mut self.out, &mut self.mix] {
            s.snap();
        }
    }
    fn process(&mut self, channels: &mut [&mut [f32]]) {
        let n = block_len(channels);
        let nch = channels.len().min(self.dc.len());
        let kind = self.pv.idx("curve");
        let tone = self.pv.v("tone");
        let a = OnePole::alpha(tone, self.sr);
        let r = DcBlock::r(self.sr);
        for i in 0..n {
            let (d, b, o, m) = (self.drive.tick(), self.bias.tick(), self.out.tick(), self.mix.tick());
            for ch in 0..nch {
                let x = channels[ch][i];
                let mut y = shape(kind, d * x + b) - shape(kind, b);
                y = self.dc[ch].tick(r, y);
                if tone < 19999.0 {
                    y = self.lp[ch].lp(a, y);
                }
                channels[ch][i] = x + m * (y * o - x);
            }
        }
    }
}

// -------------------------------------------------------------------------------- GuitarSuite

pub const GS_AMPS: &[&str] = &["None", "Clean Combo", "British Stack", "Tweed", "Modern High Gain"];
pub const GS_FILTERS: &[&str] = &["None", "Low Pass", "High Pass", "Band Pass"];
pub const GS_DIST: &[&str] = &["Soft", "Hard", "Fuzz"];

/// Guitar chain: compressor → distortion → amplifier/cabinet voicing → filter, with mix and
/// output gain.
pub struct GuitarSuite {
    pv: ParamValues,
    sr: f32,
    env: f32,
    cab: [Coeffs; 4],
    filt: Coeffs,
    st: Vec<[Biquad; 5]>,
    dc: Vec<DcBlock>,
    mix: Smoothed,
    out: Smoothed,
    drive: Smoothed,
}

impl GuitarSuite {
    pub const PARAMS: &'static [ParamSpec] = &[
        ParamSpec::new("compressor", "Compressor", 0.0, 100.0, 30.0, Unit::Percent),
        ParamSpec::new("distortion", "Distortion", 0.0, 100.0, 40.0, Unit::Percent),
        ParamSpec::choice("dist_type", "Distortion Type", GS_DIST, 0),
        ParamSpec::choice("amp", "Amplifier", GS_AMPS, 1),
        ParamSpec::choice("filter", "Filter", GS_FILTERS, 0),
        ParamSpec::log("filter_freq", "Filter Frequency", 100.0, 10000.0, 2000.0, Unit::Hertz),
        ParamSpec::new("filter_res", "Filter Resonance", 0.0, 100.0, 20.0, Unit::Percent),
        ParamSpec::new("mix", "Mix", 0.0, 100.0, 100.0, Unit::Percent),
        ParamSpec::new("output", "Output Gain", -24.0, 12.0, 0.0, Unit::Decibels),
    ];

    pub fn new(sample_rate: f32, channels: usize) -> Self {
        let ch = channels.max(1);
        let mut s = GuitarSuite {
            pv: ParamValues::new(Self::PARAMS),
            sr: sample_rate,
            env: 0.0,
            cab: [Coeffs::IDENTITY; 4],
            filt: Coeffs::IDENTITY,
            st: vec![[Biquad::new(); 5]; ch],
            dc: vec![DcBlock::default(); ch],
            mix: sm(1.0, sample_rate),
            out: sm(1.0, sample_rate),
            drive: sm(1.0, sample_rate),
        };
        s.apply_params(true);
        s
    }

    fn apply_params(&mut self, snap: bool) {
        let sr = self.sr as f64;
        let d = |k, f, q, g| Coeffs::design(k, sr, f, q, g);
        use FilterType::*;
        self.cab = match self.pv.idx("amp") {
            0 => [Coeffs::IDENTITY; 4],
            1 => [d(HighPass, 70.0, 0.7, 0.0), d(Peaking, 400.0, 0.8, -2.0), d(Peaking, 2500.0, 1.0, 3.0), d(LowPass, 6000.0, 0.8, 0.0)],
            2 => [d(HighPass, 90.0, 0.7, 0.0), d(Peaking, 800.0, 0.7, 4.0), d(Peaking, 3000.0, 1.2, 2.0), d(LowPass, 4500.0, 0.9, 0.0)],
            3 => [d(HighPass, 60.0, 0.7, 0.0), d(LowShelf, 200.0, 0.7, 3.0), d(Peaking, 1200.0, 0.8, 2.0), d(LowPass, 4000.0, 0.7, 0.0)],
            _ => [d(HighPass, 100.0, 0.8, 0.0), d(Peaking, 500.0, 1.0, -5.0), d(Peaking, 2000.0, 0.8, 5.0), d(LowPass, 5000.0, 1.0, 0.0)],
        };
        let q = 0.5 + 7.5 * self.pv.v("filter_res") / 100.0;
        let f = self.pv.v("filter_freq") as f64;
        self.filt = match self.pv.idx("filter") {
            1 => d(LowPass, f, q as f64, 0.0),
            2 => d(HighPass, f, q as f64, 0.0),
            3 => d(BandPass, f, q as f64, 0.0),
            _ => Coeffs::IDENTITY,
        };
        self.mix.set(self.pv.v("mix") / 100.0);
        self.out.set(db_to_gain(self.pv.v("output")));
        self.drive.set(db_to_gain(36.0 * self.pv.v("distortion") / 100.0));
        if snap {
            self.mix.snap();
            self.out.snap();
            self.drive.snap();
        }
    }
}

impl AudioEffect for GuitarSuite {
    param_plumbing!("guitar_suite");
    fn reset(&mut self) {
        self.env = 0.0;
        self.st.iter_mut().for_each(|s| s.iter_mut().for_each(Biquad::reset));
        self.dc.iter_mut().for_each(DcBlock::reset);
        self.mix.snap();
        self.out.snap();
        self.drive.snap();
    }
    fn process(&mut self, channels: &mut [&mut [f32]]) {
        let n = block_len(channels);
        let nch = channels.len().min(self.st.len());
        let comp = self.pv.v("compressor") / 100.0;
        let (thr, ratio) = (-6.0 - 30.0 * comp, 1.0 + 7.0 * comp);
        let (att, rel) = (coef(5.0, self.sr), coef(120.0, self.sr));
        let kind = match self.pv.idx("dist_type") {
            0 => 0,
            1 => 1,
            _ => 2,
        };
        let r = DcBlock::r(self.sr);
        for i in 0..n {
            let (mix, out, drive) = (self.mix.tick(), self.out.tick(), self.drive.tick());
            let peak = channels[..nch].iter().fold(0.0f32, |a, c| a.max(c[i].abs()));
            let l = lin_db(peak);
            let target = if comp > 0.0 && l > thr { (thr + (l - thr) / ratio) - l } else { 0.0 };
            let c = if target < self.env { att } else { rel };
            self.env = c * self.env + (1.0 - c) * target;
            if self.env.abs() < 1e-7 {
                self.env = 0.0;
            }
            let g = db_to_gain(self.env);
            // Distortion level compensation keeps the output near the input level.
            let comp_out = 1.0 / drive.sqrt();
            for ch in 0..nch {
                let x = channels[ch][i];
                let v = x * g * drive;
                let mut y = match kind {
                    0 => v.tanh(),
                    1 => v.clamp(-1.0, 1.0),
                    _ => (v + 0.3).tanh() - 0.3f32.tanh() + 0.5 * (2.0 * v).tanh(),
                } * comp_out;
                y = self.dc[ch].tick(r, y);
                let st = &mut self.st[ch];
                let mut yd = y as f64;
                for (k, c) in self.cab.iter().enumerate() {
                    yd = st[k].tick(c, yd);
                }
                yd = st[4].tick(&self.filt, yd);
                channels[ch][i] = x + mix * (yd as f32 * out - x);
            }
        }
    }
}

// --------------------------------------------------------------------------------- mastering

pub const EXCITER_MODES: &[&str] = &["Retro", "Tape", "Tube"];

/// Mastering chain: three-band EQ → reverb → exciter → widener → loudness maximiser → output.
/// At its defaults it is transparent.
pub struct Mastering {
    pv: ParamValues,
    sr: f32,
    eq: [Coeffs; 3],
    st: Vec<[Biquad; 3]>,
    rev: Reverb,
    scratch: [[f32; CHUNK]; 2],
    hp: Vec<OnePole>,
    lim_env: f32,
    rev_amt: Smoothed,
    exc: Smoothed,
    width: Smoothed,
    boost: Smoothed,
    out: Smoothed,
}

impl Mastering {
    pub const PARAMS: &'static [ParamSpec] = &[
        ParamSpec::new("eq_low", "Low Shelf Gain", -12.0, 12.0, 0.0, Unit::Decibels),
        ParamSpec::log("eq_mid_freq", "Peak Frequency", 100.0, 10000.0, 1000.0, Unit::Hertz),
        ParamSpec::new("eq_mid", "Peak Gain", -12.0, 12.0, 0.0, Unit::Decibels),
        ParamSpec::new("eq_high", "High Shelf Gain", -12.0, 12.0, 0.0, Unit::Decibels),
        ParamSpec::new("reverb", "Reverb Amount", 0.0, 100.0, 0.0, Unit::Percent),
        ParamSpec::new("exciter", "Exciter Amount", 0.0, 100.0, 0.0, Unit::Percent),
        ParamSpec::choice("exciter_mode", "Exciter Mode", EXCITER_MODES, 1),
        ParamSpec::new("widener", "Widener", 0.0, 200.0, 100.0, Unit::Percent),
        ParamSpec::new("loudness", "Loudness Maximizer", 0.0, 100.0, 0.0, Unit::Percent),
        ParamSpec::new("output", "Output Gain", -24.0, 12.0, 0.0, Unit::Decibels),
    ];

    pub fn new(sample_rate: f32, channels: usize) -> Self {
        let ch = channels.max(1);
        let mut rev = Reverb::new(sample_rate, 2);
        rev.set_param("mix", 100.0);
        rev.set_param("decay", 1.4);
        rev.set_param("size", 40.0);
        rev.reset();
        let mut s = Mastering {
            pv: ParamValues::new(Self::PARAMS),
            sr: sample_rate,
            eq: [Coeffs::IDENTITY; 3],
            st: vec![[Biquad::new(); 3]; ch],
            rev,
            scratch: [[0.0; CHUNK]; 2],
            hp: vec![OnePole::default(); ch],
            lim_env: 0.0,
            rev_amt: sm(0.0, sample_rate),
            exc: sm(0.0, sample_rate),
            width: sm(1.0, sample_rate),
            boost: sm(1.0, sample_rate),
            out: sm(1.0, sample_rate),
        };
        s.apply_params(true);
        s
    }

    fn apply_params(&mut self, snap: bool) {
        let sr = self.sr as f64;
        let q = std::f64::consts::FRAC_1_SQRT_2;
        self.eq = [
            Coeffs::design(FilterType::LowShelf, sr, 100.0, q, self.pv.v("eq_low") as f64),
            Coeffs::design(FilterType::Peaking, sr, self.pv.v("eq_mid_freq") as f64, 1.0, self.pv.v("eq_mid") as f64),
            Coeffs::design(FilterType::HighShelf, sr, 8000.0, q, self.pv.v("eq_high") as f64),
        ];
        self.rev_amt.set(0.4 * self.pv.v("reverb") / 100.0);
        self.exc.set(0.6 * self.pv.v("exciter") / 100.0);
        self.width.set(self.pv.v("widener") / 100.0);
        self.boost.set(db_to_gain(12.0 * self.pv.v("loudness") / 100.0));
        self.out.set(db_to_gain(self.pv.v("output")));
        if snap {
            for s in [&mut self.rev_amt, &mut self.exc, &mut self.width, &mut self.boost, &mut self.out] {
                s.snap();
            }
        }
    }
}

impl AudioEffect for Mastering {
    param_plumbing!("mastering");
    fn response_db(&self, freq: f64) -> Option<f64> {
        Some(self.eq.iter().map(|c| c.magnitude_db(freq, self.sr as f64)).sum::<f64>() + self.pv.v("output") as f64)
    }
    fn reset(&mut self) {
        self.st.iter_mut().for_each(|s| s.iter_mut().for_each(Biquad::reset));
        self.rev.reset();
        self.hp.iter_mut().for_each(OnePole::reset);
        self.lim_env = 0.0;
        for s in [&mut self.rev_amt, &mut self.exc, &mut self.width, &mut self.boost, &mut self.out] {
            s.snap();
        }
    }
    fn process(&mut self, channels: &mut [&mut [f32]]) {
        let n = block_len(channels);
        let nch = channels.len().min(self.st.len());
        if nch == 0 {
            return;
        }
        let loud = self.pv.v("loudness") > 0.0;
        let use_rev = self.pv.v("reverb") > 0.0;
        let mode = self.pv.idx("exciter_mode");
        let ahp = OnePole::alpha(3000.0, self.sr);
        let ceiling = db_to_gain(-0.3);
        let rel = coef(60.0, self.sr);
        let mut start = 0;
        while start < n {
            let len = (n - start).min(CHUNK);
            // EQ in place.
            for ch in 0..nch {
                for v in channels[ch][start..start + len].iter_mut() {
                    let mut y = *v as f64;
                    for (k, c) in self.eq.iter().enumerate() {
                        y = self.st[ch][k].tick(c, y);
                    }
                    *v = y as f32;
                }
            }
            if use_rev {
                for i in 0..len {
                    self.scratch[0][i] = channels[0][start + i];
                    self.scratch[1][i] = channels[nch.min(2) - 1][start + i];
                }
                let [a, b] = &mut self.scratch;
                self.rev.process(&mut [&mut a[..len], &mut b[..len]]);
            }
            for i in 0..len {
                let (ra, ex, w, boost, out) = (self.rev_amt.tick(), self.exc.tick(), self.width.tick(), self.boost.tick(), self.out.tick());
                let mut y = [0.0f32; 2];
                for ch in 0..nch.min(2) {
                    let mut v = channels[ch][start + i];
                    if use_rev {
                        v += ra * self.scratch[ch][i];
                    }
                    if ex > 0.0 {
                        let h = self.hp[ch].hp(ahp, v);
                        let s = match mode {
                            0 => (3.0 * h).clamp(-1.0, 1.0) / 3.0,
                            1 => (3.0 * h).tanh() / 3.0,
                            _ => ((3.0 * h + 0.2).tanh() - 0.2f32.tanh()) / 3.0,
                        };
                        v += ex * (s - h * 0.5) * 2.0;
                    }
                    y[ch] = v;
                }
                if nch >= 2 {
                    let (m, s) = (0.5 * (y[0] + y[1]), 0.5 * (y[0] - y[1]) * w);
                    y = [m + s, m - s];
                }
                if loud {
                    for v in y.iter_mut().take(nch.min(2)) {
                        *v *= boost;
                    }
                    let peak = y[..nch.min(2)].iter().fold(0.0f32, |a, v| a.max(v.abs()));
                    let target = (lin_db(ceiling) - lin_db(peak)).min(0.0);
                    if target < self.lim_env {
                        self.lim_env = target;
                    } else {
                        self.lim_env = rel * self.lim_env + (1.0 - rel) * target;
                    }
                    let g = db_to_gain(self.lim_env);
                    for v in y.iter_mut().take(nch.min(2)) {
                        *v = soft_clip(*v * g, ceiling);
                    }
                }
                for ch in 0..nch.min(2) {
                    channels[ch][start + i] = y[ch] * out;
                }
            }
            start += len;
        }
    }
}

// ---------------------------------------------------------------------------- vocal enhancer

pub const VOCAL_MODES: &[&str] = &["Male", "Female", "Music"];

/// Fixed voicings: Male / Female (rumble cut, de-mud, presence and air, gentle 2:1
/// compression) or Music (carves the vocal presence range so a voice sits on top).
pub struct VocalEnhancer {
    pv: ParamValues,
    sr: f32,
    eq: [Coeffs; 4],
    st: Vec<[Biquad; 4]>,
    env: f32,
}

impl VocalEnhancer {
    pub const PARAMS: &'static [ParamSpec] = &[ParamSpec::choice("mode", "Mode", VOCAL_MODES, 0)];

    pub fn new(sample_rate: f32, channels: usize) -> Self {
        let mut s = VocalEnhancer {
            pv: ParamValues::new(Self::PARAMS),
            sr: sample_rate,
            eq: [Coeffs::IDENTITY; 4],
            st: vec![[Biquad::new(); 4]; channels.max(1)],
            env: 0.0,
        };
        s.apply_params(true);
        s
    }

    fn apply_params(&mut self, _snap: bool) {
        let sr = self.sr as f64;
        let d = |k, f, q, g| Coeffs::design(k, sr, f, q, g);
        use FilterType::*;
        self.eq = match self.pv.idx("mode") {
            0 => [d(HighPass, 80.0, 0.707, 0.0), d(Peaking, 250.0, 1.0, -3.0), d(Peaking, 3000.0, 1.0, 3.0), d(HighShelf, 10000.0, 0.707, 1.5)],
            1 => [d(HighPass, 120.0, 0.707, 0.0), d(Peaking, 400.0, 1.0, -2.0), d(Peaking, 5000.0, 1.0, 3.0), d(HighShelf, 10000.0, 0.707, 2.0)],
            _ => [d(HighPass, 40.0, 0.707, 0.0), d(Peaking, 1500.0, 0.7, -4.0), d(Peaking, 3500.0, 1.0, -2.0), Coeffs::IDENTITY],
        };
    }
}

impl AudioEffect for VocalEnhancer {
    param_plumbing!("vocal_enhancer");
    fn response_db(&self, freq: f64) -> Option<f64> {
        Some(self.eq.iter().map(|c| c.magnitude_db(freq, self.sr as f64)).sum())
    }
    fn reset(&mut self) {
        self.st.iter_mut().for_each(|s| s.iter_mut().for_each(Biquad::reset));
        self.env = 0.0;
    }
    fn process(&mut self, channels: &mut [&mut [f32]]) {
        let n = block_len(channels);
        let nch = channels.len().min(self.st.len());
        let voice = self.pv.idx("mode") < 2;
        let (att, rel) = (coef(5.0, self.sr), coef(150.0, self.sr));
        for i in 0..n {
            for ch in 0..nch {
                let mut y = channels[ch][i] as f64;
                for (k, c) in self.eq.iter().enumerate() {
                    y = self.st[ch][k].tick(c, y);
                }
                channels[ch][i] = y as f32;
            }
            if voice {
                let peak = channels[..nch].iter().fold(0.0f32, |a, c| a.max(c[i].abs()));
                let l = lin_db(peak);
                let target = if l > -18.0 { (-18.0 + (l + 18.0) / 2.0) - l } else { 0.0 };
                let c = if target < self.env { att } else { rel };
                self.env = c * self.env + (1.0 - c) * target;
                if self.env.abs() < 1e-7 {
                    self.env = 0.0;
                }
                let g = db_to_gain(self.env);
                for ch in channels[..nch].iter_mut() {
                    ch[i] *= g;
                }
            }
        }
    }
}

// --------------------------------------------------------------------------- stereo expander

/// Mid/side: the side signal is scaled by Stereo Expand and the centre (mid) signal is panned.
pub struct StereoExpander {
    pv: ParamValues,
    gl: Smoothed,
    gr: Smoothed,
    side: Smoothed,
}

impl StereoExpander {
    pub const PARAMS: &'static [ParamSpec] = &[
        ParamSpec::new("center_pan", "Center Channel Pan", -100.0, 100.0, 0.0, Unit::Pan),
        ParamSpec::new("expand", "Stereo Expand", 0.0, 300.0, 100.0, Unit::Percent),
    ];

    pub fn new(sample_rate: f32, _channels: usize) -> Self {
        let mut s = StereoExpander { pv: ParamValues::new(Self::PARAMS), gl: sm(1.0, sample_rate), gr: sm(1.0, sample_rate), side: sm(1.0, sample_rate) };
        s.apply_params(true);
        s
    }

    fn apply_params(&mut self, snap: bool) {
        let p = self.pv.v("center_pan") / 100.0;
        self.gl.set((1.0 - p).min(1.0));
        self.gr.set((1.0 + p).min(1.0));
        self.side.set(self.pv.v("expand") / 100.0);
        if snap {
            for s in [&mut self.gl, &mut self.gr, &mut self.side] {
                s.snap();
            }
        }
    }
}

impl AudioEffect for StereoExpander {
    param_plumbing!("stereo_expander");
    fn reset(&mut self) {
        for s in [&mut self.gl, &mut self.gr, &mut self.side] {
            s.snap();
        }
    }
    fn process(&mut self, channels: &mut [&mut [f32]]) {
        let n = block_len(channels);
        if channels.len() < 2 {
            return;
        }
        let (a, b) = channels.split_at_mut(1);
        let (l, r) = (&mut a[0], &mut b[0]);
        for i in 0..n {
            let (gl, gr, w) = (self.gl.tick(), self.gr.tick(), self.side.tick());
            let (m, s) = (0.5 * (l[i] + r[i]), 0.5 * (l[i] - r[i]) * w);
            l[i] = m * gl + s;
            r[i] = m * gr - s;
        }
    }
}

// ------------------------------------------------------------------------------ binauralizer

const SPEED_OF_SOUND: f32 = 343.0;

/// Head-shadow filter (one-pole/one-zero spherical-head model, Brown & Duda 1998):
/// H(s) = (1 + α s / 2ω₀) / (1 + s / 2ω₀), ω₀ = c / a, α from the angle of incidence.
#[derive(Clone, Copy, Debug, Default)]
struct Shadow {
    b0: f32,
    b1: f32,
    a1: f32,
    x1: f32,
    y1: f32,
}

impl Shadow {
    fn design(incidence_deg: f32, radius: f32, sr: f32) -> Shadow {
        let alpha = 1.05 + 0.95 * (incidence_deg / 150.0 * PI).cos();
        let w0 = SPEED_OF_SOUND / radius;
        let k = 2.0 * sr;
        let t = k / (2.0 * w0);
        let a0 = 1.0 + t;
        Shadow { b0: (1.0 + alpha * t) / a0, b1: (1.0 - alpha * t) / a0, a1: (1.0 - t) / a0, x1: 0.0, y1: 0.0 }
    }
    #[inline]
    fn tick(&mut self, x: f32) -> f32 {
        let y = self.b0 * x + self.b1 * self.x1 - self.a1 * self.y1;
        self.x1 = x;
        self.y1 = flush32(y);
        y
    }
    fn set_coeffs(&mut self, o: &Shadow) {
        self.b0 = o.b0;
        self.b1 = o.b1;
        self.a1 = o.a1;
    }
}

/// Renders the stereo input as two virtual loudspeakers at ±`angle` heard through a spherical
/// head model (Woodworth interaural time difference + head-shadow filtering), for headphones.
pub struct Binauralizer {
    pv: ParamValues,
    sr: f32,
    lines: [DelayLine; 2],
    /// near-L, far-L→R, near-R, far-R→L
    sh: [Shadow; 4],
    itd: Smoothed,
    mix: Smoothed,
}

impl Binauralizer {
    pub const PARAMS: &'static [ParamSpec] = &[
        ParamSpec::new("angle", "Speaker Angle", 0.0, 90.0, 30.0, Unit::None),
        ParamSpec::new("head_size", "Head Size", 12.0, 24.0, 17.5, Unit::None),
        ParamSpec::new("mix", "Mix", 0.0, 100.0, 100.0, Unit::Percent),
    ];

    pub fn new(sample_rate: f32, _channels: usize) -> Self {
        let max = (0.002 * sample_rate) as usize + 4;
        let mut s = Binauralizer {
            pv: ParamValues::new(Self::PARAMS),
            sr: sample_rate,
            lines: [DelayLine::new(max), DelayLine::new(max)],
            sh: [Shadow::default(); 4],
            itd: sm(0.0, sample_rate),
            mix: sm(1.0, sample_rate),
        };
        s.apply_params(true);
        s
    }

    fn apply_params(&mut self, snap: bool) {
        let theta = self.pv.v("angle");
        let a = self.pv.v("head_size") * 0.01 / 2.0;
        // Ears at ±90°: near ear sees the source at 90° − θ off its axis, the far ear 90° + θ.
        let near = Shadow::design(90.0 - theta, a, self.sr);
        let far = Shadow::design(90.0 + theta, a, self.sr);
        for (k, s) in self.sh.iter_mut().enumerate() {
            s.set_coeffs(if k % 2 == 0 { &near } else { &far });
        }
        let th = theta.to_radians();
        self.itd.set(a / SPEED_OF_SOUND * (th + th.sin()) * self.sr);
        self.mix.set(self.pv.v("mix") / 100.0);
        if snap {
            self.itd.snap();
            self.mix.snap();
        }
    }
}

impl AudioEffect for Binauralizer {
    param_plumbing!("binauralizer");
    fn reset(&mut self) {
        self.lines.iter_mut().for_each(DelayLine::reset);
        for s in &mut self.sh {
            s.x1 = 0.0;
            s.y1 = 0.0;
        }
        self.itd.snap();
        self.mix.snap();
    }
    fn process(&mut self, channels: &mut [&mut [f32]]) {
        let n = block_len(channels);
        if channels.len() < 2 {
            return;
        }
        let (a, b) = channels.split_at_mut(1);
        let (l, r) = (&mut a[0], &mut b[0]);
        for i in 0..n {
            let (itd, mix) = (self.itd.tick(), self.mix.tick());
            let (xl, xr) = (l[i], r[i]);
            self.lines[0].push(xl);
            self.lines[1].push(xr);
            let near_l = self.sh[0].tick(xl);
            let far_l = self.sh[1].tick(self.lines[0].read(itd));
            let near_r = self.sh[2].tick(xr);
            let far_r = self.sh[3].tick(self.lines[1].read(itd));
            let (bl, br) = (0.5 * (near_l + far_r), 0.5 * (near_r + far_l));
            l[i] = xl + mix * (bl - xl);
            r[i] = xr + mix * (br - xr);
        }
    }
}

// ------------------------------------------------------------------------- ambisonics panner

/// Rotates the sound field (pan / tilt / roll) of a stereo source pair placed at ±30° and
/// re-renders it to stereo with constant-power panning; sources turned behind the listener
/// are attenuated, raised/lowered sources slightly softened. Identity at 0/0/0.
pub struct AmbisonicsPanner {
    pv: ParamValues,
    /// out L ← in L, out L ← in R, out R ← in L, out R ← in R
    m: [Smoothed; 4],
}

impl AmbisonicsPanner {
    pub const PARAMS: &'static [ParamSpec] = &[
        ParamSpec::new("pan", "Pan", -180.0, 180.0, 0.0, Unit::None),
        ParamSpec::new("tilt", "Tilt", -90.0, 90.0, 0.0, Unit::None),
        ParamSpec::new("roll", "Roll", -180.0, 180.0, 0.0, Unit::None),
    ];

    pub fn new(sample_rate: f32, _channels: usize) -> Self {
        let mut s = AmbisonicsPanner {
            pv: ParamValues::new(Self::PARAMS),
            m: [sm(1.0, sample_rate), sm(0.0, sample_rate), sm(0.0, sample_rate), sm(1.0, sample_rate)],
        };
        s.apply_params(true);
        s
    }

    /// (left gain, right gain) for a source at azimuth `az` (degrees, + = left) after rotation.
    pub fn source_gains(az: f32, pan: f32, tilt: f32, roll: f32) -> (f32, f32) {
        let (az, el) = (az.to_radians(), 0.0f32);
        // x forward, y left, z up
        let mut v = [el.cos() * az.cos(), el.cos() * az.sin(), el.sin()];
        let rot = |v: [f32; 3], axis: usize, ang: f32| -> [f32; 3] {
            let (s, c) = ang.to_radians().sin_cos();
            let [x, y, z] = v;
            match axis {
                0 => [x, c * y - s * z, s * y + c * z],
                1 => [c * x + s * z, y, -s * x + c * z],
                _ => [c * x - s * y, s * x + c * y, z],
            }
        };
        v = rot(v, 0, roll);
        v = rot(v, 1, tilt);
        v = rot(v, 2, pan);
        let p = (v[1] / 30f32.to_radians().sin()).clamp(-1.0, 1.0);
        // constant power: p = 1 → all left, −1 → all right
        let a = (1.0 - p) * 0.5 * FRAC_PI_2;
        let (mut gl, mut gr) = (a.cos(), a.sin());
        let att = (1.0 - 0.3 * (-v[0]).max(0.0)) * (1.0 - 0.2 * v[2].abs());
        gl *= att;
        gr *= att;
        (gl, gr)
    }

    fn apply_params(&mut self, snap: bool) {
        let (pan, tilt, roll) = (self.pv.v("pan"), self.pv.v("tilt"), self.pv.v("roll"));
        let (ll, rl) = Self::source_gains(30.0, pan, tilt, roll);
        let (lr, rr) = Self::source_gains(-30.0, pan, tilt, roll);
        let clean = |v: f32| {
            if v.abs() < 1e-6 {
                0.0
            } else if (v - 1.0).abs() < 1e-6 {
                1.0
            } else {
                v
            }
        };
        for (s, v) in self.m.iter_mut().zip([ll, lr, rl, rr]) {
            s.set(clean(v));
            if snap {
                s.snap();
            }
        }
    }
}

impl AudioEffect for AmbisonicsPanner {
    param_plumbing!("ambisonics_panner");
    fn reset(&mut self) {
        self.m.iter_mut().for_each(Smoothed::snap);
    }
    fn process(&mut self, channels: &mut [&mut [f32]]) {
        let n = block_len(channels);
        if channels.len() < 2 {
            return;
        }
        let (a, b) = channels.split_at_mut(1);
        let (l, r) = (&mut a[0], &mut b[0]);
        for i in 0..n {
            let g = [self.m[0].tick(), self.m[1].tick(), self.m[2].tick(), self.m[3].tick()];
            let (x, y) = (l[i], r[i]);
            l[i] = g[0] * x + g[1] * y;
            r[i] = g[2] * x + g[3] * y;
        }
    }
}

// ------------------------------------------------------------------------------- loudness meter

/// Pass-through effect that measures BS.1770 loudness (momentary, short-term, integrated,
/// range, true peak) of the signal at its insert point.
pub struct LoudnessMeterFx {
    pv: ParamValues,
    meter: LoudnessMeter,
}

impl LoudnessMeterFx {
    pub const PARAMS: &'static [ParamSpec] = &[ParamSpec::new("target", "Target Loudness", -36.0, -5.0, -23.0, Unit::None)];

    pub fn new(sample_rate: f32, channels: usize) -> Self {
        LoudnessMeterFx { pv: ParamValues::new(Self::PARAMS), meter: LoudnessMeter::new(sample_rate as f64, channels.clamp(1, 2)) }
    }
    fn apply_params(&mut self, _snap: bool) {}

    /// The running meter.
    pub fn meter(&self) -> &LoudnessMeter {
        &self.meter
    }
}

impl AudioEffect for LoudnessMeterFx {
    param_plumbing!("loudness_meter");
    fn reset(&mut self) {
        self.meter.reset();
    }
    fn process(&mut self, channels: &mut [&mut [f32]]) {
        let n = block_len(channels);
        let nch = self.meter.channels().min(channels.len());
        if nch == 0 || n == 0 {
            return;
        }
        if nch == 1 {
            self.meter.process(&[&channels[0][..n]]);
        } else {
            self.meter.process(&[&channels[0][..n], &channels[1][..n]]);
        }
    }
}

// ----------------------------------------------------------------------------------- mute

/// Mute with a 5 ms ramp (keyframe it to silence passages without clicks).
pub struct Mute {
    pv: ParamValues,
    g: Smoothed,
}

impl Mute {
    pub const PARAMS: &'static [ParamSpec] = &[ParamSpec::toggle("mute", "Mute", false)];

    pub fn new(sample_rate: f32, _channels: usize) -> Self {
        let mut s = Mute { pv: ParamValues::new(Self::PARAMS), g: Smoothed::with_ms(1.0, sample_rate, 5.0) };
        s.apply_params(true);
        s
    }
    fn apply_params(&mut self, snap: bool) {
        self.g.set(if self.pv.on("mute") { 0.0 } else { 1.0 });
        if snap {
            self.g.snap();
        }
    }
}

impl AudioEffect for Mute {
    param_plumbing!("mute");
    fn reset(&mut self) {
        self.g.snap();
    }
    fn process(&mut self, channels: &mut [&mut [f32]]) {
        let n = block_len(channels);
        for i in 0..n {
            let g = self.g.tick();
            for ch in channels.iter_mut() {
                ch[i] *= g;
            }
        }
    }
}
