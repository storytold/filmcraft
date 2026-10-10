//! Headless UI test of gaps in the Timeline (#648, #668): clicking empty time between two clips
//! selects the gap (drawn as a light block), and Delete, Backspace or the gap's right-click menu
//! (Ripple Delete) closes it, as in Premiere Pro. The real `FilmcraftApp` under `egui_kittest`,
//! driven over the control channel.

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
    /// A sequence with two 48-frame clips on V1 (and their sounds on A1) at frames 0 and 96.
    fn two_clips() -> (Self, ClipId, ClipId) {
        let mut s = Session::default();
        s.execute("file.openDemoProject", json!({})).unwrap();
        s.execute("file.newSequence", json!({"name": "Gaps", "video": 1, "audio": 1})).unwrap();
        let rate = s.sequence_rate();
        let item = s.project.items.values().find(|i| i.has_video() && i.has_audio() && i.as_media().is_some()).map(|i| i.id).unwrap();
        for f in [0, 96] {
            s.execute("timeline.place", json!({"item": item.0, "frame": f, "sourceIn": rate.tick_of(48).0, "duration": rate.tick_of(48).0})).unwrap();
        }
        s.execute("edit.deselectAll", json!({})).unwrap();
        let v1 = &s.active_sequence().unwrap().video_tracks[0];
        let (a, b) = (v1.items[0].id, v1.items[1].id);
        let (tx, rx) = channel();
        let app = FilmcraftApp::new(s).with_control(rx);
        let harness = Harness::builder().with_size(egui::vec2(1600.0, 980.0)).with_max_steps(10_000).build_eframe(move |_cc| app);
        let mut d = Driver { harness, tx };
        d.frames(4);
        (d, a, b)
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

    fn rect(&mut self, id: &str) -> Option<[f64; 4]> {
        self.frames(2);
        let v = self.ok("ui.elements", json!({"prefix": id}));
        let e = v.as_array().unwrap().iter().find(|e| e["id"] == json!(id))?.clone();
        let r: Vec<f64> = e["rect"].as_array().unwrap().iter().map(|x| x.as_f64().unwrap()).collect();
        Some([r[0], r[1], r[2], r[3]])
    }

    /// Click (`button`: "left" or "right") in the middle of the empty time between `a` and `b`.
    fn click_between(&mut self, a: ClipId, b: ClipId, button: &str) {
        let ra = self.rect(&format!("timeline.clip.{}", a.0)).expect("the first clip is shown");
        let rb = self.rect(&format!("timeline.clip.{}", b.0)).expect("the second clip is shown");
        let (gap_l, gap_r) = (ra[0] + ra[2], rb[0]);
        assert!(gap_r - gap_l > 10.0, "the gap is wide enough to click: {ra:?} {rb:?}");
        self.ok("ui.click", json!({"x": (gap_l + gap_r) / 2.0, "y": ra[1] + ra[3] / 2.0, "button": button}));
        self.frames(3);
    }

    fn start(&mut self, c: ClipId) -> i64 {
        self.harness.state_mut().session.active_sequence().unwrap().find_item(c).map(|(_, i)| i.start.0).unwrap()
    }

    fn frame(&mut self, n: i64) -> i64 {
        self.harness.state_mut().session.sequence_rate().tick_of(n).0
    }

    fn gap(&mut self) -> Value {
        self.ok("engine.execute", json!({"command": "sequence.inspect", "params": {}}))["gapSelection"].clone()
    }
}

#[test]
fn clicking_a_gap_selects_it_and_delete_closes_it() {
    let (mut d, a, b) = Driver::two_clips();
    let (f48, f96) = (d.frame(48), d.frame(96));
    d.click_between(a, b, "left");
    let g = d.gap();
    assert_eq!((g["start"].as_i64(), g["end"].as_i64()), (Some(f48), Some(f96)), "the gap is selected: {g}");
    assert!(d.rect("timeline.gap").is_some(), "and drawn");
    d.ok("ui.key", json!({"key": "Delete"}));
    d.frames(3);
    assert_eq!(d.start(b), f48, "Delete closes the gap");
    assert_eq!(d.start(a), 0, "the clip before stays");
    assert_eq!(d.gap(), Value::Null);
    assert!(d.rect("timeline.gap").is_none(), "nothing is drawn any more");
}

#[test]
fn backspace_closes_a_selected_gap_too() {
    let (mut d, a, b) = Driver::two_clips();
    let f48 = d.frame(48);
    d.click_between(a, b, "left");
    d.ok("ui.key", json!({"key": "Backspace"}));
    d.frames(3);
    assert_eq!(d.start(b), f48);
}

#[test]
fn right_clicking_a_gap_offers_ripple_delete() {
    let (mut d, a, b) = Driver::two_clips();
    let f48 = d.frame(48);
    d.click_between(a, b, "right");
    assert!(d.gap().is_object(), "right-clicking selects the gap");
    assert!(d.rect("timeline.gapMenu.edit.rippleDelete").is_some(), "its menu offers Ripple Delete");
    assert!(d.rect("timeline.clipMenu.edit.cut").is_none(), "not the clip menu");
    d.ok("ui.click", json!({"id": "timeline.gapMenu.edit.rippleDelete"}));
    d.frames(3);
    assert_eq!(d.start(b), f48, "Ripple Delete closes the gap");
}

#[test]
fn clicking_a_clip_or_after_the_last_clip_drops_the_gap() {
    let (mut d, a, b) = Driver::two_clips();
    let f96 = d.frame(96);
    d.click_between(a, b, "left");
    d.ok("ui.click", json!({"id": format!("timeline.clip.{}", a.0)}));
    d.frames(3);
    assert_eq!(d.gap(), Value::Null, "a clip selection replaces the gap");
    d.click_between(a, b, "left");
    // empty time after the last clip is not a gap
    let rb = d.rect(&format!("timeline.clip.{}", b.0)).unwrap();
    d.ok("ui.click", json!({"x": rb[0] + rb[2] + 30.0, "y": rb[1] + rb[3] / 2.0}));
    d.frames(3);
    assert_eq!(d.gap(), Value::Null);
    d.ok("ui.key", json!({"key": "Delete"}));
    d.frames(3);
    assert_eq!(d.start(b), f96, "Delete with nothing selected moves nothing");
}

/// A cut's edit point menu, right-clicked before, does not stand in for the gap's menu.
#[test]
fn the_gap_menu_follows_an_edit_point_menu() {
    let (mut d, a, b) = Driver::two_clips();
    d.ok("ui.click", json!({"id": format!("timeline.clip.{}", b.0), "fx": 0.0, "button": "right"}));
    d.frames(3);
    assert!(d.rect("timeline.editPointMenu.rippleIn").is_some(), "the edge's edit point menu");
    d.ok("ui.key", json!({"key": "Escape"}));
    d.frames(3);
    d.click_between(a, b, "right");
    assert!(d.rect("timeline.gapMenu.edit.rippleDelete").is_some(), "the gap's menu");
    assert!(d.rect("timeline.editPointMenu.rippleIn").is_none(), "not the edit point menu");
}
