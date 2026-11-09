//! Dynamics rack ("Dynamics": auto gate → expander → compressor → limiter), the four-band
//! Multiband Compressor (Linkwitz–Riley crossovers with all-pass phase compensation, so the
//! bands sum to an all-pass) and the Tube-modeled Compressor.

use super::dynamics::{compressor_curve, expander_curve};
use super::util::{coef, lin_db};
use crate::biquad::{Biquad, Coeffs, FilterType};
use crate::{AudioEffect, ParamSpec, ParamValues, Smoothed, Unit, block_len, db_to_gain, param_plumbing};

/// Attenuation of a closed gate (dB).
const GATE_RANGE: f32 = 80.0;
/// Most expander attenuation (dB).
const EXP_RANGE: f32 = 60.0;

#[inline]
fn smooth(env: &mut f32, target: f32, att: f32, rel: f32) {
    // Gains in dB (≤ 0): falling = attack, rising = release.
    let c = if target < *env { att } else { rel };
    *env = c * *env + (1.0 - c) * target;
    if !(1e-7..=f32::MAX).contains(&env.abs()) {
        *env = 0.0;
    }
}

/// Cubic soft clipper with ceiling `t` (linear): identity below t/2, smooth to exactly ±t.
#[inline]
pub fn soft_clip(x: f32, t: f32) -> f32 {
    let k = 0.5 * t;
    let a = x.abs();
    if a <= k {
        x
    } else {
        // Quadratic blend from slope 1 at k to slope 0 at 2t − k… normalised to reach t.
        let u = ((a - k) / (2.0 * (t - k))).min(1.0);
        let y = k + (t - k) * (2.0 * u - u * u);
        y.copysign(x)
    }
}

// --------------------------------------------------------------------------------- dynamics

/// Premiere-style Dynamics: Auto Gate, Compressor, Expander and Limiter sections in series
/// (stereo-linked peak detection), plus output gain.
pub struct DynamicsRack {
    pv: ParamValues,
    sr: f32,
    gate_env: f32,
    gate_hold: u32,
    exp_env: f32,
    comp_env: f32,
    lim_env: f32,
    out: Smoothed,
    /// Cached coefficients.
    c: [f32; 8],
    hold: u32,
}

