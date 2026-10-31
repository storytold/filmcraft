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

use std::collections::{BTreeSet, HashSet};
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use filmcraft_edit as edit;
use filmcraft_edit::transcript::{self as tx, CaptionRules, SeqWord};
use filmcraft_project::{CaptionFormat, CaptionStyle, CaptionTrack, ClipId, ItemId, ItemKind, Project, TrackId, Transcript};
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
        Some(q) => {
            let mut transcripts = s.project.transcripts.clone();
            for it in q.audio_tracks.iter().flat_map(|t| &t.items) {
                if let Some((root, _, _)) = s.project.resolve_media(it.item)
                    && let Some(tr) = s.project.transcripts.get(&root)
                {
                    transcripts.insert(it.item, tr.clone());
                }
            }
            let mut words = tx::sequence_words(q, &transcripts);
            for word in &mut words {
                if let Some((root, _, _)) = s.project.resolve_media(word.item) {
                    word.item = root;
                }
            }
            words
        }
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
pub fn target_items(s: &Session, p: &Value) -> Vec<ItemId> {
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

/// What to transcribe for `item`: its source and how many samples (at the speech rate) it has.
/// None when it has no audio.
fn item_source(s: &Session, item: ItemId) -> Result<Option<(filmcraft_media::SharedSource, usize)>> {
    let Some(project_item) = s.project.item(item) else { return Ok(None) };
    let dur = match &project_item.kind {
        ItemKind::Media(m) => m.duration(),
        _ => return Ok(None),
    };
    let Some(src) = s.source(item) else { return Ok(None) };
    if !src.info().has_audio() {
        return Ok(None);
    }
    let samples = dur.to_units_floor(i64::from(filmcraft_speech::SAMPLE_RATE));
    // The Transcriber interface takes contiguous samples. Bound that allocation until it can
    // accept a streaming source; chunked decoding alone does not bound the retained audio.
    if !(1..=i64::from(filmcraft_speech::SAMPLE_RATE) * 3600).contains(&samples) {
        return Err(bad("transcript.generate", "transcription supports up to one hour of audio per media item; split longer recordings first"));
    }
    let len = usize::try_from(samples).map_err(|_| bad("transcript.generate", "audio sample count is out of range"))?;
    Ok(Some((src, len)))
}

/// The mono speech-rate samples of a source (decoded here: in the job's thread when in the background).
fn decode_audio(src: &filmcraft_media::SharedSource, len: usize, prog: &filmcraft_export::Progress) -> std::result::Result<Vec<f32>, String> {
    let mut audio = Vec::new();
    audio.try_reserve_exact(len).map_err(|e| format!("not enough memory for transcription: {e}"))?;
    let chunk = filmcraft_speech::SAMPLE_RATE as usize * 30;
    while audio.len() < len {
        if prog.cancel.load(std::sync::atomic::Ordering::Relaxed) {
            return Err("stopped: nothing was changed".into());
        }
        let count = chunk.min(len - audio.len());
        let buf = src.audio(audio.len() as i64, count, filmcraft_speech::SAMPLE_RATE).map_err(|e| e.to_string())?;
        let mono = filmcraft_speech::downmix(&buf.channels);
        if mono.len() != count {
            return Err("audio decoder returned an incomplete transcription chunk".into());
        }
        audio.extend(mono);
    }
    Ok(audio)
}

fn speech_err(e: SpeechError) -> EngineError {
    EngineError::Other(e.to_string())
}

/// The transcriber to use: the installed one, else the named catalogue model.
/// Where the recogniser comes from: a host's, installed in the session, or a catalogue model that
/// is loaded where the work runs (the job's thread for a background transcription: loading
/// Whisper takes a while and must not hold up the editor).
enum TranscriberSource {
    Installed(Arc<dyn Transcriber>),
    Model { dir: std::path::PathBuf, id: String },
    Speakers,
}

impl TranscriberSource {
    fn load(self) -> std::result::Result<Option<Arc<dyn Transcriber>>, String> {
        match self {
            TranscriberSource::Installed(t) => Ok(Some(t)),
            TranscriberSource::Speakers => Ok(None),
            TranscriberSource::Model { dir, id } => filmcraft_speech::load(&dir, &id).map(Some).map_err(|e| e.to_string()),
        }
    }
}

/// The recogniser to use: the installed one, else the named catalogue model, checked here (an
/// unknown model or a build without speech-to-text fails at once) but not loaded yet.
fn transcriber(s: &Session, p: &Value) -> Result<TranscriberSource> {
    if let Some(t) = &s.transcriber {
        return Ok(TranscriberSource::Installed(t.clone()));
    }
    // Settings ▸ Media Analysis & Transcription ▸ Speech model
    let model = str_p(p, "model").unwrap_or(&s.prefs.media_analysis.whisper_model);
    if filmcraft_speech::models::find(model).is_none() {
        return Err(speech_err(SpeechError::UnknownModel(model.into())));
    }
    if !filmcraft_speech::available() {
        return Err(EngineError::Other(NO_SPEECH.into()));
    }
    let dir = models_dir().ok_or_else(|| EngineError::Other("no data directory for speech models".into()))?;
    Ok(TranscriberSource::Model { dir, id: model.to_string() })
}

fn generate(s: &mut Session, p: &Value) -> Result<Value> {
    start(s, p, false)
}

fn identify_speakers(s: &mut Session, p: &Value) -> Result<Value> {
    let cmd = "transcript.identifySpeakers";
    if !p.is_object()
        || p.get("items").is_some_and(|v| v.as_array().is_none_or(|a| a.is_empty() || a.len() > 100_000 || a.iter().any(|i| i.as_u64().is_none())))
        || p.get("item").is_some_and(|i| i.as_u64().is_none())
        || p.get("maxSpeakers").is_some_and(|v| v.as_u64().is_none_or(|n| !(1..=32).contains(&n)))
        || p.get("wait").is_some_and(|v| !v.is_boolean())
    {
        return Err(bad(cmd, "invalid items, speaker limit or wait flag"));
    }
    start(s, p, true)
}

fn start(s: &mut Session, p: &Value, speakers_only: bool) -> Result<Value> {
    if !s.transcribe_jobs.is_empty() {
        return Err(bad("transcript.generate", "transcription is already running; wait for completion or cancel it in Progress"));
    }
    let items = target_items(s, p);
    if items.is_empty() {
        return Err(bad("transcript.generate", "nothing to transcribe (pass `items`, select clips, or open a sequence with audio)"));
    }
    let t = if speakers_only { TranscriberSource::Speakers } else { transcriber(s, p)? };
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
    let mut work = Vec::new();
    let mut skipped = Vec::new();
    for item in items {
        match item_source(s, item)? {
            Some((src, len)) => {
                let name = s.project.item(item).map(|i| i.name.clone()).unwrap_or_default();
                let transcript = if speakers_only {
                    Some(
                        s.project
                            .transcripts
                            .get(&item)
                            .filter(|t| !t.words.is_empty())
                            .ok_or_else(|| bad("transcript.identifySpeakers", "an existing nonempty transcript is required for every item"))?
                            .as_ref()
                            .clone(),
                    )
                } else {
                    None
                };
                work.push(TranscribeWork { item, name, src, len, transcript });
            }
            None => skipped.push(item.0),
        }
    }
    if work.is_empty() {
        return Err(EngineError::Other("none of the clips has audio to transcribe".into()));
    }
    // in the background (the app), or here until done (agents and the CLI, as before)
    let wait = bool_p(p, "wait").unwrap_or(true);
    let edit_label = if speakers_only { "Identify Speakers" } else { "Transcribe" };
    let label = format!("{edit_label} ({} clip{})", work.len(), if work.len() == 1 { "" } else { "s" });
    let job = crate::narration::new_job(s, label)?;
    job.progress.total.store(work.len() as u64 * PROGRESS_STEPS, std::sync::atomic::Ordering::Relaxed);
    let results: PendingResults = Arc::default();
    let (prog, res, out) = (job.progress.clone(), job.result.clone(), results.clone());
    let id = job.id;
    let generation = s.document_epoch;
    let targets = work.iter().map(|work| work.item).collect();
    s.transcribe_jobs.push(PendingTranscribe {
        job: id,
        edit_label,
        generation,
        project: s.project.clone(),
        media: s.media.clone(),
        targets,
        results: results.clone(),
    });
    let spawned = crate::narration::spawn(s, job, wait, "filmcraft-transcribe", move || {
        let t0 = web_time::Instant::now();
        // the model loads here, in the job: a failure ends the job with its reason
        *prog.status.lock().unwrap_or_else(std::sync::PoisonError::into_inner) =
            if speakers_only { "Preparing speaker analysis" } else { "Loading the speech model" }.into();
        let r = t.load().and_then(|t| run_transcription(&work, t.as_deref(), &opts, &prog)).map(|done| {
            let words: usize = done.iter().map(|(_, t)| t.words.len()).sum();
            *out.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Some(done);
            if speakers_only { format!("Identified speakers for {words} word(s)") } else { format!("Transcribed {words} word(s)") }
        });
        crate::narration::finish(&prog, &res, r, t0);
    });
    if let Err(e) = spawned {
        s.transcribe_jobs.retain(|pending| pending.job != id);
        return Err(e);
    }
    if !wait {
        return Ok(json!({"job": id, "skipped": skipped}));
    }
    // the job ran here: apply it now and report as before
    let failed = s.jobs.iter().find(|j| j.id == id).and_then(|j| j.progress.error.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone());
    let done = results.lock().unwrap_or_else(std::sync::PoisonError::into_inner).take();
    s.transcribe_jobs.retain(|j| j.job != id);
    let Some(done) = done else { return Err(EngineError::Other(failed.unwrap_or_else(|| "transcription failed".into()))) };
    let report: Vec<Value> = done
        .iter()
        .map(|(i, t)| json!({"item": i.0, "words": t.words.len(), "speakers": t.speakers.len(), "language": t.language, "source": t.source}))
        .collect();
    apply_transcripts(s, done, edit_label)?;
    Ok(json!({"items": report, "skipped": skipped}))
}

/// Progress steps per clip (the recogniser reports a fraction).
const PROGRESS_STEPS: u64 = 1000;

type PendingResults = Arc<std::sync::Mutex<Option<Vec<(ItemId, Transcript)>>>>;

/// One clip's audio to transcribe.
struct TranscribeWork {
    item: ItemId,
    name: String,
    src: filmcraft_media::SharedSource,
    len: usize,
    transcript: Option<Transcript>,
}

/// A background transcription whose transcripts are stored when it finishes ([`poll`]).
pub struct PendingTranscribe {
    pub job: u64,
    edit_label: &'static str,
    /// The project it was started for: another project opened meanwhile gets nothing.
    generation: u64,
    /// Reconcile #1196's per-media and transcript checks with the current document epoch.
    project: Arc<Project>,
    media: Arc<crate::MediaPool>,
    targets: HashSet<ItemId>,
    results: PendingResults,
}

/// Transcribe every clip of `work` in turn, reporting progress and stopping on cancel.
fn run_transcription(
    work: &[TranscribeWork],
    t: Option<&dyn Transcriber>,
    opts: &Options,
    prog: &filmcraft_export::Progress,
) -> std::result::Result<Vec<(ItemId, Transcript)>, String> {
    use std::sync::atomic::Ordering;
    let mut done = Vec::new();
    for (k, w) in work.iter().enumerate() {
        if prog.cancel.load(Ordering::Relaxed) {
            return Err("stopped: nothing was changed".into());
        }
        *prog.status.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = format!("Reading the audio of {}", w.name);
        let audio = decode_audio(&w.src, w.len, prog)?;
        let base = k as u64 * PROGRESS_STEPS;
        let mut progress = |f: f32, what: &str| {
            let f = if f.is_finite() { f.clamp(0.0, 1.0) } else { 0.0 };
            prog.done.store(base + (f64::from(f) * PROGRESS_STEPS as f64) as u64, Ordering::Relaxed);
            *prog.status.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = format!("{}: {what}", w.name);
            !prog.cancel.load(Ordering::Relaxed)
        };
        let tr = if let Some(t) = t {
            let mut tr = t.transcribe(&audio, opts, &mut progress).map_err(|e| e.to_string())?;
            tr.normalize();
            tr
        } else {
            let mut tr = w.transcript.clone().ok_or("no transcript for speaker analysis")?;
            let params = filmcraft_speech::diarize::Params { max_speakers: opts.max_speakers, ..Default::default() };
            filmcraft_speech::diarize::diarize_with_progress(&audio, &mut tr, &params, &mut progress).map_err(|e| e.to_string())?;
            tr
        };
        tr.check()?;
        done.push((w.item, tr));
        prog.done.store(base + PROGRESS_STEPS, Ordering::Relaxed);
    }
    if done.is_empty() {
        return Err("none of the clips has audio to transcribe".into());
    }
    Ok(done)
}

/// Store transcripts as one undo step (items no longer in the project are left out).
fn apply_transcripts(s: &mut Session, done: Vec<(ItemId, Transcript)>, label: &'static str) -> Result<()> {
    s.edit(label, move |pr, _| {
        for (i, t) in done {
            if pr.item(i).is_some() {
                pr.transcripts.insert(i, Arc::new(t));
            }
        }
        Ok(())
    })?;
    Ok(())
}

/// Store the transcripts of finished background transcriptions (one undo step each) and drop
/// finished or cancelled ones. Called once per UI frame from [`Session::poll_persistence`].
pub fn poll(s: &mut Session) {
    use std::sync::atomic::Ordering;
    let mut i = 0;
    while i < s.transcribe_jobs.len() {
        let job = s.jobs.iter().find(|j| j.id == s.transcribe_jobs[i].job);
        let finished = job.is_none_or(|j| j.progress.finished.load(Ordering::Acquire));
        let cancelled = job.is_some_and(|j| j.progress.cancel.load(Ordering::Relaxed));
        if !finished {
            i += 1;
            continue;
        }
        let pj = s.transcribe_jobs.remove(i);
        let done = pj.results.lock().unwrap_or_else(std::sync::PoisonError::into_inner).take();
        let Some(done) = done.filter(|_| !cancelled && pj.generation == s.document_epoch) else { continue };
        let stale = !Arc::ptr_eq(&s.media, &pj.media)
            || done
                .iter()
                .any(|(item, _)| s.project.item(*item) != pj.project.item(*item) || s.project.transcripts.get(item) != pj.project.transcripts.get(item));
        if stale {
            let error = "media or transcript changed during transcription; results were discarded".to_string();
            if let Some(job) = s.jobs.iter().find(|j| j.id == pj.job) {
                *job.progress.error.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Some(error.clone());
                *job.result.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Some(Err(error.clone()));
            }
            s.error_toast("transcript.generate", error);
            continue;
        }
        if let Err(e) = apply_transcripts(s, done, pj.edit_label) {
            s.error_toast("transcript.generate", format!("Transcribe: {e}"));
        }
    }
}

/// A background transcription affecting audio in the active sequence. Unrelated sequences stay
/// readable; all speech work remains available in Progress and counts toward `model_busy`.
pub fn running_job(s: &Session) -> Option<u64> {
    view_running_job(s, false)
}

/// Progress scoped to the media visible in Source or Sequence.
pub fn view_running_job(s: &Session, source: bool) -> Option<u64> {
    if source {
        let item = media_item(s, s.state.source_item?)?;
        return s.transcribe_jobs.iter().find(|job| job.generation == s.document_epoch && job.targets.contains(&item)).map(|job| job.job);
    }
    let sequence = s.active_sequence()?;
    s.transcribe_jobs
        .iter()
        .find(|job| {
            job.generation == s.document_epoch
                && sequence
                    .audio_tracks
                    .iter()
                    .flat_map(|track| &track.items)
                    .filter(|clip| clip.enabled)
                    .any(|clip| media_item(s, clip.item).is_some_and(|item| job.targets.contains(&item)))
        })
        .map(|job| job.job)
}

/// Enabled audio placements, including media inside nested sequences, without recursive walks.
fn audio_placements(project: &Project) -> impl Iterator<Item = (ItemId, ClipId, ItemId)> + '_ {
    project.sequences().filter_map(|item| item.as_sequence().map(|q| (item.id, q))).flat_map(|(sequence, q)| {
        q.audio_tracks.iter().flat_map(|track| &track.items).filter(|clip| clip.enabled).map(move |clip| (sequence, clip.id, clip.item))
    })
}

