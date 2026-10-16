//! Audio effects and their registry.

pub mod channel;
pub mod declick;
pub mod dynamics;
pub mod eq;
pub mod essential;
pub mod filters;
pub mod modulation;
pub mod multiband;
pub mod pitch;
pub mod restoration;
pub mod reverbs;
pub mod special;
mod stft;
pub mod time;
pub(crate) mod util;

pub use channel::{Amplify, Balance, ChannelVolume, Invert, Route, Routing};
pub use declick::ClickRemover;
pub use dynamics::{Compressor, Gate, Limiter};
pub use eq::{ParametricEq, SimpleEq};
pub use essential::{DeEsser, DeReverb, SpeechEnhance, StereoWidth};
pub use filters::{FftFilter, FullParametricEq, GraphicEq, NotchFilter, ScientificFilter};
pub use modulation::{AnalogDelay, ChorusFlanger, Flanger, MultitapDelay, Phaser};
pub use multiband::{DynamicsRack, MultibandCompressor, TubeCompressor};
pub use pitch::PitchShifter;
pub use restoration::{DeHum, DeNoise};
pub use reverbs::{ConvolutionReverb, SurroundReverb};
pub use special::{AmbisonicsPanner, Binauralizer, ChannelMixer, Distortion, GuitarSuite, LoudnessMeterFx, Mastering, Mute, StereoExpander, VocalEnhancer};
pub use time::{Delay, Reverb};

use crate::{AudioEffect, ParamSpec};

/// Effect category (for menus / the Effects panel's Audio Effects bin).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Category {
    Filter,
    Dynamics,
    Time,
    Channel,
    Restoration,
    Pitch,
    Modulation,
    Reverb,
    Special,
}

/// Registry entry describing one effect type.
#[derive(Clone, Copy)]
pub struct EffectInfo {
    pub id: &'static str,
    pub name: &'static str,
    pub category: Category,
    pub params: &'static [ParamSpec],
    /// Construct an instance for `(sample_rate, channels)`.
    pub create: fn(f32, usize) -> Box<dyn AudioEffect>,
}

impl std::fmt::Debug for EffectInfo {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EffectInfo")
            .field("id", &self.id)
            .field("name", &self.name)
            .field("category", &self.category)
            .field("params", &self.params.len())
            .finish()
    }
}

/// Sample rates the effects are built for. Anything outside (zero, negative, NaN, infinite or
/// absurdly large, as a corrupt file can claim) is mapped into range before construction so the
/// DSP designs and delay-line sizes stay finite.
const MIN_RATE: f32 = 1_000.0;
const MAX_RATE: f32 = 1_000_000.0;
/// Effects are built for at most this many channels; per the `AudioEffect` contract extra
/// channels in `process` are passed through (channel-generic effects still process them).
const MAX_CHANNELS: usize = 64;

fn sane_rate(sr: f32) -> f32 {
    // NaN, zero and negative rates are meaningless: the usual default; the rest is clamped
    if sr > 0.0 { sr.clamp(MIN_RATE, MAX_RATE) } else { 48_000.0 }
}

macro_rules! entry {
    ($id:literal, $name:literal, $cat:ident, $ty:ty) => {
        EffectInfo {
            id: $id,
            name: $name,
            category: Category::$cat,
            params: <$ty>::PARAMS,
            create: |sr, ch| Box::new(<$ty>::new(sane_rate(sr), ch.min(MAX_CHANNELS))),
        }
    };
}

