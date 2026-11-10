//! M5.11: every video effect is listed with its folder and badges, applies by id or name through
//! `effects.apply` (undoable), takes parameters through `effects.setParam`, and renders.

use filmcraft_project::{EffectKind, effect_defs};
use serde_json::{Value, json};

use crate::Session;

fn demo() -> Session {
    let mut s = Session::default();
    s.execute("file.openDemoProject", json!({})).unwrap();
    s
}

#[test]
fn transition_preview_is_read_only_and_matches_apply_and_undo() {
    use crate::commands::preview_transition;
    use filmcraft_project::TrackKind;
    for (kind, effect) in [(TrackKind::Video, "cross_dissolve"), (TrackKind::Audio, "constant_power")] {
        let mut s = demo();
        let clips: Vec<_> = s.active_sequence().unwrap().tracks(kind)[0].items.iter().map(|c| c.id).collect();
        for clip in clips {
            for edge in ["in", "out"] {
                let params = json!({"effect": effect, "clip": clip.0, "edge": edge, "frames": 7});
                let before = s.project.to_json();
                let revision = s.revision;
                let (track, planned) = preview_transition(&s, &params, kind).unwrap();
                assert_eq!(s.project.to_json(), before);
                assert_eq!(s.revision, revision);
                let result = s.execute("effects.apply", params).unwrap();
                let applied =
                    s.active_sequence().unwrap().track(track).unwrap().transitions.iter().find(|t| Some(t.id.0) == result["transition"].as_u64()).unwrap();
                assert_eq!((applied.start, applied.duration, applied.from, applied.to), (planned.start, planned.duration, planned.from, planned.to));
                s.execute("edit.undo", json!({})).unwrap();
                assert_eq!(s.project.to_json(), before);
                s.execute("edit.redo", json!({})).unwrap();
                s.execute("edit.undo", json!({})).unwrap();
            }
        }
        assert!(preview_transition(&s, &json!({"effect": effect, "clip": u64::MAX}), kind).is_err());
        let track = s.active_sequence().unwrap().tracks(kind)[0].id;
        std::sync::Arc::make_mut(&mut s.project).sequence_mut(s.state.active_sequence.unwrap()).unwrap().track_mut(track).unwrap().locked = true;
        assert!(preview_transition(&s, &json!({"effect": effect, "clip": s.active_sequence().unwrap().track(track).unwrap().items[0].id.0}), kind).is_err());
    }
}

fn first_v1_clip(s: &Session) -> u64 {
    s.active_sequence().unwrap().video_tracks[0].items[0].id.0
}

fn video_effects() -> Vec<&'static filmcraft_project::EffectDef> {
    effect_defs().iter().filter(|d| d.kind == EffectKind::Video && !d.intrinsic && !filmcraft_project::graphic::is_layer_id(d.id)).collect()
}

#[test]
fn effects_list_reports_folders_and_badges() {
    let mut s = demo();
    let list = s.execute("effects.list", json!({})).unwrap();
    let arr = list.as_array().unwrap();
    let find = |id: &str| arr.iter().find(|e| e["id"] == id).cloned().unwrap_or_else(|| panic!("{id} not listed"));
    let wsz = find("warp_stabilizer");
    assert_eq!(wsz["folder"], "Distort");
    assert_eq!(wsz["category"], json!(["Video Effects", "Distort"]));
    assert_eq!(find("vr_rotate_sphere")["folder"], "Immersive Video");
    assert_eq!(find("vr_insta360_reframe")["folder"], "Immersive Video");
    assert_eq!(find("crop")["category"], json!(["Legacy", "Video Effects"]));
    assert_eq!(find("luma_key")["badges"], json!({"accelerated": true, "float32": true, "yuv": true}));
    assert_eq!(find("lens_flare")["folder"], "Lights & Glows");
    let n =
        arr.iter().filter(|e| e["kind"] == "Video" && e["category"][0] == "Video Effects" && e["folder"] != "Obsolete" && e["folder"] != Value::Null).count();
    assert_eq!(n, 94, "Premiere 26's Video Effects plus the sphere re-cut");
}

