//! The speech-enhancement model catalogue and the model store.
//!
//! Models are downloaded after the user confirms (the dialog shows the size, the source URL and
//! the licence) into `<data dir>/models/<id>/`. They are never bundled with FilmCraft or committed
//! to the repository. Every file is pinned to a commit of its upstream repository and checked
//! against its SHA-256 before it is used.
//!
//! Licence: DeepFilterNet (code and the model weights in its repository) is dual-licensed MIT OR
//! Apache-2.0 (<https://github.com/Rikorose/DeepFilterNet#license>).
//!
//! **The order of [`catalogue`] is part of the project file format**: the effect's `model` choice
//! stores an index. Append new models; never reorder or remove.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock, PoisonError, RwLock};
use std::time::SystemTime;

use serde::Serialize;
use sha2::Digest;

/// One file of a model.
#[derive(Clone, Copy, Debug, Serialize)]
pub struct ModelFile {
    pub name: &'static str,
    pub url: &'static str,
    pub sha256: &'static str,
    pub size: u64,
}

/// The inference code a model runs on.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub enum Backend {
    /// DeepFilterNet (libDF on tract): one `.tar.gz` holding three ONNX graphs and `config.ini`.
    DeepFilterNet,
}

/// A downloadable speech-enhancement model.
#[derive(Clone, Copy, Debug, Serialize)]
pub struct ModelInfo {
    pub id: &'static str,
    pub name: &'static str,
    pub description: &'static str,
    pub license: &'static str,
    pub license_url: &'static str,
    pub author: &'static str,
    /// Human-readable source page.
    pub source: &'static str,
    pub backend: Backend,
    /// The sample rate the model works at; other sequence rates are converted.
    pub sample_rate: u32,
    pub files: &'static [ModelFile],
}

impl ModelInfo {
    pub fn size(&self) -> u64 {
        self.files.iter().map(|f| f.size).sum()
    }
}

const DF_LICENSE: &str = "MIT OR Apache-2.0";
const DF_LICENSE_URL: &str = "https://github.com/Rikorose/DeepFilterNet/blob/978576aa8400552a4ce9730838c635aa30db5e61/LICENSE-MIT";
const DF_AUTHOR: &str = "Hendrik Schröter and others (DeepFilterNet)";
const DF_SOURCE: &str = "https://github.com/Rikorose/DeepFilterNet";

macro_rules! df {
    ($name:literal, $sha:literal, $size:expr) => {
        ModelFile {
            name: $name,
            url: concat!("https://raw.githubusercontent.com/Rikorose/DeepFilterNet/978576aa8400552a4ce9730838c635aa30db5e61/models/", $name),
            sha256: $sha,
            size: $size,
        }
    };
}

static CATALOGUE: &[ModelInfo] = &[
    ModelInfo {
        id: "deepfilternet3",
        name: "DeepFilterNet 3",
        description: "Full band (48 kHz), 2.1 M parameters. The default: strong noise removal that keeps speech natural; 40 ms latency.",
        license: DF_LICENSE,
        license_url: DF_LICENSE_URL,
        author: DF_AUTHOR,
        source: DF_SOURCE,
        backend: Backend::DeepFilterNet,
        sample_rate: 48_000,
        files: &[df!("DeepFilterNet3_onnx.tar.gz", "c94d91f70911001c946e0fabb4aa9adc37045f45a03b56008cb0c8244cb63616", 7_983_136)],
    },
    ModelInfo {
        id: "deepfilternet3-ll",
        name: "DeepFilterNet 3 (low latency)",
        description: "Full band (48 kHz), no look-ahead: 20 ms latency. A little less noise removal than DeepFilterNet 3; larger download.",
        license: DF_LICENSE,
        license_url: DF_LICENSE_URL,
        author: DF_AUTHOR,
        source: DF_SOURCE,
        backend: Backend::DeepFilterNet,
        sample_rate: 48_000,
        files: &[df!("DeepFilterNet3_ll_onnx.tar.gz", "5998e58e8ba0e09bb76986ef97b84afa065a571ef282d4a1222f341e3251cf3a", 36_359_660)],
    },
];

/// The default model.
pub const DEFAULT_MODEL: &str = "deepfilternet3";

