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
    // these words are made up over the demo's own audio, whose waveform has sound in their
    // "pause": read the transcript as an imported one, whose pauses are the gaps between words
    let pr = Arc::make_mut(&mut s.project);
    if let Some(t) = pr.transcripts.get_mut(&item) {
        assert!(!t.voice.is_empty(), "transcribing measures the voice");
        Arc::make_mut(t).voice.clear();
    }
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

/// The demo's first A1 clip with a transcript that brings its own voice map (as a transcription
/// measures it): "so" (0.5–0.8), "um" (0.9–1.1), "but" (1.2–1.5), a 0.6 s pause, "then"
/// (2.1–2.4), a 0.15 s pause, "now" (2.55–2.8). Times from the clip's In point.
fn voiced_session() -> (Session, ItemId, Tick) {
    let (mut s, item, start) = session();
    let sin = s.active_sequence().unwrap().audio_tracks[0].items[0].source_in;
    let t = |x: f64| (sin + Tick::from_seconds_f64(x)).0;
    let spans = [("so", 0.5, 0.8), ("um", 0.9, 1.1), ("but", 1.2, 1.5), ("then", 2.1, 2.4), ("now", 2.55, 2.8)];
    let words: Vec<_> = spans.iter().map(|(w, a, b)| json!({"text": w, "start": t(*a), "end": t(*b)})).collect();
    let voice: Vec<_> = spans.iter().map(|(_, a, b)| json!([t(*a), t(*b)])).collect();
    s.execute("transcript.set", json!({"item": item.0, "transcript": {"language": "en", "words": words, "voice": voice}})).unwrap();
    (s, item, start)
}

#[test]
fn pauses_and_fillers_are_found_and_deleted_one_or_all() {
    let (mut s, _, start) = voiced_session();
    let sec = |x: f64| start + Tick::from_seconds_f64(x);
    // pause length 0.15 s (the default): 0.9→… no: gaps are 0.8–0.9 (0.1), 1.1–1.2 (0.1),
    // 1.5–2.1 (0.6), 2.4–2.55 (0.15); plus the clip's head before "so" and its tail after "now"
    let r = s.execute("transcript.find", json!({"filter": "pauses"})).unwrap();
    let hits = r["hits"].as_array().unwrap();
    let inner: Vec<(i64, i64)> = hits.iter().map(|h| (h["start"].as_i64().unwrap(), h["end"].as_i64().unwrap())).filter(|(a, _)| *a > sec(0.0).0).collect();
    assert!(inner.contains(&(sec(1.5).0, sec(2.1).0)), "{r}");
    assert!(inner.contains(&(sec(2.4).0, sec(2.55).0)), "{r}");
    assert!(!inner.iter().any(|(a, _)| *a == sec(0.8).0), "0.1 s gaps are shorter than the pause length: {r}");
    // a longer pause length finds fewer
    let r2 = s.execute("transcript.find", json!({"filter": "pauses", "minSeconds": 0.5})).unwrap();
    assert!(r2["hits"].as_array().unwrap().len() < hits.len());
    // fillers and text
    let f = s.execute("transcript.find", json!({"filter": "fillers"})).unwrap();
    assert_eq!(f["hits"].as_array().unwrap().len(), 1, "{f}");
    assert_eq!(f["hits"][0]["from"], 1);
    let t = s.execute("transcript.find", json!({"filter": "text", "query": "the"})).unwrap();
    assert_eq!(t["hits"][0]["from"], 3);
    // the view lays pauses between the words
    let v = crate::transcript::view(&s, None);
    let k = v.tokens.iter().position(|t| matches!(t, filmcraft_edit::transcript::Token::Word(2))).unwrap();
    assert!(matches!(v.tokens[k + 1], filmcraft_edit::transcript::Token::Pause(_)), "{:?}", v.tokens);

    // Delete one pause (extract): the sequence gets shorter by its cut, the words all stay
    let before = s.active_sequence().unwrap().duration();
    let i = hits.iter().position(|h| h["start"] == sec(1.5).0).unwrap();
    let cut = &hits[i]["cut"];
    let r = s.execute("transcript.deleteHits", json!({"filter": "pauses", "hit": i})).unwrap();
    assert_eq!(r["removed"], 1, "{r}");
    let gone = cut["end"].as_i64().unwrap() - cut["start"].as_i64().unwrap();
    // 30 ms after "but" and 35 ms before "then" are kept, rounded inward to frames
    assert!(gone > Tick::from_seconds_f64(0.45).0 && gone <= Tick::from_seconds_f64(0.535).0, "{cut}");
    assert_eq!(s.active_sequence().unwrap().duration(), before - Tick(gone));
    assert_eq!(words(&mut s), ["so", "um", "but", "then", "now"]);
    s.execute("edit.undo", json!({})).unwrap();
    assert_eq!(s.active_sequence().unwrap().duration(), before, "one undo step");

    // Delete all pauses with Lift: same length, gaps left
    let r = s.execute("transcript.deleteHits", json!({"filter": "pauses", "mode": "lift"})).unwrap();
    assert!(r["removed"].as_u64().unwrap() >= 2, "{r}");
    assert_eq!(r["mode"], "lift");
    s.active_sequence().unwrap().check().unwrap();
    s.execute("edit.undo", json!({})).unwrap();
    // Delete all filler words
    s.execute("transcript.deleteHits", json!({"filter": "fillers"})).unwrap();
    assert_eq!(words(&mut s), ["so", "but", "then", "now"]);
    // hostile params
    assert!(s.execute("transcript.deleteHits", json!({"filter": "pauses", "hit": 999})).is_err());
    assert!(s.execute("transcript.deleteHits", json!({"filter": "nope"})).is_err());
    assert!(s.execute("transcript.deleteHits", json!({"filter": "pauses", "mode": "smash"})).is_err());
    assert!(s.execute("transcript.find", json!({"filter": "pauses", "minSeconds": -5.0})).is_ok());
}

