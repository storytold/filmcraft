use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use serde_json::json;

use crate::presets::{KeyframeMode, retime};
use crate::{Services, Session};
use filmcraft_project::{ClipId, ParamValue, TrackItem};
use filmcraft_time::{TICKS_PER_SECOND, Tick};

fn demo() -> (Session, Vec<ClipId>) {
    let mut s = Session::default();
    s.execute("file.openDemoProject", json!({})).unwrap();
    let q = s.active_sequence().unwrap();
    let clips: Vec<ClipId> = q.video_tracks[0].items.iter().map(|i| i.id).collect();
    (s, clips)
}

fn item(s: &Session, c: ClipId) -> TrackItem {
    s.active_sequence().unwrap().find_item(c).unwrap().1.clone()
}

fn secs(x: f64) -> Tick {
    Tick((x * TICKS_PER_SECOND as f64).round() as i64)
}

fn media_len(it: &TrackItem) -> Tick {
    Tick((it.duration.0 as f64 * it.speed.abs()).round() as i64)
}

/// Save a keyframed blur from clip A, apply with each keyframe mode to clip B (another length).
#[test]
fn keyframe_modes_retime_onto_the_target() {
    let (mut s, clips) = demo();
    let (a, b) = (clips[0], clips[2]);
    let ia = item(&s, a);
    s.state.selection = vec![a];
    s.execute("effects.apply", json!({"effect": "gaussian_blur"})).unwrap();
    // keyframes 0.5 s after the in point (20) and 1 s before the out point (0)
    let k1 = ia.start + secs(0.5);
    let k2 = ia.end() - secs(1.0);
    s.set_playhead(k1);
    let k1 = s.playhead();
    s.execute("effects.toggleAnimation", json!({"clip": a.0, "effect": "gaussian_blur", "param": "blurriness", "time": k1.0})).unwrap();
    s.execute("effects.setParam", json!({"clip": a.0, "effect": "gaussian_blur", "param": "blurriness", "value": 20.0, "time": k1.0})).unwrap();
    s.execute("effects.setParam", json!({"clip": a.0, "effect": "gaussian_blur", "param": "blurriness", "value": 0.0, "time": k2.0})).unwrap();
    let ib = item(&s, b);
    assert_ne!(media_len(&ia), media_len(&ib), "the demo clips differ in length");
    let la = media_len(&ia);
    let lb = media_len(&ib);
    for (mode, key) in [(KeyframeMode::AnchorToIn, "anchorIn"), (KeyframeMode::AnchorToOut, "anchorOut"), (KeyframeMode::Scale, "scale")] {
        let name = format!("blur-{key}");
        let bi = item(&s, a).effects.iter().position(|e| e.effect == "gaussian_blur").unwrap();
        s.execute("presets.save", json!({"clip": a.0, "name": name, "keyframes": key, "effects": [bi]})).unwrap();
        let before = item(&s, b).effects.len();
        s.execute("presets.apply", json!({"preset": name, "clips": [b.0]})).unwrap();
        let it = item(&s, b);
        assert_eq!(it.effects.len(), before + 1);
        let e = it.effects.iter().rfind(|e| e.effect == "gaussian_blur").unwrap();
        // standard effects go after the intrinsic ones, like effects.apply (#1879)
        assert!(it.effects.iter().position(|x| x.effect == "gaussian_blur").unwrap() > it.effects.iter().position(|x| x.effect == "motion").unwrap());
        let p = e.param("blurriness").unwrap();
        assert_eq!(
            p.keyframes.len(),
            2,
            "{:?} src {:?}",
            p.keyframes.iter().map(|k| (k.time, k.value.clone())).collect::<Vec<_>>(),
            item(&s, a)
                .effects
                .iter()
                .find(|e| e.effect == "gaussian_blur")
                .unwrap()
                .param("blurriness")
                .unwrap()
                .keyframes
                .iter()
                .map(|k| k.time)
                .collect::<Vec<_>>()
        );
        let rel1 = ia.source_time_at(k1) - ia.source_in;
        let rel2 = ia.source_time_at(k2) - ia.source_in;
        let (e1, e2) = match mode {
            KeyframeMode::AnchorToIn => (ib.source_in + rel1, ib.source_in + rel2),
            KeyframeMode::AnchorToOut => (ib.source_in + lb - (la - rel1), ib.source_in + lb - (la - rel2)),
            KeyframeMode::Scale => (
                ib.source_in + Tick((rel1.0 as i128 * lb.0 as i128 / la.0 as i128) as i64),
                ib.source_in + Tick((rel2.0 as i128 * lb.0 as i128 / la.0 as i128) as i64),
            ),
        };
        assert_eq!(p.keyframes[0].time, e1, "{key}");
        assert_eq!(p.keyframes[1].time, e2, "{key}");
        assert_eq!(p.keyframes[0].value, ParamValue::Float(20.0));
        assert_eq!(retime(rel1, mode, la, &ib), e1);
        s.undo();
        assert_eq!(item(&s, b).effects.len(), before, "apply is one undo step");
    }
}

