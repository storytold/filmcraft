//! Text to Speech (narrations): write what the narrator says, pick a voice, and get an audio clip.
//!
//! | command | does |
//! |---|---|
//! | `tts.voices` | languages, voices, vocal pitches, pace limits and the pause marker |
//! | `tts.preview` | synthesize without changing the project (`sample: true` speaks the voice's sample sentence); the host plays [`Session::tts_preview`]; `path` also writes a WAV |
//! | `tts.create` | synthesize, write `Narration <n>.wav`, import it and place it at the playhead (one undo step) |
//! | `tts.edit` | change a narration clip's script or voice settings and regenerate it in place (one undo step) |
//! | `tts.inspect` | the narration settings of a clip or item |
//! | `tts.render` | synthesize in a background job into the cache, so the UI's create / edit / preview return at once |
//! | `tts.downloadVoices` | download the natural voices (Kokoro-82M, 9 US English voices, CMUdict; 336 MB, SHA-256 checked) as a background job |
//!
//! **Where a new narration goes.** At `time` (default: the playhead) on `track` if given (it must
//! be free there), else on the first targeted audio track that is free for the narration's length,
//! else the first free audio track, else on a new audio track. Existing clips are never overwritten.
//!
//! **Editing keeps the clip's length** (user decision, 2026-10-07). The clip keeps its start,
//! duration and speed. When the new speech is longer than the clip, the clip still ends where it
//! did but the file holds the whole speech, so trimming the clip's end out reveals the rest. When it
//! is shorter, the file is padded with silence to the clip's length. The clip's source In is reset
//! to the start of the new speech. Other clips (including other clips of the old narration) are not
//! changed. Each edit writes a new file; the old item and its narration stay, so undo is exact.
//!
//! Files are named `Narration <n>.wav` (mono 32-bit float at the voice's rate, 24 kHz) and saved
//! where voice-over recordings go (Scratch Disks ▸ Captured, else next to the project, else
//! `Narrations` in the data or temporary directory); an edit writes next to the previous file.

use std::sync::{Arc, Mutex, PoisonError};

use serde_json::{Value, json};

use filmcraft_project::{ClipId, ItemId, ItemKind, MediaRef, Narration, TrackId, TrackKind, VocalPitch};
use filmcraft_time::{Tick, TimeRange};
use filmcraft_tts as tts;

use crate::commands::{CommandSpec, always, bad, has_seq, str_p, time_p, u64_p};
use crate::{EngineError, Result, Session};

/// Item name prefix of generated narrations.
const BASE_NAME: &str = "Narration";

/// The narration of a project item.
pub fn narration_of(s: &Session, item: ItemId) -> Option<&Narration> {
    s.project.narrations.get(&item)
}

/// The narration clip a command works on: `clip`, else the first selected clip whose item is a
/// narration.
pub fn target_clip(s: &Session, p: &Value) -> Option<ClipId> {
    let seq = s.active_sequence()?;
    let is_narration = |c: ClipId| seq.find_item(c).is_some_and(|(_, it)| s.project.narrations.contains_key(&it.item));
    match u64_p(p, "clip") {
        Some(c) => Some(ClipId(c)).filter(|c| is_narration(*c)),
        None => s.state.selection.iter().copied().find(|c| is_narration(*c)),
    }
}

fn has_narration_clip(s: &Session) -> std::result::Result<(), String> {
    has_seq(s)?;
    target_clip(s, &Value::Null).map(|_| ()).ok_or_else(|| "select a narration clip".into())
}

/// Script and settings from params, on top of `base`.
fn settings(cmd: &str, p: &Value, base: Option<&Narration>) -> Result<Narration> {
    // a setting of the wrong type is an error, never silently replaced by a default
    for key in ["text", "voice", "pitch"] {
        if p.get(key).is_some_and(|v| !v.is_string()) {
            return Err(bad(cmd, format!("`{key}` must be a string")));
        }
    }
    let text = match str_p(p, "text") {
        Some(t) => t.to_string(),
        None => base.map(|b| b.text.clone()).ok_or_else(|| bad(cmd, "`text` is required"))?,
    };
    let voice = str_p(p, "voice").map(str::to_string).or_else(|| base.map(|b| b.voice.clone())).unwrap_or_else(|| tts::default_voice_id().to_string());
    let info = tts::voices_in(None).into_iter().find(|v| v.id == voice).ok_or_else(|| bad(cmd, format!("unknown voice `{voice}` (see tts.voices)")))?;
    let pitch = match str_p(p, "pitch") {
        Some(v) => VocalPitch::from_id(v).ok_or_else(|| bad(cmd, format!("unknown pitch `{v}` (extraLow, low, default, high, extraHigh)")))?,
        None => base.map(|b| b.pitch).unwrap_or_default(),
    };
    let pace = match p.get("pace") {
        Some(v) => v.as_f64().ok_or_else(|| bad(cmd, "`pace` must be a number"))?,
        None => base.map_or(1.0, |b| b.pace),
    };
    let n = Narration { text, language: info.language.to_string(), voice, pitch, pace, speech_duration: Tick::ZERO };
    n.check().map_err(|e| bad(cmd, e))?;
    Ok(n)
}

