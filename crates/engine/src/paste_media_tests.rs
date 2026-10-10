//! Pasting files and images from the system clipboard (#611).

use std::path::Path;

use filmcraft_project::{BinEntry, BinId, ClipId, ItemId, TrackKind};
use filmcraft_time::Tick;
use serde_json::{Value, json};

use crate::Session;
use crate::media_test_util::tmp_dir;

fn demo() -> Session {
    let mut s = Session::default();
    s.execute("file.openDemoProject", json!({})).unwrap();
    s
}

fn write_png(path: &Path) -> String {
    image::RgbaImage::from_pixel(64, 36, image::Rgba([40, 120, 200, 255])).save(path).unwrap();
    path.to_string_lossy().into_owned()
}

fn write_wav(path: &Path, seconds: f32) -> String {
    std::fs::write(path, crate::voiceover::write_wav_f32(&vec![0.0; (48_000.0 * seconds) as usize], 48_000)).unwrap();
    path.to_string_lossy().into_owned()
}

/// Every clip of the active sequence: (track kind, track index, clip id, start, end).
fn clips(s: &Session) -> Vec<(TrackKind, usize, u64, Tick, Tick)> {
    let q = s.active_sequence().unwrap();
    let mut v = Vec::new();
    for (kind, tracks) in [(TrackKind::Video, &q.video_tracks), (TrackKind::Audio, &q.audio_tracks)] {
        for (i, t) in tracks.iter().enumerate() {
            v.extend(t.items.iter().map(|c| (kind, i, c.id.0, c.start, c.end())));
        }
    }
    v
}

fn pasted(r: &Value) -> Vec<u64> {
    r["clips"].as_array().unwrap().iter().map(|c| c.as_u64().unwrap()).collect()
}

/// An image pasted over footage goes on the first video track that is free there, from the
/// targeted one up; every clip already on the Timeline stays as it was. One undo step takes the
/// clip and the imported item back; the playhead moves to its end, as after Paste.
#[test]
fn a_pasted_image_sits_above_the_footage_without_overwriting_it() {
    let dir = tmp_dir("paste-media-image");
    let mut s = demo();
    let png = write_png(&dir.join("shot.png"));
    let at = s.playhead();
    let (before, items, undo) = (clips(&s), s.project.items.len(), s.history.undo.len());
    assert!(before.iter().any(|c| c.0 == TrackKind::Video && c.1 == 0 && c.3 <= at && c.4 > at), "the playhead is over a V1 clip");
    let r = s.execute("edit.pasteMedia", json!({"paths": [png]})).unwrap();
    let ids = pasted(&r);
    assert_eq!(ids.len(), 1, "a still has picture only");
    let after = clips(&s);
    for c in &before {
        assert!(after.contains(c), "{c:?} is untouched");
    }
    let new = *after.iter().find(|c| c.2 == ids[0]).unwrap();
    assert_eq!((new.0, new.3), (TrackKind::Video, at));
    assert!(new.1 > 0, "not on V1, where the footage is");
    assert!(!before.iter().any(|c| c.0 == TrackKind::Video && c.1 == new.1 && c.3 < new.4 && c.4 > new.3), "on a track that was free there");
    assert!(before.iter().any(|c| c.0 == TrackKind::Video && c.1 < new.1 && c.3 < new.4 && c.4 > new.3), "so it sits above a clip");
    assert_eq!(new.4 - new.3, s.prefs.timeline.still_duration(s.sequence_rate()), "for the still image default duration");
    assert_eq!(s.playhead(), new.4);
    assert_eq!(s.state.selection, vec![ClipId(ids[0])]);
    assert_eq!(s.history.undo.len(), undo + 1);
    assert_eq!(s.undo().as_deref(), Some("Paste"));
    assert_eq!((clips(&s), s.project.items.len()), (before, items), "undo takes back the clip and the import");
    let _ = std::fs::remove_dir_all(&dir);
}

