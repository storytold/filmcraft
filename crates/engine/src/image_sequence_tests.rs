//! Image sequences: File ▸ Import with Image Sequence, the Import image sequences setting,
//! missing frames, the indeterminate media timebase, timeline placement and project reopen.

use std::path::Path;

use filmcraft_media::{FrameRequest, MediaKind};
use filmcraft_project::{ClipId, ItemId};
use filmcraft_time::FrameRate;
use serde_json::{Value, json};

use crate::Session;
use crate::media_test_util::tmp_dir;

#[test]
fn import_accepts_uppercase_extensions() {
    let dir = tmp_dir("import-uppercase");
    let wav = dir.join("TONE.WAV");
    std::fs::write(&wav, filmcraft_media::wav::write_wav16(&[0.1; 4800], 2, 48_000)).unwrap();
    let mut s = Session::default();
    let r = s.execute("file.import", json!({"paths": [wav.to_string_lossy()]})).unwrap();
    assert_eq!(r["items"].as_array().unwrap().len(), 1);
    assert!(s.project.items.values().any(|it| it.name == "TONE.WAV"));
}

#[test]
fn import_replace_starts_from_an_empty_project() {
    let dir = tmp_dir("import-replace");
    let wav = dir.join("tone.wav");
    std::fs::write(&wav, filmcraft_media::wav::write_wav16(&[0.1; 4800], 2, 48_000)).unwrap();
    let mut s = demo();
    let demo_items = s.project.items.len();
    assert!(demo_items > 1);
    assert!(!s.state.open_sequences.is_empty());
    s.execute("file.import", json!({"paths": [wav.to_string_lossy()], "replace": true})).unwrap();
    assert!(s.state.open_sequences.is_empty());
    assert_eq!(s.project.items.len(), 1);
    assert!(s.project.items.values().any(|it| it.name == "tone.wav"));
    let mut merge = demo();
    merge.execute("file.import", json!({"paths": [wav.to_string_lossy()]})).unwrap();
    assert!(merge.project.items.len() > demo_items);
}

fn demo() -> Session {
    let mut s = Session::default();
    s.execute("file.openDemoProject", json!({})).unwrap();
    s
}

/// Frame `i` is a flat colour whose red channel is `10 * i`.
fn write_frames(dir: &Path, prefix: &str, numbers: &[u32], ext: &str) -> Vec<String> {
    numbers
        .iter()
        .map(|&n| {
            let p = dir.join(format!("{prefix}{n:04}.{ext}"));
            image::RgbaImage::from_pixel(32, 18, image::Rgba([(10 * n) as u8, 99, 7, 255])).save(&p).unwrap();
            p.to_string_lossy().to_string()
        })
        .collect()
}

fn red_at(s: &Session, item: ItemId, frame: i64) -> u8 {
    let src = s.media.source_for(&s.project, item, &*s.services).unwrap();
    let rate = src.info().frame_rate();
    let f = src.video_frame(FrameRequest::full(rate.tick_of(frame))).unwrap();
    match &f.data {
        filmcraft_frame::PixelData::Rgba8(p) => p[0],
        _ => panic!("rgba frames"),
    }
}

fn item_of(r: &Value) -> ItemId {
    ItemId(r["items"][0].as_u64().unwrap_or_else(|| panic!("{r}")))
}

