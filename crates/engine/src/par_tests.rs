//! Anamorphic media: the pixel aspect ratio reaches New Sequence From Clip, the placement of clips
//! (default scaling, Fit / Fill) and the rendered frame, Interpret Footage overrides it, and
//! hostile ratios are square. Synthetic media (a Color Matte given 4:3 pixels) runs everywhere;
//! ffmpeg-made 1440 x 1080-style files (at a tenth of the size) check the MP4 and Matroska
//! demuxers end to end when ffmpeg is installed.

use std::path::PathBuf;
use std::sync::Arc;

use serde_json::json;

use crate::Session;
use filmcraft_project::{ItemId, ItemKind, ParamValue};
use filmcraft_time::FrameRate;

/// A 144 x 108 Color Matte (a 1440 x 1080 stand-in) whose pixels are `par`.
fn matte(s: &mut Session, par: (u32, u32)) -> ItemId {
    let r = s.execute("file.newColorMatte", json!({"color": "#ff0000", "width": 144, "height": 108, "seconds": 2.0})).unwrap();
    let id = ItemId(r["item"].as_u64().unwrap());
    if let Some(ItemKind::Media(m)) = Arc::make_mut(&mut s.project).item_mut(id).map(|i| &mut i.kind) {
        m.info.video.as_mut().unwrap().par = par;
    }
    id
}

/// The alpha of the middle row of the program frame at columns `xs`.
fn coverage(s: &Session, xs: &[usize]) -> Vec<f32> {
    let img = s.render_program(1.0).unwrap();
    xs.iter().map(|&x| img.get(x, img.h / 2)[3]).collect()
}

/// A 192 x 108 square-pixel sequence (a 1920 x 1080 stand-in) with `item` overwritten at 0.
fn overwrite_into_square_hd(s: &mut Session, item: ItemId) -> filmcraft_project::ClipId {
    s.execute("file.newSequence", json!({"name": "HD", "width": 192, "height": 108, "fps": 24})).unwrap();
    s.execute("source.open", json!({"item": item.0})).unwrap();
    s.execute("playhead.set", json!({"seconds": 0.0})).unwrap();
    let r = s.execute("source.overwrite", json!({})).unwrap();
    filmcraft_project::ClipId(r["clips"][0].as_u64().unwrap())
}

fn motion_scale(s: &Session, clip: filmcraft_project::ClipId) -> f64 {
    let (_, it) = s.active_sequence().unwrap().find_item(clip).unwrap();
    match it.effect("motion").unwrap().params["scale"].value {
        ParamValue::Float(v) => v,
        ref v => panic!("{v:?}"),
    }
}

#[test]
fn new_sequence_from_clip_carries_the_pixel_aspect() {
    let mut s = Session::default();
    let item = matte(&mut s, (4, 3));
    let r = s.execute("file.newSequenceFromClip", json!({"items": [item.0]})).unwrap();
    let st = s.project.sequence(ItemId(r["sequence"].as_u64().unwrap())).unwrap().settings.clone();
    assert_eq!((st.width, st.height, st.par), (144, 108, (4, 3)));
    assert!(st.validate().is_ok());
    // the clip maps pixel for pixel onto its own sequence: the whole frame
    s.execute("playhead.set", json!({"seconds": 0.5})).unwrap();
    assert!(coverage(&s, &[0, 1, 72, 142, 143]).iter().all(|a| *a > 0.99));
    // New Sequence from the item too
    s.execute("file.newSequence", json!({"name": "From item", "fromItem": item.0})).unwrap();
    assert_eq!(s.active_sequence().unwrap().settings.par, (4, 3));
    // a ratio written in other terms is reduced
    let other = matte(&mut s, (16, 12));
    s.execute("file.newSequenceFromClip", json!({"items": [other.0]})).unwrap();
    assert_eq!(s.active_sequence().unwrap().settings.par, (4, 3));
}