/// Transcribe newly placed audio once per underlying media item. This runs after the outermost
/// editing command so every placement route shares the same scope, including paste and expansion.
pub(crate) fn auto_transcribe_sequence_clips(s: &mut Session, before: &Project) -> Result<()> {
    let old: HashSet<_> = audio_placements(before).collect();
    let items: BTreeSet<_> = audio_placements(&s.project)
        .filter(|placement| !old.contains(placement))
        .filter_map(|(_, _, item)| s.project.resolve_media(item))
        .filter(|(root, media, _)| media.info.has_audio() && !s.project.transcripts.contains_key(root))
        .map(|(root, _, _)| root.0)
        .collect();
    if items.is_empty() {
        return Ok(());
    }
    // Fold the transcript into the placement's undo step. Preserve the whole history, including
    // its bounded oldest entry and merge key; generate's own edit must not consume another slot.
    let history = s.history.clone();
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| generate(s, &json!({"items": items}))))
        .unwrap_or_else(|_| Err(EngineError::Other("internal error during automatic transcription (see the crash log); the clips were placed".into())));
    s.history = history;
    result.map(|_| ()).map_err(|error| match error {
        EngineError::Other(message) if message == crate::export_tools::INTERNAL_JOB_ERROR => {
            EngineError::Other("internal error during automatic transcription (see the crash log); the clips were placed".into())
        }
        other => other,
    })
}

