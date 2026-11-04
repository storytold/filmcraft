//! Kokoro-82M neural voices (feature `kokoro`): pure-Rust inference on candle (CPU).
//!
//! Implemented from the papers the model card cites (StyleTTS 2, iSTFTNet; with ALBERT, HiFi-GAN,
//! the neural source-filter model and the Snake activation they build on), the released
//! `config.json` and the tensor names and shapes in the checkpoint. No reference code was read.
//! Weights (Apache-2.0, hexgrad) are downloaded on first use and never bundled.
//!
//! [`Kokoro::speak`] takes Kokoro phonemes (from `filmcraft-tts-text`), at most
//! [`MAX_PHONEMES`] per call, and a voice pack.

mod dsp;
mod layers;
mod model;
mod weights;

use std::path::Path;

use crate::TtsError;

pub use model::SAMPLE_RATE as KOKORO_RATE;

/// Longest phoneme string one forward pass takes (the position table has 512 rows, two of them
/// for the boundary tokens).
pub const MAX_PHONEMES: usize = 510;

/// A loaded Kokoro model.
pub struct Kokoro {
    model: model::Model,
    vocab: std::collections::HashMap<char, u32>,
}

/// A loaded voice pack.
pub struct VoicePack(Vec<f32>);

impl VoicePack {
    pub fn load(path: &Path) -> Result<VoicePack, TtsError> {
        weights::read_voice(path).map(VoicePack)
    }
}

impl Kokoro {
    /// Load `config.json` and `kokoro-v1_0.pth` from `dir`.
    pub fn load(dir: &Path) -> Result<Kokoro, TtsError> {
        let cfg = weights::read_config(&dir.join("config.json"))?;
        let w = weights::Weights::load(&dir.join("kokoro-v1_0.pth"))?;
        Ok(Kokoro { model: model::Model::load(&w)?, vocab: cfg.vocab })
    }

    /// Phoneme ids of `phonemes`; symbols outside the vocabulary are dropped.
    pub fn ids(&self, phonemes: &str) -> Vec<u32> {
        phonemes.chars().filter_map(|c| self.vocab.get(&c).copied()).collect()
    }

    /// Speak `phonemes` with `voice` at `speed` (0.5–2). Deterministic for a given `seed`.
    pub fn speak(&self, phonemes: &str, voice: &VoicePack, speed: f64, seed: u64) -> Result<Vec<f32>, TtsError> {
        if !(0.5..=2.0).contains(&speed) {
            return Err(TtsError::Invalid("speed must be between 0.5 and 2".into()));
        }
        let ids = self.ids(phonemes);
        if ids.is_empty() {
            return Err(TtsError::Empty);
        }
        if ids.len() > MAX_PHONEMES {
            return Err(TtsError::Invalid(format!("at most {MAX_PHONEMES} phonemes per pass")));
        }
        let style = weights::style_for(&voice.0, ids.len(), self.model.device())?;
        let out = self.model.forward(&ids, &style, speed, seed)?;
        if out.iter().any(|s| !s.is_finite()) {
            return Err(TtsError::Model("the model produced invalid samples".into()));
        }
        Ok(out)
    }
}

/// Tests with the real model run when `FILMCRAFT_KOKORO_DIR` names a directory holding
/// `config.json`, `kokoro-v1_0.pth` and `af_heart.pt` (the downloaded package); they skip otherwise.
#[cfg(test)]
mod tests {
    use super::*;

    fn model() -> Option<(Kokoro, VoicePack)> {
        let dir = std::path::PathBuf::from(std::env::var_os("FILMCRAFT_KOKORO_DIR")?);
        let voice = [dir.join("af_heart.pt"), dir.join("voices/af_heart.pt")].into_iter().find(|p| p.exists())?;
        Some((Kokoro::load(&dir).unwrap(), VoicePack::load(&voice).unwrap()))
    }

    #[test]
    fn speaks_deterministically_and_pace_changes_the_length() {
        let Some((k, v)) = model() else { return };
        let ph = "həlˈO wˈɜɹld.";
        let a = k.speak(ph, &v, 1.0, 1).unwrap();
        assert_eq!(a, k.speak(ph, &v, 1.0, 1).unwrap(), "deterministic");
        assert_eq!(a.len() % 600, 0, "whole frames");
        let secs = a.len() as f64 / f64::from(KOKORO_RATE);
        assert!((0.5..3.0).contains(&secs), "{secs}");
        let peak = a.iter().fold(0f32, |m, x| m.max(x.abs()));
        assert!(peak > 0.05 && peak < 1.0, "{peak}");
        let fast = k.speak(ph, &v, 2.0, 1).unwrap();
        let ratio = fast.len() as f64 / a.len() as f64;
        assert!((0.35..0.7).contains(&ratio), "{ratio}");
    }

    #[test]
    fn refuses_bad_input() {
        let Some((k, v)) = model() else { return };
        assert_eq!(k.speak("", &v, 1.0, 1).err(), Some(TtsError::Empty));
        assert_eq!(k.speak("日本語👍", &v, 1.0, 1).err(), Some(TtsError::Empty), "nothing in the vocabulary");
        assert!(k.speak("a", &v, 3.0, 1).is_err());
        assert!(k.speak("a", &v, f64::NAN, 1).is_err());
        assert!(k.speak(&"ə".repeat(MAX_PHONEMES + 1), &v, 1.0, 1).is_err());
        assert_eq!(k.ids("a日b"), k.ids("ab"));
    }
}
