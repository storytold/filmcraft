//! Audio effect definitions: Premiere's Audio Effects bin (Amplitude and Compression, Delay and
//! Echo, Filter and EQ, Modulation, Noise Reduction/Restoration, Reverb, Special, Stereo
//! Imagery, Time and Pitch, and the loose Balance / Mute / Volume items).
//!
//! Parameter ids match the `filmcraft-audio-dsp` effect parameters wherever the mapping is
//! direct (`render::audio_fx` checks this), so agents can drive every DSP parameter by id.

use super::{A_AMP, A_DELAY, A_FILTER, A_NOISE, A_REVERB, A_SPECIAL, A_STEREO, A_TIME, EffectDef, ParamDef, audio, b, ch, f, fs, grp};

const A_MOD: &[&str] = &["Audio Effects", "Modulation"];
/// Loose items at the end of the Audio Effects bin.
const A_ROOT: &[&str] = &["Audio Effects"];

fn db(id: &'static str, label: &'static str, def: f64, min: f64, max: f64) -> ParamDef {
    fs(id, label, def, (min, max), (min, max), "dB", 1)
}
fn hz(id: &'static str, label: &'static str, def: f64, min: f64, max: f64) -> ParamDef {
    fs(id, label, def, (min, max), (min, max), "Hz", 0)
}
fn ms(id: &'static str, label: &'static str, def: f64, min: f64, max: f64) -> ParamDef {
    fs(id, label, def, (min, max), (min, max), "ms", 1)
}
fn pct(id: &'static str, label: &'static str, def: f64, min: f64, max: f64) -> ParamDef {
    fs(id, label, def, (min, max), (min, max), "%", 0)
}
fn num(id: &'static str, label: &'static str, def: f64, min: f64, max: f64, unit: &'static str, decimals: u8) -> ParamDef {
    fs(id, label, def, (min, max), (min, max), unit, decimals)
}
/// A non-animatable choice/bool already; non-animatable float (filter designs, impulses…).
fn fixed(mut p: ParamDef) -> ParamDef {
    p.animatable = false;
    p
}
fn group(g: &'static str, ps: Vec<ParamDef>) -> Vec<ParamDef> {
    ps.into_iter().map(|p| grp(p, g)).collect()
}

/// Band labels of the 10/20/30-band graphic equalisers (parameters `b1`…`bN`, plus `gain`).
pub const GEQ10_LABELS: [&str; 10] = ["31.5 Hz", "63 Hz", "125 Hz", "250 Hz", "500 Hz", "1 kHz", "2 kHz", "4 kHz", "8 kHz", "16 kHz"];
pub const GEQ20_LABELS: [&str; 20] = [
    "31.5 Hz", "44 Hz", "63 Hz", "88 Hz", "125 Hz", "177 Hz", "250 Hz", "355 Hz", "500 Hz", "710 Hz", "1 kHz", "1.4 kHz", "2 kHz", "2.8 kHz", "4 kHz",
    "5.6 kHz", "8 kHz", "11.2 kHz", "16 kHz", "22.4 kHz",
];
pub const GEQ30_LABELS: [&str; 30] = [
    "25 Hz", "31.5 Hz", "40 Hz", "50 Hz", "63 Hz", "80 Hz", "100 Hz", "125 Hz", "160 Hz", "200 Hz", "250 Hz", "315 Hz", "400 Hz", "500 Hz", "630 Hz", "800 Hz",
    "1 kHz", "1.25 kHz", "1.6 kHz", "2 kHz", "2.5 kHz", "3.15 kHz", "4 kHz", "5 kHz", "6.3 kHz", "8 kHz", "10 kHz", "12.5 kHz", "16 kHz", "20 kHz",
];
const BAND_IDS: [&str; 30] = [
    "b1", "b2", "b3", "b4", "b5", "b6", "b7", "b8", "b9", "b10", "b11", "b12", "b13", "b14", "b15", "b16", "b17", "b18", "b19", "b20", "b21", "b22", "b23",
    "b24", "b25", "b26", "b27", "b28", "b29", "b30",
];

