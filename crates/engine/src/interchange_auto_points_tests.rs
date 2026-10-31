//! Interchange imports and the "auto" (NaN) point defaults of the effects they add: the points
//! are resolved as placing a clip resolves them, the anchor once the media's real size is known,
//! and a project saved after an import opens again. Also how the media they name are linked.

use filmcraft_geom::Vec2;
use filmcraft_media::DemoScene;
use filmcraft_project::{ItemId, ParamValue, Project, TrackItem};
use filmcraft_time::Tick;
use serde_json::json;

use crate::Session;
use crate::media_test_util::{make_movie, tmp_dir};

const RATE: &str = "<rate><timebase>24</timebase><ntsc>FALSE</ntsc></rate>";

/// A 1920×1080 FCP7 XML sequence with one clip of `media/shot.mov` (declared 3840×2160) whose
/// Basic Motion filter sets only `scale`.
fn xml_with_a_scale_only_motion_filter() -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE xmeml>
<xmeml version="4">
  <sequence id="sequence-1">
    <name>Cut</name>
    <duration>24</duration>
    {RATE}
    <media>
      <video>
        <format><samplecharacteristics>{RATE}<width>1920</width><height>1080</height></samplecharacteristics></format>
        <track>
          <clipitem id="clipitem-1">
            <name>shot</name>
            {RATE}
            <start>0</start><end>24</end><in>0</in><out>24</out>
            <file id="file-1">
              <name>shot.mov</name>
              <pathurl>media/shot.mov</pathurl>
              {RATE}
              <duration>24</duration>
              <media><video><samplecharacteristics><width>3840</width><height>2160</height></samplecharacteristics></video></media>
            </file>
            <filter><effect>
              <name>Basic Motion</name><effectid>basic</effectid><effectcategory>motion</effectcategory><effecttype>motion</effecttype><mediatype>video</mediatype>
              <parameter><parameterid>scale</parameterid><name>Scale</name><value>50</value></parameter>
            </effect></filter>
          </clipitem>
        </track>
      </video>
    </media>
  </sequence>
</xmeml>
"#
    )
}

fn first_clip(s: &Session, seq: ItemId) -> TrackItem {
    s.project.sequence(seq).unwrap().video_tracks[0].items[0].clone()
}

fn motion_point(c: &TrackItem, k: &str) -> Vec2 {
    c.effect("motion").unwrap().vec2_at(k, Tick::ZERO)
}

/// Every effect point parameter of every clip that is still "auto".
fn auto_points(p: &Project) -> Vec<String> {
    let mut out = Vec::new();
    for q in p.sequences().filter_map(|i| i.as_sequence()) {
        for c in q.all_tracks().flat_map(|t| &t.items) {
            for e in &c.effects {
                for (k, v) in &e.params {
                    if matches!(&v.value, ParamValue::Vec2(v) if v.x.is_nan() || v.y.is_nan()) {
                        out.push(format!("{}.{k}", e.effect));
                    }
                }
            }
        }
    }
    out
}

fn import(s: &mut Session, doc: &std::path::Path) -> (serde_json::Value, ItemId) {
    let r = s.execute("file.import", json!({"paths": [doc.to_string_lossy()]})).unwrap();
    let seq = ItemId(r["sequences"][0].as_u64().unwrap_or_else(|| panic!("no sequence in {r}")));
    (r["documents"][0].clone(), seq)
}

/// A project saved after such an import held `null` points and could not be opened again
/// ("project file is damaged: invalid type: null, expected f64").
#[test]
fn a_project_saved_after_an_interchange_import_opens_again() {
    let d = tmp_dir("ix-auto-offline");
    let doc = d.join("cut.xml");
    std::fs::write(&doc, xml_with_a_scale_only_motion_filter()).unwrap();
    let mut s = Session::default();
    let before = s.project.clone();
    // the media file is not there: its real size is not known
    let (r, seq) = import(&mut s, &doc);
    assert_eq!((r["linkedMedia"].as_u64(), r["offlineMedia"].as_array().map(Vec::len)), (Some(0), Some(1)), "{r}");
    let clip = first_clip(&s, seq);
    assert_eq!(clip.effect("motion").unwrap().f64_at("scale", Tick::ZERO), 50.0);
    assert_eq!(motion_point(&clip, "position"), Vec2::new(960.0, 540.0), "the frame centre, as placing a clip resolves it");
    assert!(motion_point(&clip, "anchor").x.is_nan(), "the anchor waits for the media's real size");
    assert_eq!(auto_points(&s.project), ["motion.anchor"]);
    // the saved file opens again and holds the same project (NaN != NaN, so compare what is written)
    let path = d.join("p.fcproj").to_string_lossy().to_string();
    s.execute("file.saveAs", json!({"path": path})).unwrap();
    let mut t = Session::default();
    t.execute("file.open", json!({"path": path})).unwrap();
    assert_eq!(filmcraft_format::encode(&t.project, false), filmcraft_format::encode(&s.project, false));
    assert_eq!(auto_points(&t.project), ["motion.anchor"]);
    assert_eq!(motion_point(&first_clip(&t, seq), "position"), Vec2::new(960.0, 540.0));
    // without a link step the import is one undo step
    s.execute("edit.undo", json!({})).unwrap();
    assert_eq!(*s.project, *before);
    s.execute("edit.redo", json!({})).unwrap();
    assert_eq!(auto_points(&s.project), ["motion.anchor"]);
    let _ = std::fs::remove_dir_all(&d);
}

