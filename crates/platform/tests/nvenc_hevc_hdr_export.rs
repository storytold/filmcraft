//! HDR export as H.265 Main 10 (NVENC) through the real export pipeline: an HDR (Rec. 2100 PQ / HLG)
//! sequence gives MP4 / QuickTime files that ffprobe (an external oracle only) describes as `hvc1`
//! Main 10, yuv420p10le, BT.2020 with the right transfer, limited range, with HDR10 static metadata for
//! PQ; that ffmpeg decodes without errors to the same 10-bit pictures our own decoder makes; that
//! our importer reads back as PQ / HLG, 10-bit; whose pixels follow a ProRes HDR export of the same
//! sequence. SDR stays Main 8-bit BT.709, `settings.sdr` forces it. Its own test binary: the encoder
//! registry and the counters are process-wide. Skips (SKIPPED) only without a 10-bit HEVC encoder.
#![cfg(target_os = "windows")]

use std::path::Path;
use std::process::Command;
use std::sync::Arc;

use filmcraft_color::{ColorPipeline, WorkingSpace};
use filmcraft_export::{BitrateMode, ColorSignal, EncoderFrame, ExportSettings, Format, Progress, export, hw_encode_stats};
use filmcraft_media::generators::GeneratorSource;
use filmcraft_media::{DemoScene, FrameRequest, Generator, MediaSource};
use filmcraft_platform::nvenc::export::factory;
use filmcraft_platform::nvenc::{hevc_available, hevc_hdr_available};
use filmcraft_project::{ItemKind, Label, MediaClip, MediaRef, Project, SequenceSettings, TrackKind};
use filmcraft_render::SourceMap;
use filmcraft_time::{FrameRate, Tick, TimeRange};

/// The encoder counters are process-wide: one test at a time.
static SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());

const W: u32 = 1280;
const H: u32 = 720;
const FRAMES: i64 = 24;

fn project(working: WorkingSpace) -> (Arc<Project>, filmcraft_project::ItemId, SourceMap) {
    let rate = FrameRate::FPS_24;
    let mut p = Project::new("x");
    let g = GeneratorSource::new(Generator::Demo(DemoScene::OceanSunset), W, H, rate, rate.tick_of(FRAMES));
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
    let color = ColorPipeline { working, ..ColorPipeline::REC709 };
    let seq = p.new_sequence("s", SequenceSettings { width: W, height: H, frame_rate: rate, color, ..Default::default() }, 1, 1, None);
    let v = p.make_track_item(clip, TrackKind::Video, Tick::ZERO, TimeRange::new(Tick::ZERO, rate.tick_of(FRAMES)), rate).unwrap();
    p.sequence_mut(seq).unwrap().video_tracks[0].items.push(v);
    let mut m = SourceMap::default();
    m.0.insert(clip, Arc::new(g));
    (Arc::new(p), seq, m)
}

