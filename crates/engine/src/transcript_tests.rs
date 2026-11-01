use std::sync::Arc;

use serde_json::json;

use crate::Session;
use filmcraft_project::{ItemId, Transcript, Word};
use filmcraft_speech::FixedTranscriber;
use filmcraft_time::Tick;

/// The demo project with a fake transcriber whose transcript fits the first A1 clip's media:
/// "Hello um world." (Speaker 1), a 1.2 s pause, "Second speaker here." (Speaker 2).
fn session() -> (Session, ItemId, Tick) {
    let mut s = Session::default();
    s.execute("file.openDemoProject", json!({})).unwrap();
    let q = s.active_sequence().unwrap();
    let a = &q.audio_tracks[0].items[0];
    let (item, sin) = (a.item, a.source_in);
    let sec = |x: f64| sin + Tick::from_seconds_f64(x);
    let mut t = Transcript { language: "en".into(), ..Default::default() };
    for (text, a, b, sp) in
        [("Hello", 0.2, 0.5, 0), ("um", 0.6, 0.9, 0), ("world.", 1.0, 1.4, 0), ("Second", 2.6, 3.0, 1), ("speaker", 3.0, 3.5, 1), ("here.", 3.5, 4.0, 1)]
    {
        let mut w = Word::new(text, sec(a), sec(b));
        w.speaker = Some(sp);
        t.words.push(w);
    }
    t.normalize();
    s.transcriber = Some(Arc::new(FixedTranscriber { transcript: t, id: "fixed".into() }));
    let start = a_start(&s);
    (s, item, start)
}

fn a_start(s: &Session) -> Tick {
    s.active_sequence().unwrap().audio_tracks[0].items[0].start
}

fn words(s: &mut Session) -> Vec<String> {
    let r = s.execute("transcript.inspect", json!({})).unwrap();
    r["words"].as_array().unwrap().iter().map(|w| w["text"].as_str().unwrap().to_string()).collect()
}

struct DelayedTranscriber {
    inner: Arc<dyn filmcraft_speech::Transcriber>,
    started: std::sync::mpsc::Sender<()>,
    release: std::sync::Mutex<std::sync::mpsc::Receiver<()>>,
}

impl filmcraft_speech::Transcriber for DelayedTranscriber {
    fn id(&self) -> String {
        "delayed".into()
    }

    fn transcribe(
        &self,
        audio: &[f32],
        opts: &filmcraft_speech::Options,
        progress: filmcraft_speech::ProgressFn,
    ) -> Result<Transcript, filmcraft_speech::SpeechError> {
        self.started.send(()).unwrap();
        self.release.lock().unwrap().recv_timeout(std::time::Duration::from_secs(20)).unwrap();
        self.inner.transcribe(audio, opts, progress)
    }
}

fn delayed_session() -> (Session, ItemId, u64, std::sync::mpsc::Sender<()>) {
    let (mut s, item, _) = session();
    let (started, ready) = std::sync::mpsc::channel();
    let (release, wait) = std::sync::mpsc::channel();
    s.transcriber = Some(Arc::new(DelayedTranscriber { inner: s.transcriber.take().unwrap(), started, release: std::sync::Mutex::new(wait) }));
    let id = s.execute("transcript.generate", json!({"items": [item.0], "wait": false})).unwrap()["job"].as_u64().unwrap();
    ready.recv_timeout(std::time::Duration::from_secs(20)).unwrap();
    (s, item, id, release)
}

#[test]
fn background_transcription_preserves_a_newer_transcript_edit() {
    let (mut s, item, id, release) = delayed_session();
    s.execute("transcript.set", json!({"item": item.0, "transcript": {"words": [{"text": "Edited", "start": 0, "end": 1000}]}})).unwrap();
    let before = s.project.clone();
    let undo = s.history.undo.len();
    release.send(()).unwrap();
    finish_job(&mut s, id);
    assert_eq!(s.project.transcripts[&item].text(), "Edited");
    assert_eq!(s.project, before);
    assert_eq!(s.history.undo.len(), undo, "discarded work does not create an undo entry");
    let job = s.jobs.iter().find(|j| j.id == id).unwrap();
    assert!(job.progress.error.lock().unwrap().as_ref().is_some_and(|e| e.contains("changed")));
}

#[test]
fn background_transcription_survives_an_unrelated_edit() {
    let (mut s, item, id, release) = delayed_session();
    s.execute("file.newBin", json!({"name": "Unrelated"})).unwrap();
    let undo = s.history.undo.len();
    release.send(()).unwrap();
    finish_job(&mut s, id);
    assert_eq!(s.project.transcripts[&item].words.len(), 6);
    assert_eq!(s.history.undo.len(), undo + 1);
}

#[test]
fn transcription_refuses_exhausted_job_ids_without_mutation() {
    let (mut s, item, _) = session();
    s.jobs.push(crate::Job { id: u64::MAX, label: "Existing".into(), progress: Default::default(), result: Default::default() });
    let before = s.project.clone();
    let error = s.execute("transcript.generate", json!({"items": [item.0], "wait": false})).unwrap_err();
    assert!(error.to_string().contains("job id limit"));
    assert_eq!(s.project, before);
    assert!(s.transcribe_jobs.is_empty());
    assert_eq!(s.jobs.len(), 1);
}

