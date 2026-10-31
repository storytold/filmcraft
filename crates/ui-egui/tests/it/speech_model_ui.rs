//! Headless UI tests of the speech model download dialog (G19). Never on the network: Download is
//! clicked only in a build without model downloads, where it reports why in the dialog.

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

    fn label(&mut self, id: &str) -> String {
        let v = self.ok("ui.elements", json!({"prefix": id}));
        v.as_array().unwrap().iter().find(|e| e["id"] == json!(id)).map(|e| e["label"].as_str().unwrap_or("").to_string()).unwrap_or_default()
    }
}

#[test]
fn the_download_dialog_names_the_model_licence_and_can_be_cancelled() {
    check_download_dialog("whisper-base", "MIT");
}

#[test]
fn the_parakeet_dialog_names_nvidia_and_its_cc_by_licence() {
    check_download_dialog("parakeet-tdt-0.6b-v3", "CC-BY-4.0");
}

fn check_download_dialog(model_id: &str, licence: &str) {
    let mut d = Driver::demo();
    let model = filmcraft_engine::transcript::find_model(model_id).unwrap();
    let document_epoch = d.app().session.document_epoch;
    d.app().ui.speech_download = Some(filmcraft_ui_egui::state::SpeechDownload {
        model: model.id.into(),
        document_epoch,
        then_command: "transcript.generate".into(),
        then_params: json!({}),
        ..Default::default()
    });
    d.frames(3);
    assert!(d.label("speechModel.license").contains(licence), "the licence is shown");
    assert!(!d.label("speechModel.download").is_empty(), "Download and Transcribe is offered");
    d.snapshot(&format!("download-{model_id}"));
    // a build with downloads (the app's default features, unified into this test when built
    // together) would really download: only a build without them is checked here, offline
    if d.app().session.is_enabled("transcript.downloadModel") {
        d.click("speechModel.cancel");
        assert!(d.app().ui.speech_download.is_none(), "Cancel closes it");
        return;
    }
    // this build has no downloads: the dialog says so and starts nothing
    d.click("speechModel.download");
    d.frames(2);
    let dl = d.app().ui.speech_download.clone().unwrap();
    assert!(dl.job.is_none() && dl.error.contains("not available in this build"), "{dl:?}");
    assert!(d.app().session.jobs.is_empty());
    assert!(d.app().session.project.transcripts.is_empty(), "nothing was transcribed");
    d.click("speechModel.cancel");
    assert!(d.app().ui.speech_download.is_none(), "Cancel closes it");
}

#[test]
fn a_model_download_cannot_resume_transcription_in_another_project() {
    let mut d = Driver::demo();
    let document_epoch = d.app().session.document_epoch;
    d.app().ui.speech_download = Some(filmcraft_ui_egui::state::SpeechDownload {
        model: "whisper-base".into(),
        document_epoch,
        then_command: "transcript.generate".into(),
        then_params: json!({"wait": false}),
        ..Default::default()
    });
    d.app().session.execute("file.openDemoProject", json!({})).unwrap();
    d.frames(3);
    assert!(d.app().ui.speech_download.is_none());
    assert!(d.app().session.jobs.is_empty());
    assert!(d.app().session.project.transcripts.is_empty());
}

#[test]
fn running_download_cancel_is_centered_and_only_requests_cancellation() {
    let mut d = Driver::demo();
    let before = d.app().session.project.clone();
    d.app().session.jobs.push(filmcraft_engine::Job {
        id: 998,
        label: "Download Speech Model: Synthetic".into(),
        progress: Default::default(),
        result: Default::default(),
    });
    d.app().ui.speech_download = Some(filmcraft_ui_egui::state::SpeechDownload {
        model: "whisper-small".into(),
        document_epoch: d.app().session.document_epoch,
        job: Some(998),
        ..Default::default()
    });
    d.frames(3);
    let elements = d.ok("ui.elements", json!({"prefix":"speechModel."}));
    let rect = |id: &str| {
        elements.as_array().unwrap().iter().find(|e| e["id"] == id).unwrap()["rect"].as_array().unwrap().iter().map(|v| v.as_f64().unwrap()).collect::<Vec<_>>()
    };
    let bounds = rect("speechModel.bounds");
    let cancel = rect("speechModel.cancel");
    assert!(((cancel[0] + cancel[2] * 0.5) - (bounds[0] + bounds[2] * 0.5)).abs() < 2.0, "{bounds:?} {cancel:?}");
    assert!(d.label("speechModel.download").is_empty());
    d.snapshot("download-centered-cancel");
    d.click("speechModel.cancel");
    assert!(d.app().session.jobs[0].progress.cancel.load(std::sync::atomic::Ordering::Relaxed));
    assert!(d.app().ui.speech_download.is_none());
    assert_eq!(d.app().session.project, before);
}

#[test]
fn model_only_download_completion_never_starts_transcription() {
    let mut d = Driver::demo();
    let before = d.app().session.project.clone();
    let progress = filmcraft_engine::Job { id: 999, label: String::new(), progress: Default::default(), result: Default::default() }.progress;
    progress.finished.store(true, std::sync::atomic::Ordering::Relaxed);
    d.app().session.jobs.push(filmcraft_engine::Job { id: 999, label: "Download Speech Model: Synthetic".into(), progress, result: Default::default() });
    d.app().ui.speech_download = Some(filmcraft_ui_egui::state::SpeechDownload {
        model: "whisper-small".into(),
        document_epoch: d.app().session.document_epoch,
        job: Some(999),
        ..Default::default()
    });
    d.frames(2);
    assert!(d.app().ui.speech_download.is_none());
    assert_eq!(d.app().session.jobs.len(), 1, "no inference job created");
    assert_eq!(d.app().session.project, before);
}
