//! Multi-clip audio placement: `audio_placement_specs` and dynamic audio track allocation.

use filmcraft_media::MediaSource;
use filmcraft_project::{AudioChannels, ItemId, TrackKind};
use filmcraft_time::{FrameRate, Tick, TimeRange};

use serde_json::json;

use crate::Session;
use crate::commands::{AudioPlacementSpec, ensure_audio_tracks};

/// The placement for `streams` streams that all decode.
fn audio_placement_specs(clips: &[Vec<u16>], streams: usize) -> Vec<AudioPlacementSpec> {
    crate::commands::audio_placement_specs(clips, &vec![true; streams.min(filmcraft_media::MAX_AUDIO_STREAMS)])
}

fn demo() -> Session {
    let mut s = Session::default();
    s.execute("file.openDemoProject", json!({})).unwrap();
    s
}

fn active(s: &Session) -> ItemId {
    s.state.active_sequence.expect("demo session has an active sequence")
}

#[test]
fn specs_always_place_one_clip_on_the_destination() {
    assert_eq!(audio_placement_specs(&[], 1), vec![AudioPlacementSpec { track_offset: 0, audio_stream: 0, source_channels: None }]);
    assert_eq!(audio_placement_specs(&[vec![0, 1]], 1).len(), 1);
}

#[test]
fn specs_put_further_clips_on_the_tracks_below() {
    let specs = audio_placement_specs(&[vec![0], vec![1], vec![2, 3]], 1);
    let got: Vec<(usize, Option<Vec<u16>>)> = specs.into_iter().map(|s| (s.track_offset, s.source_channels)).collect();
    assert_eq!(got, vec![(0, None), (1, Some(vec![1])), (2, Some(vec![2, 3]))]);
}

#[test]
fn specs_are_capped_for_hostile_channel_maps() {
    let clips = vec![vec![0u16]; 100_000];
    assert!(audio_placement_specs(&clips, usize::MAX).len() <= crate::sequence_tools::MAX_TRACKS);
}

#[test]
fn specs_append_container_streams_after_channel_map_clips() {
    let specs = audio_placement_specs(&[vec![1], vec![0]], 3);
    assert_eq!(
        specs.iter().map(|s| (s.track_offset, s.audio_stream, s.source_channels.clone())).collect::<Vec<_>>(),
        vec![(0, 0, None), (1, 0, Some(vec![0])), (2, 1, None), (3, 2, None)]
    );
    assert_eq!(audio_placement_specs(&[], 0).len(), 1);
}

#[test]
fn placing_media_with_two_streams_expands_tracks_and_keeps_stream_indices() {
    let mut s = Session::default();
    let mut p = (*s.project).clone();
    let rate = FrameRate::FPS_24;
    let seq = p.new_sequence("Two streams", Default::default(), 1, 1, None);
    let source = filmcraft_media::generators::GeneratorSource::new(filmcraft_media::Generator::BarsAndTone, 32, 32, rate, rate.tick_of(24));
    let mut info = source.info().clone();
    info.audio_streams.push(info.audio_streams[0].clone());
    let item = p.add_item(
        "two streams",
        filmcraft_project::Label::Iris,
        filmcraft_project::ItemKind::Media(filmcraft_project::MediaClip {
            media: filmcraft_project::MediaRef::Generator(filmcraft_media::Generator::BarsAndTone),
            info,
            interpret: Default::default(),
            mark_in: None,
            mark_out: None,
            markers: vec![],
            offline: false,
            proxy: None,
            identity: None,
        }),
        None,
    );
    let (vdest, adest) = {
        let q = p.sequence(seq).unwrap();
        (q.video_tracks[0].id, q.audio_tracks[0].id)
    };
    s.project = std::sync::Arc::new(p);
    s.state.active_sequence = Some(seq);
    let ids =
        crate::commands::place_item(&mut s, item, TimeRange::new(Tick::ZERO, rate.tick_of(24)), Tick::ZERO, Some(vdest), Some(adest), false, "test", None)
            .unwrap();
    assert_eq!(ids.len(), 3);
    let q = s.project.sequence(seq).unwrap();
    assert_eq!(q.audio_tracks.len(), 2);
    let (a, b) = (&q.audio_tracks[0].items[0], &q.audio_tracks[1].items[0]);
    assert_eq!((a.audio_stream, b.audio_stream), (0, 1));
    assert_eq!(a.link, b.link);
    assert!(a.link.is_some());
    assert_ne!(a.id, b.id);
    assert_eq!(a.source_in, b.source_in);
    assert_eq!(a.start, b.start);
}

