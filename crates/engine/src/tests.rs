use super::*;
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
