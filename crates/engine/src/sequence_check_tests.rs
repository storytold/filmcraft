//! `sequence.check` (#4257): short clips, picture and sound holes, empty stretches and jump cuts.

use super::*;
use filmcraft_project::TrackItem;
use serde_json::json;

/// An empty sequence, a 20 s Bars and Tone item (video + audio) and a 20 s colour matte (video).
fn fresh() -> (Session, u64, u64) {
    let mut s = Session::default();
    s.execute("file.newSequence", json!({"name": "Check"})).unwrap();
    let bars = s.execute("file.newBarsAndTone", json!({"seconds": 20.0})).unwrap()["item"].as_u64().unwrap();
    let matte = s.execute("file.newColorMatte", json!({"seconds": 20.0})).unwrap()["item"].as_u64().unwrap();
    (s, bars, matte)
}

fn f(s: &Session, frames: i64) -> Tick {
    s.sequence_rate().tick_of(frames)
}

/// Place `frames` of `item` from source frame `source_in` at frame `at` on `track` (and, for media
/// with sound placed on a video track, the matching audio track). Returns the new clip ids.
fn place(s: &mut Session, item: u64, track: &str, at: i64, source_in: i64, frames: i64) -> Vec<u64> {
    let (sin, dur, at) = (f(s, source_in), f(s, frames), f(s, at));
    let audio = track.replace('V', "A");
    let r =
        s.execute("timeline.place", json!({"item": item, "track": track, "audioTrack": audio, "time": at.0, "sourceIn": sin.0, "duration": dur.0})).unwrap();
    r["clips"].as_array().unwrap().iter().map(|c| c.as_u64().unwrap()).collect()
}

fn check(s: &mut Session, p: Value) -> Value {
    s.execute("sequence.check", p).unwrap()
}

/// The findings of one check.
fn of(r: &Value, check: &str) -> Vec<Value> {
    r["findings"].as_array().unwrap().iter().filter(|f| f["check"] == check).cloned().collect()
}

/// The `field` list of a finding, sorted, as clip ids.
fn sorted(v: &Value, field: &str) -> Vec<u64> {
    let mut ids: Vec<u64> = v[field].as_array().unwrap().iter().map(|c| c.as_u64().unwrap()).collect();
    ids.sort_unstable();
    ids
}

