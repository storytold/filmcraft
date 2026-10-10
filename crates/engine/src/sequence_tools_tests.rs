//! Sequence / Markers menu commands (`sequence_tools`).

use super::*;
use filmcraft_project::{AudioChannels, Label, MarkerKind, TrackKind};
use serde_json::json;

fn demo() -> Session {
    let mut s = Session::default();
    s.execute("file.openDemoProject", json!({})).unwrap();
    s
}

/// An empty 23.976 sequence and a 20 s Bars and Tone item (video + audio).
fn fresh() -> (Session, u64) {
    let mut s = Session::default();
    s.execute("file.newSequence", json!({"name": "Test"})).unwrap();
    let item = s.execute("file.newBarsAndTone", json!({"seconds": 20.0})).unwrap()["item"].as_u64().unwrap();
    (s, item)
}

fn f(s: &Session, frames: i64) -> Tick {
    s.sequence_rate().tick_of(frames)
}

fn place(s: &mut Session, item: u64, track: &str, at: i64, source_in: i64, frames: i64) -> Vec<u64> {
    let (sin, dur) = (f(s, source_in), f(s, frames));
    let at = f(s, at);
    let audio = track.replace('V', "A");
    let r =
        s.execute("timeline.place", json!({"item": item, "track": track, "audioTrack": audio, "time": at.0, "sourceIn": sin.0, "duration": dur.0})).unwrap();
    r["clips"].as_array().unwrap().iter().map(|c| c.as_u64().unwrap()).collect()
}

fn ids() -> Vec<&'static str> {
    command_specs().iter().map(|c| c.id).collect()
}

#[test]
fn commands_are_registered_in_premiere_menu_order() {
    let s = Session::default();
    let all = ids();
    let after = |a: &str, b: &str| all.iter().position(|x| *x == a).unwrap() + 1 == all.iter().position(|x| *x == b).unwrap();
    assert!(after("sequence.matchFrame", "sequence.reverseMatchFrame"));
    assert!(after("sequence.closeGap", "sequence.goToNextGap"));
    assert!(after("sequence.addTracks", "sequence.deleteTracks"));
    assert!(after("markers.markSelection", "markers.markSplitVideoIn"));
    assert!(after("markers.goToOut", "markers.goToSplitVideoIn"));
    assert!(after("markers.add", "markers.addRange"));
    let gap = find_command("sequence.goToPrevGap").unwrap();
    assert_eq!(gap.menu, &["Sequence", "Go to Gap"]);
    assert_eq!(gap.label, "Previous in Sequence");
    assert_eq!(find_command("markers.clearAll").unwrap().label, "Clear Markers");
    assert_eq!(s.shortcuts.primary("sequence.reverseMatchFrame").as_deref(), Some("Shift+R"));
    assert_eq!(s.shortcuts.primary("sequence.goToNextGap").as_deref(), Some("Shift+;"));
    assert_eq!(s.shortcuts.primary("sequence.makeSubsequence").as_deref(), Some("Shift+U"));
    assert_eq!(s.shortcuts.primary("markers.addRangeInOut").as_deref(), Some("Ctrl+M"));
    // every id is unique
    let mut sorted = all.clone();
    sorted.sort_unstable();
    sorted.dedup();
    assert_eq!(sorted.len(), all.len());
}

#[test]
fn go_to_gap_in_sequence_and_in_track() {
    let (mut s, bars) = fresh();
    let matte = s.execute("file.newColorMatte", json!({"seconds": 20.0})).unwrap()["item"].as_u64().unwrap();
    place(&mut s, bars, "V1", 0, 0, 48); // V1/A1 0–48
    place(&mut s, bars, "V1", 96, 0, 48); // V1/A1 96–144
    s.execute("timeline.place", json!({"item": matte, "track": "V2", "frame": 48, "duration": f(&s, 24).0})).unwrap(); // V2 48–72
    // sequence: empty everywhere only in 72–96; V1: 48–96
    s.execute("playhead.set", json!({"frame": 0})).unwrap();
    let r = s.execute("sequence.goToNextGap", json!({})).unwrap();
    assert_eq!(r["time"], json!(f(&s, 72).0));
    s.execute("playhead.set", json!({"frame": 0})).unwrap();
    s.execute("sequence.goToNextGapInTrack", json!({"track": "V1"})).unwrap();
    assert_eq!(s.playhead(), f(&s, 48));
    // V2 has a leading gap from 0 and nothing after its clip
    s.execute("playhead.set", json!({"frame": 100})).unwrap();
    s.execute("sequence.goToPrevGapInTrack", json!({"track": "V2"})).unwrap();
    assert_eq!(s.playhead(), Tick::ZERO);
    s.execute("playhead.set", json!({"frame": 120})).unwrap();
    s.execute("sequence.goToPrevGap", json!({})).unwrap();
    assert_eq!(s.playhead(), f(&s, 72));
    // no further gap: the playhead stays
    let r = s.execute("sequence.goToNextGap", json!({})).unwrap();
    assert!(r["time"].is_null());
    assert_eq!(s.playhead(), f(&s, 72));
    // in track defaults to the targeted tracks
    s.execute("playhead.set", json!({"frame": 0})).unwrap();
    s.execute("sequence.goToNextGapInTrack", json!({})).unwrap();
    assert_eq!(s.playhead(), f(&s, 48));
}