#[test]
fn an_anamorphic_clip_fills_a_square_pixel_sequence() {
    let mut s = Session::default();
    let item = matte(&mut s, (4, 3));
    let clip = overwrite_into_square_hd(&mut s, item);
    // displayed 192 wide at 100 %: the whole frame (it was pillarboxed at 144 wide)
    assert_eq!(motion_scale(&s, clip), 100.0);
    s.execute("playhead.set", json!({"seconds": 0.5})).unwrap();
    assert!(coverage(&s, &[1, 10, 96, 181, 190]).iter().all(|a| *a > 0.99), "{:?}", coverage(&s, &[1, 10, 96, 181, 190]));
    // Fit / Fill fit the display size: already the frame size
    s.execute("clip.fitToFrame", json!({"clips": [clip.0]})).unwrap();
    assert!((motion_scale(&s, clip) - 100.0).abs() < 1e-9);
    s.execute("clip.fillFrame", json!({"clips": [clip.0]})).unwrap();
    assert!((motion_scale(&s, clip) - 100.0).abs() < 1e-9);
    assert!(coverage(&s, &[1, 190]).iter().all(|a| *a > 0.99));
    // a square-pixel matte of the same size is pillarboxed, and Fit to Frame leaves it so
    let square = matte(&mut s, (1, 1));
    let clip = overwrite_into_square_hd(&mut s, square);
    s.execute("playhead.set", json!({"seconds": 0.5})).unwrap();
    let c = coverage(&s, &[1, 20, 30, 96, 165, 175, 190]);
    assert!(c[0] < 0.01 && c[1] < 0.01 && c[2] > 0.99 && c[3] > 0.99 && c[4] > 0.99 && c[5] < 0.01 && c[6] < 0.01, "{c:?}");
    s.execute("clip.fitToFrame", json!({"clips": [clip.0]})).unwrap();
    assert!((motion_scale(&s, clip) - 100.0).abs() < 1e-9);
}

#[test]
fn default_media_scaling_uses_the_display_size() {
    let mut s = Session::default();
    let item = matte(&mut s, (4, 3));
    // Set to Frame Size: 1920 x 1080 display in 1920 x 1080 needs no scaling
    s.prefs.media.default_media_scaling = "setToFrameSize".into();
    let clip = overwrite_into_square_hd(&mut s, item);
    assert_eq!(motion_scale(&s, clip), 100.0);
    // in a 96 x 108 frame it is halved (display 192 wide), not scaled to 96 / 144
    s.execute("file.newSequence", json!({"name": "Narrow", "width": 96, "height": 108, "fps": 24})).unwrap();
    s.execute("source.open", json!({"item": item.0})).unwrap();
    let r = s.execute("source.overwrite", json!({})).unwrap();
    let clip = filmcraft_project::ClipId(r["clips"][0].as_u64().unwrap());
    assert_eq!(motion_scale(&s, clip), 50.0);
    // Scale to Frame Size: the renderer fits the display size
    s.prefs.media.default_media_scaling = "scaleToFrameSize".into();
    s.execute("file.newSequence", json!({"name": "Wide", "width": 384, "height": 108, "fps": 24})).unwrap();
    s.execute("source.open", json!({"item": item.0})).unwrap();
    s.execute("playhead.set", json!({"seconds": 0.0})).unwrap();
    s.execute("source.overwrite", json!({})).unwrap();
    s.execute("playhead.set", json!({"seconds": 0.5})).unwrap();
    let c = coverage(&s, &[90, 100, 192, 283, 293]);
    assert!(c[0] < 0.01 && c[1] > 0.99 && c[2] > 0.99 && c[3] > 0.99 && c[4] < 0.01, "192 wide, centred: {c:?}");
}

