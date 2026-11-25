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