#[test]
fn reverse_match_frame_finds_the_source_frame() {
    let (mut s, bars) = fresh();
    place(&mut s, bars, "V1", 0, 0, 48); // media 0–48
    place(&mut s, bars, "V1", 96, 72, 48); // media 72–120 at 96
    assert!(s.execute("sequence.reverseMatchFrame", json!({})).is_err(), "needs a source clip");
    s.execute("source.open", json!({"item": bars})).unwrap();
    s.execute("source.setPlayhead", json!({"frame": 84})).unwrap();
    let r = s.execute("sequence.reverseMatchFrame", json!({})).unwrap();
    assert_eq!(s.playhead(), f(&s, 108), "{r}");
    s.execute("source.setPlayhead", json!({"frame": 10})).unwrap();
    s.execute("sequence.reverseMatchFrame", json!({})).unwrap();
    assert_eq!(s.playhead(), f(&s, 10));
    s.execute("source.setPlayhead", json!({"frame": 60})).unwrap();
    assert!(s.execute("sequence.reverseMatchFrame", json!({})).is_err(), "frame 60 is not in the sequence");
    // match frame (F) and reverse match frame (Shift+R) are inverses
    s.execute("playhead.set", json!({"frame": 130})).unwrap();
    s.execute("sequence.matchFrame", json!({})).unwrap();
    s.execute("playhead.set", json!({"frame": 0})).unwrap();
    s.execute("sequence.reverseMatchFrame", json!({})).unwrap();
    assert_eq!(s.playhead(), f(&s, 130));
}

#[test]
fn through_edits_show_and_join() {
    let mut s = demo();
    let before = s.active_sequence().unwrap().clone();
    assert!(s.execute("sequence.throughEdits", json!({})).unwrap().as_array().unwrap().is_empty());
    s.execute("playhead.set", json!({"frame": 30})).unwrap();
    s.execute("sequence.addEditAllTracks", json!({})).unwrap();
    let te = s.execute("sequence.throughEdits", json!({})).unwrap();
    let n = te.as_array().unwrap().len();
    assert!(n >= 2, "V1 and A1 (and the music) get through edits: {te}");
    assert!(te.as_array().unwrap().iter().all(|e| e["time"] == json!(f(&s, 30).0)));
    assert_eq!(s.execute("sequence.showThroughEdits", json!({})).unwrap()["showThroughEdits"], json!(true));
    // join only the V1 one (and its linked audio)
    let v1 = te.as_array().unwrap().iter().find(|e| e["track"] == json!(before.video_tracks[0].id.0)).unwrap()["right"].as_u64().unwrap();
    let r = s.execute("sequence.joinThroughEdits", json!({"clips": [v1]})).unwrap();
    assert_eq!(r["joined"], json!(2), "V1 + linked A1");
    assert_eq!(s.execute("sequence.throughEdits", json!({})).unwrap().as_array().unwrap().len(), n - 2);
    s.execute("sequence.joinThroughEdits", json!({"all": true})).unwrap();
    let q = s.active_sequence().unwrap();
    for (a, b) in q.all_tracks().zip(before.all_tracks()) {
        assert_eq!(a.items, b.items, "{}", a.name);
    }
    assert!(s.execute("sequence.joinThroughEdits", json!({"all": true})).is_err(), "nothing left to join");
    s.execute("edit.undo", json!({})).unwrap();
    assert_eq!(s.execute("sequence.throughEdits", json!({})).unwrap().as_array().unwrap().len(), n - 2);
}