impl DynamicsRack {
    pub const PARAMS: &'static [ParamSpec] = &[
        ParamSpec::toggle("gate_on", "Auto Gate", false),
        ParamSpec::new("gate_threshold", "Gate Threshold", -80.0, 0.0, -50.0, Unit::Decibels),
        ParamSpec::log("gate_attack", "Gate Attack", 0.1, 100.0, 2.0, Unit::Milliseconds),
        ParamSpec::log("gate_release", "Gate Release", 1.0, 3000.0, 100.0, Unit::Milliseconds),
        ParamSpec::new("gate_hold", "Gate Hold", 0.0, 1000.0, 50.0, Unit::Milliseconds),
        ParamSpec::toggle("comp_on", "Compressor", true),
        ParamSpec::new("comp_threshold", "Compressor Threshold", -60.0, 0.0, -20.0, Unit::Decibels),
        ParamSpec::new("comp_ratio", "Compressor Ratio", 1.0, 30.0, 2.0, Unit::Ratio),
        ParamSpec::log("comp_attack", "Compressor Attack", 0.1, 300.0, 10.0, Unit::Milliseconds),
        ParamSpec::log("comp_release", "Compressor Release", 1.0, 3000.0, 100.0, Unit::Milliseconds),
        ParamSpec::toggle("comp_auto", "Auto Makeup", false),
        ParamSpec::new("comp_makeup", "Compressor Makeup", 0.0, 30.0, 0.0, Unit::Decibels),
        ParamSpec::toggle("exp_on", "Expander", false),
        ParamSpec::new("exp_threshold", "Expander Threshold", -80.0, 0.0, -60.0, Unit::Decibels),
        ParamSpec::new("exp_ratio", "Expander Ratio", 1.0, 30.0, 2.0, Unit::Ratio),
        ParamSpec::toggle("lim_on", "Limiter", false),
        ParamSpec::new("lim_threshold", "Limiter Threshold", -30.0, 0.0, -1.0, Unit::Decibels),
        ParamSpec::log("lim_release", "Limiter Release", 1.0, 1000.0, 50.0, Unit::Milliseconds),
        ParamSpec::toggle("soft_clip", "Soft Clip", false),
        ParamSpec::new("output", "Output Gain", -30.0, 30.0, 0.0, Unit::Decibels),
    ];

    pub fn new(sample_rate: f32, _channels: usize) -> Self {
        let mut s = DynamicsRack {
            pv: ParamValues::new(Self::PARAMS),
            sr: sample_rate,
            gate_env: 0.0,
            gate_hold: 0,
            exp_env: 0.0,
            comp_env: 0.0,
            lim_env: 0.0,
            out: Smoothed::with_ms(1.0, sample_rate, 20.0),
            c: [0.0; 8],
            hold: 0,
        };
        s.apply_params(true);
        s
    }

    fn apply_params(&mut self, snap: bool) {
        let p = &self.pv;
        let sr = self.sr;
        self.c = [
            coef(p.v("gate_attack"), sr),
            coef(p.v("gate_release"), sr),
            coef(1.0, sr),
            coef(80.0, sr),
            coef(p.v("comp_attack"), sr),
            coef(p.v("comp_release"), sr),
            0.0,
            coef(p.v("lim_release"), sr),
        ];
        self.hold = (p.v("gate_hold") * 0.001 * sr) as u32;
        self.out.set(db_to_gain(p.v("output") + self.makeup_db()));
        if snap {
            self.out.snap();
        }
    }

    fn makeup_db(&self) -> f32 {
        let p = &self.pv;
        if !p.on("comp_on") {
            return 0.0;
        }
        let auto = if p.on("comp_auto") {
            let (t, r) = (p.v("comp_threshold"), p.v("comp_ratio"));
            -0.5 * t * (1.0 - 1.0 / r)
        } else {
            0.0
        };
        p.v("comp_makeup") + auto
    }

    /// Static gain (dB) of the enabled sections for a detector level, before make-up/output.
    fn static_gain(&self, level: f32) -> f32 {
        let p = &self.pv;
        let mut l = level;
        if p.on("gate_on") && l < p.v("gate_threshold") {
            l -= GATE_RANGE;
        }
        if p.on("exp_on") {
            l = expander_curve(l, p.v("exp_threshold"), p.v("exp_ratio"), EXP_RANGE);
        }
        if p.on("comp_on") {
            l = compressor_curve(l, p.v("comp_threshold"), p.v("comp_ratio"), 0.0);
        }
        if p.on("lim_on") {
            l = l.min(p.v("lim_threshold"));
        }
        l - level
    }
}

