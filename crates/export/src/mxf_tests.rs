//! MXF export (OP1a / OP-Atom): read back with our demuxer and decoders, and against ffprobe /
//! ffmpeg as external oracles (decode must succeed and match the same essence exported as MOV;
//! PCM sample-exact).

use std::path::Path;
use std::process::Command;

use super::*;
use filmcraft_media::generators::GeneratorSource;
use filmcraft_media::{FrameRequest, Generator, MediaSource};
use filmcraft_project::{ItemKind, Label, MediaClip, MediaRef, SequenceSettings, TrackKind};
use filmcraft_render::SourceMap;
use filmcraft_time::TICKS_PER_SECOND;

/// 1 s at 24 fps, 320×180: the counting leader over V1, a 440 Hz tone on A1 (stereo mix), start
/// timecode 01:00:00:00.
fn project() -> (Arc<Project>, ItemId, SourceMap) {
    let mut p = Project::new("mxf");
    let r = FrameRate::FPS_24;
    let leader = GeneratorSource::new(Generator::CountingLeader, 320, 180, r, Tick(2 * TICKS_PER_SECOND));
    let tone = GeneratorSource::new(Generator::Tone { hz: 440.0, db: -6.0 }, 320, 180, r, Tick(2 * TICKS_PER_SECOND));
    let add = |p: &mut Project, g: &GeneratorSource| {
        let info = g.info().clone();
        p.add_item(
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
        )
    };
    let v = add(&mut p, &leader);
    let a = add(&mut p, &tone);
    let seq = p.new_sequence("MXF Test", SequenceSettings { width: 320, height: 180, frame_rate: r, ..Default::default() }, 1, 1, None);
    let vi = p.make_track_item(v, TrackKind::Video, Tick::ZERO, TimeRange::new(Tick::ZERO, r.tick_of(24)), r).unwrap();
    let ai = p.make_track_item(a, TrackKind::Audio, Tick::ZERO, TimeRange::new(Tick::ZERO, r.tick_of(24)), r).unwrap();
    let q = p.sequence_mut(seq).unwrap();
    q.video_tracks[0].items.push(vi);
    q.audio_tracks[0].items.push(ai);
    q.start_timecode = 24 * 3600;
    let mut m = SourceMap::default();
    m.0.insert(v, Arc::new(leader));
    m.0.insert(a, Arc::new(tone));
    (Arc::new(p), seq, m)
}

fn tmp(name: &str) -> String {
    let d = std::env::temp_dir().join(format!("fc-export-mxf-{}", std::process::id()));
    std::fs::create_dir_all(&d).unwrap();
    d.join(name).to_string_lossy().to_string()
}

fn settings(format: Format, codec: MxfVideoCodec, path: &str) -> ExportSettings {
    ExportSettings {
        format,
        path: path.into(),
        mxf_video_codec: codec,
        audio: AudioSettings { bits: 24, sample_rate: Some(48_000), channels: 2, ..Default::default() },
        ..Default::default()
    }
}

fn ffprobe_json(ffprobe: &Path, path: &str) -> serde_json::Value {
    let out = Command::new(ffprobe).args(["-v", "error", "-show_format", "-show_streams", "-count_frames", "-of", "json", path]).output().unwrap();
    assert!(out.status.success(), "ffprobe {path}: {}", String::from_utf8_lossy(&out.stderr));
    serde_json::from_slice(&out.stdout).unwrap()
}

/// ffmpeg's decode of `path` (stream `map`) as raw bytes in `fmt`.
fn ffmpeg_raw(ffmpeg: &Path, path: &str, map: &str, args: &[&str]) -> Vec<u8> {
    let out = Command::new(ffmpeg).args(["-v", "error", "-i", path, "-map", map]).args(args).arg("-").output().unwrap();
    assert!(out.status.success(), "ffmpeg {path}: {}", String::from_utf8_lossy(&out.stderr));
    out.stdout
}

fn codec_name(c: MxfVideoCodec) -> &'static str {
    match c {
        MxfVideoCodec::Dnxhr => "dnxhd",
        MxfVideoCodec::ProRes => "prores",
        MxfVideoCodec::H264 => "h264",
    }
}