#[test]
fn save_without_keyframes_keeps_the_value_at_the_playhead() {
    let (mut s, clips) = demo();
    let a = clips[0];
    let ia = item(&s, a);
    s.state.selection = vec![a];
    s.execute("effects.apply", json!({"effect": "gaussian_blur"})).unwrap();
    s.set_playhead(ia.start);
    s.execute("effects.toggleAnimation", json!({"clip": a.0, "effect": "gaussian_blur", "param": "blurriness", "time": ia.start.0})).unwrap();
    s.execute("effects.setParam", json!({"clip": a.0, "effect": "gaussian_blur", "param": "blurriness", "value": 40.0, "time": ia.start.0})).unwrap();
    s.execute("effects.setParam", json!({"clip": a.0, "effect": "gaussian_blur", "param": "blurriness", "value": 0.0, "time": (ia.start + secs(2.0)).0}))
        .unwrap();
    s.set_playhead(ia.start + secs(1.0));
    let bi = item(&s, a).effects.iter().position(|e| e.effect == "gaussian_blur").unwrap();
    s.execute("presets.save", json!({"name": "half blur", "keyframes": "none", "effects": [bi]})).unwrap();
    let l = s.execute("presets.list", json!({})).unwrap();
    let p = l["presets"].as_array().unwrap().iter().find(|p| p["name"] == "half blur").unwrap().clone();
    assert_eq!(p["animated"], false);
    s.execute("presets.apply", json!({"preset": "half blur", "clips": [clips[1].0]})).unwrap();
    let e = item(&s, clips[1]).effects.into_iter().find(|e| e.effect == "gaussian_blur").unwrap();
    let v = e.param("blurriness").unwrap();
    assert!(!v.is_animated());
    let ParamValue::Float(x) = v.value else { panic!() };
    assert!((x - 20.0).abs() < 1.0, "value at the playhead: {x}");
}

