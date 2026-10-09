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
        // standard effects go before the intrinsic ones
        assert!(it.effects.iter().position(|x| x.effect == "gaussian_blur").unwrap() < it.effects.iter().position(|x| x.effect == "motion").unwrap());
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

fn premiere_preset_xml(mode: u32) -> String {
    let start = 3600 * TICKS_PER_SECOND;
    format!(
        r#"<PremiereData Version="3">
      <Tree ObjectID="10"><RootBin ObjectRef="11"/></Tree>
      <BinTreeItem ObjectID="11"><Items><Item ObjectRef="1"/></Items></BinTreeItem>
      <TreeItem ObjectID="1"><TreeItemBase><Name>Native crop</Name><Data ObjectRef="2"/></TreeItemBase></TreeItem>
      <FilterPresetItem ObjectID="2"><FilterPresets><FilterPreset ObjectRef="3"/></FilterPresets></FilterPresetItem>
      <FilterPreset ObjectID="3"><Component ObjectRef="4"/><AnchorInPoint>{start}</AnchorInPoint><AnchorOutPoint>{end}</AnchorOutPoint><Type>{mode}</Type></FilterPreset>
      <VideoFilterComponent ObjectID="4"><MatchName>AE.ADBE AECrop</MatchName><Component><Params><Param ObjectRef="5"/></Params></Component></VideoFilterComponent>
      <VideoComponentParam ObjectID="5"><ParameterID>1</ParameterID><StartKeyframe>-91445760000000000,10,0,0,0,0,0,0</StartKeyframe><IsTimeVarying>true</IsTimeVarying><Keyframes>{first},10,0,0,0,0,0,0;{last},20,0,0,0,0,0,0;</Keyframes></VideoComponentParam>
    </PremiereData>"#,
        end = start + 3 * TICKS_PER_SECOND,
        first = start + TICKS_PER_SECOND,
        last = start + 2 * TICKS_PER_SECOND
    )
}

#[test]
fn premiere_native_presets_import_through_host_services_without_a_filesystem() {
    let fs = Arc::new(MemFs::default());
    let path = "/exports/native-crop.prfpset";
    fs.write_file(path, premiere_preset_xml(0).as_bytes()).unwrap();
    let mut session = Session::new(fs);
    let result = session.execute("presets.import", json!({"path":path})).unwrap();
    assert_eq!(result["imported"], json!(["Native crop"]));
    assert!(session.presets.find("Native crop").unwrap().normalised_points);
}

#[test]
fn premiere_preset_retime_overflow_leaves_the_project_unchanged() {
    let (mut s, clips) = demo();
    let mut native = crate::presets::builtin_presets().remove(0);
    native.name = "Native overflow".into();
    native.normalised_points = true;
    native.keyframes = KeyframeMode::Scale;
    native.source_duration = Tick(1);
    native.effects[0].params.get_mut("blurriness").unwrap().keyframes[1].time = Tick(i64::MAX / 4);
    s.presets.user.push(native);
    let before = serde_json::to_value(&s.project).unwrap();
    let error = s.execute("presets.apply", json!({"preset":"Native overflow","clips":[clips[1].0]})).unwrap_err();
    assert!(error.to_string().contains("out of range"));
    assert_eq!(serde_json::to_value(&s.project).unwrap(), before);
}

