//! Headless UI test of deleting caption tracks: the caption track header's right-click menu
//! (Delete Caption Track / Delete Empty Caption Tracks) and the Caption Tracks section of
//! Sequence ▸ Delete Tracks….

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
        let harness = Harness::builder().with_size(egui::vec2(1600.0, 980.0)).with_max_steps(10_000).build_eframe(move |_cc| app);
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

    fn ids(&mut self, prefix: &str) -> Vec<String> {
        let v = self.ok("ui.elements", json!({"prefix": prefix}));
        v.as_array().unwrap().iter().filter_map(|e| e["id"].as_str().map(str::to_string)).collect()
    }

    fn exec(&mut self, cmd: &str, params: Value) -> Value {
        self.ok("engine.execute", json!({"command": cmd, "params": params}))
    }

    /// Caption track names top first, with their caption counts.
    fn caption_tracks(&mut self) -> Vec<(String, usize)> {
        let r = self.exec("captions.list", json!({}));
        r["tracks"].as_array().unwrap().iter().map(|t| (t["name"].as_str().unwrap().to_string(), t["captions"].as_array().unwrap().len())).collect()
    }

    fn right_click_menu(&mut self, track: &str, item: &str) {
        self.ok("ui.click", json!({"id": format!("timeline.captionTrack.{track}"), "button": "right"}));
        self.frames(3);
        self.ok("ui.click", json!({"id": format!("timeline.captionTrack.{track}.menu.{item}")}));
        self.frames(3);
    }
}

fn setup() -> Driver {
    let mut d = Driver::demo();
    // start from no caption tracks, then add four (each goes on top): Keep (with a caption), E1–E3 empty
    while !d.caption_tracks().is_empty() {
        d.exec("captions.deleteTrack", json!({"track": "C1"}));
    }
    for name in ["E3", "E2", "Keep", "E1"] {
        d.exec("captions.newTrack", json!({"format": "Subtitle", "name": name}));
    }
    d.exec("captions.add", json!({"track": "C2", "text": "Hello", "seconds": 1.0}));
    d.frames(3);
    assert_eq!(d.caption_tracks(), [("E1".into(), 0), ("Keep".into(), 1), ("E2".into(), 0), ("E3".into(), 0)]);
    d
}

#[test]
fn header_menu_deletes_one_or_all_empty_caption_tracks() {
    let mut d = setup();
    d.right_click_menu("C1", "delete");
    assert_eq!(d.caption_tracks(), [("Keep".into(), 1), ("E2".into(), 0), ("E3".into(), 0)]);
    // from the non-empty track's menu: only the empty ones go
    d.right_click_menu("C1", "deleteEmpty");
    assert_eq!(d.caption_tracks(), [("Keep".into(), 1)]);
    // the last caption track can be deleted too, captions and all; undo brings it back
    d.right_click_menu("C1", "delete");
    assert!(d.caption_tracks().is_empty());
    assert!(d.ids("timeline.captionTrack.").is_empty(), "no caption rows left");
    d.exec("edit.undo", json!({}));
    d.frames(3);
    assert_eq!(d.caption_tracks(), [("Keep".into(), 1)]);
}

#[test]
fn delete_tracks_dialog_has_a_caption_section() {
    let mut d = setup();
    d.ok("ui.menu.invoke", json!({"command": "sequence.deleteTracks", "params": {}}));
    d.frames(3);
    d.ok("ui.click", json!({"id": "deleteTracks.captions"}));
    d.frames(2);
    // one specific track: C3 (E2)
    d.ok("ui.click", json!({"id": "deleteTracks.captions.target"}));
    d.frames(3);
    d.ok("ui.click", json!({"id": "deleteTracks.captions.option.C3"}));
    d.frames(2);
    d.ok("ui.click", json!({"id": "deleteTracks.ok"}));
    d.frames(3);
    assert_eq!(d.caption_tracks(), [("E1".into(), 0), ("Keep".into(), 1), ("E3".into(), 0)]);
    // All Empty Tracks (the default choice)
    d.ok("ui.menu.invoke", json!({"command": "sequence.deleteTracks", "params": {}}));
    d.frames(3);
    d.ok("ui.click", json!({"id": "deleteTracks.captions"}));
    d.frames(2);
    d.ok("ui.click", json!({"id": "deleteTracks.ok"}));
    d.frames(3);
    assert_eq!(d.caption_tracks(), [("Keep".into(), 1)]);
}