#[test]
fn clip_peaks_measure_the_stream_the_clip_plays() {
    let mut s = Session::default();
    let mut p = (*s.project).clone();
    let rate = FrameRate::FPS_24;
    let seq = p.new_sequence("Two streams", Default::default(), 1, 1, None);
    let source = filmcraft_media::generators::GeneratorSource::new(filmcraft_media::Generator::BarsAndTone, 32, 32, rate, rate.tick_of(24));
    let mut info = source.info().clone();
    info.audio_streams.push(info.audio_streams[0].clone());
    let item = p.add_item(
        "two streams",
        filmcraft_project::Label::Iris,
        filmcraft_project::ItemKind::Media(filmcraft_project::MediaClip {
            media: filmcraft_project::MediaRef::Generator(filmcraft_media::Generator::BarsAndTone),
            info,
            interpret: Default::default(),
            mark_in: None,
            mark_out: None,
            markers: vec![],
            offline: false,
            proxy: None,
            identity: None,
        }),
        None,
    );
    let (vdest, adest) = {
        let q = p.sequence(seq).unwrap();
        (q.video_tracks[0].id, q.audio_tracks[0].id)
    };
    s.project = std::sync::Arc::new(p);
    s.state.active_sequence = Some(seq);
    let ids =
        crate::commands::place_item(&mut s, item, TimeRange::new(Tick::ZERO, rate.tick_of(24)), Tick::ZERO, Some(vdest), Some(adest), false, "test", None)
            .unwrap();
    let q = s.project.sequence(seq).unwrap();
    let (a, b) = (q.audio_tracks[0].items[0].id, q.audio_tracks[1].items[0].id);
    assert!(ids.contains(&a) && ids.contains(&b));
    let peaks = crate::mixer::clip_peaks(&s, &[a, b]);
    let db = |c: filmcraft_project::ClipId| peaks.iter().find(|(id, _)| *id == c).map(|(_, d)| *d).unwrap();
    // The generator only has stream 0, so the clip playing stream 1 is silent, not a copy of stream 0.
    assert!(db(a) > -30.0, "{peaks:?}");
    assert!(db(b) < -100.0, "{peaks:?}");
}

#[test]
fn missing_audio_tracks_are_cloned_from_the_destination() {
    let s = demo();
    let seq = active(&s);
    let mut p = (*s.project).clone();
    let have = p.sequence(seq).map(|q| q.audio_tracks.len()).unwrap_or(0);
    assert!(have >= 1);
    if let Some(t) = p.sequence_mut(seq).and_then(|q| q.audio_tracks.get_mut(0)) {
        t.channels = AudioChannels::Mono;
        t.volume_db = -3.0;
        t.locked = true;
    }
    let ids = ensure_audio_tracks(&mut p, seq, 0, have + 3, "test").unwrap();
    assert_eq!(ids.len(), have + 3);
    let q = p.sequence(seq).unwrap();
    for t in &q.audio_tracks[have..] {
        assert_eq!(t.kind, TrackKind::Audio);
        assert_eq!(t.channels, AudioChannels::Mono);
        assert_eq!(t.volume_db, -3.0);
        assert!(t.items.is_empty() && !t.locked);
    }
    let mut unique = ids.clone();
    unique.sort_by_key(|t| t.0);
    unique.dedup();
    assert_eq!(unique.len(), ids.len(), "track ids are unique");
    // enough tracks already: nothing is added
    let again = ensure_audio_tracks(&mut p, seq, 0, have + 3, "test").unwrap();
    assert_eq!(again, ids);
}

