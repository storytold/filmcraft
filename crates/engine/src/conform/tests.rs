//! Tests of [`crate::conform`], from real take logs: a 24 fps sequence with three sections, a take
//! recorded with the synthetic input while the prompter is driven, then the report and the conform.

use std::sync::Arc;

use filmcraft_project::{ClipId, Marker};
use filmcraft_time::{FrameRate, Tick};
use serde_json::{Value, json};

use crate::{EngineError, Session};

const RATE: FrameRate = FrameRate::FPS_24;

fn sec(x: f64) -> Tick {
    RATE.tick_of((x * 24.0).round() as i64)
}

fn n(v: &Value) -> i64 {
    v.as_i64().unwrap_or_else(|| panic!("not a number: {v}"))
}

/// A recording folder of its own for each test.
fn dir(tag: &str) -> String {
    let d = std::env::temp_dir().join(format!("filmcraft-conform-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    d.to_string_lossy().into_owned()
}

/// The sections of the fixture: Intro 1–5 s, Middle 5–10 s, End 10–14 s, adjacent.
const SECTIONS: [(&str, f64, f64); 3] = [("Intro", 1.0, 5.0), ("Middle", 5.0, 10.0), ("End", 10.0, 14.0)];

/// A 24 fps sequence with picture and sound on V1/A1 in three clips (0–6, 6–14, 14–30 s, so the
/// first two sections end inside a clip and the last one on a cut), a second picture on V2 across
/// the middle, music on A2, an empty A3 for the take, the three sections and a point marker at
/// 20 s.
fn fixture() -> Session {
    let mut s = Session::default();
    s.execute("file.openDemoProject", json!({})).unwrap();
    s.execute("file.newSequence", json!({"name": "Conform", "fps": 24.0, "video": 2, "audio": 3})).unwrap();
    let item = s.project.items.values().filter(|i| i.has_video() && i.has_audio() && i.as_media().is_some()).map(|i| i.id).min().unwrap();
    for (at, len) in [(0.0, 6.0), (6.0, 8.0), (14.0, 9.0)] {
        s.execute("timeline.place", json!({"item": item.0, "track": "V1", "audioTrack": "A1", "seconds": at, "duration": sec(len).0})).unwrap();
    }
    s.execute("timeline.place", json!({"item": item.0, "track": "V2", "video": true, "audio": false, "seconds": 3.0, "duration": sec(9.0).0})).unwrap();
    s.execute("timeline.place", json!({"item": item.0, "track": "A2", "video": false, "audio": true, "seconds": 2.0, "duration": sec(10.0).0})).unwrap();
    for (name, a, b) in SECTIONS {
        s.execute("markers.addRange", json!({"time": sec(a).0, "durationFrames": ((b - a) * 24.0) as i64, "name": name})).unwrap();
    }
    s.execute("markers.add", json!({"time": sec(20.0).0, "name": "Later"})).unwrap();
    s
}

/// Record a take on A3 from `record` seconds: at each (seconds, command) move the playhead there and
/// run the prompter command, then stop at `stop` seconds. Returns the stop result.
fn take(s: &mut Session, record: f64, script: &[(f64, &str)], stop: f64, tag: &str) -> Value {
    s.execute("playhead.set", json!({"time": sec(record).0})).unwrap();
    s.execute("audio.voiceover.start", json!({"preroll": 0, "track": "A3"})).unwrap();
    for (at, id) in script {
        s.execute("playhead.set", json!({"time": sec(*at).0})).unwrap();
        s.execute(id, json!({})).unwrap();
    }
    let r = s.execute("audio.voiceover.stop", json!({"time": sec(stop).0, "dir": dir(tag)})).unwrap();
    assert_eq!(r["placed"], true, "{r}");
    r
}

fn report(s: &mut Session) -> Value {
    s.execute("sequence.conformSectionsReport", json!({})).unwrap()
}

fn row<'a>(r: &'a Value, name: &str) -> &'a Value {
    r["sections"].as_array().unwrap().iter().find(|x| x["name"] == name).unwrap_or_else(|| panic!("no row {name}: {r}"))
}