#[test]
fn import_as_image_sequence_with_missing_frames() {
    let dir = tmp_dir("imgseq-import");
    let mut s = demo();
    s.execute("prefs.set", json!({"key": "media.indeterminateTimebase", "value": "25"})).unwrap();
    // frames 1..=6 with 4 missing, plus an unrelated file
    let paths = write_frames(&dir, "shot_", &[1, 2, 3, 5, 6], "png");
    write_frames(&dir, "other_", &[1], "png");
    let r = s.execute("file.import", json!({"paths": [paths[0]], "imageSequence": true})).unwrap();
    let id = item_of(&r);
    assert_eq!(r["imageSequences"][0]["frames"], 6);
    assert_eq!(r["imageSequences"][0]["missing"], json!([3]));
    let m = s.project.item(id).unwrap().as_media().unwrap().clone();
    assert_eq!(m.info.kind, MediaKind::ImageSequence);
    assert_eq!(m.info.frame_rate(), FrameRate::FPS_25, "Indeterminate Media Timebase");
    assert_eq!(m.info.duration, FrameRate::FPS_25.tick_of(6));
    assert_eq!((m.info.video.as_ref().unwrap().width, m.info.video.as_ref().unwrap().height), (32, 18));
    assert_eq!(red_at(&s, id, 0), 10);
    assert_eq!(red_at(&s, id, 2), 30);
    assert_eq!(red_at(&s, id, 3), 30, "missing frame 4 holds frame 3");
    assert_eq!(red_at(&s, id, 5), 60);
    // starting mid-sequence: from the chosen number on
    let r = s.execute("file.importImageSequence", json!({"path": paths[2]})).unwrap();
    let mid = item_of(&r);
    assert_eq!(r["imageSequences"][0]["frames"], 4);
    assert_eq!(red_at(&s, mid, 0), 30);
    // onto the timeline: a clip of the sequence's length
    let end = s.active_sequence().unwrap().duration();
    let r = s.execute("timeline.place", json!({"item": id.0, "track": "V1", "time": end.0})).unwrap();
    let clip = ClipId(r["clips"][0].as_u64().unwrap());
    let rate = s.sequence_rate();
    let dur = s.active_sequence().unwrap().find_item(clip).unwrap().1.duration;
    assert_eq!(dur, rate.snap_nearest(FrameRate::FPS_25.tick_of(6)), "{dur:?}");
    // a non-numbered file cannot start a sequence
    let plain = dir.join("plain.png");
    image::RgbaImage::from_pixel(4, 4, image::Rgba([1, 2, 3, 255])).save(&plain).unwrap();
    let e = s.execute("file.import", json!({"paths": [plain.to_string_lossy()], "imageSequence": true})).unwrap_err().to_string();
    assert!(e.contains("numbered"), "{e}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn import_image_sequences_setting_detects_a_numbered_still() {
    let dir = tmp_dir("imgseq-auto");
    let mut s = demo();
    let paths = write_frames(&dir, "render.", &[10, 11, 12], "png");
    // off (default): a still
    let r = s.execute("file.import", json!({"paths": [paths[0]]})).unwrap();
    assert_eq!(s.project.item(item_of(&r)).unwrap().as_media().unwrap().info.kind, MediaKind::Still);
    // on: the sequence
    s.execute("prefs.set", json!({"key": "media.importImageSequences", "value": true})).unwrap();
    let r = s.execute("file.import", json!({"paths": [paths[0]]})).unwrap();
    let id = item_of(&r);
    let m = s.project.item(id).unwrap().as_media().unwrap();
    assert_eq!(m.info.kind, MediaKind::ImageSequence);
    assert_eq!(m.info.duration, m.info.frame_rate().tick_of(3));
    // several files at once stay stills
    let r = s.execute("file.import", json!({"paths": [paths[0], paths[1]]})).unwrap();
    assert!(r.get("imageSequences").is_none());
    assert_eq!(s.project.item(item_of(&r)).unwrap().as_media().unwrap().info.kind, MediaKind::Still);
    // the last frame alone is no sequence
    let r = s.execute("file.import", json!({"paths": [paths[2]]})).unwrap();
    assert_eq!(s.project.item(item_of(&r)).unwrap().as_media().unwrap().info.kind, MediaKind::Still);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn image_sequence_survives_save_and_reopen() {
    let dir = tmp_dir("imgseq-reopen");
    let mut s = demo();
    let paths = write_frames(&dir, "f", &[0, 1, 2, 3], "png");
    let id = item_of(&s.execute("file.importFromMediaBrowser", json!({"paths": [paths[0]], "imageSequence": true})).unwrap());
    let proj = dir.join("seq.fcproj");
    s.execute("file.saveAs", json!({"path": proj.to_string_lossy()})).unwrap();
    let mut t = Session::default();
    t.execute("file.open", json!({"path": proj.to_string_lossy()})).unwrap();
    let m = t.project.item(id).unwrap().as_media().unwrap();
    assert_eq!(m.info.kind, MediaKind::ImageSequence);
    assert_eq!(red_at(&t, id, 3), 30, "frames reopen through the media pool");
    assert!(t.media.offline_status(id).is_none());
    // a deleted frame is held over; deleting the first frame makes the item offline
    std::fs::remove_file(&paths[2]).unwrap();
    let mut u = Session::default();
    u.execute("file.open", json!({"path": proj.to_string_lossy()})).unwrap();
    assert_eq!(red_at(&u, id, 2), 10);
    std::fs::remove_file(&paths[0]).unwrap();
    let mut v = Session::default();
    v.execute("file.open", json!({"path": proj.to_string_lossy()})).unwrap();
    let _ = v.media.source_for(&v.project, id, &*v.services);
    assert!(v.media.offline_status(id).is_some(), "missing first frame: offline");
    let _ = std::fs::remove_dir_all(&dir);
}
