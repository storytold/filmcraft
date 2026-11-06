//! Headless UI test of two Timeline keys, as in Premiere Pro: D in the empty time between two
//! clips selects the gap (the light block a click gives, a thin line when the gap is one frame
//! zoomed out) and Delete closes it; End parks the playhead flush with the end of the last clip,
//! even when that clip ends between frames. The real `FilmcraftApp` under `egui_kittest`, driven
//! over the control channel. `FILMCRAFT_UI_SHOTS=<dir>` saves a screenshot of each step.

use std::sync::mpsc::{Sender, channel};

use egui_kittest::Harness;
use filmcraft_engine::Session;
use filmcraft_engine::project::ClipId;
use filmcraft_ui_egui::FilmcraftApp;
use filmcraft_ui_egui::control::ControlRequest;
use serde_json::{Value, json};

struct Driver {
    harness: Harness<'static, FilmcraftApp>,
    tx: Sender<ControlRequest>,
}

impl Driver {
    /// A sequence with two 48-frame clips on V1 (and their sounds on A1) starting at `frames`.
    fn two_clips(frames: [i64; 2]) -> (Self, ClipId, ClipId) {
        let mut s = Session::default();
        s.execute("file.openDemoProject", json!({})).unwrap();
        s.execute("file.newSequence", json!({"name": "Keys", "video": 1, "audio": 1})).unwrap();
        let rate = s.sequence_rate();
        let item = s.project.items.values().find(|i| i.has_video() && i.has_audio() && i.as_media().is_some()).map(|i| i.id).unwrap();
        for f in frames {
            s.execute("timeline.place", json!({"item": item.0, "frame": f, "sourceIn": rate.tick_of(48).0, "duration": rate.tick_of(48).0})).unwrap();
        }
        s.execute("edit.deselectAll", json!({})).unwrap();
        let v1 = &s.active_sequence().unwrap().video_tracks[0];
        let (a, b) = (v1.items[0].id, v1.items[1].id);
        let (tx, rx) = channel();
        let app = FilmcraftApp::new(s).with_control(rx);
        let mut builder = Harness::builder().with_size(egui::vec2(1600.0, 980.0)).with_max_steps(10_000);
        if std::env::var_os("FILMCRAFT_UI_SHOTS").is_some() {
            builder = builder.wgpu();
        }
        let harness = builder.build_eframe(move |_cc| app);
        let mut d = Driver { harness, tx };
        d.frames(4);
        (d, a, b)
    }

    fn shot(&mut self, name: &str) {
        let Some(dir) = std::env::var_os("FILMCRAFT_UI_SHOTS").map(std::path::PathBuf::from) else { return };
        self.frames(3);
        std::fs::create_dir_all(&dir).unwrap();
        self.harness.render().unwrap().save(dir.join(format!("{name}.png"))).unwrap();
    }