/// Synthesized narrations by settings, newest last (`tts.render` fills it in the background so
/// the UI's create / edit / preview don't block).
pub type SynthCache = Arc<Mutex<Vec<(String, Arc<tts::Audio>)>>>;
const CACHE_ENTRIES: usize = 6;

fn cache_key(n: &Narration) -> String {
    format!("{}\u{1f}{}\u{1f}{}\u{1f}{}", n.voice, n.pitch.id(), n.pace.to_bits(), n.text)
}

fn tts_err(cmd: &str, e: tts::TtsError) -> EngineError {
    match e {
        tts::TtsError::NotInstalled => bad(cmd, "the natural voices are not downloaded yet: Text to Speech ▸ Download natural voices (tts.downloadVoices)"),
        e => bad(cmd, e.to_string()),
    }
}

/// Synthesize `n` (no cache).
fn render(n: &Narration, models_dir: Option<&std::path::Path>) -> std::result::Result<tts::Audio, tts::TtsError> {
    let voice = tts::voice_in(&n.voice, models_dir)?;
    voice.synthesize(&n.text, &tts::Params { semitones: n.pitch.semitones(), pace: n.pace })
}

fn synthesize(s: &Session, cmd: &str, n: &Narration) -> Result<Arc<tts::Audio>> {
    let key = cache_key(n);
    if let Some(a) = s.tts_cache.lock().unwrap_or_else(PoisonError::into_inner).iter().find(|(k, _)| *k == key).map(|(_, a)| a.clone()) {
        return Ok(a);
    }
    let a = Arc::new(render(n, crate::transcript::models_dir().as_deref()).map_err(|e| tts_err(cmd, e))?);
    cache_put(&s.tts_cache, key, a.clone());
    Ok(a)
}

fn cache_put(c: &SynthCache, key: String, a: Arc<tts::Audio>) {
    let mut c = c.lock().unwrap_or_else(PoisonError::into_inner);
    c.retain(|(k, _)| *k != key);
    c.push((key, a));
    let extra = c.len().saturating_sub(CACHE_ENTRIES);
    c.drain(..extra);
}

fn package_json() -> Value {
    let dir = crate::transcript::models_dir();
    json!({
        "id": tts::catalog::PACKAGE_ID,
        "name": tts::catalog::PACKAGE_NAME,
        "installed": dir.as_deref().is_some_and(tts::catalog::installed),
        "size": tts::catalog::size(),
        "missingBytes": dir.as_deref().map_or(tts::catalog::size(), tts::catalog::missing_bytes),
        "license": tts::catalog::LICENSE,
        "source": tts::catalog::SOURCE,
        "available": cfg!(feature = "neural-voices"),
    })
}

fn voices(_s: &mut Session, _p: &Value) -> Result<Value> {
    let dir = crate::transcript::models_dir();
    Ok(json!({
        "package": package_json(),
        "languages": tts::LANGUAGES.iter().map(|(id, name)| json!({"id": id, "name": name})).collect::<Vec<_>>(),
        "voices": tts::voices_in(dir.as_deref()),
        "defaultVoice": tts::default_voice_id(),
        "pitches": VocalPitch::ALL.iter().map(|p| json!({"id": p.id(), "label": p.label(), "semitones": p.semitones()})).collect::<Vec<_>>(),
        "pace": {"min": tts::MIN_PACE, "max": tts::MAX_PACE, "default": 1.0},
        "pauseMarker": tts::script::PAUSE_MARKER,
        "maxTextBytes": tts::MAX_TEXT_BYTES,
        "sampleSentence": tts::SAMPLE_SENTENCE,
    }))
}

