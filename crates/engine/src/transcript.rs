//! Text-based editing commands (`transcript.*`): the Text panel ▸ Transcript tab.
//!
//! Transcripts belong to media items (`Project::transcripts`, media time); the sequence transcript
//! is derived from them ([`filmcraft_edit::transcript::sequence_words`]). Words of the sequence
//! transcript are addressed by index (`from`, `to`, inclusive), as `transcript.inspect` lists them.
//!
//! Speech recognition goes through a [`Transcriber`]: [`Session::transcriber`] when a host or a
//! test installed one, else the catalogue model named by `model` (Whisper or Parakeet TDT) from
//! `<data dir>/models` (needs the engine feature `whisper`, or `parakeet` for Parakeet only; without
//! them `transcript.generate` fails with a clear error, and agents
//! can still bring their own transcript with `transcript.set`).

use std::collections::BTreeMap;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex, PoisonError};

use serde_json::{Value, json};

use filmcraft_edit as edit;
use filmcraft_edit::transcript::{self as tx, CaptionRules, SeqWord};
use filmcraft_project::{CaptionFormat, CaptionTrack, ItemId, ItemKind, TrackId, Transcript};
use filmcraft_speech::{Options, SpeechError, Transcriber};
use filmcraft_time::{TICKS_PER_SECOND, Tick, TimeRange};

use crate::commands::{CommandSpec, always, bad, bool_p, f64_p, has_seq, str_p, u64_p};
use crate::{EngineError, Result, Session};

type Run = fn(&mut Session, &Value) -> Result<Value>;
type Enabled = fn(&Session) -> std::result::Result<(), String>;

fn spec(id: &'static str, label: &'static str, menu: &'static [&'static str], params: &'static str, enabled: Enabled, run: Run, journal: bool) -> CommandSpec {
    CommandSpec { id, label, menu, shortcut: None, params, enabled, run, journal }
}

/// Where downloaded speech models live (`<data dir>/models`).
pub fn models_dir() -> Option<std::path::PathBuf> {
    crate::autosave::default_data_dir().map(|d| d.join("models"))
}

/// Whether this build can transcribe with Whisper (feature `whisper`).
pub fn speech_available() -> bool {
    filmcraft_speech::available()
}

/// The words of the active sequence's transcript.
pub fn sequence_words(s: &Session) -> Vec<SeqWord> {
    match s.active_sequence() {
        Some(q) => tx::sequence_words(q, &s.project.transcripts),
        None => Vec::new(),
    }
}

fn has_transcript(s: &Session) -> std::result::Result<(), String> {
    has_seq(s)?;
    if sequence_words(s).is_empty() { Err("the sequence has no transcript (Transcribe first)".into()) } else { Ok(()) }
}

fn has_transcripts(s: &Session) -> std::result::Result<(), String> {
    if s.project.transcripts.is_empty() { Err("there are no transcripts".into()) } else { Ok(()) }
}

/// Why speech-to-text can't run in this build (no installed transcriber, built without `whisper`).
pub(crate) const NO_SPEECH: &str =
    "speech-to-text is not available in this build (built without the `whisper` feature); import a transcript with transcript.set instead";

/// `transcript.generate` can run: a host installed a transcriber or the build has speech-to-text
/// (#97: it reported enabled and then always failed).
pub(crate) fn can_transcribe(s: &Session) -> std::result::Result<(), String> {
    if s.transcriber.is_some() || speech_available() { Ok(()) } else { Err(NO_SPEECH.into()) }
}

/// `transcript.downloadModel` can run: built with `speech-download` (#98).
fn can_download(_: &Session) -> std::result::Result<(), String> {
    if cfg!(feature = "speech-download") {
        Ok(())
    } else {
        Err("model downloads are not available in this build (built without the `speech-download` feature)".into())
    }
}

/// The media item behind a project item (subclips resolve to their parent).
fn media_item(s: &Session, item: ItemId) -> Option<ItemId> {
    s.project.resolve_media(item).map(|(root, _, _)| root)
}

fn ids_p(p: &Value, k: &str) -> Option<Vec<ItemId>> {
    p.get(k).and_then(Value::as_array).map(|a| a.iter().filter_map(Value::as_u64).map(ItemId).collect())
}

/// Items to transcribe: `items` / `item`, else the Project panel selection, else the media of the
/// active sequence's enabled audio clips. Subclips resolve to their media; duplicates are removed.
fn targets(s: &Session, p: &Value) -> Vec<ItemId> {
    let mut raw = ids_p(p, "items").or_else(|| u64_p(p, "item").map(|i| vec![ItemId(i)])).unwrap_or_default();
    if raw.is_empty() {
        raw = s.state.project_selection.clone();
    }
    if raw.is_empty()
        && let Some(q) = s.active_sequence()
    {
        raw = q.audio_tracks.iter().flat_map(|t| t.items.iter()).filter(|it| it.enabled).map(|it| it.item).collect();
    }
    let mut out = Vec::new();
    for i in raw {
        if let Some(m) = media_item(s, i)
            && !out.contains(&m)
        {
            out.push(m);
        }
    }
    out
}

