//! Export using AMD AMF hardware encoder through FilmCraft's real export pipeline.
#![cfg(target_os = "windows")]

use std::sync::Arc;

use filmcraft_export::{BitrateMode, ExportSettings, Format, HardwareEncoding, Progress, export, hw_encode_stats};
use filmcraft_media::generators::GeneratorSource;
use filmcraft_media::{DemoScene, FrameRequest, Generator, MediaSource};
use filmcraft_platform::amf::{av1_available, hevc_available};
use filmcraft_project::{ItemKind, Label, MediaClip, MediaRef, Project, SequenceSettings, TrackKind};
use filmcraft_render::SourceMap;
use filmcraft_time::{FrameRate, Tick, TimeRange};

static SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn project(w: u32, h: u32, frames: i64) -> (Arc<Project>, filmcraft_project::ItemId, SourceMap) {
    project_in(w, h, frames, filmcraft_color::ColorPipeline::REC709)
}

/// A sequence in the given colour pipeline (an HDR working space makes the export HDR).
fn project_in(w: u32, h: u32, frames: i64, color: filmcraft_color::ColorPipeline) -> (Arc<Project>, filmcraft_project::ItemId, SourceMap) {
    let rate = FrameRate::FPS_24;
    let mut p = Project::new("amd_proj");
    let g = GeneratorSource::new(Generator::Demo(DemoScene::OceanSunset), w, h, rate, rate.tick_of(frames));
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
    let seq = p.new_sequence("s", SequenceSettings { width: w, height: h, frame_rate: rate, color, ..Default::default() }, 1, 1, None);
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

/// The payload of the first `fourcc` box in a file (enough for the sample-entry boxes asked about here).
fn box_payload<'a>(file: &'a [u8], fourcc: &[u8; 4]) -> Option<&'a [u8]> {
    let at = file.windows(4).position(|w| w == fourcc)?;
    let size = u32::from_be_bytes(file.get(at.checked_sub(4)?..at)?.try_into().ok()?) as usize;
    file.get(at + 4..(at - 4).checked_add(size)?)
}

/// #880: an HDR H.265 export from AMF says it is BT.2020 with PQ / HLG in its SPS VUI, not only in
/// the `colr` box (AMF writes BT.709 when its colour properties are not set), and a PQ export carries
/// the HDR10 mastering display in the stream as well as in the `mdcv` box.
#[test]
fn test_amf_hevc_hdr_export_signals_its_colour_in_the_stream() {
    let _lock = SERIAL.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    if !filmcraft_platform::amf::hevc_hdr_available() {
        eprintln!("No AMD AMF HEVC Main 10 encoder on this system, skipping test.");
        return;
    }
    // the HDR probes as well as the encoders: without them an HDR sequence exports tone-mapped to SDR
    filmcraft_platform::register();
    use filmcraft_color::{ColorPipeline, WorkingSpace};
    for (name, working, signal) in
        [("pq", WorkingSpace::Rec2100Pq, filmcraft_export::ColorSignal::PQ), ("hlg", WorkingSpace::Rec2100Hlg, filmcraft_export::ColorSignal::HLG)]
    {
        // the export's colour comes from the sequence's working space
        let (p, seq, m) = project_in(640, 360, 12, ColorPipeline { working, ..ColorPipeline::REC709 });
        let out_path = tmp(&format!("amf_test_hevc_{name}.mp4"));
        let settings = ExportSettings {
            format: Format::Hevc,
            path: out_path.clone(),
            include_audio: false,
            bitrate_kbps: 2000,
            bitrate_mode: BitrateMode::Vbr1Pass,
            hardware_encoding: HardwareEncoding::Auto,
            ..Default::default()
        };
        let report = export(&p, seq, &settings, &m, &Progress::default()).expect("AMF HEVC HDR export failed");
        assert_eq!(report.encoder.as_deref(), Some("AMD AMF"), "{name}");
        let file = std::fs::read(&out_path).unwrap();
        let hvcc = filmcraft_isobmff::HevcConfig::parse(box_payload(&file, b"hvcC").expect("an hvcC")).unwrap();
        let sps = hvcc.arrays.iter().flat_map(|a| &a.nalus).find(|n| n.first().is_some_and(|h| (h >> 1) & 0x3f == 33)).expect("an SPS");
        let sps = craft_video::hevc::decode::params::Sps::parse(&filmcraft_bitstream::unescape_rbsp(&sps[2..])).unwrap();
        let vui = sps.vui.expect("the SPS has a VUI");
        assert_eq!(
            (vui.colour_primaries, vui.transfer_characteristics, vui.matrix_coefficients),
            (signal.primaries, signal.transfer, signal.matrix),
            "{name}: the VUI's colour description"
        );
        if let Some((md, _)) = signal.static_metadata() {
            // the same primaries and white point in the `mdcv` box and in the stream's mastering-display
            // SEI (the first 16 of its 24 bytes: the luminances that follow hold `00 00` runs, which
            // the stream escapes with emulation-prevention bytes)
            let bytes = md.to_bytes();
            let bytes = &bytes[..16];
            let n = file.windows(bytes.len()).filter(|w| *w == bytes).count();
            assert!(n >= 2, "{name}: the mastering display is in the stream as well as in mdcv ({n} copies)");
        }
        assert_eq!(decode(&out_path).len(), 12, "{name}: decodes");
        let _ = std::fs::remove_file(&out_path);
    }
}