#[test]
fn transcription_refuses_oversized_sources_before_loading_audio() {
    let (mut s, _, _) = session();
    let item = s.execute("file.newBarsAndTone", json!({"seconds": 3601.0})).unwrap()["item"].as_u64().unwrap();
    let before = s.project.clone();
    let error = s.execute("transcript.generate", json!({"items": [item], "wait": false})).unwrap_err();
    assert!(error.to_string().contains("one hour"), "{error}");
    assert_eq!(s.project, before);
    assert!(s.jobs.is_empty());
}

#[test]
fn a_second_transcription_cannot_load_another_model_concurrently() {
    let (mut s, item, id, release) = delayed_session();
    let error = s.execute("transcript.generate", json!({"items": [item.0], "wait": false})).unwrap_err();
    release.send(()).unwrap();
    finish_job(&mut s, id);
    assert!(error.to_string().contains("already running"));
    assert_eq!(s.jobs.len(), 1);
    assert_eq!(s.project.transcripts[&item].words.len(), 6);
}

#[cfg(feature = "speech-download")]
#[test]
fn a_second_model_download_is_refused_before_network_access() {
    let mut s = Session::default();
    s.jobs.push(crate::Job { id: 1, label: "Download Speech Model: Existing".into(), progress: Default::default(), result: Default::default() });
    let error = s.execute("transcript.downloadModel", json!({"model": "whisper-base", "wait": false})).unwrap_err();
    assert!(error.to_string().contains("already running"));
    assert_eq!(s.jobs.len(), 1);
}

#[test]
fn generate_inspect_search_and_rename() {
    let (mut s, item, start) = session();
    assert!(s.execute("transcript.select", json!({"from": 0})).is_err(), "disabled before transcribing");
    let r = s.execute("transcript.generate", json!({"items": [item.0]})).unwrap();
    assert_eq!(r["items"][0]["words"], 6, "{r}");
    assert_eq!(r["items"][0]["source"], "fixed");
    assert_eq!(s.project.transcripts[&item].words.len(), 6);

    let r = s.execute("transcript.inspect", json!({})).unwrap();
    assert_eq!(words(&mut s), ["Hello", "um", "world.", "Second", "speaker", "here."]);
    assert_eq!(r["words"][0]["start"].as_i64().unwrap(), (start + Tick::from_seconds_f64(0.2)).0, "mapped to sequence time");
    assert_eq!(r["paragraphs"].as_array().unwrap().len(), 2, "speaker change splits paragraphs: {}", r["paragraphs"]);
    assert_eq!(r["speakers"], json!(["Speaker 1", "Speaker 2"]));

    let r = s.execute("transcript.search", json!({"query": "second spea"})).unwrap();
    assert_eq!(r["matches"], json!([{"from": 3, "to": 4, "start": r["matches"][0]["start"], "end": r["matches"][0]["end"]}]));

    s.execute("transcript.renameSpeaker", json!({"speaker": "Speaker 2", "name": "Ann"})).unwrap();
    let r = s.execute("transcript.inspect", json!({})).unwrap();
    assert_eq!(r["words"][3]["speaker"], "Ann");
    s.execute("edit.undo", json!({})).unwrap();
    assert_eq!(s.project.transcripts[&item].speakers[1].name, "Speaker 2");
    s.execute("transcript.renameSpeaker", json!({"speaker": 0, "item": item.0, "name": "Bo"})).unwrap();
    assert_eq!(s.project.transcripts[&item].speakers[0].name, "Bo");
    assert!(s.execute("transcript.renameSpeaker", json!({"speaker": "Nobody", "name": "X"})).is_err());

    // transcribing is one undo step
    s.execute("edit.undo", json!({})).unwrap();
    s.execute("edit.undo", json!({})).unwrap();
    assert!(s.project.transcripts.is_empty());
}

#[test]
fn select_extract_and_lift_by_words() {
    let (mut s, item, _) = session();
    s.execute("transcript.generate", json!({"items": [item.0]})).unwrap();
    let rate = s.sequence_rate();
    let before = s.active_sequence().unwrap().duration();

    let r = s.execute("transcript.select", json!({"from": 3, "to": 5})).unwrap();
    let (a, b) = (Tick(r["start"].as_i64().unwrap()), Tick(r["end"].as_i64().unwrap()));
    let q = s.active_sequence().unwrap();
    assert_eq!(q.mark_in, Some(a));
    assert_eq!(q.mark_out, Some(b - rate.frame_duration()), "Out is the last frame inside");
    assert_eq!(rate.snap(a), a, "frame aligned");
    assert_eq!(s.playhead(), a);

    // Extract "um": the sequence gets shorter by the word's frames and the word is gone
    let r = s.execute("transcript.extract", json!({"from": 1})).unwrap();
    let cut = Tick(r["end"].as_i64().unwrap()) - Tick(r["start"].as_i64().unwrap());
    assert!(cut > Tick::ZERO);
    assert_eq!(s.active_sequence().unwrap().duration(), before - cut);
    assert!(!words(&mut s).contains(&"um".to_string()), "{:?}", words(&mut s));
    s.active_sequence().unwrap().check().unwrap();
    s.execute("edit.undo", json!({})).unwrap();
    assert_eq!(s.active_sequence().unwrap().duration(), before);

    // Lift leaves a gap: same duration, word gone
    s.execute("transcript.lift", json!({"from": 1, "to": 1})).unwrap();
    assert_eq!(s.active_sequence().unwrap().duration(), before);
    assert_eq!(words(&mut s), ["Hello", "world.", "Second", "speaker", "here."]);
    assert!(s.execute("transcript.extract", json!({"from": 99})).is_err());
}

