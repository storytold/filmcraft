//! Export as AV1 (AV1 Main, NVENC) through the real export pipeline: MP4 files that ffprobe
//! describes as `av01` Main 8-bit 4:2:0 BT.709, that ffmpeg decodes without errors, and that our
//! own decoder decodes to pictures matching the software H.264 export; plus declined settings
//! handling. Skips without an NVIDIA GPU with an AV1 encoder.
#![cfg(target_os = "windows")]

use std::path::Path;
use std::process::Command;
use std::sync::Arc;

use filmcraft_export::{BitrateMode, ColorSignal, ExportSettings, FieldOrder, Format, H264Pass, HardwareEncoding, Progress, export, hw_encode_stats};
use filmcraft_media::generators::GeneratorSource;
use filmcraft_media::{DemoScene, FrameRequest, Generator, MediaSource};
use filmcraft_platform::nvenc::av1_available;
use filmcraft_platform::nvenc::export::factory;
use filmcraft_project::{ItemKind, Label, MediaClip, MediaRef, Project, SequenceSettings, TrackKind};
use filmcraft_render::SourceMap;
use filmcraft_time::{FrameRate, Tick, TimeRange};

fn project(w: u32, h: u32, frames: i64) -> (Arc<Project>, filmcraft_project::ItemId, SourceMap) {
    let rate = FrameRate::FPS_24;
    let mut p = Project::new("x");
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
    let seq = p.new_sequence("s", SequenceSettings { width: w, height: h, frame_rate: rate, ..Default::default() }, 1, 1, None);
    let v = p.make_track_item(clip, TrackKind::Video, Tick::ZERO, TimeRange::new(Tick::ZERO, rate.tick_of(frames)), rate).unwrap();
    p.sequence_mut(seq).unwrap().video_tracks[0].items.push(v);
    let mut m = SourceMap::default();
    m.0.insert(clip, Arc::new(g));
    (Arc::new(p), seq, m)
}