#[test]
fn op1a_reads_back_with_our_demuxer_and_decoders() {
    let (p, seq, m) = project();
    for codec in MxfVideoCodec::ALL {
        let path = tmp(&format!("op1a_{codec:?}.mxf"));
        let r = export(&p, seq, &settings(Format::MxfOp1a, codec, &path), &m, &Progress::default()).unwrap();
        assert_eq!(r.frames, 24);
        assert!(r.extra_files.is_empty());
        let bytes = std::fs::read(&path).unwrap();
        let mx = filmcraft_mxf::open(&bytes).unwrap();
        assert!(mx.warnings.is_empty(), "{codec:?}: {:?}", mx.warnings);
        assert_eq!(mx.operational_pattern.name(), "OP1a");
        assert_eq!(mx.material_package_name.as_deref(), Some("MXF Test"));
        assert_eq!(mx.timecode.unwrap().format(), "01:00:00:00");
        let v = &mx.tracks[mx.track_of_kind(filmcraft_mxf::TrackKind::Picture).unwrap()];
        assert_eq!(v.samples.len(), 24, "{codec:?}");
        assert_eq!(v.duration, Some(24));
        let a = &mx.tracks[mx.track_of_kind(filmcraft_mxf::TrackKind::Sound).unwrap()];
        assert_eq!(a.stored_sample_frames(), 48_000, "{codec:?}");
        assert_eq!(a.sound.as_ref().unwrap().bits, 24);
        if codec == MxfVideoCodec::H264 {
            assert!(v.temporal_offsets, "B pictures: the index gives the presentation order");
            assert!(v.samples[0].key && v.samples.iter().filter(|s| s.key).count() < 24);
        }
        // our MXF source decodes it like the MOV export of the same sequence
        let src = filmcraft_codecs::open_bytes("x.mxf", bytes.into()).unwrap();
        let info = src.info();
        assert_eq!(info.video.as_ref().unwrap().width, 320, "{codec:?}");
        assert!((info.duration.seconds() - 1.0).abs() < 0.05, "{codec:?}: {}", info.duration.seconds());
        let mov = tmp(&format!("op1a_{codec:?}.mov"));
        let ms = ExportSettings { format: codec.encoder_format(), multiplexer: Multiplexer::Mov, ..settings(Format::MxfOp1a, codec, &mov) };
        export(&p, seq, &ms, &m, &Progress::default()).unwrap();
        let msrc = filmcraft_codecs::open_bytes("x.mov", std::fs::read(&mov).unwrap().into()).unwrap();
        for f in [0i64, 5, 13, 23, 7] {
            let t = FrameRate::FPS_24.tick_of(f) + Tick(1);
            let a = src.video_frame(FrameRequest::full(t)).unwrap().to_rgba8();
            let b = msrc.video_frame(FrameRequest::full(t)).unwrap().to_rgba8();
            assert!(a == b, "{codec:?}: frame {f} differs from the MOV export");
        }
        let pcm = src.audio(0, 48_000, 48_000).unwrap();
        assert!((pcm.peaks()[0] - 0.5).abs() < 0.05, "{codec:?}: tone at -6 dB: {}", pcm.peaks()[0]);
    }
}

#[test]
fn op_atom_writes_one_file_per_essence() {
    let (p, seq, m) = project();
    let path = tmp("atom.mxf");
    let r = export(&p, seq, &settings(Format::MxfOpAtom, MxfVideoCodec::Dnxhr, &path), &m, &Progress::default()).unwrap();
    assert_eq!(r.extra_files, opatom_audio_paths(&path, 2));
    assert!(r.extra_files[0].ends_with("atom_A1.mxf") && r.extra_files[1].ends_with("atom_A2.mxf"));
    let v = std::fs::read(&path).unwrap();
    let mv = filmcraft_mxf::open(&v).unwrap();
    assert_eq!(mv.operational_pattern.name(), "OP-Atom");
    assert_eq!(mv.tracks.len(), 1);
    assert_eq!(mv.tracks[0].wrapping, filmcraft_mxf::Wrapping::Clip);
    assert_eq!(mv.tracks[0].samples.len(), 24);
    for (k, a) in r.extra_files.iter().enumerate() {
        let b = std::fs::read(a).unwrap();
        let ma = filmcraft_mxf::open(&b).unwrap();
        assert_eq!(ma.operational_pattern.name(), "OP-Atom");
        let t = &ma.tracks[0];
        assert_eq!(t.kind, filmcraft_mxf::TrackKind::Sound);
        assert_eq!(t.track_id, 3 + k as u32);
        assert_eq!(t.sound.as_ref().unwrap().channels, 1);
        assert_eq!(t.stored_sample_frames(), 48_000);
        assert_eq!(t.duration, Some(24), "audio edit rate is the frame rate");
        assert_eq!(ma.timecode.unwrap().format(), "01:00:00:00");
        let pcm = ma.read_pcm(&b, 0, 0, 48_000).unwrap();
        assert!((pcm[0].iter().fold(0f32, |a, s| a.max(s.abs())) - 0.5).abs() < 0.05);
    }
}

