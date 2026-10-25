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
        let push = |d: &mut Self, e: egui::Event| d.harness.input_mut().events.push(e);
        push(self, egui::Event::PointerMoved(from));
        self.frames(1);
        push(self, egui::Event::PointerButton { pos: from, button: egui::PointerButton::Primary, pressed: true, modifiers: Default::default() });
        self.frames(1);
        for i in 1..=steps {
            push(self, egui::Event::PointerMoved(from + (to - from) * (i as f32 / steps as f32)));
            self.frames(1);
        }
        push(self, egui::Event::PointerButton { pos: to, button: egui::PointerButton::Primary, pressed: false, modifiers: Default::default() });
        self.frames(2);
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
    let (x0, x1, y) = d.edges(city);
    let from = pos2((x0 + x1) / 2.0, y + 10.0);
    d.drag(from, pos2(from.x, from.y - 34.0), 12);
    let (video1, partner1) = tracks_of(&d, city);
    assert_ne!(video1, video0, "the video clip went up a track");
    let (audio_track1, audio_clip1) = partner1.expect("still linked");
    assert_eq!(audio_clip1, audio_clip);
    assert_eq!(audio_track1, audio_track0, "its linked audio stays on its own track");
    assert_eq!(d.span(city).0, start0, "a vertical drag leaves the time alone");
}

#[test]
fn moving_a_linked_audio_clip_down_a_track_leaves_its_video_on_its_track() {
    let mut d = Driver::demo();
    d.exec("sequence.addTracks", json!({"video": 2, "audio": 2}));
    d.frames(4);
    let city = d.v1_clip(CITY);
    let (video0, partner0) = tracks_of(&d, city);
    let (_, audio_clip) = partner0.expect("the clip has linked audio");
    let (ax0, ax1, ay) = d.edges(audio_clip);
    let from = pos2((ax0 + ax1) / 2.0, ay + 10.0);
    d.drag(from, pos2(from.x, from.y + 60.0), 12);
    let (video1, partner1) = tracks_of(&d, city);
    assert_eq!(video1, video0, "the video clip stays");
    assert!(partner1.is_some_and(|(t, _)| Some(t) != partner0.map(|(t, _)| t)), "the audio clip changed track");
}