fn correct_word(s: &mut Session, p: &Value) -> Result<Value> {
    let item = u64_p(p, "item").map(ItemId).ok_or_else(|| bad("transcript.correctWord", "`item` is required"))?;
    let item = media_item(s, item).ok_or_else(|| bad("transcript.correctWord", "no such media item"))?;
    let index = p
        .get("index")
        .and_then(Value::as_u64)
        .and_then(|n| usize::try_from(n).ok())
        .ok_or_else(|| bad("transcript.correctWord", "need a valid word `index`"))?;
    let text = str_p(p, "text")
        .map(str::trim)
        .filter(|t| !t.is_empty() && t.len() <= 1024 && !t.chars().any(|c| c.is_whitespace() || c.is_control()))
        .ok_or_else(|| bad("transcript.correctWord", "text must be one nonempty word of at most 1024 bytes"))?
        .to_string();
    let mut transcript = s.project.transcripts.get(&item).map(|t| (**t).clone()).ok_or_else(|| bad("transcript.correctWord", "no transcript for this item"))?;
    let word = transcript.words.get_mut(index).ok_or_else(|| bad("transcript.correctWord", "word index is out of range"))?;
    if p.get("expectedWord").is_some_and(|expected| expected != &json!(word))
        || p.get("expected").is_some_and(|expected| expected.as_str() != Some(word.text.as_str()))
    {
        return Err(bad("transcript.correctWord", "word changed while it was being edited; reopen the correction"));
    }
    if word.text != text {
        word.text = text;
        s.edit("Correct Transcript Word", move |pr, _| {
            pr.transcripts.insert(item, Arc::new(transcript));
            Ok(())
        })?;
    }
    Ok(json!({"item": item.0, "index": index}))
}