/// The issue's probe: a jump cut inside one shot, a flash frame, a hole in the picture under
/// continuing music and a stray clip after a stretch with nothing seen or heard.
#[test]
fn finds_flash_frames_holes_stray_clips_and_jump_cuts() {
    let (mut s, bars, matte) = fresh();
    let a = place(&mut s, bars, "V1", 0, 0, 120); // V1 + A1, 0–120
    let b = place(&mut s, bars, "V1", 120, 131, 80); // V1 + A1, 120–200: the source skips 11 frames
    let flash = place(&mut s, matte, "V1", 200, 0, 1); // 200–201
    let music = s.execute("timeline.place", json!({"item": bars, "track": "A2", "time": 0, "sourceIn": 0, "duration": f(&s, 240).0})).unwrap()["clips"][0]
        .as_u64()
        .unwrap(); // A2, 0–240
    let last = place(&mut s, matte, "V1", 213, 0, 27); // 213–240: V1 is empty at 201–213
    let stray = place(&mut s, matte, "V2", 300, 0, 1); // 300–301, after nothing at 240–300
    let r = check(&mut s, json!({}));
    assert_eq!(r["durationFrames"], 301, "{r:#}");

    let short = of(&r, "shortClip");
    assert_eq!(short.len(), 2, "{r:#}");
    assert_eq!(
        (short[0]["track"].as_str(), short[0]["clips"][0].as_u64(), short[0]["startFrame"].as_i64(), short[0]["endFrame"].as_i64()),
        (Some("V1"), Some(flash[0]), Some(200), Some(201))
    );
    assert_eq!((short[1]["track"].as_str(), short[1]["clips"][0].as_u64(), short[1]["startFrame"].as_i64()), (Some("V2"), Some(stray[0]), Some(300)));

    let holes = of(&r, "pictureHole");
    assert_eq!(holes.len(), 1, "{r:#}");
    assert_eq!((holes[0]["startFrame"].as_i64(), holes[0]["endFrame"].as_i64()), (Some(201), Some(213)));
    assert_eq!(sorted(&holes[0], "audioClips"), vec![music]);

    let empty = of(&r, "emptyStretch");
    assert_eq!(empty.len(), 1, "{r:#}");
    assert_eq!((empty[0]["startFrame"].as_i64(), empty[0]["endFrame"].as_i64()), (Some(240), Some(300)));
    let mut before = vec![last[0], music];
    before.sort_unstable();
    assert_eq!(sorted(&empty[0], "before"), before);
    assert_eq!(sorted(&empty[0], "after"), vec![stray[0]]);
    assert!(empty[0].get("atEnd").is_none(), "the stray clip ends the sequence");

    // the picture of 200–201 and 213–240 plays over the music; only the stray frame is silent
    let silent = of(&r, "soundHole");
    assert_eq!(silent.len(), 1, "{r:#}");
    assert_eq!((silent[0]["startFrame"].as_i64(), silent[0]["endFrame"].as_i64()), (Some(300), Some(301)));
    assert_eq!(sorted(&silent[0], "videoClips"), vec![stray[0]]);

    let jumps = of(&r, "jumpCut");
    assert_eq!(jumps.len(), 2, "picture and sound: {r:#}");
    for (j, track) in jumps.iter().zip(["V1", "A1"]) {
        assert_eq!(j["track"], track);
        assert_eq!(j["frame"], 120);
        assert_eq!(j["severity"], "info");
        assert_eq!(j["sourceSkip"].as_i64(), Some((f(&s, 131) - f(&s, 120)).0));
    }
    assert_eq!(sorted(&jumps[0], "clips"), {
        let mut v = vec![a[0], b[0]];
        v.sort_unstable();
        v
    });
    assert_eq!(r["counts"], json!({"warning": 5, "info": 2}));

    // findings come in time order
    let starts: Vec<i64> = r["findings"].as_array().unwrap().iter().map(|f| f["startFrame"].as_i64().or(f["frame"].as_i64()).unwrap()).collect();
    assert!(starts.windows(2).all(|w| w[0] <= w[1]), "{starts:?}");
}

/// A clean edit has nothing to report, and a cut where the source continues (Add Edit) is a
/// through edit, not a jump cut.
#[test]
fn a_clean_sequence_has_no_findings() {
    let (mut s, bars, _) = fresh();
    place(&mut s, bars, "V1", 0, 0, 48);
    place(&mut s, bars, "V1", 48, 48, 48);
    let r = check(&mut s, json!({}));
    assert_eq!(r["findings"], json!([]), "{r:#}");
    assert_eq!(r["counts"], json!({"warning": 0, "info": 0}));
    assert_eq!(r["durationFrames"], 96);
}

/// A disabled clip, or one on a video track whose output is off, shows nothing; a caption after
/// the last clip keeps the sequence running and is named at the end.
#[test]
fn hidden_and_disabled_clips_and_late_captions_count_as_nothing_seen() {
    let (mut s, bars, matte) = fresh();
    place(&mut s, bars, "V1", 0, 0, 48); // V1 + A1, 0–48
    let hidden = place(&mut s, matte, "V2", 48, 0, 12); // 48–60 on V2, whose output goes off
    let off = place(&mut s, matte, "V1", 60, 0, 12); // 60–72, disabled
    s.execute("timeline.setTrack", json!({"track": "V2", "enabled": false})).unwrap();
    s.execute("clip.enable", json!({"clips": off})).unwrap();
    let cap = s.execute("captions.add", json!({"text": "late", "time": f(&s, 72).0, "durationSeconds": 1.0})).unwrap()["caption"].as_u64().unwrap();
    let r = check(&mut s, json!({}));
    assert!(of(&r, "soundHole").is_empty(), "nothing is seen at 48–72: {r:#}");
    let empty = of(&r, "emptyStretch");
    assert_eq!(empty.len(), 1, "{r:#}");
    let e = &empty[0];
    assert_eq!(e["startFrame"], 48);
    assert_eq!(e["endFrame"], r["durationFrames"]);
    assert_eq!(e["atEnd"], true);
    assert_eq!(e["after"], json!([]));
    assert_eq!(sorted(e, "before").len(), 2, "the bars' picture and sound end at 48: {e}");
    let mut silent = vec![hidden[0], off[0]];
    silent.sort_unstable();
    assert_eq!(sorted(e, "clips"), silent);
    assert_eq!(sorted(e, "captions"), vec![cap]);
}

