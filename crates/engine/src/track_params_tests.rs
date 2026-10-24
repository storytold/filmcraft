//! Track parameters (`track`, `audioTrack`: an id, or "V1" / "A2"): a track the active sequence
//! does not have is a parameter error, never a silent default.

use serde_json::json;

use crate::{EngineError, Session};

fn demo() -> Session {
    let mut s = Session::default();
    s.execute("file.openDemoProject", json!({})).unwrap();
    s
}

/// `timeline.place {audioTrack: "A2"}` on a sequence with only A1 used to fall back to the
/// source-patched track and overwrite what was on A1.
#[test]
fn naming_a_missing_track_is_an_error_not_a_default() {
    let mut s = demo();
    let seq = s.execute("file.newSequence", json!({"name": "One of each", "video": 1, "audio": 1})).unwrap();
    let q = s.active_sequence().unwrap();
    assert_eq!((q.video_tracks.len(), q.audio_tracks.len()), (1, 1), "{seq}");
    let (v1, a1) = (q.video_tracks[0].id, q.audio_tracks[0].id);
    let item = s.project.items.values().find(|i| i.has_video() && i.has_audio() && i.as_media().is_some()).map(|i| i.id).unwrap();
    // omitted tracks keep their default: the source-patched V1 / A1
    s.execute("timeline.place", json!({"item": item.0, "time": 0})).unwrap();
    let before = s.project.clone();
    let undo = s.history.undo.len();
    for (key, name) in [("audioTrack", json!("A2")), ("track", json!("V2")), ("audioTrack", json!(999_999)), ("track", json!("V0")), ("track", json!("X1"))] {
        let mut p = json!({"item": item.0, "time": 0});
        p[key] = name.clone();
        let e = s.execute("timeline.place", p).unwrap_err();
        assert!(matches!(&e, EngineError::BadParams { cmd, .. } if cmd == "timeline.place"), "a parameter error of the command: {e:?}");
        let e = e.to_string();
        let shown = name.as_str().map(str::to_string).unwrap_or_else(|| name.to_string());
        assert!(e.contains(&format!("no track {shown} in this sequence")), "{e}");
    }
    assert!(s.execute("timeline.place", json!({"item": item.0, "time": 0, "audioTrack": "A2"})).unwrap_err().to_string().contains("audioTrack"));
    // hostile names are errors too, never a panic
    for name in ["", "V", "é1", "1", "V-1", "A1.5", "V99999999999999999999"] {
        assert!(s.execute("timeline.place", json!({"item": item.0, "time": 0, "track": name})).is_err(), "{name:?}");
    }
    // every command that takes a track by name refuses one that is not there
    let clip = s.active_sequence().unwrap().video_tracks[0].items[0].id;
    let e = s.execute("timeline.move", json!({"moves": [{"clip": clip.0, "track": "V2", "time": 0}]})).unwrap_err();
    assert!(matches!(&e, EngineError::BadParams { cmd, .. } if cmd == "timeline.move"), "{e:?}");
    assert!(s.execute("sequence.closeGap", json!({"track": "A2", "time": 0})).is_err());
    assert!(s.execute("sequence.deleteTrack", json!({"track": "V2"})).is_err());
    assert!(s.execute("sequence.deleteTrack", json!({"track": 999_999})).is_err());
    assert!(s.execute("timeline.setTrack", json!({"track": "A2", "muted": true})).is_err());
    assert!(s.execute("timeline.setTargeting", json!({"track": "V2", "targeted": true})).is_err());
    assert!(s.execute("timeline.razor", json!({"track": "A2", "time": 10})).is_err());
    assert!(s.execute("sequence.goToNextGapInTrack", json!({"track": "V2"})).is_err());
    assert_eq!(*s.project, *before, "nothing was edited");
    assert_eq!(s.history.undo.len(), undo, "and nothing was added to the undo history");
    // tracks that exist still resolve by name (either case) and by id
    s.execute("timeline.place", json!({"item": item.0, "seconds": 60.0, "track": "v1", "audioTrack": a1.0})).unwrap();
    s.execute("timeline.setTrack", json!({"track": v1.0, "locked": true})).unwrap();
    s.execute("timeline.setTrack", json!({"track": "a1", "muted": true})).unwrap();
    let q = s.active_sequence().unwrap();
    assert!(q.video_tracks[0].locked && q.audio_tracks[0].muted);
    s.execute("edit.undo", json!({})).unwrap();
    assert!(!s.active_sequence().unwrap().audio_tracks[0].muted);
}

