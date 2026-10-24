//! Custom sequence presets: saved from a sequence or from explicit settings, listed with the
//! built-ins, used by New Sequence and New Project, renamed and deleted, persisted in the data
//! directory, and hostile names, settings and files refused without harm.

use std::path::PathBuf;

use serde_json::json;

use crate::Session;
use crate::sequence_presets::{CUSTOM_GROUP, CustomSequencePreset, FILE_FORMAT, Fields, SequencePresetLibrary, file_bytes, parse_file};
use filmcraft_project::AudioChannels;
use filmcraft_time::FrameRate;

struct Scratch(PathBuf);

impl Scratch {
    fn new(name: &str) -> Self {
        let d = filmcraft_testkit::workspace_root().join("target").join("sequence-presets-tests").join(format!("{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        Scratch(d)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn anamorphic() -> CustomSequencePreset {
    CustomSequencePreset {
        name: "ZR anamorphic 8044 25p".into(),
        width: 8044,
        height: 3402,
        pixel_aspect: (1, 1),
        frame_rate: FrameRate::FPS_25,
        drop_frame: false,
        fields: Fields::Progressive,
        sample_rate: 48_000,
        audio_master: AudioChannels::Stereo,
        video_tracks: 4,
        audio_tracks: 2,
        audio_track_type: Some(AudioChannels::Mono),
    }
}

fn custom<'a>(list: &'a serde_json::Value, name: &str) -> Option<&'a serde_json::Value> {
    list.as_array().unwrap().iter().find(|p| p["name"] == name && p["custom"] == true)
}

/// Save from explicit settings (New Sequence) and from the active sequence (Sequence Settings),
/// list, make sequences from them, rename and delete.
#[test]
fn custom_presets_save_list_make_sequences_rename_and_delete() {
    let mut s = Session::default();
    // explicit settings: everything New Sequence shows
    let r = s
        .execute(
            "sequence.preset.save",
            json!({"name": "ZR anamorphic 8044 25p", "width": 8044, "height": 3402, "fps": 25, "pixelAspect": [1, 1], "sampleRate": 48000,
                   "mix": "Stereo", "video": 4, "audio": 2, "trackType": "Mono", "fields": "progressive"}),
        )
        .unwrap();
    assert_eq!(r["group"], CUSTOM_GROUP);
    let list = s.execute("sequence.presets", json!({})).unwrap();
    let p = custom(&list, "ZR anamorphic 8044 25p").unwrap();
    assert_eq!(
        (p["width"].clone(), p["height"].clone(), p["video"].clone(), p["audio"].clone(), p["trackType"].clone()),
        (json!(8044), json!(3402), json!(4), json!(2), json!("Mono"))
    );
    assert!(list.as_array().unwrap().iter().any(|p| p["name"] == "HD 1080p 23.976" && p.get("custom").is_none()), "the built-ins are listed too");
    assert_eq!(s.sequence_presets.user, vec![anamorphic()]);
    // New Sequence from it: the picture, the audio and the tracks
    s.execute("file.newSequence", json!({"name": "Cut", "preset": "zr ANAMORPHIC 8044 25p"})).unwrap();
    let q = s.active_sequence().unwrap();
    assert_eq!((q.settings.width, q.settings.height, q.settings.frame_rate, q.settings.par), (8044, 3402, FrameRate::FPS_25, (1, 1)));
    assert_eq!(q.settings.preset, "ZR anamorphic 8044 25p");
    assert_eq!((q.video_tracks.len(), q.audio_tracks.len()), (4, 2));
    assert!(q.audio_tracks.iter().all(|t| t.channels == AudioChannels::Mono));
    // explicit parameters still win (the sequence is then named by its picture)
    s.execute("file.newSequence", json!({"preset": "ZR anamorphic 8044 25p", "fps": 50, "video": 1})).unwrap();
    let q = s.active_sequence().unwrap();
    assert_eq!((q.settings.frame_rate, q.video_tracks.len(), q.settings.preset.as_str()), (FrameRate::FPS_50, 1, "8044x3402 50"));
    // from the active sequence (Sequence Settings' Save Preset): its settings and track layout
    s.execute("file.newSequence", json!({"name": "Squeezed", "preset": "6K Anamorphic 1.33x 6048x3402 59.94", "video": 2, "audio": 5, "mix": "5.1"})).unwrap();
    s.execute("sequence.preset.save", json!({"name": "Squeezed 59.94"})).unwrap();
    let p = s.sequence_presets.find("squeezed 59.94").unwrap().clone();
    assert_eq!((p.width, p.height, p.pixel_aspect, p.frame_rate, p.drop_frame), (6048, 3402, (4, 3), FrameRate::FPS_59_94, true));
    assert_eq!((p.video_tracks, p.audio_tracks, p.audio_master, p.audio_track_type), (2, 5, AudioChannels::Surround51, None));
    // a taken name needs `replace`; built-in names are reserved
    assert!(s.execute("sequence.preset.save", json!({"name": "SQUEEZED 59.94"})).is_err());
    s.execute("sequence.preset.save", json!({"name": "SQUEEZED 59.94", "replace": true, "audio": 1})).unwrap();
    assert_eq!(s.sequence_presets.user[1].name, "Squeezed 59.94", "a replaced preset keeps its name");
    assert_eq!(s.sequence_presets.find("Squeezed 59.94").unwrap().audio_tracks, 1);
    assert!(s.execute("sequence.preset.save", json!({"name": "hd 1080P 23.976"})).is_err(), "a built-in's name");
    // rename and delete
    s.execute("sequence.preset.rename", json!({"name": "squeezed 59.94", "to": "  6K squeezed 59.94 "})).unwrap();
    assert!(s.sequence_presets.find("Squeezed 59.94").is_none() && s.sequence_presets.find("6K squeezed 59.94").is_some());
    assert!(s.execute("sequence.preset.rename", json!({"name": "6K squeezed 59.94", "to": "ZR anamorphic 8044 25p"})).is_err(), "taken");
    assert!(s.execute("sequence.preset.rename", json!({"name": "6K squeezed 59.94", "to": "UHD 2160p 25"})).is_err(), "built-in");
    assert!(s.execute("sequence.preset.rename", json!({"name": "UHD 2160p 25", "to": "Mine"})).is_err(), "built-ins can't be renamed");
    s.execute("sequence.preset.rename", json!({"name": "6K squeezed 59.94", "to": "6K Squeezed 59.94"})).unwrap();
    assert_eq!(s.sequence_presets.find("6k squeezed 59.94").unwrap().name, "6K Squeezed 59.94", "a change of case is a rename");
    s.execute("sequence.preset.delete", json!({"name": "6K Squeezed 59.94"})).unwrap();
    assert!(s.execute("sequence.preset.delete", json!({"name": "6K Squeezed 59.94"})).is_err());
    assert!(s.execute("sequence.preset.delete", json!({"name": "HD 1080p 23.976"})).is_err(), "built-ins can't be deleted");
    assert!(s.execute("file.newSequence", json!({"preset": "6K Squeezed 59.94"})).is_err(), "deleted");
    assert_eq!(s.sequence_presets.user.len(), 1);
}

/// New Project can start with a sequence made from a preset.
#[test]
fn a_new_project_starts_with_a_sequence_from_a_preset() {
    let mut s = Session::default();
    s.sequence_presets.save(anamorphic(), false).unwrap();
    s.execute("file.newProject", json!({"name": "Anamorphic", "sequencePreset": "ZR anamorphic 8044 25p"})).unwrap();
    assert_eq!(s.project.name, "Anamorphic");
    let q = s.active_sequence().unwrap();
    assert_eq!((q.settings.width, q.video_tracks.len()), (8044, 4));
    assert_eq!(s.project.sequences().count(), 1);
    s.execute("file.newProject", json!({"name": "Built-in", "sequencePreset": "6K Anamorphic 1.33x desqueezed 8044x3402 23.976"})).unwrap();
    assert_eq!(s.active_sequence().unwrap().settings.preset, "6K Anamorphic 1.33x desqueezed 8044x3402 23.976");
    // an unknown preset changes nothing
    for bad in [json!("No such preset"), json!(""), json!(42)] {
        assert!(s.execute("file.newProject", json!({"name": "Bad", "sequencePreset": bad.clone()})).is_err(), "{bad}");
        assert_eq!(s.project.name, "Built-in", "{bad}");
    }
    // without one, an empty project as before
    s.execute("file.newProject", json!({"name": "Empty"})).unwrap();
    assert_eq!(s.project.sequences().count(), 0);
}

/// Hostile names and settings are refused and save nothing.
#[test]
fn hostile_custom_presets_are_refused() {
    let mut s = Session::default();
    for bad in [
        json!({}),
        json!({"name": ""}),
        json!({"name": "   "}),
        json!({"name": "a\nb"}),
        json!({"name": "x".repeat(129)}),
        json!({"name": 7}),
        json!({"name": "W", "width": 0}),
        json!({"name": "W", "width": 40000}),
        json!({"name": "W", "width": -1}),
        json!({"name": "W", "fps": 0}),
        json!({"name": "W", "fps": 25, "dropFrame": true}),
        json!({"name": "W", "pixelAspect": [0, 1]}),
        json!({"name": "W", "pixelAspect": [9, 1]}),
        json!({"name": "W", "sampleRate": 0}),
        json!({"name": "W", "mix": "7.1"}),
        json!({"name": "W", "video": 257}),
        json!({"name": "W", "trackType": "quad"}),
        json!({"name": "W", "fields": "upper"}),
        json!({"name": "W", "sequence": 987654}),
        json!({"name": "W", "sequence": "x"}),
    ] {
        assert!(s.execute("sequence.preset.save", bad.clone()).is_err(), "{bad}");
    }
    assert!(s.sequence_presets.user.is_empty());
    for bad in [json!({}), json!({"name": "nope", "to": "x"}), json!({"name": "nope"})] {
        assert!(s.execute("sequence.preset.rename", bad.clone()).is_err(), "{bad}");
        assert!(s.execute("sequence.preset.delete", bad.clone()).is_err(), "{bad}");
    }
    s.sequence_presets.save(anamorphic(), false).unwrap();
    for to in ["", "  ", "a\tb"] {
        assert!(s.execute("sequence.preset.rename", json!({"name": "ZR anamorphic 8044 25p", "to": to})).is_err(), "{to:?}");
    }
    assert_eq!(s.sequence_presets.user, vec![anamorphic()]);
}

/// The presets persist in the data directory, round-trip through serde, and a damaged or foreign
/// file loads as no presets (bad entries are skipped, the good ones kept).
#[test]
fn custom_presets_persist_and_damaged_files_are_survived() {
    let dir = Scratch::new("persist");
    let mut lib = SequencePresetLibrary::default();
    lib.set_dir(&dir.0);
    assert!(lib.user.is_empty());
    lib.save(anamorphic(), false).unwrap();
    let file = dir.0.join("sequence-presets.json");
    let bytes = std::fs::read(&file).unwrap();
    assert!(String::from_utf8_lossy(&bytes).contains(FILE_FORMAT));
    let mut again = SequencePresetLibrary::default();
    again.set_dir(&dir.0);
    assert_eq!(again.user, vec![anamorphic()]);
    again.rename("ZR anamorphic 8044 25p", "Renamed").unwrap();
    let mut third = SequencePresetLibrary::default();
    third.set_dir(&dir.0);
    assert_eq!(third.user[0].name, "Renamed");
    third.delete("Renamed").unwrap();
    let mut fourth = SequencePresetLibrary::default();
    fourth.set_dir(&dir.0);
    assert!(fourth.user.is_empty());
    // serde round trip, field by field
    let json = serde_json::to_value(anamorphic()).unwrap();
    assert_eq!(json["pixelAspect"], json!([1, 1]));
    assert_eq!(json["fields"], "progressive");
    assert_eq!(serde_json::from_value::<CustomSequencePreset>(json).unwrap(), anamorphic());
    assert_eq!(parse_file(&file_bytes(&[anamorphic()])).unwrap(), vec![anamorphic()]);
    // damaged and foreign files
    for bad in [
        &b""[..],
        b"{",
        b"[]",
        b"{\"format\":\"filmcraft.export-presets\",\"version\":1,\"presets\":[]}",
        b"{\"format\":\"filmcraft.sequence-presets\",\"version\":2}",
    ] {
        assert!(parse_file(bad).is_err(), "{}", String::from_utf8_lossy(bad));
        std::fs::write(&file, bad).unwrap();
        let mut lib = SequencePresetLibrary::default();
        lib.set_dir(&dir.0);
        assert!(lib.user.is_empty());
    }
    // entries a sequence can't have, built-in or duplicate names are skipped; the rest load
    let mut zero = serde_json::to_value(anamorphic()).unwrap();
    zero["width"] = json!(0);
    zero["name"] = json!("Zero");
    let mut builtin = serde_json::to_value(anamorphic()).unwrap();
    builtin["name"] = json!("HD 1080p 23.976");
    let mut dup = serde_json::to_value(anamorphic()).unwrap();
    dup["name"] = json!("zr ANAMORPHIC 8044 25P");
    let mut par = serde_json::to_value(anamorphic()).unwrap();
    par["name"] = json!("Par");
    par["pixelAspect"] = json!([0, 0]);
    let mut tracks = serde_json::to_value(anamorphic()).unwrap();
    tracks["name"] = json!("Tracks");
    tracks["videoTracks"] = json!(100000);
    let file_json = json!({"format": FILE_FORMAT, "version": 1, "presets": [zero, anamorphic(), builtin, dup, par, tracks]});
    std::fs::write(&file, serde_json::to_vec(&file_json).unwrap()).unwrap();
    let mut lib = SequencePresetLibrary::default();
    lib.set_dir(&dir.0);
    assert_eq!(lib.user, vec![anamorphic()]);
}