impl AudioEffect for DynamicsRack {
    param_plumbing!("dynamics_rack");
    fn transfer_db(&self, band: usize, input_db: f32) -> Option<f32> {
        (band == 0).then(|| input_db + self.static_gain(input_db) + self.pv.v("output") + self.makeup_db())
    }
    fn reset(&mut self) {
        self.gate_env = 0.0;
        self.gate_hold = 0;
        self.exp_env = 0.0;
        self.comp_env = 0.0;
        self.lim_env = 0.0;
        self.out.snap();
    }
    fn process(&mut self, channels: &mut [&mut [f32]]) {
        let n = block_len(channels);
        let p = &self.pv;
        let (gate_on, gt) = (p.on("gate_on"), p.v("gate_threshold"));
        let (exp_on, et, er) = (p.on("exp_on"), p.v("exp_threshold"), p.v("exp_ratio"));
        let (comp_on, ct, cr) = (p.on("comp_on"), p.v("comp_threshold"), p.v("comp_ratio"));
        let (lim_on, lt) = (p.on("lim_on"), p.v("lim_threshold"));
        let clip = p.on("soft_clip");
        let ceiling = db_to_gain(lt);
        let c = self.c;
        for i in 0..n {
            let peak = channels.iter().fold(0.0f32, |a, ch| a.max(ch[i].abs()));
            let mut l = lin_db(peak);
            let mut g = 0.0f32;
            if gate_on {
                let target = if l < gt { -GATE_RANGE } else { 0.0 };
                if target >= 0.0 {
                    self.gate_hold = self.hold;
                    smooth(&mut self.gate_env, 0.0, c[1], c[0]);
                } else if self.gate_hold > 0 {
                    self.gate_hold -= 1;
                } else {
                    smooth(&mut self.gate_env, target, c[1], c[1]);
                }
                g += self.gate_env;
                l += self.gate_env;
            }
            if exp_on {
                let target = expander_curve(l, et, er, EXP_RANGE) - l;
                smooth(&mut self.exp_env, target, c[2], c[3]);
                g += self.exp_env;
                l += self.exp_env;
            }
            if comp_on {
                let target = compressor_curve(l, ct, cr, 0.0) - l;
                smooth(&mut self.comp_env, target, c[4], c[5]);
                g += self.comp_env;
                l += self.comp_env;
            }
            if lim_on {
                let target = (lt - l).min(0.0);
                if target < self.lim_env {
                    self.lim_env = target;
                } else {
                    smooth(&mut self.lim_env, target, 0.0, c[7]);
                }
                g += self.lim_env;
            }
            let gain = db_to_gain(g);
            let o = self.out.tick();
            for ch in channels.iter_mut() {
                let mut y = ch[i] * gain;
                if lim_on {
                    y = if clip { soft_clip(y, ceiling) } else { y.clamp(-ceiling, ceiling) };
                }
                ch[i] = y * o;
            }
        }
    }
}

// ----------------------------------------------------------------------- multiband compressor

/// Number of bands.
pub const MB_BANDS: usize = 4;

macro_rules! mb_band {
    ($n:literal) => {
        [
            ParamSpec::new(concat!("b", $n, "_threshold"), concat!("Band ", $n, " Threshold"), -60.0, 0.0, -18.0, Unit::Decibels),
            ParamSpec::new(concat!("b", $n, "_ratio"), concat!("Band ", $n, " Ratio"), 1.0, 30.0, 3.0, Unit::Ratio),
            ParamSpec::log(concat!("b", $n, "_attack"), concat!("Band ", $n, " Attack"), 0.1, 500.0, 10.0, Unit::Milliseconds),
            ParamSpec::log(concat!("b", $n, "_release"), concat!("Band ", $n, " Release"), 1.0, 5000.0, 100.0, Unit::Milliseconds),
            ParamSpec::new(concat!("b", $n, "_gain"), concat!("Band ", $n, " Gain"), -18.0, 18.0, 0.0, Unit::Decibels),
            ParamSpec::toggle(concat!("b", $n, "_solo"), concat!("Band ", $n, " Solo"), false),
            ParamSpec::toggle(concat!("b", $n, "_bypass"), concat!("Band ", $n, " Bypass"), false),
        ]
    };
}

