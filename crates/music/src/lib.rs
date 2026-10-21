//! Music generation for FilmCraft: a provider-agnostic interface.
//!
//! The local goal is to run Stable Audio 3 (small/medium) inside FilmCraft with no Python,
//! exported to ONNX and/or Core ML, driven by a Rust runtime (ONNX Runtime or candle). Weights are
//! downloaded on demand into the app model cache after the user accepts the licence. If the
//! export/runtime path is blocked, the engine falls back to a documented external Python worker.
//!
//! All providers implement [`Provider`]. The engine only sees `Params` and [`Audio`]; provenance is
//! recorded in a sidecar so generated music can be audited.

#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable))]

use serde::{Deserialize, Serialize};

pub mod catalog;
pub mod hosted;
pub mod models;
#[cfg(feature = "native")]
pub mod native;
pub mod provenance;
pub mod sa3;
pub mod wav;

pub const SAMPLE_RATE: u32 = 44_100;
pub const MAX_SECONDS: f64 = 5.0 * 60.0;

#[derive(Debug, thiserror::Error, PartialEq)]
pub enum MusicError {
    #[error("unknown provider `{0}`")]
    UnknownProvider(String),
    #[error("provider `{0}` is not available")]
    Unavailable(String),
    #[error("the prompt is empty")]
    EmptyPrompt,
    #[error("the prompt is too long")]
    PromptTooLong,
    #[error("duration must be between {0} and {1} seconds")]
    BadDuration(f64, f64),
    #[error("BPM must be positive")]
    BadBpm,
    #[error("unsupported capability `{0}` for provider `{1}`")]
    UnsupportedCapability(String, String),
    #[error("inference failed: {0}")]
    Inference(String),
    #[error("model not installed: {0}")]
    NotInstalled(String),
    #[error("download failed: {0}")]
    Download(String),
    #[error("network error: {0}")]
    Network(String),
    #[error("configuration error: {0}")]
    Config(String),
    #[error("{0}")]
    Invalid(String),
}

/// Generation parameters sent to every provider.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Params {
    /// Free-form prompt / mood (required).
    pub prompt: String,
    /// Negative prompt / what to avoid.
    pub negative_prompt: String,
    /// Target duration in seconds.
    pub duration: f64,
    /// Optional BPM hint.
    pub bpm: Option<f64>,
    /// Optional key hint.
    pub key: Option<String>,
    /// Instrumental only (no vocals). Providers that cannot guarantee this still report it in
    /// provenance.
    pub instrumental: bool,
    /// Seed for deterministic providers.
    pub seed: Option<u64>,
}

impl Default for Params {
    fn default() -> Self {
        Self { prompt: String::new(), negative_prompt: String::new(), duration: 30.0, bpm: None, key: None, instrumental: true, seed: None }
    }
}

impl Params {
    /// The text a prompt-driven model sees: the prompt plus the BPM, key and instrumental hints
    /// (Stable Audio takes them as words, e.g. "... 124 BPM"), without repeating what the prompt
    /// already says.
    pub fn compose_prompt(&self) -> String {
        let mut out = self.prompt.trim().to_string();
        let lower = out.to_lowercase();
        if let Some(b) = self.bpm.filter(|b| b.is_finite() && *b > 0.0)
            && !lower.contains("bpm")
        {
            out.push_str(&format!(", {} BPM", b.round() as i64));
        }
        if let Some(k) = self.key.as_deref().map(str::trim).filter(|k| !k.is_empty() && k.len() <= 32)
            && !lower.contains(&k.to_lowercase())
        {
            out.push_str(&format!(", in {k}"));
        }
        if self.instrumental && !lower.contains("instrumental") {
            out.push_str(", instrumental");
        }
        out
    }

    pub fn check(&self) -> Result<(), MusicError> {
        if self.prompt.trim().is_empty() {
            return Err(MusicError::EmptyPrompt);
        }
        if self.prompt.len() > 4096 {
            return Err(MusicError::PromptTooLong);
        }
        if !(1.0..=MAX_SECONDS).contains(&self.duration) {
            return Err(MusicError::BadDuration(1.0, MAX_SECONDS));
        }
        if self.bpm.is_some_and(|b| !b.is_finite() || b <= 0.0) {
            return Err(MusicError::BadBpm);
        }
        Ok(())
    }
}

