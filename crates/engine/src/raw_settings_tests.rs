//! Camera raw Source Settings (`clip.sourceSettings`, `media.rawInfo`): CinemaDNG folders, DNG
//! stills and CinemaDNG in MXF developed with the item's settings, one undo step, saved in the
//! project; hostile parameters change nothing.

use std::path::Path;

use filmcraft_media::FrameRequest;
use filmcraft_media::raw::{DemosaicQuality, RawSettings, WhiteBalance};
use filmcraft_project::ItemId;
use filmcraft_time::FrameRate;
use serde_json::{Value, json};

use crate::Session;
use crate::media_test_util::tmp_dir;

const W: usize = 24;
const H: usize = 16;

/// The sRGB matrix from CIE XYZ (D65) to linear Rec. 709 (IEC 61966-2-1): as a DNG ColorMatrix
/// with calibration illuminant D65 it makes the camera's values linear Rec. 709.
const XYZ_TO_709: [f64; 9] = [3.2406, -1.5372, -0.4986, -0.9689, 1.8758, 0.0415, 0.0557, -0.2040, 1.0570];

/// A CinemaDNG frame: 24 × 16 RGGB, 12-bit lossless JPEG, a flat neutral grey at `level` shot
/// under D65 (as-shot neutral 1, 1, 1), 24 fps.
fn frame(level: f64) -> Vec<u8> {
    use filmcraft_dng::tiff::tags;
    use filmcraft_dng::writer::{Compression, Options, Value, write};
    let px: Vec<u16> = (0..W * H).map(|_| (level * 4095.0).round() as u16).collect();
    let o = Options {
        width: W,
        height: H,
        bits: 12,
        compression: Compression::LosslessJpeg { predictor: 1, components: 2, restart_lines: 0, pt: 0 },
        raw_tags: vec![(tags::WHITE_LEVEL, Value::Short(vec![4095]))],
        main_tags: vec![
            (tags::COLOR_MATRIX_1, Value::srationals(&XYZ_TO_709)),
            (tags::CALIBRATION_ILLUMINANT_1, Value::Short(vec![21])),
            (tags::AS_SHOT_NEUTRAL, Value::rationals(&[1.0, 1.0, 1.0])),
            (tags::FRAME_RATE, Value::SRational(vec![(24, 1)])),
        ],
        ..Default::default()
    };
    write(&px, &o)
}

fn level(i: usize) -> f64 {
    0.1 + 0.15 * i as f64
}

fn demo() -> Session {
    let mut s = Session::default();
    s.execute("file.openDemoProject", json!({})).unwrap();
    s
}

fn item_of(r: &Value) -> ItemId {
    ItemId(r["items"][0].as_u64().unwrap_or_else(|| panic!("{r}")))
}

/// Import a three-frame CinemaDNG folder.
fn import_folder(s: &mut Session, dir: &Path) -> ItemId {
    let paths: Vec<String> = (0..3)
        .map(|i| {
            let p = dir.join(format!("A001_C003_{i:06}.dng"));
            std::fs::write(&p, frame(level(i))).unwrap();
            p.to_string_lossy().to_string()
        })
        .collect();
    item_of(&s.execute("file.import", json!({"paths": [paths[0]], "imageSequence": true})).unwrap())
}

/// The (red, green, blue) at the centre of frame `i`, through the media pool as the renderer
/// reads it.
fn rgb(s: &Session, item: ItemId, i: i64) -> [f32; 3] {
    let src = s.media.source_for(&s.project, item, &*s.services).unwrap();
    let f = src.video_frame(FrameRequest::full(FrameRate::FPS_24.tick_of(i))).unwrap();
    let filmcraft_frame::PixelData::RgbaF32(px) = &f.data else { panic!("linear float frames") };
    let at = ((H / 2) * f.width as usize + W / 2) * 4;
    [px[at], px[at + 1], px[at + 2]]
}

fn raw_of(s: &Session, item: ItemId) -> Option<RawSettings> {
    s.project.item(item).unwrap().as_media().unwrap().interpret.raw
}

