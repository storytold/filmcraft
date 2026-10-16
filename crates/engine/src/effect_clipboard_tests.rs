//! Copy / Cut / Paste of effects (Effect Controls) and Delete Keyframes: an effect cut from a clip
//! and pasted on an adjustment layer moves the effect only, never clips.

use filmcraft_project::{ClipId, ParamValue, TrackItem};
use filmcraft_time::{TICKS_PER_SECOND, Tick};
use serde_json::json;

use crate::Session;

fn demo() -> Session {
    let mut s = Session::default();
    s.execute("file.openDemoProject", json!({})).unwrap();
    s
}

fn clip(s: &Session, id: ClipId) -> TrackItem {
    s.active_sequence().unwrap().find_item(id).unwrap().1.clone()
}

fn v1(s: &Session) -> Vec<TrackItem> {
    s.active_sequence().unwrap().video_tracks[0].items.clone()
}

/// Every clip in the sequence, by id: pasting effects must leave this set alone.
fn all_clips(s: &Session) -> Vec<ClipId> {
    let mut ids: Vec<ClipId> = s.active_sequence().unwrap().all_tracks().flat_map(|t| t.items.iter().map(|i| i.id)).collect();
    ids.sort();
    ids
}

fn has(it: &TrackItem, effect: &str) -> bool {
    it.effects.iter().any(|e| e.effect == effect)
}

fn float(it: &TrackItem, effect: &str, param: &str) -> f64 {
    match it.effect(effect).and_then(|e| e.param(param)).map(|p| &p.value) {
        Some(ParamValue::Float(v)) => *v,
        other => panic!("{effect}.{param}: {other:?}"),
    }
}

/// An adjustment layer placed on V3 over the start of the sequence.
fn adjustment_layer(s: &mut Session) -> ClipId {
    let item = s.execute("file.newAdjustmentLayer", json!({"seconds": 5})).unwrap()["item"].as_u64().unwrap();
    let r = s.execute("timeline.place", json!({"item": item, "track": "V3", "seconds": 0.0})).unwrap();
    ClipId(r["clips"][0].as_u64().unwrap())
}

/// A clip with Lumetri Color (Exposure 1.5), the effect selected in Effect Controls.
fn graded(s: &mut Session) -> ClipId {
    let a = v1(s)[0].id;
    s.execute("timeline.select", json!({"clips": [a.0]})).unwrap();
    s.execute("effects.apply", json!({"clips": [a.0], "effect": "lumetri"})).unwrap();
    s.execute("effects.setParam", json!({"clip": a.0, "effect": "lumetri", "param": "exposure", "value": 1.5})).unwrap();
    s.execute("effects.select", json!({"clip": a.0, "effect": "lumetri"})).unwrap();
    a
}

#[test]
fn a_cut_effect_pastes_on_an_adjustment_layer_without_pasting_clips() {
    let mut s = demo();
    let layer = adjustment_layer(&mut s);
    let a = graded(&mut s);
    let clips = all_clips(&s);
    let r = s.execute("effects.cut", json!({})).unwrap();
    assert_eq!(r["removed"], true);
    assert!(!has(&clip(&s, a), "lumetri"), "Cut takes the effect off its clip");
    assert!(s.state.selected_effect.is_none(), "the cut effect is no longer selected");
    assert_eq!(all_clips(&s), clips, "Cut leaves the clip itself in the Timeline");
    // select the adjustment layer and Paste (Cmd+V)
    s.execute("timeline.select", json!({"clips": [layer.0]})).unwrap();
    assert!(s.is_enabled("edit.paste"));
    s.execute("edit.paste", json!({})).unwrap();
    assert_eq!(all_clips(&s), clips, "Paste puts the effect on the layer and adds no clips");
    let l = clip(&s, layer);
    assert!(has(&l, "lumetri"));
    assert!((float(&l, "lumetri", "exposure") - 1.5).abs() < 1e-6, "the effect keeps its settings");
    let lumetri = l.effects.iter().position(|e| e.effect == "lumetri").unwrap();
    let first_intrinsic = l.effects.iter().position(|e| e.def().is_some_and(|d| d.intrinsic)).unwrap();
    assert!(lumetri < first_intrinsic, "standard effects go before Motion / Opacity, as Apply Effect puts them");
    // one undo step each
    s.execute("edit.undo", json!({})).unwrap();
    assert!(!has(&clip(&s, layer), "lumetri"));
    s.execute("edit.undo", json!({})).unwrap();
    assert!(has(&clip(&s, a), "lumetri"));
}

