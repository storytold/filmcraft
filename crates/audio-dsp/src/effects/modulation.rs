//! Modulation and echo effects: Chorus/Flanger, Flanger, Phaser, Analog Delay and Multitap
//! Delay. All modulated delays read with linear interpolation; feedback loops are bounded
//! (|g| < 1, or saturating for the Analog Delay).

use super::util::{AllPass1, DelayLine, Envelope, Lfo, OnePole, coef, uni_sine, uni_tri, wrap_phase};
use crate::{AudioEffect, ParamSpec, ParamValues, Smoothed, Unit, block_len, db_to_gain, param_plumbing};

const SM: f32 = 20.0;

// ---------------------------------------------------------------------------- chorus / flanger

/// Chorus (three modulated voices, 15–35 ms) or flanger (one voice, 0.3–5 ms, feedback).
pub struct ChorusFlanger {
    pv: ParamValues,
    sr: f32,
    lines: Vec<DelayLine>,
    lfo: Lfo,
    fb: Vec<f32>,
    fast: Vec<Envelope>,
    slow: Vec<Envelope>,
    mix: Smoothed,
    width: Smoothed,
    intensity: Smoothed,
    speed: f32,
}

impl ChorusFlanger {
    pub const PARAMS: &'static [ParamSpec] = &[
        ParamSpec::choice("mode", "Mode", &["Chorus", "Flanger"], 0),
        ParamSpec::log("speed", "Speed", 0.05, 10.0, 0.8, Unit::Hertz),
        ParamSpec::new("width", "Width", 0.0, 100.0, 50.0, Unit::Percent),
        ParamSpec::new("intensity", "Intensity", 0.0, 100.0, 30.0, Unit::Percent),
        ParamSpec::new("transience", "Transience", 0.0, 100.0, 0.0, Unit::Percent),
        ParamSpec::new("mix", "Mix", 0.0, 100.0, 50.0, Unit::Percent),
    ];

    pub fn new(sample_rate: f32, channels: usize) -> Self {
        let ch = channels.max(1);
        let max = (0.05 * sample_rate) as usize;
        let mut s = ChorusFlanger {
            pv: ParamValues::new(Self::PARAMS),
            sr: sample_rate,
            lines: vec![DelayLine::new(max); ch],
            lfo: Lfo::default(),
            fb: vec![0.0; ch],
            fast: vec![Envelope::default(); ch],
            slow: vec![Envelope::default(); ch],
            mix: Smoothed::with_ms(0.0, sample_rate, SM),
            width: Smoothed::with_ms(0.0, sample_rate, SM),
            intensity: Smoothed::with_ms(0.0, sample_rate, SM),
            speed: 1.0,
        };
        s.apply_params(true);
        s
    }

    fn apply_params(&mut self, snap: bool) {
        self.mix.set(self.pv.v("mix") / 100.0);
        self.width.set(self.pv.v("width") / 100.0);
        self.intensity.set(self.pv.v("intensity") / 100.0);
        self.speed = self.pv.v("speed");
        if snap {
            self.mix.snap();
            self.width.snap();
            self.intensity.snap();
        }
    }
}

