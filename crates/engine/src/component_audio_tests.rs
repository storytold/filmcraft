use super::*;
use filmcraft_media::{AudioStreamInfo, MediaInfo, MediaKind};
use filmcraft_mxf::{OpAtomPcm, PackageIds};
use filmcraft_project::{AudioComponent, ItemKind, MediaRef};
use serde_json::json;

#[test]
fn component_audio_commands_reject_hostile_parameters_without_edits() {
    let mut s = Session::default();
    let before = s.project.clone();
    for params in [json!(null), json!([]), json!({"items":[]}), json!({"items":[-1]}), json!({"items":["1"]}), json!({"items":vec![0; 65]})] {
        assert!(s.execute("media.bindAvidAudio", params.clone()).is_err());
        assert!(s.execute("media.bindAvidMedia", params).is_err());
        assert_eq!(s.project, before);
    }
    for params in [json!(null), json!({"item":0,"channel":-1,"path":"/missing"}), json!({"item":0,"channel":0,"path":"relative.mxf"})] {
        assert!(s.execute("media.relinkAudioComponent", params).is_err());
        assert_eq!(s.project, before);
    }
}

fn info() -> MediaInfo {
    MediaInfo {
        name: "Stereo".into(),
        kind: MediaKind::AudioOnly,
        duration: Tick::from_units(100, 48000),
        video: None,
        audio_streams: vec![AudioStreamInfo { sample_rate: 48000, channels: 2, codec: "PCM".into(), bits_per_sample: Some(16) }],
        container: "MXF audio components".into(),
        start_timecode: None,
        file_size: None,
    }
}
pub(super) fn fixtures(name: &str) -> (Session, Vec<filmcraft_project::AudioComponent>, std::path::PathBuf) {
    let dir = std::env::temp_dir().join(format!("filmcraft-component-audio-{}-{name}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let mut channels = Vec::new();
    for (i, signal) in [4000, -8000].into_iter().enumerate() {
        let ids = PackageIds::from_seed(&format!("synthetic-{i}"), "test");
        let bytes = filmcraft_mxf::write_opatom_pcm(
            &OpAtomPcm { sample_rate: 48000, bits: 16, channels: 1, edit_rate: None, ids: ids.clone(), timecode: None },
            &vec![signal; 100],
        )
        .unwrap();
        let path = dir.join(format!("channel-{i}.mxf"));
        std::fs::write(&path, bytes).unwrap();
        channels.push(AudioComponent {
            track: i as u32 + 1,
            file_package: ids.file.0,
            path: Some(path.to_str().unwrap().into()),
            stream: 0,
            channel: 0,
            source_start: Tick::ZERO,
            length: info().duration,
        });
    }
    (Session::default(), channels, dir)
}
#[test]
fn component_audio_routes_missing_slots_ranges_and_rejects_wrong_identity() {
    let (s, channels, dir) = fixtures("routing");
    let (source, failures) = component_audio::open(info(), &channels, &*s.services).unwrap();
    assert!(failures.is_empty());
    let a = source.audio(25, 8, 48000).unwrap();
    assert_eq!(a.channels[0], vec![4000.0 / 32768.0; 8]);
    assert_eq!(a.channels[1], vec![-8000.0 / 32768.0; 8]);
    let a = source.audio(-2, 4, 48000).unwrap();
    assert_eq!(&a.channels[0][..2], &[0.0, 0.0]);
    let a = source.audio(98, 4, 48000).unwrap();
    assert_eq!(&a.channels[1][2..], &[0.0, 0.0]);
    assert!(source.audio(0, usize::MAX, 48000).is_err());
    assert!(source.audio(0, 1, 0).is_err());
    let mut partial = channels.clone();
    partial[0].path = None;
    let (source, failures) = component_audio::open(info(), &partial, &*s.services).unwrap();
    assert_eq!(failures.len(), 1);
    let a = source.audio(0, 8, 48000).unwrap();
    assert_eq!(a.channels[0], vec![0.0; 8]);
    assert_eq!(a.channels[1], vec![-8000.0 / 32768.0; 8]);
    let mut wrong = channels[0].clone();
    wrong.path = channels[1].path.clone();
    assert!(component_audio::open_component(&wrong, &*s.services).is_err());
    let mut p = filmcraft_project::Project::new("test");
    let item = p.add_item(
        "Stereo",
        filmcraft_project::Label::Blue,
        ItemKind::Media(filmcraft_project::MediaClip {
            media: MediaRef::Components { channels: partial, picture: None },
            info: info(),
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
    let saved = filmcraft_format::encode(&p, false);
    let p = filmcraft_format::decode(&saved).unwrap().project;
    let mut session = Session { project: Arc::new(p), ..Default::default() };
    let before = session.project.clone();
    assert!(crate::proxies::attach(&mut session, &[(item, "unused-proxy.mxf".into())], true).is_err());
    assert_eq!(session.project, before);
    assert!(session.execute("media.relinkAudioComponent", json!({"item":item,"channel":0,"path":channels[1].path})).is_err());
    assert_eq!(session.project, before);
    session.execute("media.relinkAudioComponent", json!({"item":item,"channel":0,"path":channels[0].path})).unwrap();
    let source = session.media.full_res_source(&session.project, item, &*session.services).unwrap();
    assert_eq!(source.audio(0, 8, 48000).unwrap().channels[0], vec![4000.0 / 32768.0; 8]);
    session.execute("edit.undo", json!({})).unwrap();
    let source = session.media.full_res_source(&session.project, item, &*session.services).unwrap();
    assert_eq!(source.audio(0, 8, 48000).unwrap().channels[0], vec![0.0; 8]);
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn component_audio_binding_is_atomic_and_preserves_partial_stereo() {
    let (mut s, channels, dir) = fixtures("binding");
    let tracks: Vec<_> = channels
        .iter()
        .enumerate()
        .map(|(i, c)| {
            let raw: String = c.file_package.iter().map(|b| format!("{b:02x}")).collect();
            json!({"index":c.track,"component":{"class":"SCLP","media_kind":2,"source_id":{"raw":raw,"byte_order":"big"},"source_track":c.track,"start":0,"length":100,"rate":{"numerator":48000,"denominator":1}},"references":[{
                "status":if i == 0 {"absent"} else {"present"},
                "source_id":{"raw":raw,"byte_order":"big"},"source_track":c.track,"source_start":0,"length":100,
                "rate":{"numerator":48000,"denominator":1},
                "locations":[{"folder":dir.to_str().unwrap(),"filename":format!("channel-{i}.mxf")}]
            }]})
        })
        .collect();
    let evidence =
        json!({"version":1,"clip":{"workflow_evidence":"material_identity_in_managed_pmr", "tracks":tracks,"attributes":{"_CHANNEL_GROUP_LIST":"ST:A1A2,"}}});
    let media = filmcraft_project::MediaClip {
        media: MediaRef::File { path: String::new() },
        info: info(),
        interpret: Default::default(),
        mark_in: None,
        mark_out: None,
        markers: vec![],
        offline: true,
        proxy: None,
        identity: None,
    };
    let p = Arc::make_mut(&mut s.project);
    let good = p.add_item("good", filmcraft_project::Label::Blue, ItemKind::Media(media.clone()), None);
    p.item_mut(good).unwrap().metadata.insert(filmcraft_interchange::avid_catalog::SOURCE_KEY.into(), evidence.to_string());
    let invalid = p.add_item("invalid", filmcraft_project::Label::Blue, ItemKind::Media(media), None);
    let before = s.project.clone();
    for ids in [vec![good, good], vec![good, invalid]] {
        assert!(s.execute("media.bindAvidAudio", json!({"items":ids})).is_err());
        assert_eq!(s.project, before);
        assert!(s.history.undo.is_empty());
    }
    s.execute("media.bindAvidAudio", json!({"items":[good]})).unwrap();
    assert_eq!(s.history.undo.len(), 1);
    let source = s.media.full_res_source(&s.project, good, &*s.services).unwrap();
    let a = source.audio(0, 8, 48000).unwrap();
    assert_eq!(a.channels[0], vec![0.0; 8]);
    assert_eq!(a.channels[1], vec![-8000.0 / 32768.0; 8]);
    s.execute("edit.undo", json!({})).unwrap();
    assert_eq!(s.project, before);
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
#[ignore = "set FILMCRAFT_AVID_PHASE9_REPORT and FILMCRAFT_AVID_PHASE10_REPORT; optional FILMCRAFT_AVID_PLAYBACK_OUTPUT saves a native test project"]
fn component_audio_local_binding_placement_and_reopen() {
    for (key, expected_placements) in [("FILMCRAFT_AVID_PHASE9_REPORT", 2), ("FILMCRAFT_AVID_PHASE10_REPORT", 1)] {
        let mut s = Session::default();
        s.execute("file.importAvidCatalog", json!({"path":std::env::var(key).unwrap()})).unwrap();
        let ids: Vec<_> = s.project.items.keys().copied().collect();
        s.execute("media.bindAvidAudio", json!({"items":ids})).unwrap();
        let stereo = s.project.items.values().find(|i| i.name == "B_stereo.wav").unwrap().id;
        let m = s.project.item(stereo).unwrap().as_media().unwrap();
        let MediaRef::Components { channels, .. } = &m.media else { panic!("expected components") };
        assert_eq!(channels.len(), 2);
        let source = s.media.full_res_source(&s.project, stereo, &*s.services).unwrap();
        for start in [0, 48000, 192000, 239000] {
            let combined = source.audio(start, 1000, 48000).unwrap();
            for (i, c) in channels.iter().enumerate() {
                let direct = component_audio::open_component(c, &*s.services).unwrap().audio(start, 1000, 48000).unwrap();
                assert_eq!(combined.channels[i], direct.channels[0]);
            }
        }
        let seq = Arc::make_mut(&mut s.project).new_sequence("Avid audio playback test", Default::default(), 1, 2, None);
        s.state.active_sequence = Some(seq);
        let dest = s.project.sequence(seq).unwrap().audio_tracks[0].id;
        let inserted = crate::commands::place_item(
            &mut s,
            stereo,
            filmcraft_time::TimeRange::new(Tick::ZERO, Tick::from_units(5, 1)),
            Tick::ZERO,
            None,
            Some(dest),
            false,
            "Test placement",
            None,
        )
        .unwrap();
        assert_eq!(inserted.len(), expected_placements);
        if expected_placements == 1 {
            let provider = s.media.provider(s.project.clone(), s.services.clone());
            let mixed = filmcraft_render::mixer::mix_graph(&s.project, s.project.sequence(seq).unwrap(), 48000, 1000, &provider, None);
            let direct = source.audio(48000, 1000, 48000).unwrap();
            assert_eq!(mixed.channels.len(), 2);
            for (actual, expected) in mixed.channels.iter().zip(&direct.channels) {
                assert!(actual.iter().zip(expected).all(|(a, e)| (a - e).abs() < 0.000001));
            }
        }
        let saved = filmcraft_format::encode(&s.project, false);
        let reopened = Session { project: Arc::new(filmcraft_format::decode(&saved).unwrap().project), ..Default::default() };
        let reopened_source = reopened.media.full_res_source(&reopened.project, stereo, &*reopened.services).unwrap();
        assert_eq!(reopened_source.audio(48000, 1000, 48000).unwrap(), source.audio(48000, 1000, 48000).unwrap());
        if key == "FILMCRAFT_AVID_PHASE10_REPORT"
            && let Ok(path) = std::env::var("FILMCRAFT_AVID_PLAYBACK_OUTPUT")
        {
            s.execute("file.saveAs", json!({"path":path})).unwrap();
        }
        eprintln!("{key}: binding, four seeks, {expected_placements} audio placement(s), save/reopen passed");
    }
}

#[test]
#[ignore = "set FILMCRAFT_AVID_PHASE11_REPORT; ffmpeg is an external oracle; optional FILMCRAFT_AVID_PLAYBACK_OUTPUT saves a native project"]
fn component_audio_local_audible_pcm_and_timeline() {
    let mut s = Session::default();
    s.execute("file.importAvidCatalog", json!({"path":std::env::var("FILMCRAFT_AVID_PHASE11_REPORT").unwrap()})).unwrap();
    let ids: Vec<_> = s.project.items.keys().copied().collect();
    assert_eq!(ids.len(), 1);
    let item = ids[0];
    s.execute("media.bindAvidAudio", json!({"items":ids})).unwrap();
    let media = s.project.item(item).unwrap().as_media().unwrap();
    let duration = media.info.duration;
    assert_eq!(duration, Tick::from_units(531, 24));
    let MediaRef::Components { channels, .. } = &media.media else { panic!("expected components") };
    assert_eq!(channels.len(), 2);
    let source = s.media.full_res_source(&s.project, item, &*s.services).unwrap();
    let count = 1_062_000;
    let combined = source.audio(0, count, 48000).unwrap();
    for (i, c) in channels.iter().enumerate() {
        let oracle =
            std::process::Command::new("ffmpeg").args(["-v", "error", "-i", c.path.as_deref().unwrap(), "-map", "0:a:0", "-f", "f32le", "-"]).output().unwrap();
        assert!(oracle.status.success());
        let expected: Vec<f32> = oracle.stdout.chunks_exact(4).map(|b| f32::from_le_bytes(b.try_into().unwrap())).collect();
        // The source MXFs contain one sample beyond the AVB's frame-aligned master range.
        assert_eq!(expected.len(), count + 1);
        assert!(expected.iter().any(|x| x.abs() > 0.01), "silent fixtures cannot validate audible decoding");
        let native = component_audio::open_component(c, &*s.services).unwrap().audio(0, expected.len(), 48000).unwrap();
        assert_eq!(native.channels[0].len(), expected.len());
        assert!(native.channels[0].iter().zip(&expected).all(|(a, b)| (a - b).abs() < 1e-6));
        assert!(combined.channels[i].iter().zip(&expected[..count]).all(|(a, b)| (a - b).abs() < 1e-6));
    }
    let settings = filmcraft_project::SequenceSettings { frame_rate: filmcraft_time::FrameRate::FPS_24, ..Default::default() };
    let seq = Arc::make_mut(&mut s.project).new_sequence("Phase 11 audible playback", settings, 1, 2, None);
    s.state.active_sequence = Some(seq);
    let dest = s.project.sequence(seq).unwrap().audio_tracks[0].id;
    let inserted = crate::commands::place_item(
        &mut s,
        item,
        filmcraft_time::TimeRange::new(Tick::ZERO, duration),
        Tick::ZERO,
        None,
        Some(dest),
        false,
        "Test placement",
        None,
    )
    .unwrap();
    assert_eq!(inserted.len(), 2, "Phase 11 has two mono source channels, not an Avid stereo pair");
    // Explicit listening-test routing; this is not an inferred Avid sequence or pan setting.
    let q = Arc::make_mut(&mut s.project).sequence_mut(seq).unwrap();
    q.audio_tracks[0].pan = -100.0;
    q.audio_tracks[1].pan = 100.0;
    let saved = filmcraft_format::encode(&s.project, false);
    let reopened = Session { project: Arc::new(filmcraft_format::decode(&saved).unwrap().project), ..Default::default() };
    let reopened_source = reopened.media.full_res_source(&reopened.project, item, &*reopened.services).unwrap();
    let peak_at = combined.channels[0].iter().enumerate().max_by(|(_, a), (_, b)| a.abs().total_cmp(&b.abs())).unwrap().0;
    let provider = reopened.media.provider(reopened.project.clone(), reopened.services.clone());
    for start in [0, count / 2, peak_at.saturating_sub(512).min(count - 1024), count - 1024] {
        let actual = reopened_source.audio(start as i64, 1024, 48000).unwrap();
        let mixed = filmcraft_render::mixer::mix_graph(&reopened.project, reopened.project.sequence(seq).unwrap(), start as i64, 1024, &provider, None);
        for i in 0..2 {
            assert_eq!(actual.channels[i], combined.channels[i][start..start + 1024]);
            assert!(mixed.channels[i].iter().zip(&actual.channels[i]).all(|(a, b)| (a - b).abs() < 1e-6));
        }
    }
    if let Ok(path) = std::env::var("FILMCRAFT_AVID_PLAYBACK_OUTPUT") {
        s.execute("file.saveAs", json!({"path":path})).unwrap();
    }
    eprintln!(
        "Phase 11: both non-silent mono MXFs match FFmpeg across all 1,062,001 samples; 1,062,000-sample master, timeline routing, seeks and save/reopen passed"
    );
}