/// `cut: [left, right]` (the timeline's edit point menu, #219) joins exactly that cut and its
/// linked partner, even when one of its pieces is also part of another through edit.
#[test]
fn join_one_through_edit_by_its_cut() {
    let mut s = demo();
    let v1 = s.active_sequence().unwrap().video_tracks[0].id.0;
    for frame in [30, 40] {
        s.execute("playhead.set", json!({"frame": frame})).unwrap();
        s.execute("sequence.addEditAllTracks", json!({})).unwrap();
    }
    let te = s.execute("sequence.throughEdits", json!({})).unwrap();
    let on_v1: Vec<&Value> = te.as_array().unwrap().iter().filter(|e| e["track"] == json!(v1)).collect();
    assert_eq!(on_v1.len(), 2);
    // the second cut: its left piece is also the right piece of the first one
    let (second, first) = (on_v1[1], on_v1[0]);
    assert_eq!(second["left"], first["right"]);
    let n = te.as_array().unwrap().len();
    let r = s.execute("sequence.joinThroughEdits", json!({"cut": [second["left"], second["right"]]})).unwrap();
    assert_eq!(r["joined"], json!(2), "V1 + linked A1");
    let te = s.execute("sequence.throughEdits", json!({})).unwrap();
    assert_eq!(te.as_array().unwrap().len(), n - 2);
    assert!(te.as_array().unwrap().iter().any(|e| e["left"] == first["left"] && e["right"] == first["right"]), "the first cut stays");
}

#[test]
fn join_by_cut_rejects_hostile_parameters() {
    let mut s = demo();
    let items = s.active_sequence().unwrap().video_tracks[0].items.clone();
    let before = s.active_sequence().unwrap().clone();
    for cut in [json!([]), json!([1]), json!([1, 2, 3]), json!("x"), json!([-1, 2]), json!([u64::MAX, 0]), json!([items[0].id.0, items[1].id.0])] {
        assert!(s.execute("sequence.joinThroughEdits", json!({"cut": cut})).is_err(), "{cut}");
    }
    assert_eq!(s.active_sequence().unwrap(), &before, "nothing changed");
}

#[test]
fn make_subsequence_from_in_out_and_from_selection() {
    let mut s = demo();
    let seq = s.state.active_sequence.unwrap();
    let orig = s.active_sequence().unwrap().clone();
    assert!(s.execute("sequence.makeSubsequence", json!({})).is_err(), "needs In/Out or a selection");
    s.execute("markers.markIn", json!({"frame": 30})).unwrap();
    s.execute("markers.markOut", json!({"frame": 77})).unwrap();
    let r = s.execute("sequence.makeSubsequence", json!({})).unwrap();
    let sub = ItemId(r["sequence"].as_u64().unwrap());
    assert_eq!(r["name"], json!("Main Edit_Sub_01"));
    assert_eq!(s.state.active_sequence, Some(seq), "the original stays open");
    assert_eq!(s.state.project_selection, vec![sub]);
    let q = s.project.sequence(sub).unwrap();
    q.check().unwrap();
    assert_eq!(q.duration(), f(&s, 48));
    assert_eq!(q.video_tracks.len(), orig.video_tracks.len());
    // same frames at the same relative times
    for (nt, ot) in q.all_tracks().zip(orig.all_tracks()) {
        for it in &nt.items {
            let t = it.start + Tick(1);
            let o = ot.item_at(t + f(&s, 30)).expect("source clip");
            assert_eq!(it.item, o.item);
            assert_eq!(it.source_time_at(t), o.source_time_at(t + f(&s, 30)));
        }
    }
    // ids are fresh, links stay pairwise
    let all: Vec<_> = q.all_tracks().flat_map(|t| t.items.iter().map(|i| i.id)).collect();
    assert!(all.iter().all(|c| orig.find_item(*c).is_none()));
    for (a, b) in s.active_sequence().unwrap().all_tracks().zip(orig.all_tracks()) {
        assert_eq!(a, b, "source sequence untouched");
    }
    // from a selection (no In/Out): the clips, starting at 0
    s.execute("markers.clearInOut", json!({})).unwrap();
    let c = orig.video_tracks[0].items[2].clone();
    s.execute("timeline.select", json!({"clips": [c.id.0]})).unwrap();
    let r = s.execute("sequence.makeSubsequence", json!({"name": "Shot 3"})).unwrap();
    let q = s.project.sequence(ItemId(r["sequence"].as_u64().unwrap())).unwrap();
    assert_eq!(q.video_tracks[0].items.len(), 1);
    assert_eq!(q.video_tracks[0].items[0].start, Tick::ZERO);
    assert_eq!(q.video_tracks[0].items[0].duration, c.duration);
    assert_eq!(q.audio_tracks[0].items.len(), 1, "linked audio comes along");
    assert_eq!(s.project.item(ItemId(r["sequence"].as_u64().unwrap())).unwrap().name, "Shot 3");
    s.execute("edit.undo", json!({})).unwrap();
    assert!(s.project.items.values().all(|i| i.name != "Shot 3"));
}