#[test]
fn mxf_formats_and_presets() {
    assert_eq!(Format::from_name("mxf"), Some(Format::MxfOp1a));
    assert_eq!(Format::from_name("mxf-op1a"), Some(Format::MxfOp1a));
    assert_eq!(Format::from_name("mxf-opatom"), Some(Format::MxfOpAtom));
    assert_eq!(Format::from_name(Format::MxfOpAtom.id()), Some(Format::MxfOpAtom));
    assert_eq!(Format::MxfOp1a.extension(), "mxf");
    let s: ExportSettings = serde_json::from_value(serde_json::json!({"format": "mxf-op1a", "mxfVideoCodec": "proRes"})).unwrap();
    assert_eq!((s.format, s.video_format()), (Format::MxfOp1a, Format::ProRes));
    assert_eq!(s.audio_codec(), AudioCodec::Pcm);
    assert!(s.summary(1920, 1080, FrameRate::FPS_24, 48_000, Tick(TICKS_PER_SECOND)).format.contains("MXF OP1a (Apple ProRes)"));
    let names: Vec<String> = builtin_presets().into_iter().filter(|p| p.settings.format.is_mxf()).map(|p| p.name).collect();
    assert_eq!(names.len(), 4, "{names:?}");
    assert_eq!(
        opatom_audio_paths("/a/b/clip.mxf", 2).iter().map(std::path::PathBuf::from).collect::<Vec<_>>(),
        vec![std::path::PathBuf::from("/a/b/clip_A1.mxf"), std::path::PathBuf::from("/a/b/clip_A2.mxf")]
    );
}