static REGISTRY: &[EffectInfo] = &[
    entry!("parametric_eq", "Parametric Equalizer", Filter, ParametricEq),
    entry!("simple_eq", "Simple 3-Band EQ", Filter, SimpleEq),
    entry!("compressor", "Compressor", Dynamics, Compressor),
    entry!("gate", "Expander / Gate", Dynamics, Gate),
    entry!("limiter", "Hard Limiter", Dynamics, Limiter),
    entry!("delay", "Delay", Time, Delay),
    entry!("reverb", "Reverb", Time, Reverb),
    entry!("amplify", "Amplify", Channel, Amplify),
    entry!("channel_volume", "Channel Volume", Channel, ChannelVolume),
    entry!("balance", "Balance / Pan", Channel, Balance),
    entry!("invert", "Invert", Channel, Invert),
    EffectInfo {
        id: "swap_channels",
        name: "Swap Channels",
        category: Category::Channel,
        params: Route::PARAMS,
        create: |_, _| Box::new(Route::new(Routing::Swap)),
    },
    EffectInfo {
        id: "fill_left",
        name: "Fill Left with Right",
        category: Category::Channel,
        params: Route::PARAMS,
        create: |_, _| Box::new(Route::new(Routing::FillLeft)),
    },
    EffectInfo {
        id: "fill_right",
        name: "Fill Right with Left",
        category: Category::Channel,
        params: Route::PARAMS,
        create: |_, _| Box::new(Route::new(Routing::FillRight)),
    },
    entry!("dehum", "DeHum", Restoration, DeHum),
    entry!("denoise", "DeNoise", Restoration, DeNoise),
    entry!("pitch_shifter", "Pitch Shifter", Pitch, PitchShifter),
    entry!("deesser", "DeEsser", Restoration, DeEsser),
    entry!("dereverb", "DeReverb", Restoration, DeReverb),
    entry!("speech_enhance", "Enhance Speech", Filter, SpeechEnhance),
    entry!("stereo_width", "Stereo Width", Channel, StereoWidth),
    // Premiere audio-effect set (M7.7)
    entry!("graphic_eq_10", "Graphic Equalizer (10 Bands)", Filter, GraphicEq<10>),
    entry!("graphic_eq_20", "Graphic Equalizer (20 Bands)", Filter, GraphicEq<20>),
    entry!("graphic_eq_30", "Graphic Equalizer (30 Bands)", Filter, GraphicEq<30>),
    entry!("parametric_eq_full", "Parametric Equalizer (full)", Filter, FullParametricEq),
    entry!("notch_filter", "Notch Filter", Filter, NotchFilter),
    entry!("scientific_filter", "Scientific Filter", Filter, ScientificFilter),
    entry!("fft_filter", "FFT Filter", Filter, FftFilter),
    entry!("dynamics_rack", "Dynamics", Dynamics, DynamicsRack),
    entry!("multiband_compressor", "Multiband Compressor", Dynamics, MultibandCompressor),
    entry!("tube_compressor", "Tube-modeled Compressor", Dynamics, TubeCompressor),
    entry!("chorus_flanger", "Chorus/Flanger", Modulation, ChorusFlanger),
    entry!("flanger", "Flanger", Modulation, Flanger),
    entry!("phaser", "Phaser", Modulation, Phaser),
    entry!("analog_delay", "Analog Delay", Time, AnalogDelay),
    entry!("multitap_delay", "Multitap Delay", Time, MultitapDelay),
    entry!("convolution_reverb", "Convolution Reverb", Reverb, ConvolutionReverb),
    entry!("surround_reverb", "Surround Reverb", Reverb, SurroundReverb),
    entry!("click_remover", "Automatic Click Remover", Restoration, ClickRemover),
    entry!("channel_mixer", "Channel Mixer", Channel, ChannelMixer),
    entry!("distortion", "Distortion", Special, Distortion),
    entry!("guitar_suite", "GuitarSuite", Special, GuitarSuite),
    entry!("mastering", "Mastering", Special, Mastering),
    entry!("vocal_enhancer", "Vocal Enhancer", Special, VocalEnhancer),
    entry!("stereo_expander", "Stereo Expander", Channel, StereoExpander),
    entry!("binauralizer", "Binauralizer - Ambisonics", Special, Binauralizer),
    entry!("ambisonics_panner", "Panner - Ambisonics", Special, AmbisonicsPanner),
    entry!("loudness_meter", "Loudness Meter", Special, LoudnessMeterFx),
    entry!("mute", "Mute", Channel, Mute),
];

/// All registered effects.
pub fn effects() -> &'static [EffectInfo] {
    REGISTRY
}

/// Look up an effect by id.
pub fn effect_info(id: &str) -> Option<&'static EffectInfo> {
    REGISTRY.iter().find(|e| e.id == id)
}

