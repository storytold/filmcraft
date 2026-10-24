//! Copy and paste of effects in Effect Controls (#1680).

use filmcraft_project::TrackItem;
use filmcraft_time::Tick;
use serde_json::json;

use crate::Session;

fn demo() -> Session {
    let mut s = Session::default();
    s.execute("file.openDemoProject", json!({})).unwrap();
    s
}

fn v1(s: &Session, i: usize) -> TrackItem {
    s.active_sequence().unwrap().video_tracks[0].items[i].clone()
}

fn a1(s: &Session, i: usize) -> TrackItem {
    s.active_sequence().unwrap().audio_tracks[0].items[i].clone()
}

fn index_of(it: &TrackItem, effect: &str) -> usize {
    it.effects.iter().position(|e| e.effect == effect).unwrap()
}

#[test]
fn a_copied_effect_pastes_with_its_settings_and_keyframes_in_one_undo_step() {
    let mut s = demo();
    let (a, b) = (v1(&s, 0), v1(&s, 2));
    s.execute("effects.apply", json!({"clips": [a.id.0], "effect": "gaussian_blur"})).unwrap();
    let blur = index_of(&v1(&s, 0), "gaussian_blur");
    s.execute("effects.setParam", json!({"clip": a.id.0, "effect": "gaussian_blur", "param": "blurriness", "value": 30.0})).unwrap();
    // a keyframe one second into the clip stays one second into the clip it is pasted on
    s.set_playhead(a.start + Tick::from_seconds_f64(1.0));
    s.execute("effects.toggleAnimation", json!({"clip": a.id.0, "effect": blur, "param": "blurriness"})).unwrap();
    assert!(!v1(&s, 0).effects[blur].param("blurriness").unwrap().keyframes.is_empty());
    s.execute("effects.select", json!({"clip": a.id.0, "effect": blur})).unwrap();
    assert_eq!(s.execute("effects.copy", json!({})).unwrap()["effects"], json!(["gaussian_blur"]));
    s.execute("timeline.select", json!({"clips": [b.id.0]})).unwrap();
    let before = s.project.to_json();
    let undo = s.history.undo.len();
    assert_eq!(s.execute("effects.paste", json!({})).unwrap()["clips"], json!(1));
    assert_eq!(s.history.undo.len(), undo + 1, "one undo step");
    let pasted = v1(&s, 2);
    let e = pasted.effect("gaussian_blur").expect("pasted onto the clip");
    let src = v1(&s, 0);
    let orig = src.effect("gaussian_blur").unwrap();
    assert_eq!(e.param("blurriness").unwrap().value, orig.param("blurriness").unwrap().value);
    let k_src = &orig.param("blurriness").unwrap().keyframes;
    let k_dst = &e.param("blurriness").unwrap().keyframes;
    assert_eq!(k_src.len(), k_dst.len());
    for (x, y) in k_src.iter().zip(k_dst) {
        assert_eq!(x.time - src.source_in, y.time - pasted.source_in, "keyframes keep their offset into the clip");
    }
    s.undo();
    assert_eq!(s.project.to_json(), before);
}

#[test]
fn a_fixed_effect_pastes_its_settings_instead_of_a_second_copy() {
    let mut s = demo();
    let (a, b) = (v1(&s, 0), v1(&s, 2));
    s.execute("effects.setParam", json!({"clip": a.id.0, "effect": "motion", "param": "rotation", "value": 45.0})).unwrap();
    s.execute("effects.copy", json!({"clip": a.id.0, "indices": [index_of(&a, "motion")]})).unwrap();
    let motions = v1(&s, 2).effects.iter().filter(|e| e.effect == "motion").count();
    s.execute("effects.paste", json!({"clips": [b.id.0]})).unwrap();
    let got = v1(&s, 2);
    assert_eq!(got.effects.iter().filter(|e| e.effect == "motion").count(), motions, "Motion is replaced, not added");
    assert_eq!(got.effect("motion").unwrap().param("rotation").unwrap().value.as_f64(), Some(45.0));
}

