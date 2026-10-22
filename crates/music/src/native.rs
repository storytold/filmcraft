//! Local Stable Audio 3 inference through ONNX Runtime.
//!
//! The runtime library is loaded at run time (`ort`'s `load-dynamic`), from
//! `<model dir>/runtime/` or the path in `ORT_DYLIB_PATH`; nothing is linked, and the build stays
//! pure Rust. `ort` panics if its library cannot be loaded, so the library is loaded through the
//! fallible `init_from` first and every inference call also runs under `catch_unwind`.

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock, PoisonError};

use ort::environment::Environment;
use ort::ep;
use ort::session::Session;
use ort::value::Tensor;
use tokenizers::{PaddingParams, Tokenizer, TruncationParams};

use crate::models::Manifest;
use crate::sa3::{self, COND_DIM, COND_TOKENS, Conditioning, Graphs, LATENT_CHANNELS, Options};
use crate::{Audio, MusicError, Params, Provider, ProviderInfo, catalog};

const MAX_TOKENS: usize = 256;

fn inf(what: &str, e: impl std::fmt::Display) -> MusicError {
    MusicError::Inference(format!("{what}: {e}"))
}

fn runtime_library(model_dir: &Path) -> PathBuf {
    if let Some(p) = std::env::var_os("ORT_DYLIB_PATH").filter(|p| !p.is_empty()) {
        return PathBuf::from(p);
    }
    let name = format!("{}onnxruntime{}", std::env::consts::DLL_PREFIX, std::env::consts::DLL_SUFFIX);
    model_dir.join("runtime").join(name)
}

static RUNTIME: OnceLock<Result<Environment, String>> = OnceLock::new();

/// Load the runtime library and create the process wide environment, once.
fn init_runtime(lib: &Path) -> Result<&'static Environment, MusicError> {
    let r = RUNTIME.get_or_init(|| {
        if !lib.exists() {
            return Err(format!("the ONNX Runtime library is missing: {}", lib.display()));
        }
        ort::init_from(lib).map_err(|e| format!("loading ONNX Runtime: {e}"))?.build().map_err(|e| format!("starting ONNX Runtime: {e}"))
    });
    r.as_ref().map_err(|e| MusicError::Unavailable(e.clone()))
}

/// Which accelerator to ask for. `auto` prefers the platform GPU provider. ONNX Runtime uses its
/// CPU provider for unsupported graph nodes, and model loading retries wholly on the CPU if the
/// accelerator cannot initialize the graph.
fn execution_providers(device: &str) -> Vec<ort::ep::ExecutionProviderDispatch> {
    match device {
        "cpu" => vec![],
        "coreml" => vec![ep::CoreML::default().build()],
        "cuda" => vec![ep::CUDA::default().build()],
        _ if cfg!(target_os = "macos") => vec![ep::CoreML::default().build()],
        _ if cfg!(target_os = "windows") => vec![ep::CUDA::default().build(), ep::DirectML::default().build()],
        _ => vec![ep::CUDA::default().build()],
    }
}

struct OrtGraphs {
    conditioner: Mutex<Session>,
    dit: Mutex<Session>,
    decoder: Mutex<Session>,
    tokenizer: Tokenizer,
    pad_id: i64,
}

fn open(env: &Environment, path: &Path, device: &str) -> Result<Session, MusicError> {
    let mut b = Session::builder(env).map_err(|e| inf("session", e))?;
    let eps = execution_providers(device);
    if !eps.is_empty() {
        b = b.with_execution_providers(eps).map_err(|e| inf("execution providers", e))?;
    }
    b.commit_from_file(path).map_err(|e| inf(&format!("loading {}", path.display()), e))
}

impl OrtGraphs {
    fn load(env: &Environment, dir: &Path, device: &str) -> Result<OrtGraphs, MusicError> {
        let mut tokenizer = Tokenizer::from_file(dir.join("tokenizer/tokenizer.json")).map_err(|e| inf("tokenizer", e))?;
        let pad_id = tokenizer.token_to_id("<pad>").ok_or_else(|| MusicError::Config("the tokenizer has no <pad> token".into()))?;
        tokenizer.with_truncation(Some(TruncationParams { max_length: MAX_TOKENS, ..Default::default() })).map_err(|e| inf("tokenizer", e))?;
        tokenizer.with_padding(Some(PaddingParams { strategy: tokenizers::PaddingStrategy::Fixed(MAX_TOKENS), pad_id, ..Default::default() }));
        Ok(OrtGraphs {
            conditioner: Mutex::new(open(env, &dir.join("conditioner/conditioner.onnx"), device)?),
            dit: Mutex::new(open(env, &dir.join("dit/dit.onnx"), device)?),
            decoder: Mutex::new(open(env, &dir.join("decoder/decoder.onnx"), device)?),
            tokenizer,
            pad_id: i64::from(pad_id),
        })
    }
}

fn tensor<T: ort::value::PrimitiveTensorElementType + std::fmt::Debug + Clone + 'static>(shape: Vec<i64>, data: Vec<T>) -> Result<Tensor<T>, MusicError> {
    Tensor::from_array((shape, data)).map_err(|e| inf("building a tensor", e))
}

fn output(outputs: &ort::session::SessionOutputs<'_>, name: &str) -> Result<Vec<f32>, MusicError> {
    let v = outputs.get(name).ok_or_else(|| MusicError::Inference(format!("the graph has no output `{name}`")))?;
    let (_, data) = v.try_extract_tensor::<f32>().map_err(|e| inf("reading an output", e))?;
    Ok(data.to_vec())
}

