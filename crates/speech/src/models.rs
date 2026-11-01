//! The speech model catalogue and the model store.
//!
//! Models are downloaded only when asked for (`transcript.downloadModel`), into
//! `<data dir>/models/<id>/`. They are never bundled with FilmCraft or committed to the
//! repository. Every file is pinned to a Hugging Face revision of its publisher's own repository
//! (OpenAI's or NVIDIA's) and checked against its SHA-256 and size before it is used.
//!
//! Licence: OpenAI released the Whisper code and model weights under the MIT licence
//! (<https://github.com/openai/whisper/blob/main/LICENSE>); the safetensors conversions OpenAI
//! publishes at `huggingface.co/openai/whisper-*` are labelled Apache-2.0. NVIDIA publishes
//! Parakeet TDT 0.6B v3 at `huggingface.co/nvidia/parakeet-tdt-0.6b-v3` under CC-BY-4.0, which
//! allows use and redistribution with attribution (the catalogue entry carries it). All are
//! permissive.

use std::path::{Path, PathBuf};

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

/// The recogniser that runs a model.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Engine {
    Whisper,
    Parakeet,
}

/// A downloadable speech model.
#[derive(Clone, Copy, Debug, Serialize)]
pub struct ModelInfo {
    pub id: &'static str,
    pub engine: Engine,
    pub name: &'static str,
    /// Recognises languages other than English (and detects the language).
    pub multilingual: bool,
    pub description: &'static str,
    pub license: &'static str,
    pub license_url: &'static str,
    pub author: &'static str,
    /// Human-readable source page.
    pub source: &'static str,
    pub files: &'static [ModelFile],
}

impl ModelInfo {
    pub fn size(&self) -> u64 {
        self.files.iter().map(|f| f.size).sum()
    }
}

const LICENSE: &str = "MIT (OpenAI Whisper weights); Hugging Face conversion: Apache-2.0";
const LICENSE_URL: &str = "https://github.com/openai/whisper/blob/main/LICENSE";
const AUTHOR: &str = "OpenAI";

macro_rules! hf {
    ($repo:literal, $rev:literal, $name:literal, $sha:literal, $size:expr) => {
        ModelFile { name: $name, url: concat!("https://huggingface.co/openai/", $repo, "/resolve/", $rev, "/", $name), sha256: $sha, size: $size }
    };
}

#[cfg(test)]
const PARAKEET_REV: &str = "541d1f99c6b0c3cd0b11a95167540bb8edefd82b";

macro_rules! nvidia {
    ($name:literal, $sha:literal, $size:expr) => {
        ModelFile {
            name: $name,
            url: concat!("https://huggingface.co/nvidia/parakeet-tdt-0.6b-v3/resolve/", "541d1f99c6b0c3cd0b11a95167540bb8edefd82b", "/", $name),
            sha256: $sha,
            size: $size,
        }
    };
}

const TOKENIZER_MULTI: &str = "27fc476bfe7f17299480be2273fc0608e4d5a99aba2ab5dec5374b4482d1a566";

