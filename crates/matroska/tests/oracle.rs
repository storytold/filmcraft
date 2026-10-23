//! Oracle tests: compare our demuxer's packets, timestamps, keyframes and track parameters with
//! ffprobe on ffmpeg-generated fixtures (skipped when ffmpeg/ffprobe are not installed).

mod common;

use common::*;
use filmcraft_matroska::*;
use serde_json::Value;
use std::path::Path;

/// Compare every packet (file order) and stream parameter against ffprobe.
fn compare(ffprobe: &Path, path: &Path) -> (MkvFile, Vec<Packet>) {
    let json = ffprobe_json(ffprobe, path);
    let bytes = std::fs::read(path).unwrap();
    let mut d = Demuxer::from_slice(&bytes).unwrap();
    let name = path.file_name().unwrap().to_string_lossy().to_string();
    let file = d.file().clone();
    let streams = json["streams"].as_array().unwrap();
    assert_eq!(streams.len(), file.tracks.len(), "{name}: stream count");
    for (i, st) in streams.iter().enumerate() {
        let t = &file.tracks[i];
        assert_eq!(st["codec_name"].as_str().unwrap_or(""), t.codec.name(), "{name}: track {i} codec ({})", t.codec_id);
        assert_eq!(st["time_base"].as_str(), Some(format!("{}/{}", t.timebase.0, t.timebase.1).as_str()), "{name}: time_base");
        match t.kind {
            TrackKind::Video => {
                let v = t.video.as_ref().unwrap();
                assert_eq!(int(&st["width"]), Some(v.pixel_width as i64), "{name}: width");
                assert_eq!(int(&st["height"]), Some(v.pixel_height as i64), "{name}: height");
            }
            TrackKind::Audio => {
                let a = t.audio.as_ref().unwrap();
                assert_eq!(int(&st["sample_rate"]), Some(a.sampling_frequency as i64), "{name}: sample_rate");
                assert_eq!(int(&st["channels"]), Some(a.channels as i64), "{name}: channels");
            }
            _ => {}
        }
    }
    let packets: Vec<Packet> = d.by_ref().collect::<Result<_>>().unwrap();
    let ff = json["packets"].as_array().unwrap();
    assert_eq!(packets.len(), ff.len(), "{name}: packet count");
    let (mut pts_checked, mut dur_checked) = (0, 0);
    for (i, (p, f)) in packets.iter().zip(ff).enumerate() {
        let ctx = format!("{name}: packet {i} (track {})", p.track);
        assert_eq!(int(&f["stream_index"]), Some(p.track as i64), "{ctx}: stream");
        if file.tracks[p.track].codec_id.starts_with("D_WEBVTT") {
            // WebM WebVTT blocks carry "id\nsettings\npayload"; FFmpeg moves id/settings to side data
            assert!(p.data.len() as i64 >= int(&f["size"]).unwrap(), "{ctx}: size");
        } else {
            assert_eq!(int(&f["size"]), Some(p.data.len() as i64), "{ctx}: size");
        }
        let key = f["flags"].as_str().unwrap_or("").starts_with('K');
        assert_eq!(key, p.keyframe, "{ctx}: keyframe");
        if let Some(pts) = int(&f["pts"])
            && !(p.lace > 0 && p.duration == 0)
        {
            // FFmpeg truncates the per-lace duration to whole ticks; we place laces exactly
            // (rounded once), so later laces may differ by up to one tick per lace.
            assert!((pts - p.pts).abs() <= p.lace as i64, "{ctx}: pts ffprobe {pts} vs {}", p.pts);
            pts_checked += 1;
        }
        // FFmpeg before 8.1 overwrites audio durations with the parser's whole-frame duration, so
        // a track's last frame, trimmed by BlockDuration, reads as a full frame there (FFmpeg
        // 1dd8547193: "don't overwrite already set packet durations with parser ones").
        let last_of_track = packets[i + 1..].iter().all(|q| q.track != p.track);
        if let Some(dur) = int(&f["duration"])
            && p.duration != 0
            && !(last_of_track && (p.duration as i64) < dur)
        {
            assert!((dur - p.duration as i64).abs() <= 1, "{ctx}: duration ffprobe {dur} vs {}", p.duration);
            dur_checked += 1;
        }
    }
    assert!(pts_checked > 0, "{name}: no pts compared");
    eprintln!("{name}: {} packets, {pts_checked} pts and {dur_checked} durations compared", packets.len());
    // the sample index agrees with the packet stream
    for (ti, t) in file.tracks.iter().enumerate() {
        let pk: Vec<&Packet> = packets.iter().filter(|p| p.track == ti).collect();
        assert_eq!(t.samples.len(), pk.len(), "{name}: index size track {ti}");
        for (i, p) in pk.iter().enumerate() {
            assert_eq!(t.samples[i].pts, p.pts);
            assert_eq!(t.samples[i].keyframe, p.keyframe);
            assert_eq!(t.sample_size(i), Some(p.data.len() as u64));
        }
        if let Some(i) = pk.len().checked_sub(1) {
            assert_eq!(file.read_sample(&bytes[..], ti, i).unwrap(), pk[i].data);
        }
    }
    // duration
    if let Some(fd) = json["format"]["duration"].as_str().and_then(|s| s.parse::<f64>().ok())
        && let Some(ours) = file.duration_ns()
    {
        assert!((fd - ours as f64 / 1e9).abs() < 0.05, "{name}: duration {fd} vs {ours}");
    }
    (file, packets)
}

