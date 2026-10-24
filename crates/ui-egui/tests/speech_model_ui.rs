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
        let harness = Harness::builder().with_size(egui::vec2(1600.0, 980.0)).with_step_dt(1.0 / 60.0).with_max_steps(10_000).build_eframe(move |_cc| app);
        let mut d = Driver { harness, tx };
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
    let mut d = Driver::demo();
    let model = filmcraft_engine::transcript::find_model("whisper-base").unwrap();
    d.app().ui.speech_download = Some(filmcraft_ui_egui::state::SpeechDownload {
        model: model.id.into(),
        then_command: "transcript.generate".into(),
        then_params: json!({}),
        ..Default::default()
    });
    d.frames(3);
    assert!(d.label("speechModel.license").contains("MIT"), "the licence is shown");
    assert!(!d.label("speechModel.download").is_empty(), "Download and Transcribe is offered");
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
