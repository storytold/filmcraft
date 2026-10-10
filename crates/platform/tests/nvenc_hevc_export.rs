//! Export as H.265 (HEVC Main, NVENC) through the real export pipeline: MP4 and QuickTime files that
//! ffprobe (an external oracle only) describes as `hvc1` Main 8-bit 4:2:0 BT.709, that ffmpeg decodes
//! without errors and that our own decoder decodes to the pictures of the software H.264 export; the
//! counters; and what NVENC declines (an error that says why, counted, never a crash). Its own test
//! binary: the encoder registry and the counters are process-wide. Skips without an NVIDIA GPU with
//! an HEVC encoder.
#![cfg(target_os = "windows")]

use std::path::Path;
use std::process::Command;
use std::sync::Arc;

use filmcraft_export::{
    BitrateMode, ColorSignal, ExportSettings, FieldOrder, Format, H264Pass, HardwareEncoding, Multiplexer, Progress, export, hw_encode_stats,
};
use filmcraft_media::generators::GeneratorSource;
use filmcraft_media::{DemoScene, FrameRequest, Generator, MediaSource};
use filmcraft_platform::nvenc::export::factory;
use filmcraft_platform::nvenc::hevc_available;
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
    let d = std::env::temp_dir().join(format!("fc-nvenc-hevc-{}", std::process::id()));
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
    let (x, y) = (a.luma8().unwrap(), b.luma8().unwrap());
    let mse = x.iter().zip(&y).map(|(p, q)| (f64::from(*p) - f64::from(*q)).powi(2)).sum::<f64>() / x.len() as f64;
    if mse == 0.0 { 99.0 } else { 10.0 * (255.0f64 * 255.0 / mse).log10() }
}

fn hevc(path: String) -> ExportSettings {
    ExportSettings { format: Format::Hevc, path, include_audio: false, bitrate_kbps: 4000, bitrate_mode: BitrateMode::Vbr1Pass, ..Default::default() }
}

/// `key=value` lines of `ffprobe -show_entries` for the first video stream.
fn probe(ffprobe: &Path, path: &str) -> std::collections::HashMap<String, String> {
    let entries = "stream=codec_name,profile,codec_tag_string,pix_fmt,color_space,color_transfer,color_primaries,color_range,width,height,nb_frames,r_frame_rate,start_time,level";
    let out = Command::new(ffprobe).args(["-v", "error", "-select_streams", "v:0", "-show_entries", entries, "-of", "default=nw=1", path]).output().unwrap();
    assert!(out.status.success(), "ffprobe: {}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).lines().filter_map(|l| l.split_once('=')).map(|(k, v)| (k.to_string(), v.to_string())).collect()
}

/// Presentation times (in frames at 24 fps) of the packets ffprobe flags as keyframes.
fn keyframes(ffprobe: &Path, path: &str) -> Vec<i64> {
    let out = Command::new(ffprobe)
        .args(["-v", "error", "-select_streams", "v:0", "-show_entries", "packet=pts_time,flags", "-of", "csv=p=0", path])
        .output()
        .unwrap();
    let mut keys: Vec<i64> = String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|l| l.split_once(','))
        .filter(|(_, flags)| flags.starts_with('K'))
        .filter_map(|(t, _)| t.parse::<f64>().ok())
        .map(|t| (t * 24.0).round() as i64)
        .collect();
    keys.sort_unstable();
    keys
}

/// ffmpeg decodes the whole file with `-xerror` and says nothing.
fn assert_ffmpeg_decodes(ffmpeg: &Path, path: &str) {
    let out = Command::new(ffmpeg).args(["-v", "error", "-xerror", "-i", path, "-f", "null", "-"]).output().unwrap();
    assert!(out.status.success() && out.stderr.is_empty(), "ffmpeg: {}", String::from_utf8_lossy(&out.stderr));
}

fn delta(before: filmcraft_export::HwEncodeStats) -> (u64, u64, u64) {
    let after = hw_encode_stats();
    (after.frames - before.frames, after.sessions - before.sessions, after.declined - before.declined)
}