/// The anchor is centred in the file's real picture, not in the size the document declares
/// (3840×2160 here) or the sequence's.
#[test]
fn a_linked_clip_gets_its_anchor_from_the_real_size_of_the_media() {
    let d = tmp_dir("ix-auto-linked");
    std::fs::create_dir_all(d.join("media")).unwrap();
    make_movie(&d.join("media/shot.mov"), DemoScene::OceanSunset, 320, 180, 24);
    let doc = d.join("cut.xml");
    std::fs::write(&doc, xml_with_a_scale_only_motion_filter()).unwrap();
    let mut s = Session::default();
    let before = s.project.clone();
    let (r, seq) = import(&mut s, &doc);
    assert_eq!(r["linkedMedia"], 1, "{r}");
    let clip = first_clip(&s, seq);
    assert_eq!(s.project.source_size(clip.item), Some((320, 180)));
    assert_eq!((motion_point(&clip, "position"), motion_point(&clip, "anchor")), (Vec2::new(960.0, 540.0), Vec2::new(160.0, 90.0)));
    assert_eq!(auto_points(&s.project), Vec::<String>::new());
    // nothing is NaN, so the reopened project is equal
    let path = d.join("p.fcproj").to_string_lossy().to_string();
    s.execute("file.saveAs", json!({"path": path})).unwrap();
    let mut t = Session::default();
    t.execute("file.open", json!({"path": path})).unwrap();
    assert_eq!(*t.project, *s.project);
    // undo: Link Media takes the real size and the anchor back together, then the import itself
    let linked = s.project.clone();
    s.execute("edit.undo", json!({})).unwrap();
    let clip = first_clip(&s, seq);
    assert_eq!(s.project.source_size(clip.item), Some((3840, 2160)), "the size the document declared");
    assert!(motion_point(&clip, "anchor").x.is_nan());
    s.execute("edit.undo", json!({})).unwrap();
    assert_eq!(*s.project, *before);
    s.execute("edit.redo", json!({})).unwrap();
    s.execute("edit.redo", json!({})).unwrap();
    assert_eq!(*s.project, *linked);
    let _ = std::fs::remove_dir_all(&d);
}

/// An EDL says nothing about the picture: the importer assumes the sequence size for the media
/// until the file is probed. A clip smaller than the sequence must still be centred.
#[test]
fn an_edl_clip_is_centred_in_its_real_size_not_the_assumed_sequence_size() {
    let d = tmp_dir("ix-auto-edl");
    make_movie(&d.join("shot.mov"), DemoScene::OceanSunset, 320, 180, 24);
    let doc = d.join("cut.edl");
    let edl = "TITLE: Cut\nFCM: NON-DROP FRAME\n\n001  AX       V     C        00:00:00:00 00:00:01:00 01:00:00:00 01:00:01:00\n* FROM CLIP NAME: shot.mov\n* SOURCE FILE: shot.mov\n";
    std::fs::write(&doc, edl).unwrap();
    let mut s = Session::default();
    let (r, seq) = import(&mut s, &doc);
    assert_eq!(r["linkedMedia"], 1, "{r}");
    let st = s.project.sequence(seq).unwrap().settings.clone();
    assert_ne!((st.width, st.height), (320, 180), "the media is not the size of the sequence");
    let clip = first_clip(&s, seq);
    assert_eq!(motion_point(&clip, "position"), Vec2::new(st.width as f64 / 2.0, st.height as f64 / 2.0));
    assert_eq!(motion_point(&clip, "anchor"), Vec2::new(160.0, 90.0), "the centre of the 320×180 picture");
    assert_eq!(auto_points(&s.project), Vec::<String>::new());
    let _ = std::fs::remove_dir_all(&d);
}

