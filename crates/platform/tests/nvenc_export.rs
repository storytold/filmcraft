//! Export with Hardware encoding (NVENC) against the software encoder, through the real export
//! pipeline: the same frames, a file both decode, counters showing which encoder ran, and what NVENC
//! declines (hardware encoding Off, two-pass, MXF...) going to the software encoder. Its own test
//! binary: the encoder registry is process-wide. Skips without an NVIDIA GPU with NVENC.
#![cfg(any(target_os = "windows", all(target_os = "linux", target_pointer_width = "64")))]

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
    let d = std::env::temp_dir().join(format!("fc-nvenc-{}", std::process::id()));
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
    let before_soft = hw_encode_stats();
    export(&p, seq, &settings(soft_path.clone(), HardwareEncoding::Off), &m, &Progress::default()).unwrap();
    assert_eq!(hw_encode_stats(), before_soft, "Off: no hardware encoder attempt");

    let hw_path = tmp("hw.mp4");
    let before = hw_encode_stats();
    let report = export(&p, seq, &settings(hw_path.clone(), HardwareEncoding::Auto), &m, &Progress::default()).unwrap();
    let after = hw_encode_stats();
    if after.sessions == before.sessions {
        assert_eq!(after.declined - before.declined, 1, "Auto attempted hardware and reported the fallback");
        assert_eq!(after.frames, before.frames);
        assert_eq!(std::fs::read(&soft_path).unwrap(), std::fs::read(&hw_path).unwrap(), "fallback preserves software determinism");
        eprintln!("no NVENC here: byte-identical software fallback verified");
        return;
    }
    assert_eq!(report.frames, 72);
    assert_eq!(after.frames - before.frames, 72, "every picture went through NVENC");

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
fn what_nvenc_does_not_take_goes_to_the_software_encoder() {
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
    // an odd frame size is rounded to even by the pipeline, so NVENC still takes it
    let hardware_available = filmcraft_platform::nvenc::available();
    let odd = run("odd.mp4", &|s| s.frame_size = Some((641, 359)));
    assert_eq!(odd, if hardware_available { (24, 0) } else { (0, 1) });
    // Off never touches the hardware encoder
    assert_eq!(run("off.mp4", &|s| s.hardware_encoding = HardwareEncoding::Off), (0, 0));
    // the decoded file of a declined export is fine
    assert_eq!(decode(&tmp("odd.mp4")).len(), 24);
}