/// Seek to a range of times on `track` and check we land on the latest keyframe ≤ target.
fn check_seeks(bytes: &[u8], opts: OpenOptions, track: usize, packets: &[Packet]) {
    let mut d = Demuxer::with_options(bytes, opts).unwrap();
    let keys: Vec<(i64, i64)> = packets.iter().filter(|p| p.track == track && p.keyframe).map(|p| (p.pts, p.pts_ns)).collect();
    let last = packets.iter().filter(|p| p.track == track).map(|p| p.pts_ns).max().unwrap();
    let mut t = 0i64;
    while t <= last + 100_000_000 {
        let sp = d.seek(track, t).unwrap();
        let expect = keys.iter().filter(|k| k.1 <= t).map(|k| k.0).max().unwrap_or(keys[0].0);
        assert_eq!(sp.pts, expect, "seek to {t}ns");
        assert!(sp.pts_ns <= t || sp.pts == keys[0].0);
        // the first packet of that track after seeking is the keyframe
        let p = loop {
            let p = d.next_packet().unwrap().expect("packet after seek");
            if p.track == track {
                break p;
            }
        };
        assert!(p.keyframe);
        assert_eq!(p.pts, sp.pts);
        t += 137_000_000;
    }
}

fn run(name: &str) -> Option<(Vec<u8>, MkvFile, Vec<Packet>)> {
    let (ffmpeg, ffprobe) = tools()?;
    let path = fixture(&ffmpeg, name)?;
    let (file, packets) = compare(&ffprobe, &path);
    let bytes = std::fs::read(&path).unwrap();
    Some((bytes, file, packets))
}

#[test]
fn h264_aac() {
    let Some((bytes, f, packets)) = run("h264_aac.mkv") else { return };
    assert_eq!(f.doc_type, "matroska");
    assert!(f.warnings.is_empty(), "{:?}", f.warnings);
    let v = f.track_of_kind(TrackKind::Video).unwrap();
    let Codec::Avc { avcc } = &f.tracks[v].codec else { panic!("not avc") };
    assert_eq!(avcc[0], 1, "avcC version");
    let a = f.track_of_kind(TrackKind::Audio).unwrap();
    assert!(matches!(&f.tracks[a].codec, Codec::Aac { asc } if asc.len() >= 2));
    assert!(!f.cues.is_empty());
    assert!(!f.seek_head.is_empty());
    check_seeks(&bytes, OpenOptions::default(), v, &packets);
    check_seeks(&bytes, OpenOptions { index: false, verify_crc: false }, v, &packets);
    check_seeks(&bytes, OpenOptions::default(), a, &packets);
    // Read + Seek entry point and CRC verification (ffmpeg writes CRC-32 in level-1 elements)
    let d = Demuxer::from_reader(std::io::Cursor::new(bytes.clone())).unwrap();
    assert_eq!(d.file().tracks.len(), 2);
    let crc = open_with(&bytes[..], &OpenOptions { index: true, verify_crc: true }).unwrap();
    assert!(crc.crc_checked > 0);
    assert!(crc.crc_errors.is_empty());
    let kf = Demuxer::from_slice(&bytes).unwrap().keyframe_index(v).unwrap();
    assert_eq!(kf.len(), packets.iter().filter(|p| p.track == v && p.keyframe).count());
}

