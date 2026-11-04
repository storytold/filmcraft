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
                // the keys that were there before stay
                for old in ["item", "name", "start", "duration", "startFrame", "durationFrames", "sourceIn", "speed", "enabled", "link", "label", "effects"] {
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

/// #4283: at a rate whose frame is not a whole number of ticks (11 fps), a clip or caption that
/// runs between two frame boundaries counts the frames between them, not one fewer.
#[test]
fn durations_count_frames_between_boundaries_at_fractional_tick_rates() {
    let mut s = Session::default();
    s.execute("file.openDemoProject", json!({})).unwrap();
    let media = s.project.items.iter().find(|(_, i)| i.as_media().is_some_and(|m| m.info.video.is_some())).map(|(id, _)| id.0).unwrap();
    s.execute("file.newSequence", json!({"name": "Eleven", "width": 640, "height": 360, "fps": 11, "video": 1, "audio": 1})).unwrap();
    let rate = s.sequence_rate();
    assert_ne!(rate.tick_of(1).0 * 11, rate.tick_of(11).0, "a frame at 11 fps is not a whole number of ticks");
    s.execute("timeline.place", json!({"item": media, "track": "V1", "frame": 0, "duration": {"frame": 11}, "audio": false})).unwrap();
    for f in [1, 2, 6, 10] {
        s.execute("timeline.razor", json!({"time": rate.tick_of(f).0, "track": "V1"})).unwrap();
    }
    let js = s.execute("sequence.inspect", json!({})).unwrap();
    let frames: Vec<_> = js["video"][0]["items"].as_array().unwrap().iter().map(|c| c["durationFrames"].as_i64().unwrap()).collect();
    assert_eq!(frames, [1, 1, 4, 4, 1]);

    s.execute("captions.newTrack", json!({"format": "Subtitle"})).unwrap();
    for (text, start, end) in [("A", 1, 2), ("B", 6, 10)] {
        s.execute("captions.add", json!({"track": "C1", "text": text, "frame": start, "durationSeconds": 0.1})).unwrap();
        s.execute("captions.setTimes", json!({"startFrame": start, "endFrame": end})).unwrap();
    }
    let list = s.execute("captions.list", json!({})).unwrap();
    let durations: Vec<_> = list["tracks"][0]["captions"].as_array().unwrap().iter().map(|c| c["duration"].as_str().unwrap().to_string()).collect();
    assert_eq!(durations, ["00:00:00:01", "00:00:00:04"]);
}
