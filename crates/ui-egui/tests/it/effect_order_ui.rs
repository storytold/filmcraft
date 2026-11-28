//! Effect order in Effect Controls (#1731), driven headless: a standard effect's header dragged
//! onto another effect's header moves the effect there on release, and only then; Alt+Up / Alt+Down
//! move the effect whose header has keyboard focus, and the focus stays on it. What the UI keeps by
//! effect index (selected keyframes, collapsed rows) follows the move, through Undo too.
//!
//! With `FILMCRAFT_UI_SHOTS=<dir>` the tests also render the UI with wgpu and write PNGs there.

use crate::support::{Drive, Launch, builder, demo_session, driver, save_settled_shot, with_shots};
use egui_kittest::kittest::{NodeT, Queryable};
use filmcraft_project::{ClipId, EffectInstance};
use serde_json::json;

driver!(Driver);

impl Driver {
    /// The demo project's first video clip (without its linked audio) selected, with Rounded Crop
    /// and Drop Shadow applied after its fixed effects and every effect twirled shut, so all the
    /// headers fit in Effect Controls.
    fn demo() -> (Self, u64) {
        let mut d = Driver::open(demo_session(), with_shots(builder().with_step_dt(1.0 / 60.0)));
        let clip = d.exec("sequence.inspect", json!({}))["video"][0]["items"][0]["clip"].as_u64().unwrap();
        d.exec("timeline.select", json!({"clips": [clip], "linked": false}));
        d.ok("ui.panel.show", json!({"panel": "EffectControls"}));
        for effect in ["rounded_crop", "drop_shadow"] {
            d.exec("effects.apply", json!({"clips": [clip], "effect": effect}));
        }
        for idx in 0..d.stack(clip).len() {
            d.twirl(clip, idx);
        }
        (d, clip)
    }
    fn stack(&self, clip: u64) -> Vec<EffectInstance> {
        self.harness.state().session.active_sequence().unwrap().find_item(ClipId(clip)).unwrap().1.effects.clone()
    }
    fn index_of(&self, clip: u64, effect: &str) -> usize {
        self.stack(clip).iter().position(|e| e.effect == effect).unwrap()
    }
    fn undo_len(&self) -> usize {
        self.harness.state().session.history.undo.len()
    }
    fn header(&mut self, clip: u64, idx: usize) -> [f64; 4] {
        self.frames(2);
        let id = format!("effectControls.clip.{clip}.effect.{idx}");
        self.element_rect(&id).unwrap_or_else(|| panic!("no element {id}"))
    }
    fn twirl(&mut self, clip: u64, idx: usize) {
        self.click(&format!("effectControls.clip.{clip}.effect.{idx}.twirl"));
    }
    /// A point on the header's name (where a drag starts), in its upper or lower half.
    fn on_name(&mut self, clip: u64, idx: usize, lower: bool) -> egui::Pos2 {
        let r = self.header(clip, idx);
        egui::pos2(r[0] as f32 + 100.0, (r[1] + if lower { r[3] - 3.0 } else { 3.0 }) as f32)
    }
    fn press_drag(&mut self, from: egui::Pos2, to: egui::Pos2) {
        let input = |d: &mut Self, e| d.harness.input_mut().events.push(e);
        input(self, egui::Event::PointerMoved(from));
        self.frames(1);
        input(self, egui::Event::PointerButton { pos: from, button: egui::PointerButton::Primary, pressed: true, modifiers: egui::Modifiers::NONE });
        self.frames(1);
        for i in 1..=8 {
            input(self, egui::Event::PointerMoved(from.lerp(to, i as f32 / 8.0)));
            self.frames(1);
        }
    }
    fn release(&mut self, at: egui::Pos2) {
        self.harness.input_mut().events.push(egui::Event::PointerButton {
            pos: at,
            button: egui::PointerButton::Primary,
            pressed: false,
            modifiers: egui::Modifiers::NONE,
        });
        self.frames(3);
    }
    fn escape(&mut self) {
        for pressed in [true, false] {
            let key = egui::Event::Key { key: egui::Key::Escape, physical_key: None, pressed, repeat: false, modifiers: egui::Modifiers::NONE };
            self.harness.input_mut().events.push(key);
            self.frames(1);
        }
    }
    fn key(&mut self, key: &str) {
        self.ok("ui.key", json!({"key": key}));
        self.frames(3);
    }
    fn shot(&mut self, name: &str) {
        save_settled_shot(self, &format!("{name}.png"));
    }
}

