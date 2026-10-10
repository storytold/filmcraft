//! Multi-clip audio placement: `audio_placement_specs` and dynamic audio track allocation.

use filmcraft_media::MediaSource;
use filmcraft_project::{AudioChannels, ItemId, TrackKind};
use filmcraft_time::{FrameRate, Tick, TimeRange};

use serde_json::json;

use crate::Session;
use crate::commands::{AudioPlacementSpec, audio_placement_specs, ensure_audio_tracks};

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

#[test]
fn hostile_track_counts_are_an_error_not_a_crash() {
    let s = demo();
    let seq = active(&s);
    let mut p = (*s.project).clone();
    assert!(ensure_audio_tracks(&mut p, seq, usize::MAX, usize::MAX, "test").is_err());
    assert!(ensure_audio_tracks(&mut p, seq, 0, 1_000_000, "test").is_err());
    assert!(ensure_audio_tracks(&mut p, ItemId(u64::MAX), 0, 1, "test").is_err());
}