/// #823: a container with two PCM tracks imports both streams, and overwrite places linked clips.
#[test]
fn import_keeps_every_audio_stream_and_places_linked_clips() {
    use filmcraft_isobmff::{Brand, Mp4Writer, PcmConfig, SampleEntry, TrackConfig, WriteSample, WriterOptions};
    let mut mux = Mp4Writer::new(std::io::Cursor::new(Vec::new()), WriterOptions::new(Brand::Mov)).unwrap();
    for (rate, value) in [(48_000u32, 8192i16), (24_000, -16384)] {
        let entry = SampleEntry::pcm(PcmConfig { bits: 16, float: false, big_endian: false, signed: true, channels: 1, sample_rate: rate as f64 });
        let track = mux.add_track(TrackConfig::new(entry, rate)).unwrap();
        let data: Vec<u8> = (0..rate / 10).flat_map(|_| value.to_le_bytes()).collect();
        mux.write_sample(track, WriteSample { data: &data, duration: rate / 10, composition_offset: 0, is_sync: true }).unwrap();
    }
    let bytes = mux.finish().unwrap().into_inner();
    let mut s = Session::default();
    let item = crate::commands::import_bytes(&mut s, "/two-audio.mov", bytes.into(), None).unwrap();
    let streams = s.project.item(item).unwrap().as_media().unwrap().info.audio_streams.len();
    assert_eq!(streams, 2);
    s.execute("file.newSequence", json!({"name": "S", "fromItem": item.0, "video": 1, "audio": 1})).unwrap();
    let seq = s.state.active_sequence.unwrap();
    let q = s.project.sequence(seq).unwrap();
    assert_eq!(q.audio_tracks.len(), 2);
    let (a, b) = (&q.audio_tracks[0].items[0], &q.audio_tracks[1].items[0]);
    assert_eq!((a.audio_stream, b.audio_stream), (0, 1));
    assert_eq!(a.link, b.link);
    assert!(a.link.is_some());
}

#[test]
fn hostile_track_counts_are_an_error_not_a_crash() {
    let s = demo();
    let seq = active(&s);
    let mut p = (*s.project).clone();
    assert!(ensure_audio_tracks(&mut p, seq, usize::MAX, usize::MAX, "test").is_err());
    assert!(ensure_audio_tracks(&mut p, seq, 0, 1_000_000, "test").is_err());
    assert!(ensure_audio_tracks(&mut p, ItemId(u64::MAX), 0, 1, "test").is_err());
}

// ---- #2758: audio streams nothing decodes are left out ----

#[test]
fn specs_skip_undecodable_streams_and_start_on_the_destination() {
    // Apple Positional Audio first, AAC second: the AAC stream goes on the destination track
    let specs = crate::commands::audio_placement_specs(&[], &[false, true]);
    assert_eq!(specs, vec![AudioPlacementSpec { track_offset: 0, audio_stream: 1, source_channels: None }]);
    // a channel map describes stream 0: with stream 0 undecodable its further clips go too
    let specs = crate::commands::audio_placement_specs(&[vec![0], vec![1]], &[false, true, false, true]);
    assert_eq!(specs.iter().map(|s| (s.track_offset, s.audio_stream)).collect::<Vec<_>>(), vec![(0, 1), (1, 3)]);
    // decodable stream 0 keeps its channel-map clips ahead of the further streams
    let specs = crate::commands::audio_placement_specs(&[vec![0], vec![1]], &[true, false, true]);
    assert_eq!(specs.iter().map(|s| (s.track_offset, s.audio_stream)).collect::<Vec<_>>(), vec![(0, 0), (1, 0), (2, 2)]);
    assert!(crate::commands::audio_placement_specs(&[vec![0], vec![1]], &[false, false]).is_empty());
}

