//! Razor and trim on one half of a linked clip (#1945): with `linked: false` (Option-click with the
//! Razor, Option-drag on an edge) only the clicked clip is cut or trimmed, as in Premiere; by default
//! its linked partners follow.

use filmcraft_project::ClipId;
use filmcraft_time::Tick;
use serde_json::json;

use crate::Session;

/// One 96-frame clip at the start of a new sequence: (picture on V1, linked sound on A1).
fn linked_clip(s: &mut Session) -> (ClipId, ClipId) {
    s.execute("file.openDemoProject", json!({})).unwrap();
    s.execute("file.newSequence", json!({"name": "Split", "video": 1, "audio": 1})).unwrap();
    let rate = s.sequence_rate();
    let item = s.project.items.values().find(|i| i.has_video() && i.has_audio() && i.as_media().is_some()).map(|i| i.id).unwrap();
    s.execute("timeline.place", json!({"item": item.0, "frame": 0, "sourceIn": 0, "duration": rate.tick_of(96).0})).unwrap();
    s.execute("edit.deselectAll", json!({})).unwrap();
    let q = s.active_sequence().unwrap();
    let (v, a) = (&q.video_tracks[0].items[0], &q.audio_tracks[0].items[0]);
    assert!(v.link.is_some() && v.link == a.link, "placed as a linked pair");
    (v.id, a.id)
}

fn counts(s: &Session) -> (usize, usize) {
    let q = s.active_sequence().unwrap();
    (q.video_tracks[0].items.len(), q.audio_tracks[0].items.len())
}

fn end(s: &Session, c: ClipId) -> Tick {
    s.active_sequence().unwrap().find_item(c).map(|(_, i)| i.end()).unwrap()
}

#[test]
fn razor_cuts_the_linked_partner_unless_linked_is_false() {
    let mut s = Session::default();
    let (picture, _) = linked_clip(&mut s);
    let at = s.sequence_rate().tick_of(48);
    s.execute("timeline.razor", json!({"time": at.0, "clip": picture.0})).unwrap();
    assert_eq!(counts(&s), (2, 2), "a plain click cuts picture and sound");

    let mut s = Session::default();
    let (picture, _) = linked_clip(&mut s);
    let r = s.execute("timeline.razor", json!({"time": at.0, "clip": picture.0, "linked": false})).unwrap();
    assert_eq!(r["cuts"], json!(1));
    assert_eq!(counts(&s), (2, 1), "Option-click cuts the picture alone");
}

#[test]
fn trim_moves_the_linked_partner_unless_linked_is_false() {
    let mut s = Session::default();
    let (picture, sound) = linked_clip(&mut s);
    let rate = s.sequence_rate();
    s.execute("timeline.trim", json!({"clip": picture.0, "edge": "out", "delta": -rate.tick_of(24).0})).unwrap();
    assert_eq!(end(&s, picture), rate.tick_of(72));
    assert_eq!(end(&s, sound), rate.tick_of(72), "the sound follows by default");

    let mut s = Session::default();
    let (picture, sound) = linked_clip(&mut s);
    s.execute("timeline.trim", json!({"clip": picture.0, "edge": "out", "delta": -rate.tick_of(24).0, "linked": false})).unwrap();
    assert_eq!(end(&s, picture), rate.tick_of(72));
    assert_eq!(end(&s, sound), rate.tick_of(96), "an Option-drag leaves the sound: a split edit");
    let q = s.active_sequence().unwrap();
    assert!(q.find_item(picture).unwrap().1.link.is_some(), "the clips stay linked");
}