fn speech_err(e: SpeechError) -> EngineError {
    EngineError::Other(e.to_string())
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The label of transcription jobs (`jobs.list`, the Progress panel, the status bar).
pub const JOB_LABEL: &str = "Transcription";

/// The longest media a transcription takes (its mono 16 kHz samples are held in memory: 6 h is
/// about 1.4 GB).
const MAX_SECONDS: i64 = 6 * 3600;

/// Audio is decoded in pieces of this many seconds (progress and Cancel between them).
const DECODE_CHUNK_SECONDS: usize = 30;

/// Where the words come from: a recogniser the host or a test installed, or a catalogue model
/// that the job loads (and downloads first, when allowed).
#[derive(Clone)]
enum Recogniser {
    Installed(Arc<dyn Transcriber>),
    Model { model: &'static filmcraft_speech::models::ModelInfo, dir: std::path::PathBuf, download: bool },
}

/// The recogniser for `transcript.generate`: the installed one, else the named catalogue model
/// (Settings ▸ Media Analysis & Transcription ▸ Speech model), which must be downloaded already
/// unless `download` allows the job to fetch it.
fn recogniser(s: &Session, p: &Value) -> Result<Recogniser> {
    if let Some(t) = &s.transcriber {
        return Ok(Recogniser::Installed(t.clone()));
    }
    let id = str_p(p, "model").unwrap_or(&s.prefs.media_analysis.whisper_model);
    let model = filmcraft_speech::models::find(id).ok_or_else(|| speech_err(SpeechError::UnknownModel(id.into())))?;
    if !filmcraft_speech::available() {
        return Err(EngineError::Other(NO_SPEECH.into()));
    }
    let dir = models_dir().ok_or_else(|| EngineError::Other("no data directory for speech models".into()))?;
    let download = bool_p(p, "download").unwrap_or(false);
    if !filmcraft_speech::models::installed(&dir, model) {
        if !download {
            let mb = filmcraft_speech::models::missing_bytes(&dir, model) as f64 / 1e6;
            return Err(EngineError::Other(format!(
                "the speech model `{id}` is not downloaded ({mb:.0} MB, {}); pass \"download\": true or run transcript.downloadModel",
                model.license
            )));
        }
        can_download(s).map_err(EngineError::Other)?;
    }
    Ok(Recogniser::Model { model, dir, download })
}

/// One media item to transcribe.
struct WorkItem {
    item: ItemId,
    name: String,
    src: filmcraft_media::SharedSource,
    /// Length in 16 kHz samples.
    samples: usize,
}

/// What a finished transcription produced (applied by [`poll`]).
#[derive(Clone, Debug, Default)]
pub struct Transcribed {
    pub transcripts: Vec<(ItemId, Transcript)>,
    /// Items without audio, or too long.
    pub skipped: Vec<ItemId>,
}

/// A transcription running as a background job; [`poll`] applies its words in one undo step when
/// it finishes. A cancelled job changes nothing.
pub struct PendingTranscription {
    pub job: u64,
    pub items: Vec<ItemId>,
    /// The items' names when the job started: a transcript is only applied to the item it was
    /// made for (not to another project's item with the same id, opened meanwhile).
    pub names: Vec<String>,
    pub results: Arc<Mutex<Option<Transcribed>>>,
}

/// The transcription job that is still running, if any.
pub fn running_job(s: &Session) -> Option<&crate::Job> {
    s.transcribe_jobs.iter().find_map(|t| s.jobs.iter().find(|j| j.id == t.job && !j.progress.finished.load(Ordering::Relaxed)))
}

/// Report what the job is doing (status text and the fraction done, as per-mille of `total`).
fn report(prog: &filmcraft_export::Progress, status: impl Into<String>, done: f64) {
    prog.total.store(1000, Ordering::Relaxed);
    prog.done.store((done.clamp(0.0, 1.0) * 1000.0) as u64, Ordering::Relaxed);
    *lock(&prog.status) = status.into();
}

/// Download a missing model inside the job (`done` / `total` are bytes while it runs).
#[cfg(feature = "speech-download")]
fn fetch_model(dir: &std::path::Path, m: &filmcraft_speech::models::ModelInfo, prog: &filmcraft_export::Progress) -> std::result::Result<(), String> {
    *lock(&prog.status) = format!("Downloading {}", m.name);
    filmcraft_speech::models::download(dir, m, &mut |done, total, _| {
        prog.total.store(total.max(1), Ordering::Relaxed);
        prog.done.store(done.min(total), Ordering::Relaxed);
        !prog.cancel.load(Ordering::Relaxed)
    })
    .map_err(|e| e.to_string())
}

#[cfg(not(feature = "speech-download"))]
fn fetch_model(_: &std::path::Path, _: &filmcraft_speech::models::ModelInfo, _: &filmcraft_export::Progress) -> std::result::Result<(), String> {
    Err("model downloads are not available in this build (built without the `speech-download` feature)".into())
}

/// Mono 16 kHz samples of a work item, decoded a piece at a time (`share` of the job's progress
/// from `base`); None when cancelled.
fn decode(w: &WorkItem, prog: &filmcraft_export::Progress, status: &str, base: f64, share: f64) -> std::result::Result<Option<Vec<f32>>, String> {
    let sr = filmcraft_speech::SAMPLE_RATE;
    let chunk = DECODE_CHUNK_SECONDS * sr as usize;
    let mut out = Vec::new();
    out.try_reserve_exact(w.samples).map_err(|_| format!("{}: not enough memory for its audio", w.name))?;
    while out.len() < w.samples {
        if prog.cancel.load(Ordering::Relaxed) {
            return Ok(None);
        }
        let n = chunk.min(w.samples - out.len());
        let buf = w.src.audio(out.len() as i64, n, sr).map_err(|e| format!("{}: can't decode the audio: {e}", w.name))?;
        let mono = filmcraft_speech::downmix(&buf.channels);
        if mono.is_empty() {
            break;
        }
        out.extend(mono.into_iter().take(n));
        report(prog, status, base + share * out.len() as f64 / w.samples.max(1) as f64);
    }
    Ok(Some(out))
}

/// The body of a transcription job: get the recogniser (download, load), then decode and
/// transcribe each item. `Ok(None)`: cancelled.
fn transcribe_items(
    rec: Recogniser,
    work: &[WorkItem],
    opts: &Options,
    prog: &filmcraft_export::Progress,
) -> std::result::Result<Option<Vec<(ItemId, Transcript)>>, String> {
    let t = match rec {
        Recogniser::Installed(t) => t,
        Recogniser::Model { model, dir, download } => {
            if !filmcraft_speech::models::installed(&dir, model) {
                if !download {
                    return Err(format!("the speech model `{}` is not downloaded", model.id));
                }
                match fetch_model(&dir, model, prog) {
                    Err(_) if prog.cancel.load(Ordering::Relaxed) => return Ok(None),
                    r => r?,
                }
            }
            report(prog, format!("Loading {}", model.name), 0.0);
            filmcraft_speech::load(&dir, model.id).map_err(|e| e.to_string())?
        }
    };
    let total: f64 = work.iter().map(|w| w.samples as f64).sum::<f64>().max(1.0);
    let mut base = 0.0;
    let mut out = Vec::new();
    for (k, w) in work.iter().enumerate() {
        let share = w.samples as f64 / total;
        let status = if work.len() == 1 { format!("Transcribing {}", w.name) } else { format!("Transcribing {} ({} of {})", w.name, k + 1, work.len()) };
        // decoding takes the first tenth of the item's share, recognition the rest
        let Some(audio) = decode(w, prog, &status, base, share * 0.1)? else { return Ok(None) };
        let from = base + share * 0.1;
        let r = t.transcribe_cancellable(
            &audio,
            opts,
            &mut |f, _| {
                report(prog, status.as_str(), from + share * 0.9 * f64::from(f.clamp(0.0, 1.0)));
                true
            },
            &prog.cancel,
        );
        let mut tr = match r {
            Ok(tr) => tr,
            Err(SpeechError::Cancelled) => return Ok(None),
            Err(e) => return Err(format!("{}: {e}", w.name)),
        };
        tr.normalize();
        out.push((w.item, tr));
        base += share;
    }
    Ok(Some(out))
}

fn generate(s: &mut Session, p: &Value) -> Result<Value> {
    let items = targets(s, p);
    if items.is_empty() {
        return Err(bad("transcript.generate", "nothing to transcribe (pass `items`, select clips, or open a sequence with audio)"));
    }
    if running_job(s).is_some() {
        return Err(EngineError::Other("a transcription is already running (transcript.cancel stops it)".into()));
    }
    let rec = recogniser(s, p)?;
    // Settings ▸ Media Analysis & Transcription: language (or auto-detect) and speaker labelling
    let ma = &s.prefs.media_analysis;
    let default_language = if ma.language_auto_detect { None } else { Some(ma.default_language.clone()) };
    let opts = Options {
        language: match str_p(p, "language") {
            Some(l) => Some(l).filter(|l| !l.is_empty() && *l != "auto").map(str::to_string),
            None => default_language,
        },
        diarize: bool_p(p, "diarize").unwrap_or(ma.speaker_labeling != "off"),
        max_speakers: u64_p(p, "maxSpeakers").map(|n| n.clamp(1, 32) as usize).unwrap_or(Options::default().max_speakers),
    };
    let sr = filmcraft_speech::SAMPLE_RATE as i64;
    let mut work = Vec::new();
    let mut skipped = Vec::new();
    for item in items {
        let dur = match s.project.item(item).map(|i| &i.kind) {
            Some(ItemKind::Media(m)) => m.duration(),
            _ => {
                skipped.push(item);
                continue;
            }
        };
        let (Some(src), Some(name)) = (s.source(item).filter(|src| src.info().has_audio()), s.project.item(item).map(|i| i.name.clone())) else {
            skipped.push(item);
            continue;
        };
        let samples = dur.to_units_floor(sr);
        if samples <= 0 || samples > MAX_SECONDS * sr {
            skipped.push(item);
            continue;
        }
        work.push(WorkItem { item, name, src, samples: samples as usize });
    }
    if work.is_empty() {
        return Err(EngineError::Other("none of the clips has audio to transcribe".into()));
    }
    let ids: Vec<ItemId> = work.iter().map(|w| w.item).collect();
    let names: Vec<String> = work.iter().map(|w| w.name.clone()).collect();
    let id = s.jobs.iter().map(|j| j.id).max().unwrap_or(0) + 1;
    let job = crate::Job { id, label: JOB_LABEL.into(), progress: Default::default(), result: Default::default() };
    report(&job.progress, "Starting", 0.0);
    let results: Arc<Mutex<Option<Transcribed>>> = Arc::default();
    let (prog, res, out) = (job.progress.clone(), job.result.clone(), results.clone());
    let skipped_items = skipped.clone();
    let run = move || {
        let t0 = web_time::Instant::now();
        let r = transcribe_items(rec, &work, &opts, &prog);
        let secs = t0.elapsed().as_secs_f64();
        let r = match r {
            Ok(Some(transcripts)) => {
                let words: usize = transcripts.iter().map(|(_, t)| t.words.len()).sum();
                report(&prog, format!("Transcribed {words} word(s) ({secs:.1}s)"), 1.0);
                *lock(&out) = Some(Transcribed { transcripts, skipped: skipped_items });
                Ok(filmcraft_export::Report { path: String::new(), frames: words as u64, seconds: secs, bytes: 0, render_fps: 0.0, extra_files: Vec::new() })
            }
            Ok(None) => {
                *lock(&prog.status) = "Stopped: nothing was changed".into();
                Err("stopped".to_string())
            }
            Err(e) => {
                *lock(&prog.status) = e.clone();
                *lock(&prog.error) = Some(e.clone());
                Err(e)
            }
        };
        *lock(&res) = Some(r);
        prog.finished.store(true, Ordering::Relaxed);
    };
    let run = crate::export_tools::guard_job(job.progress.clone(), job.result.clone(), run);
    s.jobs.push(job);
    s.transcribe_jobs.push(PendingTranscription { job: id, items: ids.clone(), names, results: results.clone() });
    let wait = bool_p(p, "wait").unwrap_or(true);
    if !(wait || cfg!(target_arch = "wasm32")) {
        std::thread::Builder::new().name("filmcraft-transcribe".into()).spawn(run).map_err(|e| EngineError::Other(e.to_string()))?;
        return Ok(
            json!({"job": id, "items": ids.iter().map(|i| i.0).collect::<Vec<_>>(), "skipped": skipped.iter().map(|i| i.0).collect::<Vec<_>>(), "running": true}),
        );
    }
    run();
    let done = lock(&results).clone();
    poll(s);
    if let Some(Err(e)) = s.jobs.iter().find(|j| j.id == id).and_then(|j| lock(&j.result).clone()) {
        return Err(EngineError::Other(if e == "stopped" { "transcription stopped; nothing was changed".into() } else { e }));
    }
    let done = done.unwrap_or_default();
    let items: Vec<Value> = done
        .transcripts
        .iter()
        .map(|(i, t)| json!({"item": i.0, "words": t.words.len(), "speakers": t.speakers.len(), "language": t.language, "source": t.source}))
        .collect();
    Ok(json!({"job": id, "items": items, "skipped": skipped.iter().map(|i| i.0).collect::<Vec<_>>()}))
}

/// Apply finished transcriptions (one undo step, "Transcribe", each) and forget finished or
/// cancelled ones. Called once per UI frame from [`Session::poll_persistence`] and after a run
/// that was waited for.
pub fn poll(s: &mut Session) {
    let mut i = 0;
    while i < s.transcribe_jobs.len() {
        let job = s.jobs.iter().find(|j| j.id == s.transcribe_jobs[i].job);
        if !job.is_none_or(|j| j.progress.finished.load(Ordering::Relaxed)) {
            i += 1;
            continue;
        }
        let cancelled = job.is_some_and(|j| j.progress.cancel.load(Ordering::Relaxed));
        let failed = job.and_then(|j| lock(&j.result).clone()).and_then(|r| r.err()).filter(|e| e != "stopped");
        let pending = s.transcribe_jobs.remove(i);
        // (the Events panel logs how the job ended: `panels::log_jobs`)
        let mut toast = |message: String, error: bool| s.events.push(crate::Event::Toast { message, error });
        if let Some(e) = failed {
            toast(format!("Transcription failed: {e}"), true);
            continue;
        }
        let Some(done) = lock(&pending.results).take().filter(|_| !cancelled) else {
            if cancelled {
                toast("Transcription stopped; nothing was changed".into(), false);
            }
            continue;
        };
        // only items that are still the ones transcribed
        let still = |pr: &filmcraft_project::Project, i: ItemId| {
            let name = pending.items.iter().position(|x| *x == i).and_then(|k| pending.names.get(k));
            pr.item(i).is_some_and(|it| Some(&it.name) == name)
        };
        let transcripts: Vec<(ItemId, Transcript)> = done.transcripts.into_iter().filter(|(i, _)| still(&s.project, *i)).collect();
        if transcripts.is_empty() {
            continue;
        }
        let words: usize = transcripts.iter().map(|(_, t)| t.words.len()).sum();
        let r = s.edit("Transcribe", move |pr, _| {
            for (i, t) in transcripts {
                pr.transcripts.insert(i, Arc::new(t));
            }
            Ok(())
        });
        match r {
            Ok(()) if words == 0 => s.toast("Transcription finished: no speech was found"),
            Ok(()) => s.events.push(crate::Event::Toast { message: format!("Transcription finished: {words} word(s)"), error: false }),
            Err(e) => s.error_toast("transcript.generate", format!("Transcription: {e}")),
        }
    }
}

/// `transcript.status`: the running transcription (progress, status) or `{"running": false}`.
fn status(s: &mut Session, _: &Value) -> Result<Value> {
    Ok(match running_job(s) {
        Some(j) => {
            let mut v = j.to_json();
            v["running"] = json!(true);
            v["job"] = json!(j.id);
            v["items"] = json!(s.transcribe_jobs.iter().find(|t| t.job == j.id).map(|t| t.items.iter().map(|i| i.0).collect::<Vec<_>>()).unwrap_or_default());
            v
        }
        None => json!({"running": false}),
    })
}

fn is_transcribing(s: &Session) -> std::result::Result<(), String> {
    if running_job(s).is_some() { Ok(()) } else { Err("no transcription is running".into()) }
}

/// `transcript.cancel`: stop the running transcription; nothing changes.
fn cancel(s: &mut Session, _: &Value) -> Result<Value> {
    let j = running_job(s).ok_or_else(|| EngineError::Other("no transcription is running".into()))?;
    j.progress.cancel.store(true, Ordering::Relaxed);
    Ok(json!({"job": j.id}))
}

fn set(s: &mut Session, p: &Value) -> Result<Value> {
    let item = u64_p(p, "item").map(ItemId).ok_or_else(|| bad("transcript.set", "`item` is required"))?;
    let item = media_item(s, item).ok_or_else(|| bad("transcript.set", "no such media item"))?;
    let v = p.get("transcript").cloned().ok_or_else(|| bad("transcript.set", "`transcript` is required"))?;
    let mut t: Transcript = serde_json::from_value(v).map_err(|e| bad("transcript.set", e.to_string()))?;
    if t.source.is_empty() {
        t.source = "imported".into();
    }
    t.normalize();
    t.check().map_err(|e| bad("transcript.set", e))?;
    let n = t.words.len();
    s.edit("Set Transcript", move |pr, _| {
        pr.transcripts.insert(item, Arc::new(t));
        Ok(())
    })?;
    Ok(json!({"item": item.0, "words": n}))
}

fn delete(s: &mut Session, p: &Value) -> Result<Value> {
    let items: Vec<ItemId> = match ids_p(p, "items").or_else(|| u64_p(p, "item").map(|i| vec![ItemId(i)])) {
        Some(v) => v.into_iter().filter_map(|i| media_item(s, i)).collect(),
        None => s.project.transcripts.keys().copied().collect(),
    };
    let n = items.iter().filter(|i| s.project.transcripts.contains_key(i)).count();
    if n == 0 {
        return Err(EngineError::Other("no transcript to delete".into()));
    }
    s.edit("Delete Transcript", move |pr, _| {
        for i in items {
            pr.transcripts.remove(&i);
        }
        Ok(())
    })?;
    Ok(json!({"deleted": n}))
}

fn word_json(i: usize, w: &SeqWord, filler: bool) -> Value {
    json!({"i": i, "text": w.text, "start": w.start.0, "end": w.end.0, "speaker": w.speaker, "clip": w.clip.0, "item": w.item.0, "confidence": w.confidence, "filler": filler})
}

/// Transcript view options ▸ Minimum pause length: default and range, in seconds (Premiere's).
pub const DEFAULT_PAUSE_SECONDS: f64 = 0.75;
pub const MIN_PAUSE_SECONDS: f64 = 0.1;
pub const MAX_PAUSE_SECONDS: f64 = 3.0;

/// The shortest silence shown and found as a pause: `minPauseSeconds`, else Transcript view
/// options ▸ Minimum pause length (`transcript.minPauseLength`).
pub fn min_pause(s: &Session, p: &Value) -> Tick {
    let secs = f64_p(p, "minPauseSeconds").unwrap_or(s.prefs.transcript.min_pause_length);
    Tick::from_seconds_f64(if secs.is_finite() { secs.clamp(MIN_PAUSE_SECONDS, MAX_PAUSE_SECONDS) } else { DEFAULT_PAUSE_SECONDS })
}

/// Filler words of the sequence transcript as word index ranges. Each word is matched against the
/// list of its transcript's language ([`tx::default_fillers`]: English um/uh/erm…, German
/// äh/ähm/öhm…); `fillers` (an array of words and phrases) replaces the lists for every word.
pub fn filler_hits(s: &Session, words: &[SeqWord], p: &Value) -> Vec<std::ops::Range<usize>> {
    if let Some(a) = p.get("fillers").and_then(Value::as_array) {
        let list: Vec<String> = a.iter().filter_map(Value::as_str).map(str::to_string).collect();
        return tx::find_fillers(words, &list);
    }
    let language = |item: &ItemId| s.project.transcripts.get(item).map(|t| t.language.clone()).unwrap_or_default();
    let langs: BTreeMap<ItemId, String> = words.iter().map(|w| (w.item, language(&w.item))).collect();
    let lists: BTreeMap<&str, Vec<Vec<String>>> = langs.values().map(|l| (l.as_str(), tx::filler_phrases(tx::default_fillers(l)))).collect();
    tx::find_fillers_by(words, |w| langs.get(&w.item).and_then(|l| lists.get(l.as_str())).map(Vec::as_slice).unwrap_or_default())
}

/// Per word: is it (part of) a filler?
fn filler_flags(n: usize, hits: &[std::ops::Range<usize>]) -> Vec<bool> {
    let mut f = vec![false; n];
    for i in hits.iter().flat_map(Clone::clone) {
        if let Some(x) = f.get_mut(i) {
            *x = true;
        }
    }
    f
}

fn pause_json(p: &tx::Pause) -> Value {
    json!({"after": p.after, "start": p.start.0, "end": p.end.0, "seconds": p.duration().seconds()})
}

fn inspect(s: &mut Session, p: &Value) -> Result<Value> {
    let words = sequence_words(s);
    let gap = Tick::from_seconds_f64(f64_p(p, "paragraphGapSeconds").unwrap_or(1.5));
    let paras: Vec<Value> = tx::paragraphs(&words, gap)
        .into_iter()
        .filter_map(|r| {
            let (first, last) = (words.get(r.start)?, words.get(r.end.checked_sub(1)?)?);
            let text = words.get(r.clone())?.iter().map(|w| w.text.as_str()).collect::<Vec<_>>().join(" ");
            Some(json!({"from": r.start, "to": r.end - 1, "speaker": first.speaker, "start": first.start.0, "end": last.end.0, "text": text}))
        })
        .collect();
    let speakers: Vec<String> = {
        let mut v: Vec<String> = Vec::new();
        for w in &words {
            if let Some(n) = &w.speaker
                && !v.contains(n)
            {
                v.push(n.clone());
            }
        }
        v
    };
    let ph = s.playhead();
    let current = tx::word_at(&words, ph);
    let min = min_pause(s, p);
    let pauses = tx::pauses(&words, min);
    let fillers = filler_flags(words.len(), &filler_hits(s, &words, p));
    Ok(json!({
        "words": words.iter().enumerate().map(|(i, w)| word_json(i, w, fillers.get(i).copied().unwrap_or(false))).collect::<Vec<_>>(),
        "paragraphs": paras,
        "pauses": pauses.iter().map(pause_json).collect::<Vec<_>>(),
        "minPauseSeconds": min.seconds(),
        "speakers": speakers,
        "current": current,
        "currentPause": pauses.iter().find(|q| q.start <= ph && ph < q.end).map(|q| q.after),
        "items": s.project.transcripts.iter().map(|(i, t)| json!({"item": i.0, "words": t.words.len(), "language": t.language, "source": t.source, "speakers": t.speakers.iter().map(|k| &k.name).collect::<Vec<_>>()})).collect::<Vec<_>>(),
    }))
}

/// What a transcript search looks for (the Text panel's search filter).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Filter {
    /// Words and phrases (`query`).
    #[default]
    Text,
    /// Filler words.
    Fillers,
    /// Pauses of at least the pause length.
    Pauses,
}

impl Filter {
    pub const ALL: [Filter; 3] = [Filter::Text, Filter::Fillers, Filter::Pauses];
    pub fn name(self) -> &'static str {
        match self {
            Filter::Text => "text",
            Filter::Fillers => "fillers",
            Filter::Pauses => "pauses",
        }
    }
    pub fn parse(s: &str) -> Option<Filter> {
        Filter::ALL.into_iter().find(|f| f.name().eq_ignore_ascii_case(s))
    }
}