/// Sequence ▸ Add Tracks…: where the new tracks go, what they are, and what the amounts may be.
/// Premiere Pro 26.5.2: Placement is "Before First Track" or after one of the tracks (after the
/// last by default); a track added after Video 1 is the new Video 2, and what was on Video 2 is
/// then on Video 3; audio tracks are Standard, 5.1, Adaptive or Mono, submix tracks Stereo, 5.1,
/// Adaptive or Mono.
#[test]
fn add_tracks_places_and_types_them() {
    let mut s = demo();
    let names = |s: &Session, kind: TrackKind| s.active_sequence().unwrap().tracks(kind).iter().map(|t| t.name.clone()).collect::<Vec<_>>();
    let before = s.active_sequence().unwrap().clone();
    assert_eq!(names(&s, TrackKind::Video), ["Video 1", "Video 2", "Video 3"]);
    let on_v2 = before.video_tracks[1].items[0].id;

    // without a place: after the last track (and one video track when nothing is said, as before)
    let r = s.execute("sequence.addTracks", json!({})).unwrap();
    assert_eq!((r["video"].as_array().unwrap().len(), r["audio"].as_array().unwrap().len(), r["submix"].as_array().unwrap().len()), (1, 0, 0));
    assert_eq!(names(&s, TrackKind::Video), ["Video 1", "Video 2", "Video 3", "Video 4"]);
    assert_eq!(s.active_sequence().unwrap().video_tracks[3].id.0, r["video"][0].as_u64().unwrap());
    s.execute("edit.undo", json!({})).unwrap();

    // after Video 1: two new empty tracks there, the tracks above move up and are numbered again
    s.execute("sequence.addTracks", json!({"video": 2, "videoAfter": "V1"})).unwrap();
    let q = s.active_sequence().unwrap();
    assert_eq!(names(&s, TrackKind::Video), ["Video 1", "Video 2", "Video 3", "Video 4", "Video 5"]);
    assert!(q.video_tracks[1].items.is_empty() && q.video_tracks[2].items.is_empty());
    assert_eq!(q.video_tracks[3].id, before.video_tracks[1].id);
    assert_eq!(q.video_tracks[3].items[0].id, on_v2, "what was on Video 2 is on Video 4");
    assert_eq!(q.audio_tracks.len(), before.audio_tracks.len(), "no audio track was asked for");
    // one undo step
    s.execute("edit.undo", json!({})).unwrap();
    assert_eq!(*s.active_sequence().unwrap(), before);

    // before the first track; a name the user gave is kept, default names follow their place
    s.edit_sequence("Rename Track", |q, _, _| {
        q.audio_tracks[1].name = "Music".into();
        Ok(())
    })
    .unwrap();
    s.execute("sequence.addTracks", json!({"video": 0, "audio": 1, "audioAfter": "first", "audioType": "mono"})).unwrap();
    assert_eq!(names(&s, TrackKind::Audio), ["Audio 1", "Audio 2", "Music", "Audio 4"]);
    let q = s.active_sequence().unwrap();
    assert_eq!(q.audio_tracks[0].channels, AudioChannels::Mono);
    assert_eq!(q.audio_tracks[1].id, before.audio_tracks[0].id);
    // a number counts the tracks before the new ones; "Standard" is a stereo track
    for (ty, want) in [("standard", AudioChannels::Stereo), ("5.1", AudioChannels::Surround51), ("adaptive", AudioChannels::Adaptive)] {
        let r = s.execute("sequence.addTracks", json!({"video": 0, "audio": 1, "audioAfter": 2, "audioType": ty})).unwrap();
        let q = s.active_sequence().unwrap();
        assert_eq!((q.audio_tracks[2].id.0, q.audio_tracks[2].channels), (r["audio"][0].as_u64().unwrap(), want), "{ty}");
    }

    // submix tracks: stereo unless said, placed among the submix tracks
    assert!(before.submix_tracks.is_empty());
    s.execute("sequence.addTracks", json!({"video": 0, "submix": 2})).unwrap();
    s.execute("sequence.addTracks", json!({"video": 0, "submix": 1, "submixAfter": "S1", "submixType": "5.1"})).unwrap();
    let q = s.active_sequence().unwrap();
    assert_eq!(
        q.submix_tracks.iter().map(|t| (t.name.as_str(), t.channels)).collect::<Vec<_>>(),
        [("Submix 1", AudioChannels::Stereo), ("Submix 2", AudioChannels::Surround51), ("Submix 3", AudioChannels::Stereo)]
    );
    // all three kinds at once are one undo step
    let before = s.active_sequence().unwrap().clone();
    let r = s.execute("sequence.addTracks", json!({"video": 1, "audio": 1, "submix": 1})).unwrap();
    assert_eq!((r["video"].as_array().unwrap().len(), r["audio"].as_array().unwrap().len(), r["submix"].as_array().unwrap().len()), (1, 1, 1));
    s.execute("edit.undo", json!({})).unwrap();
    assert_eq!(*s.active_sequence().unwrap(), before);
}