#[test]
fn every_video_effect_applies_by_id_and_name_and_renders() {
    let mut s = demo();
    let clip = first_v1_clip(&s);
    for d in video_effects() {
        s.execute("effects.apply", json!({"clips": [clip], "effect": d.id})).unwrap_or_else(|e| panic!("{}: {e}", d.id));
        let it = s.active_sequence().unwrap().find_item(filmcraft_project::ClipId(clip)).unwrap().1.clone();
        let inst = it.effects.iter().find(|e| e.effect == d.id).unwrap_or_else(|| panic!("{} not on the clip", d.id));
        // auto points were resolved against the frame
        for (k, p) in &inst.params {
            if let Some(v) = p.value.as_vec2() {
                assert!(v.x.is_finite() && v.y.is_finite(), "{}.{k} unresolved", d.id);
            }
        }
        if !matches!(d.id, "warp_stabilizer" | "auto_reframe" | "echo") {
            let img = s.render_program(0.125).unwrap();
            assert!(img.px.iter().all(|v| v.is_finite()), "{} rendered non-finite pixels", d.id);
        }
        s.execute("edit.undo", json!({})).unwrap();
        assert!(s.active_sequence().unwrap().find_item(filmcraft_project::ClipId(clip)).unwrap().1.effects.iter().all(|e| e.effect != d.id), "{} undo", d.id);
        // by display name (MCP / CLI agents use either)
        s.execute("effects.apply", json!({"clips": [clip], "effect": d.name})).unwrap_or_else(|e| panic!("{} by name: {e}", d.name));
        s.execute("edit.undo", json!({})).unwrap();
    }
}

/// #1545: `effects.apply` reports each instance it added (clip, index, effect id), like
/// `timeline.place` reports its clips, so agents need no `sequence_inspect` round trip.
#[test]
fn effects_apply_reports_where_each_instance_landed() {
    let mut s = demo();
    let clip = first_v1_clip(&s);
    let r = s.execute("effects.apply", json!({"clips": [clip], "effect": "gaussian_blur"})).unwrap();
    let applied = r["applied"].as_array().unwrap();
    assert_eq!(applied.len(), 1);
    assert_eq!(applied[0]["clip"].as_u64(), Some(clip));
    assert_eq!(applied[0]["effect"], "gaussian_blur");
    let idx = applied[0]["index"].as_u64().unwrap() as usize;
    let it = s.active_sequence().unwrap().find_item(filmcraft_project::ClipId(clip)).unwrap().1.clone();
    assert_eq!(it.effects.get(idx).map(|e| e.effect.as_str()), Some("gaussian_blur"), "index addresses the new instance");
    // the index is usable straight away
    s.execute("effects.remove", json!({"clip": clip, "index": idx})).unwrap();
    assert!(s.active_sequence().unwrap().find_item(filmcraft_project::ClipId(clip)).unwrap().1.effects.iter().all(|e| e.effect != "gaussian_blur"));

    // several clips: one entry per instance, each with its own clip id
    let clips: Vec<u64> = s.active_sequence().unwrap().video_tracks[0].items.iter().map(|c| c.id.0).collect();
    assert!(clips.len() >= 2, "demo project has several V1 clips");
    let r = s.execute("effects.apply", json!({"clips": clips, "effect": "invert"})).unwrap();
    let got: Vec<u64> = r["applied"].as_array().unwrap().iter().map(|a| a["clip"].as_u64().unwrap()).collect();
    assert_eq!(got, clips);

    // audio effects report the (retargeted) audio clip, not the video clip that was passed
    let video = first_v1_clip(&s);
    let r = s.execute("effects.apply", json!({"clips": [video], "effect": "parametric_eq"})).unwrap();
    let a = &r["applied"].as_array().unwrap()[0];
    let target = a["clip"].as_u64().unwrap();
    let (track, item) = s.active_sequence().unwrap().find_item(filmcraft_project::ClipId(target)).unwrap();
    assert_eq!(s.active_sequence().unwrap().track(track).unwrap().kind, filmcraft_project::TrackKind::Audio);
    assert_eq!(item.effects.get(a["index"].as_u64().unwrap() as usize).map(|e| e.effect.as_str()), Some("parametric_eq"));
}

