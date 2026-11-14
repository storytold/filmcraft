//! #3710: with several audio clips selected, dragging the Properties panel's Level changes every
//! one of them by the same amount (each keeps its own level), and the drag is one undo step.

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
        self.tx.send(req).expect("control channel");
        for _ in 0..600 {
            self.frames(1);
            if let Ok(r) = reply.try_recv() {
                assert_eq!(r["ok"], true, "{method} {params}: {r}");
                return r["result"].clone();
            }
        }
        panic!("no reply to {method} {params}");
    }
    fn exec(&mut self, command: &str, params: Value) -> Value {
        self.ok("engine.execute", json!({"command": command, "params": params}))
    }
    /// The first audio track's clips: (id, start, end, Volume level).
    fn audio(&mut self) -> Vec<(u64, i64, i64, f64)> {
        let seq = self.exec("sequence.inspect", json!({}));
        let items = seq["audio"][0]["items"].as_array().cloned().unwrap_or_default();
        items
            .iter()
            .map(|i| {
                let level = i["effects"]
                    .as_array()
                    .and_then(|e| e.iter().find(|e| e["effect"] == "volume"))
                    .and_then(|e| e["params"]["level"]["value"].as_str())
                    .and_then(|v| v.trim_start_matches("Float(").trim_end_matches(')').parse::<f64>().ok())
                    .expect("a Volume level");
                (i["clip"].as_u64().unwrap(), i["start"].as_i64().unwrap(), i["end"].as_i64().unwrap(), level)
            })
            .collect()
    }
    fn element(&mut self, id: &str) -> Option<Value> {
        self.frames(2);
        let v = self.ok("ui.elements", json!({"prefix": id}));
        v.as_array()?.iter().find(|e| e["id"] == id).cloned()
    }
}

#[test]
fn properties_level_changes_every_selected_audio_clip() {
    let mut session = Session::default();
    session.execute("file.openDemoProject", json!({})).expect("demo project");
    let (tx, rx) = channel();
    let app = FilmcraftApp::new(session).with_control(rx);
    let harness = Harness::builder().with_size(egui::vec2(1600.0, 900.0)).with_max_steps(10_000).build_eframe(move |_cc| app);
    let mut d = Driver { harness, tx };
    d.frames(4);
    // three audio clips with different levels
    let (first, start, end, _) = d.audio()[0];
    let third = (end - start) / 3;
    d.exec("timeline.razor", json!({"clip": first, "time": start + third}));
    let second = d.audio()[1].0;
    d.exec("timeline.razor", json!({"clip": second, "time": start + 2 * third}));
    let ids: Vec<u64> = d.audio().iter().take(3).map(|c| c.0).collect();
    for (id, level) in ids.iter().zip([-6.0, 0.0, 3.0]) {
        d.exec("effects.setParam", json!({"clip": id, "effect": "volume", "param": "level", "value": level}));
    }
    let before: Vec<f64> = d.audio().iter().take(3).map(|c| c.3).collect();
    d.exec("timeline.select", json!({"clips": ids}));
    d.ok("ui.panel.show", json!({"panel": "Properties"}));
    d.frames(3);
    let group = d.element("properties.audioGroup").expect("the panel says the Level changes every selected audio clip");
    assert_eq!(group["label"], "3", "{group}");

    // the Audio section sits below the fold: scroll it into view, then drag the Level down
    d.ok("ui.scroll", json!({"id": "panel.Properties", "dy": -2000.0}));
    d.frames(3);
    let level = d.element("properties.volume").expect("the Level is laid out");
    let y = level["rect"][1].as_f64().unwrap();
    assert!(y > 0.0 && y < 880.0, "the Level is on screen: {level}");
    d.ok("ui.drag", json!({"from": {"id": "properties.volume"}, "to": {"id": "properties.volume", "fx": -1.5}, "steps": 12}));
    d.frames(3);
    let after: Vec<f64> = d.audio().iter().take(3).map(|c| c.3).collect();
    let deltas: Vec<f64> = after.iter().zip(&before).map(|(a, b)| a - b).collect();
    assert!(deltas[0] < -0.05, "the drag lowered the Level: {before:?} -> {after:?}");
    for d in &deltas {
        assert!((d - deltas[0]).abs() < 1e-9, "every clip moved by the same amount: {before:?} -> {after:?}");
    }

    // one undo step brings all three back
    d.exec("edit.undo", json!({}));
    let undone: Vec<f64> = d.audio().iter().take(3).map(|c| c.3).collect();
    assert_eq!(undone, before, "the drag is one undo step");

    // the shown clip already at full right: a Pan drag to the right still moves the others
    let pan = |d: &mut Driver, id: u64| -> f64 {
        let seq = d.exec("sequence.inspect", json!({}));
        let it = seq["audio"][0]["items"].as_array().unwrap().iter().find(|i| i["clip"] == id).cloned().unwrap();
        let e = it["effects"].as_array().unwrap().iter().find(|e| e["effect"] == "panner").cloned().unwrap();
        e["params"]["balance"]["value"].as_str().unwrap().trim_start_matches("Float(").trim_end_matches(')').parse().unwrap()
    };
    d.exec("effects.setParam", json!({"clip": ids[0], "effect": "panner", "param": "balance", "value": 100.0}));
    d.exec("timeline.select", json!({"clips": ids}));
    d.frames(3);
    d.element("properties.pan").expect("the Pan is laid out");
    d.ok("ui.drag", json!({"from": {"id": "properties.pan"}, "to": {"id": "properties.pan", "fx": 2.5}, "steps": 12}));
    d.frames(3);
    assert_eq!(pan(&mut d, ids[0]), 100.0, "the shown clip stays at its bound");
    assert!(pan(&mut d, ids[1]) > 1.0 && pan(&mut d, ids[2]) > 1.0, "the others keep moving right");
}