fn filter_p(p: &Value, cmd: &str) -> Result<Filter> {
    match str_p(p, "filter") {
        None => Ok(Filter::Text),
        Some(f) => Filter::parse(f).ok_or_else(|| bad(cmd, format!("unknown `filter` `{f}` (text, fillers, pauses)"))),
    }
}

/// One search match: words `from..=to`, or a pause.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Match {
    Words { from: usize, to: usize },
    Pause(tx::Pause),
}

impl Match {
    /// Sequence time where the match starts.
    pub fn start(&self, words: &[SeqWord]) -> Tick {
        match self {
            Match::Words { from, .. } => words.get(*from).map(|w| w.start).unwrap_or_default(),
            Match::Pause(p) => p.start,
        }
    }
}

/// Search settings: `wholeWords` / `matchCase`, else Transcript view options ▸ Search settings.
pub fn search_options(s: &Session, p: &Value) -> tx::SearchOptions {
    let t = &s.prefs.transcript;
    tx::SearchOptions { whole_words: bool_p(p, "wholeWords").unwrap_or(t.whole_words), match_case: bool_p(p, "matchCase").unwrap_or(t.match_case) }
}

/// The matches of a search over the sequence transcript, in time order.
pub fn matches(s: &Session, words: &[SeqWord], filter: Filter, query: &str, p: &Value) -> Vec<Match> {
    let words_match = |r: std::ops::Range<usize>| Some(Match::Words { from: r.start, to: r.end.checked_sub(1)? });
    match filter {
        Filter::Text => tx::search_with(words, query, search_options(s, p)).into_iter().filter_map(words_match).collect(),
        Filter::Fillers => filler_hits(s, words, p).into_iter().filter_map(words_match).collect(),
        Filter::Pauses => tx::pauses(words, min_pause(s, p)).into_iter().map(Match::Pause).collect(),
    }
}

