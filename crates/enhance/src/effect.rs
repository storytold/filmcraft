//! The audio effect: a [`filmcraft_audio_dsp::AudioEffect`] around a [`Streamer`].

use filmcraft_audio_dsp::{AudioEffect, ParamSpec, Unit};

use crate::catalog;
use crate::stream::{FrameModel, Streamer};

/// Registry id of the DSP effect.
pub const ID: &str = "neural_enhance";

/// The model names, in catalogue order (the `model` choice stores the index).
pub const MODEL_NAMES: &[&str] = &["DeepFilterNet 3", "DeepFilterNet 3 (low latency)"];

/// Default noise attenuation limit, dB. Moderate on purpose: it leaves some room tone and keeps
/// quiet, distant voices, which full strength tends to remove with the noise.
pub const DEFAULT_ATTENUATION_DB: f32 = 24.0;

/// At this attenuation limit the model removes as much as it can.
pub const UNLIMITED_DB: f32 = 100.0;

pub static PARAMS: &[ParamSpec] = &[
    ParamSpec::choice("model", "Model", MODEL_NAMES, 0),
    ParamSpec::new("attenuation", "Noise Reduction", 0.0, UNLIMITED_DB, DEFAULT_ATTENUATION_DB, Unit::Decibels),
    ParamSpec::toggle("post_filter", "Post Filter", false),
    ParamSpec::new("mix", "Mix", 0.0, 100.0, 100.0, Unit::Percent),
];

/// Why an instance does not enhance.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Bypass {
    /// The host has not set a models directory.
    NoModelsDir,
    /// The selected model is not downloaded (its id).
    NotInstalled(&'static str),
    /// This build has no inference backend (built without the `deepfilter` feature).
    Unavailable,
    /// The sequence rate or channel count cannot be served.
    Unsupported,
    /// The model could not be loaded.
    Load(String),
}

impl std::fmt::Display for Bypass {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Bypass::NoModelsDir => write!(f, "no models directory"),
            Bypass::NotInstalled(id) => write!(f, "the model `{id}` is not downloaded"),
            Bypass::Unavailable => write!(f, "neural speech enhancement is not available in this build"),
            Bypass::Unsupported => write!(f, "unsupported sample rate or channel count"),
            Bypass::Load(e) => write!(f, "the model could not be loaded: {e}"),
        }
    }
}

/// Neural speech enhancement. Without a usable model it passes audio through with no latency.
pub struct NeuralEnhance {
    values: [f32; 4],
    stream: Option<Streamer>,
    bypass: Option<Bypass>,
    configured: Option<(f32, bool)>,
}

impl NeuralEnhance {
    pub const PARAMS: &'static [ParamSpec] = PARAMS;

    /// The effect for the catalogue model `model_index`, loaded from the models directory given
    /// to [`catalog::set_models_dir`].
    pub fn new(sample_rate: f32, channels: usize, model_index: usize) -> Self {
        let rate = if sample_rate.is_finite() && sample_rate >= 1.0 { sample_rate.round().min(u32::MAX as f32) as u32 } else { 0 };
        let usable = (crate::stream::MIN_RATE..=crate::stream::MAX_RATE).contains(&rate) && (1..=crate::stream::MAX_CHANNELS).contains(&channels);
        let loaded = if usable {
            load(model_index, channels).and_then(|m| Streamer::new(m, rate, channels).ok_or(Bypass::Unsupported))
        } else {
            Err(Bypass::Unsupported)
        };
        let mut fx = Self::assemble(loaded);
        fx.values[0] = PARAMS[0].sanitize(model_index.min(MODEL_NAMES.len()) as f32);
        fx
    }

    fn assemble(loaded: Result<Streamer, Bypass>) -> Self {
        let (stream, bypass) = match loaded {
            Ok(s) => (Some(s), None),
            Err(b) => (None, Some(b)),
        };
        NeuralEnhance { values: [PARAMS[0].default, PARAMS[1].default, PARAMS[2].default, PARAMS[3].default], stream, bypass, configured: None }
    }

    #[cfg(test)]
    pub(crate) fn with_model(model: Box<dyn FrameModel>, sample_rate: u32, channels: usize) -> Self {
        Self::assemble(Streamer::new(model, sample_rate, channels).ok_or(Bypass::Unsupported))
    }

    /// Why the effect passes audio through, if it does.
    pub fn bypass(&self) -> Option<&Bypass> {
        self.bypass.as_ref()
    }

    /// The model stopped with an error while processing.
    pub fn failed(&self) -> bool {
        self.stream.as_ref().is_some_and(Streamer::failed)
    }
}

#[cfg(feature = "deepfilter")]
fn load(model_index: usize, channels: usize) -> Result<Box<dyn FrameModel>, Bypass> {
    let dir = catalog::models_dir().ok_or(Bypass::NoModelsDir)?;
    let info = catalog::by_index(model_index).ok_or(Bypass::Unavailable)?;
    if !catalog::installed(&dir, info) {
        return Err(Bypass::NotInstalled(info.id));
    }
    match info.backend {
        catalog::Backend::DeepFilterNet => {
            let file = info.files.first().ok_or(Bypass::Unavailable)?;
            crate::dfn::Dfn::load(&catalog::model_dir(&dir, info).join(file.name), channels).map(|m| Box::new(m) as Box<dyn FrameModel>).map_err(Bypass::Load)
        }
    }
}

