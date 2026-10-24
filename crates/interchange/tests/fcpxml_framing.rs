//! #2639: public interchange exports use fit-relative Scale and clip-local Anchor.
mod support;
use filmcraft_geom::Vec2;
use filmcraft_interchange::{ExportOptions, Format, export, import};
use filmcraft_project::{Param, ParamValue, Project, TrackKind};
use filmcraft_time::FrameRate;
use support::*;

fn project(size: (u32, u32), frame: (u32, u32)) -> (Project, filmcraft_project::ItemId) {
    let mut p = Project::new("framing");
    let m = media(&mut p, "/offline/source.mov", true, false, FrameRate::FPS_25);
    let v = p.item_mut(m).unwrap().as_media_mut().unwrap().info.video.as_mut().unwrap();
    (v.width, v.height) = size;
    let s = sequence(&mut p, "cut", FrameRate::FPS_25, false);
    let settings = &mut p.sequence_mut(s).unwrap().settings;
    (settings.width, settings.height) = frame;
    clip(&mut p, s, TrackKind::Video, 0, m, 0, 50, 0);
    (p, s)
}

fn xml(p: &Project, s: filmcraft_project::ItemId) -> String {
    String::from_utf8(export(p, s, Format::Fcpxml, &ExportOptions::default()).unwrap().0).unwrap()
}

#[test]
fn fitted_export_matches_reporter_static_and_animated_values() {
    let (mut p, s) = project((480, 360), (960, 720));
    let it = &mut p.sequence_mut(s).unwrap().video_tracks[0].items[0];
    let m = it.effect_mut("motion").unwrap();
    m.params.insert("scale".into(), Param::new(ParamValue::Float(200.0)));
    m.params.insert("anchor".into(), Param::new(ParamValue::Vec2(Vec2::new(240.0, 180.0))));
    let text = xml(&p, s);
    assert!(text.contains("scale=\"1 1\""), "{text}");
    assert!(text.contains("anchor=\"0 0\""), "{text}");
    assert!(text.contains("<adjust-conform type=\"fit\"/>"));
    let scale = p.sequence_mut(s).unwrap().video_tracks[0].items[0].effect_mut("motion").unwrap().param_mut("scale").unwrap();
    scale.keyframes = vec![
        filmcraft_project::Keyframe::new(FrameRate::FPS_25.tick_of(0), ParamValue::Float(200.0)),
        filmcraft_project::Keyframe::new(FrameRate::FPS_25.tick_of(25), ParamValue::Float(290.0)),
    ];
    let text = xml(&p, s);
    assert!(text.contains("value=\"1 1\""), "{text}");
    assert!(text.contains("value=\"1.45 1.45\""), "{text}");
}

#[test]
fn native_scale_is_normalized_even_without_a_motion_effect() {
    let (mut p, s) = project((480, 360), (960, 720));
    p.sequence_mut(s).unwrap().video_tracks[0].items[0].effects.retain(|e| e.effect != "motion");
    let text = xml(&p, s);
    assert!(text.contains("scale=\"0.5 0.5\""), "{text}");
}

#[test]
fn importer_keeps_anchor_animation_starting_at_center() {
    let (p, s) = project((480, 360), (960, 720));
    let text = xml(&p, s);
    let doc = text.replace("<adjust-transform scale=\"0.5 0.5\"/>", r#"<adjust-transform scale="1 1" anchor="0 0"><param name="anchor"><keyframeAnimation><keyframe time="0s" value="0 0"/><keyframe time="1s" value="10 -10"/></keyframeAnimation></param></adjust-transform>"#);
    let (imp, _) = import(doc.as_bytes(), Format::Fcpxml, None).unwrap();
    let seq = imp.project.sequence(imp.sequences[0]).unwrap();
    let a = seq.video_tracks[0].items[0].effect("motion").unwrap().param("anchor").unwrap();
    assert_eq!(a.keyframes.len(), 2, "{doc}");
    assert_eq!(a.keyframes[0].value.as_vec2().unwrap(), Vec2::new(240.0, 180.0));
    assert_eq!(a.keyframes[1].value.as_vec2().unwrap(), Vec2::new(276.0, 216.0));
}

#[test]
fn zero_dimensions_fail_export_with_actionable_error() {
    for (source, frame) in [((0, 360), (960, 720)), ((480, 0), (960, 720)), ((480, 360), (0, 720)), ((480, 360), (960, 0))] {
        let (p, s) = project(source, frame);
        let error = export(&p, s, Format::Fcpxml, &ExportOptions::default()).unwrap_err().to_string();
        assert!(error.contains("dimensions") && error.contains("repair"), "{error}");
    }
}

#[test]
fn conform_none_fit_and_absent_import_with_renderer_flags() {
    let (p, s) = project((480, 360), (960, 720));
    let original = xml(&p, s);
    for (replacement, expected) in [("", true), ("<adjust-conform type=\"fit\"/>", true), ("<adjust-conform type=\"none\"/>", false)] {
        let doc = original.replace("<adjust-conform type=\"fit\"/>", replacement);
        let (imp, _) = import(doc.as_bytes(), Format::Fcpxml, None).unwrap();
        assert_eq!(imp.project.sequence(imp.sequences[0]).unwrap().video_tracks[0].items[0].scale_to_frame, expected);
    }
}

#[test]
fn static_offcenter_anchor_uses_source_height_and_pixel_aspect() {
    let (mut p, s) = project((720, 576), (1440, 1080));
    let item = p.sequence(s).unwrap().video_tracks[0].items[0].item;
    p.item_mut(item).unwrap().as_media_mut().unwrap().info.video.as_mut().unwrap().par = (16, 15);
    let m = p.sequence_mut(s).unwrap().video_tracks[0].items[0].effect_mut("motion").unwrap();
    m.params.insert("anchor".into(), Param::new(ParamValue::Vec2(Vec2::new(414.0, 230.4))));
    let text = xml(&p, s);
    assert!(text.contains("anchor=\"10 10\""), "{text}");
    let (imp, _) = import(text.as_bytes(), Format::Fcpxml, None).unwrap();
    let a =
        imp.project.sequence(imp.sequences[0]).unwrap().video_tracks[0].items[0].effect("motion").unwrap().param("anchor").unwrap().value.as_vec2().unwrap();
    assert!((a.x - 414.0).abs() < 1e-6 && (a.y - 230.4).abs() < 1e-6);
}

#[test]
fn missing_source_metadata_fails_export_without_inventing_dimensions() {
    let (mut p, s) = project((480, 360), (960, 720));
    let item = p.sequence(s).unwrap().video_tracks[0].items[0].item;
    p.item_mut(item).unwrap().as_media_mut().unwrap().info.video = None;
    let error = export(&p, s, Format::Fcpxml, &ExportOptions::default()).unwrap_err().to_string();
    assert!(error.contains("dimensions") && error.contains("repair"), "{error}");
}