const fn mb_params() -> [ParamSpec; 3 + 4 * 7 + 5] {
    let head = [
        ParamSpec::log("xo1", "Low Crossover", 20.0, 1000.0, 120.0, Unit::Hertz),
        ParamSpec::log("xo2", "Mid Crossover", 100.0, 8000.0, 2000.0, Unit::Hertz),
        ParamSpec::log("xo3", "High Crossover", 1000.0, 20000.0, 10000.0, Unit::Hertz),
    ];
    let bands = [mb_band!("1"), mb_band!("2"), mb_band!("3"), mb_band!("4")];
    let tail = [
        ParamSpec::new("output", "Output Gain", -18.0, 18.0, 0.0, Unit::Decibels),
        ParamSpec::toggle("lim_on", "Limiter", false),
        ParamSpec::new("lim_threshold", "Limiter Threshold", -30.0, 0.0, -0.1, Unit::Decibels),
        ParamSpec::log("lim_release", "Limiter Release", 1.0, 1000.0, 50.0, Unit::Milliseconds),
        ParamSpec::toggle("link", "Link Channels", true),
    ];
    let mut out = [ParamSpec::new("", "", 0.0, 0.0, 0.0, Unit::None); 3 + 4 * 7 + 5];
    let mut i = 0;
    while i < 3 {
        out[i] = head[i];
        i += 1;
    }
    let mut b = 0;
    while b < 4 {
        let mut k = 0;
        while k < 7 {
            out[3 + b * 7 + k] = bands[b][k];
            k += 1;
        }
        b += 1;
    }
    let mut t = 0;
    while t < 5 {
        out[31 + t] = tail[t];
        t += 1;
    }
    out
}

const MB_PARAMS: [ParamSpec; 36] = mb_params();

/// Parameter ids of band `b` (0-based): threshold, ratio, attack, release, gain, solo, bypass.
pub const MB_IDS: [[&str; 7]; MB_BANDS] = [
    ["b1_threshold", "b1_ratio", "b1_attack", "b1_release", "b1_gain", "b1_solo", "b1_bypass"],
    ["b2_threshold", "b2_ratio", "b2_attack", "b2_release", "b2_gain", "b2_solo", "b2_bypass"],
    ["b3_threshold", "b3_ratio", "b3_attack", "b3_release", "b3_gain", "b3_solo", "b3_bypass"],
    ["b4_threshold", "b4_ratio", "b4_attack", "b4_release", "b4_gain", "b4_solo", "b4_bypass"],
];

/// Per-channel crossover filter state.
#[derive(Clone, Copy, Debug, Default)]
struct XoState {
    lp1: [Biquad; 2],
    hp1: [Biquad; 2],
    lp2: [Biquad; 2],
    hp2: [Biquad; 2],
    lp3: [Biquad; 2],
    hp3: [Biquad; 2],
    /// Phase compensation: band 1 through AP(f2), AP(f3); band 2 through AP(f3).
    ap1_2: Biquad,
    ap1_3: Biquad,
    ap2_3: Biquad,
}

#[derive(Clone, Copy, Debug)]
struct XoCoeffs {
    lp: [Coeffs; 3],
    hp: [Coeffs; 3],
    ap: [Coeffs; 3],
}

/// Four-band compressor with LR4 crossovers.
pub struct MultibandCompressor {
    pv: ParamValues,
    sr: f32,
    xo: [Smoothed; 3],
    xc: XoCoeffs,
    state: Vec<XoState>,
    /// Gain-reduction envelopes (dB), `[channel][band]` (channel 0 is used when linked).
    env: Vec<[f32; MB_BANDS]>,
    lim_env: f32,
    band_gain: [Smoothed; MB_BANDS],
    out: Smoothed,
    att: [f32; MB_BANDS],
    rel: [f32; MB_BANDS],
    lim_rel: f32,
    counter: u32,
}

const Q_BW: f64 = std::f64::consts::FRAC_1_SQRT_2;

impl MultibandCompressor {
    pub const PARAMS: &'static [ParamSpec] = &MB_PARAMS;

    pub fn new(sample_rate: f32, channels: usize) -> Self {
        let sm = |v: f32| Smoothed::with_ms(v, sample_rate, 30.0);
        let mut s = MultibandCompressor {
            pv: ParamValues::new(Self::PARAMS),
            sr: sample_rate,
            xo: [sm(1.0), sm(1.0), sm(1.0)],
            xc: XoCoeffs { lp: [Coeffs::IDENTITY; 3], hp: [Coeffs::IDENTITY; 3], ap: [Coeffs::IDENTITY; 3] },
            state: vec![XoState::default(); channels.max(1)],
            env: vec![[0.0; MB_BANDS]; channels.max(1)],
            lim_env: 0.0,
            band_gain: [sm(1.0), sm(1.0), sm(1.0), sm(1.0)],
            out: sm(1.0),
            att: [0.0; MB_BANDS],
            rel: [0.0; MB_BANDS],
            lim_rel: 0.0,
            counter: 0,
        };
        s.apply_params(true);
        s
    }