fn match_json(words: &[SeqWord], m: &Match) -> Value {
    match m {
        Match::Words { from, to } => json!({
            "from": from, "to": to,
            "start": words.get(*from).map(|w| w.start.0), "end": words.get(*to).map(|w| w.end.0),
        }),
        Match::Pause(q) => json!({"pauseAfter": q.after, "start": q.start.0, "end": q.end.0, "seconds": q.duration().seconds()}),
    }
}

fn search(s: &mut Session, p: &Value) -> Result<Value> {
    let filter = filter_p(p, "transcript.search")?;
    let q = str_p(p, "query").unwrap_or_default();
    if filter == Filter::Text && q.trim().is_empty() {
        return Err(bad("transcript.search", "`query` is required"));
    }
    let words = sequence_words(s);
    let hits: Vec<Value> = matches(s, &words, filter, q, p).iter().map(|m| match_json(&words, m)).collect();
    Ok(json!({"filter": filter.name(), "count": hits.len(), "matches": hits}))
}

/// Timeline range of the words `from..=to` (frame-snapped outward), or of the pause after word
/// `pauseAfter` (the whole silence, frame-snapped inward).
fn range_p(s: &Session, p: &Value, cmd: &str) -> Result<TimeRange> {
    let words = sequence_words(s);
    if let Some(after) = u64_p(p, "pauseAfter") {
        let pause =
            usize::try_from(after).ok().and_then(|a| tx::pause_after(&words, a)).ok_or_else(|| bad(cmd, format!("there is no pause after word {after}")))?;
        return tx::pause_range(&pause, Tick::ZERO, s.sequence_rate()).ok_or_else(|| bad(cmd, "the pause is shorter than a frame"));
    }
    let from = u64_p(p, "from").ok_or_else(|| bad(cmd, "`from` (word index) or `pauseAfter` is required"))? as usize;
    let to = u64_p(p, "to").map(|n| n as usize).unwrap_or(from);
    tx::word_range(&words, from, to, s.sequence_rate()).ok_or_else(|| bad(cmd, format!("word index out of range (the transcript has {} words)", words.len())))
}

