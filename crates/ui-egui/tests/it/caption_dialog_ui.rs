//! Headless UI test of the Create captions dialog: Create captions opens it, Create makes the
//! captions, and the next time it opens with the settings used last.

use std::sync::mpsc::{Sender, channel};

use egui_kittest::Harness;
use filmcraft_engine::Session;
use filmcraft_time::{TICKS_PER_SECOND, Tick};
use filmcraft_ui_egui::FilmcraftApp;
use filmcraft_ui_egui::control::ControlRequest;
use serde_json::{Value, json};

struct Driver {
    harness: Harness<'static, FilmcraftApp>,
    tx: Sender<ControlRequest>,
}

impl Driver {
    fn new(s: Session) -> Self {
        let (tx, rx) = channel();
        let app = FilmcraftApp::new(s).with_control(rx);
        let mut builder = Harness::builder().with_size(egui::vec2(1600.0, 980.0)).with_max_steps(10_000);
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

    fn call(&mut self, method: &str, params: Value) -> Value {
        let (req, reply) = ControlRequest::new(method, params.clone());
        self.tx.send(req).unwrap();
        for _ in 0..600 {
            self.frames(1);
            if let Ok(v) = reply.try_recv() {
                return v;
            }
        }
        panic!("no reply to {method} {params}");
    }

    fn ok(&mut self, method: &str, params: Value) -> Value {
        let v = self.call(method, params.clone());
        assert_eq!(v["ok"], json!(true), "{method} {params} failed: {v}");
        v["result"].clone()
    }

    fn app(&mut self) -> &mut FilmcraftApp {
        self.harness.state_mut()
    }
}

/// The demo project with a transcript on its first A1 clip.
fn session() -> Session {
    let mut s = Session::default();
    s.execute("file.openDemoProject", json!({})).unwrap();
    let a = s.active_sequence().unwrap().audio_tracks[0].items[0].clone();
    let at = |x: f64| (a.source_in + Tick((x * TICKS_PER_SECOND as f64) as i64)).0;
    let words: Vec<Value> = ["Hello", "there", "this", "is", "a", "caption", "test."]
        .iter()
        .enumerate()
        .map(|(i, w)| json!({"text": w, "start": at(0.2 + i as f64 * 0.4), "end": at(0.5 + i as f64 * 0.4)}))
        .collect();
    s.execute("transcript.set", json!({"item": a.item.0, "transcript": {"language": "en", "words": words}})).unwrap();
    s
}

#[test]
fn create_captions_opens_the_dialog_and_remembers_its_settings() {
    let mut d = Driver::new(session());
    let r = d.ok("engine.execute", json!({"command": "transcript.createCaptions"}));
    assert_eq!(r["dialog"], "captions");
    // the modal fades in before it takes clicks
    d.frames(10);
    d.snapshot("caption-dialog");
    assert!(d.app().session.active_sequence().unwrap().caption_tracks.is_empty(), "nothing is made until Create");

    d.ok("ui.click", json!({"id": "captions.layout.single"}));
    d.frames(3);
    assert_eq!(d.app().ui.caption_dialog.as_ref().unwrap().settings.lines, 1);
    d.app().ui.caption_dialog.as_mut().unwrap().settings.max_chars = 7;
    d.app().ui.caption_dialog.as_mut().unwrap().settings.min_seconds = 1.2;
    d.frames(2);
    d.ok("ui.click", json!({"id": "captions.create"}));
    d.frames(3);
    assert!(d.app().ui.caption_dialog.is_none(), "Create closes the dialog");
    let q = d.app().session.active_sequence().unwrap().clone();
    let tr = q.caption_tracks.first().expect("a caption track");
    assert!(tr.captions.len() >= 4, "short single-line captions: {:?}", tr.captions.iter().map(|c| &c.text).collect::<Vec<_>>());
    assert!(tr.captions.iter().all(|c| !c.text.contains('\n')));
    assert_eq!(d.app().ui.text_tab, "Captions");
    let last = d.app().session.prefs.captions.last.clone();
    assert_eq!((last.lines, last.max_chars, last.min_seconds), (1, 7, 1.2));

    // next time it opens with those settings; Cancel makes nothing
    d.ok("engine.execute", json!({"command": "transcript.createCaptions"}));
    d.frames(3);
    assert_eq!(d.app().ui.caption_dialog.as_ref().unwrap().settings, last);
    d.ok("ui.click", json!({"id": "captions.cancel"}));
    d.frames(3);
    assert!(d.app().ui.caption_dialog.is_none());
    assert_eq!(d.app().session.active_sequence().unwrap().caption_tracks.len(), 1);
}

#[test]
fn saved_presets_are_kept_in_preferences() {
    let mut d = Driver::new(session());
    d.ok("engine.execute", json!({"command": "transcript.createCaptions"}));
    d.frames(10);
    d.app().ui.caption_dialog.as_mut().unwrap().settings.max_chars = 7;
    d.app().ui.caption_dialog.as_mut().unwrap().saving = Some("Shorts".into());
    // the dialog grows by the name row and re-centres
    d.frames(10);
    d.ok("ui.click", json!({"id": "captions.presetSave"}));
    d.frames(3);
    let presets = d.app().session.prefs.captions.user_presets.clone();
    assert_eq!(presets.len(), 1);
    assert_eq!((presets[0].name.as_str(), presets[0].settings.max_chars), ("Shorts", 7));
    assert!(d.app().ui.caption_dialog.as_ref().unwrap().saving.is_none());
}

/// Create still makes the captions when the preferences can't be written, and the status bar
/// keeps the save error instead of "Created n captions" covering it.
#[test]
fn a_failed_settings_save_stays_in_the_status_bar() {
    let dir = tempfile::tempdir().unwrap();
    let blocker = dir.path().join("not-a-directory");
    std::fs::write(&blocker, b"").unwrap();
    let mut s = session();
    s.prefs_path = Some(blocker.join("preferences.json"));
    let mut d = Driver::new(s);
    d.ok("engine.execute", json!({"command": "transcript.createCaptions"}));
    d.frames(10);
    d.app().ui.caption_dialog.as_mut().unwrap().settings.max_chars = 7;
    d.frames(2);
    d.ok("ui.click", json!({"id": "captions.create"}));
    d.frames(3);
    assert!(d.app().ui.caption_dialog.is_none(), "Create closes the dialog");
    assert_eq!(d.app().session.active_sequence().unwrap().caption_tracks.len(), 1);
    let status = d.app().ui.status.clone();
    assert!(status.starts_with("Couldn't save caption settings"), "{status}");
}
