//! `sequence.inspect` is how an agent checks its edits: it has to show where a clip ends in the
//! sequence and in its media, the clip's audio gain, whether it plays backward and what a marker says.

use serde_json::json;

use crate::Session;

#[test]
fn sequence_inspect_reports_clip_ends_gain_and_marker_comments() {
    let mut s = Session::default();
    s.execute("file.openDemoProject", json!({})).unwrap();
    let rate = s.sequence_rate();
    // a clip at 200 %, a clip with gain and a marker with a comment, all made through commands
    s.execute("sequence.linkedSelection", json!({"on": false})).unwrap();
    let (fast, loud) = {
        let q = s.active_sequence().unwrap();
        (q.video_tracks[0].items.last().unwrap().id, q.audio_tracks[0].items[0].id)
    };
    s.execute("timeline.select", json!({"clips": [fast.0]})).unwrap();
    s.execute("clip.speedDuration", json!({"speed": 200})).unwrap();
    s.execute("timeline.select", json!({"clips": [loud.0]})).unwrap();
    s.execute("clip.audioGain", json!({"mode": "set", "db": -6.5})).unwrap();
    let marker = s.execute("markers.add", json!({"time": rate.tick_of(12).0, "name": "Beat", "comment": "cut on the downbeat"})).unwrap()["marker"].clone();

    let q = s.active_sequence().unwrap().clone();
    let js = s.execute("sequence.inspect", json!({})).unwrap();
    let m = js["markers"].as_array().unwrap().iter().find(|m| m["id"] == marker).unwrap();
    assert_eq!((m["name"].as_str(), m["comment"].as_str()), (Some("Beat"), Some("cut on the downbeat")));
    let mut seen = 0;
    for (key, tracks) in [("video", &q.video_tracks), ("audio", &q.audio_tracks)] {
        for (ti, t) in tracks.iter().enumerate() {
            for (ci, c) in t.items.iter().enumerate() {
                let j = &js[key][ti]["items"][ci];
                assert_eq!(j["clip"], c.id.0);
                assert_eq!(j["end"], c.end().0, "{j}");
                assert_eq!(j["end"].as_i64(), j["start"].as_i64().zip(j["duration"].as_i64()).map(|(a, b)| a + b));
                assert_eq!(j["endFrame"], rate.frame_at(c.end()));
                assert_eq!(j["sourceOut"], c.source_out().0, "{j}");
                assert_eq!(j["gainDb"], c.gain_db);
                // the keys that were there before stay, plus new clip state fields
                for old in [
                    "item",
                    "name",
                    "start",
                    "duration",
                    "startFrame",
                    "durationFrames",
                    "sourceIn",
                    "speed",
                    "enabled",
                    "link",
                    "label",
                    "effects",
                    "scaleToFrame",
                    "frameHold",
                ] {
                    assert!(j.get(old).is_some(), "{old} is still reported");
                }
                if c.id == fast {
                    assert_eq!(j["speed"], 2.0);
                    assert_eq!(
                        j["sourceOut"].as_i64().unwrap() - j["sourceIn"].as_i64().unwrap(),
                        2 * j["duration"].as_i64().unwrap(),
                        "at 200% a clip uses twice its length of media"
                    );
                    seen += 1;
                }
                if c.id == loud {
                    assert_eq!(j["gainDb"], -6.5);
                    seen += 1;
                }
            }
        }
    }
    assert_eq!(seen, 2, "both edited clips were found in the readback");
}

/// A clip set to play backward read back exactly like a forward one: same `speed`, same source
/// range, and nothing that said it was reversed.
#[test]
fn sequence_inspect_reports_a_reversed_clip() {
    let mut s = Session::default();
    s.execute("file.openDemoProject", json!({})).unwrap();
    let backward = s.active_sequence().unwrap().video_tracks[0].items[1].id;
    let all = |js: &serde_json::Value| -> Vec<(u64, bool)> {
        ["video", "audio"]
            .iter()
            .flat_map(|k| js[*k].as_array().unwrap().iter())
            .flat_map(|t| t["items"].as_array().unwrap().iter())
            .map(|i| (i["clip"].as_u64().unwrap(), i["reverse"].as_bool().expect("every clip reports `reverse`")))
            .collect()
    };
    let js = s.execute("sequence.inspect", json!({})).unwrap();
    assert!(all(&js).iter().all(|(_, reversed)| !reversed), "nothing plays backward yet");
    s.execute("sequence.linkedSelection", json!({"on": false})).unwrap();
    s.execute("timeline.select", json!({"clips": [backward.0]})).unwrap();
    s.execute("clip.speedDuration", json!({"speed": 100, "reverse": true})).unwrap();
    let js = s.execute("sequence.inspect", json!({})).unwrap();
    let reversed: Vec<u64> = all(&js).into_iter().filter(|(_, r)| *r).map(|(c, _)| c).collect();
    assert_eq!(reversed, vec![backward.0], "only the reversed clip says so");
    s.execute("edit.undo", json!({})).unwrap();
    let js = s.execute("sequence.inspect", json!({})).unwrap();
    assert!(all(&js).iter().all(|(_, reversed)| !reversed));
}

