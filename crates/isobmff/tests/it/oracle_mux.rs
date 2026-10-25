//! Muxer oracle tests: remux ffmpeg-made files with our writers and validate with ffmpeg/ffprobe.

use crate::common;

use common::*;
use filmcraft_isobmff::*;
use serde_json::Value;
use std::io::Cursor;
use std::path::{Path, PathBuf};

/// All samples of all tracks in decode order interleaved by decode time: (track, sample index).
fn interleave(f: &Mp4File, tracks: &[usize]) -> Vec<(usize, usize)> {
    let mut v: Vec<(usize, usize)> = tracks.iter().flat_map(|&t| (0..f.tracks[t].samples.len()).map(move |i| (t, i))).collect();
    v.sort_by(|a, b| {
        let ta = &f.tracks[a.0];
        let tb = &f.tracks[b.0];
        let x = ta.samples[a.1].dts as i128 * tb.timescale as i128;
        let y = tb.samples[b.1].dts as i128 * ta.timescale as i128;
        x.cmp(&y).then(a.0.cmp(&b.0)).then(a.1.cmp(&b.1))
    });
    v
}

fn track_config(f: &Mp4File, t: &Track, copy_edits: bool) -> TrackConfig {
    let mut cfg = TrackConfig::new(t.entries[0].clone(), t.timescale);
    cfg.language = t.language.clone();
    if copy_edits {
        cfg.edits = t.edits.clone();
    }
    let _ = f;
    cfg
}

enum Finish {
    Normal,
    Faststart,
}

fn remux(input: &Path, out_name: &str, brand: Brand, finish: Finish, timecode: Option<(TimecodeConfig, u32)>) -> Option<(Mp4File, PathBuf)> {
    let data = std::fs::read(input).ok()?;
    let f = open(data.as_slice()).unwrap();
    let mut opts = WriterOptions::new(brand);
    opts.movie_timescale = f.timescale;
    opts.metadata.push(("©nam".into(), "FilmCraft test".into()));
    let mut w = Mp4Writer::new(Cursor::new(Vec::new()), opts).unwrap();
    let av: Vec<usize> = (0..f.tracks.len()).filter(|&i| matches!(f.tracks[i].kind, TrackKind::Video | TrackKind::Audio)).collect();
    let mut map = vec![usize::MAX; f.tracks.len()];
    for &i in &av {
        map[i] = w.add_track(track_config(&f, &f.tracks[i], true)).unwrap();
    }
    if let Some((tc, start)) = timecode {
        let v = av.iter().copied().find(|&i| f.tracks[i].kind == TrackKind::Video).unwrap();
        w.add_timecode_track(map[v], tc, start).unwrap();
    }
    for (t, i) in interleave(&f, &av) {
        let s = f.tracks[t].samples[i];
        let bytes = f.read_sample(data.as_slice(), t, i).unwrap();
        w.write_sample(map[t], WriteSample { data: &bytes, duration: s.duration, composition_offset: (s.pts - s.dts) as i32, is_sync: s.is_sync }).unwrap();
    }
    let out = match finish {
        Finish::Normal => w.finish(),
        Finish::Faststart => w.finish_faststart(),
    }
    .unwrap()
    .into_inner();
    let path = out_dir().join(out_name);
    std::fs::write(&path, &out).unwrap();
    // Our own reader must see identical sample tables and bytes.
    let g = open(out.as_slice()).unwrap();
    for (k, &i) in av.iter().enumerate() {
        let (a, b) = (&f.tracks[i], &g.tracks[k]);
        assert_eq!(a.samples.len(), b.samples.len(), "{out_name}: sample count");
        assert_eq!(a.edit_offset, b.edit_offset, "{out_name}: edit offset");
        assert_eq!(a.codec(), b.codec(), "{out_name}: codec");
        for (j, (x, y)) in a.samples.iter().zip(&b.samples).enumerate() {
            assert_eq!((x.size, x.dts, x.pts, x.duration, x.is_sync), (y.size, y.dts, y.pts, y.duration, y.is_sync), "{out_name}: sample {j}");
        }
        for j in 0..a.samples.len() {
            assert_eq!(f.read_sample(data.as_slice(), i, j).unwrap(), g.read_sample(out.as_slice(), k, j).unwrap());
        }
    }
    assert_eq!(g.metadata.title(), Some("FilmCraft test"));
    Some((f, path))
}

fn stream_packets(path: &Path) -> Vec<Vec<Value>> {
    let pk = packets(path);
    let n = pk.iter().filter_map(|p| int(&p["stream_index"])).max().map(|m| m + 1).unwrap_or(0);
    (0..n).map(|i| pk.iter().filter(|p| int(&p["stream_index"]) == Some(i)).cloned().collect()).collect()
}

