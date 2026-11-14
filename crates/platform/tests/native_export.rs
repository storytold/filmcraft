#![cfg(target_os = "windows")]
use filmcraft_export::{ExportSettings, Format, Multiplexer, Progress, export};
use filmcraft_media::generators::GeneratorSource;
use filmcraft_media::{DemoScene, Generator, MediaSource};
use filmcraft_project::{ItemKind, Label, MediaClip, MediaRef, Project, SequenceSettings, TrackKind};
use filmcraft_time::{FrameRate, Tick, TimeRange};
use std::sync::Arc;

#[test]
fn av1_svt_nvenc_and_opus_round_trip() {
    filmcraft_platform::register();
    if !filmcraft_platform::svt_av1_available() {
        assert!(std::env::var_os("FILMCRAFT_REQUIRE_NATIVE_CODECS").is_none(), "requested SVT library must be installed");
        eprintln!("SKIPPED: no SVT-AV1 mainline library");
        return;
    }
    let ffmpeg = filmcraft_testkit::require_ffmpeg!();
    let ffprobe = filmcraft_testkit::require_ffprobe!();
    let rate = FrameRate::FPS_24;
    for (working, transfer) in [
        (filmcraft_color::WorkingSpace::Rec709, "bt709"),
        (filmcraft_color::WorkingSpace::Rec2100Pq, "smpte2084"),
        (filmcraft_color::WorkingSpace::Rec2100Hlg, "arib-std-b67"),
    ] {
        let mut p = Project::new("AV1 regression");
        let g = GeneratorSource::new(Generator::Demo(DemoScene::OceanSunset), 320, 180, rate, rate.tick_of(32));
        let info = g.info().clone();
        let clip = p.add_item(
            &info.name.clone(),
            Label::Iris,
            ItemKind::Media(Box::new(MediaClip {
                media: MediaRef::Generator(g.generator.clone()),
                info,
                interpret: Default::default(),
                mark_in: None,
                mark_out: None,
                markers: vec![],
                offline: false,
                proxy: None,
                identity: None,
            })),
            None,
        );
        let audio_clip = p.make_track_item(clip, TrackKind::Audio, Tick::ZERO, TimeRange::new(Tick::ZERO, rate.tick_of(32)), rate).unwrap();
        let color = filmcraft_color::ColorPipeline { working, ..filmcraft_color::ColorPipeline::REC709 };
        let seq = p.new_sequence("s", SequenceSettings { width: 320, height: 180, frame_rate: rate, color, ..Default::default() }, 1, 1, None);
        let v = p.make_track_item(clip, TrackKind::Video, Tick::ZERO, TimeRange::new(Tick::ZERO, rate.tick_of(32)), rate).unwrap();
        p.sequence_mut(seq).unwrap().video_tracks[0].items.push(v);
        p.sequence_mut(seq).unwrap().audio_tracks[0].items.push(audio_clip);
        let mut sources = filmcraft_render::SourceMap::default();
        sources.0.insert(clip, Arc::new(g));
        let p = Arc::new(p);
        for (mode, keyint, channels) in [
            (filmcraft_export::BitrateMode::Crf, Some(8), 2),
            (filmcraft_export::BitrateMode::Crf, None, 6),
            (filmcraft_export::BitrateMode::Vbr1Pass, Some(8), 2),
        ] {
            if mode == filmcraft_export::BitrateMode::Vbr1Pass && !filmcraft_platform::nvenc::av1_available() {
                eprintln!("SKIPPED: no NVENC AV1 encoder");
                continue;
            }
            for (multiplexer, ext) in [(Multiplexer::Mp4, "mp4"), (Multiplexer::Mkv, "mkv")] {
                let path =
                    filmcraft_testkit::temp_path(&filmcraft_testkit::fixtures_dir("native-av1").join(format!("{}-{mode:?}-{keyint:?}.{ext}", working.id())));
                let settings = ExportSettings {
                    format: Format::Av1,
                    multiplexer,
                    path: path.to_string_lossy().into(),
                    include_audio: true,
                    audio: filmcraft_export::AudioSettings { codec: filmcraft_export::AudioCodec::Opus, channels, ..Default::default() },
                    bitrate_kbps: 8000,
                    keyframe_distance: keyint,
                    bitrate_mode: mode,
                    crf: 35.0,
                    av1_preset: 10,
                    ..Default::default()
                };
                assert_eq!(export(&p, seq, &settings, &sources, &Progress::default()).unwrap().frames, 32);
                verify_video(&ffmpeg, &ffprobe, &path, &settings, transfer);
            }
        }
        if working == filmcraft_color::WorkingSpace::Rec709 {
            for codec in [filmcraft_export::AudioCodec::Aac, filmcraft_export::AudioCodec::Flac] {
                let path = filmcraft_testkit::temp_path(&filmcraft_testkit::fixtures_dir("native-av1").join(format!("audio-{codec:?}.mkv")));
                let settings = ExportSettings {
                    format: Format::Av1,
                    bitrate_mode: filmcraft_export::BitrateMode::Crf,
                    av1_preset: 10,
                    path: path.to_string_lossy().into(),
                    audio: filmcraft_export::AudioSettings { codec, ..Default::default() },
                    ..Default::default()
                };
                export(&p, seq, &settings, &sources, &Progress::default()).unwrap();
                let out = std::process::Command::new(&ffmpeg).args(["-v", "error", "-i"]).arg(&path).args(["-vn", "-f", "f32le", "-"]).output().unwrap();
                assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
                assert_eq!(out.stdout.len() / 8, 64000, "Matroska {codec:?} sample count");
            }
        }
        for channels in [1, 2, 6] {
            let path = filmcraft_testkit::temp_path(&filmcraft_testkit::fixtures_dir("native-opus").join(format!("{}-{channels}.opus", working.id())));
            let settings = ExportSettings {
                format: Format::Opus,
                path: path.to_string_lossy().into(),
                audio: filmcraft_export::AudioSettings { codec: filmcraft_export::AudioCodec::Opus, channels, ..Default::default() },
                ..Default::default()
            };
            export(&p, seq, &settings, &sources, &Progress::default()).unwrap();
            let out = std::process::Command::new(&ffmpeg).args(["-v", "error", "-i"]).arg(&path).args(["-f", "f32le", "-"]).output().unwrap();
            assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
            assert_eq!(out.stdout.len() / (channels as usize * 4), 64000, "Ogg Opus must trim pre-skip and tail padding");
            assert!(out.stdout.as_chunks::<4>().0.iter().any(|b| f32::from_le_bytes(*b).abs() > 0.001), "audio is not silent");
        }
    }
}