#[test]
fn new_effect_params_are_settable_and_keyframable() {
    let mut s = demo();
    let clip = first_v1_clip(&s);
    s.execute("effects.apply", json!({"clips": [clip], "effect": "Turbulent Displace"})).unwrap();
    s.execute("effects.setParam", json!({"clip": clip, "effect": "turbulent_displace", "param": "amount", "value": 120.0})).unwrap();
    s.execute("effects.toggleAnimation", json!({"clip": clip, "effect": "turbulent_displace", "param": "evolution"})).unwrap();
    s.execute("effects.apply", json!({"clips": [clip], "effect": "simple_text"})).unwrap();
    s.execute("effects.setParam", json!({"clip": clip, "effect": "simple_text", "param": "text", "value": "Hello"})).unwrap();
    s.execute("effects.apply", json!({"clips": [clip], "effect": "corner_pin"})).unwrap();
    s.execute("effects.setParam", json!({"clip": clip, "effect": "corner_pin", "param": "upper_left", "value": [100.0, 50.0]})).unwrap();
    let it = s.active_sequence().unwrap().find_item(filmcraft_project::ClipId(clip)).unwrap().1.clone();
    let td = it.effect("turbulent_displace").unwrap();
    assert_eq!(td.param("amount").unwrap().value.as_f64(), Some(120.0));
    assert!(td.param("evolution").unwrap().is_animated() || !td.param("evolution").unwrap().keyframes.is_empty());
    assert!(matches!(&it.effect("simple_text").unwrap().param("text").unwrap().value, filmcraft_project::ParamValue::Text(t) if t == "Hello"));
    let img = s.render_program(0.125).unwrap();
    assert!(img.px.iter().all(|v| v.is_finite()));
}

#[test]
fn ultra_key_setting_writes_aggressive_parameters() {
    let mut s = demo();
    let clip = first_v1_clip(&s);
    s.execute("effects.apply", json!({"clips": [clip], "effect": "ultra_key"})).unwrap();
    s.execute("effects.setParam", json!({"clip": clip, "effect": "ultra_key", "param": "setting", "value": 2})).unwrap();
    let e = s.active_sequence().unwrap().find_item(filmcraft_project::ClipId(clip)).unwrap().1.effect("ultra_key").unwrap();
    assert_eq!(e.param("tolerance").unwrap().value.as_f64(), Some(90.0));
    assert_eq!(e.param("pedestal").unwrap().value.as_f64(), Some(50.0));
    assert_eq!(e.param("choke").unwrap().value.as_f64(), Some(10.0));
    assert_eq!(e.param("contrast").unwrap().value.as_f64(), Some(10.0));
}

