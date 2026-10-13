use super::*;
use filmcraft_project::{ClipId, EffectInstance};
use serde_json::json;

fn setup(effects: &[&str]) -> (Session, ClipId) {
    let mut s = Session::default();
    s.execute("file.newSequence", json!({"width":64,"height":64,"fps":30,"video":1,"audio":1})).unwrap();
    let item = s.execute("file.newColorMatte", json!({"color":"#8090a0","width":64,"height":64,"seconds":2})).unwrap()["item"].as_u64().unwrap();
    s.execute("source.open", json!({"item":item})).unwrap();
    let c = ClipId(s.execute("source.overwrite", json!({})).unwrap()["clips"][0].as_u64().unwrap());
    for effect in effects {
        s.execute("effects.apply", json!({"clips":[c.0],"effect":effect})).unwrap();
    }
    (s, c)
}
fn stack(s: &Session, c: ClipId) -> Vec<EffectInstance> {
    s.active_sequence().unwrap().find_item(c).unwrap().1.effects.clone()
}

#[test]
fn reorder_preserves_duplicate_instances_keyframes_selection_and_history() {
    let (mut s, c) = setup(&["gaussian_blur", "rounded_crop", "gaussian_blur"]);
    s.execute("effects.setParam", json!({"clip":c.0,"effect":2,"param":"blurriness","value":41,"keyframe":true,"time":12345})).unwrap();
    s.execute("masks.add", json!({"clip":c.0,"effect":2,"shape":"ellipse","center":[32,32],"size":[20,20]})).unwrap();
    s.execute("effects.select", json!({"clip":c.0,"effect":2})).unwrap();
    let before = stack(&s, c);
    let mark = s.history.undo.len();
    s.execute("effects.reorder", json!({"clip":c.0,"from":2,"to":0})).unwrap();
    let after = stack(&s, c);
    assert_eq!(after[..3], [before[2].clone(), before[0].clone(), before[1].clone()]);
    assert_eq!(after[3..], before[3..]);
    assert_eq!(s.state.selected_effect.as_ref().unwrap().index(&after), Some(0));
    assert_eq!(s.history.undo.len(), mark + 1);
    s.execute("edit.undo", json!({})).unwrap();
    assert_eq!(stack(&s, c), before);
    assert_eq!(s.state.selected_effect.as_ref().unwrap().index(&before), Some(2));
    s.execute("edit.redo", json!({})).unwrap();
    assert_eq!(stack(&s, c), after);
    assert_eq!(s.state.selected_effect.as_ref().unwrap().index(&after), Some(0));
    let mark = s.history.undo.len();
    s.execute("effects.reorder", json!({"clip":c.0,"from":0,"to":0})).unwrap();
    assert_eq!(s.history.undo.len(), mark);
}

#[test]
fn reorder_rejects_intrinsics_missing_clips_and_hostile_indices_atomically() {
    let empty = Session::default();
    assert!(!empty.is_enabled("effects.reorder"));
    let (mut s, c) = setup(&["rounded_crop", "drop_shadow"]);
    let before = stack(&s, c);
    let mark = s.history.undo.len();
    for p in [
        json!({}),
        json!({"clip":c.0,"from":-1,"to":0}),
        json!({"clip":c.0,"from":0,"to":1.5}),
        json!({"clip":c.0,"from":u64::MAX,"to":0}),
        json!({"clip":c.0,"from":0,"to":u64::MAX}),
        json!({"clip":u64::MAX,"from":0,"to":1}),
        json!({"clip":c.0,"from":2,"to":0}),
        json!({"clip":c.0,"from":0,"to":2}),
    ] {
        assert!(s.execute("effects.reorder", p.clone()).is_err(), "{p}");
        assert_eq!(stack(&s, c), before);
        assert_eq!(s.history.undo.len(), mark);
    }
}