    /// Crossover frequencies (Hz) at the current settings, kept strictly increasing.
    pub fn crossovers(&self) -> [f32; 3] {
        let a = self.pv.v("xo1");
        let b = self.pv.v("xo2").max(a * 1.25);
        let c = self.pv.v("xo3").max(b * 1.25).min(self.sr * 0.45);
        [a, b.min(c / 1.25), c]
    }

    fn update_xo(&mut self) {
        let sr = self.sr as f64;
        for k in 0..3 {
            let f = 2f64.powf(self.xo[k].value() as f64);
            self.xc.lp[k] = Coeffs::design(FilterType::LowPass, sr, f, Q_BW, 0.0);
            self.xc.hp[k] = Coeffs::design(FilterType::HighPass, sr, f, Q_BW, 0.0);
            self.xc.ap[k] = Coeffs::design(FilterType::AllPass, sr, f, Q_BW, 0.0);
        }
    }

    fn apply_params(&mut self, snap: bool) {
        let xo = self.crossovers();
        for k in 0..3 {
            self.xo[k].set(xo[k].log2());
        }
        for b in 0..MB_BANDS {
            let ids = MB_IDS[b];
            self.att[b] = coef(self.pv.v(ids[2]), self.sr);
            self.rel[b] = coef(self.pv.v(ids[3]), self.sr);
            self.band_gain[b].set(self.band_level(b));
        }
        self.lim_rel = coef(self.pv.v("lim_release"), self.sr);
        self.out.set(db_to_gain(self.pv.v("output")));
        if snap {
            self.xo.iter_mut().for_each(Smoothed::snap);
            self.band_gain.iter_mut().for_each(Smoothed::snap);
            self.out.snap();
            self.update_xo();
        }
    }

    /// Linear output level of band `b` from its gain, solo and bypass switches.
    fn band_level(&self, b: usize) -> f32 {
        let any_solo = MB_IDS.iter().any(|ids| self.pv.on(ids[5]));
        let ids = MB_IDS[b];
        if any_solo && !self.pv.on(ids[5]) {
            return 0.0;
        }
        if self.pv.on(ids[6]) { 1.0 } else { db_to_gain(self.pv.v(ids[4])) }
    }

    #[inline]
    fn split(st: &mut XoState, c: &XoCoeffs, x: f64) -> [f64; MB_BANDS] {
        let lr = |f: &mut [Biquad; 2], co: &Coeffs, v: f64| {
            let u = f[0].tick(co, v);
            f[1].tick(co, u)
        };
        let b1 = lr(&mut st.lp1, &c.lp[0], x);
        let r1 = lr(&mut st.hp1, &c.hp[0], x);
        let b1 = st.ap1_3.tick(&c.ap[2], st.ap1_2.tick(&c.ap[1], b1));
        let b2 = lr(&mut st.lp2, &c.lp[1], r1);
        let r2 = lr(&mut st.hp2, &c.hp[1], r1);
        let b2 = st.ap2_3.tick(&c.ap[2], b2);
        let b3 = lr(&mut st.lp3, &c.lp[2], r2);
        let b4 = lr(&mut st.hp3, &c.hp[2], r2);
        [b1, b2, b3, b4]
    }
}

