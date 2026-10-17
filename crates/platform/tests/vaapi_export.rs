//! Export with Hardware encoding through VA-API (Linux AMD / Intel) against the software encoder,
//! through the real export pipeline: files both decode alike, the counters show which encoder ran,
//! 1080p is cropped right, a long GOP wraps frame_num and POC cleanly, and what VA-API declines
//! (Off, two-pass, CRF, MXF) goes to the software encoder. Its own test binary: the encoder
//! registry is process-wide. Skips without a VA-API H.264 encoder.
#![cfg(target_os = "linux")]

use std::sync::Arc;

use filmcraft_export::{BitrateMode, ExportSettings, Format, HardwareEncoding, Progress, export, hw_encode_stats};
use filmcraft_media::generators::GeneratorSource;
use filmcraft_media::{DemoScene, FrameRequest, Generator, MediaSource};
use filmcraft_project::{ItemKind, Label, MediaClip, MediaRef, Project, SequenceSettings, TrackKind};
use filmcraft_render::SourceMap;
use filmcraft_time::{FrameRate, Tick, TimeRange};

/// The encoder counters are process-wide: one test at a time.
static SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn project(w: u32, h: u32, frames: i64) -> (Arc<Project>, filmcraft_project::ItemId, SourceMap) {
    let rate = FrameRate::FPS_24;
    let mut p = Project::new("x");
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
    let d = std::env::temp_dir().join(format!("fc-vaapi-enc-{}", std::process::id()));
    std::fs::create_dir_all(&d).unwrap();
    d.join(name).to_string_lossy().to_string()
}

fn decode(path: &str) -> Vec<Arc<filmcraft_frame::VideoFrame>> {
    let bytes: Arc<[u8]> = std::fs::read(path).unwrap().into();
    let src = filmcraft_codecs::open_bytes(path, bytes).unwrap();
    let rate = src.info().frame_rate();
    let n = rate.frame_at(src.info().duration);
    (0..n).map(|i| src.video_frame(FrameRequest::full(rate.tick_of(i))).unwrap()).collect()
}

fn luma_psnr(a: &filmcraft_frame::VideoFrame, b: &filmcraft_frame::VideoFrame) -> f64 {
    let (x, y) = (a.luma8(), b.luma8());
    let mse = x.iter().zip(&y).map(|(p, q)| (f64::from(*p) - f64::from(*q)).powi(2)).sum::<f64>() / x.len() as f64;
    if mse == 0.0 { 99.0 } else { 10.0 * (255.0f64 * 255.0 / mse).log10() }
}

fn settings(path: String, hw: HardwareEncoding) -> ExportSettings {
    ExportSettings {
        format: Format::H264,
        path,
        include_audio: false,
        bitrate_kbps: 4000,
        bitrate_mode: BitrateMode::Vbr1Pass,
        hardware_encoding: hw,
        ..Default::default()
    }
}

/// Registers the platform encoders; `false` (skip) without a VA-API H.264 encoder.
fn ready() -> bool {
    filmcraft_platform::register();
    if filmcraft_platform::vaapi::h264enc::available() {
        return true;
    }
    eprintln!("SKIPPED: no VA-API H.264 encoder");
    false
}

/// Export `s` and report (hardware sessions, hardware frames) it used.
fn run(p: &Arc<Project>, seq: filmcraft_project::ItemId, m: &SourceMap, s: &ExportSettings) -> (u64, u64) {
    let before = hw_encode_stats();
    export(p, seq, s, m, &Progress::default()).unwrap();
    let after = hw_encode_stats();
    (after.sessions - before.sessions, after.frames - before.frames)
}

fn ffmpeg_decodes_cleanly(path: &str) {
    if let Some(ff) = filmcraft_testkit::ffmpeg() {
        let out = std::process::Command::new(ff).args(["-v", "error", "-i", path, "-f", "null", "-"]).output().unwrap();
        assert!(out.stderr.is_empty(), "ffmpeg: {}", String::from_utf8_lossy(&out.stderr));
    }
}

