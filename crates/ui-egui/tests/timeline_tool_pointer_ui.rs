//! Headless UI tests of the editing tools' pointers in the Timeline: the Razor, Ripple, Rolling,
//! Rate Stretch, Slip, Slide and Track Select tools show their own icon as the pointer (the system
//! pointer hidden) wherever they act in the tracks, and the Razor's preview line crosses the clip
//! and the clips linked to it, or every track with Shift.

use std::sync::mpsc::{Sender, channel};

use egui::{CursorIcon, Pos2, pos2};
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

    fn app(&self) -> &FilmcraftApp {
        self.harness.state()
    }

    /// Move the pointer to `at` with no button down, and let the Timeline react.
    fn hover(&mut self, at: Pos2) {
        self.harness.input_mut().events.push(egui::Event::PointerMoved(at));
        self.frames(2);
    }

    /// The cursor the last frame asked for.
    fn cursor(&self) -> CursorIcon {
        self.harness.output().platform_output.cursor_icon
    }
}

impl Driver {
    /// The clip named `name` on V1.
    fn v1_clip(&self, name: &str) -> u64 {
        let seq = self.app().session.active_sequence().expect("sequence");
        seq.video_tracks[0].items.iter().find(|i| i.name == name).unwrap_or_else(|| panic!("no {name} on V1")).id.0
    }
}

fn clip_center(d: &mut Driver, clip: u64) -> Pos2 {
    let id = format!("timeline.clip.{clip}");
    let els = d.ok("ui.elements", json!({"prefix": id}));
    let el = els.as_array().expect("elements").iter().find(|e| e["id"] == id.as_str()).unwrap_or_else(|| panic!("no {id}")).clone();
    let r: Vec<f32> = el["rect"].as_array().unwrap().iter().map(|x| x.as_f64().unwrap() as f32).collect();
    pos2(r[0] + r[2] / 2.0, r[1] + r[3] / 2.0)
}

#[test]
fn the_editing_tools_draw_their_own_pointer_over_the_tracks() {
    let mut d = Driver::demo();
    let city = d.v1_clip(CITY);
    let at = clip_center(&mut d, city);
    for tool in ["Razor", "Ripple", "Rolling", "RateStretch", "Slip", "Slide", "TrackSelectForward", "TrackSelectBackward", "Remix"] {
        d.ok("ui.set", json!({"tool": tool}));
        d.frames(2);
        d.hover(at);
        assert_eq!(d.cursor(), CursorIcon::None, "{tool}: its own icon is the pointer, the system one hidden");
    }
    // the tools that have a system pointer keep it, and so does the Selection tool
    for (tool, icon) in [("Selection", CursorIcon::Default), ("Hand", CursorIcon::Grab), ("Zoom", CursorIcon::ZoomIn)] {
        d.ok("ui.set", json!({"tool": tool}));
        d.frames(2);
        d.hover(at);
        assert_eq!(d.cursor(), icon, "{tool}");
    }
}

#[test]
fn the_razor_line_crosses_the_linked_clips_or_every_track_with_shift() {
    let d = Driver::demo();
    let city = d.v1_clip(CITY);
    let (video, link) = {
        let seq = d.app().session.active_sequence().unwrap();
        let (track, item) = seq.find_item(ClipId(city)).unwrap();
        (track, item.link)
    };
    let partner = {
        let seq = d.app().session.active_sequence().unwrap();
        seq.audio_tracks.iter().find(|t| t.items.iter().any(|i| i.link == link && link.is_some())).map(|t| t.id)
    };
    let linked = filmcraft_ui_egui::panels::timeline::razor_line_tracks(d.app(), ClipId(city), false);
    assert!(linked.contains(&video), "the clip's own track");
    if let Some(audio) = partner {
        assert!(linked.contains(&audio), "the linked audio's track");
    }
    let every = filmcraft_ui_egui::panels::timeline::razor_line_tracks(d.app(), ClipId(city), true);
    let seq = d.app().session.active_sequence().unwrap();
    assert_eq!(every.len(), seq.video_tracks.len() + seq.audio_tracks.len(), "Shift: every track");
    assert!(every.len() > linked.len());
}
