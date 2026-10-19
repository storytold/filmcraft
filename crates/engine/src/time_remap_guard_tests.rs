use filmcraft_project::{ClipId, ParamValue};
use filmcraft_time::Tick;
use serde_json::json;
use std::sync::Arc;

use crate::Session;

fn fixture() -> (Session, ClipId, usize) {
    let mut s = Session::default();
    s.execute("file.openDemoProject", json!({})).unwrap();
    let it = &s.active_sequence().unwrap().video_tracks[0].items[0];
    let id = it.id;
    let index = it.effects.iter().position(|e| e.effect == "time_remap").unwrap();
    (s, id, index)
}

#[test]
fn unsupported_time_remap_set_and_animation_are_transactional() {
    let (mut s, clip, index) = fixture();
    for effect in [json!("time_remap"), json!(index)] {
        for (command, extra) in [
            ("effects.setParam", json!({"value":200})),
            ("effects.setParam", json!({"value":150,"keyframe":true})),
            ("effects.toggleAnimation", json!({})),
            ("effects.addKeyframe", json!({})),
        ] {
            let mut params = json!({"clip":clip.0,"effect":effect,"param":"speed"});
            params.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
            let before = s.project.clone();
            let error = s.execute(command, params).unwrap_err().to_string();
            assert!(error.contains("not supported") && error.contains("clip.speedDuration"), "{error}");
            assert_eq!(*s.project, *before, "{command}: no partial project edit");
        }
    }
}

#[test]
fn unsupported_legacy_ramp_edits_and_batch_moves_are_transactional_but_reset_works() {
    let (mut s, clip, index) = fixture();
    let sequence = s.state.active_sequence.unwrap();
    let it = Arc::make_mut(&mut s.project).sequence_mut(sequence).unwrap().find_item_mut(clip).unwrap().1;
    it.effects[index].params.get_mut("speed").unwrap().put_keyframe(Tick::ZERO, ParamValue::Float(100.0));
    it.effects[index].params.get_mut("speed").unwrap().put_keyframe(Tick(100), ParamValue::Float(200.0));
    for (command, extra) in [
        ("effects.setKeyframe", json!({"mediaTime":0,"value":150})),
        ("effects.moveKeyframe", json!({"mediaTime":0,"to":50})),
        ("effects.setInterpolation", json!({"mediaTime":0,"interpolation":"bezier"})),
    ] {
        let mut params = json!({"clip":clip.0,"effect":index,"param":"speed"});
        params.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
        let before = s.project.clone();
        assert!(s.execute(command, params).unwrap_err().to_string().contains("not supported"));
        assert_eq!(*s.project, *before);
    }
    // A mixed request cannot move its supported lane before rejecting the inert one.
    s.execute("effects.setParam", json!({"clip":clip.0,"effect":"motion","param":"scale","time":0,"value":100,"keyframe":true})).unwrap();
    let motion = s.active_sequence().unwrap().find_item(clip).unwrap().1.effects.iter().position(|e| e.effect == "motion").unwrap();
    let motion_time = s.active_sequence().unwrap().find_item(clip).unwrap().1.source_in.0;
    let before = s.project.clone();
    let error = s
        .execute(
            "effects.moveKeyframes",
            json!({"keyframes":[
        {"clip":clip.0,"effect":motion,"param":"scale","time":motion_time},
        {"clip":clip.0,"effect":index,"param":"speed","time":0}
    ],"delta":50}),
        )
        .unwrap_err()
        .to_string();
    assert!(error.contains("not supported"), "{error}");
    assert_eq!(*s.project, *before);
    s.execute("effects.deleteKeyframe", json!({"clip":clip.0,"effect":index,"param":"speed","mediaTime":100})).unwrap();
    s.execute("effects.deleteKeyframe", json!({"clip":clip.0,"effect":index,"param":"speed","mediaTime":0})).unwrap();
    s.execute("effects.resetParam", json!({"clip":clip.0,"effect":index,"param":"speed"})).unwrap();
    let speed = &s.active_sequence().unwrap().find_item(clip).unwrap().1.effects[index].params["speed"];
    assert_eq!(speed.value, ParamValue::Float(100.0));
    assert!(speed.keyframes.is_empty());
}

#[test]
fn time_remap_guard_preserves_constant_speed_and_other_animation() {
    let (mut s, clip, _) = fixture();
    s.execute("clip.speedDuration", json!({"clips":[clip.0],"speed":200})).unwrap();
    assert_eq!(s.active_sequence().unwrap().find_item(clip).unwrap().1.speed, 2.0);
    s.execute("effects.setParam", json!({"clip":clip.0,"effect":"motion","param":"position","value":[100,200],"keyframe":true})).unwrap();
    assert_eq!(s.active_sequence().unwrap().find_item(clip).unwrap().1.effect("motion").unwrap().params["position"].keyframes.len(), 1);
}
