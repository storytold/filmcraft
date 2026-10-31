//! Public FCPXML round trips must preserve the renderer's source-to-frame placement.
#[path = "../../interchange/tests/it/support/mod.rs"]
mod support;
use filmcraft_geom::Vec2;
use filmcraft_interchange::{ExportOptions, Format, export, import};
use filmcraft_project::{ItemKind, Keyframe, Label, Param, ParamValue, Project, TrackKind};
use filmcraft_render::motion_matrix;
use filmcraft_time::{FrameRate, Tick, TimeRange};
use support::*;

#[test]
fn framing_roundtrip_preserves_rendered_corners_for_media_subclips_and_nests() {
    for (size, frame, source_par, frame_par) in [
        ((480, 360), (960, 720), (1, 1), (1, 1)),
        ((1280, 720), (960, 720), (1, 1), (1, 1)),
        ((360, 640), (1920, 1080), (1, 1), (1, 1)),
        ((720, 576), (1440, 1080), (16, 15), (4, 3)),
    ] {
        for kind in 0..3 {
            for fitted in [false, true] {
                let mut p = Project::new("framing");
                let rate = FrameRate::FPS_25;
                let media = media(&mut p, "/offline/original.mov", true, false, rate);
                p.item_mut(media).unwrap().as_media_mut().unwrap().offline = true;
                let v = p.item_mut(media).unwrap().as_media_mut().unwrap().info.video.as_mut().unwrap();
                (v.width, v.height, v.par) = (size.0, size.1, source_par);
                let source = match kind {
                    1 => p.add_item(
                        "sub",
                        Label::Iris,
                        ItemKind::Subclip { parent: media, range: TimeRange::new(Tick::ZERO, rate.tick_of(50)), restrict_trims: false },
                        None,
                    ),
                    2 => {
                        let nest = sequence(&mut p, "nest", rate, false);
                        let q = p.sequence_mut(nest).unwrap();
                        (q.settings.width, q.settings.height, q.settings.par) = (size.0, size.1, source_par);
                        clip(&mut p, nest, TrackKind::Video, 0, media, 0..50, 0);
                        nest
                    }
                    _ => media,
                };
                let seq = sequence(&mut p, "outer", rate, false);
                let q = p.sequence_mut(seq).unwrap();
                (q.settings.width, q.settings.height, q.settings.par) = (frame.0, frame.1, frame_par);
                clip(&mut p, seq, TrackKind::Video, 0, source, 0..50, 0);
                let it = &mut p.sequence_mut(seq).unwrap().video_tracks[0].items[0];
                it.scale_to_frame = fitted;
                let m = it.effect_mut("motion").unwrap();
                for (name, a, b) in [
                    ("scale", ParamValue::Float(200.0), ParamValue::Float(290.0)),
                    ("rotation", ParamValue::Float(0.0), ParamValue::Float(23.0)),
                    (
                        "anchor",
                        ParamValue::Vec2(Vec2::new(size.0 as f64 / 2.0, size.1 as f64 / 2.0)),
                        ParamValue::Vec2(Vec2::new(size.0 as f64 * 0.37, size.1 as f64 * 0.62)),
                    ),
                    (
                        "position",
                        ParamValue::Vec2(Vec2::new(frame.0 as f64 / 2.0, frame.1 as f64 / 2.0)),
                        ParamValue::Vec2(Vec2::new(frame.0 as f64 * 0.6, frame.1 as f64 * 0.4)),
                    ),
                ] {
                    let mut param = Param::new(a.clone());
                    param.keyframes = vec![Keyframe::new(Tick::ZERO, a), Keyframe::new(rate.tick_of(25), b)];
                    m.params.insert(name.into(), param);
                }
                let (xml, _) = export(&p, seq, Format::Fcpxml, &ExportOptions::default()).unwrap();
                let (imp, _) = import(&xml, Format::Fcpxml, None).unwrap();
                let after = imp.project.sequence(*imp.sequences.last().unwrap()).unwrap();
                let before = p.sequence(seq).unwrap();
                let original = &before.video_tracks[0].items[0];
                let imported = &after.video_tracks[0].items[0];
                let after_size = imp.project.source_size(imported.item).unwrap();
                assert_eq!(after_size, size);
                for t in [0, 12, 25] {
                    let a = motion_matrix(before, original, size, Some(source_par), rate.tick_of(t));
                    let b = motion_matrix(after, imported, after_size, imp.project.source_par(imported.item), rate.tick_of(t));
                    for corner in [Vec2::ZERO, Vec2::new(size.0 as f64, 0.0), Vec2::new(0.0, size.1 as f64), Vec2::new(size.0 as f64, size.1 as f64)] {
                        let x = a.apply(corner);
                        let y = b.apply(corner);
                        assert!((x.x - y.x).abs() < 0.001 && (x.y - y.y).abs() < 0.001, "kind={kind} fitted={fitted} size={size:?} t={t}: {x:?} != {y:?}");
                    }
                }
            }
        }
    }
}