/// Issue #2875: `sequence.inspect` must report `scaleToFrame` (boolean) and `frameHold`
/// (integer media ticks or null), allowing tools to inspect state before mutating.
#[test]
fn sequence_inspect_reports_scale_to_frame_and_frame_hold_state() {
    let mut s = Session::default();
    s.execute("file.openDemoProject", json!({})).unwrap();
    let rate = s.sequence_rate();

    // 1. Verify default values across all clips (video and audio)
    let js = s.execute("sequence.inspect", json!({})).unwrap();
    for key in ["video", "audio"] {
        for track in js[key].as_array().unwrap() {
            for item in track["items"].as_array().unwrap() {
                assert_eq!(item["scaleToFrame"], false, "clips default to scaleToFrame: false");
                assert!(item["frameHold"].is_null(), "clips default to frameHold: null");
            }
        }
    }

    s.execute("sequence.linkedSelection", json!({"on": false})).unwrap();
    let (c0, c1) = {
        let q = s.active_sequence().unwrap();
        (q.video_tracks[0].items[0].id, q.video_tracks[0].items[1].id)
    };

    // Helper to read a clip's JSON item by clip id
    let get_clip = |js: &serde_json::Value, id: crate::ClipId| -> serde_json::Value {
        for key in ["video", "audio"] {
            for track in js[key].as_array().unwrap() {
                for item in track["items"].as_array().unwrap() {
                    if item["clip"].as_u64() == Some(id.0) {
                        return item.clone();
                    }
                }
            }
        }
        panic!("clip {id:?} not found");
    };

    // 2. Scale to Frame Size toggle readback
    s.execute("timeline.select", json!({"clips": [c0.0]})).unwrap();
    s.execute("clip.scaleToFrameSize", json!({})).unwrap();

    let js = s.execute("sequence.inspect", json!({})).unwrap();
    let j0 = get_clip(&js, c0);
    assert_eq!(j0["scaleToFrame"], true, "toggled clip reports scaleToFrame: true");
    let j1 = get_clip(&js, c1);
    assert_eq!(j1["scaleToFrame"], false, "unaffected clip still reports scaleToFrame: false");

    // Undo scale to frame size
    s.execute("edit.undo", json!({})).unwrap();
    let js = s.execute("sequence.inspect", json!({})).unwrap();
    assert_eq!(get_clip(&js, c0)["scaleToFrame"], false, "undo restores scaleToFrame: false");

    // Redo scale to frame size
    s.execute("edit.redo", json!({})).unwrap();
    let js = s.execute("sequence.inspect", json!({})).unwrap();
    assert_eq!(get_clip(&js, c0)["scaleToFrame"], true, "redo restores scaleToFrame: true");

    // 3. Frame Hold Options - zero and nonzero source tick semantics
    s.execute("timeline.select", json!({"clips": [c1.0]})).unwrap();

    // Zero source tick semantics: Some(Tick(0)) must differ from null
    s.execute("clip.frameHoldOptions", json!({"holdOn": "sourceTimecode", "time": 0})).unwrap();
    let js = s.execute("sequence.inspect", json!({})).unwrap();
    let j1 = get_clip(&js, c1);
    assert_eq!(j1["frameHold"], json!(0), "zero tick hold reports integer 0, not null");
    assert!(!j1["frameHold"].is_null(), "zero tick hold is not null");
    assert_eq!(j1["scaleToFrame"], false, "c1 scaleToFrame is unchanged");
    assert_eq!(get_clip(&js, c0)["frameHold"], serde_json::Value::Null, "c0 frameHold unaffected");

    // Nonzero source tick semantics
    let hold_ticks = rate.tick_of(10).0;
    assert!(hold_ticks > 0, "test tick must be positive");
    s.execute("clip.frameHoldOptions", json!({"holdOn": "sourceTimecode", "time": hold_ticks})).unwrap();
    let js = s.execute("sequence.inspect", json!({})).unwrap();
    let j1 = get_clip(&js, c1);
    assert_eq!(j1["frameHold"], json!(hold_ticks), "nonzero tick hold reports media tick count");

    // Disabling frame hold resets to null
    s.execute("clip.frameHoldOptions", json!({"enabled": false})).unwrap();
    let js = s.execute("sequence.inspect", json!({})).unwrap();
    assert!(get_clip(&js, c1)["frameHold"].is_null(), "disabled frame hold reports null");

    // Undo / redo frame hold operations
    s.execute("edit.undo", json!({})).unwrap();
    let js = s.execute("sequence.inspect", json!({})).unwrap();
    assert_eq!(get_clip(&js, c1)["frameHold"], json!(hold_ticks), "undo restores nonzero hold");

    s.execute("edit.undo", json!({})).unwrap();
    let js = s.execute("sequence.inspect", json!({})).unwrap();
    assert_eq!(get_clip(&js, c1)["frameHold"], json!(0), "undo restores zero hold");

    s.execute("edit.undo", json!({})).unwrap();
    let js = s.execute("sequence.inspect", json!({})).unwrap();
    assert!(get_clip(&js, c1)["frameHold"].is_null(), "undo restores unheld (null)");

    s.execute("edit.redo", json!({})).unwrap();
    let js = s.execute("sequence.inspect", json!({})).unwrap();
    assert_eq!(get_clip(&js, c1)["frameHold"], json!(0), "redo restores zero hold");
}
