//! Sequence / Markers menu commands (`sequence_tools`).

use super::*;
use filmcraft_edit as edit;
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

fn audio_fixture(channels: u16) -> filmcraft_media::SharedSource {
    let frame: Vec<_> = (0..channels).map(|c| (f32::from(c) + 1.0) / 10.0).collect();
    let bytes = filmcraft_media::wav::write_wav16(&frame.repeat(5 * 48_000), channels, 48_000);
    std::sync::Arc::new(filmcraft_media::wav::WavSource::parse("channels.wav", bytes.into()).unwrap())
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

fn assert_preserved_audio_cut(before: &Sequence, after: &Sequence, left: ClipId, right: ClipId, video_joined: bool) {
    let mut expected = before.audio_tracks[0].items.clone();
    if video_joined {
        let link = before.find_item(left).unwrap().1.link;
        expected.iter_mut().find(|item| item.id == right).unwrap().link = link;
    }
    assert_eq!(after.audio_tracks[0].items, expected, "audio attributes stay intact; joined video retains both partners");
}

#[test]
fn join_through_edits_keeps_surviving_audio_in_linked_moves() {
    let (mut s, item) = fresh();
    place(&mut s, item, "V1", 0, 0, 24);
    s.execute("playhead.set", json!({"frame": 12})).unwrap();
    s.execute("sequence.addEditAllTracks", json!({})).unwrap();
    let right = s.active_sequence().unwrap().audio_tracks[0].items[1].id;
    s.execute("timeline.select", json!({"clips": [right.0]})).unwrap();
    s.execute("clip.audioChannels", json!({"channels": [0, 1]})).unwrap();
    let before_join = s.project.clone();
    s.execute("sequence.joinThroughEdits", json!({"all": true})).unwrap();
    let after_join = s.project.clone();
    let q = s.active_sequence().unwrap();
    assert_eq!(q.video_tracks[0].items.len(), 1);
    assert_eq!(q.audio_tracks[0].items.len(), 2);
    let video = q.video_tracks[0].items[0].id;
    let audio = q.audio_tracks[0].items.clone();
    let offset = f(&s, 24);
    s.execute("timeline.move", json!({"moves": [{"clip": video.0, "track": "V1", "time": offset.0}], "linked": true})).unwrap();
    let after_move = s.project.clone();
    let q = s.active_sequence().unwrap();
    for original in &audio {
        let moved = q.find_item(original.id).unwrap().1;
        assert_eq!(moved.start, original.start + offset, "every surviving linked audio piece follows the joined video");
        assert_eq!(moved.source_channels, original.source_channels);
        assert_eq!((moved.item, moved.source_in, moved.duration), (original.item, original.source_in, original.duration));
    }
    s.execute("edit.undo", json!({})).unwrap();
    assert_eq!(s.project, after_join);
    s.execute("edit.undo", json!({})).unwrap();
    assert_eq!(s.project, before_join);
    s.execute("edit.redo", json!({})).unwrap();
    assert_eq!(s.project, after_join);
    s.execute("edit.redo", json!({})).unwrap();
    assert_eq!(s.project, after_move);
}

#[test]
fn re_cutting_healed_video_keeps_surviving_audio_on_the_correct_side() {
    for command in ["sequence.addEditAllTracks", "sequence.addEdit"] {
        for cut in [6, 12] {
            let (mut s, item) = fresh();
            place(&mut s, item, "V1", 0, 0, 36);
            for frame in [12, 24] {
                s.execute("playhead.set", json!({"frame": frame})).unwrap();
                s.execute("sequence.addEditAllTracks", json!({})).unwrap();
            }
            let middle = s.active_sequence().unwrap().audio_tracks[0].items[1].id;
            s.execute("timeline.select", json!({"clips": [middle.0]})).unwrap();
            s.execute("clip.audioChannels", json!({"channels": [0, 1]})).unwrap();
            s.execute("sequence.joinThroughEdits", json!({"all": true})).unwrap();
            let q = s.active_sequence().unwrap();
            let partners: Vec<_> = q.all_tracks().flat_map(|track| &track.items).map(|item| item.id.0).collect();
            s.execute("timeline.select", json!({"clips": partners})).unwrap();
            s.execute("playhead.set", json!({"frame": cut})).unwrap();
            let before_cut = s.project.clone();
            s.execute(command, json!({})).unwrap();
            let after_cut = s.project.clone();
            let q = s.active_sequence().unwrap();
            let right_video = q.video_tracks[0].items[1].id;
            let audio = q.audio_tracks[0].items.clone();
            let at = f(&s, cut);
            let destination = f(&s, 36);
            s.execute("timeline.move", json!({"moves": [{"clip": right_video.0, "track": "V1", "time": destination.0}], "linked": true})).unwrap();
            let after_move = s.project.clone();
            for original in &audio {
                let moved = s.active_sequence().unwrap().find_item(original.id).unwrap().1;
                let expected = if original.start >= at { original.start + destination - at } else { original.start };
                assert_eq!(moved.start, expected, "{command}, frame {cut}: only right-hand audio follows right video");
                assert_eq!(moved.source_channels, original.source_channels);
                assert_eq!((moved.item, moved.source_in, moved.duration), (original.item, original.source_in, original.duration));
            }
            s.execute("edit.undo", json!({})).unwrap();
            assert_eq!(s.project, after_cut);
            s.execute("edit.undo", json!({})).unwrap();
            assert_eq!(s.project, before_cut);
            s.execute("edit.redo", json!({})).unwrap();
            assert_eq!(s.project, after_cut);
            s.execute("edit.redo", json!({})).unwrap();
            assert_eq!(s.project, after_move);
        }
    }
}

#[test]
fn recut_after_video_healing_keeps_unselected_audio_linked() {
    let (mut s, item) = fresh();
    place(&mut s, item, "V1", 0, 0, 36);
    for frame in [12, 24] {
        s.execute("playhead.set", json!({"frame": frame})).unwrap();
        s.execute("sequence.addEditAllTracks", json!({})).unwrap();
    }
    let middle = s.active_sequence().unwrap().audio_tracks[0].items[1].id;
    s.execute("timeline.select", json!({"clips": [middle.0]})).unwrap();
    s.execute("clip.audioChannels", json!({"channels": [0, 1]})).unwrap();
    s.execute("sequence.joinThroughEdits", json!({"all": true})).unwrap();
    let video = s.active_sequence().unwrap().video_tracks[0].items[0].id;
    let audio = s.active_sequence().unwrap().audio_tracks[0].items.clone();
    s.execute("sequence.linkedSelection", json!({"on": false})).unwrap();
    s.execute("timeline.select", json!({"clips": [video.0]})).unwrap();
    let before = s.project.clone();
    s.execute("sequence.addEdit", json!({"time": f(&s, 12).0})).unwrap();
    let after_cut = s.project.clone();
    for original in &audio {
        let mut actual = s.active_sequence().unwrap().find_item(original.id).unwrap().1.clone();
        actual.link = original.link;
        assert_eq!(&actual, original, "a video-only cut preserves every audio content field");
    }
    let right = s.active_sequence().unwrap().video_tracks[0].items[1].id;
    s.execute("timeline.move", json!({"moves": [{"clip": right.0, "track": "V1", "time": f(&s, 48).0}], "linked": true})).unwrap();
    for original in &audio {
        let actual = s.active_sequence().unwrap().find_item(original.id).unwrap().1;
        let expected = if original.start >= f(&s, 12) { original.start + f(&s, 36) } else { original.start };
        assert_eq!(actual.start, expected, "preexisting unselected right audio follows the right video");
        assert_eq!(actual.source_channels, original.source_channels);
    }
    let after_move = s.project.clone();
    for expected in [&after_cut, &before] {
        s.execute("edit.undo", json!({})).unwrap();
        assert_eq!(&s.project, expected);
    }
    for expected in [&after_cut, &after_move] {
        s.execute("edit.redo", json!({})).unwrap();
        assert_eq!(&s.project, expected);
    }
}

#[test]
fn insert_after_video_healing_keeps_right_audio_linked() {
    check_insert_after_video_healing("place");
}

#[test]
fn automate_insert_after_video_healing_keeps_right_audio_linked() {
    check_insert_after_video_healing("automate");
}

fn check_insert_after_video_healing(mode: &str) {
    let (mut s, item) = fresh();
    place(&mut s, item, "V1", 0, 0, 36);
    for frame in [12, 24] {
        s.execute("playhead.set", json!({"frame": frame})).unwrap();
        s.execute("sequence.addEditAllTracks", json!({})).unwrap();
    }
    let middle = s.active_sequence().unwrap().audio_tracks[0].items[1].id;
    s.execute("timeline.select", json!({"clips": [middle.0]})).unwrap();
    s.execute("clip.audioChannels", json!({"channels": [0, 1]})).unwrap();
    s.execute("sequence.joinThroughEdits", json!({"all": true})).unwrap();
    let matte = s.execute("file.newColorMatte", json!({"seconds": 0.25})).unwrap()["item"].as_u64().unwrap();
    let before_insert = s.project.clone();
    if mode == "automate" {
        s.execute("playhead.set", json!({"frame": 12})).unwrap();
        s.execute("clip.automateToSequence", json!({"items": [matte], "ordering": "selection", "method": "insert", "overlapFrames": 0, "ignoreAudio": true}))
            .unwrap();
    } else {
        s.execute("timeline.place", json!({"item": matte, "track": "V1", "frame": 12, "duration": f(&s, 6).0, "insert": true})).unwrap();
    }
    let after_insert = s.project.clone();
    let q = s.active_sequence().unwrap();
    let video = q.video_tracks[0].items.last().unwrap().id;
    let audio = q.audio_tracks[0].items.clone();
    s.execute("timeline.move", json!({"moves": [{"clip": video.0, "track": "V1", "time": f(&s, 48).0}], "linked": true})).unwrap();
    let after_move = s.project.clone();
    for original in &audio {
        let moved = s.active_sequence().unwrap().find_item(original.id).unwrap().1;
        let expected = if original.start >= f(&s, 18) { original.start + f(&s, 30) } else { original.start };
        assert_eq!(moved.start, expected, "inserted right audio follows the right video");
        assert_eq!(moved.source_channels, original.source_channels);
    }
    for expected in [&after_insert, &before_insert] {
        s.execute("edit.undo", json!({})).unwrap();
        assert_eq!(&s.project, expected);
    }
    for expected in [&after_insert, &after_move] {
        s.execute("edit.redo", json!({})).unwrap();
        assert_eq!(&s.project, expected);
    }
}

#[test]
fn range_edits_after_video_healing_keep_right_audio_linked() {
    for mode in ["overwrite", "lift", "extract"] {
        let (mut s, item) = fresh();
        place(&mut s, item, "V1", 0, 0, 36);
        for frame in [12, 24] {
            s.execute("playhead.set", json!({"frame": frame})).unwrap();
            s.execute("sequence.addEditAllTracks", json!({})).unwrap();
        }
        let middle = s.active_sequence().unwrap().audio_tracks[0].items[1].id;
        s.execute("timeline.select", json!({"clips": [middle.0]})).unwrap();
        s.execute("clip.audioChannels", json!({"channels": [0, 1]})).unwrap();
        s.execute("sequence.joinThroughEdits", json!({"all": true})).unwrap();
        let mut project = (*s.project).clone();
        let sequence = project.sequence_mut(s.state.active_sequence.unwrap()).unwrap();
        let tracks: Vec<_> = sequence.all_tracks().map(|track| track.id).collect();
        let start = f(&s, 12);
        let range = filmcraft_time::TimeRange::new(start, f(&s, 6));
        let mut next = 1_000_000;
        let mut ctx = edit::EditCtx { next_id: &mut next, media_duration: &|_| None, media_start: &|_| Tick::ZERO, min_duration: f(&s, 1) };
        match mode {
            "overwrite" => {
                let mut replacement = sequence.video_tracks[0].items[0].clone();
                replacement.id = ClipId(999_999);
                replacement.item = ItemId(999_998);
                replacement.start = range.start;
                replacement.duration = range.duration;
                replacement.link = None;
                let video = sequence.video_tracks[0].id;
                let audio = sequence.audio_tracks[0].id;
                let mut sound = replacement.clone();
                sound.id = ClipId(999_997);
                edit::overwrite(sequence, vec![(video, replacement), (audio, sound)], &mut ctx).unwrap();
            }
            "lift" => {
                edit::lift(sequence, &tracks, range, &mut ctx);
            }
            _ => {
                edit::extract(sequence, &tracks, range, &mut ctx);
            }
        }
        let right = sequence.video_tracks[0].items.last().unwrap();
        let right_audio: Vec<_> = sequence.audio_tracks[0].items.iter().filter(|audio| audio.start >= right.start).collect();
        assert!(!right_audio.is_empty());
        assert!(right_audio.iter().all(|audio| audio.link == right.link), "{mode} keeps all participating right fragments in the video's right group");
        assert_eq!(right_audio[0].source_channels, vec![0, 1]);
        assert_eq!(right_audio.last().unwrap().source_channels, Vec::<u16>::new());
        sequence.check().unwrap();
    }
}

#[test]
fn locked_audio_retains_content_and_links_after_healing_and_recuts() {
    let (mut s, item) = fresh();
    place(&mut s, item, "V1", 0, 0, 24);
    s.execute("playhead.set", json!({"frame": 12})).unwrap();
    s.execute("sequence.addEditAllTracks", json!({})).unwrap();
    let right = s.active_sequence().unwrap().audio_tracks[0].items[1].id;
    s.execute("timeline.select", json!({"clips": [right.0]})).unwrap();
    s.execute("clip.audioChannels", json!({"channels": [0, 1]})).unwrap();
    s.execute("timeline.setTrack", json!({"track": "A1", "locked": true})).unwrap();
    let original = s.active_sequence().unwrap().audio_tracks[0].items.clone();
    s.execute("sequence.joinThroughEdits", json!({"all": true})).unwrap();
    s.execute("playhead.set", json!({"frame": 12})).unwrap();
    s.execute("sequence.addEditAllTracks", json!({})).unwrap();
    let q = s.active_sequence().unwrap();
    for audio in &original {
        let mut actual = q.find_item(audio.id).unwrap().1.clone();
        actual.link = audio.link;
        assert_eq!(&actual, audio, "locked audio content/timing/effects/ids stay intact; only relationship ids follow the healed/re-cut video");
    }
    assert_eq!(q.find_item(right).unwrap().1.link, q.video_tracks[0].items[1].link);
    s.execute("timeline.setTrack", json!({"track": "A1", "locked": false})).unwrap();
    let before_move = s.project.clone();
    let video = s.active_sequence().unwrap().video_tracks[0].items[1].id;
    s.execute("timeline.move", json!({"moves": [{"clip": video.0, "track": "V1", "time": f(&s, 24).0}], "linked": true})).unwrap();
    assert_eq!(s.active_sequence().unwrap().find_item(right).unwrap().1.start, f(&s, 24));
    assert_eq!(s.active_sequence().unwrap().audio_tracks[0].items[0].start, Tick::ZERO);
    let after_move = s.project.clone();
    s.execute("edit.undo", json!({})).unwrap();
    assert_eq!(s.project, before_move);
    s.execute("edit.redo", json!({})).unwrap();
    assert_eq!(s.project, after_move);
}

#[test]
fn exact_through_join_rejects_a_locked_primary_cut_without_history_changes() {
    for partner_locked in [false, true] {
        let (mut s, item) = fresh();
        place(&mut s, item, "V1", 0, 0, 24);
        s.execute("playhead.set", json!({"frame": 12})).unwrap();
        s.execute("sequence.addEditAllTracks", json!({})).unwrap();
        let video = s.active_sequence().unwrap().video_tracks[0].items.clone();
        s.execute("timeline.setTrack", json!({"track": "V1", "locked": true})).unwrap();
        s.execute("timeline.setTrack", json!({"track": "A1", "locked": partner_locked})).unwrap();
        s.execute("sequence.linkedSelection", json!({"on": true})).unwrap();
        s.execute("markers.add", json!({"name": "redo sentinel"})).unwrap();
        s.execute("edit.undo", json!({})).unwrap();
        let before = s.project.clone();
        let stacks = (s.history.undo.len(), s.history.redo.len());
        assert!(stacks.1 > 0);
        assert!(s.execute("sequence.joinThroughEdits", json!({"cut": [video[0].id.0, video[1].id.0]})).is_err());
        assert_eq!(s.project, before);
        assert_eq!((s.history.undo.len(), s.history.redo.len()), stacks);
    }
}

#[test]
fn exact_audio_join_after_video_healing_keeps_the_neighboring_cut() {
    let (mut s, item) = fresh();
    place(&mut s, item, "V1", 0, 0, 36);
    for frame in [12, 24] {
        s.execute("playhead.set", json!({"frame": frame})).unwrap();
        s.execute("sequence.addEditAllTracks", json!({})).unwrap();
    }
    let audio = s.active_sequence().unwrap().audio_tracks[0].items.clone();
    let (left, middle, right) = (audio[0].id, audio[1].id, audio[2].id);
    s.execute("timeline.select", json!({"clips": [middle.0]})).unwrap();
    s.execute("clip.audioChannels", json!({"channels": [0, 1]})).unwrap();
    s.execute("sequence.joinThroughEdits", json!({"all": true})).unwrap();
    s.execute("timeline.select", json!({"clips": [middle.0]})).unwrap();
    s.execute("clip.audioChannels", json!({"channels": []})).unwrap();
    s.execute("sequence.linkedSelection", json!({"on": true})).unwrap();
    let before = s.project.clone();
    let original_right = s.active_sequence().unwrap().find_item(right).unwrap().1.clone();
    assert_eq!(s.execute("sequence.joinThroughEdits", json!({"cut": [left.0, middle.0]})).unwrap()["joined"], json!(1));
    let after = s.project.clone();
    let q = s.active_sequence().unwrap();
    assert_eq!(q.audio_tracks[0].items.len(), 2);
    assert_eq!(q.find_item(right).unwrap().1, &original_right);
    assert_eq!(q.find_item(left).unwrap().1.duration, f(&s, 24));
    s.execute("edit.undo", json!({})).unwrap();
    assert_eq!(s.project, before);
    s.execute("edit.redo", json!({})).unwrap();
    assert_eq!(s.project, after);
}

#[test]
fn join_through_edits_keeps_current_stereo_mapping_functions_strict() {
    for explicit_left in [false, true] {
        for mode in ["all", "clips", "cut"] {
            let mut s = demo();
            s.execute("playhead.set", json!({"frame": 30})).unwrap();
            s.execute("sequence.addEditAllTracks", json!({})).unwrap();
            let track = s.active_sequence().unwrap().audio_tracks[0].clone();
            let (left, right) = (track.items[0].id, track.items[1].id);
            let mapped = if explicit_left { left } else { right };
            s.execute("timeline.select", json!({"clips": [mapped.0]})).unwrap();
            s.execute("clip.audioChannels", json!({"channels": [0, 1]})).unwrap();
            let before = s.active_sequence().unwrap().clone();
            let project_before = s.project.clone();
            let (undo, redo) = (s.history.undo.len(), s.history.redo.len());
            let (a, b) = (before.find_item(left).unwrap().1, before.find_item(right).unwrap().1);
            assert_eq!(filmcraft_render::audio::source_pair(a, 2), filmcraft_render::audio::source_pair(b, 2));
            assert_eq!(filmcraft_render::audio::source_51(a, 2), filmcraft_render::audio::source_51(b, 2));
            let edits = s.execute("sequence.throughEdits", json!({})).unwrap();
            assert!(
                !edits.as_array().unwrap().iter().any(|e| e["left"] == json!(left.0) && e["right"] == json!(right.0)),
                "currently equivalent stereo mappings must preserve their distinct stored functions: {edits}"
            );
            let params = match mode {
                "all" => json!({"all": true}),
                "clips" => json!({"clips": [right.0]}),
                _ => json!({"cut": [left.0, right.0]}),
            };
            let joined = s.execute("sequence.joinThroughEdits", params);
            let after = s.active_sequence().unwrap().clone();
            assert_preserved_audio_cut(&before, &after, left, right, mode != "cut");
            assert_eq!(after.find_item(left).unwrap().1.source_channels, a.source_channels, "left selection remains");
            if mode == "cut" {
                assert!(joined.is_err());
                assert_eq!(s.project, project_before);
                assert_eq!((s.history.undo.len(), s.history.redo.len()), (undo, redo));
            } else {
                assert!(joined.unwrap()["joined"].as_u64().unwrap() > 0, "other eligible cuts may still join");
                s.execute("edit.undo", json!({})).unwrap();
                assert_eq!(s.active_sequence().unwrap(), &before);
                s.execute("edit.redo", json!({})).unwrap();
                assert_eq!(s.active_sequence().unwrap(), &after);
            }
        }
    }
}

fn remix_channel_fixture(damaged: bool) -> (Session, ItemId, std::path::PathBuf) {
    use filmcraft_isobmff::{Brand, Mp4Writer, PcmConfig, SampleEntry, TrackConfig, WriteSample, WriterOptions};
    use filmcraft_render::SourceProvider;

    let root = crate::media_test_util::tmp_dir("through-remix-channels");
    let original = root.join("mono.mov");
    let saved = root.join("project.filmcraft");
    let mut writer = Mp4Writer::new(std::io::Cursor::new(Vec::new()), WriterOptions::new(Brand::Mov)).unwrap();
    let config = PcmConfig { bits: 16, float: false, big_endian: false, signed: true, channels: 1, sample_rate: 48_000.0 };
    let audio = writer.add_track(TrackConfig::new(SampleEntry::pcm(config), 48_000)).unwrap();
    let spacer = writer.add_track(TrackConfig::new(SampleEntry::pcm(config), 48_000)).unwrap();
    let sample = 8192_i16.to_le_bytes();
    writer.write_sample(audio, WriteSample { data: &sample, duration: 1, composition_offset: 0, is_sync: true }).unwrap();
    // Interleaving the second track gives the main stream two separately readable PCM chunks.
    writer.write_sample(spacer, WriteSample { data: &sample, duration: 1, composition_offset: 0, is_sync: true }).unwrap();
    writer.write_sample(audio, WriteSample { data: &sample.repeat(47_999), duration: 47_999, composition_offset: 0, is_sync: true }).unwrap();
    let mut bytes = writer.finish().unwrap().into_inner();
    if damaged {
        let stco = bytes.windows(4).position(|b| b == b"stco").unwrap();
        assert_eq!(u32::from_be_bytes(bytes[stco + 8..stco + 12].try_into().unwrap()), 2);
        bytes[stco + 12..stco + 16].copy_from_slice(&u32::MAX.to_be_bytes());
    }
    std::fs::write(&original, bytes).unwrap();
    let mut s = Session::default();
    let imported = s.execute("file.import", json!({"paths": [original.to_string_lossy()]})).unwrap();
    let item = ItemId(imported["items"][0].as_u64().unwrap());
    s.execute("file.newSequence", json!({"video": 0, "audio": 1, "fps": 24.0})).unwrap();
    s.execute("timeline.place", json!({"item": item.0, "seconds": 0.0})).unwrap();
    s.execute("sequence.deleteTracks", json!({"audio": "A2"})).unwrap();
    let mut project = (*s.project).clone();
    let clip = &mut project.sequence_mut(s.state.active_sequence.unwrap()).unwrap().audio_tracks[0].items[0];
    let duration = Tick::from_seconds_f64(1.0);
    filmcraft_render::remix::Remix {
        target: duration,
        segments: 50.0,
        variations: 50.0,
        original: duration,
        xfade: Tick::ZERO,
        pieces: vec![filmcraft_render::remix::TickPiece { src: Tick::ZERO, len: duration }],
    }
    .store(clip);
    s.project = std::sync::Arc::new(project);
    s.execute("file.save", json!({"path": saved.to_string_lossy()})).unwrap();
    drop(s);
    let mut s = Session::default();
    s.execute("file.open", json!({"path": saved.to_string_lossy()})).unwrap();
    s.execute("playhead.set", json!({"frame": 12})).unwrap();
    s.execute("sequence.addEditAllTracks", json!({})).unwrap();
    let provider = s.media.provider(s.project.clone(), s.services.clone());
    let source = provider.source(item).unwrap();
    assert_eq!(source.info().audio_streams[0].channels, 1);
    assert_eq!(source.audio_stream(0, 0, 1, 48_000).is_err(), damaged);
    let healthy = source.audio_stream(0, 36_000, 32, 48_000).unwrap();
    assert_eq!(healthy.channel_count(), 1);
    assert_eq!(healthy.channels[0].len(), 32);
    assert!(healthy.channels[0].iter().all(|&sample| (sample - 0.25).abs() < 1e-4));
    assert_eq!(s.source(item).unwrap().info().audio_streams[0].channels, 1);
    (s, item, root)
}

fn remix_track_signal(s: &Session) -> filmcraft_frame::AudioBuffer {
    let q = s.active_sequence().unwrap();
    let provider = s.media.provider(s.project.clone(), s.services.clone());
    filmcraft_render::audio::track_input(&s.project, &q.audio_tracks[0], 36_000, 32, 48_000, &provider)
}

fn assert_remix_signal(signal: &filmcraft_frame::AudioBuffer, right: f32) {
    assert_eq!(signal.channel_count(), 2);
    for (samples, expected) in signal.channels.iter().zip([0.25, right]) {
        assert_eq!(samples.len(), 32);
        assert!(samples.iter().all(|&sample| (sample - expected).abs() < 1e-4), "expected {expected}, got {samples:?}");
    }
}

#[test]
fn join_through_edits_preserves_unproven_remix_channel_defaults() {
    for damaged in [true, false] {
        for mode in ["all", "clips", "cut"] {
            let (mut s, _, root) = remix_channel_fixture(damaged);
            let track = s.active_sequence().unwrap().audio_tracks[0].clone();
            let (left, right) = (track.items[0].id, track.items[1].id);
            s.execute("timeline.select", json!({"clips": [right.0]})).unwrap();
            s.execute("clip.audioChannels", json!({"channels": [0]})).unwrap();
            let before = s.project.clone();
            let signal_before = remix_track_signal(&s);
            assert_remix_signal(&signal_before, 0.25);
            let edits = s.execute("sequence.throughEdits", json!({})).unwrap();
            let undo = s.history.undo.len();
            let redo = s.history.redo.len();
            let params = match mode {
                "all" => json!({"all": true}),
                "clips" => json!({"clips": [left.0, right.0]}),
                _ => json!({"cut": [left.0, right.0]}),
            };
            let joined = s.execute("sequence.joinThroughEdits", params);
            let signal_after = remix_track_signal(&s);
            eprintln!("Remix damaged={damaged} mode={mode}: {joined:?}; before={:?}; after={:?}", signal_before.channels, signal_after.channels);
            let _ = std::fs::remove_dir_all(root);
            assert!(joined.is_err(), "decoder width cannot prove Remix's effective width: {joined:?}");
            assert!(edits.as_array().unwrap().is_empty());
            assert_eq!(s.project, before);
            assert_eq!(s.history.undo.len(), undo);
            assert_eq!(s.history.redo.len(), redo);
            assert_eq!(signal_after, signal_before);
        }
    }
}

#[test]
fn join_through_edits_keeps_identical_remix_channel_maps_joinable() {
    for damaged in [true, false] {
        for channels in [json!([]), json!([0])] {
            let (mut s, _, root) = remix_channel_fixture(damaged);
            let track = s.active_sequence().unwrap().audio_tracks[0].clone();
            let (left, right) = (track.items[0].id, track.items[1].id);
            s.execute("timeline.select", json!({"clips": [left.0, right.0]})).unwrap();
            s.execute("clip.audioChannels", json!({"channels": channels})).unwrap();
            let before = s.project.clone();
            let signal_before = remix_track_signal(&s);
            assert_remix_signal(&signal_before, if damaged && channels.as_array().unwrap().is_empty() { 0.0 } else { 0.25 });
            assert_eq!(s.execute("sequence.throughEdits", json!({})).unwrap().as_array().unwrap().len(), 1);
            assert_eq!(s.execute("sequence.joinThroughEdits", json!({"cut": [left.0, right.0]})).unwrap()["joined"], json!(1));
            let after = s.project.clone();
            assert_eq!(remix_track_signal(&s), signal_before);
            s.execute("edit.undo", json!({})).unwrap();
            assert_eq!(s.project, before);
            s.execute("edit.redo", json!({})).unwrap();
            assert_eq!(s.project, after);
            let _ = std::fs::remove_dir_all(root);
        }
    }
}

#[test]
fn join_through_edits_keeps_disabled_remix_file_defaults_strict() {
    let (mut s, _, root) = remix_channel_fixture(true);
    let track = s.active_sequence().unwrap().audio_tracks[0].clone();
    let (left, right) = (track.items[0].id, track.items[1].id);
    let mut project = (*s.project).clone();
    for clip in &mut project.sequence_mut(s.state.active_sequence.unwrap()).unwrap().audio_tracks[0].items {
        clip.effects.iter_mut().find(|e| e.effect == filmcraft_render::remix::EFFECT).unwrap().enabled = false;
    }
    s.project = std::sync::Arc::new(project);
    s.execute("timeline.select", json!({"clips": [right.0]})).unwrap();
    s.execute("clip.audioChannels", json!({"channels": [0]})).unwrap();
    let before = s.project.clone();
    let signal_before = remix_track_signal(&s);
    assert_remix_signal(&signal_before, 0.25);
    let undo = s.history.undo.len();
    let redo = s.history.redo.len();
    assert!(s.execute("sequence.throughEdits", json!({})).unwrap().as_array().unwrap().is_empty());
    assert!(s.execute("sequence.joinThroughEdits", json!({"cut": [left.0, right.0]})).is_err());
    assert_eq!(remix_track_signal(&s), signal_before);
    assert_eq!(s.project, before);
    assert_eq!(s.history.undo.len(), undo);
    assert_eq!(s.history.redo.len(), redo);
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn join_through_edits_preserves_unproven_proxy_channel_defaults() {
    use filmcraft_render::SourceProvider;

    let root = crate::media_test_util::tmp_dir("through-proxy-channels");
    let original = root.join("original.wav");
    let proxy = root.join("proxy.wav");
    let original_samples = [0.1, 0.2].repeat(48_000);
    let proxy_samples = [0.1, 0.2, 0.3, 0.4, 0.5, 0.6].repeat(48_000);
    std::fs::write(&original, filmcraft_media::wav::write_wav16(&original_samples, 2, 48_000)).unwrap();
    std::fs::write(&proxy, filmcraft_media::wav::write_wav16(&proxy_samples, 6, 48_000)).unwrap();
    let mut s = Session::default();
    let imported = s.execute("file.import", json!({"paths": [original.to_string_lossy()]})).unwrap();
    let item = filmcraft_project::ItemId(imported["items"][0].as_u64().unwrap());
    s.execute("file.newSequence", json!({"video": 0, "audio": 1, "fps": 24.0, "mix": "5.1", "trackType": "5.1"})).unwrap();
    s.execute("timeline.place", json!({"item": item.0, "seconds": 0.0})).unwrap();
    s.execute("media.attachProxies", json!({"item": item.0, "path": proxy.to_string_lossy()})).unwrap();
    s.execute("media.toggleProxies", json!({"enabled": true})).unwrap();
    s.execute("playhead.set", json!({"frame": 12})).unwrap();
    s.execute("sequence.addEditAllTracks", json!({})).unwrap();
    let track = s.active_sequence().unwrap().audio_tracks[0].clone();
    let (left, right) = (track.items[0].id, track.items[1].id);
    s.execute("timeline.select", json!({"clips": [right.0]})).unwrap();
    s.execute("clip.audioChannels", json!({"channels": [0, 1]})).unwrap();
    let before = s.project.clone();
    let seq = before.sequence(s.state.active_sequence.unwrap()).unwrap();
    let (a, b) = (seq.find_item(left).unwrap().1, seq.find_item(right).unwrap().1);
    let provider = s.media.provider(before.clone(), s.services.clone());
    let source = provider.source(item).unwrap();
    assert_eq!(source.info().audio_streams[0].channels, 2, "proxy reports original metadata");
    let available = source.audio_stream(0, 0, 1, 48_000).unwrap().channel_count();
    assert_eq!(available, 6, "playback returns the proxy's actual channels");
    assert_eq!(filmcraft_render::audio::source_51(a, available), vec![0, 1, 2, 3, 4, 5]);
    assert_eq!(filmcraft_render::audio::source_51(b, available), vec![0, 1]);
    let signal_before = filmcraft_render::audio::track_input(&before, &seq.audio_tracks[0], 36_000, 32, 48_000, &provider);
    let edits = s.execute("sequence.throughEdits", json!({})).unwrap();
    let undo = s.history.undo.len();
    let joined = s.execute("sequence.joinThroughEdits", json!({"cut": [left.0, right.0]}));
    if joined.is_ok() {
        let q = s.active_sequence().unwrap();
        let after_provider = s.media.provider(s.project.clone(), s.services.clone());
        let signal_after = filmcraft_render::audio::track_input(&s.project, &q.audio_tracks[0], 36_000, 32, 48_000, &after_provider);
        eprintln!("proxy join changed centre from {:?} to {:?}", signal_before.channels[2][0], signal_after.channels[2][0]);
    }
    assert!(joined.is_err(), "unproven proxy mappings must preserve the cut: {joined:?}");
    assert!(!edits.as_array().unwrap().iter().any(|e| e["left"] == json!(left.0) && e["right"] == json!(right.0)));
    assert_eq!(s.project, before);
    assert_eq!(s.history.undo.len(), undo);

    // Identical explicit and default maps remain joinable with the proxy attached.
    for (channels, expected) in [(json!([0, 1]), [0.1, 0.2, 0.0, 0.0, 0.0, 0.0]), (json!([]), [0.1, 0.2, 0.3, 0.4, 0.5, 0.6])] {
        s.execute("timeline.select", json!({"clips": [left.0, right.0]})).unwrap();
        s.execute("clip.audioChannels", json!({"channels": channels})).unwrap();
        let matching = s.project.clone();
        let q = s.active_sequence().unwrap();
        let before_provider = s.media.provider(matching.clone(), s.services.clone());
        let signal_before = filmcraft_render::audio::track_input(&matching, &q.audio_tracks[0], 36_000, 32, 48_000, &before_provider);
        assert_eq!(signal_before.channel_count(), expected.len());
        for (samples, expected) in signal_before.channels.iter().zip(expected) {
            assert_eq!(samples.len(), 32);
            assert!(samples.iter().all(|&sample| (sample - expected).abs() < 1e-4), "expected routed sample {expected}, got {samples:?}");
        }
        let edits = s.execute("sequence.throughEdits", json!({})).unwrap();
        assert!(edits.as_array().unwrap().iter().any(|e| e["left"] == json!(left.0) && e["right"] == json!(right.0)));
        let joined = s.execute("sequence.joinThroughEdits", json!({"cut": [left.0, right.0]})).unwrap();
        assert_eq!(joined["joined"], json!(1));
        let after = s.project.clone();
        let q = s.active_sequence().unwrap();
        let after_provider = s.media.provider(after.clone(), s.services.clone());
        let signal_after = filmcraft_render::audio::track_input(&after, &q.audio_tracks[0], 36_000, 32, 48_000, &after_provider);
        assert_eq!(signal_after, signal_before, "an identical proxy map preserves the sound");
        s.execute("edit.undo", json!({})).unwrap();
        assert_eq!(s.project, matching);
        s.execute("edit.redo", json!({})).unwrap();
        assert_eq!(s.project, after);
        s.execute("edit.undo", json!({})).unwrap();
        assert_eq!(s.project, matching);
    }
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn join_through_edits_preserves_reopened_channel_defaults() {
    use filmcraft_render::SourceProvider;

    let root = crate::media_test_util::tmp_dir("through-reopened-channels");
    let original = root.join("original.wav");
    let saved = root.join("project.filmcraft");
    std::fs::write(&original, filmcraft_media::wav::write_wav16(&[0.1, 0.2].repeat(48_000), 2, 48_000)).unwrap();
    let mut s = Session::default();
    let imported = s.execute("file.import", json!({"paths": [original.to_string_lossy()]})).unwrap();
    let item = filmcraft_project::ItemId(imported["items"][0].as_u64().unwrap());
    s.execute("file.newSequence", json!({"video": 0, "audio": 1, "fps": 24.0, "mix": "5.1", "trackType": "5.1"})).unwrap();
    s.execute("timeline.place", json!({"item": item.0, "seconds": 0.0})).unwrap();
    s.execute("playhead.set", json!({"frame": 12})).unwrap();
    s.execute("sequence.addEditAllTracks", json!({})).unwrap();
    let track = s.active_sequence().unwrap().audio_tracks[0].clone();
    let (left, right) = (track.items[0].id, track.items[1].id);
    s.execute("timeline.select", json!({"clips": [right.0]})).unwrap();
    s.execute("clip.audioChannels", json!({"channels": [0, 1]})).unwrap();
    s.execute("file.save", json!({"path": saved.to_string_lossy()})).unwrap();
    drop(s);

    std::fs::write(&original, filmcraft_media::wav::write_wav16(&[0.1, 0.2, 0.3, 0.4, 0.5, 0.6].repeat(48_000), 6, 48_000)).unwrap();
    let mut s = Session::default();
    s.execute("file.open", json!({"path": saved.to_string_lossy()})).unwrap();
    assert_eq!(s.execute("media.refreshChanged", json!({})).unwrap(), json!({"refreshed": []}));
    let before = s.project.clone();
    assert_eq!(before.item(item).unwrap().as_media().unwrap().info.audio_streams[0].channels, 2);
    let seq = before.sequence(s.state.active_sequence.unwrap()).unwrap();
    let provider = s.media.provider(before.clone(), s.services.clone());
    let source = provider.source(item).unwrap();
    assert_eq!(source.info().audio_streams[0].channels, 6);
    assert_eq!(source.audio_stream(0, 0, 1, 48_000).unwrap().channel_count(), 6);
    let signal_before = filmcraft_render::audio::track_input(&before, &seq.audio_tracks[0], 36_000, 32, 48_000, &provider);
    for (channel, expected) in signal_before.channels.iter().zip([0.1, 0.2, 0.0, 0.0, 0.0, 0.0]) {
        assert!(channel.iter().all(|sample| (sample - expected).abs() < 1e-4));
    }
    let edits = s.execute("sequence.throughEdits", json!({})).unwrap();
    let undo = s.history.undo.len();
    let joined = s.execute("sequence.joinThroughEdits", json!({"cut": [left.0, right.0]}));
    if joined.is_ok() {
        let q = s.active_sequence().unwrap();
        let after_provider = s.media.provider(s.project.clone(), s.services.clone());
        let signal_after = filmcraft_render::audio::track_input(&s.project, &q.audio_tracks[0], 36_000, 32, 48_000, &after_provider);
        eprintln!("reopened join changed centre from {:?} to {:?}", signal_before.channels[2][0], signal_after.channels[2][0]);
    }
    let q = s.active_sequence().unwrap();
    let after_provider = s.media.provider(s.project.clone(), s.services.clone());
    let signal_after = filmcraft_render::audio::track_input(&s.project, &q.audio_tracks[0], 36_000, 32, 48_000, &after_provider);
    let _ = std::fs::remove_dir_all(root);
    assert!(joined.is_err(), "saved metadata must not prove the current file's channel width: {joined:?}");
    assert_eq!(signal_after, signal_before);
    assert!(!edits.as_array().unwrap().iter().any(|e| e["left"] == json!(left.0) && e["right"] == json!(right.0)));
    assert_eq!(s.project, before);
    assert_eq!(s.history.undo.len(), undo);
}

fn assert_persisted_through_signal(signal: &filmcraft_frame::AudioBuffer, stereo: bool, available: usize, explicit: bool) {
    let expected = if available == 2 || explicit {
        if stereo { vec![0.1, 0.2] } else { vec![0.1, 0.2, 0.0, 0.0, 0.0, 0.0] }
    } else if stereo {
        // Current renderer folds a default six-channel source down to stereo (C and surrounds,
        // without LFE); an explicit first-pair pick still plays only L/R.
        let k = std::f32::consts::FRAC_1_SQRT_2;
        vec![0.1 + k * (0.3 + 0.5), 0.2 + k * (0.3 + 0.6)]
    } else {
        vec![0.1, 0.2, 0.3, 0.4, 0.5, 0.6]
    };
    assert_eq!(signal.channel_count(), expected.len());
    for (samples, expected) in signal.channels.iter().zip(expected) {
        assert_eq!(samples.len(), 32);
        assert!(samples.iter().all(|&sample| (sample - expected).abs() < 1e-4), "expected routed sample {expected}, got {samples:?}");
    }
}

fn reopen_through_signal(path: &std::path::Path, item: ItemId, proxy: bool, available: usize) -> (Session, filmcraft_frame::AudioBuffer) {
    let mut s = Session::default();
    s.execute("file.open", json!({"path": path.to_string_lossy()})).unwrap();
    if proxy {
        s.execute("media.toggleProxies", json!({"enabled": true})).unwrap();
    }
    assert_eq!(s.project.item(item).unwrap().as_media().unwrap().info.audio_streams[0].channels, 2, "saved metadata remains stereo");
    let source = s.source(item).unwrap();
    assert_eq!(source.info().audio_streams[0].channels as usize, if proxy { 2 } else { available });
    let decoded = source.audio_stream(0, 36_000, 32, 48_000).unwrap();
    assert_eq!(decoded.channel_count(), available, "actual fresh decoder/proxy buffer");
    for (c, samples) in decoded.channels.iter().enumerate() {
        assert_eq!(samples.len(), 32);
        assert!(samples.iter().all(|&sample| (sample - (c + 1) as f32 / 10.0).abs() < 1e-4));
    }
    let signal = remix_track_signal(&s);
    (s, signal)
}

fn persisted_through_file_case(stereo: bool, mode: &str, explicit_left: bool, maps: &str, when: &str, proxy: bool) {
    let root = crate::media_test_util::tmp_dir("through-persisted-mapping");
    let original = root.join("original.wav");
    let proxy_file = root.join("proxy.wav");
    let unjoined_file = root.join("unjoined.filmcraft");
    let attempted_file = root.join("attempted.filmcraft");
    let two = filmcraft_media::wav::write_wav16(&[0.1, 0.2].repeat(48_000), 2, 48_000);
    let six = filmcraft_media::wav::write_wav16(&[0.1, 0.2, 0.3, 0.4, 0.5, 0.6].repeat(48_000), 6, 48_000);
    std::fs::write(&original, &two).unwrap();
    std::fs::write(&proxy_file, &two).unwrap();
    let replacing = if proxy { &proxy_file } else { &original };
    let mut s = Session::default();
    let imported = s.execute("file.import", json!({"paths": [original.to_string_lossy()]})).unwrap();
    let item = ItemId(imported["items"][0].as_u64().unwrap());
    s.execute(
        "file.newSequence",
        json!({"video": 0, "audio": 1, "fps": 24.0, "mix": if stereo { "stereo" } else { "5.1" }, "trackType": if stereo { "standard" } else { "5.1" }}),
    )
    .unwrap();
    s.execute("timeline.place", json!({"item": item.0, "seconds": 0.0})).unwrap();
    if proxy {
        s.execute("media.attachProxies", json!({"item": item.0, "path": proxy_file.to_string_lossy()})).unwrap();
        s.execute("media.toggleProxies", json!({"enabled": true})).unwrap();
    }
    s.execute("playhead.set", json!({"frame": 12})).unwrap();
    s.execute("sequence.addEditAllTracks", json!({})).unwrap();
    let track = s.active_sequence().unwrap().audio_tracks[0].clone();
    let (left, right) = (track.items[0].id, track.items[1].id);
    let picked = match maps {
        "default" => Vec::new(),
        "explicit" => vec![left.0, right.0],
        _ => vec![if explicit_left { left.0 } else { right.0 }],
    };
    if !picked.is_empty() {
        s.execute("timeline.select", json!({"clips": picked})).unwrap();
        s.execute("clip.audioChannels", json!({"channels": [0, 1]})).unwrap();
    }
    let source = s.source(item).unwrap();
    assert_eq!(source.info().audio_streams[0].channels, 2);
    if when == "before" {
        std::fs::write(replacing, &six).unwrap();
    }
    let cached = source.audio_stream(0, 36_000, 32, 48_000).unwrap();
    assert_eq!(cached.channel_count(), 2, "the opened decoder retains its earlier stereo layout");
    for samples in &cached.channels {
        assert_eq!(samples.len(), 32);
        assert!(samples.iter().all(|&sample| sample > 0.05 && sample < 0.7), "the fixture must produce nonzero cached audio");
    }
    let current = remix_track_signal(&s);
    assert_eq!(current.channel_count(), if stereo { 2 } else { 6 });
    for (rendered, decoded) in current.channels[..2].iter().zip(&cached.channels) {
        assert_eq!(rendered.len(), decoded.len());
        assert!(rendered.iter().zip(decoded).all(|(a, b)| (a - b).abs() < 1e-6), "unity panning must retain the cached samples");
    }
    assert!(current.channels[2..].iter().all(|channel| channel.len() == 32 && channel.iter().all(|&sample| sample == 0.0)));
    s.execute("file.save", json!({"path": unjoined_file.to_string_lossy()})).unwrap();
    let before = s.project.clone();
    let undo = s.history.undo.len();
    let redo = s.history.redo.len();
    let opened = s.media.open_sources();
    let query = s.execute("sequence.throughEdits", json!({})).unwrap();
    assert_eq!(s.media.open_sources(), opened, "eligibility cannot open or decode a source");
    let params = match mode {
        "all" => json!({"all": true}),
        "clips" => json!({"clips": [left.0, right.0]}),
        _ => json!({"cut": [left.0, right.0]}),
    };
    let joined = s.execute("sequence.joinThroughEdits", params);
    let after = s.project.clone();
    let undo_after = s.history.undo.len();
    let redo_after = s.history.redo.len();
    assert_eq!(remix_track_signal(&s), current, "cached playback alone can look safe");
    if joined.is_ok() {
        s.execute("edit.undo", json!({})).unwrap();
        assert_eq!(s.project, before);
        s.execute("edit.redo", json!({})).unwrap();
        assert_eq!(s.project, after);
    }
    s.execute("file.save", json!({"path": attempted_file.to_string_lossy()})).unwrap();
    if when == "after" {
        std::fs::write(replacing, &six).unwrap();
    }
    drop(source);
    drop(s);
    let available = if when == "none" { 2 } else { 6 };
    let (unjoined, expected) = reopen_through_signal(&unjoined_file, item, proxy, available);
    let (attempted, actual) = reopen_through_signal(&attempted_file, item, proxy, available);
    let right_explicit = maps == "explicit" || (maps == "mixed" && !explicit_left);
    assert_persisted_through_signal(&expected, stereo, available, right_explicit);
    eprintln!(
        "persisted through stereo={stereo} mode={mode} maps={maps} explicit_left={explicit_left} replacement={when} proxy={proxy}: {joined:?}; cached={:?}; unjoined={:?}; attempted={:?}",
        current.channels, expected.channels, actual.channels
    );
    assert_eq!(actual, expected, "joining must preserve the selected-channel function after save/reopen and later file replacement");
    if maps == "mixed" {
        assert!(joined.is_err(), "mutable file/proxy defaults require exact source-channel maps: {joined:?}");
        assert!(query.as_array().unwrap().is_empty());
        assert_eq!(after, before);
        assert_eq!(undo_after, undo);
        assert_eq!(redo_after, redo);
        assert_eq!(attempted.active_sequence().unwrap().audio_tracks[0].items.len(), 2);
    } else {
        assert_eq!(joined.unwrap()["joined"], json!(1));
        assert_eq!(query.as_array().unwrap().len(), 1);
        assert_eq!(attempted.active_sequence().unwrap().audio_tracks[0].items.len(), 1);
    }
    drop((unjoined, attempted));
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn join_through_edits_preserves_surround_file_mapping_after_save_and_replacement() {
    for when in ["before", "after", "none"] {
        for explicit_left in [false, true] {
            for mode in ["all", "clips", "cut"] {
                persisted_through_file_case(false, mode, explicit_left, "mixed", when, false);
            }
        }
    }
}

#[test]
fn join_through_edits_preserves_stereo_file_mapping_after_save_and_replacement() {
    for when in ["before", "after", "none"] {
        for explicit_left in [false, true] {
            for mode in ["all", "clips", "cut"] {
                persisted_through_file_case(true, mode, explicit_left, "mixed", when, false);
            }
        }
    }
}

#[test]
fn join_through_edits_preserves_proxy_mapping_after_save_and_replacement() {
    for stereo in [false, true] {
        for when in ["before", "after"] {
            for explicit_left in [false, true] {
                for mode in ["all", "clips", "cut"] {
                    persisted_through_file_case(stereo, mode, explicit_left, "mixed", when, true);
                }
            }
        }
    }
}

#[test]
fn join_through_edits_keeps_identical_file_maps_after_save_and_replacement() {
    for stereo in [false, true] {
        for proxy in [false, true] {
            for when in ["before", "after", "none"] {
                for maps in ["default", "explicit"] {
                    persisted_through_file_case(stereo, "cut", false, maps, when, proxy);
                }
            }
        }
    }
}

#[test]
fn join_through_edits_keeps_identical_disabled_remix_generator_maps_joinable() {
    let mut s = Session::default();
    s.execute("file.newSequence", json!({"video": 0, "audio": 1, "fps": 24.0})).unwrap();
    let item = ItemId(s.execute("file.newBarsAndTone", json!({"seconds": 1.0})).unwrap()["item"].as_u64().unwrap());
    s.execute("timeline.place", json!({"item": item.0, "seconds": 0.0, "video": false})).unwrap();
    s.execute("playhead.set", json!({"frame": 12})).unwrap();
    s.execute("sequence.addEditAllTracks", json!({})).unwrap();
    let track = s.active_sequence().unwrap().audio_tracks[0].clone();
    let (left, right) = (track.items[0].id, track.items[1].id);
    let mut project = (*s.project).clone();
    for clip in &mut project.sequence_mut(s.state.active_sequence.unwrap()).unwrap().audio_tracks[0].items {
        let duration = clip.duration;
        filmcraft_render::remix::Remix {
            target: duration,
            segments: 50.0,
            variations: 50.0,
            original: duration,
            xfade: Tick::ZERO,
            pieces: vec![filmcraft_render::remix::TickPiece { src: Tick::ZERO, len: duration }],
        }
        .store(clip);
        clip.effects.iter_mut().find(|e| e.effect == filmcraft_render::remix::EFFECT).unwrap().enabled = false;
    }
    s.project = std::sync::Arc::new(project);
    s.execute("timeline.select", json!({"clips": [right.0]})).unwrap();
    s.execute("clip.audioChannels", json!({"channels": [0, 1]})).unwrap();
    let source = s.source(item).unwrap();
    assert_eq!(source.info().audio_streams[0].channels, 2);
    assert_eq!(source.audio_stream(0, 36_000, 32, 48_000).unwrap().channel_count(), 2);
    let before = s.project.clone();
    let (undo, redo) = (s.history.undo.len(), s.history.redo.len());
    let signal_before = remix_track_signal(&s);
    assert_eq!(signal_before.channel_count(), 2);
    assert!(signal_before.channels.iter().all(|samples| samples.len() == 32 && samples.iter().any(|&sample| sample.abs() > 0.01)));
    assert!(s.execute("sequence.throughEdits", json!({})).unwrap().as_array().unwrap().is_empty());
    assert!(s.execute("sequence.joinThroughEdits", json!({"cut": [left.0, right.0]})).is_err());
    assert_eq!(s.project, before);
    assert_eq!((s.history.undo.len(), s.history.redo.len()), (undo, redo));
    assert_eq!(remix_track_signal(&s), signal_before);

    s.execute("timeline.select", json!({"clips": [left.0]})).unwrap();
    s.execute("clip.audioChannels", json!({"channels": [0, 1]})).unwrap();
    let before_equal = s.project.clone();
    let signal_equal = remix_track_signal(&s);
    assert_eq!(signal_equal, signal_before);
    assert_eq!(s.execute("sequence.throughEdits", json!({})).unwrap().as_array().unwrap().len(), 1);
    assert_eq!(s.execute("sequence.joinThroughEdits", json!({"cut": [left.0, right.0]})).unwrap()["joined"], json!(1));
    let after = s.project.clone();
    assert_eq!(remix_track_signal(&s), signal_equal);
    s.execute("edit.undo", json!({})).unwrap();
    assert_eq!(s.project, before_equal);
    s.execute("edit.redo", json!({})).unwrap();
    assert_eq!(s.project, after);
}

#[test]
fn join_through_edits_keeps_nested_stereo_mapping_functions_strict() {
    for inner_mix in ["Stereo", "Mono", "Adaptive"] {
        for explicit_left in [false, true] {
            let root = crate::media_test_util::tmp_dir("through-nested-persisted-mapping");
            let original = root.join("original.wav");
            let unjoined_file = root.join("unjoined.filmcraft");
            let joined_file = root.join("joined.filmcraft");
            std::fs::write(&original, filmcraft_media::wav::write_wav16(&[0.1, 0.2].repeat(48_000), 2, 48_000)).unwrap();
            let mut s = Session::default();
            let imported = s.execute("file.import", json!({"paths": [original.to_string_lossy()]})).unwrap();
            let item = ItemId(imported["items"][0].as_u64().unwrap());
            s.execute("file.newSequence", json!({"name": "Inner", "video": 0, "audio": 1, "fps": 24.0, "mix": inner_mix, "trackType": "Standard"})).unwrap();
            let inner = s.state.active_sequence.unwrap();
            s.execute("timeline.place", json!({"item": item.0, "seconds": 0.0})).unwrap();
            s.execute("file.newSequence", json!({"name": "Outer", "video": 0, "audio": 1, "fps": 24.0, "mix": "5.1", "trackType": "5.1"})).unwrap();
            let outer = s.state.active_sequence.unwrap();
            s.execute("timeline.place", json!({"item": inner.0, "seconds": 0.0})).unwrap();
            s.execute("playhead.set", json!({"frame": 12})).unwrap();
            s.execute("sequence.addEditAllTracks", json!({})).unwrap();
            let track = s.active_sequence().unwrap().audio_tracks[0].clone();
            let (left, right) = (track.items[0].id, track.items[1].id);
            s.execute("timeline.select", json!({"clips": [if explicit_left { left.0 } else { right.0 }]})).unwrap();
            s.execute("clip.audioChannels", json!({"channels": [0, 1]})).unwrap();
            let signal_before = remix_track_signal(&s);
            assert_persisted_through_signal(&signal_before, false, 2, false);
            s.execute("file.save", json!({"path": unjoined_file.to_string_lossy()})).unwrap();
            let before = s.project.clone();
            let undo = s.history.undo.len();
            let redo = s.history.redo.len();
            assert!(s.execute("sequence.throughEdits", json!({})).unwrap().as_array().unwrap().is_empty());
            assert!(s.execute("sequence.joinThroughEdits", json!({"cut": [left.0, right.0]})).is_err());
            assert_eq!(remix_track_signal(&s), signal_before);
            assert_eq!(s.project, before, "a currently stereo nest still has a mutable channel domain");
            assert_eq!(s.history.undo.len(), undo);
            assert_eq!(s.history.redo.len(), redo);
            s.execute("file.save", json!({"path": joined_file.to_string_lossy()})).unwrap();
            drop(s);
            std::fs::write(&original, filmcraft_media::wav::write_wav16(&[0.1, 0.2, 0.3, 0.4, 0.5, 0.6].repeat(48_000), 6, 48_000)).unwrap();
            let (mut unjoined, _) = reopen_through_signal(&unjoined_file, item, false, 6);
            let (mut joined, _) = reopen_through_signal(&joined_file, item, false, 6);
            unjoined.execute("sequence.open", json!({"item": outer.0})).unwrap();
            joined.execute("sequence.open", json!({"item": outer.0})).unwrap();
            let expected = remix_track_signal(&unjoined);
            let actual = remix_track_signal(&joined);
            let k = std::f32::consts::FRAC_1_SQRT_2;
            for (samples, sample) in expected.channels.iter().zip([0.1 + k * (0.3 + 0.5), 0.2 + k * (0.3 + 0.6), 0.0, 0.0, 0.0, 0.0]) {
                assert_eq!(samples.len(), 32);
                assert!(samples.iter().all(|&value| (value - sample).abs() < 1e-4), "nested {inner_mix} expected {sample}, got {samples:?}");
            }
            assert_eq!(expected.channel_count(), 6);
            assert_eq!(actual, expected, "rejecting unequal nested maps preserves playback through original-file width changes");
            drop((unjoined, joined));
            std::fs::remove_dir_all(root).unwrap();
        }
    }
}

fn persisted_through_nested_case(inner_mix: &str, mode: &str, maps: &str, explicit_left: bool) {
    let root = crate::media_test_util::tmp_dir("through-nested-master-mapping");
    let original = root.join("original.wav");
    let unjoined_file = root.join("unjoined.filmcraft");
    let attempted_file = root.join("attempted.filmcraft");
    std::fs::write(&original, filmcraft_media::wav::write_wav16(&[0.1, 0.2, 0.3, 0.4, 0.5, 0.6].repeat(48_000), 6, 48_000)).unwrap();
    let mut s = Session::default();
    let imported = s.execute("file.import", json!({"paths": [original.to_string_lossy()]})).unwrap();
    let item = ItemId(imported["items"][0].as_u64().unwrap());
    // A stereo master initially folds this 5.1 track down; changing only that master can expose
    // its centre/rear channels later, without changing the nested item or source file.
    s.execute("file.newSequence", json!({"name": "Inner", "video": 0, "audio": 1, "fps": 24.0, "mix": inner_mix, "trackType": "5.1"})).unwrap();
    let inner = s.state.active_sequence.unwrap();
    s.execute("timeline.place", json!({"item": item.0, "seconds": 0.0})).unwrap();
    s.execute("file.newSequence", json!({"name": "Outer", "video": 0, "audio": 1, "fps": 24.0, "mix": "5.1", "trackType": "5.1"})).unwrap();
    let outer = s.state.active_sequence.unwrap();
    s.execute("timeline.place", json!({"item": inner.0, "seconds": 0.0})).unwrap();
    s.execute("playhead.set", json!({"frame": 12})).unwrap();
    s.execute("sequence.addEditAllTracks", json!({})).unwrap();
    let track = s.active_sequence().unwrap().audio_tracks[0].clone();
    let (left, right) = (track.items[0].id, track.items[1].id);
    let picked = match maps {
        "default" => Vec::new(),
        "explicit" => vec![left.0, right.0],
        _ => vec![if explicit_left { left.0 } else { right.0 }],
    };
    if !picked.is_empty() {
        s.execute("timeline.select", json!({"clips": picked})).unwrap();
        s.execute("clip.audioChannels", json!({"channels": [0, 1]})).unwrap();
    }
    let source = s.source(item).unwrap();
    let decoded = source.audio_stream(0, 36_000, 32, 48_000).unwrap();
    assert_persisted_through_signal(&decoded, false, 6, false);
    let current = remix_track_signal(&s);
    let k = std::f32::consts::FRAC_1_SQRT_2;
    for (samples, sample) in current.channels.iter().zip([0.1 + k * (0.3 + 0.5), 0.2 + k * (0.3 + 0.6), 0.0, 0.0, 0.0, 0.0]) {
        assert_eq!(samples.len(), 32);
        assert!(samples.iter().all(|&value| (value - sample).abs() < 1e-4), "nested {inner_mix} expected {sample}, got {samples:?}");
    }
    assert_eq!(current.channel_count(), 6);
    s.execute("file.save", json!({"path": unjoined_file.to_string_lossy()})).unwrap();
    let before = s.project.clone();
    let undo = s.history.undo.len();
    let redo = s.history.redo.len();
    let opened = s.media.open_sources();
    let query = s.execute("sequence.throughEdits", json!({})).unwrap();
    assert_eq!(s.media.open_sources(), opened, "nested eligibility must not open or decode media");
    let params = match mode {
        "all" => json!({"all": true}),
        "clips" => json!({"clips": [left.0, right.0]}),
        _ => json!({"cut": [left.0, right.0]}),
    };
    let joined = s.execute("sequence.joinThroughEdits", params);
    let after = s.project.clone();
    let undo_after = s.history.undo.len();
    let redo_after = s.history.redo.len();
    assert_eq!(remix_track_signal(&s), current, "the original stereo master can make unequal mapping functions sound equal");
    if joined.is_ok() {
        s.execute("edit.undo", json!({})).unwrap();
        assert_eq!(s.project, before);
        s.execute("edit.redo", json!({})).unwrap();
        assert_eq!(s.project, after);
    }
    s.execute("file.save", json!({"path": attempted_file.to_string_lossy()})).unwrap();
    drop(source);
    drop(s);
    for path in [&unjoined_file, &attempted_file] {
        let mut reopened = Session::default();
        reopened.execute("file.open", json!({"path": path.to_string_lossy()})).unwrap();
        reopened.execute("sequence.open", json!({"item": inner.0})).unwrap();
        assert_ne!(reopened.active_sequence().unwrap().settings.audio_master, filmcraft_project::AudioChannels::Surround51);
        let previous = reopened.project.clone();
        reopened.execute("sequence.settings", json!({"mix": "5.1"})).unwrap();
        assert_eq!(reopened.active_sequence().unwrap().settings.audio_master, filmcraft_project::AudioChannels::Surround51);
        let changed = reopened.project.clone();
        reopened.execute("edit.undo", json!({})).unwrap();
        assert_eq!(reopened.project, previous, "the public master-layout change is one undo step");
        reopened.execute("edit.redo", json!({})).unwrap();
        assert_eq!(reopened.project, changed);
        reopened.execute("sequence.open", json!({"item": outer.0})).unwrap();
        reopened.execute("file.save", json!({"path": path.to_string_lossy()})).unwrap();
    }
    let mut unjoined = Session::default();
    unjoined.execute("file.open", json!({"path": unjoined_file.to_string_lossy()})).unwrap();
    let mut attempted = Session::default();
    attempted.execute("file.open", json!({"path": attempted_file.to_string_lossy()})).unwrap();
    for reopened in [&mut unjoined, &mut attempted] {
        assert_eq!(reopened.project.sequence(inner).unwrap().settings.audio_master, filmcraft_project::AudioChannels::Surround51);
        reopened.execute("sequence.open", json!({"item": outer.0})).unwrap();
        let source = reopened.source(item).unwrap();
        assert_eq!(source.info().audio_streams[0].channels, 6);
        assert_persisted_through_signal(&source.audio_stream(0, 36_000, 32, 48_000).unwrap(), false, 6, false);
    }
    let expected = remix_track_signal(&unjoined);
    let actual = remix_track_signal(&attempted);
    let right_explicit = maps == "explicit" || (maps == "mixed" && !explicit_left);
    assert_persisted_through_signal(&expected, false, 6, right_explicit);
    assert!(actual.channels.iter().all(|samples| samples.len() == 32));
    assert!(actual.channels[0].iter().all(|&sample| sample > 0.09));
    assert!(actual.channels[1].iter().all(|&sample| sample > 0.19));
    eprintln!(
        "persisted nested mix={inner_mix} mode={mode} maps={maps} explicit_left={explicit_left}: {joined:?}; cached={:?}; unjoined={:?}; attempted={:?}",
        current.channels, expected.channels, actual.channels
    );
    assert_eq!(actual, expected, "joining must preserve the selected-channel function after save/reopen and a later nested-master layout change");
    if maps == "mixed" {
        assert!(joined.is_err(), "nested master layouts are mutable, so channel maps must match exactly: {joined:?}");
        assert!(query.as_array().unwrap().is_empty());
        assert_eq!(after, before);
        assert_eq!(undo_after, undo);
        assert_eq!(redo_after, redo);
        assert_eq!(attempted.active_sequence().unwrap().audio_tracks[0].items.len(), 2);
    } else {
        assert_eq!(joined.unwrap()["joined"], json!(1));
        assert_eq!(query.as_array().unwrap().len(), 1);
        assert_eq!(attempted.active_sequence().unwrap().audio_tracks[0].items.len(), 1);
    }
    drop((unjoined, attempted));
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn join_through_edits_preserves_nested_mapping_after_save_and_master_change() {
    for inner_mix in ["Stereo", "Mono", "Adaptive"] {
        for explicit_left in [false, true] {
            for mode in ["all", "clips", "cut"] {
                persisted_through_nested_case(inner_mix, mode, "mixed", explicit_left);
            }
        }
    }
}

#[test]
fn join_through_edits_keeps_identical_nested_maps_after_save_and_master_change() {
    for inner_mix in ["Stereo", "Mono", "Adaptive"] {
        for maps in ["default", "explicit"] {
            for mode in ["all", "clips", "cut"] {
                persisted_through_nested_case(inner_mix, mode, maps, false);
            }
        }
    }
}

fn persisted_through_generator_proxy_case(stereo: bool, mode: &str, maps: &str, explicit_left: bool) {
    let root = crate::media_test_util::tmp_dir("through-generator-future-proxy");
    let proxy_file = root.join("six.wav");
    let unjoined_file = root.join("unjoined.filmcraft");
    let attempted_file = root.join("attempted.filmcraft");
    std::fs::write(&proxy_file, filmcraft_media::wav::write_wav16(&[0.1, 0.2, 0.3, 0.4, 0.5, 0.6].repeat(48_000), 6, 48_000)).unwrap();
    let mut s = Session::default();
    // Public Attach Proxies is enabled when the project contains file media; force may then
    // target a generated item. Keep the imported file and both persisted controls alive.
    s.execute("file.import", json!({"paths": [proxy_file.to_string_lossy()]})).unwrap();
    s.execute(
        "file.newSequence",
        json!({"video": 0, "audio": 1, "fps": 24.0, "mix": if stereo { "Stereo" } else { "5.1" }, "trackType": if stereo { "Standard" } else { "5.1" }}),
    )
    .unwrap();
    let item = ItemId(s.execute("file.newBarsAndTone", json!({"seconds": 1.0})).unwrap()["item"].as_u64().unwrap());
    s.execute("timeline.place", json!({"item": item.0, "seconds": 0.0, "video": false})).unwrap();
    s.execute("playhead.set", json!({"frame": 12})).unwrap();
    s.execute("sequence.addEditAllTracks", json!({})).unwrap();
    let track = s.active_sequence().unwrap().audio_tracks[0].clone();
    let (left, right) = (track.items[0].id, track.items[1].id);
    let picked = match maps {
        "default" => Vec::new(),
        "explicit" => vec![left.0, right.0],
        _ => vec![if explicit_left { left.0 } else { right.0 }],
    };
    if !picked.is_empty() {
        s.execute("timeline.select", json!({"clips": picked})).unwrap();
        s.execute("clip.audioChannels", json!({"channels": [0, 1]})).unwrap();
    }
    let source = s.source(item).unwrap();
    assert_eq!(source.info().audio_streams[0].channels, 2);
    let decoded = source.audio_stream(0, 36_000, 32, 48_000).unwrap();
    assert_eq!(decoded.channel_count(), 2);
    assert!(decoded.channels.iter().all(|samples| samples.len() == 32 && samples.iter().any(|&sample| sample.abs() > 0.01)));
    let current = remix_track_signal(&s);
    assert_eq!(current.channel_count(), if stereo { 2 } else { 6 });
    assert!(current.channels[..2].iter().all(|samples| samples.len() == 32 && samples.iter().any(|&sample| sample.abs() > 0.01)));
    assert!(current.channels[2..].iter().all(|samples| samples.len() == 32 && samples.iter().all(|&sample| sample == 0.0)));
    s.execute("file.save", json!({"path": unjoined_file.to_string_lossy()})).unwrap();
    let before = s.project.clone();
    let undo = s.history.undo.len();
    let redo = s.history.redo.len();
    let opened = s.media.open_sources();
    let query = s.execute("sequence.throughEdits", json!({})).unwrap();
    assert_eq!(s.media.open_sources(), opened, "eligibility must not open or decode media");
    let params = match mode {
        "all" => json!({"all": true}),
        "clips" => json!({"clips": [left.0, right.0]}),
        _ => json!({"cut": [left.0, right.0]}),
    };
    let joined = s.execute("sequence.joinThroughEdits", params);
    let after = s.project.clone();
    let undo_after = s.history.undo.len();
    let redo_after = s.history.redo.len();
    assert_eq!(remix_track_signal(&s), current, "the native generator is stereo before later proxy attachment");
    if joined.is_ok() {
        s.execute("edit.undo", json!({})).unwrap();
        assert_eq!(s.project, before);
        s.execute("edit.redo", json!({})).unwrap();
        assert_eq!(s.project, after);
    }
    s.execute("file.save", json!({"path": attempted_file.to_string_lossy()})).unwrap();
    drop(source);
    drop(s);
    for path in [&unjoined_file, &attempted_file] {
        let mut reopened = Session::default();
        reopened.execute("file.open", json!({"path": path.to_string_lossy()})).unwrap();
        assert!(reopened.project.item(item).unwrap().as_media().unwrap().proxy.is_none());
        let attach = reopened.execute("media.attachProxies", json!({"item": item.0, "path": proxy_file.to_string_lossy(), "force": true})).unwrap();
        assert_eq!(attach["attached"][0]["item"], json!(item.0));
        assert!(matches!(reopened.project.item(item).unwrap().as_media().unwrap().media, filmcraft_project::MediaRef::Generator(_)));
        reopened.execute("media.toggleProxies", json!({"enabled": true})).unwrap();
        let selected = reopened.source(item).unwrap();
        assert_eq!(selected.info().audio_streams[0].channels, 2, "a proxy wrapper still reports the original generator's stereo metadata");
        assert_persisted_through_signal(&selected.audio_stream(0, 36_000, 32, 48_000).unwrap(), false, 6, false);
        reopened.execute("file.save", json!({"path": path.to_string_lossy()})).unwrap();
    }
    let mut unjoined = Session::default();
    unjoined.execute("file.open", json!({"path": unjoined_file.to_string_lossy()})).unwrap();
    let mut attempted = Session::default();
    attempted.execute("file.open", json!({"path": attempted_file.to_string_lossy()})).unwrap();
    for reopened in [&mut unjoined, &mut attempted] {
        reopened.execute("media.toggleProxies", json!({"enabled": true})).unwrap();
        let selected = reopened.source(item).unwrap();
        assert_eq!(selected.info().audio_streams[0].channels, 2);
        assert_persisted_through_signal(&selected.audio_stream(0, 36_000, 32, 48_000).unwrap(), false, 6, false);
    }
    let expected = remix_track_signal(&unjoined);
    let actual = remix_track_signal(&attempted);
    let right_explicit = maps == "explicit" || (maps == "mixed" && !explicit_left);
    assert_persisted_through_signal(&expected, stereo, 6, right_explicit);
    assert!(actual.channels.iter().all(|samples| samples.len() == 32));
    assert!(actual.channels[..2].iter().all(|samples| samples.iter().any(|&sample| sample.abs() > 0.09)));
    eprintln!(
        "persisted generator later-proxy stereo={stereo} mode={mode} maps={maps} explicit_left={explicit_left}: {joined:?}; unjoined={:?}; attempted={:?}",
        expected.channels, actual.channels
    );
    assert_eq!(actual, expected, "joining must preserve generator channel selection after later forced proxy attachment and save/reopen");
    if maps == "mixed" {
        assert!(joined.is_err(), "future proxies can change a generator's rendered domain, so maps must match exactly: {joined:?}");
        assert!(query.as_array().unwrap().is_empty());
        assert_eq!(after, before);
        assert_eq!(undo_after, undo);
        assert_eq!(redo_after, redo);
        assert_eq!(attempted.active_sequence().unwrap().audio_tracks[0].items.len(), 2);
    } else {
        assert_eq!(joined.unwrap()["joined"], json!(1));
        assert_eq!(query.as_array().unwrap().len(), 1);
        assert_eq!(attempted.active_sequence().unwrap().audio_tracks[0].items.len(), 1);
    }
    drop((unjoined, attempted));
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn join_through_edits_preserves_generator_mapping_after_later_proxy_and_reopen() {
    for stereo in [false, true] {
        for explicit_left in [false, true] {
            for mode in ["all", "clips", "cut"] {
                persisted_through_generator_proxy_case(stereo, mode, "mixed", explicit_left);
            }
        }
    }
}

#[test]
fn join_through_edits_keeps_identical_generator_maps_after_later_proxy_and_reopen() {
    for stereo in [false, true] {
        for maps in ["default", "explicit"] {
            for mode in ["all", "clips", "cut"] {
                persisted_through_generator_proxy_case(stereo, mode, maps, false);
            }
        }
    }
}

fn cached_width_case(kind: &str) {
    let mut s = demo();
    s.execute("playhead.set", json!({"frame": 30})).unwrap();
    s.execute("sequence.addEditAllTracks", json!({})).unwrap();
    let track = s.active_sequence().unwrap().audio_tracks[0].clone();
    let item = track.items[0].item;
    let (left, right) = (track.items[0].id, track.items[1].id);
    s.execute("timeline.select", json!({"clips": [right.0]})).unwrap();
    s.execute("clip.audioChannels", json!({"channels": [0, 1]})).unwrap();
    s.media.clear();
    match kind {
        "stale" => s.media.insert_keyed(item, "obsolete-reference".into(), audio_fixture(2)),
        "wildcard" => s.media.insert(item, audio_fixture(2)),
        _ => {}
    }
    let before = s.project.clone();
    let (undo, redo) = (s.history.undo.len(), s.history.redo.len());
    let opened = s.media.open_sources();
    let edits = s.execute("sequence.throughEdits", json!({})).unwrap();
    let eligible = edits.as_array().unwrap().iter().any(|e| e["left"] == json!(left.0) && e["right"] == json!(right.0));
    assert_eq!(s.media.open_sources(), opened, "eligibility must not open or decode a source");
    assert!(!eligible, "a current cached width cannot prove future selection equivalence: {edits}");
    assert!(s.execute("sequence.joinThroughEdits", json!({"cut": [left.0, right.0]})).is_err());
    assert_eq!(s.project, before);
    assert_eq!((s.history.undo.len(), s.history.redo.len()), (undo, redo));
    assert_eq!(s.media.open_sources(), opened);
    let source = s.source(item).unwrap();
    assert_eq!(source.info().audio_streams[0].channels, 2);
    let signal = source.audio_stream(0, 72_000, 32, 48_000).unwrap();
    assert_eq!(signal.channel_count(), 2);
    assert!(signal.channels.iter().all(|samples| samples.len() == 32 && samples.iter().any(|sample| sample.abs() > 0.0001)));
    let edits = s.execute("sequence.throughEdits", json!({})).unwrap();
    assert!(!edits.as_array().unwrap().iter().any(|e| e["left"] == json!(left.0) && e["right"] == json!(right.0)));
    assert!(s.execute("sequence.joinThroughEdits", json!({"cut": [left.0, right.0]})).is_err());
    assert_eq!(s.project, before);
    assert_eq!((s.history.undo.len(), s.history.redo.len()), (undo, redo));

    s.execute("timeline.select", json!({"clips": [left.0]})).unwrap();
    s.execute("clip.audioChannels", json!({"channels": [0, 1]})).unwrap();
    let before_equal = s.project.clone();
    let edits = s.execute("sequence.throughEdits", json!({})).unwrap();
    assert!(edits.as_array().unwrap().iter().any(|e| e["left"] == json!(left.0) && e["right"] == json!(right.0)));
    assert_eq!(s.execute("sequence.joinThroughEdits", json!({"cut": [left.0, right.0]})).unwrap()["joined"], json!(2), "V1 and its linked A1");
    let after = s.project.clone();
    assert!(s.active_sequence().unwrap().find_item(right).is_none());
    s.execute("edit.undo", json!({})).unwrap();
    assert_eq!(s.project, before_equal);
    s.execute("edit.redo", json!({})).unwrap();
    assert_eq!(s.project, after);
}

#[test]
fn join_through_edits_keeps_uncached_channel_defaults_strict() {
    cached_width_case("missing");
}

#[test]
fn join_through_edits_rejects_stale_cached_channel_width() {
    cached_width_case("stale");
}

#[test]
fn join_through_edits_keeps_reference_independent_cached_generator_maps_strict() {
    cached_width_case("wildcard");
}

#[test]
fn join_through_edits_keeps_cached_file_mapping_functions_strict() {
    let root = crate::media_test_util::tmp_dir("through-cached-replacement");
    let original = root.join("original.wav");
    std::fs::write(&original, filmcraft_media::wav::write_wav16(&[0.1, 0.2].repeat(48_000), 2, 48_000)).unwrap();
    let mut s = Session::default();
    let imported = s.execute("file.import", json!({"paths": [original.to_string_lossy()]})).unwrap();
    let item = filmcraft_project::ItemId(imported["items"][0].as_u64().unwrap());
    s.execute("file.newSequence", json!({"video": 0, "audio": 1, "fps": 24.0, "mix": "5.1", "trackType": "5.1"})).unwrap();
    s.execute("timeline.place", json!({"item": item.0, "seconds": 0.0})).unwrap();
    s.execute("playhead.set", json!({"frame": 12})).unwrap();
    s.execute("sequence.addEditAllTracks", json!({})).unwrap();
    let track = s.active_sequence().unwrap().audio_tracks[0].clone();
    let (left, right) = (track.items[0].id, track.items[1].id);
    s.execute("timeline.select", json!({"clips": [right.0]})).unwrap();
    s.execute("clip.audioChannels", json!({"channels": [0, 1]})).unwrap();
    let source = s.source(item).unwrap();
    assert_eq!(source.info().audio_streams[0].channels, 2);
    std::fs::write(&original, filmcraft_media::wav::write_wav16(&[0.1, 0.2, 0.3, 0.4, 0.5, 0.6].repeat(48_000), 6, 48_000)).unwrap();
    assert_eq!(source.info().audio_streams[0].channels, 2);
    assert_eq!(source.audio_stream(0, 36_000, 32, 48_000).unwrap().channel_count(), 2, "the opened decoder retains its audio layout");
    let before = s.project.clone();
    let seq = s.active_sequence().unwrap();
    let provider = s.media.provider(before.clone(), s.services.clone());
    let signal_before = filmcraft_render::audio::track_input(&before, &seq.audio_tracks[0], 36_000, 32, 48_000, &provider);
    assert!(signal_before.channels[0].iter().all(|&sample| sample > 0.09));
    assert!(signal_before.channels[2].iter().all(|&sample| sample == 0.0));
    let undo = s.history.undo.len();
    let redo = s.history.redo.len();
    let edits = s.execute("sequence.throughEdits", json!({})).unwrap();
    assert!(edits.as_array().unwrap().is_empty());
    assert!(s.execute("sequence.joinThroughEdits", json!({"cut": [left.0, right.0]})).is_err());
    let after = s.project.clone();
    let seq = s.active_sequence().unwrap();
    let provider = s.media.provider(after.clone(), s.services.clone());
    let signal_after = filmcraft_render::audio::track_input(&after, &seq.audio_tracks[0], 36_000, 32, 48_000, &provider);
    assert_eq!(signal_after, signal_before);
    assert_eq!(s.project, before);
    assert_eq!(s.history.undo.len(), undo);
    assert_eq!(s.history.redo.len(), redo);
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn join_through_edits_preserves_multichannel_defaults() {
    let mut s = demo();
    s.execute("playhead.set", json!({"frame": 30})).unwrap();
    s.execute("sequence.addEditAllTracks", json!({})).unwrap();
    let track = s.active_sequence().unwrap().audio_tracks[0].clone();
    let (left, right) = (track.items[0].id, track.items[1].id);
    let mut project = (*s.project).clone();
    project.item_mut(track.items[0].item).unwrap().as_media_mut().unwrap().info.audio_streams[0].channels = 6;
    project.sequence_mut(s.state.active_sequence.unwrap()).unwrap().audio_tracks[0].channels = filmcraft_project::AudioChannels::Surround51;
    s.project = std::sync::Arc::new(project);
    let (item, media, _) = s.project.resolve_media(track.items[0].item).unwrap();
    s.media.insert_keyed(item, crate::media_pool::media_key(media), audio_fixture(6));
    let source = s.source(item).unwrap();
    assert_eq!(source.info().audio_streams[0].channels, 6);
    assert_eq!(source.audio_stream(0, 72_000, 32, 48_000).unwrap().channel_count(), 6);
    s.execute("timeline.select", json!({"clips": [right.0]})).unwrap();
    s.execute("clip.audioChannels", json!({"channels": [0, 1]})).unwrap();
    let before = s.project.clone();
    let q = s.active_sequence().unwrap();
    let (a, b) = (q.find_item(left).unwrap().1, q.find_item(right).unwrap().1);
    assert_eq!(filmcraft_render::audio::source_51(a, 6), vec![0, 1, 2, 3, 4, 5]);
    assert_eq!(filmcraft_render::audio::source_51(b, 6), vec![0, 1]);
    let undo = s.history.undo.len();
    let edits = s.execute("sequence.throughEdits", json!({})).unwrap();
    assert!(!edits.as_array().unwrap().iter().any(|e| e["left"] == json!(left.0) && e["right"] == json!(right.0)));
    assert!(s.execute("sequence.joinThroughEdits", json!({"cut": [left.0, right.0]})).is_err());
    assert_eq!(s.project, before);
    assert_eq!(s.history.undo.len(), undo);
    s.execute("sequence.joinThroughEdits", json!({"all": true})).unwrap();
    assert_preserved_audio_cut(before.sequence(s.state.active_sequence.unwrap()).unwrap(), s.active_sequence().unwrap(), left, right, true);
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

#[test]
fn join_through_edits_rejects_cached_mono_width_for_a_stereo_generator() {
    let mut s = demo();
    s.execute("playhead.set", json!({"frame": 30})).unwrap();
    s.execute("sequence.addEditAllTracks", json!({})).unwrap();
    let track = s.active_sequence().unwrap().audio_tracks[0].clone();
    let (left, right) = (track.items[0].id, track.items[1].id);
    let mut project = (*s.project).clone();
    project.item_mut(track.items[0].item).unwrap().as_media_mut().unwrap().info.audio_streams[0].channels = 1;
    s.project = std::sync::Arc::new(project);
    let (item, media, _) = s.project.resolve_media(track.items[0].item).unwrap();
    s.media.insert_keyed(item, crate::media_pool::media_key(media), audio_fixture(1));
    let source = s.source(item).unwrap();
    assert_eq!(source.info().audio_streams[0].channels, 1);
    assert_eq!(source.audio_stream(0, 72_000, 32, 48_000).unwrap().channel_count(), 1);
    s.execute("timeline.select", json!({"clips": [right.0]})).unwrap();
    s.execute("clip.audioChannels", json!({"channels": [0]})).unwrap();
    let before = s.active_sequence().unwrap().clone();
    let (a, b) = (before.find_item(left).unwrap().1, before.find_item(right).unwrap().1);
    assert_eq!(filmcraft_render::audio::source_pair(a, 1), filmcraft_render::audio::source_pair(b, 1));
    assert_eq!(filmcraft_render::audio::source_51(a, 1), filmcraft_render::audio::source_51(b, 1));
    let project_before = s.project.clone();
    let undo = s.history.undo.len();
    let redo = s.history.redo.len();
    let edits = s.execute("sequence.throughEdits", json!({})).unwrap();
    assert!(!edits.as_array().unwrap().iter().any(|e| e["left"] == json!(left.0) && e["right"] == json!(right.0)));
    assert!(s.execute("sequence.joinThroughEdits", json!({"cut": [left.0, right.0]})).is_err());
    assert_eq!(s.project, project_before);
    assert_eq!(s.active_sequence().unwrap(), &before);
    assert_eq!(s.history.undo.len(), undo);
    assert_eq!(s.history.redo.len(), redo);
    // The persisted generator domain is stereo even when a substituted cached source is mono.
    s.media.clear();
    let fresh = s.source(item).unwrap();
    assert_eq!(fresh.info().audio_streams[0].channels, 2);
    let signal = fresh.audio_stream(0, 72_000, 32, 48_000).unwrap();
    assert_eq!(signal.channel_count(), 2);
    assert!(signal.channels.iter().all(|c| c.len() == 32));
    assert!(signal.channels.iter().flatten().any(|&sample| sample.abs() > 0.0001));
    assert_ne!(filmcraft_render::audio::source_pair(a, 2), filmcraft_render::audio::source_pair(b, 2));
    assert!(s.execute("sequence.joinThroughEdits", json!({"cut": [left.0, right.0]})).is_err());
    assert_eq!(s.project, project_before);
    assert_eq!(s.history.undo.len(), undo);
    assert_eq!(s.history.redo.len(), redo);
}

#[test]
fn join_through_edits_keeps_unknown_channel_defaults_strict() {
    let mut s = demo();
    s.execute("playhead.set", json!({"frame": 30})).unwrap();
    s.execute("sequence.addEditAllTracks", json!({})).unwrap();
    let track = s.active_sequence().unwrap().audio_tracks[0].clone();
    let (left, right) = (track.items[0].id, track.items[1].id);
    s.execute("timeline.select", json!({"clips": [right.0]})).unwrap();
    s.execute("clip.audioChannels", json!({"channels": [0, 1]})).unwrap();
    let mut project = (*s.project).clone();
    project.item_mut(track.items[0].item).unwrap().as_media_mut().unwrap().info.audio_streams.clear();
    s.project = std::sync::Arc::new(project);
    s.media.clear();
    let before = s.project.clone();
    let undo = s.history.undo.len();
    let edits = s.execute("sequence.throughEdits", json!({})).unwrap();
    assert!(!edits.as_array().unwrap().iter().any(|e| e["left"] == json!(left.0) && e["right"] == json!(right.0)));
    assert!(s.execute("sequence.joinThroughEdits", json!({"cut": [left.0, right.0]})).is_err());
    assert_eq!(s.project, before);
    assert_eq!(s.history.undo.len(), undo);
    s.execute("sequence.joinThroughEdits", json!({"all": true})).unwrap();
    assert_preserved_audio_cut(before.sequence(s.state.active_sequence.unwrap()).unwrap(), s.active_sequence().unwrap(), left, right, true);
}

#[test]
fn join_through_edits_preserves_changed_audio_channels() {
    let mut s = demo();
    s.execute("playhead.set", json!({"frame": 30})).unwrap();
    s.execute("sequence.addEditAllTracks", json!({})).unwrap();
    let track = s.active_sequence().unwrap().audio_tracks[0].clone();
    let (left, right) = (track.items[0].id, track.items[1].id);
    s.execute("timeline.select", json!({"clips": [left.0]})).unwrap();
    s.execute("clip.audioChannels", json!({"channels": [0]})).unwrap();
    s.execute("timeline.select", json!({"clips": [right.0]})).unwrap();
    s.execute("clip.audioChannels", json!({"channels": [1]})).unwrap();
    let before = s.active_sequence().unwrap().clone();
    s.execute("sequence.joinThroughEdits", json!({"all": true})).unwrap();
    let after = s.active_sequence().unwrap().clone();
    assert_preserved_audio_cut(&before, &after, left, right, true);
    assert!(after.video_tracks[0].items.len() < before.video_tracks[0].items.len(), "unchanged sources still join");
    s.execute("edit.undo", json!({})).unwrap();
    assert_eq!(s.active_sequence().unwrap(), &before);
    s.execute("edit.redo", json!({})).unwrap();
    assert_eq!(s.active_sequence().unwrap(), &after);
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

/// #920: Source Mark In/Out and Mark Split honour `frame` / `seconds` / `timecode`, not only `time`.
#[test]
fn source_mark_in_out_and_split_honour_frame_seconds_and_timecode() {
    let (mut s, bars) = fresh();
    s.execute("source.open", json!({"item": bars})).unwrap();
    s.execute("source.setPlayhead", json!({"frame": 0})).unwrap();
    let r = s.project.item(ItemId(bars)).unwrap().frame_rate();
    assert_eq!(s.state.source_playhead, Tick::ZERO);
    s.execute("markers.markIn", json!({"target": "source", "frame": 24})).unwrap();
    let m = s.project.item(ItemId(bars)).unwrap().as_media().unwrap();
    assert_eq!(m.mark_in, Some(r.tick_of(24)), "frame:24 marks source frame 24, not the playhead");
    s.execute("markers.markOut", json!({"target": "source", "seconds": 2.0})).unwrap();
    let m = s.project.item(ItemId(bars)).unwrap().as_media().unwrap();
    assert_eq!(m.mark_out, Some(Tick::from_seconds_f64(2.0)));
    s.execute("markers.markSplitVideoIn", json!({"target": "source", "timecode": "00:00:01:00"})).unwrap();
    assert_eq!(s.project.item(ItemId(bars)).unwrap().split.video_in, Some(r.tick_of(r.as_f64().round() as i64)));
    s.execute("markers.markSplitAudioOut", json!({"target": "source", "frame": 48})).unwrap();
    assert_eq!(s.project.item(ItemId(bars)).unwrap().split.audio_out, Some(r.tick_of(48)));
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
fn unknown_marker_colours_are_rejected_not_stored_as_green() {
    // #857: `markers.add {color:"Red"}` used to succeed and store Green
    let mut s = demo();
    let before = s.project.to_json();
    for cmd in ["markers.add", "markers.addRange", "markers.addChapter", "markers.addFlashCue"] {
        for color in [json!("Red"), json!("NotAColor"), json!(3)] {
            let err = s.execute(cmd, json!({"frame": 10, "color": color})).unwrap_err().to_string();
            assert!(err.contains("Rose"), "{cmd}: the error lists the accepted names: {err}");
            assert_eq!(s.project.to_json(), before, "{cmd} {color} must not add a marker");
        }
    }
    let id = s.execute("markers.add", json!({"frame": 10, "color": "rose"})).unwrap()["marker"].as_u64().unwrap();
    let colour = |s: &Session| s.active_sequence().unwrap().markers.iter().find(|m| m.id.0 == id).unwrap().color;
    assert_eq!(colour(&s), Label::Rose);
    assert!(s.execute("markers.edit", json!({"marker": id, "color": "Red"})).is_err());
    assert_eq!(colour(&s), Label::Rose, "a bad colour leaves the marker unchanged");
    s.execute("markers.edit", json!({"marker": id, "color": "Teal"})).unwrap();
    assert_eq!(colour(&s), Label::Teal);
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

/// #483: media dropped in the empty space past the last track goes on a new track (with
/// `"new"`), one undo step; a placement that fails leaves no new track behind.
#[test]
fn place_on_new_tracks_adds_them_in_one_undo_step() {
    let (mut s, item) = fresh();
    let before = s.active_sequence().unwrap().clone();
    let (nv, na) = (before.video_tracks.len(), before.audio_tracks.len());
    let undo = s.history.undo.len();
    let r = s.execute("timeline.place", json!({"item": item, "track": "new", "audioTrack": "new", "time": 0})).unwrap();
    let q = s.active_sequence().unwrap();
    assert_eq!((q.video_tracks.len(), q.audio_tracks.len()), (nv + 1, na + 1));
    let clips: Vec<ClipId> = r["clips"].as_array().unwrap().iter().map(|c| ClipId(c.as_u64().unwrap())).collect();
    assert_eq!(clips.len(), 2);
    assert_eq!(q.video_tracks[nv].items.iter().map(|i| i.id).collect::<Vec<_>>(), [clips[0]], "the picture is on the new top video track");
    assert_eq!(q.audio_tracks[na].items.iter().map(|i| i.id).collect::<Vec<_>>(), [clips[1]], "the sound is on the new last audio track");
    assert_eq!(s.history.undo.len(), undo + 1, "one undo step");
    s.execute("edit.undo", json!({})).unwrap();
    assert_eq!(*s.active_sequence().unwrap(), before);

    // only the picture gets a new track; the sound follows the given track
    s.execute("timeline.place", json!({"item": item, "track": "new", "audioTrack": "A1", "time": 0})).unwrap();
    let q = s.active_sequence().unwrap();
    assert_eq!((q.video_tracks.len(), q.audio_tracks.len()), (nv + 1, na));
    assert_eq!((q.video_tracks[nv].items.len(), q.audio_tracks[0].items.len()), (1, 1));
    s.execute("edit.undo", json!({})).unwrap();

    // a placement that fails after the tracks were added: nothing changes
    assert!(s.execute("timeline.place", json!({"item": item, "track": "new", "audioTrack": "A99", "time": 0})).is_err());
    assert_eq!(*s.active_sequence().unwrap(), before);
    assert_eq!(s.history.undo.len(), undo);
}

/// #1104: a clip dragged into the empty space past the last track moves onto a new track (with
/// `"new"`), one undo step; a move that fails leaves no new track behind.
#[test]
fn move_to_new_track_adds_it_in_one_undo_step() {
    let (mut s, item) = fresh();
    let clips = place(&mut s, item, "V1", 0, 0, 10);
    let before = s.active_sequence().unwrap().clone();
    let nv = before.video_tracks.len();
    let undo = s.history.undo.len();
    s.execute("timeline.move", json!({"moves": [{"clip": clips[0], "track": "new", "time": 0}], "linked": false})).unwrap();
    let q = s.active_sequence().unwrap();
    assert_eq!(q.video_tracks.len(), nv + 1);
    assert_eq!(q.video_tracks[nv].items.iter().map(|i| i.id.0).collect::<Vec<_>>(), [clips[0]], "the clip is on the new top video track");
    assert!(q.video_tracks[0].items.is_empty());
    assert_eq!(s.history.undo.len(), undo + 1, "one undo step");
    s.execute("edit.undo", json!({})).unwrap();
    assert_eq!(*s.active_sequence().unwrap(), before);

    assert!(s.execute("timeline.move", json!({"moves": [{"clip": 999_999, "track": "new", "time": 0}]})).is_err());
    assert_eq!(*s.active_sequence().unwrap(), before);
    assert_eq!(s.history.undo.len(), undo);
}
