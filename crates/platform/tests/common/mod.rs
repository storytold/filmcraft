//! Shared helpers for the platform tests: ffmpeg-made fixtures (external generator only, never
//! linked), MP4 sample reading, and decoding a run of samples to compare decoders.
#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use filmcraft_codecs::{DecodedFrame, VideoDecoder};
use filmcraft_frame::PixelData;
use filmcraft_isobmff::SampleEntry;

/// The parity fixtures: (file, ffmpeg arguments). B-frames everywhere (reordering), a 360-line
/// picture (coded 368: cropping), two GOPs or more (seeks), and an open-GOP HEVC stream (CRA with
/// RASL pictures).
pub const FIXTURES: &[(&str, &[&str])] = &[
    (
        "h264_high.mp4",
        &[
            "-f",
            "lavfi",
            "-i",
            "testsrc2=s=640x360:r=24:d=3,noise=alls=12:allf=t",
            "-c:v",
            "libx264",
            "-profile:v",
            "high",
            "-preset",
            "fast",
            "-x264-params",
            "bframes=3:b-pyramid=normal:keyint=24:min-keyint=24:scenecut=0",
            "-pix_fmt",
            "yuv420p",
        ],
    ),
    (
        "hevc_main.mp4",
        &[
            "-f",
            "lavfi",
            "-i",
            "testsrc2=s=640x360:r=24:d=3,noise=alls=12:allf=t",
            "-c:v",
            "libx265",
            "-preset",
            "fast",
            "-tag:v",
            "hvc1",
            "-x265-params",
            "log-level=error:bframes=4:keyint=24:min-keyint=24:scenecut=0:open-gop=1",
            "-pix_fmt",
            "yuv420p",
        ],
    ),
    (
        "hevc_main10.mp4",
        &[
            "-f",
            "lavfi",
            "-i",
            "testsrc2=s=640x360:r=24:d=3,noise=alls=12:allf=t",
            "-c:v",
            "libx265",
            "-preset",
            "fast",
            "-profile:v",
            "main10",
            "-tag:v",
            "hvc1",
            "-x265-params",
            "log-level=error:bframes=4:keyint=24:min-keyint=24:scenecut=0",
            "-pix_fmt",
            "yuv420p10le",
        ],
    ),
];

pub fn dir() -> PathBuf {
    filmcraft_testkit::fixtures_dir("platform")
}

/// Generate `name` with `ffmpeg -y -v error <args> <out>` (cached).
pub fn fixture(ff: &Path, name: &str, args: &[&str]) -> Option<PathBuf> {
    let out = dir().join(name);
    filmcraft_testkit::fixtures::generate(&out, |tmp| {
        let st = Command::new(ff).args(["-y", "-v", "error"]).args(args).arg(tmp).stdin(Stdio::null()).status();
        match st {
            Ok(s) if s.success() => true,
            other => {
                eprintln!("fixture {name}: ffmpeg failed: {other:?}");
                false
            }
        }
    })
}

/// The fixture by name.
pub fn named(ff: &Path, name: &str) -> Option<PathBuf> {
    let (_, args) = FIXTURES.iter().find(|f| f.0 == name)?;
    fixture(ff, name, args)
}

/// A video track's sample entry, samples (decode order) with pts, and sync flags.
pub struct Stream {
    pub entry: SampleEntry,
    pub samples: Vec<(Vec<u8>, i64)>,
    pub sync: Vec<bool>,
}

pub fn read_stream(path: &Path) -> Stream {
    let bytes = std::fs::read(path).unwrap();
    let file = filmcraft_isobmff::open(bytes.clone()).unwrap();
    let t = file.track_of_kind(filmcraft_isobmff::TrackKind::Video).unwrap();
    let track = &file.tracks[t];
    let samples = (0..track.samples.len()).map(|i| (file.read_sample(&bytes, t, i).unwrap(), track.samples[i].pts)).collect();
    Stream { entry: track.entries[0].clone(), samples, sync: track.samples.iter().map(|s| s.is_sync).collect() }
}

/// Feed `samples` then flush.
pub fn decode_all(d: &mut dyn VideoDecoder, samples: &[(Vec<u8>, i64)]) -> Vec<DecodedFrame> {
    let mut out = Vec::new();
    for (s, p) in samples {
        out.extend(d.decode(s, *p).unwrap_or_else(|e| panic!("{}: {e}", d.name())));
    }
    out.extend(d.flush());
    out
}

/// Assert two decoders' outputs are the same pictures (pts order, size, format, colour, aspect
/// and every sample).
pub fn assert_same(what: &str, a: &[DecodedFrame], b: &[DecodedFrame]) {
    let pa: Vec<i64> = a.iter().map(|f| f.pts).collect();
    let pb: Vec<i64> = b.iter().map(|f| f.pts).collect();
    assert_eq!(pa, pb, "{what}: pts sequence");
    for (x, y) in a.iter().zip(b) {
        let (fx, fy) = (x.frame.materialized(), y.frame.materialized());
        assert_eq!((fx.width, fx.height, fx.par, fx.color), (fy.width, fy.height, fy.par, fy.color), "{what}: pts {} geometry / colour", x.pts);
        match (&fx.data, &fy.data) {
            (PixelData::Yuv8 { planes: p, chroma: c, .. }, PixelData::Yuv8 { planes: q, chroma: d, .. }) => {
                assert_eq!(c, d, "{what}: chroma");
                for i in 0..3 {
                    assert!(p[i] == q[i], "{what}: pts {} plane {i} differs (max diff {})", x.pts, max_diff8(&p[i], &q[i]));
                }
            }
            (PixelData::Yuv16 { planes: p, chroma: c, bits: bx, .. }, PixelData::Yuv16 { planes: q, chroma: d, bits: by, .. }) => {
                assert_eq!((c, bx), (d, by), "{what}: chroma / bits");
                for i in 0..3 {
                    assert!(p[i] == q[i], "{what}: pts {} plane {i} differs (max diff {})", x.pts, max_diff16(&p[i], &q[i]));
                }
            }
            _ => panic!("{what}: pts {}: pixel formats differ", x.pts),
        }
    }
}

fn max_diff8(a: &[u8], b: &[u8]) -> String {
    if a.len() != b.len() {
        return format!("sizes {} vs {}", a.len(), b.len());
    }
    a.iter().zip(b).map(|(x, y)| x.abs_diff(*y)).max().unwrap_or(0).to_string()
}

fn max_diff16(a: &[u16], b: &[u16]) -> String {
    if a.len() != b.len() {
        return format!("sizes {} vs {}", a.len(), b.len());
    }
    a.iter().zip(b).map(|(x, y)| x.abs_diff(*y)).max().unwrap_or(0).to_string()
}

pub fn xorshift(seed: &mut u64) -> u64 {
    *seed ^= *seed << 13;
    *seed ^= *seed >> 7;
    *seed ^= *seed << 17;
    *seed
}