pub fn catalogue() -> &'static [ModelInfo] {
    CATALOGUE
}

pub fn find(id: &str) -> Option<&'static ModelInfo> {
    CATALOGUE.iter().find(|m| m.id == id)
}

/// The model the effect's `model` choice `index` selects (the first one for an index out of range).
pub fn by_index(index: usize) -> Option<&'static ModelInfo> {
    CATALOGUE.get(index).or_else(|| CATALOGUE.first())
}

/// Where model `m` lives under `models_dir`.
pub fn model_dir(models_dir: &Path, m: &ModelInfo) -> PathBuf {
    models_dir.join(m.id)
}

fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && !name.contains(['/', '\\'])
        && !matches!(name, "." | "..")
        && Path::new(name).components().count() == 1
        && Path::new(name).file_name().is_some_and(|part| part == name)
}

type Stamp = (u64, Option<SystemTime>, &'static str);

/// Files whose SHA-256 was checked, by path, with the length and modification time they had: the
/// audio thread asks whether a model is installed for every chain it builds, and must not hash
/// megabytes each time.
fn verified() -> &'static Mutex<HashMap<PathBuf, Stamp>> {
    static V: OnceLock<Mutex<HashMap<PathBuf, Stamp>>> = OnceLock::new();
    V.get_or_init(Default::default)
}

fn valid_file(path: &Path, file: &ModelFile) -> bool {
    if !valid_name(file.name) {
        return false;
    }
    let Ok(meta) = std::fs::metadata(path) else { return false };
    if !meta.is_file() || meta.len() != file.size {
        return false;
    }
    let stamp: Stamp = (meta.len(), meta.modified().ok(), file.sha256);
    if verified().lock().unwrap_or_else(PoisonError::into_inner).get(path) == Some(&stamp) {
        return true;
    }
    let Ok(mut input) = std::fs::File::open(path) else { return false };
    let mut hasher = sha2::Sha256::new();
    if std::io::copy(&mut input, &mut hasher).is_err() {
        return false;
    }
    let ok = format!("{:x}", hasher.finalize()).eq_ignore_ascii_case(file.sha256);
    if ok {
        let mut v = verified().lock().unwrap_or_else(PoisonError::into_inner);
        if v.len() > 64 {
            v.clear();
        }
        v.insert(path.to_path_buf(), stamp);
    }
    ok
}

/// All files present and matching their pinned size and SHA-256.
pub fn installed(models_dir: &Path, m: &ModelInfo) -> bool {
    let d = model_dir(models_dir, m);
    m.files.iter().all(|f| valid_file(&d.join(f.name), f))
}

/// Bytes still to download for `m` (files already present are skipped).
pub fn missing_bytes(models_dir: &Path, m: &ModelInfo) -> u64 {
    let d = model_dir(models_dir, m);
    m.files.iter().filter(|f| !valid_file(&d.join(f.name), f)).map(|f| f.size).sum()
}

/// Delete the files of `m`. Missing files are not an error.
pub fn remove(models_dir: &Path, m: &ModelInfo) -> std::io::Result<()> {
    let d = model_dir(models_dir, m);
    for f in m.files.iter().filter(|f| valid_name(f.name)) {
        match std::fs::remove_file(d.join(f.name)) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e),
        }
    }
    let _ = std::fs::remove_dir(&d);
    store_changed();
    Ok(())
}

fn dir_slot() -> &'static RwLock<Option<PathBuf>> {
    static D: OnceLock<RwLock<Option<PathBuf>>> = OnceLock::new();
    D.get_or_init(Default::default)
}

static GENERATION: AtomicU64 = AtomicU64::new(0);

/// Tell the effect where downloaded models live (`<data dir>/models`). The host sets this once at
/// start-up; until then the effect passes audio through.
pub fn set_models_dir(dir: Option<PathBuf>) {
    let mut slot = dir_slot().write().unwrap_or_else(PoisonError::into_inner);
    if *slot != dir {
        *slot = dir;
        drop(slot);
        store_changed();
    }
}

/// The directory given to [`set_models_dir`].
pub fn models_dir() -> Option<PathBuf> {
    dir_slot().read().unwrap_or_else(PoisonError::into_inner).clone()
}

