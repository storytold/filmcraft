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
