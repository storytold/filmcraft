//! High 10 / High 4:2:2 (Intra) streams: bit-exactness against ffmpeg's decoder on libx264
//! fixtures with 4:2:2 and/or 10-bit samples (skipped when ffmpeg is absent).

use crate::common;

use filmcraft_h264::{Decoder, Picture};
use std::path::PathBuf;
use std::process::Command;

struct HbdFixture {
    name: &'static str,
    width: u32,
    height: u32,
    frames: u32,
    /// libx264 / ffmpeg pixel format (yuv420p10le, yuv422p10le, yuv422p).
    pix_fmt: &'static str,
    args: &'static [&'static str],
}

const FIXTURES: &[HbdFixture] = &[
    // XAVC Intra-like: High 4:2:2 Intra, 10-bit, CABAC, 8x8 transform, several slices
    HbdFixture {
        name: "hbd_422p10_intra_cabac",
        width: 352,
        height: 288,
        frames: 6,
        pix_fmt: "yuv422p10le",
        args: &["-profile:v", "high422", "-x264-params", "keyint=1:slices=4"],
    },
    HbdFixture {
        name: "hbd_422p10_intra_cavlc",
        width: 352,
        height: 288,
        frames: 6,
        pix_fmt: "yuv422p10le",
        args: &["-profile:v", "high422", "-x264-params", "keyint=1:cabac=0"],
    },
    HbdFixture {
        name: "hbd_422p10_intra_qp_low",
        width: 176,
        height: 144,
        frames: 4,
        pix_fmt: "yuv422p10le",
        args: &["-profile:v", "high422", "-qp", "1", "-x264-params", "keyint=1"],
    },
    HbdFixture {
        name: "hbd_422p10_intra_qp_high",
        width: 176,
        height: 144,
        frames: 4,
        pix_fmt: "yuv422p10le",
        args: &["-profile:v", "high422", "-qp", "60", "-x264-params", "keyint=1:chroma-qp-offset=4"],
    },
    HbdFixture {
        name: "hbd_422p10_intra_no8x8",
        width: 176,
        height: 144,
        frames: 4,
        pix_fmt: "yuv422p10le",
        args: &["-profile:v", "high422", "-x264-params", "keyint=1:8x8dct=0:cqm=jvt"],
    },
    HbdFixture {
        name: "hbd_422p8_intra",
        width: 352,
        height: 288,
        frames: 4,
        pix_fmt: "yuv422p",
        args: &["-profile:v", "high422", "-x264-params", "keyint=1"],
    },
    HbdFixture {
        name: "hbd_422p8_intra_cavlc",
        width: 176,
        height: 144,
        frames: 4,
        pix_fmt: "yuv422p",
        args: &["-profile:v", "high422", "-x264-params", "keyint=1:cabac=0"],
    },
    // High 10: inter prediction with B-frames, weighted prediction
    HbdFixture {
        name: "hbd_420p10_inter",
        width: 352,
        height: 288,
        frames: 20,
        pix_fmt: "yuv420p10le",
        args: &["-profile:v", "high10", "-x264-params", "bframes=3:weightp=2:ref=3"],
    },
    HbdFixture {
        name: "hbd_420p10_inter_cavlc",
        width: 176,
        height: 144,
        frames: 12,
        pix_fmt: "yuv420p10le",
        args: &["-profile:v", "high10", "-x264-params", "cabac=0:bframes=2"],
    },
];

fn fixture(name: &str) -> &'static HbdFixture {
    FIXTURES.iter().find(|f| f.name == name).unwrap_or_else(|| panic!("unknown fixture {name}"))
}

/// Generate (if needed) the stream and its ffmpeg reference decode (16-bit LE above 8 bits).
fn ensure(f: &HbdFixture) -> Option<(PathBuf, PathBuf)> {
    let ff = filmcraft_testkit::ffmpeg_or_skip(f.name)?;
    let dir = common::fixtures_dir();
    let h264 = dir.join(format!("{}.h264", f.name));
    let yuv = dir.join(format!("{}.yuv", f.name));
    if !h264.exists() {
        let tmp = filmcraft_testkit::temp_path(&h264);
        let vf = format!("testsrc2=size={}x{}:rate=25,format={},noise=alls=10:allf=t+u", f.width, f.height, f.pix_fmt);
        let ok = Command::new(&ff)
            .args(["-hide_banner", "-loglevel", "error", "-y", "-f", "lavfi", "-i", &vf, "-frames:v", &f.frames.to_string(), "-c:v", "libx264"])
            .args(["-pix_fmt", f.pix_fmt])
            .args(f.args)
            .args(["-f", "h264"])
            .arg(&tmp)
            .status()
            .is_ok_and(|s| s.success());
        assert!(ok, "fixture generation failed for {}", f.name);
        std::fs::rename(&tmp, &h264).unwrap();
    }
    if !yuv.exists() {
        let tmp = filmcraft_testkit::temp_path(&yuv);
        let ok = Command::new(&ff)
            .args(["-hide_banner", "-loglevel", "error", "-y", "-i"])
            .arg(&h264)
            .args(["-fps_mode", "passthrough", "-f", "rawvideo", "-pix_fmt", f.pix_fmt])
            .arg(&tmp)
            .status()
            .is_ok_and(|s| s.success());
        assert!(ok, "reference decode failed for {}", f.name);
        std::fs::rename(&tmp, &yuv).unwrap();
    }
    Some((h264, yuv))
}