/// Imported with the media missing, saved, reopened, and only then linked: Link Media gives the
/// item its real size, and the anchors that waited for it become numbers.
#[test]
fn relinking_later_resolves_the_anchor_that_waited_for_the_real_size() {
    let d = tmp_dir("ix-auto-relink");
    let doc = d.join("cut.xml");
    std::fs::write(&doc, xml_with_a_scale_only_motion_filter()).unwrap();
    let mut s = Session::default();
    let (_, seq) = import(&mut s, &doc);
    let path = d.join("p.fcproj").to_string_lossy().to_string();
    s.execute("file.saveAs", json!({"path": path})).unwrap();
    let mut t = Session::default();
    t.execute("file.open", json!({"path": path})).unwrap();
    let clip = first_clip(&t, seq);
    assert!(motion_point(&clip, "anchor").x.is_nan(), "still auto after the reopen");
    // the file turns up in another folder, smaller than the document said
    std::fs::create_dir_all(d.join("found")).unwrap();
    let found = d.join("found/shot.mov");
    make_movie(&found, DemoScene::OceanSunset, 320, 180, 24);
    let waiting = t.project.clone();
    let r = t.execute("media.relink", json!({"item": clip.item.0, "path": found.to_string_lossy(), "force": true})).unwrap();
    assert_eq!(r["relinked"].as_array().map(Vec::len), Some(1), "{r}");
    let clip = first_clip(&t, seq);
    assert_eq!(t.project.source_size(clip.item), Some((320, 180)));
    assert_eq!((motion_point(&clip, "position"), motion_point(&clip, "anchor")), (Vec2::new(960.0, 540.0), Vec2::new(160.0, 90.0)));
    assert_eq!(auto_points(&t.project), Vec::<String>::new());
    // one undo step takes the link and the anchor back together
    let linked = t.project.clone();
    t.execute("edit.undo", json!({})).unwrap();
    assert_eq!(filmcraft_format::encode(&t.project, false), filmcraft_format::encode(&waiting, false));
    assert_eq!(auto_points(&t.project), ["motion.anchor"]);
    t.execute("edit.redo", json!({})).unwrap();
    assert_eq!(*t.project, *linked);
    // an anchor that is already a number is not touched by a later relink to another size
    std::fs::create_dir_all(d.join("other")).unwrap();
    let other = d.join("other/shot.mov");
    make_movie(&other, DemoScene::OceanSunset, 160, 90, 24);
    t.execute("media.relink", json!({"item": clip.item.0, "path": other.to_string_lossy(), "force": true})).unwrap();
    assert_eq!(motion_point(&first_clip(&t, seq), "anchor"), Vec2::new(160.0, 90.0));
    let _ = std::fs::remove_dir_all(&d);
}

