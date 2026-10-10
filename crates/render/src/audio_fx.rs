//! Clip audio effects: project effect instances → `filmcraft-audio-dsp` processors.
//!
//! The mixer is pull-based (any sample range, any order), but most audio effects are stateful
//! (filters, dynamics, delay/reverb tails). A small cache keeps processed effect chains per clip
//! keyed by the next timeline sample they expect: sequential consumers (playback, export, meters)
//! continue their chain; any other request starts a fresh chain with enough pre-roll for the
//! chain's tails to build up. Latency (limiter look-ahead, STFT effects) is compensated by feeding
//! the chain ahead of its output.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

use filmcraft_audio_dsp::AudioEffect;
use filmcraft_project::{EffectInstance, TrackItem};
use filmcraft_time::Tick;

/// How a project effect maps onto a DSP effect.
pub(crate) struct Mapping {
    pub(crate) dsp: &'static str,
    /// Pre-roll (seconds) for a fresh chain; closures get the effect instance at the start time.
    pub(crate) preroll: fn(&EffectInstance, Tick) -> f64,
    /// Set DSP parameters from the (keyframed) project parameters at media time `mt`.
    pub(crate) apply: fn(&mut dyn AudioEffect, &EffectInstance, Tick),
}

fn ignore(_: bool) {}

fn f(e: &EffectInstance, id: &str, mt: Tick) -> f32 {
    e.f64_at(id, mt) as f32
}