/// The picture as the bytes ffmpeg writes for its pixel format.
fn raw(p: &Picture) -> Vec<u8> {
    match &p.wide {
        Some(w) => [&w.y, &w.u, &w.v].into_iter().flat_map(|pl| pl.iter().flat_map(|v| v.to_le_bytes())).collect(),
        None => [&p.y, &p.u, &p.v].into_iter().flat_map(|pl| pl.iter().copied()).collect(),
    }
}

fn check(name: &str, threads: usize) {
    let f = fixture(name);
    let Some((h264, yuv)) = ensure(f) else { return };
    let data = std::fs::read(h264).unwrap();
    let want = std::fs::read(yuv).unwrap();
    let mut dec = Decoder::with_threads(threads);
    let mut pics = Vec::new();
    for au in common::split_access_units(&data) {
        pics.extend(dec.decode(au, 0).unwrap_or_else(|e| panic!("{name}: {e}")));
    }
    pics.extend(dec.flush());
    assert!(dec.take_error().is_none(), "{name}: decode error");
    assert_eq!(pics.len(), f.frames as usize, "{name}: frame count");
    let wide = f.pix_fmt.ends_with("10le");
    let c422 = f.pix_fmt.starts_with("yuv422");
    let frame_bytes = (f.width * f.height) as usize * if c422 { 2 } else { 3 } / if c422 { 1 } else { 2 } * if wide { 2 } else { 1 };
    for (i, p) in pics.iter().enumerate() {
        assert_eq!((p.width, p.height), (f.width, f.height));
        assert_eq!(p.bit_depth, if wide { 10 } else { 8 }, "{name}");
        assert_eq!(p.chroma_format_idc, if c422 { 2 } else { 1 }, "{name}");
        assert_eq!(p.wide.is_some(), wide, "{name}");
        let got = raw(p);
        let exp = &want[i * frame_bytes..(i + 1) * frame_bytes];
        if got != exp {
            let first = got.iter().zip(exp).position(|(a, b)| a != b).unwrap_or(0);
            panic!("{name} ({threads} threads): frame {i} differs from ffmpeg (first byte {first} of {frame_bytes})");
        }
    }
}

macro_rules! hbd_tests {
    ($($name:ident),* $(,)?) => {
        $(
            #[test]
            fn $name() {
                check(stringify!($name), 1);
                check(stringify!($name), 4);
            }
        )*
    };
}

hbd_tests!(
    hbd_422p10_intra_cabac,
    hbd_422p10_intra_cavlc,
    hbd_422p10_intra_qp_low,
    hbd_422p10_intra_qp_high,
    hbd_422p10_intra_no8x8,
    hbd_422p8_intra,
    hbd_422p8_intra_cavlc,
    hbd_420p10_inter,
    hbd_420p10_inter_cavlc,
);

/// 4:2:2 with inter prediction (High 4:2:2 long-GOP) is reported as unsupported, not decoded
/// into wrong pictures and not a crash.
#[test]
fn hbd_422_inter_is_unsupported() {
    let Some(ff) = filmcraft_testkit::ffmpeg_or_skip("hbd_422_inter") else { return };
    let path = common::fixtures_dir().join("hbd_422_inter.h264");
    if !path.exists() {
        let tmp = filmcraft_testkit::temp_path(&path);
        let ok = Command::new(&ff)
            .args(["-hide_banner", "-loglevel", "error", "-y", "-f", "lavfi", "-i", "testsrc2=size=176x144:rate=25", "-frames:v", "8"])
            .args(["-c:v", "libx264", "-pix_fmt", "yuv422p10le", "-profile:v", "high422", "-f", "h264"])
            .arg(&tmp)
            .status()
            .is_ok_and(|s| s.success());
        assert!(ok);
        std::fs::rename(&tmp, &path).unwrap();
    }
    let data = std::fs::read(path).unwrap();
    let mut dec = Decoder::with_threads(1);
    let mut err = None;
    for au in common::split_access_units(&data) {
        if let Err(e) = dec.decode(au, 0) {
            err = Some(e);
            break;
        }
    }
    dec.flush();
    let err = err.or_else(|| dec.take_error());
    assert!(err.is_some_and(|e| e.to_string().contains("4:2:2")), "4:2:2 inter slices must be refused");
}
