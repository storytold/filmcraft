//! Settings: schema, persistence, defaults, migration, validation, and every engine-side behaviour
//! the settings drive.

use std::path::Path;

use filmcraft_project::{ItemId, ItemKind, Label, ParamValue};
use filmcraft_time::{FrameRate, Tick};
use serde_json::{Value, json};

use crate::Session;
use crate::autosave::Preferences;
use crate::media_test_util::tmp_dir;
use crate::settings::{self, Kind};

fn demo() -> Session {
    let mut s = Session::default();
    s.execute("file.openDemoProject", json!({})).unwrap();
    s
}

fn set(s: &mut Session, key: &str, value: Value) {
    s.execute("prefs.set", json!({"key": key, "value": value})).unwrap_or_else(|e| panic!("{key}: {e}"));
}

#[test]
fn interface_language_persists_and_rejects_hostile_values() {
    let dir = tmp_dir("interface-language");
    let path = dir.join("preferences.json");
    let mut s = Session { prefs_path: Some(path.clone()), ..Session::default() };
    set(&mut s, "general.interfaceLanguage", json!("es"));
    assert_eq!(Preferences::load(&path).general.interface_language, "es");
    for value in [json!("xx"), json!("es-MX"), json!("de-DE"), json!("fr-FR"), json!(null), json!(-1), json!({}), json!("x".repeat(4096))] {
        assert!(s.execute("prefs.set", json!({"key": "general.interfaceLanguage", "value": value})).is_err());
        assert_eq!(s.prefs.general.interface_language, "es");
    }
    assert_eq!(Preferences::load(&path).general.interface_language, "es");
    set(&mut s, "general.interfaceLanguage", json!("de"));
    assert_eq!(Preferences::load(&path).general.interface_language, "de");
    set(&mut s, "general.interfaceLanguage", json!("fr"));
    assert_eq!(Preferences::load(&path).general.interface_language, "fr");
    // Indonesian is stored as `id`, not a locale tag or the legacy `in` code
    for value in [json!("id-ID"), json!("in"), json!("ID")] {
        assert!(s.execute("prefs.set", json!({"key": "general.interfaceLanguage", "value": value})).is_err());
    }
    set(&mut s, "general.interfaceLanguage", json!("id"));
    assert_eq!(Preferences::load(&path).general.interface_language, "id");
    // Italian is stored as `it`, not a locale tag
    for value in [json!("it-IT"), json!("IT")] {
        assert!(s.execute("prefs.set", json!({"key": "general.interfaceLanguage", "value": value})).is_err());
    }
    set(&mut s, "general.interfaceLanguage", json!("it"));
    assert_eq!(Preferences::load(&path).general.interface_language, "it");
    // Korean and Czech likewise store only their bare codes.
    for value in [json!("ko-KR"), json!("KO")] {
        assert!(s.execute("prefs.set", json!({"key": "general.interfaceLanguage", "value": value})).is_err());
    }
    set(&mut s, "general.interfaceLanguage", json!("ko"));
    assert_eq!(Preferences::load(&path).general.interface_language, "ko");
    for value in [json!("cs-CZ"), json!("CS")] {
        assert!(s.execute("prefs.set", json!({"key": "general.interfaceLanguage", "value": value})).is_err());
    }
    set(&mut s, "general.interfaceLanguage", json!("cs"));
    assert_eq!(Preferences::load(&path).general.interface_language, "cs");
    set(&mut s, "general.interfaceLanguage", json!("pl"));
    assert_eq!(Preferences::load(&path).general.interface_language, "pl");
    // Swedish is stored as `sv`, not a locale tag
    for value in [json!("sv-SE"), json!("sv-FI"), json!("SV")] {
        assert!(s.execute("prefs.set", json!({"key": "general.interfaceLanguage", "value": value})).is_err());
    }
    set(&mut s, "general.interfaceLanguage", json!("sv"));
    assert_eq!(Preferences::load(&path).general.interface_language, "sv");
    // Turkish is stored as `tr`, not a locale tag
    for value in [json!("tr-TR"), json!("TR")] {
        assert!(s.execute("prefs.set", json!({"key": "general.interfaceLanguage", "value": value})).is_err());
    }
    set(&mut s, "general.interfaceLanguage", json!("tr"));
    assert_eq!(Preferences::load(&path).general.interface_language, "tr");
    // back to following the operating system (#218)
    set(&mut s, "general.interfaceLanguage", json!("system"));
    assert_eq!(Preferences::load(&path).general.interface_language, "system");
    std::fs::remove_dir_all(dir).unwrap();
}