pub(crate) fn mapping(id: &str) -> Option<Mapping> {
    Some(match id {
        "amplify" => Mapping { dsp: "amplify", preroll: |_, _| 0.0, apply: |d, e, t| ignore(d.set_param("gain", f(e, "gain", t))) },
        "dynamics" => Mapping {
            dsp: "compressor",
            preroll: |e, t| (f(e, "release", t) as f64 / 1000.0 * 5.0).clamp(0.05, 3.0),
            apply: |d, e, t| {
                d.set_param("threshold", f(e, "threshold", t));
                d.set_param("ratio", f(e, "ratio", t));
                d.set_param("attack", f(e, "attack", t));
                d.set_param("release", f(e, "release", t));
            },
        },
        "hard_limiter" => Mapping {
            dsp: "limiter",
            preroll: |e, t| (f(e, "lookahead", t) as f64 / 1000.0 + f(e, "release", t) as f64 / 1000.0 * 5.0).clamp(0.05, 2.0),
            apply: |d, e, t| {
                d.set_param("ceiling", f(e, "max", t));
                d.set_param("input_gain", f(e, "boost", t).max(0.0));
                d.set_param("release", f(e, "release", t));
                d.set_param("lookahead", f(e, "lookahead", t));
            },
        },
        "delay" => Mapping {
            dsp: "delay",
            preroll: |e, t| {
                // enough echoes to decay by ~80 dB
                let time = f(e, "delay", t).max(0.001) as f64;
                let fb = (f(e, "feedback", t) as f64 / 100.0).clamp(0.0, 0.95);
                let repeats = if fb > 0.001 { (-4.0 / fb.log10()).clamp(1.0, 60.0) } else { 1.0 };
                (time * repeats).min(8.0)
            },
            apply: |d, e, t| {
                d.set_param("time", f(e, "delay", t) * 1000.0);
                d.set_param("feedback", f(e, "feedback", t));
                d.set_param("mix", f(e, "mix", t));
            },
        },
        // Single-band filters via band 1 of the parametric EQ (types: see FILTER_TYPE_NAMES).
        "highpass" => Mapping { dsp: "parametric_eq", preroll: |_, _| 0.05, apply: |d, e, t| band(d, 4.0, f(e, "cutoff", t), 0.707) },
        "lowpass" => Mapping { dsp: "parametric_eq", preroll: |_, _| 0.05, apply: |d, e, t| band(d, 3.0, f(e, "cutoff", t), 0.707) },
        "bandpass" => Mapping { dsp: "parametric_eq", preroll: |_, _| 0.05, apply: |d, e, t| band(d, 6.0, f(e, "center", t), f(e, "q", t)) },
        // Amount 0…100 % → 0…40 dB of reduction.
        "denoise" => Mapping { dsp: "denoise", preroll: |_, _| 1.0, apply: |d, e, t| ignore(d.set_param("reduction", f(e, "amount", t) * 0.4)) },
        // Gain is the notch depth: the wet/dry amount that leaves 10^(gain/20) of the hum.
        "dehummer" => Mapping {
            dsp: "dehum",
            preroll: |_, _| 0.2,
            apply: |d, e, t| {
                d.set_param("frequency", e.param("freq").and_then(|p| p.value_at(t).as_f64()).unwrap_or(1.0) as f32);
                d.set_param("amount", (1.0 - 10f32.powf(f(e, "gain", t).min(0.0) / 20.0)) * 100.0);
                set_if_changed(d, "harmonics", f(e, "harmonics", t));
                set_if_changed(d, "q", f(e, "q", t));
            },
        },
        "deesser" => Mapping {
            dsp: "deesser",
            preroll: |_, _| 0.1,
            apply: |d, e, t| {
                d.set_param("frequency", f(e, "frequency", t));
                d.set_param("threshold", f(e, "threshold", t));
                d.set_param("reduction", f(e, "reduction", t));
            },
        },
        "dereverb" => Mapping {
            dsp: "dereverb",
            preroll: |e, t| (f(e, "rt60", t) as f64 + 0.3).clamp(0.5, 3.0),
            apply: |d, e, t| {
                d.set_param("amount", f(e, "amount", t));
                d.set_param("rt60", f(e, "rt60", t));
            },
        },
        "speech_enhance" => Mapping {
            dsp: "speech_enhance",
            preroll: |_, _| 0.3,
            apply: |d, e, t| {
                d.set_param("mix", f(e, "mix", t));
                d.set_param("tone", e.param("tone").and_then(|p| p.value_at(t).as_f64()).unwrap_or(0.0) as f32);
            },
        },
        "stereo_width" => Mapping { dsp: "stereo_width", preroll: |_, _| 0.0, apply: |d, e, t| ignore(d.set_param("width", f(e, "width", t))) },
        "studio_reverb" => Mapping {
            dsp: "reverb",
            preroll: |e, t| 0.3 * 30f64.powf(f(e, "decay", t) as f64 / 100.0).min(8.0),
            apply: |d, e, t| {
                d.set_param("size", f(e, "room", t));
                d.set_param("decay", (0.3 * 30f32.powf(f(e, "decay", t) / 100.0)).clamp(0.1, 20.0));
                d.set_param("damping", f(e, "damping", t));
                let (dry, wet) = (f(e, "dry", t).max(0.0), f(e, "wet", t).max(0.0));
                d.set_param("mix", if dry + wet > 0.0 { wet / (dry + wet) * 100.0 } else { 0.0 });
            },
        },
        "invert_a" => Mapping { dsp: "invert", preroll: |_, _| 0.0, apply: |_, _, _| {} },
        "pitch_shifter" => Mapping {
            dsp: "pitch_shifter",
            preroll: |_, _| 0.1,
            apply: |d, e, t| ignore(d.set_param("semitones", (f(e, "semitones", t) + f(e, "cents", t) / 100.0).clamp(-12.0, 12.0))),
        },
        "single_band_compressor" => Mapping {
            dsp: "compressor",
            preroll: |e, t| (f(e, "release", t) as f64 / 1000.0 * 5.0).clamp(0.05, 3.0),
            apply: |d, e, t| {
                set_if_changed(d, "knee", 0.0);
                for id in ["threshold", "ratio", "attack", "release"] {
                    set_if_changed(d, id, f(e, id, t));
                }
                set_if_changed(d, "makeup", f(e, "output", t));
            },
        },
        "bass" => Mapping { dsp: "parametric_eq", preroll: |_, _| 0.05, apply: |d, e, t| shelf(d, 1.0, 200.0, f(e, "boost", t)) },
        "treble" => Mapping { dsp: "parametric_eq", preroll: |_, _| 0.05, apply: |d, e, t| shelf(d, 2.0, 4000.0, f(e, "boost", t)) },
        "simple_notch" => Mapping { dsp: "parametric_eq", preroll: |_, _| 0.1, apply: |d, e, t| band(d, 5.0, f(e, "center", t), f(e, "q", t)) },
        "simple_eq" => Mapping {
            dsp: "parametric_eq",
            preroll: |_, _| 0.05,
            apply: |d, e, t| {
                band(d, 0.0, f(e, "center", t), f(e, "q", t));
                set_if_changed(d, "b1.gain", f(e, "boost", t));
            },
        },
        "volume_a" => Mapping {
            dsp: "amplify",
            preroll: |_, _| 0.0,
            apply: |d, e, t| {
                let bypass = e.f64_at("bypass", t) >= 0.5;
                set_if_changed(d, "gain", if bypass { 0.0 } else { f(e, "level", t).clamp(-96.0, 24.0) });
            },
        },
        "analog_delay" => Mapping {
            dsp: "analog_delay",
            preroll: |e, t| {
                let time = f(e, "delay", t).max(1.0) as f64 / 1000.0;
                let fb = (f(e, "feedback", t) as f64 / 100.0).clamp(0.0, 0.95);
                let repeats = if fb > 0.001 { (-4.0 / fb.log10()).clamp(1.0, 60.0) } else { 1.0 };
                (time * repeats).clamp(0.12, 8.0)
            },
            apply: direct,
        },
        "multitap_delay" => Mapping {
            dsp: "multitap_delay",
            preroll: |e, t| {
                let mut worst = 0.0f64;
                for k in 1..=4 {
                    let time = e.f64_at(["delay1", "delay2", "delay3", "delay4"][k - 1], t).max(1.0) / 1000.0;
                    let fb = (e.f64_at(["feedback1", "feedback2", "feedback3", "feedback4"][k - 1], t) / 100.0).clamp(0.0, 0.95);
                    let repeats = if fb > 0.001 { (-4.0 / fb.log10()).clamp(1.0, 60.0) } else { 1.0 };
                    worst = worst.max(time * repeats);
                }
                worst.clamp(0.12, 8.0)
            },
            apply: direct,
        },
        "convolution_reverb" => Mapping {
            dsp: "convolution_reverb",
            preroll: |e, t| {
                let rt = [0.45, 0.8, 1.9, 3.2, 1.6, 0.3, 0.2][(e.f64_at("impulse", t) as usize).min(6)];
                (rt * 1.1 * e.f64_at("room_size", t) / 100.0 + e.f64_at("predelay", t) / 1000.0).clamp(0.12, 4.0)
            },
            apply: direct,
        },
        "surround_reverb" => {
            Mapping { dsp: "surround_reverb", preroll: |e, t| (e.f64_at("decay", t) + e.f64_at("predelay", t) / 1000.0).clamp(0.12, 8.0), apply: direct }
        }
        _ => {
            let &(_, dsp, _) = DIRECT.iter().find(|(p, _, _)| *p == id)?;
            Mapping { dsp, preroll: |e, _| direct_preroll(&e.effect), apply: direct }
        }
    })
}