#[test]
fn exposure_changes_every_frame_and_is_one_undo_step() {
    let dir = tmp_dir("raw-exposure");
    let mut s = demo();
    let id = import_folder(&mut s, &dir);
    let before: Vec<[f32; 3]> = (0..3).map(|i| rgb(&s, id, i)).collect();
    for (i, c) in before.iter().enumerate() {
        assert!((c[0] - level(i) as f32).abs() < 2e-3 && (c[2] - level(i) as f32).abs() < 2e-3, "frame {i}: {c:?}");
    }
    let undo_depth = s.history.undo.len();
    let r = s.execute("clip.sourceSettings", json!({"items": [id.0], "exposure": 1.0})).unwrap();
    assert_eq!(r["settings"]["exposure"], 1.0);
    assert_eq!(s.history.undo.len(), undo_depth + 1);
    assert_eq!(s.history.undo.last().unwrap().0, "Source Settings");
    assert_eq!(raw_of(&s, id).unwrap().exposure, 1.0);
    for (i, c) in before.iter().enumerate() {
        let now = rgb(&s, id, i as i64);
        for k in 0..3 {
            assert!((now[k] - 2.0 * c[k]).abs() < 3e-3, "frame {i} channel {k}: {} vs {}", now[k], c[k]);
        }
    }
    s.execute("edit.undo", json!({})).unwrap();
    assert_eq!(raw_of(&s, id), None);
    assert!((rgb(&s, id, 2)[1] - before[2][1]).abs() < 1e-6, "undo develops as the file says again");
    s.execute("edit.redo", json!({})).unwrap();
    assert!((rgb(&s, id, 2)[1] - 2.0 * before[2][1]).abs() < 3e-3);
    // reset goes back to the file's development and stores nothing
    s.execute("clip.sourceSettings", json!({"items": [id.0], "reset": true})).unwrap();
    assert_eq!(raw_of(&s, id), None);
    assert!((rgb(&s, id, 1)[1] - before[1][1]).abs() < 1e-6);
}

#[test]
fn white_balance_from_the_as_shot_temperature() {
    let dir = tmp_dir("raw-wb");
    let mut s = demo();
    let id = import_folder(&mut s, &dir);
    let info = s.execute("media.rawInfo", json!({"item": id.0})).unwrap();
    assert_eq!(info["raw"], true);
    assert_eq!(info["settings"]["whiteBalance"], "asShot");
    // shot under D65: about 6500 K; D65 is slightly green of the Planckian locus (Δuv +0.003), so
    // the as-shot tint compensates with magenta (positive tint = a more magenta picture)
    let t = info["asShot"]["temperature"].as_f64().unwrap();
    let tint = info["asShot"]["tint"].as_f64().unwrap();
    assert!((t - 6504.0).abs() < 60.0 && (7.0..12.0).contains(&tint), "{t} K, tint {tint}");
    // the dialog's description is the same
    let d = s.execute("clip.sourceSettings", json!({"item": id.0})).unwrap();
    assert_eq!((d["raw"].clone(), d["asShot"].clone(), d["codec"].clone()), (json!(true), info["asShot"].clone(), json!("DNG sequence")));
    assert_eq!(d["pixelFormat"], "Bayer 12-bit raw (lossless JPEG)");
    let grey = rgb(&s, id, 1);
    // the as-shot values as a custom white balance develop the same picture
    s.execute("clip.sourceSettings", json!({"items": [id.0], "temperature": t, "tint": tint})).unwrap();
    assert!(matches!(raw_of(&s, id).unwrap().white_balance, WhiteBalance::Custom { .. }));
    let same = rgb(&s, id, 1);
    for k in 0..3 {
        assert!((same[k] - grey[k]).abs() < 0.01 * grey[k].max(0.01), "channel {k}: {same:?} vs {grey:?}");
    }
    // a tungsten white balance on daylight footage: the grey turns blue
    s.execute("clip.sourceSettings", json!({"items": [id.0], "whiteBalance": "custom", "temperature": 3200})).unwrap();
    let warm = rgb(&s, id, 1);
    assert!(warm[2] > 1.3 * warm[0], "3200 K on D65 footage: {warm:?}");
    // tint towards magenta lowers green
    s.execute("clip.sourceSettings", json!({"items": [id.0], "temperature": t, "tint": 60})).unwrap();
    let magenta = rgb(&s, id, 1);
    assert!(magenta[1] < 0.95 * magenta[0] && magenta[1] < 0.95 * magenta[2], "{magenta:?}");
    // back to the camera's
    s.execute("clip.sourceSettings", json!({"items": [id.0], "whiteBalance": "asShot"})).unwrap();
    assert_eq!(raw_of(&s, id), None);
    let back = rgb(&s, id, 1);
    for k in 0..3 {
        assert!((back[k] - grey[k]).abs() < 1e-6);
    }
}