/// Instantiate an effect by id.
pub fn create_effect(id: &str, sample_rate: f32, channels: usize) -> Option<Box<dyn AudioEffect>> {
    effect_info(id).map(|e| (e.create)(sample_rate, channels))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Unit;
    use crate::testutil::*;

    const SR: f32 = 48000.0;

    fn noise(seed: u64, n: usize, amp: f32) -> Vec<f32> {
        let mut r = Rng::new(seed);
        (0..n).map(|_| r.uniform() * amp).collect()
    }

    /// A non-default setting for every parameter (to exercise real processing paths).
    fn tweak(fx: &mut dyn AudioEffect) {
        for p in fx.params() {
            let v = match p.unit {
                Unit::Toggle => 1.0,
                Unit::Choice => p.max,
                _ => p.min + (p.max - p.min) * 0.37,
            };
            assert!(fx.set_param(p.id, v));
        }
    }

    #[test]
    fn registry_is_consistent() {
        let mut ids = std::collections::HashSet::new();
        for e in effects() {
            assert!(ids.insert(e.id), "duplicate id {}", e.id);
            let fx = (e.create)(SR, 2);
            assert_eq!(fx.id(), e.id);
            assert_eq!(fx.params(), e.params);
            let mut pids = std::collections::HashSet::new();
            for p in e.params {
                assert!(pids.insert(p.id), "{}: duplicate param {}", e.id, p.id);
                assert!(p.min <= p.default && p.default <= p.max, "{}.{}", e.id, p.id);
                assert_eq!(fx.param(p.id), Some(p.default));
                if p.unit == Unit::Choice {
                    assert_eq!(p.choices.len() as f32, p.max + 1.0);
                }
            }
        }
        assert!(effect_info("reverb").is_some());
        assert!(create_effect("nope", SR, 2).is_none());
        let mut c = create_effect("compressor", SR, 2).unwrap();
        assert!(!c.set_param("nope", 1.0));
        assert!(c.set_param("ratio", 1000.0));
        assert_eq!(c.param("ratio"), Some(30.0));
        assert!(c.set_param("ratio", f32::NAN));
        assert_eq!(c.param("ratio"), Some(4.0));
    }

    #[test]
    fn block_size_independence_with_param_changes() {
        let n = 30000;
        let src_l = noise(1, n, 0.7);
        let src_r = noise(2, n, 0.7);
        let change_at = 11111;
        for e in effects() {
            let run = |sizes: &mut dyn FnMut() -> usize| {
                let mut fx = (e.create)(SR, 2);
                let (mut l, mut r) = (src_l.clone(), src_r.clone());
                let mut pos = 0;
                let mut changed = false;
                while pos < n {
                    if !changed && pos == change_at {
                        tweak(fx.as_mut());
                        changed = true;
                    }
                    let mut len = sizes().min(n - pos);
                    if !changed && pos + len > change_at {
                        len = change_at - pos;
                    }
                    fx.process(&mut [&mut l[pos..pos + len], &mut r[pos..pos + len]]);
                    pos += len;
                }
                (l, r)
            };
            let big = run(&mut || usize::MAX);
            let mut rng = Rng::new(5);
            let small = run(&mut || match rng.next_u64() % 4 {
                0 => 0,
                1 => 1,
                2 => (rng.next_u64() % 64) as usize,
                _ => (rng.next_u64() % 3000) as usize,
            });
            for (a, b) in big.0.iter().chain(&big.1).zip(small.0.iter().chain(&small.1)) {
                assert!((a - b).abs() <= 1e-6, "{}: block-size dependent output", e.id);
            }
            assert!(big.0.iter().chain(&big.1).all(|v| v.is_finite()), "{}", e.id);
        }
    }

    #[test]
    fn zero_length_and_single_sample_blocks() {
        for e in effects() {
            let mut fx = (e.create)(SR, 2);
            fx.process(&mut []);
            fx.process(&mut [&mut [], &mut []]);
            let (mut a, mut b) = ([0.25f32], [-0.25f32]);
            fx.process(&mut [&mut a, &mut b]);
            // Mono and more channels than configured don't panic.
            fx.process(&mut [&mut [0.1f32; 3]]);
            fx.process(&mut [&mut [0.1f32; 3], &mut [0.1; 3], &mut [0.1; 3]]);
            assert!(a[0].is_finite() && b[0].is_finite(), "{}", e.id);
        }
    }

    #[test]
    fn extreme_settings_stay_finite_and_denormal_free() {
        for e in effects() {
            for setting in [0.0f32, 1.0] {
                let mut fx = (e.create)(SR, 2);
                for p in e.params {
                    fx.set_param(p.id, if setting == 0.0 { p.min } else { p.max });
                }
                let mut l = noise(3, 24000, 4.0);
                let mut r = noise(4, 24000, 4.0);
                l[100] = 1e6;
                r[200] = -1e6;
                fx.process(&mut [&mut l, &mut r]);
                assert!(l.iter().chain(&r).all(|v| v.is_finite()), "{} setting {setting}", e.id);
                // Long silence afterwards: output decays without denormals.
                fx.reset();
                let mut l = noise(3, 4800, 0.5);
                let mut r = noise(4, 4800, 0.5);
                fx.process(&mut [&mut l, &mut r]);
                for _ in 0..40 {
                    let mut l = vec![0.0f32; 48000];
                    let mut r = vec![0.0f32; 48000];
                    fx.process(&mut [&mut l, &mut r]);
                    assert!(l.iter().chain(&r).all(|v| v.is_finite() && !v.is_subnormal()), "{} setting {setting}: denormal/NaN in tail", e.id);
                }
            }
        }
    }

    #[test]
    fn reset_restores_initial_behaviour() {
        for e in effects() {
            let mut fx = (e.create)(SR, 2);
            let x = noise(8, 6000, 0.5);
            let (mut a1, mut a2) = (x.clone(), x.clone());
            fx.process(&mut [&mut a1, &mut a2]);
            let (mut j1, mut j2) = (noise(9, 3000, 0.9), noise(10, 3000, 0.9));
            fx.process(&mut [&mut j1, &mut j2]);
            fx.reset();
            let (mut b1, mut b2) = (x.clone(), x.clone());
            fx.process(&mut [&mut b1, &mut b2]);
            for (p, q) in a1.iter().zip(&b1) {
                assert!((p - q).abs() < 1e-6, "{}: reset didn't clear state", e.id);
            }
        }
    }
}

#[cfg(test)]
mod premiere_tests;