static CATALOGUE: &[ModelInfo] = &[
    ModelInfo {
        id: "whisper-tiny",
        engine: Engine::Whisper,
        name: "Whisper tiny (multilingual)",
        multilingual: true,
        description: "39 M parameters. Fastest; fine for clear speech, rough on noisy audio.",
        license: LICENSE,
        license_url: LICENSE_URL,
        author: AUTHOR,
        source: "https://huggingface.co/openai/whisper-tiny",
        files: &[
            hf!(
                "whisper-tiny",
                "169d4a4341b33bc18d8881c4b69c2e104e1cc0af",
                "config.json",
                "ffdccec4f3211f4c63310f2b7098f309fe70f3952cedc5e4d11e43f5b2379b98",
                1983
            ),
            hf!(
                "whisper-tiny",
                "169d4a4341b33bc18d8881c4b69c2e104e1cc0af",
                "generation_config.json",
                "a5d5325911f16e74001a72fa13d6e208eee51548f994646de1f4b4cc8b35b512",
                3747
            ),
            hf!(
                "whisper-tiny",
                "169d4a4341b33bc18d8881c4b69c2e104e1cc0af",
                "tokenizer.json",
                "27fc476bfe7f17299480be2273fc0608e4d5a99aba2ab5dec5374b4482d1a566",
                2480466
            ),
            hf!(
                "whisper-tiny",
                "169d4a4341b33bc18d8881c4b69c2e104e1cc0af",
                "model.safetensors",
                "7ebd0e69e78190ffe1438491fa05cc1f5c1aa3a4c4db3bc1723adbb551ea2395",
                151061672
            ),
        ],
    },
    ModelInfo {
        id: "whisper-base",
        engine: Engine::Whisper,
        name: "Whisper base (multilingual)",
        multilingual: true,
        description: "74 M parameters. The default: a good balance of speed and accuracy.",
        license: LICENSE,
        license_url: LICENSE_URL,
        author: AUTHOR,
        source: "https://huggingface.co/openai/whisper-base",
        files: &[
            hf!(
                "whisper-base",
                "e37978b90ca9030d5170a5c07aadb050351a65bb",
                "config.json",
                "a153c53883a6799b6f056b4a8d1a515c9926d03994682ba88a7616618d7da0c1",
                1983
            ),
            hf!(
                "whisper-base",
                "e37978b90ca9030d5170a5c07aadb050351a65bb",
                "generation_config.json",
                "444b3f636d2fff89dd9ecf549e2a085b61f7ff0fa0246d4628bac6a3b8cc9ba4",
                3807
            ),
            hf!(
                "whisper-base",
                "e37978b90ca9030d5170a5c07aadb050351a65bb",
                "tokenizer.json",
                "27fc476bfe7f17299480be2273fc0608e4d5a99aba2ab5dec5374b4482d1a566",
                2480466
            ),
            hf!(
                "whisper-base",
                "e37978b90ca9030d5170a5c07aadb050351a65bb",
                "model.safetensors",
                "07cadb9f25677c8d50df603e66a98fbd842cce45047139baeb16e6219a1e807b",
                290403936
            ),
        ],
    },
    ModelInfo {
        id: "whisper-small",
        engine: Engine::Whisper,
        name: "Whisper small (multilingual)",
        multilingual: true,
        description: "244 M parameters. Most accurate here; about 4× slower than base.",
        license: LICENSE,
        license_url: LICENSE_URL,
        author: AUTHOR,
        source: "https://huggingface.co/openai/whisper-small",
        files: &[
            hf!(
                "whisper-small",
                "973afd24965f72e36ca33b3055d56a652f456b4d",
                "config.json",
                "e6a2b489da1b5aed65a8eb8d1e7466fa867ad5643a8bc138ba708bd56b2875c4",
                1967
            ),
            hf!(
                "whisper-small",
                "973afd24965f72e36ca33b3055d56a652f456b4d",
                "generation_config.json",
                "71565b8ef50d0bf7a1193ed4bbed195b94e70c18894d81bba2f1233dcec3ab53",
                3868
            ),
            hf!(
                "whisper-small",
                "973afd24965f72e36ca33b3055d56a652f456b4d",
                "tokenizer.json",
                "27fc476bfe7f17299480be2273fc0608e4d5a99aba2ab5dec5374b4482d1a566",
                2480466
            ),
            hf!(
                "whisper-small",
                "973afd24965f72e36ca33b3055d56a652f456b4d",
                "model.safetensors",
                "1d7734884874f1a1513ed9aa760a4f8e97aaa02fd6d93a3a85d27b2ae9ca596b",
                966995080
            ),
        ],
    },
    ModelInfo {
        id: "parakeet-tdt-0.6b-v3",
        engine: Engine::Parakeet,
        name: "Parakeet TDT 0.6B v3 (25 European languages)",
        multilingual: true,
        description: "600 M parameters (NVIDIA). Punctuation and per-word timing for word-by-word captions. English and 24 other European languages, detected automatically. Needs about 4 GB of memory.",
        license: "CC-BY-4.0 (NVIDIA Parakeet TDT 0.6B v3)",
        license_url: "https://creativecommons.org/licenses/by/4.0/",
        author: "NVIDIA",
        source: "https://huggingface.co/nvidia/parakeet-tdt-0.6b-v3",
        files: &[
            nvidia!("config.json", "e747b85e1bdfd300c8b8ac63bac8dd5221f8fe9bc275b48d06c735fcd6971b6e", 1153),
            nvidia!("tokenizer.json", "bd321b096832a3f270bd3b2a88823957920f1a5c5ada71114a26ea729d0cbe91", 1159960),
            nvidia!("model.safetensors", "3a2026366188c8c68598edbbff92f8d11590a08e0ae2e6775544e7b07d6a5e11", 2508311120),
        ],
    },
];