#[test]
fn vp9_opus_webm() {
    let Some((bytes, f, packets)) = run("vp9_opus.webm") else { return };
    assert!(f.is_webm());
    let a = f.track_of_kind(TrackKind::Audio).unwrap();
    let t = &f.tracks[a];
    assert!(matches!(&t.codec, Codec::Opus { head } if head.starts_with(b"OpusHead")));
    assert!(t.codec_delay_ns > 0 && t.seek_pre_roll_ns > 0);
    assert!(packets.iter().any(|p| p.track == a && p.pts < 0), "codec delay applied");
    let v = f.track_of_kind(TrackKind::Video).unwrap();
    assert!(matches!(f.tracks[v].codec, Codec::Vp9 { .. }));
    check_seeks(&bytes, OpenOptions::default(), v, &packets);
    check_seeks(&bytes, OpenOptions { index: false, verify_crc: false }, v, &packets);
}

#[test]
fn flac() {
    let Some((_, f, _)) = run("flac.mkv") else { return };
    assert!(matches!(&f.tracks[0].codec, Codec::Flac { private } if private.starts_with(b"fLaC")));
}

#[test]
fn vorbis() {
    let Some((_, f, _)) = run("vorbis.mkv") else { return };
    let Codec::Vorbis { headers } = &f.tracks[0].codec else { panic!("not vorbis") };
    assert_eq!(headers.len(), 3);
    assert_eq!(&headers[0][..7], b"\x01vorbis");
    assert_eq!(&headers[1][..7], b"\x03vorbis");
    assert_eq!(&headers[2][..7], b"\x05vorbis");
}

#[test]
fn subtitles() {
    let Some((_, f, packets)) = run("subs.mkv") else { return };
    let subs: Vec<usize> = (0..f.tracks.len()).filter(|&i| f.tracks[i].kind == TrackKind::Subtitle).collect();
    assert_eq!(subs.len(), 2);
    assert_eq!(f.tracks[subs[0]].codec, Codec::SubRip);
    assert_eq!(f.tracks[subs[0]].language, "fre");
    assert!(matches!(&f.tracks[subs[1]].codec, Codec::Ass { header, ssa: false } if !header.is_empty()));
    assert_eq!(f.tracks[subs[1]].name, "Styled");
    let first = packets.iter().find(|p| p.track == subs[0]).unwrap();
    assert_eq!(first.data, b"Hello");
    assert_eq!((first.pts, first.duration), (200, 700));
}

#[test]
fn webvtt_webm() {
    let Some((_, f, packets)) = run("webvtt.webm") else { return };
    let s = f.track_of_kind(TrackKind::Subtitle).unwrap();
    assert_eq!(f.tracks[s].codec, Codec::WebVtt);
    let cues: Vec<&Packet> = packets.iter().filter(|p| p.track == s).collect();
    assert_eq!(cues.len(), 2);
    assert!(cues[0].data.ends_with(b"Hello"));
}

#[test]
fn cues_at_front() {
    let Some((bytes, f, packets)) = run("cues_front.mkv") else { return };
    assert!(!f.cues.is_empty());
    assert!(f.clusters.len() >= 5);
    let v = f.track_of_kind(TrackKind::Video).unwrap();
    check_seeks(&bytes, OpenOptions { index: false, verify_crc: false }, v, &packets);
}