impl AudioEffect for MultibandCompressor {
    param_plumbing!("multiband_compressor");
    fn transfer_db(&self, band: usize, input_db: f32) -> Option<f32> {
        let ids = MB_IDS.get(band)?;
        if self.pv.on(ids[6]) {
            return Some(input_db);
        }
        Some(compressor_curve(input_db, self.pv.v(ids[0]), self.pv.v(ids[1]), 0.0) + self.pv.v(ids[4]))
    }
    fn reset(&mut self) {
        self.xo.iter_mut().for_each(Smoothed::snap);
        self.band_gain.iter_mut().for_each(Smoothed::snap);
        self.out.snap();
        self.update_xo();
        self.state.iter_mut().for_each(|s| *s = XoState::default());
        self.env.iter_mut().for_each(|e| *e = [0.0; MB_BANDS]);
        self.lim_env = 0.0;
        self.counter = 0;
    }
    fn process(&mut self, channels: &mut [&mut [f32]]) {
        let n = block_len(channels);
        let nch = channels.len().min(self.state.len());
        let link = self.pv.on("link");
        let lim_on = self.pv.on("lim_on");
        let lt = self.pv.v("lim_threshold");
        let ceiling = db_to_gain(lt);
        let mut thr = [0.0f32; MB_BANDS];
        let mut ratio = [1.0f32; MB_BANDS];
        let mut byp = [false; MB_BANDS];
        for b in 0..MB_BANDS {
            thr[b] = self.pv.v(MB_IDS[b][0]);
            ratio[b] = self.pv.v(MB_IDS[b][1]);
            byp[b] = self.pv.on(MB_IDS[b][6]);
        }
        let mut bands = [[0.0f64; MB_BANDS]; 8];
        for i in 0..n {
            if self.counter == 0 && self.xo.iter().any(Smoothed::is_smoothing) {
                self.update_xo();
            }
            self.counter = (self.counter + 1) % 16;
            for x in &mut self.xo {
                x.tick();
            }
            let mut bg = [0.0f32; MB_BANDS];
            for b in 0..MB_BANDS {
                bg[b] = self.band_gain[b].tick();
            }
            let o = self.out.tick();
            for ch in 0..nch.min(8) {
                bands[ch] = Self::split(&mut self.state[ch], &self.xc, channels[ch][i] as f64);
            }
            // Detection (per band; linked = loudest channel drives all).
            let mut gains = [[1.0f32; MB_BANDS]; 8];
            for b in 0..MB_BANDS {
                if byp[b] {
                    continue;
                }
                if link {
                    let peak = (0..nch.min(8)).fold(0.0f32, |a, ch| a.max(bands[ch][b].abs() as f32));
                    let l = lin_db(peak);
                    let target = compressor_curve(l, thr[b], ratio[b], 0.0) - l;
                    smooth(&mut self.env[0][b], target, self.att[b], self.rel[b]);
                    let g = db_to_gain(self.env[0][b]);
                    for gc in gains.iter_mut().take(nch.min(8)) {
                        gc[b] = g;
                    }
                } else {
                    for ch in 0..nch.min(8) {
                        let l = lin_db(bands[ch][b].abs() as f32);
                        let target = compressor_curve(l, thr[b], ratio[b], 0.0) - l;
                        smooth(&mut self.env[ch][b], target, self.att[b], self.rel[b]);
                        gains[ch][b] = db_to_gain(self.env[ch][b]);
                    }
                }
            }
            let mut ys = [0.0f32; 8];
            for ch in 0..nch.min(8) {
                let mut y = 0.0f64;
                for b in 0..MB_BANDS {
                    y += bands[ch][b] * (gains[ch][b] * bg[b]) as f64;
                }
                ys[ch] = y as f32 * o;
            }
            if lim_on {
                let peak = ys[..nch.min(8)].iter().fold(0.0f32, |a, v| a.max(v.abs()));
                let target = (lt - lin_db(peak)).min(0.0);
                if target < self.lim_env {
                    self.lim_env = target;
                } else {
                    smooth(&mut self.lim_env, target, 0.0, self.lim_rel);
                }
                let g = db_to_gain(self.lim_env);
                for y in ys.iter_mut().take(nch.min(8)) {
                    *y = (*y * g).clamp(-ceiling, ceiling);
                }
            }
            for ch in 0..nch.min(8) {
                channels[ch][i] = ys[ch];
            }
        }
    }
}

// --------------------------------------------------------------------- tube-modeled compressor