#[test]
fn test_amf_hevc_export_and_decode() {
    let _lock = SERIAL.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    if !hevc_available() {
        eprintln!("No AMD AMF HEVC encoder on this system, skipping test.");
        return;
    }

    filmcraft_export::register_encoder(filmcraft_platform::amf::export::factory);

    // Match the capability probe: some AMD runtimes reject smaller HEVC pictures.
    let (p, seq, m) = project(640, 360, 24);
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
    let report = export(&p, seq, &settings, &m, &Progress::default()).expect("AMF HEVC export failed");
    let after = hw_encode_stats();
    assert_eq!(report.encoder.as_deref(), Some("AMD AMF"), "the report names the encoder that ran");
    assert!(report.warnings.is_empty(), "no software-fallback warning: {:?}", report.warnings);

    assert!(after.sessions > before.sessions, "AMF session was not recorded");
    assert!(after.frames >= before.frames + 24, "AMF frames were not recorded");

    let frames = decode(&out_path);
    assert_eq!(frames.len(), 24, "Decoded frame count mismatch");
    assert_eq!(frames[0].width, 640);
    assert_eq!(frames[0].height, 360);

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
        signal: Default::default(),
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
    let report = export(&p, seq, &settings, &m, &Progress::default()).expect("AMF H.264 export failed");
    let after = hw_encode_stats();
    assert_eq!(report.encoder.as_deref(), Some("AMD AMF"), "the report names the encoder that ran");
    assert!(report.warnings.is_empty(), "no software-fallback warning: {:?}", report.warnings);

    assert!(after.sessions > before.sessions, "AMF session was not recorded");
    assert!(after.frames >= before.frames + 24, "AMF frames were not recorded");

    let frames = decode(&out_path);
    assert_eq!(frames.len(), 24, "Decoded frame count mismatch");
    assert_eq!(frames[0].width, 320);
    assert_eq!(frames[0].height, 240);

    let _ = std::fs::remove_file(&out_path);
}

#[test]
fn test_amf_av1_export_and_decode() {
    let _lock = SERIAL.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    if !av1_available() {
        eprintln!("No AMD AMF AV1 encoder on this system, skipping test.");
        return;
    }

    filmcraft_export::register_encoder(filmcraft_platform::amf::export::factory);

    // Cross two GOP boundaries so delayed output and later keyframes are exercised.
    let (p, seq, m) = project(640, 360, 72);
    let out_path = tmp("amf_test_av1.mp4");
    let settings = ExportSettings {
        format: Format::Av1,
        path: out_path.clone(),
        include_audio: false,
        bitrate_kbps: 2000,
        bitrate_mode: BitrateMode::Vbr1Pass,
        keyframe_distance: Some(24),
        hardware_encoding: HardwareEncoding::Auto,
        ..Default::default()
    };

    let before = hw_encode_stats();
    let report = export(&p, seq, &settings, &m, &Progress::default()).expect("AMF AV1 export failed");
    let after = hw_encode_stats();
    assert_eq!(report.frames, 72);
    assert_eq!(report.encoder.as_deref(), Some("AMD AMF"), "the report names the encoder that ran");
    assert!(report.warnings.is_empty(), "no software-fallback warning: {:?}", report.warnings);
    assert_eq!(after.sessions - before.sessions, 1, "AV1 must use one AMF session");
    assert_eq!(after.frames - before.frames, 72, "Every AV1 picture must go through AMF");

    let frames = decode(&out_path);
    assert_eq!(frames.len(), 72, "Decoded AV1 frame count mismatch");
    assert!(frames.iter().all(|frame| frame.width == 640 && frame.height == 360));

    let bytes: Arc<[u8]> = std::fs::read(&out_path).unwrap().into();
    let src = filmcraft_codecs::open_bytes(&out_path, bytes).unwrap();
    let rate = src.info().frame_rate();
    // A fresh source must also seek into later GOPs, rather than only decode from frame zero.
    for index in [49, 25, 0, 71] {
        let frame = src.video_frame(FrameRequest::full(rate.tick_of(index))).unwrap();
        assert_eq!((frame.width, frame.height), (640, 360));
    }

    std::fs::remove_file(&out_path).unwrap();
}