#[test]
fn reordered_crop_and_shadow_change_render_order_and_undo_restores_pixels() {
    let (mut s, c) = setup(&["rounded_crop", "drop_shadow"]);
    for (effect, param, value) in
        [(0, "left", 20), (0, "right", 20), (0, "top", 20), (0, "bottom", 20), (0, "radius", 8), (1, "distance", 10), (1, "opacity", 100)]
    {
        s.execute("effects.setParam", json!({"clip":c.0,"effect":effect,"param":param,"value":value})).unwrap();
    }
    s.execute("effects.setParam", json!({"clip":c.0,"effect":1,"param":"color","value":"#ffffff"})).unwrap();
    s.execute("playhead.set", json!({"seconds":0.5})).unwrap();
    let before = s.render_program(1.0).unwrap();
    assert!(before.px.chunks_exact(4).any(|p| p[0] > 0.1), "fixture must render inside the clip");
    s.execute("effects.reorder", json!({"clip":c.0,"from":1,"to":0})).unwrap();
    let after = s.render_program(1.0).unwrap();
    assert!(before.px.iter().zip(&after.px).any(|(a, b)| (a - b).abs() > 0.01));
    s.execute("edit.undo", json!({})).unwrap();
    assert_eq!(s.render_program(1.0).unwrap().px, before.px);
    s.execute("edit.redo", json!({})).unwrap();
    assert_eq!(s.render_program(1.0).unwrap().px, after.px);
}

#[test]
fn audio_reorder_leaves_intrinsic_effects_in_their_original_positions() {
    let mut s = Session::default();
    s.execute("file.openDemoProject", json!({})).unwrap();
    let c = s.active_sequence().unwrap().audio_tracks[0].items[0].id;
    let fixed = stack(&s, c).len();
    for effect in ["tube_compressor", "chorus_flanger"] {
        s.execute("effects.apply", json!({"clips":[c.0],"effect":effect})).unwrap();
    }
    let before = stack(&s, c);
    s.execute("effects.reorder", json!({"clip":c.0,"from":fixed+1,"to":fixed})).unwrap();
    let after = stack(&s, c);
    assert_eq!(after[..fixed], before[..fixed]);
    assert_eq!(after[fixed], before[fixed + 1]);
    assert!(s.execute("effects.reorder", json!({"clip":c.0,"from":fixed,"to":0})).is_err());
    s.execute("edit.undo", json!({})).unwrap();
    assert_eq!(stack(&s, c), before);
}

#[test]
fn history_clears_selection_when_identical_duplicate_instances_are_ambiguous() {
    let (mut s, c) = setup(&["gaussian_blur", "rounded_crop", "gaussian_blur"]);
    s.execute("effects.select", json!({"clip":c.0,"effect":2})).unwrap();
    let before = stack(&s, c);
    s.execute("effects.reorder", json!({"clip":c.0,"from":2,"to":0})).unwrap();
    let after = stack(&s, c);
    assert_eq!(s.state.selected_effect.as_ref().unwrap().index(&after), Some(0));
    s.execute("edit.undo", json!({})).unwrap();
    assert_eq!(stack(&s, c), before);
    assert!(s.state.selected_effect.is_none());
    s.execute("edit.redo", json!({})).unwrap();
    assert_eq!(stack(&s, c), after);
}

#[test]
fn selected_mask_follows_its_effect_through_reorder_and_history() {
    let (mut s, c) = setup(&["gaussian_blur", "rounded_crop", "gaussian_blur"]);
    s.execute("masks.add", json!({"clip":c.0,"effect":2,"shape":"ellipse","center":[32,32],"size":[20,20]})).unwrap();
    assert_eq!(s.state.selected_mask.unwrap().effect, 2);
    s.execute("effects.reorder", json!({"clip":c.0,"from":2,"to":0})).unwrap();
    assert_eq!(s.state.selected_mask.unwrap().effect, 0);
    s.execute("edit.undo", json!({})).unwrap();
    assert_eq!(s.state.selected_mask.unwrap().effect, 2);
    s.execute("edit.redo", json!({})).unwrap();
    assert_eq!(s.state.selected_mask.unwrap().effect, 0);
}
