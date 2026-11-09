//! Generated-music provenance: a JSON sidecar next to every generated WAV so licensing can be
//! audited later. Never written into the project file.

use serde::{Deserialize, Serialize};

use crate::{Capability, Params};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Provenance {
    pub provider: String,
    pub provider_kind: String,
    pub model: String,
    pub prompt: String,
    pub negative_prompt: String,
    pub duration: f64,
    pub bpm: Option<f64>,
    pub key: Option<String>,
    pub instrumental: bool,
    pub seed: Option<u64>,
    pub capabilities: Vec<Capability>,
    pub sample_rate: u32,
    pub channels: u16,
    pub samples: usize,
    pub seconds: f64,
    pub license: String,
    pub source: String,
    pub generated_at_utc_unix: u64,
}

/// Context required to build a [`Provenance`] record from a successful generation.
#[derive(Clone, Debug, PartialEq)]
pub struct GenerationCtx<'a> {
    pub provider_id: &'a str,
    pub provider_kind: &'a str,
    pub model: &'a str,
    pub license: &'a str,
    pub source: &'a str,
    pub capabilities: &'a [Capability],
    pub params: &'a Params,
    pub audio: &'a crate::Audio,
    pub now_unix: u64,
}

impl Provenance {
    pub fn from_generation(ctx: GenerationCtx<'_>) -> Self {
        Self {
            provider: ctx.provider_id.into(),
            provider_kind: ctx.provider_kind.into(),
            model: ctx.model.into(),
            prompt: ctx.params.prompt.clone(),
            negative_prompt: ctx.params.negative_prompt.clone(),
            duration: ctx.params.duration,
            bpm: ctx.params.bpm,
            key: ctx.params.key.clone(),
            instrumental: ctx.params.instrumental,
            seed: ctx.params.seed,
            capabilities: ctx.capabilities.to_vec(),
            sample_rate: ctx.audio.sample_rate,
            channels: ctx.audio.channels,
            samples: ctx.audio.samples.len(),
            seconds: ctx.audio.seconds(),
            license: ctx.license.into(),
            source: ctx.source.into(),
            generated_at_utc_unix: ctx.now_unix,
        }
    }

    pub fn to_json(&self) -> serde_json::Value {
        serde_json::to_value(self).unwrap_or(serde_json::Value::Null)
    }

    /// Sidecar path for a WAV `wav_path`.
    pub fn sidecar_path(wav_path: &str) -> String {
        format!("{wav_path}.music-provenance.json")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Audio;

    #[test]
    fn sidecar_appends_suffix() {
        assert_eq!(Provenance::sidecar_path("/a/b/Cue 1.wav"), "/a/b/Cue 1.wav.music-provenance.json");
    }

    #[test]
    fn provenance_roundtrips() {
        let p = Params { prompt: "warm pad".into(), duration: 30.0, instrumental: true, ..Default::default() };
        let a = Audio { samples: vec![0.0; 100], sample_rate: 44_100, channels: 2 };
        let pv = Provenance::from_generation(GenerationCtx {
            provider_id: "stable-audio-local",
            provider_kind: "local",
            model: "stable-audio-3-small",
            license: "Stability Community License",
            source: "https://huggingface.co/stabilityai/stable-audio-3-small",
            capabilities: &[Capability::Seed, Capability::Instrumental],
            params: &p,
            audio: &a,
            now_unix: 1_700_000_000,
        });
        let v = pv.to_json();
        assert_eq!(v["provider"], "stable-audio-local");
        assert_eq!(v["seed"], serde_json::Value::Null);
    }
}
