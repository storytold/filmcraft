//! Verified local model package. No downloader, Python embedding, server or foreign ML library.
use std::io::Read;
use std::path::Path;
use std::sync::Arc;

use rten::{Model, ModelOptions, RunOptions, ThreadPool, Value, ValueView};
use rten_tensor::prelude::*;
use serde::Deserialize;
use sha2::{Digest, Sha256};

const MAX_MODEL: u64 = 256 * 1024 * 1024;
const EDGE_SHA: &str = "ed2d4850b8792c239689b043c47046ec239b6e808a3d9b6ae676c803fd8780df";
const MATTE_SHA: &str = "7bbe217ec9d6945679b6191ccd071006a3975675326312df1ecb5518488049a8";
const NAMES: [&str; 6] = ["image", "prompt_one", "prompt_many", "track", "memory", "matte"];
// Reproducible offline exports of the pinned, Apache-2.0 checkpoints. Trust neither
// graph bytes nor a replacement manifest supplied beside them.
const PINS: [(u64, &str); 6] = [
    (19_801_552, "d2e3999e2f9671cac127f9738b8ae65041c7a00ca619bb1dde861a42fe7913f8"),
    (17_788_006, "80de9312dffe918de0509aabcbc558f160176bb10ce6c318164bd163f370de43"),
    (18_001_256, "a8252972a4d5d5b1eea375f8ac4d4f5a2c2bda94afeacb839f14e961aebc75f0"),
    (38_428_922, "8e4dc9c092874142f9af9844c79e74e1e0ef3361b8d13649986f066437a9bc7b"),
    (6_787_157, "241d645eed150df3f01c75495d28313b035c6b7f5b45f2e4f00ce8c85f642ac3"),
    (104_286_749, "8efc28a9a5dcca47c6f7b8bf1c4330889f19893aa589645823cabd776dca9eb5"),
];

/// Read at most the allowed model bytes plus one sentinel byte; reject oversized files.
fn read_bounded(path: &Path, limit: u64) -> Result<Vec<u8>, String> {
    let file = std::fs::File::open(path).map_err(|error| format!("{}: {error}", path.display()))?;
    let mut data = Vec::new();
    file.take(limit.saturating_add(1)).read_to_end(&mut data).map_err(|error| error.to_string())?;
    if data.len() as u64 > limit {
        return Err("Native model file exceeds its size budget".into());
    }
    Ok(data)
}

#[derive(Deserialize)]
struct Manifest {
    version: u32,
    edge_sha256: String,
    matte_sha256: String,
    opset: u32,
    files: Vec<Entry>,
}

#[derive(Deserialize)]
struct Entry {
    name: String,
    file: String,
    bytes: u64,
    sha256: String,
}

#[derive(Clone, Debug)]
pub struct Floats {
    pub shape: Vec<usize>,
    pub data: Vec<f32>,
}

impl Floats {
    /// Borrow a tensor view, rejecting a shape that does not fit the stored values.
    pub fn view(&self) -> Result<ValueView<'_>, String> {
        ValueView::from_shape(&self.shape, &self.data).map_err(|e| e.to_string())
    }

    /// Require the exact expected shape, element count and finite model outputs.
    pub fn checked(self, shape: &[usize]) -> Result<Self, String> {
        let length = shape.iter().try_fold(1usize, |n, dimension| n.checked_mul(*dimension));
        if self.shape != shape || length != Some(self.data.len()) || !self.data.iter().all(|x| x.is_finite()) {
            return Err(format!("Model returned invalid output shape {:?}; expected {shape:?}", self.shape));
        }
        Ok(self)
    }
}

pub struct Graph {
    model: Model,
    pool: Arc<ThreadPool>,
}