impl AudioEffect for ChorusFlanger {
    param_plumbing!("chorus_flanger");
    fn reset(&mut self) {
        self.lines.iter_mut().for_each(DelayLine::reset);
        self.lfo.reset();
        self.fb.iter_mut().for_each(|v| *v = 0.0);
        self.fast.iter_mut().for_each(|e| e.env = 0.0);
        self.slow.iter_mut().for_each(|e| e.env = 0.0);
        self.mix.snap();
        self.width.snap();
        self.intensity.snap();
    }
    fn process(&mut self, channels: &mut [&mut [f32]]) {
        let n = block_len(channels);
        let nch = channels.len().min(self.lines.len());
        let flanger = self.pv.idx("mode") == 1;
        let trans = self.pv.v("transience") / 100.0;
        let ms = self.sr * 0.001;
        let (fa, fr, sa, sr_) = (coef(0.5, self.sr), coef(20.0, self.sr), coef(30.0, self.sr), coef(200.0, self.sr));
        for i in 0..n {
            let mix = self.mix.tick();
            let width = self.width.tick();
            let inten = self.intensity.tick();
            for ch in 0..nch {
                let x = channels[ch][i];
                let off = ch as f64 * 0.25;
                let line = &mut self.lines[ch];
                let wet = if flanger {
                    let d = (0.3 + 4.7 * width * uni_tri(self.lfo.phase(off))) * ms;
                    let y = line.read(d);
                    self.fb[ch] = y;
                    y
                } else {
                    let mut acc = 0.0;
                    for v in 0..3 {
                        let d = (15.0 + 20.0 * width * uni_sine(self.lfo.phase(off + v as f64 / 3.0))) * ms;
                        acc += line.read(d);
                    }
                    let y = acc / 3.0;
                    self.fb[ch] = y;
                    y
                };
                let g = if flanger { 0.95 * inten } else { 0.6 * inten };
                line.push(x + g * self.fb[ch]);
                let mut w = wet;
                if trans > 0.0 {
                    let a = x.abs();
                    let f = self.fast[ch].tick(fa, fr, a);
                    let s = self.slow[ch].tick(sa, sr_, a);
                    let boost = if s > 1e-9 { ((f - s) / s).clamp(0.0, 2.0) } else { 0.0 };
                    w *= 1.0 + trans * boost;
                }
                channels[ch][i] = x + mix * (w - x);
            }
            self.lfo.advance(self.speed, self.sr);
        }
    }
}

// ------------------------------------------------------------------------------------ flanger

/// Classic flanger: the delay sweeps between the initial and final delay; stereo phasing
/// offsets the right channel's LFO; feedback, inverted and "special effects" (normal minus
/// inverted sweep) modes.
pub struct Flanger {
    pv: ParamValues,
    sr: f32,
    lines: Vec<DelayLine>,
    lfo: Lfo,
    d0: Smoothed,
    d1: Smoothed,
    fb: Smoothed,
    mix: Smoothed,
}

impl Flanger {
    pub const PARAMS: &'static [ParamSpec] = &[
        ParamSpec::log("initial_delay", "Initial Delay", 0.1, 20.0, 1.0, Unit::Milliseconds),
        ParamSpec::log("final_delay", "Final Delay", 0.1, 20.0, 5.0, Unit::Milliseconds),
        ParamSpec::new("stereo_phasing", "Stereo Phasing", 0.0, 180.0, 90.0, Unit::None),
        ParamSpec::new("feedback", "Feedback", -100.0, 100.0, 50.0, Unit::Percent),
        ParamSpec::log("rate", "Modulation Rate", 0.01, 10.0, 0.5, Unit::Hertz),
        ParamSpec::toggle("inverted", "Inverted", false),
        ParamSpec::toggle("special", "Special Effects", false),
        ParamSpec::toggle("sinusoidal", "Sinusoidal", true),
        ParamSpec::new("mix", "Mix", 0.0, 100.0, 50.0, Unit::Percent),
    ];

    pub fn new(sample_rate: f32, channels: usize) -> Self {
        let ch = channels.max(1);
        let sm = |v| Smoothed::with_ms(v, sample_rate, SM);
        let mut s = Flanger {
            pv: ParamValues::new(Self::PARAMS),
            sr: sample_rate,
            lines: vec![DelayLine::new((0.025 * sample_rate) as usize); ch],
            lfo: Lfo::default(),
            d0: sm(0.0),
            d1: sm(0.0),
            fb: sm(0.0),
            mix: sm(0.0),
        };
        s.apply_params(true);
        s
    }

    fn apply_params(&mut self, snap: bool) {
        let ms = self.sr * 0.001;
        self.d0.set(self.pv.v("initial_delay") * ms);
        self.d1.set(self.pv.v("final_delay") * ms);
        self.fb.set(0.95 * self.pv.v("feedback") / 100.0);
        self.mix.set(self.pv.v("mix") / 100.0);
        if snap {
            for s in [&mut self.d0, &mut self.d1, &mut self.fb, &mut self.mix] {
                s.snap();
            }
        }
    }
}

