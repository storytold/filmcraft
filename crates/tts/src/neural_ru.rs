//! Kokoro-RU studio neural voices for Russian (feature `kokoro`).
//! Pure-Rust inference on candle (CPU), 24 kHz.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock, PoisonError};

use filmcraft_audio_dsp::AudioEffect;
use filmcraft_tts_text::{Chunk, Lang, Lexicon, Overrides};

use crate::catalog;
use crate::kokoro::{KOKORO_RATE, Kokoro, VoicePack};
use crate::{check_text, Audio, Params, TtsError, Voice, VoiceInfo, MAX_SECONDS};

const SENTENCE_GAP_MS: u32 = 120;

struct LoadedRu {
    dir: PathBuf,
    model: Arc<Kokoro>,
    packs: HashMap<&'static str, Arc<VoicePack>>,
}

fn cache() -> &'static Mutex<Option<LoadedRu>> {
    static C: OnceLock<Mutex<Option<LoadedRu>>> = OnceLock::new();
    C.get_or_init(Default::default)
}

/// A Kokoro Russian studio voice ready to speak.
pub struct NeuralRuVoice {
    info: VoiceInfo,
    model: Arc<Kokoro>,
    pack: Arc<VoicePack>,
}

/// Load (or reuse) the `kokoro-ru` package in `models_dir` and the voice `id`.
pub fn load(id: &str, models_dir: &Path) -> Result<NeuralRuVoice, TtsError> {
    let v = catalog::find(id).ok_or_else(|| TtsError::UnknownVoice(id.to_string()))?;
    if !catalog::kokoro_ru_installed(models_dir) {
        return Err(TtsError::NotInstalled);
    }
    let dir = catalog::kokoro_ru_package_dir(models_dir);
    let mut guard = cache().lock().unwrap_or_else(PoisonError::into_inner);
    if guard.as_ref().is_none_or(|l| l.dir != dir) {
        let model = Arc::new(Kokoro::load_file(&dir, "kokoro-ru-v2-base.pth")?);
        *guard = Some(LoadedRu { dir: dir.clone(), model, packs: HashMap::new() });
    }
    let loaded = guard.as_mut().ok_or_else(|| TtsError::Model("kokoro-ru voice package not loaded".into()))?;
    let pack = match loaded.packs.get(v.file) {
        Some(p) => p.clone(),
        None => {
            let p = Arc::new(VoicePack::load(&dir.join(v.file))?);
            loaded.packs.insert(v.file, p.clone());
            p
        }
    };
    Ok(NeuralRuVoice { info: VoiceInfo { installed: true, ..v.info }, model: loaded.model.clone(), pack })
}

impl Voice for NeuralRuVoice {
    fn info(&self) -> VoiceInfo {
        self.info
    }

    fn synthesize(&self, text: &str, params: &Params) -> Result<Audio, TtsError> {
        check_text(text)?;
        params.check()?;
        let chunks = filmcraft_tts_text::phonemize(text, Lang::RuRu, &Lexicon::empty(), &Overrides::default())
            .map_err(|e| TtsError::Invalid(e.to_string()))?;
        let rate = KOKORO_RATE;
        let max = (MAX_SECONDS * f64::from(rate)) as usize;
        let mut out: Vec<f32> = Vec::new();
        let mut spoke = false;
        for (i, c) in chunks.iter().enumerate() {
            match c {
                Chunk::Pause { millis } => {
                    let n = (u64::from(*millis) * u64::from(rate) / 1000) as usize;
                    out.resize(out.len().saturating_add(n), 0.0);
                }
                Chunk::Phonemes(p) => {
                    let kokoro_phonemes = russian_to_kokoro_symbols(p);
                    if self.model.ids(&kokoro_phonemes).is_empty() {
                        continue;
                    }
                    if spoke {
                        out.resize(out.len().saturating_add((SENTENCE_GAP_MS * rate / 1000) as usize), 0.0);
                    }
                    let s = self.model.speak(&kokoro_phonemes, &self.pack, params.pace, i as u64 + 1)?;
                    out.extend_from_slice(&s);
                    spoke = true;
                }
            }
            if out.len() > max {
                return Err(TtsError::AudioTooLong);
            }
        }
        if !spoke {
            return Err(TtsError::Empty);
        }
        if params.semitones.abs() > 1e-6 {
            out = pitch_shift(out, params.semitones, rate);
        }
        let peak = out.iter().fold(0f32, |m, x| m.max(x.abs()));
        if peak > 0.98 {
            let g = 0.98 / peak;
            out.iter_mut().for_each(|x| *x *= g);
        }
        Ok(Audio { samples: out, sample_rate: rate })
    }
}

/// Convert Russian Piper IPA phonemes to Kokoro vocab symbols.
fn russian_to_kokoro_symbols(s: &str) -> String {
    s.replace("tɕ", "ʧ")
        .replace("ts", "ʦ")
        .replace("ɕː", "ɕ")
        .replace('ʐ', "ʒ")
        .replace('ʂ', "ʃ")
}

fn pitch_shift(mut x: Vec<f32>, semitones: f64, rate: u32) -> Vec<f32> {
    let mut ps = filmcraft_audio_dsp::effects::PitchShifter::new(rate as f32, 1);
    ps.set_param("semitones", semitones as f32);
    ps.reset();
    let lat = ps.latency();
    let n = x.len();
    x.resize(n + lat, 0.0);
    for block in x.chunks_mut(4096) {
        ps.process(&mut [block]);
    }
    x.drain(..lat.min(x.len()));
    x.truncate(n);
    x
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_russian_to_kokoro_symbols() {
        assert_eq!(russian_to_kokoro_symbols("vaʂɨvo"), "vaʃɨvo");
        assert_eq!(russian_to_kokoro_symbols("ʐɨznʲ"), "ʒɨznʲ");
        assert_eq!(russian_to_kokoro_symbols("tɕaj"), "ʧaj");
        assert_eq!(russian_to_kokoro_symbols("tsarʲ"), "ʦarʲ");
        assert_eq!(russian_to_kokoro_symbols("ɕːi"), "ɕi");
    }
}