/// Amounts and places come from a dialog, a script or the control channel: a wrong one is an
/// error that changes nothing, and no amount can make the sequence grow without bound.
#[test]
fn add_tracks_refuses_what_it_cannot_do() {
    let mut s = demo();
    let before = s.active_sequence().unwrap().clone();
    for bad in [
        json!({"video": 0}),
        json!({"video": 100}),
        json!({"video": u64::MAX}),
        json!({"video": 0, "audio": 1_000_000}),
        json!({"video": "many"}),
        json!({"video": 1, "videoAfter": "V9"}),
        json!({"video": 1, "videoAfter": "A1"}),
        json!({"video": 1, "videoAfter": 4}),
        json!({"video": 1, "videoAfter": "top"}),
        json!({"video": 0, "audio": 1, "audioAfter": "V1"}),
        json!({"video": 0, "audio": 1, "audioType": "quad"}),
        json!({"video": 0, "submix": 1, "submixAfter": "S1"}),
        json!({"video": 0, "submix": 1, "submixType": "standard-ish"}),
    ] {
        let r = s.execute("sequence.addTracks", bad.clone());
        assert!(r.is_err(), "{bad} was accepted: {r:?}");
        assert_eq!(*s.active_sequence().unwrap(), before, "{bad} changed the sequence");
    }
    // 99 at a time, and a sequence stops at 999 tracks of a kind
    for _ in 0..10 {
        s.execute("sequence.addTracks", json!({"video": 99})).unwrap();
    }
    assert_eq!(s.active_sequence().unwrap().video_tracks.len(), 3 + 990);
    assert!(s.execute("sequence.addTracks", json!({"video": 7})).is_err());
    s.execute("sequence.addTracks", json!({"video": 6})).unwrap();
    assert_eq!(s.active_sequence().unwrap().video_tracks.len(), 999);
}

#[test]
fn delete_tracks_empty_and_specific() {
    let mut s = demo();
    let q = s.active_sequence().unwrap().clone();
    let empty_v = q.video_tracks.iter().filter(|t| t.items.is_empty()).count();
    let empty_a = q.audio_tracks.iter().filter(|t| t.items.is_empty()).count();
    assert!(empty_v > 0 && empty_a > 0, "demo has empty tracks");
    let r = s.execute("sequence.deleteTracks", json!({"video": "empty", "audio": "empty"})).unwrap();
    assert_eq!(r["deleted"], json!(empty_v + empty_a));
    let q2 = s.active_sequence().unwrap();
    assert!(q2.all_tracks().all(|t| !t.items.is_empty()));
    assert!(s.execute("sequence.deleteTracks", json!({"video": "empty"})).is_err(), "nothing left to delete");
    // a specific track; its clips go with it
    s.execute("sequence.deleteTracks", json!({"video": "V2"})).unwrap();
    assert_eq!(s.active_sequence().unwrap().video_tracks.len(), q.video_tracks.len() - empty_v - 1);
    assert!(s.execute("sequence.deleteTracks", json!({"video": "A1"})).is_err(), "kind must match");
    // the last track of a kind stays
    let r = s.execute("sequence.deleteTracks", json!({"video": "V1"}));
    assert!(r.is_err(), "{r:?}");
    s.execute("edit.undo", json!({})).unwrap();
    s.execute("edit.undo", json!({})).unwrap();
    assert_eq!(s.active_sequence().unwrap(), &q);
}

