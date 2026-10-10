use std::sync::Arc;

use serde_json::{Value, json};

use crate::Session;
use filmcraft_project::{ItemId, Transcript, Word};
use filmcraft_speech::FixedTranscriber;
use filmcraft_time::Tick;

/// The demo project with a fake transcriber whose transcript fits the first A1 clip's media:
/// "Hello um world." (Speaker 1), a 1.2 s pause, "Second speaker here." (Speaker 2).
fn session() -> (Session, ItemId, Tick) {
    let mut s = Session::default();
    s.execute("file.openDemoProject", json!({})).unwrap();
    let item = s.active_sequence().unwrap().audio_tracks[0].items[0].item;
    let t = demo_transcript(&s);
    s.transcriber = Some(Arc::new(FixedTranscriber { transcript: t, id: "fixed".into() }));
    let start = a_start(&s);
    (s, item, start)
}

/// "Hello um world." (Speaker 1), a 1.2 s pause, "Second speaker here." (Speaker 2), in the media
/// time of the first A1 clip.
fn demo_transcript(s: &Session) -> Transcript {
    let sin = s.active_sequence().unwrap().audio_tracks[0].items[0].source_in;
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
    t
}

fn a_start(s: &Session) -> Tick {
    s.active_sequence().unwrap().audio_tracks[0].items[0].start
}

