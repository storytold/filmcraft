//! Export with Hardware encoding (VA-API) against the software encoder, through the real export
//! pipeline: the same frames, a file both decode, counters showing which encoder ran, and what NVENC
//! declines (hardware encoding Off, two-pass, MXF...) going to the software encoder. Its own test
//! binary: the encoder registry is process-wide. Skips without a VA-API driver that encodes H.264.
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

#[test]
fn hardware_export_matches_the_software_export() {
    let _one = SERIAL.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    filmcraft_platform::register();
    let (p, seq, m) = project(1280, 720, 72);
    let soft_path = tmp("soft.mp4");
    let off = hw_encode_stats().sessions;
    export(&p, seq, &settings(soft_path.clone(), HardwareEncoding::Off), &m, &Progress::default()).unwrap();
    assert_eq!(hw_encode_stats().sessions, off, "Off: no hardware encoder");

    let hw_path = tmp("hw.mp4");
    let before = hw_encode_stats();
    let report = export(&p, seq, &settings(hw_path.clone(), HardwareEncoding::Auto), &m, &Progress::default()).unwrap();
    let after = hw_encode_stats();
    if after.sessions == before.sessions {
        eprintln!("SKIPPED: no VA-API H.264 encoder here (declined {})", after.declined - before.declined);
        return;
    }
    assert_eq!(report.frames, 72);
    assert_eq!(after.frames - before.frames, 72, "every picture went through the GPU encoder");

    let (soft, hard) = (decode(&soft_path), decode(&hw_path));
    assert_eq!((soft.len(), hard.len()), (72, 72), "frame counts");
    let worst = soft.iter().zip(&hard).map(|(a, b)| luma_psnr(a, b)).fold(99.0, f64::min);
    assert!(worst > 30.0, "hardware export differs from the software one: {worst:.1} dB");
    let size = |p: &str| std::fs::metadata(p).unwrap().len();
    eprintln!("software {} bytes, hardware {} bytes, worst luma PSNR between them {worst:.1} dB", size(&soft_path), size(&hw_path));

    // the file plays: ffmpeg decodes it without errors (external oracle only)
    if let Some(ff) = filmcraft_testkit::ffmpeg() {
        let out = std::process::Command::new(ff).args(["-v", "error", "-i", &hw_path, "-f", "null", "-"]).output().unwrap();
        assert!(out.stderr.is_empty(), "ffmpeg: {}", String::from_utf8_lossy(&out.stderr));
    }
}

#[test]
fn what_the_gpu_does_not_take_goes_to_the_software_encoder() {
    let _one = SERIAL.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    filmcraft_platform::register();
    let (p, seq, m) = project(640, 360, 24);
    let run = |name: &str, f: &dyn Fn(&mut ExportSettings)| {
        let mut s = settings(tmp(name), HardwareEncoding::Auto);
        f(&mut s);
        let before = hw_encode_stats();
        export(&p, seq, &s, &m, &Progress::default()).unwrap();
        let after = hw_encode_stats();
        (after.frames - before.frames, after.declined - before.declined)
    };
    // two-pass VBR is a software feature (both passes ask, both are declined)
    assert_eq!(run("two_pass.mp4", &|s| s.bitrate_mode = BitrateMode::Vbr2Pass), (0, 2));
    // MXF H.264 is an Annex B stream with in-band parameter sets: the software encoder's
    assert_eq!(
        run("mxf.mxf", &|s| {
            s.format = Format::MxfOp1a;
            s.mxf_video_codec = filmcraft_export::MxfVideoCodec::H264;
        }),
        (0, 1)
    );
    // an odd frame size is rounded to even by the pipeline, so the GPU still takes it (cropped in the SPS)
    assert_eq!(run("odd.mp4", &|s| s.frame_size = Some((641, 359))), (24, 0));
    // Off never touches the hardware encoder
    assert_eq!(run("off.mp4", &|s| s.hardware_encoding = HardwareEncoding::Off), (0, 0));
    // the decoded file of a declined export is fine
    assert_eq!(decode(&tmp("odd.mp4")).len(), 24);
}