/// Video with no sound anywhere is a sound hole; sound alone is a picture hole.
#[test]
fn picture_without_sound_and_sound_without_picture() {
    let (mut s, bars, matte) = fresh();
    let m = place(&mut s, matte, "V1", 0, 0, 24);
    let tone =
        s.execute("timeline.place", json!({"item": bars, "track": "A1", "time": f(&s, 24).0, "duration": f(&s, 24).0})).unwrap()["clips"][0].as_u64().unwrap();
    let r = check(&mut s, json!({}));
    let (silent, holes) = (of(&r, "soundHole"), of(&r, "pictureHole"));
    assert_eq!((silent.len(), holes.len()), (1, 1), "{r:#}");
    assert_eq!((silent[0]["startFrame"].as_i64(), silent[0]["endFrame"].as_i64()), (Some(0), Some(24)));
    assert_eq!(sorted(&silent[0], "videoClips"), m);
    assert_eq!((holes[0]["startFrame"].as_i64(), holes[0]["endFrame"].as_i64()), (Some(24), Some(48)));
    assert_eq!(sorted(&holes[0], "audioClips"), vec![tone]);
    // a muted track is not heard, so the tone's frames have nothing at all
    s.execute("timeline.setTrack", json!({"track": "A1", "muted": true})).unwrap();
    let r = check(&mut s, json!({}));
    assert!(of(&r, "pictureHole").is_empty(), "{r:#}");
    let empty = of(&r, "emptyStretch");
    assert_eq!(empty.len(), 1, "{r:#}");
    assert_eq!(empty[0]["atEnd"], true);
    assert_eq!(sorted(&empty[0], "clips"), vec![tone]);
}

#[test]
fn checks_and_min_frames_choose_what_runs() {
    let (mut s, bars, matte) = fresh();
    place(&mut s, bars, "V1", 0, 0, 48);
    place(&mut s, matte, "V1", 48, 0, 4); // 4 frames, no sound under it
    let r = check(&mut s, json!({}));
    assert!(of(&r, "shortClip").is_empty(), "4 frames is not short by default: {r:#}");
    assert_eq!(of(&r, "soundHole").len(), 1);
    let r = check(&mut s, json!({"minFrames": 5}));
    assert_eq!(of(&r, "shortClip").len(), 1, "{r:#}");
    let r = check(&mut s, json!({"checks": ["shortClip"], "minFrames": 5}));
    assert_eq!(r["findings"].as_array().unwrap().len(), 1, "{r:#}");
    assert_eq!(r["findings"][0]["check"], "shortClip");
    let r = check(&mut s, json!({"checks": []}));
    assert_eq!(r["findings"], json!([]));
}

/// Hostile parameters are refused with a message, never a panic or a different sequence.
#[test]
fn bad_parameters_are_errors() {
    let (mut s, bars, _) = fresh();
    place(&mut s, bars, "V1", 0, 0, 48);
    for p in [
        json!({"checks": "shortClip"}),
        json!({"checks": [1]}),
        json!({"checks": ["flashFrame"]}),
        json!({"checks": ["transitionHandles"]}),
        json!({"minFrames": 0}),
        json!({"minFrames": -3}),
        json!({"minFrames": 2.5}),
        json!({"minFrames": i64::MAX}),
        json!({"minFrames": "3"}),
        json!({"item": bars}),
        json!({"item": u64::MAX}),
        // a fractional id is not truncated to a sequence
        json!({"item": s.state.active_sequence.unwrap().0 as f64 + 0.5}),
        json!({"item": "V1"}),
    ] {
        let e = s.execute("sequence.check", p.clone());
        assert!(e.is_err(), "{p} was accepted: {e:?}");
    }
    let e = s.execute("sequence.check", json!({"checks": ["transitionHandles"]})).unwrap_err().to_string();
    assert!(e.contains("unsupported"), "{e}");
    // no sequence at all
    let mut empty = Session::default();
    assert!(empty.execute("sequence.check", json!({})).is_err());
}

