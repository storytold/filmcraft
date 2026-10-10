use super::*;

#[test]
fn invalid_preview_scales_are_rejected_without_allocating() {
    let mut s = Session::default();
    s.execute("file.newSequence", serde_json::json!({"width":160,"height":90})).unwrap();
    for scale in [0.0, -1.0, f32::NAN, f32::INFINITY, f32::MAX] {
        assert!(s.render_program(scale).is_none());
        assert!(s.render_program_working(scale).is_err());
    }
    assert!(s.try_render_program_at(f32::NAN, Tick::ZERO).unwrap_err().to_string().contains("finite"), "the reason is reported");
    assert_eq!(s.render_program(0.5).unwrap().w, 80);
}
use serde_json::json;

fn demo() -> Session {
    let mut s = Session::default();
    s.execute("file.openDemoProject", json!({})).unwrap();
    s
}

fn clip_ids(s: &Session, track: usize) -> Vec<u64> {
    s.active_sequence().unwrap().video_tracks[track].items.iter().map(|i| i.id.0).collect()
}

#[test]
fn demo_project_is_valid_and_renders() {
    let s = demo();
    let q = s.active_sequence().unwrap();
    q.check().unwrap();
    assert_eq!(q.video_tracks[0].items.len(), 6);
    assert!(q.duration() > Tick::ZERO);
    let img = s.render_program(0.125).unwrap();
    assert_eq!((img.w, img.h), (240, 135));
    assert!(img.px.chunks(4).any(|p| p[3] > 0.9));
}

/// Ordinary and configured multi-frame offsets remain valid.
#[test]
fn frame_steps_in_new_custom_rate_sequences_move_in_the_requested_direction() {
    for fps in [31.0, 23.98, 24.999] {
        let mut s = Session::default();
        s.execute("file.newSequence", json!({"fps": fps})).unwrap();
        let rate = s.sequence_rate();
        s.execute("playhead.set", json!({"frame": 10})).unwrap();
        assert_eq!(rate.frame_at(s.playhead()), 10);
        for frame in 11..=42 {
            s.execute("playhead.stepForward", json!({})).unwrap();
            assert_eq!(rate.frame_at(s.playhead()), frame);
        }
        for frame in (10..42).rev() {
            s.execute("playhead.stepBack", json!({})).unwrap();
            assert_eq!(rate.frame_at(s.playhead()), frame);
        }
        s.execute("prefs.set", json!({"key": "playback.stepManyFrames", "value": 12})).unwrap();
        s.execute("playhead.stepForward5", json!({})).unwrap();
        assert_eq!(rate.frame_at(s.playhead()), 22);
        s.execute("playhead.stepBack5", json!({})).unwrap();
        assert_eq!(rate.frame_at(s.playhead()), 10);
        s.execute("playhead.step", json!({"frames": -20})).unwrap();
        s.execute("playhead.stepBack", json!({})).unwrap();
        assert_eq!(s.playhead(), Tick::ZERO);
    }
}

#[test]
fn overflowing_frame_steps_are_errors_and_leave_the_playhead_unchanged() {
    let mut s = Session::default();
    s.execute("file.newSequence", json!({"fps": 31.0})).unwrap();
    for start in [10, 0] {
        s.execute("playhead.set", json!({"frame": start})).unwrap();
        let before = s.playhead();
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| s.execute("playhead.step", json!({"frames": i64::MAX}))));
        assert!(result.is_ok(), "an extreme frame offset must not panic");
        assert!(matches!(result.unwrap(), Err(EngineError::BadParams { .. })));
        assert_eq!(s.playhead(), before);
    }
    s.execute("playhead.step", json!({"frames": i64::MIN})).unwrap();
    assert_eq!(s.playhead(), Tick::ZERO);
}

#[test]
fn add_edit_undo_redo() {
    let mut s = demo();
    let before = clip_ids(&s, 0).len();
    s.execute("playhead.set", json!({"seconds": 2.0})).unwrap();
    s.execute("sequence.addEditAllTracks", json!({})).unwrap();
    assert_eq!(clip_ids(&s, 0).len(), before + 1);
    s.execute("edit.undo", json!({})).unwrap();
    assert_eq!(clip_ids(&s, 0).len(), before);
    s.execute("edit.redo", json!({})).unwrap();
    assert_eq!(clip_ids(&s, 0).len(), before + 1);
    assert!(s.execute("edit.redo", json!({})).is_err(), "disabled when nothing to redo");
}

#[test]
fn source_insert_three_point() {
    let mut s = demo();
    let item = s
        .project
        .root
        .children
        .iter()
        .find_map(|c| {
            if let filmcraft_project::BinEntry::Bin(b) = c {
                b.children.first().and_then(|e| if let filmcraft_project::BinEntry::Item(i) = e { Some(*i) } else { None })
            } else {
                None
            }
        })
        .unwrap();
    s.execute("source.open", json!({"item": item.0})).unwrap();
    let r = s.sequence_rate();
    s.execute("project.setMarks", json!({"item": item.0, "in": r.tick_of(24).0, "out": r.tick_of(47).0})).unwrap();
    s.execute("playhead.set", json!({"frame": 0})).unwrap();
    let dur0 = s.active_sequence().unwrap().duration();
    s.execute("source.insert", json!({})).unwrap();
    let dur1 = s.active_sequence().unwrap().duration();
    assert_eq!(dur1 - dur0, r.tick_of(24), "one second inserted, sequence rippled");
    assert_eq!(s.playhead(), r.tick_of(24), "playhead moves to end of edit");
    s.active_sequence().unwrap().check().unwrap();
}

