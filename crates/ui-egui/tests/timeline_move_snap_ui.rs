//! Headless UI tests of snapping while a clip is dragged in the Timeline (#1893): moved to another
//! track, a clip keeps its own time when that is within snapping reach, ahead of a nearby edit, as
//! in Premiere; dragged along its own track it still snaps to the edit. The real `FilmcraftApp`
//! under `egui_kittest` with the demo project, snapping on.

use std::sync::mpsc::{Sender, channel};

use egui::{Pos2, pos2};
use egui_kittest::Harness;
use filmcraft_engine::Session;
use filmcraft_project::ClipId;
use filmcraft_time::Tick;
use filmcraft_ui_egui::FilmcraftApp;
use filmcraft_ui_egui::control::ControlRequest;
use filmcraft_ui_egui::panels::timeline::Layout;
use serde_json::{Value, json};

struct Driver {
    harness: Harness<'static, FilmcraftApp>,
    tx: Sender<ControlRequest>,
}

impl Driver {
    fn demo() -> Self {
        let mut session = Session::default();
        session.execute("file.openDemoProject", json!({})).expect("demo project");
        session.execute("sequence.snap", json!({"on": true})).expect("snapping on");
        let (tx, rx) = channel();
        let app = FilmcraftApp::new(session).with_control(rx);
        let harness = Harness::builder().with_size(egui::vec2(1600.0, 980.0)).with_step_dt(1.0 / 60.0).with_max_steps(10_000).build_eframe(move |_cc| app);
        let mut d = Driver { harness, tx };
        d.frames(4);
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

    fn layout(&self) -> Layout {
        self.harness.state().tl.layout.clone().expect("layout")
    }

    fn start(&self, clip: ClipId) -> Tick {
        self.harness.state().session.active_sequence().and_then(|s| s.find_item(clip)).expect("clip").1.start
    }

    fn track_of(&self, clip: ClipId) -> usize {
        let seq = self.harness.state().session.active_sequence().expect("sequence");
        let (tid, _) = seq.find_item(clip).expect("clip");
        seq.video_tracks.iter().position(|t| t.id == tid).expect("a video track")
    }

    /// Press at `from`, move through `path` a few pixels per frame (as a mouse does), release at its end.
    fn drag(&mut self, from: Pos2, path: &[Pos2], steps: usize) {
        let push = |d: &mut Self, e: egui::Event| d.harness.input_mut().events.push(e);
        push(self, egui::Event::PointerMoved(from));
        self.frames(1);
        push(self, egui::Event::PointerButton { pos: from, button: egui::PointerButton::Primary, pressed: true, modifiers: Default::default() });
        self.frames(1);
        let mut at = from;
        for &to in path {
            for i in 1..=steps {
                push(self, egui::Event::PointerMoved(at + (to - at) * (i as f32 / steps as f32)));
                self.frames(1);
            }
            at = to;
        }
        push(self, egui::Event::PointerButton { pos: at, button: egui::PointerButton::Primary, pressed: false, modifiers: Default::default() });
        self.frames(2);
    }

    /// The V2 overlay of the demo, moved to start a few pixels after the first cut on V1 (5 s),
    /// close enough to snap to it. Returns the clip, the cut and the overlay's new start.
    fn overlay_near_cut(&mut self) -> (ClipId, Tick, Tick) {
        let (clip, cut, rate) = {
            let seq = self.harness.state().session.active_sequence().expect("sequence");
            (seq.video_tracks[1].items[0].id, seq.video_tracks[0].items[0].end(), seq.settings.frame_rate)
        };
        let layout = self.layout();
        // the frame closest to 4 px after the cut: inside the snapping reach of 9 px
        let start = rate.snap_nearest(layout.tick_at(layout.x_of(cut) + 4.0));
        assert!(start > cut, "the overlay starts after the cut");
        self.exec("timeline.move", json!({"moves": [{"clip": clip.0, "track": "V2", "time": start.0}], "linked": false}));
        self.frames(2);
        assert_eq!(self.start(clip), start);
        (clip, cut, start)
    }

    /// The middle of `clip`'s bar, away from its trim edges.
    fn body(&self, clip: ClipId) -> Pos2 {
        let layout = self.layout();
        let seq = self.harness.state().session.active_sequence().expect("sequence");
        let (tid, it) = seq.find_item(clip).expect("clip");
        let row = layout.rows.iter().find(|r| r.track == tid).expect("row");
        pos2((layout.x_of(it.start) + layout.x_of(it.end())) / 2.0, row.rect.center().y)
    }
}

#[test]
fn moved_to_another_track_a_clip_keeps_its_time_over_a_nearby_cut() {
    let mut d = Driver::demo();
    let (clip, cut, start) = d.overlay_near_cut();
    let from = d.body(clip);
    let v3 = {
        let seq = d.harness.state().session.active_sequence().expect("sequence");
        let id = seq.video_tracks[2].id;
        d.layout().rows.iter().find(|r| r.track == id).expect("V3 row").rect.center().y
    };
    // up to V3, a pixel towards the cut: both the clip's own time and the cut are in reach
    d.drag(from, &[pos2(from.x - 1.0, v3)], 12);

    assert_eq!(d.track_of(clip), 2, "the overlay moved to V3");
    assert_eq!(d.start(clip), start, "it kept its time instead of snapping to the cut at {cut:?}");
}

#[test]
fn dragged_along_its_track_a_clip_still_snaps_to_a_nearby_cut() {
    let mut d = Driver::demo();
    let (clip, cut, _) = d.overlay_near_cut();
    let from = d.body(clip);
    // out past egui's drag threshold and back, ending a pixel towards the cut
    d.drag(from, &[pos2(from.x + 40.0, from.y), pos2(from.x - 1.0, from.y)], 12);

    assert_eq!(d.track_of(clip), 1, "the overlay stayed on V2");
    assert_eq!(d.start(clip), cut, "it snapped to the cut");
}