#[test]
fn copy_keeps_the_effect_and_pastes_on_every_selected_clip() {
    let mut s = demo();
    let a = graded(&mut s);
    let (b, c) = (v1(&s)[1].id, v1(&s)[2].id);
    s.execute("effects.copy", json!({})).unwrap();
    assert!(has(&clip(&s, a), "lumetri"), "Copy leaves the effect on its clip");
    s.execute("timeline.select", json!({"clips": [b.0, c.0]})).unwrap();
    let r = s.execute("edit.paste", json!({})).unwrap();
    assert_eq!(r["clips"], 2);
    assert!(has(&clip(&s, b), "lumetri") && has(&clip(&s, c), "lumetri"));
    // pasting again adds a second instance, as Premiere does
    s.execute("edit.paste", json!({})).unwrap();
    assert_eq!(clip(&s, b).effects.iter().filter(|e| e.effect == "lumetri").count(), 2);
}

#[test]
fn the_clipboard_holds_clips_or_effects() {
    let mut s = demo();
    let a = graded(&mut s);
    s.execute("timeline.select", json!({"clips": [a.0]})).unwrap();
    s.execute("edit.copy", json!({})).unwrap();
    assert!(!s.state.clipboard.is_empty());
    s.execute("effects.copy", json!({})).unwrap();
    assert!(s.state.clipboard.is_empty(), "copying an effect replaces the copied clips");
    assert!(s.state.effect_clipboard.is_some());
    s.execute("edit.copy", json!({})).unwrap();
    assert!(s.state.effect_clipboard.is_none(), "copying clips replaces the copied effect");
    // and Paste pastes clips again
    let n = all_clips(&s).len();
    s.execute("edit.paste", json!({})).unwrap();
    assert!(all_clips(&s).len() > n);
}

#[test]
fn paste_needs_a_clip_of_the_effects_kind() {
    let mut s = demo();
    graded(&mut s);
    s.execute("effects.copy", json!({})).unwrap();
    s.execute("timeline.select", json!({"none": true})).ok();
    s.state.selection.clear();
    assert!(!s.is_enabled("edit.paste"), "nothing to paste on");
    // a video effect on an audio clip: refused, nothing changes
    let audio = s.active_sequence().unwrap().audio_tracks[0].items[0].clone();
    s.execute("sequence.linkedSelection", json!({"on": false})).unwrap();
    s.execute("timeline.select", json!({"clips": [audio.id.0]})).unwrap();
    let rev = s.project.clone();
    assert!(s.execute("edit.paste", json!({})).is_err());
    assert_eq!(s.project, rev);
}

#[test]
fn cut_motion_copies_it_and_paste_replaces_the_target_motion() {
    let mut s = demo();
    let (a, b) = (v1(&s)[0].id, v1(&s)[1].id);
    s.execute("effects.setParam", json!({"clip": a.0, "effect": "motion", "param": "scale", "value": 50.0})).unwrap();
    s.execute("timeline.select", json!({"clips": [a.0]})).unwrap();
    s.execute("effects.select", json!({"clip": a.0, "effect": "motion"})).unwrap();
    let r = s.execute("effects.cut", json!({})).unwrap();
    assert_eq!(r["removed"], false, "Motion belongs to every clip and stays");
    assert!((float(&clip(&s, a), "motion", "scale") - 50.0).abs() < 1e-6);
    s.execute("timeline.select", json!({"clips": [b.0]})).unwrap();
    s.execute("edit.paste", json!({})).unwrap();
    let pasted = clip(&s, b);
    assert!((float(&pasted, "motion", "scale") - 50.0).abs() < 1e-6);
    assert_eq!(pasted.effects.iter().filter(|e| e.effect == "motion").count(), 1, "replaced, not added");
}