fn change(r: &Value, name: &str) -> String {
    row(r, name)["change"].as_str().unwrap().to_string()
}

fn marker(s: &Session, name: &str) -> Marker {
    s.active_sequence().unwrap().markers.iter().find(|m| m.name == name).cloned().unwrap()
}

fn disabled(s: &mut Session) -> String {
    match s.execute("sequence.conformSections", json!({})) {
        Err(EngineError::Disabled(_, why)) => why,
        other => panic!("expected the conform to be disabled, got {other:?}"),
    }
}

/// Intro left early by Next at 3 s (2 s delivered of 4), Middle prompted at half speed from 5 s to
/// 11 s (8 s of 5), End at the normal rate (4 s of 4).
const EARLY_THEN_SLOW: &[(f64, &str)] = &[
    (3.0, "prompter.next"),
    (5.0, "prompter.slower"),
    (5.0, "prompter.slower"),
    (5.0, "prompter.slower"),
    (5.0, "prompter.slower"),
    (5.0, "prompter.slower"),
    (5.0, "prompter.slower"),
    (5.0, "prompter.slower"),
    (5.0, "prompter.slower"),
    (5.0, "prompter.slower"),
    (5.0, "prompter.slower"),
    (11.0, "prompter.resetRate"),
];

#[test]
fn a_take_without_prompter_moves_changes_nothing() {
    let mut s = fixture();
    take(&mut s, 0.0, &[], 16.0, "still");
    let r = report(&mut s);
    for (name, a, b) in SECTIONS {
        let x = row(&r, name);
        assert_eq!(x["change"], "none", "{name}: {x}");
        assert_eq!(n(&x["deliveredLength"]), n(&x["plannedLength"]), "{name} was delivered at its picture length");
        assert_eq!((n(&x["enteredAt"]), n(&x["leftAt"])), (sec(a).0, sec(b).0), "while the picture played it");
    }
    let (project, undo) = (s.project.clone(), s.history.undo.len());
    assert!(disabled(&mut s).contains("nothing to change"));
    assert!(Arc::ptr_eq(&project, &s.project) && s.history.undo.len() == undo, "the conform is the identity: nothing is edited");
}

#[test]
fn each_conformed_section_then_plays_while_it_was_narrated() {
    let mut s = fixture();
    let stop = take(&mut s, 0.0, EARLY_THEN_SLOW, 16.0, "replay");
    let take_clip = ClipId(n(&stop["clip"]) as u64);
    let take_before = s.active_sequence().unwrap().find_item(take_clip).unwrap().1.clone();
    let r = report(&mut s);
    assert_eq!((change(&r, "Intro"), change(&r, "Middle"), change(&r, "End")), ("trim".into(), "extend".into(), "none".into()), "{r}");

    let done = s.execute("sequence.conformSections", json!({})).unwrap();
    let log = s.execute("prompter.takeLog", json!({})).unwrap()["last"].clone();
    let rs = Tick(n(&log["recordStart"]));
    for (name, ..) in SECTIONS {
        let x = row(&done, name);
        let m = marker(&s, name);
        let (entered, left) = (rs + Tick(n(&x["enteredAt"])), rs + Tick(n(&x["leftAt"])));
        // replaying the take: the picture of the section starts when the narrator started it and
        // ends when they left it, to the nearest frame
        assert_eq!(m.start, RATE.snap_nearest(entered), "{name} starts when it was narrated");
        assert_eq!(m.start + m.duration, RATE.snap_nearest(left), "{name} ends when the narrator left it");
        assert_eq!(n(&x["offset"]), 0);
    }
    assert_eq!(s.active_sequence().unwrap().find_item(take_clip).unwrap().1, &take_before, "the take is untouched");
    assert_eq!(n(&done["durationAfter"]) - n(&done["durationBefore"]), n(&done["totals"]["change"]), "the length changes by the sum of the changes");
    assert_eq!(n(&done["totals"]["change"]), sec(1.0).0, "2 s off the Intro, 3 s onto the Middle");
    assert_eq!(marker(&s, "Later").start, sec(21.0), "later markers move with the ripple");
}

