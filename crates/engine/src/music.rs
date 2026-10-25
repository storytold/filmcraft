//! Music generation: describe a cue, pick a provider, and get an audio clip typed as Music.
//!
//! | command | does |
//! |---|---|
//! | `music.providers` | the providers (on-device Stable Audio 3, Stability AI, other audio APIs), whether each is ready, and the licences to accept |
//! | `music.render` | generate in a background job into the cache (progress, cancel), so `music.create` returns at once |
//! | `music.create` | generate (or take the cached render), write `Music <n>.wav` plus a provenance sidecar, import it, place it and mark it Music (one undo step) |
//! | `music.inspect` | the provenance of a generated clip: provider, model, prompt, seed, licence |
//! | `music.acceptLicenses` | record that the user accepted the model licences |
//! | `music.download` | download the on-device model as a background job (after the licences are accepted) |
//!
//! **Where a cue goes.** At `time` (default: the playhead), or filling `fill: "inout"` (the sequence
//! In to Out range sets the duration), on `track` if given (it must be free), else the first
//! targeted audio track that is free, else the first free audio track, else a new audio track.
//!
//! **Providers** live in the `filmcraft-music` crate behind one interface. Settings (provider,
//! endpoints, API keys) are preferences (`music.*`), written to `preferences.json`, never to the
//! project file; `prefs.get` hides keys. Every generated file gets `<file>.music-provenance.json`
//! (provider, model, prompt, seed, licence, date) so licensing can be audited.

use std::collections::HashMap;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex, PoisonError};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use filmcraft_music as music;
use filmcraft_project::{ClipId, ItemId, ItemKind, MediaRef, TrackId, TrackKind};
use filmcraft_time::{Tick, TimeRange};

use crate::commands::{CommandSpec, always, bad, has_seq, str_p, time_p, u64_p};
use crate::narration::{finish, new_job, spawn, track_is_free};
use crate::{EngineError, Result, Session};

const BASE_NAME: &str = "Music";

/// Settings ▸ Music (`music.*`). Keys are secrets: they are stored in `preferences.json` only and
/// `prefs.get` reports them as set or empty.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct MusicPrefs {
    /// Default provider id.
    pub provider: String,
    /// `auto` | `cpu` | `coreml` | `cuda`: where the on-device model runs.
    pub device: String,
    pub api: MusicApiPrefs,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct MusicApiPrefs {
    pub stability_api_key: String,
    /// The Stability AI audio generation URL (from their API reference).
    pub stability_endpoint: String,
    pub other_endpoint: String,
    pub other_api_key: String,
    pub other_model: String,
}

impl Default for MusicPrefs {
    fn default() -> Self {
        Self { provider: music::catalog::STABLE_AUDIO_LOCAL.into(), device: "auto".into(), api: MusicApiPrefs::default() }
    }
}

/// Hide secrets in a preferences value that leaves the process (`prefs.get`).
pub fn redact(v: &mut Value) {
    if let Some(api) = v.pointer_mut("/music/api").and_then(Value::as_object_mut) {
        for k in ["stabilityApiKey", "otherApiKey"] {
            if let Some(slot) = api.get_mut(k) {
                let set = slot.as_str().is_some_and(|s| !s.is_empty());
                *slot = json!(if set { "(set)" } else { "" });
            }
        }
    }
}

/// One preference, with API keys hidden.
pub fn redacted_get(prefs: &crate::autosave::Preferences, key: &str) -> Option<Value> {
    let mut all = prefs.to_value();
    redact(&mut all);
    key.split('.').try_fold(&all, |v, k| v.get(k)).cloned()
}

/// Where providers come from. The default asks `filmcraft-music`; tests supply a fake.
pub trait MusicSource: Send + Sync {
    fn provider(
        &self,
        id: &str,
        models_dir: Option<&std::path::Path>,
        cfg: &HashMap<String, String>,
    ) -> std::result::Result<Box<dyn music::Provider>, music::MusicError>;
}