#[test]
fn trim_linked_and_ripple_delete() {
    let mut s = demo();
    let first = s.active_sequence().unwrap().video_tracks[0].items[0].clone();
    s.execute("timeline.trim", json!({"clip": first.id.0, "edge": "out", "mode": "regular", "deltaFrames": -12})).unwrap();
    let q = s.active_sequence().unwrap();
    assert_eq!(q.video_tracks[0].items[0].duration, first.duration - s.sequence_rate().tick_of(12));
    let a = q.audio_tracks[0].items.iter().find(|i| i.link == first.link).unwrap();
    assert_eq!(a.duration, q.video_tracks[0].items[0].duration, "linked audio trimmed too");
    s.execute("timeline.select", json!({"clips": [first.id.0]})).unwrap();
    assert_eq!(s.state.selection.len(), 2, "linked selection adds audio");
    assert!(s.execute("edit.rippleDelete", json!({})).is_err(), "music on sync-locked A2 blocks the ripple");
    s.execute("timeline.setTrack", json!({"track": "A2", "syncLock": false})).unwrap();
    s.execute("edit.rippleDelete", json!({})).unwrap();
    let q = s.active_sequence().unwrap();
    assert_eq!(q.video_tracks[0].items[0].start, s.sequence_rate().tick_of(12), "the trim gap remains");
}

#[test]
fn slide_moves_linked_audio_with_the_video() {
    let mut s = demo();
    let v = s.active_sequence().unwrap().video_tracks[0].items[1].clone();
    s.execute("timeline.slide", json!({"clip": v.id.0, "deltaFrames": 5})).unwrap();
    let q = s.active_sequence().unwrap();
    let (_, nv) = q.find_item(v.id).unwrap();
    let a = q.audio_tracks[0].items.iter().find(|i| i.link == v.link).unwrap();
    assert_eq!(nv.start, v.start + s.sequence_rate().tick_of(5));
    assert_eq!((a.start, a.source_in), (nv.start, nv.source_in), "linked audio slides too and stays in sync");
}

#[test]
fn sequence_parameters_are_bounded_and_failed_changes_are_atomic() {
    let mut s = demo();
    let before = (*s.project).clone();
    let history = s.history.undo.len();
    for command in ["file.newSequence", "sequence.settings"] {
        for params in [
            json!({"width":0}),
            json!({"height":0}),
            json!({"sampleRate":0}),
            json!({"width":4_294_967_360_u64}),
            json!({"height":u64::MAX}),
            json!({"sampleRate":u64::MAX}),
            json!({"width":-1}),
            json!({"width":1.5}),
            json!({"width":32768,"height":16384}),
            json!({"width":1920.25}),
            json!({"fps":1001}),
            json!({"sampleRate":384001}),
        ] {
            assert!(s.execute(command, params.clone()).is_err(), "{command} {params}");
            assert_eq!(*s.project, before);
            assert_eq!(s.history.undo.len(), history);
        }
    }
    for params in [json!({"video":u64::MAX}), json!({"audio":u64::MAX}), json!({"video":257}), json!({"audio":-1})] {
        assert!(s.execute("file.newSequence", params.clone()).is_err(), "{params}");
        assert_eq!(*s.project, before);
    }
    s.execute("file.newSequence", json!({"width":7680,"height":4320,"video":0,"audio":0})).unwrap();
    assert_eq!(s.active_sequence().unwrap().settings.width, 7680, "standard 8K remains supported");
    s.execute("file.newSequence", json!({"width":15360.0,"height":8640.0,"sampleRate":48000.0,"video":1.0,"audio":0})).unwrap();
    assert_eq!(s.active_sequence().unwrap().settings.width, 15360, "16K, and integer-valued floats, are accepted");
    s.execute("sequence.settings", json!({"width":16384,"height":8192})).unwrap();
    assert_eq!(s.active_sequence().unwrap().settings.height, 8192, "a 16384x8192 panorama is accepted");
}

/// The first video clip's Motion Position and Scale (static values).
fn first_motion(s: &Session) -> (u64, (f64, f64), f64) {
    let q = s.active_sequence().unwrap();
    let it = q.video_tracks.iter().flat_map(|t| &t.items).find(|i| i.effect("motion").is_some()).unwrap();
    let m = it.effect("motion").unwrap();
    let p = match m.param("position").unwrap().value {
        filmcraft_project::ParamValue::Vec2(v) => (v.x, v.y),
        _ => panic!("position is a point"),
    };
    let sc = match m.param("scale").unwrap().value {
        filmcraft_project::ParamValue::Float(v) => v,
        _ => panic!("scale is a number"),
    };
    (it.id.0, p, sc)
}