/// Sphere Reframe's Fov Preset and Frame Range drive FOV Control and Distortion Control one way.
#[test]
fn insta360_frame_range_syncs_fov_and_distortion() {
    let mut s = demo();
    let clip = first_v1_clip(&s);
    s.execute("effects.apply", json!({"clips": [clip], "effect": "vr_insta360_reframe"})).unwrap();
    let get = |s: &Session, p: &str| {
        s.active_sequence()
            .unwrap()
            .find_item(filmcraft_project::ClipId(clip))
            .unwrap()
            .1
            .effect("vr_insta360_reframe")
            .unwrap()
            .param(p)
            .unwrap()
            .value
            .clone()
    };
    // The Fov Preset writes all three (UltraWide: 73 / 79° / 0.95).
    s.execute("effects.setParam", json!({"clip": clip, "effect": "vr_insta360_reframe", "param": "fov_preset", "value": "UltraWide"})).unwrap();
    let e = s.active_sequence().unwrap().find_item(filmcraft_project::ClipId(clip)).unwrap().1.effect("vr_insta360_reframe").unwrap().clone();
    assert_eq!(e.param("frame_range").unwrap().value.as_f64(), Some(73.0));
    assert_eq!(e.param("fov").unwrap().value.as_f64(), Some(79.0));
    assert_eq!(e.param("distortion").unwrap().value.as_f64(), Some(0.95));
    // Dragging Frame Range re-derives FOV Control and Distortion Control linearly (50 → 85° / 0.475).
    s.execute("effects.setParam", json!({"clip": clip, "effect": "vr_insta360_reframe", "param": "frame_range", "value": 50.0})).unwrap();
    let e = s.active_sequence().unwrap().find_item(filmcraft_project::ClipId(clip)).unwrap().1.effect("vr_insta360_reframe").unwrap().clone();
    assert_eq!(e.param("fov").unwrap().value.as_f64(), Some(85.0));
    assert_eq!(e.param("distortion").unwrap().value.as_f64(), Some(0.475));
    // Editing the two derived parameters one way never moves Frame Range back.
    s.execute("effects.setParam", json!({"clip": clip, "effect": "vr_insta360_reframe", "param": "fov", "value": 120.0})).unwrap();
    s.execute("effects.setParam", json!({"clip": clip, "effect": "vr_insta360_reframe", "param": "distortion", "value": 0.2})).unwrap();
    assert_eq!(get(&s, "frame_range").as_f64(), Some(50.0), "frame_range is not pulled back");
    assert_eq!(get(&s, "fov").as_f64(), Some(120.0));
    assert_eq!(get(&s, "distortion").as_f64(), Some(0.2));
}

/// The one-way sync must never destroy animation the user put on the derived
/// controls, and Frame Range - whose keyframes the renderer never reads - is not
/// keyframable at all.
#[test]
fn insta360_sync_keeps_keyframed_targets() {
    let mut s = demo();
    let clip = first_v1_clip(&s);
    s.execute("effects.apply", json!({"clips": [clip], "effect": "vr_insta360_reframe"})).unwrap();
    let prm = |s: &Session, p: &str| {
        s.active_sequence().unwrap().find_item(filmcraft_project::ClipId(clip)).unwrap().1.effect("vr_insta360_reframe").unwrap().param(p).unwrap().clone()
    };
    let tick = filmcraft_time::Tick;
    s.execute("effects.setParam", json!({"clip": clip, "effect": "vr_insta360_reframe", "param": "fov", "value": 100.0, "keyframe": true, "time": tick(0).0}))
        .unwrap();
    s.execute("effects.setParam", json!({"clip": clip, "effect": "vr_insta360_reframe", "param": "fov", "value": 140.0, "keyframe": true, "time": tick(1).0}))
        .unwrap();
    // Moving Frame Range now re-derives the static Distortion Control but leaves
    // the animated FOV Control alone - it used to wipe its keyframes.
    s.execute("effects.setParam", json!({"clip": clip, "effect": "vr_insta360_reframe", "param": "frame_range", "value": 50.0})).unwrap();
    let fov = prm(&s, "fov");
    assert_eq!(fov.keyframes.len(), 2, "the sync cleared keyframes the user animated");
    assert_eq!(fov.keyframes[1].value.as_f64(), Some(140.0));
    assert_eq!(prm(&s, "distortion").value.as_f64(), Some(0.475), "static targets still sync");
    // Frame Range drives the other two and is never read at render time, so the
    // stopwatch is refused on it.
    let r = s.execute(
        "effects.setParam",
        json!({"clip": clip, "effect": "vr_insta360_reframe", "param": "frame_range", "value": 60.0, "keyframe": true, "time": tick(0).0}),
    );
    assert!(r.is_err(), "frame_range must not invite keyframes the renderer never reads");
}

