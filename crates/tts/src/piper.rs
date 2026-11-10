//! Piper neural voices (feature `piper`): pure-Rust ONNX inference via tract-onnx.
//!
//! Models are VITS architecture exported as ONNX with a companion `.onnx.json` config.
//! Weights and configs are MIT-licensed (rhasspy/piper-voices) and downloaded on demand.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock, PoisonError};

use prost::Message;
use serde::Deserialize;
use tract_onnx::prelude::*;

use filmcraft_audio_dsp::AudioEffect;
use filmcraft_tts_text::{Chunk, Lang, Lexicon, Overrides};

use crate::catalog;
use crate::{Audio, MAX_SECONDS, Params, TtsError, Voice, VoiceInfo, check_text};

const SENTENCE_GAP_MS: u32 = 120;
const BUCKET_SIZES: [usize; 11] = [32, 64, 96, 128, 192, 256, 384, 512, 640, 768, 832];

#[derive(Clone, Debug, Deserialize)]
pub struct PiperAudioConfig {
    pub sample_rate: u32,
}

#[derive(Clone, Debug, Deserialize)]
pub struct PiperInferenceConfig {
    #[serde(default = "default_noise_scale")]
    pub noise_scale: f32,
    #[serde(default = "default_length_scale")]
    pub length_scale: f32,
    #[serde(default = "default_noise_w")]
    pub noise_w: f32,
}

fn default_noise_scale() -> f32 {
    0.667
}

fn default_length_scale() -> f32 {
    1.0
}

fn default_noise_w() -> f32 {
    0.8
}

impl Default for PiperInferenceConfig {
    fn default() -> Self {
        Self { noise_scale: default_noise_scale(), length_scale: default_length_scale(), noise_w: default_noise_w() }
    }
}

#[derive(Clone, Debug, Deserialize)]
pub struct PiperConfig {
    pub audio: PiperAudioConfig,
    #[serde(default)]
    pub inference: PiperInferenceConfig,
    pub phoneme_id_map: HashMap<String, Vec<i64>>,
}

impl PiperConfig {
    pub fn load(path: &Path) -> Result<Self, TtsError> {
        let bytes = std::fs::read(path).map_err(|e| TtsError::Model(format!("{}: {e}", path.display())))?;
        serde_json::from_slice(&bytes).map_err(|e| TtsError::Model(format!("failed to parse piper config: {e}")))
    }

    pub fn text_to_ids(&self, phonemes: &str) -> Vec<i64> {
        let pad = self.phoneme_id_map.get("_").and_then(|v| v.first().copied()).unwrap_or(0);
        let bos = self.phoneme_id_map.get("^").and_then(|v| v.first().copied()).unwrap_or(1);
        let eos = self.phoneme_id_map.get("$").and_then(|v| v.first().copied()).unwrap_or(2);

        let mut ids = vec![bos, pad];
        for ch in phonemes.chars() {
            let mut s = String::new();
            s.push(ch);
            if let Some(pids) = self.phoneme_id_map.get(&s) {
                for pid in pids {
                    ids.push(*pid);
                    ids.push(pad);
                }
            }
        }
        ids.push(eos);
        ids
    }
}

pub struct PiperModel {
    pub config: PiperConfig,
    raw_proto: tract_onnx::pb::ModelProto,
    buckets: Mutex<HashMap<usize, Arc<TypedRunnableModel>>>,
}

