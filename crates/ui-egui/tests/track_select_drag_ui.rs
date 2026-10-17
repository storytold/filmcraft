//! The Track Select Forward tool (A) selects every clip from the pointer on, and a drag with it
//! moves them all (Premiere); before, the drag only selected and nothing moved. What it selected
//! also moves with a Selection-tool drag afterwards.

use std::collections::HashMap;
use std::sync::mpsc::{Sender, channel};

use egui_kittest::Harness;
use filmcraft_engine::Session;
use filmcraft_time::Tick;
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
    fn rect(&mut self, id: &str) -> [f64; 4] {
        let v = self.ok("ui.elements", json!({"prefix": id}));
        let e = v.as_array().unwrap().iter().find(|e| e["id"] == id).unwrap_or_else(|| panic!("no element {id}")).clone();
        let r: Vec<f64> = e["rect"].as_array().unwrap().iter().map(|x| x.as_f64().unwrap()).collect();
        [r[0], r[1], r[2], r[3]]
    }
    /// Every clip's id → (start, end).
    fn spans(&mut self) -> HashMap<u64, (Tick, Tick)> {
        let q = self.harness.state().session.active_sequence().unwrap();
        q.all_tracks().flat_map(|t| t.items.iter()).map(|i| (i.id.0, (i.start, i.end()))).collect()
    }
}

/// The middle of the second clip on V1 (its ends carry transitions), and that clip's start: the clips before it stay put.
fn second_v1_clip(d: &mut Driver) -> (f64, f64, Tick) {
    let q = d.harness.state().session.active_sequence().unwrap().clone();
    let c = q.video_tracks[0].items.get(1).expect("a second clip on V1").clone();
    let r = d.rect(&format!("timeline.clip.{}", c.id.0));
    (r[0] + r[2] / 2.0, r[1] + r[3] / 2.0, c.start)
}

/// Every clip that starts at or after `from` moved by the same positive offset; every clip that
/// ends at or before it stayed.
fn assert_moved_from(before: &HashMap<u64, (Tick, Tick)>, after: &HashMap<u64, (Tick, Tick)>, from: Tick) {
    let mut offset = None;
    for (id, &(s, e)) in before {
        let (s2, _) = after[id];
        if s >= from {
            let o = s2 - s;
            assert!(o > Tick::ZERO, "clip {id} at or after the click moved right");
            assert_eq!(*offset.get_or_insert(o), o, "clip {id} moved with the rest");
        } else if e <= from {
            assert_eq!(s2, s, "clip {id} before the click stayed");
        }
    }
    assert!(offset.is_some(), "there were clips after the click");
}

#[test]
fn a_drag_with_track_select_forward_moves_every_clip_after_the_pointer() {
    let mut d = Driver::demo();
    let (x, y, from) = second_v1_clip(&mut d);
    let before = d.spans();
    d.ok("ui.set", json!({"tool": "TrackSelectForward"}));
    d.ok("ui.drag", json!({"from": {"x": x, "y": y}, "to": {"x": x + 240.0, "y": y}, "steps": 12}));
    d.frames(3);
    let after = d.spans();
    assert_moved_from(&before, &after, from);
}

#[test]
fn clips_selected_with_track_select_forward_move_with_the_selection_tool() {
    let mut d = Driver::demo();
    let (x, y, from) = second_v1_clip(&mut d);
    let before = d.spans();
    d.ok("ui.set", json!({"tool": "TrackSelectForward"}));
    d.ok("ui.click", json!({"x": x, "y": y}));
    d.frames(2);
    assert!(d.harness.state().session.state.selection.len() > 1, "the click selected the clips after it");
    d.ok("ui.set", json!({"tool": "Selection"}));
    d.ok("ui.drag", json!({"from": {"x": x, "y": y}, "to": {"x": x + 240.0, "y": y}, "steps": 12}));
    d.frames(3);
    let after = d.spans();
    assert_moved_from(&before, &after, from);
}

#[test]
fn track_select_forward_skips_a_locked_music_track_and_still_moves_the_rest() {
    let mut d = Driver::demo();
    let (x, y, from) = second_v1_clip(&mut d);
    let q = d.harness.state().session.active_sequence().unwrap().clone();
    // the music on A2 runs under the whole edit; locking it keeps it in place (Premiere)
    let music: Vec<u64> = q.audio_tracks[1].items.iter().map(|i| i.id.0).collect();
    assert!(!music.is_empty(), "the demo project has music on A2");
    d.ok("timeline.setTrack", json!({"track": "A2", "locked": true}));
    let before = d.spans();
    d.ok("ui.set", json!({"tool": "TrackSelectForward"}));
    d.ok("ui.drag", json!({"from": {"x": x, "y": y}, "to": {"x": x + 240.0, "y": y}, "steps": 12}));
    d.frames(3);
    let after = d.spans();
    for id in &music {
        assert_eq!(before[id], after[id], "the music on the locked track stayed");
    }
    assert_moved_from(&before, &after, from);
}