impl Graphs for OrtGraphs {
    fn encode(&self, prompt: &str, seconds: f32) -> Result<Conditioning, MusicError> {
        let enc = self.tokenizer.encode(prompt, true).map_err(|e| inf("tokenizing", e))?;
        let mut ids: Vec<i64> = enc.get_ids().iter().map(|&i| i64::from(i)).collect();
        let mut mask: Vec<i64> = enc.get_attention_mask().iter().map(|&m| i64::from(m)).collect();
        ids.resize(MAX_TOKENS, self.pad_id);
        mask.resize(MAX_TOKENS, 0);
        let mut s = self.conditioner.lock().unwrap_or_else(PoisonError::into_inner);
        let out = catch_unwind(AssertUnwindSafe(|| {
            s.run(ort::inputs! { "ids" => tensor(vec![1, MAX_TOKENS as i64], ids)?, "mask" => tensor(vec![1, MAX_TOKENS as i64], mask)?, "secs" => tensor(vec![1], vec![seconds])? })
                .map_err(|e| inf("conditioner", e))
                .and_then(|o| Ok(Conditioning { cross: output(&o, "cross")?, global: output(&o, "glob")? }))
        }))
        .map_err(|_| MusicError::Inference("the conditioner graph crashed".into()))??;
        Ok(out)
    }

    fn denoise(&self, x: &[f32], frames: usize, t: f32, cond: &Conditioning, valid: usize) -> Result<Vec<f32>, MusicError> {
        let f = frames as i64;
        let pmask: Vec<bool> = (0..frames).map(|i| i < valid).collect();
        let local = vec![0.0f32; (COND_TOKENS * frames).max(1)];
        let mut s = self.dit.lock().unwrap_or_else(PoisonError::into_inner);
        catch_unwind(AssertUnwindSafe(|| {
            s.run(ort::inputs! {
                "x" => tensor(vec![1, LATENT_CHANNELS as i64, f], x.to_vec())?,
                "t" => tensor(vec![1], vec![t])?,
                "cross" => tensor(vec![1, COND_TOKENS as i64, COND_DIM as i64], cond.cross.clone())?,
                "glob" => tensor(vec![1, COND_DIM as i64], cond.global.clone())?,
                "local" => tensor(vec![1, COND_TOKENS as i64, f], local)?,
                "pmask" => tensor(vec![1, f], pmask)?,
            })
            .map_err(|e| inf("DiT", e))
            .and_then(|o| output(&o, "v"))
        }))
        .map_err(|_| MusicError::Inference("the DiT graph crashed".into()))?
    }

    fn decode(&self, z: &[f32], frames: usize) -> Result<Vec<f32>, MusicError> {
        let mut s = self.decoder.lock().unwrap_or_else(PoisonError::into_inner);
        catch_unwind(AssertUnwindSafe(|| {
            s.run(ort::inputs! { "z" => tensor(vec![1, LATENT_CHANNELS as i64, frames as i64], z.to_vec())? })
                .map_err(|e| inf("decoder", e))
                .and_then(|o| output(&o, "audio"))
        }))
        .map_err(|_| MusicError::Inference("the decoder graph crashed".into()))?
    }
}

/// The on-device Stable Audio 3 provider.
pub struct Local {
    graphs: OrtGraphs,
    max_samples: usize,
    info: ProviderInfo,
}

impl Local {
    pub fn load(dir: &Path, manifest: &Manifest, configured_device: Option<&str>) -> Result<Local, MusicError> {
        let model_dir = crate::models::model_dir(dir, &manifest.id);
        let env = init_runtime(&runtime_library(&model_dir))?;
        let device = std::env::var("FILMCRAFT_MUSIC_DEVICE")
            .ok()
            .filter(|d| !d.is_empty())
            .or_else(|| configured_device.map(str::to_string))
            .unwrap_or_else(|| "auto".into());
        let accelerated = catch_unwind(AssertUnwindSafe(|| OrtGraphs::load(env, &model_dir, &device)))
            .map_err(|_| MusicError::Inference("loading the model crashed".into()))?;
        let graphs = match accelerated {
            Ok(graphs) => graphs,
            Err(_) if device == "auto" => catch_unwind(AssertUnwindSafe(|| OrtGraphs::load(env, &model_dir, "cpu")))
                .map_err(|_| MusicError::Inference("loading the CPU fallback crashed".into()))??,
            Err(error) => return Err(error),
        };
        let mut info = catalog::stable_audio_local_info(Some(dir));
        info.installed = true;
        Ok(Local { graphs, max_samples: manifest.max_samples, info })
    }
}

impl Provider for Local {
    fn info(&self) -> ProviderInfo {
        self.info.clone()
    }

    fn generate(&self, params: &Params) -> Result<Audio, MusicError> {
        self.generate_with_progress(params, &mut |_| true)
    }

    fn generate_with_progress(&self, params: &Params, progress: &mut dyn FnMut(f32) -> bool) -> Result<Audio, MusicError> {
        params.check()?;
        let seed = params.seed.unwrap_or_else(|| std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos() as u64).unwrap_or(0));
        let opts = Options { steps: sa3::DEFAULT_STEPS, max_samples: self.max_samples };
        sa3::generate(&self.graphs, &params.compose_prompt(), params.duration, seed, opts, progress)
    }
}

#[cfg(all(test, target_os = "macos"))]
mod device_tests {
    use super::*;

    #[test]
    fn auto_prefers_core_ml_and_cpu_remains_available() {
        assert_eq!(execution_providers("auto").len(), 1);
        assert!(execution_providers("cpu").is_empty());
        assert_eq!(execution_providers("coreml").len(), 1);
    }
}