#[test]
fn hevc_exports_play_everywhere() {
    let _one = SERIAL.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    filmcraft_platform::register();
    assert_eq!(filmcraft_export::available(Format::Hevc), hevc_available(), "the format is available iff NVENC can encode HEVC");
    if !hevc_available() {
        eprintln!("SKIPPED: no NVENC HEVC here");
        return;
    }
    let (p, seq, m) = project(1280, 720, 72);

    // the software H.264 export of the same frames is the reference for our decoder's pictures
    let soft_path = tmp("soft.mp4");
    let soft = ExportSettings { format: Format::H264, hardware_encoding: HardwareEncoding::Off, ..hevc(soft_path.clone()) };
    let before = hw_encode_stats();
    export(&p, seq, &soft, &m, &Progress::default()).unwrap();
    assert_eq!(delta(before), (0, 0, 0), "H.264 with hardware encoding Off never touches NVENC");
    let reference = decode(&soft_path);

    let ffprobe = filmcraft_testkit::ffprobe_or_skip("nvenc hevc export");
    let ffmpeg = filmcraft_testkit::ffmpeg_or_skip("nvenc hevc export");
    for (name, multiplexer, toggle) in [
        ("hevc.mp4", Multiplexer::Mp4, HardwareEncoding::Auto),
        ("hevc.mov", Multiplexer::Mov, HardwareEncoding::Auto),
        // HEVC has no software encoder: the toggle does not apply
        ("hevc-off.mp4", Multiplexer::Mp4, HardwareEncoding::Off),
    ] {
        let path = tmp(name);
        let s = ExportSettings { multiplexer, hardware_encoding: toggle, ..hevc(path.clone()) };
        let before = hw_encode_stats();
        let report = export(&p, seq, &s, &m, &Progress::default()).unwrap_or_else(|e| panic!("{name}: {e}"));
        assert_eq!(report.frames, 72, "{name}");
        assert_eq!(delta(before), (72, 1, 0), "{name}: every picture through one NVENC session, nothing declined");

        // our own decoder: 72 pictures close to the software export's
        let pictures = decode(&path);
        assert_eq!(pictures.len(), 72, "{name}");
        let worst = pictures.iter().zip(&reference).map(|(a, b)| luma_psnr(a, b)).fold(99.0, f64::min);
        eprintln!(
            "{name}: {} bytes (software H.264 {}), worst luma PSNR against the software export {worst:.1} dB",
            std::fs::metadata(&path).unwrap().len(),
            std::fs::metadata(&soft_path).unwrap().len()
        );
        assert!(worst > 48.0, "{name}: {worst:.1} dB");

        let (Some(ffprobe), Some(ffmpeg)) = (&ffprobe, &ffmpeg) else { continue };
        let v = probe(ffprobe, &path);
        let get = |k: &str| v.get(k).map(String::as_str).unwrap_or("?");
        assert_eq!((get("codec_name"), get("profile"), get("codec_tag_string"), get("pix_fmt")), ("hevc", "Main", "hvc1", "yuv420p"), "{name}: {v:?}");
        assert_eq!((get("color_space"), get("color_transfer"), get("color_primaries"), get("color_range")), ("bt709", "bt709", "bt709", "tv"), "{name}: {v:?}");
        assert_eq!((get("width"), get("height"), get("nb_frames"), get("r_frame_rate")), ("1280", "720", "72", "24/1"), "{name}: {v:?}");
        assert!(get("start_time").parse::<f64>().is_ok_and(|t| t.abs() < 1e-3), "{name}: starts at {}", get("start_time"));
        assert_eq!(keyframes(ffprobe, &path), vec![0, 48], "{name}: keyframes every 2 s (the default distance)");
        assert_ffmpeg_decodes(ffmpeg, &path);
        eprintln!("{name}: ffprobe {v:?}, ffmpeg -xerror decodes it cleanly");
    }
}

