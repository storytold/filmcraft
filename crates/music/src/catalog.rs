//! The built-in providers: what they are, whether they are ready, and how to construct them.

use std::collections::HashMap;
use std::path::Path;

use crate::{Capability, MusicError, Provider, ProviderInfo, hosted, models};

/// Native Stable Audio 3 (small music) running inside FilmCraft.
pub const STABLE_AUDIO_LOCAL: &str = "stable-audio-local";
/// Stability AI's hosted Stable Audio (any size, including the large model).
pub const STABLE_AUDIO_HOSTED: &str = "stability-api";
/// Any other service with an official audio-generation API.
pub const AUDIO_API: &str = "audio-api";

/// Model folder of the local provider inside the models directory.
pub const LOCAL_MODEL_ID: &str = "sa3-small-music";

const LOCAL_CAPS: &[Capability] = &[Capability::Instrumental, Capability::Duration, Capability::Seed, Capability::LocalInference];

pub fn stable_audio_local_info(models_dir: Option<&Path>) -> ProviderInfo {
    ProviderInfo {
        id: STABLE_AUDIO_LOCAL,
        name: "Stable Audio 3 (on this computer)",
        kind: "local",
        capabilities: LOCAL_CAPS,
        license: "Stability AI Community License + Gemma Terms of Use",
        source: "https://huggingface.co/stabilityai/stable-audio-3-small-music",
        description: "Open weights, runs offline on your GPU by default and falls back to the CPU when acceleration is unavailable. Downloads about 2.4 GB on first use after you accept the licences.",
        installed: models_dir.is_some_and(|d| models::is_installed(d, LOCAL_MODEL_ID)),
    }
}

pub fn stable_audio_hosted_info() -> ProviderInfo {
    ProviderInfo {
        id: STABLE_AUDIO_HOSTED,
        name: "Stability AI API",
        kind: "hosted",
        capabilities: hosted::HOSTED_CAPS,
        license: "Stability AI API terms",
        source: "https://platform.stability.ai",
        description: "Hosted Stable Audio. Needs your API key; audio is generated on Stability AI's servers.",
        installed: true,
    }
}

pub fn audio_api_info() -> ProviderInfo {
    ProviderInfo {
        id: AUDIO_API,
        name: "Other audio API",
        kind: "hosted",
        capabilities: hosted::HOSTED_CAPS,
        license: "Per provider",
        source: "",
        description: "Any audio-generation service with an official API that returns WAV: set its endpoint and key.",
        installed: true,
    }
}

#[cfg(feature = "download")]
fn transport() -> Result<Box<dyn hosted::Transport>, MusicError> {
    Ok(Box::new(hosted::UreqTransport))
}

#[cfg(not(feature = "download"))]
fn transport() -> Result<Box<dyn hosted::Transport>, MusicError> {
    Err(MusicError::Unavailable("this build has no network support (feature `download`)".into()))
}

pub fn stable_audio_hosted(cfg: &HashMap<String, String>) -> Result<Box<dyn Provider>, MusicError> {
    Ok(Box::new(hosted::Hosted::stability(cfg, transport()?)?))
}

pub fn audio_api(cfg: &HashMap<String, String>) -> Result<Box<dyn Provider>, MusicError> {
    Ok(Box::new(hosted::Hosted::generic(cfg, transport()?)?))
}

#[cfg_attr(not(feature = "native"), allow(unused_variables))]
pub fn stable_audio_local(models_dir: Option<&Path>, cfg: &HashMap<String, String>) -> Result<Box<dyn Provider>, MusicError> {
    let dir = models_dir.ok_or_else(|| MusicError::Unavailable("no models folder".into()))?;
    let manifest = models::load_manifest(dir, LOCAL_MODEL_ID)?;
    if !models::missing_files(dir, &manifest).is_empty() {
        return Err(MusicError::NotInstalled(manifest.name));
    }
    if !models::licenses_accepted(dir, &manifest) {
        return Err(MusicError::Unavailable("accept the Stable Audio 3 licences first".into()));
    }
    #[cfg(feature = "native")]
    {
        Ok(Box::new(crate::native::Local::load(dir, &manifest, cfg.get("device").map(String::as_str))?))
    }
    #[cfg(not(feature = "native"))]
    {
        Err(MusicError::Unavailable("this build was made without local inference (feature `native`)".into()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_are_distinct_and_listed() {
        let v = crate::providers(None);
        assert_eq!(v.len(), 3);
        let ids: std::collections::HashSet<_> = v.iter().map(|i| i.id).collect();
        assert_eq!(ids.len(), 3);
        assert!(!stable_audio_local_info(None).installed);
        assert!(stable_audio_local_info(None).capabilities.contains(&Capability::LocalInference));
    }

    #[test]
    fn local_provider_reports_why_it_is_not_ready() {
        let none = stable_audio_local(None, &HashMap::new());
        assert!(matches!(none, Err(MusicError::Unavailable(_))));
        let empty = std::env::temp_dir().join(format!("fc-music-cat-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&empty);
        assert!(matches!(stable_audio_local(Some(&empty), &HashMap::new()), Err(MusicError::NotInstalled(_))));
        let _ = std::fs::remove_dir_all(&empty);
    }
}