/// The default model.
pub const DEFAULT_MODEL: &str = "whisper-base";

pub fn catalogue() -> &'static [ModelInfo] {
    CATALOGUE
}

pub fn find(id: &str) -> Option<&'static ModelInfo> {
    CATALOGUE.iter().find(|m| m.id == id)
}

/// Where model `m` lives under `models_dir`.
pub fn model_dir(models_dir: &Path, m: &ModelInfo) -> PathBuf {
    models_dir.join(m.id)
}

/// Fast completeness hint for UI controls; inference/download still verify SHA-256.
pub fn cached(models_dir: &Path, m: &ModelInfo) -> bool {
    let dir = model_dir(models_dir, m);
    m.files.iter().all(|file| std::fs::metadata(dir.join(file.name)).is_ok_and(|metadata| metadata.is_file() && metadata.len() == file.size))
}

/// Cooperative cross-process lease. Readers retain it for inference; download/delete use
/// an exclusive lease. Lock files stay outside the removable model directory.
pub fn lease(models_dir: &Path, m: &ModelInfo, exclusive: bool) -> Result<std::fs::File, crate::SpeechError> {
    if !valid_name(m.id) {
        return Err(crate::SpeechError::UnknownModel(m.id.into()));
    }
    std::fs::create_dir_all(models_dir)?;
    let canonical_model = std::fs::canonicalize(model_dir(models_dir, m)).ok();
    let root = canonical_model.as_ref().and_then(|p| p.parent()).unwrap_or(models_dir);
    let locks = root.join(".locks");
    std::fs::create_dir_all(&locks)?;
    let file = std::fs::File::options().read(true).write(true).create(true).truncate(false).open(locks.join(format!("{}.lock", m.id)))?;
    let result = if exclusive { file.try_lock() } else { file.try_lock_shared() };
    result.map_err(|error| match error {
        std::fs::TryLockError::WouldBlock => {
            crate::SpeechError::Model("this model is in use or downloading in another FilmCraft session; stop it and try again".into())
        }
        std::fs::TryLockError::Error(error) => crate::SpeechError::Io(error.to_string()),
    })?;
    Ok(file)
}

/// Remove only this catalogue model's cache. Symlinks are unlinked, never followed;
/// unexpected subdirectories or excessive entries are rejected before deleting anything.
pub fn remove(models_dir: &Path, id: &str) -> Result<bool, crate::SpeechError> {
    let model = find(id).ok_or_else(|| crate::SpeechError::UnknownModel(id.into()))?;
    let _lease = lease(models_dir, model, true)?;
    let dir = model_dir(models_dir, model);
    let metadata = match std::fs::symlink_metadata(&dir) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error.into()),
    };
    if metadata.file_type().is_symlink() {
        // remove_dir_all does not follow a directory symlink, including on Windows.
        std::fs::remove_dir_all(&dir)?;
    } else if metadata.is_dir() {
        let entries = std::fs::read_dir(&dir)?.take(65).collect::<Result<Vec<_>, _>>()?;
        if entries.len() > 64
            || entries.iter().any(|entry| {
                let name = entry.file_name();
                let known = model.files.iter().any(|file| name == file.name || name == format!("{}.part", file.name).as_str());
                !known || entry.file_type().map_or(true, |kind| kind.is_dir())
            })
        {
            return Err(crate::SpeechError::Io("model cache contains unexpected files or folders; nothing was deleted".into()));
        }
        for entry in entries {
            std::fs::remove_file(entry.path())?;
        }
        std::fs::remove_dir(&dir)?;
    } else {
        return Err(crate::SpeechError::Io("model cache is not a directory; nothing was deleted".into()));
    }
    Ok(true)
}

fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && !name.contains(['/', '\\'])
        && !matches!(name, "." | "..")
        && Path::new(name).components().count() == 1
        && Path::new(name).file_name().is_some_and(|part| part == name)
}

