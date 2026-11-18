//! `edit.rippleDelete` on a clip whose sound edge was split (a J or an L cut): the clip and its
//! sound go, and the later clips move up by one amount on every track.

use filmcraft_project::ClipId;
use filmcraft_time::Tick;
use serde_json::json;

use crate::Session;

fn range(s: &Session, c: ClipId) -> (Tick, Tick) {
    s.active_sequence().unwrap().find_item(c).map(|(_, i)| (i.start, i.end())).unwrap()
}

/// Four 48-frame clips in a row, pictures on V1 and their linked sounds on A1: [(picture, sound); 4].
fn four_clips(s: &mut Session) -> [(ClipId, ClipId); 4] {
    s.execute("file.openDemoProject", json!({})).unwrap();
    s.execute("file.newSequence", json!({"name": "Split edits", "video": 1, "audio": 1})).unwrap();
    let rate = s.sequence_rate();
    let item = s.project.items.values().find(|i| i.has_video() && i.has_audio() && i.as_media().is_some()).map(|i| i.id).unwrap();
    for n in 0..4 {
        s.execute("timeline.place", json!({"item": item.0, "frame": 48 * n, "sourceIn": rate.tick_of(48).0, "duration": rate.tick_of(48).0})).unwrap();
    }
    let q = s.active_sequence().unwrap();
    [0, 1, 2, 3].map(|n| (q.video_tracks[0].items[n].id, q.audio_tracks[0].items[n].id))
}

/// Move one sound edge alone, as a split edit does (Linked Selection off for the trim).
fn trim_sound(s: &mut Session, sound: ClipId, edge: &str, frames: i64) {
    s.execute("sequence.linkedSelection", json!({"on": false})).unwrap();
    s.execute("timeline.trim", json!({"clip": sound.0, "edge": edge, "mode": "regular", "deltaFrames": frames})).unwrap();
    s.execute("sequence.linkedSelection", json!({"on": true})).unwrap();
}

/// The second clip's sound starts 12 frames early (a J cut). Ripple delete was refused: the picture
/// and the sound have different ranges, each range was closed on its own, and the clips after the
/// next one were moved up twice, onto it. With a single clip after it the delete went through and
/// a later marker moved twice.
#[test]
fn ripple_delete_of_a_j_cut_clip_closes_one_gap() {
    let mut s = Session::default();
    let [(_, sound1), (picture2, sound2), (picture3, sound3), (picture4, sound4)] = four_clips(&mut s);
    let f = |n: i64| s.sequence_rate().tick_of(n);
    let (f36, f48, f72, f96, f120, f144) = (f(36), f(48), f(72), f(96), f(120), f(144));
    trim_sound(&mut s, sound1, "out", -12);
    trim_sound(&mut s, sound2, "in", -12);
    assert_eq!((range(&s, picture2).0, range(&s, sound2).0), (f48, f36), "the sound leads by 12 frames");
    s.state.ripple_sequence_markers = true;
    let marker = s.execute("markers.add", json!({"time": f120.0, "name": "Later"})).unwrap()["marker"].clone();
    let before = s.project.clone();

    s.execute("edit.rippleDelete", json!({"clips": [picture2.0]})).unwrap();
    let q = s.active_sequence().unwrap();
    q.check().unwrap();
    assert!(q.find_item(picture2).is_none() && q.find_item(sound2).is_none(), "the clip and its sound are gone");
    assert_eq!(range(&s, picture3), (f48, f96), "the next picture closes the gap");
    assert_eq!(range(&s, sound3), (f48, f96), "and its sound moves up by the same amount");
    assert_eq!((range(&s, picture4), range(&s, sound4)), ((f96, f144), (f96, f144)), "the clip after it moves up once");
    assert_eq!(range(&s, sound1).1, f36, "the sound before is untouched");
    assert_eq!(q.duration(), f144);
    let at = q.markers.iter().find(|m| json!(m.id.0) == marker).map(|m| m.start);
    assert_eq!(at, Some(f72), "a later marker moves up once, by the same amount");
    let after = s.project.clone();
    s.execute("edit.undo", json!({})).unwrap();
    assert_eq!(*s.project, *before, "one undo step restores it");
    s.execute("edit.redo", json!({})).unwrap();
    assert_eq!(*s.project, *after);
}