/// H.265 has no software encoder: on a GPU that encodes it the format is offered, and its export
/// decodes in our decoder (close to the software H.264 export of the same frames) and in ffmpeg.
#[test]
fn hevc_export_on_the_gpu() {
    let _one = SERIAL.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    filmcraft_platform::register();
    if !filmcraft_export::available(Format::Hevc) {
        eprintln!("SKIPPED: no VA-API H.265 encoder here");
        return;
    }
    let (p, seq, m) = project(1280, 720, 72);
    let reference = tmp("ref_h264.mp4");
    export(&p, seq, &settings(reference.clone(), HardwareEncoding::Off), &m, &Progress::default()).unwrap();
    let path = tmp("hevc.mp4");
    let mut s = settings(path.clone(), HardwareEncoding::Off);
    s.format = Format::Hevc;
    let before = hw_encode_stats();
    let report = export(&p, seq, &s, &m, &Progress::default()).unwrap();
    assert_eq!(report.frames, 72);
    assert_eq!(hw_encode_stats().frames - before.frames, 72, "every picture went through the GPU encoder");
    let (want, got) = (decode(&reference), decode(&path));
    assert_eq!(got.len(), 72, "frame count");
    let worst = want.iter().zip(&got).map(|(a, b)| luma_psnr(a, b)).fold(99.0, f64::min);
    eprintln!("H.265 {} bytes, worst luma PSNR against the H.264 export {worst:.1} dB", std::fs::metadata(&path).unwrap().len());
    assert!(worst > 30.0, "H.265 export differs: {worst:.1} dB");
    if let Some(ff) = filmcraft_testkit::ffmpeg() {
        let out = std::process::Command::new(&ff).args(["-v", "error", "-i", &path, "-f", "null", "-"]).output().unwrap();
        assert!(out.stderr.is_empty(), "ffmpeg: {}", String::from_utf8_lossy(&out.stderr));
        let probe = ff.with_file_name("ffprobe");
        let out = std::process::Command::new(probe)
            .args([
                "-v",
                "error",
                "-select_streams",
                "v:0",
                "-show_entries",
                "stream=codec_name,profile,codec_tag_string,pix_fmt,width,height,color_primaries",
                "-of",
                "csv=p=0",
                &path,
            ])
            .output()
            .unwrap();
        let info = String::from_utf8_lossy(&out.stdout);
        assert_eq!(info.trim(), "hevc,Main,hvc1,1280,720,yuv420p,bt709", "ffprobe");
    }
    // sizes that are not whole coding blocks / tree blocks: the SPS crops them
    for (w, h) in [(642, 362), (640, 360), (1000, 562)] {
        let (p, seq, m) = project(w, h, 12);
        let path = tmp(&format!("hevc_{w}x{h}.mp4"));
        let mut s = settings(path.clone(), HardwareEncoding::Off);
        s.format = Format::Hevc;
        export(&p, seq, &s, &m, &Progress::default()).unwrap();
        let got = decode(&path);
        assert_eq!(got.len(), 12, "{w}x{h}");
        assert_eq!((got[0].width, got[0].height), (w, h), "cropped to the exported size");
        let reference = tmp(&format!("ref_{w}x{h}.mp4"));
        export(&p, seq, &settings(reference.clone(), HardwareEncoding::Off), &m, &Progress::default()).unwrap();
        let worst = decode(&reference).iter().zip(&got).map(|(a, b)| luma_psnr(a, b)).fold(99.0, f64::min);
        assert!(worst > 30.0, "{w}x{h}: {worst:.1} dB from the H.264 export");
        if let Some(ff) = filmcraft_testkit::ffmpeg() {
            let out = std::process::Command::new(ff).args(["-v", "error", "-i", &path, "-f", "null", "-"]).output().unwrap();
            assert!(out.stderr.is_empty(), "ffmpeg {w}x{h}: {}", String::from_utf8_lossy(&out.stderr));
        }
    }
}