#[test]
fn hevc_export_with_audio() {
    let _one = SERIAL.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    filmcraft_platform::register();
    if !hevc_available() {
        eprintln!("SKIPPED: no NVENC HEVC here");
        return;
    }
    let (p, seq, m) = project(640, 360, 48);
    let path = tmp("hevc-audio.mp4");
    let s = ExportSettings { include_audio: true, keyframe_distance: Some(24), ..hevc(path.clone()) };
    export(&p, seq, &s, &m, &Progress::default()).expect("export");
    let bytes = std::fs::read(&path).unwrap();
    let file = filmcraft_isobmff::open(bytes.as_slice()).unwrap();
    assert!(file.track_of_kind(filmcraft_isobmff::TrackKind::Video).is_some());
    let at = file.track_of_kind(filmcraft_isobmff::TrackKind::Audio).expect("an audio track");
    assert!(matches!(&file.tracks[at].entries[0].codec, filmcraft_isobmff::CodecConfig::Aac(_)));
    if let (Some(ffprobe), Some(ffmpeg)) = (filmcraft_testkit::ffprobe_or_skip("nvenc hevc export"), filmcraft_testkit::ffmpeg_or_skip("nvenc hevc export")) {
        let out = Command::new(&ffprobe)
            .args(["-v", "error", "-show_entries", "stream=codec_name,codec_type,codec_tag_string", "-of", "csv=p=0", &path])
            .output()
            .unwrap();
        let streams = String::from_utf8_lossy(&out.stdout).to_string();
        assert!(streams.contains("hevc,video,hvc1") && streams.contains("aac,audio,mp4a"), "{streams}");
        assert_ffmpeg_decodes(&ffmpeg, &path);
        assert_eq!(keyframes(&ffprobe, &path), vec![0, 24]);
    }
}

#[test]
fn sizes_that_are_not_multiples_of_the_coding_block_are_cropped_back() {
    let _one = SERIAL.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    filmcraft_platform::register();
    if !hevc_available() {
        eprintln!("SKIPPED: no NVENC HEVC here");
        return;
    }
    // 1080 is not a multiple of the 32-pixel coding tree block (the SPS carries a conformance window);
    // 642x362 is even but a multiple of nothing
    for (w, h) in [(1920u32, 1080u32), (642, 362)] {
        let (p, seq, m) = project(w, h, 12);
        let path = tmp(&format!("hevc-{w}x{h}.mp4"));
        let before = hw_encode_stats();
        export(&p, seq, &hevc(path.clone()), &m, &Progress::default()).unwrap_or_else(|e| panic!("{w}x{h}: {e}"));
        assert_eq!(delta(before), (12, 1, 0), "{w}x{h}");
        let pictures = decode(&path);
        assert_eq!(pictures.len(), 12);
        assert!(pictures.iter().all(|f| (f.width, f.height) == (w, h)), "{w}x{h}: decoded {}x{}", pictures[0].width, pictures[0].height);
        if let (Some(ffprobe), Some(ffmpeg)) = (filmcraft_testkit::ffprobe_or_skip("nvenc hevc export"), filmcraft_testkit::ffmpeg_or_skip("nvenc hevc export"))
        {
            let v = probe(&ffprobe, &path);
            assert_eq!((v["width"].as_str(), v["height"].as_str()), (w.to_string().as_str(), h.to_string().as_str()), "{v:?}");
            assert_ffmpeg_decodes(&ffmpeg, &path);
        }
    }
}