#[test]
fn paste_settings_replaces_one_effect_and_keeps_its_place_and_switch() {
    let mut s = demo();
    let (a, b) = (v1(&s, 0), v1(&s, 2));
    for c in [a.id.0, b.id.0] {
        s.execute("effects.apply", json!({"clips": [c], "effect": "gaussian_blur"})).unwrap();
    }
    s.execute("effects.setParam", json!({"clip": a.id.0, "effect": "gaussian_blur", "param": "blurriness", "value": 50.0})).unwrap();
    s.execute("effects.copy", json!({"clip": a.id.0, "indices": [index_of(&v1(&s, 0), "gaussian_blur")]})).unwrap();
    let target = index_of(&v1(&s, 2), "gaussian_blur");
    s.execute("effects.toggleEnabled", json!({"clip": b.id.0, "index": target})).unwrap();
    let enabled = v1(&s, 2).effects[target].enabled;
    let count = v1(&s, 2).effects.len();
    s.execute("effects.pasteSettings", json!({"clip": b.id.0, "index": target})).unwrap();
    let got = v1(&s, 2);
    assert_eq!(got.effects.len(), count, "no effect is added");
    assert_eq!(got.effects[target].effect, "gaussian_blur");
    assert_eq!(got.effects[target].param("blurriness").unwrap().value.as_f64(), Some(50.0));
    assert_eq!(got.effects[target].enabled, enabled);
    // onto an effect of another kind it is refused and nothing changes
    let before = s.project.to_json();
    let motion = index_of(&got, "motion");
    assert!(s.execute("effects.pasteSettings", json!({"clip": b.id.0, "index": motion})).is_err());
    assert_eq!(s.project.to_json(), before);
}

#[test]
fn hostile_or_mismatched_requests_are_refused_without_a_change() {
    let mut s = demo();
    let (a, aud) = (v1(&s, 0), a1(&s, 0));
    assert!(s.execute("effects.paste", json!({"clips": [a.id.0]})).is_err(), "nothing copied yet");
    assert!(s.execute("effects.copy", json!({})).is_err(), "no effect selected");
    for p in [
        json!({"clip": a.id.0, "indices": [999]}),
        json!({"clip": a.id.0, "indices": "x"}),
        json!({"clip": a.id.0, "indices": []}),
        json!({"clip": 987654, "indices": [0]}),
    ] {
        assert!(s.execute("effects.copy", p.clone()).is_err(), "{p}");
    }
    s.execute("effects.copy", json!({"clip": a.id.0, "indices": [0, 0]})).unwrap();
    let before = s.project.to_json();
    let undo = s.history.undo.len();
    assert!(s.execute("effects.paste", json!({"clips": [aud.id.0]})).is_err(), "video effects do not paste onto audio clips");
    for p in [json!({"clip": a.id.0, "index": 999}), json!({"clip": a.id.0}), json!({"index": 0})] {
        assert!(s.execute("effects.pasteSettings", p.clone()).is_err(), "{p}");
    }
    assert_eq!(s.project.to_json(), before);
    assert_eq!(s.history.undo.len(), undo);
}

#[test]
fn a_paste_naming_an_unknown_clip_changes_nothing() {
    // #2471 review: the unknown id used to be dropped and the rest pasted
    let mut s = demo();
    let (a, b) = (v1(&s, 0), v1(&s, 2));
    s.execute("effects.copy", json!({"clip": a.id.0, "indices": [index_of(&a, "motion")]})).unwrap();
    let before = s.project.to_json();
    let undo = s.history.undo.len();
    for p in [json!({"clips": [b.id.0, 987_654]}), json!({"clips": [b.id.0, "x"]}), json!({"clips": b.id.0}), json!({"clip": "x"})] {
        assert!(s.execute("effects.paste", p.clone()).is_err(), "{p}");
    }
    assert_eq!(s.project.to_json(), before);
    assert_eq!(s.history.undo.len(), undo);
}

#[test]
fn copying_clips_replaces_copied_effects() {
    // #2471 review: the last thing copied wins
    let mut s = demo();
    let a = v1(&s, 0);
    s.execute("effects.copy", json!({"clip": a.id.0, "indices": [0]})).unwrap();
    assert!(s.state.effect_clipboard.is_some());
    s.execute("timeline.select", json!({"clips": [a.id.0]})).unwrap();
    s.execute("edit.copy", json!({})).unwrap();
    assert!(s.state.effect_clipboard.is_none());
    assert!(s.execute("effects.paste", json!({"clips": [a.id.0]})).is_err());
}
