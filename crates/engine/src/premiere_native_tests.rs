//! Native `.prproj` open/save round-trip (hand-written fixtures only). MIT OR Apache-2.0.

use super::*;
use serde_json::json;

fn fixture_xml() -> String {
    r#"<PremiereData>
<Sequence ObjectUID="parent"><Name>Parent</Name><TrackGroups><TrackGroup><Second ObjectRef="group"/></TrackGroup></TrackGroups></Sequence>
<VideoTrackGroup ObjectID="group"><TrackGroup><FrameRate>8467200000</FrameRate><Tracks><Track ObjectURef="track"/></Tracks></TrackGroup></VideoTrackGroup>
<VideoClipTrack ObjectUID="track"><ClipTrack><Track><Index>0</Index></Track><ClipItems><TrackItems><TrackItem ObjectRef="one"/></TrackItems></ClipItems></ClipTrack></VideoClipTrack>
<VideoClipTrackItem ObjectID="one"><ClipTrackItem><TrackItem><Start>0</Start><End>254016000000</End></TrackItem><SubClip ObjectRef="sub-one"/></ClipTrackItem></VideoClipTrackItem>
<SubClip ObjectID="sub-one"><Name>clip.mov</Name><Clip ObjectRef="clip-one"/></SubClip>
<VideoClip ObjectID="clip-one"><Clip><InPoint>0</InPoint><OutPoint>254016000000</OutPoint><PlaybackSpeed>1</PlaybackSpeed><Source ObjectRef="source-one"/></Clip></VideoClip>
<VideoMediaSource ObjectID="source-one"><MediaSource><Media ObjectURef="media-one"/></MediaSource></VideoMediaSource>
<Media ObjectUID="media-one"><Title>clip.mov</Title><FilePath>clip.mov</FilePath></Media>
<OrphanCustom ObjectUID="keep-me"><Payload>unmodeled</Payload></OrphanCustom>
</PremiereData>"#
        .into()
}