/// With every video track busy at the playhead a new one is added on top; undo removes it too.
/// Audio goes on the first free audio track from the targeted one down, or a new one at the bottom.
#[test]
fn a_track_is_added_when_every_track_is_busy() {
    let dir = tmp_dir("paste-media-tracks");
    let mut s = demo();
    let png = write_png(&dir.join("a.png"));
    let wav = write_wav(&dir.join("b.wav"), 2.0);
    let at = s.playhead();
    // fill every track at the playhead
    for _ in 0..8 {
        let q = s.active_sequence().unwrap();
        let busy = |ts: &[filmcraft_project::Track]| ts.iter().all(|t| t.items.iter().any(|c| c.start <= at && c.end() > at + Tick::from_seconds_f64(5.0)));
        if busy(&q.video_tracks) && busy(&q.audio_tracks) {
            break;
        }
        s.set_playhead(at);
        s.execute("edit.pasteMedia", json!({"paths": [png.clone(), wav.clone()]})).unwrap();
    }
    let (nv, na) = {
        let q = s.active_sequence().unwrap();
        (q.video_tracks.len(), q.audio_tracks.len())
    };
    let before = clips(&s);
    s.set_playhead(at);
    let r = s.execute("edit.pasteMedia", json!({"paths": [png, wav]})).unwrap();
    let ids = pasted(&r);
    let q = s.active_sequence().unwrap();
    assert_eq!((q.video_tracks.len(), q.audio_tracks.len()), (nv + 1, na + 1), "one new track of each kind");
    let after = clips(&s);
    let image = after.iter().find(|c| c.2 == ids[0]).unwrap();
    assert_eq!((image.0, image.1), (TrackKind::Video, nv), "the image on the new top track");
    let sound = after.iter().find(|c| c.2 == ids[1]).unwrap();
    assert_eq!((sound.0, sound.1, sound.3), (TrackKind::Audio, na, image.4), "the sound on the new bottom track, after the image");
    for c in &before {
        assert!(after.contains(c), "{c:?} is untouched");
    }
    s.undo();
    let q = s.active_sequence().unwrap();
    assert_eq!((q.video_tracks.len(), q.audio_tracks.len()), (nv, na), "undo removes the tracks it added");
    let _ = std::fs::remove_dir_all(&dir);
}

/// `insert` is Paste Insert: on the targeted video track, pushing later clips right.
#[test]
fn paste_insert_inserts_on_the_targeted_track() {
    let dir = tmp_dir("paste-media-insert");
    let mut s = demo();
    for t in ["V2", "V3", "A2", "A3"] {
        let _ = s.execute("timeline.setTrack", json!({"track": t, "syncLock": false}));
    }
    let png = write_png(&dir.join("i.png"));
    let at = s.playhead();
    let last = s.active_sequence().unwrap().video_tracks[0].items.iter().map(|c| c.end()).max().unwrap();
    let r = s.execute("edit.pasteMedia", json!({"paths": [png], "insert": true})).unwrap();
    let new = *clips(&s).iter().find(|c| c.2 == pasted(&r)[0]).unwrap();
    let still = s.prefs.timeline.still_duration(s.sequence_rate());
    assert_eq!((new.0, new.1, new.3), (TrackKind::Video, 0, at), "on the targeted track, V1");
    assert_eq!(s.active_sequence().unwrap().video_tracks[0].items.iter().map(|c| c.end()).max().unwrap(), last + still, "later clips moved right");
    assert_eq!(s.undo().as_deref(), Some("Paste Insert"));
    let _ = std::fs::remove_dir_all(&dir);
}

/// Without `place` (the Project panel) the files are only imported, into `bin`.
#[test]
fn without_place_files_are_only_imported() {
    let dir = tmp_dir("paste-media-import");
    let mut s = demo();
    let png = write_png(&dir.join("p.png"));
    let bin = s.execute("file.newBin", json!({"name": "Pasted"})).unwrap()["bin"].as_u64().unwrap();
    let before = clips(&s);
    let r = s.execute("edit.pasteMedia", json!({"paths": [png], "place": false, "bin": bin})).unwrap();
    assert_eq!(clips(&s), before);
    let item = ItemId(r["items"][0].as_u64().unwrap());
    let pasted_bin = s.project.root.find_bin(BinId(bin)).unwrap();
    assert!(pasted_bin.children.iter().any(|e| matches!(e, BinEntry::Item(i) if *i == item)), "imported into the bin");
    assert!(s.execute("edit.pasteMedia", json!({"paths": []})).is_err(), "nothing to paste");
    let _ = std::fs::remove_dir_all(&dir);
}

/// A pasted image is written as `Pasted Image <n>.png` next to the saved project, with the first
/// number no file or project item uses.
#[test]
fn pasted_images_are_saved_next_to_the_project() {
    let dir = tmp_dir("paste-media-save");
    let mut s = demo();
    s.path = Some(dir.join("film.fcproj").to_string_lossy().into_owned());
    let mut png = Vec::new();
    image::RgbaImage::from_pixel(8, 8, image::Rgba([0, 0, 0, 255])).write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png).unwrap();
    let first = crate::paste_media::save_pasted_image(&mut s, &png).unwrap();
    assert_eq!(Path::new(&first), dir.join("Pasted Image 1.png"));
    assert_eq!(std::fs::read(&first).unwrap(), png);
    let second = crate::paste_media::save_pasted_image(&mut s, &png).unwrap();
    assert_eq!(Path::new(&second), dir.join("Pasted Image 2.png"));
    s.execute("edit.pasteMedia", json!({"paths": [second]})).unwrap();
    assert!(s.project.items.values().any(|i| i.name == "Pasted Image 2.png"));
    let _ = std::fs::remove_dir_all(&dir);
}