/// Project effects whose parameters map one-to-one (same ids, same units) onto a DSP effect:
/// (project id, DSP id, pre-roll seconds for a fresh chain).
pub const DIRECT: &[(&str, &str, f64)] = &[
    ("channel_mixer_a", "channel_mixer", 0.0),
    ("channel_volume_a", "channel_volume", 0.0),
    ("dynamics_rack", "dynamics_rack", 1.0),
    ("multiband_compressor", "multiband_compressor", 1.0),
    ("tube_compressor", "tube_compressor", 1.5),
    ("fft_filter", "fft_filter", 0.1),
    ("graphic_eq", "graphic_eq_10", 0.1),
    ("graphic_eq_20", "graphic_eq_20", 0.1),
    ("graphic_eq_30", "graphic_eq_30", 0.1),
    ("notch", "notch_filter", 0.1),
    ("parametric_eq", "parametric_eq_full", 0.1),
    ("scientific_filter", "scientific_filter", 0.2),
    ("chorus_flanger", "chorus_flanger", 0.1),
    ("flanger", "flanger", 0.2),
    ("phaser", "phaser", 0.1),
    ("declicker", "click_remover", 0.05),
    ("binauralizer", "binauralizer", 0.01),
    ("distortion", "distortion", 0.05),
    ("fill_left", "fill_left", 0.0),
    ("fill_right", "fill_right", 0.0),
    ("swap_channels", "swap_channels", 0.0),
    ("guitar_suite", "guitar_suite", 0.2),
    ("loudness_radar", "loudness_meter", 0.0),
    ("mastering", "mastering", 2.0),
    ("panner_ambisonics", "ambisonics_panner", 0.0),
    ("vocal_enhancer", "vocal_enhancer", 0.3),
    ("stereo_expander", "stereo_expander", 0.0),
    ("balance_a", "balance", 0.0),
    ("mute", "mute", 0.0),
    ("analog_delay", "analog_delay", 0.0),
    ("multitap_delay", "multitap_delay", 0.0),
    ("convolution_reverb", "convolution_reverb", 0.0),
    ("surround_reverb", "surround_reverb", 0.0),
];

