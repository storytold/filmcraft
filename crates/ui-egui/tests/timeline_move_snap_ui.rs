//! Moving clips snaps whichever edge of the moved clips is nearest a snap point (#374), not only
//! the outer edges of the moved block.

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
    fn exec(&mut self, command: &str, params: Value) -> Value {
        self.ok("engine.execute", json!({"command": command, "params": params}))
    }
    fn rect(&mut self, id: &str) -> [f64; 4] {
        let v = self.ok("ui.elements", json!({"prefix": id}));
        let e = v.as_array().unwrap().iter().find(|e| e["id"] == id).unwrap_or_else(|| panic!("no element {id}")).clone();
        let r: Vec<f64> = e["rect"].as_array().unwrap().iter().map(|x| x.as_f64().unwrap()).collect();
        [r[0], r[1], r[2], r[3]]
    }
}

#[test]
fn a_linked_partners_inner_edge_snaps_while_moving() {
    let mut d = Driver::demo();
    d.exec("file.newSequence", json!({"name": "Snap", "video": 1, "audio": 1}));
    let session = &d.harness.state().session;
    let item = session.project.items.values().find(|i| i.has_video() && i.has_audio() && i.as_media().is_some()).unwrap().id;
    let rate = session.sequence_rate();
    d.exec("timeline.place", json!({"item": item.0, "frame": 0, "duration": rate.tick_of(48).0}));
    let q = d.harness.state().session.active_sequence().unwrap().clone();
    let (video, audio) = (q.video_tracks[0].items[0].clone(), q.audio_tracks[0].items[0].clone());
    // the audio starts 12 frames after its video (trimmed on its own)
    d.harness.state_mut().session.state.linked_selection = false;
    d.exec("timeline.trim", json!({"clip": audio.id.0, "edge": "in", "deltaFrames": 12}));
    d.harness.state_mut().session.state.linked_selection = true;
    d.exec("playhead.set", json!({"frame": 100}));
    d.ok("ui.set", json!({"timeline": {"pps": 48.0, "scroll": 0.0}}));
    d.frames(3);
    // drag the video 90 frames: its audio's start lands 2 frames (4 px) past the playhead, while
    // the block's outer edges (frames 90 and 138) are far from any snap point
    let r = d.rect(&format!("timeline.clip.{}", video.id.0));
    let (x, y) = (r[0] + r[2] * 0.5, r[1] + r[3] * 0.5);
    d.ok("ui.drag", json!({"from": {"x": x, "y": y}, "to": {"x": x + 90.0 * 2.0, "y": y}, "steps": 12}));
    d.frames(3);
    let q = d.harness.state().session.active_sequence().unwrap().clone();
    let a = q.find_item(audio.id).unwrap().1.clone();
    let v = q.find_item(video.id).unwrap().1.clone();
    assert_eq!(rate.frame_at(a.start), 100, "the audio's start snapped to the playhead (video at {})", rate.frame_at(v.start));
    assert_eq!(a.start - v.start, rate.tick_of(12), "the pair moved together");
}

/// With two edges in reach of different snap points, the nearer one decides: the inner audio start
/// lands exactly on the playhead while the block's start is a frame (4 px) from a marker. Before,
/// the block's start won whenever it was in reach.
#[test]
fn the_nearest_of_several_edges_in_reach_wins() {
    let mut d = Driver::demo();
    d.exec("file.newSequence", json!({"name": "Snap", "video": 1, "audio": 1}));
    let session = &d.harness.state().session;
    let item = session.project.items.values().find(|i| i.has_video() && i.has_audio() && i.as_media().is_some()).unwrap().id;
    let rate = session.sequence_rate();
    d.exec("timeline.place", json!({"item": item.0, "frame": 0, "duration": rate.tick_of(48).0}));
    let q = d.harness.state().session.active_sequence().unwrap().clone();
    let (video, audio) = (q.video_tracks[0].items[0].clone(), q.audio_tracks[0].items[0].clone());
    d.harness.state_mut().session.state.linked_selection = false;
    d.exec("timeline.trim", json!({"clip": audio.id.0, "edge": "in", "deltaFrames": 12}));
    d.harness.state_mut().session.state.linked_selection = true;
    // a marker one frame past where the video's start lands, the playhead exactly where the audio's does
    d.exec("markers.add", json!({"target": "program", "time": rate.tick_of(91).0}));
    d.exec("playhead.set", json!({"frame": 102}));
    d.ok("ui.set", json!({"timeline": {"pps": 96.0, "scroll": 0.0}}));
    d.frames(3);
    let r = d.rect(&format!("timeline.clip.{}", video.id.0));
    let (x, y) = (r[0] + r[2] * 0.5, r[1] + r[3] * 0.5);
    d.ok("ui.drag", json!({"from": {"x": x, "y": y}, "to": {"x": x + 90.0 * 4.0, "y": y}, "steps": 12}));
    d.frames(3);
    let q = d.harness.state().session.active_sequence().unwrap().clone();
    let (a, v) = (q.find_item(audio.id).unwrap().1.clone(), q.find_item(video.id).unwrap().1.clone());
    assert_eq!((rate.frame_at(a.start), rate.frame_at(v.start)), (102, 90), "the audio's start (0 px) beat the video's start (4 px)");
}
