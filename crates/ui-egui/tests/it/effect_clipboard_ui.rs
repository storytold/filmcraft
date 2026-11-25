//! Cmd+X / Cmd+C / Delete with Effect Controls focused act on the effect selected there, and
//! Cmd+V on a selected adjustment layer pastes that effect: the real `FilmcraftApp` under
//! `egui_kittest`, driven by key presses as a user types them.

use crate::support::{Drive, Launch, driver};
use filmcraft_project::{ClipId, TrackItem};
use serde_json::json;

driver!(Driver);

impl Driver {
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

/// Motion clicked in Effect Controls, Cmd+C, another clip clicked in the Timeline, Cmd+V: the clip
/// takes the copied Motion in place of its own, and nothing else changes.
#[test]
fn motion_clicked_and_copied_pastes_on_a_clip_clicked_in_the_timeline() {
    let mut d = Driver::demo();
    let v1: Vec<ClipId> = d.harness.state_mut().session.active_sequence().unwrap().video_tracks[0].items.iter().map(|i| i.id).collect();
    let (a, b) = (v1[0], v1[1]);
    d.exec("effects.setParam", json!({"clip": a.0, "effect": "motion", "param": "scale", "value": 42.0}));
    d.exec("timeline.select", json!({"clips": [a.0]}));
    d.ok("ui.panel.show", json!({"panel": "EffectControls"}));
    d.focus("Timeline");
    d.frames(4);
    d.ok("ui.click", json!({"id": "effectControls.effect.motion"}));
    d.frames(4);
    let clips = d.all_clips();
    d.press(egui::Key::C, CMD);
    d.exec("timeline.select", json!({"clips": [b.0]}));
    d.focus("Timeline");
    d.frames(4);
    let before = d.clip(b).effects.len();
    d.press(egui::Key::V, CMD);
    assert_eq!(d.all_clips(), clips, "no clip is pasted");
    let after = d.clip(b);
    assert_eq!(after.effects.len(), before, "Motion is replaced, not added");
    let motion = after.effects.iter().find(|e| e.effect == "motion").unwrap();
    assert_eq!(
        format!("{:?}", motion.param("scale").unwrap().value),
        format!("{:?}", d.clip(a).effects.iter().find(|e| e.effect == "motion").unwrap().param("scale").unwrap().value)
    );
}

/// With a caption selected Effect Controls shows no clip: Cut and Copy there leave the caption alone
/// and say to select an effect.
#[test]
fn cut_in_effect_controls_leaves_a_selected_caption_alone() {
    let mut d = Driver::demo();
    let id = d.exec("captions.add", json!({"seconds": 1.0, "durationSeconds": 1.0, "text": "stay"}))["caption"].as_u64().unwrap();
    d.exec("timeline.select", json!({"none": true}));
    d.exec("captions.select", json!({"captions": [id]}));
    d.focus("EffectControls");
    d.press(egui::Key::X, CMD);
    let captions = d.exec("captions.list", json!({}));
    assert_eq!(captions["tracks"][0]["captions"].as_array().unwrap().len(), 1, "the caption stays: {captions}");
    assert_eq!(d.harness.state_mut().ui.status, "select an effect in Effect Controls first");
}

/// A transition shown in Effect Controls is not an effect: Copy and Cut go to the Timeline as before.
#[test]
fn a_transition_shown_in_effect_controls_keeps_the_timelines_cut_and_copy() {
    let mut d = Driver::demo();
    let x = d.harness.state_mut().session.active_sequence().unwrap().all_tracks().flat_map(|t| &t.transitions).next().expect("the demo has a transition").id;
    d.exec("timeline.select", json!({"none": true}));
    d.harness.state_mut().session.state.transition_selection = [x].into_iter().collect();
    d.focus("EffectControls");
    d.press(egui::Key::C, CMD);
    assert_ne!(d.harness.state_mut().ui.status, "select an effect in Effect Controls first");
}