pub struct RealSource;

impl MusicSource for RealSource {
    fn provider(
        &self,
        id: &str,
        models_dir: Option<&std::path::Path>,
        cfg: &HashMap<String, String>,
    ) -> std::result::Result<Box<dyn music::Provider>, music::MusicError> {
        music::provider(id, models_dir, cfg)
    }
}

/// Rendered cues by settings, newest last.
pub type MusicCache = Arc<Mutex<Vec<(String, Arc<music::Audio>)>>>;
const CACHE_ENTRIES: usize = 3;

/// A cue request.
#[derive(Clone, Debug)]
struct Request {
    provider: String,
    params: music::Params,
}

fn err(cmd: &str, e: music::MusicError) -> EngineError {
    bad(cmd, e.to_string())
}

fn f64_p(p: &Value, k: &str) -> Option<f64> {
    p.get(k).and_then(Value::as_f64)
}

fn now_unix() -> u64 {
    web_time::SystemTime::now().duration_since(web_time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

/// Provider settings the music crate reads, from preferences.
fn provider_config(s: &Session, id: &str) -> HashMap<String, String> {
    let a = &s.prefs.music.api;
    let mut c = HashMap::new();
    match id {
        music::catalog::STABLE_AUDIO_LOCAL => {
            c.insert("device".into(), s.prefs.music.device.clone());
        }
        music::catalog::STABLE_AUDIO_HOSTED => {
            c.insert("apiKey".into(), a.stability_api_key.clone());
            c.insert("endpoint".into(), a.stability_endpoint.clone());
        }
        music::catalog::AUDIO_API => {
            c.insert("apiKey".into(), a.other_api_key.clone());
            c.insert("endpoint".into(), a.other_endpoint.clone());
            c.insert("model".into(), a.other_model.clone());
        }
        _ => {}
    }
    c
}

#[cfg(test)]
mod device_tests {
    use super::*;

    #[test]
    fn local_device_preference_reaches_the_provider() {
        let mut session = Session::default();
        session.prefs.music.device = "cpu".into();
        let config = provider_config(&session, music::catalog::STABLE_AUDIO_LOCAL);
        assert_eq!(config.get("device").map(String::as_str), Some("cpu"));
    }
}

fn models_dir() -> Option<std::path::PathBuf> {
    crate::transcript::models_dir()
}

fn request(s: &Session, cmd: &str, p: &Value) -> Result<Request> {
    for key in ["prompt", "negativePrompt", "key", "provider"] {
        if p.get(key).is_some_and(|v| !v.is_string()) {
            return Err(bad(cmd, format!("`{key}` must be a string")));
        }
    }
    let mut params = music::Params { prompt: str_p(p, "prompt").unwrap_or("").to_string(), ..Default::default() };
    if let Some(n) = str_p(p, "negativePrompt") {
        params.negative_prompt = n.to_string();
    }
    if let Some(d) = f64_p(p, "duration") {
        params.duration = d;
    } else if p.get("duration").is_some() {
        return Err(bad(cmd, "`duration` must be a number of seconds"));
    } else if p.get("fill").and_then(Value::as_str) == Some("inout") {
        params.duration = crate::commands::in_out_range(s)?.duration.seconds();
    } else {
        return Err(bad(cmd, "pass `duration` (seconds) or `fill: \"inout\"`"));
    }
    if let Some(b) = p.get("bpm") {
        params.bpm = Some(b.as_f64().ok_or_else(|| bad(cmd, "`bpm` must be a number"))?);
    }
    params.key = str_p(p, "key").map(str::to_string);
    if let Some(i) = p.get("instrumental") {
        params.instrumental = i.as_bool().ok_or_else(|| bad(cmd, "`instrumental` must be true or false"))?;
    }
    if let Some(sd) = p.get("seed") {
        params.seed = Some(sd.as_u64().ok_or_else(|| bad(cmd, "`seed` must be a non-negative integer"))?);
    }
    params.check().map_err(|e| err(cmd, e))?;
    let provider = str_p(p, "provider").map(str::to_string).unwrap_or_else(|| s.prefs.music.provider.clone());
    if !music::providers(None).iter().any(|i| i.id == provider) {
        return Err(bad(cmd, format!("unknown provider `{provider}` (see music.providers)")));
    }
    Ok(Request { provider, params })
}

/// Fix the seed so a render and the create that follows agree on the cache key.
fn with_seed(mut r: Request) -> Request {
    if r.params.seed.is_none() {
        r.params.seed = Some(now_unix().wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ u64::from(std::process::id()));
    }
    r
}

fn cache_key(r: &Request) -> String {
    let p = &r.params;
    format!(
        "{}\u{1f}{}\u{1f}{}\u{1f}{}\u{1f}{:?}\u{1f}{:?}\u{1f}{}\u{1f}{:?}",
        r.provider,
        p.prompt,
        p.negative_prompt,
        p.duration.to_bits(),
        p.bpm.map(f64::to_bits),
        p.key,
        p.instrumental,
        p.seed
    )
}

fn cache_put(c: &MusicCache, key: String, a: Arc<music::Audio>) {
    let mut c = c.lock().unwrap_or_else(PoisonError::into_inner);
    c.retain(|(k, _)| *k != key);
    c.push((key, a));
    let extra = c.len().saturating_sub(CACHE_ENTRIES);
    c.drain(..extra);
}

fn cached(c: &MusicCache, key: &str) -> Option<Arc<music::Audio>> {
    c.lock().unwrap_or_else(PoisonError::into_inner).iter().find(|(k, _)| k == key).map(|(_, a)| a.clone())
}

/// Everything a worker needs to run a request without touching the session.
#[derive(Clone)]
struct Worker {
    source: Arc<dyn MusicSource>,
    models: Option<std::path::PathBuf>,
    cfg: HashMap<String, String>,
}

impl Worker {
    fn of(s: &Session, provider: &str) -> Worker {
        Worker { source: s.music_source.clone(), models: models_dir(), cfg: provider_config(s, provider) }
    }

    fn run(&self, r: &Request, progress: &mut dyn FnMut(f32) -> bool) -> std::result::Result<(music::ProviderInfo, music::Audio), music::MusicError> {
        let provider = self.source.provider(&r.provider, self.models.as_deref(), &self.cfg)?;
        let audio = provider.generate_with_progress(&r.params, progress)?;
        Ok((provider.info(), audio))
    }
}

fn model_name(provider: &str) -> &'static str {
    match provider {
        music::catalog::STABLE_AUDIO_LOCAL => "stable-audio-3-small-music",
        music::catalog::STABLE_AUDIO_HOSTED => "stable-audio (Stability AI API)",
        _ => "provider default",
    }
}

fn providers(s: &mut Session, _p: &Value) -> Result<Value> {
    let dir = models_dir();
    let list: Vec<Value> = music::providers(dir.as_deref())
        .into_iter()
        .map(|i| {
            let ready = match i.id {
                music::catalog::STABLE_AUDIO_LOCAL => {
                    i.installed
                        && dir
                            .as_deref()
                            .and_then(|d| music::models::load_manifest(d, music::catalog::LOCAL_MODEL_ID).ok())
                            .is_some_and(|m| music::models::licenses_accepted(dir.as_deref().unwrap_or(std::path::Path::new("")), &m))
                }
                id => s.music_source.provider(id, dir.as_deref(), &provider_config(s, id)).is_ok(),
            };
            let mut v = serde_json::to_value(&i).unwrap_or_default();
            v["ready"] = json!(ready);
            v
        })
        .collect();
    let model = dir.as_deref().and_then(|d| music::models::load_manifest(d, music::catalog::LOCAL_MODEL_ID).ok());
    Ok(json!({
        "providers": list,
        "default": s.prefs.music.provider,
        "model": model.as_ref().map(|m| json!({
            "id": m.id, "name": m.name, "size": m.total_size(),
            "licenses": m.licenses, "notice": m.notice,
            "accepted": dir.as_deref().is_some_and(|d| music::models::licenses_accepted(d, m)),
            "missingFiles": dir.as_deref().map(|d| music::models::missing_files(d, m).len()),
        })),
        "maxSeconds": music::MAX_SECONDS,
        "poweredBy": "Powered by Stability AI",
    }))
}

fn accept_licenses(_s: &mut Session, _p: &Value) -> Result<Value> {
    let cmd = "music.acceptLicenses";
    let dir = models_dir().ok_or_else(|| bad(cmd, "no data directory for models"))?;
    let m = music::models::load_manifest(&dir, music::catalog::LOCAL_MODEL_ID).map_err(|e| err(cmd, e))?;
    music::models::record_acceptance(&dir, &m, now_unix()).map_err(|e| err(cmd, e))?;
    Ok(json!({"accepted": m.licenses.iter().map(|l| &l.name).collect::<Vec<_>>()}))
}

/// Generate in the background into the cache.
fn render_job(s: &mut Session, p: &Value) -> Result<Value> {
    let cmd = "music.render";
    let r = with_seed(request(s, cmd, p)?);
    let key = cache_key(&r);
    if cached(&s.music_cache, &key).is_some() {
        return Ok(json!({"job": null, "cached": true, "seed": r.params.seed}));
    }
    let job = new_job(s, format!("Generating music: {}", short_label(&r.params.prompt)));
    job.progress.total.store(1000, Ordering::Relaxed);
    let (prog, res, cache, worker) = (job.progress.clone(), job.result.clone(), s.music_cache.clone(), Worker::of(s, &r.provider));
    let wait = p.get("wait").and_then(Value::as_bool).unwrap_or(false);
    let seed = r.params.seed;
    let id = spawn(s, job, wait, "filmcraft-music", move || {
        let t0 = web_time::Instant::now();
        *prog.status.lock().unwrap_or_else(PoisonError::into_inner) = "Generating…".into();
        let mut step = |f: f32| {
            prog.done.store((f.clamp(0.0, 1.0) * 1000.0) as u64, Ordering::Relaxed);
            !prog.cancel.load(Ordering::Relaxed)
        };
        let out = match worker.run(&r, &mut step) {
            Ok((_, audio)) => {
                let secs = audio.seconds();
                cache_put(&cache, key, Arc::new(audio));
                prog.done.store(1000, Ordering::Relaxed);
                Ok(format!("{secs:.1} s of music"))
            }
            Err(e) => Err(e.to_string()),
        };
        finish(&prog, &res, out, t0);
    })?;
    Ok(json!({"job": id, "cached": false, "seed": seed}))
}

fn short_label(prompt: &str) -> String {
    let t: String = prompt.trim().chars().take(40).collect();
    if prompt.trim().chars().count() > 40 { format!("{t}…") } else { t }
}

fn media_dir_of(s: &Session, item: ItemId) -> Option<String> {
    match &s.project.items.get(&item)?.kind {
        ItemKind::Media(m) => match &m.media {
            MediaRef::File { path } => Some(path.clone()),
            MediaRef::Generator(_) => None,
        },
        _ => None,
    }
}

fn placement(s: &Session, cmd: &str, p: &Value, range: TimeRange) -> Result<Option<TrackId>> {
    let seq = s.active_sequence().ok_or(EngineError::NoSequence)?;
    if let Some(v) = p.get("track") {
        let id =
            crate::mixer::strip_ref(seq, v).filter(|id| seq.audio_tracks.iter().any(|t| t.id == *id)).ok_or_else(|| bad(cmd, format!("no audio track {v}")))?;
        if !track_is_free(s, id, range) {
            return Err(bad(cmd, format!("audio track {v} is locked or has clips there")));
        }
        return Ok(Some(id));
    }
    let targeted = s.targeting().targeted;
    let order = seq.audio_tracks.iter().filter(|t| targeted.contains(&t.id)).chain(seq.audio_tracks.iter().filter(|t| !targeted.contains(&t.id)));
    Ok(order.map(|t| t.id).find(|id| track_is_free(s, *id, range)))
}

fn create(s: &mut Session, p: &Value) -> Result<Value> {
    let cmd = "music.create";
    let seq_id = s.state.active_sequence.ok_or(EngineError::NoSequence)?;
    let r = with_seed(request(s, cmd, p)?);
    let fill_start = if p.get("fill").and_then(Value::as_str) == Some("inout") { crate::commands::in_out_range(s).ok().map(|r| r.start) } else { None };
    let at = time_p(s, p, "").or(fill_start).unwrap_or(s.playhead());
    if at < Tick::ZERO {
        return Err(bad(cmd, "the start time cannot be negative"));
    }
    let key = cache_key(&r);
    let (info, audio) = match cached(&s.music_cache, &key) {
        Some(a) => {
            let info =
                s.music_source.provider(&r.provider, models_dir().as_deref(), &provider_config(s, &r.provider)).map(|pr| pr.info()).map_err(|e| err(cmd, e))?;
            (info, a)
        }
        None => {
            let (info, a) = Worker::of(s, &r.provider).run(&r, &mut |_| true).map_err(|e| err(cmd, e))?;
            let a = Arc::new(a);
            cache_put(&s.music_cache, key, a.clone());
            (info, a)
        }
    };
    let rate = audio.sample_rate.max(1);
    let frames = audio.samples.len() / usize::from(audio.channels.max(1));
    let dur = Tick::from_units(frames as i64, i64::from(rate));
    if dur <= Tick::ZERO {
        return Err(bad(cmd, "the provider returned no audio"));
    }
    let range = TimeRange::new(at, dur);
    let track = placement(s, cmd, p, range)?;
    let ducking = p.get("ducking").and_then(Value::as_bool).unwrap_or(false);
    let n0 = s.history.undo.len();
    let result = (|| -> Result<Value> {
        let track = match track {
            Some(t) => t,
            None => {
                let before: Vec<TrackId> = s.active_sequence().map(|q| q.audio_tracks.iter().map(|t| t.id).collect()).unwrap_or_default();
                s.execute("sequence.addTracks", json!({"video": 0, "audio": 1, "audioAfter": "last"}))?;
                s.active_sequence()
                    .and_then(|q| q.audio_tracks.iter().map(|t| t.id).find(|id| !before.contains(id)))
                    .ok_or_else(|| EngineError::Other("could not add an audio track".into()))?
            }
        };
        let dir = crate::voiceover::media_dir(s, p, "Music");
        let path = crate::voiceover::unique_wav_path(s, &dir, BASE_NAME)?;
        if !cfg!(target_arch = "wasm32") {
            std::fs::create_dir_all(&dir).map_err(|e| EngineError::Other(format!("{dir}: {e}")))?;
        }
        let bytes = music::wav::encode_f32(&audio);
        s.services.write_file(&path, &bytes).map_err(|e| EngineError::Other(format!("{path}: {e}")))?;
        let prov = music::provenance::Provenance::from_generation(
            info.id,
            info.kind,
            model_name(info.id),
            info.license,
            info.source,
            info.capabilities,
            &r.params,
            &audio,
            now_unix(),
        );
        let sidecar = music::provenance::Provenance::sidecar_path(&path);
        let text = serde_json::to_vec_pretty(&prov.to_json()).unwrap_or_default();
        s.services.write_file(&sidecar, &text).map_err(|e| EngineError::Other(format!("{sidecar}: {e}")))?;
        let item = crate::commands::import_bytes(s, &path, bytes.into(), None)?;
        let seq_rate = s.project.sequence(seq_id).map(|q| q.settings.frame_rate).unwrap_or_default();
        let clip = s.edit("New Music", |pr, st| {
            let mut it = pr
                .make_track_item(item, TrackKind::Audio, at, TimeRange::new(Tick::ZERO, dur), seq_rate)
                .ok_or(EngineError::Other("the music is not importable".into()))?;
            it.duration = dur;
            let id = it.id;
            let seq = pr.sequence_mut(seq_id).ok_or(EngineError::NoSequence)?;
            let t = seq.audio_tracks.iter_mut().find(|t| t.id == track).ok_or_else(|| bad(cmd, "the music track was deleted"))?;
            let pos = t.items.partition_point(|x| x.start <= at);
            t.items.insert(pos, it);
            seq.check().map_err(EngineError::Other)?;
            st.selection = vec![id];
            Ok(id)
        })?;
        s.execute("essentialSound.setType", json!({"clips": [clip.0], "type": "music"}))?;
        if ducking {
            s.execute("essentialSound.generateDucking", json!({"clips": [clip.0]}))?;
        }
        let seq = s.project.sequence(seq_id).ok_or(EngineError::NoSequence)?;
        Ok(json!({
            "item": item.0, "clip": clip.0, "track": crate::mixer::strip_label(seq, track),
            "start": at.0, "duration": dur.0, "path": path, "provenance": sidecar,
            "provider": info.id, "seed": r.params.seed, "sampleRate": rate, "ducking": ducking,
        }))
    })();
    crate::clip_ops::collapse_history(s, n0, "New Music");
    if result.is_err() && s.history.undo.len() > n0 {
        let _ = s.undo();
        s.history.redo.clear();
    }
    result
}

fn clip_item(s: &Session, cmd: &str, p: &Value) -> Result<ItemId> {
    if let Some(i) = u64_p(p, "item") {
        return Ok(ItemId(i));
    }
    let seq = s.active_sequence().ok_or(EngineError::NoSequence)?;
    let c = match u64_p(p, "clip") {
        Some(c) => ClipId(c),
        None => s.state.selection.first().copied().ok_or_else(|| bad(cmd, "pass `item` or `clip`, or select a generated music clip"))?,
    };
    seq.find_item(c).map(|(_, it)| it.item).ok_or_else(|| bad(cmd, "no such clip"))
}

fn inspect(s: &mut Session, p: &Value) -> Result<Value> {
    let cmd = "music.inspect";
    let item = clip_item(s, cmd, p)?;
    let path = media_dir_of(s, item).ok_or_else(|| bad(cmd, "not a generated music file"))?;
    let sidecar = music::provenance::Provenance::sidecar_path(&path);
    let bytes = s.services.read_file(&sidecar).map_err(|_| bad(cmd, "this media has no music provenance"))?;
    let v: Value = serde_json::from_slice(&bytes).map_err(|e| bad(cmd, format!("unreadable provenance: {e}")))?;
    Ok(json!({"item": item.0, "path": path, "provenance": v}))
}

/// Download the on-device model files in the background.
fn download(s: &mut Session, p: &Value) -> Result<Value> {
    let cmd = "music.download";
    let dir = models_dir().ok_or_else(|| bad(cmd, "no data directory for models"))?;
    let m = music::models::load_manifest(&dir, music::catalog::LOCAL_MODEL_ID).map_err(|e| err(cmd, e))?;
    if !music::models::licenses_accepted(&dir, &m) {
        return Err(bad(cmd, "accept the model licences first (music.acceptLicenses)"));
    }
    let missing = music::models::missing_files(&dir, &m);
    if missing.is_empty() {
        return Ok(json!({"job": null, "installed": true}));
    }
    if !cfg!(feature = "music-download") {
        return Err(bad(cmd, "downloads are not available in this build (built without the `music-download` feature)"));
    }
    let wait = p.get("wait").and_then(Value::as_bool).unwrap_or(false);
    let job = new_job(s, format!("Downloading {}", m.name));
    let (prog, res) = (job.progress.clone(), job.result.clone());
    let id = spawn(s, job, wait, "filmcraft-music-download", move || {
        let t0 = web_time::Instant::now();
        let r = download_files(&dir, &m, &missing, &prog);
        finish(&prog, &res, r.map(|()| format!("{} installed", m.name)), t0);
    })?;
    Ok(json!({"job": id, "installed": false}))
}

#[cfg(feature = "music-download")]
fn download_files(
    dir: &std::path::Path,
    m: &music::models::Manifest,
    missing: &[String],
    prog: &filmcraft_export::Progress,
) -> std::result::Result<(), String> {
    let total: u64 = m.files.iter().filter(|f| missing.contains(&f.name)).fold(0u64, |a, f| a.saturating_add(f.size));
    prog.total.store(total, Ordering::Relaxed);
    let mut done = 0u64;
    for f in m.files.iter().filter(|f| missing.contains(&f.name)) {
        *prog.status.lock().unwrap_or_else(PoisonError::into_inner) = format!("Downloading {}", f.name);
        music::models::download_file(dir, m, f, &mut |n| {
            prog.done.store(done.saturating_add(n), Ordering::Relaxed);
            !prog.cancel.load(Ordering::Relaxed)
        })
        .map_err(|e| e.to_string())?;
        done = done.saturating_add(f.size);
    }
    Ok(())
}

#[cfg(not(feature = "music-download"))]
fn download_files(_: &std::path::Path, _: &music::models::Manifest, _: &[String], _: &filmcraft_export::Progress) -> std::result::Result<(), String> {
    Err("downloads are not available in this build".into())
}

fn spec(
    id: &'static str,
    label: &'static str,
    params: &'static str,
    enabled: fn(&Session) -> std::result::Result<(), String>,
    run: fn(&mut Session, &Value) -> Result<Value>,
    journal: bool,
) -> CommandSpec {
    CommandSpec { id, label, menu: &[], shortcut: None, params, enabled, run, journal }
}

pub fn commands() -> Vec<CommandSpec> {
    vec![
        spec("music.providers", "List Music Providers", "{}", always, providers, false),
        spec("music.acceptLicenses", "Accept Music Model Licences", "{}", always, accept_licenses, false),
        spec("music.download", "Download Music Model", r#"{"wait":bool?}"#, always, download, false),
        spec(
            "music.render",
            "Generate Music",
            r#"{"prompt":str,"duration":seconds|"fill":"inout","provider":str?,"bpm":f64?,"key":str?,"instrumental":bool?,"negativePrompt":str?,"seed":u64?,"wait":bool?}"#,
            always,
            render_job,
            false,
        ),
        spec(
            "music.create",
            "New Music",
            r#"{"prompt":str,"duration":seconds|"fill":"inout","provider":str?,"bpm":f64?,"key":str?,"instrumental":bool?,"negativePrompt":str?,"seed":u64?,"time":ticks?,"track":"A1"|id?,"ducking":bool?,"dir":str?}"#,
            has_seq,
            create,
            true,
        ),
        spec("music.inspect", "Inspect Music Provenance", r#"{"clip":id?,"item":id?}"#, always, inspect, false),
    ]
}

#[cfg(test)]
mod unit {
    use super::*;

    #[test]
    fn keys_are_redacted_but_other_prefs_are_not() {
        let mut v = json!({"music": {"provider": "x", "api": {"stabilityApiKey": "sk-1", "otherApiKey": "", "stabilityEndpoint": "https://e"}}});
        redact(&mut v);
        assert_eq!(v["music"]["api"]["stabilityApiKey"], "(set)");
        assert_eq!(v["music"]["api"]["otherApiKey"], "");
        assert_eq!(v["music"]["api"]["stabilityEndpoint"], "https://e");
        assert!(!v.to_string().contains("sk-1"));
    }
}