/// #5195: the stopwatch cannot turn on animation for a parameter whose definition
/// marks it non-animatable - Frame Range's keyframes are never read by the renderer,
/// so toggling it on offered motion it would not show. Toggling *off* stays
/// available: projects saved before the rule may carry its keyframes.
#[test]
fn insta360_frame_range_stopwatch_is_refused() {
    let mut s = demo();
    let clip = first_v1_clip(&s);
    s.execute("effects.apply", json!({"clips": [clip], "effect": "vr_insta360_reframe"})).unwrap();
    let r = s.execute("effects.toggleAnimation", json!({"clip": clip, "effect": "vr_insta360_reframe", "param": "frame_range"}));
    assert!(r.is_err(), "the stopwatch must refuse a non-animatable parameter");
    let get = |s: &Session, p: &str| {
        s.active_sequence()
            .unwrap()
            .find_item(filmcraft_project::ClipId(clip))
            .unwrap()
            .1
            .effect(p)
            .map(|e| e.params.get("frame_range").unwrap().keyframes.clone())
    };
    assert!(get(&s, "vr_insta360_reframe").unwrap().is_empty(), "the refusal left no keyframe");
    // A derived control the renderer does evaluate still toggles both ways.
    s.execute("effects.toggleAnimation", json!({"clip": clip, "effect": "vr_insta360_reframe", "param": "fov"})).unwrap();
    // An older project that carries a Frame Range keyframe can still clear it.
    let sid = s.state.active_sequence.unwrap();
    {
        let (_, it) = std::sync::Arc::make_mut(&mut s.project).sequence_mut(sid).unwrap().find_item_mut(filmcraft_project::ClipId(clip)).unwrap();
        it.effects
            .iter_mut()
            .find(|e| e.effect == "vr_insta360_reframe")
            .unwrap()
            .params
            .get_mut("frame_range")
            .unwrap()
            .keyframes
            .push(filmcraft_project::Keyframe::new(filmcraft_time::Tick(0), filmcraft_project::ParamValue::Float(50.0)));
    }
    s.execute("effects.toggleAnimation", json!({"clip": clip, "effect": "vr_insta360_reframe", "param": "frame_range"})).unwrap();
    assert!(
        s.active_sequence()
            .unwrap()
            .find_item(filmcraft_project::ClipId(clip))
            .unwrap()
            .1
            .effect("vr_insta360_reframe")
            .unwrap()
            .params
            .get("frame_range")
            .unwrap()
            .keyframes
            .is_empty(),
        "the stale stopwatch turned off"
    );
}