#[test]
fn transcribing_in_the_background_reports_progress_and_can_be_cancelled() {
    let (mut s, item, _) = session();
    let r = s.execute("transcript.generate", json!({"items": [item.0], "wait": false})).unwrap();
    let job = r["job"].as_u64().unwrap();
    assert!(r.get("items").is_some());
    // a second one waits for the first
    let mut stored = false;
    for _ in 0..500 {
        s.poll_persistence();
        if s.project.transcripts.contains_key(&item) {
            stored = true;
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    assert!(stored, "the job finished and its transcript was stored");
    assert!(crate::transcript::running(&s).is_none());
    assert!(s.jobs.iter().any(|j| j.id == job));
    let tr = &s.project.transcripts[&item];
    assert_eq!(tr.words.len(), 6);
    assert!(!tr.voice.is_empty(), "the voice was measured from the clip's audio");
    s.execute("edit.undo", json!({})).unwrap();
    assert!(!s.project.transcripts.contains_key(&item), "one undo step");

    // nothing to cancel; a cancelled job changes nothing
    assert!(s.execute("transcript.cancel", json!({})).is_err());
    s.execute("transcript.generate", json!({"items": [item.0], "wait": false})).unwrap();
    let _ = s.execute("transcript.cancel", json!({}));
    for _ in 0..500 {
        s.poll_persistence();
        if s.transcribe_jobs.is_empty() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    assert!(s.transcribe_jobs.is_empty());
}

#[test]
fn find_pauses_measures_imported_transcripts() {
    let (mut s, item, _) = session();
    let t = json!({"language": "en", "words": [{"text": "hi", "start": 0, "end": 1000}]});
    s.execute("transcript.set", json!({"item": item.0, "transcript": t})).unwrap();
    assert!(s.project.transcripts[&item].voice.is_empty());
    assert_eq!(crate::transcript::view(&s, None).without_voice, 1);
    let r = s.execute("transcript.findPauses", json!({})).unwrap();
    assert!(r["items"].as_array().is_some_and(|a| !a.is_empty()), "{r}");
    assert!(!s.project.transcripts[&item].voice.is_empty());
    assert_eq!(s.project.transcripts[&item].words.len(), 1, "an imported transcript keeps its words as they are");
    assert_eq!(crate::transcript::view(&s, None).without_voice, 0);
}
