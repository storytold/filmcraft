//! Headless UI tests for Escape key drag cancellation in the Timeline (#580).

use crate::support::{Drive, Launch, driver};
use egui::pos2;
use filmcraft_project::ClipId;
use filmcraft_ui_egui::FilmcraftApp;
use filmcraft_ui_egui::panels::timeline::Drag;
use serde_json::{Value, json};

const DUNES: &str = "Desert_Dunes.mp4";

driver!(Driver);

impl Driver {
    fn app(&self) -> &FilmcraftApp {
        self.harness.state()
    }

    fn send(&mut self, e: egui::Event) {
        self.harness.input_mut().events.push(e);
        self.frames(1);
    }

    fn escape(&mut self) {
        self.send(egui::Event::Key { key: egui::Key::Escape, physical_key: None, pressed: true, repeat: false, modifiers: Default::default() });
    }

    fn rect(&mut self, id: &str) -> [f64; 4] {
        self.expect_rect(id)
    }

    fn v1_clip(&self, name: &str) -> u64 {
        let seq = self.app().session.active_sequence().expect("sequence");
        seq.video_tracks[0].items.iter().find(|i| i.name == name).unwrap_or_else(|| panic!("no {name} on V1")).id.0
    }

    fn span(&self, clip: u64) -> (i64, i64) {
        let seq = self.app().session.active_sequence().expect("sequence");
        let (_, it) = seq.find_item(ClipId(clip)).expect("clip");
        (it.start.0, it.end().0)
    }

    fn edges(&mut self, clip: u64) -> (f32, f32, f32) {
        let i = self.ok("ui.timeline.locate", json!({"clip": clip, "edge": "in"}));
        let o = self.ok("ui.timeline.locate", json!({"clip": clip, "edge": "out"}));
        let f = |v: &Value, k: &str| v[k].as_f64().expect("number") as f32;
        let id = format!("timeline.clip.{clip}");
        let els = self.ok("ui.elements", json!({"prefix": id}));
        let el = els.as_array().expect("elements").iter().find(|e| e["id"] == id.as_str()).unwrap_or_else(|| panic!("no {id}: {els}")).clone();
        let top = el["rect"][1].as_f64().expect("rect") as f32;
        (f(&i, "x") - 2.0, f(&o, "x") + 2.0, top + 8.0)
    }
}

#[test]
fn escape_cancels_clip_move_drag() {
    let mut d = Driver::demo_snap_off();
    let q = d.app().session.active_sequence().unwrap().clone();
    let v2 = q.video_tracks[1].items[0].clone();
    let r = d.rect(&format!("timeline.clip.{}", v2.id.0));
    let from = pos2(r[0] as f32 + 10.0, (r[1] + r[3] / 2.0) as f32);
    let to = pos2(from.x + 120.0, from.y);

    d.send(egui::Event::PointerMoved(from));
    d.send(egui::Event::PointerButton { pos: from, button: egui::PointerButton::Primary, pressed: true, modifiers: Default::default() });
    for i in 1..=10 {
        d.send(egui::Event::PointerMoved(from.lerp(to, i as f32 / 10.0)));
    }
    assert!(matches!(d.app().tl.drag, Some(Drag::Move { .. })), "drag is active");

    d.escape();
    assert!(d.app().tl.drag.is_none(), "Escape cleared the drag state");

    d.send(egui::Event::PointerButton { pos: to, button: egui::PointerButton::Primary, pressed: false, modifiers: Default::default() });
    d.frames(2);

    let q_after = d.app().session.active_sequence().unwrap().clone();
    let item_after = q_after.find_item(v2.id).unwrap().1;
    assert_eq!(item_after.start, v2.start, "clip position is unchanged");
    assert_eq!(item_after.duration, v2.duration, "clip duration is unchanged");
}