/// A sequence with one video and one audio track, and a bars-and-tone item whose audio streams
/// decode as `decodable` (stream 0 is the generator's tone).
fn generator_with_streams(decodable: &[bool]) -> (Session, ItemId, ItemId, filmcraft_project::TrackId, filmcraft_project::TrackId) {
    let mut s = Session::default();
    let mut p = (*s.project).clone();
    let rate = FrameRate::FPS_24;
    let seq = p.new_sequence("Streams", Default::default(), 1, 1, None);
    let source = filmcraft_media::generators::GeneratorSource::new(filmcraft_media::Generator::BarsAndTone, 32, 32, rate, rate.tick_of(24));
    let mut info = source.info().clone();
    let tone = info.audio_streams[0].clone();
    info.audio_streams = decodable
        .iter()
        .map(|d| {
            let mut a = tone.clone();
            if !d {
                a.codec = "Apple Positional Audio (apac)".into();
                a.decodable = false;
            }
            a
        })
        .collect();
    let item = p.add_item(
        "iphone",
        filmcraft_project::Label::Iris,
        filmcraft_project::ItemKind::Media(filmcraft_project::MediaClip {
            media: filmcraft_project::MediaRef::Generator(filmcraft_media::Generator::BarsAndTone),
            info,
            interpret: Default::default(),
            mark_in: None,
            mark_out: None,
            markers: vec![],
            offline: false,
            proxy: None,
            identity: None,
        }),
        None,
    );
    let (vdest, adest) = {
        let q = p.sequence(seq).unwrap();
        (q.video_tracks[0].id, q.audio_tracks[0].id)
    };
    s.project = std::sync::Arc::new(p);
    s.state.active_sequence = Some(seq);
    (s, seq, item, vdest, adest)
}

fn place(
    s: &mut Session,
    item: ItemId,
    v: Option<filmcraft_project::TrackId>,
    a: Option<filmcraft_project::TrackId>,
) -> crate::Result<Vec<filmcraft_project::ClipId>> {
    let rate = FrameRate::FPS_24;
    crate::commands::place_item(s, item, TimeRange::new(Tick::ZERO, rate.tick_of(24)), Tick::ZERO, v, a, false, "test", None)
}

fn warnings(s: &Session) -> Vec<String> {
    s.log.entries.iter().filter(|e| e.level == crate::panels::Level::Warning).map(|e| e.message.clone()).collect()
}

#[test]
fn an_undecodable_first_stream_leaves_the_decodable_one_on_the_destination_track() {
    let (mut s, seq, item, vdest, adest) = generator_with_streams(&[false, true]);
    let ids = place(&mut s, item, Some(vdest), Some(adest)).unwrap();
    assert_eq!(ids.len(), 2, "picture and one audio clip");
    let q = s.project.sequence(seq).unwrap();
    assert_eq!(q.audio_tracks.len(), 1, "no track added for the undecodable stream");
    let a = &q.audio_tracks[0].items[0];
    assert_eq!(a.audio_stream, 1);
    assert!(a.source_channels.is_empty());
    assert!(a.link.is_some() && a.link == q.video_tracks[0].items[0].link);
    let w = warnings(&s);
    assert!(w.iter().any(|m| m.contains("stream 1") && m.contains("apac")), "{w:?}");
}

#[test]
fn media_whose_audio_never_decodes_places_its_picture_only() {
    let (mut s, seq, item, vdest, adest) = generator_with_streams(&[false]);
    let ids = place(&mut s, item, Some(vdest), Some(adest)).unwrap();
    assert_eq!(ids.len(), 1);
    let q = s.project.sequence(seq).unwrap();
    assert!(q.audio_tracks[0].items.is_empty());
    assert_eq!(q.video_tracks[0].items.len(), 1);
    assert!(q.video_tracks[0].items[0].link.is_none());
    let w = warnings(&s);
    assert!(w.iter().any(|m| m.contains("picture only")), "{w:?}");
    // the sound alone has nothing to place: refused with the reason, nothing changes
    let (mut s, _, item, _, adest) = generator_with_streams(&[false]);
    let e = place(&mut s, item, None, Some(adest)).unwrap_err().to_string();
    assert!(e.contains("can't decode"), "{e}");
}