/// The check reads another sequence by id, and changes nothing: not the project, its dirty flag,
/// the history, the selection or the playhead.
#[test]
fn reads_any_sequence_and_changes_nothing() {
    let (mut s, bars, matte) = fresh();
    let first = s.state.active_sequence.unwrap();
    place(&mut s, matte, "V1", 0, 0, 1);
    s.execute("file.newSequence", json!({"name": "Other"})).unwrap();
    place(&mut s, bars, "V1", 0, 0, 48);
    s.execute("playhead.set", json!({"frame": 10})).unwrap();
    let before = (s.project.clone(), s.is_dirty(), s.history.undo.len(), s.history.redo.len(), serde_json::to_value(&s.state).unwrap(), s.playhead());
    let r = check(&mut s, json!({"item": first.0}));
    assert_eq!(r["sequence"], first.0);
    assert_eq!(of(&r, "shortClip").len(), 1, "{r:#}");
    let r = check(&mut s, json!({}));
    assert_eq!(r["findings"], json!([]), "the active sequence is clean: {r:#}");
    let after = (s.project.clone(), s.is_dirty(), s.history.undo.len(), s.history.redo.len(), serde_json::to_value(&s.state).unwrap(), s.playhead());
    assert!(before == after, "sequence.check changed the session");
}

/// Projects are hostile input: absurd times, zero-length and overlapping clips and a broken frame
/// rate give findings or nothing, never a panic.
#[test]
fn hostile_sequences_do_not_panic() {
    let (mut s, bars, _) = fresh();
    place(&mut s, bars, "V1", 0, 0, 48);
    place(&mut s, bars, "V1", 48, 10, 48);
    let id = s.state.active_sequence.unwrap();
    let q = std::sync::Arc::make_mut(&mut s.project).sequence_mut(id).unwrap();
    q.settings.frame_rate = filmcraft_time::FrameRate { num: 0, den: 0 };
    let tr = &mut q.video_tracks[0];
    tr.items[0].duration = Tick(i64::MIN);
    tr.items[1].start = Tick(i64::MAX - 5);
    tr.items[1].duration = Tick(i64::MAX);
    tr.items[1].source_in = Tick(i64::MIN);
    tr.items[1].speed = f64::NAN;
    let r = s.execute("sequence.check", json!({"minFrames": 1_000}));
    assert!(r.is_ok(), "{r:?}");
}

/// Change clip `id` in the active sequence.
fn set(s: &mut Session, id: u64, f: impl FnOnce(&mut TrackItem)) {
    s.edit_sequence("test", |q, _, _| {
        f(q.find_item_mut(ClipId(id)).unwrap().1);
        Ok(())
    })
    .unwrap();
}

/// A clip that ends partway through a frame covers that frame in the export, so the partial last
/// frame (which `durationFrames` counts) is not an empty stretch.
#[test]
fn a_clip_ending_partway_through_a_frame_covers_that_frame() {
    let (mut s, bars, _) = fresh();
    let clips = place(&mut s, bars, "V1", 0, 0, 10);
    let half = s.sequence_rate().frame_duration().0 / 2;
    for c in &clips {
        set(&mut s, *c, |it| it.duration += Tick(half));
    }
    let r = check(&mut s, json!({}));
    assert_eq!(r["durationFrames"], 11);
    assert_eq!(r["findings"], json!([]), "{r:#}");
}

