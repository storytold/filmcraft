//! Headless UI test of the Text panel's Transcript tab: the Transcribe button, the word view,
//! clicking words (In/Out from the selection) and extracting the selected text.
//!
//! Set `FILMCRAFT_UI_SNAPSHOT_DIR=<dir>` to also render the window offscreen with wgpu and write
//! `transcript-*.png` there; without it no GPU is needed.

use crate::support::{Drive, Launch, builder, demo_session, driver, save_snapshot, with_snapshots};
use serde_json::{Value, json};

/// Reproducible redraw cost for a long sequence transcript, with and without many search hits.
/// Run alone with `--ignored --nocapture`; timings are observations, not CI thresholds.
#[test]
#[ignore = "manual transcript performance measurement"]
fn long_transcript_redraw_benchmark() {
    let mut d = Driver::demo();
    let item = d.harness.state().session.active_sequence().unwrap().audio_tracks[0].items[0].item;
    let words: Vec<Value> = (0..10_000)
        .map(|i| {
            json!({"text": "hello", "start": filmcraft_time::Tick::from_seconds_f64(i as f64 * 0.4).0,
            "end": filmcraft_time::Tick::from_seconds_f64(i as f64 * 0.4 + 0.3).0, "speaker": (i / 40) % 2})
        })
        .collect();
    d.ok(
        "engine.execute",
        json!({"command": "transcript.set", "params": {"item": item.0,
        "transcript": {"language": "en", "speakers": [{"name": "First"}, {"name": "Second"}], "words": words}}}),
    );
    d.ok("ui.set", json!({"workspace": "Captions and Graphics"}));
    d.harness.state_mut().ui.text_tab = "Transcript".into();
    // Extend only the synthetic fixture clip so all 10,000 words are actually laid out.
    let session = &mut d.harness.state_mut().session;
    let id = session.state.active_sequence.unwrap();
    let seq = std::sync::Arc::make_mut(&mut session.project).sequence_mut(id).unwrap();
    seq.audio_tracks[0].items.truncate(1);
    seq.audio_tracks[0].items[0].source_in = filmcraft_time::Tick::ZERO;
    seq.audio_tracks[0].items[0].duration = filmcraft_time::Tick::from_seconds_f64(4000.0);
    assert_eq!(filmcraft_engine::transcript::sequence_words(session).len(), 10_000);
    for query in ["", "hello"] {
        d.harness.state_mut().ui.transcript_search = query.into();
        d.frames(5);
        let mut times = Vec::new();
        for _ in 0..40 {
            let start = std::time::Instant::now();
            d.frames(1);
            times.push(start.elapsed().as_secs_f64() * 1000.0);
        }
        times.sort_by(f64::total_cmp);
        println!("transcript words=10000 query={query:?} p50_ms={:.3} p95_ms={:.3}", times[20], times[38]);
    }
}

driver!(Driver);

impl Driver {
    fn demo() -> Self {
        Self::open(demo_session(), with_snapshots(builder()))
    }
    fn snapshot(&mut self, name: &str) {
        save_snapshot(self, 2, &format!("transcript-{name}.png"));
    }
}

#[test]
fn transcript_tab_selects_and_extracts_words() {
    let mut d = Driver::demo();
    d.ok("ui.set", json!({"workspace": "Captions and Graphics"}));
    d.frames(2);
    d.ok("ui.click", json!({"id": "text.tab.Transcript"}));
    d.frames(3);
    assert_eq!(d.ids("text.transcript.generate"), vec!["text.transcript.generate".to_string()], "empty state offers Transcribe");

    // bring a transcript for the first A1 clip's media (as an agent without a speech model would)
    let probe = demo_session();
    let a = probe.active_sequence().unwrap().audio_tracks[0].items[0].clone();
    let tk = |s: f64| a.source_in.0 + (s * filmcraft_time::TICKS_PER_SECOND as f64) as i64;
    let words: Vec<Value> = ["Hello", "um", "there", "friend."]
        .iter()
        .enumerate()
        .map(|(i, w)| json!({"text": w, "start": tk(0.5 + i as f64 * 0.5), "end": tk(0.9 + i as f64 * 0.5), "speaker": 0}))
        .collect();
    d.ok("engine.execute", json!({"command": "transcript.set", "params": {"item": a.item.0, "transcript": {"language": "en", "words": words}}}));
    d.frames(3);
    let ids = d.ids("text.transcript.word.");
    assert_eq!(ids.len(), 4, "{ids:?}");
    d.snapshot("words");

    d.ok("ui.click", json!({"id": "text.transcript.word.1"}));
    d.frames(3);
    d.ok("ui.click", json!({"id": "text.transcript.extract"}));
    d.frames(3);
    let r = d.ok("engine.execute", json!({"command": "transcript.inspect", "params": {}}));
    let left: Vec<&str> = r["words"].as_array().unwrap().iter().filter_map(|w| w["text"].as_str()).collect();
    assert_eq!(left, ["Hello", "there", "friend."]);
    assert_eq!(d.ids("text.transcript.word.").len(), 3);
}

/// #2402: without speech-to-text (a build without `whisper`) the Transcribe button is disabled,
/// with the reason as its tooltip, instead of looking ready and doing nothing visible.
#[test]
fn transcribe_is_disabled_without_speech_to_text() {
    if filmcraft_engine::transcript::speech_available() {
        return;
    }
    let mut d = Driver::demo();
    d.ok("ui.set", json!({"workspace": "Captions and Graphics"}));
    d.frames(2);
    d.ok("ui.click", json!({"id": "text.tab.Transcript"}));
    d.frames(3);
    let els = d.ok("ui.elements", json!({"prefix": "text.transcript.generate"}));
    let button = els.as_array().unwrap().iter().find(|e| e["id"] == "text.transcript.generate").expect("the Transcribe button").clone();
    assert_eq!(button["enabled"], json!(false), "{button}");
    let undo = d.harness.state().session.history.undo.len();
    d.ok("ui.click", json!({"id": "text.transcript.generate"}));
    d.frames(3);
    assert_eq!(d.harness.state().session.history.undo.len(), undo, "nothing ran");
}