impl AudioEffect for Flanger {
    param_plumbing!("flanger");
    fn reset(&mut self) {
        self.lines.iter_mut().for_each(DelayLine::reset);
        self.lfo.reset();
        for s in [&mut self.d0, &mut self.d1, &mut self.fb, &mut self.mix] {
            s.snap();
        }
    }
    fn process(&mut self, channels: &mut [&mut [f32]]) {
        let n = block_len(channels);
        let nch = channels.len().min(self.lines.len());
        let sine = self.pv.on("sinusoidal");
        let inv = if self.pv.on("inverted") { -1.0 } else { 1.0 };
        let special = self.pv.on("special");
        let phasing = self.pv.v("stereo_phasing") as f64 / 360.0;
        let rate = self.pv.v("rate");
        let shape = |p: f64| if sine { uni_sine(p) } else { uni_tri(p) };
        for i in 0..n {
            let (d0, d1, fb, mix) = (self.d0.tick(), self.d1.tick(), self.fb.tick(), self.mix.tick());
            for ch in 0..nch {
                let x = channels[ch][i];
                let p = self.lfo.phase(if ch == 1 { phasing } else { 0.0 });
                let line = &mut self.lines[ch];
                let y = line.read(d0 + (d1 - d0) * shape(p));
                let wet = if special {
                    let y2 = line.read(d0 + (d1 - d0) * shape(wrap_phase(p + 0.5)));
                    0.5 * (y - y2) + 0.5 * y
                } else {
                    y
                };
                line.push(x + fb * y);
                channels[ch][i] = x + mix * (inv * wet - x);
            }
            self.lfo.advance(rate, self.sr);
        }
    }
}

// -------------------------------------------------------------------------------------- phaser

const MAX_STAGES: usize = 12;

/// Swept cascade of first-order all-pass stages with feedback.
pub struct Phaser {
    pv: ParamValues,
    sr: f32,
    ap: Vec<[AllPass1; MAX_STAGES]>,
    coefs: Vec<f64>,
    fbs: Vec<f64>,
    lfo: Lfo,
    counter: u32,
    intensity: Smoothed,
    mix: Smoothed,
    fb: Smoothed,
    out: Smoothed,
}

impl Phaser {
    pub const PARAMS: &'static [ParamSpec] = &[
        ParamSpec::new("stages", "Stages", 2.0, MAX_STAGES as f32, 4.0, Unit::None),
        ParamSpec::new("intensity", "Intensity", 0.0, 100.0, 100.0, Unit::Percent),
        ParamSpec::new("depth", "Depth", 0.0, 100.0, 70.0, Unit::Percent),
        ParamSpec::log("rate", "Modulation Rate", 0.01, 10.0, 0.5, Unit::Hertz),
        ParamSpec::new("phase_diff", "Phase Difference", 0.0, 180.0, 90.0, Unit::None),
        ParamSpec::log("upper_freq", "Upper Frequency", 100.0, 20000.0, 2500.0, Unit::Hertz),
        ParamSpec::new("feedback", "Feedback", -100.0, 100.0, 0.0, Unit::Percent),
        ParamSpec::new("mix", "Mix", 0.0, 100.0, 50.0, Unit::Percent),
        ParamSpec::new("output", "Output Gain", -24.0, 24.0, 0.0, Unit::Decibels),
    ];

    pub fn new(sample_rate: f32, channels: usize) -> Self {
        let ch = channels.max(1);
        let sm = |v| Smoothed::with_ms(v, sample_rate, SM);
        let mut s = Phaser {
            pv: ParamValues::new(Self::PARAMS),
            sr: sample_rate,
            ap: vec![[AllPass1::default(); MAX_STAGES]; ch],
            coefs: vec![0.0; ch],
            fbs: vec![0.0; ch],
            lfo: Lfo::default(),
            counter: 0,
            intensity: sm(0.0),
            mix: sm(0.0),
            fb: sm(0.0),
            out: sm(1.0),
        };
        s.apply_params(true);
        s
    }

    fn apply_params(&mut self, snap: bool) {
        self.intensity.set(self.pv.v("intensity") / 100.0);
        self.mix.set(self.pv.v("mix") / 100.0);
        self.fb.set(0.9 * self.pv.v("feedback") / 100.0);
        self.out.set(db_to_gain(self.pv.v("output")));
        if snap {
            for s in [&mut self.intensity, &mut self.mix, &mut self.fb, &mut self.out] {
                s.snap();
            }
        }
    }
}