#[test]
fn rows_report_entry_and_exit_from_the_log_and_the_run_does_what_the_report_said() {
    let mut s = fixture();
    take(&mut s, 0.0, EARLY_THEN_SLOW, 16.0, "agree");
    let (project, undo) = (s.project.clone(), s.history.undo.len());
    let planned = report(&mut s);
    assert!(Arc::ptr_eq(&project, &s.project) && s.history.undo.len() == undo, "the report changes nothing");
    assert_eq!(planned["applied"], false);
    let x = row(&planned, "Middle");
    assert_eq!((n(&x["enteredAt"]), n(&x["leftAt"])), (sec(3.0).0, sec(11.0).0), "entered by Next at 3 s, left when the slow prompt reached its end");
    assert_eq!(n(&x["by"]), sec(3.0).0);

    let done = s.execute("sequence.conformSections", json!({})).unwrap();
    assert_eq!(done["applied"], true);
    for k in ["sections", "totals", "durationAfter", "removedMarkers"] {
        assert_eq!(done[k], planned[k], "the run did what the report said: {k}");
    }
}

#[test]
fn a_section_returned_to_with_previous_lasts_until_it_is_left_for_the_last_time() {
    let mut s = fixture();
    // Next into the Middle at 3 s, straight back to the Intro's start at 3.5 s; the Intro then runs
    // to its end at 7.5 s
    take(&mut s, 0.0, &[(3.0, "prompter.next"), (3.5, "prompter.previous")], 16.0, "again");
    let r = report(&mut s);
    let intro = row(&r, "Intro");
    assert_eq!((n(&intro["enteredAt"]), n(&intro["leftAt"])), (sec(1.0).0, sec(7.5).0), "{intro}");
    assert_eq!(intro["change"], "extend", "the false start on the Middle belongs to the Intro");
    let middle = row(&r, "Middle");
    assert_eq!(n(&middle["enteredAt"]), n(&intro["leftAt"]), "the Middle starts when the Intro was finally left");
    assert_eq!(middle["change"], "none");
}

#[test]
fn a_skipped_section_is_left_as_it_is_and_reported() {
    let mut s = fixture();
    take(&mut s, 0.0, &[(3.0, "prompter.next"), (3.0, "prompter.next")], 16.0, "skip");
    let r = report(&mut s);
    let middle = row(&r, "Middle");
    assert_eq!(middle["change"], "notConformed");
    assert!(middle["reason"].as_str().unwrap().contains("skipped"), "{middle}");
    assert!(middle["deliveredLength"].is_null(), "no delivered length is claimed for it");
    assert_eq!(change(&r, "Intro"), "trim");
    assert!(n(&row(&r, "End")["offset"]) != 0, "the End's picture is off by the skipped Middle, and the row says by how much");
}

#[test]
fn only_sections_the_take_covers_whole_are_conformed() {
    let mut s = fixture();
    s.execute("markers.addRange", json!({"time": sec(16.0).0, "durationFrames": 48, "name": "After"})).unwrap();
    // starts inside the Intro, leaves the Middle early, ends inside the End
    take(&mut s, 2.0, &[(7.0, "prompter.next")], 10.5, "partial");
    let r = report(&mut s);
    let why = |name| row(&r, name)["reason"].as_str().unwrap_or("").to_string();
    assert_eq!(change(&r, "Intro"), "notConformed");
    assert!(why("Intro").contains("started inside"), "{}", why("Intro"));
    assert_eq!(change(&r, "Middle"), "trim", "the Middle was delivered whole, in 2 s of its 5");
    assert_eq!(change(&r, "End"), "notConformed");
    assert!(why("End").contains("ended inside"), "{}", why("End"));
    assert_eq!(change(&r, "After"), "outside");
    assert!(row(&r, "Intro")["deliveredLength"].is_null() && row(&r, "End")["deliveredLength"].is_null(), "a partly recorded section has no delivered length");
}