/// Call after a model was installed or removed, so effect chains are rebuilt with the new state.
pub fn store_changed() {
    GENERATION.fetch_add(1, Ordering::Relaxed);
}

/// Changes whenever the set of installed models may have changed.
pub fn generation() -> u64 {
    GENERATION.load(Ordering::Relaxed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalogue_is_pinned_and_checksummed() {
        assert!(find(DEFAULT_MODEL).is_some());
        let mut ids = std::collections::HashSet::new();
        for m in catalogue() {
            assert!(ids.insert(m.id), "duplicate id {}", m.id);
            assert!(valid_name(m.id), "{}", m.id);
            assert!(m.size() > 1_000_000, "{}", m.id);
            assert!(m.sample_rate > 0);
            assert!(!m.license.is_empty() && m.license_url.starts_with("https://"));
            for f in m.files {
                assert!(valid_name(f.name), "{}", f.name);
                assert!(f.url.starts_with("https://raw.githubusercontent.com/Rikorose/DeepFilterNet/"), "{}", f.url);
                // pinned to a full commit id, not a branch or tag
                let rev = f.url.split('/').nth(5).unwrap();
                assert!(rev.len() == 40 && rev.bytes().all(|b| b.is_ascii_hexdigit()), "{}", f.url);
                assert!(f.url.ends_with(f.name));
                assert_eq!(f.sha256.len(), 64);
                assert!(f.sha256.bytes().all(|b| b.is_ascii_hexdigit()));
            }
        }
    }

    /// The effect's `model` choice stores an index: the order is persistent.
    #[test]
    fn catalogue_order_is_stable() {
        let ids: Vec<_> = catalogue().iter().map(|m| m.id).collect();
        assert_eq!(ids, ["deepfilternet3", "deepfilternet3-ll"]);
        assert_eq!(by_index(0).unwrap().id, "deepfilternet3");
        assert_eq!(by_index(1).unwrap().id, "deepfilternet3-ll");
        assert_eq!(by_index(usize::MAX).unwrap().id, DEFAULT_MODEL);
    }

    #[test]
    fn artifact_names_stay_inside_the_model_directory() {
        for name in ["../escape", "/tmp/escape", r"..\escape", r"C:\escape", "", ".", ".."] {
            assert!(!valid_name(name), "accepted {name:?}");
        }
        assert!(valid_name("DeepFilterNet3_onnx.tar.gz"));
    }

    fn tiny() -> ModelInfo {
        static FILES: &[ModelFile] = &[ModelFile {
            name: "tiny.bin",
            url: "https://example.invalid/tiny.bin",
            sha256: "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad",
            size: 3,
        }];
        ModelInfo {
            id: "integrity-test",
            name: "integrity test",
            description: "",
            license: "",
            license_url: "",
            author: "",
            source: "",
            backend: Backend::DeepFilterNet,
            sample_rate: 48_000,
            files: FILES,
        }
    }

    #[test]
    fn same_length_corruption_is_not_installed() {
        let model = tiny();
        let root = tempfile::tempdir().unwrap();
        assert!(!installed(root.path(), &model));
        assert_eq!(missing_bytes(root.path(), &model), 3);
        let directory = model_dir(root.path(), &model);
        std::fs::create_dir_all(&directory).unwrap();
        std::fs::write(directory.join("tiny.bin"), b"bad").unwrap();
        assert!(!installed(root.path(), &model));
        assert_eq!(missing_bytes(root.path(), &model), 3);
        std::fs::write(directory.join("tiny.bin"), b"abc").unwrap();
        assert!(installed(root.path(), &model));
        // the cached verdict does not outlive the file
        std::fs::write(directory.join("tiny.bin"), b"abcd").unwrap();
        assert!(!installed(root.path(), &model));
    }

    #[test]
    fn remove_deletes_the_files_and_bumps_the_generation() {
        let model = tiny();
        let root = tempfile::tempdir().unwrap();
        let directory = model_dir(root.path(), &model);
        std::fs::create_dir_all(&directory).unwrap();
        std::fs::write(directory.join("tiny.bin"), b"abc").unwrap();
        let before = generation();
        remove(root.path(), &model).unwrap();
        assert!(!installed(root.path(), &model));
        assert!(!directory.exists());
        assert!(generation() > before);
        remove(root.path(), &model).unwrap();
    }
}