#[test]
fn delete_tracks_removes_caption_tracks() {
    let mut s = demo();
    let before = s.active_sequence().unwrap().clone();
    let n0 = before.caption_tracks.len();
    for _ in 0..3 {
        s.execute("captions.newTrack", json!({"format": "Subtitle"})).unwrap();
    }
    // new tracks are inserted on top: C1 gets a caption, C2 and C3 stay empty
    let cap = s.execute("captions.add", json!({"track": "C1", "text": "Hi", "seconds": 1.0})).unwrap()["caption"].as_u64().unwrap();
    s.execute("captions.select", json!({"captions": [cap]})).unwrap();
    let empty = s.active_sequence().unwrap().caption_tracks.iter().filter(|t| t.captions.is_empty()).count();
    assert!(empty >= 2);
    let r = s.execute("sequence.deleteTracks", json!({"captions": "empty"})).unwrap();
    assert_eq!(r["deleted"], json!(empty));
    let q = s.active_sequence().unwrap();
    assert!(q.caption_tracks.iter().all(|t| !t.captions.is_empty()));
    assert_eq!(q.video_tracks.len(), before.video_tracks.len(), "video tracks untouched");
    assert!(s.execute("sequence.deleteTracks", json!({"captions": "empty"})).is_err(), "nothing left to delete");
    // unknown names, ids and value types are errors, not panics
    for bad in [json!("C99"), json!("C0"), json!("Cx"), json!(999_999), json!(true), json!(["C1"])] {
        assert!(s.execute("sequence.deleteTracks", json!({"captions": bad})).is_err(), "{bad}");
    }
    // a specific track (with its captions); the last caption track may go too
    let left = s.active_sequence().unwrap().caption_tracks.len();
    for _ in 0..left {
        s.execute("sequence.deleteTracks", json!({"captions": "C1"})).unwrap();
    }
    assert!(s.active_sequence().unwrap().caption_tracks.is_empty());
    assert!(s.state.caption_selection.is_empty(), "selection of deleted captions cleared");
    for _ in 0..left + 1 {
        s.execute("edit.undo", json!({})).unwrap();
    }
    assert_eq!(s.active_sequence().unwrap().caption_tracks.len(), n0 + 3);
}

#[test]
fn split_points_on_the_sequence() {
    let (mut s, bars) = fresh();
    place(&mut s, bars, "V1", 0, 0, 240);
    s.execute("markers.markIn", json!({"frame": 10})).unwrap();
    s.execute("markers.markOut", json!({"frame": 100})).unwrap();
    assert!(!s.is_enabled("markers.goToSplitVideoIn"), "no split yet");
    s.execute("markers.markSplitAudioIn", json!({"frame": 20})).unwrap();
    s.execute("markers.markSplitVideoOut", json!({"frame": 90})).unwrap();
    let q = s.active_sequence().unwrap();
    assert_eq!(q.split.audio_in, Some(f(&s, 20)));
    assert_eq!(q.split.video_out, Some(f(&s, 90)));
    // go to: split points, or the ordinary In/Out for the other channel
    s.execute("markers.goToSplitAudioIn", json!({})).unwrap();
    assert_eq!(s.playhead(), f(&s, 20));
    s.execute("markers.goToSplitVideoIn", json!({})).unwrap();
    assert_eq!(s.playhead(), f(&s, 10));
    s.execute("markers.goToSplitVideoOut", json!({})).unwrap();
    assert_eq!(s.playhead(), f(&s, 90));
    s.execute("markers.goToSplitAudioOut", json!({})).unwrap();
    assert_eq!(s.playhead(), f(&s, 100));
    // a split Out before a split In clears the In of that channel
    s.execute("markers.markSplitAudioOut", json!({"frame": 15})).unwrap();
    assert_eq!(s.active_sequence().unwrap().split.audio_in, None);
    // Mark In replaces the In splits; Clear In and Out clears everything
    s.execute("markers.markSplitAudioIn", json!({"frame": 5})).unwrap();
    s.execute("markers.markIn", json!({"frame": 12})).unwrap();
    let sp = s.active_sequence().unwrap().split;
    assert_eq!((sp.video_in, sp.audio_in), (None, None));
    assert!(sp.video_out.is_some());
    s.execute("markers.clearInOut", json!({})).unwrap();
    assert!(s.active_sequence().unwrap().split.is_empty());
    s.execute("edit.undo", json!({})).unwrap();
    assert!(!s.active_sequence().unwrap().split.is_empty(), "split points are undoable");
}