#[test]
fn overlapping_sections_the_take_touches_are_refused() {
    let mut s = fixture();
    s.execute("markers.addRange", json!({"time": sec(6.0).0, "durationFrames": 24, "name": "Nested"})).unwrap();
    take(&mut s, 0.0, EARLY_THEN_SLOW, 16.0, "nested");
    let e = s.execute("sequence.conformSectionsReport", json!({})).unwrap_err().to_string();
    assert!(e.contains("overlap") && e.contains("Nested"), "{e}");
    assert!(disabled(&mut s).contains("overlap"));
}

#[test]
fn the_picture_and_sound_around_each_change_hold_split_and_ripple() {
    let mut s = fixture();
    s.execute("captions.add", json!({"seconds": 9.0, "durationSeconds": 2.0, "text": "across the Middle's end"})).unwrap();
    s.execute("markers.add", json!({"time": sec(4.0).0, "name": "In the cut"})).unwrap();
    take(&mut s, 0.0, EARLY_THEN_SLOW, 16.0, "tracks");
    s.execute("sequence.conformSections", json!({})).unwrap();
    let q = s.active_sequence().unwrap();
    q.check().unwrap();
    let at = |track: usize, t: f64| q.video_tracks[track].item_at(sec(t)).cloned();
    // the Middle is now 3–11 s: its picture (5 s) is followed by its last frame held from 8 to 11 s,
    // on V1 and on V2
    for track in [0, 1] {
        let hold = at(track, 9.5).unwrap();
        assert!(hold.frame_hold.is_some() && hold.start == sec(8.0) && hold.end() == sec(11.0), "V{}: {hold:?}", track + 1);
    }
    assert!(at(0, 11.5).is_some_and(|c| c.frame_hold.is_none()), "the picture carries on after the hold");
    let a1 = &q.audio_tracks[0];
    assert!(a1.item_at(sec(9.5)).is_none(), "the sound under the hold is silence");
    assert!(a1.item_at(sec(11.5)).is_some(), "and the rest of it moved on");
    assert!(q.caption_tracks[0].caption_at(sec(9.5)).is_none(), "a caption across the end is split around the added time");
    assert!(q.markers.iter().all(|m| m.name != "In the cut"), "a point marker in the trimmed range goes, as in a ripple delete");
}

#[test]
fn a_section_ending_on_a_transition_is_not_conformed() {
    let mut s = fixture();
    // a dissolve on V1's cut at 14 s, where the End ends; the End is then prompted slower
    let third = s.active_sequence().unwrap().video_tracks[0].items[2].id;
    s.execute("sequence.applyVideoTransition", json!({"clip": third.0, "edge": "in", "frames": 12})).unwrap();
    take(&mut s, 0.0, &[(10.0, "prompter.slower"), (10.0, "prompter.slower")], 16.0, "transition");
    let r = report(&mut s);
    let end = row(&r, "End");
    assert_eq!(end["change"], "notConformed", "{end}");
    assert!(end["reason"].as_str().unwrap().contains("transition on V1"), "{end}");
}

#[test]
fn one_undo_restores_the_sequence_exactly_and_redo_reapplies_it() {
    let mut s = fixture();
    take(&mut s, 0.0, EARLY_THEN_SLOW, 16.0, "undo");
    let before = s.active_sequence().unwrap().clone();
    let undo = s.history.undo.len();
    s.execute("sequence.conformSections", json!({})).unwrap();
    let after = s.active_sequence().unwrap().clone();
    assert_eq!(s.history.undo.len(), undo + 1, "one undo step");
    s.execute("edit.undo", json!({})).unwrap();
    assert_eq!(s.active_sequence().unwrap(), &before, "undo restores everything");
    s.execute("edit.redo", json!({})).unwrap();
    assert_eq!(s.active_sequence().unwrap(), &after, "redo reapplies it");
    assert!(disabled(&mut s).contains("sections changed"), "a conformed take is not conformed twice");
}