/// Linking the media an interchange document names opens each file through the host's reader, as
/// a direct import does, instead of reading it whole into memory (#461: minutes for 4K files).
#[cfg(any(unix, windows))]
#[test]
fn an_interchange_import_links_media_without_reading_the_files_whole() {
    /// The desktop filesystem, except that reading a media file whole fails.
    struct NoWholeMediaReads;
    impl crate::Services for NoWholeMediaReads {
        fn read_file(&self, path: &str) -> std::io::Result<Vec<u8>> {
            if path.ends_with(".mov") {
                return Err(std::io::Error::other(format!("{path}: read whole")));
            }
            crate::FsServices.read_file(path)
        }
        fn write_file(&self, path: &str, data: &[u8]) -> std::io::Result<()> {
            crate::FsServices.write_file(path, data)
        }
        fn file_size(&self, path: &str) -> std::io::Result<u64> {
            crate::FsServices.file_size(path)
        }
        fn read_range(&self, path: &str, offset: u64, len: usize) -> std::io::Result<Vec<u8>> {
            crate::FsServices.read_range(path, offset, len)
        }
        fn reader(&self, path: &str) -> Option<std::io::Result<filmcraft_media::SharedReader>> {
            crate::FsServices.reader(path)
        }
    }
    let d = tmp_dir("ix-streamed-link");
    std::fs::create_dir_all(d.join("media")).unwrap();
    make_movie(&d.join("media/shot.mov"), DemoScene::OceanSunset, 320, 180, 24);
    let doc = d.join("cut.xml");
    std::fs::write(&doc, xml_with_a_scale_only_motion_filter()).unwrap();
    let mut s = Session::new(std::sync::Arc::new(NoWholeMediaReads));
    let (r, seq) = import(&mut s, &doc);
    assert_eq!(r["linkedMedia"], 1, "{r}");
    let clip = first_clip(&s, seq);
    assert_eq!(s.project.source_size(clip.item), Some((320, 180)), "the file's real size, from its index");
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn otio_without_sequence_settings_uses_the_linked_picture_size() {
    let d = tmp_dir("otio-source-size");
    make_movie(&d.join("portrait.mov"), DemoScene::OceanSunset, 48, 80, 24);
    let range = json!({"OTIO_SCHEMA":"TimeRange.1","start_time":{"OTIO_SCHEMA":"RationalTime.1","value":0,"rate":24},"duration":{"OTIO_SCHEMA":"RationalTime.1","value":24,"rate":24}});
    let doc = json!({"OTIO_SCHEMA":"Timeline.1","name":"Portrait cut","tracks":{"OTIO_SCHEMA":"Stack.1","children":[{"OTIO_SCHEMA":"Track.1","kind":"Video","children":[{"OTIO_SCHEMA":"Clip.2","name":"Portrait","source_range":range,"media_reference":{"OTIO_SCHEMA":"ExternalReference.1","target_url":"portrait.mov","available_range":range}}]}]}});
    let path = d.join("cut.otio");
    std::fs::write(&path, serde_json::to_vec(&doc).unwrap()).unwrap();
    let mut s = Session::default();
    let (result, seq) = import(&mut s, &path);
    assert_eq!(result["linkedMedia"], 1, "the host actually probed the local source");
    let settings = &s.project.sequence(seq).unwrap().settings;
    assert_eq!((settings.width, settings.height), (48, 80));
    let clip = first_clip(&s, seq);
    assert_eq!(motion_point(&clip, "position"), Vec2::new(24.0, 40.0), "resolve placement against the inferred canvas");
    assert_eq!(motion_point(&clip, "anchor"), Vec2::new(24.0, 40.0));
}

fn portrait_otio(path: &str) -> serde_json::Value {
    let range = json!({"OTIO_SCHEMA":"TimeRange.1","start_time":{"OTIO_SCHEMA":"RationalTime.1","value":0,"rate":24},"duration":{"OTIO_SCHEMA":"RationalTime.1","value":24,"rate":24}});
    json!({"OTIO_SCHEMA":"Timeline.1","name":"Cut","tracks":{"OTIO_SCHEMA":"Stack.1","children":[{"OTIO_SCHEMA":"Track.1","kind":"Video","children":[{"OTIO_SCHEMA":"Clip.2","source_range":range,"media_reference":{"OTIO_SCHEMA":"ExternalReference.1","target_url":path,"available_range":range}}]}]}})
}

#[test]
fn otio_explicit_hd_canvas_overrides_a_linked_portrait() {
    let d = tmp_dir("otio-explicit-size");
    make_movie(&d.join("portrait.mov"), DemoScene::OceanSunset, 48, 80, 24);
    let mut doc = portrait_otio("portrait.mov");
    doc["metadata"] = json!({"filmcraft":{"settings":filmcraft_project::SequenceSettings::default()}});
    let path = d.join("cut.otio");
    std::fs::write(&path, serde_json::to_vec(&doc).unwrap()).unwrap();
    let mut session = Session::default();
    let (_, id) = import(&mut session, &path);
    let settings = &session.project.sequence(id).unwrap().settings;
    assert_eq!((settings.width, settings.height), (1920, 1080));
    assert_eq!(motion_point(&first_clip(&session, id), "position"), Vec2::new(960., 540.));
    std::fs::remove_dir_all(d).unwrap();
}

#[test]
fn otio_offline_picture_keeps_the_default_canvas() {
    let d = tmp_dir("otio-offline-size");
    let path = d.join("cut.otio");
    std::fs::write(&path, serde_json::to_vec(&portrait_otio("absent.mov")).unwrap()).unwrap();
    let mut session = Session::default();
    let (result, id) = import(&mut session, &path);
    assert_eq!(result["linkedMedia"], 0);
    let settings = &session.project.sequence(id).unwrap().settings;
    assert_eq!((settings.width, settings.height), (1920, 1080));
    assert_eq!(motion_point(&first_clip(&session, id), "position"), Vec2::new(960., 540.));
    std::fs::remove_dir_all(d).unwrap();
}

#[test]
fn otio_nested_canvas_uses_explicit_settings_or_the_linked_picture() {
    let d = tmp_dir("otio-nested-size");
    make_movie(&d.join("portrait.mov"), DemoScene::OceanSunset, 48, 80, 24);
    for explicit in [false, true] {
        let original = portrait_otio("portrait.mov");
        let mut nested = original["tracks"].clone();
        nested["source_range"] = original["tracks"]["children"][0]["children"][0]["source_range"].clone();
        if explicit {
            nested["metadata"] = json!({"filmcraft":{"settings":filmcraft_project::SequenceSettings::default()}});
        }
        let mut doc = original;
        doc["tracks"]["children"][0]["children"] = json!([nested]);
        let path = d.join("cut.otio");
        std::fs::write(&path, serde_json::to_vec(&doc).unwrap()).unwrap();
        let mut session = Session::default();
        let (_, id) = import(&mut session, &path);
        let settings = &session.project.sequence(id).unwrap().settings;
        let expected = if explicit { (1920, 1080) } else { (48, 80) };
        assert_eq!((settings.width, settings.height), expected);
        assert_eq!(motion_point(&first_clip(&session, id), "position"), Vec2::new(expected.0 as f64 / 2., expected.1 as f64 / 2.));
        assert!(auto_points(&session.project).is_empty());
    }
    std::fs::remove_dir_all(d).unwrap();
}

#[test]
fn otio_source_sizing_and_auto_points_undo_and_redo_together() {
    let d = tmp_dir("otio-size-history");
    make_movie(&d.join("portrait.mov"), DemoScene::OceanSunset, 48, 80, 24);
    let path = d.join("cut.otio");
    std::fs::write(&path, serde_json::to_vec(&portrait_otio("portrait.mov")).unwrap()).unwrap();
    let mut session = Session::default();
    let before = session.project.clone();
    let (_, _) = import(&mut session, &path);
    let imported = session.project.clone();
    session.execute("edit.undo", json!({})).unwrap();
    assert_eq!(*session.project, *before);
    session.execute("edit.redo", json!({})).unwrap();
    assert_eq!(*session.project, *imported);
    std::fs::remove_dir_all(d).unwrap();
}

#[test]
fn otio_picture_choice_follows_video_timeline_not_media_allocation() {
    let d = tmp_dir("otio-picture-order");
    make_movie(&d.join("portrait.mov"), DemoScene::OceanSunset, 48, 80, 24);
    make_movie(&d.join("landscape.mov"), DemoScene::OceanSunset, 80, 48, 24);
    let mut doc = portrait_otio("portrait.mov");
    let landscape = portrait_otio("landscape.mov");
    let mut audio = landscape["tracks"]["children"][0].clone();
    audio["kind"] = json!("Audio");
    let mut video = doc["tracks"]["children"][0].clone();
    video["children"].as_array_mut().unwrap().push(landscape["tracks"]["children"][0]["children"][0].clone());
    // Audio creates landscape's media item first; it must not choose the video canvas.
    doc["tracks"]["children"] = json!([audio, video]);
    let path = d.join("cut.otio");
    std::fs::write(&path, serde_json::to_vec(&doc).unwrap()).unwrap();
    let mut session = Session::default();
    let (result, id) = import(&mut session, &path);
    assert_eq!(result["linkedMedia"], 2);
    let settings = &session.project.sequence(id).unwrap().settings;
    assert_eq!((settings.width, settings.height), (48, 80));
    assert_eq!(settings.frame_rate, filmcraft_time::FrameRate::new(24, 1));
    assert_eq!(settings.preset, "Custom 48x80 24");
    assert!(auto_points(&session.project).is_empty());
    std::fs::remove_dir_all(d).unwrap();
}

#[test]
fn otio_explicit_parent_waits_for_an_inferred_nested_anchor() {
    let d = tmp_dir("otio-nested-anchor");
    make_movie(&d.join("portrait.mov"), DemoScene::OceanSunset, 48, 80, 24);
    let original = portrait_otio("portrait.mov");
    let mut nested = original["tracks"].clone();
    nested["source_range"] = original["tracks"]["children"][0]["children"][0]["source_range"].clone();
    let mut doc = original;
    doc["metadata"] = json!({"filmcraft":{"settings":filmcraft_project::SequenceSettings::default()}});
    doc["tracks"]["children"][0]["children"] = json!([nested]);
    let path = d.join("cut.otio");
    std::fs::write(&path, serde_json::to_vec(&doc).unwrap()).unwrap();
    let mut session = Session::default();
    let (_, id) = import(&mut session, &path);
    let clip = first_clip(&session, id);
    assert_eq!(motion_point(&clip, "position"), Vec2::new(960., 540.));
    assert_eq!(motion_point(&clip, "anchor"), Vec2::new(24., 40.));
    std::fs::remove_dir_all(d).unwrap();
}