#[test]
fn sequence_settings_scales_motion_with_the_frame_size_in_one_undo_step() {
    let mut s = demo();
    let before = (*s.project).clone();
    let (id, p0, s0) = first_motion(&s);
    assert_eq!((s.active_sequence().unwrap().settings.width, s.active_sequence().unwrap().settings.height), (1920, 1080));
    let r = s.execute("sequence.settings", json!({"width":1280,"height":720,"scaleMotion":true})).unwrap();
    assert!(r["scaledClips"].as_u64().unwrap() >= 1, "{r}");
    let (id1, p1, s1) = first_motion(&s);
    assert_eq!(id1, id);
    assert!((p1.0 - p0.0 * 2.0 / 3.0).abs() < 1e-9 && (p1.1 - p0.1 * 2.0 / 3.0).abs() < 1e-9, "{p0:?} -> {p1:?}");
    assert!((s1 - s0 * 2.0 / 3.0).abs() < 1e-9, "{s0} -> {s1}");
    let after = (*s.project).clone();
    s.execute("edit.undo", json!({})).unwrap();
    assert_eq!(*s.project, before, "size and Motion come back in one undo step");
    s.execute("edit.redo", json!({})).unwrap();
    assert_eq!(*s.project, after);
    // without the option the frame size changes alone (the command's default, as before)
    let r = s.execute("sequence.settings", json!({"width":3840,"height":2160})).unwrap();
    assert_eq!(r["scaledClips"], json!(0));
    assert_eq!(first_motion(&s), (id, p1, s1));
    assert_eq!(s.active_sequence().unwrap().settings.width, 3840);
}

#[test]
fn sequence_settings_timecode_render_quality_and_colour() {
    let mut s = demo();
    s.execute("sequence.settings", json!({"fps":29.97,"dropFrame":true,"maxRenderQuality":true})).unwrap();
    let st = s.active_sequence().unwrap().settings.clone();
    assert_eq!(st.frame_rate, FrameRate::FPS_29_97);
    assert!(st.drop_frame && st.max_render_quality);
    s.execute("sequence.settings", json!({"dropFrame":false})).unwrap();
    assert!(!s.active_sequence().unwrap().settings.drop_frame);
    // drop-frame only exists for the NTSC rates; a refused change leaves everything as it was
    let before = (*s.project).clone();
    let history = s.history.undo.len();
    for params in [json!({"fps":25,"dropFrame":true}), json!({"workingSpace":"rec2020-log"}), json!({"fps":25,"dropFrame":true,"width":1280})] {
        assert!(s.execute("sequence.settings", params.clone()).is_err(), "{params}");
        assert_eq!(*s.project, before, "{params}");
        assert_eq!(s.history.undo.len(), history);
    }
    s.execute("sequence.settings", json!({"workingSpace":"rec2100-pq","wideGamut":true,"autoToneMap":true})).unwrap();
    let st = s.active_sequence().unwrap().settings.clone();
    assert_eq!(st.color.working, filmcraft_color::WorkingSpace::Rec2100Pq);
    assert!(st.color.wide_gamut && st.color.auto_tone_map);
    assert_eq!(st.working_space, "Rec. 2100 PQ");
    // disabled without a sequence
    let mut empty = Session::default();
    assert!(empty.execute("sequence.settings", json!({"width":1280})).is_err());
}

#[test]
fn zero_fps_is_refused() {
    let mut s = demo();
    let rate = s.sequence_rate();
    assert!(s.execute("file.newSequence", json!({"name": "z", "fps": 0})).is_err());
    assert!(s.execute("sequence.settings", json!({"fps": 0})).is_err());
    assert!(s.execute("sequence.settings", json!({"fps": -24})).is_err());
    assert_eq!(s.sequence_rate(), rate, "the active sequence keeps its rate");
    s.execute("markers.markIn", json!({"time": 0})).unwrap();
}

#[test]
fn effects_and_keyframes() {
    let mut s = demo();
    let c = s.active_sequence().unwrap().video_tracks[0].items[1].id.0;
    s.execute("effects.apply", json!({"clips": [c], "effect": "Gaussian Blur"})).unwrap();
    s.execute("effects.setParam", json!({"clip": c, "effect": "gaussian_blur", "param": "blurriness", "value": 25.0})).unwrap();
    let q = s.active_sequence().unwrap();
    let it = q.find_item(filmcraft_project::ClipId(c)).unwrap().1;
    assert_eq!(it.effects[0].effect, "gaussian_blur", "standard effects render before intrinsics");
    assert_eq!(it.effects[0].params["blurriness"].value, filmcraft_project::ParamValue::Float(25.0));
    s.execute("effects.toggleAnimation", json!({"clip": c, "effect": "motion", "param": "scale"})).unwrap();
    let it = s.active_sequence().unwrap().find_item(filmcraft_project::ClipId(c)).unwrap().1.clone();
    assert!(it.effect("motion").unwrap().params["scale"].is_animated());
}