impl Graph {
    /// Resolve named graph inputs/outputs and execute on the selected bounded thread pool.
    pub fn run(&self, inputs: Vec<(&str, ValueView<'_>)>, outputs: &[&str]) -> Result<Vec<Floats>, String> {
        let inputs = inputs
            .into_iter()
            .map(|(name, value)| self.model.node_id(name).map(|id| (id, value.into())).map_err(|e| e.to_string()))
            .collect::<Result<Vec<_>, _>>()?;
        let outputs = outputs.iter().map(|name| self.model.node_id(name).map_err(|e| e.to_string())).collect::<Result<Vec<_>, _>>()?;
        self.model
            .run(inputs, &outputs, Some(RunOptions::default().with_thread_pool(Some(self.pool.clone()))))
            .map_err(|e| e.to_string())?
            .into_iter()
            .map(|value| {
                if let Value::FloatTensor(tensor) = value {
                    Ok(Floats { shape: tensor.shape().to_vec(), data: tensor.into_data() })
                } else {
                    Err("Model returned a non-float output".into())
                }
            })
            .collect()
    }
}

pub struct Models {
    pub image: Graph,
    pub prompt_one: Graph,
    pub prompt_many: Graph,
    pub track: Graph,
    pub memory: Graph,
    pub matte: Graph,
    preview_pool: Arc<ThreadPool>,
    sequence_pool: Arc<ThreadPool>,
    sequence_workers: usize,
}

/// Derive capped preview and sequence thread budgets, validating any explicit override.
fn thread_budgets(available: usize, requested: Option<&str>) -> Result<(usize, usize), String> {
    let available = available.clamp(1, 64);
    match requested {
        Some(value) => value
            .parse::<usize>()
            .ok()
            .filter(|n| (1..=available).contains(n))
            .map(|n| (n, n))
            .ok_or_else(|| format!("FILMCRAFT_AI_ROTO_THREADS must be 1..{available}")),
        None => Ok((available.min(4), available.min(8))),
    }
}

/// Bound parallel matte tiles by both the sequence thread budget and the four-tile limit.
fn tile_worker_budget(threads: usize, requested: Option<&str>) -> Result<usize, String> {
    let limit = threads.clamp(1, 4);
    match requested {
        Some(value) => {
            value.parse::<usize>().ok().filter(|n| (1..=limit).contains(n)).ok_or_else(|| format!("FILMCRAFT_AI_ROTO_TILE_WORKERS must be 1..{limit}"))
        }
        None => Ok(limit),
    }
}

impl Models {
    /// Select the preview or sequence pool for all graphs and return the tile-worker budget.
    pub(crate) fn sequence_mode(&mut self, sequence: bool) -> usize {
        let pool = if sequence { &self.sequence_pool } else { &self.preview_pool };
        for graph in [&mut self.image, &mut self.prompt_one, &mut self.prompt_many, &mut self.track, &mut self.memory, &mut self.matte] {
            graph.pool = pool.clone();
        }
        if sequence { self.sequence_workers } else { 1 }
    }