    fn frames(&mut self, n: usize) {
        for _ in 0..n {
            // the control channel's clicks and keys enter through the input hook
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

    fn exec(&mut self, command: &str, params: Value) -> Value {
        self.ok("engine.execute", json!({"command": command, "params": params}))
    }

    fn key(&mut self, key: &str) {
        self.ok("ui.key", json!({"key": key}));
        self.frames(3);
    }

    /// [x, y, w, h] of an on-screen element.
    fn rect(&mut self, id: &str) -> Option<[f64; 4]> {
        self.frames(2);
        let v = self.ok("ui.elements", json!({"prefix": id}));
        let e = v.as_array().unwrap().iter().find(|e| e["id"] == json!(id))?.clone();
        let r: Vec<f64> = e["rect"].as_array().unwrap().iter().map(|x| x.as_f64().unwrap()).collect();
        Some([r[0], r[1], r[2], r[3]])
    }

    fn start(&mut self, c: ClipId) -> i64 {
        self.harness.state_mut().session.active_sequence().unwrap().find_item(c).map(|(_, i)| i.start.0).unwrap()
    }

    fn end(&mut self, c: ClipId) -> i64 {
        self.harness.state_mut().session.active_sequence().unwrap().find_item(c).map(|(_, i)| i.end().0).unwrap()
    }

    fn frame(&mut self, n: i64) -> i64 {
        self.harness.state_mut().session.sequence_rate().tick_of(n).0
    }

    fn gap(&mut self) -> Value {
        self.exec("sequence.inspect", json!({}))["gapSelection"].clone()
    }

    /// The screen x of the playhead line in the Timeline.
    fn playhead_x(&mut self) -> f64 {
        let tracks = self.rect("timeline.tracks").expect("the Timeline's tracks are shown");
        let app = self.harness.state();
        let ph = app.session.playhead().seconds();
        tracks[0] + (ph - app.ui.timeline.scroll) * app.ui.timeline.pps
    }
}

#[test]
fn d_in_a_gap_selects_it_and_delete_closes_it() {
    let (mut d, a, b) = Driver::two_clips([0, 96]);
    let (f48, f60, f96) = (d.frame(48), d.frame(60), d.frame(96));
    d.exec("playhead.set", json!({"time": f60}));
    d.frames(3);
    d.shot("d-gap-1-playhead-in-gap");
    assert!(d.rect("timeline.gap").is_none(), "nothing selected yet");

    d.key("D");
    d.shot("d-gap-2-selected");
    let g = d.gap();
    assert_eq!((g["start"].as_i64(), g["end"].as_i64()), (Some(f48), Some(f96)), "D selects the gap under the playhead: {g}");
    let block = d.rect("timeline.gap").expect("the selected gap is drawn");
    let (ra, rb) = (d.rect(&format!("timeline.clip.{}", a.0)).unwrap(), d.rect(&format!("timeline.clip.{}", b.0)).unwrap());
    assert!((block[0] - (ra[0] + ra[2])).abs() < 1.5 && (block[0] + block[2] - rb[0]).abs() < 1.5, "the block fills the gap: {block:?} {ra:?} {rb:?}");

    d.key("Delete");
    d.shot("d-gap-3-closed");
    assert_eq!(d.start(b), f48, "Delete closes the gap");
    assert_eq!(d.start(a), 0, "the clip before stays");
    assert_eq!(d.gap(), Value::Null);
    assert!(d.rect("timeline.gap").is_none(), "nothing is drawn any more");
}

#[test]
fn a_one_frame_gap_shows_as_a_thin_line_zoomed_out() {
    let (mut d, a, b) = Driver::two_clips([0, 49]);
    let f48 = d.frame(48);
    // zoomed out so far that one frame is less than a point wide
    d.ok("ui.set", json!({"timeline": {"pps": 10.0, "scroll": 0.0}}));
    d.frames(6);
    d.exec("playhead.set", json!({"time": f48}));
    d.key("D");
    d.shot("d-gap-4-one-frame-zoomed-out");
    assert!(d.gap().is_object(), "D selects the one-frame gap");
    let block = d.rect("timeline.gap").expect("the selected gap is drawn");
    assert!(block[2] >= 3.0, "a one-frame gap is still drawn wide enough to see: {block:?}");
    d.key("Delete");
    assert_eq!(d.start(b), f48);
    assert_eq!(d.start(a), 0);
}

#[test]
fn end_parks_flush_with_a_last_clip_that_ends_between_frames() {
    let (mut d, a, b) = Driver::two_clips([0, 96]);
    // move the last clip (and its sound) a third of a frame later: the sequence now ends between frames
    let third = d.harness.state().session.sequence_rate().frame_duration().0 / 3;
    let f96 = d.frame(96);
    let (sound, v1, a1) = {
        let q = d.harness.state().session.active_sequence().unwrap();
        (q.audio_tracks[0].items[1].id, q.video_tracks[0].id, q.audio_tracks[0].id)
    };
    d.exec("timeline.move", json!({"moves": [{"clip": b.0, "track": v1.0, "time": f96 + third}, {"clip": sound.0, "track": a1.0, "time": f96 + third}]}));
    let end = d.end(b);
    // zoomed in so a frame is 25 points wide: a playhead a third of a frame short shows 8 points
    // of clip after it (and the 5% margin End scrolls to still shows the clip's end)
    d.ok("ui.set", json!({"timeline": {"pps": 600.0, "scroll": 0.0}}));
    d.frames(6);

    d.key("End");
    d.shot("end-1-flush-with-last-clip");
    let ph = d.harness.state().session.playhead().0;
    assert!(ph >= end, "End parks at or after the last clip's end: playhead {ph}, end {end}");
    let clip = d.rect(&format!("timeline.clip.{}", b.0)).expect("the last clip is in view after End");
    let x = d.playhead_x();
    assert!(x >= clip[0] + clip[2] - 0.5, "no part of the clip shows after the playhead: playhead x {x}, clip right {}", clip[0] + clip[2]);
    assert!(x - (clip[0] + clip[2]) < 26.0, "and the playhead is within a frame of it");
    assert_eq!(d.start(a), 0);
}
