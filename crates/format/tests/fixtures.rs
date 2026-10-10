//! Old project files keep opening: hand-made fixtures of every past schema are upgraded on load.

use filmcraft_format::{FormatError, SCHEMA_VERSION, decode, encode};
use filmcraft_project::{ClipId, ItemId, ItemKind, Project};
use filmcraft_time::Tick;

fn fixture(name: &str) -> Vec<u8> {
    std::fs::read(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures").join(name)).unwrap()
}

#[test]
fn v1_minimal_loads() {
    let l = decode(&fixture("v1-minimal.fcproj")).unwrap();
    assert_eq!(l.schema_version, 1);
    assert!(l.migrated());
    assert_eq!(l.project.name, "Legacy Minimal");
    assert!(l.project.items.is_empty());
    assert_eq!(l.project.next_id, 1);
}

/// Main briefly wrote bare projects with `"version": 2` (caption tracks) before the envelope landed.
#[test]
fn bare_version_2_loads_as_schema_v1() {
    let mut v: serde_json::Value = serde_json::from_slice(&fixture("v1-edit.fcproj")).unwrap();
    v["version"] = 2.into();
    let l = decode(&serde_json::to_vec(&v).unwrap()).unwrap();
    assert_eq!(l.schema_version, 1);
    assert!(l.migrated());
    assert_eq!(l.project.name, decode(&fixture("v1-edit.fcproj")).unwrap().project.name);
}

#[test]
fn v1_edit_loads_with_defaults_for_later_fields() {
    let l = decode(&fixture("v1-edit.fcproj")).unwrap();
    assert_eq!(l.schema_version, 1);
    let p = &l.project;
    assert_eq!(p.name, "Legacy Edit");
    assert_eq!(p.next_id, 48);
    let media = p.item(ItemId(5)).unwrap();
    assert_eq!(media.name, "Ocean_Sunset.mp4");
    assert!(media.metadata.is_empty());
    let seq = p.sequence(ItemId(21)).unwrap();
    assert_eq!(seq.video_tracks.len(), 1);
    assert_eq!(seq.audio_tracks.len(), 1);
    assert_eq!(seq.markers.len(), 1);
    let clip = &seq.video_tracks[0].items[0];
    assert_eq!(clip.id, ClipId(29));
    assert_eq!(clip.source_in, Tick(243_675_432_000));
    assert!(!clip.reverse && clip.link.is_none() && clip.gain_db == 0.0 && !clip.scale_to_frame);
    assert_eq!(clip.effects.len(), 3);
    assert!(seq.audio_tracks[0].effects.is_empty());
    seq.check().unwrap();
}

#[test]
fn legacy_audio_field_becomes_primary_stream_and_clip_defaults_to_zero() {
    let p = decode(&fixture("v1-edit.fcproj")).unwrap().project;
    let media = p.item(ItemId(5)).unwrap().as_media().unwrap();
    assert_eq!(media.info.audio_streams.len(), 1);
    assert!(media.info.has_audio());
    let clip = &p.sequence(ItemId(21)).unwrap().audio_tracks[0].items[0];
    assert_eq!(clip.audio_stream, 0);
    let again = decode(&encode(&p, false)).unwrap();
    assert_eq!(again.project.item(ItemId(5)).unwrap().as_media().unwrap().info.audio_streams, media.info.audio_streams);
}

/// Out-of-range audio stream data is repaired on load, never a reason to refuse the project.
#[test]
fn hostile_audio_stream_counts_and_indices_are_repaired_not_rejected() {
    let mut p = decode(&fixture("v1-edit.fcproj")).unwrap().project;
    let seq = ItemId(21);
    for bad in [filmcraft_media::MAX_AUDIO_STREAMS, usize::MAX] {
        p.sequence_mut(seq).unwrap().audio_tracks[0].items[0].audio_stream = bad;
        let loaded = decode(&encode(&p, false)).unwrap().project;
        assert_eq!(loaded.sequence(seq).unwrap().audio_tracks[0].items[0].audio_stream, 0);
    }
    p.sequence_mut(seq).unwrap().audio_tracks[0].items[0].audio_stream = filmcraft_media::MAX_AUDIO_STREAMS - 1;
    let loaded = decode(&encode(&p, false)).unwrap().project;
    assert_eq!(loaded.sequence(seq).unwrap().audio_tracks[0].items[0].audio_stream, filmcraft_media::MAX_AUDIO_STREAMS - 1, "in range: kept");
    let media = p.item_mut(ItemId(5)).unwrap().as_media_mut().unwrap();
    let stream = media.info.audio_streams[0].clone();
    media.info.audio_streams.resize(filmcraft_media::MAX_AUDIO_STREAMS + 1, stream);
    let loaded = decode(&encode(&p, false)).unwrap().project;
    assert_eq!(loaded.item(ItemId(5)).unwrap().as_media().unwrap().info.audio_streams.len(), filmcraft_media::MAX_AUDIO_STREAMS);
}

/// Schema 8 (before M3.10): no time interpolation / Hold Filters / Field Options / source channels
/// on clips, no audio channel map, subclips without Restrict Trims.
#[test]
fn v8_subclip_loads_with_m3_10_defaults() {
    let l = decode(&fixture("v8-subclip.fcproj")).unwrap();
    assert_eq!(l.schema_version, 8);
    assert!(l.migrated());
    let p = &l.project;
    assert_eq!(p.name, "Schema 8 Edit");
    let ItemKind::Subclip { parent, range, restrict_trims } = &p.item(ItemId(48)).unwrap().kind else { panic!("not a subclip") };
    assert_eq!((*parent, range.duration.0 > 0, *restrict_trims), (ItemId(5), true, false));
    assert!(p.item(ItemId(5)).unwrap().as_media().unwrap().interpret.audio_channels.is_none());
    let clip = &p.sequence(ItemId(21)).unwrap().video_tracks[0].items[0];
    assert!(clip.time_interpolation.is_default() && !clip.hold_filters && clip.field_options.is_none() && clip.source_channels.is_empty());
    // resaved in the current schema, losslessly
    let again = decode(&encode(p, true)).unwrap();
    assert_eq!(again.schema_version, SCHEMA_VERSION);
    assert_eq!(&again.project, p);
}

#[test]
fn upgraded_file_resaves_in_current_schema_losslessly() {
    let old = decode(&fixture("v1-edit.fcproj")).unwrap().project;
    let bytes = encode(&old, true);
    let v: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(v["schema_version"], SCHEMA_VERSION);
    let again = decode(&bytes).unwrap();
    assert!(!again.migrated());
    assert_eq!(again.project, old);
}

#[test]
fn future_schema_is_refused_with_a_clear_error() {
    let mut v: serde_json::Value = serde_json::from_slice(&fixture("v1-minimal.fcproj")).unwrap();
    // A bare v1-shaped project claiming a future `version` is refused too.
    v["version"] = 99.into();
    let e = decode(&serde_json::to_vec(&v).unwrap()).unwrap_err();
    assert!(matches!(e, FormatError::TooNew { found: 99, .. }), "{e}");
    assert!(e.to_string().contains("Update FilmCraft"));
}

/// Build a project with `n` clips (half video, half linked audio) on 4+4 tracks.
pub fn big_project(n: usize) -> Project {
    let mut p = decode(&fixture("v1-edit.fcproj")).unwrap().project;
    let seq_id = ItemId(21);
    let mut next = 1_000_000u64;
    let seq = match &mut p.items.get_mut(&seq_id).unwrap().kind {
        ItemKind::Sequence(s) => std::sync::Arc::make_mut(s),
        _ => unreachable!(),
    };
    let v0 = seq.video_tracks[0].items[0].clone();
    let a0 = seq.audio_tracks[0].items[0].clone();
    for t in seq.video_tracks.iter_mut().chain(seq.audio_tracks.iter_mut()) {
        t.items.clear();
    }
    let mut vt = seq.video_tracks[0].clone();
    let mut at = seq.audio_tracks[0].clone();
    for k in 1..4 {
        vt.id = filmcraft_project::TrackId(500 + k);
        vt.name = format!("Video {}", k + 1);
        seq.video_tracks.push(vt.clone());
        at.id = filmcraft_project::TrackId(600 + k);
        at.name = format!("Audio {}", k + 1);
        seq.audio_tracks.push(at.clone());
    }
    let dur = Tick(254_016_000_000); // 1 s each
    for i in 0..n / 2 {
        let track = i % 4;
        let slot = (i / 4) as i64;
        let mut v = v0.clone();
        v.id = ClipId(next);
        v.start = Tick(dur.0 * slot);
        v.duration = dur;
        v.link = Some(next);
        let mut a = a0.clone();
        a.id = ClipId(next + 1);
        a.start = v.start;
        a.duration = dur;
        a.link = Some(next);
        next += 2;
        seq.video_tracks[track].items.push(v);
        seq.audio_tracks[track].items.push(a);
    }
    p.next_id = next;
    p
}

#[test]
fn large_project_roundtrip_and_timing() {
    let p = big_project(2000);
    let clips: usize = p.sequence(ItemId(21)).unwrap().all_tracks().map(|t| t.items.len()).sum();
    assert_eq!(clips, 2000);
    let reps = 5;
    let t0 = std::time::Instant::now();
    let mut pretty = Vec::new();
    for _ in 0..reps {
        pretty = encode(&p, true);
    }
    let pretty_ms = t0.elapsed().as_secs_f64() * 1000.0 / reps as f64;
    let t0 = std::time::Instant::now();
    let mut compact = Vec::new();
    for _ in 0..reps {
        compact = encode(&p, false);
    }
    let compact_ms = t0.elapsed().as_secs_f64() * 1000.0 / reps as f64;
    let t0 = std::time::Instant::now();
    let back = decode(&compact).unwrap();
    let decode_ms = t0.elapsed().as_secs_f64() * 1000.0;
    assert_eq!(back.project, p);
    let t0 = std::time::Instant::now();
    let dir = std::env::temp_dir().join(format!("filmcraft-big-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    filmcraft_format::atomic_write(&dir.join("big.fcproj"), &compact).unwrap();
    let write_ms = t0.elapsed().as_secs_f64() * 1000.0;
    std::fs::remove_dir_all(&dir).unwrap();
    println!(
        "2000-clip project: pretty {:.1} KB in {pretty_ms:.2} ms, compact {:.1} KB in {compact_ms:.2} ms, decode {decode_ms:.2} ms, atomic write+fsync {write_ms:.2} ms",
        pretty.len() as f64 / 1024.0,
        compact.len() as f64 / 1024.0
    );
}

#[test]
fn v8_loads_without_transcripts_and_v9_roundtrips_them() {
    let l = decode(&fixture("v8-minimal.fcproj")).unwrap();
    assert_eq!(l.schema_version, 8);
    assert!(l.migrated());
    assert!(l.project.transcripts.is_empty());
    let mut p = l.project;
    let mut t = filmcraft_project::Transcript { language: "en".into(), source: "manual".into(), ..Default::default() };
    t.words.push(filmcraft_project::Word::new("Hello", Tick(0), Tick(1000)));
    t.words[0].speaker = Some(0);
    t.normalize();
    p.transcripts.insert(ItemId(7), std::sync::Arc::new(t));
    let again = decode(&encode(&p, true)).unwrap();
    assert!(!again.migrated());
    assert_eq!(again.project, p);
    assert_eq!(again.project.transcripts[&ItemId(7)].speakers[0].name, "Speaker 1");
}

/// Schemas 12 and 13 (before generated media): no ComfyUI clip recipes; both load (through every
/// later step) with none, and a recipe survives a save and load.
#[test]
fn v12_and_v13_minimal_load_without_generated_media() {
    for (file, version, name) in [("v12-minimal.fcproj", 12, "Before Generated Clips"), ("v13-minimal.fcproj", 13, "Before Generated Clips (v13)")] {
        let l = decode(&fixture(file)).unwrap();
        assert_eq!(l.schema_version, version);
        assert!(l.migrated());
        let mut p = l.project.clone();
        assert_eq!(p.name, name);
        assert!(p.generated.is_empty());
        let g = filmcraft_project::Generation {
            provider: "comfyui".into(),
            recipe: serde_json::json!({"workflow": {"9": {"class_type": "SaveImage", "inputs": {}}}, "inputs": [{"node": "9", "input": "filename_prefix", "value": "x"}]}),
            last_run: serde_json::Value::Null,
        };
        p.generated.insert(ItemId(1), std::sync::Arc::new(g));
        let again = decode(&encode(&p, true)).unwrap();
        assert_eq!(again.schema_version, SCHEMA_VERSION);
        assert!(!again.migrated());
        const { assert!(SCHEMA_VERSION >= 14) };
        assert_eq!(again.project, p);
    }
}

/// Schema 11 (before M10.7): no source graphics; graphic clips without template / roll /
/// responsive data and layers without style runs or pins load with those empty.
#[test]
fn v11_minimal_loads_without_graphics_design_data() {
    let l = decode(&fixture("v11-minimal.fcproj")).unwrap();
    assert_eq!(l.schema_version, 11);
    assert!(l.migrated());
    let p = &l.project;
    assert_eq!(p.name, "Before Graphics Templates");
    assert!(p.source_graphics.is_empty());
    assert!(p.root.find_bin(filmcraft_project::BinId(1)).is_some());
    let again = decode(&encode(p, true)).unwrap();
    assert_eq!(again.schema_version, SCHEMA_VERSION);
    const { assert!(SCHEMA_VERSION >= 12) };
    assert_eq!(&again.project, p);
}

/// Schema 10 (before M3.11): no search bins, no safe areas / capture format / scratch disks in
/// the project settings.
#[test]
fn v10_minimal_loads_with_m3_11_defaults() {
    let l = decode(&fixture("v10-minimal.fcproj")).unwrap();
    assert_eq!(l.schema_version, 10);
    assert!(l.migrated());
    let p = &l.project;
    assert_eq!(p.name, "Before Search Bins");
    assert!(p.search_bins.is_empty());
    assert_eq!((p.settings.title_safe, p.settings.action_safe), ((20.0, 20.0), (10.0, 10.0)));
    assert_eq!(p.settings.capture_format, "DV");
    assert_eq!(p.settings.scratch, Default::default());
    assert!(p.root.find_bin(filmcraft_project::BinId(1)).is_some());
    let again = decode(&encode(p, true)).unwrap();
    assert_eq!(again.schema_version, SCHEMA_VERSION);
    assert_eq!(&again.project, p);
}