/// #5195: resetting a driving parameter must re-derive its companions, the same
/// one-way rule `effects.setParam` applies - a reset that restores Frame Range
/// while leaving FOV Control on the dragged value shows a window that is neither
/// the default nor what the slider implies.
#[test]
fn insta360_reset_param_rederives_companions() {
    let mut s = demo();
    let clip = first_v1_clip(&s);
    s.execute("effects.apply", json!({"clips": [clip], "effect": "vr_insta360_reframe"})).unwrap();
    let at = |s: &Session, p: &str| {
        s.active_sequence()
            .unwrap()
            .find_item(filmcraft_project::ClipId(clip))
            .unwrap()
            .1
            .effect("vr_insta360_reframe")
            .unwrap()
            .param(p)
            .unwrap()
            .value
            .as_f64()
    };
    s.execute("effects.setParam", json!({"clip": clip, "effect": "vr_insta360_reframe", "param": "frame_range", "value": 50.0})).unwrap();
    assert_eq!(at(&s, "fov"), Some(85.0), "the drag synced first");
    // Resetting Frame Range restores its default AND re-derives the static targets
    // from it - here the derivation, not the (untouched) definitions' own defaults.
    s.execute("effects.resetParam", json!({"clip": clip, "effect": "vr_insta360_reframe", "param": "frame_range"})).unwrap();
    let (fov, distortion) = filmcraft_project::effect::insta360_frame_range(76.0);
    assert_eq!(at(&s, "frame_range"), Some(76.0));
    assert_eq!(at(&s, "fov"), Some(fov), "reset left a stale FOV Control");
    assert_eq!(at(&s, "distortion"), Some(distortion), "reset left a stale Distortion Control");
    // Resetting the preset re-derives all three from the preset row (MegaView).
    s.execute("effects.resetParam", json!({"clip": clip, "effect": "vr_insta360_reframe", "param": "fov_preset"})).unwrap();
    let e = s.active_sequence().unwrap().find_item(filmcraft_project::ClipId(clip)).unwrap().1.effect("vr_insta360_reframe").unwrap().clone();
    assert_eq!(e.param("fov").unwrap().value.as_f64(), Some(88.0), "the preset reset must write its own row");
    assert_eq!(e.param("distortion").unwrap().value.as_f64(), Some(0.95));
    // A target the user animated keeps its own story through either reset.
    s.execute(
        "effects.setParam",
        json!({"clip": clip, "effect": "vr_insta360_reframe", "param": "fov", "value": 100.0, "keyframe": true, "time": filmcraft_time::Tick(0).0}),
    )
    .unwrap();
    s.execute("effects.resetParam", json!({"clip": clip, "effect": "vr_insta360_reframe", "param": "frame_range"})).unwrap();
    let e = s.active_sequence().unwrap().find_item(filmcraft_project::ClipId(clip)).unwrap().1.effect("vr_insta360_reframe").unwrap().clone();
    assert_eq!(e.param("fov").unwrap().keyframes.len(), 1, "a reset cleared animated FOV Control");
}

/// #5195: the driver of a reset may be ANIMATED (Fov Preset is a stepped choice
/// the timeline diamond does keyframe): resetParam lands the default in the
/// keyframe at the reset time and leaves the parameter's static value on the old
/// preset, so the companion sync must derive from the value at that time, not
/// from the stale static one.
#[test]
fn insta360_reset_of_an_animated_driver_syncs_its_keyframed_value() {
    let mut s = demo();
    let clip = first_v1_clip(&s);
    s.execute("effects.apply", json!({"clips": [clip], "effect": "vr_insta360_reframe"})).unwrap();
    s.execute("effects.setParam", json!({"clip": clip, "effect": "vr_insta360_reframe", "param": "fov_preset", "value": "UltraWide"})).unwrap();
    // A keyframe at the playhead (stepped choice: allowed), then a setParam moves
    // that keyframe's value without touching the old static one.
    s.execute("effects.addKeyframe", json!({"clip": clip, "effect": "vr_insta360_reframe", "param": "fov_preset"})).unwrap();
    s.execute("effects.setParam", json!({"clip": clip, "effect": "vr_insta360_reframe", "param": "fov_preset", "value": "Dewarp"})).unwrap();
    // Reset writes the DEFAULT through the keyframe: the companions must follow
    // MegaView (76 / 88 / 0.95), not the static UltraWide (73 / 79) or Dewarp (52 / 100).
    s.execute("effects.resetParam", json!({"clip": clip, "effect": "vr_insta360_reframe", "param": "fov_preset"})).unwrap();
    let e = s.active_sequence().unwrap().find_item(filmcraft_project::ClipId(clip)).unwrap().1.effect("vr_insta360_reframe").unwrap().clone();
    assert_eq!(e.param("fov_preset").unwrap().keyframes.len(), 1, "the animated preset stayed animated");
    assert_eq!(e.param("fov").unwrap().value.as_f64(), Some(88.0), "the companions derived from the stale static preset");
    assert_eq!(e.param("frame_range").unwrap().value.as_f64(), Some(76.0));
    assert_eq!(e.param("distortion").unwrap().value.as_f64(), Some(0.95));
}

