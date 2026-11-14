//! Headless UI tests of the Clip Mismatch Warning (Settings ▸ Timeline ▸ Show Clip Mismatch Warning
//! dialog, `panels::clip_mismatch`): the first clip dragged onto an empty sequence it doesn't match,
//! or put in with Overwrite, asks whether the sequence takes the clip's settings.
//!
//! Set `FILMCRAFT_UI_SNAPSHOT_DIR=<dir>` to also render offscreen with wgpu and write
//! `clip-mismatch.png`.

use std::sync::mpsc::{Sender, channel};

use egui_kittest::Harness;
use filmcraft_engine::Session;
use filmcraft_project::BinEntry;
use filmcraft_time::FrameRate;
use filmcraft_ui_egui::FilmcraftApp;
use filmcraft_ui_egui::control::ControlRequest;
use serde_json::{Value, json};

/// The empty sequence the tests open, and the demo's movies, which don't match it.
const SEQUENCE: (u32, u32, FrameRate) = (1280, 720, FrameRate::FPS_25);
const CLIP: (u32, u32, FrameRate) = (1920, 1080, FrameRate::FPS_23_976);

struct Driver {
    harness: Harness<'static, FilmcraftApp>,
    tx: Sender<ControlRequest>,
    snapshots: Option<std::path::PathBuf>,
}

impl Driver {
    /// The demo project with a new, empty 1280 x 720, 25 fps sequence open.
    fn new() -> Self {
        let mut s = Session::default();
        s.execute("file.openDemoProject", json!({})).unwrap();
        s.execute("file.newSequence", json!({"name": "Empty", "width": 1280, "height": 720, "fps": 25})).unwrap();
        let (tx, rx) = channel();
        let app = FilmcraftApp::new(s).with_control(rx);
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

    fn exec(&mut self, command: &str, params: Value) -> Value {
        self.ok("engine.execute", json!({"command": command, "params": params}))
    }

    fn click(&mut self, id: &str) {
        self.ok("ui.click", json!({"id": id}));
        self.frames(3);
    }

    fn has(&mut self, id: &str) -> bool {
        let v = self.ok("ui.elements", json!({"prefix": id}));
        v.as_array().unwrap().iter().any(|e| e["id"] == json!(id))
    }

    fn wait_for(&mut self, id: &str) {
        for _ in 0..400 {
            if self.has(id) {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
            self.frames(1);
        }
        panic!("{id} never appeared");
    }

    fn app(&mut self) -> &mut FilmcraftApp {
        self.harness.state_mut()
    }

    fn dialog(&mut self) -> Value {
        self.ok("ui.inspect", json!({}))["dialog"].clone()
    }

    fn settings(&mut self) -> (u32, u32, FrameRate) {
        let q = self.app().session.active_sequence().unwrap();
        (q.settings.width, q.settings.height, q.settings.frame_rate)
    }

    fn clips(&mut self) -> usize {
        self.app().session.active_sequence().unwrap().all_tracks().map(|t| t.items.len()).sum()
    }

    /// The first movie of the demo's Footage bin, shown in the Project panel (the bin opened in place).
    fn movie(&mut self) -> u64 {
        let p = &self.app().session.project;
        let bin = p
            .root
            .children
            .iter()
            .find_map(|e| match e {
                BinEntry::Bin(b) if b.name == "Footage" => Some(b.clone()),
                _ => None,
            })
            .expect("Footage bin");
        let movie = bin
            .children
            .iter()
            .find_map(|e| match e {
                BinEntry::Item(i) if p.item(*i).is_some_and(|it| it.has_video() && it.has_audio()) => Some(i.0),
                _ => None,
            })
            .expect("a movie in Footage");
        self.ok("ui.menu.invoke", json!({"id": "projectPanel.openBin", "params": {"bin": bin.id.0, "how": "inPlace"}}));
        self.wait_for(&format!("project.item.{movie}"));
        movie
    }

    fn drag_to_timeline(&mut self, item: u64) {
        self.ok("ui.drag", json!({"from": {"id": format!("project.item.{item}")}, "to": {"id": "timeline.tracks", "fx": 0.3, "fy": 0.1}, "steps": 16}));
        self.frames(4);
    }

    fn snapshot(&mut self, name: &str) {
        let Some(dir) = self.snapshots.clone() else { return };
        self.frames(2);
        match self.harness.render() {
            Ok(img) => {
                std::fs::create_dir_all(&dir).unwrap();
                img.save(dir.join(format!("{name}.png"))).unwrap();
            }
            Err(e) => eprintln!("snapshot {name} skipped: {e}"),
        }
    }
}

#[test]
fn a_first_clip_that_does_not_match_asks_and_the_sequence_can_take_its_settings() {
    let mut d = Driver::new();
    let movie = d.movie();
    d.drag_to_timeline(movie);
    let status = d.app().ui.status.clone();
    assert_eq!(d.dialog(), json!("ClipMismatch"), "status: {status}");
    assert_eq!(d.clips(), 0, "nothing goes in before the answer");
    assert!(d.has("clipMismatch.change") && d.has("clipMismatch.keep"));
    d.snapshot("clip-mismatch");
    d.click("clipMismatch.change");
    assert_eq!(d.dialog(), Value::Null);
    assert_eq!((d.settings(), d.clips()), (CLIP, 2), "picture and sound, in a sequence that took the clip's settings");
    // one undo takes the clip out and puts the sequence's settings back
    d.exec("edit.undo", json!({}));
    assert_eq!((d.settings(), d.clips()), (SEQUENCE, 0));
}

#[test]
fn keep_existing_settings_puts_the_clip_in_as_it_is() {
    let mut d = Driver::new();
    let movie = d.movie();
    d.drag_to_timeline(movie);
    assert_eq!(d.dialog(), json!("ClipMismatch"));
    d.click("clipMismatch.keep");
    assert_eq!(d.dialog(), Value::Null);
    assert_eq!((d.settings(), d.clips()), (SEQUENCE, 2));
    // the sequence holds clips now: the next one goes in without asking
    d.drag_to_timeline(movie);
    assert_eq!(d.dialog(), Value::Null);
}

#[test]
fn escape_puts_nothing_in() {
    let mut d = Driver::new();
    let movie = d.movie();
    d.drag_to_timeline(movie);
    assert_eq!(d.dialog(), json!("ClipMismatch"));
    d.ok("ui.key", json!({"key": "Escape"}));
    d.frames(3);
    assert_eq!(d.dialog(), Value::Null);
    assert_eq!((d.settings(), d.clips()), (SEQUENCE, 0));
}

#[test]
fn with_the_warning_turned_off_the_clip_goes_in_as_it_is() {
    let mut d = Driver::new();
    d.exec("prefs.set", json!({"key": "timeline.clipMismatchWarning", "value": false}));
    let movie = d.movie();
    d.drag_to_timeline(movie);
    assert_eq!(d.dialog(), Value::Null);
    assert_eq!((d.settings(), d.clips()), (SEQUENCE, 2));
}

#[test]
fn overwrite_from_the_source_monitor_asks_too() {
    let mut d = Driver::new();
    let movie = d.movie();
    d.exec("source.open", json!({"item": movie}));
    // the menu path, as the Overwrite button and the . key take
    let r = d.ok("ui.menu.invoke", json!({"id": "source.overwrite", "params": {}}));
    assert_eq!(r, json!({"dialog": "clipMismatch"}));
    d.frames(2);
    assert_eq!(d.dialog(), json!("ClipMismatch"));
    assert_eq!(d.clips(), 0);
    d.click("clipMismatch.change");
    assert_eq!((d.settings(), d.clips()), (CLIP, 2));
}
