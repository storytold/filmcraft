//! #4613: a project saved before stills applied EXIF Orientation (#1583) keeps the size the
//! file was stored at, unturned. The picture itself decodes upright today, and a clip placed
//! from the still baked its Motion anchor from the unturned size, so on reopen the still
//! shows scaled up and off-centre. Opening such a project must swap the stored size to the
//! oriented one and turn the placed clips' anchor points with it; a project saved by a
//! current build must open byte-identical in everything the migration could touch.

use std::path::Path;

use filmcraft_dng::tiff::tags;
use filmcraft_dng::writer::{Compression, Options, Value, write};
use filmcraft_geom::Vec2;
use filmcraft_media::DemoScene;
use filmcraft_project::{ItemId, ItemKind, Keyframe, ParamValue};
use filmcraft_time::{FrameRate, Tick};
use serde_json::json;

use crate::Session;
use crate::media_test_util::{make_movie, tmp_dir};

/// A little-endian EXIF block holding only an Orientation tag, as the media crate's own
/// orientation tests build one.
fn orientation_exif(o: u16) -> Vec<u8> {
    let [a, b] = o.to_le_bytes();
    let mut out = b"II*\0".to_vec();
    out.extend(8u32.to_le_bytes());
    out.extend(1u16.to_le_bytes());
    out.extend(0x0112u16.to_le_bytes());
    out.extend(3u16.to_le_bytes());
    out.extend(1u32.to_le_bytes());
    out.extend([a, b, 0, 0]);
    out.extend(0u32.to_le_bytes());
    out
}

/// A flat `w` x `h` PNG carrying EXIF Orientation `o`.
fn oriented_png(path: &Path, w: u32, h: u32, o: u16) {
    use image::ImageEncoder;
    let px: Vec<u8> = (0..w * h).flat_map(|i| [(i % 256) as u8, 90, 160]).collect();
    let mut out = Vec::new();
    let mut enc = image::codecs::png::PngEncoder::new(&mut out);
    enc.set_exif_metadata(orientation_exif(o)).unwrap();
    enc.write_image(&px, w, h, image::ExtendedColorType::Rgb8).unwrap();
    std::fs::write(path, out).unwrap();
}

fn stored_size(s: &Session, item: ItemId) -> (u32, u32) {
    let v = s.project.item(item).and_then(|i| i.as_media()).and_then(|m| m.info.video.as_ref()).unwrap();
    (v.width, v.height)
}

fn placed_anchor(s: &Session, item: ItemId) -> (f64, f64) {
    for q in s.project.items.values().filter_map(|i| match &i.kind {
        ItemKind::Sequence(q) => Some(q),
        _ => None,
    }) {
        for t in q.tracks(filmcraft_project::TrackKind::Video).iter().chain(q.tracks(filmcraft_project::TrackKind::Audio)) {
            for ti in &t.items {
                if ti.item == item {
                    let m = ti.effect("motion").unwrap();
                    match &m.param("anchor").unwrap().value {
                        ParamValue::Vec2(v) => return (v.x, v.y),
                        other => panic!("anchor is {other:?}"),
                    }
                }
            }
        }
    }
    panic!("no clip placed from item {item:?}");
}

/// Import the still at `path`, place it in a fresh sequence and save the project built so
/// far to `proj`; returns the item id.
fn import_place_save(s: &mut Session, path: &Path, proj: &Path) -> ItemId {
    let r = s.execute("file.import", json!({"paths": [path.to_string_lossy()]})).unwrap();
    let item = ItemId(r["items"][0].as_u64().unwrap());
    s.execute("file.newSequence", json!({"video": 1, "audio": 0})).unwrap();
    s.execute("timeline.place", json!({"item": item.0, "track": "V1", "seconds": 0.0})).unwrap();
    s.execute("file.save", json!({"path": proj.to_string_lossy()})).unwrap();
    item
}

fn reopen(proj: &Path) -> Session {
    let mut s = Session::default();
    s.execute("file.open", json!({"path": proj.to_string_lossy()})).unwrap();
    s
}

