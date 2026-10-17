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

use std::sync::Arc;

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

/// Mono 16 kHz audio of a media item (None: no audio).
fn item_audio(s: &Session, item: ItemId) -> Option<Vec<f32>> {
    let dur = match &s.project.item(item)?.kind {
        ItemKind::Media(m) => m.duration(),
        _ => return None,
    };
    let src = s.source(item)?;
    if !src.info().has_audio() {
        return None;
    }
    let sr = filmcraft_speech::SAMPLE_RATE;
    let len = dur.to_units_floor(sr as i64).max(0) as usize;
    let buf = src.audio(0, len, sr).ok()?;
    Some(filmcraft_speech::downmix(&buf.channels))
}

fn speech_err(e: SpeechError) -> EngineError {
    EngineError::Other(e.to_string())
}

/// The transcriber to use: the installed one, else the named catalogue model.
fn transcriber(s: &Session, p: &Value) -> Result<Arc<dyn Transcriber>> {
    if let Some(t) = &s.transcriber {
        return Ok(t.clone());
    }
    // Settings ▸ Media Analysis & Transcription ▸ Speech model
    let model = str_p(p, "model").unwrap_or(&s.prefs.media_analysis.whisper_model);
    let m = filmcraft_speech::models::find(model).ok_or_else(|| speech_err(SpeechError::UnknownModel(model.into())))?;
    #[cfg(not(feature = "speech-download"))]
    let _ = m;
    if !filmcraft_speech::available() {
        return Err(EngineError::Other(NO_SPEECH.into()));
    }
    let dir = models_dir().ok_or_else(|| EngineError::Other("no data directory for speech models".into()))?;
    #[cfg(feature = "speech-download")]
    {
        if !filmcraft_speech::models::installed(&dir, m) {
            log::info!("Auto-downloading speech model {}...", m.id);
            filmcraft_speech::models::download(&dir, m, &mut |_, _, _| true).map_err(speech_err)?;
        }
    }
    filmcraft_speech::load(&dir, model).map_err(speech_err)
}

/// Completed clip transcript plus optional AI-segmented caption blocks.
pub struct ClipTranscriptDone {
    pub item: ItemId,
    pub transcript: Transcript,
    pub ai_captions: Vec<(usize, usize, Vec<String>)>,
    pub ai_corrected_words: usize,
    pub ai_provider: Option<String>,
    pub ai_warning: Option<String>,
}

/// A running background transcription job; applied by [`poll`] when it finishes.
pub struct PendingTranscript {
    pub job: u64,
    pub results: Arc<std::sync::Mutex<Option<Vec<ClipTranscriptDone>>>>,
    pub auto_create_captions: bool,
}

