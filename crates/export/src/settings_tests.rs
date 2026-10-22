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
pub(super) struct Scratch(pub(super) PathBuf);

impl Scratch {
    pub(super) fn new(name: &str) -> Self {
        let d = filmcraft_testkit::fixtures::workspace_root().join("target").join("export-tests").join(format!("{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        Scratch(d)
    }
    pub(super) fn path(&self, name: &str) -> String {
        self.0.join(name).to_string_lossy().to_string()
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// A `w`×`h` 24 fps sequence: a colour matte (1 s) and optionally a 440 Hz tone at `tone_db`.
pub(super) fn matte(color: [f32; 4], w: u32, h: u32, tone_db: Option<f32>) -> (Arc<Project>, ItemId, SourceMap) {
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

pub(super) fn ffprobe_json(args: &[&str], path: &str) -> Option<serde_json::Value> {
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

/// JPEG, Targa and DPX sequences (#779): numbered like the others, JPEG at the export quality,
/// Targa with alpha when asked; every still decoded by ffmpeg (external test oracle).
#[test]
fn jpeg_targa_and_dpx_sequences() {
    let (p, seq, m) = matte([0.0, 0.0, 1.0, 0.5], 64, 36, None);
    let mut jpeg_sizes = Vec::new();
    for (fmt, ext, alpha, quality) in [
        (Format::JpegSequence, "jpg", false, 95),
        (Format::JpegSequence, "jpg", false, 20),
        (Format::TgaSequence, "tga", false, 90),
        (Format::TgaSequence, "tga", true, 90),
        (Format::DpxSequence, "dpx", false, 90),
    ] {
        let dir = Scratch::new(&format!("seq3-{ext}-{alpha}-{quality}"));
        let mut s = ExportSettings { format: fmt, path: dir.path(&format!("Shot.{ext}")), alpha, quality, ..Default::default() };
        s.range = Some(TimeRange::new(Tick::ZERO, FrameRate::FPS_24.tick_of(3)));
        assert_eq!(Format::from_name(fmt.id()), Some(fmt));
        assert!(fmt.is_image_sequence() && s.is_image_sequence() && !s.has_audio());
        let r = export(&p, seq, &s, &m, &Progress::default()).unwrap();
        assert_eq!(r.frames, 3);
        let want: Vec<String> = (0..3).map(|i| format!("Shot{i:03}.{ext}")).collect();
        assert_eq!(files_in(&dir.0), want, "{fmt:?}");
        let first = dir.0.join(&want[0]);
        let bytes = std::fs::read(&first).unwrap();
        match fmt {
            Format::JpegSequence => {
                let px = image::load_from_memory(&bytes).unwrap().to_rgb8().get_pixel(32, 18).0;
                assert!(px[2] > 100 && px[0] < 20, "{px:?}");
                jpeg_sizes.push(bytes.len());
            }
            Format::DpxSequence => assert_eq!(bytes.len(), 2048 + 64 * 36 * 4),
            _ => assert_eq!(bytes.len(), 18 + 64 * 36 * if alpha { 4 } else { 3 } + 26),
        }
        let Some(ffmpeg) = filmcraft_testkit::ffmpeg_or_skip("jpeg / targa / dpx sequences") else { continue };
        let first = first.to_string_lossy().to_string();
        let out = std::process::Command::new(ffmpeg).args(["-v", "error", "-i", &first, "-f", "rawvideo", "-pix_fmt", "rgba", "-"]).output().unwrap();
        assert!(out.status.success(), "{first}: {}", String::from_utf8_lossy(&out.stderr));
        assert_eq!(out.stdout.len(), 64 * 36 * 4, "{first}");
        let at = (18 * 64 + 32) * 4;
        let px = &out.stdout[at..at + 4];
        if alpha {
            assert!((100..=160).contains(&px[3]) && px[2] > 200 && px[0] < 15, "{first} straight alpha: {px:?}");
        } else {
            assert!(px[3] == 255 && px[2] > 100 && px[2] < 240 && px[0] < 20, "{first} flattened over black: {px:?}");
        }
        // the top-left pixel is the top row (Targa is stored bottom-up), and all of it is blue
        assert!(out.stdout.as_chunks::<4>().0.iter().all(|p| p[2] > 100 && p[0] < 20), "{first}");
    }
    assert!(jpeg_sizes[0] > jpeg_sizes[1], "quality 95 must be larger than quality 20: {jpeg_sizes:?}");
}

/// OpenEXR sequences (#806): numbered like the others, half-float linear light, RGBA with
/// premultiplied alpha when asked; ffprobe / ffmpeg (external test oracles) read every still.
#[test]
fn openexr_sequences() {
    let (p, seq, m) = matte([0.0, 0.0, 1.0, 0.5], 64, 36, None);
    for alpha in [false, true] {
        let dir = Scratch::new(&format!("seq-exr-{alpha}"));
        let mut s = ExportSettings { format: Format::ExrSequence, path: dir.path("Shot.exr"), alpha, ..Default::default() };
        s.range = Some(TimeRange::new(Tick::ZERO, FrameRate::FPS_24.tick_of(3)));
        assert_eq!(Format::from_name("OpenEXR"), Some(Format::ExrSequence));
        assert!(s.is_image_sequence() && s.supports_alpha() && s.keeps_alpha() == alpha && !s.has_audio());
        let r = export(&p, seq, &s, &m, &Progress::default()).unwrap();
        assert_eq!(r.frames, 3);
        let want: Vec<String> = (0..3).map(|i| format!("Shot{i:03}.exr")).collect();
        assert_eq!(files_in(&dir.0), want);
        let first = dir.0.join(&want[0]);
        let bytes = std::fs::read(&first).unwrap();
        assert_eq!(&bytes[..4], &[0x76, 0x2F, 0x31, 0x01]);
        let Some(ffmpeg) = filmcraft_testkit::ffmpeg_or_skip("OpenEXR sequences") else { continue };
        let out = std::process::Command::new(ffmpeg)
            .args(["-v", "error", "-i"])
            .arg(&first)
            .args(["-f", "rawvideo", "-pix_fmt", "gbrapf32le", "-"])
            .output()
            .unwrap();
        assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
        let n = 64 * 36;
        assert_eq!(out.stdout.len(), n * 16);
        let plane = |p: usize| -> Vec<f32> { out.stdout[p * n * 4..(p + 1) * n * 4].as_chunks::<4>().0.iter().map(|b| f32::from_le_bytes(*b)).collect() };
        let (g, b, r, a) = (plane(0), plane(1), plane(2), plane(3));
        for i in 0..n {
            assert!(g[i].abs() < 1e-3 && r[i].abs() < 1e-3, "pixel {i}: no red or green");
            if alpha {
                // pure blue (linear 1.0) at 50 % opacity, premultiplied
                assert!((a[i] - 0.5).abs() < 0.01 && (b[i] - a[i]).abs() < 2e-3, "pixel {i}: {} / {}", b[i], a[i]);
            } else {
                // flattened over black, then linearised: well below the encoded half level
                assert!(a[i] == 1.0 && b[i] > 0.1 && b[i] < 0.5, "pixel {i}: {} / {}", b[i], a[i]);
            }
        }
    }
}

/// Compressed OpenEXR sequences (#874): the export setting reaches every still, which shrinks
/// and (ffmpeg as the external oracle) decodes to the same pixels as the uncompressed file.
#[test]
fn openexr_sequences_compress() {
    let (p, seq, m) = matte([0.2, 0.6, 1.0, 0.5], 64, 36, None);
    let mut files = Vec::new();
    for c in ExrCompression::ALL {
        let dir = Scratch::new(&format!("seq-exr-{c:?}"));
        let mut s = ExportSettings { format: Format::ExrSequence, path: dir.path("c.exr"), alpha: true, exr_compression: c, ..Default::default() };
        s.range = Some(TimeRange::new(Tick::ZERO, FrameRate::FPS_24.tick_of(1)));
        export(&p, seq, &s, &m, &Progress::default()).unwrap();
        let path = dir.0.join("c000.exr");
        let bytes = std::fs::read(&path).unwrap();
        let at = bytes.windows(24).position(|w| w == b"compression\0compression\0").unwrap() + 28;
        assert_eq!(bytes[at], [0, 1, 2, 3][files.len()], "{c:?}");
        files.push((c, path, bytes.len(), dir));
    }
    let plain = files[0].2;
    for (c, _, len, _) in &files[1..] {
        assert!(*len < plain / 2, "{c:?}: {len} bytes vs {plain} uncompressed");
    }
    let Some(ffmpeg) = filmcraft_testkit::ffmpeg_or_skip("compressed OpenEXR sequences") else { return };
    let decode = |path: &Path| {
        let out =
            std::process::Command::new(&ffmpeg).args(["-v", "error", "-i"]).arg(path).args(["-f", "rawvideo", "-pix_fmt", "gbrapf32le", "-"]).output().unwrap();
        assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
        out.stdout
    };
    let want = decode(&files[0].1);
    assert_eq!(want.len(), 64 * 36 * 16);
    for (c, path, _, _) in &files[1..] {
        assert!(decode(path) == want, "{c:?} decodes differently");
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

/// Root-mean-square level of decoded planar audio.
fn rms(planes: &[Vec<f32>]) -> f32 {
    let n = planes.iter().map(Vec::len).sum::<usize>().max(1);
    (planes.iter().flatten().map(|s| s * s).sum::<f32>() / n as f32).sqrt()
}

#[test]
fn aac_audio_writes_m4a_and_adts() {
    let (p, seq, m) = matte([0.0, 0.0, 0.0, 1.0], 64, 36, Some(-6.0));
    let dir = Scratch::new("aac-audio");
    let range = Some(TimeRange::new(Tick::ZERO, Tick(TICKS_PER_SECOND)));
    // .m4a: one AAC track, no video, the encoder delay in an edit list
    let m4a = dir.path("a.m4a");
    let s = ExportSettings { format: Format::Aac, path: m4a.clone(), range, ..Default::default() };
    assert!(!s.has_video());
    assert_eq!(s.audio_codec(), AudioCodec::Aac);
    assert_eq!(s.extension(), "m4a");
    let r = export(&p, seq, &s, &m, &Progress::default()).unwrap();
    assert_eq!(r.frames, 48_000, "one second of samples");
    let bytes = std::fs::read(&m4a).unwrap();
    let file = filmcraft_isobmff::open(bytes.clone()).unwrap();
    assert_eq!(file.tracks.len(), 1, "audio only");
    let t = &file.tracks[0];
    let filmcraft_isobmff::CodecConfig::Aac(cfg) = &t.entries[0].codec else { panic!("not AAC: {:?}", t.entries[0].codec) };
    assert_eq!((cfg.object_type, cfg.sample_rate, cfg.channel_config), (2, 48_000, 2));
    assert!(t.samples.len() >= 48_000 / 1024, "{} access units", t.samples.len());
    let mut dec = filmcraft_aac::Decoder::new(&cfg.asc).unwrap();
    let mut pcm: Vec<Vec<f32>> = Vec::new();
    for i in 0..t.samples.len() {
        let out = dec.decode(&file.read_sample(&bytes, 0, i).unwrap()).unwrap();
        pcm.resize(out.len(), Vec::new());
        pcm.iter_mut().zip(out).for_each(|(a, b)| a.extend(b));
    }
    // the -6 dBFS tone (RMS about 0.35) survives the round trip
    assert!((0.2..0.5).contains(&rms(&pcm)), "rms {}", rms(&pcm));
    if let Some(j) = ffprobe_json(&["-show_streams", "-show_format"], &m4a) {
        assert_eq!(j["streams"].as_array().unwrap().len(), 1);
        let st = &j["streams"][0];
        assert_eq!(st["codec_name"], "aac");
        assert_eq!(st["sample_rate"], "48000");
        assert_eq!(st["channels"], 2);
        let d: f64 = j["format"]["duration"].as_str().unwrap().parse().unwrap();
        assert!((d - 1.0).abs() < 0.03, "duration {d}");
    }

    // .aac: raw ADTS, mono 44.1 kHz at 128 kbps
    let adts = dir.path("a.aac");
    let mut s = ExportSettings { format: Format::Aac, path: adts.clone(), range, ..Default::default() };
    s.audio = AudioSettings { sample_rate: Some(44_100), channels: 1, bitrate_kbps: 128, ..Default::default() };
    export(&p, seq, &s, &m, &Progress::default()).unwrap();
    let b = std::fs::read(&adts).unwrap();
    // AAC-LC 44.1 kHz (index 4) mono: 00010 0100 0001 000
    let mut dec = filmcraft_aac::Decoder::new(&[0x12, 0x08]).unwrap();
    let (mut pos, mut units, mut pcm) = (0usize, 0usize, vec![Vec::new()]);
    while pos < b.len() {
        let h = &b[pos..pos + 7];
        assert_eq!((h[0], h[1] & 0xF6), (0xFF, 0xF0), "ADTS sync at {pos}");
        assert_eq!(h[1] & 0x01, 1, "no CRC");
        assert_eq!(((h[2] >> 2) & 0x0F, ((h[2] & 1) << 2) | (h[3] >> 6)), (4, 1), "44.1 kHz mono");
        let len = ((h[3] as usize & 0x03) << 11) | ((h[4] as usize) << 3) | (h[5] as usize >> 5);
        assert!(len > 7 && pos + len <= b.len(), "frame length {len} at {pos}");
        pcm[0].extend(dec.decode(&b[pos + 7..pos + len]).unwrap().swap_remove(0));
        pos += len;
        units += 1;
    }
    assert!(units >= 44_100 / 1024, "{units} frames");
    assert!((0.2..0.5).contains(&rms(&pcm)), "rms {}", rms(&pcm));
    if let Some(j) = ffprobe_json(&["-show_streams", "-show_format"], &adts) {
        assert_eq!(j["format"]["format_name"], "aac");
        let st = &j["streams"][0];
        assert_eq!(st["codec_name"], "aac");
        assert_eq!(st["sample_rate"], "44100");
        assert_eq!(st["channels"], 1);
    }
    if let Some(ffmpeg) = filmcraft_testkit::ffmpeg_or_skip("AAC Audio decode") {
        for f in [&m4a, &adts] {
            let out = std::process::Command::new(&ffmpeg).args(["-v", "error", "-xerror", "-i", f, "-f", "null", "-"]).output().unwrap();
            assert!(out.status.success() && out.stderr.is_empty(), "{f}: {}", String::from_utf8_lossy(&out.stderr));
        }
    }
}

#[test]
fn aac_audio_settings_and_names() {
    assert_eq!(Format::from_name("m4a"), Some(Format::Aac));
    assert_eq!(Format::from_name("AAC Audio"), Some(Format::Aac));
    assert_eq!(Format::from_name(Format::Aac.id()), Some(Format::Aac));
    assert!(Format::ALL.contains(&Format::Aac) && Format::Aac.is_audio_only() && !Format::H264.is_audio_only());
    let s = ExportSettings { format: Format::Aac, include_audio: false, ..Default::default() };
    let sum = s.summary(1920, 1080, FrameRate::FPS_24, 48_000, Tick(10 * TICKS_PER_SECOND));
    assert_eq!(sum.video, "No video");
    assert_eq!(sum.audio, "AAC, 320 kbps, 48000 Hz, Stereo");
    assert_eq!(sum.estimated_bytes, 400_000, "320 kbps for 10 s, no picture");
    let preset = presets::find_builtin("AAC Audio 48 kHz 320 kbps").unwrap();
    assert_eq!(preset.settings.format, Format::Aac);
}

#[test]
fn wav_export_honours_explicit_sample_rates() {
    let (project, sequence, sources) = matte([0.0, 0.0, 0.0, 1.0], 64, 36, Some(-6.0));
    let dir = Scratch::new("wav-sample-rates");
    for rate in [1, 4000, 7350, 8000, 48_000, 192_000, 192_001, 200_000, 352_800, 384_000] {
        let settings = ExportSettings {
            format: Format::Wav,
            path: dir.path(&format!("{rate}.wav")),
            audio: AudioSettings { sample_rate: Some(rate), ..Default::default() },
            range: Some(TimeRange::new(Tick::ZERO, Tick(TICKS_PER_SECOND / 4))),
            ..Default::default()
        };
        settings.validate().unwrap();
        let resolved = settings.resolve(64, 36, FrameRate::FPS_24, 48_000);
        assert_eq!(resolved.sample_rate, rate, "requested {rate} Hz");
        export(&project, sequence, &settings, &sources, &Progress::default()).unwrap();
        let bytes = std::fs::read(&settings.path).unwrap();
        assert_eq!(u32::from_le_bytes(bytes[24..28].try_into().unwrap()), rate);
        assert_eq!(u32::from_le_bytes(bytes[40..44].try_into().unwrap()), rate / 4 * 4, "quarter-second stereo 16-bit PCM");
        if let Some(probe) = ffprobe_json(&["-show_streams"], &settings.path) {
            assert_eq!(probe["streams"][0]["sample_rate"], rate.to_string());
        }
    }
    let settings = ExportSettings { format: Format::Wav, ..Default::default() };
    assert_eq!(settings.resolve(64, 36, FrameRate::FPS_24, 44_100).sample_rate, 44_100);
}

#[test]
fn aac_export_rejects_unsupported_explicit_sample_rates() {
    let (project, sequence, sources) = matte([0.0, 0.0, 0.0, 1.0], 64, 36, Some(-6.0));
    let dir = Scratch::new("aac-sample-rates");
    for rate in [4000, 192_001, 200_000, 384_000] {
        let settings = ExportSettings {
            path: dir.path(&format!("{rate}.mp4")),
            audio: AudioSettings { sample_rate: Some(rate), ..Default::default() },
            range: Some(TimeRange::new(Tick::ZERO, Tick(TICKS_PER_SECOND / 4))),
            ..Default::default()
        };
        let error = export(&project, sequence, &settings, &sources, &Progress::default()).unwrap_err();
        assert!(error.to_string().contains("sample rate"), "{error}");
        assert!(!Path::new(&settings.path).exists(), "an unsupported rate must not produce a substitute file");
    }
}

#[test]
fn flac_is_lossless_against_wav() {
    let (p, seq, m) = matte([0.0, 0.0, 0.0, 1.0], 64, 36, Some(-6.0));
    let dir = Scratch::new("flac");
    let range = Some(TimeRange::new(Tick::ZERO, Tick(3 * TICKS_PER_SECOND / 2)));
    let audio = AudioSettings { channels: 2, bits: 24, ..Default::default() };
    let (wav, flac) = (dir.path("a.wav"), dir.path("a.flac"));
    let w = ExportSettings { format: Format::Wav, path: wav.clone(), range, audio: audio.clone(), ..Default::default() };
    export(&p, seq, &w, &m, &Progress::default()).unwrap();
    let f = ExportSettings { format: Format::Flac, path: flac.clone(), range, audio, ..Default::default() };
    let prog = Progress::default();
    let r = export(&p, seq, &f, &m, &prog).unwrap();
    assert_eq!(r.frames, 72_000, "1.5 s at 48 kHz");
    // one progress step per second of audio
    assert_eq!((prog.total.load(Ordering::Relaxed), prog.done.load(Ordering::Relaxed)), (2, 2));
    let (wb, fb) = (std::fs::read(&wav).unwrap(), std::fs::read(&flac).unwrap());
    assert_eq!(&fb[..4], b"fLaC");
    assert!(fb.len() * 2 < wb.len(), "a tone compresses: FLAC {} bytes, WAV {}", fb.len(), wb.len());
    // STREAMINFO was filled in at the end: total samples
    let si = &fb[8..42];
    assert_eq!((u64::from(si[13] & 0x0F) << 32) | u64::from(u32::from_be_bytes(si[14..18].try_into().unwrap())), 72_000);

    // the same samples through FilmCraft's own readers
    let decode = |name: &str, bytes: Vec<u8>| filmcraft_codecs::open_bytes(name, bytes.into()).unwrap().audio(0, 72_000, 48_000).unwrap();
    let (a, b) = (decode("a.wav", wb), decode("a.flac", fb));
    assert_eq!(a.channels.len(), 2);
    for (ca, cb) in a.channels.iter().zip(&b.channels) {
        assert_eq!(ca.len(), cb.len());
        let worst = ca.iter().zip(cb).map(|(x, y)| (x - y).abs()).fold(0.0f32, f32::max);
        assert!(worst < 1e-6, "differs by {worst}");
        assert!(ca.iter().any(|v| v.abs() > 0.1), "the tone is there");
    }
    // and bit for bit through ffmpeg
    if let Some(ffmpeg) = filmcraft_testkit::ffmpeg_or_skip("flac export") {
        let raw = |path: &str| {
            let out = std::process::Command::new(&ffmpeg).args(["-v", "error", "-i", path, "-f", "s32le", "-c:a", "pcm_s32le", "-"]).output().unwrap();
            assert!(out.status.success() && out.stderr.is_empty(), "{}", String::from_utf8_lossy(&out.stderr));
            out.stdout
        };
        let (a, b) = (raw(&wav), raw(&flac));
        assert_eq!(a.len(), 72_000 * 2 * 4);
        assert!(a == b, "FLAC must decode to the WAV's samples exactly");
    }
    if let Some(j) = ffprobe_json(&["-show_streams"], &flac) {
        let st = &j["streams"][0];
        assert_eq!((st["codec_name"].as_str(), st["sample_rate"].as_str(), st["bits_per_raw_sample"].as_str()), (Some("flac"), Some("48000"), Some("24")));
    }
}

#[test]
fn flac_audio_in_mp4_and_mov() {
    let (p, seq, m) = matte([0.2, 0.4, 0.6, 1.0], 64, 36, Some(-6.0));
    let dir = Scratch::new("flac-in");
    // 1.5 s at 48 kHz: 17 full FLAC frames of 4096 samples and a last one of 2368
    let range = Some(TimeRange::new(Tick::ZERO, Tick(3 * TICKS_PER_SECOND / 2)));
    let audio = AudioSettings { codec: AudioCodec::Flac, bits: 24, ..Default::default() };
    let wav = dir.path("ref.wav");
    export(&p, seq, &ExportSettings { format: Format::Wav, path: wav.clone(), range, audio: audio.clone(), ..Default::default() }, &m, &Progress::default())
        .unwrap();
    for (format, mux, name) in
        [(Format::H264, Multiplexer::Mp4, "a.mp4"), (Format::H264, Multiplexer::Mov, "b.mov"), (Format::ProRes, Multiplexer::Mp4, "c.mov")]
    {
        let path = dir.path(name);
        let s = ExportSettings { format, multiplexer: mux, path: path.clone(), range, audio: audio.clone(), ..Default::default() };
        assert_eq!(s.audio_codec(), AudioCodec::Flac, "{name}");
        assert!(s.summary(64, 36, FrameRate::FPS_24, 48_000, Tick(TICKS_PER_SECOND)).audio.starts_with("FLAC (lossless) 24-bit"), "{name}");
        export(&p, seq, &s, &m, &Progress::default()).unwrap();
        let bytes = std::fs::read(&path).unwrap();
        // `dfLa` was rewritten at the end: STREAMINFO with frame sizes, total samples and the MD5
        let at = bytes.windows(4).position(|w| w == b"dfLa").unwrap() + 4 + 4 + 4;
        let si = &bytes[at..at + 34];
        assert_eq!(bytes[at - 4], 0x80, "{name}: STREAMINFO is the last metadata block");
        assert!(si[4..10].iter().any(|&b| b != 0), "{name}: frame sizes");
        assert_eq!(u32::from_be_bytes(si[14..18].try_into().unwrap()), 72_000, "{name}: total samples");
        assert!(si[18..34].iter().any(|&b| b != 0), "{name}: MD5");
        // FilmCraft's own importer reads the FLAC track back, sample-accurately long
        let src = filmcraft_codecs::open_bytes(name, bytes.into()).unwrap();
        let a = src.audio(0, 72_000, 48_000).unwrap();
        assert!(a.channels.iter().all(|c| c.len() == 72_000) && a.channels[0].iter().any(|v| v.abs() > 0.1), "{name}");
        if let Some(j) = ffprobe_json(&["-show_streams", "-count_packets"], &path) {
            let st = j["streams"].as_array().unwrap().iter().find(|s| s["codec_type"] == "audio").unwrap().clone();
            assert_eq!((st["codec_name"].as_str(), st["sample_rate"].as_str(), st["channels"].as_u64()), (Some("flac"), Some("48000"), Some(2)), "{name}");
            assert_eq!(st["nb_read_packets"].as_str(), Some("18"), "{name}: one FLAC frame per packet");
            assert_eq!(st["duration_ts"].as_u64(), Some(72_000), "{name}: the short last frame is timed exactly");
        }
        // the audio decodes to the WAV export's samples bit for bit
        if let Some(ffmpeg) = filmcraft_testkit::ffmpeg_or_skip("flac in containers") {
            let raw = |path: &str| {
                let out = std::process::Command::new(&ffmpeg)
                    .args(["-v", "error", "-i", path, "-map", "0:a", "-f", "s32le", "-c:a", "pcm_s32le", "-"])
                    .output()
                    .unwrap();
                assert!(out.status.success() && out.stderr.is_empty(), "{name}: {}", String::from_utf8_lossy(&out.stderr));
                out.stdout
            };
            assert!(raw(&path) == raw(&wav), "{name}: FLAC must decode to the WAV's samples exactly");
        }
    }
    // MXF stays PCM whatever is asked for
    let mxf = ExportSettings { format: Format::MxfOp1a, audio: audio.clone(), ..Default::default() };
    assert_eq!(mxf.audio_codec(), AudioCodec::Pcm);
}

#[test]
fn flac_settings_round_trip_and_summary() {
    let s: ExportSettings = serde_json::from_value(serde_json::json!({"format": "flac"})).unwrap();
    assert_eq!((s.format, s.audio_codec(), s.audio.flac_level, s.has_video()), (Format::Flac, AudioCodec::Flac, 5, false));
    assert_eq!((s.extension(), Format::from_name("FLAC"), Format::Flac.id()), ("flac", Some(Format::Flac), "flac"));
    // settings saved before FLAC read the default level
    let old: AudioSettings = serde_json::from_value(serde_json::json!({"bits": 24})).unwrap();
    assert_eq!(old.flac_level, 5);
    let sum = ExportSettings { format: Format::Flac, ..Default::default() }.summary(64, 36, FrameRate::FPS_24, 48_000, Tick(TICKS_PER_SECOND));
    assert!(sum.audio.starts_with("FLAC (lossless) 16-bit, level 5, 48000 Hz"), "{}", sum.audio);
    assert_eq!(sum.video, "No video");
    // estimated below PCM
    let pcm = ExportSettings { format: Format::Wav, ..Default::default() }.estimate_bytes(64, 36, FrameRate::FPS_24, 48_000, Tick(TICKS_PER_SECOND));
    let flac = ExportSettings { format: Format::Flac, ..Default::default() }.estimate_bytes(64, 36, FrameRate::FPS_24, 48_000, Tick(TICKS_PER_SECOND));
    assert!(flac < pcm, "{flac} < {pcm}");
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
        assert!((rep.measured_lufs + rep.gain_db - target).abs() < 0.1, "unlimited tone needs no correction");
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
fn crf_h264() {
    // settings saved before CRF existed read as the default; the range is checked
    let old: ExportSettings = serde_json::from_value(serde_json::json!({"format": "h264", "bitrateMode": "vbr1Pass"})).unwrap();
    assert_eq!(old.crf, DEFAULT_CRF);
    let c: ExportSettings = serde_json::from_value(serde_json::json!({"bitrateMode": "crf", "crf": 18.0})).unwrap();
    assert_eq!((c.bitrate_mode, c.crf), (BitrateMode::Crf, 18.0));
    for bad in [-1.0, 51.5, f32::NAN, f32::INFINITY] {
        assert!(ExportSettings { bitrate_mode: BitrateMode::Crf, crf: bad, ..Default::default() }.validate().is_err(), "{bad}");
    }
    // the value only matters in CRF mode
    assert!(ExportSettings { crf: f32::NAN, ..Default::default() }.validate().is_ok());
    let summary = c.summary(1920, 1080, FrameRate::FPS_25, 48_000, Tick(10 * TICKS_PER_SECOND));
    assert!(summary.video.contains("CRF (constant quality) 18"), "{}", summary.video);

    // one pass at constant quality: a lower factor spends more bits, and both decode to the picture
    let (p, seq, m) = matte([0.2, 0.5, 0.8, 1.0], 320, 180, None);
    let dir = Scratch::new("crf");
    let mut sizes = Vec::new();
    for crf in [10.0, 40.0] {
        let path = dir.path(&format!("crf{crf}.mp4"));
        let s = ExportSettings { format: Format::H264, path: path.clone(), bitrate_mode: BitrateMode::Crf, crf, include_audio: false, ..Default::default() };
        let prog = Progress::default();
        let r = export(&p, seq, &s, &m, &prog).unwrap();
        assert_eq!((r.frames, prog.total.load(Ordering::Relaxed)), (24, 24));
        let bytes: Arc<[u8]> = std::fs::read(&path).unwrap().into();
        sizes.push(bytes.len());
        let src = filmcraft_codecs::open_bytes("x.mp4", bytes).unwrap();
        let f = src.video_frame(FrameRequest::full(Tick(TICKS_PER_SECOND / 2))).unwrap().to_rgba8();
        assert!((f[2] as i32 - 204).abs() < 12 && (f[1] as i32 - 127).abs() < 12, "CRF {crf}: {:?}", &f[..4]);
    }
    assert!(sizes[0] > sizes[1], "CRF 10 {} bytes, CRF 40 {} bytes", sizes[0], sizes[1]);
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
fn av1_is_an_mp4_format_that_needs_a_registered_encoder() {
    for name in ["av1", "AV1", "av01"] {
        assert_eq!(Format::from_name(name), Some(Format::Av1), "{name}");
    }
    assert_eq!(Format::from_name(Format::Av1.id()), Some(Format::Av1));
    assert_eq!((Format::Av1.extension(), Format::Av1.label()), ("mp4", "AV1"));
    assert!(Format::ALL.contains(&Format::Av1));
    let s: ExportSettings = serde_json::from_value(serde_json::json!({"format": "av1"})).unwrap();
    assert_eq!(s.format, Format::Av1);
    assert_eq!(serde_json::to_value(&s).unwrap()["format"], "av1");
    // MP4 with AAC only: a QuickTime multiplexer setting left over from H.264 does not apply
    let a = ExportSettings { format: Format::Av1, ..Default::default() };
    assert_eq!((a.audio_codec(), a.extension()), (AudioCodec::Aac, "mp4"));
    let mov = ExportSettings { multiplexer: Multiplexer::Mov, ..a.clone() };
    assert_eq!((mov.audio_codec(), mov.extension()), (AudioCodec::Aac, "mp4"));
    let b = a.estimate_bytes(1920, 1088, FrameRate::FPS_25, 48_000, Tick(10 * TICKS_PER_SECOND));
    assert!((b as f64 - 25.4e6).abs() < 0.2e6, "{b}");
    let summary = a.summary(1920, 1088, FrameRate::FPS_25, 48_000, Tick(10 * TICKS_PER_SECOND));
    assert_eq!(summary.format, "AV1 (MP4)");
    assert!(summary.video.contains("AV1 Main"), "{}", summary.video);
    let e = ExportSettings { bitrate_mode: BitrateMode::Vbr2Pass, ..a.clone() }.validate().unwrap_err();
    assert!(e.to_string().contains("two-pass"), "{e}");
    let e = ExportSettings { bitrate_mode: BitrateMode::Crf, ..a.clone() }.validate().unwrap_err();
    assert!(e.to_string().contains("no CRF mode"), "{e}");
    assert!(!Format::Av1.has_builtin_encoder());
    assert!(!available(Format::Av1), "nothing registered an encoder for it");
    let (p, seq, m) = matte([0.0, 0.0, 1.0, 1.0], 160, 96, None);
    let sc = Scratch::new("av1-missing");
    let err = export(&p, seq, &ExportSettings { path: sc.path("x.mp4"), include_audio: false, ..a.clone() }, &m, &Progress::default()).unwrap_err().to_string();
    assert!(err.contains("AV1") && err.contains("encoder not available"), "{err}");
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
    assert!(Format::ALL.contains(&Format::Hevc) && Format::ALL.len() == 21);
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
    let e = ExportSettings { bitrate_mode: BitrateMode::Crf, ..h.clone() }.validate().unwrap_err();
    assert!(e.to_string().contains("CRF"), "{e}");

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
fn tiff_size_estimate_tracks_alpha_and_real_output() {
    let (project, sequence, sources) = matte([0.0, 0.0, 1.0, 0.5], 320, 180, None);
    let rate = FrameRate::FPS_24;
    let duration = rate.tick_of(1);
    for alpha in [false, true] {
        let dir = Scratch::new(&format!("tiff-size-{alpha}"));
        let mut settings = ExportSettings { format: Format::TiffSequence, path: dir.path("frame.tif"), alpha, ..Default::default() };
        settings.range = Some(TimeRange::new(Tick::ZERO, duration));
        settings.validate().unwrap();

        let expected = 320_u64 * 180 * if alpha { 4 } else { 3 };
        let summary = settings.summary(320, 180, rate, 48_000, duration);
        assert_eq!(summary.estimated_bytes, expected, "alpha={alpha}");
        assert_eq!(settings.estimate_bytes(320, 180, rate, 48_000, duration), expected);
        let result = export(&project, sequence, &settings, &sources, &Progress::default()).unwrap();
        assert_eq!(result.frames, 1);
        let paths = files_in(&dir.0);
        assert_eq!(paths.len(), 1, "{paths:?}");
        let actual_bytes = std::fs::metadata(dir.0.join(&paths[0])).unwrap().len();
        assert!(actual_bytes.abs_diff(summary.estimated_bytes) < 1024, "alpha={alpha}: estimate={} actual={actual_bytes}", summary.estimated_bytes);
    }

    // Other uncompressed formats already have the correct channel policy.
    for format in [Format::BmpSequence, Format::TgaSequence] {
        let rgb = ExportSettings { format, alpha: false, ..Default::default() };
        let rgba = ExportSettings { format, alpha: true, ..Default::default() };
        assert_eq!(rgb.estimate_bytes(320, 180, rate, 48_000, duration), 320 * 180 * 3);
        let expected = if format == Format::BmpSequence { 3 } else { 4 };
        assert_eq!(rgba.estimate_bytes(320, 180, rate, 48_000, duration), 320 * 180 * expected);
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

/// Brief large peaks over a quiet bed exercise loudness lost during limiter release.
struct CrestSource(filmcraft_media::SharedSource, Arc<std::sync::atomic::AtomicUsize>);
impl MediaSource for CrestSource {
    fn info(&self) -> &filmcraft_media::MediaInfo {
        self.0.info()
    }
    fn video_frame(&self, req: FrameRequest) -> filmcraft_media::Result<Arc<filmcraft_frame::VideoFrame>> {
        self.0.video_frame(req)
    }
    fn audio(&self, start: i64, frames: usize, sample_rate: u32) -> filmcraft_media::Result<filmcraft_frame::AudioBuffer> {
        self.1.fetch_add(1, Ordering::Relaxed);
        let samples: Vec<f32> = (0..frames)
            .map(|i| {
                let at = start + i as i64;
                let phase = at.rem_euclid(sample_rate as i64 / 4);
                let amplitude = if phase < sample_rate as i64 / 500 { 0.9 } else { 0.08 };
                amplitude * (std::f32::consts::TAU * 1000.0 * at as f32 / sample_rate as f32).sin()
            })
            .collect();
        Ok(filmcraft_frame::AudioBuffer { sample_rate, channels: vec![samples.clone(), samples] })
    }
}

#[test]
fn loudness_normalization_corrects_limiter_loss_and_bounds_impossible_targets() {
    use std::cell::Cell;
    let (p, seq, mut sources) = matte([0.0, 0.0, 0.0, 1.0], 64, 36, Some(-6.0));
    let reads = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    for source in sources.0.values_mut() {
        *source = Arc::new(CrestSource(source.clone(), reads.clone()));
    }
    let range = TimeRange::new(Tick::ZERO, Tick(TICKS_PER_SECOND));
    let mut settings = ExportSettings::default();
    settings.effects.loudness = LoudnessNormalization { enabled: true, target_lufs: -14.0, true_peak_dbtp: -2.0 };
    for target in [-14.0, 2.0] {
        settings.effects.loudness.target_lufs = target;
        let mut audio = audio_out::AudioOut::new(p.clone(), seq, &settings, range).unwrap();
        let chunks = Cell::new(0);
        audio
            .measure(&settings, &sources, &|| {
                chunks.set(chunks.get() + 1);
                false
            })
            .unwrap();
        let report = audio.loudness.unwrap();
        let result = audio.rest(&sources).unwrap();
        let (integrated, peak) = lufs(&result, 48_000);
        eprintln!("target {target}: output {integrated:.3} LUFS, {peak:.3} dBTP, gain {:.3}, chunks {}", report.gain_db, chunks.get());
        assert!(chunks.get() <= 5, "one raw pass plus at most four limited trials");
        assert_eq!(result[0].len(), 48_000, "measuring must rewind output and limiter latency");
        assert!(peak <= -1.8, "limiting must remain enabled: {peak}");
        if target == -14.0 {
            assert!((integrated - target).abs() < 0.15, "achieved {integrated}");
            assert!(report.gain_db > target - report.measured_lufs + 0.3, "fixture must require limiter correction");
        } else {
            assert!(integrated < target - 1.0, "impossible target sacrifices loudness, not peak ceiling");
        }
    }
    // Cancel after raw measurement and one limited trial, before further refinement.
    // Record source reads at cancellation to prove no further mixing occurs.
    let mut audio = audio_out::AudioOut::new(p, seq, &settings, range).unwrap();
    let chunks = Cell::new(0);
    let reads_at_cancel = Cell::new(None);
    let cancel = || {
        chunks.set(chunks.get() + 1);
        if chunks.get() > 2 {
            reads_at_cancel.set(Some(reads.load(Ordering::Relaxed)));
            true
        } else {
            false
        }
    };
    assert!(matches!(audio.measure(&settings, &sources, &cancel), Err(ExportError::Cancelled)));
    assert_eq!(chunks.get(), 3);
    assert_eq!(reads_at_cancel.get(), Some(reads.load(Ordering::Relaxed)));
}

/// 32-bit integer and float audio in every container that takes them, read back by ffprobe (an
/// external oracle) and, where FilmCraft imports the format, by FilmCraft's own readers.
#[test]
fn thirty_two_bit_audio_in_every_container() {
    let (p, seq, m) = matte([0.0, 0.0, 0.0, 1.0], 64, 36, Some(-6.0));
    let dir = Scratch::new("pcm32");
    let range = Some(TimeRange::new(Tick::ZERO, Tick(TICKS_PER_SECOND / 2)));
    let cases: [(Format, SampleFormat, &str, &str, &str); 8] = [
        (Format::Wav, SampleFormat::Float32, "f.wav", "pcm_f32le", "32"),
        (Format::Wav, SampleFormat::Int32, "i.wav", "pcm_s32le", "32"),
        (Format::Aiff, SampleFormat::Float32, "f.aif", "pcm_f32be", "32"),
        (Format::Aiff, SampleFormat::Int32, "i.aif", "pcm_s32be", "32"),
        (Format::ProRes, SampleFormat::Float32, "f.mov", "pcm_f32le", "32"),
        (Format::ProRes, SampleFormat::Int32, "i.mov", "pcm_s32le", "32"),
        (Format::MxfOp1a, SampleFormat::Int32, "i.mxf", "pcm_s32le", "32"),
        (Format::Flac, SampleFormat::Int32, "i.flac", "flac", "32"),
    ];
    for (format, sf, name, codec, bits) in cases {
        let path = dir.path(name);
        let mut s = ExportSettings { format, path: path.clone(), range, ..Default::default() };
        s.audio.set_sample_format(sf);
        assert!(s.supports_sample_format(sf), "{name}");
        export(&p, seq, &s, &m, &Progress::default()).unwrap_or_else(|e| panic!("{name}: {e}"));
        let bytes = std::fs::read(&path).unwrap();
        // FilmCraft's own importer: the -6 dB tone at a peak of 0.5
        if !matches!(format, Format::MxfOp1a) {
            let src = filmcraft_codecs::open_bytes(name, bytes.into()).unwrap_or_else(|e| panic!("{name}: {e}"));
            let a = src.audio(0, 24_000, 48_000).unwrap();
            let peak = a.channels[0].iter().fold(0.0f32, |m, v| m.max(v.abs()));
            assert!((peak - 0.5).abs() < 0.01, "{name}: peak {peak}");
        }
        if let Some(j) = ffprobe_json(&["-show_streams"], &path) {
            let st = j["streams"].as_array().unwrap().iter().find(|s| s["codec_type"] == "audio").unwrap().clone();
            assert_eq!(st["codec_name"].as_str(), Some(codec), "{name}");
            let depth = if codec == "flac" { &st["bits_per_raw_sample"] } else { &st["bits_per_sample"] };
            assert_eq!(depth.as_str().map(str::to_string).or_else(|| depth.as_u64().map(|v| v.to_string())).as_deref(), Some(bits), "{name}");
        }
    }
    // every 32-bit file holds the same samples (float exactly, integers within one step of 2^-31)
    if let Some(ffmpeg) = filmcraft_testkit::ffmpeg_or_skip("32-bit audio") {
        let raw = |name: &str| {
            let out = std::process::Command::new(&ffmpeg)
                .args(["-v", "error", "-i", &dir.path(name), "-map", "0:a", "-f", "f32le", "-c:a", "pcm_f32le", "-"])
                .output()
                .unwrap();
            assert!(out.status.success(), "{name}: {}", String::from_utf8_lossy(&out.stderr));
            out.stdout.as_chunks::<4>().0.iter().map(|b| f32::from_le_bytes(*b)).collect::<Vec<f32>>()
        };
        let reference = raw("f.wav");
        assert_eq!(reference.len(), 24_000 * 2);
        for name in ["f.aif", "f.mov"] {
            assert!(raw(name) == reference, "{name}: float samples differ from the WAV's");
        }
        for name in ["i.wav", "i.aif", "i.mov", "i.mxf", "i.flac"] {
            let got = raw(name);
            assert_eq!(got.len(), reference.len(), "{name}");
            let worst = got.iter().zip(&reference).map(|(a, b)| (a - b).abs()).fold(0.0f32, f32::max);
            assert!(worst < 1e-6, "{name}: differs by {worst}");
        }
    }
}

/// The mix is 32-bit float: 32-bit float export keeps a +6 dB tone's peaks of 2.0, integers clip
/// them at full scale.
#[test]
fn float_export_keeps_peaks_above_full_scale() {
    let (p, seq, m) = matte([0.0, 0.0, 0.0, 1.0], 64, 36, Some(6.0));
    let dir = Scratch::new("pcm-hot");
    let range = Some(TimeRange::new(Tick::ZERO, Tick(TICKS_PER_SECOND / 4)));
    let peak = |sf: SampleFormat, name: &str| {
        let path = dir.path(name);
        let mut s = ExportSettings { format: Format::Wav, path: path.clone(), range, ..Default::default() };
        s.audio.set_sample_format(sf);
        export(&p, seq, &s, &m, &Progress::default()).unwrap();
        let src = filmcraft_codecs::open_bytes(name, std::fs::read(&path).unwrap().into()).unwrap();
        src.audio(0, 12_000, 48_000).unwrap().channels[0].iter().fold(0.0f32, |m, v| m.max(v.abs()))
    };
    let float = peak(SampleFormat::Float32, "f.wav");
    assert!((float - 2.0).abs() < 0.02, "float keeps +6 dBFS: {float}");
    for (sf, name) in [(SampleFormat::Int32, "i32.wav"), (SampleFormat::Int24, "i24.wav"), (SampleFormat::Int16, "i16.wav")] {
        let p = peak(sf, name);
        assert!(p <= 1.0 && p > 0.99, "{name} clips at full scale: {p}");
    }
}

#[test]
fn sample_formats_each_codec_and_container_stores() {
    use SampleFormat::*;
    let supported = |format: Format, codec: AudioCodec| {
        let s = ExportSettings { format, audio: AudioSettings { codec, ..Default::default() }, ..Default::default() };
        SampleFormat::ALL.iter().filter(|&&f| s.supports_sample_format(f)).copied().collect::<Vec<_>>()
    };
    assert_eq!(supported(Format::Wav, AudioCodec::Auto), [Int16, Int24, Int32, Float32]);
    assert_eq!(supported(Format::Aiff, AudioCodec::Auto), [Int16, Int24, Int32, Float32]);
    assert_eq!(supported(Format::ProRes, AudioCodec::Pcm), [Int16, Int24, Int32, Float32]);
    assert_eq!(supported(Format::MxfOp1a, AudioCodec::Auto), [Int16, Int24, Int32]);
    assert_eq!(supported(Format::MxfOpAtom, AudioCodec::Auto), [Int16, Int24, Int32]);
    assert_eq!(supported(Format::Flac, AudioCodec::Auto), [Int16, Int24, Int32]);
    assert_eq!(supported(Format::ProRes, AudioCodec::Flac), [Int16, Int24, Int32]);
    assert!(supported(Format::H264, AudioCodec::Auto).is_empty(), "MP4 is AAC");
    // `bits` rounds down to a size; `float` only with 32
    let fmt = |bits, float| AudioSettings { bits, float, ..Default::default() }.sample_format();
    assert_eq!(
        [fmt(0, false), fmt(20, false), fmt(24, false), fmt(31, false), fmt(32, false), fmt(64, false), fmt(32, true)],
        [Int16, Int16, Int24, Int24, Int32, Int32, Float32]
    );
}

/// Hostile or contradictory settings (CLI, control channel, MCP, old presets) are errors, never
/// a panic or a silently different file.
#[test]
fn unsupported_sample_formats_are_refused() {
    let refused = |format: Format, codec: AudioCodec, bits: u16, float: bool| {
        let s = ExportSettings { format, path: "x".into(), audio: AudioSettings { codec, bits, float, ..Default::default() }, ..Default::default() };
        s.validate().err().map(|e| e.to_string())
    };
    assert!(refused(Format::Wav, AudioCodec::Auto, 24, true).is_some_and(|e| e.contains("32")));
    assert!(refused(Format::Flac, AudioCodec::Auto, 32, true).is_some_and(|e| e.contains("FLAC")));
    assert!(refused(Format::ProRes, AudioCodec::Flac, 32, true).is_some_and(|e| e.contains("FLAC")));
    assert!(refused(Format::MxfOp1a, AudioCodec::Auto, 32, true).is_some_and(|e| e.contains("MXF")));
    for (format, codec) in [(Format::Wav, AudioCodec::Auto), (Format::Aiff, AudioCodec::Auto), (Format::ProRes, AudioCodec::Pcm)] {
        assert_eq!(refused(format, codec, 32, true), None, "{format:?}");
    }
    assert_eq!(refused(Format::Flac, AudioCodec::Auto, 32, false), None);
    assert_eq!(refused(Format::MxfOp1a, AudioCodec::Auto, 32, false), None);
    // AAC has no sample size: the PCM / FLAC choice is ignored, as before
    assert_eq!(refused(Format::H264, AudioCodec::Auto, 32, true), None);
    // no audio, nothing to refuse
    let s = ExportSettings {
        format: Format::MxfOp1a,
        include_audio: false,
        audio: AudioSettings { bits: 32, float: true, ..Default::default() },
        ..Default::default()
    };
    assert!(s.validate().is_ok());
}

#[test]
fn sample_format_settings_round_trip_summary_and_estimate() {
    let s: ExportSettings = serde_json::from_value(serde_json::json!({"format": "wav", "audio": {"bits": 32, "float": true}})).unwrap();
    assert_eq!(s.audio.sample_format(), SampleFormat::Float32);
    let back: ExportSettings = serde_json::from_value(serde_json::to_value(&s).unwrap()).unwrap();
    assert_eq!(back.audio.sample_format(), SampleFormat::Float32);
    // settings saved before 32-bit audio read integers
    let old: AudioSettings = serde_json::from_value(serde_json::json!({"bits": 24})).unwrap();
    assert_eq!((old.float, old.sample_format()), (false, SampleFormat::Int24));
    let sum = |sf: SampleFormat, format: Format| {
        let mut s = ExportSettings { format, ..Default::default() };
        s.audio.set_sample_format(sf);
        (
            s.summary(64, 36, FrameRate::FPS_24, 48_000, Tick(TICKS_PER_SECOND)).audio,
            s.estimate_bytes(64, 36, FrameRate::FPS_24, 48_000, Tick(TICKS_PER_SECOND)),
        )
    };
    let (f, f_bytes) = sum(SampleFormat::Float32, Format::Wav);
    assert!(f.starts_with("Uncompressed 32-bit float PCM, 48000 Hz"), "{f}");
    let (i, _) = sum(SampleFormat::Int32, Format::Wav);
    assert!(i.starts_with("Uncompressed 32-bit PCM"), "{i}");
    let (_, i16_bytes) = sum(SampleFormat::Int16, Format::Wav);
    assert!(f_bytes > i16_bytes * 19 / 10 && f_bytes < i16_bytes * 21 / 10, "{f_bytes} vs {i16_bytes}");
    let (flac, _) = sum(SampleFormat::Int32, Format::Flac);
    assert!(flac.starts_with("FLAC (lossless) 32-bit"), "{flac}");
}

/// FilmCraft imports its own 32-bit FLAC (symphonia rejected the frame header's 32-bit size code
/// as reserved: "unsupported media"), at full scale and with noise, sample for sample.
#[test]
fn own_importer_reads_32_bit_flac_at_full_scale() {
    let n = 20_000usize;
    let mut seed = 7u64;
    let noise: Vec<i32> = (0..n)
        .map(|_| {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            (seed >> 32) as u32 as i32
        })
        .collect();
    let extremes: Vec<i32> = (0..n).map(|i| if i % 3 == 0 { i32::MAX } else { i32::MIN + 1 }).collect();
    for level in [0u8, 5, 8] {
        let mut enc = filmcraft_flac::Encoder::new(filmcraft_flac::EncoderConfig { level, ..filmcraft_flac::EncoderConfig::new(48_000, 2, 32) }).unwrap();
        let mut file = enc.header();
        file.extend(enc.encode(&[&extremes, &noise]).unwrap());
        file.extend(enc.finish());
        let at = filmcraft_flac::STREAMINFO_OFFSET as usize;
        file[at..at + 34].copy_from_slice(&enc.streaminfo());
        let src = filmcraft_codecs::open_bytes("x.flac", file.into()).unwrap_or_else(|e| panic!("level {level}: {e}"));
        let a = src.audio(0, n, 48_000).unwrap();
        for (c, want) in [&extremes, &noise].iter().enumerate() {
            let got = &a.channels[c];
            assert_eq!(got.len(), n, "level {level} channel {c}");
            for (i, (&g, &w)) in got.iter().zip(want.iter()).enumerate() {
                let w = (f64::from(w) / 2_147_483_648.0) as f32;
                assert!((g - w).abs() <= 1e-6, "level {level} channel {c} sample {i}: {g} vs {w}");
            }
        }
    }
}
