//! Text-based editing commands (`transcript.*`): the Text panel ▸ Transcript tab.
//!
//! Transcripts belong to media items (`Project::transcripts`, media time); the sequence transcript
//! is derived from them ([`filmcraft_edit::transcript::sequence_words`]). Words of the sequence
//! transcript are addressed by index (`from`, `to`, inclusive), as `transcript.inspect` lists them.
//!
//! Speech recognition goes through a [`Transcriber`]: [`Session::transcriber`] when a host or a
//! test installed one, else the Whisper model named by `model` from `<data dir>/models` (needs the
//! engine feature `whisper`; without it `transcript.generate` fails with a clear error, and agents
//! can still bring their own transcript with `transcript.set`).

use std::sync::{Arc, Mutex};

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

/// The speech model Transcribe uses (Settings ▸ Media Analysis ▸ Speech model).
#[derive(Clone, Debug, PartialEq)]
pub struct ModelStatus {
    pub id: String,
    pub name: String,
    /// Downloaded and ready (always true for a recogniser a host installed).
    pub installed: bool,
    /// Bytes still to download.
    pub download_bytes: u64,
    pub license: String,
    pub source: String,
}

/// The model Transcribe would use, or `None` when this build can't transcribe.
pub fn model_status(s: &Session) -> Option<ModelStatus> {
    if let Some(t) = &s.transcriber {
        return Some(ModelStatus { id: t.id(), name: t.id(), installed: true, download_bytes: 0, license: String::new(), source: String::new() });
    }
    if !speech_available() {
        return None;
    }
    let m = filmcraft_speech::models::find(&s.prefs.media_analysis.speech_model)
        .or_else(|| filmcraft_speech::models::find(filmcraft_speech::models::DEFAULT_MODEL))?;
    let dir = models_dir();
    let installed = dir.as_ref().is_some_and(|d| filmcraft_speech::models::installed(d, m));
    let download_bytes = dir.as_ref().map(|d| filmcraft_speech::models::missing_bytes(d, m)).unwrap_or(m.size());
    Some(ModelStatus { id: m.id.into(), name: m.name.into(), installed, download_bytes, license: m.license.into(), source: m.source.into() })
}

/// Languages offered for transcription: (ISO 639-1 code, name).
pub fn languages() -> &'static [(&'static str, &'static str)] {
    crate::settings::LANGUAGES
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

/// Transcription options from the params, else Settings ▸ Media Analysis & Transcription.
fn options(s: &Session, p: &Value) -> Options {
    let ma = &s.prefs.media_analysis;
    let default_language = if ma.language_auto_detect { None } else { Some(ma.default_language.clone()) };
    Options {
        language: match str_p(p, "language") {
            Some(l) => Some(l).filter(|l| !l.is_empty() && *l != "auto").map(str::to_string),
            None => default_language,
        },
        diarize: bool_p(p, "diarize").unwrap_or(ma.speaker_labeling != "off"),
        max_speakers: u64_p(p, "maxSpeakers").map(|n| n.clamp(1, 32) as usize).unwrap_or(Options::default().max_speakers),
    }
}