impl PiperModel {
    pub fn load(onnx_path: &Path, json_path: &Path) -> Result<Self, TtsError> {
        let config = PiperConfig::load(json_path)?;
        let bytes = std::fs::read(onnx_path).map_err(|e| TtsError::Model(format!("{}: {e}", onnx_path.display())))?;
        let mut model_proto = tract_onnx::pb::ModelProto::decode(bytes.as_slice()).map_err(|e| TtsError::Model(format!("failed to decode onnx proto: {e}")))?;

        // Ensure /ReduceMax_output_0 is exposed as second graph output so inference knows
        // the exact valid frame count.
        let graph = model_proto.graph.as_mut().ok_or_else(|| TtsError::Model("onnx graph is missing".into()))?;
        let has_reducemax = graph.output.iter().any(|o| o.name == "/ReduceMax_output_0");
        if !has_reducemax {
            graph.output.push(tract_onnx::pb::ValueInfoProto {
                name: "/ReduceMax_output_0".to_string(),
                r#type: Some(tract_onnx::pb::TypeProto {
                    value: Some(tract_onnx::pb::type_proto::Value::TensorType(tract_onnx::pb::type_proto::Tensor {
                        elem_type: tract_onnx::pb::tensor_proto::DataType::Int64 as i32,
                        shape: Some(tract_onnx::pb::TensorShapeProto { dim: vec![] }),
                    })),
                    ..Default::default()
                }),
                ..Default::default()
            });
        }

        Ok(Self { config, raw_proto: model_proto, buckets: Mutex::new(HashMap::new()) })
    }

    fn compile_bucket(&self, bucket_len: usize) -> Result<Arc<TypedRunnableModel>, TtsError> {
        let mut proto = self.raw_proto.clone();
        let graph = proto.graph.as_mut().ok_or_else(|| TtsError::Model("onnx graph missing".into()))?;

        // Static maximum frame ceiling for this bucket:
        let k_val = (bucket_len as i64).saturating_mul(16).max(128);
        let k_tensor = tract_onnx::pb::TensorProto {
            dims: vec![],
            data_type: tract_onnx::pb::tensor_proto::DataType::Int64 as i32,
            int64_data: vec![k_val],
            name: "fixed_k_limit".to_string(),
            ..Default::default()
        };
        let k_node = tract_onnx::pb::NodeProto {
            op_type: "Constant".to_string(),
            name: "fixed_k_node".to_string(),
            output: vec!["fixed_k_limit".to_string()],
            attribute: vec![tract_onnx::pb::AttributeProto {
                name: "value".to_string(),
                r#type: tract_onnx::pb::attribute_proto::AttributeType::Tensor as i32,
                t: Some(k_tensor),
                ..Default::default()
            }],
            ..Default::default()
        };
        graph.node.push(k_node);

        for node in &mut graph.node {
            if node.name == "/Range" && node.op_type == "Range" && node.input.len() >= 2 {
                node.input[1] = "fixed_k_limit".to_string();
            }
        }

        let mut bytes = Vec::new();
        proto.encode(&mut bytes).map_err(|e| TtsError::Model(format!("failed to encode patched onnx proto: {e}")))?;

        let mut raw = tract_onnx::onnx()
            .with_ignore_output_shapes(true)
            .with_ignore_value_info(true)
            .model_for_read(&mut bytes.as_slice())
            .map_err(|e| TtsError::Model(format!("tract onnx model load error: {e}")))?;

        let batch_size = raw.symbols.sym("batch_size");
        let mut subs = HashMap::new();
        subs.insert(batch_size, 1.to_dim());

        let phonemes = raw.symbols.sym("phonemes");
        subs.insert(phonemes, (bucket_len as i64).to_dim());
        raw.set_symbols(&subs).map_err(|e| TtsError::Model(format!("tract symbol substitution error: {e}")))?;

        let runnable = raw
            .into_optimized()
            .map_err(|e| TtsError::Model(format!("tract optimize error: {e}")))?
            .into_runnable()
            .map_err(|e| TtsError::Model(format!("tract into_runnable error: {e}")))?;

        Ok(runnable)
    }

    fn get_or_compile_bucket(&self, bucket_len: usize) -> Result<Arc<TypedRunnableModel>, TtsError> {
        let mut guard = self.buckets.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(r) = guard.get(&bucket_len) {
            return Ok(r.clone());
        }
        let runnable = self.compile_bucket(bucket_len)?;
        guard.insert(bucket_len, runnable.clone());
        Ok(runnable)
    }

    pub fn speak(&self, ids: &[i64], speed: f64) -> Result<Vec<f32>, TtsError> {
        let n = ids.len();
        if n == 0 {
            return Err(TtsError::Empty);
        }

        // Pick discrete bucket size to maximize cache reuse
        let bucket_len = BUCKET_SIZES.iter().copied().find(|&b| b >= n).unwrap_or_else(|| n.checked_add(31).map(|v| (v / 32) * 32).unwrap_or(n));

        let runnable = self.get_or_compile_bucket(bucket_len)?;

        let mut padded = ids.to_vec();
        padded.resize(bucket_len, 0);

        let input: Tensor =
            tract_ndarray::Array2::from_shape_vec((1, bucket_len), padded).map_err(|e| TtsError::Model(format!("input shape error: {e}")))?.into();
        let input_lengths: Tensor = tract_ndarray::Array1::from_vec(vec![n as i64]).into();
        let length_scale = (self.config.inference.length_scale as f64 / speed.clamp(0.5, 2.0)) as f32;
        let scales_t: Tensor = tract_ndarray::Array1::from_vec(vec![self.config.inference.noise_scale, length_scale, self.config.inference.noise_w]).into();

        let outputs = runnable.run(tvec!(input.into(), input_lengths.into(), scales_t.into())).map_err(|e| TtsError::Model(format!("tract run error: {e}")))?;

        let audio_view = outputs[0].to_plain_array_view::<f32>().map_err(|e| TtsError::Model(format!("tract output read error: {e}")))?;
        let slice = audio_view.as_slice().ok_or_else(|| TtsError::Model("audio output not contiguous".into()))?;

        // Determine exact valid frame count from ReduceMax output
        let valid_samples = if outputs.len() > 1 {
            let frames = outputs[1].cast_to_scalar::<i64>().unwrap_or(0);
            if frames > 0 { (frames as usize).saturating_mul(256).min(slice.len()) } else { slice.len() }
        } else {
            slice.len()
        };

        Ok(slice[..valid_samples].to_vec())
    }
}

