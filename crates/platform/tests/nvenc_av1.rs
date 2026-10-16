//! AV1 NVENC Main 10 regression: bit depth, colour signaling, references and keyframes.
#![cfg(target_os = "windows")]
use filmcraft_platform::nvenc::{Config, Nvenc, Profile, Signal};

#[test]
fn av1_export_round_trips_mp4_mov_sdr_pq_and_hlg() {
    use filmcraft_export::{ExportSettings, Format, Multiplexer, Progress, export};
    use filmcraft_media::generators::GeneratorSource;
    use filmcraft_media::{DemoScene, FrameRequest, Generator, MediaSource};
    use filmcraft_project::{ItemKind, Label, MediaClip, MediaRef, Project, SequenceSettings, TrackKind};
    use filmcraft_time::{FrameRate, Tick, TimeRange};
    use std::sync::Arc;

    filmcraft_platform::register();
    if !filmcraft_platform::nvenc::av1_available() {
        eprintln!("SKIPPED: no NVENC AV1 encoder");
        return;
    }
    let ffmpeg = filmcraft_testkit::require_ffmpeg!();
    let ffprobe = filmcraft_testkit::require_ffprobe!();
    let rate = FrameRate::FPS_24;
    for (working, transfer, pixel_format) in [
        (filmcraft_color::WorkingSpace::Rec709, "bt709", "yuv420p"),
        (filmcraft_color::WorkingSpace::Rec2100Pq, "smpte2084", "yuv420p10le"),
        (filmcraft_color::WorkingSpace::Rec2100Hlg, "arib-std-b67", "yuv420p10le"),
    ] {
        let mut p = Project::new("AV1 regression");
        let g = GeneratorSource::new(Generator::Demo(DemoScene::OceanSunset), 320, 180, rate, rate.tick_of(8));
        let info = g.info().clone();
        let clip = p.add_item(
            &info.name.clone(),
            Label::Iris,
            ItemKind::Media(MediaClip {
                media: MediaRef::Generator(g.generator.clone()),
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
        let color = filmcraft_color::ColorPipeline { working, ..filmcraft_color::ColorPipeline::REC709 };
        let seq = p.new_sequence("s", SequenceSettings { width: 320, height: 180, frame_rate: rate, color, ..Default::default() }, 1, 1, None);
        let v = p.make_track_item(clip, TrackKind::Video, Tick::ZERO, TimeRange::new(Tick::ZERO, rate.tick_of(8)), rate).unwrap();
        p.sequence_mut(seq).unwrap().video_tracks[0].items.push(v);
        let mut sources = filmcraft_render::SourceMap::default();
        sources.0.insert(clip, Arc::new(g));
        let p = Arc::new(p);
        for (multiplexer, ext) in [(Multiplexer::Mp4, "mp4"), (Multiplexer::Mov, "mov")] {
            let path = filmcraft_testkit::temp_path(&filmcraft_testkit::fixtures_dir("nvenc-av1").join(format!("{}.{ext}", working.id())));
            let settings = ExportSettings {
                format: Format::Av1,
                multiplexer,
                path: path.to_string_lossy().into(),
                include_audio: false,
                bitrate_kbps: 8000,
                keyframe_distance: Some(4),
                ..Default::default()
            };
            assert_eq!(export(&p, seq, &settings, &sources, &Progress::default()).unwrap().frames, 8);
            let out = std::process::Command::new(&ffprobe)
                .args(["-v", "error", "-show_entries", "stream=codec_name,codec_tag_string,pix_fmt,color_transfer,nb_frames", "-of", "json"])
                .arg(&path)
                .output()
                .unwrap();
            assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
            let info = String::from_utf8_lossy(&out.stdout);
            for expected in ["av1", "av01", pixel_format, transfer, "\"nb_frames\": \"8\""] {
                assert!(info.contains(expected), "{expected}: {info}");
            }
            let out = std::process::Command::new(&ffmpeg).args(["-v", "error", "-xerror", "-i"]).arg(&path).args(["-f", "null", "-"]).output().unwrap();
            assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
            let bytes: Arc<[u8]> = std::fs::read(&path).unwrap().into();
            let src = filmcraft_codecs::open_bytes(&settings.path, bytes).unwrap();
            for i in [7, 0, 4, 2] {
                let frame = src.video_frame(FrameRequest::full(rate.tick_of(i))).unwrap();
                assert_eq!((frame.width, frame.height), (320, 180));
            }
        }
    }
}

#[test]
fn main10_encodes_hdr_signal_and_repeated_keyframes() {
    if !filmcraft_platform::nvenc::av1_available() {
        eprintln!("SKIPPED: no NVENC AV1 encoder");
        return;
    }
    let cfg = Config {
        width: 640,
        height: 360,
        fps: (25, 1),
        bitrate_kbps: 8000,
        max_bitrate_kbps: 12000,
        cbr: false,
        keyint: 4,
        profile: Profile::Av1Main10,
        level: None,
        sar: None,
        bframes: false,
    };
    let signal = Signal {
        primaries: 9,
        transfer: 16,
        matrix: 9,
        sei: vec![
            filmcraft_platform::nvenc::Sei { payload_type: 137, payload: filmcraft_isobmff::MasteringDisplay::bt2020(1000.0, 0.005).to_bytes() },
            filmcraft_platform::nvenc::Sei { payload_type: 144, payload: [1000u16.to_be_bytes(), 400u16.to_be_bytes()].concat() },
        ],
    };
    // A Main10 session must open on the test machine with AV1 10-bit support;
    // do not use av1_hdr_available() as a skip, which would hide this regression.
    let mut enc = Nvenc::with_signal(&cfg, &signal).expect("AV1 Main10 encoder");
    let config = enc.av1_config().expect("av1C");
    assert!(config.high_bitdepth);
    let mut dec = filmcraft_av1::Decoder::with_threads(1);
    assert!(!config.seq_tier_0);
    dec.decode(&config.config_obus).unwrap();
    let color = &dec.sequence_header().unwrap().color;
    assert_eq!((color.bit_depth, color.color_primaries, color.transfer_characteristics, color.matrix_coefficients), (10, 9, 16, 9));
    let uv = vec![512u16; 320 * 180];
    let mut packets = Vec::new();
    for i in 0..12 {
        let y = vec![128u16 + i as u16 * 40; 640 * 360];
        packets.extend(enc.encode_10(&y, &uv, &uv, i).unwrap());
    }
    packets.extend(enc.flush().unwrap());
    assert_eq!(packets.len(), 12);
    let ffprobe = filmcraft_testkit::require_ffprobe!();
    let path = filmcraft_testkit::temp_path(&filmcraft_testkit::fixtures_dir("nvenc-av1").join("main10.obu"));
    std::fs::write(&path, packets.iter().flat_map(|p| p.data.iter().copied()).collect::<Vec<_>>()).unwrap();
    let output = std::process::Command::new(ffprobe).args(["-v", "error", "-show_frames", "-of", "json"]).arg(&path).output().unwrap();
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    let info = String::from_utf8_lossy(&output.stdout);
    assert!(info.contains("Mastering display metadata"), "{info}");
    assert!(info.contains("Content light level metadata"), "{info}");
    assert!(info.contains("\"max_content\": 1000"), "{info}");
    assert!(info.contains("\"max_average\": 400"), "{info}");
    for i in [0, 4, 8] {
        assert!(packets[i].key, "keyframe {i}");
        assert!(filmcraft_av1::is_key_frame_unit(&packets[i].data), "repeat sequence header {i}");
    }
    let mut pictures = Vec::new();
    for packet in &packets {
        pictures.extend(dec.decode_pts(&packet.data, packet.pts).unwrap());
    }
    pictures.extend(dec.flush());
    assert_eq!(pictures.len(), 12);
    for (i, p) in pictures.iter().enumerate() {
        assert_eq!(p.bit_depth, 10);
        let mean = p.planes[0].iter().map(|&v| f64::from(v)).sum::<f64>() / p.planes[0].len() as f64;
        assert!((mean - (128.0 + i as f64 * 40.0)).abs() <= 2.0, "frame {i}: luma {mean}");
    }
}
