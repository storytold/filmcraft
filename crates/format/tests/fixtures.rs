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
    // the file says 48, but its sequence marker is 800000: new ids start past it (#2809)
    assert_eq!(p.next_id, 800_001);
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
    // the transcript's key is an id in use, so new ids start past it (#2809)
    p.next_id = 8;
    assert!(!again.migrated());
    assert_eq!(again.project, p);
    assert_eq!(again.project.transcripts[&ItemId(7)].speakers[0].name, "Speaker 1");
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

/// #2809: `next_id` is read from the file, and nothing raised it past the ids already in it. A file
/// whose `next_id` lagged behind (an older build, a hand edit, a merge) handed out an id in use, and
/// the new item silently replaced the old one. Each kind of id is raised past on its own, so a kind
/// the loader forgets fails here.
#[test]
fn next_id_is_raised_past_every_id_in_the_file() {
    use filmcraft_project::{
        BinId, Camera, Caption, CaptionFormat, CaptionTrack, Label, Marker, MarkerId, MarkerKind, MergedClip, MulticamSource, Narration, ProjectLut, SearchBin,
        Track, TrackId, TrackKind, TrackSend, Transition, TransitionId,
    };
    const BIG: u64 = 5_000_000;
    let base = decode(&fixture("v1-edit.fcproj")).unwrap().project;
    let seq = ItemId(21);
    let marker = |id| Marker {
        id: MarkerId(id),
        start: Tick::ZERO,
        duration: Tick::ZERO,
        name: String::new(),
        comment: String::new(),
        kind: MarkerKind::Comment,
        color: Label::Green,
    };
    let kinds: Vec<(&str, Box<dyn Fn(&mut Project)>)> = vec![
        (
            "item",
            Box::new(|p| {
                p.next_id = BIG;
                p.add_item("Layer", Label::Iris, p.item(ItemId(5)).unwrap().kind.clone(), None);
            }),
        ),
        (
            "bin",
            Box::new(|p| {
                p.next_id = BIG;
                p.add_bin("Bin", None);
            }),
        ),
        ("search bin", Box::new(|p| p.search_bins.push(SearchBin { id: BinId(BIG), name: "Find".into(), query: Default::default() }))),
        ("media marker", Box::new(move |p| p.item_mut(ItemId(5)).unwrap().as_media_mut().unwrap().markers.push(marker(BIG)))),
        ("sequence marker", Box::new(move |p| p.sequence_mut(seq).unwrap().markers[0].id = MarkerId(BIG))),
        ("track", Box::new(move |p| p.sequence_mut(seq).unwrap().audio_tracks[0].id = TrackId(BIG))),
        ("submix track", Box::new(move |p| p.sequence_mut(seq).unwrap().submix_tracks.push(Track::new(TrackId(BIG), TrackKind::Audio, "Submix".into())))),
        ("clip", Box::new(move |p| p.sequence_mut(seq).unwrap().video_tracks[0].items[0].id = ClipId(BIG))),
        ("link", Box::new(move |p| p.sequence_mut(seq).unwrap().video_tracks[0].items[0].link = Some(BIG))),
        ("group", Box::new(move |p| p.sequence_mut(seq).unwrap().video_tracks[0].items[0].group = Some(BIG))),
        ("clip marker", Box::new(move |p| p.sequence_mut(seq).unwrap().video_tracks[0].items[0].markers.push(marker(BIG)))),
        (
            "transition",
            Box::new(move |p| {
                let s = p.sequence_mut(seq).unwrap();
                let clip = s.video_tracks[0].items[0].clone();
                let effect = clip.effects[0].clone();
                s.video_tracks[0].transitions.push(Transition {
                    id: TransitionId(BIG),
                    effect,
                    start: clip.start,
                    duration: Tick::ZERO,
                    from: None,
                    to: Some(clip.id),
                    align: Default::default(),
                    reverse: false,
                });
            }),
        ),
        (
            "caption track",
            Box::new(move |p| p.sequence_mut(seq).unwrap().caption_tracks.push(CaptionTrack::new(TrackId(BIG), "Subtitle".into(), CaptionFormat::Subtitle))),
        ),
        (
            "caption",
            Box::new(move |p| {
                let mut track = CaptionTrack::new(TrackId(7), "Subtitle".into(), CaptionFormat::Subtitle);
                track.captions.push(Caption {
                    id: ClipId(BIG),
                    start: Tick::ZERO,
                    duration: Tick(1),
                    text: "Hi".into(),
                    speaker: None,
                    cue_id: None,
                    settings: String::new(),
                });
                p.sequence_mut(seq).unwrap().caption_tracks.push(track);
            }),
        ),
        (
            "LUT",
            Box::new(|p| p.luts.push(ProjectLut { id: format!("lut{BIG}"), name: "Look".into(), source_path: None, format: "cube".into(), text: "".into() })),
        ),
        // left behind by a deleted item: a new item must not take its id and inherit them
        ("transcript", Box::new(|p| _ = p.transcripts.insert(ItemId(BIG), Default::default()))),
        ("source graphic", Box::new(|p| _ = p.source_graphics.insert(ItemId(BIG), Default::default()))),
        (
            "narration",
            Box::new(|p| {
                let narration = Narration {
                    text: "Hello".into(),
                    language: "en-US".into(),
                    voice: "basic-female".into(),
                    pitch: Default::default(),
                    pace: 1.0,
                    speech_duration: Tick(1),
                };
                p.narrations.insert(ItemId(BIG), narration);
            }),
        ),
        // only referenced, the target gone: a new item taking that id would be picked up by the reference
        ("clip's item", Box::new(move |p| p.sequence_mut(seq).unwrap().video_tracks[0].items[0].item = ItemId(BIG))),
        (
            "subclip's parent",
            Box::new(|p| {
                let range = filmcraft_time::TimeRange::new(Tick::ZERO, Tick(1));
                p.add_item("Subclip", Label::Iris, ItemKind::Subclip { parent: ItemId(BIG), range, restrict_trims: false }, None);
            }),
        ),
        (
            "transition's clip",
            Box::new(move |p| {
                let s = p.sequence_mut(seq).unwrap();
                let clip = s.video_tracks[0].items[0].clone();
                s.video_tracks[0].transitions.push(Transition {
                    id: TransitionId(9),
                    effect: clip.effects[0].clone(),
                    start: clip.start,
                    duration: Tick::ZERO,
                    from: None,
                    to: Some(ClipId(BIG)),
                    align: Default::default(),
                    reverse: false,
                });
            }),
        ),
        ("send target", Box::new(move |p| p.sequence_mut(seq).unwrap().audio_tracks[0].mixer.sends.push(TrackSend::new(TrackId(BIG))))),
        ("track output", Box::new(move |p| p.sequence_mut(seq).unwrap().master_mixer.output = Some(TrackId(BIG)))),
        (
            "camera's source",
            Box::new(move |p| {
                let camera = Camera { name: "Cam 1".into(), video_track: None, audio_tracks: Vec::new(), enabled: true, source: Some(ItemId(BIG)) };
                p.sequence_mut(seq).unwrap().multicam = Some(MulticamSource { cameras: vec![camera], ..Default::default() });
            }),
        ),
        (
            "camera's track",
            Box::new(move |p| {
                let camera = Camera { name: "Cam 1".into(), video_track: Some(TrackId(BIG)), audio_tracks: Vec::new(), enabled: true, source: None };
                p.sequence_mut(seq).unwrap().multicam = Some(MulticamSource { cameras: vec![camera], ..Default::default() });
            }),
        ),
        ("merged clip", Box::new(move |p| p.sequence_mut(seq).unwrap().merged = Some(MergedClip { audio: vec![ItemId(BIG)], ..Default::default() }))),
    ];
    for (kind, add) in &kinds {
        let mut p = base.clone();
        add(&mut p);
        p.next_id = 1;
        let loaded = decode(&encode(&p, false)).unwrap_or_else(|e| panic!("{kind}: {e}"));
        assert_eq!(loaded.project.next_id, BIG + 1, "{kind}");
    }
}