#[test]
fn source_split_points_make_l_and_j_cuts() {
    let (mut s, bars) = fresh();
    s.execute("source.open", json!({"item": bars})).unwrap();
    let r = s.sequence_rate();
    s.execute("project.setMarks", json!({"item": bars, "in": r.tick_of(48).0, "out": r.tick_of(95).0})).unwrap();
    // J-cut: audio starts 12 frames before the picture
    s.execute("markers.markSplitAudioIn", json!({"target": "source", "time": r.tick_of(36).0})).unwrap();
    assert_eq!(s.project.item(ItemId(bars)).unwrap().split.audio_in, Some(r.tick_of(36)));
    s.execute("markers.goToSplitAudioIn", json!({"target": "source"})).unwrap();
    assert_eq!(s.state.source_playhead, r.tick_of(36));
    s.execute("playhead.set", json!({"frame": 0})).unwrap();
    s.execute("source.overwrite", json!({})).unwrap();
    let q = s.active_sequence().unwrap();
    let v = &q.video_tracks[0].items[0];
    let a = &q.audio_tracks[0].items[0];
    assert_eq!((v.start, v.duration, v.source_in), (r.tick_of(12), r.tick_of(48), r.tick_of(48)));
    assert_eq!((a.start, a.duration, a.source_in), (Tick::ZERO, r.tick_of(60), r.tick_of(36)));
    assert_eq!(v.link, a.link, "still linked");
    assert_eq!(v.source_time_at(r.tick_of(30)), a.source_time_at(r.tick_of(30)), "in sync");
    assert_eq!(s.playhead(), r.tick_of(60));
    // an ordinary In clears the source split In
    s.execute("markers.markIn", json!({"target": "source", "time": r.tick_of(40).0})).unwrap();
    assert!(s.project.item(ItemId(bars)).unwrap().split.audio_in.is_none());
}

#[test]
fn range_and_chapter_markers() {
    let mut s = demo();
    s.execute("playhead.set", json!({"frame": 24})).unwrap();
    let id = s.execute("markers.addRange", json!({})).unwrap()["marker"].as_u64().unwrap();
    let m = s.active_sequence().unwrap().markers.iter().find(|m| m.id.0 == id).unwrap().clone();
    assert_eq!((m.start, m.duration), (f(&s, 24), f(&s, 24)), "one second");
    s.execute("markers.addRange", json!({"durationFrames": 10, "color": "Rose"})).unwrap();
    assert!(!s.is_enabled("markers.addRangeInOut"));
    s.execute("markers.markIn", json!({"frame": 48})).unwrap();
    s.execute("markers.markOut", json!({"frame": 71})).unwrap();
    let id = s.execute("markers.addRangeInOut", json!({"name": "Scene"})).unwrap()["marker"].as_u64().unwrap();
    let m = s.active_sequence().unwrap().markers.iter().find(|m| m.id.0 == id).unwrap().clone();
    assert_eq!((m.start, m.duration, m.name.as_str()), (f(&s, 48), f(&s, 24), "Scene"));
    let id = s.execute("markers.addChapter", json!({"frame": 100})).unwrap()["marker"].as_u64().unwrap();
    let m = s.active_sequence().unwrap().markers.iter().find(|m| m.id.0 == id).unwrap().clone();
    assert_eq!((m.kind, m.name.as_str(), m.start), (MarkerKind::Chapter, "Chapter 1", f(&s, 100)));
    // colour filter + Show All Marker Colors
    assert!(!s.is_enabled("markers.showAllMarkerColors"));
    s.execute("markers.filterColors", json!({"color": "Rose", "visible": false})).unwrap();
    assert_eq!(s.state.hidden_marker_colors, vec![Label::Rose]);
    s.execute("markers.showAllMarkerColors", json!({})).unwrap();
    assert!(s.state.hidden_marker_colors.is_empty());
    assert!(s.execute("markers.filterColors", json!({"hidden": ["Nope"]})).is_err());
    s.execute("markers.clearAll", json!({})).unwrap();
    assert!(s.active_sequence().unwrap().markers.is_empty());
}

#[test]
fn ripple_sequence_markers_follow_ripple_edits() {
    let mut s = demo();
    s.execute("markers.clearAll", json!({})).unwrap();
    let mk = |s: &mut Session, frame: i64| {
        s.execute("markers.add", json!({"frame": frame})).unwrap()["marker"].as_u64().unwrap();
    };
    mk(&mut s, 30);
    mk(&mut s, 200);
    let starts = |s: &Session| s.active_sequence().unwrap().markers.iter().map(|m| m.start).collect::<Vec<_>>();
    // off: extract leaves markers alone
    s.execute("markers.markIn", json!({"frame": 24})).unwrap();
    s.execute("markers.markOut", json!({"frame": 47})).unwrap();
    s.execute("sequence.extract", json!({})).unwrap();
    assert_eq!(starts(&s), vec![f(&s, 30), f(&s, 200)]);
    s.execute("edit.undo", json!({})).unwrap();
    // on: the marker inside goes, the later one moves left
    assert_eq!(s.execute("markers.rippleSequenceMarkers", json!({})).unwrap()["rippleSequenceMarkers"], json!(true));
    s.execute("sequence.extract", json!({})).unwrap();
    assert_eq!(starts(&s), vec![f(&s, 176)]);
    s.execute("edit.undo", json!({})).unwrap();
    // insert from the source pushes later markers right
    let item = s.active_sequence().unwrap().video_tracks[0].items[0].item;
    s.execute("source.open", json!({"item": item.0})).unwrap();
    let r = s.sequence_rate();
    s.execute("project.setMarks", json!({"item": item.0, "in": r.tick_of(0).0, "out": r.tick_of(11).0})).unwrap();
    s.execute("markers.clearInOut", json!({})).unwrap();
    s.execute("playhead.set", json!({"frame": 100})).unwrap();
    s.execute("source.insert", json!({})).unwrap();
    assert_eq!(starts(&s), vec![f(&s, 30), f(&s, 212)]);
    // ripple trim of the first clip's out point moves the later markers
    let first = s.active_sequence().unwrap().video_tracks[0].items[0].clone();
    s.execute("timeline.setTrack", json!({"track": "A2", "syncLock": false})).unwrap();
    s.execute("timeline.trim", json!({"clip": first.id.0, "edge": "out", "mode": "ripple", "deltaFrames": -6})).unwrap();
    let st = starts(&s);
    assert_eq!(st.last(), Some(&f(&s, 206)), "{st:?}");
}

