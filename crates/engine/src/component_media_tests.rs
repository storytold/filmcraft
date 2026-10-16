use super::*;
use filmcraft_media::FrameRequest;
use filmcraft_project::MediaRef;
use serde_json::json;

#[test]
fn component_media_missing_picture_retains_sound_and_legacy_references_load() {
    use filmcraft_media::{
        MediaSource,
        generators::{Generator, GeneratorSource},
    };
    let (mut s, channels, dir) = component_audio_tests::fixtures("missing-picture");
    let picture = filmcraft_project::PictureComponent {
        format_pending: false,
        track: 1,
        file_package: [1; 32],
        path: None,
        source_start: Tick::ZERO,
        length: Tick::from_units(100, 48000),
    };
    let generator = GeneratorSource::new(Generator::BlackVideo, 16, 16, filmcraft_time::FrameRate::FPS_24, picture.length);
    let mut info = generator.info().clone();
    info.audio_streams = vec![filmcraft_media::AudioStreamInfo { sample_rate: 48000, channels: 2, codec: "PCM".into(), bits_per_sample: Some(16) }];
    let (source, issues) = component_media::open(info.clone(), &channels, Some(&picture), &*s.services).unwrap();
    assert_eq!(issues.len(), 1);
    assert!(issues[0].starts_with("V1:"));
    assert!(source.video_frame(FrameRequest::full(Tick::ZERO)).is_ok());
    assert_eq!(source.audio(0, 8, 48000).unwrap().channels[1], vec![-8000.0 / 32768.0; 8]);
    assert!(source.video_frame(FrameRequest::full(Tick(-1))).is_err());
    assert!(source.video_frame(FrameRequest::full(picture.length)).is_err());
    let mut wrong = picture.clone();
    wrong.path = channels[0].path.clone();
    wrong.file_package = channels[0].file_package;
    assert!(component_media::open_picture(&wrong, &*s.services).is_err());
    let id = Arc::make_mut(&mut s.project).add_item(
        "partial",
        filmcraft_project::Label::Blue,
        filmcraft_project::ItemKind::Media(filmcraft_project::MediaClip {
            media: MediaRef::Components { picture: Some(picture), channels: channels.clone() },
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
    let before = s.project.clone();
    for p in [json!(null), json!({"item":id,"path":"relative.mxf"}), json!({"item":id,"path":channels[0].path})] {
        assert!(s.execute("media.relinkPictureComponent", p).is_err());
        assert_eq!(s.project, before);
    }
    assert_eq!(crate::relink::scan(&s.project, &*s.services).len(), 1);
    // Pending format is a distinct reason for staying offline, even if a path exists.
    let m = Arc::make_mut(&mut s.project).item_mut(id).unwrap().as_media_mut().unwrap();
    let MediaRef::Components { picture: Some(picture), .. } = &mut m.media else { panic!("picture component") };
    picture.format_pending = true;
    picture.path = channels[0].path.clone();
    let status = s.execute("media.status", json!({"item":id})).unwrap();
    assert_eq!(status[0]["status"], "partial");
    let picture_status = status[0]["components"].as_array().unwrap().iter().find(|c| c["kind"] == "picture").unwrap();
    assert_eq!(picture_status["formatPending"], true);
    assert_eq!(picture_status["present"], false);
    assert!(picture_status["error"].as_str().unwrap().contains("pending identity-checked relink"));
    let old = json!({"AudioComponents":{"channels":channels}});
    let reference: MediaRef = serde_json::from_value(old).unwrap();
    assert!(matches!(reference,MediaRef::Components { picture:None, channels } if channels.len()==2));
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
#[ignore = "set FILMCRAFT_AVID_PHASE12_REPORT; ffmpeg oracle; optional FILMCRAFT_AVID_PLAYBACK_OUTPUT saves test project"]
fn component_media_local_picture_audio_and_oracle() {
    let mut s = Session::default();
    s.execute("file.importAvidCatalog", json!({"path":std::env::var("FILMCRAFT_AVID_PHASE12_REPORT").unwrap()})).unwrap();
    let id = s.project.items.values().find(|i| i.name.starts_with("hf_")).unwrap().id;
    s.execute("media.bindAvidMedia", json!({"items":[id]})).unwrap();
    // This disposable playback fixture contains only the source under test, avoiding
    // unrelated unbound placeholders and the startup Link Media dialog.
    let unused: Vec<_> = s.project.items.keys().copied().filter(|item| *item != id).collect();
    let p = Arc::make_mut(&mut s.project);
    for item in unused {
        p.items.remove(&item);
        p.root.remove_item(item);
    }
    let m = s.project.item(id).unwrap().as_media().unwrap();
    let duration = m.info.duration;
    assert_eq!(duration, Tick::from_units(242, 24));
    let MediaRef::Components { channels, picture: Some(picture) } = &m.media else { panic!("expected picture/audio components") };
    let picture = picture.clone();
    assert_eq!(channels.len(), 2);
    let source = s.media.full_res_source(&s.project, id, &*s.services).unwrap();
    for (index, c) in channels.iter().enumerate() {
        let oracle =
            std::process::Command::new("ffmpeg").args(["-v", "error", "-i", c.path.as_deref().unwrap(), "-map", "0:a:0", "-f", "f32le", "-"]).output().unwrap();
        assert!(oracle.status.success());
        let expected: Vec<f32> = oracle.stdout.chunks_exact(4).map(|x| f32::from_le_bytes(x.try_into().unwrap())).collect();
        let actual = source.audio(0, 484000, 48000).unwrap();
        assert!(expected.len() >= 484000);
        assert!(expected.iter().any(|x| x.abs() > 0.01));
        assert!(actual.channels[index].iter().zip(&expected).all(|(a, b)| (a - b).abs() < 1e-6));
    }
    for frame in [0, 120, 21, 241, 87] {
        let actual = source.video_frame(FrameRequest::full(Tick::from_units(frame, 24))).unwrap();
        let filmcraft_frame::PixelData::Yuv8 { planes, .. } = &actual.data else { panic!("expected YUV8") };
        assert_eq!((actual.width, actual.height), (1920, 1080));
        let oracle = std::process::Command::new("ffmpeg")
            .args([
                "-v",
                "error",
                "-i",
                picture.path.as_deref().unwrap(),
                "-vf",
                &format!("select=eq(n\\,{frame})"),
                "-frames:v",
                "1",
                "-pix_fmt",
                "yuv422p",
                "-f",
                "rawvideo",
                "-",
            ])
            .output()
            .unwrap();
        assert!(oracle.status.success());
        assert_eq!(oracle.stdout.len(), 1920 * 1080 * 2);
        let max_diff = planes.iter().flat_map(|p| p.iter()).zip(&oracle.stdout).map(|(a, b)| a.abs_diff(*b)).max().unwrap();
        assert!(max_diff <= 2, "frame {frame}: max pixel difference {max_diff}");
        eprintln!("picture frame {frame}: maximum YUV difference {max_diff}");
    }
    let settings = filmcraft_project::SequenceSettings { frame_rate: filmcraft_time::FrameRate::FPS_24, ..Default::default() };
    let seq = Arc::make_mut(&mut s.project).new_sequence("Phase 12 picture and stereo audio", settings, 1, 1, None);
    s.state.active_sequence = Some(seq);
    let q = s.project.sequence(seq).unwrap();
    let (v, a) = (q.video_tracks[0].id, q.audio_tracks[0].id);
    let placed = crate::commands::place_item(
        &mut s,
        id,
        filmcraft_time::TimeRange::new(Tick::ZERO, duration),
        Tick::ZERO,
        Some(v),
        Some(a),
        false,
        "Test placement",
        None,
    )
    .unwrap();
    assert_eq!(placed.len(), 2);
    let saved = filmcraft_format::encode(&s.project, false);
    let reopened = Session { project: Arc::new(filmcraft_format::decode(&saved).unwrap().project), ..Default::default() };
    let provider = reopened.media.provider(reopened.project.clone(), reopened.services.clone());
    let reopened_source = reopened.media.full_res_source(&reopened.project, id, &*reopened.services).unwrap();
    for start in [0, 42000, 240000, 482976] {
        let direct = source.audio(start, 1024, 48000).unwrap();
        assert_eq!(reopened_source.audio(start, 1024, 48000).unwrap(), direct);
        let mixed = filmcraft_render::mixer::mix_graph(&reopened.project, reopened.project.sequence(seq).unwrap(), start, 1024, &provider, None);
        for (a, b) in mixed.channels.iter().zip(&direct.channels) {
            assert!(a.iter().zip(b).all(|(a, b)| (a - b).abs() < 1e-6));
        }
    }
    assert!(reopened_source.video_frame(FrameRequest::full(Tick::from_units(21, 24))).is_ok());
    let mut partial = Session { project: reopened.project.clone(), ..Default::default() };
    let media = Arc::make_mut(&mut partial.project).item_mut(id).unwrap().as_media_mut().unwrap();
    let MediaRef::Components { picture: Some(c), .. } = &mut media.media else { panic!("missing picture binding") };
    c.path = None;
    let offline = partial.media.full_res_source(&partial.project, id, &*partial.services).unwrap();
    assert!(matches!(offline.video_frame(FrameRequest::full(Tick::ZERO)).unwrap().data, filmcraft_frame::PixelData::Rgba8(_)));
    assert_eq!(offline.audio(42000, 1024, 48000).unwrap(), source.audio(42000, 1024, 48000).unwrap());
    partial.execute("media.relinkPictureComponent", json!({"item":id,"path":picture.path})).unwrap();
    let restored = partial.media.full_res_source(&partial.project, id, &*partial.services).unwrap();
    assert!(matches!(restored.video_frame(FrameRequest::full(Tick::ZERO)).unwrap().data, filmcraft_frame::PixelData::Yuv8 { .. }));
    partial.execute("edit.undo", json!({})).unwrap();
    let offline = partial.media.full_res_source(&partial.project, id, &*partial.services).unwrap();
    assert!(matches!(offline.video_frame(FrameRequest::full(Tick::ZERO)).unwrap().data, filmcraft_frame::PixelData::Rgba8(_)));
    if let Ok(path) = std::env::var("FILMCRAFT_AVID_PLAYBACK_OUTPUT") {
        s.execute("file.saveAs", json!({"path":path})).unwrap();
    }
    if let Ok(path) = std::env::var("FILMCRAFT_AVID_LEGACY_PROJECT") {
        let legacy = filmcraft_format::decode(&std::fs::read(path).unwrap()).unwrap();
        assert_eq!(legacy.schema_version, 16);
        assert!(
            legacy
                .project
                .items
                .values()
                .any(|i| i.as_media().is_some_and(|m| matches!(&m.media,MediaRef::Components { picture:None,channels } if channels.len()==2)))
        );
    }
    eprintln!("Phase 12: picture+stereo source, full PCM, five picture seeks, timeline mixing and save/reopen passed");
}