/// #2809: the case the issue describes. A file whose `next_id` names an item already in it: the next
/// new item used to take that id and replace the item. It gets a fresh id, and both are still there
/// after a save and a reload.
#[test]
fn a_new_item_never_replaces_one_from_a_file_with_a_stale_next_id() {
    let mut p = decode(&fixture("v1-edit.fcproj")).unwrap().project;
    p.next_id = 5;
    let mut p = decode(&encode(&p, false)).unwrap().project;
    let before = p.items.len();
    let kind = p.item(ItemId(5)).unwrap().kind.clone();
    let new = p.add_item("Copy", filmcraft_project::Label::Iris, kind, None);
    assert_ne!(new, ItemId(5));
    assert_eq!(p.items.len(), before + 1, "a new item replaced an old one");
    assert_eq!(p.item(ItemId(5)).unwrap().name, "Ocean_Sunset.mp4");
    let again = decode(&encode(&p, false)).unwrap().project;
    assert_eq!(again, p, "the round trip keeps both items and the raised next_id");
    assert!(again.next_id > new.0);
}

/// #2809: ids past what can ever be allocated. The largest possible id has no successor, and a
/// `next_id` that high overflows `alloc_id`; such a file is refused instead of panicking (debug) or
/// wrapping to 0 (release) on the first new item.
#[test]
fn an_exhausted_id_space_is_refused_on_load() {
    use filmcraft_project::MarkerId;
    let base = decode(&fixture("v1-edit.fcproj")).unwrap().project;
    let mut p = base.clone();
    p.sequence_mut(ItemId(21)).unwrap().markers[0].id = MarkerId(u64::MAX);
    let e = decode(&encode(&p, false)).unwrap_err();
    assert!(matches!(&e, FormatError::Corrupt(m) if m.contains("ID space exhausted")), "{e}");
    let mut p = base.clone();
    p.next_id = u64::MAX;
    let e = decode(&encode(&p, false)).unwrap_err();
    assert!(matches!(&e, FormatError::Corrupt(m) if m.contains("ID space exhausted")), "{e}");
    let mut p = base;
    p.next_id = Project::MAX_NEXT_ID;
    assert_eq!(decode(&encode(&p, false)).unwrap().project.next_id, Project::MAX_NEXT_ID, "the last usable next_id still opens");
}