#[test]
fn copy_paste_includes_sequence_markers() {
    let mut s = demo();
    s.execute("markers.clearAll", json!({})).unwrap();
    let first = s.active_sequence().unwrap().video_tracks[0].items[0].clone();
    s.execute("markers.add", json!({"frame": 10, "name": "a"})).unwrap();
    s.execute("timeline.select", json!({"clips": [first.id.0]})).unwrap();
    s.execute("edit.copy", json!({})).unwrap();
    s.execute("playhead.set", json!({"frame": 500})).unwrap();
    s.execute("edit.paste", json!({})).unwrap();
    assert_eq!(s.active_sequence().unwrap().markers.len(), 1, "off: markers stay behind");
    s.execute("edit.undo", json!({})).unwrap();
    s.execute("markers.copyPasteIncludesSequenceMarkers", json!({"on": true})).unwrap();
    s.execute("timeline.select", json!({"clips": [first.id.0]})).unwrap();
    s.execute("edit.copy", json!({})).unwrap();
    s.execute("playhead.set", json!({"frame": 500})).unwrap();
    s.execute("edit.paste", json!({})).unwrap();
    let ms = &s.active_sequence().unwrap().markers;
    assert_eq!(ms.len(), 2);
    assert_eq!(ms[1].start, f(&s, 510));
    assert_eq!(ms[1].name, "a");
    assert_ne!(ms[0].id, ms[1].id);
}

#[test]
fn selection_follows_playhead() {
    let mut s = demo();
    s.execute("sequence.selectionFollowsPlayhead", json!({"on": true})).unwrap();
    s.execute("playhead.set", json!({"frame": 10})).unwrap();
    let q = s.active_sequence().unwrap();
    let v = q.video_tracks[0].item_at(f(&s, 10)).unwrap().id;
    let a = q.audio_tracks[0].item_at(f(&s, 10)).unwrap().id;
    assert!(s.state.selection.contains(&v) && s.state.selection.contains(&a), "{:?}", s.state.selection);
    s.execute("playhead.nextEdit", json!({})).unwrap();
    assert!(!s.state.selection.contains(&v), "moves on with the playhead");
    s.execute("sequence.selectionFollowsPlayhead", json!({})).unwrap();
    let sel = s.state.selection.clone();
    s.execute("playhead.set", json!({"frame": 10})).unwrap();
    assert_eq!(s.state.selection, sel, "off again");
}

/// #208: with no In / Out mark, Go to In / Go to Out move to the start / end of the sequence
/// (they did nothing); with marks they go to them.
#[test]
fn go_to_in_out_without_marks_go_to_the_sequence_ends() {
    let mut s = demo();
    let end = s.active_sequence().unwrap().duration();
    let mid = filmcraft_time::Tick(end.0 / 2);
    s.execute("playhead.set", json!({"time": mid.0})).unwrap();
    s.execute("markers.goToIn", json!({})).unwrap();
    assert_eq!(s.playhead(), filmcraft_time::Tick::ZERO, "the start");
    s.execute("markers.goToOut", json!({})).unwrap();
    assert_eq!(s.playhead(), end, "the end");
    // with marks, the marks
    let at = filmcraft_time::Tick(end.0 / 4);
    s.execute("playhead.set", json!({"time": at.0})).unwrap();
    s.execute("markers.markIn", json!({})).unwrap();
    s.execute("playhead.set", json!({"time": mid.0})).unwrap();
    s.execute("markers.goToIn", json!({})).unwrap();
    assert_eq!(s.playhead(), s.active_sequence().unwrap().mark_in.unwrap());
}
