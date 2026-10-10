//! M6.5 export settings: image sequences, audio-only formats, frame size / rate / scaling,
//! loudness normalization, two-pass and CBR H.264, the video limiter, overlays, metadata and the
//! QuickTime multiplexer. ffprobe / ffmpeg are used only as external test oracles.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use filmcraft_media::generators::GeneratorSource;
use filmcraft_media::{FrameRequest, Generator, MediaSource};
use filmcraft_project::{ItemKind, Label, MediaClip, MediaRef, SequenceSettings, TrackKind};
use filmcraft_render::SourceMap;
use filmcraft_time::TICKS_PER_SECOND;

use super::*;

/// A scratch directory under the workspace `target/`, removed when dropped.
struct Scratch(PathBuf);

impl Scratch {
    fn new(name: &str) -> Self {
        let d = filmcraft_testkit::fixtures::workspace_root().join("target").join("export-tests").join(format!("{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        Scratch(d)
    }
    fn path(&self, name: &str) -> String {
        self.0.join(name).to_string_lossy().to_string()
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// A `w`×`h` 24 fps sequence: a colour matte (1 s) and optionally a 440 Hz tone at `tone_db`.
fn matte(color: [f32; 4], w: u32, h: u32, tone_db: Option<f32>) -> (Arc<Project>, ItemId, SourceMap) {
    let mut p = Project::new("m");
    let r = FrameRate::FPS_24;
    let dur = Tick(2 * TICKS_PER_SECOND);
    let mut m = SourceMap::default();
    let mut add = |p: &mut Project, g: GeneratorSource| {
        let info = g.info().clone();
        let id = p.add_item(
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
        m.0.insert(id, Arc::new(g) as Arc<dyn MediaSource>);
        id
    };
    let v = add(&mut p, GeneratorSource::new(Generator::ColorMatte { color }, w, h, r, dur));
    let a = tone_db.map(|db| add(&mut p, GeneratorSource::new(Generator::Tone { hz: 440.0, db }, w, h, r, dur)));
    let seq = p.new_sequence("s", SequenceSettings { width: w, height: h, frame_rate: r, ..Default::default() }, 1, 1, None);
    let vi = p.make_track_item(v, TrackKind::Video, Tick::ZERO, TimeRange::new(Tick::ZERO, r.tick_of(24)), r).unwrap();
    p.sequence_mut(seq).unwrap().video_tracks[0].items.push(vi);
    if let Some(a) = a {
        let ai = p.make_track_item(a, TrackKind::Audio, Tick::ZERO, TimeRange::new(Tick::ZERO, r.tick_of(24)), r).unwrap();
        p.sequence_mut(seq).unwrap().audio_tracks[0].items.push(ai);
    }
    (Arc::new(p), seq, m)
}

fn ffprobe_json(args: &[&str], path: &str) -> Option<serde_json::Value> {
    let ffprobe = filmcraft_testkit::ffprobe_or_skip("export settings")?;
    let mut a = vec!["-v", "error", "-of", "json"];
    a.extend_from_slice(args);
    a.push(path);
    let out = std::process::Command::new(ffprobe).args(&a).output().unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    Some(serde_json::from_slice(&out.stdout).unwrap())
}

fn files_in(dir: &Path) -> Vec<String> {
    let mut v: Vec<String> = std::fs::read_dir(dir).unwrap().flatten().map(|e| e.file_name().to_string_lossy().to_string()).collect();
    v.sort();
    v
}

#[test]
fn image_sequences_are_numbered_like_premiere() {
    let (p, seq, m) = matte([1.0, 0.0, 0.0, 1.0], 320, 180, None);
    for (fmt, ext) in [(Format::PngSequence, "png"), (Format::TiffSequence, "tif"), (Format::BmpSequence, "bmp")] {
        let dir = Scratch::new(&format!("seq-{ext}"));
        let mut s = ExportSettings { format: fmt, path: dir.path(&format!("Shot 01.{ext}")), scale: 0.5, ..Default::default() };
        s.range = Some(TimeRange::new(Tick::ZERO, FrameRate::FPS_24.tick_of(12)));
        let prog = Progress::default();
        let r = export(&p, seq, &s, &m, &prog).unwrap();
        assert_eq!(r.frames, 12);
        assert_eq!(prog.done.load(Ordering::Relaxed), 12);
        let want: Vec<String> = (0..12).map(|i| format!("Shot 01{i:03}.{ext}")).collect();
        assert_eq!(files_in(&dir.0), want, "{fmt:?}");
        let img = image::open(dir.0.join(&want[5])).unwrap().to_rgb8();
        assert_eq!((img.width(), img.height()), (160, 90));
        let px = img.get_pixel(80, 45).0;
        assert!(px[0] > 240 && px[1] < 15, "{fmt:?} {px:?}");
    }
}

/// `alpha` keeps straight alpha in PNG and TIFF sequences; off (the default) flattens over black (#160).
#[test]
fn image_sequences_keep_alpha_when_asked() {
    let (p, seq, m) = matte([0.0, 0.0, 1.0, 0.5], 64, 36, None);
    for (fmt, ext) in [(Format::PngSequence, "png"), (Format::TiffSequence, "tif")] {
        for alpha in [false, true] {
            let dir = Scratch::new(&format!("alpha-{ext}-{alpha}"));
            let mut s = ExportSettings { format: fmt, path: dir.path(&format!("a.{ext}")), alpha, ..Default::default() };
            s.range = Some(TimeRange::new(Tick::ZERO, FrameRate::FPS_24.tick_of(2)));
            export(&p, seq, &s, &m, &Progress::default()).unwrap();
            let img = image::open(dir.0.join(format!("a000.{ext}"))).unwrap();
            let px = img.to_rgba8().get_pixel(32, 18).0;
            if alpha {
                assert!(img.color().has_alpha(), "{fmt:?} must be written with an alpha channel");
                assert!((100..=160).contains(&px[3]) && px[2] > 200 && px[0] < 15, "{fmt:?} straight alpha: {px:?}");
            } else {
                assert!(px[3] == 255 && px[2] > 100 && px[2] < 240 && px[0] < 15, "{fmt:?} flattened over black: {px:?}");
            }
        }
    }
}

#[test]
fn wav_and_aiff_audio_only() {
    let (p, seq, m) = matte([0.0, 0.0, 0.0, 1.0], 64, 36, Some(-6.0));
    let dir = Scratch::new("audio");
    // WAV, 24-bit mono at 44.1 kHz
    let wav = dir.path("a.wav");
    let mut s = ExportSettings { format: Format::Wav, path: wav.clone(), ..Default::default() };
    s.audio = AudioSettings { sample_rate: Some(44_100), channels: 1, bits: 24, ..Default::default() };
    s.range = Some(TimeRange::new(Tick::ZERO, Tick(TICKS_PER_SECOND / 2)));
    export(&p, seq, &s, &m, &Progress::default()).unwrap();
    let b = std::fs::read(&wav).unwrap();
    assert_eq!(u16::from_le_bytes([b[22], b[23]]), 1, "mono");
    assert_eq!(u32::from_le_bytes(b[24..28].try_into().unwrap()), 44_100);
    assert_eq!(u16::from_le_bytes([b[34], b[35]]), 24);
    assert_eq!(u32::from_le_bytes(b[40..44].try_into().unwrap()), 22_050 * 3, "0.5 s of 24-bit mono");
    // AIFF, 16-bit stereo at the sequence rate
    let aif = dir.path("a.aif");
    let mut s = ExportSettings { format: Format::Aiff, path: aif.clone(), ..Default::default() };
    s.range = Some(TimeRange::new(Tick::ZERO, Tick(TICKS_PER_SECOND / 2)));
    export(&p, seq, &s, &m, &Progress::default()).unwrap();
    let b = std::fs::read(&aif).unwrap();
    assert_eq!(&b[..4], b"FORM");
    assert_eq!(&b[8..12], b"AIFF");
    assert_eq!(u16::from_be_bytes([b[20], b[21]]), 2);
    assert_eq!(u32::from_be_bytes(b[22..26].try_into().unwrap()), 24_000);
    if let Some(j) = ffprobe_json(&["-show_streams"], &aif) {
        let st = &j["streams"][0];
        assert_eq!(st["codec_name"], "pcm_s16be");
        assert_eq!(st["sample_rate"], "48000");
        assert_eq!(st["channels"], 2);
    }
    if let Some(j) = ffprobe_json(&["-show_streams"], &wav) {
        assert_eq!(j["streams"][0]["codec_name"], "pcm_s24le");
    }
}

#[test]
fn invalid_export_allocations_are_rejected_before_rendering() {
    let (project, sequence, _) = matte([1.0, 0.0, 0.0, 1.0], 64, 36, None);
    for size in [(0, 36), (64, 0), (u32::MAX, u32::MAX), (32768, 16384), (40000, 16)] {
        let settings = ExportSettings { frame_size: Some(size), ..Default::default() };
        assert!(settings.validate().is_err(), "{size:?}");
        assert!(pipeline::Pipeline::new(project.clone(), sequence, &settings, false).is_err());
    }
    for size in [(15360, 8640), (16384, 8192)] {
        assert!(ExportSettings { frame_size: Some(size), ..Default::default() }.validate().is_ok(), "{size:?} is a real output size");
    }
    for scale in [0.0, -1.0, f32::NAN, f32::INFINITY, 1e30] {
        let settings = ExportSettings { scale, ..Default::default() };
        assert!(pipeline::Pipeline::new(project.clone(), sequence, &settings, false).is_err(), "scale {scale}");
    }
    for rate in [0, 384_001, u32::MAX] {
        let settings = ExportSettings { audio: AudioSettings { sample_rate: Some(rate), ..Default::default() }, ..Default::default() };
        assert!(settings.validate().is_err());
    }
}

#[test]
fn hostile_export_ranges_are_rejected_before_time_arithmetic() {
    let (project, seq, sources) = matte([1.0, 0.0, 0.0, 1.0], 64, 36, None);
    for range in [
        TimeRange::new(Tick(i64::MIN), Tick(i64::MAX)),
        TimeRange::new(Tick(i64::MAX), Tick(1)),
        TimeRange::new(Tick::ZERO, Tick(-1)),
        TimeRange::new(Tick::ZERO, Tick::ZERO),
    ] {
        let settings = ExportSettings { range: Some(range), ..Default::default() };
        assert!(settings.validate().is_err());
        assert!(export_range(&project, seq, &settings).is_err());
        assert!(export(&project, seq, &settings, &sources, &Progress::default()).is_err());
    }
}

#[test]
fn frame_size_rate_and_scaling() {
    let (p, seq, m) = matte([1.0, 0.0, 0.0, 1.0], 320, 180, Some(-12.0));
    let dir = Scratch::new("size");
    let path = dir.path("square.mp4");
    let s = ExportSettings {
        format: Format::H264,
        path: path.clone(),
        frame_size: Some((256, 256)),
        frame_rate: Some(FrameRate::new(12, 1)),
        scaling: Scaling::ScaleToFit,
        ..Default::default()
    };
    let r = export(&p, seq, &s, &m, &Progress::default()).unwrap();
    assert_eq!(r.frames, 12, "1 s at 12 fps");
    let bytes: Arc<[u8]> = std::fs::read(&path).unwrap().into();
    let src = filmcraft_codecs::open_bytes("square.mp4", bytes).unwrap();
    let v = src.info().video.clone().unwrap();
    assert_eq!((v.width, v.height), (256, 256));
    let f = src.video_frame(FrameRequest::full(Tick(TICKS_PER_SECOND / 2))).unwrap().to_rgba8();
    let at = |x: usize, y: usize| &f[(y * 256 + x) * 4..(y * 256 + x) * 4 + 3];
    assert!(at(128, 128)[0] > 200, "picture in the middle: {:?}", at(128, 128));
    assert!(at(128, 10)[0] < 40, "letterbox bar on top: {:?}", at(128, 10));
    assert!(at(128, 245)[0] < 40, "letterbox bar below: {:?}", at(128, 245));
    if let Some(j) = ffprobe_json(&["-count_frames", "-show_streams", "-select_streams", "v:0"], &path) {
        let st = &j["streams"][0];
        assert_eq!(st["r_frame_rate"], "12/1");
        assert_eq!(st["nb_read_frames"], "12");
        assert_eq!(st["width"], 256);
    }
    // fill crops instead of letterboxing
    let path = dir.path("fill.mov");
    let s = ExportSettings {
        format: Format::ProRes,
        path: path.clone(),
        frame_size: Some((200, 200)),
        scaling: Scaling::ScaleToFill,
        include_audio: false,
        ..Default::default()
    };
    export(&p, seq, &s, &m, &Progress::default()).unwrap();
    let bytes: Arc<[u8]> = std::fs::read(&path).unwrap().into();
    let src = filmcraft_codecs::open_bytes("fill.mov", bytes).unwrap();
    let f = src.video_frame(FrameRequest::full(Tick(TICKS_PER_SECOND / 2))).unwrap().to_rgba8();
    assert!(f[(5 * 200 + 100) * 4] > 200, "filled to the top edge");
}

/// Integrated loudness of planar audio.
fn lufs(planar: &[Vec<f32>], sr: u32) -> (f64, f64) {
    let refs: Vec<&[f32]> = planar.iter().map(Vec::as_slice).collect();
    let s = filmcraft_audio_dsp::loudness::measure(&refs, sr as f64);
    (s.integrated_lufs, s.true_peak_dbtp)
}

fn read_wav16(path: &str) -> Vec<Vec<f32>> {
    let b = std::fs::read(path).unwrap();
    let ch = u16::from_le_bytes([b[22], b[23]]) as usize;
    let data = &b[44..];
    let mut out = vec![Vec::new(); ch];
    for (i, s) in data.as_chunks::<2>().0.iter().enumerate() {
        out[i % ch].push(i16::from_le_bytes([s[0], s[1]]) as f32 / 32768.0);
    }
    out
}

#[test]
fn loudness_normalization_hits_target_and_true_peak() {
    let (p, seq, m) = matte([0.0, 0.0, 0.0, 1.0], 64, 36, Some(-6.0));
    let dir = Scratch::new("loud");
    for target in [-23.0, -16.0, -14.0] {
        let path = dir.path(&format!("n{}.wav", -target as i32));
        let mut s = ExportSettings { format: Format::Wav, path: path.clone(), ..Default::default() };
        s.effects.loudness = LoudnessNormalization { enabled: true, target_lufs: target, true_peak_dbtp: -1.0 };
        let prog = Progress::default();
        export(&p, seq, &s, &m, &prog).unwrap();
        let (i, tp) = lufs(&read_wav16(&path), 48_000);
        assert!((i - target).abs() < 0.5, "target {target}: measured {i:.2} LUFS");
        assert!(tp <= -0.9, "true peak {tp:.2} dBTP");
        let rep = prog.loudness.lock().unwrap().unwrap();
        assert!((rep.measured_lufs - (target - rep.gain_db)).abs() < 1e-6);
    }
    // a target the peaks cannot reach: the limiter holds the true-peak ceiling
    let path = dir.path("hot.wav");
    let mut s = ExportSettings { format: Format::Wav, path: path.clone(), ..Default::default() };
    s.effects.loudness = LoudnessNormalization { enabled: true, target_lufs: -3.0, true_peak_dbtp: -3.0 };
    export(&p, seq, &s, &m, &Progress::default()).unwrap();
    let (_, tp) = lufs(&read_wav16(&path), 48_000);
    assert!(tp <= -2.8, "limited true peak {tp:.2} dBTP");
    // and through the stepped H.264 + AAC path, decoded by our AAC decoder
    let path = dir.path("loud.mp4");
    let mut s = ExportSettings { format: Format::H264, path: path.clone(), ..Default::default() };
    s.effects.loudness = LoudnessNormalization { enabled: true, target_lufs: -20.0, true_peak_dbtp: -1.0 };
    export(&p, seq, &s, &m, &Progress::default()).unwrap();
    let bytes: Arc<[u8]> = std::fs::read(&path).unwrap().into();
    let src = filmcraft_codecs::open_bytes("loud.mp4", bytes).unwrap();
    let a = src.audio(4800, 38_400, 48_000).unwrap();
    let (i, _) = lufs(&a.channels, 48_000);
    assert!((i + 20.0).abs() < 0.5, "AAC: {i:.2} LUFS");
}

#[test]
fn two_pass_and_cbr_h264() {
    let (p, seq, m) = matte([0.2, 0.5, 0.8, 1.0], 320, 180, None);
    let dir = Scratch::new("2pass");
    for mode in [BitrateMode::Vbr2Pass, BitrateMode::Cbr] {
        let path = dir.path(&format!("{mode:?}.mp4"));
        let s = ExportSettings { format: Format::H264, path: path.clone(), bitrate_mode: mode, bitrate_kbps: 2000, include_audio: false, ..Default::default() };
        let prog = Progress::default();
        let r = export(&p, seq, &s, &m, &prog).unwrap();
        assert_eq!(r.frames, 24);
        let passes = if mode == BitrateMode::Vbr2Pass { 2 } else { 1 };
        assert_eq!(prog.total.load(Ordering::Relaxed), 24 * passes);
        assert_eq!(prog.done.load(Ordering::Relaxed), 24 * passes);
        let bytes: Arc<[u8]> = std::fs::read(&path).unwrap().into();
        let src = filmcraft_codecs::open_bytes("x.mp4", bytes).unwrap();
        let f = src.video_frame(FrameRequest::full(Tick(TICKS_PER_SECOND / 2))).unwrap().to_rgba8();
        assert!((f[2] as i32 - 204).abs() < 12 && (f[1] as i32 - 127).abs() < 12, "{mode:?}: {:?}", &f[..4]);
    }
}

#[test]
fn h264_profile_level_and_keyframes() {
    let (p, seq, m) = matte([0.5, 0.5, 0.5, 1.0], 320, 180, None);
    let dir = Scratch::new("profile");
    let path = dir.path("main.mp4");
    let s = ExportSettings {
        format: Format::H264,
        path: path.clone(),
        h264_profile: H264Profile::Main,
        h264_level: Some(51),
        keyframe_distance: Some(6),
        include_audio: false,
        ..Default::default()
    };
    export(&p, seq, &s, &m, &Progress::default()).unwrap();
    if let Some(j) = ffprobe_json(&["-show_streams"], &path) {
        assert_eq!(j["streams"][0]["profile"], "Main");
        assert_eq!(j["streams"][0]["level"], 51);
    }
    if let Some(j) = ffprobe_json(&["-show_frames", "-select_streams", "v:0", "-show_entries", "frame=key_frame"], &path) {
        let keys = j["frames"].as_array().unwrap().iter().filter(|f| f["key_frame"] == 1).count();
        assert_eq!(keys, 4, "24 frames, a keyframe every 6");
    }
}

#[test]
fn video_limiter_clamps_luma() {
    let (p, seq, m) = matte([1.0, 1.0, 1.0, 1.0], 64, 36, None);
    let dir = Scratch::new("limiter");
    let mut s = ExportSettings { format: Format::PngSequence, path: dir.path("w.png"), ..Default::default() };
    s.range = Some(TimeRange::new(Tick::ZERO, FrameRate::FPS_24.tick_of(1)));
    s.effects.video_limiter = VideoLimiter { enabled: true, min_percent: 0.0, max_percent: 80.0 };
    export(&p, seq, &s, &m, &Progress::default()).unwrap();
    let img = image::open(dir.0.join("w000.png")).unwrap().to_rgb8();
    let mx = img.pixels().flat_map(|p| p.0).max().unwrap();
    assert!((200..=204).contains(&mx), "white limited to 80 %: {mx}");
    let mut px = vec![255u8, 255, 255, 255, 0, 0, 0, 255];
    limit_rgba8(&mut px, 10.0, 90.0);
    assert!(px[0] <= 230 && px[4] >= 25, "{px:?}");
}

#[test]
fn overlays_draw_where_placed() {
    let (p, seq, m) = matte([0.0, 0.0, 0.0, 1.0], 320, 180, None);
    let dir = Scratch::new("overlay");
    // a 10×10 green PNG as the image overlay
    let logo = dir.path("logo.png");
    image::RgbaImage::from_pixel(10, 10, image::Rgba([0, 255, 0, 255])).save(&logo).unwrap();
    let mut s = ExportSettings { format: Format::PngSequence, path: dir.path("o.png"), ..Default::default() };
    s.range = Some(TimeRange::new(Tick::ZERO, FrameRate::FPS_24.tick_of(1)));
    s.effects.image_overlay = ImageOverlay { enabled: true, path: logo, placement: Placement::TopRight, size_percent: 10.0, ..Default::default() };
    s.effects.name_overlay =
        TextOverlay { enabled: true, text: "CLIENT REVIEW".into(), placement: Placement::BottomCenter, size_percent: 8.0, ..Default::default() };
    s.effects.timecode_overlay = TextOverlay { enabled: true, placement: Placement::TopLeft, size_percent: 8.0, ..Default::default() };
    export(&p, seq, &s, &m, &Progress::default()).unwrap();
    let img = image::open(dir.0.join("o000.png")).unwrap().to_rgb8();
    let count = |x0: u32, x1: u32, y0: u32, y1: u32, f: &dyn Fn([u8; 3]) -> bool| {
        (y0..y1).flat_map(|y| (x0..x1).map(move |x| (x, y))).filter(|&(x, y)| f(img.get_pixel(x, y).0)).count()
    };
    let green = |p: [u8; 3]| p[1] > 200 && p[0] < 60;
    let white = |p: [u8; 3]| p[0] > 200 && p[1] > 200 && p[2] > 200;
    assert!(count(260, 320, 0, 60, &green) > 200, "logo top right");
    assert_eq!(count(0, 160, 0, 180, &green), 0, "no logo on the left");
    assert!(count(60, 260, 140, 180, &white) > 50, "name bottom centre");
    assert!(count(0, 160, 0, 40, &white) > 50, "timecode top left");
    assert_eq!(count(0, 320, 70, 110, &white), 0, "nothing in the middle");
}

#[test]
fn metadata_and_quicktime_multiplexer() {
    let (p, seq, m) = matte([0.0, 0.0, 1.0, 1.0], 160, 90, Some(-12.0));
    let dir = Scratch::new("meta");
    let path = dir.path("m.mov");
    let mut s = ExportSettings { format: Format::H264, path: path.clone(), multiplexer: Multiplexer::Mov, ..Default::default() };
    s.metadata = ExportMetadata { title: "Test Title".into(), creator: "FilmCraft Tests".into(), copyright: "CC0".into(), ..Default::default() };
    s.audio.codec = AudioCodec::Pcm;
    s.audio.bits = 24;
    assert_eq!(s.extension(), "mov");
    export(&p, seq, &s, &m, &Progress::default()).unwrap();
    if let Some(j) = ffprobe_json(&["-show_format", "-show_streams"], &path) {
        assert!(j["format"]["format_name"].as_str().unwrap().contains("mov"));
        let tags = &j["format"]["tags"];
        assert_eq!(tags["title"], "Test Title");
        assert_eq!(tags["copyright"], "CC0");
        let codecs: Vec<&str> = j["streams"].as_array().unwrap().iter().map(|s| s["codec_name"].as_str().unwrap()).collect();
        assert_eq!(codecs, ["h264", "pcm_s24le"]);
    }
}

#[test]
fn interlaced_output_is_rejected_honestly() {
    let (p, seq, m) = matte([0.0, 0.0, 1.0, 1.0], 160, 90, None);
    let s = ExportSettings { format: Format::ProRes, path: "unused.mov".into(), field_order: FieldOrder::UpperFirst, ..Default::default() };
    let e = export(&p, seq, &s, &m, &Progress::default()).unwrap_err();
    assert!(e.to_string().contains("progressive"), "{e}");
}

#[test]
fn settings_serde_and_estimates() {
    let s: ExportSettings = serde_json::from_value(serde_json::json!({"format": "prores", "frameSize": [1280, 720], "proresProfile": "lt"})).unwrap();
    assert_eq!(s.format, Format::ProRes);
    assert_eq!(s.frame_size, Some((1280, 720)));
    assert!(s.include_audio, "defaults fill the rest");
    let back: ExportSettings = serde_json::from_value(serde_json::to_value(&s).unwrap()).unwrap();
    assert_eq!(back.prores_profile, "lt");
    // 10 s of 20 Mbps H.264 + 320 kbps AAC ≈ 25.4 MB
    let h = ExportSettings::default();
    let b = h.estimate_bytes(1920, 1080, FrameRate::FPS_25, 48_000, Tick(10 * TICKS_PER_SECOND));
    assert!((b as f64 - 25.4e6).abs() < 0.2e6, "{b}");
    assert_eq!(format_bytes(b), "25 MB");
    // adaptive bitrate scales with the frame size
    let a = ExportSettings { adaptive_bitrate: Some(0.2), ..Default::default() };
    assert_eq!(a.resolve(1920, 1080, FrameRate::FPS_30, 48_000).target_kbps, 12_442);
    assert_eq!(a.resolve(3840, 2160, FrameRate::FPS_30, 48_000).target_kbps, 49_766);
}

#[test]
fn builtin_presets_are_valid_and_unique() {
    let all = builtin_presets();
    assert!(all.len() >= 24);
    let mut keys: Vec<String> = all.iter().map(|p| presets::preset_key(&p.name)).collect();
    keys.sort();
    keys.dedup();
    assert_eq!(keys.len(), all.len(), "unique names");
    for p in &all {
        p.settings.validate().unwrap();
        assert!(p.builtin && !p.category.is_empty() && !p.description.is_empty(), "{}", p.name);
    }
    assert_eq!(presets::find_builtin("match source - adaptive high bitrate").unwrap().name, presets::DEFAULT_PRESET);
    let json = serde_json::to_value(&all[0]).unwrap();
    let back: ExportPreset = serde_json::from_value(json).unwrap();
    assert_eq!(back.settings.adaptive_bitrate, Some(0.2));
}

/// #108: the default 320 kbps is more than AAC can carry in stereo at 22.05 kHz (6 bits per sample
/// and channel). The export failed with "bitrate out of range"; it is now capped to the limit.
#[test]
fn aac_bitrate_is_capped_for_low_sample_rates() {
    let mut s = ExportSettings::default();
    s.audio.bitrate_kbps = 320;
    for (rate, ch) in [(22_050, 2), (16_000, 2), (8_000, 6), (48_000, 2)] {
        let enc = aac_factory(Format::H264, rate, ch, &s).expect("AAC is always available");
        assert!(enc.is_ok(), "{rate} Hz × {ch}: {:?}", enc.err());
    }
}

#[test]
fn a_time_left_reads_like_a_clock() {
    use std::time::Duration;
    let f = |ms: u64| format_eta(Duration::from_millis(ms));
    assert_eq!(f(0), "0 s");
    assert_eq!(f(1), "1 s", "rounded up: never say 0 s while there is time left");
    assert_eq!(f(45_000), "45 s");
    assert_eq!(f(59_001), "1:00");
    assert_eq!(f(60_000), "1:00");
    assert_eq!(f(125_000), "2:05");
    assert_eq!(f(3_599_000), "59:59");
    assert_eq!(f(3_600_000), "1:00:00");
    assert_eq!(f(3_725_000), "1:02:05");
    assert_eq!(f(100 * 3600 * 1000), "100:00:00");
    assert!(!format_eta(Duration::MAX).is_empty(), "the largest duration cannot overflow");
}

#[test]
fn h265_is_a_format_that_needs_a_registered_encoder() {
    use std::sync::atomic::{AtomicBool, Ordering};
    // names, ids, files and serialized settings
    for name in ["hevc", "H.265", "h265", "HVC1", "x265"] {
        assert_eq!(Format::from_name(name), Some(Format::Hevc), "{name}");
    }
    assert_eq!(Format::from_name(Format::Hevc.id()), Some(Format::Hevc));
    assert_eq!((Format::Hevc.extension(), Format::Hevc.label()), ("mp4", "H.265 (HEVC)"));
    assert!(Format::ALL.contains(&Format::Hevc) && Format::ALL.len() == 14);
    let s: ExportSettings = serde_json::from_value(serde_json::json!({"format": "hevc"})).unwrap();
    assert_eq!(s.format, Format::Hevc);
    assert_eq!(serde_json::to_value(&s).unwrap()["format"], "hevc");

    // it is H.264's family: MP4 with AAC, or QuickTime; the same size estimate
    let h = ExportSettings { format: Format::Hevc, ..Default::default() };
    assert_eq!((h.audio_codec(), h.extension()), (AudioCodec::Aac, "mp4"));
    assert_eq!(ExportSettings { multiplexer: Multiplexer::Mov, ..h.clone() }.extension(), "mov");
    // 10 s of 20 Mbps video + 320 kbps AAC ≈ 25.4 MB, like H.264
    let b = h.estimate_bytes(1920, 1080, FrameRate::FPS_25, 48_000, Tick(10 * TICKS_PER_SECOND));
    assert!((b as f64 - 25.4e6).abs() < 0.2e6, "{b}");
    let summary = h.summary(1920, 1080, FrameRate::FPS_25, 48_000, Tick(10 * TICKS_PER_SECOND));
    assert_eq!(summary.format, "H.265 (HEVC) (MP4)");
    assert!(summary.video.contains("HEVC Main") && summary.video.contains("Target 20.00 Mbps"), "{}", summary.video);

    // a hardware encoder has one pass
    let e = ExportSettings { bitrate_mode: BitrateMode::Vbr2Pass, ..h.clone() }.validate().unwrap_err();
    assert!(e.to_string().contains("two-pass"), "{e}");
    assert!(ExportSettings { bitrate_mode: BitrateMode::Cbr, ..h.clone() }.validate().is_ok());

    // formats with a built-in encoder are always available; this one only when a registered probe says so
    assert!(Format::ALL.iter().filter(|f| f.has_builtin_encoder()).all(|f| available(*f)));
    assert!(!Format::Hevc.has_builtin_encoder());
    static HERE: AtomicBool = AtomicBool::new(false);
    fn probe() -> bool {
        HERE.load(Ordering::SeqCst)
    }
    assert!(!available(Format::Hevc), "nothing registered an encoder for it");
    // and an export says so, honestly
    let (p, seq, m) = matte([0.0, 0.0, 1.0, 1.0], 160, 90, None);
    let sc = Scratch::new("h265-missing");
    let err = export(&p, seq, &ExportSettings { path: sc.path("x.mp4"), include_audio: false, ..h.clone() }, &m, &Progress::default()).unwrap_err().to_string();
    assert!(err.contains("H.265") && err.contains("encoder not available"), "{err}");
    register_format_probe(Format::Hevc, probe);
    register_format_probe(Format::Hevc, probe); // the same probe twice is harmless
    assert!(!available(Format::Hevc));
    HERE.store(true, Ordering::SeqCst);
    assert!(available(Format::Hevc));
    HERE.store(false, Ordering::SeqCst);
    assert!(!available(Format::Hevc));
}

#[test]
fn extreme_bitrates_do_not_overflow_resolution() {
    let s = ExportSettings { bitrate_kbps: u32::MAX, adaptive_bitrate: None, ..Default::default() };
    let resolved = s.resolve(1920, 1080, FrameRate::FPS_30, 48_000);
    assert_eq!(resolved.target_kbps, u32::MAX);
    assert_eq!(resolved.max_kbps, u32::MAX);
}

#[test]
fn extreme_bitrates_do_not_overflow_encoder_setup() {
    for max_bitrate_kbps in [None, Some(u32::MAX)] {
        let settings = ExportSettings { bitrate_kbps: u32::MAX, max_bitrate_kbps, ..Default::default() };
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| super::h264_factory(Format::H264, 64, 36, FrameRate::FPS_24, &settings)));
        assert!(result.is_ok(), "an extreme bitrate must return an encoder result without overflowing its fallback");
        assert!(result.unwrap().unwrap().is_err(), "an unrepresentable encoder buffer rate must be rejected");
    }
}

#[test]
fn gpu_rendering_is_off_by_default() {
    assert_eq!(ExportSettings::default().gpu_rendering, crate::GpuRendering::Off);
    // settings saved before the field existed get the default too
    let s: ExportSettings = serde_json::from_value(serde_json::json!({"format": "h264"})).unwrap();
    assert_eq!(s.gpu_rendering, crate::GpuRendering::Off);
    let auto: ExportSettings = serde_json::from_value(serde_json::json!({"format": "h264", "gpuRendering": "auto"})).unwrap();
    assert_eq!(auto.gpu_rendering, crate::GpuRendering::Auto);
}