/// Pre-roll of a direct mapping: its own tail, and at least long enough for the DSP's
/// parameter smoothing (≤ 100 ms) to settle from the defaults to the clip's settings.
fn direct_preroll(id: &str) -> f64 {
    DIRECT.iter().find(|(p, _, _)| *p == id).map_or(0.0, |d| d.2).max(0.12)
}

/// Set a DSP parameter only when it changed (keeps parameter-dependent rebuilds — impulse
/// responses, filter designs — off the per-block path).
fn set_if_changed(d: &mut dyn AudioEffect, id: &str, v: f32) {
    if d.param(id) != Some(v) {
        d.set_param(id, v);
    }
}

/// Copy every scalar project parameter (Float / Choice / Bool) to the DSP parameter of the
/// same id.
fn direct(d: &mut dyn AudioEffect, e: &EffectInstance, t: Tick) {
    let Some(def) = e.def() else { return };
    for p in &def.params {
        let v = e.params.get(p.id).map_or_else(|| p.default.as_f64().unwrap_or(0.0), |q| q.scalar_at(t));
        // sanitise like the DSP would, so unchanged values compare equal
        let v = d.params().iter().find(|s| s.id == p.id).map_or(v as f32, |s| s.sanitize(v as f32));
        set_if_changed(d, p.id, v);
    }
}

/// A DSP instance configured like the project effect `e` at media time `t` (for the graphical
/// effect editors: [`AudioEffect::response_db`] / [`AudioEffect::transfer_db`]).
pub fn configured(e: &EffectInstance, t: Tick, sample_rate: u32) -> Option<Box<dyn AudioEffect>> {
    let m = mapping(&e.effect)?;
    let mut d = filmcraft_audio_dsp::create_effect(m.dsp, sample_rate as f32, 2)?;
    (m.apply)(d.as_mut(), e, t);
    Some(d)
}

/// Processing latency (samples) of a project audio effect at `sample_rate`.
pub fn latency(effect_id: &str, sample_rate: u32) -> usize {
    mapping(effect_id).and_then(|m| filmcraft_audio_dsp::create_effect(m.dsp, sample_rate as f32, 2)).map_or(0, |d| d.latency())
}

fn shelf(d: &mut dyn AudioEffect, kind: f32, freq: f32, gain: f32) {
    band(d, kind, freq, std::f32::consts::FRAC_1_SQRT_2);
    set_if_changed(d, "b1.gain", gain);
}

/// Whether a project audio effect has a DSP implementation (clip effects and mixer inserts).
pub fn supported(effect_id: &str) -> bool {
    mapping(effect_id).is_some()
}

fn band(d: &mut dyn AudioEffect, kind: f32, freq: f32, q: f32) {
    d.set_param("b1.on", 1.0);
    d.set_param("b1.type", kind);
    d.set_param("b1.freq", freq);
    d.set_param("b1.q", q);
}

/// The item's enabled, DSP-backed audio effects in rack order.
fn active(item: &TrackItem) -> Vec<(&EffectInstance, Mapping)> {
    item.effects.iter().filter(|e| e.enabled && !e.def().is_some_and(|d| d.intrinsic)).filter_map(|e| mapping(&e.effect).map(|m| (e, m))).collect()
}

/// Whether the item has audio effects that change its signal.
pub fn has_effects(item: &TrackItem) -> bool {
    !active(item).is_empty()
}

struct Chain {
    fx: Vec<Box<dyn AudioEffect>>,
    latency: usize,
    /// Next timeline sample this chain will output.
    next_out: i64,
}

type Key = (u64, u64, u32, usize);

fn cache() -> &'static Mutex<HashMap<Key, Vec<Chain>>> {
    static C: OnceLock<Mutex<HashMap<Key, Vec<Chain>>>> = OnceLock::new();
    C.get_or_init(Default::default)
}