#[test]
fn plain_media_places_as_before_without_a_warning() {
    let (mut s, seq, item, vdest, adest) = generator_with_streams(&[true, true]);
    assert_eq!(place(&mut s, item, Some(vdest), Some(adest)).unwrap().len(), 3);
    let q = s.project.sequence(seq).unwrap();
    assert_eq!((q.audio_tracks[0].items[0].audio_stream, q.audio_tracks[1].items[0].audio_stream), (0, 1));
    assert!(warnings(&s).is_empty());
}

#[test]
fn timeline_place_and_new_sequence_from_clip_skip_undecodable_streams() {
    let (mut s, seq, item, _, _) = generator_with_streams(&[false, true, false]);
    let r = s.execute("timeline.place", json!({"item": item.0, "time": 0})).unwrap();
    assert_eq!(r["clips"].as_array().unwrap().len(), 2, "{r}");
    assert_eq!(
        r["skippedAudioStreams"],
        json!([{"stream": 0, "codec": "Apple Positional Audio (apac)"}, {"stream": 2, "codec": "Apple Positional Audio (apac)"}])
    );
    let q = s.project.sequence(seq).unwrap();
    assert_eq!(q.audio_tracks[0].items[0].audio_stream, 1);
    // New Sequence From Clip makes one audio track, with the decodable stream on it
    s.execute("file.newSequenceFromClip", json!({"items": [item.0]})).unwrap();
    let q = s.active_sequence().unwrap();
    assert_eq!(q.audio_tracks.len(), 1);
    assert_eq!(q.audio_tracks[0].items.len(), 1);
    assert_eq!(q.audio_tracks[0].items[0].audio_stream, 1);
    // Media properties list every stream and say which ones decode
    s.state.project_selection = vec![item];
    let props = s.execute("file.mediaProperties", json!({"items": [item.0]})).unwrap();
    let streams = props[0]["audioStreams"].as_array().unwrap().iter().map(|a| a["decodable"].as_bool().unwrap()).collect::<Vec<_>>();
    assert_eq!(streams, vec![false, true, false]);
}

/// A QuickTime movie (audio only) with an Apple Positional Audio track first (a made-up `apac`
/// sample entry: nothing decodes it) and a 16-bit PCM tone second, like an iPhone 17 Pro video.
fn apac_first_mov(with_pcm: bool) -> Vec<u8> {
    use filmcraft_isobmff::{Brand, CodecConfig, FourCc, Mp4Writer, PcmConfig, SampleEntry, TrackConfig, WriteSample, WriterOptions};
    let rate = 48_000u32;
    let mut mux = Mp4Writer::new(std::io::Cursor::new(Vec::new()), WriterOptions::new(Brand::Mov)).unwrap();
    // AudioSampleEntry body: reserved, data reference 1, version fields, 4 channels, 16 bits, 48 kHz
    let mut raw = vec![0u8; 6];
    raw.extend_from_slice(&1u16.to_be_bytes());
    raw.extend_from_slice(&[0; 8]);
    raw.extend_from_slice(&4u16.to_be_bytes());
    raw.extend_from_slice(&16u16.to_be_bytes());
    raw.extend_from_slice(&[0; 4]);
    raw.extend_from_slice(&(rate << 16).to_be_bytes());
    let fourcc = FourCc(*b"apac");
    let entry = SampleEntry::audio(fourcc, CodecConfig::Unknown { fourcc, raw }, 4, f64::from(rate), 16);
    let apac = mux.add_track(TrackConfig::new(entry, rate)).unwrap();
    for _ in 0..10 {
        mux.write_sample(apac, WriteSample { data: &[0x5a; 64], duration: 1024, composition_offset: 0, is_sync: true }).unwrap();
    }
    if with_pcm {
        let entry = SampleEntry::pcm(PcmConfig { bits: 16, float: false, big_endian: false, signed: true, channels: 1, sample_rate: f64::from(rate) });
        let pcm = mux.add_track(TrackConfig::new(entry, rate)).unwrap();
        let data: Vec<u8> = (0..rate / 5).flat_map(|_| 8192i16.to_le_bytes()).collect();
        mux.write_sample(pcm, WriteSample { data: &data, duration: rate / 5, composition_offset: 0, is_sync: true }).unwrap();
    }
    mux.finish().unwrap().into_inner()
}