fn range_json(r: TimeRange) -> Value {
    json!({"start": r.start.0, "end": r.end().0})
}

fn select(s: &mut Session, p: &Value) -> Result<Value> {
    let r = range_p(s, p, "transcript.select")?;
    let fd = s.sequence_rate().frame_duration();
    s.edit_sequence("Mark Transcript Selection", |q, _, _| {
        q.mark_in = Some(r.start);
        q.mark_out = Some(r.end() - fd);
        Ok(())
    })?;
    s.set_playhead(r.start);
    Ok(range_json(r))
}

fn extract_or_lift(s: &mut Session, p: &Value, extract: bool) -> Result<Value> {
    let cmd = if extract { "transcript.extract" } else { "transcript.lift" };
    let r = range_p(s, p, cmd)?;
    let tg = s.targeting().targeted;
    s.edit_sequence(if extract { "Extract Text" } else { "Lift Text" }, |q, ctx, _| {
        if extract {
            edit::extract(q, &tg, r, ctx);
        } else {
            edit::lift(q, &tg, r, ctx);
        }
        q.mark_in = None;
        q.mark_out = None;
        Ok(())
    })?;
    s.set_playhead(r.start);
    Ok(range_json(r))
}

fn rename_speaker(s: &mut Session, p: &Value) -> Result<Value> {
    let name = str_p(p, "name").map(str::trim).filter(|n| !n.is_empty()).ok_or_else(|| bad("transcript.renameSpeaker", "`name` is required"))?.to_string();
    let item = u64_p(p, "item").map(ItemId).and_then(|i| media_item(s, i));
    // `speaker`: the current name (every transcript), or an index (needs `item`)
    let (old_name, index) = match p.get("speaker") {
        Some(Value::String(n)) => (Some(n.clone()), None),
        Some(v) if v.is_u64() => (None, v.as_u64().map(|n| n as usize)),
        _ => return Err(bad("transcript.renameSpeaker", "`speaker` (name, or index with `item`) is required")),
    };
    if index.is_some() && item.is_none() {
        return Err(bad("transcript.renameSpeaker", "a speaker index needs `item`"));
    }
    let mut n = 0;
    let mut next = s.project.transcripts.clone();
    for (i, t) in next.iter_mut() {
        if item.is_some_and(|x| x != *i) {
            continue;
        }
        let tt = Arc::make_mut(t);
        for (k, sp) in tt.speakers.iter_mut().enumerate() {
            if old_name.as_ref().is_some_and(|o| *o == sp.name) || index == Some(k) {
                sp.name = name.clone();
                n += 1;
            }
        }
    }
    if n == 0 {
        return Err(EngineError::Other("no such speaker".into()));
    }
    s.edit("Rename Speaker", move |pr, _| {
        pr.transcripts = next;
        Ok(())
    })?;
    Ok(json!({"renamed": n}))
}

