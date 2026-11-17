//! #4613: a project saved before stills applied EXIF Orientation (#1583) keeps the size the
//! file was stored at, unturned. The picture itself decodes upright today, and a clip placed
//! from the still baked its Motion anchor from the unturned size, so on reopen the still
//! shows scaled up and off-centre. Opening such a project must swap the stored size to the
//! oriented one and turn the placed clips' anchor points with it; a project saved by a
//! current build must open byte-identical in everything the migration could touch.

use std::path::Path;

use filmcraft_media::DemoScene;
use filmcraft_project::{ItemId, ItemKind, ParamValue};
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
/// as a build of the time did. Then the project is saved and reopened.
fn legacy_project(dir: &Path, o: u16) -> (Session, ItemId) {
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
    let proj = dir.join("legacy.fcproj");
    s.execute("file.save", json!({"path": proj.to_string_lossy()})).unwrap();
    (reopen(&proj), item)
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