fn geq(labels: &[&'static str]) -> Vec<ParamDef> {
    let mut v: Vec<ParamDef> = labels.iter().zip(BAND_IDS).map(|(l, id)| db(id, l, 0.0, -24.0, 24.0)).collect();
    v.push(db("gain", "Master Gain", 0.0, -24.0, 24.0));
    v
}

const SLOPES: &[&str] = &["12 dB/oct", "24 dB/oct", "36 dB/oct", "48 dB/oct"];

fn peq_band(p: [&'static str; 4], name: &'static str, freq: f64, q: f64) -> Vec<ParamDef> {
    group(name, vec![b(p[0], "On", true), hz(p[1], "Frequency", freq, 20.0, 20000.0), db(p[2], "Gain", 0.0, -30.0, 30.0), num(p[3], "Q", q, 0.1, 30.0, "", 2)])
}

fn parametric_eq() -> Vec<ParamDef> {
    let mut v = vec![db("master_gain", "Master Gain", 0.0, -30.0, 30.0)];
    v.extend(group("High Pass", vec![b("hp_on", "On", false), hz("hp_freq", "Frequency", 30.0, 10.0, 20000.0), ch("hp_slope", "Slope", SLOPES, 0)]));
    // Low shelf / band 3 / high shelf keep the ids of the earlier three-band version.
    v.extend(peq_band(["low_on", "low_freq", "low_gain", "low_q"], "Low Shelf", 100.0, 0.71));
    v.extend(peq_band(["b1_on", "b1_freq", "b1_gain", "b1_q"], "Band 1", 200.0, 1.0));
    v.extend(peq_band(["b2_on", "b2_freq", "b2_gain", "b2_q"], "Band 2", 500.0, 1.0));
    v.extend(peq_band(["mid_on", "mid_freq", "mid_gain", "mid_q"], "Band 3", 1000.0, 1.0));
    v.extend(peq_band(["b4_on", "b4_freq", "b4_gain", "b4_q"], "Band 4", 2000.0, 1.0));
    v.extend(peq_band(["b5_on", "b5_freq", "b5_gain", "b5_q"], "Band 5", 5000.0, 1.0));
    v.extend(peq_band(["high_on", "high_freq", "high_gain", "high_q"], "High Shelf", 8000.0, 0.71));
    v.extend(group("Low Pass", vec![b("lp_on", "On", false), hz("lp_freq", "Frequency", 18000.0, 20.0, 22000.0), ch("lp_slope", "Slope", SLOPES, 0)]));
    v
}

fn multiband() -> Vec<ParamDef> {
    let mut v =
        group("Crossovers", vec![hz("xo1", "Low", 120.0, 20.0, 1000.0), hz("xo2", "Mid", 2000.0, 100.0, 8000.0), hz("xo3", "High", 10000.0, 1000.0, 20000.0)]);
    let ids: [[&'static str; 7]; 4] = [
        ["b1_threshold", "b1_ratio", "b1_attack", "b1_release", "b1_gain", "b1_solo", "b1_bypass"],
        ["b2_threshold", "b2_ratio", "b2_attack", "b2_release", "b2_gain", "b2_solo", "b2_bypass"],
        ["b3_threshold", "b3_ratio", "b3_attack", "b3_release", "b3_gain", "b3_solo", "b3_bypass"],
        ["b4_threshold", "b4_ratio", "b4_attack", "b4_release", "b4_gain", "b4_solo", "b4_bypass"],
    ];
    for (k, i) in ids.iter().enumerate() {
        let name = ["Band 1", "Band 2", "Band 3", "Band 4"][k];
        v.extend(group(
            name,
            vec![
                db(i[0], "Threshold", -18.0, -60.0, 0.0),
                num(i[1], "Ratio", 3.0, 1.0, 30.0, ":1", 1),
                ms(i[2], "Attack", 10.0, 0.1, 500.0),
                ms(i[3], "Release", 100.0, 1.0, 5000.0),
                db(i[4], "Gain", 0.0, -18.0, 18.0),
                b(i[5], "Solo", false),
                b(i[6], "Bypass", false),
            ],
        ));
    }
    v.extend(group(
        "Output",
        vec![
            db("output", "Output Gain", 0.0, -18.0, 18.0),
            b("lim_on", "Limiter", false),
            db("lim_threshold", "Limiter Threshold", -0.1, -30.0, 0.0),
            ms("lim_release", "Limiter Release", 50.0, 1.0, 1000.0),
            b("link", "Link Channels", true),
        ],
    ));
    v
}

fn dynamics() -> Vec<ParamDef> {
    let mut v = group(
        "Auto Gate",
        vec![
            b("gate_on", "Auto Gate", false),
            db("gate_threshold", "Threshold", -50.0, -80.0, 0.0),
            ms("gate_attack", "Attack", 2.0, 0.1, 100.0),
            ms("gate_release", "Release", 100.0, 1.0, 3000.0),
            ms("gate_hold", "Hold", 50.0, 0.0, 1000.0),
        ],
    );
    v.extend(group(
        "Compressor",
        vec![
            b("comp_on", "Compressor", true),
            db("comp_threshold", "Threshold", -20.0, -60.0, 0.0),
            num("comp_ratio", "Ratio", 2.0, 1.0, 30.0, ":1", 1),
            ms("comp_attack", "Attack", 10.0, 0.1, 300.0),
            ms("comp_release", "Release", 100.0, 1.0, 3000.0),
            b("comp_auto", "Auto Makeup", false),
            db("comp_makeup", "Makeup", 0.0, 0.0, 30.0),
        ],
    ));
    v.extend(group(
        "Expander",
        vec![b("exp_on", "Expander", false), db("exp_threshold", "Threshold", -60.0, -80.0, 0.0), num("exp_ratio", "Ratio", 2.0, 1.0, 30.0, ":1", 1)],
    ));
    v.extend(group(
        "Limiter",
        vec![
            b("lim_on", "Limiter", false),
            db("lim_threshold", "Threshold", -1.0, -30.0, 0.0),
            ms("lim_release", "Release", 50.0, 1.0, 1000.0),
            b("soft_clip", "Soft Clip", false),
        ],
    ));
    v.push(db("output", "Output Gain", 0.0, -30.0, 30.0));
    v
}

fn notch_filter() -> Vec<ParamDef> {
    let ids: [[&'static str; 3]; 6] = [
        ["n1_on", "n1_freq", "n1_gain"],
        ["n2_on", "n2_freq", "n2_gain"],
        ["n3_on", "n3_freq", "n3_gain"],
        ["n4_on", "n4_freq", "n4_gain"],
        ["n5_on", "n5_freq", "n5_gain"],
        ["n6_on", "n6_freq", "n6_gain"],
    ];
    let mut v = Vec::new();
    for (k, i) in ids.iter().enumerate() {
        let name = ["Notch 1", "Notch 2", "Notch 3", "Notch 4", "Notch 5", "Notch 6"][k];
        v.extend(group(name, vec![b(i[0], "On", false), hz(i[1], "Frequency", 60.0 * (k + 1) as f64, 20.0, 20000.0), db(i[2], "Gain", -30.0, -90.0, 0.0)]));
    }
    v.push(ch("width", "Notch Width", &["Narrow", "Very Narrow", "Super Narrow"], 0));
    v
}

fn fft_filter() -> Vec<ParamDef> {
    let ids: [[&'static str; 2]; 8] = [
        ["p1_freq", "p1_gain"],
        ["p2_freq", "p2_gain"],
        ["p3_freq", "p3_gain"],
        ["p4_freq", "p4_gain"],
        ["p5_freq", "p5_gain"],
        ["p6_freq", "p6_gain"],
        ["p7_freq", "p7_gain"],
        ["p8_freq", "p8_gain"],
    ];
    let freqs = [40.0, 100.0, 250.0, 600.0, 1500.0, 4000.0, 9000.0, 16000.0];
    let mut v = Vec::new();
    for (k, i) in ids.iter().enumerate() {
        let name = ["Point 1", "Point 2", "Point 3", "Point 4", "Point 5", "Point 6", "Point 7", "Point 8"][k];
        v.extend(group(name, vec![hz(i[0], "Frequency", freqs[k], 20.0, 20000.0), db(i[1], "Gain", 0.0, -60.0, 20.0)]));
    }
    v.push(ch("interp", "Interpolation", &["Linear", "Smooth"], 1));
    v
}

/// All audio effect definitions, in Effects-panel order.
pub(super) fn defs() -> Vec<EffectDef> {
    vec![
        // ---- Amplitude and Compression ----
        audio("amplify", "Amplify", A_AMP, vec![fs("gain", "Gain", 0.0, (-96.0, 24.0), (-24.0, 24.0), "dB", 1)]),
        audio(
            "channel_mixer_a",
            "Channel Mixer",
            A_AMP,
            vec![
                pct("l_from_l", "Left - Left", 100.0, -100.0, 100.0),
                pct("l_from_r", "Left - Right", 0.0, -100.0, 100.0),
                b("invert_l", "Invert Left", false),
                pct("r_from_l", "Right - Left", 0.0, -100.0, 100.0),
                pct("r_from_r", "Right - Right", 100.0, -100.0, 100.0),
                b("invert_r", "Invert Right", false),
            ],
        ),
        audio(
            "channel_volume_a",
            "Channel Volume",
            A_AMP,
            vec![fs("left", "Left", 0.0, (-96.0, 24.0), (-60.0, 6.0), "dB", 1), fs("right", "Right", 0.0, (-96.0, 24.0), (-60.0, 6.0), "dB", 1)],
        ),
        audio(
            "deesser",
            "DeEsser",
            A_AMP,
            vec![
                hz("frequency", "Frequency", 6000.0, 2000.0, 12000.0),
                db("threshold", "Threshold", -12.0, -40.0, 0.0),
                db("reduction", "Maximum Reduction", 8.0, 0.0, 24.0),
            ],
        ),
        audio("dynamics_rack", "Dynamics", A_AMP, dynamics()),
        audio(
            "dynamics",
            "Dynamics Processing",
            A_AMP,
            vec![
                db("threshold", "Threshold", -20.0, -60.0, 0.0),
                num("ratio", "Ratio", 4.0, 1.0, 30.0, ":1", 1),
                ms("attack", "Attack", 10.0, 0.1, 500.0),
                fs("release", "Release", 100.0, (1.0, 5000.0), (1.0, 5000.0), "ms", 0),
            ],
        ),
        audio(
            "hard_limiter",
            "Hard Limiter",
            A_AMP,
            vec![
                db("max", "Maximum Amplitude", -0.1, -30.0, 0.0),
                db("boost", "Input Boost", 0.0, -30.0, 30.0),
                db_ms_lookahead(),
                fs("release", "Release Time", 100.0, (1.0, 1000.0), (1.0, 1000.0), "ms", 0),
            ],
        ),
        audio("multiband_compressor", "Multiband Compressor", A_AMP, multiband()),
        audio(
            "single_band_compressor",
            "Single-band Compressor",
            A_AMP,
            vec![
                db("threshold", "Threshold", -20.0, -60.0, 0.0),
                num("ratio", "Ratio", 4.0, 1.0, 30.0, ":1", 1),
                ms("attack", "Attack", 10.0, 0.1, 300.0),
                ms("release", "Release", 100.0, 1.0, 3000.0),
                db("output", "Output Gain", 0.0, -30.0, 30.0),
            ],
        ),
        audio(
            "tube_compressor",
            "Tube-modeled Compressor",
            A_AMP,
            vec![
                db("threshold", "Threshold", -20.0, -60.0, 0.0),
                num("ratio", "Ratio", 4.0, 1.0, 30.0, ":1", 1),
                ms("attack", "Attack", 10.0, 0.1, 500.0),
                ms("release", "Release", 100.0, 1.0, 5000.0),
                db("output", "Output Gain", 0.0, -30.0, 30.0),
            ],
        ),
        // ---- Delay and Echo ----
        audio(
            "analog_delay",
            "Analog Delay",
            A_DELAY,
            vec![
                ch("mode", "Mode", &["Tape", "Tape/Tube", "Analog"], 0),
                pct("dry", "Dry Out", 100.0, 0.0, 100.0),
                pct("wet", "Wet Out", 40.0, 0.0, 100.0),
                ms("delay", "Delay", 250.0, 5.0, 8000.0),
                pct("feedback", "Feedback", 40.0, 0.0, 200.0),
                pct("trash", "Trash", 0.0, 0.0, 200.0),
                pct("spread", "Spread", 0.0, 0.0, 200.0),
            ],
        ),
        audio(
            "delay",
            "Delay",
            A_DELAY,
            vec![
                fs("delay", "Delay", 1.0, (0.0, 2.0), (0.0, 2.0), "s", 2),
                f("feedback", "Feedback", 0.0, 0.0, 100.0, "%"),
                f("mix", "Mix", 50.0, 0.0, 100.0, "%"),
            ],
        ),
        audio(
            "multitap_delay",
            "Multitap Delay",
            A_DELAY,
            vec![
                ms("delay1", "Delay 1", 250.0, 1.0, 4000.0),
                pct("feedback1", "Feedback 1", 0.0, 0.0, 95.0),
                db("level1", "Level 1", -6.0, -96.0, 0.0),
                ms("delay2", "Delay 2", 500.0, 1.0, 4000.0),
                pct("feedback2", "Feedback 2", 0.0, 0.0, 95.0),
                db("level2", "Level 2", -9.0, -96.0, 0.0),
                ms("delay3", "Delay 3", 750.0, 1.0, 4000.0),
                pct("feedback3", "Feedback 3", 0.0, 0.0, 95.0),
                db("level3", "Level 3", -12.0, -96.0, 0.0),
                ms("delay4", "Delay 4", 1000.0, 1.0, 4000.0),
                pct("feedback4", "Feedback 4", 0.0, 0.0, 95.0),
                db("level4", "Level 4", -15.0, -96.0, 0.0),
                pct("mix", "Mix", 50.0, 0.0, 100.0),
            ],
        ),
        // ---- Filter and EQ ----
        audio("bandpass", "Bandpass", A_FILTER, vec![hz("center", "Center", 1000.0, 20.0, 20000.0), num("q", "Q", 1.0, 0.1, 20.0, "", 2)]),
        audio("bass", "Bass", A_FILTER, vec![db("boost", "Boost", 0.0, -24.0, 24.0)]),
        audio("fft_filter", "FFT Filter", A_FILTER, fft_filter()),
        audio("graphic_eq", "Graphic Equalizer (10 Bands)", A_FILTER, geq(&GEQ10_LABELS)),
        audio("graphic_eq_20", "Graphic Equalizer (20 Bands)", A_FILTER, geq(&GEQ20_LABELS)),
        audio("graphic_eq_30", "Graphic Equalizer (30 Bands)", A_FILTER, geq(&GEQ30_LABELS)),
        audio("highpass", "Highpass", A_FILTER, vec![fs("cutoff", "Cutoff", 80.0, (20.0, 20000.0), (20.0, 2000.0), "Hz", 0)]),
        audio("lowpass", "Lowpass", A_FILTER, vec![fs("cutoff", "Cutoff", 8000.0, (20.0, 20000.0), (500.0, 20000.0), "Hz", 0)]),
        audio("notch", "Notch Filter", A_FILTER, notch_filter()),
        audio("parametric_eq", "Parametric Equalizer", A_FILTER, parametric_eq()),
        audio(
            "scientific_filter",
            "Scientific Filter",
            A_FILTER,
            vec![
                ch("type", "Type", &["Bessel", "Butterworth", "Chebyshev", "Elliptical"], 1),
                ch("mode", "Mode", &["Low Pass", "High Pass", "Band Pass", "Band Stop"], 0),
                fixed(num("order", "Order", 6.0, 1.0, 12.0, "", 0)),
                hz("cutoff", "Cutoff", 1000.0, 20.0, 20000.0),
                hz("high_cutoff", "High Cutoff", 4000.0, 20.0, 20000.0),
                fixed(num("ripple", "Passband Ripple", 1.0, 0.01, 6.0, "dB", 2)),
                fixed(db("stop_atten", "Stopband Attenuation", 60.0, 20.0, 120.0)),
                db("gain", "Master Gain", 0.0, -30.0, 30.0),
            ],
        ),
        audio("simple_notch", "Simple Notch Filter", A_FILTER, vec![hz("center", "Center", 1000.0, 20.0, 20000.0), num("q", "Q", 10.0, 0.1, 100.0, "", 1)]),
        audio(
            "simple_eq",
            "Simple Parametric EQ",
            A_FILTER,
            vec![hz("center", "Center", 1000.0, 20.0, 20000.0), num("q", "Q", 1.0, 0.1, 20.0, "", 2), db("boost", "Boost", 0.0, -24.0, 24.0)],
        ),
        audio("treble", "Treble", A_FILTER, vec![db("boost", "Boost", 0.0, -24.0, 24.0)]),
        // ---- Modulation ----
        audio(
            "chorus_flanger",
            "Chorus/Flanger",
            A_MOD,
            vec![
                ch("mode", "Mode", &["Chorus", "Flanger"], 0),
                num("speed", "Speed", 0.8, 0.05, 10.0, "Hz", 2),
                pct("width", "Width", 50.0, 0.0, 100.0),
                pct("intensity", "Intensity", 30.0, 0.0, 100.0),
                pct("transience", "Transience", 0.0, 0.0, 100.0),
                pct("mix", "Mix", 50.0, 0.0, 100.0),
            ],
        ),
        audio(
            "flanger",
            "Flanger",
            A_MOD,
            vec![
                ms("initial_delay", "Initial Delay", 1.0, 0.1, 20.0),
                ms("final_delay", "Final Delay", 5.0, 0.1, 20.0),
                num("stereo_phasing", "Stereo Phasing", 90.0, 0.0, 180.0, "°", 0),
                pct("feedback", "Feedback", 50.0, -100.0, 100.0),
                num("rate", "Modulation Rate", 0.5, 0.01, 10.0, "Hz", 2),
                b("inverted", "Inverted", false),
                b("special", "Special Effects", false),
                b("sinusoidal", "Sinusoidal", true),
                pct("mix", "Mix", 50.0, 0.0, 100.0),
            ],
        ),
        audio(
            "phaser",
            "Phaser",
            A_MOD,
            vec![
                fixed(num("stages", "Stages", 4.0, 2.0, 12.0, "", 0)),
                pct("intensity", "Intensity", 100.0, 0.0, 100.0),
                pct("depth", "Depth", 70.0, 0.0, 100.0),
                num("rate", "Modulation Rate", 0.5, 0.01, 10.0, "Hz", 2),
                num("phase_diff", "Phase Difference", 90.0, 0.0, 180.0, "°", 0),
                hz("upper_freq", "Upper Frequency", 2500.0, 100.0, 20000.0),
                pct("feedback", "Feedback", 0.0, -100.0, 100.0),
                pct("mix", "Mix", 50.0, 0.0, 100.0),
                db("output", "Output Gain", 0.0, -24.0, 24.0),
            ],
        ),
        // ---- Noise Reduction/Restoration ----
        audio(
            "declicker",
            "Automatic Click Remover",
            A_NOISE,
            vec![num("threshold", "Threshold", 30.0, 1.0, 100.0, "", 0), num("complexity", "Complexity", 16.0, 1.0, 100.0, "", 0)],
        ),
        audio(
            "dehummer",
            "DeHummer",
            A_NOISE,
            vec![
                ch("freq", "Frequency", &["50 Hz", "60 Hz"], 1),
                f("gain", "Gain", -40.0, -80.0, 0.0, "dB"),
                fixed(num("harmonics", "Number of Harmonics", 5.0, 1.0, 10.0, "", 0)),
                num("q", "Q", 30.0, 1.0, 100.0, "", 0),
            ],
        ),
        audio("denoise", "DeNoise", A_NOISE, vec![f("amount", "Amount", 40.0, 0.0, 100.0, "%")]),
        audio(
            "dereverb",
            "DeReverb",
            A_NOISE,
            vec![f("amount", "Amount", 50.0, 0.0, 100.0, "%"), fs("rt60", "Decay Time (RT60)", 0.8, (0.1, 5.0), (0.1, 3.0), "s", 2)],
        ),
        audio("speech_enhance", "Enhance Speech", A_NOISE, vec![f("mix", "Mix", 100.0, 0.0, 100.0, "%"), ch("tone", "Voice", &["Low Tone", "High Tone"], 0)]),
        // A neural network removes the noise (`filmcraft-enhance`). The model list is that crate's
        // catalogue, in its order: the choice stores the index, so models are only ever appended.
        audio(
            "neural_enhance",
            "Enhance Speech (Neural)",
            A_NOISE,
            vec![
                ch("model", "Model", NEURAL_ENHANCE_MODELS, 0),
                f("attenuation", "Noise Reduction", 24.0, 0.0, 100.0, "dB"),
                b("post_filter", "Post Filter", false),
                f("mix", "Mix", 100.0, 0.0, 100.0, "%"),
            ],
        ),
        // ---- Reverb ----
        audio(
            "convolution_reverb",
            "Convolution Reverb",
            A_REVERB,
            vec![
                ch("impulse", "Impulse", &["Small Room", "Medium Room", "Large Hall", "Cathedral", "Plate", "Ambience", "Vocal Booth"], 1),
                pct("mix", "Mix", 30.0, 0.0, 100.0),
                fixed(pct("room_size", "Room Size", 100.0, 10.0, 100.0)),
                fixed(hz("damping_lf", "Damping LF", 10.0, 10.0, 1000.0)),
                fixed(hz("damping_hf", "Damping HF", 20000.0, 1000.0, 20000.0)),
                ms("predelay", "Pre-Delay", 0.0, 0.0, 500.0),
                pct("width", "Width", 100.0, 0.0, 100.0),
                db("gain", "Gain", 0.0, -24.0, 12.0),
            ],
        ),
        audio(
            "studio_reverb",
            "Studio Reverb",
            A_REVERB,
            vec![
                f("room", "Room Size", 50.0, 0.0, 100.0, "%"),
                f("decay", "Decay", 50.0, 0.0, 100.0, "%"),
                f("damping", "High Frequency Damping", 50.0, 0.0, 100.0, "%"),
                f("dry", "Dry", 90.0, 0.0, 100.0, "%"),
                f("wet", "Wet", 35.0, 0.0, 100.0, "%"),
            ],
        ),
        audio(
            "surround_reverb",
            "Surround Reverb",
            A_REVERB,
            vec![
                pct("center_input", "Center Input", 100.0, 0.0, 100.0),
                pct("room_size", "Room Size", 50.0, 0.0, 100.0),
                num("decay", "Decay", 1.5, 0.1, 20.0, "s", 2),
                ms("predelay", "Pre-Delay", 20.0, 0.0, 200.0),
                pct("damping", "Damping", 40.0, 0.0, 100.0),
                pct("early", "Early Reflections", 40.0, 0.0, 100.0),
                hz("low_cut", "Low Frequency Cut", 20.0, 20.0, 1000.0),
                hz("high_cut", "High Frequency Cut", 12000.0, 1000.0, 20000.0),
                pct("width", "Width", 100.0, 0.0, 100.0),
                pct("dry", "Dry", 80.0, 0.0, 100.0),
                pct("wet", "Wet", 30.0, 0.0, 100.0),
            ],
        ),
        // ---- Special ----
        audio(
            "binauralizer",
            "Binauralizer - Ambisonics",
            A_SPECIAL,
            vec![
                num("angle", "Speaker Angle", 30.0, 0.0, 90.0, "°", 0),
                num("head_size", "Head Size", 17.5, 12.0, 24.0, "cm", 1),
                pct("mix", "Mix", 100.0, 0.0, 100.0),
            ],
        ),
        audio(
            "distortion",
            "Distortion",
            A_SPECIAL,
            vec![
                db("drive", "Drive", 12.0, 0.0, 48.0),
                ch("curve", "Curve", &["Soft Clip", "Hard Clip", "Tube", "Foldback"], 0),
                pct("symmetry", "Asymmetry", 0.0, -100.0, 100.0),
                hz("tone", "Tone", 20000.0, 500.0, 20000.0),
                db("output", "Output Gain", 0.0, -48.0, 12.0),
                pct("mix", "Mix", 100.0, 0.0, 100.0),
            ],
        ),
        audio("fill_left", "Fill Left with Right", A_SPECIAL, vec![]),
        audio("fill_right", "Fill Right with Left", A_SPECIAL, vec![]),
        audio(
            "guitar_suite",
            "GuitarSuite",
            A_SPECIAL,
            vec![
                pct("compressor", "Compressor", 30.0, 0.0, 100.0),
                pct("distortion", "Distortion", 40.0, 0.0, 100.0),
                ch("dist_type", "Distortion Type", &["Soft", "Hard", "Fuzz"], 0),
                ch("amp", "Amplifier", &["None", "Clean Combo", "British Stack", "Tweed", "Modern High Gain"], 1),
                ch("filter", "Filter", &["None", "Low Pass", "High Pass", "Band Pass"], 0),
                hz("filter_freq", "Filter Frequency", 2000.0, 100.0, 10000.0),
                pct("filter_res", "Filter Resonance", 20.0, 0.0, 100.0),
                pct("mix", "Mix", 100.0, 0.0, 100.0),
                db("output", "Output Gain", 0.0, -24.0, 12.0),
            ],
        ),
        audio("invert_a", "Invert", A_SPECIAL, vec![]),
        audio("loudness_radar", "Loudness Meter", A_SPECIAL, vec![num("target", "Target Loudness", -23.0, -36.0, -5.0, "LUFS", 1)]),
        audio(
            "mastering",
            "Mastering",
            A_SPECIAL,
            vec![
                db("eq_low", "Low Shelf Gain", 0.0, -12.0, 12.0),
                hz("eq_mid_freq", "Peak Frequency", 1000.0, 100.0, 10000.0),
                db("eq_mid", "Peak Gain", 0.0, -12.0, 12.0),
                db("eq_high", "High Shelf Gain", 0.0, -12.0, 12.0),
                pct("reverb", "Reverb Amount", 0.0, 0.0, 100.0),
                pct("exciter", "Exciter Amount", 0.0, 0.0, 100.0),
                ch("exciter_mode", "Exciter Mode", &["Retro", "Tape", "Tube"], 1),
                pct("widener", "Widener", 100.0, 0.0, 200.0),
                pct("loudness", "Loudness Maximizer", 0.0, 0.0, 100.0),
                db("output", "Output Gain", 0.0, -24.0, 12.0),
            ],
        ),
        audio(
            "panner_ambisonics",
            "Panner - Ambisonics",
            A_SPECIAL,
            vec![num("pan", "Pan", 0.0, -180.0, 180.0, "°", 1), num("tilt", "Tilt", 0.0, -90.0, 90.0, "°", 1), num("roll", "Roll", 0.0, -180.0, 180.0, "°", 1)],
        ),
        audio("swap_channels", "Swap Channels", A_SPECIAL, vec![]),
        audio("vocal_enhancer", "Vocal Enhancer", A_SPECIAL, vec![ch("mode", "Mode", &["Male", "Female", "Music"], 0)]),
        // ---- Stereo Imagery ----
        audio(
            "stereo_expander",
            "Stereo Expander",
            A_STEREO,
            vec![num("center_pan", "Center Channel Pan", 0.0, -100.0, 100.0, "", 0), pct("expand", "Stereo Expand", 100.0, 0.0, 300.0)],
        ),
        audio("stereo_width", "Stereo Width", A_STEREO, vec![fs("width", "Width", 100.0, (0.0, 200.0), (0.0, 200.0), "%", 0)]),
        // ---- Time and Pitch ----
        audio(
            "pitch_shifter",
            "Pitch Shifter",
            A_TIME,
            vec![fs("semitones", "Semi-tones", 0.0, (-12.0, 12.0), (-12.0, 12.0), "", 0), fs("cents", "Cents", 0.0, (-100.0, 100.0), (-100.0, 100.0), "", 0)],
        ),
        // ---- loose items ----
        audio("balance_a", "Balance", A_ROOT, vec![num("balance", "Balance", 0.0, -100.0, 100.0, "", 1)]),
        audio("mute", "Mute", A_ROOT, vec![mute_param()]),
        audio("volume_a", "Volume", A_ROOT, vec![b("bypass", "Bypass", false), fs("level", "Level", 0.0, (-96.0, 15.0), (-60.0, 15.0), "dB", 1)]),
    ]
}

fn db_ms_lookahead() -> ParamDef {
    fs("lookahead", "Look-Ahead Time", 7.0, (0.0, 30.0), (0.0, 30.0), "ms", 1)
}

/// Mute is keyframed to silence passages, so it is animatable (unlike most toggles).
fn mute_param() -> ParamDef {
    let mut p = b("mute", "Mute", false);
    p.animatable = true;
    p
}

/// Ids of the effects Premiere lists in its Audio Effects bin (excluding Audio Units).
/// Model names of the `neural_enhance` effect (the catalogue of `filmcraft-enhance`, same order).
pub const NEURAL_ENHANCE_MODELS: &[&str] = &["DeepFilterNet 3", "DeepFilterNet 3 (low latency)"];

pub const PREMIERE_AUDIO_EFFECTS: [&str; 53] = [
    "amplify",
    "channel_mixer_a",
    "channel_volume_a",
    "deesser",
    "dynamics_rack",
    "dynamics",
    "hard_limiter",
    "multiband_compressor",
    "single_band_compressor",
    "tube_compressor",
    "analog_delay",
    "delay",
    "multitap_delay",
    "bandpass",
    "bass",
    "fft_filter",
    "graphic_eq",
    "graphic_eq_20",
    "graphic_eq_30",
    "highpass",
    "lowpass",
    "notch",
    "parametric_eq",
    "scientific_filter",
    "simple_notch",
    "simple_eq",
    "treble",
    "chorus_flanger",
    "flanger",
    "phaser",
    "declicker",
    "dehummer",
    "denoise",
    "dereverb",
    "convolution_reverb",
    "studio_reverb",
    "surround_reverb",
    "binauralizer",
    "distortion",
    "fill_left",
    "fill_right",
    "guitar_suite",
    "invert_a",
    "loudness_radar",
    "mastering",
    "panner_ambisonics",
    "swap_channels",
    "vocal_enhancer",
    "stereo_expander",
    "pitch_shifter",
    "balance_a",
    "mute",
    "volume_a",
];

#[cfg(test)]
mod tests {
    use super::*;
    use crate::effect::{EffectKind, find_effect};

    #[test]
    fn premiere_audio_set_is_complete_and_foldered() {
        let mut seen = std::collections::HashSet::new();
        for id in PREMIERE_AUDIO_EFFECTS {
            assert!(seen.insert(id), "duplicate {id}");
            let d = find_effect(id).unwrap_or_else(|| panic!("missing {id}"));
            assert_eq!(d.kind, EffectKind::Audio, "{id}");
            assert!(!d.intrinsic, "{id}");
            assert_eq!(d.category.first(), Some(&"Audio Effects"), "{id}");
        }
        let folder = |id: &str| find_effect(id).unwrap().category.get(1).copied();
        assert_eq!(folder("multiband_compressor"), Some("Amplitude and Compression"));
        assert_eq!(folder("multitap_delay"), Some("Delay and Echo"));
        assert_eq!(folder("graphic_eq_30"), Some("Filter and EQ"));
        assert_eq!(folder("phaser"), Some("Modulation"));
        assert_eq!(folder("declicker"), Some("Noise Reduction/Restoration"));
        assert_eq!(folder("surround_reverb"), Some("Reverb"));
        assert_eq!(folder("guitar_suite"), Some("Special"));
        assert_eq!(folder("stereo_expander"), Some("Stereo Imagery"));
        assert_eq!(folder("pitch_shifter"), Some("Time and Pitch"));
        assert_eq!(folder("mute"), None, "loose item");
    }

    #[test]
    fn parametric_eq_keeps_the_three_band_ids() {
        let d = find_effect("parametric_eq").unwrap();
        for id in ["low_freq", "low_gain", "mid_freq", "mid_gain", "mid_q", "high_freq", "high_gain"] {
            assert!(d.param(id).is_some(), "{id}");
        }
    }
}