pub fn source_words(s: &Session) -> Vec<SeqWord> {
    let Some(item) = s.state.source_item else { return Vec::new() };
    let Some((root, _, range)) = s.project.resolve_media(item) else { return Vec::new() };
    let Some(tr) = s.project.transcripts.get(&root) else { return Vec::new() };
    tr.words
        .iter()
        .enumerate()
        .filter(|(_, w)| range.is_none_or(|r| w.end > r.start && w.start < r.end()))
        .map(|(index, w)| SeqWord {
            text: w.text.clone(),
            start: w.start,
            end: w.end,
            clip: filmcraft_project::ClipId(0),
            item: root,
            index,
            track: 0,
            speaker: tr.speaker_name(w),
            confidence: w.confidence,
        })
        .collect()
}

fn speaker_name(p: &Value, cmd: &str) -> Result<String> {
    str_p(p, "name")
        .map(str::trim)
        .filter(|n| !n.is_empty() && n.len() <= 256 && !n.chars().any(char::is_control))
        .map(str::to_string)
        .ok_or_else(|| bad(cmd, "speaker name must be nonempty, at most 256 bytes, without control characters"))
}

fn assign_speaker(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "transcript.assignSpeaker";
    let item = u64_p(p, "item").map(ItemId).and_then(|i| media_item(s, i)).ok_or_else(|| bad(CMD, "no such media item"))?;
    let indices = p["indices"].as_array().filter(|a| !a.is_empty() && a.len() <= 10_000).ok_or_else(|| bad(CMD, "need 1 to 10000 word indices"))?;
    let mut indices =
        indices.iter().map(|v| v.as_u64().and_then(|n| usize::try_from(n).ok()).ok_or_else(|| bad(CMD, "invalid word index"))).collect::<Result<Vec<_>>>()?;
    indices.sort_unstable();
    indices.dedup();
    let mut transcript = s.project.transcripts.get(&item).ok_or_else(|| bad(CMD, "no transcript for this item"))?.as_ref().clone();
    let mut expected = Vec::new();
    for index in &indices {
        let word = transcript.words.get(*index).ok_or_else(|| bad(CMD, "word index is out of range"))?;
        expected.push(json!({"index": index, "word": word}));
    }
    if p.get("expected").is_some_and(|value| value != &json!(expected)) {
        return Err(bad(CMD, "transcript changed; reopen the speaker menu"));
    }
    let speaker = if p.get("name").is_some() {
        if p.get("speaker").is_some() {
            return Err(bad(CMD, "pass either speaker or name"));
        }
        let name = speaker_name(p, CMD)?;
        let index = if let Some(index) = transcript.speakers.iter().position(|sp| sp.name == name) {
            index
        } else {
            if transcript.speakers.len() >= 64 {
                return Err(bad(CMD, "a transcript supports at most 64 manually named speakers"));
            }
            transcript.speakers.push(filmcraft_project::Speaker { name });
            transcript.speakers.len() - 1
        };
        Some(u32::try_from(index).map_err(|_| bad(CMD, "speaker index is too large"))?)
    } else {
        match p.get("speaker") {
            Some(Value::Null) => None,
            Some(value) => Some(
                value
                    .as_u64()
                    .and_then(|n| u32::try_from(n).ok())
                    .filter(|n| (*n as usize) < transcript.speakers.len())
                    .ok_or_else(|| bad(CMD, "speaker index is out of range"))?,
            ),
            None => return Err(bad(CMD, "pass speaker (index or null) or a new name")),
        }
    };
    for index in &indices {
        if let Some(word) = transcript.words.get_mut(*index) {
            word.speaker = speaker;
        }
    }
    transcript.check().map_err(EngineError::Other)?;
    s.edit("Assign Speaker", move |pr, _| {
        pr.transcripts.insert(item, Arc::new(transcript));
        Ok(())
    })?;
    Ok(json!({"item": item.0, "words": indices.len(), "speaker": speaker}))
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
    json!({"i": i, "index": w.index, "text": w.text, "start": w.start.0, "end": w.end.0, "speaker": w.speaker, "clip": w.clip.0, "item": w.item.0, "confidence": w.confidence})
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
    Ok(json!({
        "words": words.iter().enumerate().map(|(i, w)| word_json(i, w)).collect::<Vec<_>>(),
        "paragraphs": paras,
        "speakers": speakers,
        "current": current,
        "items": s.project.transcripts.iter().map(|(i, t)| json!({"item": i.0, "words": t.words.len(), "language": t.language, "source": t.source, "speakers": t.speakers.iter().map(|k| &k.name).collect::<Vec<_>>()})).collect::<Vec<_>>(),
    }))
}