/// `effects.addKeyframe` is the keyframe diamond of Effect Controls and Properties: it adds a
/// keyframe at the playhead or removes the one there, and the value then follows the keyframes
/// that are left (Premiere's Add/Remove Keyframe), unlike the stopwatch, which drops them all.
#[test]
fn add_keyframe_toggles_the_keyframe_at_the_playhead() {
    let mut s = demo();
    let c = s.active_sequence().unwrap().video_tracks[0].items[0].id.0;
    let scale = |s: &Session| {
        let it = s.active_sequence().unwrap().find_item(filmcraft_project::ClipId(c)).unwrap().1;
        let p = &it.effect("motion").unwrap().params["scale"];
        (p.keyframes.len(), p.f64_at(it.source_time_at(s.playhead())))
    };
    let key = json!({"clip": c, "effect": "motion", "param": "scale"});
    s.execute("playhead.set", json!({"seconds": 0.5})).unwrap();
    s.execute("effects.addKeyframe", key.clone()).unwrap();
    assert_eq!(scale(&s), (1, 100.0), "the first keyframe turns animation on");
    s.execute("playhead.set", json!({"seconds": 2.5})).unwrap();
    s.execute("effects.addKeyframe", key.clone()).unwrap();
    s.execute("effects.setParam", json!({"clip": c, "effect": "motion", "param": "scale", "value": 50.0})).unwrap();
    assert_eq!(scale(&s), (2, 50.0));
    // on a keyframe: only that keyframe goes, and Scale is the other keyframe's 100 again
    s.execute("effects.addKeyframe", key.clone()).unwrap();
    assert_eq!(scale(&s), (1, 100.0));
    s.undo();
    assert_eq!(scale(&s), (2, 50.0));
    s.redo();
    assert_eq!(scale(&s), (1, 100.0));
    // parameters that name nothing are errors and change nothing
    for bad in [
        json!({}),
        json!({"clip": c}),
        json!({"clip": u64::MAX, "param": "scale"}),
        json!({"clip": c, "effect": "nope", "param": "scale"}),
        json!({"clip": c, "effect": u64::MAX, "param": "scale"}),
        json!({"clip": c, "effect": "motion", "param": "nope"}),
        json!({"clip": c, "effect": "motion", "param": "scale", "mask": 7}),
    ] {
        assert!(s.execute("effects.addKeyframe", bad.clone()).is_err(), "{bad}");
    }
    assert_eq!(scale(&s), (1, 100.0));
    // the stopwatch ends the animation and keeps the value at the playhead
    s.execute("effects.toggleAnimation", key).unwrap();
    assert_eq!(scale(&s), (0, 100.0));
}

/// Dragging the Volume line or one of its keyframes in the timeline (#223) sends a keyframe edit
/// every frame; `merge` keeps the whole drag one undo step and `begin` starts the next one.
#[test]
fn keyframe_drags_are_one_undo_step() {
    let mut s = demo();
    let c = s.active_sequence().unwrap().audio_tracks[0].items[0].id.0;
    let level = |s: &Session| -> Vec<(i64, f64)> {
        let it = s.active_sequence().unwrap().find_item(filmcraft_project::ClipId(c)).unwrap().1;
        it.effect("volume").unwrap().params["level"].keyframes.iter().map(|k| (k.time.0, k.value.as_f64().unwrap())).collect()
    };
    let key = json!({"clip": c, "effect": "volume", "param": "level"});
    for sec in [0.5, 1.5] {
        s.execute("playhead.set", json!({"seconds": sec})).unwrap();
        s.execute("effects.addKeyframe", key.clone()).unwrap();
    }
    let before = level(&s);
    let [(t0, _), (t1, _)] = before[..] else { panic!("two keyframes: {before:?}") };

    // the line between them: both keyframes move together, frame after frame
    for (i, v) in [-1.0, -2.0, -3.0].into_iter().enumerate() {
        for t in [t0, t1] {
            let p = json!({"clip": c, "effect": "volume", "param": "level", "mediaTime": t, "value": v, "merge": true, "begin": i == 0 && t == t0});
            s.execute("effects.setKeyframe", p).unwrap();
        }
    }
    assert_eq!(level(&s), [(t0, -3.0), (t1, -3.0)]);
    s.undo();
    assert_eq!(level(&s), before);

    // one keyframe: time and value in one command
    let mut at = t1;
    for (i, d) in [1000, 2000, 3000].into_iter().enumerate() {
        let p = json!({"clip": c, "effect": "volume", "param": "level", "mediaTime": at, "to": t1 + d, "value": -6.0, "merge": true, "begin": i == 0});
        s.execute("effects.moveKeyframe", p).unwrap();
        at = t1 + d;
    }
    assert_eq!(level(&s), [before[0], (t1 + 3000, -6.0)]);
    s.undo();
    assert_eq!(level(&s), before);

    let bad = json!({"clip": c, "effect": "volume", "param": "level", "mediaTime": t0, "to": t0 + 1, "value": "loud"});
    assert!(s.execute("effects.moveKeyframe", bad).is_err());
    assert_eq!(level(&s), before);
}

#[test]
fn transitions_and_markers() {
    let mut s = demo();
    let q = s.active_sequence().unwrap();
    let cut = q.video_tracks[0].items[2].start;
    s.execute("playhead.set", json!({"time": cut.0})).unwrap();
    s.execute("sequence.applyVideoTransition", json!({"effect": "wipe"})).unwrap();
    let q = s.active_sequence().unwrap();
    assert!(q.video_tracks[0].transitions.iter().any(|t| t.effect.effect == "wipe"));
    let n = q.markers.len();
    s.execute("markers.add", json!({"name": "Test"})).unwrap();
    assert_eq!(s.active_sequence().unwrap().markers.len(), n + 1);
}