#[test]
fn premiere_presets_import_apply_retime_persist_and_undo() {
    for (mode, timing) in [(0, KeyframeMode::Scale), (1, KeyframeMode::AnchorToIn), (2, KeyframeMode::AnchorToOut)] {
        let dir = std::env::temp_dir().join(format!("filmcraft-native-presets-{}-{mode}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("synthetic.PRFPSET");
        std::fs::write(&path, premiere_preset_xml(mode)).unwrap();
        let (mut s, clips) = demo();
        s.presets.set_dir(&dir.join("library"));
        let result = s.execute("presets.import", json!({"path":path.to_string_lossy()})).unwrap();
        assert_eq!(result["imported"], json!(["Native crop"]));
        assert!(result["report"].as_array().is_some());
        assert_eq!(s.presets.find("Native crop").unwrap().keyframes, timing);
        let target = item(&s, clips[1]);
        let before = s.project.clone();
        s.execute("presets.apply", json!({"preset":"Native crop","clips":[clips[1].0]})).unwrap();
        let applied = item(&s, clips[1]);
        let left = applied.effect("crop").unwrap().param("left").unwrap();
        let expected: Vec<Tick> = [secs(1.0), secs(2.0)]
            .into_iter()
            .map(|t| {
                let offset = match timing {
                    KeyframeMode::Scale => (i128::from(t.0) * i128::from(media_len(&target).0) / i128::from(3 * TICKS_PER_SECOND)) as i64,
                    KeyframeMode::AnchorToIn => t.0,
                    KeyframeMode::AnchorToOut => media_len(&target).0 - 3 * TICKS_PER_SECOND + t.0,
                };
                target.source_in + Tick(offset)
            })
            .collect();
        assert_eq!(left.keyframes.iter().map(|k| k.time).collect::<Vec<_>>(), expected);
        assert_eq!(left.keyframes[0].value, ParamValue::Float(10.0));
        let after = s.project.clone();
        assert!(s.undo().is_some());
        assert_eq!(s.project, before);
        assert!(s.redo().is_some());
        assert_eq!(s.project, after);
        let mut reloaded = crate::presets::PresetLibrary::default();
        reloaded.set_dir(&dir.join("library"));
        assert_eq!(reloaded.user, s.presets.user);
        let previous = s.presets.user.clone();
        std::fs::write(&path, b"<PremiereData Version='3'><broken>").unwrap();
        assert!(s.execute("presets.import", json!({"path":path.to_string_lossy()})).is_err());
        assert_eq!(s.presets.user, previous, "parse failure must not replace a user's preset library");
        let _ = std::fs::remove_dir_all(dir);
    }
}

#[test]
fn premiere_preset_unknown_effect_and_failed_persistence_keep_the_library() {
    let dir = std::env::temp_dir().join(format!("filmcraft-native-presets-errors-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("synthetic.prfpset");
    std::fs::write(&path, premiere_preset_xml(0).replace("AE.ADBE AECrop", "AE.Example.Unsupported")).unwrap();
    let (mut s, _) = demo();
    let previous = s.presets.user.clone();
    let result = s.execute("presets.import", json!({"path":path.to_string_lossy()})).unwrap();
    assert_eq!(result["imported"], json!([]));
    assert!(result["report"].to_string().contains("AE.Example.Unsupported"));
    assert_eq!(s.presets.user, previous);
    let blocked = dir.join("file-instead-of-library-directory");
    std::fs::write(&blocked, b"synthetic test").unwrap();
    s.presets.set_dir(&blocked);
    std::fs::write(&path, premiere_preset_xml(0)).unwrap();
    assert!(s.execute("presets.import", json!({"path":path.to_string_lossy()})).is_err());
    assert_eq!(s.presets.user, previous, "save failure must roll back the in-memory library too");
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn premiere_project_import_uses_file_command_and_is_undoable() {
    let xml = r#"<PremiereData Version="3">
      <Project ObjectID="1"><RootProjectItem ObjectURef="root"/></Project>
      <RootProjectItem ObjectUID="root"><ProjectItemContainer><Items><Item ObjectURef="sequence-item"/></Items></ProjectItemContainer></RootProjectItem>
      <ClipProjectItem ObjectUID="sequence-item"><MasterClip ObjectURef="master"/></ClipProjectItem>
      <MasterClip ObjectUID="master"><Clips><Clip ObjectRef="20"/></Clips></MasterClip>
      <VideoClip ObjectID="20"><Clip><Source ObjectRef="25"/></Clip></VideoClip>
      <VideoSequenceSource ObjectID="25"><SequenceSource><Sequence ObjectURef="sequence"/></SequenceSource></VideoSequenceSource>
      <Sequence ObjectUID="sequence"><Name>Native sequence</Name><TrackGroups><TrackGroup><Second ObjectRef="30"/></TrackGroup></TrackGroups></Sequence>
      <VideoTrackGroup ObjectID="30"><FrameRect>0,0,64,48</FrameRect><TrackGroup><FrameRate>8467200000</FrameRate><Tracks><Track ObjectURef="video"/></Tracks></TrackGroup></VideoTrackGroup>
      <VideoClipTrack ObjectUID="video"><ClipTrack><ClipItems><TrackItems><TrackItem ObjectRef="50"/></TrackItems></ClipItems></ClipTrack></VideoClipTrack>
      <VideoClipTrackItem ObjectID="50"><ClipTrackItem><TrackItem><Start>0</Start><End>254016000000</End></TrackItem><SubClip ObjectRef="51"/></ClipTrackItem></VideoClipTrackItem>
      <SubClip ObjectID="51"><Name>Native cut</Name><Clip ObjectRef="52"/></SubClip>
      <VideoClip ObjectID="52"><Clip><Source ObjectRef="22"/><InPoint>0</InPoint><OutPoint>254016000000</OutPoint></Clip></VideoClip>
      <VideoMediaSource ObjectID="22"><MediaSource><Media ObjectURef="media"/></MediaSource></VideoMediaSource>
      <Media ObjectUID="media"><FilePath>test-only-missing.mov</FilePath><VideoStream ObjectRef="23"/></Media>
      <VideoStream ObjectID="23"><FrameRect>0,0,64,48</FrameRect><FrameRate>8467200000</FrameRate><Duration>1016064000000</Duration></VideoStream>
    </PremiereData>"#;
    let dir = std::env::temp_dir().join(format!("filmcraft-native-project-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("synthetic.PRPROJ");
    std::fs::write(&path, xml).unwrap();
    let mut s = Session::default();
    let before = serde_json::to_value(&s.project).unwrap();
    let result = s.execute("file.import", json!({"paths":[path.to_string_lossy()]})).unwrap();
    assert_eq!(result["sequences"].as_array().unwrap().len(), 1);
    let sequence = s.active_sequence().unwrap();
    assert_eq!(sequence.video_tracks[0].items.len(), 1);
    assert_eq!(sequence.video_tracks[0].items[0].duration, Tick(TICKS_PER_SECOND));
    let after = serde_json::to_value(&s.project).unwrap();
    assert!(s.undo().is_some());
    assert_eq!(serde_json::to_value(&s.project).unwrap(), before);
    assert!(s.redo().is_some());
    assert_eq!(serde_json::to_value(&s.project).unwrap(), after);
    std::fs::write(&path, xml.replace("ObjectRef=\"22\"", "ObjectRef=\"missing\"")).unwrap();
    assert!(s.execute("file.import", json!({"paths":[path.to_string_lossy()]})).is_err());
    assert_eq!(serde_json::to_value(&s.project).unwrap(), after, "a damaged native project must not merge a partial fragment");
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn premiere_motion_preset_uses_sequence_position_and_source_anchor_in_a_portrait_edit() {
    let dir = std::env::temp_dir().join(format!("filmcraft-native-geometry-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("motion.prfpset");
    let xml=premiere_preset_xml(0).replace("AE.ADBE AECrop","AE.ADBE Motion")
        .replace("<Param ObjectRef=\"5\"/>","<Param ObjectRef=\"5\"/><Param ObjectRef=\"6\"/>")
        .replace("<VideoComponentParam ObjectID=\"5\"><ParameterID>1</ParameterID><StartKeyframe>-91445760000000000,10,0,0,0,0,0,0</StartKeyframe><IsTimeVarying>true</IsTimeVarying>","<PointComponentParam ObjectID=\"5\"><ParameterID>1</ParameterID><StartKeyframe>-91445760000000000,0.5:0.5,0,0,0,0,0,0</StartKeyframe><IsTimeVarying>false</IsTimeVarying>")
        .replace("</VideoComponentParam>","</PointComponentParam>")
        .replace("</PremiereData>","<PointComponentParam ObjectID=\"6\"><ParameterID>6</ParameterID><StartKeyframe>-91445760000000000,0.5:0.5,0,0,0,0,0,0</StartKeyframe></PointComponentParam></PremiereData>");
    std::fs::write(&path, xml).unwrap();
    let (mut s, clips) = demo();
    let target = item(&s, clips[0]);
    let size = filmcraft_render::source_size(&s.project, target.item).unwrap();
    let q = std::sync::Arc::make_mut(&mut s.project).sequence_mut(s.state.active_sequence.unwrap()).unwrap();
    q.settings.width = 1080;
    q.settings.height = 1920;
    s.execute("presets.import", json!({"path":path.to_string_lossy()})).unwrap();
    assert!(s.presets.find("Native crop").unwrap().normalised_points);
    s.execute("presets.apply", json!({"preset":"Native crop","clips":[target.id.0]})).unwrap();
    let target = item(&s, clips[0]);
    let motion = target.effect("motion").unwrap();
    assert_eq!(motion.vec2_at("position", target.source_in), filmcraft_geom::Vec2::new(540.0, 960.0));
    assert_eq!(motion.vec2_at("anchor", target.source_in), filmcraft_geom::Vec2::new(f64::from(size.0) / 2.0, f64::from(size.1) / 2.0));
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn premiere_cross_dissolve_preset_applies_at_an_edit_point_with_one_undo_step() {
    let dir = std::env::temp_dir().join(format!("filmcraft-native-transition-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("transition.prfpset");
    let xml = premiere_preset_xml(0)
        .replace("AE.ADBE AECrop", "AE.ADBE Cross Dissolve New")
        .replace("<Type>0</Type>", "<Type>0</Type><TransitionDuration>254016000000</TransitionDuration>");
    std::fs::write(&path, xml).unwrap();
    let (mut s, clips) = demo();
    s.execute("presets.import", json!({"path":path.to_string_lossy()})).unwrap();
    let before = serde_json::to_value(&s.project).unwrap();
    let r = s.execute("presets.apply", json!({"preset":"Native crop","clips":[clips[0].0],"edge":"out"})).unwrap();
    assert_eq!(r["applied"], 1);
    assert!(r["transition"].as_u64().is_some());
    let transition = s.active_sequence().unwrap().video_tracks.iter().flat_map(|t| &t.transitions).find(|t| t.effect.effect == "cross_dissolve").unwrap();
    assert_eq!(transition.duration, Tick(TICKS_PER_SECOND * 1001 / 1000));
    assert!(r["report"].to_string().contains("rounded"));
    assert!(!item(&s, clips[0]).effects.iter().any(|e| e.effect == "cross_dissolve"), "transitions are timeline objects, not clip filters");
    assert!(s.undo().is_some());
    assert_eq!(serde_json::to_value(&s.project).unwrap(), before);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn premiere_oversized_project_is_rejected_before_reading_the_file() {
    struct Oversized;
    impl crate::Services for Oversized {
        fn read_file(&self, _: &str) -> std::io::Result<Vec<u8>> {
            panic!("oversized native project must not be read")
        }
        fn write_file(&self, _: &str, _: &[u8]) -> std::io::Result<()> {
            Ok(())
        }
        fn file_size(&self, _: &str) -> std::io::Result<u64> {
            Ok(filmcraft_interchange::premiere::MAX_DOCUMENT_BYTES as u64 + 1)
        }
    }
    let mut s = Session { services: std::sync::Arc::new(Oversized), ..Session::default() };
    let before = serde_json::to_value(&s.project).unwrap();
    let result = s.execute("file.import", json!({"paths":["oversized.prproj"]}));
    assert!(result.unwrap_err().to_string().contains("64 MiB"));
    assert_eq!(serde_json::to_value(&s.project).unwrap(), before);
}