fn search(s: &mut Session, p: &Value) -> Result<Value> {
    match p.get("scope") {
        Some(Value::String(scope)) if scope == "project" => return crate::transcript_search::run(s, p),
        None => {}
        Some(Value::String(scope)) if scope == "sequence" => {}
        _ => return Err(bad("transcript.search", "scope must be sequence or project")),
    }
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

/// Rename several source-local speaker identities as one validated, atomic edit.
fn rename_speakers(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "transcript.renameSpeakers";
    let sources =
        p.get("sources").and_then(Value::as_array).filter(|a| !a.is_empty() && a.len() <= 256).ok_or_else(|| bad(CMD, "need 1 to 256 source groups"))?;
    let mut seen = BTreeSet::new();
    let mut replacements = Vec::new();
    let mut changed = 0usize;
    let mut total = 0usize;
    for source in sources {
        let item = source.get("item").and_then(Value::as_u64).map(ItemId).and_then(|id| media_item(s, id)).ok_or_else(|| bad(CMD, "no such media item"))?;
        if !seen.insert(item) {
            return Err(bad(CMD, "duplicate source group"));
        }
        let current = s.project.transcripts.get(&item).ok_or_else(|| bad(CMD, "source has no transcript"))?;
        // One complete guard per source also detects re-identification that retained its old names.
        if source.get("expected").is_some_and(|v| v != &json!(current)) {
            return Err(bad(CMD, "transcript changed; close and reopen Speakers"));
        }
        let names = source
            .get("names")
            .and_then(Value::as_array)
            .filter(|a| !a.is_empty() && a.len() <= 4096)
            .ok_or_else(|| bad(CMD, "need 1 to 4096 speaker names"))?;
        total = total.saturating_add(names.len());
        if total > 4096 {
            return Err(bad(CMD, "rename at most 4096 speakers at once"));
        }
        let mut next = current.as_ref().clone();
        let mut speakers = BTreeSet::new();
        for entry in names {
            let index = entry.get("speaker").and_then(Value::as_u64).and_then(|i| usize::try_from(i).ok()).ok_or_else(|| bad(CMD, "invalid speaker index"))?;
            if !speakers.insert(index) {
                return Err(bad(CMD, "duplicate speaker index"));
            }
            let name = speaker_name(entry, CMD)?;
            let speaker = next.speakers.get_mut(index).ok_or_else(|| bad(CMD, "no such speaker"))?;
            if speaker.name != name {
                speaker.name = name;
                changed += 1;
            }
        }
        if next != **current {
            replacements.push((item, Arc::new(next)));
        }
    }
    if changed > 0 {
        s.edit("Rename Speakers", move |project, _| {
            for (item, transcript) in replacements {
                project.transcripts.insert(item, transcript);
            }
            Ok(())
        })?;
    }
    Ok(json!({"renamed": changed}))
}

fn rename_speaker(s: &mut Session, p: &Value) -> Result<Value> {
    let name = speaker_name(p, "transcript.renameSpeaker")?;
    let item = if p.get("item").is_some() {
        Some(u64_p(p, "item").map(ItemId).and_then(|i| media_item(s, i)).ok_or_else(|| bad("transcript.renameSpeaker", "no such media item"))?)
    } else {
        None
    };
    // `speaker`: the current name (every transcript), or an index (needs `item`)
    let (old_name, index) = match p.get("speaker") {
        Some(Value::String(n)) => (Some(n.clone()), None),
        Some(v) if v.is_u64() => (None, v.as_u64().and_then(|n| usize::try_from(n).ok())),
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
                if p.get("expected").is_some_and(|expected| expected.as_str() != Some(sp.name.as_str())) {
                    return Err(bad("transcript.renameSpeaker", "speaker changed; reopen the speaker menu"));
                }
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
    let max_words = u64_p(p, "wordsPerCaption").map(|n| n.min(1000) as usize).unwrap_or(0);
    // word-by-word captions keep each word's own timing unless a minimum is asked for
    let min_default = if max_words > 0 && max_words <= 3 { Tick::ZERO } else { d.min_duration };
    // hostile numbers: lines × characters is multiplied when laying out, durations become ticks
    let seconds = |key: &str| f64_p(p, key).filter(|x| x.is_finite()).map(|x| Tick::from_seconds_f64(x.clamp(0.0, 3600.0)));
    let rules = CaptionRules {
        max_chars: u64_p(p, "maxChars").map(|n| n.clamp(1, 10_000) as usize).unwrap_or(d.max_chars),
        lines: u64_p(p, "lines").map(|n| n.clamp(1, 4) as usize).unwrap_or(d.lines),
        max_words,
        min_duration: seconds("minSeconds").unwrap_or(min_default),
        max_duration: seconds("maxSeconds").unwrap_or(d.max_duration),
        gap_frames: p.get("gapFrames").and_then(Value::as_i64).map(|n| n.clamp(0, 10_000)).unwrap_or(d.gap_frames),
        break_pause: d.break_pause,
    };
    let style = match p.get("style").filter(|v| !v.is_null()) {
        Some(v) => Some(serde_json::from_value::<CaptionStyle>(v.clone()).map_err(|e| bad("transcript.createCaptions", format!("style: {e}")))?),
        None => None,
    };
    let blocks = tx::caption_blocks(&words, &rules, s.sequence_rate());
    let format = str_p(p, "format").and_then(CaptionFormat::from_name).unwrap_or_default();
    let name = str_p(p, "name").unwrap_or("Transcript").to_string();
    let n = blocks.len();
    let tid = s.edit_sequence("Create Captions", |q, ctx, st| {
        let tid = TrackId(ctx.alloc());
        let mut t = CaptionTrack::new(tid, name, format);
        if let Some(style) = style {
            t.style = style;
        }
        t.captions = tx::blocks_to_captions(&blocks, ctx);
        q.caption_tracks.insert(0, t);
        st.caption_selection.clear();
        Ok(tid)
    })?;
    Ok(json!({"track": tid.0, "captions": n}))
}

/// Create Captions dialog settings (Premiere's "Captioning Preferences"), as
/// `transcript.createCaptions` takes them.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct CaptionSettings {
    /// [`CaptionFormat`] label: `Subtitle`, `CEA-608`, `CEA-708`, `Teletext`.
    pub format: String,
    /// 1 = Single Line, 2 = Double Line.
    pub lines: u32,
    pub max_chars: u32,
    pub min_seconds: f64,
    pub gap_frames: u32,
    /// Most words per caption (0 = no limit; 1 = word by word).
    pub words_per_caption: u32,
    /// Track style for the new track (`None` = the default style).
    pub style: Option<CaptionStyle>,
    /// What the Style menu shows for `style` (the track it was taken from).
    pub style_name: String,
}

impl Default for CaptionSettings {
    fn default() -> Self {
        let d = CaptionRules::default();
        Self {
            format: CaptionFormat::Subtitle.label().into(),
            lines: d.lines as u32,
            max_chars: d.max_chars as u32,
            min_seconds: d.min_duration.seconds(),
            gap_frames: d.gap_frames as u32,
            words_per_caption: 0,
            style: None,
            style_name: String::new(),
        }
    }
}

impl CaptionSettings {
    /// The `transcript.createCaptions` parameters.
    pub fn params(&self) -> Value {
        let mut p = json!({
            "format": self.format,
            "lines": self.lines.clamp(1, 2),
            "maxChars": self.max_chars.max(1),
            "minSeconds": self.min_seconds,
            "gapFrames": self.gap_frames,
        });
        if self.words_per_caption > 0 {
            p["wordsPerCaption"] = json!(self.words_per_caption);
        }
        if let Some(s) = &self.style {
            p["style"] = json!(s);
        }
        p
    }
}

/// A named set of [`CaptionSettings`] (the dialog's Caption preset menu).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct CaptionPreset {
    pub name: String,
    pub settings: CaptionSettings,
}