#[test]
fn interpret_footage_pixel_aspect_wins_over_the_file_and_undoes() {
    let mut s = Session::default();
    let item = matte(&mut s, (4, 3));
    let r = s.execute("clip.interpretFootage", json!({"items": [item.0], "pixelAspect": [2, 1]})).unwrap();
    assert_eq!(r["pixelAspect"], json!([2, 1]));
    let media = |s: &Session| s.project.item(item).unwrap().as_media().unwrap().clone();
    assert_eq!((media(&s).interpret.par, media(&s).pixel_aspect()), (Some((2, 1)), (2, 1)));
    // the colour interpretation is left as it was
    assert_eq!(media(&s).interpret.color_space, None);
    s.execute("file.newSequenceFromClip", json!({"items": [item.0]})).unwrap();
    assert_eq!(s.active_sequence().unwrap().settings.par, (2, 1));
    // a 2:1 picture 144 wide shows 288 wide: wider than the 192-wide square frame at 100 %
    let clip = overwrite_into_square_hd(&mut s, item);
    assert_eq!(motion_scale(&s, clip), 100.0);
    s.execute("playhead.set", json!({"seconds": 0.5})).unwrap();
    assert!(coverage(&s, &[1, 190]).iter().all(|a| *a > 0.99));
    s.execute("clip.fitToFrame", json!({"clips": [clip.0]})).unwrap();
    assert!((motion_scale(&s, clip) - 100.0 * 192.0 / 288.0).abs() < 1e-9);
    // "file" goes back to the file's ratio; undo restores the override
    s.execute("clip.interpretFootage", json!({"items": [item.0], "pixelAspect": "file"})).unwrap();
    assert_eq!((media(&s).interpret.par, media(&s).pixel_aspect()), (None, (4, 3)));
    s.execute("edit.undo", json!({})).unwrap();
    assert_eq!(media(&s).interpret.par, Some((2, 1)));
    // hostile values are refused and change nothing
    let rev = s.revision;
    for bad in [
        json!([0, 1]),
        json!([1, 0]),
        json!([9, 1]),
        json!([1, 9]),
        json!([4]),
        json!([4, 3, 1]),
        json!([-4, 3]),
        json!([4.5, 3]),
        json!([u64::MAX, 1]),
        json!("wide"),
        json!({"n": 4}),
    ] {
        assert!(s.execute("clip.interpretFootage", json!({"items": [item.0], "pixelAspect": bad})).is_err(), "{bad}");
    }
    assert!(s.execute("clip.interpretFootage", json!({"items": [item.0]})).is_err(), "nothing to change");
    assert_eq!((s.revision, media(&s).interpret.par), (rev, Some((2, 1))));
}

/// New Sequence takes a named preset (`sequence.presets` lists them): 6K, UHD and DCI 4K at the
/// common rates, and the 1.33x anamorphic 6K preset carries 4:3 pixels. Explicit parameters override
/// the preset's picture, and a sequence that no longer matches is named by its picture.
#[test]
fn new_sequence_presets_name_6k_and_anamorphic_sequences() {
    let mut s = Session::default();
    let list = s.execute("sequence.presets", json!({})).unwrap();
    let list = list.as_array().unwrap();
    assert!(list.iter().any(|p| p["name"] == "6K 16:9 6048x3402 23.976" && p["width"] == 6048 && p["height"] == 3402), "{list:?}");
    assert!(list.iter().any(|p| p["name"] == "UHD 2160p 59.94" && p["group"] == "UHD 4K"));
    assert!(list.iter().any(|p| p["name"] == "DCI 4K 2160p 24" && p["width"] == 4096));
    s.execute("file.newSequence", json!({"preset": "6K 16:9 6048x3402 59.94"})).unwrap();
    let st = s.active_sequence().unwrap().settings.clone();
    assert_eq!((st.width, st.height, st.frame_rate, st.par, st.preset.as_str()), (6048, 3402, FrameRate::FPS_59_94, (1, 1), "6K 16:9 6048x3402 59.94"));
    assert!(st.drop_frame);
    s.execute("file.newSequence", json!({"preset": "6K Anamorphic 1.33x 6048x3402 23.976"})).unwrap();
    let st = s.active_sequence().unwrap().settings.clone();
    assert_eq!((st.par, st.preset.as_str()), ((4, 3), "6K Anamorphic 1.33x 6048x3402 23.976"));
    // a 4:3-pixel clip fills the anamorphic sequence edge to edge at 100 %
    let item = matte(&mut s, (4, 3));
    s.execute("source.open", json!({"item": item.0})).unwrap();
    s.execute("playhead.set", json!({"seconds": 0.0})).unwrap();
    let r = s.execute("source.overwrite", json!({})).unwrap();
    let clip = filmcraft_project::ClipId(r["clips"][0].as_u64().unwrap());
    assert_eq!(motion_scale(&s, clip), 100.0);
    // explicit parameters win; the sequence is then named by its picture
    s.execute("file.newSequence", json!({"preset": "UHD 2160p 23.976", "fps": 25})).unwrap();
    let st = s.active_sequence().unwrap().settings.clone();
    assert_eq!((st.width, st.height, st.frame_rate, st.preset.as_str()), (3840, 2160, FrameRate::FPS_25, "3840x2160 25"));
    // hostile preset names are refused and make nothing
    let n = s.project.sequences().count();
    for bad in [json!("6K"), json!("HD 1080p 23.98"), json!(""), json!(6048), json!(["6K 16:9 6048x3402 59.94"])] {
        assert!(s.execute("file.newSequence", json!({"preset": bad})).is_err(), "{bad}");
    }
    assert_eq!(s.project.sequences().count(), n);
}