fn lock<T>(m: &std::sync::Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

struct AudioWork {
    item: ItemId,
    name: String,
    dur: Tick,
    src: filmcraft_media::SharedSource,
}

fn generate(s: &mut Session, p: &Value) -> Result<Value> {
    let items = targets(s, p);
    if items.is_empty() {
        return Err(bad("transcript.generate", "nothing to transcribe (pass `items`, select clips, or open a sequence with audio)"));
    }
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
    let ai_refine = bool_p(p, "aiRefine").unwrap_or(ma.ai_refine);
    let ai_provider = str_p(p, "aiProvider").unwrap_or(&ma.ai_provider).to_string();
    let ai_api_key = str_p(p, "aiApiKey").unwrap_or(&ma.ai_api_key).to_string();
    let ai_prompt = str_p(p, "aiPrompt").unwrap_or(&ma.ai_prompt).to_string();
    let ai_auto_captions = bool_p(p, "aiAutoCaptions").unwrap_or(ma.ai_auto_captions);

    let run_async = bool_p(p, "async").unwrap_or(false) && !cfg!(target_arch = "wasm32");
    if run_async {
        if !s.transcript_jobs.is_empty() {
            return Err(EngineError::Other("Transcription is already running in the background".into()));
        }
        let installed_transcriber = s.transcriber.clone();
        let model_id = str_p(p, "model").unwrap_or(&s.prefs.media_analysis.whisper_model).to_string();
        let model_info = if installed_transcriber.is_none() {
            let m = filmcraft_speech::models::find(&model_id).ok_or_else(|| speech_err(SpeechError::UnknownModel(model_id.clone())))?;
            if !filmcraft_speech::available() {
                return Err(EngineError::Other(NO_SPEECH.into()));
            }
            let dir = models_dir().ok_or_else(|| EngineError::Other("no data directory for speech models".into()))?;
            Some((m, dir))
        } else {
            None
        };

        let mut work = Vec::new();
        let mut skipped = Vec::new();
        for item in items {
            let Some(proj_item) = s.project.item(item) else {
                skipped.push(item.0);
                continue;
            };
            let ItemKind::Media(m) = &proj_item.kind else {
                skipped.push(item.0);
                continue;
            };
            let dur = m.duration();
            let Some(src) = s.source(item) else {
                skipped.push(item.0);
                continue;
            };
            if !src.info().has_audio() {
                skipped.push(item.0);
                continue;
            }
            work.push(AudioWork { item, name: proj_item.name.clone(), dur, src });
        }
        if work.is_empty() {
            return Err(EngineError::Other("none of the clips has audio to transcribe".into()));
        }

        let id = s.jobs.iter().map(|j| j.id).max().unwrap_or(0) + 1;
        let n_clips = work.len();
        let label = format!("Transcribing ({} clip{})", n_clips, if n_clips == 1 { "" } else { "s" });
        let job = crate::Job { id, label, progress: Default::default(), result: Default::default() };
        let total_steps = (n_clips as u64).max(1) * 1000;
        job.progress.total.store(total_steps, std::sync::atomic::Ordering::Relaxed);
        *lock(&job.progress.status) = "Preparing transcription…".into();
        let results: Arc<std::sync::Mutex<Option<Vec<ClipTranscriptDone>>>> = Arc::default();
        let (prog, res, out) = (job.progress.clone(), job.result.clone(), results.clone());

        let auto_create_captions = ai_auto_captions && ai_refine && !ai_api_key.trim().is_empty();
        let run = move || {
            use std::sync::atomic::Ordering;
            let t0 = web_time::Instant::now();
            let step_res: std::result::Result<Vec<ClipTranscriptDone>, String> = (|| {
                let t: Arc<dyn Transcriber> = if let Some(inst) = installed_transcriber {
                    inst
                } else if let Some((m, dir)) = model_info {
                    #[cfg(not(feature = "speech-download"))]
                    let _ = m;
                    #[cfg(feature = "speech-download")]
                    {
                        if !filmcraft_speech::models::installed(&dir, m) {
                            *lock(&prog.status) = format!("Downloading speech model {}…", m.id);
                            let dl_res = filmcraft_speech::models::download(&dir, m, &mut |done_b, total_b, file| {
                                let pct = if total_b > 0 { (done_b as f64 / total_b as f64 * 100.0) as u64 } else { 0 };
                                let step_done = (done_b as f64 / total_b.max(1) as f64 * (total_steps as f64 * 0.15)) as u64;
                                prog.done.store(step_done, Ordering::Relaxed);
                                *lock(&prog.status) = format!("Downloading {} ({file}) — {pct}%", m.id);
                                !prog.cancel.load(Ordering::Relaxed)
                            });
                            dl_res.map_err(|e| e.to_string())?;
                        }
                    }
                    if prog.cancel.load(Ordering::Relaxed) {
                        return Err("stopped".into());
                    }
                    *lock(&prog.status) = format!("Loading model {}…", model_id);
                    filmcraft_speech::load(&dir, &model_id).map_err(|e| e.to_string())?
                } else {
                    return Err("no speech model".into());
                };

                let mut done: Vec<ClipTranscriptDone> = Vec::new();
                let sr = filmcraft_speech::SAMPLE_RATE;
                let use_ai = ai_refine && !ai_api_key.trim().is_empty();
                let whisper_weight = if use_ai { 0.85_f32 } else { 1.0_f32 };
                for (idx, w) in work.iter().enumerate() {
                    if prog.cancel.load(Ordering::Relaxed) {
                        return Err("stopped".into());
                    }
                    let base_frac = idx as f32 / n_clips as f32;
                    let clip_span = 1.0 / n_clips as f32;
                    prog.done.store((base_frac * total_steps as f32) as u64, Ordering::Relaxed);
                    *lock(&prog.status) = format!("Extracting audio: {} ({}/{})…", w.name, idx + 1, n_clips);
                    let len = w.dur.to_units_floor(sr as i64).max(0) as usize;
                    let Ok(buf) = w.src.audio(0, len, sr) else {
                        continue;
                    };
                    let audio = filmcraft_speech::downmix(&buf.channels);
                    let clip_name = &w.name;
                    let mut tr = t
                        .transcribe(&audio, &opts, &mut |frac, msg| {
                            let overall = (base_frac + frac.clamp(0.0, 1.0) * whisper_weight * clip_span).clamp(0.0, 1.0);
                            prog.done.store((overall * total_steps as f32) as u64, Ordering::Relaxed);
                            *lock(&prog.status) = format!("{clip_name} ({}/{}): {msg}", idx + 1, n_clips);
                            !prog.cancel.load(Ordering::Relaxed)
                        })
                        .map_err(|e| e.to_string())?;
                    tr.normalize();
                    tx::apply_glossary(&mut tr);

                    let mut ai_captions = Vec::new();
                    let mut ai_corrected_words = 0usize;
                    let mut ai_provider_used = None;
                    let mut ai_warning = None;

                    if use_ai && !tr.words.is_empty() {
                        if prog.cancel.load(Ordering::Relaxed) {
                            return Err("stopped".into());
                        }
                        let ai_frac = (base_frac + 0.88 * clip_span).clamp(0.0, 0.99);
                        prog.done.store((ai_frac * total_steps as f32) as u64, Ordering::Relaxed);
                        let prov_name = filmcraft_speech::llm::resolve_provider(&ai_provider, &ai_api_key);
                        *lock(&prog.status) = format!("{clip_name}: Refining text & separating captions with AI ({prov_name})…");
                        match filmcraft_speech::llm::refine_transcript(&mut tr, &ai_provider, &ai_api_key, &ai_prompt, 30, 2) {
                            Ok(ai_res) => {
                                ai_corrected_words = ai_res.corrected_words;
                                ai_provider_used = Some(ai_res.provider_used);
                                ai_captions = ai_res.captions.into_iter().map(|c| (c.start_word, c.end_word, c.lines)).collect();
                            }
                            Err(e) => {
                                ai_warning = Some(e.to_string());
                            }
                        }
                    }

                    done.push(ClipTranscriptDone { item: w.item, transcript: tr, ai_captions, ai_corrected_words, ai_provider: ai_provider_used, ai_warning });
                }
                Ok(done)
            })();

            let secs = t0.elapsed().as_secs_f64();
            match step_res {
                Ok(done) => {
                    let n_words: usize = done.iter().map(|d| d.transcript.words.len()).sum();
                    prog.done.store(total_steps, Ordering::Relaxed);
                    *lock(&prog.status) = format!("Transcribed {n_words} word(s) in {:.1}s", secs);
                    *lock(&out) = Some(done);
                    *lock(&res) = Some(Ok(filmcraft_export::Report {
                        path: String::new(),
                        frames: n_words as u64,
                        seconds: secs,
                        bytes: 0,
                        render_fps: 0.0,
                        extra_files: Vec::new(),
                    }));
                }
                Err(e) => {
                    *lock(&prog.status) = if e == "stopped" { "Cancelled".into() } else { e.clone() };
                    *lock(&prog.error) = Some(e.clone());
                    *lock(&res) = Some(Err(e));
                }
            }
            prog.finished.store(true, Ordering::Relaxed);
        };

        let run = crate::export_tools::guard_job(job.progress.clone(), job.result.clone(), run);
        s.jobs.push(job);
        s.transcript_jobs.push(PendingTranscript { job: id, results, auto_create_captions });
        std::thread::Builder::new().name("filmcraft-transcribe".into()).spawn(run).map_err(|e| EngineError::Other(e.to_string()))?;
        return Ok(json!({"job": id, "clips": n_clips, "skipped": skipped}));
    }

    let t = transcriber(s, p)?;
    let mut done: Vec<(ItemId, Transcript)> = Vec::new();
    let mut skipped = Vec::new();
    let use_ai = ai_refine && !ai_api_key.trim().is_empty();
    for item in items {
        let Some(audio) = item_audio(s, item) else {
            skipped.push(item.0);
            continue;
        };
        let mut tr = t.transcribe(&audio, &opts, &mut |_, _| true).map_err(speech_err)?;
        tr.normalize();
        tx::apply_glossary(&mut tr);
        if use_ai && !tr.words.is_empty() {
            let _ = filmcraft_speech::llm::refine_transcript(&mut tr, &ai_provider, &ai_api_key, &ai_prompt, 30, 2);
        }
        done.push((item, tr));
    }
    if done.is_empty() {
        return Err(EngineError::Other("none of the clips has audio to transcribe".into()));
    }
    let report: Vec<Value> = done
        .iter()
        .map(|(i, t)| json!({"item": i.0, "words": t.words.len(), "speakers": t.speakers.len(), "language": t.language, "source": t.source}))
        .collect();
    s.edit("Transcribe", move |pr, _| {
        for (i, t) in done {
            pr.transcripts.insert(i, Arc::new(t));
        }
        Ok(())
    })?;
    Ok(json!({"items": report, "skipped": skipped}))
}

/// Apply finished background transcription jobs to the project. Called once per UI frame from
/// [`Session::poll_persistence`].
pub fn poll(s: &mut Session) {
    use std::sync::atomic::Ordering;
    let mut i = 0;
    while i < s.transcript_jobs.len() {
        let job_id = s.transcript_jobs[i].job;
        let job = s.jobs.iter().find(|j| j.id == job_id);
        let finished = job.is_none_or(|j| j.progress.finished.load(Ordering::Relaxed));
        let cancelled = job.is_some_and(|j| j.progress.cancel.load(Ordering::Relaxed));
        let err = job.and_then(|j| lock(&j.result).clone()).and_then(std::result::Result::err);
        if !finished {
            i += 1;
            continue;
        }
        let pj = s.transcript_jobs.remove(i);
        if cancelled {
            s.toast("Transcription cancelled");
            continue;
        }
        if let Some(e) = err {
            if e != "stopped" {
                s.error_toast("transcript.generate", format!("Transcribe: {e}"));
            }
            continue;
        }
        let Some(done) = lock(&pj.results).take() else { continue };
        let n_words: usize = done.iter().map(|d| d.transcript.words.len()).sum();
        let n_clips = done.len();
        let total_ai_corrected: usize = done.iter().map(|d| d.ai_corrected_words).sum();
        let ai_provider: Option<String> = done.iter().find_map(|d| d.ai_provider.clone());
        let ai_warn: Option<String> = done.iter().find_map(|d| d.ai_warning.clone());
        let ai_by_item: std::collections::BTreeMap<ItemId, Vec<(usize, usize, Vec<String>)>> =
            done.iter().filter(|d| !d.ai_captions.is_empty()).map(|d| (d.item, d.ai_captions.clone())).collect();
        let transcripts_to_insert: Vec<(ItemId, Transcript)> = done.into_iter().map(|d| (d.item, d.transcript)).collect();
        if let Err(e) = s.edit("Transcribe", move |pr, _| {
            for (item, tr) in transcripts_to_insert {
                pr.transcripts.insert(item, Arc::new(tr));
            }
            Ok(())
        }) {
            s.error_toast("transcript.generate", format!("Transcribe: {e}"));
            continue;
        }

        if n_words == 0 {
            s.toast("Transcription finished: no speech detected in the audio clips");
            continue;
        }

        if pj.auto_create_captions && s.active_sequence().is_some() {
            let seq_words = sequence_words(s);
            if !seq_words.is_empty() {
                let d = CaptionRules::default();
                let rules = CaptionRules { max_chars: 30, lines: 2, smart_balance: true, ..d };
                let rate = s.sequence_rate();
                let mut mapped_ai_caps: Vec<(usize, usize, Vec<String>)> = Vec::new();
                if !ai_by_item.is_empty() {
                    let mut run_start = 0usize;
                    while run_start < seq_words.len() {
                        let clip_id = seq_words[run_start].clip;
                        let item_id = seq_words[run_start].item;
                        let mut run_end = run_start + 1;
                        while run_end < seq_words.len() && seq_words[run_end].clip == clip_id {
                            run_end += 1;
                        }
                        if let Some(clip_caps) = ai_by_item.get(&item_id) {
                            for (c_start, c_end, lines) in clip_caps {
                                let mut first_seq = None;
                                let mut last_seq = None;
                                for (seq_idx, sw) in seq_words[run_start..run_end].iter().enumerate() {
                                    if sw.index >= *c_start && sw.index <= *c_end {
                                        let global_seq = run_start + seq_idx;
                                        if first_seq.is_none() {
                                            first_seq = Some(global_seq);
                                        }
                                        last_seq = Some(global_seq);
                                    }
                                }
                                if let (Some(a), Some(b)) = (first_seq, last_seq) {
                                    mapped_ai_caps.push((a, b, lines.clone()));
                                }
                            }
                        }
                        run_start = run_end;
                    }
                }
                let blocks = if !mapped_ai_caps.is_empty() {
                    tx::caption_blocks_from_ai_groups(&seq_words, &mapped_ai_caps, &rules, rate)
                } else {
                    tx::caption_blocks(&seq_words, &rules, rate)
                };
                let n_blocks = blocks.len();
                let _ = s.edit_sequence("Create AI Captions", |q, ctx, st| {
                    let tid = TrackId(ctx.alloc());
                    let mut t = CaptionTrack::new(tid, "Captions (AI)".into(), CaptionFormat::default());
                    t.captions = tx::blocks_to_captions(&blocks, ctx);
                    q.caption_tracks.insert(0, t);
                    st.caption_selection.clear();
                    Ok(tid)
                });
                if let Some(w) = ai_warn {
                    s.error_toast("transcript.ai", format!("Transcrição concluída ({n_words} palavras, {n_blocks} legendas), mas houve erro na IA: {w}"));
                } else if let Some(prov) = ai_provider {
                    s.toast(format!("Transcrição + IA ({prov}): {n_words} palavras ({total_ai_corrected} ajustadas) e {n_blocks} legendas criadas!"));
                } else {
                    s.toast(format!("Transcription finished: {n_words} word(s), {n_blocks} captions created"));
                }
                continue;
            }
        }

        if let Some(w) = ai_warn {
            s.error_toast("transcript.ai", format!("Transcrição concluída ({n_words} palavras), mas houve erro na IA: {w}"));
        } else if let Some(prov) = ai_provider {
            s.toast(format!("Transcrição + IA ({prov}) concluída: {n_words} palavras ({total_ai_corrected} ajustadas pela IA)"));
        } else {
            s.toast(format!("Transcription finished: {n_words} word(s) across {n_clips} clip(s)"));
        }
    }
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
    tx::apply_glossary(&mut t);
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
    Ok(json!({
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
    let mut words = sequence_words(s);
    let d = CaptionRules::default();
    let max_chars = u64_p(p, "maxChars").map(|n| n as usize).unwrap_or(d.max_chars);
    let lines = u64_p(p, "lines").map(|n| n as usize).unwrap_or(d.lines);
    let rules = CaptionRules {
        max_chars,
        lines,
        min_duration: f64_p(p, "minSeconds").map(Tick::from_seconds_f64).unwrap_or(d.min_duration),
        max_duration: f64_p(p, "maxSeconds").map(Tick::from_seconds_f64).unwrap_or(d.max_duration),
        gap_frames: p.get("gapFrames").and_then(Value::as_i64).unwrap_or(d.gap_frames),
        break_pause: d.break_pause,
        smart_balance: bool_p(p, "smartBalance").unwrap_or(max_chars <= 35),
    };
    let ma = s.prefs.media_analysis.clone();
    let use_ai = bool_p(p, "useAi").unwrap_or(ma.ai_refine) && !ma.ai_api_key.trim().is_empty() && !words.is_empty();
    let mut ai_status: Option<std::result::Result<(String, usize), String>> = None;
    let blocks = if use_ai {
        let mut temp_tr = Transcript {
            language: String::new(),
            speakers: Vec::new(),
            words: words
                .iter()
                .map(|w| filmcraft_project::Word { text: w.text.clone(), start: w.start, end: w.end, speaker: None, confidence: w.confidence })
                .collect(),
            source: "sequence".into(),
        };
        match filmcraft_speech::llm::refine_transcript(&mut temp_tr, &ma.ai_provider, &ma.ai_api_key, &ma.ai_prompt, max_chars, lines) {
            Ok(ai_res) if !ai_res.captions.is_empty() => {
                let mut next_transcripts = s.project.transcripts.clone();
                let mut changed_any = false;
                for (w, tw) in words.iter_mut().zip(temp_tr.words.iter()) {
                    if w.text != tw.text {
                        w.text = tw.text.clone();
                        if let Some(tr_arc) = next_transcripts.get_mut(&w.item) {
                            let tr_mut = Arc::make_mut(tr_arc);
                            if let Some(orig_w) = tr_mut.words.get_mut(w.index) {
                                orig_w.text = tw.text.clone();
                                changed_any = true;
                            }
                        }
                    }
                }
                if changed_any {
                    let _ = s.edit("AI Refine Transcript", move |pr, _| {
                        pr.transcripts = next_transcripts;
                        Ok(())
                    });
                }
                ai_status = Some(Ok((ai_res.provider_used, ai_res.corrected_words)));
                let groups: Vec<(usize, usize, Vec<String>)> = ai_res.captions.into_iter().map(|c| (c.start_word, c.end_word, c.lines)).collect();
                tx::caption_blocks_from_ai_groups(&words, &groups, &rules, s.sequence_rate())
            }
            Ok(ai_res) => {
                ai_status = Some(Ok((ai_res.provider_used, ai_res.corrected_words)));
                tx::caption_blocks(&words, &rules, s.sequence_rate())
            }
            Err(e) => {
                ai_status = Some(Err(e.to_string()));
                tx::caption_blocks(&words, &rules, s.sequence_rate())
            }
        }
    } else {
        tx::caption_blocks(&words, &rules, s.sequence_rate())
    };
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
    match ai_status {
        Some(Ok((prov, corr))) => {
            s.toast(format!("Legendas + IA ({prov}): {corr} palavras ajustadas e {n} legendas criadas!"));
        }
        Some(Err(err)) => {
            s.error_toast("transcript.ai", format!("Legendas criadas ({n}), mas houve erro na IA: {err}"));
        }
        None => {}
    }
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
            r#"{"items":[id]?,"model":"whisper-base"?,"language":"en|auto"?,"diarize":bool?,"maxSpeakers":n?}"#,
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