/// Preferences ▸ the Create Captions dialog: the settings last used (it opens with them, unlike
/// Premiere, which goes back to its default preset every time) and the presets the user saved.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct CaptionPrefs {
    pub last: CaptionSettings,
    pub user_presets: Vec<CaptionPreset>,
}

/// The built-in caption presets.
pub fn builtin_caption_presets() -> Vec<CaptionPreset> {
    let base = CaptionSettings::default();
    vec![
        CaptionPreset { name: "Subtitle Default".into(), settings: base.clone() },
        CaptionPreset { name: "Single Line".into(), settings: CaptionSettings { lines: 1, max_chars: 32, ..base.clone() } },
        CaptionPreset { name: "Word by Word".into(), settings: CaptionSettings { lines: 1, words_per_caption: 1, min_seconds: 0.0, ..base.clone() } },
        CaptionPreset {
            name: "CEA-608 Closed Captions".into(),
            settings: CaptionSettings { format: CaptionFormat::Cea608.label().into(), max_chars: 32, ..base },
        },
    ]
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

/// A catalogue speech model by id.
pub fn find_model(id: &str) -> Option<&'static filmcraft_speech::models::ModelInfo> {
    filmcraft_speech::models::find(id)
}

/// The speech model `transcript.generate` will use (Settings ▸ Media Analysis & Transcription ▸
/// Speech model), and whether it is downloaded. None when no transcriber needs one (a host
/// installed its own) or the setting names no catalogue model.
pub fn selected_model(s: &Session) -> Option<(&'static filmcraft_speech::models::ModelInfo, bool)> {
    if s.transcriber.is_some() {
        return None;
    }
    let m = filmcraft_speech::models::find(&s.prefs.media_analysis.whisper_model)?;
    let installed = models_dir().is_some_and(|d| filmcraft_speech::models::installed(&d, m));
    Some((m, installed))
}