#[test]
fn hardware_export_matches_the_software_export() {
    let _one = SERIAL.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    if !ready() {
        return;
    }
    let (p, seq, m) = project(1280, 720, 72);
    let (soft_path, hw_path) = (tmp("soft.mp4"), tmp("hw.mp4"));
    assert_eq!(run(&p, seq, &m, &settings(soft_path.clone(), HardwareEncoding::Off)), (0, 0), "Off never touches the hardware");
    assert_eq!(run(&p, seq, &m, &settings(hw_path.clone(), HardwareEncoding::Auto)), (1, 72), "one VA-API session, every picture");
    let (soft, hard) = (decode(&soft_path), decode(&hw_path));
    assert_eq!((soft.len(), hard.len()), (72, 72));
    let worst = soft.iter().zip(&hard).map(|(a, b)| luma_psnr(a, b)).fold(99.0, f64::min);
    assert!(worst > 30.0, "hardware export differs from the software one: {worst:.1} dB");
    ffmpeg_decodes_cleanly(&hw_path);
}

#[test]
fn hd_1080_is_cropped_to_1080_rows() {
    let _one = SERIAL.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    if !ready() {
        return;
    }
    let (p, seq, m) = project(1920, 1080, 24);
    let (soft_path, hw_path) = (tmp("soft1080.mp4"), tmp("hw1080.mp4"));
    run(&p, seq, &m, &settings(soft_path.clone(), HardwareEncoding::Off));
    assert_eq!(run(&p, seq, &m, &settings(hw_path.clone(), HardwareEncoding::Auto)), (1, 24));
    let (soft, hard) = (decode(&soft_path), decode(&hw_path));
    for f in &hard {
        assert_eq!((f.width, f.height), (1920, 1080), "cropped from the 1088-row coded picture");
    }
    let worst = soft.iter().zip(&hard).map(|(a, b)| luma_psnr(a, b)).fold(99.0, f64::min);
    assert!(worst > 30.0, "1080p: {worst:.1} dB (a wrong crop shifts or pads the bottom rows)");
    ffmpeg_decodes_cleanly(&hw_path);
}

#[test]
fn long_gop_wraps_frame_num_and_poc() {
    let _one = SERIAL.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    if !ready() {
        return;
    }
    // 320 frames, one keyframe at 0 and one at 300: frame_num and the POC LSB (8 bits each) wrap
    let (p, seq, m) = project(640, 360, 320);
    let (soft_path, hw_path) = (tmp("soft_gop.mp4"), tmp("hw_gop.mp4"));
    let long = |path: String, hw| ExportSettings { keyframe_distance: Some(300), ..settings(path, hw) };
    run(&p, seq, &m, &long(soft_path.clone(), HardwareEncoding::Off));
    assert_eq!(run(&p, seq, &m, &long(hw_path.clone(), HardwareEncoding::Auto)), (1, 320));
    let (soft, hard) = (decode(&soft_path), decode(&hw_path));
    assert_eq!(hard.len(), 320, "every frame decodes");
    let worst = soft.iter().zip(&hard).map(|(a, b)| luma_psnr(a, b)).fold(99.0, f64::min);
    assert!(worst > 28.0, "long GOP: worst frame {worst:.1} dB");
    ffmpeg_decodes_cleanly(&hw_path);
}

