//! Dragging a Project item over the Timeline previews every track the drop fills (#1887): an item
//! with picture and sound shows a box on its video track and on its audio track, not only on the
//! row under the pointer, and the drop then lands on exactly the previewed tracks.

use std::collections::BTreeSet;

use crate::support::{Drive, builder, demo_session, driver, egui_rect, launch, save_snapshot, start, with_snapshots};
use filmcraft_project::{BinEntry, ItemId};
use serde_json::json;

driver!(Driver);

impl Driver {
    /// The demo project with a new, empty sequence open.
    fn demo() -> Self {
        let mut session = demo_session();
        session.execute("file.newSequence", json!({"name": "Drop preview", "width": 1280, "height": 720, "fps": 24})).expect("sequence");
        session.prefs.timeline.clip_mismatch_warning = false;
        let (harness, tx) = launch(session, with_snapshots(builder()));
        start(Driver { harness, tx })
    }
    fn ids(&mut self, prefix: &str) -> BTreeSet<String> {
        let v = self.ok("ui.elements", json!({"prefix": prefix}));
        v.as_array().unwrap().iter().filter_map(|e| e["id"].as_str().map(str::to_string)).collect()
    }
    fn rect(&mut self, id: &str) -> egui::Rect {
        egui_rect(self.expect_rect(id))
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
        save_snapshot(self, 0, &format!("{name}.png"));
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

/// Review on #2151: with the nest toggle off a sequence edits in as its clips, one track per
/// source track, counted from the track it is dropped on. Dropped on V2 (not V1), the previewed
/// rows are the rows the clips land on.
#[test]
fn a_sequence_dropped_unnested_on_v2_previews_the_tracks_its_clips_land_on() {
    let mut d = Driver::demo();
    d.ok("engine.execute", json!({"command": "sequence.nestSequences", "params": {"on": false}}));
    let (seq, bin) = {
        let s = &d.harness.state().session;
        let p = &s.project;
        let bin = p
            .root
            .children
            .iter()
            .find_map(|e| if let BinEntry::Bin(b) = e { (b.name == "Sequences").then(|| b.clone()) } else { None })
            .expect("the demo's Sequences bin");
        let seq = bin
            .children
            .iter()
            .find_map(|e| match e {
                BinEntry::Item(i) if p.sequence(*i).is_some() && Some(*i) != s.state.active_sequence => Some(*i),
                _ => None,
            })
            .expect("the demo sequence");
        (seq, bin.id)
    };
    d.ok("ui.panel.show", json!({"panel": "Project"}));
    d.ok("engine.execute", json!({"command": "project.view.set", "params": {"view": "list"}}));
    d.ok("ui.menu.invoke", json!({"id": "projectPanel.openBin", "params": {"bin": bin.0, "how": "inPlace"}}));
    d.frames(4);
    let from = d.rect(&format!("project.item.{}", seq.0)).center();
    let shown = d.drag_to_track(from, "V2");
    let filled: BTreeSet<String> = {
        let q = d.harness.state().session.active_sequence().unwrap();
        let named = |letter: char, tracks: &[filmcraft_project::Track]| {
            tracks
                .iter()
                .enumerate()
                .filter(|(_, t)| !t.items.is_empty())
                .map(move |(k, _)| format!("timeline.dropPreview.{letter}{}", k + 1))
                .collect::<Vec<_>>()
        };
        named('V', &q.video_tracks).into_iter().chain(named('A', &q.audio_tracks)).collect()
    };
    assert!(filled.len() > 2, "the demo sequence spans several tracks: {filled:?}");
    assert!(!filled.contains("timeline.dropPreview.V1"), "nothing lands below the drop track: {filled:?}");
    assert_eq!(shown, filled, "the previewed rows are the rows the clips land on");
}