/// ffprobe identifies our files (operational pattern, codec, frame count, duration, timecode);
/// ffmpeg decodes them to exactly what it decodes from the same essence in a MOV export, and
/// the PCM is sample-exact against what we wrote.
#[test]
fn ffmpeg_oracle_op1a_and_op_atom() {
    let (Some(ffprobe), Some(ffmpeg)) = (filmcraft_testkit::ffprobe_or_skip("MXF export probe"), filmcraft_testkit::ffmpeg_or_skip("MXF export decode")) else {
        return;
    };
    let (p, seq, m) = project();
    for codec in MxfVideoCodec::ALL {
        let path = tmp(&format!("oracle_{codec:?}.mxf"));
        export(&p, seq, &settings(Format::MxfOp1a, codec, &path), &m, &Progress::default()).unwrap();
        let j = ffprobe_json(&ffprobe, &path);
        assert_eq!(j["format"]["format_name"], "mxf", "{codec:?}");
        let op = j["format"]["tags"]["operational_pattern_ul"].as_str().unwrap_or_default();
        assert!(op.starts_with("060e2b34.04010101.0d010201.0101"), "{codec:?}: OP1a label {op}");
        let streams = j["streams"].as_array().unwrap();
        let v = streams.iter().find(|s| s["codec_type"] == "video").unwrap();
        let a = streams.iter().find(|s| s["codec_type"] == "audio").unwrap();
        assert_eq!(v["codec_name"], codec_name(codec), "{codec:?}");
        assert_eq!((v["width"].as_u64(), v["height"].as_u64()), (Some(320), Some(180)));
        assert_eq!(v["nb_read_frames"], "24", "{codec:?}");
        assert_eq!(v["r_frame_rate"], "24/1", "{codec:?}");
        let tc = j["format"]["tags"]["timecode"].as_str().or(v["tags"]["timecode"].as_str()).unwrap_or_default();
        assert_eq!(tc, "01:00:00:00", "{codec:?}");
        let dur: f64 = j["format"]["duration"].as_str().unwrap().parse().unwrap();
        assert!((dur - 1.0).abs() < 0.05, "{codec:?}: {dur}");
        assert_eq!(a["codec_name"], "pcm_s24le", "{codec:?}");
        assert_eq!(a["channels"], 2);
        assert_eq!(a["sample_rate"], "48000");
        // ffmpeg's decode of the MXF equals its decode of the same essence in MOV
        let mov = tmp(&format!("oracle_{codec:?}.mov"));
        let ms = ExportSettings { format: codec.encoder_format(), multiplexer: Multiplexer::Mov, ..settings(Format::MxfOp1a, codec, &mov) };
        export(&p, seq, &ms, &m, &Progress::default()).unwrap();
        let raw = ["-f", "rawvideo", "-pix_fmt", "yuv422p10le"];
        let a_mxf = ffmpeg_raw(&ffmpeg, &path, "0:v:0", &raw);
        let a_mov = ffmpeg_raw(&ffmpeg, &mov, "0:v:0", &raw);
        assert_eq!(a_mxf.len(), 24 * 320 * 180 * 4, "{codec:?}: 24 frames decoded");
        assert!(a_mxf == a_mov, "{codec:?}: ffmpeg decodes the MXF and the MOV essence differently");
        // PCM exact: ffmpeg's samples are what our demuxer reads back
        let pcm = ffmpeg_raw(&ffmpeg, &path, "0:a:0", &["-f", "s32le", "-acodec", "pcm_s32le"]);
        let bytes = std::fs::read(&path).unwrap();
        let mx = filmcraft_mxf::open(&bytes).unwrap();
        let at = mx.track_of_kind(filmcraft_mxf::TrackKind::Sound).unwrap();
        let ours = mx.read_pcm(&bytes, at, 0, 48_000).unwrap();
        assert_eq!(pcm.len(), 48_000 * 2 * 4, "{codec:?}");
        for (i, c) in pcm.as_chunks::<4>().0.iter().enumerate() {
            let s = i32::from_le_bytes([c[0], c[1], c[2], c[3]]) >> 8;
            assert_eq!(s as f32 / 8_388_608.0, ours[i % 2][i / 2], "{codec:?}: sample {i}");
        }
    }
    // OP-Atom: picture file and one mono PCM file per channel
    let path = tmp("oracle_atom.mxf");
    let r = export(&p, seq, &settings(Format::MxfOpAtom, MxfVideoCodec::Dnxhr, &path), &m, &Progress::default()).unwrap();
    let j = ffprobe_json(&ffprobe, &path);
    let op = j["format"]["tags"]["operational_pattern_ul"].as_str().unwrap_or_default();
    assert!(op.starts_with("060e2b34.04010102.0d010201.10"), "OP-Atom label {op}");
    let v = &j["streams"][0];
    assert_eq!(v["codec_name"], "dnxhd");
    assert_eq!(v["nb_read_frames"], "24");
    assert_eq!(j["streams"].as_array().unwrap().len(), 1);
    let mov = tmp("oracle_atom.mov");
    let ms = ExportSettings { format: Format::DnxHr, multiplexer: Multiplexer::Mov, ..settings(Format::MxfOpAtom, MxfVideoCodec::Dnxhr, &mov) };
    export(&p, seq, &ms, &m, &Progress::default()).unwrap();
    let raw = ["-f", "rawvideo", "-pix_fmt", "yuv422p10le"];
    assert!(ffmpeg_raw(&ffmpeg, &path, "0:v:0", &raw) == ffmpeg_raw(&ffmpeg, &mov, "0:v:0", &raw), "OP-Atom picture decode");
    let wav = tmp("oracle_atom.wav");
    let ws = ExportSettings { format: Format::Wav, ..settings(Format::Wav, MxfVideoCodec::Dnxhr, &wav) };
    export(&p, seq, &ws, &m, &Progress::default()).unwrap();
    let stereo = ffmpeg_raw(&ffmpeg, &wav, "0:a:0", &["-f", "s32le", "-acodec", "pcm_s32le"]);
    for (k, a) in r.extra_files.iter().enumerate() {
        let j = ffprobe_json(&ffprobe, a);
        let s = &j["streams"][0];
        assert_eq!((s["codec_name"].as_str(), s["channels"].as_u64(), s["sample_rate"].as_str()), (Some("pcm_s24le"), Some(1), Some("48000")), "{a}");
        let mono = ffmpeg_raw(&ffmpeg, a, "0:a:0", &["-f", "s32le", "-acodec", "pcm_s32le"]);
        assert_eq!(mono.len(), 48_000 * 4, "{a}");
        let want: Vec<u8> = stereo.as_chunks::<8>().0.iter().flat_map(|f| f[k * 4..k * 4 + 4].to_vec()).collect();
        assert!(mono == want, "{a}: channel {k} differs from the WAV export");
    }
}
