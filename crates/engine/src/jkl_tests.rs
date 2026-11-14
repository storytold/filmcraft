//! J / L shuttle speed state machine in both modes (#2718).

use serde_json::json;

use crate::Session;
use crate::autosave::Preferences;
use crate::jkl::{ShuttleKey, next_speed};
use crate::media_test_util::tmp_dir;

const L: ShuttleKey = ShuttleKey::Forward;
const J: ShuttleKey = ShuttleKey::Reverse;

/// Run a key sequence from stopped; each step feeds the previous speed back in.
fn run(keys: &[ShuttleKey], signed: bool) -> Vec<f64> {
    let mut speed = 0.0;
    keys.iter()
        .map(|&k| {
            speed = next_speed(speed, k, signed);
            speed
        })
        .collect()
}

#[test]
fn signed_mode_follows_the_issue_example() {
    // L → +1, L → +2, J → +1, J → 0 (pause), J → −1
    assert_eq!(run(&[L, L, J, J, J], true), [1.0, 2.0, 1.0, 0.0, -1.0]);
}

#[test]
fn signed_mode_uses_the_doubling_ladder_and_clamps_at_8x() {
    assert_eq!(run(&[L, L, L, L, L, L], true), [1.0, 2.0, 4.0, 8.0, 8.0, 8.0]);
    assert_eq!(run(&[J, J, J, J, J], true), [-1.0, -2.0, -4.0, -8.0, -8.0]);
    // from full speed backward, L climbs back through zero
    assert_eq!(run(&[J, J, J, J, L, L, L, L, L], true), [-1.0, -2.0, -4.0, -8.0, -4.0, -2.0, -1.0, 0.0, 1.0]);
}

#[test]
fn signed_mode_steps_off_ladder_speeds_to_the_nearest_rung() {
    assert_eq!(next_speed(0.25, L, true), 1.0);
    assert_eq!(next_speed(0.25, J, true), 0.0);
    assert_eq!(next_speed(-0.25, J, true), -1.0);
    assert_eq!(next_speed(-0.25, L, true), 0.0);
    assert_eq!(next_speed(3.0, L, true), 4.0);
    assert_eq!(next_speed(3.0, J, true), 2.0);
}

#[test]
fn default_mode_switches_direction_immediately() {
    // Premiere default: J while fast-forwarding reverses at normal speed
    assert_eq!(run(&[L, L, J, J, J], false), [1.0, 2.0, -1.0, -2.0, -4.0]);
    assert_eq!(run(&[L, L, L, L, L], false), [1.0, 2.0, 4.0, 8.0, 8.0]);
    assert_eq!(run(&[J, L], false), [-1.0, 1.0]);
    // from slow forward, L keeps doubling (unchanged behaviour)
    assert_eq!(next_speed(0.25, L, false), 0.5);
    assert_eq!(next_speed(0.25, J, false), -1.0);
}

#[test]
fn hostile_speeds_are_treated_as_stopped_or_clamped() {
    for signed in [false, true] {
        for bad in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY, 1e300, -1e300] {
            for key in [L, J] {
                let s = next_speed(bad, key, signed);
                assert!(s.is_finite() && s.abs() <= 8.0, "{bad} {key:?} {signed} → {s}");
            }
        }
        assert_eq!(next_speed(f64::NAN, L, signed), 1.0);
        assert_eq!(next_speed(f64::NAN, J, signed), -1.0);
    }
}

#[test]
fn signed_speed_pref_defaults_off_and_round_trips() {
    assert!(!Preferences::default().playback.jkl_signed_speed, "Premiere's direction switching is the default");
    let dir = tmp_dir("jkl-signed-speed");
    let path = dir.join("preferences.json");
    let mut s = Session { prefs_path: Some(path.clone()), ..Session::default() };
    s.execute("prefs.set", json!({"key": "playback.jklSignedSpeed", "value": true})).unwrap();
    assert!(s.prefs.playback.jkl_signed_speed);
    assert_eq!(s.execute("prefs.get", json!({"key": "playback.jklSignedSpeed"})).unwrap(), json!(true));
    assert!(Preferences::load(&path).playback.jkl_signed_speed);
    assert!(s.execute("prefs.set", json!({"key": "playback.jklSignedSpeed", "value": "yes"})).is_err());
    assert!(s.prefs.playback.jkl_signed_speed);
    // older preference files load it as off
    assert!(!Preferences::from_value(json!({"version": 2, "playback": {"stepManyFrames": 7}})).playback.jkl_signed_speed);
    std::fs::remove_dir_all(dir).unwrap();
}
