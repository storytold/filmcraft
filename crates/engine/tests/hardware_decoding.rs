//! Process-wide decoder policy: own executable excludes parallel preference writers.

use filmcraft_engine::{
    Session,
    settings::{self, Kind},
};
use serde_json::json;

#[test]
fn hardware_decoding_setting_drives_the_decoder_switch() {
    let mut s = Session::default();
    assert_eq!(s.prefs.playback.hardware_decoding, "auto", "Auto by default");
    s.execute("prefs.set", json!({"key": "playback.hardwareDecoding", "value": "off"})).unwrap();
    assert!(!filmcraft_codecs::hw::hardware_decoding(), "Off reaches the decoder registry");
    assert!(s.execute("prefs.set", json!({"key": "playback.hardwareDecoding", "value": "gpu"})).is_err());
    s.execute("prefs.set", json!({"key": "playback.hardwareDecoding", "value": "auto"})).unwrap();
    assert!(filmcraft_codecs::hw::hardware_decoding());
    assert!(settings::field("playback.hardwareDecoding").is_some_and(|f| f.wired && matches!(f.kind, Kind::Choice(_))));
}