/// Stereo or mono audio at [`SAMPLE_RATE`].
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Audio {
    pub samples: Vec<f32>,
    pub sample_rate: u32,
    pub channels: u16,
}

impl Audio {
    pub fn seconds(&self) -> f64 {
        let denom = self.sample_rate.max(1) as usize * self.channels.max(1) as usize;
        self.samples.len() as f64 / denom as f64
    }

    pub fn is_empty(&self) -> bool {
        self.samples.is_empty()
    }
}

/// A capability a provider may advertise.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Hash)]
#[serde(rename_all = "camelCase")]
pub enum Capability {
    Instrumental,
    Duration,
    Bpm,
    Key,
    Seed,
    NegativePrompt,
    LocalInference,
}

/// Static metadata for a provider.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderInfo {
    pub id: &'static str,
    pub name: &'static str,
    /// `local` | `hosted` | `worker`.
    pub kind: &'static str,
    pub capabilities: &'static [Capability],
    pub license: &'static str,
    pub source: &'static str,
    pub description: &'static str,
    pub installed: bool,
}

/// A provider that turns [`Params`] into [`Audio`].
pub trait Provider: Send + Sync {
    fn info(&self) -> ProviderInfo;
    fn generate(&self, params: &Params) -> Result<Audio, MusicError>;
    /// Like [`Provider::generate`], reporting a fraction in `0..=1`; `progress` returns `false` to
    /// cancel. Providers without fine grained progress use this default.
    fn generate_with_progress(&self, params: &Params, progress: &mut dyn FnMut(f32) -> bool) -> Result<Audio, MusicError> {
        if !progress(0.0) {
            return Err(MusicError::Inference("cancelled".into()));
        }
        let audio = self.generate(params)?;
        progress(1.0);
        Ok(audio)
    }
}

/// Every built-in provider. Add entries here as more open-weight or hosted APIs are integrated.
pub fn providers(models_dir: Option<&std::path::Path>) -> Vec<ProviderInfo> {
    vec![catalog::stable_audio_local_info(models_dir), catalog::stable_audio_hosted_info(), catalog::audio_api_info()]
}

/// Resolve a provider by id, returning a boxed instance if available.
pub fn provider(id: &str, models_dir: Option<&std::path::Path>, cfg: &std::collections::HashMap<String, String>) -> Result<Box<dyn Provider>, MusicError> {
    match id {
        catalog::STABLE_AUDIO_LOCAL => catalog::stable_audio_local(models_dir, cfg),
        catalog::STABLE_AUDIO_HOSTED => catalog::stable_audio_hosted(cfg),
        catalog::AUDIO_API => catalog::audio_api(cfg),
        _ => Err(MusicError::UnknownProvider(id.into())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn params_reject_empty_and_bad_duration() {
        let mut p = Params { prompt: "   ".into(), ..Default::default() };
        assert_eq!(p.check(), Err(MusicError::EmptyPrompt));
        p.prompt = "a cue".into();
        p.duration = 0.5;
        assert_eq!(p.check(), Err(MusicError::BadDuration(1.0, MAX_SECONDS)));
        p.duration = 600.0;
        assert!(matches!(p.check(), Err(MusicError::BadDuration(_, _))));
        p.duration = 30.0;
        assert!(p.check().is_ok());
    }

    #[test]
    fn hints_become_words_once() {
        let p = Params { prompt: "warm pad".into(), bpm: Some(70.4), key: Some("D minor".into()), ..Default::default() };
        assert_eq!(p.compose_prompt(), "warm pad, 70 BPM, in D minor, instrumental");
        let q = Params { prompt: "Instrumental pad 90 bpm in d minor".into(), bpm: Some(90.0), key: Some("D minor".into()), ..Default::default() };
        assert_eq!(q.compose_prompt(), "Instrumental pad 90 bpm in d minor");
        let r = Params { prompt: "x".into(), bpm: Some(f64::NAN), instrumental: false, ..Default::default() };
        assert_eq!(r.compose_prompt(), "x");
    }

    #[test]
    fn provider_list_has_local_and_hosted() {
        let v = providers(None);
        assert!(v.iter().any(|i| i.id == catalog::STABLE_AUDIO_LOCAL));
        assert!(v.iter().any(|i| i.id == catalog::STABLE_AUDIO_HOSTED));
        assert!(v.iter().any(|i| i.id == catalog::AUDIO_API));
    }
}