#[test]
fn remove_fillers_pauses_and_create_captions() {
    let (mut s, item, _) = session();
    s.execute("transcript.generate", json!({"items": [item.0]})).unwrap();
    let before = s.active_sequence().unwrap().duration();
    let r = s.execute("transcript.removeFillers", json!({})).unwrap();
    assert_eq!(r["removed"], 1, "{r}");
    assert!(!words(&mut s).contains(&"um".to_string()));
    let r = s.execute("transcript.removePauses", json!({"minSeconds": 1.0, "keepSeconds": 0.1})).unwrap();
    assert_eq!(r["removed"], 1, "{r}");
    let after = s.active_sequence().unwrap().duration();
    assert!(after < before - Tick::from_seconds_f64(1.0), "the pause and the filler are gone");
    assert_eq!(words(&mut s).len(), 5);
    s.active_sequence().unwrap().check().unwrap();

    let r = s.execute("transcript.createCaptions", json!({"maxChars": 32})).unwrap();
    assert_eq!(r["captions"], 2, "one caption per speaker: {r}");
    let q = s.active_sequence().unwrap();
    let tr = &q.caption_tracks[0];
    assert_eq!(tr.captions[0].text, "Hello world.");
    assert_eq!(tr.captions[1].speaker.as_deref(), Some("Speaker 2"));
    q.check().unwrap();
}

#[test]
fn set_delete_and_models() {
    let (mut s, item, _) = session();
    let t = json!({"language": "en", "words": [
        {"text": "b", "start": 2000, "end": 3000, "speaker": 1},
        {"text": "a", "start": 0, "end": 1000},
    ]});
    let r = s.execute("transcript.set", json!({"item": item.0, "transcript": t})).unwrap();
    assert_eq!(r["words"], 2);
    let tr = &s.project.transcripts[&item];
    assert_eq!(tr.words[0].text, "a", "normalized: sorted");
    assert_eq!(tr.speakers.len(), 2, "missing speaker labels added");
    assert_eq!(tr.source, "imported");
    assert!(s.execute("transcript.set", json!({"item": 999_999, "transcript": {}})).is_err());
    assert!(s.execute("transcript.set", json!({"item": item.0, "transcript": {"words": 3}})).is_err());

    // survives save/load
    let bytes = filmcraft_format::encode(&s.project, true);
    let back = filmcraft_format::decode(&bytes).unwrap();
    assert_eq!(back.project.transcripts[&item].words.len(), 2);

    s.execute("transcript.delete", json!({"items": [item.0]})).unwrap();
    assert!(s.project.transcripts.is_empty());
    assert!(s.execute("transcript.delete", json!({})).is_err());

    let m = s.execute("transcript.models", json!({})).unwrap();
    assert_eq!(m["available"], filmcraft_speech::available());
    assert!(m["models"].as_array().unwrap().iter().any(|x| x["id"] == "whisper-base"));
}

#[test]
fn generate_without_a_transcriber() {
    let (mut s, item, _) = session();
    s.transcriber = None;
    let e = s.execute("transcript.generate", json!({"items": [item.0], "model": "nope"})).unwrap_err().to_string();
    // a build without speech-to-text says that first: the command is disabled (#97)
    let why = if filmcraft_speech::available() { "unknown speech model" } else { "not available in this build" };
    assert!(e.contains(why), "{e}");
    if !filmcraft_speech::available() {
        assert!(!s.is_enabled("sequence.transcribe"), "Transcribe Sequence follows transcript.generate");
        let e = s.execute("transcript.generate", json!({"items": [item.0]})).unwrap_err().to_string();
        assert!(e.contains("whisper") && e.contains("not available"), "{e}");
        #[cfg(not(feature = "speech-download"))]
        assert!(s.execute("transcript.downloadModel", json!({})).unwrap_err().to_string().contains("not available"));
    }
    // defaults to the media of the open sequence's audio clips
    s.transcriber = Some(Arc::new(FixedTranscriber { transcript: Transcript::default(), id: "empty".into() }));
    assert!(s.is_enabled("sequence.transcribe"), "an installed recogniser enables Transcribe Sequence");
    let r = s.execute("transcript.generate", json!({})).unwrap();
    assert!(r["items"].as_array().unwrap().len() >= 2, "{r}");
}