    /// Verify the manifest and independent graph pins before loading the six neural graphs.
    pub fn load(directory: &Path) -> Result<Self, String> {
        let manifest_path = directory.join("manifest.json");
        if std::fs::metadata(&manifest_path).map_err(|_| "Prepare native models with experiments/rotoscope/run-editor.sh")?.len() > 16 * 1024 {
            return Err("Native model manifest exceeds 16 KiB".into());
        }
        let manifest: Manifest = serde_json::from_slice(&read_bounded(&manifest_path, 16 * 1024)?).map_err(|e| e.to_string())?;
        if manifest.version != 1 || manifest.opset != 23 || manifest.edge_sha256 != EDGE_SHA || manifest.matte_sha256 != MATTE_SHA || manifest.files.len() != 6
        {
            return Err("Native model package does not match the pinned model revisions".into());
        }
        let mut graphs = Vec::new();
        // Interactive previews keep a small budget. Explicit multi-frame jobs can
        // use more cores; all graphs within a job still share one bounded pool.
        let available = std::thread::available_parallelism().map(usize::from).unwrap_or(1).min(64);
        let requested = match std::env::var("FILMCRAFT_AI_ROTO_THREADS") {
            Ok(value) => Some(value),
            Err(std::env::VarError::NotPresent) => None,
            Err(error) => return Err(format!("Invalid inference thread setting: {error}")),
        };
        let (preview_threads, sequence_threads) = thread_budgets(available, requested.as_deref())?;
        let workers = match std::env::var("FILMCRAFT_AI_ROTO_TILE_WORKERS") {
            Ok(value) => Some(value),
            Err(std::env::VarError::NotPresent) => None,
            Err(error) => return Err(format!("Invalid matte worker setting: {error}")),
        };
        let sequence_workers = tile_worker_budget(sequence_threads, workers.as_deref())?;
        let pool = Arc::new(ThreadPool::with_num_threads(preview_threads));
        let sequence_pool = if preview_threads == sequence_threads { pool.clone() } else { Arc::new(ThreadPool::with_num_threads(sequence_threads)) };
        for (name, (size, checksum)) in NAMES.into_iter().zip(PINS) {
            let matches: Vec<_> = manifest.files.iter().filter(|f| f.name == name).collect();
            let [entry] = matches.as_slice() else {
                return Err(format!("Missing or duplicate native {name} graph"));
            };
            if entry.file != format!("{name}.onnx") || entry.bytes != size || entry.bytes > MAX_MODEL || entry.sha256 != checksum {
                return Err(format!("Invalid native {name} graph filename or size"));
            }
            let path = directory.join(&entry.file);
            if std::fs::metadata(&path).map_err(|e| e.to_string())?.len() != entry.bytes {
                return Err(format!("Native {name} graph size differs"));
            }
            let bytes = read_bounded(&path, size)?;
            if bytes.len() as u64 != size || format!("{:x}", Sha256::digest(&bytes)) != checksum {
                return Err(format!("Native {name} graph checksum differs; rerun model setup"));
            }
            let model = ModelOptions::default().prepack_weights(true).load(bytes).map_err(|e| format!("Native {name} graph: {e}"))?;
            graphs.push(Graph { model, pool: pool.clone() });
        }
        let mut graphs = graphs.into_iter();
        Ok(Self {
            image: graphs.next().ok_or("missing image model")?,
            prompt_one: graphs.next().ok_or("missing prompt model")?,
            prompt_many: graphs.next().ok_or("missing multi-point model")?,
            track: graphs.next().ok_or("missing tracking model")?,
            memory: graphs.next().ok_or("missing memory model")?,
            matte: graphs.next().ok_or("missing matte model")?,
            preview_pool: pool,
            sequence_pool,
            sequence_workers,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inference_thread_budgets_bound_preview_and_sequence_and_honor_overrides() {
        for (available, expected) in [(0, (1, 1)), (1, (1, 1)), (2, (2, 2)), (4, (4, 4)), (8, (4, 8)), (128, (4, 8))] {
            assert_eq!(thread_budgets(available, None).unwrap(), expected);
        }
        assert_eq!(thread_budgets(8, Some("2")).unwrap(), (2, 2));
        for invalid in ["0", "9", "-1", "NaN", "", "999999999999999999999999999999"] {
            assert!(thread_budgets(8, Some(invalid)).is_err());
        }
        assert_eq!(tile_worker_budget(1, None).unwrap(), 1);
        assert_eq!(tile_worker_budget(8, None).unwrap(), 4);
        assert_eq!(tile_worker_budget(8, Some("1")).unwrap(), 1);
        assert_eq!(tile_worker_budget(8, Some("2")).unwrap(), 2);
        for invalid in ["0", "5", "-1", "NaN", "", "999999999999999999999999999999"] {
            assert!(tile_worker_budget(8, Some(invalid)).is_err());
        }
        assert!(tile_worker_budget(2, Some("3")).is_err());
    }

    #[test]
    fn malformed_truncated_and_modified_model_packages_fail_safely() {
        let unique = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
        let directory = std::env::temp_dir().join(format!("filmcraft-native-models-{}-{unique}", std::process::id()));
        std::fs::create_dir(&directory).unwrap();
        let manifest_path = directory.join("manifest.json");
        let pinned = include_bytes!("../../../experiments/rotoscope/native-models.json");
        for bytes in [b"{".as_slice(), &pinned[..32], &vec![0; 16 * 1024 + 1]] {
            std::fs::write(&manifest_path, bytes).unwrap();
            let result = std::panic::catch_unwind(|| Models::load(&directory));
            assert!(result.is_ok() && result.unwrap().is_err());
        }
        std::fs::write(&manifest_path, pinned).unwrap();
        let image = directory.join("image.onnx");
        for length in [0, 1, 19_801_551, 19_801_552] {
            let file = std::fs::File::create(&image).unwrap();
            file.set_len(length).unwrap(); // Sparse zeros: correct length still fails the pinned checksum.
            let result = std::panic::catch_unwind(|| Models::load(&directory));
            assert!(result.is_ok() && result.unwrap().is_err());
        }
        let mut altered: serde_json::Value = serde_json::from_slice(pinned).unwrap();
        altered["files"][0]["file"] = "../outside.onnx".into();
        std::fs::write(&manifest_path, serde_json::to_vec(&altered).unwrap()).unwrap();
        assert!(Models::load(&directory).is_err(), "manifest cannot redirect model reads");
        std::fs::remove_dir_all(directory).unwrap();
    }
}