#[test]
fn live_stream_without_cues() {
    let Some((bytes, f, packets)) = run("live.mkv") else { return };
    assert!(f.cues.is_empty(), "streamed output has no Cues");
    assert!(f.live || f.info.duration.is_none());
    assert!(f.duration_ns().unwrap() > 1_900_000_000);
    let v = f.track_of_kind(TrackKind::Video).unwrap();
    // no cues: seeking builds the index by scanning clusters
    check_seeks(&bytes, OpenOptions { index: false, verify_crc: false }, v, &packets);
    let mut d = Demuxer::with_options(&bytes[..], OpenOptions { index: false, verify_crc: false }).unwrap();
    assert!(!d.file().indexed);
    assert!(!d.keyframe_index(v).unwrap().is_empty());
    assert!(d.file().indexed);
}

#[test]
fn hevc_hdr_colour() {
    let Some((_, f, _)) = run("hevc_hdr.mkv") else { return };
    let t = &f.tracks[0];
    assert!(matches!(&t.codec, Codec::Hevc { hvcc } if hvcc.first() == Some(&1)));
    let c = t.video.as_ref().unwrap().colour.as_ref().expect("Colour element");
    assert_eq!(c.primaries, Some(9));
    assert_eq!(c.transfer_characteristics, Some(16));
    assert_eq!(c.matrix_coefficients, Some(9));
    assert_eq!(c.range, Some(1));
    // mastering metadata / CLL are written by ffmpeg from stream side data when available
    if let Some(m) = &c.mastering {
        assert!((m.primaries[1].0 - 0.265).abs() < 1e-3, "{m:?}");
        assert!((m.luminance_max - 1000.0).abs() < 1e-3);
    }
    if let Some(cll) = c.max_cll {
        assert_eq!(cll, 1000);
    }
}

#[test]
fn prores_pcm() {
    let Some((_, f, _)) = run("prores_pcm.mkv") else { return };
    let a = f.track_of_kind(TrackKind::Audio).unwrap();
    assert_eq!(f.tracks[a].codec, Codec::Pcm { float: false, big_endian: false, bits: 24 });
}

#[test]
fn mjpeg_ac3() {
    let Some((_, f, _)) = run("mjpeg_ac3.mkv") else { return };
    assert_eq!(f.info.title.as_deref(), Some("FilmCraft test"));
    assert!(f.info.muxing_app.starts_with("Lavf"));
    assert!(f.tags.iter().flat_map(|t| &t.simple).any(|s| s.name == "ENCODER"));
}

#[test]
fn av1() {
    let Some((_, f, _)) = run("av1.mkv") else { return };
    assert!(matches!(&f.tracks[0].codec, Codec::Av1 { av1c } if av1c.first() == Some(&0x81)));
}