/// #5195: the timeline keyframe diamond obeys the same `animatable` rule as the
/// stopwatch and `effects.setParam` - Frame Range never gains keyframes the
/// renderer will not read - while removing one a legacy project carries stays
/// possible.
#[test]
fn insta360_add_keyframe_respects_non_animatable() {
    let mut s = demo();
    let clip = first_v1_clip(&s);
    s.execute("effects.apply", json!({"clips": [clip], "effect": "vr_insta360_reframe"})).unwrap();
    let r = s.execute("effects.addKeyframe", json!({"clip": clip, "effect": "vr_insta360_reframe", "param": "frame_range", "time": filmcraft_time::Tick(1).0}));
    assert!(r.is_err(), "a keyframe must not be added to a non-animatable parameter");
    let e_kf = s
        .active_sequence()
        .unwrap()
        .find_item(filmcraft_project::ClipId(clip))
        .unwrap()
        .1
        .effect("vr_insta360_reframe")
        .unwrap()
        .params
        .get("frame_range")
        .unwrap()
        .keyframes
        .clone();
    assert!(e_kf.is_empty(), "the refusal left a keyframe behind");
    // an animatable control gains one the same way (remove at the same time is its toggle)
    s.execute("effects.addKeyframe", json!({"clip": clip, "effect": "vr_insta360_reframe", "param": "fov", "time": filmcraft_time::Tick(1).0})).unwrap();
    // a stale Frame Range keyframe from before the rule can still be removed
    // here; the command keys on the clip's effect time, not the raw tick
    let sid = s.state.active_sequence.unwrap();
    let mt0 = s.active_sequence().unwrap().find_item(filmcraft_project::ClipId(clip)).unwrap().1.effect_time_at(filmcraft_time::Tick(0));
    {
        let (_, it) = std::sync::Arc::make_mut(&mut s.project).sequence_mut(sid).unwrap().find_item_mut(filmcraft_project::ClipId(clip)).unwrap();
        it.effects
            .iter_mut()
            .find(|e| e.effect == "vr_insta360_reframe")
            .unwrap()
            .params
            .get_mut("frame_range")
            .unwrap()
            .keyframes
            .push(filmcraft_project::Keyframe::new(mt0, filmcraft_project::ParamValue::Float(50.0)));
    }
    let r = s
        .execute("effects.addKeyframe", json!({"clip": clip, "effect": "vr_insta360_reframe", "param": "frame_range", "time": filmcraft_time::Tick(0).0}))
        .unwrap();
    assert_eq!(r["added"], false, "the diamond toggled off, not added");
    let e = s.active_sequence().unwrap().find_item(filmcraft_project::ClipId(clip)).unwrap().1.effect("vr_insta360_reframe").unwrap().clone();
    assert!(e.params.get("frame_range").unwrap().keyframes.is_empty(), "the legacy keyframe survived");
}

/// #2679: a value outside a parameter's range (the `min`/`max` `effects.list` publishes) is
/// refused with the range, not saved: gamma 1.1 meant as a factor turned the frame black.
#[test]
fn set_param_refuses_values_outside_the_range() {
    let mut s = demo();
    let clip = s.active_sequence().unwrap().video_tracks[0].items[0].id.0;
    s.execute("effects.apply", json!({"clips": [clip], "effect": "levels"})).unwrap();
    s.execute("effects.apply", json!({"clips": [clip], "effect": "lumetri"})).unwrap();
    let before = s.project.to_json();
    let undo = s.history.undo.len();
    for (effect, param, value) in
        [("levels", "gamma", 1.1), ("lumetri", "saturation", 500.0), ("lumetri", "vibrance", -300.0), ("opacity", "opacity", 250.0), ("motion", "scale", -50.0)]
    {
        let e = s.execute("effects.setParam", json!({"clip": clip, "effect": effect, "param": param, "value": value})).unwrap_err().to_string();
        assert!(e.contains("must be between"), "{effect}.{param} = {value}: {e}");
    }
    assert_eq!(s.project.to_json(), before, "nothing was saved");
    assert_eq!(s.history.undo.len(), undo);
    // the ends of the range and values inside it still go in
    s.execute("effects.setParam", json!({"clip": clip, "effect": "levels", "param": "gamma", "value": 110})).unwrap();
    s.execute("effects.setParam", json!({"clip": clip, "effect": "opacity", "param": "opacity", "value": 100})).unwrap();
    s.execute("effects.setParam", json!({"clip": clip, "effect": "opacity", "param": "opacity", "value": 0})).unwrap();
}

