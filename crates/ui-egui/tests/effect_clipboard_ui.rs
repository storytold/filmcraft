//! Cmd+X / Cmd+C / Delete with Effect Controls focused act on the effect selected there, and
//! Cmd+V on a selected adjustment layer pastes that effect: the real `FilmcraftApp` under
//! `egui_kittest`, driven by key presses as a user types them.

use std::sync::mpsc::{Sender, channel};

use egui_kittest::Harness;
use filmcraft_engine::Session;
use filmcraft_project::{ClipId, TrackItem};
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

    fn exec(&mut self, id: &str, params: Value) -> Value {
        self.harness.state_mut().session.execute(id, params).unwrap_or_else(|e| panic!("{id}: {e}"))
    }

    fn focus(&mut self, panel: &str) {
        self.ok("ui.set", json!({"focused": panel}));
    }

    /// A key pressed and released with `modifiers` held.
    fn press(&mut self, key: egui::Key, modifiers: egui::Modifiers) {
        let ev = |pressed| egui::Event::Key { key, physical_key: Some(key), pressed, repeat: false, modifiers };
        self.harness.input_mut().events.extend([egui::Event::ModifiersChanged(modifiers), ev(true)]);
        self.frames(1);
        self.harness.input_mut().events.extend([ev(false), egui::Event::ModifiersChanged(egui::Modifiers::NONE)]);
        self.frames(2);
    }

    fn clip(&mut self, id: ClipId) -> TrackItem {
        self.harness.state_mut().session.active_sequence().unwrap().find_item(id).unwrap().1.clone()
    }

    fn all_clips(&mut self) -> Vec<ClipId> {
        let mut ids: Vec<ClipId> =
            self.harness.state_mut().session.active_sequence().unwrap().all_tracks().flat_map(|t| t.items.iter().map(|i| i.id)).collect();
        ids.sort();
        ids
    }

    /// An adjustment layer on V3 and the first V1 clip with Lumetri Color selected in Effect
    /// Controls (which has keyboard focus).
    fn graded_clip_and_layer(&mut self) -> (ClipId, ClipId) {
        let item = self.exec("file.newAdjustmentLayer", json!({"seconds": 5}))["item"].as_u64().unwrap();
        let layer = ClipId(self.exec("timeline.place", json!({"item": item, "track": "V3", "seconds": 0.0}))["clips"][0].as_u64().unwrap());
        let a = self.harness.state_mut().session.active_sequence().unwrap().video_tracks[0].items[0].id;
        self.exec("timeline.select", json!({"clips": [a.0]}));
        self.exec("effects.apply", json!({"clips": [a.0], "effect": "lumetri"}));
        self.exec("effects.setParam", json!({"clip": a.0, "effect": "lumetri", "param": "exposure", "value": 1.5}));
        self.exec("effects.select", json!({"clip": a.0, "effect": "lumetri"}));
        self.focus("EffectControls");
        (a, layer)
    }
}

fn has(it: &TrackItem, effect: &str) -> bool {
    it.effects.iter().any(|e| e.effect == effect)
}

const CMD: egui::Modifiers = egui::Modifiers::COMMAND;

/// The reported bug: Lumetri cut in Effect Controls and pasted on an adjustment layer pasted the
/// video clips instead. Now Cmd+X takes only the effect off the clip and Cmd+V puts it on the layer.
#[test]
fn cmd_x_in_effect_controls_then_cmd_v_on_an_adjustment_layer_moves_only_the_effect() {
    let mut d = Driver::demo();
    let (a, layer) = d.graded_clip_and_layer();
    let clips = d.all_clips();
    d.press(egui::Key::X, CMD);
    assert_eq!(d.all_clips(), clips, "Cut in Effect Controls leaves the clip in the Timeline");
    assert!(!has(&d.clip(a), "lumetri"), "and takes the effect off it");
    d.exec("timeline.select", json!({"clips": [layer.0]}));
    d.focus("Timeline");
    d.press(egui::Key::V, CMD);
    assert_eq!(d.all_clips(), clips, "Paste adds no clips");
    assert!(has(&d.clip(layer), "lumetri"), "the effect is on the adjustment layer");
}

#[test]
fn cmd_c_then_cmd_v_with_effect_controls_focused_copies_the_effect() {
    let mut d = Driver::demo();
    let (a, layer) = d.graded_clip_and_layer();
    let clips = d.all_clips();
    d.press(egui::Key::C, CMD);
    d.exec("timeline.select", json!({"clips": [layer.0]}));
    d.press(egui::Key::V, CMD);
    assert_eq!(d.all_clips(), clips);
    assert!(has(&d.clip(a), "lumetri") && has(&d.clip(layer), "lumetri"));
}

#[test]
fn delete_in_effect_controls_removes_the_effect_not_the_clip() {
    let mut d = Driver::demo();
    let (a, _) = d.graded_clip_and_layer();
    let clips = d.all_clips();
    d.press(egui::Key::Backspace, egui::Modifiers::NONE);
    d.press(egui::Key::Delete, egui::Modifiers::NONE);
    assert_eq!(d.all_clips(), clips, "the clip stays");
    assert!(!has(&d.clip(a), "lumetri"), "the selected effect is gone");
}

#[test]
fn with_nothing_selected_in_effect_controls_cut_and_delete_leave_the_clip_alone() {
    let mut d = Driver::demo();
    let (a, _) = d.graded_clip_and_layer();
    d.exec("effects.select", json!({"none": true}));
    let clips = d.all_clips();
    d.press(egui::Key::X, CMD);
    d.press(egui::Key::Backspace, egui::Modifiers::NONE);
    assert_eq!(d.all_clips(), clips);
    assert!(has(&d.clip(a), "lumetri"));
    // the Timeline still cuts clips
    d.focus("Timeline");
    d.press(egui::Key::X, CMD);
    assert_eq!(d.all_clips().len(), clips.len() - 2, "the clip and its linked audio are cut");
}