impl AudioEffect for Phaser {
    param_plumbing!("phaser");
    fn reset(&mut self) {
        self.ap.iter_mut().for_each(|a| a.iter_mut().for_each(AllPass1::reset));
        self.fbs.iter_mut().for_each(|v| *v = 0.0);
        self.lfo.reset();
        self.counter = 0;
        for s in [&mut self.intensity, &mut self.mix, &mut self.fb, &mut self.out] {
            s.snap();
        }
    }
    fn process(&mut self, channels: &mut [&mut [f32]]) {
        let n = block_len(channels);
        let nch = channels.len().min(self.ap.len());
        let stages = ((self.pv.v("stages").round() as usize / 2) * 2).clamp(2, MAX_STAGES);
        let depth = self.pv.v("depth") / 100.0;
        let upper = self.pv.v("upper_freq");
        let diff = self.pv.v("phase_diff") as f64 / 360.0;
        let rate = self.pv.v("rate");
        for i in 0..n {
            if self.counter == 0 {
                for ch in 0..nch {
                    let p = self.lfo.phase(if ch == 1 { diff } else { 0.0 });
                    // Sweep down from the upper frequency by up to 6 octaves.
                    let f = upper * 2f32.powf(-6.0 * depth * uni_sine(p));
                    self.coefs[ch] = AllPass1::coef(f, self.sr);
                }
            }
            self.counter = (self.counter + 1) % 8;
            let (inten, mix, fb, out) = (self.intensity.tick(), self.mix.tick(), self.fb.tick() as f64, self.out.tick());
            for ch in 0..nch {
                let x = channels[ch][i];
                let a = self.coefs[ch];
                let mut y = x as f64 + fb * self.fbs[ch];
                for st in self.ap[ch].iter_mut().take(stages) {
                    y = st.tick(a, y);
                }
                self.fbs[ch] = y;
                let wet = x + inten * (y as f32 - x);
                channels[ch][i] = (x + mix * (wet - x)) * out;
            }
            self.lfo.advance(rate, self.sr);
        }
    }
}

// -------------------------------------------------------------------------------- analog delay

pub const ANALOG_MODES: &[&str] = &["Tape", "Tape/Tube", "Analog"];

/// Delay with a coloured, saturating feedback path (tape / tape+tube / bucket-brigade style
/// low-pass and soft clipping) and stereo spread.
pub struct AnalogDelay {
    pv: ParamValues,
    sr: f32,
    lines: Vec<DelayLine>,
    lp: Vec<OnePole>,
    time: Smoothed,
    dry: Smoothed,
    wet: Smoothed,
    fb: Smoothed,
    drive: Smoothed,
    spread: Smoothed,
}

