//! Headless UI tests of transcribing in the background from the Text panel: the progress shows,
//! the editor stays responsive, Cancel stops it without a change, and a finished run stores the
//! transcript.

use std::sync::atomic::{AtomicBool, Ordering};

use filmcraft_engine::project::Transcript;
use filmcraft_engine::speech::{Options, ProgressFn, SpeechError, Transcriber};

use std::sync::Arc;
use std::sync::mpsc::{Sender, channel};

use egui_kittest::Harness;
use filmcraft_engine::Session;
use filmcraft_ui_egui::FilmcraftApp;
use filmcraft_ui_egui::control::ControlRequest;
use serde_json::{Value, json};

struct Driver {
    harness: Harness<'static, FilmcraftApp>,
    tx: Sender<ControlRequest>,
}

impl Driver {
    fn demo() -> Self {
        let mut session = Session::default();
        session.execute("file.openDemoProject", json!({})).expect("demo project");
        let (tx, rx) = channel();
        let app = FilmcraftApp::new(session).with_control(rx);
        let mut builder = Harness::builder().with_size(egui::vec2(1600.0, 980.0)).with_step_dt(1.0 / 60.0).with_max_steps(10_000);
        if std::env::var_os("FILMCRAFT_UI_SNAPSHOT_DIR").is_some() {
            builder = builder.wgpu();
        }
        let harness = builder.build_eframe(move |_cc| app);
        let mut d = Driver { harness, tx };
        d.frames(4);
        d
    }

    fn snapshot(&mut self, name: &str) {
        let Some(dir) = std::env::var_os("FILMCRAFT_UI_SNAPSHOT_DIR") else { return };
        self.frames(2);
        let dir = std::path::PathBuf::from(dir);
        std::fs::create_dir_all(&dir).unwrap();
        self.harness.render().unwrap().save(dir.join(format!("speech-{name}.png"))).unwrap();
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

    fn click(&mut self, id: &str) {
        self.ok("ui.click", json!({"id": id}));
        self.frames(2);
    }

    fn app(&mut self) -> &mut FilmcraftApp {
        self.harness.state_mut()
    }

    fn has(&mut self, id: &str) -> bool {
        let v = self.ok("ui.elements", json!({"prefix": id}));
        v.as_array().unwrap().iter().any(|e| e["id"] == json!(id))
    }
}

/// A recogniser that works until `release` is set (or it is cancelled), then returns one word.
struct Slow {
    release: Arc<AtomicBool>,
}

impl Transcriber for Slow {
    fn id(&self) -> String {
        "slow".into()
    }
    fn transcribe(&self, _: &[f32], _: &Options, progress: ProgressFn) -> Result<Transcript, SpeechError> {
        for _ in 0..2000 {
            if !progress(0.5, "Transcribing") {
                return Err(SpeechError::Cancelled);
            }
            if self.release.load(Ordering::Relaxed) {
                let mut t = Transcript { language: "en".into(), ..Default::default() };
                t.words.push(filmcraft_engine::project::Word::new(
                    "Hello",
                    filmcraft_engine::time::Tick::ZERO,
                    filmcraft_engine::time::Tick::from_seconds_f64(0.5),
                ));
                return Ok(t);
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        Err(SpeechError::Cancelled)
    }
}

#[test]
fn transcribing_runs_in_the_background_with_progress_and_cancel() {
    let mut d = Driver::demo();
    let release = Arc::new(AtomicBool::new(false));
    d.app().session.transcriber = Some(Arc::new(Slow { release: release.clone() }));
    d.ok("ui.panel.show", json!({"panel": "Text"}));
    d.frames(3);
    d.click("text.tab.Transcript");
    d.click("text.transcript.generate");
    // the click returns at once: the panel shows the progress while it runs
    assert!(d.has("text.transcribing.progress"), "the progress shows");
    assert!(d.app().session.project.transcripts.is_empty());
    let original_sequence = d.app().session.state.active_sequence.unwrap();
    d.ok("engine.execute", json!({"command":"file.newSequence","params":{"name":"Other sequence"}}));
    assert!(!d.has("text.transcribing.progress"), "unrelated sequence remains readable");
    assert!(d.has("text.transcript.generate"));
    d.ok("engine.execute", json!({"command":"sequence.open","params":{"item":original_sequence.0}}));
    assert!(d.has("text.transcribing.progress"), "returning to the affected sequence restores progress");
    // Cancel stops it, and nothing is stored
    d.click("text.transcribing.cancel");
    assert!(d.app().session.journal.iter().any(|(command, _)| command == "jobs.cancel"), "Cancel uses the registered command");
    for _ in 0..200 {
        d.frames(1);
        if filmcraft_engine::transcript::running_job(&d.app().session).is_none() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    assert!(filmcraft_engine::transcript::running_job(&d.app().session).is_none(), "stopped");
    assert!(d.app().session.project.transcripts.is_empty(), "a cancelled run changes nothing");

    // again, and let it finish: the transcript is stored
    d.frames(3);
    d.click("text.transcript.generate");
    assert!(d.has("text.transcribing.progress"));
    d.snapshot("background-progress");
    release.store(true, Ordering::Relaxed);
    for _ in 0..400 {
        d.frames(1);
        if !d.app().session.project.transcripts.is_empty() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    assert!(!d.app().session.project.transcripts.is_empty(), "the finished run stores its transcript");
}