#[test]
fn pasted_keyframes_keep_their_offset_from_the_in_point() {
    let mut s = demo();
    let a = graded(&mut s);
    let b = v1(&s)[1].id;
    let one = Tick(TICKS_PER_SECOND);
    let ai = clip(&s, a);
    let at = ai.start + one;
    s.execute("effects.setParam", json!({"clip": a.0, "effect": "lumetri", "param": "exposure", "value": 2.0, "time": at.0, "keyframe": true})).unwrap();
    let key = clip(&s, a).effect("lumetri").unwrap().param("exposure").unwrap().keyframes[0].time;
    let offset = key - ai.source_in;
    s.execute("effects.copy", json!({"clip": a.0, "effect": clip(&s, a).effects.iter().position(|e| e.effect == "lumetri").unwrap()})).unwrap();
    s.execute("effects.paste", json!({"clips": [b.0]})).unwrap();
    let bi = clip(&s, b);
    let k = bi.effect("lumetri").unwrap().param("exposure").unwrap().keyframes[0].time;
    assert_eq!(k - bi.source_in, offset);
}

#[test]
fn graphic_layers_and_bad_targets_are_refused() {
    let mut s = demo();
    assert!(s.execute("effects.copy", json!({})).is_err(), "nothing selected in Effect Controls");
    assert!(s.execute("effects.copy", json!({"clip": 999_999, "effect": 0})).is_err());
    assert!(s.execute("effects.copy", json!({"clip": v1(&s)[0].id.0})).is_err(), "clip without effect");
    assert!(s.execute("effects.copy", json!({"clip": v1(&s)[0].id.0, "effect": 999})).is_err());
    assert!(s.execute("effects.paste", json!({})).is_err(), "nothing copied");
}

#[test]
fn delete_keyframes_is_one_undo_step() {
    let mut s = demo();
    let a = v1(&s)[0].clone();
    let one = Tick(TICKS_PER_SECOND);
    for (param, value) in [("scale", 50.0), ("rotation", 10.0)] {
        s.execute("effects.setParam", json!({"clip": a.id.0, "effect": "motion", "param": param, "value": value, "time": (a.start + one).0, "keyframe": true}))
            .unwrap();
    }
    let it = clip(&s, a.id);
    let motion = it.effects.iter().position(|e| e.effect == "motion").unwrap();
    let keys: Vec<_> = ["scale", "rotation"]
        .iter()
        .map(|p| json!({"clip": a.id.0, "effect": motion, "param": p, "time": it.effects[motion].param(p).unwrap().keyframes[0].time.0}))
        .collect();
    let r = s.execute("effects.deleteKeyframes", json!({"keyframes": keys})).unwrap();
    assert_eq!(r["deleted"], 2);
    let after = clip(&s, a.id);
    assert!(after.effects[motion].param("scale").unwrap().keyframes.is_empty());
    assert!(after.effects[motion].param("rotation").unwrap().keyframes.is_empty());
    s.execute("edit.undo", json!({})).unwrap();
    let back = clip(&s, a.id);
    assert_eq!(back.effects[motion].param("scale").unwrap().keyframes.len(), 1);
    assert_eq!(back.effects[motion].param("rotation").unwrap().keyframes.len(), 1);
    assert!(s.execute("effects.deleteKeyframes", json!({"keyframes": []})).is_err());
}