#[test]
fn escape_cancels_clip_trim_drag() {
    let mut d = Driver::demo_snap_off();
    let clip = d.v1_clip(DUNES);
    let (s0, e0) = d.span(clip);
    let (_, x_out, y) = d.edges(clip);
    let from = pos2(x_out, y);
    let to = pos2(from.x - 40.0, from.y);

    d.send(egui::Event::PointerMoved(from));
    d.send(egui::Event::PointerButton { pos: from, button: egui::PointerButton::Primary, pressed: true, modifiers: Default::default() });
    for i in 1..=10 {
        d.send(egui::Event::PointerMoved(from.lerp(to, i as f32 / 10.0)));
    }
    assert!(matches!(d.app().tl.drag, Some(Drag::Trim { .. })), "trim drag is active");

    d.escape();
    assert!(d.app().tl.drag.is_none(), "Escape cleared the trim drag");

    d.send(egui::Event::PointerButton { pos: to, button: egui::PointerButton::Primary, pressed: false, modifiers: Default::default() });
    d.frames(2);

    assert_eq!(d.span(clip), (s0, e0), "clip bounds are unchanged after cancel");
}

#[test]
fn escape_cancels_ruler_scrub_and_restores_playhead() {
    let mut d = Driver::demo_snap_off();
    let initial_playhead = d.app().session.playhead();
    let layout = d.app().tl.layout.as_ref().expect("layout").clone();
    let from = pos2(layout.x_of(initial_playhead), layout.ruler.center().y);
    let to = pos2(from.x + 100.0, from.y);

    d.send(egui::Event::PointerMoved(from));
    d.send(egui::Event::PointerButton { pos: from, button: egui::PointerButton::Primary, pressed: true, modifiers: Default::default() });
    for i in 1..=10 {
        d.send(egui::Event::PointerMoved(from.lerp(to, i as f32 / 10.0)));
    }
    assert!(matches!(d.app().tl.drag, Some(Drag::Scrub { .. })), "ruler scrub is active");
    assert_ne!(d.app().session.playhead(), initial_playhead, "playhead scrubbed to a new position");

    d.escape();
    assert!(d.app().tl.drag.is_none(), "Escape cleared the scrub state");
    assert_eq!(d.app().session.playhead(), initial_playhead, "Escape restored the playhead to initial time");

    d.send(egui::Event::PointerButton { pos: to, button: egui::PointerButton::Primary, pressed: false, modifiers: Default::default() });
    d.frames(2);

    assert_eq!(d.app().session.playhead(), initial_playhead, "playhead remains at initial time after release");
}

#[test]
fn escape_cancels_marquee_selection() {
    let mut d = Driver::demo_snap_off();
    let q = d.app().session.active_sequence().unwrap().clone();
    let v2 = q.video_tracks[1].items[0].clone();
    d.exec("timeline.select", json!({"clips": [v2.id.0]}));
    assert_eq!(d.app().session.state.selection, vec![v2.id]);

    let layout = d.app().tl.layout.as_ref().expect("layout").clone();
    let from = pos2(layout.content.min.x + 50.0, layout.content.max.y - 10.0);
    let to = pos2(from.x + 200.0, from.y - 50.0);

    d.send(egui::Event::PointerMoved(from));
    d.send(egui::Event::PointerButton { pos: from, button: egui::PointerButton::Primary, pressed: true, modifiers: Default::default() });
    for i in 1..=10 {
        d.send(egui::Event::PointerMoved(from.lerp(to, i as f32 / 10.0)));
    }
    assert!(matches!(d.app().tl.drag, Some(Drag::Marquee { .. })), "marquee is active");

    d.escape();
    assert!(d.app().tl.drag.is_none(), "Escape cleared the marquee");

    d.send(egui::Event::PointerButton { pos: to, button: egui::PointerButton::Primary, pressed: false, modifiers: Default::default() });
    d.frames(2);

    assert_eq!(d.app().session.state.selection, vec![v2.id], "original selection is preserved");
}