/// The pre-#1583 state, built honestly: the stored size is put back to the file's stored
/// (unturned) size first, so the placement below bakes its anchor from that size, exactly
/// as a build of the time did. Unsaved: the caller sets anchors, saves, and reopens.
fn legacy_built(dir: &Path, o: u16) -> (Session, ItemId) {
    let png = dir.join("photo.png");
    oriented_png(&png, 40, 20, o);
    let mut s = Session::default();
    let r = s.execute("file.import", json!({"paths": [png.to_string_lossy()]})).unwrap();
    let item = ItemId(r["items"][0].as_u64().unwrap());
    assert_eq!(stored_size(&s, item), (20, 40), "a fresh import stores the oriented size");
    s.edit("legacy size", |p, _| {
        let v = p.item_mut(item).and_then(|i| i.as_media_mut()).and_then(|m| m.info.video.as_mut()).unwrap();
        (v.width, v.height) = (40, 20);
        Ok(())
    })
    .unwrap();
    s.execute("file.newSequence", json!({"video": 1, "audio": 0})).unwrap();
    s.execute("timeline.place", json!({"item": item.0, "track": "V1", "seconds": 0.0})).unwrap();
    assert_eq!(placed_anchor(&s, item), (20.0, 10.0), "the legacy placement bakes the unturned centre as the anchor");
    (s, item)
}

fn legacy_project(dir: &Path, o: u16) -> (Session, ItemId) {
    let (mut s, item) = legacy_built(dir, o);
    let proj = dir.join("legacy.fcproj");
    s.execute("file.save", json!({"path": proj.to_string_lossy()})).unwrap();
    (reopen(&proj), item)
}

/// Where a stored anchor (x, y) of a 40 x 20 still lands once the picture is turned
/// upright, for each transposing EXIF Orientation: the mapping the decoder applies to
/// the pixels (crates/media/tests/still_orientation.rs), in continuous coordinates.
fn turned_anchor(o: u16, x: f64, y: f64) -> (f64, f64) {
    match o {
        5 => (y, x),
        6 => (20.0 - y, x),
        7 => (20.0 - y, 40.0 - x),
        8 => (y, 40.0 - x),
        _ => (x, y),
    }
}

/// Set the placed clip's Motion anchor to a noncentre point, with a noncentre keyframe.
fn set_anchor(s: &mut Session, placed: ItemId, x: f64, y: f64, kx: f64, ky: f64) {
    s.edit("set a noncentre anchor", |p, _| {
        for it in p.items.values_mut() {
            let ItemKind::Sequence(q) = &mut it.kind else { continue };
            for t in std::sync::Arc::make_mut(q).tracks_mut(filmcraft_project::TrackKind::Video) {
                for ti in &mut t.items {
                    if ti.item != placed {
                        continue;
                    }
                    for e in &mut ti.effects {
                        if e.effect == "motion"
                            && let Some(param) = e.param_mut("anchor")
                        {
                            param.value = ParamValue::Vec2(Vec2::new(x, y));
                            param.keyframes.push(Keyframe::new(Tick(0), ParamValue::Vec2(Vec2::new(kx, ky))));
                        }
                    }
                }
            }
        }
        Ok(())
    })
    .unwrap();
}

fn placed_anchor_keyframes(s: &Session, item: ItemId) -> Vec<(f64, f64)> {
    for q in s.project.items.values().filter_map(|i| match &i.kind {
        ItemKind::Sequence(q) => Some(q),
        _ => None,
    }) {
        for t in q.tracks(filmcraft_project::TrackKind::Video).iter().chain(q.tracks(filmcraft_project::TrackKind::Audio)) {
            for ti in &t.items {
                if ti.item == item {
                    let m = ti.effect("motion").unwrap();
                    let p = m.param("anchor").unwrap();
                    return p
                        .keyframes
                        .iter()
                        .map(|k| match &k.value {
                            ParamValue::Vec2(v) => (v.x, v.y),
                            other => panic!("keyframe anchor is {other:?}"),
                        })
                        .collect();
                }
            }
        }
    }
    panic!("no clip placed from item {item:?}");
}