fn verify_video(ffmpeg: &std::path::Path, ffprobe: &std::path::Path, path: &std::path::Path, settings: &ExportSettings, transfer: &str) {
    let ext = settings.extension();
    let keyint = settings.keyframe_distance;
    let channels = settings.audio.channels;
    let mode = settings.bitrate_mode;
    let pixel_format = if mode == filmcraft_export::BitrateMode::Vbr1Pass && transfer == "bt709" { "yuv420p" } else { "yuv420p10le" };
    let out = std::process::Command::new(ffprobe)
        .args(["-v", "error", "-count_frames", "-show_entries", "stream=codec_name,codec_tag_string,pix_fmt,color_transfer,nb_read_frames", "-of", "json"])
        .arg(path)
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let info = String::from_utf8_lossy(&out.stdout);
    for expected in ["av1", "av01", pixel_format, transfer, "\"nb_read_frames\": \"32\""] {
        if expected == "av01" && ext == "mkv" {
            continue;
        }

        assert!(info.contains(expected), "{expected}: {info}");
    }
    assert!(info.contains("opus"), "{info}");
    if ext == "mkv" {
        assert_eq!(&std::fs::read(path).unwrap()[..4], &[0x1a, 0x45, 0xdf, 0xa3]);
        let seek = std::process::Command::new(ffmpeg)
            .args(["-v", "error", "-ss", "0.5", "-i"])
            .arg(path)
            .args(["-map", "0:v:0", "-frames:v", "1", "-f", "rawvideo", "-pix_fmt", "rgb24", "-"])
            .output()
            .unwrap();
        assert!(seek.status.success(), "{}", String::from_utf8_lossy(&seek.stderr));
        assert_eq!(seek.stdout.len(), 320 * 180 * 3);
        let audio = std::process::Command::new(ffmpeg).args(["-v", "error", "-ss", "0.5", "-i"]).arg(path).args(["-vn", "-f", "f32le", "-"]).output().unwrap();
        assert!(audio.status.success());
        assert_eq!(audio.stdout.len() / (channels as usize * 4), 40000, "seeking Matroska must retain the remaining audio");
    }
    if ext == "mp4" {
        let timing =
            std::process::Command::new(ffprobe).args(["-v", "error", "-show_entries", "stream=start_time,duration", "-of", "json"]).arg(path).output().unwrap();
        let timing: serde_json::Value = serde_json::from_slice(&timing.stdout).unwrap();
        for stream in timing["streams"].as_array().unwrap() {
            assert_eq!(stream["start_time"].as_str().unwrap().parse::<f64>().unwrap(), 0.0);
            assert!((stream["duration"].as_str().unwrap().parse::<f64>().unwrap() - 32.0 / 24.0).abs() < 0.000002);
        }
    }
    let out = std::process::Command::new(ffprobe)
        .args(["-v", "error", "-select_streams", "v:0", "-show_frames", "-show_entries", "frame=key_frame", "-of", "csv=p=0"])
        .arg(path)
        .output()
        .unwrap();
    let flags: Vec<bool> = String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|l| l.split(',').next())
        .filter_map(|l| match l.trim() {
            "1" => Some(true),
            "0" => Some(false),
            _ => None,
        })
        .collect();
    if keyint.is_some() {
        assert_eq!(flags.iter().enumerate().filter_map(|(i, k)| k.then_some(i)).collect::<Vec<_>>(), vec![0, 8, 16, 24]);
    }
    let audio = std::process::Command::new(ffmpeg).args(["-v", "error", "-i"]).arg(path).args(["-vn", "-f", "f32le", "-"]).output().unwrap();
    assert!(audio.status.success());
    assert_eq!(audio.stdout.len() / (channels as usize * 4), 64000, "Opus audio duration");
    let out = std::process::Command::new(ffmpeg).args(["-v", "error", "-xerror", "-i"]).arg(path).args(["-f", "null", "-"]).output().unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
}