/// Compare ffprobe's view of two files (per-stream packets: pts, dts, size, key flag).
fn assert_same_packets(a: &Path, b: &Path) {
    let pa = stream_packets(a);
    let pb = stream_packets(b);
    for (i, (x, y)) in pa.iter().zip(&pb).enumerate() {
        let key = |p: &Value| (int(&p["pts"]), int(&p["dts"]), int(&p["size"]), p["flags"].as_str().map(|f| f.starts_with('K')));
        let kx: Vec<_> = x.iter().map(key).collect();
        let ky: Vec<_> = y.iter().map(key).collect();
        assert_eq!(kx.len(), ky.len(), "stream {i}: packet count {} vs {}", a.display(), b.display());
        for (j, (p, q)) in kx.iter().zip(&ky).enumerate() {
            assert_eq!(p, q, "stream {i} packet {j}");
        }
    }
}

fn assert_same_streams(a: &Path, b: &Path) {
    let sa = streams(a);
    let sb = streams(b);
    for (x, y) in sa.iter().zip(&sb) {
        if x["codec_type"] == "data" {
            continue;
        }
        for k in ["codec_name", "width", "height", "sample_rate", "channels", "nb_frames", "duration_ts", "time_base", "profile", "pix_fmt"] {
            assert_eq!(x[k], y[k], "stream field {k}");
        }
    }
}

fn decode_clean(path: &Path) {
    let err = ffmpeg_decode_errors(path).unwrap();
    assert!(err.trim().is_empty(), "ffmpeg reported errors for {}: {err}", path.display());
}

#[test]
fn remux_h264_aac_mp4_variants() {
    let (Some(src), Some(_)) = (fixture("h264_bframes.mp4"), ffprobe()) else { return };
    for (name, brand, finish) in [
        ("remux_h264_aac.mp4", Brand::Mp4, Finish::Normal),
        ("remux_h264_aac_fast.mp4", Brand::Mp4, Finish::Faststart),
        ("remux_h264_aac.mov", Brand::Mov, Finish::Normal),
    ] {
        let (_, out) = remux(&src, name, brand, finish, None).unwrap();
        decode_clean(&out);
        assert_same_packets(&src, &out);
        assert_same_streams(&src, &out);
        eprintln!("{name}: ok");
    }
    // Faststart layout: moov before mdat.
    let d = std::fs::read(out_dir().join("remux_h264_aac_fast.mp4")).unwrap();
    let moov = d.windows(4).position(|w| w == b"moov").unwrap();
    let mdat = d.windows(4).position(|w| w == b"mdat").unwrap();
    assert!(moov < mdat);
}

#[test]
fn remux_other_codecs() {
    if ffprobe().is_none() {
        return;
    }
    for (src, name, brand) in [
        ("hevc.mp4", "remux_hevc.mp4", Brand::Mp4),
        ("h264.mov", "remux_h264_pcm.mov", Brand::Mov),
        ("mjpeg.mov", "remux_mjpeg.mov", Brand::Mov),
        ("pcm_f32.mov", "remux_pcm_f32.mov", Brand::Mov),
        ("pcm_s16be.mov", "remux_pcm_s16be.mov", Brand::Mov),
        ("aac_stereo44.m4a", "remux_aac.m4a", Brand::Mp4),
        ("colr_pasp.mov", "remux_colr_pasp.mov", Brand::Mov),
        ("editlist_cut.mp4", "remux_editlist.mp4", Brand::Mp4),
        ("alac.m4a", "remux_alac.m4a", Brand::Mp4),
        ("flac.mp4", "remux_flac.mp4", Brand::Mp4),
        ("opus.mp4", "remux_opus.mp4", Brand::Mp4),
        ("av1.mp4", "remux_av1.mp4", Brand::Mp4),
        ("vp9.mp4", "remux_vp9.mp4", Brand::Mp4),
        ("ac3.mp4", "remux_ac3.mp4", Brand::Mp4),
        ("dnxhd.mov", "remux_dnxhd.mov", Brand::Mov),
    ] {
        let Some(p) = fixture(src) else { continue };
        let (f, out) = remux(&p, name, brand, Finish::Faststart, None).unwrap();
        decode_clean(&out);
        let pcm = f.tracks.iter().any(|t| t.pcm_chunked);
        if !pcm {
            assert_same_packets(&p, &out);
        }
        assert_same_streams(&p, &out);
        eprintln!("{name}: ok");
    }
}

