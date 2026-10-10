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
        let mut b = Harness::builder().with_step_dt(1.0 / 60.0).with_size(egui::vec2(1600.0, 980.0)).with_max_steps(10_000);
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

    d.ok("engine.execute", json!({"command": "source.open", "params": {"item": a.item.0}}));
    d.frames(3);
    d.ok("ui.panel.show", json!({"panel": "Text"}));
    d.frames(3);
    d.ok("ui.click", json!({"id": "text.transcript.view.source"}));
    d.frames(3);
    d.snapshot("source-before");
    assert_eq!(
        d.ids("text.transcript.word.").len(),
        4,
        "source still contains extracted words; source={:?}, view={:?}",
        d.harness.state().session.state.source_item,
        d.harness.state().ui.transcript_source
    );
    d.frames(40);
    d.ok("ui.click", json!({"id": "text.transcript.word.0"}));
    d.ok("ui.click", json!({"id": "text.transcript.word.0"}));
    d.frames(3);
    d.snapshot("correction");
    assert_eq!(d.ids("text.transcript.correction").len(), 3, "editor state: {:?}", d.harness.state().ui.transcript_edit);
    d.ok("ui.click", json!({"id": "text.transcript.correction"}));
    d.ok("ui.key", json!({"key": "A", "command": true}));
    d.ok("ui.type", json!({"text": "Howdy"}));
    d.frames(2);
    d.ok("ui.click", json!({"id": "text.transcript.correction.apply"}));
    d.frames(3);
    let source = d.ok("engine.execute", json!({"command": "transcript.source", "params": {}}));
    assert_eq!(source["words"][0]["text"], "Howdy");
    d.snapshot("source");
    d.ok("ui.click", json!({"id": "text.transcript.models"}));
    d.frames(3);
    assert_eq!(d.ids("text.transcript.model.").len(), 3);
    d.snapshot("models");
    d.ok("ui.key", json!({"key": "Escape"}));
    d.frames(3);

    let job = filmcraft_engine::Job { id: 1, label: "Transcribe".into(), progress: Default::default(), result: Default::default() };
    job.progress.total.store(100, std::sync::atomic::Ordering::Relaxed);
    job.progress.done.store(25, std::sync::atomic::Ordering::Relaxed);
    *job.progress.status.lock().unwrap() = "Recognizing dialogue".into();
    let progress = job.progress.clone();
    d.harness.state_mut().session.jobs.push(job);
    d.frames(3);
    assert_eq!(d.ids("text.transcript.progress").len(), 1);
    d.snapshot("progress");
    d.ok("ui.click", json!({"id": "text.transcript.cancel"}));
    d.frames(3);
    assert!(progress.cancel.load(std::sync::atomic::Ordering::Relaxed));
}