impl AnalogDelay {
    pub const MAX_MS: f32 = 8000.0;
    pub const PARAMS: &'static [ParamSpec] = &[
        ParamSpec::choice("mode", "Mode", ANALOG_MODES, 0),
        ParamSpec::new("dry", "Dry Out", 0.0, 100.0, 100.0, Unit::Percent),
        ParamSpec::new("wet", "Wet Out", 0.0, 100.0, 40.0, Unit::Percent),
        ParamSpec::log("delay", "Delay", 5.0, 8000.0, 250.0, Unit::Milliseconds),
        ParamSpec::new("feedback", "Feedback", 0.0, 200.0, 40.0, Unit::Percent),
        ParamSpec::new("trash", "Trash", 0.0, 200.0, 0.0, Unit::Percent),
        ParamSpec::new("spread", "Spread", 0.0, 200.0, 0.0, Unit::Percent),
    ];

    pub fn new(sample_rate: f32, channels: usize) -> Self {
        let ch = channels.max(1);
        let max = (Self::MAX_MS * 1.5 * 0.001 * sample_rate) as usize + 4;
        let sm = |v| Smoothed::with_ms(v, sample_rate, SM);
        let mut s = AnalogDelay {
            pv: ParamValues::new(Self::PARAMS),
            sr: sample_rate,
            lines: vec![DelayLine::new(max); ch],
            lp: vec![OnePole::default(); ch],
            time: Smoothed::with_ms(1.0, sample_rate, 100.0),
            dry: sm(1.0),
            wet: sm(0.0),
            fb: sm(0.0),
            drive: sm(1.0),
            spread: sm(0.0),
        };
        s.apply_params(true);
        s
    }

    fn apply_params(&mut self, snap: bool) {
        self.time.set(self.pv.v("delay") * 0.001 * self.sr);
        self.dry.set(self.pv.v("dry") / 100.0);
        self.wet.set(self.pv.v("wet") / 100.0);
        self.fb.set(self.pv.v("feedback") / 100.0);
        self.drive.set(1.0 + 4.0 * self.pv.v("trash") / 100.0);
        self.spread.set(self.pv.v("spread") / 100.0);
        if snap {
            for s in [&mut self.time, &mut self.dry, &mut self.wet, &mut self.fb, &mut self.drive, &mut self.spread] {
                s.snap();
            }
        }
    }
}

impl AudioEffect for AnalogDelay {
    param_plumbing!("analog_delay");
    fn reset(&mut self) {
        self.lines.iter_mut().for_each(DelayLine::reset);
        self.lp.iter_mut().for_each(OnePole::reset);
        for s in [&mut self.time, &mut self.dry, &mut self.wet, &mut self.fb, &mut self.drive, &mut self.spread] {
            s.snap();
        }
    }
    fn process(&mut self, channels: &mut [&mut [f32]]) {
        let n = block_len(channels);
        let nch = channels.len().min(self.lines.len());
        let mode = self.pv.idx("mode");
        let cutoff = [6500.0, 5000.0, 3200.0][mode.min(2)];
        let a = OnePole::alpha(cutoff, self.sr);
        for i in 0..n {
            let (t, dry, wet, fb, drive, spread) = (self.time.tick(), self.dry.tick(), self.wet.tick(), self.fb.tick(), self.drive.tick(), self.spread.tick());
            for ch in 0..nch {
                let x = channels[ch][i];
                let side = match (nch, ch) {
                    (1, _) => 0.0,
                    (_, 0) => -0.25,
                    _ => 0.25,
                };
                let d = t * (1.0 + side * spread);
                let line = &mut self.lines[ch];
                let y = line.read(d - 1.0);
                let c = self.lp[ch].lp(a, y) * drive;
                let sat = match mode {
                    0 => c.tanh(),
                    1 => (c + 0.2).tanh() - 0.2f32.tanh(),
                    _ => c / (1.0 + c.abs()),
                } / drive;
                line.push(x + fb * sat);
                channels[ch][i] = dry * x + wet * y;
            }
        }
    }
}

// ------------------------------------------------------------------------------- multitap delay

const TAPS: usize = 4;
const TAP_IDS: [[&str; 3]; TAPS] =
    [["delay1", "feedback1", "level1"], ["delay2", "feedback2", "level2"], ["delay3", "feedback3", "level3"], ["delay4", "feedback4", "level4"]];

/// Four independent feedback delay taps mixed into one wet signal.
pub struct MultitapDelay {
    pv: ParamValues,
    sr: f32,
    /// `[channel * TAPS + tap]`.
    lines: Vec<DelayLine>,
    time: [Smoothed; TAPS],
    fb: [Smoothed; TAPS],
    level: [Smoothed; TAPS],
    mix: Smoothed,
}