fn structure_hash(item: &TrackItem) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    for (e, m) in active(item) {
        e.effect.hash(&mut h);
        m.dsp.hash(&mut h);
    }
    h.finish()
}

const BLOCK: usize = 256;

/// Run `input` (planar, `input[c].len()` samples starting at timeline sample `in0`) through the
/// chain, setting keyframed parameters per block.
fn run(chain: &mut Chain, item: &TrackItem, in0: i64, input: &mut [Vec<f32>], sr: u32) {
    let act = active(item);
    let n = input[0].len();
    let mut i = 0;
    while i < n {
        // absolute block grid: parameters update at the same samples however requests are cut
        let pos = in0 + i as i64;
        let end = (((pos.div_euclid(BLOCK as i64) + 1) * BLOCK as i64 - in0) as usize).min(n);
        let mt = item.source_time_at(Tick::from_units(pos.div_euclid(BLOCK as i64) * BLOCK as i64, sr as i64));
        for ((e, m), d) in act.iter().zip(chain.fx.iter_mut()) {
            (m.apply)(d.as_mut(), e, mt);
        }
        let mut chans: Vec<&mut [f32]> = input.iter_mut().map(|c| &mut c[i..end]).collect();
        for d in chain.fx.iter_mut() {
            d.process(&mut chans);
        }
        i = end;
    }
}

