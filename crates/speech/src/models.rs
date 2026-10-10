//! The speech model catalogue and the model store.
//!
//! Models are downloaded on first use, after the user confirms (the dialog shows the size, the
//! source URL and the licence), into `<data dir>/models/<id>/`. They are never bundled with
//! FilmCraft or committed to the repository. Every file is pinned to a Hugging Face revision of
//! the publisher's own repository (OpenAI, NVIDIA) and checked against its SHA-256 before it is used.
//!
//! Licence: OpenAI released the Whisper code and model weights under the MIT licence
//! (<https://github.com/openai/whisper/blob/main/LICENSE>); the safetensors conversions OpenAI
//! publishes at `huggingface.co/openai/whisper-*` are labelled Apache-2.0. Both are permissive.
//! NVIDIA's Parakeet TDT weights (`huggingface.co/nvidia/parakeet-tdt-*`) are CC-BY-4.0: they are
//! downloaded unmodified, and the credit line ([`ModelInfo::attribution`]) is shown with them.

use std::path::{Path, PathBuf};

use serde::Serialize;

/// One file of a model.
#[derive(Clone, Copy, Debug, Serialize)]
pub struct ModelFile {
    pub name: &'static str,
    pub url: &'static str,
    pub sha256: &'static str,
    pub size: u64,
}

/// A downloadable speech model.
#[derive(Clone, Copy, Debug, Serialize)]
pub struct ModelInfo {
    pub id: &'static str,
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

    /// The recogniser that runs this model.
    pub fn engine(&self) -> Engine {
        if self.id.starts_with("parakeet-") { Engine::Parakeet } else { Engine::Whisper }
    }

    /// The credit line to show with the model (download dialog, About): name, author, licence and
    /// source. CC-BY models require it.
    pub fn attribution(&self) -> String {
        format!("{} by {}, {} ({}), from {}; used unmodified.", self.name, self.author, self.license, self.license_url, self.source)
    }
}

/// Which recogniser runs a catalogue model.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub enum Engine {
    /// [`crate::whisper`] (feature `whisper`).
    Whisper,
    /// [`crate::parakeet`] (feature `parakeet`).
    Parakeet,
}

const LICENSE: &str = "MIT (OpenAI Whisper weights); Hugging Face conversion: Apache-2.0";
const LICENSE_URL: &str = "https://github.com/openai/whisper/blob/main/LICENSE";
const AUTHOR: &str = "OpenAI";

macro_rules! hf {
    ($repo:literal, $rev:literal, $name:literal, $sha:literal, $size:expr) => {
        ModelFile { name: $name, url: concat!("https://huggingface.co/openai/", $repo, "/resolve/", $rev, "/", $name), sha256: $sha, size: $size }
    };
}

const TOKENIZER_MULTI: &str = "27fc476bfe7f17299480be2273fc0608e4d5a99aba2ab5dec5374b4482d1a566";

static CATALOGUE: &[ModelInfo] = &[
    ModelInfo {
        id: "whisper-tiny",
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
    // NVIDIA Parakeet TDT: one `.nemo` archive each (config, weights, tokenizer), CC-BY-4.0.
    ModelInfo {
        id: "parakeet-tdt-0.6b-v3",
        name: "Parakeet TDT 0.6B v3 (25 European languages)",
        multilingual: true,
        description: "600 M parameters (NVIDIA FastConformer TDT). English, German and 23 more European languages, found automatically. The most accurate model here and faster than Whisper base on the CPU; keeps English filler words (um, uh); word times from the model itself.",
        license: PARAKEET_LICENSE,
        license_url: PARAKEET_LICENSE_URL,
        author: "NVIDIA",
        source: "https://huggingface.co/nvidia/parakeet-tdt-0.6b-v3",
        files: &[ModelFile {
            name: "parakeet-tdt-0.6b-v3.nemo",
            url: "https://huggingface.co/nvidia/parakeet-tdt-0.6b-v3/resolve/541d1f99c6b0c3cd0b11a95167540bb8edefd82b/parakeet-tdt-0.6b-v3.nemo",
            sha256: "3cbdc85877e668ca7b82d0d56770eb1fac76691f55d6b97545e8d61ca588d10d",
            size: 2_509_332_480,
        }],
    },
    ModelInfo {
        id: "parakeet-tdt-0.6b-v2",
        name: "Parakeet TDT 0.6B v2 (English)",
        multilingual: false,
        description: "600 M parameters (NVIDIA FastConformer TDT), English only. Very accurate on English, with punctuation and capitals; as fast as v3.",
        license: PARAKEET_LICENSE,
        license_url: PARAKEET_LICENSE_URL,
        author: "NVIDIA",
        source: "https://huggingface.co/nvidia/parakeet-tdt-0.6b-v2",
        files: &[ModelFile {
            name: "parakeet-tdt-0.6b-v2.nemo",
            url: "https://huggingface.co/nvidia/parakeet-tdt-0.6b-v2/resolve/ae9ad07059c7c739ffaf932226a8fe64ae2620b0/parakeet-tdt-0.6b-v2.nemo",
            sha256: "d99e39955c9d3d0350d8fb7c75e40c64a2b2eaeb003883d7c941fd2e8747b28c",
            size: 2_472_222_720,
        }],
    },
];

const PARAKEET_LICENSE: &str = "CC-BY-4.0 (NVIDIA Parakeet TDT weights)";
const PARAKEET_LICENSE_URL: &str = "https://creativecommons.org/licenses/by/4.0/";

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

/// All files present with their catalogue sizes (contents were verified when downloaded).
pub fn installed(models_dir: &Path, m: &ModelInfo) -> bool {
    let d = model_dir(models_dir, m);
    m.files.iter().all(|f| std::fs::metadata(d.join(f.name)).is_ok_and(|md| md.len() == f.size))
}

/// Bytes still to download for `m` (files already present are skipped).
pub fn missing_bytes(models_dir: &Path, m: &ModelInfo) -> u64 {
    let d = model_dir(models_dir, m);
    m.files.iter().filter(|f| !std::fs::metadata(d.join(f.name)).is_ok_and(|md| md.len() == f.size)).map(|f| f.size).sum()
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
    use crate::SpeechError;
    use sha2::Digest;
    use std::io::{Read, Write};
    let dir = model_dir(models_dir, m);
    std::fs::create_dir_all(&dir)?;
    let total = missing_bytes(models_dir, m);
    let agent = agent();
    let mut done = 0u64;
    for f in m.files {
        let dest = dir.join(f.name);
        if std::fs::metadata(&dest).is_ok_and(|md| md.len() == f.size) {
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
    fn catalogue_is_pinned_and_checksummed() {
        assert!(find(DEFAULT_MODEL).is_some());
        for m in catalogue() {
            assert!(m.size() > 100_000_000, "{}", m.id);
            let repo = match m.engine() {
                Engine::Whisper => "https://huggingface.co/openai/whisper-",
                Engine::Parakeet => "https://huggingface.co/nvidia/parakeet-",
            };
            assert!(m.attribution().contains(m.license_url));
            for f in m.files {
                assert!(f.url.starts_with(repo), "{}", f.url);
                assert!(f.url.contains("/resolve/") && f.url.ends_with(f.name));
                assert_eq!(f.sha256.len(), 64);
                assert!(f.sha256.bytes().all(|b| b.is_ascii_hexdigit()));
                if f.name == "tokenizer.json" {
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
}