/// A running transcription or voice analysis (Text panel ▸ Transcribe); [`poll`] stores its
/// transcripts as one undo step when it finishes.
pub struct PendingTranscription {
    pub job: u64,
    pub results: Arc<Mutex<Option<Vec<(ItemId, Transcript)>>>>,
    /// Voice analysis of existing transcripts only (no speech recognition).
    pub voice_only: bool,
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// One clip to transcribe: its media, read on the job's thread.
struct Work {
    item: ItemId,
    name: String,
    src: filmcraft_media::SharedSource,
    samples: usize,
    /// For voice analysis: the transcript to keep.
    existing: Option<Transcript>,
}

/// Where the recogniser comes from.
enum Recogniser {
    /// A host or test installed one.
    Installed(Arc<dyn Transcriber>),
    /// A catalogue model in `dir`, loaded (and first downloaded, with `download`) on the job thread.
    Model { id: String, dir: std::path::PathBuf, download: bool },
    /// Voice analysis only.
    None,
}

fn work_items(s: &Session, items: &[ItemId], existing: bool) -> (Vec<Work>, Vec<u64>) {
    let sr = filmcraft_speech::SAMPLE_RATE as i64;
    let (mut work, mut skipped) = (Vec::new(), Vec::new());
    for &item in items {
        let media = s.project.item(item).and_then(|i| match &i.kind {
            ItemKind::Media(m) => Some((i.name.clone(), m.duration())),
            _ => None,
        });
        let src = s.source(item).filter(|src| src.info().has_audio());
        let (Some((name, dur)), Some(src)) = (media, src) else {
            skipped.push(item.0);
            continue;
        };
        let existing = if existing { s.project.transcripts.get(&item).map(|t| (**t).clone()) } else { None };
        let samples = usize::try_from(dur.to_units_floor(sr).max(0)).unwrap_or(0);
        work.push(Work { item, name, src, samples, existing });
    }
    (work, skipped)
}

/// The recogniser for `transcript.generate`: the installed one, else the catalogue model named by
/// `model` or the settings (it must be downloaded, or `download: true` fetches it first).
fn recogniser(s: &Session, p: &Value) -> Result<Recogniser> {
    if let Some(t) = &s.transcriber {
        return Ok(Recogniser::Installed(t.clone()));
    }
    // Settings ▸ Media Analysis & Transcription ▸ Speech model
    let model = str_p(p, "model").unwrap_or(&s.prefs.media_analysis.speech_model).to_string();
    let m = filmcraft_speech::models::find(&model).ok_or_else(|| speech_err(SpeechError::UnknownModel(model.clone())))?;
    if !filmcraft_speech::available() {
        return Err(EngineError::Other(NO_SPEECH.into()));
    }
    let dir = models_dir().ok_or_else(|| EngineError::Other("no data directory for speech models".into()))?;
    let download = bool_p(p, "download").unwrap_or(false);
    let can_fetch = download && cfg!(feature = "speech-download");
    if !(filmcraft_speech::models::installed(&dir, m) || can_fetch) {
        return Err(speech_err(SpeechError::NotInstalled(model)));
    }
    Ok(Recogniser::Model { id: model, dir, download })
}

/// Run `work` as a job: recognise (unless voice-only), measure the voice in the waveform, snap
/// Whisper's words to it, and hand the transcripts to [`poll`]. With `wait` (the default; the UI
/// passes false) it runs here and the transcripts are stored before returning.
fn start(s: &mut Session, p: &Value, work: Vec<Work>, skipped: Vec<u64>, rec: Recogniser, opts: Options) -> Result<Value> {
    use std::sync::atomic::Ordering;
    let voice_only = matches!(rec, Recogniser::None);
    if s.transcribe_jobs.iter().any(|j| s.jobs.iter().any(|x| x.id == j.job && !x.progress.finished.load(Ordering::Relaxed))) {
        return Err(EngineError::Other("a transcription is already running (cancel it first)".into()));
    }
    let id = s.jobs.iter().map(|j| j.id).max().unwrap_or(0) + 1;
    let n = work.len();
    let label = if voice_only {
        format!("Find pauses ({n} clip{})", if n == 1 { "" } else { "s" })
    } else {
        format!("Transcribe ({n} clip{})", if n == 1 { "" } else { "s" })
    };
    let job = crate::Job { id, label, progress: Default::default(), result: Default::default() };
    const TOTAL: u64 = 1000;
    job.progress.total.store(TOTAL, Ordering::Relaxed);
    let results: Arc<Mutex<Option<Vec<(ItemId, Transcript)>>>> = Arc::default();
    let (prog, res, out) = (job.progress.clone(), job.result.clone(), results.clone());
    let run = move || {
        let t0 = web_time::Instant::now();
        let status = |m: String| *lock(&prog.status) = m;
        let cancelled = || prog.cancel.load(Ordering::Relaxed);
        let mut err: Option<String> = None;
        // the recogniser, downloaded and loaded on this thread
        let t: Option<Arc<dyn Transcriber>> = match rec {
            Recogniser::Installed(t) => Some(t),
            Recogniser::None => None,
            Recogniser::Model { id, dir, download } => {
                let m = filmcraft_speech::models::find(&id);
                #[cfg(feature = "speech-download")]
                if download && let Some(m) = m.filter(|m| !filmcraft_speech::models::installed(&dir, m)) {
                    status(format!("Downloading {} ({:.1} GB)", m.name, m.size() as f64 / 1e9));
                    let r = filmcraft_speech::models::download(&dir, m, &mut |done, total, _| {
                        prog.done.store(done.saturating_mul(TOTAL / 4) / total.max(1), Ordering::Relaxed);
                        !cancelled()
                    });
                    if let Err(e) = r {
                        err = Some(if cancelled() { "stopped".into() } else { e.to_string() });
                    }
                }
                #[cfg(not(feature = "speech-download"))]
                let _ = download;
                if err.is_none() {
                    status(format!("Loading {}", m.map(|m| m.name).unwrap_or(id.as_str())));
                    match filmcraft_speech::load(&dir, &id) {
                        Ok(t) => Some(t),
                        Err(e) => {
                            err = Some(e.to_string());
                            None
                        }
                    }
                } else {
                    None
                }
            }
        };
        let base = if prog.done.load(Ordering::Relaxed) > 0 { TOTAL / 4 } else { 0 };
        let per = (TOTAL - base) / (n.max(1) as u64);
        let mut done = Vec::new();
        for (k, w) in work.into_iter().enumerate() {
            if err.is_some() {
                break;
            }
            if cancelled() {
                err = Some("stopped".into());
                break;
            }
            let at = base + per * k as u64;
            prog.done.store(at, Ordering::Relaxed);
            status(format!("{}: reading the audio", w.name));
            let audio = match w.src.audio(0, w.samples, filmcraft_speech::SAMPLE_RATE) {
                Ok(b) => filmcraft_speech::downmix(&b.channels),
                Err(e) => {
                    err = Some(format!("{}: {e}", w.name));
                    break;
                }
            };
            let mut tr = match (&t, w.existing) {
                (Some(t), _) => {
                    let mut cb = |f: f32, st: &str| {
                        prog.done.store(at + (per as f32 * 0.95 * f.clamp(0.0, 1.0)) as u64, Ordering::Relaxed);
                        *lock(&prog.status) = format!("{}: {st}", w.name);
                        !cancelled()
                    };
                    match t.transcribe(&audio, &opts, &mut cb) {
                        Ok(tr) => tr,
                        Err(e) => {
                            err = Some(if cancelled() { "stopped".into() } else { format!("{}: {e}", w.name) });
                            break;
                        }
                    }
                }
                (None, Some(tr)) => tr,
                (None, None) => continue,
            };
            status(format!("{}: finding pauses", w.name));
            // where the waveform has voice: the pauses come from here, not from word gaps; Whisper's
            // words (aligned by attention, which lets the word after a pause swallow it) snap to it
            let voice = filmcraft_speech::voice::voice_map(&audio, &tr.words);
            if tr.source.starts_with("whisper") {
                filmcraft_speech::voice::snap_words(&mut tr.words, &voice);
            }
            tr.voice = voice.spans.iter().map(|r| (r.start, r.end())).collect();
            tr.normalize();
            done.push((w.item, tr));
        }
        let secs = t0.elapsed().as_secs_f64();
        let words: usize = done.iter().map(|(_, t)| t.words.len()).sum();
        status(match &err {
            Some(e) if e == "stopped" => "Stopped: nothing was changed".into(),
            Some(e) => e.clone(),
            None if voice_only => format!("Found the pauses in {} clip(s) ({secs:.1}s)", done.len()),
            None => format!("Transcribed {words} words in {} clip(s) ({secs:.1}s)", done.len()),
        });
        let r = match err {
            Some(e) => Err(e),
            None => {
                *lock(&out) = Some(done);
                Ok(filmcraft_export::Report { path: String::new(), frames: words as u64, seconds: secs, bytes: 0, render_fps: 0.0, extra_files: Vec::new() })
            }
        };
        prog.done.store(TOTAL, Ordering::Relaxed);
        *lock(&res) = Some(r);
        prog.finished.store(true, Ordering::Relaxed);
    };
    let run = crate::export_tools::guard_job(job.progress.clone(), job.result.clone(), run);
    s.jobs.push(job);
    s.transcribe_jobs.push(PendingTranscription { job: id, results: results.clone(), voice_only });
    let wait = bool_p(p, "wait").unwrap_or(true);
    if !wait && !cfg!(target_arch = "wasm32") {
        std::thread::Builder::new().name("filmcraft-transcribe".into()).spawn(run).map_err(|e| EngineError::Other(e.to_string()))?;
        return Ok(json!({"job": id, "items": n, "skipped": skipped}));
    }
    run();
    let report: Vec<Value> = lock(&results)
        .as_ref()
        .map(|r| r.iter().map(|(i, t)| json!({"item": i.0, "words": t.words.len(), "speakers": t.speakers.len(), "language": t.language, "source": t.source, "voiceSpans": t.voice.len()})).collect())
        .unwrap_or_default();
    poll(s);
    if let Some(Err(e)) = s.jobs.iter().find(|j| j.id == id).and_then(|j| lock(&j.result).clone()) {
        return Err(EngineError::Other(e));
    }
    if report.is_empty() {
        return Err(EngineError::Other("none of the clips has audio to transcribe".into()));
    }
    Ok(json!({"job": id, "items": report, "skipped": skipped}))
}

/// Store finished transcriptions (one undo step each). Called every UI frame from
/// [`Session::poll_persistence`] and after a run with `wait`.
pub fn poll(s: &mut Session) {
    use std::sync::atomic::Ordering;
    let mut i = 0;
    while i < s.transcribe_jobs.len() {
        let job = s.jobs.iter().find(|j| j.id == s.transcribe_jobs[i].job);
        let finished = job.is_none_or(|j| j.progress.finished.load(Ordering::Relaxed));
        let cancelled = job.is_some_and(|j| j.progress.cancel.load(Ordering::Relaxed));
        if !finished {
            i += 1;
            continue;
        }
        let pj = s.transcribe_jobs.remove(i);
        let Some(done) = lock(&pj.results).take().filter(|d| !cancelled && !d.is_empty()) else { continue };
        let label = if pj.voice_only { "Find Pauses" } else { "Transcribe" };
        let r = s.edit(label, move |pr, _| {
            for (item, t) in done {
                pr.transcripts.insert(item, Arc::new(t));
            }
            Ok(())
        });
        if let Err(e) = r {
            s.error_toast("transcript.generate", format!("Transcribe: {e}"));
        }
    }
}

/// The running transcription job, if any: (job id, fraction done, status).
pub fn running(s: &Session) -> Option<(u64, f32, String)> {
    use std::sync::atomic::Ordering;
    s.transcribe_jobs.iter().find_map(|pj| {
        let j = s.jobs.iter().find(|j| j.id == pj.job)?;
        (!j.progress.finished.load(Ordering::Relaxed)).then(|| (j.id, j.progress.fraction(), lock(&j.progress.status).clone()))
    })
}

fn generate(s: &mut Session, p: &Value) -> Result<Value> {
    let items = targets(s, p);
    if items.is_empty() {
        return Err(bad("transcript.generate", "nothing to transcribe (pass `items`, select clips, or open a sequence with audio)"));
    }
    let rec = recogniser(s, p)?;
    let opts = options(s, p);
    let (work, skipped) = work_items(s, &items, false);
    if work.is_empty() {
        return Err(EngineError::Other("none of the clips has audio to transcribe".into()));
    }
    start(s, p, work, skipped, rec, opts)
}

/// `transcript.findPauses`: measure the voice of transcripts that have none (imported, or made
/// before voice analysis), so their pauses come from the waveform too.
fn find_pauses_cmd(s: &mut Session, p: &Value) -> Result<Value> {
    let items: Vec<ItemId> = match ids_p(p, "items") {
        Some(v) => v.into_iter().filter_map(|i| media_item(s, i)).filter(|i| s.project.transcripts.contains_key(i)).collect(),
        None => s.project.transcripts.iter().filter(|(_, t)| t.voice.is_empty()).map(|(i, _)| *i).collect(),
    };
    if items.is_empty() {
        return Ok(json!({"items": 0}));
    }
    let (work, skipped) = work_items(s, &items, true);
    if work.is_empty() {
        return Err(EngineError::Other("none of the transcribed clips has audio to analyse".into()));
    }
    start(s, p, work, skipped, Recogniser::None, Options::default())
}

/// `transcript.cancel`: stop the running transcription (nothing changes).
fn cancel(s: &mut Session, _: &Value) -> Result<Value> {
    use std::sync::atomic::Ordering;
    let (id, _, _) = running(s).ok_or_else(|| EngineError::Other("no transcription is running".into()))?;
    if let Some(j) = s.jobs.iter().find(|j| j.id == id) {
        j.progress.cancel.store(true, Ordering::Relaxed);
    }
    Ok(json!({"job": id}))
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

fn word_json(i: usize, w: &SeqWord) -> Value {
    json!({"i": i, "text": w.text, "start": w.start.0, "end": w.end.0, "speaker": w.speaker, "clip": w.clip.0, "item": w.item.0, "confidence": w.confidence})
}

fn inspect(s: &mut Session, p: &Value) -> Result<Value> {
    let words = sequence_words(s);
    let gap = Tick::from_seconds_f64(f64_p(p, "paragraphGapSeconds").unwrap_or(1.5));
    let paras: Vec<Value> = tx::paragraphs(&words, gap)
        .into_iter()
        .map(|r| {
            let text = words[r.clone()].iter().map(|w| w.text.as_str()).collect::<Vec<_>>().join(" ");
            json!({"from": r.start, "to": r.end - 1, "speaker": words[r.start].speaker, "start": words[r.start].start.0, "end": words[r.end - 1].end.0, "text": text})
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
    let current = tx::word_at(&words, s.playhead());
    let (min, after, before) = pause_params(s, p);
    let v = view(s, Some(min));
    let rate = s.sequence_rate();
    let pauses: Vec<Value> = v
        .pauses
        .iter()
        .enumerate()
        .map(|(i, pz)| {
            let cut = tx::pause_cut(pz, after, before, rate);
            json!({"i": i, "start": pz.range.start.0, "end": pz.range.end().0, "seconds": pz.range.duration.0 as f64 / TICKS_PER_SECOND as f64, "before": pz.before, "after": pz.after, "cut": cut.map(|c| json!({"start": c.start.0, "end": c.end().0}))})
        })
        .collect();
    let tokens: Vec<Value> = v
        .tokens
        .iter()
        .map(|t| match t {
            tx::Token::Word(i) => json!({"word": i}),
            tx::Token::Pause(i) => json!({"pause": i}),
            tx::Token::Speech(i) => json!({"speech": i}),
        })
        .collect();
    let running = running(s).map(|(job, f, st)| json!({"job": job, "progress": f, "status": st}));
    Ok(json!({
        "pauses": pauses,
        "speech": v.speech.iter().map(|r| json!({"start": r.start.0, "end": r.end().0})).collect::<Vec<_>>(),
        "tokens": tokens,
        "untranscribed": v.untranscribed,
        "withoutVoice": v.without_voice,
        "running": running,
        "words": words.iter().enumerate().map(|(i, w)| word_json(i, w)).collect::<Vec<_>>(),
        "paragraphs": paras,
        "speakers": speakers,
        "current": current,
        "items": s.project.transcripts.iter().map(|(i, t)| json!({"item": i.0, "words": t.words.len(), "language": t.language, "source": t.source, "speakers": t.speakers.iter().map(|k| &k.name).collect::<Vec<_>>()})).collect::<Vec<_>>(),
    }))
}

fn search(s: &mut Session, p: &Value) -> Result<Value> {
    let q = str_p(p, "query").ok_or_else(|| bad("transcript.search", "`query` is required"))?;
    let words = sequence_words(s);
    let hits: Vec<Value> = tx::search(&words, q)
        .into_iter()
        .map(|r| json!({"from": r.start, "to": r.end - 1, "start": words[r.start].start.0, "end": words[r.end - 1].end.0}))
        .collect();
    Ok(json!({"matches": hits}))
}

/// Timeline range of the words `from..=to` (frame-snapped outward).
fn range_p(s: &Session, p: &Value, cmd: &str) -> Result<TimeRange> {
    let words = sequence_words(s);
    let from = u64_p(p, "from").ok_or_else(|| bad(cmd, "`from` (word index) is required"))? as usize;
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

/// What the Text panel shows for the active sequence: the words, the pauses (silences of at least
/// the pause length, measured from the waveform) and the voice no word covers, in time order.
#[derive(Clone, Debug, Default)]
pub struct TranscriptView {
    pub words: Vec<SeqWord>,
    pub pauses: Vec<tx::Pause>,
    /// Voice with no word on it (a stutter or restart the recogniser tidied away).
    pub speech: Vec<TimeRange>,
    pub tokens: Vec<tx::Token>,
    /// Enabled audio clips of the sequence with no transcript.
    pub untranscribed: usize,
    /// Transcribed clips whose transcript has no voice map (their pauses are word gaps until
    /// `transcript.findPauses` measures them).
    pub without_voice: usize,
}

/// Pause settings: (shortest pause, kept after a word, kept before the next one), from `p`
/// (`minSeconds`, `keepAfterSeconds`, `keepBeforeSeconds`; `keepSeconds` sets both) or the
/// settings.
fn pause_params(s: &Session, p: &Value) -> (Tick, Tick, Tick) {
    let ma = &s.prefs.media_analysis;
    let ms = |v: u32| Tick::from_seconds_f64(f64::from(v) / 1000.0);
    let sec = |k: &str| f64_p(p, k).filter(|v| v.is_finite() && *v >= 0.0).map(Tick::from_seconds_f64);
    let keep = sec("keepSeconds");
    (
        sec("minSeconds").unwrap_or(ms(ma.pause_min_ms)).max(Tick(1)),
        sec("keepAfterSeconds").or(keep).unwrap_or(ms(ma.pause_keep_after_ms)),
        sec("keepBeforeSeconds").or(keep).unwrap_or(ms(ma.pause_keep_before_ms)),
    )
}

/// The Text panel's view of the active sequence (pause length from the settings, or `min`).
pub fn view(s: &Session, min: Option<Tick>) -> TranscriptView {
    let Some(q) = s.active_sequence() else { return TranscriptView::default() };
    let words = tx::sequence_words(q, &s.project.transcripts);
    let voice = tx::sequence_voice(q, &s.project.transcripts);
    let min = min.unwrap_or_else(|| pause_params(s, &json!({})).0);
    let pauses = tx::find_voice_pauses(&words, &voice, min);
    let speech = tx::unlabelled_speech(&words, &voice, Tick::from_seconds_f64(0.1));
    let tokens = tx::layout(&words, &pauses, &speech);
    let mut untranscribed = 0;
    let mut without_voice = 0;
    let mut seen = Vec::new();
    for it in q.audio_tracks.iter().flat_map(|t| t.items.iter()).filter(|i| i.enabled) {
        let root = media_item(s, it.item).unwrap_or(it.item);
        if seen.contains(&root) {
            continue;
        }
        seen.push(root);
        match s.project.transcripts.get(&root) {
            None => untranscribed += 1,
            Some(t) if t.voice.is_empty() && !t.words.is_empty() => without_voice += 1,
            Some(_) => {}
        }
    }
    TranscriptView { words, pauses, speech, tokens, untranscribed, without_voice }
}

/// What the Text panel's search filter looks for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Filter {
    Text,
    Fillers,
    Pauses,
}

impl Filter {
    pub fn from_name(s: &str) -> Option<Filter> {
        match s {
            "text" | "Text" | "transcript" => Some(Filter::Text),
            "fillers" | "Fillers" | "filler" | "fillerWords" => Some(Filter::Fillers),
            "pauses" | "Pauses" | "pause" => Some(Filter::Pauses),
            _ => None,
        }
    }
}

/// One search result: words `words` (Text, Fillers) or pause `pause` (Pauses); `range` is what it
/// covers in the sequence, `cut` what Delete removes.
#[derive(Clone, Debug, PartialEq)]
pub struct Hit {
    pub words: Option<std::ops::Range<usize>>,
    pub pause: Option<usize>,
    pub range: TimeRange,
    pub cut: Option<TimeRange>,
}

/// The results of `filter` (with `query` for Text) in time order.
pub fn hits(s: &Session, v: &TranscriptView, filter: Filter, query: &str, p: &Value) -> Vec<Hit> {
    let rate = s.sequence_rate();
    let (_, after, before) = pause_params(s, p);
    let words_hit = |r: std::ops::Range<usize>, cut: Option<TimeRange>| {
        let (a, b) = (v.words.get(r.start).map(|w| w.start).unwrap_or_default(), v.words.get(r.end.saturating_sub(1)).map(|w| w.end).unwrap_or_default());
        Hit { range: TimeRange::from_bounds(a, b.max(a)), words: Some(r), pause: None, cut }
    };
    match filter {
        Filter::Text => tx::search(&v.words, query)
            .into_iter()
            .map(|r| {
                let cut = tx::word_range(&v.words, r.start, r.end.saturating_sub(1), rate);
                words_hit(r, cut)
            })
            .collect(),
        Filter::Fillers => {
            let fillers: Vec<String> = match p.get("fillers").and_then(Value::as_array) {
                Some(a) => a.iter().filter_map(Value::as_str).map(str::to_string).collect(),
                None => tx::DEFAULT_FILLERS.iter().map(|f| f.to_string()).collect(),
            };
            let found = tx::find_fillers(&v.words, &fillers);
            let cuts = tx::filler_ranges(&v.words, &found, rate);
            // filler_ranges drops a filler shorter than a frame: pair cuts by position
            found
                .into_iter()
                .map(|r| {
                    let a = v.words.get(r.start).map(|w| w.start).unwrap_or_default();
                    let cut = cuts
                        .iter()
                        .find(|c| c.overlaps(&TimeRange::from_bounds(a, v.words.get(r.end.saturating_sub(1)).map(|w| w.end).unwrap_or(a).max(a))))
                        .copied();
                    words_hit(r, cut)
                })
                .collect()
        }
        Filter::Pauses => v
            .pauses
            .iter()
            .enumerate()
            .map(|(i, pz)| Hit { words: None, pause: Some(i), range: pz.range, cut: tx::pause_cut(pz, after, before, rate) })
            .collect(),
    }
}

fn hit_json(i: usize, h: &Hit) -> Value {
    json!({
        "i": i,
        "from": h.words.as_ref().map(|r| r.start), "to": h.words.as_ref().map(|r| r.end.saturating_sub(1)),
        "pause": h.pause,
        "start": h.range.start.0, "end": h.range.end().0, "seconds": h.range.duration.0 as f64 / TICKS_PER_SECOND as f64,
        "cut": h.cut.map(|c| json!({"start": c.start.0, "end": c.end().0})),
    })
}

fn filter_p(p: &Value, cmd: &str) -> Result<Filter> {
    match str_p(p, "filter") {
        None => Ok(Filter::Text),
        Some(f) => Filter::from_name(f).ok_or_else(|| bad(cmd, format!("unknown `filter` `{f}` (text, fillers, pauses)"))),
    }
}

/// `transcript.find`: the Text panel's search with its filter (Transcript text / Filler words /
/// Pauses).
fn find(s: &mut Session, p: &Value) -> Result<Value> {
    let filter = filter_p(p, "transcript.find")?;
    let query = str_p(p, "query").unwrap_or("");
    let min = pause_params(s, p).0;
    let v = view(s, Some(min));
    let h = hits(s, &v, filter, query, p);
    Ok(json!({"hits": h.iter().enumerate().map(|(i, h)| hit_json(i, h)).collect::<Vec<_>>()}))
}

/// `transcript.delete…` on search results: one result (`hit`, its index in `transcript.find`) or
/// all of them, extracted (closing the gaps; the default) or lifted. One undo step.
fn delete_hits(s: &mut Session, p: &Value) -> Result<Value> {
    let cmd = "transcript.deleteHits";
    let filter = filter_p(p, cmd)?;
    let query = str_p(p, "query").unwrap_or("");
    let lift = match str_p(p, "mode") {
        None | Some("extract") => false,
        Some("lift") => true,
        Some(m) => return Err(bad(cmd, format!("unknown `mode` `{m}` (extract, lift)"))),
    };
    let v = view(s, Some(pause_params(s, p).0));
    let all = hits(s, &v, filter, query, p);
    let chosen: Vec<&Hit> = match u64_p(p, "hit") {
        Some(i) => vec![all.get(i as usize).ok_or_else(|| bad(cmd, format!("no result {i} (there are {})", all.len())))?],
        None => all.iter().collect(),
    };
    let ranges: Vec<TimeRange> = chosen.iter().filter_map(|h| h.cut).collect();
    if ranges.is_empty() {
        return Err(EngineError::Other(match filter {
            Filter::Pauses if !all.is_empty() => "these pauses are shorter than the kept margins and a frame: nothing to delete".into(),
            Filter::Pauses => "no pauses found (lower the pause length to find shorter ones)".into(),
            Filter::Fillers => "no filler words found".into(),
            Filter::Text => "nothing matches the search".into(),
        }));
    }
    let what = match filter {
        Filter::Pauses => "Pauses",
        Filter::Fillers => "Filler Words",
        Filter::Text => "Text",
    };
    let n = ranges.len();
    let label = format!("{} {what}", if lift { "Lift" } else { "Delete" });
    let total = s.edit_sequence(&label, |q, ctx, _| Ok(if lift { tx::lift_ranges(q, ranges, ctx) } else { tx::ripple_delete_ranges(q, ranges, ctx) }))?;
    Ok(json!({"removed": n, "ticks": total.0, "seconds": total.0 as f64 / TICKS_PER_SECOND as f64, "mode": if lift { "lift" } else { "extract" }}))
}

fn remove_ranges(s: &mut Session, label: &str, ranges: Vec<TimeRange>) -> Result<Value> {
    let n = ranges.len();
    if n == 0 {
        return Ok(json!({"removed": 0, "ticks": 0}));
    }
    let total = s.edit_sequence(label, |q, ctx, _| Ok(tx::ripple_delete_ranges(q, ranges, ctx)))?;
    Ok(json!({"removed": n, "ticks": total.0, "seconds": total.0 as f64 / TICKS_PER_SECOND as f64}))
}

/// Remove Pauses: every pause of at least the pause length, from the waveform's voice (see
/// [`view`]), keeping the margins from the settings (or `minSeconds`, `keepAfterSeconds`,
/// `keepBeforeSeconds`, `keepSeconds`).
fn remove_pauses(s: &mut Session, p: &Value) -> Result<Value> {
    let (min, after, before) = pause_params(s, p);
    let rate = s.sequence_rate();
    let ranges: Vec<TimeRange> = view(s, Some(min)).pauses.iter().filter_map(|pz| tx::pause_cut(pz, after, before, rate)).collect();
    remove_ranges(s, "Remove Pauses", ranges)
}

fn remove_fillers(s: &mut Session, p: &Value) -> Result<Value> {
    let words = sequence_words(s);
    let fillers: Vec<String> = match p.get("fillers").and_then(Value::as_array) {
        Some(a) => a.iter().filter_map(Value::as_str).map(str::to_string).collect(),
        None => tx::DEFAULT_FILLERS.iter().map(|f| f.to_string()).collect(),
    };
    let hits = tx::find_fillers(&words, &fillers);
    let ranges = tx::filler_ranges(&words, &hits, s.sequence_rate());
    remove_ranges(s, "Remove Filler Words", ranges)
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
            "license": m.license, "source": m.source, "size": m.size(),
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
            r#"{"items":[id]?,"model":"whisper-large-v3-turbo"?,"language":"en|auto"?,"diarize":bool?,"maxSpeakers":n?,"download":bool?,"wait":bool?}"#,
            can_transcribe,
            generate,
            true,
        ),
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
        spec("transcript.inspect", "Inspect Transcript", &[], r#"{"paragraphGapSeconds":f?}"#, always, inspect, false),
        spec("transcript.search", "Search Transcript", &[], r#"{"query":str}"#, always, search, false),
        spec("transcript.models", "List Speech Models", &[], "{}", always, models, false),
        spec("transcript.downloadModel", "Download Speech Model", &[], r#"{"model":"whisper-base"?}"#, can_download, download_model, true),
        spec("transcript.select", "Mark Selected Text", &[], r#"{"from":word,"to":word?}"#, has_transcript, select, true),
        spec("transcript.extract", "Extract Selected Text", &[], r#"{"from":word,"to":word?}"#, has_transcript, |s, p| extract_or_lift(s, p, true), true),
        spec("transcript.lift", "Lift Selected Text", &[], r#"{"from":word,"to":word?}"#, has_transcript, |s, p| extract_or_lift(s, p, false), true),
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
            r#"{"minSeconds":f?,"keepAfterSeconds":f?,"keepBeforeSeconds":f?,"keepSeconds":f?}"#,
            has_transcript,
            remove_pauses,
            true,
        ),
        spec("transcript.find", "Find in Transcript", &[], r#"{"filter":"text"|"fillers"|"pauses","query":str?,"minSeconds":f?}"#, always, find, false),
        spec(
            "transcript.deleteHits",
            "Delete Search Results",
            &[],
            r#"{"filter":"text"|"fillers"|"pauses","query":str?,"hit":n?,"mode":"extract"|"lift"?,"minSeconds":f?,"keepAfterSeconds":f?,"keepBeforeSeconds":f?}"#,
            has_transcript,
            delete_hits,
            true,
        ),
        spec("transcript.findPauses", "Find Pauses in Transcribed Clips", &[], r#"{"items":[id]?,"wait":bool?}"#, has_transcripts, find_pauses_cmd, true),
        spec("transcript.cancel", "Stop Transcribing", &[], "{}", always, cancel, false),
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