/// A jump cut nobody sees or hears is not reported, nor is a cut across a speed ramp.
#[test]
fn jump_cuts_only_between_clips_seen_or_heard_and_not_across_a_speed_ramp() {
    let (mut s, bars, _) = fresh();
    let a = place(&mut s, bars, "V1", 0, 0, 48);
    let b = place(&mut s, bars, "V1", 48, 60, 48);
    let tracks = |r: &Value| of(r, "jumpCut").iter().map(|j| j["track"].as_str().unwrap().to_string()).collect::<Vec<_>>();
    assert_eq!(tracks(&check(&mut s, json!({"checks": ["jumpCut"]}))), ["V1", "A1"]);
    s.execute("timeline.setTrack", json!({"track": "V1", "enabled": false})).unwrap();
    assert_eq!(tracks(&check(&mut s, json!({"checks": ["jumpCut"]}))), ["A1"], "V1's output is off");
    s.execute("clip.enable", json!({"clips": [b[1]]})).unwrap();
    assert!(tracks(&check(&mut s, json!({"checks": ["jumpCut"]}))).is_empty(), "the sound after the cut is disabled");
    s.execute("timeline.setTrack", json!({"track": "V1", "enabled": true})).unwrap();
    set(&mut s, a[0], |it| {
        let p = it
            .effect_mut("time_remap")
            .unwrap()
            .params
            .entry("speed".into())
            .or_insert_with(|| filmcraft_project::Param::new(filmcraft_project::ParamValue::Float(100.0)));
        p.keyframes.push(filmcraft_project::Keyframe::new(Tick::ZERO, filmcraft_project::ParamValue::Float(100.0)));
        p.keyframes.push(filmcraft_project::Keyframe::new(Tick(1), filmcraft_project::ParamValue::Float(50.0)));
    });
    assert!(tracks(&check(&mut s, json!({"checks": ["jumpCut"]}))).is_empty(), "the left piece ramps its speed");
}

/// A caption over a stretch in the middle of the edit is named with it.
#[test]
fn an_empty_stretch_names_the_captions_over_it() {
    let (mut s, bars, _) = fresh();
    place(&mut s, bars, "V1", 0, 0, 24);
    place(&mut s, bars, "V1", 72, 0, 24);
    let cap = s.execute("captions.add", json!({"text": "mid", "time": f(&s, 30).0, "durationSeconds": 1.0})).unwrap()["caption"].as_u64().unwrap();
    let r = check(&mut s, json!({}));
    let empty = of(&r, "emptyStretch");
    assert_eq!(empty.len(), 1, "{r:#}");
    assert_eq!((empty[0]["startFrame"].as_i64(), empty[0]["endFrame"].as_i64()), (Some(24), Some(72)));
    assert_eq!(sorted(&empty[0], "captions"), vec![cap]);
    assert!(empty[0].get("atEnd").is_none());
}

/// A transition shows its enabled side over its whole length, into that clip's handles, so a
/// disabled clip after a dissolve leaves nothing seen only from the transition's end.
#[test]
fn a_transition_shows_its_enabled_side_past_the_cut() {
    let (mut s, _, matte) = fresh();
    let a = place(&mut s, matte, "V1", 0, 0, 48);
    let b = place(&mut s, matte, "V1", 48, 0, 48);
    s.execute("sequence.applyVideoTransition", json!({"clip": a[0], "edge": "out", "frames": 24, "align": "center"})).unwrap();
    s.execute("clip.enable", json!({"clips": b})).unwrap();
    let r = check(&mut s, json!({"checks": ["soundHole", "emptyStretch"]}));
    let silent = of(&r, "soundHole");
    assert_eq!(silent.len(), 1, "{r:#}");
    assert_eq!((silent[0]["startFrame"].as_i64(), silent[0]["endFrame"].as_i64()), (Some(0), Some(60)), "{r:#}");
    assert_eq!(sorted(&silent[0], "videoClips"), a, "each clip named once");
    let empty = of(&r, "emptyStretch");
    assert_eq!(empty.len(), 1, "{r:#}");
    assert_eq!((empty[0]["startFrame"].as_i64(), empty[0]["endFrame"].as_i64(), empty[0]["atEnd"].as_bool()), (Some(60), Some(96), Some(true)));
    assert_eq!(sorted(&empty[0], "before"), a);
    assert_eq!(sorted(&empty[0], "clips"), b);
}