/// Where the picture of `clip` lands in its sequence: the frame-space box of its source corners.
fn placed_box(s: &Session, clip: filmcraft_project::ClipId) -> (f64, f64, f64, f64) {
    let q = s.active_sequence().unwrap();
    let (_, it) = q.find_item(clip).unwrap();
    let src = s.project.source_size(it.item).unwrap();
    let m = filmcraft_render::motion_matrix(q, it, src, s.project.source_par(it.item), it.source_in);
    let corners = [(0.0, 0.0), (f64::from(src.0), 0.0), (0.0, f64::from(src.1)), (f64::from(src.0), f64::from(src.1))]
        .map(|(x, y)| m.apply(filmcraft_geom::Vec2::new(x, y)));
    let xs = corners.map(|p| p.x);
    let ys = corners.map(|p| p.y);
    (
        xs.iter().copied().fold(f64::MAX, f64::min),
        ys.iter().copied().fold(f64::MAX, f64::min),
        xs.iter().copied().fold(f64::MIN, f64::max),
        ys.iter().copied().fold(f64::MIN, f64::max),
    )
}

/// The desqueezed anamorphic workflow: a 6048 x 3402 clip with an exact 1.33x
/// squeeze edited desqueezed in square pixels. Sequence From Clip with `desqueeze` makes an
/// 8044 x 3402 square-pixel sequence (8043.84 rounded to the nearest even width), named after the
/// desqueezed 6K preset, and the clip is scaled to fill it exactly: no bars, not even a hairline.
#[test]
fn a_desqueezed_sequence_from_clip_is_square_at_the_display_size_and_filled() {
    let mut s = Session::default();
    s.execute("file.newSequence", json!({"name": "Rate", "fps": 23.976})).unwrap();
    let r = s.execute("file.newColorMatte", json!({"color": "#ff0000", "width": 6048, "height": 3402, "seconds": 2.0})).unwrap();
    let item = ItemId(r["item"].as_u64().unwrap());
    s.execute("clip.interpretFootage", json!({"items": [item.0], "pixelAspect": [133, 100]})).unwrap();
    let r = s.execute("file.newSequenceFromClip", json!({"items": [item.0], "desqueeze": true})).unwrap();
    assert_eq!(r["desqueezed"], true);
    let st = s.project.sequence(ItemId(r["sequence"].as_u64().unwrap())).unwrap().settings.clone();
    assert_eq!((st.width, st.height, st.par), (8044, 3402, (1, 1)));
    assert_eq!(st.preset, "6K Anamorphic 1.33x desqueezed 8044x3402 23.976");
    let clip = filmcraft_project::ClipId(r["clips"][0].as_u64().unwrap());
    // scaled to fit the width (8044 / 8043.84), which also covers the height
    assert!((motion_scale(&s, clip) - 8044.0 / (6048.0 * 1.33) * 100.0).abs() < 1e-9, "{}", motion_scale(&s, clip));
    let (x0, y0, x1, y1) = placed_box(&s, clip);
    assert!(x0 <= 1e-9 && y0 <= 1e-9 && x1 >= 8044.0 - 1e-9 && y1 >= 3402.0 - 1e-9, "covers the frame: {:?}", (x0, y0, x1, y1));
    assert!(x0 > -0.01 && x1 < 8044.01 && y0 > -0.1 && y1 < 3402.1, "and crops less than a pixel: {:?}", (x0, y0, x1, y1));
    // the same from New Sequence (the Project panel's New Desqueezed Sequence From Clip)
    s.execute("file.newSequence", json!({"name": "Ana", "fromItem": item.0, "desqueeze": true})).unwrap();
    assert_eq!(s.active_sequence().unwrap().settings.preset, "6K Anamorphic 1.33x desqueezed 8044x3402 23.976");
    // without `desqueeze` the sequence keeps the clip's pixels (the squeezed way)
    let r = s.execute("file.newSequenceFromClip", json!({"items": [item.0]})).unwrap();
    assert_eq!(r["desqueezed"], false);
    let st = s.active_sequence().unwrap().settings.clone();
    assert_eq!((st.width, st.height, st.par), (6048, 3402, (133, 100)));
    // the menu command: enabled for a squeezed clip only
    s.execute("project.select", json!({"items": [item.0]})).unwrap();
    let r = s.execute("file.newDesqueezedSequenceFromClip", json!({})).unwrap();
    assert_eq!(s.project.sequence(ItemId(r["sequence"].as_u64().unwrap())).unwrap().settings.width, 8044);
    let square = s.execute("file.newColorMatte", json!({"color": "#00ff00", "width": 64, "height": 36})).unwrap()["item"].as_u64().unwrap();
    s.execute("project.select", json!({"items": [square]})).unwrap();
    assert!(s.execute("file.newDesqueezedSequenceFromClip", json!({})).is_err(), "square pixels: nothing to desqueeze");
    // with `desqueeze` a square clip makes its ordinary sequence
    let r = s.execute("file.newSequenceFromClip", json!({"items": [square], "desqueeze": true})).unwrap();
    assert_eq!((r["desqueezed"].clone(), s.active_sequence().unwrap().settings.width), (json!(false), 64));
}