fn valid_file(path: &Path, file: &ModelFile) -> bool {
    if !valid_name(file.name) || !std::fs::metadata(path).is_ok_and(|metadata| metadata.is_file() && metadata.len() == file.size) {
        return false;
    }
    let Ok(mut input) = std::fs::File::open(path) else { return false };
    let mut hasher = sha2::Sha256::new();
    if std::io::copy(&mut input, &mut hasher).is_err() {
        return false;
    }
    format!("{:x}", hasher.finalize()).eq_ignore_ascii_case(file.sha256)
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

/// The tokenizer shared by the multilingual models has one checksum.
pub fn shared_tokenizer_sha() -> &'static str {
    TOKENIZER_MULTI
}

/// Download progress: `(bytes done, bytes total, file)`; return false to cancel.
#[cfg(feature = "download")]
pub type DownloadProgress<'a> = &'a mut dyn FnMut(u64, u64, &str) -> bool;

/// Download the missing files of `m` into `models_dir/<id>/`: each file streams into `<name>.part`,
/// its SHA-256 is checked, then it is renamed into place. A wrong checksum deletes the part file
/// and fails. Pure Rust TLS (rustls + RustCrypto) with the operating system's certificate
/// verifier.
#[cfg(feature = "download")]
pub fn download(models_dir: &Path, m: &ModelInfo, progress: DownloadProgress) -> Result<(), crate::SpeechError> {
    let _lease = lease(models_dir, m, true)?;
    download_files(&model_dir(models_dir, m), m.files, progress)
}

/// [`download`] for any pinned file list (the neural voices use it too): missing files of
/// `files` go into `dir`, each verified against its SHA-256 and size before it is renamed into
/// place.
#[cfg(feature = "download")]
pub fn download_files(dir: &Path, files: &[ModelFile], progress: DownloadProgress) -> Result<(), crate::SpeechError> {
    use crate::SpeechError;
    use std::io::{Read, Write};
    if let Some(file) = files.iter().find(|file| !valid_name(file.name)) {
        return Err(SpeechError::Download(format!("invalid model artifact name: {:?}", file.name)));
    }
    std::fs::create_dir_all(dir)?;
    let total: u64 = files.iter().filter(|f| !valid_file(&dir.join(f.name), f)).map(|f| f.size).sum();
    let agent = agent();
    let mut done = 0u64;
    for f in files {
        let dest = dir.join(f.name);
        if valid_file(&dest, f) {
            continue;
        }
        let part = dir.join(format!("{}.part", f.name));
        let resp = agent.get(f.url).call().map_err(|e| SpeechError::Download(format!("{}: {e}", f.url)))?;
        let mut body = resp.into_body();
        let mut reader = body.as_reader();
        let mut out = std::io::BufWriter::new(std::fs::File::create(&part)?);
        let mut hasher = sha2::Sha256::new();
        let mut buf = vec![0u8; 1 << 20];
        let mut got = 0u64;
        loop {
            let n = reader.read(&mut buf).map_err(|e| SpeechError::Download(format!("{}: {e}", f.name)))?;
            if n == 0 {
                break;
            }
            // The server is untrusted: never fill the disk beyond the catalogue's pinned size
            // while waiting for the final checksum. Reject before hashing or writing excess bytes.
            if n as u64 > f.size.saturating_sub(got) {
                drop(out);
                let _ = std::fs::remove_file(&part);
                return Err(SpeechError::Download(format!("{}: response exceeds pinned size of {} bytes", f.name, f.size)));
            }
            hasher.update(&buf[..n]);
            out.write_all(&buf[..n])?;
            got += n as u64;
            if !progress(done + got, total, f.name) {
                drop(out);
                let _ = std::fs::remove_file(&part);
                return Err(SpeechError::Cancelled);
            }
        }
        out.flush()?;
        drop(out);
        let sha: String = hasher.finalize().iter().map(|b| format!("{b:02x}")).collect();
        if sha != f.sha256 || got != f.size {
            let _ = std::fs::remove_file(&part);
            return Err(SpeechError::Download(format!("{}: checksum mismatch (got {sha}, {got} bytes; expected {}, {} bytes)", f.name, f.sha256, f.size)));
        }
        std::fs::rename(&part, &dest)?;
        done += f.size;
    }
    Ok(())
}