#[cfg(not(feature = "deepfilter"))]
fn load(model_index: usize, _channels: usize) -> Result<Box<dyn FrameModel>, Bypass> {
    let _ = (catalog::by_index(model_index), catalog::models_dir());
    Err(Bypass::Unavailable)
}

impl AudioEffect for NeuralEnhance {
    fn id(&self) -> &'static str {
        ID
    }

    fn params(&self) -> &'static [ParamSpec] {
        PARAMS
    }

    fn set_param(&mut self, id: &str, value: f32) -> bool {
        match PARAMS.iter().position(|p| p.id == id) {
            Some(i) => {
                self.values[i] = PARAMS[i].sanitize(value);
                true
            }
            None => false,
        }
    }

    fn param(&self, id: &str) -> Option<f32> {
        PARAMS.iter().position(|p| p.id == id).map(|i| self.values[i])
    }

    fn reset(&mut self) {
        if let Some(s) = self.stream.as_mut() {
            s.reset();
        }
    }

    fn process(&mut self, channels: &mut [&mut [f32]]) {
        let Some(s) = self.stream.as_mut() else { return };
        let want = (self.values[1], self.values[2] >= 0.5);
        if self.configured != Some(want) {
            s.configure(want.0, want.1);
            self.configured = Some(want);
        }
        s.process(channels, self.values[3] / 100.0);
    }

    fn latency(&self) -> usize {
        self.stream.as_ref().map_or(0, Streamer::latency)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::stream::tests::Mock;

    #[test]
    fn the_model_names_follow_the_catalogue() {
        let names: Vec<_> = catalog::catalogue().iter().map(|m| m.name).collect();
        assert_eq!(names, MODEL_NAMES);
        assert_eq!(catalog::by_index(0).map(|m| m.id), Some(catalog::DEFAULT_MODEL));
    }

    #[test]
    fn without_a_model_audio_passes_through_untouched() {
        let mut fx = NeuralEnhance::new(48_000.0, 2, 0);
        assert!(fx.bypass().is_some());
        assert_eq!(fx.latency(), 0);
        let mut l: Vec<f32> = (0..1000).map(|i| (i as f32 * 0.01).sin()).collect();
        let (want, mut r) = (l.clone(), l.clone());
        fx.process(&mut [&mut l, &mut r]);
        fx.reset();
        assert_eq!(l, want);
        assert_eq!(r, want);
    }

    #[test]
    fn hostile_construction_parameters_give_a_pass_through() {
        for (rate, ch, model) in [(f32::NAN, 2, 0), (0.0, 0, usize::MAX), (-48_000.0, 2, 1), (f32::INFINITY, usize::MAX, 7), (1e12, 2, 0)] {
            let mut fx = NeuralEnhance::new(rate, ch, model);
            let mut x = vec![0.5f32; 64];
            fx.process(&mut [&mut x]);
            assert!(x.iter().all(|v| *v == 0.5));
            assert_eq!(fx.latency(), 0);
        }
    }

    #[test]
    fn parameters_are_clamped_and_unknown_ids_refused() {
        let mut fx = NeuralEnhance::new(48_000.0, 2, 0);
        assert_eq!(fx.param("attenuation"), Some(DEFAULT_ATTENUATION_DB));
        assert!(fx.set_param("attenuation", 1e9));
        assert_eq!(fx.param("attenuation"), Some(UNLIMITED_DB));
        assert!(fx.set_param("attenuation", f32::NAN));
        assert_eq!(fx.param("attenuation"), Some(DEFAULT_ATTENUATION_DB));
        assert!(fx.set_param("mix", -5.0));
        assert_eq!(fx.param("mix"), Some(0.0));
        assert!(fx.set_param("model", 99.0));
        assert_eq!(fx.param("model"), Some((MODEL_NAMES.len() - 1) as f32));
        assert!(!fx.set_param("nope", 1.0));
        assert_eq!(fx.param("nope"), None);
        assert_eq!(fx.params(), PARAMS);
        assert_eq!(fx.id(), ID);
    }

    #[test]
    fn the_attenuation_limit_reaches_the_model_and_the_mix_scales_it() {
        let mut fx = NeuralEnhance::with_model(Box::new(Mock::new(48_000, 480, 0)), 48_000, 1);
        assert!(fx.bypass().is_none());
        assert_eq!(fx.latency(), 480);
        fx.set_param("attenuation", 20.0);
        let mut x = vec![1.0f32; 4800];
        fx.process(&mut [&mut x]);
        // the mock applies the limit as a plain gain: −20 dB
        assert!(x[2400..].iter().all(|v| (v - 0.1).abs() < 1e-6), "{}", x[2400]);
        fx.set_param("mix", 50.0);
        // the mix moves to its new value over one block
        fx.process(&mut [&mut vec![1.0f32; 256]]);
        let mut x = vec![1.0f32; 4800];
        fx.process(&mut [&mut x]);
        assert!(x[2400..].iter().all(|v| (v - 0.55).abs() < 1e-6), "{}", x[2400]);
    }
}