impl MultitapDelay {
    pub const MAX_MS: f32 = 4000.0;
    pub const PARAMS: &'static [ParamSpec] = &[
        ParamSpec::log("delay1", "Delay 1", 1.0, 4000.0, 250.0, Unit::Milliseconds),
        ParamSpec::new("feedback1", "Feedback 1", 0.0, 95.0, 0.0, Unit::Percent),
        ParamSpec::new("level1", "Level 1", -96.0, 0.0, -6.0, Unit::Decibels),
        ParamSpec::log("delay2", "Delay 2", 1.0, 4000.0, 500.0, Unit::Milliseconds),
        ParamSpec::new("feedback2", "Feedback 2", 0.0, 95.0, 0.0, Unit::Percent),
        ParamSpec::new("level2", "Level 2", -96.0, 0.0, -9.0, Unit::Decibels),
        ParamSpec::log("delay3", "Delay 3", 1.0, 4000.0, 750.0, Unit::Milliseconds),
        ParamSpec::new("feedback3", "Feedback 3", 0.0, 95.0, 0.0, Unit::Percent),
        ParamSpec::new("level3", "Level 3", -96.0, 0.0, -12.0, Unit::Decibels),
        ParamSpec::log("delay4", "Delay 4", 1.0, 4000.0, 1000.0, Unit::Milliseconds),
        ParamSpec::new("feedback4", "Feedback 4", 0.0, 95.0, 0.0, Unit::Percent),
        ParamSpec::new("level4", "Level 4", -96.0, 0.0, -15.0, Unit::Decibels),
        ParamSpec::new("mix", "Mix", 0.0, 100.0, 50.0, Unit::Percent),
    ];

    pub fn new(sample_rate: f32, channels: usize) -> Self {
        let ch = channels.max(1);
        let max = (Self::MAX_MS * 0.001 * sample_rate) as usize + 4;
        let sm = |v| Smoothed::with_ms(v, sample_rate, SM);
        let tm = || Smoothed::with_ms(1.0, sample_rate, 100.0);
        let mut s = MultitapDelay {
            pv: ParamValues::new(Self::PARAMS),
            sr: sample_rate,
            lines: vec![DelayLine::new(max); ch * TAPS],
            time: [tm(), tm(), tm(), tm()],
            fb: [sm(0.0), sm(0.0), sm(0.0), sm(0.0)],
            level: [sm(0.0), sm(0.0), sm(0.0), sm(0.0)],
            mix: sm(0.0),
        };
        s.apply_params(true);
        s
    }

    fn apply_params(&mut self, snap: bool) {
        for (k, ids) in TAP_IDS.iter().enumerate() {
            self.time[k].set(self.pv.v(ids[0]) * 0.001 * self.sr);
            self.fb[k].set(self.pv.v(ids[1]) / 100.0);
            let l = self.pv.v(ids[2]);
            self.level[k].set(if l <= -96.0 { 0.0 } else { db_to_gain(l) });
        }
        self.mix.set(self.pv.v("mix") / 100.0);
        if snap {
            self.snap();
        }
    }

    fn snap(&mut self) {
        for k in 0..TAPS {
            self.time[k].snap();
            self.fb[k].snap();
            self.level[k].snap();
        }
        self.mix.snap();
    }
}

impl AudioEffect for MultitapDelay {
    param_plumbing!("multitap_delay");
    fn reset(&mut self) {
        self.lines.iter_mut().for_each(DelayLine::reset);
        self.snap();
    }
    fn process(&mut self, channels: &mut [&mut [f32]]) {
        let n = block_len(channels);
        let nch = channels.len().min(self.lines.len() / TAPS);
        for i in 0..n {
            let mut t = [0.0f32; TAPS];
            let mut fb = [0.0f32; TAPS];
            let mut lv = [0.0f32; TAPS];
            for k in 0..TAPS {
                t[k] = self.time[k].tick();
                fb[k] = self.fb[k].tick();
                lv[k] = self.level[k].tick();
            }
            let mix = self.mix.tick();
            for ch in 0..nch {
                let x = channels[ch][i];
                let mut wet = 0.0;
                for k in 0..TAPS {
                    let line = &mut self.lines[ch * TAPS + k];
                    let y = line.read(t[k] - 1.0);
                    line.push(x + fb[k] * y);
                    wet += lv[k] * y;
                }
                channels[ch][i] = x + mix * (wet - x);
            }
        }
    }
}
