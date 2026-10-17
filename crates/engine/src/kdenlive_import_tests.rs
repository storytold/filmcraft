//! File ▸ Import of a Kdenlive project (#1089): the timeline opens, the media found on disk is
//! linked, a file that is missing is listed offline, and what was not imported is reported.

use filmcraft_media::DemoScene;
use filmcraft_project::{ItemId, ItemKind};
use filmcraft_time::FrameRate;
use serde_json::json;

use crate::media_test_util::{make_movie, tmp_dir};
use crate::{EngineError, Session, media_browser};

/// A Kdenlive-shaped project at 24 fps: one video track with `media/shot.mov` (on disk) from
/// 0.250 s to 0.958 s, a 12-frame blank, then 24 frames of `media/missing.mp4` (not on disk); a
/// wipe on the timeline.
const PROJECT: &str = r#"<?xml version='1.0' encoding='utf-8'?>
<mlt LC_NUMERIC="C" producer="main_bin" version="7.37.0">
 <profile description="64x36 24 fps" frame_rate_num="24" frame_rate_den="1" width="64" height="36" progressive="1" sample_aspect_num="1" sample_aspect_den="1" display_aspect_num="16" display_aspect_den="9" colorspace="709"/>
 <chain id="chain0" out="00:00:00.958">
  <property name="length">24</property>
  <property name="resource">media/shot.mov</property>
  <property name="mlt_service">avformat-novalidate</property>
  <property name="kdenlive:clipname">Shot</property>
 </chain>
 <chain id="chain1" out="00:00:01.958">
  <property name="length">48</property>
  <property name="resource">media/missing.mp4</property>
  <property name="mlt_service">avformat-novalidate</property>
 </chain>
 <playlist id="main_bin">
  <property name="xml_retain">1</property>
  <entry producer="chain0" in="00:00:00.000" out="00:00:00.958"/>
  <entry producer="chain1" in="00:00:00.000" out="00:00:01.958"/>
 </playlist>
 <playlist id="playlist0">
  <entry producer="chain0" in="00:00:00.250" out="00:00:00.958"/>
  <blank length="12"/>
  <entry producer="chain1" in="0" out="23"/>
 </playlist>
 <playlist id="playlist1"/>
 <tractor id="tractor0">
  <track producer="playlist0"/>
  <track producer="playlist1"/>
 </tractor>
 <tractor id="tractor1">
  <property name="kdenlive:clipname">Cut</property>
  <track producer="tractor0"/>
  <transition id="transition0">
   <property name="mlt_service">luma</property>
   <property name="kdenlive_id">wipe</property>
  </transition>
 </tractor>
</mlt>
"#;

#[test]
fn file_import_opens_a_kdenlive_project() {
    let dir = tmp_dir("kdenlive-import");
    std::fs::create_dir_all(dir.join("media")).unwrap();
    make_movie(&dir.join("media/shot.mov"), DemoScene::OceanSunset, 64, 36, 24);
    let path = dir.join("edit.kdenlive");
    std::fs::write(&path, PROJECT).unwrap();
    let missing = dir.join("media").join("missing.mp4").to_string_lossy().replace('\\', "/");

    let mut s = Session::default();
    let r = s.execute("file.import", json!({"paths": [path.to_string_lossy()]})).unwrap();
    assert_eq!(r["errors"].as_array().map_or(0, Vec::len), 0, "{r}");
    let imported = &r["documents"][0];
    assert_eq!(imported["format"], "Kdenlive (MLT XML)", "{r}");
    assert_eq!(imported["linkedMedia"], 1, "{r}");
    let offline: Vec<String> = imported["offlineMedia"].as_array().unwrap().iter().map(|p| p.as_str().unwrap().replace('\\', "/")).collect();
    assert_eq!(offline, [missing], "{r}");
    assert!(imported["report"].as_array().unwrap().iter().any(|m| m == "transition \"wipe\" was not imported"), "{r}");

    // the timeline is the active sequence, at the profile's settings
    let seq_id = ItemId(r["sequences"][0].as_u64().unwrap());
    assert_eq!(s.state.active_sequence, Some(seq_id));
    assert_eq!(s.project.item(seq_id).unwrap().name, "Cut");
    let seq = s.active_sequence().unwrap();
    let rate = FrameRate::FPS_24;
    assert_eq!((seq.settings.frame_rate, seq.settings.width, seq.settings.height), (rate, 64, 36));
    let v1: Vec<(String, i64, i64, i64)> =
        seq.video_tracks[0].items.iter().map(|c| (c.name.clone(), rate.frame_at(c.start), rate.frame_at(c.end()), rate.frame_at(c.source_in))).collect();
    // 0.250 s = frame 6, 0.958 s = frame 23 (inclusive): 18 frames; then a 12-frame blank
    assert_eq!(v1, [("Shot".to_string(), 0, 18, 6), ("missing.mp4".to_string(), 30, 54, 0)]);

    // the media on disk is linked (its real stream info), the missing file is not
    let media = |i: usize| match &s.project.item(seq.video_tracks[0].items[i].item).unwrap().kind {
        ItemKind::Media(m) => m.clone(),
        other => panic!("{other:?}"),
    };
    assert!(media(0).identity.is_some() && media(0).info.video.as_ref().is_some_and(|v| v.width == 64));
    assert!(media(1).identity.is_none());

    // Kdenlive projects are read, not written
    let e = s.execute("file.exportInterchange", json!({"format": "kdenlive", "path": dir.join("out.kdenlive").to_string_lossy()})).unwrap_err();
    assert!(matches!(&e, EngineError::BadParams { .. }) && e.to_string().ends_with("use one of edl, xml, fcpxml, otio, aaf, omf"), "{e}");
    assert!(!dir.join("out.kdenlive").exists());
    // the Media Browser lists them with the projects it imports
    assert_eq!(media_browser::kind_of("edit.kdenlive"), Some("project"));
    let _ = std::fs::remove_dir_all(&dir);
}