#[test]
fn demosaic_quality_and_reduced_resolution_requests() {
    let dir = tmp_dir("raw-demosaic");
    let mut s = demo();
    let id = import_folder(&mut s, &dir);
    let high = rgb(&s, id, 0);
    s.execute("clip.sourceSettings", json!({"items": [id.0], "demosaic": "fast"})).unwrap();
    assert_eq!(raw_of(&s, id).unwrap().demosaic, DemosaicQuality::Fast);
    let fast = rgb(&s, id, 0);
    for k in 0..3 {
        assert!((fast[k] - high[k]).abs() < 2e-3, "a flat field is the same with either demosaic");
    }
    // half-resolution playback asks the source for a smaller picture; it is never smaller than asked
    let src = s.media.source_for(&s.project, id, &*s.services).unwrap();
    let half = src.video_frame(FrameRequest { time: filmcraft_time::Tick::ZERO, scale: 0.5 }).unwrap();
    assert!((12..24).contains(&half.width) && (8..16).contains(&half.height), "{}x{}", half.width, half.height);
}

#[test]
fn settings_apply_to_dng_stills_and_cinemadng_mxf() {
    let dir = tmp_dir("raw-still-mxf");
    let mut s = demo();
    // a single DNG is a still
    let still = dir.join("still.dng");
    std::fs::write(&still, frame(0.25)).unwrap();
    let sid = item_of(&s.execute("file.import", json!({"paths": [still.to_string_lossy()]})).unwrap());
    // CinemaDNG in MXF (CinemaDNG specification §7), written by our MXF writer
    let rate = filmcraft_mxf::Rational::new(24, 1);
    let mut cfg = filmcraft_mxf::WriterConfig::new(filmcraft_mxf::Pattern::Op1a, rate, filmcraft_mxf::PackageIds::from_seed("raw", "A001_C004"));
    cfg.picture = Some(filmcraft_mxf::PictureDesc::new(filmcraft_mxf::PictureCoding::CinemaDng { linear_raw: false }, W as u32, H as u32));
    let mut w = filmcraft_mxf::MxfWriter::new(std::io::Cursor::new(Vec::new()), cfg).unwrap();
    for i in 0..3 {
        w.push_picture(&frame(level(i)), filmcraft_mxf::FrameInfo::intra()).unwrap();
    }
    let mxf = dir.join("A001_C004.mxf");
    std::fs::write(&mxf, w.finish().unwrap().into_inner()).unwrap();
    let mid = item_of(&s.execute("file.import", json!({"paths": [mxf.to_string_lossy()]})).unwrap());
    assert_eq!(s.project.item(mid).unwrap().as_media().unwrap().info.video.as_ref().unwrap().codec, "CinemaDNG");
    let (s0, m0) = (rgb(&s, sid, 0), rgb(&s, mid, 2));
    assert!((m0[1] - level(2) as f32).abs() < 2e-3, "{m0:?}");
    // both in one command: one undo step
    s.execute("clip.sourceSettings", json!({"items": [sid.0, mid.0], "exposure": -1.0})).unwrap();
    assert!((rgb(&s, sid, 0)[1] - 0.5 * s0[1]).abs() < 2e-3);
    assert!((rgb(&s, mid, 2)[1] - 0.5 * m0[1]).abs() < 2e-3);
    assert_eq!(s.execute("media.rawInfo", json!({"item": mid.0})).unwrap()["settings"]["exposure"], -1.0);
    s.execute("edit.undo", json!({})).unwrap();
    assert!((rgb(&s, sid, 0)[1] - s0[1]).abs() < 1e-6 && (rgb(&s, mid, 2)[1] - m0[1]).abs() < 1e-6);
}