/// The second clip's sound runs 12 frames under the next picture (an L cut).
#[test]
fn ripple_delete_of_an_l_cut_clip_keeps_the_next_clip_in_sync() {
    let mut s = Session::default();
    let [_, (picture2, sound2), (picture3, sound3), (picture4, sound4)] = four_clips(&mut s);
    let f = |n: i64| s.sequence_rate().tick_of(n);
    let (f48, f60, f96, f108, f144) = (f(48), f(60), f(96), f(108), f(144));
    trim_sound(&mut s, sound3, "in", 12);
    trim_sound(&mut s, sound2, "out", 12);
    assert_eq!((range(&s, picture2).1, range(&s, sound2).1), (f96, f108), "the sound runs on by 12 frames");

    s.execute("edit.rippleDelete", json!({"clips": [picture2.0]})).unwrap();
    let q = s.active_sequence().unwrap();
    q.check().unwrap();
    assert!(q.find_item(sound2).is_none());
    assert_eq!(range(&s, picture3), (f48, f96));
    assert_eq!(range(&s, sound3), (f60, f96), "the next sound still starts 12 frames after its picture");
    assert_eq!((range(&s, picture4), range(&s, sound4)), ((f96, f144), (f96, f144)));
}

/// A straight cut is unchanged: the picture and its sound share one range and close one gap.
#[test]
fn ripple_delete_of_a_straight_cut_clip_is_unchanged() {
    let mut s = Session::default();
    let [_, (picture2, _), (picture3, sound3), _] = four_clips(&mut s);
    let f = |n: i64| s.sequence_rate().tick_of(n);
    let (f48, f96) = (f(48), f(96));
    s.execute("edit.rippleDelete", json!({"clips": [picture2.0]})).unwrap();
    assert_eq!((range(&s, picture3), range(&s, sound3)), ((f48, f96), (f48, f96)));
}

/// Two neighbours deleted together, with a split edit between them (the first one's sound runs 12
/// frames under the second picture): one gap closes, the size of both clips.
#[test]
fn ripple_delete_of_two_neighbours_with_a_split_edit_between_them() {
    let mut s = Session::default();
    let [_, (picture2, sound2), (picture3, sound3), (picture4, sound4)] = four_clips(&mut s);
    let f = |n: i64| s.sequence_rate().tick_of(n);
    let (f48, f96) = (f(48), f(96));
    trim_sound(&mut s, sound3, "in", 12);
    trim_sound(&mut s, sound2, "out", 12);
    s.execute("edit.rippleDelete", json!({"clips": [picture2.0, picture3.0]})).unwrap();
    let q = s.active_sequence().unwrap();
    q.check().unwrap();
    assert!([picture2, sound2, picture3, sound3].iter().all(|c| q.find_item(*c).is_none()));
    assert_eq!((range(&s, picture4), range(&s, sound4)), ((f48, f96), (f48, f96)), "the clip after them closes the whole gap");
}

/// Straight cuts with the sounds on alternating audio tracks: deleting two neighbours closes the
/// whole gap, as it did before.
#[test]
fn ripple_delete_of_two_neighbours_with_sounds_on_alternating_tracks() {
    let mut s = Session::default();
    s.execute("file.openDemoProject", json!({})).unwrap();
    s.execute("file.newSequence", json!({"name": "Alternating", "video": 1, "audio": 2})).unwrap();
    let rate = s.sequence_rate();
    let item = s.project.items.values().find(|i| i.has_video() && i.has_audio() && i.as_media().is_some()).map(|i| i.id).unwrap();
    let mut pictures = Vec::new();
    for n in 0..4 {
        let track = if n % 2 == 0 { "A1" } else { "A2" };
        let r = s.execute("timeline.place", json!({"item": item.0, "audioTrack": track, "frame": 48 * n, "duration": rate.tick_of(48).0})).unwrap();
        pictures.push(ClipId(r["clips"][0].as_u64().unwrap()));
    }
    s.execute("edit.rippleDelete", json!({"clips": [pictures[1].0, pictures[2].0]})).unwrap();
    let q = s.active_sequence().unwrap();
    q.check().unwrap();
    assert_eq!(range(&s, pictures[3]), (rate.tick_of(48), rate.tick_of(96)));
    assert_eq!(q.audio_tracks[1].items.iter().map(|i| i.range()).collect::<Vec<_>>(), vec![q.video_tracks[0].items[1].range()], "its sound moved with it");
    assert_eq!(q.duration(), rate.tick_of(96));
}