#[test]
fn remux_prores_pcm24_with_timecode() {
    let (Some(src), Some(_)) = (fixture("prores.mov"), ffprobe()) else { return };
    let tc = TimecodeConfig { flags: 0, timescale: 25, frame_duration: 1, frames_per_second: 25, start_frame: None, name: None };
    let (f, out) = remux(&src, "remux_prores_tc.mov", Brand::Mov, Finish::Normal, Some((tc, 25 * 3600))).unwrap();
    decode_clean(&out);
    assert_same_streams(&src, &out);
    let st = streams(&out);
    assert!(st.iter().any(|s| s["tags"]["timecode"] == "01:00:00:00"), "timecode missing: {st:?}");
    // PCM byte totals survive ffmpeg's repacketisation.
    let g = open(std::fs::read(&out).unwrap()).unwrap();
    let a = f.track_of_kind(TrackKind::Audio).unwrap();
    let sum = |m: &Mp4File, i: usize| m.tracks[i].samples.iter().map(|s| s.size as u64).sum::<u64>();
    assert_eq!(sum(&f, a), sum(&g, g.track_of_kind(TrackKind::Audio).unwrap()));
    let t = &g.tracks[g.track_of_kind(TrackKind::Timecode).unwrap()];
    let CodecConfig::Timecode(tc) = t.codec().unwrap() else { panic!() };
    assert_eq!(tc.start_frame, Some(90000));
}

#[test]
fn pcm_in_mp4_ipcm() {
    let (Some(src), Some(_)) = (fixture("pcm_s16be.mov"), ffprobe()) else { return };
    let (_, out) = remux(&src, "remux_ipcm.mp4", Brand::Mp4, Finish::Normal, None).unwrap();
    let st = streams(&out);
    assert_eq!(st[0]["codec_tag_string"], "ipcm");
    assert_eq!(st[0]["codec_name"], "pcm_s16be");
    decode_clean(&out);
}

#[test]
fn aac_priming_edit() {
    let (Some(src), Some(_)) = (fixture("aac.mp4"), ffprobe()) else { return };
    let data = std::fs::read(&src).unwrap();
    let f = open(data.as_slice()).unwrap();
    let t = &f.tracks[0];
    let mut w = Mp4Writer::new(Cursor::new(Vec::new()), WriterOptions::new(Brand::Mp4)).unwrap();
    let mut cfg = TrackConfig::new(t.entries[0].clone(), t.timescale);
    cfg.media_start = Some(1024);
    let k = w.add_track(cfg).unwrap();
    for (i, s) in t.samples.iter().enumerate() {
        let d = f.read_sample(data.as_slice(), 0, i).unwrap();
        w.write_sample(k, WriteSample { data: &d, duration: s.duration, composition_offset: 0, is_sync: true }).unwrap();
    }
    let out = out_dir().join("aac_priming.mp4");
    std::fs::write(&out, w.finish().unwrap().into_inner()).unwrap();
    decode_clean(&out);
    let pk = packets(&out);
    assert_eq!(int(&pk[0]["pts"]), Some(-1024));
    assert_eq!(int(&streams(&out)[0]["duration_ts"]), int(&streams(&src)[0]["duration_ts"]));
}

#[test]
fn fragmented_writer_ffmpeg() {
    let (Some(src), Some(_)) = (fixture("h264_bframes.mp4"), ffprobe()) else { return };
    let data = std::fs::read(&src).unwrap();
    let f = open(data.as_slice()).unwrap();
    let mut w = FragmentedWriter::new(Vec::new(), WriterOptions::new(Brand::Mp4));
    let tracks: Vec<usize> = (0..f.tracks.len()).collect();
    for t in &f.tracks {
        w.add_track(TrackConfig::new(t.entries[0].clone(), t.timescale)).unwrap();
    }
    let v = f.track_of_kind(TrackKind::Video).unwrap();
    for (t, i) in interleave(&f, &tracks) {
        let s = f.tracks[t].samples[i];
        if t == v && s.is_sync && i > 0 {
            w.flush_fragment().unwrap();
        }
        let d = f.read_sample(data.as_slice(), t, i).unwrap();
        w.write_sample(t, WriteSample { data: &d, duration: s.duration, composition_offset: (s.pts - s.dts) as i32, is_sync: s.is_sync }).unwrap();
    }
    let bytes = w.finish().unwrap();
    let out = out_dir().join("our_frag.mp4");
    std::fs::write(&out, &bytes).unwrap();
    decode_clean(&out);
    let g = open(bytes.as_slice()).unwrap();
    assert!(g.fragmented && g.fragment_count >= 4);
    // ffprobe sees raw timestamps (no edit list in our init segment).
    let ours = stream_packets(&out);
    for (ti, t) in f.tracks.iter().enumerate() {
        assert_eq!(ours[ti].len(), t.samples.len());
        for (p, s) in ours[ti].iter().zip(&t.samples) {
            assert_eq!(int(&p["pts"]), Some(s.pts));
            assert_eq!(int(&p["dts"]), Some(s.dts));
            assert_eq!(int(&p["size"]), Some(s.size as i64));
        }
        for i in 0..t.samples.len() {
            assert_eq!(f.read_sample(data.as_slice(), ti, i).unwrap(), g.read_sample(bytes.as_slice(), ti, i).unwrap());
        }
    }
}
