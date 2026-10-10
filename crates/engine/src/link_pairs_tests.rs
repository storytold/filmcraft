//! Clip > Link on several picture/sound pairs gives each picture its own sound (#507).

use super::*;
use serde_json::json;

fn link_of(s: &Session, id: ClipId) -> Option<u64> {
    s.active_sequence().unwrap().find_item(id).unwrap().1.link
}

fn three_pairs(s: &mut Session) -> [(ClipId, ClipId); 3] {
    s.execute("file.openDemoProject", json!({})).unwrap();
    s.execute("file.newSequence", json!({"name": "Link", "video": 1, "audio": 1})).unwrap();
    let rate = s.sequence_rate();
    let item = s.project.items.values().find(|i| i.has_video() && i.has_audio() && i.as_media().is_some()).map(|i| i.id).unwrap();
    for n in 0..3 {
        s.execute("timeline.place", json!({"item": item.0, "frame": 48 * n, "sourceIn": rate.tick_of(48).0, "duration": rate.tick_of(48).0})).unwrap();
    }
    let q = s.active_sequence().unwrap();
    [0, 1, 2].map(|n| (q.video_tracks[0].items[n].id, q.audio_tracks[0].items[n].id))
}

fn reopen(s: &mut Session) -> Session {
    let dir = crate::temp_dir().join(format!("fc-link-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("link.fcproj");
    s.execute("file.saveAs", json!({"path": path.to_string_lossy()})).unwrap();
    let mut s2 = Session::default();
    s2.execute("file.open", json!({"path": path.to_string_lossy()})).unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    s2
}

#[test]
fn linking_several_pairs_gives_each_picture_its_own_sound() {
    let mut s = Session::default();
    let pairs = three_pairs(&mut s);
    let ids: Vec<u64> = pairs.iter().flat_map(|(v, a)| [v.0, a.0]).collect();
    assert_eq!(s.execute("clip.link", json!({"clips": ids.clone()})).unwrap()["linked"], false);
    for (v, a) in pairs {
        assert_eq!((link_of(&s, v), link_of(&s, a)), (None, None));
    }
    assert_eq!(s.execute("clip.link", json!({"clips": ids})).unwrap()["linked"], true);
    let mut distinct = std::collections::BTreeSet::new();
    for (v, a) in pairs {
        let (lv, la) = (link_of(&s, v).unwrap(), link_of(&s, a).unwrap());
        assert_eq!(lv, la);
        distinct.insert(lv);
    }
    assert_eq!(distinct.len(), 3);
    s.active_sequence().unwrap().check().unwrap();
    let saved = reopen(&mut s);
    for (v, a) in pairs {
        assert_eq!(link_of(&saved, v), link_of(&saved, a));
        assert!(link_of(&saved, v).is_some());
    }
    s.execute("edit.undo", json!({})).unwrap();
    for (v, a) in pairs {
        assert_eq!((link_of(&s, v), link_of(&s, a)), (None, None));
    }
    s.execute("edit.redo", json!({})).unwrap();
    for (v, a) in pairs {
        assert_eq!(link_of(&s, v), link_of(&s, a));
        assert!(link_of(&s, v).is_some());
    }
}

fn place_one(s: &mut Session, item: u64, track: &str, frame: i64, frames: i64, video: bool) -> ClipId {
    let duration = s.sequence_rate().tick_of(frames).0;
    let id = s.execute("timeline.place", json!({"item": item, "track": track, "frame": frame, "duration": duration, "video": video, "audio": !video})).unwrap()
        ["clips"][0]
        .as_u64()
        .unwrap();
    ClipId(id)
}

#[test]
fn sound_prefers_the_picture_from_the_same_media_over_a_longer_overlap() {
    let mut s = Session::default();
    s.execute("file.newProject", json!({"name": "Pairs"})).unwrap();
    s.execute("file.newSequence", json!({"name": "Sequence", "fps": 25, "width": 16, "height": 16, "video": 1, "audio": 1})).unwrap();
    s.execute("sequence.addTracks", json!({"video": 1})).unwrap();
    let item = |s: &mut Session, name: &str| {
        s.execute("file.newOfflineFile", json!({"name": name, "seconds": 4, "fps": 25, "width": 16, "height": 16})).unwrap()["item"].as_u64().unwrap()
    };
    let a = item(&mut s, "A");
    let b = item(&mut s, "B");
    // Picture A covers 0..20. Picture B covers 10..50. Sound A covers 10..40, so B overlaps it
    // for longer, and the shared media still belongs with A.
    let picture_a = place_one(&mut s, a, "V1", 0, 20, true);
    let picture_b = place_one(&mut s, b, "V2", 10, 40, true);
    let sound_a = place_one(&mut s, a, "A1", 10, 30, false);
    let ids = vec![picture_a.0, picture_b.0, sound_a.0];
    assert_eq!(s.execute("clip.link", json!({"clips": ids})).unwrap()["linked"], true);
    assert_eq!(link_of(&s, picture_a), link_of(&s, sound_a));
    assert!(link_of(&s, picture_a).is_some());
    assert_eq!(link_of(&s, picture_b), None);
    s.active_sequence().unwrap().check().unwrap();
}

#[test]
fn one_picture_still_links_every_selected_clip() {
    let mut s = Session::default();
    s.execute("file.newProject", json!({"name": "One"})).unwrap();
    s.execute("file.newSequence", json!({"name": "Sequence", "fps": 25, "width": 16, "height": 16, "video": 1, "audio": 2})).unwrap();
    let item =
        s.execute("file.newOfflineFile", json!({"name": "Media", "seconds": 4, "fps": 25, "width": 16, "height": 16})).unwrap()["item"].as_u64().unwrap();
    let picture = place_one(&mut s, item, "V1", 0, 20, true);
    let near = place_one(&mut s, item, "A1", 0, 20, false);
    let far = place_one(&mut s, item, "A2", 80, 10, false);
    assert_eq!(s.execute("clip.link", json!({"clips": [picture.0, near.0, far.0]})).unwrap()["linked"], true);
    let link = link_of(&s, picture).unwrap();
    assert_eq!((link_of(&s, near), link_of(&s, far)), (Some(link), Some(link)));
}

#[test]
fn two_pictures_and_no_sound_still_share_one_link() {
    let mut s = Session::default();
    s.execute("file.newProject", json!({"name": "Pictures"})).unwrap();
    s.execute("file.newSequence", json!({"name": "Sequence", "fps": 25, "width": 16, "height": 16, "video": 1, "audio": 0})).unwrap();
    let item = s.execute("file.newOfflineFile", json!({"name": "Media", "seconds": 4, "fps": 25, "width": 16, "height": 16, "audio": false})).unwrap()["item"]
        .as_u64()
        .unwrap();
    let a = place_one(&mut s, item, "V1", 0, 20, true);
    let b = place_one(&mut s, item, "V1", 20, 20, true);
    assert_eq!(s.execute("clip.link", json!({"clips": [a.0, b.0]})).unwrap()["linked"], true);
    assert_eq!(link_of(&s, a), link_of(&s, b));
    assert!(link_of(&s, a).is_some());
}
