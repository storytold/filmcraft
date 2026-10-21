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

impl Provenance {
    pub fn from_generation(
        provider_id: &str,
        provider_kind: &str,
        model: &str,
        license: &str,
        source: &str,
        capabilities: &[Capability],
        params: &Params,
        audio: &crate::Audio,
        now_unix: u64,
    ) -> Self {
        Self {
            provider: provider_id.into(),
            provider_kind: provider_kind.into(),
            model: model.into(),
            prompt: params.prompt.clone(),
            negative_prompt: params.negative_prompt.clone(),
            duration: params.duration,
            bpm: params.bpm,
            key: params.key.clone(),
            instrumental: params.instrumental,
            seed: params.seed,
            capabilities: capabilities.to_vec(),
            sample_rate: audio.sample_rate,
            channels: audio.channels,
            samples: audio.samples.len(),
            seconds: audio.seconds(),
            license: license.into(),
            source: source.into(),
            generated_at_utc_unix: now_unix,
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
        let pv = Provenance::from_generation(
            "stable-audio-local",
            "local",
            "stable-audio-3-small",
            "Stability Community License",
            "https://huggingface.co/stabilityai/stable-audio-3-small",
            &[Capability::Seed, Capability::Instrumental],
            &p,
            &a,
            1_700_000_000,
        );
        let v = pv.to_json();
        assert_eq!(v["provider"], "stable-audio-local");
        assert_eq!(v["seed"], serde_json::Value::Null);
    }
}