#[test]
fn dropping_a_header_on_another_moves_the_effect_on_release_and_rows_follow_it() {
    let (mut d, clip) = Driver::demo();
    let (crop, shadow) = (d.index_of(clip, "rounded_crop"), d.index_of(clip, "drop_shadow"));
    d.exec("effects.setParam", json!({"clip": clip, "effect": crop, "param": "left", "value": 10, "time": 0, "keyframe": true}));
    let time = d.stack(clip)[crop].params["left"].keyframes[0].time.0;
    d.ok("ui.set", json!({"keyframeSelection": [{"clip": clip, "effect": crop, "param": "left", "time": time}]}));
    d.twirl(clip, shadow);
    let before = d.stack(clip);
    let mark = d.undo_len();
    let (from, to) = (d.on_name(clip, shadow, false), d.on_name(clip, crop, false));
    d.press_drag(from, to);
    assert_eq!(d.stack(clip), before, "nothing moves before the release");
    assert_eq!(d.undo_len(), mark);
    assert!(d.has("effectControls.insertion"), "a line shows where the effect will go");
    d.shot("effect-order-insertion");
    d.release(to);
    let after = d.stack(clip);
    assert_eq!((&after[crop], &after[shadow]), (&before[shadow], &before[crop]), "Drop Shadow now runs before Rounded Crop");
    assert_eq!(d.undo_len(), mark + 1, "one undo step");
    let ui = &d.harness.state().ui;
    assert_eq!(ui.keyframe_selection.first().map(|k| k.effect), Some(shadow), "the selected keyframe stays on Rounded Crop");
    assert!(ui.collapsed_fx.contains(&format!("{clip}:{shadow}")) && !ui.collapsed_fx.contains(&format!("{clip}:{crop}")), "Drop Shadow stays open");
    d.shot("effect-order-after");
    d.exec("edit.undo", json!({}));
    d.frames(2);
    assert_eq!(d.stack(clip), before);
    let ui = &d.harness.state().ui;
    assert_eq!(ui.keyframe_selection.first().map(|k| k.effect), Some(crop), "Undo takes the selection back with the effect");
    assert!(!ui.collapsed_fx.contains(&format!("{clip}:{shadow}")));
}

#[test]
fn escape_a_drop_off_target_and_a_fixed_header_leave_the_stack_alone() {
    let (mut d, clip) = Driver::demo();
    let (crop, shadow, motion) = (d.index_of(clip, "rounded_crop"), d.index_of(clip, "drop_shadow"), d.index_of(clip, "motion"));
    let before = d.stack(clip);
    let mark = d.undo_len();
    let (from, to) = (d.on_name(clip, crop, false), d.on_name(clip, shadow, true));
    d.press_drag(from, to);
    assert!(d.has("effectControls.insertion"));
    d.escape();
    assert!(!d.has("effectControls.insertion"), "Escape takes the insertion line away");
    d.release(to);
    // a drop on its own header, outside the panel, and a drag of a fixed effect
    for (from, to) in [(from, from + egui::vec2(60.0, 0.0)), (from, egui::pos2(-20.0, -20.0)), (d.on_name(clip, motion, false), to)] {
        d.press_drag(from, to);
        d.release(to);
    }
    assert_eq!(d.stack(clip), before);
    assert_eq!(d.undo_len(), mark);
}

#[test]
fn a_drop_after_the_stack_changed_during_the_drag_moves_nothing() {
    let (mut d, clip) = Driver::demo();
    let (crop, shadow) = (d.index_of(clip, "rounded_crop"), d.index_of(clip, "drop_shadow"));
    let (from, to) = (d.on_name(clip, shadow, false), d.on_name(clip, crop, false));
    d.press_drag(from, to);
    d.exec("effects.setParam", json!({"clip": clip, "effect": crop, "param": "left", "value": 17}));
    let expected = d.stack(clip);
    let mark = d.undo_len();
    d.release(to);
    assert_eq!(d.stack(clip), expected, "the indices the drag picked up may no longer name the same effects");
    assert_eq!(d.undo_len(), mark);
}

#[test]
fn duplicate_effects_have_headers_of_their_own() {
    let (mut d, clip) = Driver::demo();
    d.exec("effects.apply", json!({"clips": [clip], "effect": "rounded_crop"}));
    let first = d.index_of(clip, "rounded_crop");
    let last = d.stack(clip).len() - 1;
    d.exec("effects.setParam", json!({"clip": clip, "effect": last, "param": "left", "value": 17}));
    d.twirl(clip, last);
    let before = d.stack(clip);
    let (from, to) = (d.on_name(clip, last, false), d.on_name(clip, first, false));
    d.press_drag(from, to);
    d.release(to);
    let after = d.stack(clip);
    assert_eq!(after[first], before[last], "the second Rounded Crop moved, not the first");
    assert_eq!(after[first + 1], before[first]);
    assert_eq!(d.harness.state().session.state.selected_effect.as_ref().unwrap().index(&after), Some(first));
}

#[test]
fn alt_arrows_move_the_focused_effect_and_keyboard_focus_stays_on_it() {
    let (mut d, clip) = Driver::demo();
    d.ok("ui.set", json!({"focused": "EffectControls"}));
    let shadow = d.index_of(clip, "drop_shadow");
    d.harness.get_by_label("Drop Shadow, 2 of 2").focus();
    d.frames(3);
    assert_eq!(
        d.harness.state().session.state.selected_effect.as_ref().map(|e| e.effect.as_str()),
        Some("drop_shadow"),
        "focusing a header selects its effect"
    );
    let node = d.harness.get_by_label("Drop Shadow, 2 of 2");
    assert!(node.accesskit_node().is_selected().unwrap_or(false));
    assert!(node.accesskit_node().data().keyboard_shortcut().is_some_and(|k| !k.is_empty()), "the header tells its Move Up / Move Down keys");
    d.key("Alt+Up");
    let moved = d.stack(clip);
    assert_eq!(moved[shadow - 1].effect, "drop_shadow");
    assert!(d.harness.get_by_label("Drop Shadow, 1 of 2").is_focused(), "focus moved with the effect");
    let mark = d.undo_len();
    d.key("Alt+Up");
    assert_eq!(d.stack(clip), moved, "the first effect can't move up");
    assert_eq!(d.undo_len(), mark);
    d.key("Alt+Down");
    assert_eq!(d.stack(clip)[shadow].effect, "drop_shadow");
    assert!(d.harness.get_by_label("Drop Shadow, 2 of 2").is_focused());
}