#[test]
fn every_video_transition_applies_via_command() {
    let mut s = demo();
    let cut = s.active_sequence().unwrap().video_tracks[0].items[2].start;
    s.execute("playhead.set", json!({"time": cut.0})).unwrap();
    let defs: Vec<_> =
        filmcraft_project::effect_defs().iter().filter(|d| d.kind == filmcraft_project::EffectKind::VideoTransition && d.category[0] != "Obsolete").collect();
    assert_eq!(defs.len(), 84 + 21);
    for (i, d) in defs.iter().enumerate() {
        // alternate id and display-name lookups
        let name = if i % 2 == 0 { d.id } else { d.name };
        let r = s.execute("sequence.applyVideoTransition", json!({"effect": name})).unwrap_or_else(|e| panic!("{}: {e}", d.id));
        let q = s.active_sequence().unwrap();
        let tr = q.video_tracks[0].transitions.iter().find(|t| t.id.0 == r["transition"].as_u64().unwrap()).unwrap();
        assert_eq!(tr.effect.effect, d.id);
        // rendering the middle of the transition works through the engine
        let mid = tr.start + tr.duration.mul_ratio(1, 2);
        s.execute("playhead.set", json!({"time": mid.0})).unwrap();
        s.execute("edit.undo", json!({})).unwrap();
        assert!(s.active_sequence().unwrap().video_tracks[0].transitions.iter().all(|t| t.id.0 != r["transition"].as_u64().unwrap()));
        s.execute("playhead.set", json!({"time": cut.0})).unwrap();
    }
    // names shared with video effects resolve to the transition here
    let r = s.execute("sequence.applyVideoTransition", json!({"effect": "Mosaic"})).unwrap();
    let q = s.active_sequence().unwrap();
    assert!(q.video_tracks[0].transitions.iter().any(|t| t.id.0 == r["transition"].as_u64().unwrap() && t.effect.effect == "mosaic_transition"));
    assert!(s.execute("sequence.applyVideoTransition", json!({"effect": "Constant Power"})).is_err());
}

#[test]
fn transition_params_and_reverse_via_commands() {
    let mut s = demo();
    let cut = s.active_sequence().unwrap().video_tracks[0].items[2].start;
    s.execute("playhead.set", json!({"time": cut.0})).unwrap();
    let r = s
        .execute(
            "sequence.applyVideoTransition",
            json!({"effect": "Iris Round", "params": {"border_width": 12.0, "border_color": "#ff0000", "antialias": "High"}, "reverse": true}),
        )
        .unwrap();
    let id = r["transition"].as_u64().unwrap();
    let find = |s: &Session| s.active_sequence().unwrap().video_tracks[0].transitions.iter().find(|t| t.id.0 == id).cloned().unwrap();
    let tr = find(&s);
    assert!(tr.reverse);
    assert_eq!(tr.effect.params["border_width"].value, filmcraft_project::ParamValue::Float(12.0));
    assert_eq!(tr.effect.params["antialias"].value, filmcraft_project::ParamValue::Choice(3));
    s.execute("sequence.setTransition", json!({"transition": id, "params": {"center": [100.0, 50.0], "border_width": 9999.0}, "reverse": false})).unwrap();
    let tr = find(&s);
    assert!(!tr.reverse);
    assert_eq!(tr.effect.params["border_width"].value, filmcraft_project::ParamValue::Float(200.0), "clamped to the range");
    assert!(s.execute("sequence.setTransition", json!({"transition": id, "params": {"nope": 1}})).is_err());
    assert!(s.execute("sequence.setTransition", json!({"transition": id, "params": {"antialias": "Ultra"}})).is_err());
    s.execute("edit.undo", json!({})).unwrap();
    assert!(find(&s).reverse, "setTransition is one undo step");
    s.execute("sequence.setTransition", json!({"transition": id, "reset": true})).unwrap();
    assert_eq!(find(&s).effect.params["border_width"].value, filmcraft_project::ParamValue::Float(0.0));
    let seq = s.execute("sequence.inspect", json!({})).unwrap();
    let js = seq["video"][0]["transitions"].as_array().unwrap().iter().find(|t| t["id"] == id).unwrap().clone();
    assert_eq!(js["effect"], "iris_round");
    assert!(js["params"]["border_width"].is_object() || js["params"]["border_width"].is_number(), "{js}");
}

#[test]
fn effects_list_reports_transition_folders() {
    let s = demo();
    let mut s = s;
    let all = s.execute("effects.list", json!({"kind": "VideoTransition"})).unwrap();
    let all = all.as_array().unwrap();
    assert!(all.iter().all(|e| e["kind"] == "VideoTransition" && e["folder"].is_string()));
    let wipes = s.execute("effects.list", json!({"folder": "Video Transitions/Wipe", "detail": true})).unwrap();
    let wipes = wipes.as_array().unwrap();
    assert_eq!(wipes.len(), 9);
    let lw = wipes.iter().find(|e| e["id"] == "linear_wipe").unwrap();
    assert_eq!(lw["folder"], "Wipe");
    assert_eq!(lw["path"], "Video Transitions/Wipe");
    let aa = lw["paramInfo"].as_array().unwrap().iter().find(|p| p["id"] == "antialias").unwrap();
    assert_eq!(aa["type"], "choice");
    assert_eq!(aa["options"].as_array().unwrap().len(), 4);
    let legacy = s.execute("effects.list", json!({"folder": "Legacy/Video Transitions"})).unwrap();
    assert_eq!(legacy.as_array().unwrap().len(), 21);
    // the default transition accepts names shared with video effects
    s.execute("effects.setDefaultTransition", json!({"effect": "Mosaic"})).unwrap();
    assert_eq!(s.state.default_video_transition, "mosaic_transition");
}

#[test]
fn commands_are_unique_and_described() {
    let mut ids = std::collections::HashSet::new();
    for c in command_specs() {
        assert!(ids.insert(c.id), "duplicate {}", c.id);
    }
    assert!(command_specs().len() > 90, "{}", command_specs().len());
    let s = demo();
    let list = commands::inspect_project(&s);
    assert!(list["root"]["children"].as_array().unwrap().len() >= 4);
}

