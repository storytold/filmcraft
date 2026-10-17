use filmcraft_engine::{Session, export_tools::settings_from_params};
use filmcraft_export::{ExportSettings, HardwareEncoding};
use serde_json::{Value, json};

fn resolve(params: Value) -> filmcraft_engine::Result<ExportSettings> {
    settings_from_params(&Session::default(), &params, "file.exportMedia").map(|(_, settings)| settings)
}

#[test]
fn cli_settings_shorthand_enables_loudness_normalization() {
    // This is the exact shape the CLI builds for --settings JSON.
    let settings = resolve(json!({"settings": {
        "hardwareEncoding": "auto", "bitrateKbps": 60000, "loudnessLufs": -14
    }}))
    .unwrap();
    assert!(settings.effects.loudness.enabled);
    assert_eq!(settings.effects.loudness.target_lufs, -14.0);
    assert_eq!(settings.hardware_encoding, HardwareEncoding::Auto);
    assert_eq!(settings.bitrate_kbps, 60000);
}

#[test]
fn loudness_shorthand_precedence_is_flat_then_settings_then_canonical() {
    let nested = json!({"loudnessLufs": -14, "effects": {"loudness": {
        "enabled": false, "targetLufs": -23, "truePeakDbtp": -1
    }}});
    let settings = resolve(json!({"settings": nested})).unwrap();
    assert!(settings.effects.loudness.enabled);
    assert_eq!(settings.effects.loudness.target_lufs, -14.0);
    assert_eq!(settings.effects.loudness.true_peak_dbtp, -1.0);
    let settings = resolve(json!({"settings": nested, "loudnessLufs": -16})).unwrap();
    assert!(settings.effects.loudness.enabled);
    assert_eq!(settings.effects.loudness.target_lufs, -16.0);
}

#[test]
fn invalid_loudness_shorthand_is_rejected_in_both_locations() {
    for value in [json!(null), json!(true), json!("-14"), json!([]), json!({})] {
        assert!(resolve(json!({"loudnessLufs": value})).is_err(), "flat {value}");
        assert!(resolve(json!({"settings": {"loudnessLufs": value}})).is_err(), "nested {value}");
        // A valid flat override must not conceal malformed nested input.
        assert!(resolve(json!({"settings": {"loudnessLufs": value}, "loudnessLufs": -14})).is_err(), "overridden {value}");
    }
    for value in [-70.0, -14.0, 0.0] {
        assert!(resolve(json!({"settings": {"loudnessLufs": value}})).is_ok());
    }
}

#[test]
fn canonical_loudness_and_unrelated_settings_validation_are_preserved() {
    assert!(!resolve(json!({})).unwrap().effects.loudness.enabled);
    let settings = resolve(json!({"settings": {"effects": {"loudness": {
        "enabled": true, "targetLufs": -18, "truePeakDbtp": -1
    }}}}))
    .unwrap();
    assert!(settings.effects.loudness.enabled);
    assert_eq!(settings.effects.loudness.target_lufs, -18.0);
    assert_eq!(settings.effects.loudness.true_peak_dbtp, -1.0);
    for patch in [json!({"hardwareEncoding": "invalid"}), json!({"bitrateKbps": -1}), json!({"scale": 0})] {
        assert!(resolve(json!({"settings": patch})).is_err());
    }
}

#[test]
fn settings_shorthand_normalizes_an_actual_audio_export() {
    use filmcraft_media::MediaSource;

    let mut session = Session::default();
    session.execute("file.openDemoProject", json!({})).unwrap();
    let directory = std::env::temp_dir().join(format!("filmcraft-loudness-settings-{}", std::process::id()));
    std::fs::create_dir(&directory).unwrap();
    let path = directory.join("normalized.wav");
    let result = session.execute(
        "file.exportMedia",
        json!({
            "path": path, "format": "wav", "wait": true,
            "range": "custom", "startSeconds": 0, "endSeconds": 3,
            "settings": {"loudnessLufs": -14}
        }),
    );
    assert!(result.is_ok(), "{result:?}");
    let source = filmcraft_media::wav::WavSource::parse("normalized.wav", std::fs::read(&path).unwrap().into()).unwrap();
    let audio = source.audio(0, 3 * 48_000, 48_000).unwrap();
    let channels: Vec<&[f32]> = audio.channels.iter().map(Vec::as_slice).collect();
    let measured = filmcraft_audio_dsp::loudness::measure(&channels, 48_000.0);
    std::fs::remove_file(&path).unwrap();
    std::fs::remove_dir(&directory).unwrap();
    eprintln!("nested loudness settings: measured {:.3} LUFS (target -14)", measured.integrated_lufs);
    assert!((measured.integrated_lufs + 14.0).abs() < 0.5, "measured {} LUFS", measured.integrated_lufs);
}