/// Sync-locked caption tracks follow `edit.rippleDelete` and restore cleanly on undo.
#[test]
fn ripple_delete_shifts_sync_locked_caption_tracks_and_restores_on_undo() {
    let mut s = Session::default();
    let [(_, _), (picture2, _), (_, _), (_, _)] = four_clips(&mut s);
    let rate = s.sequence_rate();
    let f = move |n: i64| rate.tick_of(n);

    let dir = std::env::temp_dir().join(format!("fc-ripple-cap-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let srt = dir.join("dialogue.srt");
    // Frame times at 24fps:
    // Cue 1: 10..30 (0.417s .. 1.250s)
    // Cue 2: 55..75 (2.292s .. 3.125s)
    // Cue 3: 90..110 (3.750s .. 4.583s)
    // Cue 4: 150..170 (6.250s .. 7.083s)
    std::fs::write(
        &srt,
        "1\r\n00:00:00,417 --> 00:00:01,250\r\nBefore\r\n\r\n2\r\n00:00:02,292 --> 00:00:03,125\r\nInside clip 2\r\n\r\n3\r\n00:00:03,750 --> 00:00:04,583\r\nOverlapping boundary\r\n\r\n4\r\n00:00:06,250 --> 00:00:07,083\r\nAfter\r\n",
    )
    .unwrap();
    s.execute("captions.import", json!({"path": srt.to_string_lossy()})).unwrap();

    let q = s.active_sequence().unwrap();
    let tr = &q.caption_tracks[0];
    assert!(tr.sync_lock, "imported caption track has sync lock on");
    assert_eq!(tr.captions.len(), 4);
    assert_eq!((tr.captions[0].start, tr.captions[0].end()), (f(10), f(30)));
    assert_eq!((tr.captions[1].start, tr.captions[1].end()), (f(55), f(75)));
    assert_eq!((tr.captions[2].start, tr.captions[2].end()), (f(90), f(110)));
    assert_eq!((tr.captions[3].start, tr.captions[3].end()), (f(150), f(170)));

    let before = s.project.clone();

    // Ripple delete picture2 (clip 2 occupies frames 48..96, duration 48)
    s.execute("edit.rippleDelete", json!({"clips": [picture2.0]})).unwrap();

    let q = s.active_sequence().unwrap();
    q.check().unwrap();
    let tr = &q.caption_tracks[0];

    assert_eq!(tr.captions.len(), 3, "inside caption is removed, leaving 3 captions");
    assert_eq!(tr.captions[0].text, "Before");
    assert_eq!((tr.captions[0].start, tr.captions[0].end()), (f(10), f(30)));

    assert_eq!(tr.captions[1].text, "Overlapping boundary");
    assert_eq!((tr.captions[1].start, tr.captions[1].end()), (f(48), f(62)));

    assert_eq!(tr.captions[2].text, "After");
    assert_eq!((tr.captions[2].start, tr.captions[2].end()), (f(102), f(122)));

    let after = s.project.clone();
    s.execute("edit.undo", json!({})).unwrap();
    assert_eq!(*s.project, *before, "one undo step restores caption track completely");

    s.execute("edit.redo", json!({})).unwrap();
    assert_eq!(*s.project, *after, "redo restores rippled captions");
    std::fs::remove_dir_all(dir).unwrap();
}
