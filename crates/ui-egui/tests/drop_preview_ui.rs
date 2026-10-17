//! Dragging a Project item over the Timeline previews every track the drop fills (#1887): an item
//! with picture and sound shows a box on its video track and on its audio track, not only on the
//! row under the pointer, and the drop then lands on exactly the previewed tracks.

use std::collections::BTreeSet;
use std::sync::mpsc::{Sender, channel};

use egui_kittest::Harness;
use filmcraft_engine::Session;
use filmcraft_project::{BinEntry, ItemId};
use filmcraft_ui_egui::FilmcraftApp;
use filmcraft_ui_egui::control::ControlRequest;
use serde_json::{Value, json};

struct Driver {
    harness: Harness<'static, FilmcraftApp>,
    tx: Sender<ControlRequest>,
    /// `FILMCRAFT_UI_SNAPSHOT_DIR`: where each drag's mid-drag frame is saved (off by default).
    snapshots: Option<std::path::PathBuf>,
}

impl Driver {
    /// The demo project with a new, empty sequence open.
    fn demo() -> Self {
        let mut session = Session::default();
        session.execute("file.openDemoProject", json!({})).expect("demo project");
        session.execute("file.newSequence", json!({"name": "Drop preview", "width": 1280, "height": 720, "fps": 24})).expect("sequence");
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
            if let Ok(r) = reply.try_recv() {
                assert_eq!(r["ok"], true, "{method} {params}: {r}");
                return r["result"].clone();
            }
        }
        panic!("no reply to {method} {params}");
    }
    fn ids(&mut self, prefix: &str) -> BTreeSet<String> {
        let v = self.ok("ui.elements", json!({"prefix": prefix}));
        v.as_array().unwrap().iter().filter_map(|e| e["id"].as_str().map(str::to_string)).collect()
    }
    fn rect(&mut self, id: &str) -> egui::Rect {
        let v = self.ok("ui.elements", json!({"prefix": id}));
        let e = v.as_array().unwrap().iter().find(|e| e["id"] == id).unwrap_or_else(|| panic!("no element {id}")).clone();
        let r: Vec<f32> = e["rect"].as_array().unwrap().iter().map(|x| x.as_f64().unwrap() as f32).collect();
        egui::Rect::from_min_size(egui::pos2(r[0], r[1]), egui::vec2(r[2], r[3]))
    }
    fn send(&mut self, e: egui::Event) {
        self.harness.input_mut().events.push(e);
        self.frames(1);
    }
    /// The demo's Footage bin open in the Project panel's list view: an item in it carrying
    /// `video` / `audio`, and its row's centre.
    fn footage_item(&mut self, video: bool, audio: bool) -> Option<(ItemId, egui::Pos2)> {
        let p = &self.harness.state().session.project;
        let bin = p.root.children.iter().find_map(|e| if let BinEntry::Bin(b) = e { (b.name == "Footage").then(|| b.clone()) } else { None })?;
        let item = bin.children.iter().find_map(|e| match e {
            BinEntry::Item(i) if p.source_streams(*i) == (video, audio) => Some(*i),
            _ => None,
        })?;
        self.ok("ui.panel.show", json!({"panel": "Project"}));
        self.ok("engine.execute", json!({"command": "project.view.set", "params": {"view": "list"}}));
        self.ok("ui.menu.invoke", json!({"id": "projectPanel.openBin", "params": {"bin": bin.id.0, "how": "inPlace"}}));
        self.frames(4);
        let row = self.rect(&format!("project.item.{}", item.0));
        Some((item, row.center()))
    }
    /// Drag from `from` to 60 px into the clip area of the track row labelled `track`; returns the
    /// drop previews shown there while the button is still down, then drops.
    fn drag_to_track(&mut self, from: egui::Pos2, track: &str) -> BTreeSet<String> {
        let row = self.rect(&format!("timeline.track.{track}.target"));
        let panel = self.rect("panel.Timeline");
        let to = egui::pos2(panel.min.x + self.harness.state().ui.timeline.header_w + 60.0, row.center().y);
        let button = |pos, pressed| egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed, modifiers: Default::default() };
        self.send(egui::Event::PointerMoved(from));
        self.send(button(from, true));
        for k in 1..=12 {
            self.send(egui::Event::PointerMoved(from.lerp(to, k as f32 / 12.0)));
        }
        self.frames(2);
        let shown = self.ids("timeline.dropPreview.");
        self.snapshot(&format!("drop-preview-over-{track}"));
        self.send(button(to, false));
        self.frames(4);
        shown
    }
    fn snapshot(&mut self, name: &str) {
        let Some(dir) = self.snapshots.clone() else { return };
        match self.harness.render() {
            Ok(img) => {
                std::fs::create_dir_all(&dir).unwrap();
                let path = dir.join(format!("{name}.png"));
                img.save(&path).unwrap();
                eprintln!("snapshot: {}", path.display());
            }
            Err(e) => eprintln!("snapshot {name} skipped: {e}"),
        }
    }
    /// "V2" / "A1" labels of the active sequence's tracks that hold a clip of `item`.
    fn tracks_holding(&self, item: ItemId) -> BTreeSet<String> {
        let q = self.harness.state().session.active_sequence().unwrap();
        let named = |letter: char, tracks: &[filmcraft_project::Track]| {
            tracks
                .iter()
                .enumerate()
                .filter(|(_, t)| t.items.iter().any(|c| c.item == item))
                .map(move |(k, _)| format!("{letter}{}", k + 1))
                .collect::<Vec<_>>()
        };
        named('V', &q.video_tracks).into_iter().chain(named('A', &q.audio_tracks)).collect()
    }
}

fn previews(tracks: &[&str]) -> BTreeSet<String> {
    tracks.iter().map(|t| format!("timeline.dropPreview.{t}")).collect()
}

#[test]
fn a_clip_with_picture_and_sound_previews_its_video_and_audio_tracks() {
    for (hover, expected) in [("V2", ["V2", "A2"]), ("A1", ["V1", "A1"])] {
        let mut d = Driver::demo();
        let (item, from) = d.footage_item(true, true).expect("the demo has a clip with picture and sound");
        let shown = d.drag_to_track(from, hover);
        assert_eq!(shown, previews(&expected), "hovering {hover}");
        let placed: BTreeSet<String> = d.tracks_holding(item).into_iter().map(|t| format!("timeline.dropPreview.{t}")).collect();
        assert_eq!(placed, shown, "the drop lands on the previewed tracks (hovering {hover})");
    }
}

#[test]
fn a_picture_only_item_over_an_audio_track_previews_the_video_track_it_goes_to() {
    let mut d = Driver::demo();
    let made = d.ok("engine.execute", json!({"command": "file.newColorMatte", "params": {"width": 64, "height": 36, "color": "#ff0000"}}));
    let item = ItemId(made["item"].as_u64().expect("a color matte"));
    assert_eq!(d.harness.state().session.project.source_streams(item), (true, false));
    d.ok("ui.panel.show", json!({"panel": "Project"}));
    d.ok("engine.execute", json!({"command": "project.view.set", "params": {"view": "list"}}));
    d.frames(4);
    let from = d.rect(&format!("project.item.{}", item.0)).center();
    let shown = d.drag_to_track(from, "A2");
    assert_eq!(shown, previews(&["V2"]));
    assert_eq!(d.tracks_holding(item), ["V2".to_string()].into_iter().collect());
}
