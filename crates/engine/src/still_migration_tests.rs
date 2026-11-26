//! #4613: a project saved before stills applied EXIF Orientation (#1583) keeps the size the
//! file was stored at, unturned. The picture itself decodes upright today, and a clip placed
//! from the still baked its Motion anchor from the unturned size, so on reopen the still
//! shows scaled up and off-centre. Opening such a project must swap the stored size to the
//! oriented one and turn the placed clips' anchor points with it; a project saved by a
//! current build must open byte-identical in everything the migration could touch.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

use filmcraft_dng::tiff::tags;
use filmcraft_dng::writer::{Compression, Options, Value, write};
use filmcraft_geom::Vec2;
use filmcraft_media::{ByteReader, DemoScene, SharedReader};
use filmcraft_project::{ItemId, ItemKind, Keyframe, Param, ParamValue};
use filmcraft_time::{FrameRate, Tick};
use serde_json::json;

use crate::media_test_util::{make_movie, tmp_dir};
use crate::{FsServices, Services, Session};

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

/// Set the placed clip's Motion anchor value, leaving its keyframes alone.
fn set_anchor_value(s: &mut Session, placed: ItemId, x: f64, y: f64) {
    s.edit("set an anchor value", |p, _| {
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

fn placed_motion_param(s: &Session, item: ItemId, name: &str) -> Param {
    for q in s.project.items.values().filter_map(|i| match &i.kind {
        ItemKind::Sequence(q) => Some(q),
        _ => None,
    }) {
        for t in q.tracks(filmcraft_project::TrackKind::Video).iter().chain(q.tracks(filmcraft_project::TrackKind::Audio)) {
            for ti in &t.items {
                if ti.item == item {
                    return ti.effect("motion").unwrap().param(name).unwrap().clone();
                }
            }
        }
    }
    panic!("no clip placed from item {item:?}");
}

fn placed_anchors_all(s: &Session, item: ItemId) -> Vec<(f64, f64)> {
    let mut out = Vec::new();
    for q in s.project.items.values().filter_map(|i| match &i.kind {
        ItemKind::Sequence(q) => Some(q),
        _ => None,
    }) {
        for t in q.tracks(filmcraft_project::TrackKind::Video).iter().chain(q.tracks(filmcraft_project::TrackKind::Audio)) {
            for ti in &t.items {
                if ti.item == item {
                    let m = ti.effect("motion").unwrap();
                    match &m.param("anchor").unwrap().value {
                        ParamValue::Vec2(v) => out.push((v.x, v.y)),
                        other => panic!("anchor is {other:?}"),
                    }
                }
            }
        }
    }
    out
}

fn set_position(s: &mut Session, placed: ItemId, x: f64, y: f64) {
    s.edit("set position", |p, _| {
        for it in p.items.values_mut() {
            let ItemKind::Sequence(q) = &mut it.kind else { continue };
            for t in std::sync::Arc::make_mut(q).tracks_mut(filmcraft_project::TrackKind::Video) {
                for ti in &mut t.items {
                    if ti.item != placed {
                        continue;
                    }
                    for e in &mut ti.effects {
                        if e.effect == "motion"
                            && let Some(param) = e.param_mut("position")
                        {
                            param.value = ParamValue::Vec2(Vec2::new(x, y));
                        }
                    }
                }
            }
        }
        Ok(())
    })
    .unwrap();
}

fn save_and_reopen(s: &mut Session, proj: &Path) -> Session {
    s.execute("file.save", json!({"path": proj.to_string_lossy()})).unwrap();
    reopen(proj)
}

#[test]
fn a_legacy_portrait_still_is_migrated_on_open() {
    for o in [5u16, 6, 7, 8] {
        let dir = tmp_dir(&format!("still-migration-{o}"));
        let (mut s, item) = legacy_built(&dir, o);
        set_position(&mut s, item, 120.0, -45.0);
        let want_pos = placed_motion_param(&s, item, "position");
        let want_scale = placed_motion_param(&s, item, "scale");
        let want_rot = placed_motion_param(&s, item, "rotation");
        let mut s = save_and_reopen(&mut s, &dir.join("legacy.fcproj"));
        assert_eq!(stored_size(&s, item), (20, 40), "orientation {o}: the stored size is swapped to the oriented one");
        assert_eq!(placed_anchor(&s, item), (10.0, 20.0), "orientation {o}: the anchor turns with the size");
        assert_eq!(placed_motion_param(&s, item, "position"), want_pos, "orientation {o}: position is unchanged");
        assert_eq!(placed_motion_param(&s, item, "scale"), want_scale, "orientation {o}: scale is unchanged");
        assert_eq!(placed_motion_param(&s, item, "rotation"), want_rot, "orientation {o}: rotation is unchanged");
        let s = save_and_reopen(&mut s, &dir.join("migrated.fcproj"));
        assert_eq!(stored_size(&s, item), (20, 40), "orientation {o}: second reopen keeps stored size");
        assert_eq!(placed_anchor(&s, item), (10.0, 20.0), "orientation {o}: second reopen keeps anchor without drift");
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
        set_position(&mut s, item, 320.0, 180.0);
        let want_pos = placed_motion_param(&s, item, "position");
        let mut s = save_and_reopen(&mut s, &dir.join("legacy.fcproj"));
        assert_eq!(stored_size(&s, item), (20, 40), "orientation {o}: the stored size is swapped to the oriented one");
        assert_eq!(placed_anchor(&s, item), turned_anchor(o, 4.0, 6.0), "orientation {o}: a noncentre anchor turns with the picture");
        assert_eq!(placed_anchor_keyframes(&s, item), vec![turned_anchor(o, 30.0, 15.0)], "orientation {o}: a keyframed anchor turns too");
        assert_eq!(placed_motion_param(&s, item, "position"), want_pos, "orientation {o}: position is unchanged");
        let s = save_and_reopen(&mut s, &dir.join("migrated.fcproj"));
        assert_eq!(stored_size(&s, item), (20, 40), "orientation {o}: second reopen keeps size");
        assert_eq!(placed_anchor(&s, item), turned_anchor(o, 4.0, 6.0), "orientation {o}: second reopen keeps anchor");
        assert_eq!(placed_anchor_keyframes(&s, item), vec![turned_anchor(o, 30.0, 15.0)], "orientation {o}: second reopen keeps keyframes");
        assert_eq!(placed_motion_param(&s, item, "position"), want_pos, "orientation {o}: second reopen keeps position");
        let _ = std::fs::remove_dir_all(&dir);
    }
}

#[test]
fn a_clip_placed_from_a_subclip_is_migrated_too() {
    for o in [5u16, 6, 7, 8] {
        let dir = tmp_dir(&format!("still-migration-subclip-{o}"));
        let (mut s, item) = legacy_built(&dir, o);
        let rate = FrameRate::FPS_29_97;
        s.execute("source.open", json!({"item": item.0})).unwrap();
        s.execute("project.setMarks", json!({"item": item.0, "in": rate.tick_of(0).0, "out": rate.tick_of(24).0})).unwrap();
        let r = s.execute("clip.makeSubclip", json!({})).unwrap();
        let sub = ItemId(r["item"].as_u64().unwrap());
        s.execute("timeline.place", json!({"item": sub.0, "track": "V1", "seconds": 5.0})).unwrap();
        assert_eq!(placed_anchor(&s, sub), (20.0, 10.0), "orientation {o}: subclip placement bakes the unturned centre");
        set_anchor(&mut s, sub, 4.0, 6.0, 30.0, 15.0);
        set_position(&mut s, sub, 111.0, 222.0);
        let want_pos = placed_motion_param(&s, sub, "position");
        let mut s = save_and_reopen(&mut s, &dir.join("legacy.fcproj"));
        assert_eq!(stored_size(&s, item), (20, 40));
        assert_eq!(placed_anchor(&s, sub), turned_anchor(o, 4.0, 6.0), "orientation {o}: subclip anchor turns");
        assert_eq!(placed_anchor_keyframes(&s, sub), vec![turned_anchor(o, 30.0, 15.0)]);
        assert_eq!(placed_motion_param(&s, sub, "position"), want_pos, "orientation {o}: subclip position unchanged");
        let s = save_and_reopen(&mut s, &dir.join("migrated.fcproj"));
        assert_eq!(placed_anchor(&s, sub), turned_anchor(o, 4.0, 6.0), "orientation {o}: subclip second reopen keeps anchor");
        assert_eq!(placed_anchor_keyframes(&s, sub), vec![turned_anchor(o, 30.0, 15.0)]);
        let _ = std::fs::remove_dir_all(&dir);
    }
}

#[test]
fn a_subclip_saved_with_the_fallback_anchor_is_recentred() {
    // The saved state a build from before the subclip source-size fix wrote: placing a
    // subclip had no size for it, so the anchor was resolved against a 1920 x 1080
    // fallback frame and saved as its centre, (960, 540) — not a point in the still's
    // coordinates at all. (The current build places the subclip against the still's own
    // size, so the fixture puts the saved value back, as `legacy_built` does for the
    // stored size.) Turning (960, 540) like a stored point of a 40 x 20 still would
    // land it off-canvas; as the frame centre it belongs at the upright centre.
    for o in [5u16, 6, 7, 8] {
        let dir = tmp_dir(&format!("still-migration-subclip-fallback-{o}"));
        let (mut s, item) = legacy_built(&dir, o);
        let rate = FrameRate::FPS_29_97;
        s.execute("source.open", json!({"item": item.0})).unwrap();
        s.execute("project.setMarks", json!({"item": item.0, "in": rate.tick_of(0).0, "out": rate.tick_of(24).0})).unwrap();
        let r = s.execute("clip.makeSubclip", json!({})).unwrap();
        let sub = ItemId(r["item"].as_u64().unwrap());
        s.execute("timeline.place", json!({"item": sub.0, "track": "V1", "seconds": 5.0})).unwrap();
        set_anchor_value(&mut s, sub, 960.0, 540.0);
        set_position(&mut s, sub, 400.0, 300.0);
        let want_pos = placed_motion_param(&s, sub, "position");
        let mut s = save_and_reopen(&mut s, &dir.join("legacy.fcproj"));
        assert_eq!(stored_size(&s, item), (20, 40));
        assert_eq!(placed_anchor(&s, sub), (10.0, 20.0), "orientation {o}: fallback centre migrates to upright centre");
        assert_eq!(placed_motion_param(&s, sub, "position"), want_pos, "orientation {o}: position unchanged");
        // Idempotent: the migrated project, saved and reopened, changes nothing.
        let s = save_and_reopen(&mut s, &dir.join("migrated.fcproj"));
        assert_eq!(stored_size(&s, item), (20, 40));
        assert_eq!(placed_anchor(&s, sub), (10.0, 20.0), "orientation {o}: second reopen leaves anchor unchanged");
        assert_eq!(placed_motion_param(&s, sub, "position"), want_pos);
        let _ = std::fs::remove_dir_all(&dir);
    }
}

#[test]
fn a_copied_clip_and_controls_migrate_without_drift() {
    for o in [5u16, 6, 7, 8] {
        let dir = tmp_dir(&format!("still-migration-copied-{o}"));
        let (mut s, item) = legacy_built(&dir, o);
        // Place a second copy of the still and copy edited + keyframed controls onto both.
        s.execute("timeline.place", json!({"item": item.0, "track": "V1", "seconds": 5.0})).unwrap();
        set_anchor(&mut s, item, 4.0, 6.0, 30.0, 15.0);
        set_position(&mut s, item, 250.0, 140.0);
        let mut s = save_and_reopen(&mut s, &dir.join("legacy.fcproj"));
        assert_eq!(stored_size(&s, item), (20, 40));
        assert_eq!(placed_anchors_all(&s, item), vec![turned_anchor(o, 4.0, 6.0), turned_anchor(o, 4.0, 6.0)]);
        let s = save_and_reopen(&mut s, &dir.join("migrated.fcproj"));
        assert_eq!(placed_anchors_all(&s, item), vec![turned_anchor(o, 4.0, 6.0), turned_anchor(o, 4.0, 6.0)]);
        let _ = std::fs::remove_dir_all(&dir);
    }
}

#[test]
fn opening_a_project_probes_stills_without_caching_a_source() {
    // The migration decides from the file's headers alone: opening a project, current
    // or legacy, must not pull the still's decoded picture into the media pool.
    let dir = tmp_dir("still-migration-no-decode-current");
    let png = dir.join("photo.png");
    oriented_png(&png, 40, 20, 6);
    let mut s = Session::default();
    let proj = dir.join("current.fcproj");
    let item = import_place_save(&mut s, &png, &proj);
    let s = reopen(&proj);
    assert!(s.media.cached(item).is_none(), "opening a current project cached a decoded source for its still");
    let _ = std::fs::remove_dir_all(&dir);
    let dir = tmp_dir("still-migration-no-decode-legacy");
    let (s, item) = legacy_project(&dir, 6);
    assert_eq!(stored_size(&s, item), (20, 40), "the legacy still was migrated");
    assert!(s.media.cached(item).is_none(), "the migration cached a decoded source for the still");
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

struct SparseTiffReader {
    len: u64,
    ifd_offset: u64,
    header: [u8; 8],
    ifd_block: Vec<u8>,
    bytes_read: Arc<AtomicU64>,
}

impl SparseTiffReader {
    fn new_orientation_6(bytes_read: Arc<AtomicU64>) -> Self {
        let ifd_offset = 72_000_008u64;
        let mut header = [0u8; 8];
        header[0..4].copy_from_slice(b"II*\0");
        header[4..8].copy_from_slice(&(ifd_offset as u32).to_le_bytes());

        let entries: [(u16, u16, u32, [u8; 4]); 10] = [
            (256, 4, 1, 6000u32.to_le_bytes()),
            (257, 4, 1, 4000u32.to_le_bytes()),
            (258, 3, 3, ((ifd_offset + 2 + 10 * 12 + 4) as u32).to_le_bytes()),
            (259, 3, 1, [1, 0, 0, 0]),
            (262, 3, 1, [2, 0, 0, 0]),
            (273, 4, 1, 8u32.to_le_bytes()),
            (274, 3, 1, [6, 0, 0, 0]),
            (277, 3, 1, [3, 0, 0, 0]),
            (278, 4, 1, 4000u32.to_le_bytes()),
            (279, 4, 1, 72_000_000u32.to_le_bytes()),
        ];
        let mut ifd_block = Vec::new();
        ifd_block.extend((entries.len() as u16).to_le_bytes());
        for (tag, ty, count, val) in entries {
            ifd_block.extend(tag.to_le_bytes());
            ifd_block.extend(ty.to_le_bytes());
            ifd_block.extend(count.to_le_bytes());
            ifd_block.extend(val);
        }
        ifd_block.extend(0u32.to_le_bytes());
        for bits in [8u16, 8, 8] {
            ifd_block.extend(bits.to_le_bytes());
        }
        let len = ifd_offset + ifd_block.len() as u64;
        Self { len, ifd_offset, header, ifd_block, bytes_read }
    }
}

impl ByteReader for SparseTiffReader {
    fn len(&self) -> u64 {
        self.len
    }

    fn read_at(&self, offset: u64, buf: &mut [u8]) -> std::io::Result<()> {
        if offset.saturating_add(buf.len() as u64) > self.len {
            return Err(std::io::Error::new(std::io::ErrorKind::UnexpectedEof, "past eof"));
        }
        self.bytes_read.fetch_add(buf.len() as u64, Ordering::Relaxed);
        buf.fill(0);
        for (i, dst) in buf.iter_mut().enumerate() {
            let pos = offset + i as u64;
            if pos < 8 {
                *dst = self.header[pos as usize];
            } else if pos >= self.ifd_offset && pos < self.ifd_offset + self.ifd_block.len() as u64 {
                *dst = self.ifd_block[(pos - self.ifd_offset) as usize];
            }
        }
        Ok(())
    }
}

struct SpyServices {
    tiff_path: PathBuf,
    bytes_read: Arc<AtomicU64>,
    read_file_calls: Arc<AtomicUsize>,
}

impl Services for SpyServices {
    fn read_file(&self, path: &str) -> std::io::Result<Vec<u8>> {
        if Path::new(path) == self.tiff_path {
            self.read_file_calls.fetch_add(1, Ordering::Relaxed);
            return Err(std::io::Error::new(std::io::ErrorKind::Unsupported, "whole-file read forbidden"));
        }
        FsServices.read_file(path)
    }

    fn read_file_bounded(&self, path: &str, max: u64) -> std::io::Result<Vec<u8>> {
        if Path::new(path) == self.tiff_path {
            self.read_file_calls.fetch_add(1, Ordering::Relaxed);
            return Err(std::io::Error::new(std::io::ErrorKind::Unsupported, "whole-file read forbidden"));
        }
        FsServices.read_file_bounded(path, max)
    }

    fn write_file(&self, path: &str, data: &[u8]) -> std::io::Result<()> {
        FsServices.write_file(path, data)
    }

    fn file_size(&self, path: &str) -> std::io::Result<u64> {
        if Path::new(path) == self.tiff_path {
            return Ok(SparseTiffReader::new_orientation_6(self.bytes_read.clone()).len());
        }
        FsServices.file_size(path)
    }

    fn reader(&self, path: &str) -> Option<std::io::Result<SharedReader>> {
        if Path::new(path) == self.tiff_path {
            return Some(Ok(Arc::new(SparseTiffReader::new_orientation_6(self.bytes_read.clone()))));
        }
        FsServices.reader(path)
    }
}

#[test]
fn large_tiff_and_bounded_header_read_do_not_decode_full_pixels() {
    let dir = tmp_dir("still-migration-large-tiff-header-only");
    let png = dir.join("seed.png");
    oriented_png(&png, 40, 20, 6);
    let large_tiff = dir.join("large_6000x4000_o6.tif");

    let mut s = Session::default();
    let r = s.execute("file.import", json!({"paths": [png.to_string_lossy()]})).unwrap();
    let item = ItemId(r["items"][0].as_u64().unwrap());
    s.edit("make 6000x4000 legacy tiff", |p, _| {
        let m = p.item_mut(item).and_then(|i| i.as_media_mut()).unwrap();
        m.media = filmcraft_project::MediaRef::File { path: large_tiff.to_string_lossy().into_owned() };
        let v = m.info.video.as_mut().unwrap();
        (v.width, v.height) = (6000, 4000);
        Ok(())
    })
    .unwrap();
    s.execute("file.newSequence", json!({"video": 1, "audio": 0})).unwrap();
    s.execute("timeline.place", json!({"item": item.0, "track": "V1", "seconds": 0.0})).unwrap();
    set_position(&mut s, item, 150.0, 250.0);
    let want_pos = placed_motion_param(&s, item, "position");
    let proj = dir.join("legacy.fcproj");
    s.execute("file.save", json!({"path": proj.to_string_lossy()})).unwrap();

    let bytes_read = Arc::new(AtomicU64::new(0));
    let read_file_calls = Arc::new(AtomicUsize::new(0));
    let spy = Arc::new(SpyServices { tiff_path: large_tiff, bytes_read: bytes_read.clone(), read_file_calls: read_file_calls.clone() });
    let mut s = Session::new(spy.clone());
    s.execute("file.open", json!({"path": proj.to_string_lossy()})).unwrap();

    assert_eq!(stored_size(&s, item), (4000, 6000), ">64 MiB TIFF with IFD past 64 MiB must still migrate");
    assert_eq!(placed_anchor(&s, item), (2000.0, 3000.0));
    assert_eq!(placed_motion_param(&s, item, "position"), want_pos);
    assert_eq!(read_file_calls.load(Ordering::Relaxed), 0, "migration must not fall back to whole-file reads");
    assert!(
        bytes_read.load(Ordering::Relaxed) <= 256 * 1024,
        "migration must read only bounded header bytes, read {} bytes",
        bytes_read.load(Ordering::Relaxed)
    );
    assert!(s.media.cached(item).is_none(), "migration must not decode or cache full raster pixels");

    let proj2 = dir.join("migrated.fcproj");
    s.execute("file.save", json!({"path": proj2.to_string_lossy()})).unwrap();
    let mut s2 = Session::new(spy);
    s2.execute("file.open", json!({"path": proj2.to_string_lossy()})).unwrap();
    assert_eq!(stored_size(&s2, item), (4000, 6000));
    assert_eq!(placed_anchor(&s2, item), (2000.0, 3000.0));
    assert_eq!(placed_motion_param(&s2, item, "position"), want_pos);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn replacing_still_with_normal_orientation_swapped_dimensions_does_not_migrate_motion() {
    let dir = tmp_dir("still-migration-replaced-normal");
    let png = dir.join("photo.png");
    oriented_png(&png, 40, 20, 1);

    let mut s = Session::default();
    let r = s.execute("file.import", json!({"paths": [png.to_string_lossy()]})).unwrap();
    let item = ItemId(r["items"][0].as_u64().unwrap());
    s.execute("file.newSequence", json!({"video": 1, "audio": 0})).unwrap();
    s.execute("timeline.place", json!({"item": item.0, "track": "V1", "seconds": 0.0})).unwrap();
    set_anchor(&mut s, item, 7.0, 9.0, 12.0, 16.0);
    set_position(&mut s, item, 310.0, 220.0);
    let want_pos = placed_motion_param(&s, item, "position");
    let proj = dir.join("project.fcproj");
    s.execute("file.save", json!({"path": proj.to_string_lossy()})).unwrap();

    // Replace the file at the same path with a normal-orientation (1) still whose
    // dimensions are the transpose of the stored ones.
    oriented_png(&png, 20, 40, 1);

    let mut s = reopen(&proj);
    assert_eq!(stored_size(&s, item), (40, 20), "normal-orientation replacement must not swap stored size");
    assert_eq!(placed_anchor(&s, item), (7.0, 9.0), "normal-orientation replacement must preserve noncentral static anchor");
    assert_eq!(placed_anchor_keyframes(&s, item), vec![(12.0, 16.0)], "normal-orientation replacement must preserve keyframed anchor");
    assert_eq!(placed_motion_param(&s, item, "position"), want_pos, "normal-orientation replacement must preserve position");

    let s = save_and_reopen(&mut s, &dir.join("resaved.fcproj"));
    assert_eq!(stored_size(&s, item), (40, 20));
    assert_eq!(placed_anchor(&s, item), (7.0, 9.0));
    assert_eq!(placed_anchor_keyframes(&s, item), vec![(12.0, 16.0)]);
    assert_eq!(placed_motion_param(&s, item, "position"), want_pos);
    let _ = std::fs::remove_dir_all(&dir);
}
