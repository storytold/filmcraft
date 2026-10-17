//! Export using AMD AMF hardware encoder through FilmCraft's real export pipeline.
#![cfg(target_os = "windows")]

use std::sync::Arc;

use filmcraft_export::{BitrateMode, ExportSettings, Format, HardwareEncoding, Progress, export, hw_encode_stats};
use filmcraft_media::generators::GeneratorSource;
use filmcraft_media::{DemoScene, FrameRequest, Generator, MediaSource};
use filmcraft_platform::amf::hevc_available;
use filmcraft_project::{ItemKind, Label, MediaClip, MediaRef, Project, SequenceSettings, TrackKind};
use filmcraft_render::SourceMap;
use filmcraft_time::{FrameRate, Tick, TimeRange};

static SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn project(w: u32, h: u32, frames: i64) -> (Arc<Project>, filmcraft_project::ItemId, SourceMap) {
    let rate = FrameRate::FPS_24;
    let mut p = Project::new("amd_proj");
    let g = GeneratorSource::new(Generator::Demo(DemoScene::OceanSunset), w, h, rate, rate.tick_of(frames));
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
    let seq = p.new_sequence("s", SequenceSettings { width: w, height: h, frame_rate: rate, ..Default::default() }, 1, 1, None);
    let v = p.make_track_item(clip, TrackKind::Video, Tick::ZERO, TimeRange::new(Tick::ZERO, rate.tick_of(frames)), rate).unwrap();
    p.sequence_mut(seq).unwrap().video_tracks[0].items.push(v);
    let mut m = SourceMap::default();
    m.0.insert(clip, Arc::new(g));
    (Arc::new(p), seq, m)
}

fn tmp(name: &str) -> String {
    let d = std::env::temp_dir().join(format!("fc-amf-test-{}", std::process::id()));
    let _ = std::fs::create_dir_all(&d);
    d.join(name).to_string_lossy().to_string()
}

fn decode(path: &str) -> Vec<Arc<filmcraft_frame::VideoFrame>> {
    let bytes: Arc<[u8]> = std::fs::read(path).unwrap().into();
    let src = filmcraft_codecs::open_bytes(path, bytes).unwrap();
    let rate = src.info().frame_rate();
    let n = rate.frame_at(src.info().duration);
    (0..n).map(|i| src.video_frame(FrameRequest::full(rate.tick_of(i))).unwrap()).collect()
}

#[test]
fn test_amf_hevc_export_and_decode() {
    let _lock = SERIAL.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    if !hevc_available() {
        eprintln!("No AMD AMF HEVC encoder on this system, skipping test.");
        return;
    }

    filmcraft_export::register_encoder(filmcraft_platform::amf::export::factory);

    let (p, seq, m) = project(320, 240, 24);
    let out_path = tmp("amf_test_hevc.mp4");
    let settings = ExportSettings {
        format: Format::Hevc,
        path: out_path.clone(),
        include_audio: false,
        bitrate_kbps: 1000,
        bitrate_mode: BitrateMode::Vbr1Pass,
        hardware_encoding: HardwareEncoding::Auto,
        ..Default::default()
    };

    let before = hw_encode_stats();
    let res = export(&p, seq, &settings, &m, &Progress::default());
    assert!(res.is_ok(), "AMF HEVC export failed: {res:?}");
    let after = hw_encode_stats();

    assert!(after.sessions > before.sessions, "AMF session was not recorded");
    assert!(after.frames >= before.frames + 24, "AMF frames were not recorded");

    let frames = decode(&out_path);
    assert_eq!(frames.len(), 24, "Decoded frame count mismatch");
    assert_eq!(frames[0].width, 320);
    assert_eq!(frames[0].height, 240);

    let _ = std::fs::remove_file(&out_path);
}

#[test]
fn test_amf_h264_export_and_decode() {
    let _lock = SERIAL.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let probe_cfg = filmcraft_platform::amf::Config {
        width: 320,
        height: 240,
        fps: (24, 1),
        bitrate_kbps: 1000,
        max_bitrate_kbps: 1500,
        cbr: false,
        keyint: 24,
        profile: filmcraft_platform::amf::Profile::H264High,
    };
    if filmcraft_platform::amf::Session::open(&probe_cfg).is_err() {
        eprintln!("No AMD AMF H.264 encoder on this system, skipping test.");
        return;
    }

    filmcraft_export::register_encoder(filmcraft_platform::amf::export::factory);

    let (p, seq, m) = project(320, 240, 24);
    let out_path = tmp("amf_test_h264.mp4");
    let settings = ExportSettings {
        format: Format::H264,
        path: out_path.clone(),
        include_audio: false,
        bitrate_kbps: 1000,
        bitrate_mode: BitrateMode::Vbr1Pass,
        hardware_encoding: HardwareEncoding::Auto,
        ..Default::default()
    };

    let before = hw_encode_stats();
    let res = export(&p, seq, &settings, &m, &Progress::default());
    assert!(res.is_ok(), "AMF H.264 export failed: {res:?}");
    let after = hw_encode_stats();

    assert!(after.sessions > before.sessions, "AMF session was not recorded");
    assert!(after.frames >= before.frames + 24, "AMF frames were not recorded");

    let frames = decode(&out_path);
    assert_eq!(frames.len(), 24, "Decoded frame count mismatch");
    assert_eq!(frames[0].width, 320);
    assert_eq!(frames[0].height, 240);

    let _ = std::fs::remove_file(&out_path);
}