/// A squeezed clip edited into a desqueezed preset sequence by hand fills it the same way, and the
/// rendered frame is opaque to the very edge (a small stand-in: 603 x 34 at 1.33 shows 801.99 wide
/// in an 802 x 34 frame, which at 100 % left the first and last columns partly transparent).
#[test]
fn a_squeezed_clip_fills_a_desqueezed_sequence_to_the_edge() {
    let mut s = Session::default();
    let r = s.execute("file.newColorMatte", json!({"color": "#ff0000", "width": 603, "height": 34, "seconds": 2.0})).unwrap();
    let item = ItemId(r["item"].as_u64().unwrap());
    s.execute("clip.interpretFootage", json!({"items": [item.0], "pixelAspect": [133, 100]})).unwrap();
    assert_eq!(filmcraft_project::desqueezed_size((603, 34), (133, 100)), (802, 34));
    s.execute("file.newSequence", json!({"name": "Desqueezed", "width": 802, "height": 34, "fps": 24})).unwrap();
    s.execute("source.open", json!({"item": item.0})).unwrap();
    s.execute("playhead.set", json!({"seconds": 0.0})).unwrap();
    let r = s.execute("source.overwrite", json!({})).unwrap();
    let clip = filmcraft_project::ClipId(r["clips"][0].as_u64().unwrap());
    assert!((motion_scale(&s, clip) - 802.0 / (603.0 * 1.33) * 100.0).abs() < 1e-9);
    s.execute("playhead.set", json!({"seconds": 0.5})).unwrap();
    let img = s.render_program(1.0).unwrap();
    for (x, y) in [(0, 0), (801, 0), (0, 33), (801, 33), (0, 17), (801, 17), (401, 17)] {
        assert!(img.get(x, y)[3] > 0.999, "({x}, {y}) alpha {}", img.get(x, y)[3]);
    }
    // a picture a whole pixel or more off keeps the default scaling (100 %)
    s.execute("file.newSequence", json!({"name": "Wider", "width": 804, "height": 34, "fps": 24})).unwrap();
    s.execute("source.open", json!({"item": item.0})).unwrap();
    let r = s.execute("source.overwrite", json!({})).unwrap();
    assert_eq!(motion_scale(&s, filmcraft_project::ClipId(r["clips"][0].as_u64().unwrap())), 100.0);
}

/// A desqueezed size beyond the largest frame is refused and makes nothing.
#[test]
fn a_desqueeze_beyond_the_largest_frame_is_refused() {
    let mut s = Session::default();
    let item = matte(&mut s, (8, 1));
    if let Some(ItemKind::Media(m)) = Arc::make_mut(&mut s.project).item_mut(item).map(|i| &mut i.kind) {
        m.info.video.as_mut().unwrap().width = 30_000;
    }
    let n = s.project.sequences().count();
    assert!(s.execute("file.newSequenceFromClip", json!({"items": [item.0], "desqueeze": true})).is_err());
    assert!(s.execute("file.newSequence", json!({"fromItem": item.0, "desqueeze": true})).is_err());
    assert_eq!(s.project.sequences().count(), n);
}

