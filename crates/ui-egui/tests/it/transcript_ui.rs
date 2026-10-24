//! Headless UI test of the Text panel's Transcript tab: the Transcribe button, the word view,
//! clicking words (In/Out from the selection) and extracting the selected text.
//!
//! Set `FILMCRAFT_UI_SNAPSHOT_DIR=<dir>` to also render the window offscreen with wgpu and write
//! `transcript-*.png` there; without it no GPU is needed.

use std::sync::mpsc::{Sender, channel};

use egui_kittest::Harness;
use filmcraft_engine::Session;
use filmcraft_ui_egui::FilmcraftApp;
use filmcraft_ui_egui::control::ControlRequest;
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

struct Driver {
    harness: Harness<'static, FilmcraftApp>,
    tx: Sender<ControlRequest>,
    snapshots: Option<std::path::PathBuf>,
}

impl Driver {
    fn demo() -> Self {
        let mut session = Session::default();
        session.execute("file.openDemoProject", json!({})).expect("demo project");
        let (tx, rx) = channel();
        let app = FilmcraftApp::new(session).with_control(rx);
        let snapshots = std::env::var_os("FILMCRAFT_UI_SNAPSHOT_DIR").map(std::path::PathBuf::from);
        let mut b = Harness::builder().with_size(egui::vec2(1600.0, 980.0)).with_max_steps(10_000);
        if snapshots.is_some() {
            b = b.wgpu();
        }
        let harness = b.build_eframe(move |_cc| app);
        let mut d = Driver { harness, tx, snapshots };
        d.frames(4);
        d
    }

    fn frames(&mut self, n: usize) {
        for _ in 0..n {
            let ctx = self.harness.ctx.clone();
            let mut raw = std::mem::take(self.harness.input_mut());
            eframe::App::raw_input_hook(self.harness.state_mut(), &ctx, &mut raw);
            *self.harness.input_mut() = raw;
            self.harness.step();
        }
    }

    fn ok(&mut self, method: &str, params: Value) -> Value {
        let (req, reply) = ControlRequest::new(method, params.clone());
        self.tx.send(req).unwrap();
        for _ in 0..600 {
            self.frames(1);
            if let Ok(v) = reply.try_recv() {
                assert_eq!(v["ok"], json!(true), "{method} {params} failed: {v}");
                return v["result"].clone();
            }
        }
        panic!("no reply to {method} {params}");
    }

    fn ids(&mut self, prefix: &str) -> Vec<String> {
        let v = self.ok("ui.elements", json!({"prefix": prefix}));
        v.as_array().unwrap().iter().filter_map(|e| e["id"].as_str().map(str::to_string)).collect()
    }

    fn snapshot(&mut self, name: &str) {
        let Some(dir) = self.snapshots.clone() else { return };
        self.frames(2);
        match self.harness.render() {
            Ok(img) => {
                std::fs::create_dir_all(&dir).unwrap();
                img.save(dir.join(format!("transcript-{name}.png"))).unwrap();
            }
            Err(e) => eprintln!("snapshot {name} skipped: {e}"),
        }
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
    let mut probe = Session::default();
    probe.execute("file.openDemoProject", json!({})).unwrap();
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

#[test]
fn retranscribe_replaces_current_text_and_can_be_undone() {
    use filmcraft_project::{Transcript, Word};
    use filmcraft_speech::FixedTranscriber;
    use std::sync::Arc;

    let mut d = Driver::demo();
    let clip = d.harness.state().session.active_sequence().unwrap().audio_tracks[0].items[0].clone();
    let start = clip.source_in + filmcraft_time::Tick::from_seconds_f64(0.5);
    let end = start + filmcraft_time::Tick::from_seconds_f64(0.4);
    d.ok(
        "engine.execute",
        json!({"command":"transcript.set", "params":{"item":clip.item.0,
        "transcript":{"language":"en","words":[{"text":"Original","start":start.0,"end":end.0}]}}}),
    );
    d.harness.state_mut().session.transcriber = Some(Arc::new(FixedTranscriber {
        transcript: Transcript { language: "de".into(), words: vec![Word::new("Neu", start, end)], ..Default::default() },
        id: "new-transcription".into(),
    }));
    d.ok("ui.set", json!({"workspace":"Captions and Graphics"}));
    d.harness.state_mut().ui.text_tab = "Transcript".into();
    d.frames(3);
    let previous_job = d.harness.state().session.jobs.iter().map(|job| job.id).max().unwrap_or(0);
    d.ok("ui.click", json!({"id":"text.transcript.retranscribe"}));
    let job_id = d.harness.state().session.jobs.iter().find(|job| job.id > previous_job).map(|job| job.id).expect("retranscribe starts a background job");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        d.frames(1);
        let state = d.harness.state();
        let result = state.session.jobs.iter().find(|job| job.id == job_id).and_then(|job| job.result.lock().unwrap_or_else(|e| e.into_inner()).clone());
        if let Some(ref result) = result {
            assert!(result.is_ok(), "transcription job failed: {result:?}");
        }
        if result.is_some() && state.session.project.transcripts[&clip.item].text() == "Neu" {
            break;
        }
        assert!(std::time::Instant::now() < deadline, "transcription job did not finish");
    }
    assert_eq!(d.harness.state().session.project.transcripts[&clip.item].text(), "Neu");
    d.ok("engine.execute", json!({"command":"edit.undo"}));
    assert_eq!(d.harness.state().session.project.transcripts[&clip.item].text(), "Original");
}
