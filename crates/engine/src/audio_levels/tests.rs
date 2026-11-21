//! `audio.levels` must hear what playback plays: track mutes silence the report, and a silent
//! range comes back as one quiet stretch.

use serde_json::{Value, json};

use crate::Session;

fn levels(s: &mut Session, p: Value) -> Value {
    s.execute("audio.levels", p).unwrap()
}

#[test]
fn muting_every_audio_track_turns_the_range_into_one_quiet_stretch() {
    let mut s = Session::default();
    s.execute("file.openDemoProject", json!({})).unwrap();
    let range = json!({"startSeconds": 0.0, "endSeconds": 8.0, "windowSeconds": 0.5});
    let live = levels(&mut s, range.clone());
    assert_eq!(live["rmsDb"].as_array().map(Vec::len), live["peakDb"].as_array().map(Vec::len));
    assert!(live["rmsDb"].as_array().unwrap().len() >= 16, "one level per window: {live}");
    let tracks: Vec<u64> = s.active_sequence().unwrap().audio_tracks.iter().map(|t| t.id.0).collect();
    for t in tracks {
        s.execute("timeline.setTrack", json!({"track": t, "muted": true})).unwrap();
    }
    let muted = levels(&mut s, range);
    assert!(muted["overallRmsDb"].as_f64() < live["overallRmsDb"].as_f64(), "live {live}\nmuted {muted}");
    let quiet = muted["quiet"].as_array().unwrap();
    assert_eq!(quiet.len(), 1, "{muted}");
    assert!(quiet[0]["seconds"].as_f64().unwrap() > 7.9, "{muted}");
    // the live mix of the demo is not silent from start to end
    assert!(live["quiet"].as_array().unwrap().iter().all(|q| q["seconds"].as_f64() < Some(8.0)), "{live}");
}

#[test]
fn an_empty_or_reversed_range_is_refused() {
    let mut s = Session::default();
    s.execute("file.openDemoProject", json!({})).unwrap();
    assert!(s.execute("audio.levels", json!({"startSeconds": 5.0, "endSeconds": 2.0})).is_err());
    assert!(s.execute("audio.levels", json!({"windowSeconds": 0.0})).is_err());
}