#[test]
fn a_legacy_portrait_still_is_migrated_on_open() {
    for o in [5u16, 6, 7, 8] {
        let dir = tmp_dir(&format!("still-migration-{o}"));
        let (s, item) = legacy_project(&dir, o);
        assert_eq!(stored_size(&s, item), (20, 40), "orientation {o}: the stored size is swapped to the oriented one");
        assert_eq!(placed_anchor(&s, item), (10.0, 20.0), "orientation {o}: the anchor turns with the size");
        let _ = std::fs::remove_dir_all(&dir);
    }
}

#[test]
fn a_current_project_opens_unchanged() {
    let dir = tmp_dir("still-migration-current");
    let png = dir.join("photo.png");
    oriented_png(&png, 40, 20, 6);
    let mut s = Session::default();
    let proj = dir.join("current.fcproj");
    let item = import_place_save(&mut s, &png, &proj);
    assert_eq!(placed_anchor(&s, item), (10.0, 20.0), "a fresh placement anchors at the oriented centre");
    let s = reopen(&proj);
    assert_eq!(stored_size(&s, item), (20, 40));
    assert_eq!(placed_anchor(&s, item), (10.0, 20.0));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn an_unturned_still_and_a_movie_are_not_migrated() {
    let dir = tmp_dir("still-migration-untouched");
    // Orientation 1: nothing is transposed, so even a hand-swapped stored size is not the
    // legacy pattern (the file's own size is the stored one).
    let png = dir.join("flat.png");
    oriented_png(&png, 40, 20, 1);
    let mut s = Session::default();
    let proj = dir.join("flat.fcproj");
    let item = import_place_save(&mut s, &png, &proj);
    let s = reopen(&proj);
    assert_eq!(stored_size(&s, item), (40, 20));
    assert_eq!(placed_anchor(&s, item), (20.0, 10.0));
    // A movie whose stored size is the transpose of its real size is damaged some other
    // way; the still migration must not touch it.
    let mov = dir.join("clip.mov");
    make_movie(&mov, DemoScene::Aurora, 40, 20, 4);
    let mut s = Session::default();
    let r = s.execute("file.import", json!({"paths": [mov.to_string_lossy()]})).unwrap();
    let movie = ItemId(r["items"][0].as_u64().unwrap());
    s.edit("swapped movie size", |p, _| {
        let v = p.item_mut(movie).and_then(|i| i.as_media_mut()).and_then(|m| m.info.video.as_mut()).unwrap();
        (v.width, v.height) = (20, 40);
        Ok(())
    })
    .unwrap();
    let proj = dir.join("movie.fcproj");
    s.execute("file.save", json!({"path": proj.to_string_lossy()})).unwrap();
    let s = reopen(&proj);
    assert_eq!(stored_size(&s, movie), (20, 40), "movies keep their stored size");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_legacy_noncentre_anchor_turns_with_the_orientation() {
    // A centre anchor is invariant under every transposing orientation, so it cannot
    // tell a plain axis swap from the orientation's own mapping: for Orientation 6 a
    // stored (4, 6) must land at (14, 4), not (6, 4).
    for o in [5u16, 6, 7, 8] {
        let dir = tmp_dir(&format!("still-migration-anchor-{o}"));
        let (mut s, item) = legacy_built(&dir, o);
        set_anchor(&mut s, item, 4.0, 6.0, 30.0, 15.0);
        let proj = dir.join("legacy.fcproj");
        s.execute("file.save", json!({"path": proj.to_string_lossy()})).unwrap();
        let s = reopen(&proj);
        assert_eq!(stored_size(&s, item), (20, 40), "orientation {o}: the stored size is swapped to the oriented one");
        assert_eq!(placed_anchor(&s, item), turned_anchor(o, 4.0, 6.0), "orientation {o}: a noncentre anchor turns with the picture");
        assert_eq!(placed_anchor_keyframes(&s, item), vec![turned_anchor(o, 30.0, 15.0)], "orientation {o}: a keyframed anchor turns too");
        let _ = std::fs::remove_dir_all(&dir);
    }
}

#[test]
fn a_clip_placed_from_a_subclip_is_migrated_too() {
    let dir = tmp_dir("still-migration-subclip");
    let (mut s, item) = legacy_built(&dir, 6);
    let rate = FrameRate::FPS_29_97;
    s.execute("source.open", json!({"item": item.0})).unwrap();
    s.execute("project.setMarks", json!({"item": item.0, "in": rate.tick_of(0).0, "out": rate.tick_of(24).0})).unwrap();
    let r = s.execute("clip.makeSubclip", json!({})).unwrap();
    let sub = ItemId(r["item"].as_u64().unwrap());
    s.execute("timeline.place", json!({"item": sub.0, "track": "V1", "seconds": 5.0})).unwrap();
    assert_eq!(placed_anchor(&s, sub), (20.0, 10.0), "the subclip placement bakes the unturned centre as the anchor");
    set_anchor(&mut s, sub, 4.0, 6.0, 30.0, 15.0);
    let proj = dir.join("legacy.fcproj");
    s.execute("file.save", json!({"path": proj.to_string_lossy()})).unwrap();
    let s = reopen(&proj);
    assert_eq!(stored_size(&s, item), (20, 40));
    assert_eq!(placed_anchor(&s, sub), (14.0, 4.0), "a clip placed from a subclip of the still turns its anchor too");
    assert_eq!(placed_anchor_keyframes(&s, sub), vec![(5.0, 30.0)]);
    let _ = std::fs::remove_dir_all(&dir);
}

/// A 16 x 12 RGGB DNG of flat grey, as the media crate's own DNG tests build one,
/// with an Orientation tag of value `o` in IFD 0.
fn dng_with_orientation(o: u16) -> Vec<u8> {
    let (w, h) = (16, 12);
    let neutral = [0.5, 1.0, 0.5];
    let pat = [0usize, 1, 1, 2];
    let px: Vec<u16> = (0..w * h).map(|i| (0.5f64 * neutral[pat[(i / w % 2) * 2 + i % w % 2]] * 4095.0).round() as u16).collect();
    let main = vec![
        (tags::AS_SHOT_NEUTRAL, Value::rationals(&neutral)),
        // TIFF tag 274 (Orientation); craft-tiff's tags module does not name it.
        (274, Value::Short(vec![o])),
    ];
    let opts = Options {
        width: w,
        height: h,
        bits: 12,
        compression: Compression::LosslessJpeg { predictor: 1, components: 2, restart_lines: 0, pt: 0 },
        raw_tags: vec![(tags::WHITE_LEVEL, Value::Short(vec![4095]))],
        main_tags: main,
        preview: true,
        ..Default::default()
    };
    write(&px, &opts)
}

#[test]
fn a_dng_with_an_orientation_tag_is_not_migrated() {
    // DNG ignores EXIF Orientation: the raw develops at its unturned size, so a DNG
    // with Orientation 6 keeps that size and its anchors on reopen.
    let dir = tmp_dir("still-migration-dng");
    let dng = dir.join("frame.dng");
    std::fs::write(&dng, dng_with_orientation(6)).unwrap();
    let mut s = Session::default();
    let proj = dir.join("dng.fcproj");
    let item = import_place_save(&mut s, &dng, &proj);
    assert_eq!(stored_size(&s, item), (16, 12), "a DNG import stores the raw size, whatever its Orientation tag says");
    assert_eq!(placed_anchor(&s, item), (8.0, 6.0));
    let s = reopen(&proj);
    assert_eq!(stored_size(&s, item), (16, 12), "the migration leaves a DNG alone");
    assert_eq!(placed_anchor(&s, item), (8.0, 6.0));
    let _ = std::fs::remove_dir_all(&dir);
}