fn tmp(name: &str) -> String {
    let d = std::env::temp_dir().join(format!("fc-nvenc-hdr-{}", std::process::id()));
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

fn hevc(path: String) -> ExportSettings {
    ExportSettings { format: Format::Hevc, path, include_audio: false, bitrate_kbps: 10_000, bitrate_mode: BitrateMode::Vbr1Pass, ..Default::default() }
}

fn probe(ffprobe: &Path, path: &str) -> std::collections::HashMap<String, String> {
    let entries = "stream=codec_name,profile,codec_tag_string,pix_fmt,color_space,color_transfer,color_primaries,color_range,width,height,nb_frames,level";
    let out = Command::new(ffprobe).args(["-v", "error", "-select_streams", "v:0", "-show_entries", entries, "-of", "default=nw=1", path]).output().unwrap();
    assert!(out.status.success(), "ffprobe: {}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).lines().filter_map(|l| l.split_once('=')).map(|(k, v)| (k.to_string(), v.to_string())).collect()
}

/// The side data ffprobe reports for the first frame.
fn first_frame_side_data(ffprobe: &Path, path: &str) -> String {
    let out = Command::new(ffprobe)
        .args([
            "-v",
            "error",
            "-select_streams",
            "v:0",
            "-read_intervals",
            "%+#1",
            "-show_frames",
            "-show_entries",
            "frame=side_data_list",
            "-of",
            "default=nw=1",
            path,
        ])
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout).to_string()
}

fn assert_ffmpeg_decodes(ffmpeg: &Path, path: &str) {
    let out = Command::new(ffmpeg).args(["-v", "error", "-xerror", "-i", path, "-f", "null", "-"]).output().unwrap();
    assert!(out.status.success() && out.stderr.is_empty(), "ffmpeg: {}", String::from_utf8_lossy(&out.stderr));
}

/// The first picture as ffmpeg decodes it to yuv420p10le: (Y, Cb, Cr) code values.
fn ffmpeg_first_picture(ffmpeg: &Path, path: &str, w: usize, h: usize) -> (Vec<u16>, Vec<u16>, Vec<u16>) {
    let out = Command::new(ffmpeg).args(["-v", "error", "-i", path, "-frames:v", "1", "-f", "rawvideo", "-pix_fmt", "yuv420p10le", "-"]).output().unwrap();
    assert!(out.status.success(), "ffmpeg: {}", String::from_utf8_lossy(&out.stderr));
    let samples: Vec<u16> = out.stdout.as_chunks::<2>().0.iter().map(|b| u16::from_le_bytes(*b)).collect();
    assert_eq!(samples.len(), w * h * 3 / 2, "one yuv420p10le picture");
    let (y, c) = samples.split_at(w * h);
    let (u, v) = c.split_at(c.len() / 2);
    (y.to_vec(), u.to_vec(), v.to_vec())
}

fn frame<'a>(rgba: &'a [u8], hdr: Option<&'a [f32]>, index: u64) -> EncoderFrame<'a> {
    EncoderFrame { width: 640, height: 360, rgba, hdr, index }
}

fn psnr10(a: &[u16], b: &[u16]) -> f64 {
    let mse = a.iter().zip(b).map(|(x, y)| (f64::from(*x) - f64::from(*y)).powi(2)).sum::<f64>() / a.len() as f64;
    if mse == 0.0 { 99.0 } else { 10.0 * (1023.0f64 * 1023.0 / mse).log10() }
}

fn delta(before: filmcraft_export::HwEncodeStats) -> (u64, u64, u64) {
    let after = hw_encode_stats();
    (after.frames - before.frames, after.sessions - before.sessions, after.declined - before.declined)
}

fn yuv16(f: &filmcraft_frame::VideoFrame) -> ([Arc<Vec<u16>>; 3], u32) {
    match &f.data {
        filmcraft_frame::PixelData::Yuv16 { planes, bits, .. } => (planes.clone(), *bits),
        _ => panic!("16-bit planes expected, got {}", f.format_label()),
    }
}

#[test]
fn hdr_sequences_export_as_main_10() {
    let _one = SERIAL.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    filmcraft_platform::register();
    assert_eq!(filmcraft_export::hdr_available(Format::Hevc), hevc_hdr_available(), "HDR H.265 is available iff NVENC can encode Main 10");
    if !hevc_hdr_available() {
        eprintln!("SKIPPED: no NVENC HEVC Main 10 here (hevc: {})", hevc_available());
        return;
    }
    let ffprobe = filmcraft_testkit::ffprobe_or_skip("nvenc hdr export");
    let ffmpeg = filmcraft_testkit::ffmpeg_or_skip("nvenc hdr export");
    for (ws, ext, ff_transfer, pq) in [
        (WorkingSpace::Rec2100Pq, "mp4", "smpte2084", true),
        (WorkingSpace::Rec2100Hlg, "mp4", "arib-std-b67", false),
        (WorkingSpace::Rec2100Pq, "mov", "smpte2084", true),
    ] {
        let name = format!("{}.{ext}", ws.id());
        let (p, seq, m) = project(ws);

        // the ProRes HDR export of the same sequence is the reference for the pixels (10-bit BT.2020, same matrix and range)
        let ref_path = tmp(&format!("ref-{}.mov", ws.id()));
        let before = hw_encode_stats();
        export(
            &p,
            seq,
            &ExportSettings { format: Format::ProRes, path: ref_path.clone(), include_audio: false, ..Default::default() },
            &m,
            &Progress::default(),
        )
        .unwrap();
        assert_eq!(delta(before), (0, 0, 0), "ProRes never touches NVENC");
        let reference = decode(&ref_path);

        let path = tmp(&name);
        let s = hevc(path.clone());
        let before = hw_encode_stats();
        let report = export(&p, seq, &s, &m, &Progress::default()).unwrap_or_else(|e| panic!("{name}: {e}"));
        assert_eq!(report.frames, FRAMES as u64, "{name}");
        assert_eq!(delta(before), (FRAMES as u64, 1, 0), "{name}: every picture through one NVENC session, nothing declined");

        // our importer: PQ / HLG, BT.2020, 10-bit planes
        let bytes: Arc<[u8]> = std::fs::read(&path).unwrap().into();
        let src = filmcraft_codecs::open_bytes(&path, bytes.clone()).unwrap();
        let v = src.info().video.clone().unwrap();
        let (want_transfer, want_hdr) = if pq { (filmcraft_color::Transfer::Pq, true) } else { (filmcraft_color::Transfer::Hlg, false) };
        assert_eq!(
            (v.color.transfer, v.color.primaries, v.color.matrix),
            (want_transfer, filmcraft_color::Primaries::Bt2020, filmcraft_color::Matrix::Bt2020Ncl),
            "{name}"
        );
        if want_hdr {
            let hdr = v.hdr.unwrap_or_else(|| panic!("{name}: no HDR metadata"));
            assert_eq!((hdr.mastering_max_nits, hdr.max_cll, hdr.peak_nits()), (Some(1000.0), None, Some(1000.0)), "{name}");
        }
        // and the sample entry boxes
        let file = filmcraft_isobmff::open(&bytes[..]).unwrap();
        let vt = file.track_of_kind(filmcraft_isobmff::TrackKind::Video).expect("a video track");
        let ve = file.tracks[vt].entries[0].video.clone().expect("a video sample entry");
        assert!(matches!(ve.color, Some(filmcraft_isobmff::ColorInfo::Nclx { primaries: 9, matrix: 9, full_range: false, .. })), "{name}: {:?}", ve.color);
        assert_eq!(ve.mastering_display.is_some() && ve.content_light == Some((0, 0)), pq, "{name}: mdcv / clli");

        let pictures = decode(&path);
        assert_eq!(pictures.len(), FRAMES as usize, "{name}");
        let (mut worst, mut best_levels) = (99.0f64, usize::MAX);
        let (mut peak, mut ref_peak) = (0u16, 0u16);
        for (a, b) in pictures.iter().zip(&reference) {
            let (pa, bits) = yuv16(a);
            let (pb, _) = yuv16(b);
            assert_eq!(bits, 10, "{name}: decoded bit depth");
            assert!(pa[0].iter().all(|c| *c < 1024));
            worst = worst.min(psnr10(&pa[0], &pb[0]));
            peak = peak.max(pa[0].iter().copied().max().unwrap_or(0));
            ref_peak = ref_peak.max(pb[0].iter().copied().max().unwrap_or(0));
            let mut levels: Vec<u16> = pa[0].to_vec();
            levels.sort_unstable();
            levels.dedup();
            best_levels = best_levels.min(levels.len());
        }
        eprintln!(
            "{name}: {} bytes, worst luma PSNR against the ProRes HDR export {worst:.1} dB, {best_levels} distinct luma levels per picture (min), peak luma code {peak} (ProRes {ref_peak})",
            report.bytes
        );
        assert!(worst > 35.0, "{name}: {worst:.1} dB against the ProRes HDR export");
        assert!(best_levels > 256, "{name}: {best_levels} distinct luma levels: not 10-bit");
        assert!(peak.abs_diff(ref_peak) < 24, "{name}: peak {peak} against {ref_peak}");

        let (Some(ffprobe), Some(ffmpeg)) = (&ffprobe, &ffmpeg) else { continue };
        let f = probe(ffprobe, &path);
        let get = |k: &str| f.get(k).map(String::as_str).unwrap_or("?");
        assert_eq!((get("codec_name"), get("profile"), get("codec_tag_string"), get("pix_fmt")), ("hevc", "Main 10", "hvc1", "yuv420p10le"), "{name}: {f:?}");
        assert_eq!(
            (get("color_primaries"), get("color_transfer"), get("color_space"), get("color_range")),
            ("bt2020", ff_transfer, "bt2020nc", "tv"),
            "{name}: {f:?}"
        );
        assert_eq!((get("width"), get("height"), get("nb_frames")), ("1280", "720", "24"), "{name}: {f:?}");
        let side = first_frame_side_data(ffprobe, &path);
        if pq {
            assert!(
                side.contains("Mastering display metadata") && side.contains("max_luminance=10000000/10000") && side.contains("min_luminance=1/10000"),
                "{name}: {side}"
            );
            assert!(side.contains("Content light level metadata") && side.contains("max_content=0") && side.contains("max_average=0"), "{name}: {side}");
        } else {
            assert!(!side.contains("Mastering display") && !side.contains("Content light level"), "{name}: {side}");
        }
        assert_ffmpeg_decodes(ffmpeg, &path);
        // ffmpeg's yuv420p10le picture is our decoder's picture
        let (fy, fu, fv) = ffmpeg_first_picture(ffmpeg, &path, W as usize, H as usize);
        let (ours, _) = yuv16(&pictures[0]);
        let diff = |a: &[u16], b: &[u16]| a.iter().zip(b).map(|(x, y)| x.abs_diff(*y)).max().unwrap_or(0);
        eprintln!(
            "{name}: ffprobe {f:?}\n{name}: first-frame side data {:?}\n{name}: ffmpeg vs our decoder, max code difference Y {} Cb {} Cr {}",
            side.lines().collect::<Vec<_>>(),
            diff(&fy, &ours[0]),
            diff(&fu, &ours[1]),
            diff(&fv, &ours[2])
        );
        assert_eq!(
            (diff(&fy, &ours[0]), diff(&fu, &ours[1]), diff(&fv, &ours[2])),
            (0, 0, 0),
            "{name}: our HEVC decoder and ffmpeg disagree on the 10-bit pictures"
        );
    }
}

#[test]
fn sdr_stays_main_8_bit_and_the_sdr_setting_tone_maps_hdr_sequences() {
    let _one = SERIAL.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    filmcraft_platform::register();
    if !hevc_hdr_available() {
        eprintln!("SKIPPED: no NVENC HEVC Main 10 here");
        return;
    }
    let ffprobe = filmcraft_testkit::ffprobe_or_skip("nvenc hdr export");
    // an HDR sequence with SDR asked for, and a plain SDR sequence: Main, 8-bit, BT.709
    for (what, ws, sdr) in [
        ("HDR sequence, settings.sdr", WorkingSpace::Rec2100Pq, true),
        ("HLG sequence, settings.sdr", WorkingSpace::Rec2100Hlg, true),
        ("SDR sequence", ColorPipeline::REC709.working, false),
    ] {
        let (p, seq, m) = project(ws);
        let path = tmp(&format!("sdr-{}.mp4", what.replace([' ', ','], "-")));
        let s = ExportSettings { sdr, ..hevc(path.clone()) };
        let before = hw_encode_stats();
        export(&p, seq, &s, &m, &Progress::default()).unwrap_or_else(|e| panic!("{what}: {e}"));
        assert_eq!(delta(before), (FRAMES as u64, 1, 0), "{what}");
        let pictures = decode(&path);
        assert!(matches!(&pictures[0].data, filmcraft_frame::PixelData::Yuv8 { .. }), "{what}: 8-bit planes");
        assert_eq!(pictures[0].color.transfer, filmcraft_color::Transfer::Bt709, "{what}");
        let bytes: Arc<[u8]> = std::fs::read(&path).unwrap().into();
        let file = filmcraft_isobmff::open(&bytes[..]).unwrap();
        let vt = file.track_of_kind(filmcraft_isobmff::TrackKind::Video).unwrap();
        let ve = file.tracks[vt].entries[0].video.clone().unwrap();
        assert!(ve.mastering_display.is_none() && ve.content_light.is_none(), "{what}: no HDR boxes");
        if let Some(ffprobe) = &ffprobe {
            let f = probe(ffprobe, &path);
            let get = |k: &str| f.get(k).map(String::as_str).unwrap_or("?");
            assert_eq!(
                (get("profile"), get("pix_fmt"), get("color_primaries"), get("color_transfer"), get("color_space"), get("color_range")),
                ("Main", "yuv420p", "bt709", "bt709", "bt709", "tv"),
                "{what}: {f:?}"
            );
            assert!(!first_frame_side_data(ffprobe, &path).contains("Mastering"), "{what}");
        }
    }
}

#[test]
fn the_factory_rejects_mismatched_pictures_and_clamps_hostile_floats() {
    let _one = SERIAL.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    filmcraft_platform::register();
    if !hevc_hdr_available() {
        eprintln!("SKIPPED: no NVENC HEVC Main 10 here");
        return;
    }
    let rate = FrameRate::FPS_24;
    let (w, h) = (640u32, 360u32);
    let rgba = vec![128u8; (w * h * 4) as usize];
    let hdr_settings = ExportSettings { signal: ColorSignal::PQ, ..hevc(tmp("x.mp4")) };
    let sdr_settings = hevc(tmp("y.mp4"));
    let Some(Ok(mut ten)) = factory(Format::Hevc, w, h, rate, &hdr_settings) else { panic!("a Main 10 encoder") };
    let Some(Ok(mut eight)) = factory(Format::Hevc, w, h, rate, &sdr_settings) else { panic!("a Main encoder") };
    let good = vec![0.5f32; (w * h * 3) as usize];
    // the HDR encoder wants the HDR picture, the SDR encoder the RGBA one
    assert!(ten.encode(&frame(&rgba, None, 0)).is_err());
    assert!(eight.encode(&frame(&rgba, Some(&good), 0)).is_err());
    // wrong-length floats (short, long, empty) and a wrong picture size: errors, nothing submitted
    for bad in [&good[..good.len() - 1], &[][..], &vec![0.5f32; good.len() + 3][..]] {
        assert!(ten.encode(&frame(&rgba, Some(bad), 0)).is_err(), "{} floats", bad.len());
    }
    assert!(ten.encode(&EncoderFrame { width: w + 2, height: h, rgba: &rgba, hdr: Some(&good), index: 0 }).is_err());
    // NaN, infinities and out-of-range values clamp: the pictures encode
    let mut hostile = vec![0.25f32; good.len()];
    for (i, x) in hostile.iter_mut().enumerate() {
        *x = [f32::NAN, f32::INFINITY, f32::NEG_INFINITY, -3.0, 7.0, 0.5][i % 6];
    }
    let mut packets = 0;
    for i in 0..4 {
        packets += ten.encode(&frame(&rgba, Some(&hostile), i)).unwrap().len();
    }
    packets += ten.flush().unwrap().len();
    assert_eq!(packets, 4);
    // the sample entry of the HDR encoder carries colr + mdcv + clli, the SDR one nothing HDR
    let e = ten.sample_entry();
    let v = e.video.as_ref().unwrap();
    assert!(v.color.is_some() && v.mastering_display.is_some() && v.content_light == Some((0, 0)));
    assert!(eight.sample_entry().video.as_ref().unwrap().color.is_none());
    // HLG: colr only
    let Some(Ok(hlg)) = factory(Format::Hevc, w, h, rate, &ExportSettings { signal: ColorSignal::HLG, ..sdr_settings.clone() }) else {
        panic!("an HLG encoder")
    };
    let v = hlg.sample_entry().video.unwrap();
    assert!(v.color.is_some() && v.mastering_display.is_none() && v.content_light.is_none());
    // H.264 still declines HDR (software signalling), and says why
    let h264 = ExportSettings { format: Format::H264, signal: ColorSignal::PQ, ..sdr_settings };
    assert!(factory(Format::H264, w, h, rate, &h264).is_none());
}