#[test]
fn what_nvenc_cannot_do_is_an_error_that_says_why() {
    let _one = SERIAL.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    filmcraft_platform::register();
    let rate = FrameRate::FPS_24;
    let base = hevc(tmp("declined.mp4"));
    // (what, settings, size): each declined by the configuration alone, so even a machine without
    // NVENC counts and explains it
    let cases: Vec<(&str, ExportSettings, (u32, u32))> = vec![
        // HDR is Main 10 since the HDR round (see nvenc_hevc_hdr_export.rs); what stays declined is a colour description
        // this backend does not write
        ("an unwritable colour signal", ExportSettings { signal: ColorSignal { primaries: 9, transfer: 1, matrix: 1 }, ..base.clone() }, (1280, 720)),
        ("HDR with BT.709 primaries", ExportSettings { signal: ColorSignal { primaries: 1, transfer: 16, matrix: 9 }, ..base.clone() }, (1280, 720)),
        ("an analysis pass", ExportSettings { h264_pass: H264Pass::First, ..base.clone() }, (1280, 720)),
        ("two-pass VBR", ExportSettings { bitrate_mode: BitrateMode::Vbr2Pass, ..base.clone() }, (1280, 720)),
        ("non-square pixels", ExportSettings { pixel_aspect: Some((4, 3)), ..base.clone() }, (1280, 720)),
        ("MXF", ExportSettings { format: Format::MxfOp1a, ..base.clone() }, (1280, 720)),
        ("interlaced output", ExportSettings { field_order: FieldOrder::UpperFirst, ..base.clone() }, (1280, 720)),
        ("wider than an MP4 sample entry", base.clone(), (70_000, 64)),
        ("taller than an MP4 sample entry", base.clone(), (64, 70_000)),
    ];
    for (what, s, (w, h)) in cases {
        let before = hw_encode_stats();
        let r = factory(Format::Hevc, w, h, rate, &s);
        let Some(Err(e)) = r else { panic!("{what}: expected a declined request, got {}", if r.is_some() { "an encoder" } else { "None" }) };
        assert!(e.to_string().contains("NVENC"), "{what}: {e}");
        assert_eq!(delta(before), (0, 0, 1), "{what}: counted as declined, no session");
    }
    // HDR (PQ / HLG) is taken as Main 10 where the GPU has 10-bit HEVC, declined (counted, with the reason) where it has not
    for signal in [ColorSignal::PQ, ColorSignal::HLG] {
        let before = hw_encode_stats();
        let r = factory(Format::Hevc, 640, 360, rate, &ExportSettings { signal, ..base.clone() });
        if filmcraft_platform::nvenc::hevc_hdr_available() {
            assert!(matches!(r, Some(Ok(_))), "{signal:?}");
            assert_eq!(delta(before), (0, 1, 0), "{signal:?}");
        } else if hevc_available() {
            let Some(Err(e)) = r else { panic!("{signal:?}: expected a declined request") };
            assert!(e.to_string().contains("10-bit"), "{signal:?}: {e}");
            assert_eq!(delta(before), (0, 0, 1), "{signal:?}");
        }
    }
    // interlaced output is refused by the export's own validation, before any encoder is asked
    let (p, seq, m) = project(640, 360, 6);
    let before = hw_encode_stats();
    let s = ExportSettings { field_order: FieldOrder::UpperFirst, ..hevc(tmp("interlaced.mp4")) };
    assert!(export(&p, seq, &s, &m, &Progress::default()).is_err());
    assert_eq!(delta(before), (0, 0, 0));

    if !hevc_available() {
        // no HEVC encoder: the export's own "encoder not available" error, nothing written, no panic
        let err = export(&p, seq, &hevc(tmp("none.mp4")), &m, &Progress::default()).expect_err("no encoder").to_string();
        assert!(err.contains("encoder not available"), "{err}");
        eprintln!("SKIPPED the NVENC-limit cases: no NVENC HEVC here");
        return;
    }
    // sizes outside what the GPU's HEVC encoder takes, through the real export
    let (p, seq, m) = project(640, 360, 6);
    for (w, h) in [(64u32, 64u32), (16, 16), (10_000, 64)] {
        let path = tmp(&format!("small-{w}x{h}.mp4"));
        let s = ExportSettings { frame_size: Some((w, h)), ..hevc(path.clone()) };
        let before = hw_encode_stats();
        let r = export(&p, seq, &s, &m, &Progress::default());
        match r {
            Err(e) => {
                let e = e.to_string();
                eprintln!("{w}x{h}: {e}");
                assert!(e.contains("NVENC"), "{w}x{h}: {e}");
                assert_eq!(delta(before), (0, 0, 1), "{w}x{h}");
            }
            // a GPU that does take the size: a good file
            Ok(_) => assert_eq!(delta(before).1, 1, "{w}x{h}"),
        }
    }
    // the factory itself: an odd size and a size outside the limits are declined, a good one is taken
    let before = hw_encode_stats();
    assert!(matches!(factory(Format::Hevc, 641, 360, rate, &base), Some(Err(_))));
    assert!(matches!(factory(Format::Hevc, 20_000, 20_000, rate, &base), Some(Err(_))));
    assert_eq!(delta(before), (0, 0, 2));
    let before = hw_encode_stats();
    assert!(matches!(factory(Format::Hevc, 640, 360, rate, &base), Some(Ok(_))));
    assert_eq!(delta(before), (0, 1, 0));
    // H.264 with hardware encoding Off is still not NVENC's business, HEVC's toggle does not matter
    let before = hw_encode_stats();
    let off = ExportSettings { format: Format::H264, hardware_encoding: HardwareEncoding::Off, ..base.clone() };
    assert!(factory(Format::H264, 640, 360, rate, &off).is_none());
    assert!(matches!(factory(Format::Hevc, 640, 360, rate, &ExportSettings { hardware_encoding: HardwareEncoding::Off, ..base }), Some(Ok(_))));
    assert_eq!(delta(before), (0, 1, 0));
}