#[test]
fn save_and_open_roundtrip() {
    let dir = std::env::temp_dir().join(format!("fc-test-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("demo.fcproj").to_string_lossy().to_string();
    let mut s = demo();
    s.execute("file.saveAs", json!({"path": path})).unwrap();
    assert!(!s.is_dirty());
    let mut t = Session::default();
    t.execute("file.open", json!({"path": path})).unwrap();
    assert_eq!(*t.project, *s.project);
    // media re-created from generator refs
    assert!(t.render_program(0.1).is_some());
    std::fs::remove_dir_all(dir).ok();
}

/// Demo project with sync lock off on every track except V1/A1 (the A2 score spans every cut).
fn demo_unlocked() -> Session {
    let mut s = demo();
    for t in ["V2", "V3", "A2", "A3"] {
        let _ = s.execute("timeline.setTrack", json!({"track": t, "syncLock": false}));
    }
    s
}

fn v1(s: &Session) -> Vec<(u64, Tick, Tick)> {
    s.active_sequence().unwrap().video_tracks[0].items.iter().map(|i| (i.id.0, i.start, i.end())).collect()
}

#[test]
fn trim_mode_roll_ripple_and_toggle() {
    let mut s = demo_unlocked();
    let rate = s.sequence_rate();
    let clips = v1(&s);
    // park near the first V1 cut and pick it as a roll
    let cut = clips[0].2;
    s.execute("playhead.set", json!({"time": (cut + rate.tick_of(2)).0})).unwrap();
    let r = s.execute("trim.selectNearest", json!({"kind": "roll"})).unwrap();
    assert!(!r["editPoints"].as_array().unwrap().is_empty());
    let dur_before = s.active_sequence().unwrap().duration();
    s.execute("trim.forwardMany", json!({})).unwrap();
    let after = v1(&s);
    let left = after.iter().find(|c| c.0 == clips[0].0).unwrap();
    assert_eq!(left.2, cut + rate.tick_of(5), "roll moved the cut 5 frames later");
    assert_eq!(s.active_sequence().unwrap().duration(), dur_before, "roll keeps the duration");
    // toggle Roll -> Trim -> Ripple; ripple backward shortens the sequence
    s.execute("trim.toggleType", json!({})).unwrap();
    s.execute("trim.toggleType", json!({})).unwrap();
    assert_eq!(s.state.edit_points[0].kind, crate::trim::TrimKind::Ripple);
    let d0 = s.active_sequence().unwrap().duration();
    s.execute("trim.backward", json!({})).unwrap();
    assert!(s.active_sequence().unwrap().duration() <= d0, "ripple trim back must not lengthen");
    s.execute("trim.clear", json!({})).unwrap();
    assert!(s.execute("trim.forward", json!({})).is_err(), "disabled without edit points");
}

#[test]
fn ripple_trim_to_playhead_q_w() {
    let mut s = demo_unlocked();
    let rate = s.sequence_rate();
    let clips = v1(&s);
    let (id, start, end) = clips[1];
    let d0 = s.active_sequence().unwrap().duration();
    // W: ripple trim the end of the clip under the playhead to the playhead
    let ph = start + rate.tick_of(10);
    s.execute("playhead.set", json!({"time": ph.0})).unwrap();
    s.execute("trim.rippleNext", json!({})).unwrap();
    let c = v1(&s).into_iter().find(|c| c.0 == id).unwrap();
    assert_eq!(c.2, ph, "clip now ends at the playhead");
    assert_eq!(s.active_sequence().unwrap().duration(), d0 - (end - ph), "later material rippled left");
    // Q: ripple trim the start of the (new) clip under the playhead to the playhead
    let (_, start2, end2) = v1(&s)[2];
    let d1 = s.active_sequence().unwrap().duration();
    let ph2 = start2 + rate.tick_of(6);
    s.execute("playhead.set", json!({"time": ph2.0})).unwrap();
    s.execute("trim.ripplePrevious", json!({})).unwrap();
    let c2 = v1(&s).into_iter().find(|c| c.1 == start2).expect("a clip still starts at the old edit");
    assert_eq!(c2.2, end2 - rate.tick_of(6), "its head (6 frames) was removed");
    assert_eq!(s.active_sequence().unwrap().duration(), d1 - rate.tick_of(6));
    assert_eq!(s.playhead(), start2, "playhead parks on the new edit");
}

#[test]
fn interchange_roundtrip_through_files() {
    let dir = std::env::temp_dir().join(format!("fc-ix-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let mut s = demo();
    let n0 = clip_ids(&s, 0).len();
    for (cmd, ext) in [("file.exportFcp7Xml", "xml"), ("file.exportOtio", "otio"), ("file.exportFcpxml", "fcpxml"), ("file.exportEdl", "edl")] {
        let path = dir.join(format!("demo.{ext}")).to_string_lossy().to_string();
        s.execute(cmd, json!({"path": path})).unwrap();
        let r = s.execute("file.import", json!({"paths": [path]})).unwrap();
        let seq = r["sequences"][0].as_u64().unwrap_or_else(|| panic!("{ext}: no sequence in {r}"));
        assert_eq!(s.state.active_sequence, Some(ItemId(seq)), "{ext}: imported sequence is opened");
        // FCP7 XML and OTIO carry our generator (synthetic) media; FCPXML/EDL only file media.
        if matches!(ext, "xml" | "otio") {
            assert_eq!(clip_ids(&s, 0).len(), n0, "{ext}: V1 clip count survives the round trip");
        }
        s.execute("sequence.open", json!({"item": s.state.open_sequences[0].0})).unwrap();
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// `file.exportInterchange {format: "<unknown>"}` used to write FCP7 XML without a word.
#[test]
fn export_interchange_refuses_an_unknown_format() {
    let d = std::env::temp_dir().join(format!("fc-ix-format-{}", std::process::id()));
    std::fs::create_dir_all(&d).unwrap();
    let path = |n: &str| d.join(n).to_string_lossy().to_string();
    let mut s = demo();
    // unknown names, values that are not names, and spellings that are not the documented ones
    for bad in [json!("premiere"), json!("mp4"), json!(""), json!(7), json!(["xml"]), json!(".xml"), json!("fcpxmld"), json!("omfi"), json!("xml ")] {
        let e = s.execute("file.exportInterchange", json!({"format": bad, "path": path("bad.out")})).unwrap_err();
        assert!(matches!(&e, EngineError::BadParams { cmd, .. } if cmd == "file.exportInterchange"), "{bad}: {e:?}");
        let e = e.to_string();
        assert!(e.contains("unknown format") && e.ends_with("use one of edl, xml, fcpxml, otio, aaf, omf"), "the error lists exactly the accepted names: {e}");
        assert!(!d.join("bad.out").exists(), "nothing is written for {bad}");
    }
    // the documented formats still export (in either case), and no format still means FCP7 XML
    let text = |n: &str| String::from_utf8_lossy(&std::fs::read(d.join(n)).unwrap()).into_owned();
    s.execute("file.exportInterchange", json!({"path": path("default.xml")})).unwrap();
    s.execute("file.exportInterchange", json!({"format": "xml", "path": path("a.xml")})).unwrap();
    assert!(text("default.xml").contains("<xmeml") && text("a.xml").contains("<xmeml"));
    s.execute("file.exportInterchange", json!({"format": "fcpxml", "path": path("a.fcpxml")})).unwrap();
    assert!(text("a.fcpxml").contains("<fcpxml"));
    s.execute("file.exportInterchange", json!({"format": "otio", "path": path("a.otio")})).unwrap();
    assert!(text("a.otio").contains("OTIO_SCHEMA"));
    s.execute("file.exportInterchange", json!({"format": "EDL", "path": path("a.edl")})).unwrap();
    assert!(text("a.edl").contains("TITLE:"));
    for (format, magic) in [("aaf", "aaf"), ("omf", "omf")] {
        let r = s.execute("file.exportInterchange", json!({"format": format, "path": path(&format!("a.{magic}"))})).unwrap();
        assert!(r["bytes"].as_u64().unwrap() > 0, "{format}");
    }
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn move_items_to_bin_and_back_with_undo() {
    let mut s = demo();
    let bin = s.execute("file.newBin", json!({"name": "Selects"})).unwrap()["bin"].as_u64().unwrap();
    let mut items = Vec::new();
    s.project.root.all_items(&mut items);
    let item = items[0];
    let home = s.project.root.parent_of(item);
    let r = s.execute("project.moveToBin", json!({"items": [item.0], "bin": bin})).unwrap();
    assert_eq!(r["moved"], 1);
    assert_eq!(s.project.root.parent_of(item), Some(filmcraft_project::BinId(bin)));
    // the item is in the tree exactly once
    let mut all = Vec::new();
    s.project.root.all_items(&mut all);
    assert_eq!(all.iter().filter(|i| **i == item).count(), 1);
    s.execute("edit.undo", json!({})).unwrap();
    assert_eq!(s.project.root.parent_of(item), home);
    s.execute("edit.redo", json!({})).unwrap();
    assert_eq!(s.project.root.parent_of(item), Some(filmcraft_project::BinId(bin)));
    // back to the root with `bin: null`
    s.execute("project.moveToBin", json!({"items": [item.0], "bin": null})).unwrap();
    assert_eq!(s.project.root.parent_of(item), Some(s.project.root.id));
    // an unknown bin is an error, and so is no items and no selection
    assert!(s.execute("project.moveToBin", json!({"items": [item.0], "bin": 999_999})).is_err());
    s.state.project_selection.clear();
    assert!(s.execute("project.moveToBin", json!({})).is_err());
}

#[test]
fn fps_validation_keeps_real_rates_and_refuses_degenerate_ones() {
    let mut s = demo();
    // real rates still work, through both commands
    for fps in [23.976, 25.0, 29.97, 59.94, 120.0] {
        s.execute("file.newSequence", json!({"name": format!("r{fps}"), "fps": fps})).unwrap_or_else(|e| panic!("{fps}: {e}"));
        let r = s.sequence_rate();
        assert!((r.as_f64() - fps).abs() < 0.01, "{fps} -> {}", r.as_f64());
        s.execute("sequence.settings", json!({"fps": 24})).unwrap();
        assert!((s.sequence_rate().as_f64() - 24.0).abs() < 1e-9);
    }
    // a rate that rounds to nothing is refused, and nothing changes
    let before = s.sequence_rate();
    let n = s.execute("project.inspect", json!({})).unwrap().to_string().len();
    assert!(s.execute("file.newSequence", json!({"name": "tiny", "fps": 1e-12})).is_err());
    assert!(s.execute("sequence.settings", json!({"fps": -0.0})).is_err());
    assert_eq!(s.sequence_rate(), before);
    assert_eq!(s.execute("project.inspect", json!({})).unwrap().to_string().len(), n, "no sequence was created");
}

/// #164: with a clip selected, Q/W trim only its tracks (and its linked sound), not every
/// targeted track under the playhead (the music on A2 used to be cut too).
#[test]
fn ripple_trim_to_playhead_follows_the_selection() {
    let mut s = demo_unlocked();
    let rate = s.sequence_rate();
    let a2 = |s: &Session| s.active_sequence().unwrap().audio_tracks[1].items.iter().map(|i| (i.start, i.duration)).collect::<Vec<_>>();
    let music = a2(&s);
    assert!(!music.is_empty(), "the demo has music on A2");
    let (id, start, _) = v1(&s)[1];
    s.execute("timeline.select", json!({"clips": [id]})).unwrap();
    let ph = start + rate.tick_of(10);
    s.execute("playhead.set", json!({"time": ph.0})).unwrap();
    s.execute("trim.rippleNext", json!({})).unwrap();
    assert_eq!(v1(&s).into_iter().find(|c| c.0 == id).unwrap().2, ph, "the selected clip ends at the playhead");
    assert_eq!(a2(&s), music, "the music on A2 is untouched");
    let linked = s.active_sequence().unwrap().audio_tracks[0].items.iter().find(|i| i.start == start).map(|i| i.end());
    assert_eq!(linked, Some(ph), "its linked sound on A1 is trimmed with it");
}

#[test]
fn empty_items_do_not_panic_keyframe_commands() {
    let mut s = demo();
    let seq = s.state.active_sequence.unwrap();
    let clip = {
        let item = &mut std::sync::Arc::make_mut(&mut s.project).sequence_mut(seq).unwrap().video_tracks[0].items[0];
        item.duration = Tick::ZERO;
        item.id
    };
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        s.execute("effects.toggleAnimation", json!({"clip":clip.0,"effect":"motion","param":"position"}))
    }));
    assert!(result.is_ok(), "an empty clip must not supply reversed clamp bounds");
    let error = result.unwrap().unwrap_err();
    assert!(error.to_string().contains("non-positive duration"), "expected the ordinary invariant error, got: {error}");
}

/// #201: a drag of an effect parameter (`merge`, starting with `begin`) is one undo step, and the
/// next drag is another.
#[test]
fn dragging_an_effect_parameter_is_one_undo_step() {
    let mut s = demo_unlocked();
    let id = v1(&s)[0].0;
    let opacity = |s: &Session| {
        let q = s.active_sequence().unwrap();
        let it = q.find_item(ClipId(id)).unwrap().1;
        let e = it.effects.iter().find(|e| e.effect == "opacity").unwrap();
        e.params["opacity"].value.as_f64().unwrap()
    };
    let start = opacity(&s);
    let set = |s: &mut Session, v: f64, begin: bool| {
        s.execute("effects.setParam", json!({"clip": id, "effect": "opacity", "param": "opacity", "value": v, "merge": true, "begin": begin})).unwrap();
    };
    set(&mut s, 90.0, true);
    set(&mut s, 70.0, false);
    set(&mut s, 30.0, false);
    set(&mut s, 50.0, true); // a second drag
    set(&mut s, 60.0, false);
    assert_eq!(opacity(&s), 60.0);
    s.undo();
    assert_eq!(opacity(&s), 30.0, "undo takes back the whole second drag");
    s.undo();
    assert_eq!(opacity(&s), start, "and then the whole first one");
}

/// #484: Enable flips each selected clip on its own, as in Premiere. With one enabled and one
/// disabled clip selected it swaps them, instead of first making both the same.
#[test]
fn enable_flips_each_selected_clip() {
    let mut s = demo();
    let enabled = |s: &Session, c: u64| s.active_sequence().unwrap().find_item(ClipId(c)).unwrap().1.enabled;
    let partner = |s: &Session, c: u64| {
        let q = s.active_sequence().unwrap();
        let link = q.find_item(ClipId(c)).unwrap().1.link?;
        q.all_tracks().flat_map(|t| t.items.iter()).find(|i| i.link == Some(link) && i.id != ClipId(c)).map(|i| i.id.0)
    };
    let (a, b) = (v1(&s)[0].0, v1(&s)[1].0);
    s.execute("clip.enable", json!({"clips": [a]})).unwrap();
    assert_eq!((enabled(&s, a), enabled(&s, b)), (false, true));
    // mixed selection: each flips
    s.execute("clip.enable", json!({"clips": [a, b]})).unwrap();
    assert_eq!((enabled(&s, a), enabled(&s, b)), (true, false));
    s.execute("clip.enable", json!({"clips": [a, b]})).unwrap();
    assert_eq!((enabled(&s, a), enabled(&s, b)), (false, true));
    // a clip named twice still flips once, and one press is one undo step
    s.execute("clip.enable", json!({"clips": [a, a]})).unwrap();
    assert!(enabled(&s, a));
    s.undo();
    assert_eq!((enabled(&s, a), enabled(&s, b)), (false, true));
    // with linked selection on, a clip's linked audio flips with it
    if let Some(au) = partner(&s, a) {
        assert_eq!(enabled(&s, au), enabled(&s, a));
    }
    // a uniform selection still toggles as before
    s.execute("clip.enable", json!({"clips": [b]})).unwrap();
    s.execute("clip.enable", json!({"clips": [a, b]})).unwrap();
    assert_eq!((enabled(&s, a), enabled(&s, b)), (true, true));
}