fn tmp(name: &str) -> String {
    let d = std::env::temp_dir().join(format!("fc-nvenc-av1-{}", std::process::id()));
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

fn av1(path: String) -> ExportSettings {
    ExportSettings { format: Format::Av1, path, include_audio: false, bitrate_kbps: 4000, bitrate_mode: BitrateMode::Vbr1Pass, ..Default::default() }
}

fn probe(ffprobe: &Path, path: &str) -> std::collections::HashMap<String, String> {
    let entries = "stream=codec_name,profile,codec_tag_string,pix_fmt,color_space,color_transfer,color_primaries,color_range,width,height,nb_frames,r_frame_rate,start_time";
    let out = Command::new(ffprobe).args(["-v", "error", "-select_streams", "v:0", "-show_entries", entries, "-of", "default=nw=1", path]).output().unwrap();
    assert!(out.status.success(), "ffprobe: {}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).lines().filter_map(|l| l.split_once('=')).map(|(k, v)| (k.to_string(), v.to_string())).collect()
}

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

fn assert_ffmpeg_decodes(ffmpeg: &Path, path: &str) {
    let out = Command::new(ffmpeg).args(["-v", "error", "-xerror", "-i", path, "-f", "null", "-"]).output().unwrap();
    assert!(out.status.success() && out.stderr.is_empty(), "ffmpeg: {}", String::from_utf8_lossy(&out.stderr));
}

fn delta(before: filmcraft_export::HwEncodeStats) -> (u64, u64, u64) {
    let after = hw_encode_stats();
    (after.frames - before.frames, after.sessions - before.sessions, after.declined - before.declined)
}

#[test]
fn av1_exports_play_everywhere() {
    let _one = crate::common::NVENC_SERIAL.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    filmcraft_platform::register();
    assert_eq!(filmcraft_export::available(Format::Av1), av1_available(), "the format is available iff NVENC can encode AV1");
    if !av1_available() {
        eprintln!("SKIPPED: no NVENC AV1 here");
        return;
    }
    let (p, seq, m) = project(1280, 720, 72);

    let soft_path = tmp("soft.mp4");
    let soft = ExportSettings { format: Format::H264, hardware_encoding: HardwareEncoding::Off, ..av1(soft_path.clone()) };
    let before = hw_encode_stats();
    export(&p, seq, &soft, &m, &Progress::default()).unwrap();
    assert_eq!(delta(before), (0, 0, 0), "H.264 with hardware encoding Off never touches NVENC");
    let reference = decode(&soft_path);

    let ffprobe = filmcraft_testkit::ffprobe_or_skip("nvenc av1 export");
    let ffmpeg = filmcraft_testkit::ffmpeg_or_skip("nvenc av1 export");
    for (name, toggle) in [("av1.mp4", HardwareEncoding::Auto), ("av1-off.mp4", HardwareEncoding::Off)] {
        let path = tmp(name);
        let s = ExportSettings { hardware_encoding: toggle, ..av1(path.clone()) };
        let before = hw_encode_stats();
        let report = export(&p, seq, &s, &m, &Progress::default()).unwrap_or_else(|e| panic!("{name}: {e}"));
        assert_eq!(report.frames, 72, "{name}");
        assert_eq!(delta(before), (72, 1, 0), "{name}: every picture through one NVENC session, nothing declined");

        let pictures = decode(&path);
        assert_eq!(pictures.len(), 72, "{name}");
        let worst = pictures.iter().zip(&reference).map(|(a, b)| luma_psnr(a, b)).fold(99.0, f64::min);
        eprintln!(
            "{name}: {} bytes (software H.264 {}), worst luma PSNR against the software export {worst:.1} dB",
            std::fs::metadata(&path).unwrap().len(),
            std::fs::metadata(&soft_path).unwrap().len()
        );
        assert!(worst > 45.0, "{name}: {worst:.1} dB");

        let (Some(ffprobe), Some(ffmpeg)) = (&ffprobe, &ffmpeg) else { continue };
        let v = probe(ffprobe, &path);
        let get = |k: &str| v.get(k).map(String::as_str).unwrap_or("?");
        assert_eq!((get("codec_name"), get("profile"), get("codec_tag_string"), get("pix_fmt")), ("av1", "Main", "av01", "yuv420p"), "{name}: {v:?}");
        assert_eq!((get("color_space"), get("color_transfer"), get("color_primaries"), get("color_range")), ("bt709", "bt709", "bt709", "tv"), "{name}: {v:?}");
        assert_eq!((get("width"), get("height"), get("nb_frames"), get("r_frame_rate")), ("1280", "720", "72", "24/1"), "{name}: {v:?}");
        assert!(get("start_time").parse::<f64>().is_ok_and(|t| t.abs() < 1e-3), "{name}: starts at {}", get("start_time"));
        assert_eq!(keyframes(ffprobe, &path), vec![0, 48], "{name}: keyframes every 2 s");
        assert_ffmpeg_decodes(ffmpeg, &path);
        eprintln!("{name}: ffprobe {v:?}, ffmpeg -xerror decodes it cleanly");
    }
}

#[test]
fn av1_export_with_audio() {
    let _one = crate::common::NVENC_SERIAL.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    filmcraft_platform::register();
    if !av1_available() {
        eprintln!("SKIPPED: no NVENC AV1 here");
        return;
    }
    let (p, seq, m) = project(640, 360, 48);
    let path = tmp("av1-audio.mp4");
    let s = ExportSettings { include_audio: true, keyframe_distance: Some(24), ..av1(path.clone()) };
    export(&p, seq, &s, &m, &Progress::default()).expect("export");
    let bytes = std::fs::read(&path).unwrap();
    assert!(!has_stbl_ctts(&bytes), "AV1 ISOBMFF must not contain a ctts box in any track stbl");
    let file = filmcraft_isobmff::open(bytes.as_slice()).unwrap();
    assert!(file.track_of_kind(filmcraft_isobmff::TrackKind::Video).is_some());
    let at = file.track_of_kind(filmcraft_isobmff::TrackKind::Audio).expect("an audio track");
    assert!(matches!(&file.tracks[at].entries[0].codec, filmcraft_isobmff::CodecConfig::Aac(_)));
    if let (Some(ffprobe), Some(ffmpeg)) = (filmcraft_testkit::ffprobe_or_skip("nvenc av1 export"), filmcraft_testkit::ffmpeg_or_skip("nvenc av1 export")) {
        let out = Command::new(&ffprobe)
            .args(["-v", "error", "-show_entries", "stream=codec_name,codec_type,codec_tag_string", "-of", "csv=p=0", &path])
            .output()
            .unwrap();
        let streams = String::from_utf8_lossy(&out.stdout).to_string();
        assert!(streams.contains("av1,video,av01") && streams.contains("aac,audio,mp4a"), "{streams}");
        assert_ffmpeg_decodes(&ffmpeg, &path);
        assert_eq!(keyframes(&ffprobe, &path), vec![0, 24]);
    }
}

#[test]
fn what_nvenc_av1_cannot_do_is_an_error_that_says_why() {
    let _one = crate::common::NVENC_SERIAL.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    filmcraft_platform::register();
    let rate = FrameRate::FPS_24;
    let base = av1(tmp("declined.mp4"));
    let cases: Vec<(&str, ExportSettings, (u32, u32))> = vec![
        ("HDR PQ", ExportSettings { signal: ColorSignal::PQ, ..base.clone() }, (1280, 720)),
        ("HDR HLG", ExportSettings { signal: ColorSignal::HLG, ..base.clone() }, (1280, 720)),
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
        let r = factory(Format::Av1, w, h, rate, &s);
        let Some(Err(e)) = r else { panic!("{what}: expected a declined request, got {}", if r.is_some() { "an encoder" } else { "None" }) };
        assert!(e.to_string().contains("NVENC"), "{what}: {e}");
        assert_eq!(delta(before), (0, 0, 1), "{what}: counted as declined, no session");
    }
}

fn iter_mp4_boxes(mut data: &[u8]) -> Vec<([u8; 4], &[u8])> {
    let mut boxes = Vec::new();
    while data.len() >= 8 {
        let size32 = u32::from_be_bytes([data[0], data[1], data[2], data[3]]) as usize;
        let box_type = [data[4], data[5], data[6], data[7]];
        let (box_len, mut header_len) = match size32 {
            0 => (data.len(), 8),
            1 => {
                if data.len() < 16 {
                    break;
                }
                let Ok(len) = usize::try_from(u64::from_be_bytes(data[8..16].try_into().unwrap())) else {
                    break;
                };
                (len, 16)
            }
            n => (n, 8),
        };
        if box_type == *b"uuid" {
            header_len += 16;
        }
        if box_len < header_len || box_len > data.len() {
            break;
        }
        boxes.push((box_type, &data[header_len..box_len]));
        data = &data[box_len..];
    }
    boxes
}

fn has_box_path(data: &[u8], path: &[[u8; 4]]) -> bool {
    let Some((&target, rest)) = path.split_first() else {
        return true;
    };
    iter_mp4_boxes(data).into_iter().any(|(tag, body)| tag == target && has_box_path(body, rest))
}

fn has_stbl_ctts(mp4: &[u8]) -> bool {
    has_box_path(mp4, &[*b"moov", *b"trak", *b"mdia", *b"minf", *b"stbl", *b"ctts"])
}

#[test]
fn mp4_ctts_check_ignores_media_payload_and_detects_stbl_ctts() {
    fn mp4_box(tag: [u8; 4], body: &[u8]) -> Vec<u8> {
        let len = (8 + body.len()) as u32;
        let mut out = Vec::with_capacity(len as usize);
        out.extend_from_slice(&len.to_be_bytes());
        out.extend_from_slice(&tag);
        out.extend_from_slice(body);
        out
    }

    let clean_stbl = mp4_box(*b"stbl", &mp4_box(*b"stsd", &[0u8; 8]));
    let clean_moov = mp4_box(*b"moov", &mp4_box(*b"trak", &mp4_box(*b"mdia", &mp4_box(*b"minf", &clean_stbl))));
    let mdat_with_ctts_bytes = mp4_box(*b"mdat", b"video-payload-ctts-bytes");
    let mut clean_mp4 = mp4_box(*b"ftyp", b"isom   isom");
    clean_mp4.extend_from_slice(&mdat_with_ctts_bytes);
    clean_mp4.extend_from_slice(&clean_moov);
    assert!(!has_stbl_ctts(&clean_mp4), "ctts bytes inside mdat payload must not trigger false positive");

    let mut stbl_with_ctts_body = mp4_box(*b"stsd", &[0u8; 8]);
    stbl_with_ctts_body.extend_from_slice(&mp4_box(*b"ctts", &[0u8; 12]));
    let dirty_moov = mp4_box(*b"moov", &mp4_box(*b"trak", &mp4_box(*b"mdia", &mp4_box(*b"minf", &mp4_box(*b"stbl", &stbl_with_ctts_body)))));
    let mut dirty_mp4 = mp4_box(*b"ftyp", b"isom   isom");
    dirty_mp4.extend_from_slice(&dirty_moov);
    assert!(has_stbl_ctts(&dirty_mp4), "ctts box inside track stbl must be detected");
}