/// Process the item's audio for timeline samples `[a0, a0 + n)`. `read(x0, len)` returns the clip's
/// raw planar audio (2 channels, or 6 on a 5.1 track) for timeline samples `[x0, x0 + len)`
/// (silence outside the clip).
pub fn process(item: &TrackItem, a0: i64, n: usize, sr: u32, read: &dyn Fn(i64, usize) -> Vec<Vec<f32>>) -> Vec<Vec<f32>> {
    let width = read(a0, 0).len().max(1);
    let key = (item.id.0, structure_hash(item), sr, width);
    let taken = {
        let mut c = cache().lock().unwrap_or_else(|e| e.into_inner());
        c.get_mut(&key).and_then(|v| v.iter().position(|ch| ch.next_out == a0).map(|i| v.swap_remove(i)))
    };
    let (mut chain, mut input, skip) = match taken {
        Some(chain) => {
            let l = chain.latency as i64;
            (chain, read(a0 + l, n), 0usize)
        }
        None => {
            let act = active(item);
            let mt = item.source_time_at(Tick::from_units(a0, sr as i64));
            let pre_s = act.iter().map(|(e, m)| (m.preroll)(e, mt)).fold(0.0, f64::max);
            let pre = (pre_s * sr as f64).ceil() as usize;
            let mut fx: Vec<Box<dyn AudioEffect>> = Vec::new();
            for (_, m) in &act {
                if let Some(d) = filmcraft_audio_dsp::create_effect(m.dsp, sr as f32, width) {
                    fx.push(d);
                }
            }
            let latency = fx.iter().map(|d| d.latency()).sum::<usize>();
            let chain = Chain { fx, latency, next_out: a0 - pre as i64 };
            let x0 = a0 - pre as i64;
            (chain, read(x0 + latency as i64, pre + n), pre)
        }
    };
    let in0 = chain.next_out + chain.latency as i64;
    run(&mut chain, item, in0, &mut input, sr);
    chain.next_out = a0 + n as i64;
    let out: Vec<Vec<f32>> = input.iter().map(|c| c[skip..].to_vec()).collect();
    let mut c = cache().lock().unwrap_or_else(|e| e.into_inner());
    let v = c.entry(key).or_default();
    v.push(chain);
    if v.len() > 4 {
        v.remove(0);
    }
    if c.len() > 256 {
        c.clear();
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use filmcraft_audio_dsp::Unit;
    use filmcraft_project::{EffectKind, ParamKind, effect_defs};

    #[test]
    fn every_audio_effect_has_dsp() {
        for d in effect_defs().iter().filter(|d| d.kind == EffectKind::Audio && !d.intrinsic) {
            let m = mapping(d.id).unwrap_or_else(|| panic!("{} has no DSP mapping", d.id));
            assert!(filmcraft_audio_dsp::effect_info(m.dsp).is_some(), "{} → unknown DSP {}", d.id, m.dsp);
            let inst = d.instance();
            let mut fx = configured(&inst, Tick::ZERO, 48000).unwrap();
            let mut l: Vec<f32> = (0..4800).map(|i| ((i * 7919 % 1000) as f32 / 1000.0 - 0.5) * 0.5).collect();
            let mut r = l.clone();
            fx.process(&mut [&mut l, &mut r]);
            assert!(l.iter().chain(&r).all(|v| v.is_finite()), "{}", d.id);
        }
        assert_eq!(filmcraft_project::effect::PREMIERE_AUDIO_EFFECTS.len(), 53);
    }

    #[test]
    fn direct_mappings_match_dsp_parameters() {
        for &(pid, dsp, _) in DIRECT {
            let def = filmcraft_project::find_effect(pid).unwrap_or_else(|| panic!("{pid}"));
            let info = filmcraft_audio_dsp::effect_info(dsp).unwrap_or_else(|| panic!("{dsp}"));
            for p in &def.params {
                let s = info.params.iter().find(|s| s.id == p.id).unwrap_or_else(|| panic!("{pid}.{} missing in {dsp}", p.id));
                match &p.kind {
                    ParamKind::Float { min, max, .. } => {
                        assert!(*min >= s.min as f64 - 1e-3 && *max <= s.max as f64 + 1e-3, "{pid}.{}: range {min}..{max} vs {}..{}", p.id, s.min, s.max);
                        let dv = p.default.as_f64().unwrap();
                        assert!((dv - s.default as f64).abs() < 0.02, "{pid}.{}: default {dv} vs {}", p.id, s.default);
                    }
                    ParamKind::Choice(opts) => {
                        assert_eq!(s.unit, Unit::Choice, "{pid}.{}", p.id);
                        assert_eq!(opts.len(), s.choices.len(), "{pid}.{}", p.id);
                        assert_eq!(p.default.as_f64().unwrap(), s.default as f64, "{pid}.{}", p.id);
                    }
                    ParamKind::Bool => {
                        assert_eq!(s.unit, Unit::Toggle, "{pid}.{}", p.id);
                        assert_eq!(p.default.as_f64().unwrap(), s.default as f64, "{pid}.{}", p.id);
                    }
                    k => panic!("{pid}.{}: unsupported kind {k:?}", p.id),
                }
            }
        }
    }

    #[test]
    fn editor_curves_follow_project_parameters() {
        let def = filmcraft_project::find_effect("graphic_eq").unwrap();
        let mut inst = def.instance();
        inst.param_mut("b6").unwrap().value = filmcraft_project::ParamValue::Float(9.0);
        let fx = configured(&inst, Tick::ZERO, 48000).unwrap();
        assert!((fx.response_db(1000.0).unwrap() - 9.0).abs() < 1.0);
        let def = filmcraft_project::find_effect("dynamics_rack").unwrap();
        let fx = configured(&def.instance(), Tick::ZERO, 48000).unwrap();
        assert!((fx.transfer_db(0, 0.0).unwrap() + 10.0).abs() < 1e-3, "−20 dB threshold, 2:1");
        assert_eq!(latency("fft_filter", 48000), 2048);
        assert_eq!(latency("amplify", 48000), 0);
    }

    #[test]
    fn parametric_eq_applies_every_control() {
        let def = filmcraft_project::find_effect("parametric_eq").unwrap();
        let resp = |id: &str, v: filmcraft_project::ParamValue, extra: Option<(&str, filmcraft_project::ParamValue)>| {
            let mut inst = def.instance();
            inst.param_mut(id).unwrap().value = v;
            if let Some((k, x)) = extra {
                inst.param_mut(k).unwrap().value = x;
            }
            configured(&inst, Tick::ZERO, 48000).unwrap().response_db(1000.0).unwrap()
        };
        use filmcraft_project::ParamValue::{Bool, Float};
        assert!((resp("master_gain", Float(-30.0), None) + 30.0).abs() < 1.0);
        for b in ["b1", "b2", "b4", "b5"] {
            let g = resp(&format!("{b}_gain"), Float(-24.0), Some((&format!("{b}_freq"), Float(1000.0))));
            assert!((g + 24.0).abs() < 1.0, "{b}: {g}");
        }
        assert!(resp("hp_on", Bool(true), Some(("hp_freq", Float(10000.0)))) < -20.0);
        assert!(resp("lp_on", Bool(true), Some(("lp_freq", Float(100.0)))) < -20.0);
    }
}
