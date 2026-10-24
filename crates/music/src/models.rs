//! The on-device model store for the local provider.
//!
//! Weights are never bundled or committed. A model lives in `<models dir>/<id>/` and is described
//! by a `manifest.json` (id, name, licence texts and links, and a pinned URL, SHA-256 and size for
//! every file). The user accepts the licences before anything is downloaded; the accepted licence
//! ids are recorded in `<models dir>/<id>/accepted.json`.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::MusicError;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelFile {
    /// Path relative to the model directory (no `..`, no leading slash).
    pub name: String,
    pub url: String,
    pub sha256: String,
    pub size: u64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct License {
    pub name: String,
    pub url: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Manifest {
    pub id: String,
    pub name: String,
    /// Longest cue the model generates, in seconds.
    pub max_seconds: f64,
    /// Latent frames the model's `sample_size` covers times 4096.
    pub max_samples: usize,
    pub licenses: Vec<License>,
    /// Notice text that must travel with the weights (Stability AI attribution).
    pub notice: String,
    pub files: Vec<ModelFile>,
}

impl Manifest {
    pub fn total_size(&self) -> u64 {
        self.files.iter().fold(0u64, |a, f| a.saturating_add(f.size))
    }

    /// Reject manifests that could write outside the model directory.
    pub fn validate(&self) -> Result<(), MusicError> {
        if self.id.is_empty() || self.id.contains(['/', '\\']) || self.id.starts_with('.') {
            return Err(MusicError::Config(format!("bad model id `{}`", self.id)));
        }
        for f in &self.files {
            let p = Path::new(&f.name);
            let bad = f.name.is_empty() || p.is_absolute() || p.components().any(|c| !matches!(c, std::path::Component::Normal(_)));
            if bad {
                return Err(MusicError::Config(format!("unsafe file name `{}` in manifest", f.name)));
            }
            if !f.url.starts_with("https://") {
                return Err(MusicError::Config(format!("`{}` must be downloaded over https", f.name)));
            }
            if f.sha256.len() != 64 || !f.sha256.bytes().all(|b| b.is_ascii_hexdigit()) {
                return Err(MusicError::Config(format!("`{}` has no valid SHA-256", f.name)));
            }
        }
        Ok(())
    }
}

pub fn model_dir(models_dir: &Path, id: &str) -> PathBuf {
    models_dir.join(id)
}

pub fn load_manifest(models_dir: &Path, id: &str) -> Result<Manifest, MusicError> {
    let path = model_dir(models_dir, id).join("manifest.json");
    let bytes = std::fs::read(&path).map_err(|_| MusicError::NotInstalled(id.into()))?;
    let m: Manifest = serde_json::from_slice(&bytes).map_err(|e| MusicError::Config(format!("manifest.json: {e}")))?;
    m.validate()?;
    if m.id != id {
        return Err(MusicError::Config(format!("manifest id `{}` does not match folder `{id}`", m.id)));
    }
    Ok(m)
}

/// Files of the manifest that are absent or have the wrong size.
pub fn missing_files(models_dir: &Path, m: &Manifest) -> Vec<String> {
    let dir = model_dir(models_dir, &m.id);
    m.files.iter().filter(|f| std::fs::metadata(dir.join(&f.name)).map(|md| md.len() != f.size).unwrap_or(true)).map(|f| f.name.clone()).collect()
}

pub fn is_installed(models_dir: &Path, id: &str) -> bool {
    load_manifest(models_dir, id).map(|m| missing_files(models_dir, &m).is_empty()).unwrap_or(false)
}

fn accepted_path(models_dir: &Path, id: &str) -> PathBuf {
    model_dir(models_dir, id).join("accepted.json")
}

/// Record that the user accepted the licences of the manifest (names, in order).
pub fn record_acceptance(models_dir: &Path, m: &Manifest, now_unix: u64) -> Result<(), MusicError> {
    let dir = model_dir(models_dir, &m.id);
    std::fs::create_dir_all(&dir).map_err(|e| MusicError::Config(format!("create {}: {e}", dir.display())))?;
    let v = serde_json::json!({ "licenses": m.licenses.iter().map(|l| &l.name).collect::<Vec<_>>(), "acceptedAtUnix": now_unix });
    std::fs::write(accepted_path(models_dir, &m.id), v.to_string()).map_err(|e| MusicError::Config(format!("write acceptance: {e}")))
}

pub fn licenses_accepted(models_dir: &Path, m: &Manifest) -> bool {
    let Ok(bytes) = std::fs::read(accepted_path(models_dir, &m.id)) else { return false };
    let Ok(v) = serde_json::from_slice::<serde_json::Value>(&bytes) else { return false };
    let accepted: Vec<&str> = v["licenses"].as_array().map(|a| a.iter().filter_map(|s| s.as_str()).collect()).unwrap_or_default();
    m.licenses.iter().all(|l| accepted.contains(&l.name.as_str()))
}

/// Download one manifest file into the model directory: streamed to `<name>.part`, checked against
/// the pinned size and SHA-256, then moved into place. `progress` gets the bytes done so far and
/// returns `false` to cancel.
#[cfg(feature = "download")]
pub fn download_file(models_dir: &Path, m: &Manifest, f: &ModelFile, progress: &mut dyn FnMut(u64) -> bool) -> Result<(), MusicError> {
    use sha2::{Digest, Sha256};
    use std::io::{Read, Write};

    m.validate()?;
    let dest = model_dir(models_dir, &m.id).join(&f.name);
    let part = dest.with_extension("part");
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent).map_err(|e| MusicError::Download(format!("create {}: {e}", parent.display())))?;
    }
    let agent: ureq::Agent = ureq::Agent::config_builder().timeout_global(Some(std::time::Duration::from_secs(60 * 60))).build().into();
    let resp = agent.get(&f.url).call().map_err(|e| MusicError::Download(format!("{}: {e}", f.name)))?;
    let mut body = resp.into_body();
    let mut reader = body.as_reader();
    let mut out = std::fs::File::create(&part).map_err(|e| MusicError::Download(format!("write {}: {e}", part.display())))?;
    let mut hash = Sha256::new();
    let mut buf = vec![0u8; 1 << 20];
    let mut done = 0u64;
    loop {
        let n = reader.read(&mut buf).map_err(|e| MusicError::Download(format!("{}: {e}", f.name)))?;
        if n == 0 {
            break;
        }
        done = done.saturating_add(n as u64);
        if done > f.size {
            let _ = std::fs::remove_file(&part);
            return Err(MusicError::Download(format!("{} is larger than the manifest says", f.name)));
        }
        let chunk = buf.get(..n).unwrap_or(&[]);
        hash.update(chunk);
        out.write_all(chunk).map_err(|e| MusicError::Download(format!("write {}: {e}", part.display())))?;
        if !progress(done) {
            let _ = std::fs::remove_file(&part);
            return Err(MusicError::Download("cancelled".into()));
        }
    }
    drop(out);
    let digest: String = hash.finalize().iter().map(|b| format!("{b:02x}")).collect();
    if done != f.size || !digest.eq_ignore_ascii_case(&f.sha256) {
        let _ = std::fs::remove_file(&part);
        return Err(MusicError::Download(format!("{} failed its SHA-256 / size check", f.name)));
    }
    std::fs::rename(&part, &dest).map_err(|e| MusicError::Download(format!("install {}: {e}", f.name)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn manifest() -> Manifest {
        Manifest {
            id: "sa3-small-music".into(),
            name: "Stable Audio 3 Small Music".into(),
            max_seconds: 120.0,
            max_samples: 5_292_032,
            licenses: vec![
                License { name: "Stability AI Community License".into(), url: "https://stability.ai/license".into() },
                License { name: "Gemma Terms of Use".into(), url: "https://ai.google.dev/gemma/terms".into() },
            ],
            notice: "This Stability AI Model is licensed under the Stability AI Community License, Copyright © Stability AI Ltd. All Rights Reserved".into(),
            files: vec![ModelFile { name: "dit/dit.onnx".into(), url: "https://example.org/dit.onnx".into(), sha256: "a".repeat(64), size: 10 }],
        }
    }

    fn tmp(name: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!("fc-music-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&p);
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    #[test]
    fn manifest_rejects_path_traversal_http_and_bad_hashes() {
        let mut m = manifest();
        assert!(m.validate().is_ok());
        m.files[0].name = "../escape".into();
        assert!(m.validate().is_err());
        m.files[0].name = "/abs".into();
        assert!(m.validate().is_err());
        m.files[0].name = "ok".into();
        m.files[0].url = "http://insecure".into();
        assert!(m.validate().is_err());
        m.files[0].url = "https://x".into();
        m.files[0].sha256 = "xyz".into();
        assert!(m.validate().is_err());
        m.id = "../x".into();
        assert!(m.validate().is_err());
    }

    #[test]
    fn install_state_follows_files_on_disk() {
        let root = tmp("install");
        let m = manifest();
        assert!(!is_installed(&root, &m.id));
        let dir = model_dir(&root, &m.id);
        std::fs::create_dir_all(dir.join("dit")).unwrap();
        std::fs::write(dir.join("manifest.json"), serde_json::to_vec(&m).unwrap()).unwrap();
        assert_eq!(missing_files(&root, &m), vec!["dit/dit.onnx".to_string()]);
        std::fs::write(dir.join("dit/dit.onnx"), b"0123456789").unwrap();
        assert!(is_installed(&root, &m.id));
        std::fs::write(dir.join("dit/dit.onnx"), b"short").unwrap();
        assert!(!is_installed(&root, &m.id));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn licence_acceptance_is_recorded_per_licence() {
        let root = tmp("accept");
        let mut m = manifest();
        assert!(!licenses_accepted(&root, &m));
        record_acceptance(&root, &m, 1_700_000_000).unwrap();
        assert!(licenses_accepted(&root, &m));
        m.licenses.push(License { name: "New terms".into(), url: "https://x".into() });
        assert!(!licenses_accepted(&root, &m));
        let _ = std::fs::remove_dir_all(&root);
    }
}