/// A new installation follows the operating system's language (#218); a saved choice is kept.
#[test]
fn interface_language_defaults_to_the_system_language() {
    assert_eq!(Preferences::default().general.interface_language, "system");
    let dir = tmp_dir("interface-language-default");
    let path = dir.join("preferences.json");
    assert_eq!(Preferences::load(&path).general.interface_language, "system", "no preferences file yet");
    std::fs::write(&path, r#"{"general":{"interfaceLanguage":"en"}}"#).unwrap();
    assert_eq!(Preferences::load(&path).general.interface_language, "en");
    std::fs::remove_dir_all(dir).unwrap();
}

fn write_png(path: &Path, w: u32, h: u32) {
    image::RgbaImage::from_pixel(w, h, image::Rgba([200, 40, 40, 255])).save(path).unwrap();
}

/// Import a `w`×`h` PNG; returns its item.
fn import_png(s: &mut Session, dir: &Path, name: &str, w: u32, h: u32) -> ItemId {
    let p = dir.join(name);
    write_png(&p, w, h);
    let r = s.execute("file.import", json!({"paths": [p.to_string_lossy()]})).unwrap();
    ItemId(r["items"][0].as_u64().unwrap_or_else(|| panic!("{r}")))
}

#[test]
fn categories_follow_premiere_order() {
    let ids: Vec<&str> = settings::categories().iter().map(|c| c.id).collect();
    assert_eq!(
        ids,
        [
            "general",
            "appearance",
            "audio",
            "audioHardware",
            "autoSave",
            "color",
            "graphics",
            "labels",
            "media",
            "mediaAnalysis",
            "mediaCache",
            "memory",
            "playback",
            "plugins",
            "timeline",
            "trim"
        ]
    );
}

#[test]
fn every_schema_key_exists_with_a_valid_default() {
    let p = Preferences::default();
    let keys = p.keys();
    let fields = settings::fields();
    assert!(fields.len() > 140, "{} fields", fields.len());
    let mut seen = std::collections::HashSet::new();
    for (cat, f) in fields {
        assert!(seen.insert(f.key), "duplicate {}", f.key);
        assert!(f.key.starts_with(&format!("{cat}.")), "{} is not under {cat}", f.key);
        let v = p.get(f.key).unwrap_or_else(|| panic!("no preference {}", f.key));
        assert!(keys.iter().any(|k| k == f.key), "{} not a leaf key", f.key);
        settings::validate(f.key, &v).unwrap_or_else(|e| panic!("default of {}: {e}", f.key));
        match f.kind {
            Kind::Bool => assert!(v.is_boolean(), "{}", f.key),
            Kind::Int { .. } | Kind::Float { .. } => assert!(v.is_number(), "{}", f.key),
            Kind::Duration { unit_key } => assert!(v.is_number() && p.get(unit_key).is_some(), "{}", f.key),
            _ => {}
        }
        if let Some(by) = f.enabled_by {
            assert!(p.get(by).is_some_and(|v| v.is_boolean()), "{} enabled by {by}", f.key);
        }
    }
    // Premiere's factory defaults
    assert_eq!(p.get("timeline.videoTransitionDuration"), Some(json!(30.0)));
    assert_eq!(p.get("timeline.videoTransitionUnit"), Some(json!("frames")));
    assert_eq!(p.get("timeline.stillImageDuration"), Some(json!(5.0)));
    assert_eq!(p.get("playback.stepManyFrames"), Some(json!(5)));
    assert_eq!(p.get("media.indeterminateTimebase"), Some(json!("29.97df")));
    assert_eq!(p.get("labels.colors.violet.color"), Some(json!("#380ea7")));
    assert_eq!(p.get("labels.defaults.movie"), Some(json!("Iris")));
    assert_eq!(p.get("audioHardware.bufferSize"), Some(json!(512)));
    assert_eq!(p.version, settings::PREFS_VERSION);
}

#[test]
fn prefs_persist_through_the_data_directory() {
    let dir = tmp_dir("prefs-persist");
    let mut s = Session::default();
    s.start_autosave(crate::autosave::AutosaveConfig::new(&dir)).unwrap();
    s.execute(
        "prefs.set",
        json!({"values": {"timeline.stillImageDuration": 3, "appearance.colorTheme": "light", "labels.colors.rose.name": "Hero", "audioHardware.sampleRate": "96000"}}),
    )
    .unwrap();
    s.shutdown();
    let file: Value = serde_json::from_slice(&std::fs::read(dir.join("preferences.json")).unwrap()).unwrap();
    assert_eq!(file["version"], settings::PREFS_VERSION);
    assert_eq!(file["timeline"]["stillImageDuration"], 3.0);

    let mut s2 = Session::default();
    s2.start_autosave(crate::autosave::AutosaveConfig::new(&dir)).unwrap();
    assert_eq!(s2.prefs.timeline.still_image_duration, 3.0);
    assert_eq!(s2.prefs.appearance.color_theme, "light");
    assert_eq!(s2.prefs.labels.name(Label::Rose), "Hero");
    assert_eq!(s2.prefs.audio_hardware.sample_rate, 96_000);
    let got = s2.execute("prefs.get", json!({"key": "labels.colors.rose.name"})).unwrap();
    assert_eq!(got, "Hero");
    s2.shutdown();
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn v1_preferences_migrate_and_bad_values_are_repaired() {
    let dir = tmp_dir("prefs-migrate");
    let path = dir.join("preferences.json");
    // a v1 file: only the old groups, no version
    let v1 = json!({
        "autoSave": {"enabled": false, "intervalMinutes": 9, "maxVersions": 3},
        "playback": {"prerollSeconds": 4, "postrollSeconds": 1},
        "trim": {"largeTrimOffset": 12, "playheadDeterminesLoop": true},
        "media": {"enableProxies": true},
        "guides": {"templates": [{"name": "T", "guides": [{"vertical": true, "position": 10.0}]}]}
    });
    std::fs::write(&path, serde_json::to_vec(&v1).unwrap()).unwrap();
    let p = Preferences::load(&path);
    assert_eq!(p.version, settings::PREFS_VERSION);
    assert!(!p.auto_save.enabled);
    assert_eq!((p.auto_save.interval_minutes, p.auto_save.max_versions), (9, 3));
    assert_eq!((p.playback.preroll_seconds, p.playback.postroll_seconds), (4.0, 1.0));
    assert_eq!(p.playback.step_many_frames, 5, "new key gets its default");
    assert_eq!((p.trim.large_trim_offset, p.trim.playhead_determines_loop), (12, true));
    assert!(p.media.enable_proxies);
    assert_eq!(p.media.default_media_scaling, "none");
    assert_eq!(p.guides.templates[0].name, "T");
    assert_eq!(p.timeline, settings::TimelinePrefs::default());
    assert_eq!(p.labels.colors.len(), 16);

    // values a hand-edited (or future) file may carry are repaired, not fatal
    let bad = json!({
        "version": 2,
        "timeline": {"autoScroll": "sideways", "stillImageDuration": -4, "stillImageUnit": "fortnights", "videoTransitionDuration": 1e9},
        "appearance": {"highlightColor": "blue"},
        "memory": {"frameCacheMb": 1},
        "labels": {"colors": {"violet": {"name": "Purple-ish", "color": "#zzzzzz"}, "nope": {"name": "x", "color": "#000000"}}},
        "unknownCategory": {"x": 1}
    });
    std::fs::write(&path, serde_json::to_vec(&bad).unwrap()).unwrap();
    let p = Preferences::load(&path);
    assert_eq!(p.timeline.auto_scroll, "pageScroll");
    assert_eq!(p.timeline.still_image_unit, "seconds");
    assert_eq!(p.timeline.still_image_duration, 0.01);
    assert_eq!(p.timeline.video_transition_duration, 100_000.0);
    assert_eq!(p.appearance.highlight_color, settings::DEFAULT_HIGHLIGHT);
    assert_eq!(p.memory.frame_cache_mb, 64);
    assert_eq!(p.labels.name(Label::Violet), "Purple-ish");
    assert_eq!(p.labels.rgb(Label::Violet), Label::Violet.rgb());
    assert!(!p.labels.colors.contains_key("nope"));
    // a broken file falls back to defaults
    std::fs::write(&path, b"{not json").unwrap();
    assert_eq!(Preferences::load(&path), Preferences::default());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn prefs_set_validates_and_reset_works_per_category() {
    let mut s = Session::default();
    let e = s.execute("prefs.set", json!({"key": "timeline.autoScroll", "value": "sideways"})).unwrap_err().to_string();
    assert!(e.contains("noScroll"), "{e}");
    assert!(s.execute("prefs.set", json!({"key": "appearance.highlightColor", "value": "blue"})).is_err());
    assert!(s.execute("prefs.set", json!({"key": "timeline.stillImageUnit", "value": "weeks"})).is_err());
    assert!(s.execute("prefs.set", json!({"key": "labels.defaults.movie", "value": "Chartreuse"})).is_err());
    assert!(s.execute("prefs.set", json!({"key": "nope.key", "value": 1})).is_err());
    // numeric choices accept numbers and strings; ints accept floats; ranges clamp
    set(&mut s, "audioHardware.bufferSize", json!("1024"));
    assert_eq!(s.prefs.audio_hardware.buffer_size, 1024);
    set(&mut s, "audioHardware.bufferSize", json!(256));
    assert_eq!(s.prefs.audio_hardware.buffer_size, 256);
    assert!(s.execute("prefs.set", json!({"key": "audioHardware.bufferSize", "value": 300})).is_err());
    set(&mut s, "playback.stepManyFrames", json!(12.0));
    assert_eq!(s.prefs.playback.step_many_frames, 12);
    set(&mut s, "memory.frameCacheMb", json!(10_000_000));
    assert_eq!(s.prefs.memory.frame_cache_mb, 65_536);
    set(&mut s, "labels.defaults.movie", json!("Rose"));
    set(&mut s, "timeline.autoScroll", json!("smoothScroll"));
    set(&mut s, "general.showToolTips", json!(false));
    s.prefs.general.recent_projects = vec!["/a.fcproj".into()];
    // reset one category
    s.execute("prefs.reset", json!({"category": "timeline"})).unwrap();
    assert_eq!(s.prefs.timeline.auto_scroll, "pageScroll");
    assert_eq!(s.prefs.labels.defaults.movie, Label::Rose, "other categories keep their values");
    s.execute("prefs.reset", json!({"category": "general"})).unwrap();
    assert!(s.prefs.general.show_tool_tips);
    assert_eq!(s.prefs.general.recent_projects, ["/a.fcproj"], "recent projects survive a reset");
    assert!(s.execute("prefs.reset", json!({"category": "nope"})).is_err());
    s.execute("prefs.reset", json!({})).unwrap();
    assert_eq!(s.prefs, Preferences::default());
}

#[test]
fn schema_command_describes_every_category() {
    let mut s = Session::default();
    let r = s.execute("prefs.schema", json!({})).unwrap();
    let cats = r["categories"].as_array().unwrap();
    assert_eq!(cats.len(), settings::categories().len());
    let tl = cats.iter().find(|c| c["id"] == "timeline").unwrap();
    assert_eq!(tl["command"], "app.settings.timeline");
    let auto = tl["fields"].as_array().unwrap().iter().find(|f| f["key"] == "timeline.autoScroll").unwrap();
    assert_eq!(auto["value"], "pageScroll");
    assert_eq!(auto["wired"], true);
    assert!(auto["kind"]["choices"].as_array().unwrap().iter().any(|c| c["value"] == "smoothScroll"));

    // `file.open` restores the saved tabs and views with this on (#928)
    let restore = tl["fields"].as_array().unwrap().iter().find(|f| f["key"] == "timeline.restoreOpenSequences").unwrap();
    assert_eq!((restore["wired"].clone(), restore["value"].clone()), (json!(true), json!(true)));

    // This option is stored, but Insert/Overwrite does not yet implement focus transfer.
    let focus = tl["fields"].as_array().unwrap().iter().find(|f| f["key"] == "timeline.focusTimelineOnEdit").unwrap();
    assert_eq!(focus["kind"], json!({"type": "bool"}));
    assert_eq!(focus["wired"], false);
    assert_eq!(focus["value"], false);
    set(&mut s, "timeline.focusTimelineOnEdit", json!(true));
    let updated = s.execute("prefs.schema", json!({"category": "timeline"})).unwrap();
    let focus = updated["categories"][0]["fields"].as_array().unwrap().iter().find(|f| f["key"] == "timeline.focusTimelineOnEdit").unwrap();
    assert_eq!(focus["value"], true);
    assert_eq!(focus["wired"], false);

    let one = s.execute("prefs.schema", json!({"category": "trim"})).unwrap();
    assert_eq!(one["categories"].as_array().unwrap().len(), 1);
    assert!(s.execute("prefs.schema", json!({"category": "nope"})).is_err());
}

#[test]
fn still_image_default_duration_and_indeterminate_timebase() {
    let dir = tmp_dir("prefs-still");
    let mut s = demo();
    let rate = s.sequence_rate();
    set(&mut s, "media.indeterminateTimebase", json!("25"));
    let still = import_png(&mut s, &dir, "a.png", 64, 36);
    let m = s.project.item(still).unwrap().as_media().unwrap();
    assert_eq!(m.info.frame_rate(), FrameRate::FPS_25, "stills take the indeterminate media timebase");
    set(&mut s, "timeline.stillImageDuration", json!(3));
    let end = s.active_sequence().unwrap().duration();
    let r = s.execute("timeline.place", json!({"item": still.0, "track": "V1", "time": end.0})).unwrap();
    let clip = filmcraft_project::ClipId(r["clips"][0].as_u64().unwrap());
    let dur = s.active_sequence().unwrap().find_item(clip).unwrap().1.duration;
    assert_eq!(dur, rate.snap_nearest(Tick::from_seconds_f64(3.0)));
    // in frames
    s.execute("prefs.set", json!({"values": {"timeline.stillImageUnit": "frames", "timeline.stillImageDuration": 10}})).unwrap();
    let end = s.active_sequence().unwrap().duration();
    let r = s.execute("timeline.place", json!({"item": still.0, "track": "V1", "time": end.0})).unwrap();
    let clip = filmcraft_project::ClipId(r["clips"][0].as_u64().unwrap());
    assert_eq!(s.active_sequence().unwrap().find_item(clip).unwrap().1.duration, rate.tick_of(10));
    // Source monitor insert of a still uses it too
    s.execute("source.open", json!({"item": still.0})).unwrap();
    let (_, range) = crate::commands::source_range(&s).unwrap();
    assert_eq!(range.duration, rate.tick_of(10));
    let _ = std::fs::remove_dir_all(&dir);
}

fn transition_duration(s: &Session, r: &Value) -> Tick {
    let id = r["transition"].as_u64().unwrap_or_else(|| panic!("{r}"));
    let q = s.active_sequence().unwrap();
    q.all_tracks().flat_map(|t| t.transitions.iter()).find(|t| t.id.0 == id).unwrap().duration
}

#[test]
fn default_transition_durations() {
    let mut s = demo();
    let rate = s.sequence_rate();
    let q = s.active_sequence().unwrap();
    let cut = q.video_tracks[0].items[2].start;
    let aclip = q.audio_tracks[0].items[1].id.0;
    set(&mut s, "timeline.videoTransitionDuration", json!(10));
    s.execute("playhead.set", json!({"time": cut.0})).unwrap();
    let r = s.execute("sequence.applyVideoTransition", json!({})).unwrap();
    assert_eq!(transition_duration(&s, &r), rate.tick_of(10));
    // audio: 0.5 seconds, snapped to frames
    s.execute("prefs.set", json!({"values": {"timeline.audioTransitionDuration": 0.5, "timeline.audioTransitionUnit": "seconds"}})).unwrap();
    let r = s.execute("sequence.applyAudioTransition", json!({"clip": aclip})).unwrap();
    assert_eq!(transition_duration(&s, &r), rate.snap_nearest(Tick::from_seconds_f64(0.5)));
    // explicit frames still win
    let cut2 = s.active_sequence().unwrap().video_tracks[0].items[3].start;
    s.execute("playhead.set", json!({"time": cut2.0})).unwrap();
    let r = s.execute("sequence.applyVideoTransition", json!({"frames": 4})).unwrap();
    assert_eq!(transition_duration(&s, &r), rate.tick_of(4));
}

#[test]
fn step_many_frames() {
    let mut s = demo();
    let rate = s.sequence_rate();
    s.execute("playhead.set", json!({"frame": 50})).unwrap();
    s.execute("playhead.stepForward5", json!({})).unwrap();
    assert_eq!(rate.frame_at(s.playhead()), 55);
    set(&mut s, "playback.stepManyFrames", json!(12));
    s.execute("playhead.stepForward5", json!({})).unwrap();
    assert_eq!(rate.frame_at(s.playhead()), 67);
    s.execute("playhead.stepBack5", json!({})).unwrap();
    s.execute("playhead.stepBack5", json!({})).unwrap();
    assert_eq!(rate.frame_at(s.playhead()), 43);
}

#[test]
fn label_defaults_names_and_colours() {
    let dir = tmp_dir("prefs-labels");
    let mut s = Session::default();
    let a = import_png(&mut s, &dir, "a.png", 32, 18);
    assert_eq!(s.project.item(a).unwrap().label, Label::Lavender);
    set(&mut s, "labels.defaults.still", json!("Yellow"));
    let b = import_png(&mut s, &dir, "b.png", 32, 18);
    assert_eq!(s.project.item(b).unwrap().label, Label::Yellow);
    set(&mut s, "labels.defaults.sequence", json!("Teal"));
    let r = s.execute("file.newSequence", json!({"name": "S"})).unwrap();
    let seq = ItemId(r["sequence"].as_u64().unwrap());
    assert_eq!(s.project.item(seq).unwrap().label, Label::Teal);
    // movie / video-only / audio-only media
    let l = &s.prefs.labels;
    use filmcraft_media::MediaKind as K;
    assert_eq!(l.for_media(K::Movie, true, true), Label::Iris);
    assert_eq!(l.for_media(K::Movie, true, false), Label::Violet);
    assert_eq!(l.for_media(K::AudioOnly, false, true), Label::Caribbean);
    // editable names and colours
    s.execute("prefs.set", json!({"values": {"labels.colors.teal.name": "B-roll", "labels.colors.teal.color": "#00ff80"}})).unwrap();
    assert_eq!(s.prefs.labels.name(Label::Teal), "B-roll");
    assert_eq!(s.prefs.labels.rgb(Label::Teal), [0, 255, 128]);
    assert!(s.execute("prefs.set", json!({"key": "labels.colors.teal.color", "value": "green"})).is_err());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn default_media_scaling() {
    let dir = tmp_dir("prefs-scaling");
    let mut s = demo();
    let (w, h) = s.active_sequence().map(|q| (q.settings.width, q.settings.height)).unwrap();
    let big = import_png(&mut s, &dir, "big.png", w * 2, h * 2);
    let place = |s: &mut Session| {
        let end = s.active_sequence().unwrap().duration();
        let r = s.execute("timeline.place", json!({"item": big.0, "track": "V1", "time": end.0})).unwrap();
        let c = filmcraft_project::ClipId(r["clips"][0].as_u64().unwrap());
        s.active_sequence().unwrap().find_item(c).unwrap().1.clone()
    };
    let none = place(&mut s);
    assert!(!none.scale_to_frame);
    assert_eq!(none.effect("motion").unwrap().params["scale"].value, ParamValue::Float(100.0));
    set(&mut s, "media.defaultMediaScaling", json!("scaleToFrameSize"));
    assert!(place(&mut s).scale_to_frame);
    set(&mut s, "media.defaultMediaScaling", json!("setToFrameSize"));
    let set_to = place(&mut s);
    assert!(!set_to.scale_to_frame);
    assert_eq!(set_to.effect("motion").unwrap().params["scale"].value, ParamValue::Float(50.0));
    // media already at frame size is left alone
    let fit = import_png(&mut s, &dir, "fit.png", w, h);
    let end = s.active_sequence().unwrap().duration();
    let r = s.execute("timeline.place", json!({"item": fit.0, "track": "V1", "time": end.0})).unwrap();
    let c = filmcraft_project::ClipId(r["clips"][0].as_u64().unwrap());
    let it = s.active_sequence().unwrap().find_item(c).unwrap().1.clone();
    assert_eq!(it.effect("motion").unwrap().params["scale"].value, ParamValue::Float(100.0));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn recent_projects_are_remembered() {
    let dir = tmp_dir("prefs-recent");
    let mut s = demo();
    let a = dir.join("a.fcproj").to_string_lossy().into_owned();
    let b = dir.join("b.fcproj").to_string_lossy().into_owned();
    s.execute("file.saveAs", json!({"path": a})).unwrap();
    s.execute("file.saveAs", json!({"path": b})).unwrap();
    s.execute("file.open", json!({"path": a})).unwrap();
    assert_eq!(s.prefs.general.recent_projects, [a.clone(), b.clone()]);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn media_cache_location_policy_and_clean() {
    let dir = tmp_dir("prefs-cache");
    let mut s = Session::default();
    s.start_autosave(crate::autosave::AutosaveConfig::new(dir.join("data"))).unwrap();
    let cache = s.media_cache_dir().unwrap();
    assert_eq!(cache, dir.join("data").join("Media Cache"));
    // unsaved projects keep their previews in the media cache
    assert!(s.previews.dir().unwrap().starts_with(cache.join("Previews")), "{:?}", s.previews.dir());
    // a custom location moves them, into a folder of FilmCraft's own inside it
    let location = dir.join("cache2");
    set(&mut s, "mediaCache.location", json!(location.to_string_lossy()));
    let custom = location.join(settings::CUSTOM_CACHE_FOLDER);
    assert_eq!(s.media_cache_dir().unwrap(), custom);
    assert!(s.previews.dir().unwrap().starts_with(custom.join("Previews")));
    // stale files from earlier sessions
    let stale = custom.join("Previews").join("untitled-1-old");
    std::fs::create_dir_all(&stale).unwrap();
    let old = stale.join("0123456789abcdef0123456789abcdef.mov");
    std::fs::write(&old, vec![0u8; 1000]).unwrap();
    let ten_days = std::time::Duration::from_secs(10 * 86_400);
    std::fs::File::options().write(true).open(&old).unwrap().set_modified(std::time::SystemTime::now() - ten_days).unwrap();
    let fresh = stale.join("fedcba9876543210fedcba9876543210.wav");
    std::fs::write(&fresh, vec![0u8; 500]).unwrap();
    let mine = s.previews.dir().unwrap();
    std::fs::create_dir_all(&mine).unwrap();
    std::fs::write(mine.join("00000000000000000000000000000000.mov"), vec![0u8; 100]).unwrap();
    let info = s.execute("mediaCache.info", json!({})).unwrap();
    assert_eq!((info["files"].as_u64(), info["bytes"].as_u64()), (Some(3), Some(1600)));
    // "older than 5 days" removes only the old file
    let mut p = settings::MediaCachePrefs { management: "olderThan".into(), older_than_days: 5, ..Default::default() };
    assert_eq!(settings::enforce_policy(&custom, &p, Some(&mine), std::time::SystemTime::now()), (1, 1000));
    assert!(!old.exists() && fresh.exists());
    // "exceeds size" with a 0 GB limit removes everything but the open project's previews
    p.management = "exceedsSize".into();
    p.max_size_gb = 0;
    settings::enforce_policy(&custom, &p, Some(&mine), std::time::SystemTime::now());
    assert!(!fresh.exists() && !stale.exists());
    assert!(mine.join("00000000000000000000000000000000.mov").exists());
    // Delete… keeps the open project's files unless `all`
    let r = s.execute("mediaCache.clean", json!({})).unwrap();
    assert_eq!(r["files"], 0);
    let r = s.execute("mediaCache.clean", json!({"all": true})).unwrap();
    assert_eq!(r["files"], 1);
    s.shutdown();
    let _ = std::fs::remove_dir_all(&dir);
}

/// #2772: a custom media cache location is a folder the user picked (the home folder, a footage
/// folder), so the policy and "Delete…" work in a folder of FilmCraft's own inside it and leave
/// everything else alone; neither a symlink inside the cache nor one in place of it is followed.
#[test]
fn media_cache_cleaning_keeps_the_files_of_the_chosen_folder() {
    let dir = tmp_dir("prefs-cache-keep");
    let mut s = Session::default();
    s.start_autosave(crate::autosave::AutosaveConfig::new(dir.join("data"))).unwrap();
    let location = dir.join("Videos");
    let footage = location.join("holiday");
    std::fs::create_dir_all(&footage).unwrap();
    let keep = location.join("keep.txt");
    std::fs::write(&keep, b"mine").unwrap();
    std::fs::write(footage.join("clip.mov"), vec![0u8; 1000]).unwrap();
    std::fs::create_dir_all(location.join("empty")).unwrap();
    let old = std::time::SystemTime::now() - std::time::Duration::from_secs(10 * 86_400);
    for f in [&keep, &footage.join("clip.mov")] {
        std::fs::File::options().write(true).open(f).unwrap().set_modified(old).unwrap();
    }
    set(&mut s, "mediaCache.location", json!(location.to_string_lossy()));
    let p = settings::MediaCachePrefs { management: "olderThan".into(), older_than_days: 0, ..Default::default() };
    let cache = s.media_cache_dir().unwrap();
    settings::enforce_policy(&cache, &p, None, std::time::SystemTime::now());
    s.execute("mediaCache.clean", json!({"all": true})).unwrap();
    assert!(keep.exists() && footage.join("clip.mov").exists(), "files of the chosen folder are kept");
    assert!(location.join("empty").exists(), "so are its empty folders");
    // An empty folder outside the cache, reached through a symlink inside it, stays.
    #[cfg(unix)]
    {
        let outside = dir.join("outside").join("empty");
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::create_dir_all(&cache).unwrap();
        std::os::unix::fs::symlink(dir.join("outside"), cache.join("link")).unwrap();
        // The link is newer than the policy's age, so it stays and pruning walks past it.
        let p = settings::MediaCachePrefs { management: "olderThan".into(), older_than_days: 5, ..Default::default() };
        settings::enforce_policy(&cache, &p, None, std::time::SystemTime::now());
        assert!(outside.exists(), "pruning followed a symlink out of the cache");
        // A link in place of the cache folder itself, to a folder of the user's: nothing in it goes.
        std::fs::remove_dir_all(&cache).unwrap();
        let documents = dir.join("Documents");
        std::fs::create_dir_all(documents.join("drafts")).unwrap();
        std::fs::write(documents.join("letter.txt"), b"mine").unwrap();
        std::os::unix::fs::symlink(&documents, &cache).unwrap();
        let p = settings::MediaCachePrefs { management: "olderThan".into(), older_than_days: 0, ..Default::default() };
        assert_eq!(settings::enforce_policy(&cache, &p, None, std::time::SystemTime::now()), (0, 0));
        s.execute("mediaCache.clean", json!({"all": true})).unwrap();
        assert!(documents.join("letter.txt").exists() && documents.join("drafts").exists(), "cleaning followed a linked cache folder");
    }
    s.shutdown();
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn output_mapping_and_smart_quotes() {
    let (l, r) = ([0.5f32, 0.25], [-0.5f32, 1.5]);
    let mut out = [9.0f32; 8];
    settings::map_output(&l, &r, &mut out, 4, [2, 3]);
    assert_eq!(out, [0.0, 0.0, 0.5, -0.5, 0.0, 0.0, 0.25, 1.0]);
    let mut st = [0.0f32; 4];
    settings::map_output(&l, &r, &mut st, 2, [1, 0]);
    assert_eq!(st, [-0.5, 0.5, 1.0, 0.25], "swapped");
    settings::map_output(&l, &r, &mut st, 2, [7, 9]);
    assert_eq!(st, [0.5, -0.5, 0.25, 1.0], "out-of-range mapping falls back to 1:1");
    let mut mono = [0.0f32; 2];
    settings::map_output(&l, &r, &mut mono, 1, [0, 1]);
    assert_eq!(mono, [0.0, 0.875]);
    assert_eq!(settings::smart_quotes(r#"He said "it's 'fine'" (ok)"#), "He said “it’s ‘fine’” (ok)");
}

#[test]
fn graphics_text_uses_the_text_settings() {
    let mut s = demo();
    s.execute("graphics.newText", json!({"text": "\"Hi\""})).unwrap();
    let layer = |s: &Session| {
        let c = s.state.selection[0];
        let it = s.active_sequence().unwrap().find_item(c).unwrap().1.clone();
        it.effects.iter().rev().find(|e| e.effect.contains("text")).cloned().unwrap()
    };
    let l = layer(&s);
    assert_eq!(l.params["text"].value, ParamValue::Text("“Hi”".into()));
    assert_eq!(l.params["ligatures"].value, ParamValue::Bool(true));
    s.execute("prefs.set", json!({"values": {"graphics.smartQuotes": false, "graphics.ligatures": false, "graphics.defaultFont": "JetBrains Mono"}})).unwrap();
    s.execute("graphics.newText", json!({"text": "\"Hi\"", "newClip": true})).unwrap();
    let l = layer(&s);
    assert_eq!(l.params["text"].value, ParamValue::Text("\"Hi\"".into()));
    assert_eq!(l.params["ligatures"].value, ParamValue::Bool(false));
    assert_eq!(l.params["font"].value, ParamValue::Text("JetBrains Mono".into()));
}

#[test]
fn auto_transcribe_on_import_and_transcription_defaults() {
    use filmcraft_project::{Transcript, Word};
    let dir = tmp_dir("prefs-transcribe");
    let mov = dir.join("talk.mov");
    crate::media_test_util::make_movie(&mov, filmcraft_media::DemoScene::OceanSunset, 64, 36, 24);
    let mut s = Session::default();
    let mut t = Transcript { language: "en".into(), ..Default::default() };
    t.words.push(Word::new("hello", Tick::ZERO, Tick::from_seconds_f64(0.4)));
    s.transcriber = Some(std::sync::Arc::new(filmcraft_speech::FixedTranscriber { transcript: t, id: "fixed".into() }));
    // off by default
    let r = s.execute("file.import", json!({"paths": [mov.to_string_lossy()]})).unwrap();
    let a = ItemId(r["items"][0].as_u64().unwrap());
    assert!(!s.project.transcripts.contains_key(&a));
    // on, but only for clips in sequences (Premiere's default scope): still nothing on import
    set(&mut s, "mediaAnalysis.autoTranscribe", json!(true));
    let r = s.execute("file.import", json!({"paths": [mov.to_string_lossy()]})).unwrap();
    assert!(!s.project.transcripts.contains_key(&ItemId(r["items"][0].as_u64().unwrap())));
    set(&mut s, "mediaAnalysis.autoTranscribeScope", json!("allImported"));
    let r = s.execute("file.import", json!({"paths": [mov.to_string_lossy()]})).unwrap();
    let c = ItemId(r["items"][0].as_u64().unwrap());
    assert_eq!(s.project.transcripts[&c].words[0].text, "hello");
    assert!(matches!(s.project.item(c).unwrap().kind, ItemKind::Media(_)));
    // the model setting is validated against the catalogue
    assert!(s.execute("prefs.set", json!({"key": "mediaAnalysis.whisperModel", "value": "whisper-huge"})).is_err());
    set(&mut s, "mediaAnalysis.whisperModel", json!("whisper-tiny"));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn auto_transcribe_sequence_clips_after_insertion() {
    use filmcraft_project::{Transcript, Word};
    let dir = tmp_dir("prefs-sequence-transcribe");
    let mov = dir.join("talk.mov");
    crate::media_test_util::make_movie(&mov, filmcraft_media::DemoScene::OceanSunset, 64, 36, 24);
    let mut missing = Vec::new();
    for (command, audio) in [("source.insert", true), ("source.overwrite", true), ("timeline.place", true), ("timeline.place", false)] {
        let mut s = Session::default();
        let mut t = Transcript { language: "en".into(), ..Default::default() };
        t.words.push(Word::new("hello", Tick::ZERO, Tick::from_seconds_f64(0.4)));
        s.transcriber = Some(std::sync::Arc::new(filmcraft_speech::FixedTranscriber { transcript: t, id: "fixed".into() }));
        set(&mut s, "mediaAnalysis.autoTranscribe", json!(true));
        assert_eq!(s.prefs.media_analysis.auto_transcribe_scope, "sequenceClips");
        let r = s.execute("file.import", json!({"paths": [mov.to_string_lossy()]})).unwrap();
        assert_eq!(r["errors"], json!([]));
        let item = ItemId(r["items"][0].as_u64().unwrap());
        assert!(!s.project.transcripts.contains_key(&item));
        s.execute("file.newSequence", json!({"video": 1, "audio": 1})).unwrap();
        s.execute("source.open", json!({"item": item.0})).unwrap();
        let params = if command == "timeline.place" { json!({"item": item.0, "track": "V1", "audioTrack": "A1", "audio": audio}) } else { json!({}) };
        s.execute(command, params).unwrap();
        assert_eq!(s.active_sequence().unwrap().audio_tracks.iter().flat_map(|t| &t.items).any(|c| c.item == item), audio);
        let automatic = s.project.transcripts.contains_key(&item);
        if automatic {
            assert_eq!(s.project.transcripts[&item].words[0].text, "hello");
        }
        s.execute("transcript.generate", json!({"items": [item.0]})).unwrap();
        assert_eq!(s.project.transcripts[&item].words[0].text, "hello");
        if automatic != audio {
            missing.push(command);
        }
    }
    std::fs::remove_dir_all(dir).unwrap();
    assert!(missing.is_empty(), "sequence-scoped transcription did not run: {missing:?}");
}

struct SequenceRecognizer {
    calls: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    outcome: &'static str,
}

impl filmcraft_speech::Transcriber for SequenceRecognizer {
    fn id(&self) -> String {
        "sequence-test".into()
    }
    fn transcribe(
        &self,
        audio: &[f32],
        _: &filmcraft_speech::Options,
        _: filmcraft_speech::ProgressFn,
    ) -> Result<filmcraft_project::Transcript, filmcraft_speech::SpeechError> {
        self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        assert!(!audio.is_empty(), "the synthetic audio must decode");
        match self.outcome {
            "error" => Err(filmcraft_speech::SpeechError::Io("test recognizer failed".into())),
            "panic" => panic!("test recognizer panicked"),
            _ => Ok(filmcraft_project::Transcript {
                words: vec![filmcraft_project::Word::new("hello", Tick::ZERO, Tick::from_seconds_f64(0.4))],
                source: self.id(),
                ..Default::default()
            }),
        }
    }
}

fn sequence_transcription_session(dir: &Path, outcome: &'static str, limit: usize) -> (Session, ItemId, std::sync::Arc<std::sync::atomic::AtomicUsize>) {
    let wav = dir.join("talk.wav");
    std::fs::write(&wav, crate::voiceover::write_wav_f32(&vec![0.125; 16_000], 16_000)).unwrap();
    let calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let mut s = Session::default();
    s.history.limit = limit;
    s.transcriber = Some(std::sync::Arc::new(SequenceRecognizer { calls: calls.clone(), outcome }));
    let r = s.execute("file.import", json!({"paths": [wav.to_string_lossy()]})).unwrap();
    assert_eq!(r["errors"], json!([]));
    let item = ItemId(r["items"][0].as_u64().unwrap());
    s.execute("file.newSequence", json!({"video": 1, "audio": 1})).unwrap();
    s.execute("source.open", json!({"item": item.0})).unwrap();
    (s, item, calls)
}

#[test]
fn auto_transcribe_sequence_clips_from_clipboard_and_sequence_expansion() {
    let dir = tmp_dir("prefs-sequence-transcribe-routes");
    for route in ["file.newSequence", "edit.paste", "edit.pasteInsert", "expand"] {
        let (mut s, item, calls) = sequence_transcription_session(&dir, "ok", 200);
        if route != "file.newSequence" {
            // Set up an untranscribed source clip with the preference off.
            s.execute("source.insert", json!({})).unwrap();
            assert!(s.project.transcripts.is_empty());
            if route == "expand" {
                let source = s.state.active_sequence.unwrap();
                s.execute("file.newSequence", json!({"video": 1, "audio": 1})).unwrap();
                s.execute("sequence.nestSequences", json!({"on": false})).unwrap();
                s.execute("source.open", json!({"item": source.0})).unwrap();
            } else {
                s.execute("edit.copy", json!({})).unwrap();
            }
        }
        set(&mut s, "mediaAnalysis.autoTranscribe", json!(true));
        let (command, params) = match route {
            "file.newSequence" => (route, json!({"fromItem": item.0, "video": 1, "audio": 1})),
            "expand" => ("source.insert", json!({})),
            _ => (route, json!({})),
        };
        s.execute(command, params).unwrap();
        assert_eq!(s.project.transcripts[&item].words[0].text, "hello", "{route}");
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1, "{route}");
    }
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn auto_transcribe_sequence_clips_share_undo_and_preserve_existing_transcripts() {
    let dir = tmp_dir("prefs-sequence-transcribe-history");
    for limit in [0, 1, 200] {
        let (mut s, item, calls) = sequence_transcription_session(&dir, "ok", limit);
        set(&mut s, "mediaAnalysis.autoTranscribe", json!(true));
        let before = s.project.to_json();
        let undo = s.history.undo.clone();
        s.execute("source.insert", json!({})).unwrap();
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1);
        let after = s.project.to_json();
        assert_eq!(filmcraft_project::Project::from_json(&after).unwrap().to_json(), after);
        assert_eq!(s.history.undo.len(), (undo.len() + 1).min(limit));
        if limit > 0 {
            if limit > 1 {
                for (old, kept) in undo.iter().zip(&s.history.undo) {
                    assert_eq!(old.0, kept.0);
                    assert!(std::sync::Arc::ptr_eq(&old.1, &kept.1), "older undo steps must be retained");
                }
            }
            s.execute("edit.undo", json!({})).unwrap();
            assert_eq!(s.project.to_json(), before);
            s.execute("edit.redo", json!({})).unwrap();
            assert_eq!(s.project.to_json(), after);
            assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1, "redo must restore the saved transcript");
        }
        let transcript = s.project.transcripts[&item].clone();
        s.execute("source.insert", json!({})).unwrap();
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1, "reuse the media's existing transcript");
        assert!(std::sync::Arc::ptr_eq(&s.project.transcripts[&item], &transcript));
    }
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn auto_transcribe_sequence_clips_failures_warn_and_keep_the_placement() {
    let dir = tmp_dir("prefs-sequence-transcribe-failure");
    for outcome in ["error", "panic", "unavailable"] {
        if outcome == "unavailable" && crate::transcript::speech_available() {
            continue;
        }
        let (mut s, item, calls) = sequence_transcription_session(&dir, outcome, 200);
        if outcome == "unavailable" {
            s.transcriber = None;
        }
        set(&mut s, "mediaAnalysis.autoTranscribe", json!(true));
        let before = s.project.to_json();
        let undo = s.history.undo.len();
        s.execute("source.insert", json!({})).unwrap();
        assert!(s.active_sequence().unwrap().audio_tracks[0].items.iter().any(|c| c.item == item));
        assert!(!s.project.transcripts.contains_key(&item));
        assert_eq!(s.history.undo.len(), undo + 1);
        let reason = match outcome {
            "error" => "test recognizer failed",
            "panic" => "internal error during automatic transcription",
            _ => "not available in this build",
        };
        assert!(s.log.entries.iter().any(|e| e.level == crate::panels::Level::Warning && e.source == "source.insert" && e.message.contains(reason)));
        assert!(s.events.iter().any(|e| matches!(e, crate::Event::Toast { message, error: true } if message.contains(reason))));
        s.execute("edit.undo", json!({})).unwrap();
        assert_eq!(s.project.to_json(), before);
        s.execute("edit.redo", json!({})).unwrap();
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), usize::from(outcome != "unavailable"));
    }
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn auto_transcribe_sequence_clips_ignore_disabled_scope_and_failed_placement() {
    let dir = tmp_dir("prefs-sequence-transcribe-controls");
    for scope in ["off", "allImported", "locked"] {
        let (mut s, item, calls) = sequence_transcription_session(&dir, "ok", 200);
        if scope != "off" {
            set(&mut s, "mediaAnalysis.autoTranscribe", json!(true));
        }
        if scope == "allImported" {
            set(&mut s, "mediaAnalysis.autoTranscribeScope", json!("allImported"));
        }
        if scope == "locked" {
            s.execute("timeline.setTrack", json!({"track": "A1", "locked": true})).unwrap();
        }
        let r = s.execute("timeline.place", json!({"item": item.0, "track": "A1"}));
        assert_eq!(r.is_ok(), scope != "locked", "{scope}: {r:?}");
        assert!(s.project.transcripts.is_empty(), "{scope}");
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 0, "{scope}");
    }
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn auto_transcribe_sequence_clips_do_not_change_opened_documents() {
    let dir = tmp_dir("prefs-sequence-transcribe-open");
    let (mut s, _, calls) = sequence_transcription_session(&dir, "ok", 200);
    s.execute("source.insert", json!({})).unwrap();
    let saved = s.project.to_json();
    let path = dir.join("project.fcproj");
    std::fs::write(&path, &saved).unwrap();
    s.execute("file.newProject", json!({})).unwrap();
    set(&mut s, "mediaAnalysis.autoTranscribe", json!(true));
    s.execute("file.open", json!({"path": path.to_string_lossy()})).unwrap();
    assert_eq!(s.project.to_json(), saved);
    assert!(!s.is_dirty());
    s.execute("file.revert", json!({})).unwrap();
    assert_eq!(s.project.to_json(), saved);
    assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 0);
    std::fs::remove_dir_all(dir).unwrap();
}

/// In a build without speech-to-text (the release builds), the transcript commands that can't
/// run say so through `enabled` (#97, #98), and an import with auto-transcribe on reports why it
/// made no transcript instead of saying nothing (#89).
#[test]
fn transcription_without_speech_to_text_says_so() {
    let mut s = Session::default();
    assert_eq!(s.is_enabled("transcript.generate"), crate::transcript::speech_available());
    assert_eq!(s.is_enabled("transcript.downloadModel"), cfg!(feature = "speech-download"));
    if crate::transcript::speech_available() {
        return;
    }
    let dir = tmp_dir("prefs-no-speech");
    let mov = dir.join("talk.mov");
    crate::media_test_util::make_movie(&mov, filmcraft_media::DemoScene::OceanSunset, 64, 36, 24);
    // off: an import reports nothing
    let r = s.execute("file.import", json!({"paths": [mov.to_string_lossy()]})).unwrap();
    assert_eq!(r["errors"], json!([]));
    set(&mut s, "mediaAnalysis.autoTranscribe", json!(true));
    set(&mut s, "mediaAnalysis.autoTranscribeScope", json!("allImported"));
    let r = s.execute("file.import", json!({"paths": [mov.to_string_lossy()]})).unwrap();
    let errors = r["errors"].as_array().unwrap();
    assert!(errors.iter().any(|e| e.as_str().is_some_and(|e| e.starts_with("transcription:") && e.contains("not available in this build"))), "{errors:?}");
    assert!(!s.project.transcripts.contains_key(&ItemId(r["items"][0].as_u64().unwrap())));
    // with a transcriber installed (a host's), the command is enabled again
    s.transcriber = Some(std::sync::Arc::new(filmcraft_speech::FixedTranscriber { transcript: Default::default(), id: "fixed".into() }));
    assert!(s.is_enabled("transcript.generate"));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn hardware_decoding_setting_drives_the_decoder_switch() {
    let mut s = Session::default();
    assert_eq!(s.prefs.playback.hardware_decoding, "auto", "Auto by default");
    set(&mut s, "playback.hardwareDecoding", json!("off"));
    assert!(!filmcraft_codecs::hw::hardware_decoding(), "Off reaches the decoder registry");
    assert!(s.execute("prefs.set", json!({"key": "playback.hardwareDecoding", "value": "gpu"})).is_err());
    set(&mut s, "playback.hardwareDecoding", json!("auto"));
    assert!(filmcraft_codecs::hw::hardware_decoding());
    assert!(settings::field("playback.hardwareDecoding").is_some_and(|f| f.wired && matches!(f.kind, Kind::Choice(_))));
}

/// Settings ▸ Timeline ▸ "Place files dropped onto the Timeline directly on the Timeline": on by default.
#[test]
fn drop_import_to_timeline_defaults_to_on() {
    assert!(settings::TimelinePrefs::default().drop_import_to_timeline);
    assert!(Preferences::default().timeline.drop_import_to_timeline);
}

/// The key is saved under `timeline.dropImportToTimeline`, read back by `prefs.get`, and persisted.
#[test]
fn drop_import_to_timeline_round_trips_through_prefs() {
    let dir = tmp_dir("drop-import-to-timeline");
    let path = dir.join("preferences.json");
    let mut s = Session { prefs_path: Some(path.clone()), ..Session::default() };
    set(&mut s, "timeline.dropImportToTimeline", json!(false));
    assert!(!s.prefs.timeline.drop_import_to_timeline);
    assert_eq!(s.execute("prefs.get", json!({"key": "timeline.dropImportToTimeline"})).unwrap(), json!(false));
    assert!(!Preferences::load(&path).timeline.drop_import_to_timeline);
    // a non-boolean is rejected and the stored value is kept
    assert!(s.execute("prefs.set", json!({"key": "timeline.dropImportToTimeline", "value": "no"})).is_err());
    assert!(!s.prefs.timeline.drop_import_to_timeline);
    std::fs::remove_dir_all(dir).unwrap();
}

/// Preferences files written before the key existed load it as on.
#[test]
fn drop_import_to_timeline_defaults_to_on_for_older_prefs() {
    assert!(serde_json::from_value::<settings::TimelinePrefs>(json!({})).unwrap().drop_import_to_timeline);
    let old = Preferences::from_value(json!({"version": 2, "timeline": {"stillImageDuration": 3}}));
    assert!(old.timeline.drop_import_to_timeline);
    assert_eq!(old.timeline.still_image_duration, 3.0);
}

/// The key is a Boolean field of the Timeline category in the settings registry.
#[test]
fn drop_import_to_timeline_is_in_the_settings_registry() {
    let (cat, f) = settings::fields().into_iter().find(|(_, f)| f.key == "timeline.dropImportToTimeline").expect("registry field");
    assert_eq!(cat, "timeline");
    assert!(matches!(f.kind, Kind::Bool));
    assert_eq!(settings::field("timeline.dropImportToTimeline").map(|f| f.key), Some("timeline.dropImportToTimeline"));
}

/// New users keep the dark Darkest theme (following the system is opt-in); a file saved before
/// appearance modes keeps showing its Color Theme as a fixed Dark or Light mode.
#[test]
fn appearance_defaults_and_legacy_theme_migrate() {
    let a = Preferences::default().appearance;
    assert_eq!((a.appearance_mode.as_str(), a.dark_theme.as_str(), a.light_theme.as_str()), ("dark", "darkest", "light"));
    assert_eq!(a.shown_theme(Some(true)), "darkest", "Dark mode ignores a light system");
    let dir = tmp_dir("prefs-appearance-migrate");
    let path = dir.join("preferences.json");
    for (theme, mode, dark, light) in [("darkest", "dark", "darkest", "light"), ("dark", "dark", "dark", "light"), ("light", "light", "darkest", "light")] {
        std::fs::write(&path, json!({"version": 2, "appearance": {"colorTheme": theme}}).to_string()).unwrap();
        let a = Preferences::load(&path).appearance;
        assert_eq!((a.appearance_mode.as_str(), a.dark_theme.as_str(), a.light_theme.as_str()), (mode, dark, light), "{theme}");
        assert_eq!(a.shown_theme(None), theme);
    }
    // a v1 file without an appearance section, and a hostile theme, get the defaults
    std::fs::write(&path, r#"{"autoSave":{"enabled":true}}"#).unwrap();
    assert_eq!(Preferences::load(&path).appearance, settings::AppearancePrefs::default());
    std::fs::write(&path, r#"{"appearance":{"colorTheme":"neon","appearanceMode":7,"darkTheme":"light"}}"#).unwrap();
    assert_eq!(Preferences::load(&path).appearance, settings::AppearancePrefs::default());
    // a file with a mode keeps it
    std::fs::write(&path, r#"{"appearance":{"colorTheme":"darkest","appearanceMode":"auto","lightTheme":"light","darkTheme":"dark"}}"#).unwrap();
    let a = Preferences::load(&path).appearance;
    assert_eq!((a.appearance_mode.as_str(), a.shown_theme(Some(true)), a.shown_theme(Some(false)), a.shown_theme(None)), ("auto", "light", "dark", "dark"));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn appearance_choices_validate_and_legacy_theme_still_selects() {
    let mut s = Session::default();
    s.execute("prefs.set", json!({"values": {"appearance.appearanceMode": "auto", "appearance.darkTheme": "dark"}})).unwrap();
    assert_eq!((s.prefs.appearance.appearance_mode.as_str(), s.prefs.appearance.dark_theme.as_str()), ("auto", "dark"));
    for (key, value) in [
        ("appearance.darkTheme", json!("light")),
        ("appearance.lightTheme", json!("darkest")),
        ("appearance.appearanceMode", json!("system")),
        ("appearance.appearanceMode", json!(1)),
    ] {
        assert!(s.execute("prefs.set", json!({"key": key, "value": value})).is_err(), "{key} {value}");
    }
    // the single Color Theme of older clients selects that theme and fixes the mode to its family
    set(&mut s, "appearance.colorTheme", json!("light"));
    assert_eq!((s.prefs.appearance.appearance_mode.as_str(), s.prefs.appearance.light_theme.as_str()), ("light", "light"));
    assert_eq!(s.prefs.appearance.dark_theme, "dark", "the dark choice is kept");
    set(&mut s, "appearance.colorTheme", json!("darkest"));
    assert_eq!((s.prefs.appearance.appearance_mode.as_str(), s.prefs.appearance.dark_theme.as_str()), ("dark", "darkest"));
    // the legacy Color Theme follows the mode and theme choices, so clients reading it aren't stale
    assert_eq!(s.prefs.appearance.color_theme, "darkest");
    set(&mut s, "appearance.appearanceMode", json!("light"));
    assert_eq!(s.prefs.appearance.color_theme, "light");
    set(&mut s, "appearance.appearanceMode", json!("dark"));
    set(&mut s, "appearance.darkTheme", json!("dark"));
    assert_eq!(s.prefs.appearance.color_theme, "dark");
    set(&mut s, "appearance.appearanceMode", json!("auto"));
    assert_eq!(s.prefs.appearance.color_theme, "dark", "Auto: the dark theme");
    set(&mut s, "appearance.colorTheme", json!("light"));
    assert_eq!((s.prefs.appearance.appearance_mode.as_str(), s.prefs.appearance.color_theme.as_str()), ("light", "light"));
    // the header button's cycle
    let mut a = settings::AppearancePrefs::default();
    let mut seen = Vec::new();
    for _ in 0..4 {
        a.appearance_mode = a.next_mode().into();
        seen.push(a.appearance_mode.clone());
    }
    assert_eq!(seen, ["auto", "light", "dark", "auto"]);
}

#[test]
fn favorite_themes_validate_and_persist_without_changing_other_settings() {
    let dir = tmp_dir("theme-favorites");
    let path = dir.join("preferences.json");
    let mut s = Session { prefs_path: Some(path.clone()), ..Session::default() };
    set(&mut s, "appearance.favoriteThemes", json!(["dark", "light"]));
    assert_eq!(Preferences::load(&path).appearance.favorite_themes, vec!["dark", "light"]);
    for value in [json!("light"), json!(["unknown"]), json!(["dark", "dark"]), json!([3]), json!(null)] {
        assert!(s.execute("prefs.set", json!({"key": "appearance.favoriteThemes", "value": value})).is_err());
        assert_eq!(s.prefs.appearance.favorite_themes, vec!["dark", "light"]);
    }
    let mut corrupt = s.prefs.to_value();
    corrupt["appearance"]["favoriteThemes"] = json!(["invalid", "invalid"]);
    let repaired = Preferences::from_value(corrupt);
    assert!(repaired.appearance.favorite_themes.is_empty());
    assert_eq!(repaired.appearance.dark_theme, s.prefs.appearance.dark_theme);
}

fn preset_json(name: &str, n: usize, color: &str) -> Vec<u8> {
    let labels: Vec<Value> = (0..n).map(|i| json!({"name": format!(" Label {i} "), "color": color})).collect();
    serde_json::to_vec(&json!({"name": name, "labels": labels})).unwrap()
}

/// Label color preset files are checked strictly: size, exactly 16 labels, names trimmed and
/// capped, colours `#rrggbb`. A bad file is an error, never a panic or a partial preset.
#[test]
fn label_presets_reject_bad_files() {
    use settings::LabelPreset;
    let p = LabelPreset::parse(&preset_json("  Team  ", 16, "#A0B0C0")).unwrap();
    assert_eq!(p.name, "Team");
    assert_eq!((p.labels[0].name.as_str(), p.labels[0].color.as_str()), ("Label 0", "#a0b0c0"));
    let builtin = LabelPreset::builtin();
    assert_eq!(builtin.name, "Default");
    assert_eq!(builtin.labels.len(), 16);
    assert!(builtin.matches(&settings::LabelPrefs::default()), "the built-in preset is today's labels");
    assert_eq!(LabelPreset::parse(&serde_json::to_vec(&builtin).unwrap()).unwrap(), builtin);

    let long = "x".repeat(65);
    let mut too_big = preset_json("Big", 16, "#000000");
    too_big.extend(std::iter::repeat_n(b' ', 64 * 1024));
    for (why, bytes) in [
        ("15 labels", preset_json("A", 15, "#000000")),
        ("17 labels", preset_json("A", 17, "#000000")),
        ("no #", preset_json("A", 16, "000000")),
        ("not hex", preset_json("A", 16, "#12345g")),
        ("a colour name", preset_json("A", 16, "red")),
        ("short", preset_json("A", 16, "#fff")),
        ("empty name", preset_json("   ", 16, "#000000")),
        ("long name", preset_json(&long, 16, "#000000")),
        (
            "long label",
            serde_json::to_vec(&json!({"name": "A", "labels": (0..16).map(|_| json!({"name": long, "color": "#000000"})).collect::<Vec<_>>()})).unwrap(),
        ),
        ("no colour", serde_json::to_vec(&json!({"name": "A", "labels": (0..16).map(|_| json!({"name": "a"})).collect::<Vec<_>>()})).unwrap()),
        ("not an object", b"[1,2,3]".to_vec()),
        ("too big", too_big),
        ("not utf-8", vec![0xff, 0xfe, 0x00]),
    ] {
        assert!(LabelPreset::parse(&bytes).is_err(), "{why} should be refused");
    }
    // every truncation of a good file is refused cleanly
    let good = preset_json("Team", 16, "#102030");
    for n in 0..good.len() {
        assert!(LabelPreset::parse(&good[..n]).is_err());
    }
}

/// Import copies a preset into `Label Presets` beside the preferences, export writes the current
/// labels named after the file, and delete removes user presets only. A bad file changes nothing.
#[test]
fn label_presets_import_export_and_delete() {
    let dir = tmp_dir("label-presets");
    let mut s = Session { prefs_path: Some(dir.join("preferences.json")), ..Session::default() };
    let names = |s: &mut Session| -> Vec<String> {
        let v = s.execute("labels.presets", json!({})).unwrap();
        v["presets"].as_array().unwrap().iter().map(|p| p["name"].as_str().unwrap().to_string()).collect()
    };
    assert_eq!(names(&mut s), ["Default", "Bright", "Roles"]);
    let src = dir.join("from someone.json");
    std::fs::write(&src, preset_json("Team", 16, "#102030")).unwrap();
    let r = s.execute("labels.importPreset", json!({"path": src.to_string_lossy()})).unwrap();
    assert_eq!(r["name"], "Team");
    assert!(dir.join("Label Presets").join("Team.json").is_file());
    assert_eq!(names(&mut s), ["Default", "Bright", "Roles", "Team"]);

    // a bad file: an error, and the folder is as it was
    let bad = dir.join("bad.json");
    std::fs::write(&bad, preset_json("Bad", 3, "#102030")).unwrap();
    assert!(s.execute("labels.importPreset", json!({"path": bad.to_string_lossy()})).is_err());
    std::fs::write(dir.join("Label Presets").join("junk.json"), b"{not json").unwrap();
    assert_eq!(names(&mut s), ["Default", "Bright", "Roles", "Team"], "unreadable files in the folder are skipped");

    // a preset named like a built-in one keeps both
    std::fs::write(&src, preset_json("default", 16, "#405060")).unwrap();
    assert_eq!(s.execute("labels.importPreset", json!({"path": src.to_string_lossy()})).unwrap()["name"], "default (imported)");
    std::fs::write(&src, preset_json("ROLES", 16, "#405060")).unwrap();
    assert_eq!(s.execute("labels.importPreset", json!({"path": src.to_string_lossy()})).unwrap()["name"], "ROLES (imported)");

    // export writes the applied labels, named after the file, and imports back
    set(&mut s, "labels.colors.rose.color", json!("#123456"));
    let out = dir.join("Our Labels.json");
    assert_eq!(s.execute("labels.exportPreset", json!({"path": out.to_string_lossy()})).unwrap()["name"], "Our Labels");
    let back = settings::LabelPreset::read(&out).unwrap();
    assert!(back.matches(&s.prefs.labels));

    for builtin in ["Default", "Bright", "Roles"] {
        assert!(s.execute("labels.deletePreset", json!({"name": builtin})).is_err(), "{builtin} stays");
    }
    s.execute("labels.deletePreset", json!({"name": "Team"})).unwrap();
    assert_eq!(names(&mut s), ["Default", "Bright", "Roles", "default (imported)", "ROLES (imported)"]);
    assert!(s.execute("labels.deletePreset", json!({"name": "Team"})).is_err());
}

/// The built-in presets: Default (the labels as shipped), Bright and Roles, each with 16 distinct
/// names and valid colours (Bright's and Roles' clearly different); Roles puts the label defaults
/// on matching roles.
#[test]
fn builtin_label_presets_are_complete_and_distinct() {
    use settings::LabelPreset;
    let all = LabelPreset::builtins();
    assert_eq!(all.iter().map(|p| p.name.as_str()).collect::<Vec<_>>(), ["Default", "Bright", "Roles"]);
    for p in &all {
        assert!(settings::is_builtin_label_preset(&p.name));
        assert_eq!(&LabelPreset::parse(&serde_json::to_vec(p).unwrap()).unwrap(), p, "{} is a valid preset file", p.name);
        let mut names: Vec<&str> = p.labels.iter().map(|l| l.name.as_str()).collect();
        names.sort();
        names.dedup();
        assert_eq!(names.len(), 16, "{}: names are unique", p.name);
        let rgb: Vec<[u8; 3]> = p.labels.iter().map(|l| settings::parse_hex(&l.color).unwrap()).collect();
        if p.name == "Default" {
            continue; // the colours labels have always had
        }
        for (i, a) in rgb.iter().enumerate() {
            for b in &rgb[i + 1..] {
                let d: f32 = a.iter().zip(b).map(|(x, y)| (f32::from(*x) - f32::from(*y)).powi(2)).sum::<f32>().sqrt();
                assert!(d > 30.0, "{}: {a:?} and {b:?} are too close", p.name);
            }
        }
    }
    let roles = &all[2];
    let mut labels = settings::LabelPrefs::default();
    roles.apply(&mut labels);
    assert!(roles.matches(&labels));
    let d = labels.defaults.clone();
    let named = [
        (d.video, "B-Roll"),
        (d.movie, "Footage"),
        (d.audio, "Dialogue"),
        (d.still, "Graphics"),
        (d.sequence, "Sequence"),
        (d.dynamic_link, "Visual Effects"),
        (d.bin, "Bin"),
        (d.captions, "Bin"),
    ];
    for (label, role) in named {
        assert_eq!(labels.name(label), role);
    }
    assert!(!settings::is_builtin_label_preset("Team"));
}

// ---------- #3089: Selection Follows Playhead persists ----------

/// Sequence ▸ Selection Follows Playhead persists: turning it on writes the preference, saves
/// `preferences.json`, and the next session starts with it on (#3089 — it used to live only in
/// volatile session state). Fails on `main`: the preference key does not exist there.
#[test]
fn selection_follows_playhead_persists_across_sessions() {
    let dir = tmp_dir("sfp-persist");
    let mut s = Session::default();
    s.start_autosave(crate::autosave::AutosaveConfig::new(&dir)).unwrap();
    s.execute("sequence.selectionFollowsPlayhead", json!({"on": true})).unwrap();
    assert!(s.state.selection_follows_playhead);
    assert_eq!(s.prefs.get("timeline.selectionFollowsPlayhead"), Some(json!(true)), "command persisted the pref");
    let file: Value = serde_json::from_slice(&std::fs::read(dir.join("preferences.json")).unwrap()).unwrap();
    assert_eq!(file["timeline"]["selectionFollowsPlayhead"], json!(true));
    s.shutdown();

    let mut s2 = Session::default();
    s2.start_autosave(crate::autosave::AutosaveConfig::new(&dir)).unwrap();
    assert_eq!(s2.prefs.get("timeline.selectionFollowsPlayhead"), Some(json!(true)), "restored from the file");
    assert!(s2.state.selection_follows_playhead, "applied to the session at load");
    s2.shutdown();
    std::fs::remove_dir_all(&dir).unwrap();
}

/// The off direction persists too, and an explicit set through the settings surface works:
/// a fix that simply defaulted the flag on would pass the test above and fail this one.
#[test]
fn selection_follows_playhead_stays_off_when_turned_off() {
    let dir = tmp_dir("sfp-off");
    let mut s = Session::default();
    s.start_autosave(crate::autosave::AutosaveConfig::new(&dir)).unwrap();
    s.execute("sequence.selectionFollowsPlayhead", json!({"on": true})).unwrap();
    s.execute("sequence.selectionFollowsPlayhead", json!({"on": false})).unwrap();
    assert!(!s.state.selection_follows_playhead);
    assert_eq!(s.prefs.get("timeline.selectionFollowsPlayhead"), Some(json!(false)));
    // set back on through prefs.set (the Settings dialog / agent path) and the live session follows
    s.execute("prefs.set", json!({"key": "timeline.selectionFollowsPlayhead", "value": true})).unwrap();
    assert!(s.state.selection_follows_playhead, "prefs.set drives the live session");
    s.execute("prefs.set", json!({"key": "timeline.selectionFollowsPlayhead", "value": false})).unwrap();
    assert!(!s.state.selection_follows_playhead);
    let file: Value = serde_json::from_slice(&std::fs::read(dir.join("preferences.json")).unwrap()).unwrap();
    assert_eq!(file["timeline"]["selectionFollowsPlayhead"], json!(false));
    s.shutdown();
    let mut s2 = Session::default();
    s2.start_autosave(crate::autosave::AutosaveConfig::new(&dir)).unwrap();
    assert!(!s2.state.selection_follows_playhead, "off survives the restart");
    s2.shutdown();
    std::fs::remove_dir_all(&dir).unwrap();
}

/// Omitting `on` still toggles (from the persisted value, not from volatile state): off -> on
/// through the command alone, persisted both ways.
#[test]
fn selection_follows_playhead_toggle_without_on_persists() {
    let dir = tmp_dir("sfp-toggle");
    let mut s = Session::default();
    s.start_autosave(crate::autosave::AutosaveConfig::new(&dir)).unwrap();
    s.execute("sequence.selectionFollowsPlayhead", json!({})).unwrap();
    assert!(s.state.selection_follows_playhead, "off -> on");
    assert_eq!(s.prefs.get("timeline.selectionFollowsPlayhead"), Some(json!(true)));
    s.execute("sequence.selectionFollowsPlayhead", json!({})).unwrap();
    assert!(!s.state.selection_follows_playhead, "on -> off");
    assert_eq!(s.prefs.get("timeline.selectionFollowsPlayhead"), Some(json!(false)));
    s.shutdown();
    std::fs::remove_dir_all(&dir).unwrap();
}

/// A hostile value is rejected and the stored value is kept (the shared prefs.set contract).
#[test]
fn selection_follows_playhead_rejects_hostile_values() {
    let dir = tmp_dir("sfp-hostile");
    let mut s = Session { prefs_path: Some(dir.join("preferences.json")), ..Session::default() };
    s.execute("prefs.set", json!({"key": "timeline.selectionFollowsPlayhead", "value": true})).unwrap();
    assert!(s.execute("prefs.set", json!({"key": "timeline.selectionFollowsPlayhead", "value": "no"})).is_err());
    assert_eq!(s.prefs.get("timeline.selectionFollowsPlayhead"), Some(json!(true)), "bad write kept the old value");
    std::fs::remove_dir_all(&dir).unwrap();
}

/// A preferences file written before the key existed keeps today's behaviour: off, and every
/// other Timeline value untouched.
#[test]
fn selection_follows_playhead_defaults_to_off_for_older_prefs() {
    let dir = tmp_dir("sfp-legacy");
    let path = dir.join("preferences.json");
    std::fs::write(&path, json!({"version": 2, "timeline": {"stillImageDuration": 3}}).to_string()).unwrap();
    let prefs = Preferences::load(&path);
    assert_eq!(prefs.get("timeline.selectionFollowsPlayhead"), Some(json!(false)));
    assert_eq!(prefs.timeline.still_image_duration, 3.0);
    std::fs::remove_dir_all(&dir).unwrap();
}

/// The key is a Boolean field of the Timeline category in the settings registry, so the Settings
/// dialog and the agent-facing settings surface see it like every other persisted toggle.
#[test]
fn selection_follows_playhead_is_in_the_settings_registry() {
    let (cat, f) = settings::fields().into_iter().find(|(_, f)| f.key == "timeline.selectionFollowsPlayhead").expect("registry field");
    assert_eq!(cat, "timeline");
    assert!(matches!(f.kind, Kind::Bool));
    assert_eq!(settings::field("timeline.selectionFollowsPlayhead").map(|f| f.key), Some("timeline.selectionFollowsPlayhead"));
}