/// #3455: a transition's requested duration can be longer than the clips around its cut can
/// hold (a crossfade next to a few-frames clip). The preview clamps it to what the cut
/// supports — the same invariant `edit::transitions::set_span` keeps when a transition is
/// resized: a centered crossfade spends half its length on each side of the cut, a fade to
/// or from nothing spends it all on its one side.
#[test]
fn transition_duration_is_clamped_to_what_the_cut_supports() {
    use crate::commands::preview_transition;
    use filmcraft_project::TrackKind;
    use filmcraft_time::Tick;

    let mut s = demo();
    let sid = s.state.active_sequence.unwrap();
    let frame = s.sequence_rate().tick_of(1);
    let track = s.active_sequence().unwrap().video_tracks[0].id;
    let clips: Vec<_> = s.active_sequence().unwrap().video_tracks[0].items.iter().map(|c| c.id).collect();
    assert!(clips.len() >= 2, "the demo project needs at least two V1 clips for this test");
    let (first_start, second_end) = {
        let items = &mut std::sync::Arc::make_mut(&mut s.project).sequence_mut(sid).unwrap().track_mut(track).unwrap().items;
        items[0].duration = frame.mul_ratio(3, 1);
        let cut = items[0].end();
        items[1].start = cut;
        (items[0].start, items[1].end())
    };

    // Crossfade at the cut between the 3-frame clip and its neighbour: the short side
    // caps a centered transition at 2 x 3 frames.
    let (_, planned) = preview_transition(&s, &json!({"effect": "cross_dissolve", "clip": clips[1].0, "edge": "in", "frames": 999}), TrackKind::Video).unwrap();
    assert_eq!(planned.duration, frame.mul_ratio(6, 1), "clamped to twice the short side of the cut");
    assert!(planned.start >= first_start && planned.start + planned.duration <= second_end, "the clamped transition stays inside the clips");

    // The same cut with a request that fits is untouched.
    let (_, planned) = preview_transition(&s, &json!({"effect": "cross_dissolve", "clip": clips[1].0, "edge": "in", "frames": 4}), TrackKind::Video).unwrap();
    assert_eq!(planned.duration, frame.mul_ratio(4, 1));

    // A fade to nothing on the last clip's out edge can be at most that clip's own length.
    let (last, last_duration) = {
        let items = &s.active_sequence().unwrap().track(track).unwrap().items;
        (items.last().unwrap().id, items.last().unwrap().duration)
    };
    let (_, planned) = preview_transition(&s, &json!({"effect": "cross_dissolve", "clip": last.0, "edge": "out", "frames": 9999}), TrackKind::Video).unwrap();
    assert_eq!(planned.duration, last_duration, "a one-sided fade is clamped to its clip");
    let items = s.active_sequence().unwrap().track(track).unwrap().items.clone();
    let last = items.last().unwrap();
    assert_eq!(planned.start + planned.duration, last.end());

    // A cut whose clips are shorter than a frame is refused instead of creating a
    // zero-length transition.
    let items = &mut std::sync::Arc::make_mut(&mut s.project).sequence_mut(sid).unwrap().track_mut(track).unwrap().items;
    items[1].duration = Tick(0);
    let err =
        preview_transition(&s, &json!({"effect": "cross_dissolve", "clip": clips[1].0, "edge": "in", "frames": 5}), TrackKind::Video).unwrap_err().to_string();
    assert!(err.contains("no room for a transition"), "{err}");
}