#[test]
fn presets_with_masks_and_intrinsics_round_trip_through_files() {
    let (mut s, clips) = demo();
    let a = clips[0];
    s.state.selection = vec![a];
    s.execute("effects.apply", json!({"effect": "black_white"})).unwrap();
    s.execute("masks.add", json!({"effect": "black_white", "shape": "ellipse", "center": [960, 540], "size": [800, 400]})).unwrap();
    s.execute("effects.setParam", json!({"clip": a.0, "effect": "opacity", "param": "opacity", "value": 50.0})).unwrap();
    let ia = item(&s, a);
    let bw = ia.effects.iter().position(|e| e.effect == "black_white").unwrap();
    let op = ia.effects.iter().position(|e| e.effect == "opacity").unwrap();
    s.execute("presets.save", json!({"name": "masked bw", "effects": [bw, op], "description": "test"})).unwrap();
    let dir = std::env::temp_dir().join(format!("filmcraft-presets-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("p.json").to_string_lossy().to_string();
    let r = s.execute("presets.export", json!({"path": path, "names": ["masked bw", "Fade In"]})).unwrap();
    assert_eq!(r["count"], 2);
    let text = std::fs::read_to_string(&path).unwrap();
    assert!(text.contains("filmcraft.effect-presets"));
    // a fresh session imports and applies it
    let (mut s2, clips2) = demo();
    s2.execute("presets.import", json!({"path": path})).unwrap();
    let names: Vec<String> =
        s2.execute("presets.list", json!({})).unwrap()["presets"].as_array().unwrap().iter().map(|p| p["name"].as_str().unwrap().to_string()).collect();
    assert!(names.contains(&"masked bw".to_string()));
    s2.execute("presets.apply", json!({"preset": "masked bw", "clips": [clips2[1].0]})).unwrap();
    let it = item(&s2, clips2[1]);
    let e = it.effects.iter().find(|e| e.effect == "black_white").unwrap();
    assert_eq!(e.masks.len(), 1);
    assert_eq!(it.effects.iter().filter(|e| e.effect == "opacity").count(), 1, "intrinsic replaced, not duplicated");
    assert_eq!(it.effect("opacity").unwrap().param("opacity").unwrap().value, ParamValue::Float(50.0));
    // garbage is rejected
    std::fs::write(dir.join("bad.json"), b"{\"format\":\"something\",\"version\":1,\"presets\":[]}").unwrap();
    assert!(s2.execute("presets.import", json!({"path": dir.join("bad.json").to_string_lossy()})).is_err());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn builtins_apply_and_user_presets_persist() {
    let (mut s, clips) = demo();
    let l = s.execute("presets.list", json!({})).unwrap();
    let all = l["presets"].as_array().unwrap();
    assert!(all.len() >= 8);
    assert!(all.iter().all(|p| p["builtin"] == true));
    for p in all {
        let name = p["name"].as_str().unwrap();
        s.execute("presets.apply", json!({"preset": name, "clips": [clips[0].0]})).unwrap_or_else(|e| panic!("{name}: {e}"));
    }
    // Fade Out anchors to the out point
    let it = item(&s, clips[0]);
    let op = it.effect("opacity").unwrap().param("opacity").unwrap();
    assert_eq!(op.keyframes.last().unwrap().time, it.source_in + media_len(&it));
    assert!(s.execute("presets.delete", json!({"name": "Fade In"})).is_err(), "built-ins stay");
    // persistence through the data directory
    let dir = std::env::temp_dir().join(format!("filmcraft-presets-lib-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    s.presets.set_dir(&dir);
    s.state.selection = vec![clips[1]];
    s.execute("effects.apply", json!({"effect": "invert"})).unwrap();
    s.execute("presets.save", json!({"name": "Mine"})).unwrap();
    s.execute("presets.rename", json!({"name": "Mine", "to": "Mine 2"})).unwrap();
    let mut lib = crate::presets::PresetLibrary::default();
    lib.set_dir(&dir);
    assert_eq!(lib.user.len(), 1);
    assert_eq!(lib.user[0].name, "Mine 2");
    s.execute("presets.delete", json!({"name": "Mine 2"})).unwrap();
    let mut lib = crate::presets::PresetLibrary::default();
    lib.set_dir(&dir);
    assert!(lib.user.is_empty());
    let _ = std::fs::remove_dir_all(&dir);
}

/// #333: Fade In then Fade Out on one clip keeps both ramps (they keyframe the same Opacity).
#[test]
fn fade_in_and_fade_out_add_up_on_one_clip() {
    let (mut s, clips) = demo();
    let c = clips[0];
    s.execute("presets.apply", json!({"preset": "Fade In", "clips": [c.0]})).unwrap();
    s.execute("presets.apply", json!({"preset": "Fade Out", "clips": [c.0]})).unwrap();
    let it = item(&s, c);
    let op = it.effect("opacity").unwrap().param("opacity").unwrap();
    let keys: Vec<(Tick, ParamValue)> = op.keyframes.iter().map(|k| (k.time, k.value.clone())).collect();
    let (tin, tout) = (it.source_in, it.source_in + media_len(&it));
    let f = ParamValue::Float;
    assert_eq!(keys, vec![(tin, f(0.0)), (tin + secs(1.0), f(100.0)), (tout - secs(1.0), f(100.0)), (tout, f(0.0))], "both fades survive");
    // re-applying Fade In replaces its own ramp, not the Fade Out one
    s.execute("presets.apply", json!({"preset": "Fade In", "clips": [c.0]})).unwrap();
    let it = item(&s, c);
    assert_eq!(it.effect("opacity").unwrap().param("opacity").unwrap().keyframes.len(), 4);
}

#[test]
fn mask_geometry_scales_to_the_target_frame_size() {
    let (s, clips) = demo();
    let it = item(&s, clips[0]);
    let p = crate::presets::builtin_presets().into_iter().find(|p| p.name == "Spotlight").unwrap();
    let fx = crate::presets::instantiate(&p, &it, (960, 540));
    let m = &fx[0].masks[0];
    let path = m.path_at(it.source_in);
    let c = path.centroid();
    assert!((c.x - 480.0).abs() < 1e-6 && (c.y - 270.0).abs() < 1e-6, "{c:?}");
    assert_eq!(m.feather.value, ParamValue::Float(80.0));
}

/// A host without a filesystem (the web): files exist only through `Services`.
#[derive(Default)]
struct MemFs(Mutex<BTreeMap<String, Vec<u8>>>);

impl Services for MemFs {
    fn read_file(&self, path: &str) -> std::io::Result<Vec<u8>> {
        self.0.lock().unwrap().get(path).cloned().ok_or_else(|| std::io::Error::new(std::io::ErrorKind::NotFound, path.to_string()))
    }
    fn write_file(&self, path: &str, data: &[u8]) -> std::io::Result<()> {
        self.0.lock().unwrap().insert(path.into(), data.to_vec());
        Ok(())
    }
}

/// #379: export and import go through the host's services, not `std::fs` (which traps on the web).
#[test]
fn export_and_import_go_through_host_services() {
    let fs = Arc::new(MemFs::default());
    let mut s = Session::new(fs.clone());
    let path = "/exports/owned-preset.prfpset";
    let r = s.execute("presets.export", json!({"path": path, "names": ["Fade In"]})).unwrap();
    assert_eq!(r["count"], 1);
    let bytes = fs.read_file(path).unwrap();
    assert!(String::from_utf8_lossy(&bytes).contains("filmcraft.effect-presets"));
    // a second session on the same host imports it from there
    let mut s2 = Session::new(fs);
    let r = s2.execute("presets.import", json!({"path": path})).unwrap();
    assert_eq!(r["imported"], json!(["Fade In"]));
    assert_eq!(s2.presets.user.len(), 1);
    assert!(s2.execute("presets.import", json!({"path": "/exports/missing.prfpset"})).is_err());
}

/// #2377 review: a rename that brings back a built-in the preset hid by having its name, and two
/// presets of one file with the same id, would leave a command applying the wrong preset.
#[test]
fn renames_and_imports_keep_one_preset_per_id() {
    let fs = Arc::new(MemFs::default());
    let mut s = Session::new(fs.clone());
    let fade = crate::presets::builtin_presets().into_iter().map(|p| p.name).find(|n| n.contains(' ')).unwrap();
    let variant = fade.to_uppercase().replace(' ', "-");
    // a user preset with exactly the built-in's name takes its place; renamed to another spelling
    // of it, the built-in would come back with the same id
    let path = "/exports/one.prfpset";
    s.execute("presets.export", json!({"path": path, "names": [fade]})).unwrap();
    s.execute("presets.import", json!({"path": path})).unwrap();
    assert!(s.presets.user.iter().any(|p| p.name == fade));
    let e = s.execute("presets.rename", json!({"name": fade, "to": variant})).unwrap_err().to_string();
    assert!(e.contains("shortcut id"), "{e}");
    s.execute("presets.rename", json!({"name": fade, "to": "Mine"})).unwrap();
    // one file, two presets with one id: the first is imported, the second skipped
    let mut file: serde_json::Value = serde_json::from_slice(&fs.read_file(path).unwrap()).unwrap();
    let mut second = file["presets"][0].clone();
    file["presets"][0]["name"] = json!("Glow");
    second["name"] = json!("glow!");
    file["presets"].as_array_mut().unwrap().push(second);
    fs.write_file("/exports/two.prfpset", &serde_json::to_vec(&file).unwrap()).unwrap();
    let r = s.execute("presets.import", json!({"path": "/exports/two.prfpset"})).unwrap();
    assert_eq!((r["imported"].clone(), r["skipped"].clone()), (json!(["Glow"]), json!(["glow!"])));
}

/// Every effect preset is a bindable command, `presets.apply.<name>` (#1356): listed in Keyboard
/// Shortcuts under Presets, assignable a key, applying the preset to the selected clips in
/// one undo step; a deleted preset takes its command with it.
#[test]
fn every_preset_is_a_shortcut_command() {
    use crate::presets::command_key;
    assert_eq!(command_key("Blur In (Short)"), "blur-in-short");
    assert_eq!(command_key("  Glow--Pop!  "), "glow-pop");
    assert_eq!(command_key("???"), "");
    let (mut s, clips) = demo();
    let ids = |s: &mut Session, q: &str| -> Vec<String> {
        let r = s.execute("shortcuts.list", json!({"query": q})).unwrap();
        r["commands"].as_array().or_else(|| r.as_array()).unwrap().iter().filter_map(|c| c["id"].as_str().map(str::to_string)).collect()
    };
    // the built-in presets are there from the start
    let builtin = crate::presets::builtin_presets().into_iter().next().unwrap().name;
    let builtin_id = format!("presets.apply.{}", command_key(&builtin));
    assert!(ids(&mut s, &builtin).contains(&builtin_id), "{builtin_id}");
    // a saved preset adds its command
    s.state.selection = vec![clips[0]];
    s.execute("effects.apply", json!({"effect": "gaussian_blur"})).unwrap();
    let bi = item(&s, clips[0]).effects.iter().position(|e| e.effect == "gaussian_blur").unwrap();
    let revision = s.shortcuts.revision;
    s.execute("presets.save", json!({"clip": clips[0].0, "name": "Soft Glow!", "effects": [bi]})).unwrap();
    assert!(ids(&mut s, "Soft Glow").contains(&"presets.apply.soft-glow".to_string()));
    // the running UI rebuilds its key map from the new commands (#2377 review)
    assert_ne!(s.shortcuts.revision, revision);
    let info = s.shortcuts.command("presets.apply.soft-glow").unwrap();
    assert_eq!((info.label.as_str(), info.category.as_str()), ("Soft Glow!", "Presets"));
    s.execute("shortcuts.set", json!({"command": "presets.apply.soft-glow", "keys": "Ctrl+Alt+G"})).unwrap();
    // the command applies the preset to the selected clips, one undo step
    let target = clips[2];
    s.state.selection = vec![target];
    let before = item(&s, target).effects.iter().filter(|e| e.effect == "gaussian_blur").count();
    let undo = s.history.undo.len();
    s.execute("presets.apply.soft-glow", json!({})).unwrap();
    assert_eq!(item(&s, target).effects.iter().filter(|e| e.effect == "gaussian_blur").count(), before + 1);
    assert_eq!(s.history.undo.len(), undo + 1);
    // deleting the preset removes the command
    s.execute("presets.delete", json!({"name": "Soft Glow!"})).unwrap();
    assert!(!ids(&mut s, "Soft Glow").contains(&"presets.apply.soft-glow".to_string()));
    assert!(s.execute("presets.apply.soft-glow", json!({})).is_err());
    // a name without a letter or digit would give no command: refused (#2377 review)
    let user = s.presets.user.len();
    let e = s.execute("presets.save", json!({"clip": clips[0].0, "name": "???", "effects": [bi]})).unwrap_err().to_string();
    assert!(e.contains("letter or a digit"), "{e}");
    assert_eq!(s.presets.user.len(), user);
    s.execute("presets.save", json!({"clip": clips[0].0, "name": "Glow 2", "effects": [bi]})).unwrap();
    assert!(s.execute("presets.rename", json!({"name": "Glow 2", "to": "--"})).is_err());
    assert!(s.presets.user.iter().any(|x| x.name == "Glow 2"), "the rename changed nothing");
    // nor a name whose id another preset has: a key bound to it would apply the other (#2377 review)
    let fade = crate::presets::builtin_presets().into_iter().map(|p| p.name).find(|n| n.contains(' ')).unwrap();
    let variant = fade.to_uppercase().replace(' ', "-");
    assert_ne!(variant, fade);
    let user = s.presets.user.len();
    let e = s.execute("presets.save", json!({"clip": clips[0].0, "name": variant, "effects": [bi]})).unwrap_err().to_string();
    assert!(e.contains("shortcut id"), "{e}");
    assert_eq!(s.presets.user.len(), user);
    assert!(s.execute("presets.rename", json!({"name": "Glow 2", "to": variant})).unwrap_err().to_string().contains("shortcut id"));
    // renaming a preset to another spelling of its own name is fine
    s.execute("presets.rename", json!({"name": "Glow 2", "to": "glow-2"})).unwrap();
    assert!(s.presets.user.iter().any(|x| x.name == "glow-2"));
}

#[test]
fn built_in_presets_have_ids_of_their_own() {
    let mut keys: Vec<String> = crate::presets::builtin_presets().iter().map(|p| crate::presets::command_key(&p.name)).collect();
    assert!(keys.iter().all(|k| !k.is_empty()));
    let n = keys.len();
    keys.sort();
    keys.dedup();
    assert_eq!(keys.len(), n, "two built-in presets share a command id");
}