/// Each refusal leaves the project and the history as they were and says why.
fn refused(s: &mut Session, says: &str) {
    let (project, undo, redo) = (s.project.clone(), s.history.undo.len(), s.history.redo.len());
    let why = disabled(s);
    assert!(why.contains(says), "expected \"{says}\" in: {why}");
    assert!(Arc::ptr_eq(&project, &s.project) && s.history.undo.len() == undo && s.history.redo.len() == redo);
}

/// The fixture with the early-then-slow take recorded; returns the take's clip.
fn recorded(tag: &str) -> (Session, i64) {
    let mut s = fixture();
    let stop = take(&mut s, 0.0, EARLY_THEN_SLOW, 16.0, tag);
    (s, n(&stop["clip"]))
}

#[test]
fn every_refusal_changes_nothing_and_says_why() {
    let mut s = fixture();
    refused(&mut s, "no voice-over take");

    let (mut s, _) = recorded("locked");
    Arc::make_mut(&mut s.project).sequence_mut(s.state.active_sequence.unwrap()).unwrap().video_tracks[1].locked = true;
    refused(&mut s, "V2 is locked");

    let (mut s, clip) = recorded("moved");
    s.execute("timeline.move", json!({"moves": [{"clip": clip, "track": "A3", "time": sec(0.5).0}]})).unwrap();
    refused(&mut s, "moved");

    let (mut s, clip) = recorded("deleted");
    s.execute("edit.clear", json!({"clips": [clip]})).unwrap();
    refused(&mut s, "deleted");

    let (mut s, _) = recorded("changed");
    let id = marker(&s, "Middle").id;
    s.execute("markers.edit", json!({"marker": id.0, "durationFrames": 100})).unwrap();
    refused(&mut s, "sections changed");

    let (mut s, _) = recorded("other");
    s.execute("file.newSequence", json!({"name": "Other", "fps": 24.0})).unwrap();
    refused(&mut s, "another sequence");

    let (mut s, _) = recorded("replaced");
    s.execute("file.openDemoProject", json!({})).unwrap();
    refused(&mut s, "replaced");
}

#[test]
fn hostile_take_logs_are_refused_or_conformed_without_panicking() {
    let (mut s, _) = recorded("hostile");
    let good = s.prompter.last_take.clone().unwrap();
    let extremes = [i64::MIN, Tick::MIN.0, -1, 0, 1, sec(5.0).0, Tick::MAX.0, i64::MAX];
    let mut logs = Vec::new();
    for k in 0..good.entries.len() {
        for &v in &extremes {
            for field in 0..4 {
                let mut l = good.clone();
                let e = &mut l.entries[k];
                match field {
                    0 => e.take_time = Tick(v),
                    1 => e.prompt_after = Tick(v),
                    2 => e.prompt_before = Tick(v),
                    _ => e.playhead = Tick(v),
                }
                logs.push(l);
            }
        }
        for rate in [0, 1, 49, 201, u32::MAX] {
            let mut l = good.clone();
            l.entries[k].rate_after = rate;
            logs.push(l);
        }
        let mut l = good.clone();
        l.entries.swap(0, k);
        logs.push(l);
    }
    let mut empty = good.clone();
    empty.entries.clear();
    logs.push(empty);
    let mut dropped = good.clone();
    dropped.dropped = 3;
    logs.push(dropped);
    for &v in &extremes {
        let mut l = good.clone();
        l.record_start = Tick(v);
        logs.push(l);
    }
    let undo = s.history.undo.len();
    for log in logs {
        s.prompter.last_take = Some(log);
        for id in ["sequence.conformSectionsReport", "sequence.conformSections"] {
            let mut s2 = std::panic::AssertUnwindSafe(&mut s);
            let r = std::panic::catch_unwind(move || s2.execute(id, json!({})).map(|_| ()));
            assert!(r.is_ok(), "{id} panicked");
        }
        assert!(s.active_sequence().unwrap().check().is_ok());
        while s.history.undo.len() > undo {
            s.execute("edit.undo", json!({})).unwrap();
        }
    }
    s.prompter.last_take = Some(good);
    assert!(s.execute("sequence.conformSections", json!({})).is_ok(), "the real log still conforms after all that");
}