fn preview(s: &mut Session, p: &Value) -> Result<Value> {
    let cmd = "tts.preview";
    let base = target_clip(s, p).and_then(|c| s.active_sequence()?.find_item(c).map(|(_, it)| it.item)).and_then(|i| s.project.narrations.get(&i).cloned());
    let mut q = p.clone();
    if q.get("sample").and_then(Value::as_bool).unwrap_or(false) {
        q["text"] = json!(tts::SAMPLE_SENTENCE);
    }
    let n = settings(cmd, &q, base.as_ref())?;
    let audio = synthesize(s, cmd, &n)?;
    let seconds = audio.seconds();
    let samples = audio.samples.len();
    let rate = audio.sample_rate;
    let mut path = Value::Null;
    if let Some(dest) = str_p(p, "path") {
        let bytes = crate::voiceover::write_wav_f32(&audio.samples, rate);
        s.services.write_file(dest, &bytes).map_err(|e| EngineError::Other(format!("{dest}: {e}")))?;
        path = json!(dest);
    }
    s.tts_preview = Some(audio);
    Ok(json!({"seconds": seconds, "samples": samples, "sampleRate": rate, "voice": n.voice, "path": path}))
}

/// Write `samples` as a new WAV and import it (its own undo step; callers collapse history).
/// `near`: an edit writes next to the narration's previous file unless `dir` is given.
fn write_and_import(s: &mut Session, p: &Value, samples: &[f32], rate: u32, near: Option<ItemId>) -> Result<(ItemId, String)> {
    let beside = near.filter(|_| p.get("dir").is_none()).and_then(|i| match &s.project.items.get(&i)?.kind {
        ItemKind::Media(m) => match &m.media {
            MediaRef::File { path } => std::path::Path::new(path).parent().filter(|d| !d.as_os_str().is_empty()).map(|d| d.to_string_lossy().into_owned()),
            MediaRef::Generator(_) => None,
        },
        _ => None,
    });
    let dir = beside.unwrap_or_else(|| crate::voiceover::media_dir(s, p, "Narrations"));
    let path = crate::voiceover::unique_wav_path(s, &dir, BASE_NAME)?;
    if !cfg!(target_arch = "wasm32") {
        std::fs::create_dir_all(&dir).map_err(|e| EngineError::Other(format!("{dir}: {e}")))?;
    }
    let bytes = crate::voiceover::write_wav_f32(samples, rate);
    s.services.write_file(&path, &bytes).map_err(|e| EngineError::Other(format!("{path}: {e}")))?;
    let item = crate::commands::import_bytes(s, &path, bytes.into(), None)?;
    Ok((item, path))
}

fn track_is_free(s: &Session, track: TrackId, range: TimeRange) -> bool {
    s.active_sequence()
        .and_then(|q| q.audio_tracks.iter().find(|t| t.id == track))
        .is_some_and(|t| !t.locked && !t.items.iter().any(|it| it.start < range.end() && range.start < it.end()))
}

/// The track a new narration of `range` goes to (None: add a track).
fn placement(s: &Session, p: &Value, range: TimeRange) -> Result<Option<TrackId>> {
    let cmd = "tts.create";
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
    let cmd = "tts.create";
    let seq_id = s.state.active_sequence.ok_or(EngineError::NoSequence)?;
    let mut n = settings(cmd, p, None)?;
    let at = time_p(s, p, "").unwrap_or(s.playhead());
    if at < Tick::ZERO {
        return Err(bad(cmd, "the start time cannot be negative"));
    }
    let audio = synthesize(s, cmd, &n)?;
    let rate = audio.sample_rate.max(1);
    let dur = Tick::from_units(audio.samples.len() as i64, i64::from(rate));
    n.speech_duration = dur;
    let range = TimeRange::new(at, dur);
    let track = placement(s, p, range)?;
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
        let (item, path) = write_and_import(s, p, &audio.samples, rate, None)?;
        let seq_rate = s.project.sequence(seq_id).map(|q| q.settings.frame_rate).unwrap_or_default();
        let narration = n.clone();
        let clip = s.edit("New Narration", |pr, st| {
            let mut it = pr
                .make_track_item(item, TrackKind::Audio, at, TimeRange::new(Tick::ZERO, dur), seq_rate)
                .ok_or(EngineError::Other("the narration is not importable".into()))?;
            it.duration = dur;
            let id = it.id;
            let seq = pr.sequence_mut(seq_id).ok_or(EngineError::NoSequence)?;
            let t = seq.audio_tracks.iter_mut().find(|t| t.id == track).ok_or_else(|| bad(cmd, "the narration track was deleted"))?;
            let pos = t.items.partition_point(|x| x.start <= at);
            t.items.insert(pos, it);
            seq.check().map_err(EngineError::Other)?;
            pr.narrations.insert(item, narration);
            st.selection = vec![id];
            Ok(id)
        })?;
        let seq = s.project.sequence(seq_id).ok_or(EngineError::NoSequence)?;
        Ok(json!({
            "item": item.0,
            "clip": clip.0,
            "track": crate::mixer::strip_label(seq, track),
            "start": at.0,
            "duration": dur.0,
            "speechDuration": dur.0,
            "path": path,
            "sampleRate": rate,
        }))
    })();
    crate::clip_ops::collapse_history(s, n0, "New Narration");
    if result.is_err() && s.history.undo.len() > n0 {
        // a step failed after the track was added or the file imported: roll back to before
        let _ = s.undo();
        s.history.redo.clear();
    }
    result
}