#[cfg(feature = "download")]
fn agent() -> ureq::Agent {
    let provider = std::sync::Arc::new(rustls_rustcrypto::provider());
    ureq::Agent::config_builder()
        .tls_config(
            ureq::tls::TlsConfig::builder()
                .provider(ureq::tls::TlsProvider::Rustls)
                .root_certs(ureq::tls::RootCerts::PlatformVerifier)
                .unversioned_rustls_crypto_provider(provider)
                .build(),
        )
        .timeout_connect(Some(std::time::Duration::from_secs(30)))
        .build()
        .into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn model_cache_deletion_is_scoped_and_requires_a_catalogue_id() {
        let root = tempfile::tempdir().unwrap();
        let model = find("whisper-tiny").unwrap();
        let dir = model_dir(root.path(), model);
        std::fs::create_dir(&dir).unwrap();
        std::fs::write(dir.join("model.safetensors"), b"synthetic weights").unwrap();
        std::fs::write(dir.join("model.safetensors.part"), b"partial").unwrap();
        std::fs::write(root.path().join("project.fcproj"), b"keep project and transcript").unwrap();
        let other = root.path().join("whisper-base");
        std::fs::create_dir(&other).unwrap();
        std::fs::write(other.join("config.json"), b"keep other model").unwrap();
        for id in ["../whisper-tiny", "/whisper-tiny", "", "not-a-model"] {
            assert!(remove(root.path(), id).is_err());
            assert!(dir.join("model.safetensors").exists());
        }
        assert!(remove(root.path(), model.id).unwrap());
        assert!(!dir.exists());
        assert_eq!(std::fs::read(root.path().join("project.fcproj")).unwrap(), b"keep project and transcript");
        assert!(other.join("config.json").exists());
        assert!(!remove(root.path(), model.id).unwrap());
    }

    #[test]
    fn model_cache_deletion_refuses_nested_or_excessive_entries_before_removal() {
        let root = tempfile::tempdir().unwrap();
        let model = find("whisper-tiny").unwrap();
        let dir = model_dir(root.path(), model);
        std::fs::create_dir_all(dir.join("unexpected")).unwrap();
        std::fs::write(dir.join("config.json"), b"keep").unwrap();
        assert!(remove(root.path(), model.id).is_err());
        assert!(dir.join("config.json").exists());
        std::fs::remove_dir(dir.join("unexpected")).unwrap();
        std::fs::write(dir.join("personal-notes.txt"), b"keep user data").unwrap();
        assert!(remove(root.path(), model.id).is_err());
        assert!(dir.join("config.json").exists());
        assert_eq!(std::fs::read(dir.join("personal-notes.txt")).unwrap(), b"keep user data");
        std::fs::remove_file(dir.join("personal-notes.txt")).unwrap();
        for n in 0..65 {
            std::fs::write(dir.join(format!("file{n}")), []).unwrap();
        }
        assert!(remove(root.path(), model.id).is_err());
        assert!(dir.join("config.json").exists());
    }

    #[test]
    fn model_leases_protect_inference_download_and_deletion() {
        let root = tempfile::tempdir().unwrap();
        let model = find("whisper-tiny").unwrap();
        let first = lease(root.path(), model, false).unwrap();
        let second = lease(root.path(), model, false).unwrap();
        assert!(lease(root.path(), model, true).is_err());
        assert!(remove(root.path(), model.id).is_err());
        drop(first);
        assert!(lease(root.path(), model, true).is_err());
        drop(second);
        let writer = lease(root.path(), model, true).unwrap();
        assert!(lease(root.path(), model, false).is_err());
        assert!(lease(root.path(), model, true).is_err());
        drop(writer);
        assert!(lease(root.path(), model, false).is_ok());
    }

    #[cfg(unix)]
    #[test]
    fn model_cache_symlinks_share_leases_and_deletion_keeps_shared_weights() {
        let shared = tempfile::tempdir().unwrap();
        let cache = tempfile::tempdir().unwrap();
        let model = find("whisper-tiny").unwrap();
        let target = model_dir(shared.path(), model);
        std::fs::create_dir(&target).unwrap();
        std::fs::write(target.join("model.safetensors"), b"keep shared weights").unwrap();
        let link = model_dir(cache.path(), model);
        std::os::unix::fs::symlink(&target, &link).unwrap();
        let reader = lease(shared.path(), model, false).unwrap();
        assert!(remove(cache.path(), model.id).is_err());
        drop(reader);
        assert!(remove(cache.path(), model.id).unwrap());
        assert!(!link.exists());
        assert_eq!(std::fs::read(target.join("model.safetensors")).unwrap(), b"keep shared weights");
    }

    #[test]
    fn catalogue_is_pinned_and_checksummed() {
        assert!(find(DEFAULT_MODEL).is_some());
        for m in catalogue() {
            assert!(m.size() > 100_000_000, "{}", m.id);
            for f in m.files {
                match m.engine {
                    Engine::Whisper => assert!(f.url.starts_with("https://huggingface.co/openai/whisper-"), "{}", f.url),
                    Engine::Parakeet => assert!(f.url.starts_with("https://huggingface.co/nvidia/parakeet-") && f.url.contains(PARAKEET_REV), "{}", f.url),
                }
                assert!(f.url.contains("/resolve/") && f.url.ends_with(f.name));
                assert_eq!(f.sha256.len(), 64);
                assert!(f.sha256.bytes().all(|b| b.is_ascii_hexdigit()));
                if f.name == "tokenizer.json" && m.engine == Engine::Whisper {
                    assert_eq!(f.sha256, shared_tokenizer_sha());
                }
            }
        }
    }

    #[test]
    fn installed_checks_sizes() {
        let dir = std::env::temp_dir().join(format!("filmcraft-speech-models-{}", std::process::id()));
        let m = find("whisper-tiny").unwrap();
        assert!(!installed(&dir, m));
        assert_eq!(missing_bytes(&dir, m), m.size());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn artifact_names_stay_inside_the_model_directory() {
        for name in ["../escape", "/tmp/escape", r"..\escape", r"C:\escape", "", ".", ".."] {
            assert!(!valid_name(name), "accepted {name:?}");
        }
        assert!(valid_name("model.safetensors"));
    }

    #[test]
    fn same_length_corruption_is_not_installed() {
        static FILES: &[ModelFile] = &[ModelFile {
            name: "tiny.bin",
            url: "https://example.invalid/tiny.bin",
            sha256: "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad",
            size: 3,
        }];
        let model = ModelInfo {
            id: "integrity-test",
            engine: Engine::Whisper,
            name: "integrity test",
            multilingual: false,
            description: "",
            license: "",
            license_url: "",
            author: "",
            source: "",
            files: FILES,
        };
        let root = tempfile::tempdir().unwrap();
        let directory = model_dir(root.path(), &model);
        std::fs::create_dir_all(&directory).unwrap();
        std::fs::write(directory.join("tiny.bin"), b"bad").unwrap();
        assert!(!installed(root.path(), &model));
        assert_eq!(missing_bytes(root.path(), &model), 3);
        std::fs::write(directory.join("tiny.bin"), b"abc").unwrap();
        assert!(installed(root.path(), &model));
    }

    #[cfg(feature = "download")]
    #[test]
    fn oversized_download_stops_at_the_pinned_size_without_publishing() {
        use std::io::{Read, Write};
        let server = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/tiny.bin", server.local_addr().unwrap());
        let worker = std::thread::spawn(move || {
            let (mut stream, _) = server.accept().unwrap();
            stream.set_read_timeout(Some(std::time::Duration::from_secs(5))).unwrap();
            let mut request = Vec::new();
            for byte in std::io::BufReader::new(Read::by_ref(&mut stream).take(8192)).bytes() {
                request.push(byte.unwrap());
                if request.ends_with(b"\r\n\r\n") {
                    break;
                }
            }
            stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 4\r\nConnection: close\r\n\r\nabcd").unwrap();
        });
        let file = ModelFile {
            name: "tiny.bin",
            url: Box::leak(url.into_boxed_str()),
            sha256: "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad",
            size: 3,
        };
        let root = tempfile::tempdir().unwrap();
        let mut reported = Vec::new();
        let error = download_files(root.path(), &[file], &mut |done, total, _| {
            reported.push((done, total));
            true
        })
        .unwrap_err();
        worker.join().unwrap();
        assert!(error.to_string().contains("exceeds pinned size"), "{error}");
        assert!(reported.iter().all(|(done, total)| done <= total));
        assert!(!root.path().join("tiny.bin").exists());
        assert!(!root.path().join("tiny.bin.part").exists());
    }
}
