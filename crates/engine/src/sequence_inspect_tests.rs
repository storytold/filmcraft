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

/// `from`/`to` limit the readback to the clips, transitions and markers that touch the window, so an
/// agent can read a long sequence a stretch at a time. Track entries all stay, in order, with their ids.
#[test]
fn sequence_inspect_reads_only_the_window_it_is_given() {
    let mut s = Session::default();
    s.execute("file.openDemoProject", json!({})).unwrap();
    let rate = s.sequence_rate();
    // a transition on the window's clip and one on the last clip of V1
    let (on_middle, on_last) = {
        let v1 = &s.active_sequence().unwrap().video_tracks[0].items;
        (v1[1].id.0, v1.last().unwrap().id.0)
    };
    for clip in [on_middle, on_last] {
        s.execute("sequence.applyVideoTransition", json!({"clip": clip, "edge": "out", "frames": 10})).unwrap();
    }
    let q = s.active_sequence().unwrap().clone();
    let middle = &q.video_tracks[0].items[1];
    let (from, to) = (middle.start, middle.end());
    let inside = s.execute("markers.add", json!({"time": from.0 + 1, "name": "In"})).unwrap()["marker"].clone();
    let outside = s.execute("markers.add", json!({"time": to.0, "name": "After"})).unwrap()["marker"].clone();
    let clips = |js: &serde_json::Value| -> Vec<u64> {
        ["video", "audio"]
            .iter()
            .flat_map(|k| js[*k].as_array().unwrap().iter())
            .flat_map(|t| t["items"].as_array().unwrap().iter())
            .map(|i| i["clip"].as_u64().unwrap())
            .collect()
    };
    let touching: Vec<u64> =
        q.video_tracks.iter().chain(&q.audio_tracks).flat_map(|t| &t.items).filter(|i| i.start < to && i.end() > from).map(|i| i.id.0).collect();
    let all: Vec<u64> = q.video_tracks.iter().chain(&q.audio_tracks).flat_map(|t| &t.items).map(|i| i.id.0).collect();
    assert!(touching.len() < all.len(), "the window leaves clips out");

    let js = s.execute("sequence.inspect", json!({"from": from.0, "to": to.0})).unwrap();
    assert_eq!(clips(&js), touching, "{js}");
    assert!(!clips(&js).contains(&q.video_tracks[0].items[0].id.0), "the clip that ends where the window starts is left out");
    assert_eq!(js["window"], json!({"from": from.0, "to": to.0}));
    assert_eq!(js["video"].as_array().unwrap().len(), q.video_tracks.len(), "every track stays listed");
    let transitions = |js: &serde_json::Value| -> Vec<u64> {
        ["video", "audio"]
            .iter()
            .flat_map(|k| js[*k].as_array().unwrap().iter())
            .flat_map(|t| t["transitions"].as_array().unwrap().iter())
            .map(|x| x["id"].as_u64().unwrap())
            .collect()
    };
    let all_transitions: Vec<u64> = q.video_tracks.iter().chain(&q.audio_tracks).flat_map(|t| &t.transitions).map(|x| x.id.0).collect();
    let touching_transitions: Vec<u64> = q
        .video_tracks
        .iter()
        .chain(&q.audio_tracks)
        .flat_map(|t| &t.transitions)
        .filter(|x| x.start < to && x.start + x.duration > from)
        .map(|x| x.id.0)
        .collect();
    assert!(!touching_transitions.is_empty() && touching_transitions.len() < all_transitions.len(), "one transition in the window, one outside");
    assert_eq!(transitions(&js), touching_transitions, "{js}");
    let markers: Vec<_> = js["markers"].as_array().unwrap().iter().map(|m| m["id"].clone()).collect();
    assert_eq!(markers, vec![inside.clone()], "a marker at `to` is outside: {js}");

    // time objects work as elsewhere, and an open end reaches the end of the sequence
    let secs = from.0 as f64 / filmcraft_time::TICKS_PER_SECOND as f64;
    let open = s.execute("sequence.inspect", json!({"from": {"seconds": secs}})).unwrap();
    assert!(clips(&open).contains(&q.video_tracks[0].items.last().unwrap().id.0), "{open}");
    assert!(open["markers"].as_array().unwrap().iter().any(|m| m["id"] == outside));
    let framed = s.execute("sequence.inspect", json!({"from": {"frame": rate.frame_at(from)}, "to": {"frame": rate.frame_at(to)}})).unwrap();
    assert_eq!(clips(&framed), touching);

    // without a window nothing changes
    let whole = s.execute("sequence.inspect", json!({})).unwrap();
    assert_eq!(clips(&whole), all);
    assert_eq!(transitions(&whole), all_transitions);
    assert!(whole["window"].is_null());

    for bad in [json!({"from": to.0, "to": from.0}), json!({"from": from.0, "to": from.0}), json!({"from": "soon"})] {
        assert!(s.execute("sequence.inspect", bad.clone()).is_err(), "{bad} is refused");
    }
}