#[test]
fn file_open_save_reopen_keeps_edited_clip_start_and_unknown_xml() {
    let dir = filmcraft_testkit::workspace_root().join("target/premiere-native-open").join(std::process::id().to_string());
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("edit.prproj");
    // Plain XML is a valid .prproj (Premiere also writes gzip); native open accepts both.
    std::fs::write(&path, fixture_xml()).unwrap();

    let mut s = Session::default();
    let opened = s.execute("file.open", json!({"path": path.to_string_lossy()})).unwrap();
    assert_eq!(opened["format"], "prproj");
    assert_eq!(opened["native"], true);
    assert!(s.premiere_native.is_some());

    let seq_id = s.state.active_sequence.expect("sequence");
    {
        let project = std::sync::Arc::make_mut(&mut s.project);
        let seq = project.sequence_mut(seq_id).unwrap();
        seq.video_tracks[0].items[0].start = filmcraft_time::Tick(50);
        seq.video_tracks[0].items[0].duration = filmcraft_time::Tick(150);
    }
    s.revision += 1;

    let saved = s.execute("file.save", json!({})).unwrap();
    assert_eq!(saved["format"], "prproj");
    assert_eq!(saved["native"], true);
    assert!(saved["warnings"].as_array().is_some_and(|w| !w.is_empty()), "unmapped fields must be reported");

    let mut t = Session::default();
    let reopened = t.execute("file.open", json!({"path": path.to_string_lossy()})).unwrap();
    assert_eq!(reopened["native"], true);
    let seq = t.project.sequence(t.state.active_sequence.unwrap()).unwrap();
    assert_eq!(seq.video_tracks[0].items[0].start, filmcraft_time::Tick(50));
    assert_eq!(seq.video_tracks[0].items[0].duration, filmcraft_time::Tick(150));

    let xml = std::fs::read_to_string(&path).unwrap();
    assert!(xml.contains("OrphanCustom"), "unmodeled Premiere siblings must survive Save");
    assert!(xml.contains("unmodeled"));
    assert!(xml.contains("<Start>50</Start>"));
    assert!(xml.contains("<End>200</End>"));

    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn save_as_fcproj_clears_native_premiere_state() {
    let dir = filmcraft_testkit::workspace_root().join("target/premiere-native-fcproj").join(std::process::id().to_string());
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let prproj = dir.join("src.prproj");
    let fcproj = dir.join("out.fcproj");
    std::fs::write(&prproj, fixture_xml()).unwrap();

    let mut s = Session::default();
    s.execute("file.open", json!({"path": prproj.to_string_lossy()})).unwrap();
    assert!(s.premiere_native.is_some());
    s.execute("file.saveAs", json!({"path": fcproj.to_string_lossy()})).unwrap();
    assert!(s.premiere_native.is_none());
    assert!(fcproj.is_file());

    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn autosave_also_saves_current_prproj_via_native_writer() {
    // "Also save current" must patch Premiere XML into the open `.prproj`, not write FilmCraft bytes over it.
    let dir = filmcraft_testkit::workspace_root().join("target/premiere-native-autosave").join(std::process::id().to_string());
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let prproj = dir.join("live.prproj");
    std::fs::write(&prproj, fixture_xml()).unwrap();

    let mut s = Session::default();
    let mut cfg = autosave::AutosaveConfig::new(&dir);
    cfg.journal_debounce = std::time::Duration::from_millis(20);
    s.start_autosave(cfg).unwrap();
    s.execute("file.open", json!({"path": prproj.to_string_lossy()})).unwrap();
    s.execute("prefs.set", json!({"key": "autoSave.saveCurrentProject", "value": true})).unwrap();

    let seq_id = s.state.active_sequence.expect("sequence");
    {
        let project = std::sync::Arc::make_mut(&mut s.project);
        let seq = project.sequence_mut(seq_id).unwrap();
        seq.video_tracks[0].items[0].start = filmcraft_time::Tick(77);
        seq.video_tracks[0].items[0].duration = filmcraft_time::Tick(123);
    }
    s.revision += 1;
    s.sync_persistence();

    s.execute("file.autoSaveNow", json!({})).unwrap();
    let t0 = std::time::Instant::now();
    while s.is_dirty() {
        assert!(t0.elapsed() < std::time::Duration::from_secs(10), "timed out waiting for also-save-current");
        s.poll_persistence();
        std::thread::sleep(std::time::Duration::from_millis(10));
    }

    let xml = std::fs::read_to_string(&prproj).unwrap();
    assert!(xml.contains("<PremiereData>"), "original path must stay Premiere XML");
    assert!(!xml.contains("filmcraft.project"), "must not overwrite .prproj with FilmCraft encoding");
    assert!(xml.contains("OrphanCustom"), "unmodeled siblings must survive autosave");
    assert!(xml.contains("<Start>77</Start>"));
    assert!(xml.contains("<End>200</End>"));
    assert!(s.premiere_native.is_some());

    s.shutdown();
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn recover_reattaches_premiere_native_and_save_patches_prproj() {
    // Crash recovery must restore the Premiere sidecar so ordinary Save writes the `.prproj` again.
    let dir = filmcraft_testkit::workspace_root().join("target/premiere-native-recover").join(std::process::id().to_string());
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let prproj = dir.join("live.prproj");
    std::fs::write(&prproj, fixture_xml()).unwrap();

    let mut a = Session::default();
    let mut cfg = autosave::AutosaveConfig::new(&dir);
    cfg.journal_debounce = std::time::Duration::from_millis(20);
    a.start_autosave(cfg).unwrap();
    a.execute("file.open", json!({"path": prproj.to_string_lossy()})).unwrap();
    assert!(a.premiere_native.is_some());
    let seq_id = a.state.active_sequence.expect("sequence");
    {
        let project = std::sync::Arc::make_mut(&mut a.project);
        let seq = project.sequence_mut(seq_id).unwrap();
        seq.video_tracks[0].items[0].start = filmcraft_time::Tick(50);
        seq.video_tracks[0].items[0].duration = filmcraft_time::Tick(150);
    }
    a.revision += 1;
    a.sync_persistence();
    assert_eq!(a.persistence.as_ref().unwrap().flush().unwrap(), Some(a.revision));
    a.persistence.take().unwrap().simulate_crash();

    let mut b = Session::default();
    let mut cfg = autosave::AutosaveConfig::new(&dir);
    cfg.journal_debounce = std::time::Duration::from_millis(20);
    b.start_autosave(cfg).unwrap();
    assert_eq!(b.recovery_candidates().len(), 1);
    let recovered = b.execute("file.recover", json!({})).unwrap();
    assert_eq!(recovered["native"], true);
    assert!(b.premiere_native.is_some(), "sidecar must re-attach on recover");
    assert_eq!(b.path.as_deref(), Some(prproj.to_string_lossy().as_ref()));
    let saved = b.execute("file.save", json!({})).unwrap();
    assert_eq!(saved["format"], "prproj");
    assert_eq!(saved["native"], true);
    let xml = std::fs::read_to_string(&prproj).unwrap();
    assert!(xml.contains("<Start>50</Start>") && xml.contains("<End>200</End>"), "{xml}");
    assert!(xml.contains("OrphanCustom"));
    b.shutdown();
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn recover_prproj_without_native_sidecar_clears_path() {
    // Legacy journals with a `.prproj` path but no native files must not leave Save claiming a native path.
    let dir = filmcraft_testkit::workspace_root().join("target/premiere-native-recover-legacy").join(std::process::id().to_string());
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let prproj = dir.join("orphan.prproj");
    std::fs::write(&prproj, fixture_xml()).unwrap();

    let old = autosave::recovery_root(&dir).join("1-1");
    std::fs::create_dir_all(&old).unwrap();
    let project = filmcraft_project::Project::new("orphan");
    let bytes = filmcraft_format::encode(&project, false);
    std::fs::write(old.join("snapshot.fcproj"), &bytes).unwrap();
    let meta = autosave::JournalMeta {
        project_name: "orphan".into(),
        project_path: Some(prproj.to_string_lossy().into_owned()),
        has_native: false,
        ..Default::default()
    };
    std::fs::write(old.join("session.json"), serde_json::to_vec_pretty(&meta).unwrap()).unwrap();

    let mut s = Session::default();
    let mut cfg = autosave::AutosaveConfig::new(&dir);
    cfg.journal_debounce = std::time::Duration::from_millis(20);
    s.start_autosave(cfg).unwrap();
    let recovered = s.execute("file.recover", json!({"id": "1-1"})).unwrap();
    assert_eq!(recovered["native"], false);
    assert!(recovered["nativeWarning"].as_str().unwrap().contains("Save As"));
    assert!(s.path.is_none(), "path cleared so Save asks for Save As");
    assert!(s.premiere_native.is_none());
    assert!(s.execute("file.save", json!({})).unwrap_err().to_string().contains("Save As"));
    s.shutdown();
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn new_and_demo_clear_premiere_native_sidecar() {
    // Replacing the project must drop the previous `.prproj` sidecar so Save As cannot patch old XML.
    let dir = filmcraft_testkit::workspace_root().join("target/premiere-native-new-demo").join(std::process::id().to_string());
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let prproj = dir.join("src.prproj");
    std::fs::write(&prproj, fixture_xml()).unwrap();

    let mut s = Session::default();
    s.execute("file.open", json!({"path": prproj.to_string_lossy()})).unwrap();
    assert!(s.premiere_native.is_some());
    s.execute("file.newProject", json!({"name": "Blank"})).unwrap();
    assert!(s.premiere_native.is_none(), "New must clear the Premiere sidecar");

    s.execute("file.open", json!({"path": prproj.to_string_lossy()})).unwrap();
    assert!(s.premiere_native.is_some());
    s.execute("file.openDemoProject", json!({})).unwrap();
    assert!(s.premiere_native.is_none(), "Demo must clear the Premiere sidecar");

    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn failed_save_as_fcproj_keeps_premiere_native() {
    // A refused `.fcproj` write must not drop the sidecar while the session still points at `.prproj`.
    struct RefuseFcproj;
    impl crate::Services for RefuseFcproj {
        fn read_file(&self, path: &str) -> std::io::Result<Vec<u8>> {
            crate::FsServices.read_file(path)
        }
        fn write_file(&self, path: &str, data: &[u8]) -> std::io::Result<()> {
            if path.ends_with(".fcproj") {
                return Err(std::io::Error::new(std::io::ErrorKind::PermissionDenied, "refused"));
            }
            crate::FsServices.write_file(path, data)
        }
    }

    let dir = filmcraft_testkit::workspace_root().join("target/premiere-native-fcproj-fail").join(std::process::id().to_string());
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let prproj = dir.join("src.prproj");
    let fcproj = dir.join("out.fcproj");
    std::fs::write(&prproj, fixture_xml()).unwrap();

    let mut s = Session::default();
    s.execute("file.open", json!({"path": prproj.to_string_lossy()})).unwrap();
    assert!(s.premiere_native.is_some());
    s.services = std::sync::Arc::new(RefuseFcproj);
    assert!(s.execute("file.saveAs", json!({"path": fcproj.to_string_lossy()})).is_err());
    assert!(s.premiere_native.is_some(), "sidecar must survive a failed Save As to .fcproj");
    assert_eq!(s.path.as_deref(), Some(prproj.to_string_lossy().as_ref()));
    assert!(!fcproj.exists());

    std::fs::remove_dir_all(&dir).unwrap();
}