/// Type `text` into the Properties panel's Level and confirm it.
fn type_level(d: &mut Driver, text: &str) {
    d.element("properties.volume").expect("the Level is laid out");
    d.ok("ui.click", json!({"id": "properties.volume"}));
    d.frames(3);
    d.ok("ui.type", json!({"text": text}));
    d.ok("ui.key", json!({"key": "Enter"}));
    d.frames(3);
}

/// The Audio section shows a clip its Level changes: not a selected clip on a locked track (whose
/// value would stay put, so typing the same value again moved the others again), and also with a
/// video selected that has no audio of its own in the selection.
#[test]
fn properties_level_shows_a_clip_it_changes() {
    let mut session = Session::default();
    session.execute("file.openDemoProject", json!({})).expect("demo project");
    let (tx, rx) = channel();
    let app = FilmcraftApp::new(session).with_control(rx);
    let harness = Harness::builder().with_size(egui::vec2(1600.0, 900.0)).with_max_steps(10_000).build_eframe(move |_cc| app);
    let mut d = Driver { harness, tx };
    d.frames(4);
    let (first, start, end, _) = d.audio()[0];
    let third = (end - start) / 3;
    d.exec("timeline.razor", json!({"clip": first, "time": start + third}));
    let second = d.audio()[1].0;
    d.exec("timeline.razor", json!({"clip": second, "time": start + 2 * third}));
    let ids: Vec<u64> = d.audio().iter().take(3).map(|c| c.0).collect();
    for (id, level) in ids.iter().zip([-6.0, 0.0, 3.0]) {
        d.exec("effects.setParam", json!({"clip": id, "effect": "volume", "param": "level", "value": level}));
    }
    let level = |d: &mut Driver, id: u64| -> f64 {
        let seq = d.exec("sequence.inspect", json!({}));
        let tracks = seq["audio"].as_array().cloned().unwrap_or_default();
        let it = tracks.iter().flat_map(|t| t["items"].as_array().cloned().unwrap_or_default()).find(|i| i["clip"] == id).expect("the clip");
        let e = it["effects"].as_array().unwrap().iter().find(|e| e["effect"] == "volume").cloned().unwrap();
        e["params"]["level"]["value"].as_str().unwrap().trim_start_matches("Float(").trim_end_matches(')').parse().unwrap()
    };
    // the first clip on a track of its own, locked; it is selected first, so it would be the one shown
    d.exec("timeline.move", json!({"moves": [{"clip": ids[0], "track": "new", "time": start}], "linked": false}));
    let locked = d.exec("sequence.inspect", json!({}))["audio"].as_array().map_or(0, Vec::len);
    d.exec("timeline.setTrack", json!({"track": format!("A{locked}"), "locked": true}));
    d.exec("timeline.select", json!({"clips": ids}));
    d.ok("ui.panel.show", json!({"panel": "Properties"}));
    d.ok("ui.scroll", json!({"id": "panel.Properties", "dy": -2000.0}));
    d.frames(3);
    assert_eq!(d.element("properties.audioGroup").expect("two clips change")["label"], "2");
    // typed twice: the shown clip moves to the typed value, so the second time changes nothing
    type_level(&mut d, "-10");
    type_level(&mut d, "-10");
    let levels: Vec<f64> = ids.iter().map(|id| level(&mut d, *id)).collect();
    assert_eq!(levels, [-6.0, -10.0, -7.0], "the locked clip stays, the others move once by the same amount");

    // linked selection off, a video and the two unlocked audio clips: the Audio section is there
    d.exec("sequence.linkedSelection", json!({"on": false}));
    let video = d.exec("sequence.inspect", json!({}))["video"][0]["items"][0]["clip"].as_u64().expect("a video clip");
    d.exec("timeline.select", json!({"clips": [video, ids[1], ids[2]]}));
    d.frames(3);
    assert_eq!(d.element("properties.audioGroup").expect("the Audio section with a video selected")["label"], "2");
}