/// `timeline.place {track: "A3"}` put a picture item on the audio track A3 and a second sound
/// clip on A1; a sound-only item went to A1 whatever audio track `track` named.
#[test]
fn place_on_an_audio_track_places_the_sound_there_and_nothing_else() {
    let mut s = demo();
    s.execute("file.newSequence", json!({"name": "Sound tracks", "video": 1, "audio": 3})).unwrap();
    let a3 = s.active_sequence().unwrap().audio_tracks[2].id;
    let with_picture = s.project.items.values().find(|i| i.has_video() && i.has_audio() && i.as_media().is_some()).map(|i| i.id).unwrap();
    let sound_only = s.project.items.values().find(|i| !i.has_video() && i.has_audio() && i.as_media().is_some()).map(|i| i.id).unwrap();
    let picture_only = s.project.items.values().find(|i| i.has_video() && !i.has_audio() && i.as_media().is_some()).map(|i| i.id).unwrap();
    // how many clips sit on [V1, A1, A2, A3]
    let counts = |s: &Session| {
        let q = s.active_sequence().unwrap();
        [q.video_tracks[0].items.len(), q.audio_tracks[0].items.len(), q.audio_tracks[1].items.len(), q.audio_tracks[2].items.len()]
    };
    // a clip with picture and sound, sent to A3 by name: its sound alone, on A3
    let r = s.execute("timeline.place", json!({"item": with_picture.0, "track": "A3", "frame": 0})).unwrap();
    assert_eq!(counts(&s), [0, 0, 0, 1], "{r}");
    let q = s.active_sequence().unwrap();
    assert_eq!(r["clips"], json!([q.audio_tracks[2].items[0].id.0]));
    assert_eq!(q.audio_tracks[2].items[0].link, None, "no picture was placed, so nothing to link to");
    q.check().unwrap();
    // a sound-only item goes to the audio track named, by name or by id
    s.execute("timeline.place", json!({"item": sound_only.0, "track": "A2", "frame": 0})).unwrap();
    assert_eq!(counts(&s), [0, 0, 1, 1]);
    s.execute("timeline.place", json!({"item": sound_only.0, "track": a3.0, "seconds": 120.0})).unwrap();
    assert_eq!(counts(&s), [0, 0, 1, 2]);
    // what cannot be meant is a parameter error that says why, and changes nothing
    let before = s.project.clone();
    let undo = s.history.undo.len();
    for (p, why) in [
        (json!({"item": with_picture.0, "track": "A3", "audioTrack": "A1", "seconds": 300.0}), "name the sound's track once"),
        (json!({"item": with_picture.0, "audioTrack": "V1", "seconds": 300.0}), "`audioTrack` names a video track"),
        (json!({"item": with_picture.0, "track": "V1", "audioTrack": "V1", "seconds": 300.0}), "`audioTrack` names a video track"),
        (json!({"item": picture_only.0, "track": "A1", "seconds": 300.0}), "this item has no sound"),
        (json!({"item": sound_only.0, "track": "V1", "seconds": 300.0}), "this item has no picture"),
    ] {
        let e = s.execute("timeline.place", p.clone()).unwrap_err();
        assert!(matches!(&e, EngineError::BadParams { cmd, .. } if cmd == "timeline.place"), "{p}: {e:?}");
        assert!(e.to_string().contains(why), "{p}: {e}");
    }
    assert_eq!(*s.project, *before, "a refused place changes nothing");
    assert_eq!(s.history.undo.len(), undo, "and adds nothing to the undo history");
    // a sound-only item dropped on a video row still lands on the audio track sent with it
    s.execute("timeline.place", json!({"item": sound_only.0, "track": "V1", "audioTrack": "A1", "seconds": 900.0})).unwrap();
    s.execute("edit.undo", json!({})).unwrap();
    // the usual forms are unchanged: picture on `track`, sound on `audioTrack` or the default A1
    s.execute("timeline.place", json!({"item": with_picture.0, "track": "V1", "audioTrack": "A2", "seconds": 300.0})).unwrap();
    assert_eq!(counts(&s), [1, 0, 2, 2]);
    s.execute("timeline.place", json!({"item": with_picture.0, "seconds": 600.0})).unwrap();
    assert_eq!(counts(&s), [2, 1, 2, 2]);
}

/// Track colour labels (#1186): set, shown by `sequence.inspect`, saved with the project,
/// cleared with "" / null, one undo step each; unknown names are refused.
#[test]
fn a_track_carries_a_colour_label() {
    let mut s = demo();
    let label = |s: &mut Session, kind: &str| s.execute("sequence.inspect", json!({})).unwrap()[kind][0]["label"].clone();
    assert_eq!(label(&mut s, "video"), json!(null));
    s.execute("timeline.setTrack", json!({"track": "V1", "label": "Forest"})).unwrap();
    s.execute("timeline.setTrack", json!({"track": "A1", "label": "Mango"})).unwrap();
    assert_eq!(label(&mut s, "video"), json!("Forest"));
    assert_eq!(label(&mut s, "audio"), json!("Mango"));
    // saved with the project, and a project from before the field still loads
    let text = serde_json::to_string(&*s.project).unwrap();
    let back: filmcraft_project::Project = serde_json::from_str(&text).unwrap();
    let q = back.sequence(s.state.active_sequence.unwrap()).unwrap();
    assert_eq!(q.video_tracks[0].label, Some(filmcraft_project::Label::Forest));
    let mut old = serde_json::to_value(&q.video_tracks[0]).unwrap();
    old.as_object_mut().unwrap().remove("label");
    let old: filmcraft_project::Track = serde_json::from_value(old).unwrap();
    assert_eq!(old.label, None);
    // a plain track does not write the key at all
    assert!(serde_json::to_value(&q.video_tracks[1]).unwrap().get("label").is_none());
    // clear, undo, refuse
    s.execute("timeline.setTrack", json!({"track": "V1", "label": ""})).unwrap();
    assert_eq!(label(&mut s, "video"), json!(null));
    s.execute("edit.undo", json!({})).unwrap();
    assert_eq!(label(&mut s, "video"), json!("Forest"));
    s.execute("timeline.setTrack", json!({"track": "V1", "label": null})).unwrap();
    assert_eq!(label(&mut s, "video"), json!(null));
    assert!(matches!(s.execute("timeline.setTrack", json!({"track": "V1", "label": "Chartreuse"})), Err(EngineError::BadParams { .. })));
    // other settings leave the label alone
    s.execute("timeline.setTrack", json!({"track": "A1", "muted": true})).unwrap();
    assert_eq!(label(&mut s, "audio"), json!("Mango"));
}
