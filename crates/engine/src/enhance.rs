//! Models of the **Enhance Speech (Neural)** audio effect (`neural_enhance`; `filmcraft-enhance`).
//!
//! The effect is an ordinary audio effect: it is applied, keyframed and rendered like the others.
//! What it needs besides is a downloaded model, and these commands manage them:
//!
//! | Command | |
//! |---|---|
//! | `enhance.models` | the catalogue (name, description, size, licence, source, installed) |
//! | `enhance.downloadModel` | download a model (SHA-256 checked) as a background job |
//! | `enhance.removeModel` | delete a downloaded model |
//!
//! Models live in `<data dir>/models/<id>/`, next to the speech-to-text and voice models. They are
//! never bundled. Hosts show the size, source and licence (`enhance.models`) and ask before
//! downloading. While a model is missing, or in a build without the `neural-enhance` feature, the
//! effect passes audio through unchanged, so a project that uses it opens and plays everywhere.

use serde_json::{Value, json};

use filmcraft_enhance::catalog;

use crate::commands::{CommandSpec, always, bad, str_p};
use crate::narration::{finish, new_job, spawn};
use crate::{EngineError, Result, Session};

/// Point the effect at `<data dir>/models`. Called when a session starts.
pub(crate) fn init() {
    catalog::set_models_dir(crate::transcript::models_dir());
}

/// Whether this build can run the models (feature `neural-enhance`).
pub fn available() -> bool {
    filmcraft_enhance::available()
}

fn model_json(dir: Option<&std::path::Path>, m: &catalog::ModelInfo, index: usize) -> Value {
    json!({
        "id": m.id,
        "index": index,
        "name": m.name,
        "description": m.description,
        "license": m.license,
        "licenseUrl": m.license_url,
        "author": m.author,
        "source": m.source,
        "sampleRate": m.sample_rate,
        "size": m.size(),
        "missingBytes": dir.map_or(m.size(), |d| catalog::missing_bytes(d, m)),
        "installed": dir.is_some_and(|d| catalog::installed(d, m)),
    })
}

fn models(_: &mut Session, _: &Value) -> Result<Value> {
    let dir = crate::transcript::models_dir();
    Ok(json!({
        "available": available(),
        "canDownload": cfg!(feature = "speech-download"),
        "default": catalog::DEFAULT_MODEL,
        "effect": "neural_enhance",
        "dir": dir.as_ref().map(|d| d.to_string_lossy().to_string()),
        "models": catalog::catalogue().iter().enumerate().map(|(i, m)| model_json(dir.as_deref(), m, i)).collect::<Vec<_>>(),
    }))
}

fn find(cmd: &str, p: &Value) -> Result<&'static catalog::ModelInfo> {
    let id = str_p(p, "model").unwrap_or(catalog::DEFAULT_MODEL);
    catalog::find(id).ok_or_else(|| bad(cmd, format!("unknown speech-enhancement model `{id}` (see enhance.models)")))
}

/// Download a catalogue model in the background (feature `speech-download`).
fn download_model(s: &mut Session, p: &Value) -> Result<Value> {
    let cmd = "enhance.downloadModel";
    let m = find(cmd, p)?;
    let dir = crate::transcript::models_dir().ok_or_else(|| bad(cmd, "no data directory for models"))?;
    if catalog::installed(&dir, m) {
        return Ok(json!({"job": null, "model": m.id, "installed": true}));
    }
    if !cfg!(feature = "speech-download") {
        return Err(bad(cmd, "model downloads are not available in this build (built without the `speech-download` feature)"));
    }
    let wait = p.get("wait").and_then(Value::as_bool).unwrap_or(false);
    let job = new_job(s, format!("Downloading {}", m.name));
    job.progress.total.store(catalog::missing_bytes(&dir, m), std::sync::atomic::Ordering::Relaxed);
    let (prog, res) = (job.progress.clone(), job.result.clone());
    let id = spawn(s, job, wait, "filmcraft-enhance-download", move || {
        let t0 = web_time::Instant::now();
        let r = download(&dir, m, &prog);
        catalog::store_changed();
        finish(&prog, &res, r.map(|()| format!("{} installed", m.name)), t0);
    })?;
    Ok(json!({"job": id, "model": m.id, "installed": false}))
}

#[cfg(feature = "speech-download")]
fn download(dir: &std::path::Path, m: &catalog::ModelInfo, prog: &filmcraft_export::Progress) -> std::result::Result<(), String> {
    use std::sync::PoisonError;
    use std::sync::atomic::Ordering;
    let files: Vec<filmcraft_speech::models::ModelFile> =
        m.files.iter().map(|f| filmcraft_speech::models::ModelFile { name: f.name, url: f.url, sha256: f.sha256, size: f.size }).collect();
    let mut progress = |done: u64, total: u64, file: &str| {
        prog.done.store(done, Ordering::Relaxed);
        prog.total.store(total, Ordering::Relaxed);
        *prog.status.lock().unwrap_or_else(PoisonError::into_inner) = format!("Downloading {file}");
        !prog.cancel.load(Ordering::Relaxed)
    };
    filmcraft_speech::models::download_files(&catalog::model_dir(dir, m), &files, &mut progress).map_err(|e| e.to_string())
}

#[cfg(not(feature = "speech-download"))]
fn download(_: &std::path::Path, _: &catalog::ModelInfo, _: &filmcraft_export::Progress) -> std::result::Result<(), String> {
    Err("model downloads are not available in this build".into())
}

fn remove_model(_: &mut Session, p: &Value) -> Result<Value> {
    let cmd = "enhance.removeModel";
    let id = str_p(p, "model").ok_or_else(|| bad(cmd, "missing `model`"))?;
    let m = catalog::find(id).ok_or_else(|| bad(cmd, format!("unknown speech-enhancement model `{id}` (see enhance.models)")))?;
    let dir = crate::transcript::models_dir().ok_or_else(|| bad(cmd, "no data directory for models"))?;
    let was = catalog::installed(&dir, m);
    catalog::remove(&dir, m).map_err(|e| EngineError::Other(format!("could not remove {}: {e}", m.name)))?;
    Ok(json!({"model": m.id, "removed": was}))
}

fn spec(id: &'static str, label: &'static str, params: &'static str, run: fn(&mut Session, &Value) -> Result<Value>) -> CommandSpec {
    CommandSpec { id, label, menu: &[], shortcut: None, params, enabled: always, run, journal: false, named: crate::commands::NamedKeys::NONE }
}

pub fn commands() -> Vec<CommandSpec> {
    vec![
        spec("enhance.models", "List Speech Enhancement Models", "{}", models),
        spec("enhance.downloadModel", "Download Speech Enhancement Model", r#"{"model":"deepfilternet3"?,"wait":bool?}"#, download_model),
        spec("enhance.removeModel", "Remove Speech Enhancement Model", r#"{"model":str}"#, remove_model),
    ]
}