#[test]
fn hostile_file_pixel_aspects_make_square_sequences() {
    for bad in [(0, 0), (0, 3), (4, 0), (u32::MAX, 1), (1, u32::MAX), (9, 1)] {
        let mut s = Session::default();
        let item = matte(&mut s, bad);
        s.execute("file.newSequenceFromClip", json!({"items": [item.0]})).unwrap();
        let st = s.active_sequence().unwrap().settings.clone();
        assert_eq!(st.par, (1, 1), "{bad:?}");
        assert!(st.validate().is_ok());
        s.execute("playhead.set", json!({"seconds": 0.5})).unwrap();
        assert!(coverage(&s, &[0, 143]).iter().all(|a| *a > 0.99), "{bad:?}");
        let clip = overwrite_into_square_hd(&mut s, item);
        assert_eq!(motion_scale(&s, clip), 100.0);
        s.execute("playhead.set", json!({"seconds": 0.5})).unwrap();
        let c = coverage(&s, &[1, 96, 190]);
        assert!(c[0] < 0.01 && c[1] > 0.99 && c[2] < 0.01, "{bad:?}: square, pillarboxed: {c:?}");
    }
}

#[test]
fn generators_made_for_a_non_square_sequence_have_its_pixels() {
    let mut s = Session::default();
    let item = matte(&mut s, (4, 3));
    s.execute("file.newSequenceFromClip", json!({"items": [item.0]})).unwrap();
    // a Color Matte made now is 144 x 108 with the sequence's 4:3 pixels: it fills the frame
    let r = s.execute("file.newColorMatte", json!({"color": "#00ff00"})).unwrap();
    let matte2 = ItemId(r["item"].as_u64().unwrap());
    let m = s.project.item(matte2).unwrap().as_media().unwrap();
    assert_eq!((m.info.video.as_ref().unwrap().width, m.pixel_aspect()), (144, (4, 3)));
}

/// ffmpeg-made 144 x 108 H.264 with 4:3 samples (`setsar=4/3`): MP4 (`pasp`) and Matroska
/// (DisplayWidth / DisplayHeight).
fn anamorphic_fixtures() -> Option<Vec<PathBuf>> {
    let ffmpeg = filmcraft_testkit::oracle::ffmpeg_or_skip("par")?;
    let dir = filmcraft_testkit::fixtures_dir("engine/par");
    let mut out = Vec::new();
    for ext in ["mp4", "mkv"] {
        let path = dir.join(format!("anamorphic_144x108_sar43.{ext}"));
        out.push(filmcraft_testkit::fixtures::generate(&path, |tmp| {
            std::process::Command::new(&ffmpeg)
                .args(["-y", "-loglevel", "error", "-f", "lavfi", "-i", "testsrc2=size=144x108:rate=24:duration=1"])
                .args(["-vf", "setsar=4/3", "-c:v", "libx264", "-preset", "fast", "-pix_fmt", "yuv420p"])
                .arg(tmp)
                .status()
                .is_ok_and(|s| s.success())
        })?);
    }
    Some(out)
}

#[test]
fn ffmpeg_anamorphic_files_fill_a_square_frame_and_make_anamorphic_sequences() {
    let Some(files) = anamorphic_fixtures() else { return };
    for file in files {
        let mut s = Session::default();
        let r = s.execute("file.import", json!({"paths": [file.to_string_lossy()]})).unwrap();
        let item = ItemId(r["items"][0].as_u64().unwrap());
        let v = s.project.item(item).unwrap().as_media().unwrap().info.video.clone().unwrap();
        assert_eq!((v.width, v.height, v.par), (144, 108, (4, 3)), "{}", file.display());
        s.execute("file.newSequenceFromClip", json!({"items": [item.0]})).unwrap();
        assert_eq!(s.active_sequence().unwrap().settings.par, (4, 3));
        overwrite_into_square_hd(&mut s, item);
        s.execute("playhead.set", json!({"seconds": 0.5})).unwrap();
        // testsrc2 is opaque everywhere: covered edge to edge
        let c = coverage(&s, &[1, 10, 181, 190]);
        assert!(c.iter().all(|a| *a > 0.99), "{}: {c:?}", file.display());
    }
}