#[test]
fn an_imported_movie_with_apple_positional_audio_first_plays_its_pcm_stream_on_a1() {
    let dir = filmcraft_testkit::fixtures_dir("engine/apac");
    let path = dir.join("apac_then_pcm.mov");
    std::fs::write(&path, apac_first_mov(true)).unwrap();
    let mut s = demo();
    let r = s.execute("file.import", json!({"paths": [path.to_string_lossy()]})).unwrap();
    let item = ItemId(r["items"][0].as_u64().unwrap_or_else(|| panic!("{r}")));
    let info = s.project.item(item).unwrap().as_media().unwrap().info.clone();
    assert_eq!(info.audio_streams.len(), 2, "{:?}", info.audio_streams);
    assert!(!info.audio_streams[0].can_decode() && info.audio_streams[0].codec.contains("apac"), "{:?}", info.audio_streams[0]);
    assert!(info.audio_streams[1].can_decode());
    let seq = active(&s);
    let a1 = s.project.sequence(seq).unwrap().audio_tracks[0].id;
    let before = s.project.sequence(seq).unwrap().audio_tracks.iter().map(|t| t.items.len()).collect::<Vec<_>>();
    let r = s.execute("timeline.place", json!({"item": item.0, "time": 0, "audioTrack": a1.0})).unwrap();
    assert_eq!(r["clips"].as_array().unwrap().len(), 1, "{r}");
    assert_eq!(r["skippedAudioStreams"][0]["stream"], json!(0));
    let q = s.project.sequence(seq).unwrap();
    let after = q.audio_tracks.iter().map(|t| t.items.len()).collect::<Vec<_>>();
    assert_eq!(after[0], before[0] + 1, "{before:?} -> {after:?}");
    assert_eq!(after[1..], before[1..], "nothing placed below A1");
    let clip = q.audio_tracks[0].items.iter().find(|c| c.item == item).unwrap();
    assert_eq!(clip.audio_stream, 1);
    // and what it plays is the tone, not silence
    let peaks = crate::mixer::clip_peaks(&s, &[clip.id]);
    assert!(peaks.iter().all(|(_, db)| *db > -30.0), "{peaks:?}");
}

#[test]
fn an_imported_movie_with_only_undecodable_audio_is_refused_with_the_reason() {
    let dir = filmcraft_testkit::fixtures_dir("engine/apac");
    let path = dir.join("apac_only.mov");
    std::fs::write(&path, apac_first_mov(false)).unwrap();
    let mut s = demo();
    let r = s.execute("file.import", json!({"paths": [path.to_string_lossy()]})).unwrap();
    let item = ItemId(r["items"][0].as_u64().unwrap_or_else(|| panic!("{r}")));
    let e = s.execute("timeline.place", json!({"item": item.0, "time": 0})).unwrap_err().to_string();
    assert!(e.contains("can't decode") && e.contains("apac"), "{e}");
}

#[test]
fn the_decodable_flag_round_trips_and_older_projects_load_as_decodable() {
    let old: filmcraft_media::AudioStreamInfo =
        serde_json::from_value(json!({"sample_rate": 48000, "channels": 2, "codec": "AAC", "bits_per_sample": null})).unwrap();
    assert!(old.decodable && old.can_decode());
    // a decodable stream is written as before; an undecodable one says so
    assert!(serde_json::to_value(&old).unwrap().get("decodable").is_none());
    let apac = filmcraft_media::AudioStreamInfo { decodable: false, codec: "Apple Positional Audio (apac)".into(), ..old };
    let v = serde_json::to_value(&apac).unwrap();
    assert_eq!(v["decodable"], json!(false));
    assert_eq!(serde_json::from_value::<filmcraft_media::AudioStreamInfo>(v).unwrap(), apac);
}