#[test]
fn settings_are_saved_with_the_project() {
    let dir = tmp_dir("raw-save");
    let mut s = demo();
    let id = import_folder(&mut s, &dir);
    s.execute("clip.sourceSettings", json!({"items": [id.0], "exposure": 0.5, "temperature": 5000, "tint": -10, "demosaic": "fast"})).unwrap();
    let want = raw_of(&s, id).unwrap();
    assert_eq!(
        want,
        RawSettings {
            white_balance: WhiteBalance::Custom { temperature: 5000.0, tint: -10.0 },
            exposure: 0.5,
            demosaic: DemosaicQuality::Fast,
            ..Default::default()
        }
    );
    let bright = rgb(&s, id, 1);
    let path = dir.join("raw.fcproj").to_string_lossy().to_string();
    s.execute("file.saveAs", json!({"path": path})).unwrap();
    let mut t = Session::default();
    t.execute("file.open", json!({"path": path})).unwrap();
    assert_eq!(raw_of(&t, id), Some(want));
    let again = rgb(&t, id, 1);
    for k in 0..3 {
        assert!((again[k] - bright[k]).abs() < 1e-6);
    }
}

#[test]
fn hostile_parameters_change_nothing() {
    let dir = tmp_dir("raw-hostile");
    let mut s = demo();
    let id = import_folder(&mut s, &dir);
    s.execute("clip.sourceSettings", json!({"items": [id.0], "exposure": 0.25})).unwrap();
    let project = s.project.clone();
    let undo = s.history.undo.len();
    let bad = [
        json!({"items": [id.0], "exposure": 9.0}),
        json!({"items": [id.0], "exposure": -1e308}),
        json!({"items": [id.0], "exposure": "bright"}),
        json!({"items": [id.0], "temperature": 100}),
        json!({"items": [id.0], "temperature": 1e9}),
        json!({"items": [id.0], "temperature": 5000, "tint": 151}),
        json!({"items": [id.0], "tint": 10}),
        json!({"items": [id.0], "whiteBalance": "custom"}),
        json!({"items": [id.0], "whiteBalance": "asShot", "temperature": 5000}),
        json!({"items": [id.0], "whiteBalance": "daylight"}),
        json!({"items": [id.0], "highlights": "recover"}),
        json!({"items": [id.0], "highlights": 3}),
        json!({"items": [id.0], "demosaic": "ultra"}),
        json!({"items": [id.0], "output": "rec2020"}),
        json!({"items": [999_999], "exposure": 1.0}),
        json!({"items": "all", "exposure": 1.0}),
        json!({"items": [id.0], "reset": "yes"}),
        json!([1, 2]),
    ];
    for p in bad {
        assert!(s.execute("clip.sourceSettings", p.clone()).is_err(), "{p} was accepted");
        assert!(std::sync::Arc::ptr_eq(&project, &s.project) || *project == *s.project, "{p} changed the project");
        assert_eq!(s.history.undo.len(), undo, "{p} added an undo step");
    }
    assert!(s.execute("media.rawInfo", json!({})).is_err());
    assert!(s.execute("media.rawInfo", json!({"item": 999_999})).is_err());
}

#[test]
fn only_camera_raw_footage_has_source_settings() {
    let mut s = demo();
    // the demo's footage is generated, not camera raw: the dialog says why, settings are refused
    let demo_item = s.project.items.values().find(|i| i.as_media().is_some()).unwrap().id;
    let info = s.execute("media.rawInfo", json!({"item": demo_item.0})).unwrap();
    assert_eq!((info["raw"].clone(), info["settings"].clone()), (json!(false), Value::Null));
    assert!(info["message"].as_str().unwrap().contains("no source settings"), "{info}");
    let undo = s.history.undo.len();
    assert!(s.execute("clip.sourceSettings", json!({"items": [demo_item.0], "exposure": 1.0})).is_err());
    assert_eq!(s.history.undo.len(), undo);
    let c = crate::find_command("clip.sourceSettings").unwrap();
    s.state.project_selection = vec![];
    s.state.selection.clear();
    assert!((c.enabled)(&s).is_err(), "nothing selected");
    s.state.project_selection = vec![demo_item];
    assert!((c.enabled)(&s).is_ok());
    assert_eq!(c.menu, &["Clip"]);
    // selected raw footage enables it, and the selection is the default target
    let dir = tmp_dir("raw-enabled");
    let id = import_folder(&mut s, &dir);
    s.state.project_selection = vec![id];
    assert!((c.enabled)(&s).is_ok());
    s.execute("clip.sourceSettings", json!({"exposure": 0.5})).unwrap();
    assert_eq!(raw_of(&s, id).unwrap().exposure, 0.5);
}
