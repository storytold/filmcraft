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
                // Existing fields retain their values and JSON types.
                assert_eq!(j["item"], c.item.0);
                assert_eq!(j["name"], c.name);
                assert_eq!(j["start"], c.start.0);
                assert_eq!(j["duration"], c.duration.0);
                assert_eq!(j["startFrame"], rate.frame_at(c.start));
                assert_eq!(j["durationFrames"], rate.frame_at(c.duration));
                assert_eq!(j["sourceIn"], c.source_in.0);
                assert_eq!(j["speed"], c.speed);
                assert_eq!(j["enabled"], c.enabled);
                assert_eq!(j["link"], json!(c.link));
                assert_eq!(j["label"], c.label.name());
                assert_eq!(j["effects"].as_array().unwrap().len(), c.effects.len());
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

#[test]
fn sequence_inspect_reports_scale_to_frame_and_frame_hold() {
    let mut s = Session::default();
    s.execute("file.openDemoProject", json!({})).unwrap();
    s.execute("sequence.linkedSelection", json!({"on": false})).unwrap();
    let (video, audio) = {
        let q = s.active_sequence().unwrap();
        (q.video_tracks[0].items[0].id, q.audio_tracks[0].items[0].id)
    };
    s.execute("timeline.select", json!({"clips": [video.0, audio.0]})).unwrap();
    let rate = s.sequence_rate();
    let before = s.execute("sequence.inspect", json!({})).unwrap();
    for (scaled, hold_frame) in [(false, None), (true, Some(0)), (true, Some(12)), (false, None)] {
        if s.active_sequence().unwrap().video_tracks[0].items[0].scale_to_frame != scaled {
            s.execute("clip.scaleToFrameSize", json!({})).unwrap();
        }
        let held = hold_frame.map(|f| rate.tick_of(f));
        if let Some(held) = held {
            s.execute("clip.frameHoldOptions", json!({"clips": [video.0], "holdOn": "sourceTimecode", "time": held.0})).unwrap();
        } else {
            s.execute("clip.frameHoldOptions", json!({"clips": [video.0], "enabled": false})).unwrap();
        }
        let q = s.active_sequence().unwrap().clone();
        let js = s.execute("sequence.inspect", json!({})).unwrap();
        for (key, tracks) in [("video", &q.video_tracks), ("audio", &q.audio_tracks)] {
            for (ti, t) in tracks.iter().enumerate() {
                for (ci, c) in t.items.iter().enumerate() {
                    let j = &js[key][ti]["items"][ci];
                    assert_eq!(j["scaleToFrame"].as_bool(), Some(scaled && (c.id == video || c.id == audio)));
                    assert_eq!(j["scaleToFrame"], c.scale_to_frame);
                    let hold = if c.id == video { json!(held) } else { serde_json::Value::Null };
                    assert_eq!(j.get("frameHold"), Some(&hold), "{j}");
                    assert_eq!(j["frameHold"], serde_json::to_value(c.frame_hold).unwrap());
                }
            }
        }
        if !scaled {
            assert_eq!(js, before, "clearing the states restores the original inspection");
        }
    }
}

#[test]
fn sequence_inspect_effect_values_match_project_serde() {
    let mut s = Session::default();
    s.execute("file.openDemoProject", json!({})).unwrap();
    let (video, audio) = {
        let q = s.active_sequence().unwrap();
        (q.video_tracks[0].items[0].id, q.audio_tracks[0].items[0].id)
    };
    for (clip, effect, param, value) in [
        (video, "motion", "scale", json!(125.0)),
        (video, "motion", "position", json!([1920.0, 1080.0])),
        (audio, "volume", "level", json!(-3.0)),
    ] {
        s.execute("effects.setParam", json!({"clip": clip.0, "effect": effect, "param": param, "value": value})).unwrap();
    }
    let q = s.active_sequence().unwrap().clone();
    let project = s.project.to_json();
    let revision = s.revision;
    let js = s.execute("sequence.inspect", json!({})).unwrap();
    let motion = js["video"][0]["items"][0]["effects"].as_array().unwrap().iter().find(|e| e["effect"] == "motion").unwrap();
    assert_eq!(motion["params"]["scale"]["value"], json!({"Float": 125.0}));
    assert_eq!(motion["params"]["position"]["value"], json!({"Vec2": {"x": 1920.0, "y": 1080.0}}));
    let mut animated = 0;
    for (key, tracks) in [("video", &q.video_tracks), ("audio", &q.audio_tracks)] {
        for (ti, t) in tracks.iter().enumerate() {
            for (ci, c) in t.items.iter().enumerate() {
                let effects = js[key][ti]["items"][ci]["effects"].as_array().unwrap();
                assert_eq!(effects.len(), c.effects.len());
                for (j, e) in effects.iter().zip(&c.effects) {
                    assert_eq!(j["effect"], e.effect);
                    assert_eq!(j["enabled"], e.enabled);
                    assert_eq!(j["masks"], e.masks.len());
                    assert_eq!(j["params"].as_object().unwrap().len(), e.params.len());
                    for (name, p) in &e.params {
                        assert_eq!(j["params"][name]["value"], serde_json::to_value(&p.value).unwrap(), "{key} clip {} {name}", c.id.0);
                        assert_eq!(j["params"][name]["keyframes"], p.keyframes.len());
                        if p.is_animated() {
                            animated += 1;
                        }
                    }
                }
            }
        }
    }
    assert!(animated > 0, "the demo includes animated parameters; keyframes remains a count");
    assert_eq!(s.project.to_json(), project, "inspection leaves the stored parameters unchanged");
    assert_eq!(s.revision, revision);
}