#[test]
fn what_vaapi_does_not_take_goes_to_the_software_encoder() {
    let _one = SERIAL.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    if !ready() {
        return;
    }
    let (p, seq, m) = project(640, 360, 24);
    let auto = |name: &str| settings(tmp(name), HardwareEncoding::Auto);
    assert_eq!(run(&p, seq, &m, &ExportSettings { bitrate_mode: BitrateMode::Vbr2Pass, ..auto("two_pass.mp4") }), (0, 0));
    assert_eq!(run(&p, seq, &m, &ExportSettings { bitrate_mode: BitrateMode::Crf, ..auto("crf.mp4") }), (0, 0));
    assert_eq!(
        run(&p, seq, &m, &ExportSettings { format: Format::MxfOp1a, mxf_video_codec: filmcraft_export::MxfVideoCodec::H264, ..auto("mxf.mxf") }),
        (0, 0)
    );
    assert_eq!(run(&p, seq, &m, &settings(tmp("off.mp4"), HardwareEncoding::Off)), (0, 0));
    // the software export of a declined request is fine
    assert_eq!(decode(&tmp("crf.mp4")).len(), 24);
}

#[test]
fn a_short_picture_is_an_error_not_a_panic() {
    let _one = SERIAL.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    if !ready() {
        return;
    }
    let s = settings(tmp("short.mp4"), HardwareEncoding::Auto);
    let mut enc = match filmcraft_platform::vaapi::h264enc::factory(Format::H264, 640, 360, FrameRate::FPS_24, &s) {
        Some(Ok(e)) => e,
        Some(Err(e)) => panic!("factory error: {e}"),
        None => panic!("VA-API declined a plain 640x360 H.264 export"),
    };
    let short = vec![0u8; 640 * 4 * 10];
    let frame = filmcraft_export::EncoderFrame { width: 640, height: 360, rgba: &short, hdr: None, index: 0 };
    assert!(enc.encode(&frame).is_err(), "a picture with too few bytes is an export error");
    // the encoder still works afterwards
    let full = vec![128u8; 640 * 360 * 4];
    let frame = filmcraft_export::EncoderFrame { width: 640, height: 360, rgba: &full, hdr: None, index: 0 };
    assert_eq!(enc.encode(&frame).unwrap().len(), 1);
}

#[test]
fn every_profile_and_rate_control_encodes_with_keyframes_where_asked() {
    let _one = SERIAL.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    if !ready() {
        return;
    }
    use filmcraft_export::H264Profile;
    let (p, seq, m) = project(640, 360, 48);
    let soft_path = tmp("matrix_soft.mp4");
    run(&p, seq, &m, &settings(soft_path.clone(), HardwareEncoding::Off));
    let soft = decode(&soft_path);
    for (profile, name) in [(H264Profile::Baseline, "Constrained Baseline"), (H264Profile::Main, "Main"), (H264Profile::High, "High")] {
        for mode in [BitrateMode::Cbr, BitrateMode::Vbr1Pass] {
            let path = tmp(&format!("matrix_{profile:?}_{mode:?}.mp4"));
            let s = ExportSettings { h264_profile: profile, bitrate_mode: mode, keyframe_distance: Some(12), ..settings(path.clone(), HardwareEncoding::Auto) };
            assert_eq!(run(&p, seq, &m, &s), (1, 48), "{profile:?} {mode:?}: every picture through VA-API");
            let bytes = std::fs::read(&path).unwrap();
            let file = filmcraft_isobmff::open(bytes.as_slice()).unwrap();
            assert_eq!(file.tracks[0].sync_samples(), &[0, 12, 24, 36], "{profile:?} {mode:?}: an IDR every keyframe distance");
            let hard = decode(&path);
            assert_eq!(hard.len(), 48, "{profile:?} {mode:?}");
            let worst = soft.iter().zip(&hard).map(|(a, b)| luma_psnr(a, b)).fold(99.0, f64::min);
            assert!(worst > 30.0, "{profile:?} {mode:?}: {worst:.1} dB from the software export");
            ffmpeg_decodes_cleanly(&path);
            if let Some(probe) = filmcraft_testkit::ffprobe() {
                let out = std::process::Command::new(probe)
                    .args(["-v", "error", "-select_streams", "v:0", "-show_entries", "stream=profile", "-of", "default=nw=1:nk=1", &path])
                    .output()
                    .unwrap();
                assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), name, "{profile:?} {mode:?}");
            }
        }
    }
}