/// Whether this session has speech work that must finish before model deletion.
pub fn model_busy(s: &Session) -> bool {
    !s.transcribe_jobs.is_empty()
        || s.jobs.iter().any(|job| job.label.starts_with("Download Speech Model: ") && !job.progress.finished.load(std::sync::atomic::Ordering::Acquire))
}

fn delete_model(s: &mut Session, p: &Value) -> Result<Value> {
    let cmd = "transcript.deleteModel";
    let id = p.get("model").and_then(Value::as_str).ok_or_else(|| bad(cmd, "model must be an explicit catalogue model id"))?;
    if p.get("confirm") != Some(&Value::Bool(true)) {
        return Err(bad(cmd, "confirmation is required (confirm: true)"));
    }
    if model_busy(s) {
        return Err(bad(cmd, "stop speech transcription and model downloads before deleting a model"));
    }
    let dir = models_dir().ok_or_else(|| bad(cmd, "no data directory for speech models"))?;
    let deleted = filmcraft_speech::models::remove(&dir, id).map_err(speech_err)?;
    Ok(json!({"model": id, "deleted": deleted}))
}

/// Download a catalogue model into `<data dir>/models` (feature `speech-download`). Hosts show
/// the size, source and licence (`transcript.models`) and ask before running this. With
/// `"wait": false` it runs as a background job (progress, cancel) and returns its id; by default
/// it finishes before returning, as it always did.
fn download_model(s: &mut Session, p: &Value) -> Result<Value> {
    let cmd = "transcript.downloadModel";
    // Concurrent writers to the same `.part` files would corrupt each other's downloads.
    if s.jobs.iter().any(|job| job.label.starts_with("Download Speech Model: ") && !job.progress.finished.load(std::sync::atomic::Ordering::Acquire)) {
        return Err(bad(cmd, "a speech model download is already running"));
    }
    let id = str_p(p, "model").unwrap_or(filmcraft_speech::models::DEFAULT_MODEL);
    let m = filmcraft_speech::models::find(id).ok_or_else(|| speech_err(SpeechError::UnknownModel(id.into())))?;
    let dir = models_dir().ok_or_else(|| EngineError::Other("no data directory for speech models".into()))?;
    if filmcraft_speech::models::installed(&dir, m) {
        return Ok(json!({"model": m.id, "job": null, "installed": true, "dir": filmcraft_speech::models::model_dir(&dir, m).to_string_lossy()}));
    }
    if !cfg!(feature = "speech-download") {
        return Err(bad(cmd, "model downloads are not available in this build (built without the `speech-download` feature)"));
    }
    let wait = p.get("wait").and_then(Value::as_bool).unwrap_or(true);
    let job = crate::narration::new_job(s, format!("Download Speech Model: {}", m.name))?;
    job.progress.total.store(filmcraft_speech::models::missing_bytes(&dir, m), std::sync::atomic::Ordering::Relaxed);
    let (prog, res) = (job.progress.clone(), job.result.clone());
    let target = filmcraft_speech::models::model_dir(&dir, m).to_string_lossy().to_string();
    let job_id = crate::narration::spawn(s, job, wait, "filmcraft-speech-download", move || {
        let t0 = web_time::Instant::now();
        let r = fetch_model(&dir, m, &prog);
        crate::narration::finish(&prog, &res, r.map(|()| format!("{} installed", m.name)), t0);
    })?;
    if wait {
        // the job ran here: report its outcome as the command's
        let failed = s.jobs.iter().find(|j| j.id == job_id).and_then(|j| j.progress.error.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone());
        if let Some(e) = failed {
            return Err(EngineError::Other(e));
        }
        return Ok(json!({"model": m.id, "job": job_id, "installed": true, "dir": target}));
    }
    Ok(json!({"model": m.id, "job": job_id, "installed": false, "dir": target}))
}

