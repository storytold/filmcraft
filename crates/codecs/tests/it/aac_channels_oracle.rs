//! Multichannel AAC in MP4 against ffmpeg (#922). The AudioSpecificConfig's channel configuration
//! is an index into ISO/IEC 14496-3 Table 1.19 (with Amd 4), not a channel count: 7 and 12 are
//! both 7.1 (eight channels) and 11 is 6.1 (seven). Each channel carries its own tone, so a
//! dropped or swapped channel shows up against ffmpeg's decode.

use crate::common;

use std::io::Cursor;

use common::*;
use filmcraft_isobmff::{Brand, Mp4Writer, SampleEntry, TrackConfig, WriteSample, WriterOptions};

const RATE: u32 = 48_000;

/// A one-second AAC MP4 of `layout` (`n` channels), channel `i` a tone of 200 + 100·i Hz, except
/// the LFE (fourth in all these layouts) at 60 Hz: AAC band-limits it.
fn tones(ff: &std::path::Path, name: &str, layout: &str, n: usize) -> Option<std::path::PathBuf> {
    let mut graph: String =
        (0..n).map(|i| format!("sine=frequency={}:sample_rate={RATE}:duration=1[a{i}];", if i == 3 { 60 } else { 200 + 100 * i })).collect();
    graph.extend((0..n).map(|i| format!("[a{i}]")));
    graph.push_str(&format!("join=inputs={n}:channel_layout={layout}[out]"));
    fixture(ff, name, &["-filter_complex", &graph, "-map", "[out]", "-c:a", "aac", "-b:a", "512k"])
}

/// The AAC access units of `path`'s audio track, rewrapped in a new MP4 whose AudioSpecificConfig
/// says `channel_config` (AAC LC, 48 kHz), keeping each sample's duration and the priming edit.
fn rewrap(path: &std::path::Path, name: &str, channel_config: u8, channels: u32) -> std::path::PathBuf {
    let data = bytes(path);
    let file = filmcraft_isobmff::open(&data).unwrap();
    let track = file.tracks.iter().find(|t| t.kind == filmcraft_isobmff::TrackKind::Audio).unwrap();
    // object type 2, rate index 3 (48 kHz), then the configuration
    let asc = (((2u16 << 11) | (3 << 7) | ((channel_config as u16) << 3)).to_be_bytes()).to_vec();
    let mut mux = Mp4Writer::new(Cursor::new(Vec::new()), WriterOptions::new(Brand::Mp4)).unwrap();
    let id = mux.add_track(TrackConfig::new(SampleEntry::aac(asc, channels, RATE), RATE)).unwrap();
    for s in &track.samples {
        let au = &data[s.offset as usize..s.offset as usize + s.size as usize];
        mux.write_sample(id, WriteSample { data: au, duration: s.duration, composition_offset: 0, is_sync: true }).unwrap();
    }
    let out = dir().join(name);
    std::fs::write(&out, mux.finish().unwrap().into_inner()).unwrap();
    out
}

/// Our decode of the whole file, channel by channel.
fn ours(path: &std::path::Path, name: &str) -> (u32, Vec<Vec<f32>>) {
    let src = filmcraft_codecs::open_bytes(name, bytes(path)).unwrap();
    let ch = src.info().audio().map(|a| a.channels).unwrap();
    (ch, src.audio(0, RATE as usize * 9 / 10, RATE).unwrap().channels)
}

/// ffmpeg's decode, deinterleaved into `n` channels.
fn theirs(ff: &std::path::Path, path: &std::path::Path, n: usize) -> Vec<Vec<f32>> {
    let all = ffmpeg_audio_f32(ff, path, &[]);
    (0..n).map(|c| all.iter().skip(c).step_by(n).copied().collect()).collect()
}

/// Largest sample difference over the first `len` samples, best over a small lag either way (the
/// two decoders may place the priming differently by a few samples).
fn diff(a: &[f32], b: &[f32]) -> f32 {
    let len = a.len().min(b.len()).saturating_sub(64);
    (0..=32).flat_map(|l| [(l, 0), (0, l)]).map(|(x, y)| (32..len).map(|i| (a[i + x] - b[i + y]).abs()).fold(0.0, f32::max)).fold(f32::MAX, f32::min)
}

fn peak(x: &[f32]) -> f32 {
    x.iter().fold(0.0, |m, v| m.max(v.abs()))
}

#[test]
fn aac_channel_configurations_report_and_decode_every_channel() {
    let ff = filmcraft_testkit::require_ffmpeg!();
    // ffmpeg 8 writes 7.1 as configuration 7 and ffmpeg 9 as 12: the same five elements
    let Some(seven) = tones(&ff, "aac_71.mp4", "7.1", 8) else {
        eprintln!("SKIPPED: this ffmpeg can't encode 7.1 AAC");
        return;
    };
    let asc = filmcraft_isobmff::open(bytes(&seven)).unwrap().tracks.iter().find_map(|t| match &t.entries.first()?.codec {
        filmcraft_isobmff::CodecConfig::Aac(a) => Some(a.channel_config),
        _ => None,
    });
    let twelve = match asc {
        Some(7) => rewrap(&seven, "aac_71_cfg12.mp4", 12, 8),
        Some(12) => seven.clone(),
        other => panic!("ffmpeg wrote 7.1 AAC as configuration {other:?}"),
    };
    let seven = if asc == Some(7) { seven } else { rewrap(&seven, "aac_71_cfg7.mp4", 7, 8) };

    // configuration 7: eight channels reported and returned, the last one too
    let (ch, got) = ours(&seven, "aac_71_cfg7.mp4");
    assert_eq!((ch, got.len()), (8, 8), "configuration 7");
    assert!(got.iter().all(|c| peak(c) > 0.05), "configuration 7: a silent channel");

    // configuration 12 (7.1 back): decoded, in ffmpeg's (and WAV's) L R C LFE Lb Rb Ls Rs order
    let (ch, got) = ours(&twelve, "aac_71_cfg12.mp4");
    assert_eq!((ch, got.len()), (8, 8), "configuration 12");
    let want = theirs(&ff, &twelve, 8);
    for (c, (a, b)) in got.iter().zip(&want).enumerate() {
        let d = diff(a, b);
        println!("configuration 12, channel {c}: peak {:.3}, max |ours - ffmpeg| {d:.5}", peak(a));
        assert!(peak(a) > 0.05 && d < 0.01, "configuration 12, channel {c}: off ffmpeg's decode by {d}");
    }

    // controls: 5.1 (configuration 6) and 6.1 (ffmpeg writes a PCE)
    for (layout, n, name) in [("5.1", 6, "aac_51.mp4"), ("6.1", 7, "aac_61.mp4")] {
        let path = tones(&ff, name, layout, n).unwrap();
        let (ch, got) = ours(&path, name);
        assert_eq!((ch as usize, got.len()), (n, n), "{layout}");
    }
}