/// A jump cut off the frame grid is reported at the first frame that shows the incoming clip.
#[test]
fn an_off_grid_jump_cut_is_at_the_first_frame_of_the_incoming_clip() {
    let (mut s, bars, _) = fresh();
    let a = place(&mut s, bars, "V1", 0, 0, 48);
    let b = place(&mut s, bars, "V1", 48, 60, 48);
    let half = Tick(s.sequence_rate().frame_duration().0 / 2);
    // move the cut half a frame later, both sides in one edit
    s.edit_sequence("test", |q, _, _| {
        q.find_item_mut(ClipId(a[0])).unwrap().1.duration += half;
        let right = q.find_item_mut(ClipId(b[0])).unwrap().1;
        right.start += half;
        right.duration -= half;
        right.source_in += half;
        Ok(())
    })
    .unwrap();
    let r = check(&mut s, json!({"checks": ["jumpCut"]}));
    let v1: Vec<Value> = of(&r, "jumpCut").into_iter().filter(|j| j["track"] == "V1").collect();
    assert_eq!(v1.len(), 1, "{r:#}");
    assert_eq!(v1[0]["frame"], 49, "frame 48 still samples the outgoing clip");
}

/// Where two video transitions overlap, the compositor draws only the first one, so the second
/// one's enabled side is seen only from where the first ends.
#[test]
fn overlapping_video_transitions_count_only_the_one_drawn() {
    let (mut s, bars, matte) = fresh();
    let a = place(&mut s, matte, "V1", 0, 0, 48)[0];
    let b = place(&mut s, matte, "V1", 48, 0, 48)[0];
    place(&mut s, matte, "V1", 96, 0, 48); // C, enabled
    s.execute("timeline.place", json!({"item": bars, "track": "A1", "time": 0, "duration": f(&s, 144).0})).unwrap();
    s.execute("sequence.applyVideoTransition", json!({"clip": a, "edge": "out", "frames": 24, "align": "center"})).unwrap();
    s.execute("sequence.applyVideoTransition", json!({"clip": b, "edge": "out", "frames": 24, "align": "center"})).unwrap();
    // stretch them so they overlap: A→B over 24–72, B→C over 60–132
    let (f24, f48, f60, f72) = (f(&s, 24), f(&s, 48), f(&s, 60), f(&s, 72));
    s.edit_sequence("test", |q, _, _| {
        let tr = &mut q.video_tracks[0];
        tr.transitions.sort_by_key(|x| x.start);
        assert_eq!(tr.transitions.len(), 2);
        (tr.transitions[0].start, tr.transitions[0].duration) = (f24, f48);
        (tr.transitions[1].start, tr.transitions[1].duration) = (f60, f72);
        Ok(())
    })
    .unwrap();
    s.execute("clip.enable", json!({"clips": [a, b]})).unwrap();
    let r = check(&mut s, json!({"checks": ["pictureHole"]}));
    let holes = of(&r, "pictureHole");
    assert_eq!(holes.len(), 1, "{r:#}");
    assert_eq!((holes[0]["startFrame"].as_i64(), holes[0]["endFrame"].as_i64()), (Some(0), Some(72)), "C shows from where A→B ends: {r:#}");
}

/// A clip shorter than a frame that no frame samples shows nothing, so the cut into it is no
/// jump cut, and no finding points past the end of the sequence.
#[test]
fn a_jump_into_a_clip_no_frame_samples_is_not_reported() {
    let (mut s, bars, _) = fresh();
    let a = place(&mut s, bars, "V1", 0, 0, 48);
    let b = place(&mut s, bars, "V1", 48, 60, 1);
    let half = Tick(s.sequence_rate().frame_duration().0 / 2);
    s.edit_sequence("test", |q, _, _| {
        q.find_item_mut(ClipId(a[0])).unwrap().1.duration += half;
        let right = q.find_item_mut(ClipId(b[0])).unwrap().1;
        right.start += half;
        right.duration -= half;
        right.source_in += half;
        Ok(())
    })
    .unwrap();
    let r = check(&mut s, json!({"checks": ["jumpCut"]}));
    let v1: Vec<Value> = of(&r, "jumpCut").into_iter().filter(|j| j["track"] == "V1").collect();
    assert!(v1.is_empty(), "{r:#}");
    assert_eq!(r["durationFrames"], 49);
}
