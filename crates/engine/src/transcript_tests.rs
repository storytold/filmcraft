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

struct GateTranscriber {
    started: Arc<std::sync::atomic::AtomicBool>,
    release: Arc<std::sync::atomic::AtomicBool>,
    transcript: Transcript,
    panic: bool,
}
impl filmcraft_speech::Transcriber for GateTranscriber {
    fn id(&self) -> String {
        "gate".into()
    }
    fn transcribe(
        &self,
        _: &[f32],
        _: &filmcraft_speech::Options,
        progress: filmcraft_speech::ProgressFn,
    ) -> Result<Transcript, filmcraft_speech::SpeechError> {
        use std::sync::atomic::Ordering;
        self.started.store(true, Ordering::Release);
        let until = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while !self.release.load(Ordering::Acquire) && std::time::Instant::now() < until {
            if !progress(0.5, "Waiting in test recognizer") {
                return Err(filmcraft_speech::SpeechError::Cancelled);
            }
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        assert!(!self.panic, "test worker panic");
        Ok(self.transcript.clone())
    }
}

fn gated() -> (Session, ItemId, Arc<std::sync::atomic::AtomicBool>, Arc<std::sync::atomic::AtomicBool>) {
    let (mut s, item, _) = session();
    s.execute("transcript.generate", json!({"item": item.0})).unwrap();
    let transcript = (*s.project.transcripts[&item]).clone();
    let started = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let release = Arc::new(std::sync::atomic::AtomicBool::new(false));
    s.transcriber = Some(Arc::new(GateTranscriber { started: started.clone(), release: release.clone(), transcript, panic: false }));
    (s, item, started, release)
}

fn finish(s: &mut Session, job: u64) -> serde_json::Value {
    let until = std::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        s.poll_persistence();
        let status = s.jobs.iter().find(|j| j.id == job).unwrap().to_json();
        if status["finished"] == true && s.transcript_jobs.is_empty() {
            return status;
        }
        assert!(std::time::Instant::now() < until, "transcription timed out: {status}");
        std::thread::sleep(std::time::Duration::from_millis(2));
    }
}

#[test]
fn transcript_background_progress_cancel_and_stale_results() {
    use std::sync::atomic::Ordering;
    let (mut s, item, started, release) = gated();
    let before = s.project.clone();
    let r = s.execute("transcript.generate", json!({"item": item.0, "wait": false})).unwrap();
    let job = r["job"].as_u64().unwrap();
    let until = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while !started.load(Ordering::Acquire) {
        assert!(std::time::Instant::now() < until);
        std::thread::sleep(std::time::Duration::from_millis(2));
    }
    assert!(s.execute("transcript.generate", json!({"item": item.0, "wait": false})).is_err());
    assert!(s.execute("transcript.inspect", json!({})).is_ok(), "session remains usable during inference");
    s.execute("jobs.cancel", json!({"job": job})).unwrap();
    release.store(true, Ordering::Release);
    assert!(finish(&mut s, job)["result"]["error"].is_string());
    assert_eq!(*s.project, *before);

    let (mut s, item, _, release) = gated();
    let r = s.execute("transcript.generate", json!({"item": item.0, "wait": false})).unwrap();
    s.execute("transcript.correctWord", json!({"item": item.0, "index": 0, "text": "Corrected"})).unwrap();
    release.store(true, Ordering::Release);
    let status = finish(&mut s, r["job"].as_u64().unwrap());
    assert!(status["result"]["error"].as_str().unwrap().contains("changed"));
    assert_eq!(s.project.transcripts[&item].words[0].text, "Corrected");
}

#[test]
fn transcript_background_success_applies_once_and_worker_panic_is_reported() {
    use std::sync::atomic::Ordering;
    let (mut s, item, _, release) = gated();
    let before = s.project.clone();
    let r = s.execute("transcript.generate", json!({"item": item.0, "wait": false})).unwrap();
    release.store(true, Ordering::Release);
    assert!(finish(&mut s, r["job"].as_u64().unwrap())["result"]["error"].is_null());
    s.execute("edit.undo", json!({})).unwrap();
    assert_eq!(*s.project, *before);

    let transcript = (*s.project.transcripts[&item]).clone();
    s.transcriber =
        Some(Arc::new(GateTranscriber { started: Default::default(), release: Arc::new(std::sync::atomic::AtomicBool::new(true)), transcript, panic: true }));
    let r = s.execute("transcript.generate", json!({"item": item.0, "wait": false})).unwrap();
    assert!(finish(&mut s, r["job"].as_u64().unwrap())["result"]["error"].as_str().unwrap().contains("internal error"));
    assert_eq!(*s.project, *before);
}

#[test]
fn transcript_background_cannot_apply_to_a_reopened_project() {
    use std::sync::atomic::Ordering;
    let (mut s, item, _, release) = gated();
    let r = s.execute("transcript.generate", json!({"item": item.0, "wait": false})).unwrap();
    s.execute("file.openDemoProject", json!({})).unwrap();
    let reopened = s.project.clone();
    release.store(true, Ordering::Release);
    assert!(finish(&mut s, r["job"].as_u64().unwrap())["result"]["error"].is_string());
    assert_eq!(*s.project, *reopened);
}