struct LoadedCache {
    dir: PathBuf,
    models: HashMap<String, Arc<PiperModel>>,
}

fn cache() -> &'static Mutex<Option<LoadedCache>> {
    static C: OnceLock<Mutex<Option<LoadedCache>>> = OnceLock::new();
    C.get_or_init(Default::default)
}

pub struct PiperVoice {
    info: VoiceInfo,
    model: Arc<PiperModel>,
}

pub fn load(id: &str, models_dir: &Path) -> Result<PiperVoice, TtsError> {
    let v = catalog::find(id).ok_or_else(|| TtsError::UnknownVoice(id.to_string()))?;
    if !catalog::piper_ru_installed(models_dir) {
        return Err(TtsError::NotInstalled);
    }
    let dir = catalog::piper_ru_package_dir(models_dir);
    let mut guard = cache().lock().unwrap_or_else(PoisonError::into_inner);
    if guard.as_ref().is_none_or(|l| l.dir != dir) {
        *guard = Some(LoadedCache { dir: dir.clone(), models: HashMap::new() });
    }
    let loaded = guard.as_mut().ok_or_else(|| TtsError::Model("piper cache error".into()))?;
    let model = match loaded.models.get(v.file) {
        Some(m) => m.clone(),
        None => {
            let onnx_path = dir.join(v.file);
            let json_file = format!("{}.json", v.file);
            let json_path = dir.join(&json_file);
            let m = Arc::new(PiperModel::load(&onnx_path, &json_path)?);
            loaded.models.insert(v.file.to_string(), m.clone());
            m
        }
    };
    Ok(PiperVoice { info: VoiceInfo { installed: true, ..v.info }, model })
}

impl Voice for PiperVoice {
    fn info(&self) -> VoiceInfo {
        self.info
    }

    fn synthesize(&self, text: &str, params: &Params) -> Result<Audio, TtsError> {
        check_text(text)?;
        params.check()?;
        let chunks = filmcraft_tts_text::phonemize(text, Lang::RuRu, &Lexicon::empty(), &Overrides::default()).map_err(|e| TtsError::Invalid(e.to_string()))?;
        let rate = self.model.config.audio.sample_rate;
        let max = (MAX_SECONDS * f64::from(rate)) as usize;
        let mut out: Vec<f32> = Vec::new();
        let mut spoke = false;
        for c in &chunks {
            match c {
                Chunk::Pause { millis } => {
                    let n = (u64::from(*millis) * u64::from(rate) / 1000) as usize;
                    out.resize(out.len().saturating_add(n), 0.0);
                }
                Chunk::Phonemes(p) => {
                    let ids = self.model.config.text_to_ids(p);
                    if ids.len() <= 3 {
                        continue;
                    }
                    if spoke {
                        out.resize(out.len().saturating_add((SENTENCE_GAP_MS * rate / 1000) as usize), 0.0);
                    }
                    let s = self.model.speak(&ids, params.pace)?;
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
    fn test_piper_synthesize_russian() {
        let home = match std::env::var("HOME") {
            Ok(h) => h,
            Err(_) => return,
        };
        let path = std::path::PathBuf::from(home).join(".local/share/filmcraft/models/piper-ru/ru_RU-dmitri-medium.onnx");
        if !path.exists() {
            return;
        }
        let json_path = path.with_extension("onnx.json");
        let model = PiperModel::load(&path, &json_path).expect("failed to load piper model");

        let voice_desc = catalog::find("piper-dmitri").expect("piper-dmitri not in catalog");
        let voice = PiperVoice { info: VoiceInfo { installed: true, ..voice_desc.info }, model: Arc::new(model) };

        let params = Params::default();
        let audio = voice.synthesize("Привет! Это проверка синтеза речи.", &params).expect("synthesize failed");
        assert!(!audio.samples.is_empty());
        assert_eq!(audio.sample_rate, 22050);
        let peak = audio.samples.iter().fold(0f32, |m, x| m.max(x.abs()));
        assert!(peak > 0.05);
    }
}