/// G19: the model Transcribe needs (Settings ▸ Speech model), and none once a host installed its
/// own recogniser; `transcript.downloadModel` refuses without the download feature and names an
/// unknown model.
#[test]
fn the_selected_speech_model_and_download_refusals() {
    let mut s = Session::default();
    let (m, _) = crate::transcript::selected_model(&s).expect("the default model is in the catalogue");
    assert_eq!(m.id, filmcraft_speech::models::DEFAULT_MODEL);
    assert!(m.size() > 0 && m.license.contains("MIT"));
    assert!(s.execute("transcript.downloadModel", json!({"model": "no-such-model"})).is_err());
    if !cfg!(feature = "speech-download") {
        let installed = crate::transcript::selected_model(&s).is_some_and(|(_, i)| i);
        let r = s.execute("transcript.downloadModel", json!({"wait": false}));
        assert!(installed || r.is_err(), "{r:?}");
        assert!(s.jobs.is_empty(), "no job without downloads");
    }
    s.transcriber = Some(Arc::new(FixedTranscriber { transcript: Transcript::default(), id: "fixed".into() }));
    assert!(crate::transcript::selected_model(&s).is_none(), "an installed recogniser needs no model");
}

/// Wait (bounded) until the background job `id` has finished, then apply it as a UI frame would.
fn finish_job(s: &mut Session, id: u64) {
    for _ in 0..1000 {
        let done = s.jobs.iter().find(|j| j.id == id).is_none_or(|j| j.progress.finished.load(std::sync::atomic::Ordering::Relaxed));
        if done {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    s.poll_persistence();
}

/// `"wait": false` transcribes in a background job (the app): it returns at once with the job,
/// and the transcript is stored, as one undo step, once the job has finished.
#[test]
fn a_background_transcription_stores_its_transcript_when_it_finishes() {
    let (mut s, item, _) = session();
    let undo = s.history.undo.len();
    let r = s.execute("transcript.generate", json!({"items": [item.0], "wait": false})).unwrap();
    let id = r["job"].as_u64().expect("a job");
    assert_eq!(crate::transcript::running_job(&s), Some(id));
    finish_job(&mut s, id);
    assert!(s.project.transcripts.contains_key(&item));
    assert_eq!(s.history.undo.len(), undo + 1, "one undo step");
    assert!(crate::transcript::running_job(&s).is_none());
    let job = s.jobs.iter().find(|j| j.id == id).unwrap();
    assert_eq!(job.progress.done.load(std::sync::atomic::Ordering::Relaxed), job.progress.total.load(std::sync::atomic::Ordering::Relaxed));
}

/// A cancelled background transcription, or one whose project was replaced meanwhile, stores
/// nothing.
#[test]
fn a_cancelled_or_outdated_background_transcription_stores_nothing() {
    let (mut s, item, _) = session();
    let id = s.execute("transcript.generate", json!({"items": [item.0], "wait": false})).unwrap()["job"].as_u64().unwrap();
    s.jobs.iter().find(|j| j.id == id).unwrap().progress.cancel.store(true, std::sync::atomic::Ordering::Relaxed);
    finish_job(&mut s, id);
    assert!(s.project.transcripts.is_empty(), "cancelled: nothing stored");
    assert!(crate::transcript::running_job(&s).is_none());

    let (mut s, item, _) = session();
    let id = s.execute("transcript.generate", json!({"items": [item.0], "wait": false})).unwrap()["job"].as_u64().unwrap();
    let transcriber = s.transcriber.clone();
    s.execute("file.openDemoProject", json!({})).unwrap();
    s.transcriber = transcriber;
    finish_job(&mut s, id);
    assert!(s.project.transcripts.is_empty(), "another project: nothing stored in it");
}

/// #2517 review: a catalogue model is loaded in the background job, not before it: the call
/// returns the job at once, and a model that can't be loaded (here: not downloaded) ends the job
/// with the reason and stores nothing.
#[cfg(feature = "whisper")]
#[test]
fn a_background_transcription_loads_its_model_in_the_job() {
    let (mut s, item, _) = session();
    s.transcriber = None;
    let model = "whisper-tiny";
    let dir = crate::transcript::models_dir();
    let m = filmcraft_speech::models::find(model).unwrap();
    if dir.as_ref().is_some_and(|d| filmcraft_speech::models::installed(d, m)) {
        eprintln!("SKIPPED: {model} is installed here");
        return;
    }
    let r = s.execute("transcript.generate", json!({"items": [item.0], "model": model, "wait": false})).unwrap();
    let id = r["job"].as_u64().expect("the call returns the job at once");
    finish_job(&mut s, id);
    let job = s.jobs.iter().find(|j| j.id == id).unwrap();
    let err = job.progress.error.lock().unwrap().clone().unwrap_or_default();
    assert!(err.contains("not downloaded"), "{err}");
    assert!(s.project.transcripts.is_empty());
}

/// The Create captions dialog's settings: the style lands on the new track, hostile numbers are
/// clamped (lines × characters no longer overflows), and the settings survive a prefs round trip.
#[test]
fn create_captions_with_dialog_settings() {
    use crate::transcript::{CaptionPrefs, CaptionSettings, builtin_caption_presets};
    use filmcraft_project::CaptionStyle;
    let (mut s, _, _) = session();
    s.execute("transcript.generate", json!({})).unwrap();
    let style = CaptionStyle { font: "Lato".into(), size: 80.0, background: false, ..Default::default() };
    let set = CaptionSettings { lines: 1, max_chars: 7, min_seconds: 1.2, style: Some(style.clone()), style_name: "TEXS.ORG".into(), ..Default::default() };
    let r = s.execute("transcript.createCaptions", set.params()).unwrap();
    let q = s.active_sequence().unwrap();
    let tr = &q.caption_tracks[0];
    assert_eq!(tr.style, style);
    assert_eq!(r["captions"].as_u64(), Some(tr.captions.len() as u64));
    assert!(tr.captions.iter().all(|c| !c.text.contains('\n')), "single line");
    q.check().unwrap();

    let r = s.execute("transcript.createCaptions", json!({"maxChars": u64::MAX, "lines": u64::MAX, "minSeconds": -1e300, "gapFrames": i64::MIN})).unwrap();
    assert!(r["captions"].as_u64().is_some_and(|n| n > 0));
    assert!(s.execute("transcript.createCaptions", json!({"style": 5})).is_err());

    let prefs = CaptionPrefs { last: set.clone(), user_presets: vec![] };
    let back: CaptionPrefs = serde_json::from_value(serde_json::to_value(&prefs).unwrap()).unwrap();
    assert_eq!(back, prefs);
    let old: crate::autosave::Preferences = serde_json::from_value(json!({"version": 2})).unwrap();
    assert_eq!(old.captions.last, CaptionSettings::default(), "older prefs files open with the defaults");
    assert_eq!(builtin_caption_presets()[0].settings, CaptionSettings::default());

    // the dialog always sends its minimum: the Word by Word preset's 0 keeps each word's own
    // timing, and a word-by-word setup with a minimum keeps that minimum
    let words = builtin_caption_presets().into_iter().find(|p| p.name == "Word by Word").unwrap().settings;
    assert_eq!((words.params()["wordsPerCaption"].as_u64(), words.params()["minSeconds"].as_f64()), (Some(1), Some(0.0)));
    let held = CaptionSettings { words_per_caption: 1, min_seconds: 1.2, ..words };
    assert_eq!(held.params()["minSeconds"].as_f64(), Some(1.2));
}

#[test]
fn delete_model_requires_explicit_confirmation_and_rejects_hostile_parameters() {
    let (mut s, _, _) = session();
    let before = s.project.clone();
    let undo = s.history.undo.len();
    for params in [
        json!({}),
        json!({"model":7,"confirm":true}),
        json!({"model":"whisper-tiny"}),
        json!({"model":"whisper-tiny","confirm":"true"}),
        json!({"model":"whisper-tiny","confirm":false}),
        json!({"model":"../escape","confirm":true}),
        json!({"model":"unknown","confirm":true}),
        json!([]),
    ] {
        assert!(s.execute("transcript.deleteModel", params.clone()).is_err(), "{params}");
    }
    assert_eq!(s.project, before);
    assert_eq!(s.history.undo.len(), undo);
}

#[test]
fn delete_model_refuses_active_transcription_without_changing_the_project() {
    let (mut s, _, id, release) = delayed_session();
    let before = s.project.clone();
    let undo = s.history.undo.len();
    let error = s.execute("transcript.deleteModel", json!({"model":"whisper-tiny","confirm":true})).unwrap_err();
    assert!(error.to_string().contains("stop speech transcription"), "{error}");
    assert_eq!(s.project, before);
    assert_eq!(s.history.undo.len(), undo);
    release.send(()).unwrap();
    finish_job(&mut s, id);
}

#[test]
fn delete_model_refuses_an_active_download() {
    let mut s = Session::default();
    s.jobs.push(crate::Job { id: 1, label: "Download Speech Model: Existing".into(), progress: Default::default(), result: Default::default() });
    let before = s.project.clone();
    let error = s.execute("transcript.deleteModel", json!({"model":"whisper-tiny","confirm":true})).unwrap_err();
    assert!(error.to_string().contains("stop speech transcription"));
    assert_eq!(s.project, before);
    assert_eq!(s.jobs.len(), 1);
}

#[test]
fn transcription_progress_does_not_replace_an_unrelated_sequences_transcript() {
    let (mut s, _, id, release) = delayed_session();
    let first = s.state.active_sequence.unwrap();
    assert_eq!(crate::transcript::running_job(&s), Some(id));
    s.execute("file.newSequence", json!({"name":"Other sequence","video":1,"audio":1})).unwrap();
    let unrelated = crate::transcript::running_job(&s);
    s.execute("sequence.open", json!({"item":first.0})).unwrap();
    let original = crate::transcript::running_job(&s);
    release.send(()).unwrap();
    finish_job(&mut s, id);
    assert_eq!(unrelated, None, "an unrelated sequence must stay readable while another clip is transcribed");
    assert_eq!(original, Some(id));
}

#[test]
fn transcript_correction_preserves_timing_and_round_trips() {
    let (mut s, item, _) = session();
    s.execute("transcript.generate", json!({"item": item.0})).unwrap();
    let original = s.project.transcripts[&item].words[0].clone();
    s.execute("transcript.correctWord", json!({"item": item.0, "index": 0, "text": "Hallo", "expected": "Hello"})).unwrap();
    let corrected = &s.project.transcripts[&item].words[0];
    assert_eq!(corrected.text, "Hallo");
    assert_eq!((corrected.start, corrected.end, corrected.speaker), (original.start, original.end, original.speaker));
    let encoded = filmcraft_format::encode(&s.project, false);
    let decoded = filmcraft_format::decode(&encoded).unwrap().project;
    assert_eq!(decoded.transcripts[&item].words[0].text, "Hallo");
    assert!(s.execute("transcript.correctWord", json!({"item": item.0, "index": 0, "text": "Oops", "expected": "Hello"})).is_err());
    for params in [
        json!({"item": item.0, "index": u64::MAX, "text": "x"}),
        json!({"item": item.0, "index": 0, "text": ""}),
        json!({"item": item.0, "index": 0, "text": "two words"}),
        json!({"item": item.0, "index": 0, "text": "x".repeat(1025)}),
    ] {
        assert!(s.execute("transcript.correctWord", params).is_err());
    }
    s.execute("edit.undo", json!({})).unwrap();
    assert_eq!(s.project.transcripts[&item].words[0], original);
    s.execute("edit.redo", json!({})).unwrap();
    assert_eq!(s.project.transcripts[&item].words[0].text, "Hallo");
}

#[test]
fn transcript_source_view_resolves_subclips() {
    let (mut s, item, _) = session();
    s.execute("transcript.generate", json!({"item": item.0})).unwrap();
    s.execute("source.open", json!({"item": item.0})).unwrap();
    let root_words = crate::transcript::source_words(&s);
    assert_eq!(root_words.len(), 6);
    let a = s.project.transcripts[&item].words[0].start;
    let b = s.project.transcripts[&item].words[1].end;
    let id = s
        .edit("Test subclip", |p, _| {
            let id = ItemId(p.alloc_id());
            let mut sub = p.item(item).unwrap().clone();
            sub.id = id;
            sub.kind = filmcraft_project::ItemKind::Subclip { parent: item, range: filmcraft_time::TimeRange::from_bounds(a, b), restrict_trims: true };
            p.items.insert(id, sub);
            Ok(id)
        })
        .unwrap();
    s.execute("source.open", json!({"item": id.0})).unwrap();
    let words = crate::transcript::source_words(&s);
    assert_eq!(words.len(), 2);
    assert!(words.iter().all(|w| w.item == item));
    let reply = s.execute("transcript.source", json!({})).unwrap();
    assert_eq!(reply["words"][0]["item"], item.0);
    assert_eq!(reply["words"][1]["index"], 1);
    let seq = s.state.active_sequence.unwrap();
    s.edit("Use subclip", |p, _| {
        let clip = &mut p.sequence_mut(seq).unwrap().audio_tracks[0].items[0];
        clip.item = id;
        clip.source_in = a;
        clip.duration = b - a;
        Ok(())
    })
    .unwrap();
    let words = crate::transcript::sequence_words(&s);
    assert_eq!(words.len(), 2, "parent transcript also resolves through sequence subclip references");
    assert!(words.iter().all(|w| w.item == item));
}

#[test]
fn speaker_assignment_is_precise_undoable_and_persistent() {
    let (mut s, item, _) = session();
    s.execute("transcript.generate", json!({"item": item.0})).unwrap();
    let before = s.project.clone();
    let expected = json!([{"index": 0, "word": before.transcripts[&item].words[0]}, {"index": 2, "word": before.transcripts[&item].words[2]}]);
    let params = json!({"item": item.0, "indices": [2, 0, 2], "name": "Alice", "expected": expected});
    let r = s.execute("transcript.assignSpeaker", params.clone()).unwrap();
    assert_eq!(r["words"], 2);
    let after = s.project.clone();
    let tr = &after.transcripts[&item];
    assert_eq!(tr.speakers[2].name, "Alice");
    for (i, (old, new)) in before.transcripts[&item].words.iter().zip(&tr.words).enumerate() {
        let mut want = old.clone();
        if i == 0 || i == 2 {
            want.speaker = Some(2);
        }
        assert_eq!(*new, want, "only selected speaker assignments may change");
    }
    assert!(s.execute("transcript.assignSpeaker", params).is_err(), "stale speaker assignment rejected");
    s.execute("edit.undo", json!({})).unwrap();
    assert_eq!(*s.project, *before);
    s.execute("edit.redo", json!({})).unwrap();
    assert_eq!(*s.project, *after);
    let path = std::env::temp_dir().join(format!("filmcraft-speaker-persistence-{}.fcproj", std::process::id()));
    s.execute("file.saveAs", json!({"path": path})).unwrap();
    s.execute("file.open", json!({"path": path})).unwrap();
    assert_eq!(s.project.transcripts, after.transcripts);
    std::fs::remove_file(path).unwrap();
    s.execute("transcript.assignSpeaker", json!({"item": item.0, "indices": [0], "speaker": null})).unwrap();
    assert_eq!(s.project.transcripts[&item].words[0].speaker, None);
    s.execute("transcript.assignSpeaker", json!({"item": item.0, "indices": [1], "name": "Alice"})).unwrap();
    assert_eq!(s.project.transcripts[&item].speakers.len(), 3, "existing name reused");
}

#[test]
fn speaker_commands_reject_hostile_input_without_changes() {
    let (mut s, item, _) = session();
    s.execute("transcript.generate", json!({"item": item.0})).unwrap();
    let before = s.project.clone();
    for params in [
        json!({}),
        json!({"item": 999999, "indices": [0], "speaker": 0}),
        json!({"item": item.0, "indices": [], "speaker": 0}),
        json!({"item": item.0, "indices": [0, 9999], "speaker": 0}),
        json!({"item": item.0, "indices": [-1], "speaker": 0}),
        json!({"item": item.0, "indices": ["0"], "speaker": 0}),
        json!({"item": item.0, "indices": [u64::MAX], "speaker": 0}),
        json!({"item": item.0, "indices": [0], "speaker": u64::MAX}),
        json!({"item": item.0, "indices": [0], "name": "x", "speaker": 0}),
        json!({"item": item.0, "indices": [0], "name": "\n"}),
        json!({"item": item.0, "indices": [0], "name": "x".repeat(257)}),
        json!({"item": item.0, "indices": [0], "expected": [], "speaker": 0}),
    ] {
        assert!(s.execute("transcript.assignSpeaker", params.clone()).is_err(), "{params}");
        assert_eq!(*s.project, *before);
    }
    for params in [
        json!({"item": 999999, "speaker": "Speaker 1", "name": "Bad"}),
        json!({"item": "invalid", "speaker": "Speaker 1", "name": "Bad"}),
        json!({"item": item.0, "speaker": 0, "name": "A\nB"}),
        json!({"item": item.0, "speaker": 0, "name": "Alice", "expected": "Different"}),
    ] {
        assert!(s.execute("transcript.renameSpeaker", params).is_err());
        assert_eq!(*s.project, *before, "invalid item must never rename other transcripts");
    }
}

#[test]
fn speaker_analysis_reuses_words_without_loading_a_speech_model() {
    let (mut s, item, _) = session();
    s.execute("transcript.generate", json!({"item":item.0})).unwrap();
    s.transcriber = None;
    s.prefs.media_analysis.whisper_model = "missing-model-is-not-used".into();
    let before = s.project.clone();
    let id = s.execute("transcript.identifySpeakers", json!({"item":item.0,"maxSpeakers":1,"wait":false})).unwrap()["job"].as_u64().unwrap();
    finish_job(&mut s, id);
    assert!(s.jobs.iter().find(|job| job.id == id).unwrap().progress.error.lock().unwrap().is_none());
    let tr = &s.project.transcripts[&item];
    assert_eq!(tr.source, before.transcripts[&item].source);
    for (old, new) in before.transcripts[&item].words.iter().zip(&tr.words) {
        let mut want = old.clone();
        want.speaker = Some(0);
        assert_eq!(*new, want);
    }
    s.undo();
    assert_eq!(s.project, before);
    let id = s.execute("transcript.identifySpeakers", json!({"item":item.0,"wait":false})).unwrap()["job"].as_u64().unwrap();
    s.execute("transcript.correctWord", json!({"item":item.0,"index":0,"text":"Edited"})).unwrap();
    finish_job(&mut s, id);
    assert_eq!(s.project.transcripts[&item].words[0].text, "Edited");
    assert!(s.jobs.iter().find(|job| job.id == id).unwrap().progress.error.lock().unwrap().as_ref().is_some_and(|error| error.contains("changed")));
    let before = s.project.clone();
    let id = s.execute("transcript.identifySpeakers", json!({"item":item.0,"wait":false})).unwrap()["job"].as_u64().unwrap();
    s.execute("jobs.cancel", json!({"job":id})).unwrap();
    finish_job(&mut s, id);
    assert_eq!(s.project, before, "cancellation must retain all original words and labels");
}

#[test]
fn speaker_analysis_rejects_hostile_parameters_before_creating_jobs() {
    let (mut s, item, _) = session();
    let before = s.project.clone();
    let undo = s.history.undo.len();
    for params in [
        json!([]),
        json!({"items":false}),
        json!({"items":[]}),
        json!({"items":[-1]}),
        json!({"item":"bad"}),
        json!({"item":item.0,"maxSpeakers":0}),
        json!({"item":item.0,"maxSpeakers":33}),
        json!({"item":item.0,"maxSpeakers":u64::MAX}),
        json!({"item":item.0,"wait":"false"}),
        json!({"item":item.0}),
    ] {
        assert!(s.execute("transcript.identifySpeakers", params.clone()).is_err(), "{params}");
        assert!(s.jobs.is_empty());
        assert_eq!(s.project, before);
        assert_eq!(s.history.undo.len(), undo);
    }
}

#[test]
fn word_correction_checks_complete_word_identity_and_keeps_timing() {
    let (mut s, item, _) = session();
    s.execute("transcript.generate", json!({"item":item.0})).unwrap();
    let old = s.project.transcripts[&item].words[0].clone();
    s.execute("transcript.correctWord", json!({"item":item.0,"index":0,"text":"Corrected","expectedWord":old})).unwrap();
    let mut want = old.clone();
    want.text = "Corrected".into();
    assert_eq!(s.project.transcripts[&item].words[0], want);
    s.undo();
    let mut tr = s.project.transcripts[&item].as_ref().clone();
    tr.words[0].start += Tick(1);
    s.execute("transcript.set", json!({"item":item.0,"transcript":tr})).unwrap();
    let before = s.project.clone();
    let undo = s.history.undo.len();
    assert!(s.execute("transcript.correctWord", json!({"item":item.0,"index":0,"text":"Other","expectedWord":old})).is_err());
    assert_eq!(s.project, before);
    assert_eq!(s.history.undo.len(), undo);
    for index in [json!(-1), json!(0.5), json!("0"), json!(u64::MAX)] {
        assert!(s.execute("transcript.correctWord", json!({"item":item.0,"index":index,"text":"Bad"})).is_err());
        assert_eq!(s.project, before);
    }
    for text in ["", "two words", "bad\0word"] {
        assert!(s.execute("transcript.correctWord", json!({"item":item.0,"index":0,"text":text})).is_err());
    }
}

#[test]
fn batch_speaker_renaming_is_atomic_source_local_and_persistent() {
    let (mut s, a, _) = session();
    s.execute("transcript.generate", json!({"item":a.0})).unwrap();
    let b = s.active_sequence().unwrap().audio_tracks[0].items[1].item;
    let original = s.project.transcripts[&a].clone();
    s.execute("transcript.set", json!({"item":b.0,"transcript":original})).unwrap();
    let before = s.project.clone();
    let undo = s.history.undo.len();
    let params = json!({"sources":[
        {"item":a.0,"expected":original,"names":[{"speaker":0,"name":"Alice"},{"speaker":1,"name":"Interviewer"}]},
        {"item":b.0,"expected":original,"names":[{"speaker":0,"name":"Bob"}]}]});
    assert_eq!(s.execute("transcript.renameSpeakers", params.clone()).unwrap()["renamed"], 3);
    assert_eq!(s.history.undo.len(), undo + 1);
    assert_eq!(s.project.transcripts[&a].speakers[0].name, "Alice");
    assert_eq!(s.project.transcripts[&b].speakers[0].name, "Bob");
    for id in [a, b] {
        assert_eq!(s.project.transcripts[&id].words, before.transcripts[&id].words);
    }
    let after = s.project.clone();
    assert!(s.execute("transcript.renameSpeakers", params).is_err());
    assert_eq!(s.project, after);
    s.undo();
    assert_eq!(s.project, before);
    s.redo();
    assert_eq!(s.project, after);
    let path = std::env::temp_dir().join(format!("filmcraft-batch-speakers-{}.fcproj", std::process::id()));
    s.execute("file.saveAs", json!({"path":path})).unwrap();
    s.execute("file.open", json!({"path":path})).unwrap();
    assert_eq!(s.project.transcripts, after.transcripts);
    std::fs::remove_file(path).unwrap();
}

#[test]
fn batch_speaker_names_validate_all_sources_before_any_edit() {
    let (mut s, a, _) = session();
    s.execute("transcript.generate", json!({"item":a.0})).unwrap();
    let before = s.project.clone();
    let undo = s.history.undo.len();
    let valid = json!({"item":a.0,"names":[{"speaker":0,"name":"Alice"}]});
    for params in [
        json!({}),
        json!({"sources":[]}),
        json!({"sources":false}),
        json!({"sources":[valid.clone(),valid.clone()]}),
        json!({"sources":[valid.clone(),{"item":999999,"names":[{"speaker":0,"name":"Bob"}]}]}),
        json!({"sources":[{"item":a.0,"expected":{},"names":[{"speaker":0,"name":"Bob"}]}]}),
        json!({"sources":[{"item":a.0,"names":[{"speaker":0,"name":"Alice"},{"speaker":0,"name":"Bob"}]}]}),
        json!({"sources":[{"item":a.0,"names":[{"speaker":0,"name":"Alice"},{"speaker":u64::MAX,"name":"Bob"}]}]}),
        json!({"sources":[{"item":a.0,"names":[{"speaker":-1,"name":"Alice"}]}]}),
        json!({"sources":[{"item":a.0,"names":[{"speaker":0,"name":"bad\nname"}]}]}),
        json!({"sources":[{"item":a.0,"names":[{"speaker":0,"name":"x".repeat(257)}]}]}),
        json!({"sources":vec![valid;257]}),
    ] {
        assert!(s.execute("transcript.renameSpeakers", params.clone()).is_err(), "{params}");
        assert_eq!(s.project, before);
        assert_eq!(s.history.undo.len(), undo);
    }
    let name = before.transcripts[&a].speakers[0].name.clone();
    assert_eq!(s.execute("transcript.renameSpeakers", json!({"sources":[{"item":a.0,"names":[{"speaker":0,"name":name}]}]})).unwrap()["renamed"], 0);
    assert_eq!(s.history.undo.len(), undo);
}