fn remove_ranges(s: &mut Session, label: &str, ranges: Vec<TimeRange>) -> Result<Value> {
    let n = ranges.len();
    if n == 0 {
        return Ok(json!({"removed": 0, "ticks": 0}));
    }
    let total = s.edit_sequence(label, |q, ctx, _| Ok(tx::ripple_delete_ranges(q, ranges, ctx)))?;
    Ok(json!({"removed": n, "ticks": total.0, "seconds": total.0 as f64 / TICKS_PER_SECOND as f64}))
}

fn remove_pauses(s: &mut Session, p: &Value) -> Result<Value> {
    let words = sequence_words(s);
    let min = Tick::from_seconds_f64(f64_p(p, "minSeconds").unwrap_or(1.0));
    let keep = Tick::from_seconds_f64(f64_p(p, "keepSeconds").unwrap_or(0.15));
    let ranges = tx::find_pauses(&words, min, keep, s.sequence_rate());
    remove_ranges(s, "Remove Pauses", ranges)
}

fn remove_fillers(s: &mut Session, p: &Value) -> Result<Value> {
    let words = sequence_words(s);
    let hits = filler_hits(s, &words, p);
    let ranges = tx::filler_ranges(&words, &hits, s.sequence_rate());
    remove_ranges(s, "Remove Filler Words", ranges)
}