#[cfg(feature = "speech-download")]
fn fetch_model(dir: &std::path::Path, m: &'static filmcraft_speech::models::ModelInfo, prog: &filmcraft_export::Progress) -> std::result::Result<(), String> {
    use std::sync::atomic::Ordering;
    let mut progress = |done: u64, total: u64, file: &str| {
        prog.done.store(done, Ordering::Relaxed);
        prog.total.store(total, Ordering::Relaxed);
        *prog.status.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = format!("Downloading {file}");
        !prog.cancel.load(Ordering::Relaxed)
    };
    filmcraft_speech::models::download(dir, m, &mut progress).map_err(|e| e.to_string())
}

#[cfg(not(feature = "speech-download"))]
fn fetch_model(_: &std::path::Path, _: &'static filmcraft_speech::models::ModelInfo, _: &filmcraft_export::Progress) -> std::result::Result<(), String> {
    Err("model downloads are not available in this build (built without the `speech-download` feature)".into())
}

pub fn commands() -> Vec<CommandSpec> {
    vec![
        spec(
            "transcript.generate",
            "Transcribe…",
            &["Sequence", "Transcript"],
            r#"{"items":[id]?,"model":"whisper-base"?,"language":"en|auto"?,"diarize":bool?,"maxSpeakers":n?,"wait":bool=true}"#,
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
        spec(
            "transcript.correctWord",
            "Correct Transcript Word",
            &[],
            r#"{"item":id,"index":n,"text":str,"expected":str?,"expectedWord":Word?}"#,
            always,
            correct_word,
            true,
        ),
        spec(
            "transcript.identifySpeakers",
            "Identify Speakers",
            &[],
            r#"{"items":[id]?,"item":id?,"maxSpeakers":n?,"wait":bool=true}"#,
            always,
            identify_speakers,
            true,
        ),
        spec(
            "transcript.renameSpeakers",
            "Rename Speakers",
            &[],
            r#"{"sources":[{"item":id,"expected":Transcript?,"names":[{"speaker":n,"name":str}]}]}"#,
            always,
            rename_speakers,
            true,
        ),
        spec(
            "transcript.assignSpeaker",
            "Assign Speaker",
            &[],
            r#"{"item":id,"indices":[n],"speaker":n|null?,"name":str?,"expected":[{index:n,word:word}]?}"#,
            always,
            assign_speaker,
            true,
        ),
        spec(
            "transcript.source",
            "Inspect Source Transcript",
            &[],
            "{}",
            always,
            |s, _| Ok(json!({"words": source_words(s).iter().enumerate().map(|(i, w)| word_json(i, w)).collect::<Vec<_>>()})),
            false,
        ),
        spec(
            "transcript.openSearchResult",
            "Open Transcript Search Result",
            &[],
            r#"{"item":id,"from":n,"to":n}"#,
            always,
            crate::transcript_search::open,
            false,
        ),
        spec("transcript.models", "List Speech Models", &[], "{}", always, models, false),
        spec("transcript.deleteModel", "Delete Downloaded Speech Model", &[], r#"{"model":str,"confirm":true}"#, always, delete_model, false),
        spec("transcript.downloadModel", "Download Speech Model", &[], r#"{"model":"whisper-base"?,"wait":bool=true}"#, can_download, download_model, false),
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
            r#"{"maxChars":n?,"lines":1|2?,"wordsPerCaption":n?,"minSeconds":f?,"maxSeconds":f?,"gapFrames":n?,"format":str?,"name":str?,"style":CaptionStyle?}"#,
            has_transcript,
            create_captions,
            true,
        ),
    ]
}