/// Samples of media a clip of `duration` at `speed` plays, rounded up (exact for 100 %).
fn needed_samples(duration: Tick, speed: f64, rate: u32) -> usize {
    let rate = i64::from(rate.max(1));
    let media = if speed.is_finite() && speed != 0.0 && (speed.abs() - 1.0).abs() > 1e-12 {
        // speed is a ratio, not edit time: scale the exact tick count
        Tick::from_seconds_f64(duration.seconds() * speed.abs())
    } else {
        duration
    };
    let mut n = media.to_units_floor(rate);
    if Tick::from_units(n, rate) < media {
        n = n.saturating_add(1);
    }
    let cap = (tts::MAX_SECONDS * 2.0) as i64 * rate;
    n.clamp(0, cap) as usize
}

fn edit(s: &mut Session, p: &Value) -> Result<Value> {
    let cmd = "tts.edit";
    let seq_id = s.state.active_sequence.ok_or(EngineError::NoSequence)?;
    let clip = target_clip(s, p).ok_or_else(|| bad(cmd, "select a narration clip (or pass `clip`)"))?;
    let seq = s.active_sequence().ok_or(EngineError::NoSequence)?;
    let (track, it) = seq.find_item(clip).ok_or_else(|| bad(cmd, "no such clip"))?;
    if seq.audio_tracks.iter().any(|t| t.id == track && t.locked) {
        return Err(bad(cmd, "the clip's track is locked"));
    }
    let (old_item, clip_dur, speed) = (it.item, it.duration, it.speed);
    let base = s.project.narrations.get(&old_item).cloned().ok_or_else(|| bad(cmd, "the clip is not a narration"))?;
    let mut n = settings(cmd, p, Some(&base))?;
    let audio = synthesize(s, cmd, &n)?;
    let rate = audio.sample_rate.max(1);
    let speech = audio.samples.len();
    n.speech_duration = Tick::from_units(speech as i64, i64::from(rate));
    // the clip keeps its length: pad the file with silence to what the clip plays
    let needed = needed_samples(clip_dur, speed, rate);
    let mut samples = audio.samples.clone();
    if needed > samples.len() {
        samples.try_reserve(needed - samples.len()).map_err(|e| bad(cmd, format!("unable to allocate the narration: {e}")))?;
        samples.resize(needed, 0.0);
    }
    let file_len = samples.len();
    let n0 = s.history.undo.len();
    let result = (|| -> Result<Value> {
        let (item, path) = write_and_import(s, p, &samples, rate, Some(old_item))?;
        let name = s.project.items.get(&item).map(|i| i.name.clone()).unwrap_or_default();
        let narration = n.clone();
        s.edit("Edit Narration", |pr, _| {
            let seq = pr.sequence_mut(seq_id).ok_or(EngineError::NoSequence)?;
            let (_, it) = seq.find_item_mut(clip).ok_or_else(|| bad(cmd, "the clip was deleted"))?;
            it.item = item;
            it.name = name;
            it.source_in = Tick::ZERO;
            seq.check().map_err(EngineError::Other)?;
            pr.narrations.insert(item, narration);
            Ok(())
        })?;
        Ok(json!({
            "clip": clip.0,
            "item": item.0,
            "previousItem": old_item.0,
            "duration": clip_dur.0,
            "speechDuration": n.speech_duration.0,
            "fileDuration": Tick::from_units(file_len as i64, i64::from(rate)).0,
            "path": path,
            "sampleRate": rate,
        }))
    })();
    crate::clip_ops::collapse_history(s, n0, "Edit Narration");
    if result.is_err() && s.history.undo.len() > n0 {
        let _ = s.undo();
        s.history.redo.clear();
    }
    result
}