/// The timeline ranges that remove search matches: words frame-snapped outward (filler words to
/// the nearest frames, never into their neighbours), pauses whole and frame-snapped inward.
pub fn match_ranges(words: &[SeqWord], found: &[Match], filter: Filter, rate: filmcraft_time::FrameRate) -> Vec<TimeRange> {
    let spans: Vec<std::ops::Range<usize>> =
        found.iter().filter_map(|m| if let Match::Words { from, to } = m { Some(*from..to.saturating_add(1)) } else { None }).collect();
    let pauses = found.iter().filter_map(|m| if let Match::Pause(q) = m { tx::pause_range(q, Tick::ZERO, rate) } else { None });
    match filter {
        Filter::Fillers => tx::filler_ranges(words, &spans, rate),
        Filter::Text => spans.iter().filter_map(|r| tx::word_range(words, r.start, r.end.saturating_sub(1), rate)).collect(),
        Filter::Pauses => pauses.collect(),
    }
}

/// `transcript.deleteAll`: remove every match of a search at once, in one undo step: the words
/// found by `query`, every filler word or every pause (the whole silence). Extract (ripple, the
/// default) or lift (`lift: true`: leaves gaps), on every unlocked track.
fn delete_all(s: &mut Session, p: &Value) -> Result<Value> {
    let cmd = "transcript.deleteAll";
    let filter = filter_p(p, cmd)?;
    let q = str_p(p, "query").unwrap_or_default();
    if filter == Filter::Text && q.trim().is_empty() {
        return Err(bad(cmd, "`query` is required"));
    }
    let lift = bool_p(p, "lift").unwrap_or(false);
    let words = sequence_words(s);
    let found = matches(s, &words, filter, q, p);
    let ranges = match_ranges(&words, &found, filter, s.sequence_rate());
    let label = match (filter, lift) {
        (Filter::Fillers, false) => "Delete All Filler Words",
        (Filter::Pauses, false) => "Delete All Pauses",
        (Filter::Text, false) => "Delete All Matches",
        (Filter::Fillers, true) => "Lift All Filler Words",
        (Filter::Pauses, true) => "Lift All Pauses",
        (Filter::Text, true) => "Lift All Matches",
    };
    if !lift {
        let mut v = remove_ranges(s, label, ranges)?;
        v["filter"] = json!(filter.name());
        return Ok(v);
    }
    let ranges = tx::merge_ranges(ranges);
    let n = ranges.len();
    if n > 0 {
        s.edit_sequence(label, |q, ctx, _| {
            let tracks: Vec<TrackId> = q.all_tracks().filter(|t| !t.locked).map(|t| t.id).collect();
            for r in &ranges {
                edit::lift(q, &tracks, *r, ctx);
            }
            Ok(())
        })?;
    }
    Ok(json!({"removed": n, "filter": filter.name(), "lifted": true}))
}

fn create_captions(s: &mut Session, p: &Value) -> Result<Value> {
    let words = sequence_words(s);
    let d = CaptionRules::default();
    let rules = CaptionRules {
        max_chars: u64_p(p, "maxChars").map(|n| n as usize).unwrap_or(d.max_chars),
        lines: u64_p(p, "lines").map(|n| n as usize).unwrap_or(d.lines),
        min_duration: f64_p(p, "minSeconds").map(Tick::from_seconds_f64).unwrap_or(d.min_duration),
        max_duration: f64_p(p, "maxSeconds").map(Tick::from_seconds_f64).unwrap_or(d.max_duration),
        gap_frames: p.get("gapFrames").and_then(Value::as_i64).unwrap_or(d.gap_frames),
        break_pause: d.break_pause,
    };
    let blocks = tx::caption_blocks(&words, &rules, s.sequence_rate());
    let format = str_p(p, "format").and_then(CaptionFormat::from_name).unwrap_or_default();
    let name = str_p(p, "name").unwrap_or("Transcript").to_string();
    let n = blocks.len();
    let tid = s.edit_sequence("Create Captions", |q, ctx, st| {
        let tid = TrackId(ctx.alloc());
        let mut t = CaptionTrack::new(tid, name, format);
        t.captions = tx::blocks_to_captions(&blocks, ctx);
        q.caption_tracks.insert(0, t);
        st.caption_selection.clear();
        Ok(tid)
    })?;
    Ok(json!({"track": tid.0, "captions": n}))
}