/// Compressor with an RMS detector, wide soft knee, program-dependent release and a gentle
/// symmetric tube-style saturation stage (`tanh(kx)/k`, k = ½: −0.2 dB at −6 dBFS).
pub struct TubeCompressor {
    pv: ParamValues,
    sr: f32,
    ms: f32,
    env: f32,
    slow: f32,
    att: f32,
    rel: f32,
    rms_c: f32,
    out: Smoothed,
}

const TUBE_K: f32 = 0.5;
const TUBE_KNEE: f32 = 10.0;

impl TubeCompressor {
    pub const PARAMS: &'static [ParamSpec] = &[
        ParamSpec::new("threshold", "Threshold", -60.0, 0.0, -20.0, Unit::Decibels),
        ParamSpec::new("ratio", "Ratio", 1.0, 30.0, 4.0, Unit::Ratio),
        ParamSpec::log("attack", "Attack", 0.1, 500.0, 10.0, Unit::Milliseconds),
        ParamSpec::log("release", "Release", 1.0, 5000.0, 100.0, Unit::Milliseconds),
        ParamSpec::new("output", "Output Gain", -30.0, 30.0, 0.0, Unit::Decibels),
    ];

    pub fn new(sample_rate: f32, _channels: usize) -> Self {
        let mut s = TubeCompressor {
            pv: ParamValues::new(Self::PARAMS),
            sr: sample_rate,
            ms: 0.0,
            env: 0.0,
            slow: 0.0,
            att: 0.0,
            rel: 0.0,
            rms_c: coef(5.0, sample_rate),
            out: Smoothed::with_ms(1.0, sample_rate, 20.0),
        };
        s.apply_params(true);
        s
    }

    fn apply_params(&mut self, snap: bool) {
        self.att = coef(self.pv.v("attack"), self.sr);
        self.rel = coef(self.pv.v("release"), self.sr);
        self.out.set(db_to_gain(self.pv.v("output")));
        if snap {
            self.out.snap();
        }
    }
}

impl AudioEffect for TubeCompressor {
    param_plumbing!("tube_compressor");
    fn transfer_db(&self, band: usize, input_db: f32) -> Option<f32> {
        (band == 0).then(|| compressor_curve(input_db, self.pv.v("threshold"), self.pv.v("ratio"), TUBE_KNEE) + self.pv.v("output"))
    }
    fn reset(&mut self) {
        self.ms = 0.0;
        self.env = 0.0;
        self.slow = 0.0;
        self.out.snap();
    }
    fn process(&mut self, channels: &mut [&mut [f32]]) {
        let n = block_len(channels);
        let (t, r) = (self.pv.v("threshold"), self.pv.v("ratio"));
        let slow_c = coef(1500.0, self.sr);
        for i in 0..n {
            let sq = channels.iter().fold(0.0f32, |a, ch| a.max(ch[i] * ch[i]));
            self.ms = self.rms_c * self.ms + (1.0 - self.rms_c) * sq;
            if !(1e-30..=f32::MAX).contains(&self.ms) {
                self.ms = 0.0;
            }
            let l = 10.0 * self.ms.max(1e-20).log10();
            let target = compressor_curve(l, t, r, TUBE_KNEE) - l;
            // Program-dependent release: long, sustained reduction releases more slowly.
            self.slow = slow_c * self.slow + (1.0 - slow_c) * self.env;
            if !(1e-7..=f32::MAX).contains(&self.slow.abs()) {
                self.slow = 0.0;
            }
            let rel = self.rel + (self.slow / -20.0).clamp(0.0, 1.0) * (1.0 - self.rel) * 0.5;
            smooth(&mut self.env, target, self.att, rel.min(0.999_999));
            let g = db_to_gain(self.env);
            let o = self.out.tick();
            for buf in channels.iter_mut() {
                let y = buf[i] * g;
                buf[i] = (TUBE_K * y).tanh() / TUBE_K * o;
            }
        }
    }
}