fn inspect(s: &mut Session, p: &Value) -> Result<Value> {
    let cmd = "tts.inspect";
    let (item, clip) = match u64_p(p, "item") {
        Some(i) => (ItemId(i), None),
        None => {
            let c = target_clip(s, p).ok_or_else(|| bad(cmd, "pass `item` or `clip`, or select a narration clip"))?;
            let seq = s.active_sequence().ok_or(EngineError::NoSequence)?;
            (seq.find_item(c).map(|(_, it)| it.item).ok_or_else(|| bad(cmd, "no such clip"))?, Some(c.0))
        }
    };
    let n = s.project.narrations.get(&item).ok_or_else(|| bad(cmd, "not a narration"))?;
    Ok(json!({"item": item.0, "clip": clip, "narration": n}))
}

fn new_job(s: &mut Session, label: String) -> crate::Job {
    let id = s.jobs.iter().map(|j| j.id).max().unwrap_or(0) + 1;
    crate::Job { id, label, progress: Default::default(), result: Default::default() }
}

fn spawn(s: &mut Session, job: crate::Job, wait: bool, name: &str, run: impl FnOnce() + Send + 'static) -> Result<u64> {
    let id = job.id;
    let run = crate::export_tools::guard_job(job.progress.clone(), job.result.clone(), run);
    s.jobs.push(job);
    if wait || cfg!(target_arch = "wasm32") {
        run();
    } else {
        std::thread::Builder::new().name(name.into()).spawn(run).map_err(|e| EngineError::Other(e.to_string()))?;
    }
    Ok(id)
}

type JobResult = std::sync::Mutex<Option<std::result::Result<filmcraft_export::Report, String>>>;

fn finish(prog: &filmcraft_export::Progress, res: &JobResult, r: std::result::Result<String, String>, t0: web_time::Instant) {
    use std::sync::atomic::Ordering;
    let secs = t0.elapsed().as_secs_f64();
    let r = match r {
        Ok(status) => {
            *prog.status.lock().unwrap_or_else(PoisonError::into_inner) = status;
            Ok(filmcraft_export::Report { path: String::new(), frames: 0, seconds: secs, bytes: 0, render_fps: 0.0, extra_files: Vec::new() })
        }
        Err(e) => {
            *prog.status.lock().unwrap_or_else(PoisonError::into_inner) = e.clone();
            *prog.error.lock().unwrap_or_else(PoisonError::into_inner) = Some(e.clone());
            Err(e)
        }
    };
    prog.finished.store(true, Ordering::Relaxed);
    *res.lock().unwrap_or_else(PoisonError::into_inner) = Some(r);
}

/// Synthesize in the background into the cache, so `tts.create` / `tts.edit` / `tts.preview` with
/// the same settings return at once.
fn render_job(s: &mut Session, p: &Value) -> Result<Value> {
    let cmd = "tts.render";
    let base = target_clip(s, p).and_then(|c| s.active_sequence()?.find_item(c).map(|(_, it)| it.item)).and_then(|i| s.project.narrations.get(&i).cloned());
    let mut q = p.clone();
    if q.get("sample").and_then(Value::as_bool).unwrap_or(false) {
        q["text"] = json!(tts::SAMPLE_SENTENCE);
    }
    let n = settings(cmd, &q, base.as_ref())?;
    let key = cache_key(&n);
    if s.tts_cache.lock().unwrap_or_else(PoisonError::into_inner).iter().any(|(k, _)| *k == key) {
        return Ok(json!({"job": null, "cached": true}));
    }
    let job = new_job(s, "Synthesizing narration".into());
    let (prog, res, cache) = (job.progress.clone(), job.result.clone(), s.tts_cache.clone());
    let wait = p.get("wait").and_then(Value::as_bool).unwrap_or(false);
    let id = spawn(s, job, wait, "filmcraft-tts", move || {
        let t0 = web_time::Instant::now();
        *prog.status.lock().unwrap_or_else(PoisonError::into_inner) = "Synthesizing…".into();
        let r = match render(&n, crate::transcript::models_dir().as_deref()) {
            Ok(a) => {
                let secs = a.seconds();
                cache_put(&cache, key, Arc::new(a));
                Ok(format!("{secs:.1} s of speech"))
            }
            Err(tts::TtsError::NotInstalled) => Err("the natural voices are not downloaded yet".to_string()),
            Err(e) => Err(e.to_string()),
        };
        finish(&prog, &res, r, t0);
    })?;
    Ok(json!({"job": id, "cached": false}))
}