fn models(_: &mut Session, _: &Value) -> Result<Value> {
    let dir = models_dir();
    Ok(json!({
        "available": filmcraft_speech::available(),
        "default": filmcraft_speech::models::DEFAULT_MODEL,
        "dir": dir.as_ref().map(|d| d.to_string_lossy().to_string()),
        "models": filmcraft_speech::models::catalogue().iter().map(|m| json!({
            "id": m.id, "name": m.name, "multilingual": m.multilingual, "description": m.description,
            "license": m.license, "licenseUrl": m.license_url, "attribution": m.attribution(), "source": m.source, "size": m.size(),
            "installed": dir.as_ref().is_some_and(|d| filmcraft_speech::models::installed(d, m)),
        })).collect::<Vec<_>>(),
    }))
}

/// Download a catalogue model into `<data dir>/models` (feature `speech-download`). Hosts show
/// the size, source and licence (`transcript.models`) and ask before running this.
fn download_model(_: &mut Session, p: &Value) -> Result<Value> {
    let id = str_p(p, "model").unwrap_or(filmcraft_speech::models::DEFAULT_MODEL);
    let m = filmcraft_speech::models::find(id).ok_or_else(|| speech_err(SpeechError::UnknownModel(id.into())))?;
    let dir = models_dir().ok_or_else(|| EngineError::Other("no data directory for speech models".into()))?;
    #[cfg(feature = "speech-download")]
    {
        filmcraft_speech::models::download(&dir, m, &mut |_, _, _| true).map_err(speech_err)?;
        Ok(json!({"model": m.id, "dir": filmcraft_speech::models::model_dir(&dir, m).to_string_lossy()}))
    }
    #[cfg(not(feature = "speech-download"))]
    {
        let _ = (m, dir);
        Err(EngineError::Other("model downloads are not available in this build (built without the `speech-download` feature)".into()))
    }
}

pub fn commands() -> Vec<CommandSpec> {
    vec![
        spec(
            "transcript.generate",
            "Transcribe…",
            &["Sequence", "Transcript"],
            r#"{"items":[id]?,"model":"whisper-base"?,"language":"en|auto"?,"diarize":bool?,"maxSpeakers":n?,"download":bool=false,"wait":bool=true}"#,
            can_transcribe,
            generate,
            true,
        ),
        spec("transcript.status", "Transcription Status", &[], "{}", always, status, false),
        spec("transcript.cancel", "Cancel Transcription", &[], "{}", is_transcribing, cancel, false),
        spec(
            "transcript.set",
            "Import Transcript",
            &[],
            r#"{"item":id,"transcript":{"language":str,"speakers":[{"name":str}],"words":[{"text":str,"start":tick,"end":tick,"speaker":n?}]}}"#,
            always,
            set,
            true,
        ),
        spec("transcript.delete", "Delete Transcript", &["Sequence", "Transcript"], r#"{"items":[id]?}"#, has_transcripts, delete, true),
        spec("transcript.inspect", "Inspect Transcript", &[], r#"{"paragraphGapSeconds":f?,"minPauseSeconds":f?}"#, always, inspect, false),
        spec(
            "transcript.search",
            "Search Transcript",
            &[],
            r#"{"query":str?,"filter":"text|fillers|pauses"=text,"wholeWords":bool?,"matchCase":bool?,"minPauseSeconds":f?,"fillers":[str]?}"#,
            always,
            search,
            false,
        ),
        spec(
            "transcript.deleteAll",
            "Delete All Matches",
            &[],
            r#"{"filter":"text|fillers|pauses"=text,"query":str?,"lift":bool=false,"wholeWords":bool?,"matchCase":bool?,"minPauseSeconds":f?,"fillers":[str]?}"#,
            has_transcript,
            delete_all,
            true,
        ),
        spec("transcript.models", "List Speech Models", &[], "{}", always, models, false),
        spec("transcript.downloadModel", "Download Speech Model", &[], r#"{"model":"whisper-base"?}"#, can_download, download_model, true),
        spec("transcript.select", "Mark Selected Text", &[], r#"{"from":word,"to":word?}|{"pauseAfter":word}"#, has_transcript, select, true),
        spec(
            "transcript.extract",
            "Extract Selected Text",
            &[],
            r#"{"from":word,"to":word?}|{"pauseAfter":word}"#,
            has_transcript,
            |s, p| extract_or_lift(s, p, true),
            true,
        ),
        spec(
            "transcript.lift",
            "Lift Selected Text",
            &[],
            r#"{"from":word,"to":word?}|{"pauseAfter":word}"#,
            has_transcript,
            |s, p| extract_or_lift(s, p, false),
            true,
        ),
        spec(
            "transcript.renameSpeaker",
            "Rename Speaker…",
            &[],
            r#"{"speaker":"Speaker 1"|index,"name":str,"item":id?}"#,
            has_transcripts,
            rename_speaker,
            true,
        ),
        spec(
            "transcript.removePauses",
            "Remove Pauses",
            &["Sequence", "Transcript"],
            r#"{"minSeconds":f?,"keepSeconds":f?}"#,
            has_transcript,
            remove_pauses,
            true,
        ),
        spec("transcript.removeFillers", "Remove Filler Words", &["Sequence", "Transcript"], r#"{"fillers":[str]?}"#, has_transcript, remove_fillers, true),
        spec(
            "transcript.createCaptions",
            "Create Captions from Transcript…",
            &["Sequence", "Transcript"],
            r#"{"maxChars":n?,"lines":1|2?,"minSeconds":f?,"maxSeconds":f?,"gapFrames":n?,"format":str?,"name":str?}"#,
            has_transcript,
            create_captions,
            true,
        ),
    ]
}
