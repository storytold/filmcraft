//! Background transcription. Workers own immutable inputs; only the session applies results.
use std::sync::{Arc, Mutex, atomic::Ordering};

use filmcraft_project::{ItemId, ItemKind, Project, Transcript};
use filmcraft_speech::{Options, SpeechError, Transcriber};
use serde_json::{Value, json};

use crate::commands::{bool_p, str_p, u64_p};
use crate::{EngineError, Job, Result, Session};

pub struct PendingTranscript {
    pub job: u64,
    project: Arc<Project>,
    media: Arc<crate::MediaPool>,
    results: Arc<Mutex<Option<Vec<(ItemId, Transcript)>>>>,
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// A recogniser is loaded in the worker, never on the UI thread.
enum Recognizer {
    Installed(Arc<dyn Transcriber>),
    Model(std::path::PathBuf, String),
}
impl Recognizer {
    fn load(self) -> std::result::Result<Arc<dyn Transcriber>, String> {
        match self {
            Self::Installed(t) => Ok(t),
            Self::Model(dir, id) => filmcraft_speech::load(&dir, &id).map_err(|e| e.to_string()),
        }
    }
}

pub(super) fn generate(s: &mut Session, p: &Value) -> Result<Value> {
    poll(s);
    if !s.transcript_jobs.is_empty() {
        return Err(EngineError::Other("transcription is already running; wait or cancel it in Progress".into()));
    }
    let items = super::targets(s, p);
    if items.is_empty() {
        return Err(EngineError::Other("nothing to transcribe (select media or open a sequence with audio)".into()));
    }
    let recognizer = if let Some(t) = &s.transcriber {
        Recognizer::Installed(t.clone())
    } else {
        let id = str_p(p, "model").unwrap_or(&s.prefs.media_analysis.whisper_model);
        let m = filmcraft_speech::models::find(id).ok_or_else(|| super::speech_err(SpeechError::UnknownModel(id.into())))?;
        let dir = super::models_dir().ok_or_else(|| EngineError::Other("no data directory for speech models".into()))?;
        if !filmcraft_speech::models::installed(&dir, m) {
            return Err(super::speech_err(SpeechError::NotInstalled(id.into())));
        }
        Recognizer::Model(dir, id.to_string())
    };
    let ma = &s.prefs.media_analysis;
    let language =
        str_p(p, "language").map(str::to_string).unwrap_or_else(|| if ma.language_auto_detect { "auto".into() } else { ma.default_language.clone() });
    let opts = Options {
        language: (!language.is_empty() && language != "auto").then_some(language),
        diarize: bool_p(p, "diarize").unwrap_or(ma.speaker_labeling != "off"),
        max_speakers: u64_p(p, "maxSpeakers").map(|n| n.clamp(1, 32) as usize).unwrap_or(6),
    };
    let mut work = Vec::new();
    let mut skipped = Vec::new();
    for item in items {
        let Some(m) = s.project.item(item).and_then(|i| match &i.kind {
            ItemKind::Media(m) => Some(m),
            _ => None,
        }) else {
            continue;
        };
        if !m.info.has_audio() {
            skipped.push(item.0);
            continue;
        }
        // Bound inference memory; decode in short chunks so cancellation also works while loading.
        let samples = m.duration().to_units_floor(i64::from(filmcraft_speech::SAMPLE_RATE));
        if samples <= 0 || samples > i64::from(filmcraft_speech::SAMPLE_RATE) * 3600 {
            return Err(EngineError::Other("transcription requires audio between one sample and one hour per media item".into()));
        }
        work.push((item, samples as usize));
    }
    if work.is_empty() {
        return Err(EngineError::Other("none of the clips has audio to transcribe".into()));
    }
    let id = s.jobs.iter().map(|j| j.id).max().unwrap_or(0).checked_add(1).ok_or_else(|| EngineError::Other("job id limit reached".into()))?;
    let job = Job { id, label: "Transcribe".into(), progress: Default::default(), result: Default::default() };
    job.progress.total.store(10_000, Ordering::Relaxed);
    let results = Arc::new(Mutex::new(None));
    let pending = PendingTranscript { job: id, project: s.project.clone(), media: s.media.clone(), results: results.clone() };
    let (project, media, services) = (s.project.clone(), s.media.clone(), s.services.clone());
    let (progress, result, output) = (job.progress.clone(), job.result.clone(), results.clone());
    let run = move || {
        let started = web_time::Instant::now();
        let task = || -> std::result::Result<Vec<(ItemId, Transcript)>, String> {
            if progress.cancel.load(Ordering::Relaxed) {
                return Err("stopped".into());
            }
            *lock(&progress.status) = "Loading speech model".into();
            let recognizer = recognizer.load()?;
            let mut transcripts = Vec::new();
            let total = work.len() as f32;
            for (n, (item, samples)) in work.iter().enumerate() {
                let src = media.source_for(&project, *item, &*services).ok_or_else(|| format!("media {} is unavailable", item.0))?;
                let mut audio = Vec::new();
                audio.try_reserve_exact(*samples).map_err(|e| format!("not enough memory for transcription: {e}"))?;
                let chunk = filmcraft_speech::SAMPLE_RATE as usize * 30;
                while audio.len() < *samples {
                    if progress.cancel.load(Ordering::Relaxed) {
                        return Err("stopped".into());
                    }
                    *lock(&progress.status) = format!("Loading audio {} of {}", n + 1, work.len());
                    let count = chunk.min(samples.saturating_sub(audio.len()));
                    let buffer = src.audio(audio.len() as i64, count, filmcraft_speech::SAMPLE_RATE).map_err(|e| e.to_string())?;
                    let mono = filmcraft_speech::downmix(&buffer.channels);
                    if mono.len() != count {
                        return Err("audio decoder returned an incomplete transcription chunk".into());
                    }
                    audio.extend(mono);
                }
                let mut transcript = recognizer
                    .transcribe(&audio, &opts, &mut |fraction, status| {
                        let fraction = if fraction.is_finite() { fraction.clamp(0.0, 1.0) } else { 0.0 };
                        progress.done.store(((n as f32 + fraction) / total * 10_000.0) as u64, Ordering::Relaxed);
                        *lock(&progress.status) = format!("{} of {}: {status}", n + 1, work.len());
                        !progress.cancel.load(Ordering::Relaxed)
                    })
                    .map_err(|e| e.to_string())?;
                transcript.normalize();
                transcript.check()?;
                transcripts.push((*item, transcript));
            }
            if progress.cancel.load(Ordering::Relaxed) {
                return Err("stopped".into());
            }
            Ok(transcripts)
        };
        let completion = task().map(|transcripts| {
            let words = transcripts.iter().map(|(_, t)| t.words.len() as u64).sum();
            *lock(&output) = Some(transcripts);
            filmcraft_export::Report {
                path: String::new(),
                frames: words,
                seconds: started.elapsed().as_secs_f64(),
                bytes: 0,
                render_fps: 0.0,
                extra_files: Vec::new(),
            }
        });
        if let Err(e) = &completion {
            *lock(&progress.status) = e.clone();
            *lock(&progress.error) = Some(e.clone());
        }
        *lock(&result) = Some(completion);
        progress.finished.store(true, Ordering::Release);
    };
    let run = crate::export_tools::guard_job(job.progress.clone(), job.result.clone(), run);
    let wait = bool_p(p, "wait").unwrap_or(true) || cfg!(target_arch = "wasm32");
    if wait {
        s.jobs.push(job);
        s.transcript_jobs.push(pending);
        run();
        let report = lock(&results).as_ref().map(|ts| {
            ts.iter()
                .map(|(i, t)| json!({"item": i.0, "words": t.words.len(), "speakers": t.speakers.len(), "language": t.language, "source": t.source}))
                .collect::<Vec<_>>()
        });
        poll(s);
        if let Some(Err(e)) = s.jobs.iter().find(|j| j.id == id).and_then(|j| lock(&j.result).clone()) {
            return Err(EngineError::Other(e));
        }
        Ok(json!({"job": id, "items": report.unwrap_or_default(), "skipped": skipped}))
    } else {
        std::thread::Builder::new()
            .name("filmcraft-transcription".into())
            .spawn(run)
            .map_err(|e| EngineError::Other(format!("could not start transcription: {e}")))?;
        s.jobs.push(job);
        s.transcript_jobs.push(pending);
        Ok(json!({"job": id, "skipped": skipped}))
    }
}

/// Stop pending work before installing a different project, even if its media IDs match.
pub(crate) fn cancel_pending(s: &mut Session) {
    for pending in &s.transcript_jobs {
        if let Some(job) = s.jobs.iter().find(|j| j.id == pending.job) {
            job.progress.cancel.store(true, Ordering::Relaxed);
        }
    }
    s.transcript_jobs.clear();
}

pub(crate) fn poll(s: &mut Session) {
    let mut i = 0;
    while i < s.transcript_jobs.len() {
        let pending = &s.transcript_jobs[i];
        let job = s.jobs.iter().find(|j| j.id == pending.job).cloned();
        if job.as_ref().is_some_and(|j| !j.progress.finished.load(Ordering::Acquire)) {
            i += 1;
            continue;
        }
        let pending = s.transcript_jobs.remove(i);
        let Some(job) = job else { continue };
        if job.progress.cancel.load(Ordering::Relaxed) {
            *lock(&job.result) = Some(Err("stopped".into()));
            *lock(&job.progress.status) = "Stopped: nothing was changed".into();
            continue;
        }
        let Some(results) = lock(&pending.results).take() else {
            if let Some(Err(e)) = lock(&job.result).clone() {
                s.error_toast("transcript.generate", e);
            }
            continue;
        };
        let stale = !Arc::ptr_eq(&s.media, &pending.media)
            || results.iter().any(|(item, _)| {
                s.project.item(*item) != pending.project.item(*item) || s.project.transcripts.get(item) != pending.project.transcripts.get(item)
            });
        let applied = if stale {
            Err(EngineError::Other("media or transcript changed during transcription; results were discarded".into()))
        } else {
            s.edit("Transcribe", move |pr, _| {
                for (item, transcript) in results {
                    pr.transcripts.insert(item, Arc::new(transcript));
                }
                Ok(())
            })
        };
        if let Err(e) = applied {
            *lock(&job.result) = Some(Err(e.to_string()));
            *lock(&job.progress.status) = e.to_string();
            s.error_toast("transcript.generate", e.to_string());
        } else {
            job.progress.done.store(10_000, Ordering::Relaxed);
            *lock(&job.progress.status) = "Transcription applied".into();
        }
    }
}

pub(super) fn download(s: &mut Session, p: &Value) -> Result<Value> {
    let id = str_p(p, "model").unwrap_or(filmcraft_speech::models::DEFAULT_MODEL);
    let model = filmcraft_speech::models::find(id).ok_or_else(|| super::speech_err(SpeechError::UnknownModel(id.into())))?;
    let dir = super::models_dir().ok_or_else(|| EngineError::Other("no data directory for speech models".into()))?;
    #[cfg(not(feature = "speech-download"))]
    {
        let _ = (s, model, dir);
        Err(EngineError::Other("model downloads are not available in this build (built without the `speech-download` feature)".into()))
    }
    #[cfg(feature = "speech-download")]
    {
        if s.jobs.iter().any(|j| j.label == "Download Speech Model" && lock(&j.result).is_none()) {
            return Err(EngineError::Other("a speech model download is already running".into()));
        }
        let id = s.jobs.iter().map(|j| j.id).max().unwrap_or(0).checked_add(1).ok_or_else(|| EngineError::Other("job id limit reached".into()))?;
        let job = Job { id, label: "Download Speech Model".into(), progress: Default::default(), result: Default::default() };
        let (progress, result) = (job.progress.clone(), job.result.clone());
        let output = filmcraft_speech::models::model_dir(&dir, model).to_string_lossy().into_owned();
        let model_path = output.clone();
        let run = move || {
            let started = web_time::Instant::now();
            let completion = filmcraft_speech::models::download(&dir, model, &mut |done, total, status| {
                progress.done.store(done, Ordering::Relaxed);
                progress.total.store(total, Ordering::Relaxed);
                *lock(&progress.status) = status.into();
                !progress.cancel.load(Ordering::Relaxed)
            })
            .map_err(|e| e.to_string())
            .map(|()| filmcraft_export::Report {
                path: output,
                frames: 0,
                seconds: started.elapsed().as_secs_f64(),
                bytes: model.size(),
                render_fps: 0.0,
                extra_files: Vec::new(),
            });
            if let Err(e) = &completion {
                *lock(&progress.error) = Some(e.clone());
            }
            *lock(&progress.status) = completion.as_ref().map(|_| "Model installed".to_string()).unwrap_or_else(Clone::clone);
            *lock(&result) = Some(completion);
            progress.finished.store(true, Ordering::Release);
        };
        let run = crate::export_tools::guard_job(job.progress.clone(), job.result.clone(), run);
        if bool_p(p, "wait").unwrap_or(true) || cfg!(target_arch = "wasm32") {
            run();
            let result = lock(&job.result).clone();
            s.jobs.push(job);
            if let Some(Err(e)) = result {
                return Err(EngineError::Other(e));
            }
        } else {
            std::thread::Builder::new().name("filmcraft-speech-download".into()).spawn(run).map_err(|e| EngineError::Other(e.to_string()))?;
            s.jobs.push(job);
        }
        Ok(json!({"job": id, "model": model.id, "dir": model_path}))
    }
}