/// Download the natural voices (Kokoro-82M, its voices and CMUdict) in the background.
fn download_voices(s: &mut Session, p: &Value) -> Result<Value> {
    let cmd = "tts.downloadVoices";
    let dir = crate::transcript::models_dir().ok_or_else(|| bad(cmd, "no data directory for voice models"))?;
    if tts::catalog::installed(&dir) {
        return Ok(json!({"job": null, "installed": true}));
    }
    if !cfg!(feature = "neural-voices") {
        return Err(bad(cmd, "natural voices are not available in this build (built without the `neural-voices` feature)"));
    }
    let wait = p.get("wait").and_then(Value::as_bool).unwrap_or(false);
    let job = new_job(s, format!("Downloading {}", tts::catalog::PACKAGE_NAME));
    job.progress.total.store(tts::catalog::missing_bytes(&dir), std::sync::atomic::Ordering::Relaxed);
    let (prog, res) = (job.progress.clone(), job.result.clone());
    let id = spawn(s, job, wait, "filmcraft-tts-download", move || {
        let t0 = web_time::Instant::now();
        let r = download_package(&dir, &prog);
        finish(&prog, &res, r.map(|()| "Natural voices installed".to_string()), t0);
    })?;
    Ok(json!({"job": id, "installed": false}))
}

#[cfg(feature = "neural-voices")]
fn download_package(dir: &std::path::Path, prog: &filmcraft_export::Progress) -> std::result::Result<(), String> {
    use std::sync::atomic::Ordering;
    let files: Vec<filmcraft_speech::models::ModelFile> =
        tts::catalog::FILES.iter().map(|f| filmcraft_speech::models::ModelFile { name: f.name, url: f.url, sha256: f.sha256, size: f.size }).collect();
    let mut progress = |done: u64, total: u64, file: &str| {
        prog.done.store(done, Ordering::Relaxed);
        prog.total.store(total, Ordering::Relaxed);
        *prog.status.lock().unwrap_or_else(PoisonError::into_inner) = format!("Downloading {file}");
        !prog.cancel.load(Ordering::Relaxed)
    };
    filmcraft_speech::models::download_files(&tts::catalog::package_dir(dir), &files, &mut progress).map_err(|e| e.to_string())
}

#[cfg(not(feature = "neural-voices"))]
fn download_package(_: &std::path::Path, _: &filmcraft_export::Progress) -> std::result::Result<(), String> {
    Err("natural voices are not available in this build".into())
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
        spec("tts.voices", "List Narration Voices", "{}", always, voices, false),
        spec(
            "tts.preview",
            "Preview Narration",
            r#"{"text":str?,"voice":str?,"pitch":"default"?,"pace":f64?,"sample":bool?,"clip":id?,"path":str?}"#,
            always,
            preview,
            false,
        ),
        spec(
            "tts.create",
            "New Narration",
            r#"{"text":str,"voice":str?,"pitch":"extraLow|low|default|high|extraHigh"?,"pace":0.5..2?,"time":ticks?,"track":"A1"|id?,"dir":str?}"#,
            has_seq,
            create,
            true,
        ),
        spec("tts.edit", "Edit Narration", r#"{"clip":id?,"text":str?,"voice":str?,"pitch":str?,"pace":f64?,"dir":str?}"#, has_narration_clip, edit, true),
        spec("tts.inspect", "Inspect Narration", r#"{"clip":id?,"item":id?}"#, always, inspect, false),
        spec(
            "tts.render",
            "Synthesize Narration",
            r#"{"text":str?,"voice":str?,"pitch":str?,"pace":f64?,"sample":bool?,"clip":id?,"wait":bool?}"#,
            always,
            render_job,
            false,
        ),
        spec("tts.downloadVoices", "Download Natural Voices", r#"{"wait":bool?}"#, always, download_voices, false),
    ]
}