/// New Desqueezed Sequence From Clip on a subclip: the sequence of its anamorphic parent, the
/// subclip's range edited in and filled like the parent; `desqueezed` is in the result.
#[test]
fn a_subclip_of_an_anamorphic_clip_makes_the_same_desqueezed_sequence() {
    let mut s = Session::default();
    s.execute("file.newSequence", json!({"name": "Rate", "fps": 23.976})).unwrap();
    let r = s.execute("file.newColorMatte", json!({"color": "#ff0000", "width": 6048, "height": 3402, "seconds": 2.0})).unwrap();
    let item = ItemId(r["item"].as_u64().unwrap());
    s.execute("clip.interpretFootage", json!({"items": [item.0], "pixelAspect": [133, 100]})).unwrap();
    let r24 = FrameRate::FPS_23_976;
    s.execute("project.setMarks", json!({"item": item.0, "in": r24.tick_of(12).0, "out": r24.tick_of(35).0})).unwrap();
    let sub = ItemId(s.execute("clip.makeSubclip", json!({"item": item.0, "name": "Sub"})).unwrap()["item"].as_u64().unwrap());
    let Some(ItemKind::Subclip { range, .. }) = s.project.item(sub).map(|i| i.kind.clone()) else { panic!("not a subclip") };
    let r = s.execute("file.newSequence", json!({"name": "Sub", "fromItem": sub.0, "desqueeze": true})).unwrap();
    assert_eq!(r["desqueezed"], true, "{r}");
    let st = s.active_sequence().unwrap().settings.clone();
    assert_eq!((st.width, st.height, st.par), (8044, 3402, (1, 1)));
    assert_eq!(st.preset, "6K Anamorphic 1.33x desqueezed 8044x3402 23.976");
    let it = s.active_sequence().unwrap().video_tracks[0].items[0].clone();
    assert_eq!((it.item, it.source_in), (sub, range.start), "the subclip's own range");
    // scaled from the media's size, as its parent is (not from a 1920 x 1080 fallback)
    assert!((motion_scale(&s, it.id) - 8044.0 / (6048.0 * 1.33) * 100.0).abs() < 1e-9, "{}", motion_scale(&s, it.id));
    // the parent reports it too; without `desqueeze` it is false
    let r = s.execute("file.newSequence", json!({"name": "Media", "fromItem": item.0, "desqueeze": true})).unwrap();
    assert_eq!(r["desqueezed"], true);
    let r = s.execute("file.newSequence", json!({"name": "Squeezed", "fromItem": sub.0})).unwrap();
    assert_eq!(r["desqueezed"], false);
    assert_eq!(s.active_sequence().unwrap().settings.par, (133, 100), "a subclip keeps its parent's pixels");
}

/// A sequence from a clip (or its subclip) takes the frame rate the clip is interpreted with, and
/// is named with it.
#[test]
fn a_sequence_from_a_clip_uses_its_interpreted_frame_rate() {
    let mut s = Session::default();
    s.execute("file.newSequence", json!({"name": "Rate", "fps": 23.976})).unwrap();
    let r = s.execute("file.newColorMatte", json!({"color": "#ff0000", "width": 640, "height": 360, "seconds": 2.0})).unwrap();
    let item = ItemId(r["item"].as_u64().unwrap());
    let file_rate = s.project.item(item).and_then(|i| i.as_media()).map(|m| m.info.frame_rate()).unwrap();
    let rate = if file_rate == FrameRate::FPS_25 { FrameRate::FPS_23_976 } else { FrameRate::FPS_25 };
    if let Some(ItemKind::Media(m)) = Arc::make_mut(&mut s.project).item_mut(item).map(|i| &mut i.kind) {
        m.interpret.frame_rate = Some(rate);
    }
    let sub = ItemId(s.execute("clip.makeSubclip", json!({"item": item.0, "name": "Sub"})).unwrap()["item"].as_u64().unwrap());
    for from in [item, sub] {
        s.execute("file.newSequence", json!({"name": "From", "fromItem": from.0})).unwrap();
        let st = s.active_sequence().unwrap().settings.clone();
        assert_eq!(st.frame_rate, rate, "{from:?}");
        assert_eq!(st.preset, format!("640x360 {}", rate.label()), "{from:?}");
    }
    let r = s.execute("file.newSequenceFromClip", json!({"items": [sub.0]})).unwrap();
    assert_eq!(s.project.sequence(ItemId(r["sequence"].as_u64().unwrap())).unwrap().settings.frame_rate, rate);
}
