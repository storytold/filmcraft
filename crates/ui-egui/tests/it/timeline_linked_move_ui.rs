//! Headless UI tests of moving a linked clip to another track (#1484): the clip grabbed changes
//! track and its linked partner of the other kind stays on its own track, moving in time with it.
//! The real `FilmcraftApp` under `egui_kittest` with the demo project.

use std::sync::mpsc::{Sender, channel};

use egui::{Pos2, pos2};
use egui_kittest::Harness;
use filmcraft_engine::Session;
use filmcraft_project::ClipId;
use filmcraft_ui_egui::FilmcraftApp;
use filmcraft_ui_egui::control::ControlRequest;
use serde_json::{Value, json};

/// A clip of the demo project on V1, linked to its audio.
const CITY: &str = "City_Night_Drive.mp4";

struct Driver {
    harness: Harness<'static, FilmcraftApp>,
    tx: Sender<ControlRequest>,
}

impl Driver {
    /// The demo project, snapping off so drags land exactly where the pointer goes. Frames are
    /// 1/60 s apart, as on a display: at the harness's default 1/4 s, a press held for four frames
    /// is already a drag by time (egui's 0.8 s click limit), wherever the pointer is.
    fn demo() -> Self {
        let mut session = Session::default();
        session.execute("file.openDemoProject", json!({})).expect("demo project");
        session.execute("sequence.snap", json!({"on": false})).expect("snapping off");
        let (tx, rx) = channel();
        let app = FilmcraftApp::new(session).with_control(rx);
        let harness = Harness::builder().with_size(egui::vec2(1600.0, 980.0)).with_step_dt(1.0 / 60.0).with_max_steps(10_000).build_eframe(move |_cc| app);
        let mut d = Driver { harness, tx };
        d.frames(4);
        // `ui.elements` answers once the timeline has finished fitting the sequence
        d.ok("ui.elements", json!({"prefix": "timeline.clip."}));
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

    fn app(&self) -> &FilmcraftApp {
        self.harness.state()
    }

    /// Press at `from`, move to `to` over `steps` frames (as a mouse does), release.
    fn drag(&mut self, from: Pos2, to: Pos2, steps: usize) {
        self.drag_with(from, to, steps, egui::Modifiers::NONE);
    }

    /// As [`Self::drag`], with `modifiers` held throughout.
    fn drag_with(&mut self, from: Pos2, to: Pos2, steps: usize, modifiers: egui::Modifiers) {
        let push = |d: &mut Self, e: egui::Event| d.harness.input_mut().events.push(e);
        push(self, egui::Event::ModifiersChanged(modifiers));
        push(self, egui::Event::PointerMoved(from));
        self.frames(1);
        push(self, egui::Event::PointerButton { pos: from, button: egui::PointerButton::Primary, pressed: true, modifiers });
        self.frames(1);
        for i in 1..=steps {
            push(self, egui::Event::PointerMoved(from + (to - from) * (i as f32 / steps as f32)));
            self.frames(1);
        }
        push(self, egui::Event::PointerButton { pos: to, button: egui::PointerButton::Primary, pressed: false, modifiers });
        self.frames(2);
        push(self, egui::Event::ModifiersChanged(egui::Modifiers::NONE));
        self.frames(1);
    }

    /// The clip named `name` on V1.
    fn v1_clip(&self, name: &str) -> u64 {
        let seq = self.app().session.active_sequence().expect("sequence");
        seq.video_tracks[0].items.iter().find(|i| i.name == name).unwrap_or_else(|| panic!("no {name} on V1")).id.0
    }

    /// A clip's (start, end) in ticks.
    fn span(&self, clip: u64) -> (i64, i64) {
        let seq = self.app().session.active_sequence().expect("sequence");
        let (_, it) = seq.find_item(ClipId(clip)).expect("clip");
        (it.start.0, it.end().0)
    }

    /// Screen x of a clip's In and Out edges, and a y 8 px below the top of its track: the band
    /// where edges are grabbed, above the lower part of a track where a transition on a cut takes
    /// the press (the demo has one on the City_Night_Drive / Desert_Dunes cut).
    fn edges(&mut self, clip: u64) -> (f32, f32, f32) {
        // `ui.timeline.locate` answers with points 2 px inside the edges
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

/// (video track, audio partner track and clip) of `clip`'s link.
fn tracks_of(d: &Driver, clip: u64) -> (u64, Option<(u64, u64)>) {
    let s = d.app().session.active_sequence().unwrap();
    let (tr, it) = s.find_item(ClipId(clip)).unwrap();
    let link = it.link;
    let partner =
        s.audio_tracks.iter().flat_map(|t| t.items.iter().map(move |i| (t.id, i))).find(|(_, i)| i.link == link && link.is_some()).map(|(t, i)| (t.0, i.id.0));
    (tr.0, partner)
}

#[test]
fn moving_a_linked_video_clip_up_a_track_leaves_its_audio_on_its_track() {
    let mut d = Driver::demo();
    d.exec("sequence.addTracks", json!({"video": 2, "audio": 2}));
    d.frames(4);
    let city = d.v1_clip(CITY);
    let (video0, partner0) = tracks_of(&d, city);
    let (audio_track0, audio_clip) = partner0.expect("the clip has linked audio");
    let start0 = d.span(city).0;
    let audio_start0 = d.span(audio_clip).0;
    let (x0, x1, y) = d.edges(city);
    let from = pos2((x0 + x1) / 2.0, y + 10.0);
    d.drag(from, pos2(from.x, from.y - 34.0), 12);
    let (video1, partner1) = tracks_of(&d, city);
    let above = d.harness.state().session.active_sequence().unwrap().video_tracks.iter().position(|t| t.id.0 == video0).map(|i| i + 1);
    let above = above.and_then(|i| d.harness.state().session.active_sequence().unwrap().video_tracks.get(i).map(|t| t.id.0));
    assert_eq!(Some(video1), above, "the video clip went up one track");
    let (audio_track1, audio_clip1) = partner1.expect("still linked");
    assert_eq!(audio_clip1, audio_clip);
    assert_eq!(audio_track1, audio_track0, "its linked audio stays on its own track");
    assert_eq!(d.span(city).0, start0, "a vertical drag leaves the time alone");
    assert_eq!(d.span(audio_clip).0, audio_start0, "and the linked audio's too");
}

#[test]
fn moving_a_linked_audio_clip_down_a_track_leaves_its_video_on_its_track() {
    let mut d = Driver::demo();
    d.exec("sequence.addTracks", json!({"video": 2, "audio": 2}));
    d.frames(4);
    let city = d.v1_clip(CITY);
    let (video0, partner0) = tracks_of(&d, city);
    let (_, audio_clip) = partner0.expect("the clip has linked audio");
    let video_start0 = d.span(city).0;
    let audio_start0 = d.span(audio_clip).0;
    let (ax0, ax1, ay) = d.edges(audio_clip);
    let from = pos2((ax0 + ax1) / 2.0, ay + 10.0);
    d.drag(from, pos2(from.x, from.y + 60.0), 12);
    let (video1, partner1) = tracks_of(&d, city);
    assert_eq!(video1, video0, "the video clip stays");
    let audio = &d.harness.state().session.active_sequence().unwrap().audio_tracks;
    let below = partner0.and_then(|(t, _)| audio.iter().position(|a| a.id.0 == t)).and_then(|i| audio.get(i + 1)).map(|a| a.id.0);
    assert_eq!(partner1.map(|(t, _)| t), below, "the audio clip went down one track");
    assert_eq!(d.span(city).0, video_start0, "a vertical drag leaves the video's time alone");
    assert_eq!(d.span(audio_clip).0, audio_start0, "and the audio's");
}

/// Option-dragging a linked pair up a track copies the grabbed clip there; the audio partner would
/// land on itself, so it is not copied, and the original pair stays linked.
#[test]
fn an_option_drag_up_a_track_copies_the_clip_and_leaves_the_original_pair_alone() {
    let mut d = Driver::demo();
    d.exec("sequence.addTracks", json!({"video": 2, "audio": 2}));
    d.frames(4);
    let city = d.v1_clip(CITY);
    let (video0, partner0) = tracks_of(&d, city);
    let (audio_track0, audio_clip) = partner0.expect("the clip has linked audio");
    let link0 = d.app().session.active_sequence().unwrap().find_item(ClipId(city)).unwrap().1.link;
    let count = |d: &Driver| d.app().session.active_sequence().unwrap().all_tracks().map(|t| t.items.len()).sum::<usize>();
    let before = count(&d);
    // Option at the press grabs one side of a link, unless the pair is already selected
    d.exec("timeline.select", json!({"clips": [city], "linked": true}));
    let audio_span0 = d.span(audio_clip);
    let (x0, x1, y) = d.edges(city);
    let from = pos2((x0 + x1) / 2.0, y + 10.0);
    d.drag_with(from, pos2(from.x, from.y - 34.0), 12, egui::Modifiers::ALT);
    assert_eq!(count(&d), before + 1, "one clip was copied: the video clip");
    let (video1, partner1) = tracks_of(&d, city);
    assert_eq!(video1, video0, "the original video clip stays");
    let (audio_track1, audio_clip1) = partner1.expect("the original pair is still linked");
    assert_eq!((audio_track1, audio_clip1), (audio_track0, audio_clip), "the original audio clip is still there on its track");
    assert_eq!(d.span(audio_clip), audio_span0, "and was not overwritten");
    let seq = d.app().session.active_sequence().unwrap();
    assert_eq!(seq.find_item(ClipId(audio_clip)).unwrap().1.link, link0, "with its link");
    assert!(seq.video_tracks[1].items.iter().any(|i| i.name == CITY), "the copy is on the track above");
}

/// An Option-drag to another track and along in time keeps the partner on its own track: a copy
/// that overlaps the original would overwrite it, so it is not made, and one clear of it is.
#[test]
fn an_option_drag_across_tracks_and_time_copies_the_partner_only_clear_of_the_original() {
    let mut d = Driver::demo();
    d.exec("sequence.addTracks", json!({"video": 2, "audio": 2}));
    d.frames(4);
    let city = d.v1_clip(CITY);
    let (_, partner0) = tracks_of(&d, city);
    let (audio_track0, audio_clip) = partner0.expect("the clip has linked audio");
    d.exec("timeline.select", json!({"clips": [city], "linked": true}));
    let audio_span0 = d.span(audio_clip);
    let on_audio_track = |d: &Driver| {
        let seq = d.app().session.active_sequence().unwrap();
        seq.audio_tracks.iter().find(|t| t.id.0 == audio_track0).unwrap().items.iter().map(|i| i.start.0).collect::<Vec<_>>()
    };
    let items0 = on_audio_track(&d);
    let (x0, x1, y) = d.edges(city);
    let from = pos2((x0 + x1) / 2.0, y + 10.0);
    d.drag_with(from, pos2(from.x + (x1 - x0) / 4.0, from.y - 34.0), 12, egui::Modifiers::ALT);
    assert_eq!(d.span(audio_clip), audio_span0, "the original audio clip is not overwritten");
    assert_eq!(on_audio_track(&d), items0, "and no copy of it was made");
    let seq = d.app().session.active_sequence().unwrap();
    assert!(seq.video_tracks[1].items.iter().any(|i| i.name == CITY), "the video clip was copied");
    d.exec("edit.undo", json!({}));
    d.frames(2);
    d.exec("timeline.select", json!({"clips": [city], "linked": true}));
    d.drag_with(from, pos2(from.x + (x1 - x0) + 20.0, from.y - 34.0), 12, egui::Modifiers::ALT);
    let seq = d.app().session.active_sequence().unwrap();
    let copy = seq.video_tracks[1].items.iter().find(|i| i.name == CITY).expect("the video clip was copied").start.0;
    assert!(copy >= audio_span0.1, "the copy is clear of the original");
    assert!(on_audio_track(&d).contains(&copy), "and its partner was copied with it");
    assert_eq!(d.span(audio_clip), audio_span0, "the original audio clip is whole");
}

/// An Option-drag along in time on the same tracks copies the whole pair, overlapping or not.
#[test]
fn an_option_drag_in_time_copies_the_partner_too() {
    let mut d = Driver::demo();
    let city = d.v1_clip(CITY);
    let (_, partner0) = tracks_of(&d, city);
    let (audio_track0, audio_clip) = partner0.expect("the clip has linked audio");
    d.exec("timeline.select", json!({"clips": [city], "linked": true}));
    let audio_start = d.span(audio_clip).0;
    let (x0, x1, y) = d.edges(city);
    let from = pos2((x0 + x1) / 2.0, y + 10.0);
    d.drag_with(from, pos2(from.x + (x1 - x0) / 4.0, from.y), 12, egui::Modifiers::ALT);
    let seq = d.app().session.active_sequence().unwrap();
    let copies = seq
        .audio_tracks
        .iter()
        .find(|t| t.id.0 == audio_track0)
        .unwrap()
        .items
        .iter()
        .filter(|i| i.start.0 > audio_start && i.start.0 < d.span(city).1 + 1)
        .count();
    assert_eq!(copies, 1, "the audio was copied along with the video");
}

/// A Cmd+Option drag to another track inserts the copy: the gap it opens leaves room on the
/// partner's own track, so the copy keeps its audio, and the original pair stays in sync.
#[test]
fn an_insert_copy_up_a_track_keeps_its_partner() {
    let mut d = Driver::demo();
    d.exec("sequence.addTracks", json!({"video": 2, "audio": 2}));
    d.frames(4);
    let city = d.v1_clip(CITY);
    let (_, partner0) = tracks_of(&d, city);
    let (audio_track0, audio_clip) = partner0.expect("the clip has linked audio");
    d.exec("timeline.select", json!({"clips": [city], "linked": true}));
    let (video_start0, audio_start0) = (d.span(city).0, d.span(audio_clip).0);
    let (x0, x1, y) = d.edges(city);
    let from = pos2((x0 + x1) / 2.0, y + 10.0);
    d.drag_with(from, pos2(from.x, from.y - 34.0), 12, egui::Modifiers::ALT | egui::Modifiers::COMMAND);
    let seq = d.app().session.active_sequence().unwrap();
    let copy = seq.video_tracks[1].items.iter().find(|i| i.name == CITY).expect("the video clip was copied");
    assert!(copy.link.is_some(), "the copy is linked");
    let audio = seq.audio_tracks.iter().find(|t| t.id.0 == audio_track0).unwrap();
    let partner = audio.items.iter().find(|i| i.link == copy.link).expect("the copy kept its audio");
    assert_ne!(partner.id.0, audio_clip);
    assert_eq!(partner.start, copy.start, "in sync with it");
    assert_eq!(d.span(city).0 - video_start0, d.span(audio_clip).0 - audio_start0, "the original pair stays in sync");
}
