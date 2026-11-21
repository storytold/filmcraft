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

/// A quiet stretch cut short by the end of the range is reported only if what is left is still
/// `minQuietSeconds` long.
#[test]
fn a_quiet_stretch_clipped_by_the_range_end_must_still_be_long_enough() {
    let mut s = Session::default();
    s.execute("file.openDemoProject", json!({})).unwrap();
    let tracks: Vec<u64> = s.active_sequence().unwrap().audio_tracks.iter().map(|t| t.id.0).collect();
    for t in tracks {
        s.execute("timeline.setTrack", json!({"track": t, "muted": true})).unwrap();
    }
    // 0.75 s of silence: two 0.5 s windows, the second clipped to the range end
    let short = levels(&mut s, json!({"startSeconds": 0.0, "endSeconds": 0.75, "minQuietSeconds": 1.0}));
    assert!(short["quiet"].as_array().unwrap().is_empty(), "{short}");
    let long = levels(&mut s, json!({"startSeconds": 0.0, "endSeconds": 1.25, "minQuietSeconds": 1.0}));
    let q = long["quiet"].as_array().unwrap();
    assert_eq!(q.len(), 1, "{long}");
    assert!((q[0]["seconds"].as_f64().unwrap() - 1.25).abs() < 1e-9, "{long}");
}

/// The windows are whole samples: the reported start and width are what was measured, so the
/// times of quiet stretches and loud moments line up with the audio.
#[test]
fn reported_windows_are_the_measured_sample_spans() {
    let mut s = Session::default();
    s.execute("file.openDemoProject", json!({})).unwrap();
    let sr = f64::from(s.active_sequence().unwrap().settings.sample_rate);
    let r = levels(&mut s, json!({"startSeconds": 0.10001, "endSeconds": 3.0, "windowSeconds": 1.0 / 3.0}));
    let (start, window) = (r["startSeconds"].as_f64().unwrap(), r["windowSeconds"].as_f64().unwrap());
    let whole = |x: f64| ((x * sr) - (x * sr).round()).abs() < 1e-6;
    assert!(whole(start) && whole(window), "{start} {window} at {sr} Hz");
    assert!(start <= 0.10001 && 0.10001 - start < 1.0 / sr, "the window starts on the sample of the requested start");
}

/// A quiet stretch of exactly minQuietSeconds is kept: lengths are compared in samples.
#[test]
fn a_quiet_stretch_of_exactly_the_minimum_is_kept() {
    let mut s = Session::default();
    s.execute("file.openDemoProject", json!({})).unwrap();
    let tracks: Vec<u64> = s.active_sequence().unwrap().audio_tracks.iter().map(|t| t.id.0).collect();
    for t in tracks {
        s.execute("timeline.setTrack", json!({"track": t, "muted": true})).unwrap();
    }
    for (start, end, window, min) in [(0.4, 1.4, 0.5, 1.0), (0.6, 0.9, 0.1, 0.3)] {
        let r = levels(&mut s, json!({"startSeconds": start, "endSeconds": end, "windowSeconds": window, "minQuietSeconds": min}));
        assert_eq!(r["quiet"].as_array().unwrap().len(), 1, "{start}..{end} by {window}: {r}");
    }
}