/// Re-mux a fixture with our writer using each lacing mode; ffprobe must read the result identically
/// to our demuxer (and to the source packets).
#[test]
fn laced_remux() {
    let Some((ffmpeg, ffprobe)) = tools() else { return };
    for (src, modes) in [
        ("vorbis.mkv", &[LacingMode::Xiph, LacingMode::Ebml][..]),
        ("h264_aac.mkv", &[LacingMode::Ebml, LacingMode::Xiph][..]),
        ("prores_pcm.mkv", &[LacingMode::FixedOrEbml][..]),
        ("flac.mkv", &[LacingMode::Ebml][..]),
    ] {
        let Some(path) = fixture(&ffmpeg, src) else { continue };
        let bytes = std::fs::read(&path).unwrap();
        let d = Demuxer::from_slice(&bytes).unwrap();
        let file = d.file().clone();
        let packets: Vec<Packet> = d.collect::<Result<_>>().unwrap();
        for &mode in modes {
            let specs: Vec<TrackSpec> = file
                .tracks
                .iter()
                .map(|t| {
                    let mut s = TrackSpec::new(t.kind, t.codec_id.clone());
                    s.codec_private = t.codec_private.clone();
                    s.default_duration_ns = t.default_duration_ns.or_else(|| {
                        // give AAC a DefaultDuration so every lace gets a timestamp
                        matches!(t.codec, Codec::Aac { .. }).then(|| (1024.0 * 1e9 / t.audio.as_ref().unwrap().sampling_frequency) as u64)
                    });
                    s.video_size = t.video.as_ref().map(|v| (v.pixel_width, v.pixel_height));
                    s.audio = t.audio.as_ref().map(|a| (a.sampling_frequency, a.channels, a.bit_depth));
                    s.codec_delay_ns = t.codec_delay_ns;
                    s
                })
                .collect();
            let opts = MuxOptions { lacing: mode, ..Default::default() };
            let mut w = MkvWriter::new(std::io::Cursor::new(Vec::new()), specs, opts).unwrap();
            for p in &packets {
                w.write_frame(p.track, p.pts_ns, p.keyframe, &p.data, None).unwrap();
            }
            let out = w.finish().unwrap().into_inner();
            let laced = {
                let f = open(&out[..]).unwrap();
                f.tracks.iter().any(|t| t.samples.iter().any(|s| s.lace > 0))
            };
            assert!(laced, "{src} {mode:?}: nothing was laced");
            let outp = fixture_dir().join(format!("remux_{mode:?}_{src}"));
            std::fs::write(&outp, &out).unwrap();
            let (_, got) = compare(&ffprobe, &outp);
            assert_eq!(got.len(), packets.len());
            for (a, b) in got.iter().zip(&packets) {
                assert_eq!(a.data, b.data, "{src} {mode:?}: payload");
                assert_eq!(a.track, b.track);
            }
            std::fs::remove_file(&outp).unwrap();
        }
    }
}

/// Damaged file: overwrite bytes in the middle of the clusters; the demuxer must not fail and must
/// resync on a later cluster.
#[test]
fn damaged_resync() {
    let Some((mut bytes, f, packets)) = run("h264_aac.mkv") else { return };
    let mid = f.clusters[f.clusters.len() / 2];
    let at = mid.data_start as usize + 40;
    for b in &mut bytes[at..at + 200] {
        *b = 0x00;
    }
    let d = Demuxer::from_slice(&bytes).unwrap();
    assert!(!d.file().warnings.is_empty());
    let got: Vec<Packet> = d.collect::<Result<_>>().unwrap();
    assert!(got.len() < packets.len());
    assert!(got.len() > packets.len() / 2);
    // packets after the damaged cluster are intact
    let last = packets.last().unwrap();
    assert_eq!(got.last().unwrap(), last);
    // truncated file
    let cut = &bytes[..bytes.len() * 2 / 3];
    let d = Demuxer::from_slice(cut).unwrap();
    let got: Vec<Packet> = d.collect::<Result<_>>().unwrap();
    assert!(!got.is_empty());
    // garbage never panics
    for n in [0usize, 10, 100, 1000] {
        let _ = Demuxer::from_slice(&bytes[..n.min(bytes.len())]).map(|d| d.count());
    }
}

#[test]
fn not_matroska() {
    assert!(matches!(open(&b"hello world"[..]), Err(Error::NotMatroska(_))));
    let mut ebml = vec![0x1A, 0x45, 0xDF, 0xA3, 0x84, 0x42, 0x82, 0x81, b'x'];
    ebml.extend_from_slice(&[0; 8]);
    assert!(matches!(open(&ebml[..]), Err(Error::NotMatroska(_))));
    let _ = Value::Null;
}

/// Pre-generate every fixture (`cargo xtask fixtures`).
#[test]
#[ignore]
fn generate_fixtures() {
    let Some((ffmpeg, _)) = tools() else { return };
    for name in ALL_FIXTURES {
        let out = [fixture_dir().join(name)];
        filmcraft_testkit::fixtures::generate_and_report(&format!("matroska/{name}"), &out, || fixture(&ffmpeg, name));
    }
}