fn words(s: &mut Session) -> Vec<String> {
    let r = s.execute("transcript.inspect", json!({})).unwrap();
    r["words"].as_array().unwrap().iter().map(|w| w["text"].as_str().unwrap().to_string()).collect()
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

/// A stand-in for a slow model: reports progress in `steps` steps and waits for the test to
/// release each one, so progress and Cancel are checked without timing.
struct SteppedTranscriber {
    inner: FixedTranscriber,
    steps: u32,
    /// (steps released so far, wakes the transcriber)
    gate: Arc<(std::sync::Mutex<u32>, std::sync::Condvar)>,
}

impl filmcraft_speech::Transcriber for SteppedTranscriber {
    fn id(&self) -> String {
        "stepped".into()
    }
    fn transcribe(
        &self,
        audio: &[f32],
        opts: &filmcraft_speech::Options,
        progress: filmcraft_speech::ProgressFn,
    ) -> Result<Transcript, filmcraft_speech::SpeechError> {
        let (lock, cv) = &*self.gate;
        for k in 1..=self.steps {
            let mut released = lock.lock().unwrap();
            while *released < k {
                let (g, timeout) = cv.wait_timeout(released, std::time::Duration::from_secs(20)).unwrap();
                released = g;
                if timeout.timed_out() {
                    return Err(filmcraft_speech::SpeechError::Model("the test never released the step".into()));
                }
            }
            drop(released);
            if !progress(k as f32 / self.steps as f32, "step") {
                return Err(filmcraft_speech::SpeechError::Cancelled);
            }
        }
        self.inner.transcribe(audio, opts, &mut |_, _| true)
    }
}

fn release(gate: &(std::sync::Mutex<u32>, std::sync::Condvar), steps: u32) {
    *gate.0.lock().unwrap() = steps;
    gate.1.notify_all();
}

/// Poll like the UI does each frame until `done` (at most 20 s).
fn poll_until(s: &mut Session, mut done: impl FnMut(&mut Session) -> bool) {
    let t0 = std::time::Instant::now();
    while !done(s) {
        assert!(t0.elapsed().as_secs() < 20, "timed out");
        s.poll_persistence();
        std::thread::sleep(std::time::Duration::from_millis(2));
    }
}

fn stepped(s: &mut Session, steps: u32) -> Arc<(std::sync::Mutex<u32>, std::sync::Condvar)> {
    let gate: Arc<(std::sync::Mutex<u32>, std::sync::Condvar)> = Arc::default();
    let inner = FixedTranscriber { transcript: demo_transcript(s), id: "stepped".into() };
    s.transcriber = Some(Arc::new(SteppedTranscriber { inner, steps, gate: gate.clone() }));
    gate
}

fn progress(s: &mut Session) -> f64 {
    s.execute("transcript.status", json!({})).unwrap()["progress"].as_f64().unwrap_or(0.0)
}

#[test]
fn transcription_runs_in_the_background_with_progress_and_applies_as_one_undo_step() {
    let (mut s, item, _) = session();
    let gate = stepped(&mut s, 4);
    let undo_before = s.history.undo.len();
    let r = s.execute("transcript.generate", json!({"items": [item.0], "wait": false})).unwrap();
    assert_eq!(r["running"], true, "{r}");
    let job = r["job"].as_u64().unwrap();
    // the command returned at once; nothing is applied yet and a second run is refused
    assert!(s.project.transcripts.is_empty());
    assert!(s.execute("transcript.generate", json!({"items": [item.0], "wait": false})).unwrap_err().to_string().contains("already running"));
    // decoding is the first tenth; then each released step moves the bar
    poll_until(&mut s, |s| progress(s) >= 0.0999);
    let st = s.execute("transcript.status", json!({})).unwrap();
    assert_eq!((st["running"].clone(), st["job"].as_u64(), st["items"].clone()), (json!(true), Some(job), json!([item.0])), "{st}");
    assert!(st["status"].as_str().unwrap().starts_with("Transcribing"), "{st}");
    release(&gate, 2);
    poll_until(&mut s, |s| progress(s) >= 0.549);
    assert!(s.project.transcripts.is_empty(), "still running");
    release(&gate, 4);
    poll_until(&mut s, |s| !s.project.transcripts.is_empty());
    assert_eq!(s.project.transcripts[&item].words.len(), 6);
    assert_eq!(s.history.undo.len(), undo_before + 1, "one undo step");
    assert_eq!(s.history.undo.last().unwrap().0, "Transcribe");
    assert_eq!(s.execute("transcript.status", json!({})).unwrap(), json!({"running": false}));
    let jobs = s.execute("jobs.list", json!({})).unwrap();
    let j = jobs.as_array().unwrap().iter().find(|j| j["id"] == job).unwrap();
    assert_eq!((j["label"].as_str(), j["finished"].as_bool()), (Some(crate::transcript::JOB_LABEL), Some(true)), "{j}");
    assert!(s.transcribe_jobs.is_empty());
    s.execute("edit.undo", json!({})).unwrap();
    assert!(s.project.transcripts.is_empty());
}

#[test]
fn cancelling_a_transcription_changes_nothing() {
    let (mut s, item, _) = session();
    let gate = stepped(&mut s, 3);
    let undo_before = s.history.undo.len();
    assert!(!s.is_enabled("transcript.cancel"), "nothing to cancel");
    s.execute("transcript.generate", json!({"items": [item.0], "wait": false})).unwrap();
    assert!(s.is_enabled("transcript.cancel"));
    release(&gate, 1);
    poll_until(&mut s, |s| progress(s) >= 0.39);
    s.execute("transcript.cancel", json!({})).unwrap();
    // the recogniser is told at its next progress report
    release(&gate, 3);
    poll_until(&mut s, |s| s.transcribe_jobs.is_empty());
    assert!(s.project.transcripts.is_empty());
    assert_eq!(s.history.undo.len(), undo_before);
    let jobs = s.execute("jobs.list", json!({})).unwrap();
    let job = jobs.as_array().unwrap().iter().find(|j| j["label"] == crate::transcript::JOB_LABEL).unwrap();
    assert_eq!(job["result"]["error"], "stopped", "{jobs}");
    assert!(s.drain_events().iter().any(|e| matches!(e, crate::Event::Toast { message, .. } if message.contains("stopped"))));
    // and it can run again
    let gate = stepped(&mut s, 1);
    release(&gate, 1);
    s.execute("transcript.generate", json!({"items": [item.0], "wait": false})).unwrap();
    poll_until(&mut s, |s| !s.project.transcripts.is_empty());
}

#[test]
fn a_finished_job_skips_items_that_changed_meanwhile() {
    let (mut s, item, _) = session();
    let gate = stepped(&mut s, 1);
    s.execute("transcript.generate", json!({"items": [item.0], "wait": false})).unwrap();
    // another project with an item of the same id is opened while the job runs
    if let Some(it) = std::sync::Arc::make_mut(&mut s.project).item_mut(item) {
        it.name = "someone else's clip".into();
    }
    release(&gate, 1);
    poll_until(&mut s, |s| s.transcribe_jobs.is_empty());
    assert!(s.project.transcripts.is_empty(), "not applied to a different item");
}

#[test]
fn waiting_for_a_cancelled_or_failing_job_reports_it() {
    let (mut s, item, _) = session();
    // a recogniser that fails: the error comes back, nothing changes
    struct Failing;
    impl filmcraft_speech::Transcriber for Failing {
        fn id(&self) -> String {
            "failing".into()
        }
        fn transcribe(&self, _: &[f32], _: &filmcraft_speech::Options, _: filmcraft_speech::ProgressFn) -> Result<Transcript, filmcraft_speech::SpeechError> {
            Err(filmcraft_speech::SpeechError::Model("weights are corrupt".into()))
        }
    }
    s.transcriber = Some(Arc::new(Failing));
    let e = s.execute("transcript.generate", json!({"items": [item.0]})).unwrap_err().to_string();
    assert!(e.contains("weights are corrupt"), "{e}");
    assert!(s.project.transcripts.is_empty() && s.transcribe_jobs.is_empty());
    // a recogniser that panics: the job guard turns it into an error, the session lives on
    struct Panicking;
    impl filmcraft_speech::Transcriber for Panicking {
        fn id(&self) -> String {
            "panicking".into()
        }
        fn transcribe(&self, _: &[f32], _: &filmcraft_speech::Options, _: filmcraft_speech::ProgressFn) -> Result<Transcript, filmcraft_speech::SpeechError> {
            panic!("bug in a recogniser")
        }
    }
    s.transcriber = Some(Arc::new(Panicking));
    let e = s.execute("transcript.generate", json!({"items": [item.0]})).unwrap_err().to_string();
    assert!(e.contains("internal error"), "{e}");
    s.execute("transcript.generate", json!({"items": [item.0], "wait": false})).unwrap();
    poll_until(&mut s, |s| s.transcribe_jobs.is_empty());
    assert!(s.project.transcripts.is_empty());
    assert!(s.execute("transcript.cancel", json!({})).is_err());
}

#[test]
fn pauses_fillers_search_filters_and_delete_all() {
    let (mut s, item, start) = session();
    s.execute("transcript.generate", json!({"items": [item.0]})).unwrap();
    // pauses: 0.1 s between the words of "Hello um world." and 1.2 s before "Second"
    let r = s.execute("transcript.inspect", json!({})).unwrap();
    assert_eq!(r["minPauseSeconds"], 0.75, "Premiere's default");
    assert_eq!(r["pauses"].as_array().unwrap().len(), 1, "{}", r["pauses"]);
    assert_eq!(r["pauses"][0]["after"], 2);
    assert!((r["pauses"][0]["seconds"].as_f64().unwrap() - 1.2).abs() < 1e-6);
    let r = s.execute("transcript.inspect", json!({"minPauseSeconds": 0.1})).unwrap();
    assert_eq!(r["pauses"].as_array().unwrap().len(), 3, "{}", r["pauses"]);
    // fillers are flagged per word
    let flags: Vec<bool> = r["words"].as_array().unwrap().iter().map(|w| w["filler"].as_bool().unwrap()).collect();
    assert_eq!(flags, [false, true, false, false, false, false]);
    // the playhead in the pause
    s.set_playhead(start + Tick::from_seconds_f64(2.0));
    let r = s.execute("transcript.inspect", json!({})).unwrap();
    assert_eq!((r["current"].clone(), r["currentPause"].clone()), (Value::Null, json!(2)));

    // search filters
    let r = s.execute("transcript.search", json!({"filter": "fillers"})).unwrap();
    assert_eq!((r["count"].as_u64(), r["matches"][0]["from"].as_u64()), (Some(1), Some(1)), "{r}");
    let r = s.execute("transcript.search", json!({"filter": "pauses"})).unwrap();
    assert_eq!((r["count"].as_u64(), r["matches"][0]["pauseAfter"].as_u64()), (Some(1), Some(2)), "{r}");
    let r = s.execute("transcript.search", json!({"query": "WORLD"})).unwrap();
    assert_eq!((r["filter"].as_str(), r["count"].as_u64()), (Some("text"), Some(1)));
    // search settings: per call or from Transcript view options
    assert_eq!(s.execute("transcript.search", json!({"query": "WORLD", "matchCase": true})).unwrap()["count"], 0);
    assert_eq!(s.execute("transcript.search", json!({"query": "wor"})).unwrap()["count"], 1);
    s.execute("prefs.set", json!({"values": {"transcript.wholeWords": true, "transcript.minPauseLength": 99}})).unwrap();
    assert_eq!(s.execute("transcript.search", json!({"query": "wor"})).unwrap()["count"], 0);
    assert_eq!(s.prefs.transcript.min_pause_length, 3.0, "clamped");
    s.prefs.transcript = Default::default();
    assert!(s.execute("transcript.search", json!({"filter": "text"})).is_err(), "text needs a query");
    assert!(s.execute("transcript.search", json!({"filter": "speakers"})).is_err());

    // one pause: select marks it, extract removes the whole silence
    let rate = s.sequence_rate();
    let r = s.execute("transcript.select", json!({"pauseAfter": 2})).unwrap();
    let (a, b) = (Tick(r["start"].as_i64().unwrap()), Tick(r["end"].as_i64().unwrap()));
    assert_eq!((rate.snap(a), rate.snap(b)), (a, b), "inward to frames");
    assert_eq!(s.active_sequence().unwrap().mark_in, Some(a));
    assert!(s.execute("transcript.select", json!({"pauseAfter": 5})).is_err(), "nothing after the last word");
    assert!(s.execute("transcript.select", json!({"pauseAfter": u64::MAX})).is_err());
    let before = s.active_sequence().unwrap().duration();
    s.execute("transcript.extract", json!({"pauseAfter": 2})).unwrap();
    assert_eq!(s.active_sequence().unwrap().duration(), before - (b - a));
    let r = s.execute("transcript.search", json!({"filter": "pauses"})).unwrap();
    assert_eq!(r["count"], 0, "{r}");
    s.execute("edit.undo", json!({})).unwrap();

    // delete all: fillers, then pauses, each one undo step
    let undo = s.history.undo.len();
    let r = s.execute("transcript.deleteAll", json!({"filter": "fillers"})).unwrap();
    assert_eq!(r["removed"], 1, "{r}");
    assert_eq!(s.history.undo.last().unwrap().0, "Delete All Filler Words");
    let r = s.execute("transcript.deleteAll", json!({"filter": "pauses", "minPauseSeconds": 0.05})).unwrap();
    assert!(r["removed"].as_u64().unwrap() >= 2, "{r}");
    assert_eq!(s.history.undo.len(), undo + 2);
    assert_eq!(words(&mut s), ["Hello", "world.", "Second", "speaker", "here."]);
    assert_eq!(s.execute("transcript.search", json!({"filter": "pauses", "minPauseSeconds": 0.1})).unwrap()["count"], 0);
    s.active_sequence().unwrap().check().unwrap();
    // lift leaves the sequence as long as it was
    s.execute("edit.undo", json!({})).unwrap();
    s.execute("edit.undo", json!({})).unwrap();
    let before = s.active_sequence().unwrap().duration();
    let r = s.execute("transcript.deleteAll", json!({"query": "speaker", "lift": true})).unwrap();
    assert_eq!((r["removed"].as_u64(), r["lifted"].as_bool()), (Some(1), Some(true)), "{r}");
    assert_eq!(s.active_sequence().unwrap().duration(), before);
    assert!(!words(&mut s).contains(&"speaker".to_string()));
    // hostile parameters are errors, never panics
    for p in [
        json!({"filter": 3}),
        json!({"filter": "pauses", "minPauseSeconds": f64::MAX}),
        json!({"filter": "pauses", "minPauseSeconds": -1}),
        json!({"query": ""}),
    ] {
        let _ = s.execute("transcript.deleteAll", p.clone());
        let _ = s.execute("transcript.search", p);
    }
}

#[test]
fn german_transcripts_use_german_fillers() {
    let (mut s, item, _) = session();
    let a = s.active_sequence().unwrap().audio_tracks[0].items[0].source_in;
    let sec = |x: f64| (a + Tick::from_seconds_f64(x)).0;
    let words: Vec<Value> = ["Er", "hat", "äh", "ÄHM,", "gesagt"]
        .iter()
        .enumerate()
        .map(|(i, w)| json!({"text": w, "start": sec(0.2 + i as f64 * 0.5), "end": sec(0.6 + i as f64 * 0.5)}))
        .collect();
    s.execute("transcript.set", json!({"item": item.0, "transcript": {"language": "de", "words": words}})).unwrap();
    let r = s.execute("transcript.search", json!({"filter": "fillers"})).unwrap();
    let found: Vec<u64> = r["matches"].as_array().unwrap().iter().map(|m| m["from"].as_u64().unwrap()).collect();
    assert_eq!(found, [2, 3], "{r}");
    s.execute("transcript.removeFillers", json!({})).unwrap();
    assert_eq!(crate::transcript::sequence_words(&s).iter().map(|w| w.text.as_str()).collect::<Vec<_>>(), ["Er", "hat", "gesagt"]);
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
