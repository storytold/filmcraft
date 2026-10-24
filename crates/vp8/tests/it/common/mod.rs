//! Test fixture generation (ffmpeg + libvpx as external encoder and oracle), a tiny IVF reader
//! and the decode helpers.
#![allow(dead_code)]

use filmcraft_vp8::{Decoder, Picture};
use std::path::{Path, PathBuf};
use std::process::Command;

/// One encoder configuration of the fixture matrix.
pub struct Fixture {
    pub name: &'static str,
    /// lavfi source (without size / rate).
    pub source: &'static str,
    pub width: u32,
    pub height: u32,
    pub frames: u32,
    /// libvpx arguments; a leading "2pass" encodes in two passes.
    pub args: &'static [&'static str],
}

macro_rules! fx {
    ($name:expr, $src:expr, $w:expr, $h:expr, $n:expr, [$($a:expr),* $(,)?]) => {
        Fixture { name: $name, source: $src, width: $w, height: $h, frames: $n, args: &[$($a),*] }
    };
}

pub const FIXTURES: &[Fixture] = &[
    fx!("default", "testsrc2", 352, 288, 20, ["-b:v", "600k"]),
    fx!("odd_size", "testsrc2", 177, 99, 12, ["-b:v", "300k"]),
    fx!("tiny", "testsrc2", 33, 17, 10, ["-b:v", "50k"]),
    fx!("intra_only", "mandelbrot", 352, 288, 6, ["-g", "1", "-b:v", "2M"]),
    fx!("noise_high_rate", "cellauto", 352, 288, 10, ["-b:v", "4M"]),
    fx!("near_lossless", "testsrc2", 176, 144, 8, ["-qmin", "0", "-qmax", "0", "-b:v", "20M"]),
    fx!("coarse", "mandelbrot", 352, 288, 12, ["-qmin", "60", "-qmax", "63", "-crf", "62", "-b:v", "100k"]),
    fx!("partitions8", "testsrc2", 640, 360, 8, ["-b:v", "1M", "-slices", "8"]),
    fx!("altref_2pass", "mandelbrot", 352, 288, 30, ["2pass", "-b:v", "500k", "-auto-alt-ref", "1", "-lag-in-frames", "16"]),
    fx!("sharpness7", "testsrc2", 352, 288, 10, ["-b:v", "300k", "-sharpness", "7"]),
    fx!("version1_bilinear_simple", "testsrc2", 352, 288, 12, ["-b:v", "500k", "-profile:v", "1"]),
    fx!("version2", "testsrc2", 352, 288, 12, ["-b:v", "500k", "-profile:v", "2"]),
    fx!("version3_fullpixel", "mandelbrot", 352, 288, 12, ["-b:v", "500k", "-profile:v", "3"]),
    fx!("realtime_segments", "testsrc2", 352, 288, 20, ["-b:v", "300k", "-deadline", "realtime", "-cpu-used", "8", "-error-resilient", "1"]),
    fx!("screen", "smptehdbars", 352, 288, 12, ["-b:v", "200k", "-screen-content-mode", "1"]),
    fx!(
        "temporal_layers",
        "testsrc2",
        352,
        288,
        20,
        [
            "-b:v",
            "800k",
            "-deadline",
            "realtime",
            "-cpu-used",
            "8",
            // error resilient: frames keep their probability updates to themselves, so the
            // enhancement layer can be dropped
            "-error-resilient",
            "1",
            "-ts-parameters",
            "ts_number_layers=2:ts_target_bitrate=400,800:ts_rate_decimator=2,1:ts_periodicity=2:ts_layer_id=0,1:ts_layering_mode=2"
        ]
    ),
    fx!("hd_1080p", "testsrc2", 1920, 1080, 3, ["-b:v", "4M", "-deadline", "realtime", "-cpu-used", "8"]),
];

pub fn fixture(name: &str) -> &'static Fixture {
    FIXTURES.iter().find(|f| f.name == name).unwrap_or_else(|| panic!("unknown fixture {name}"))
}

pub fn ffmpeg() -> Option<PathBuf> {
    for p in ["/opt/homebrew/bin/ffmpeg", "/usr/local/bin/ffmpeg", "/usr/bin/ffmpeg"] {
        if Path::new(p).exists() {
            return Some(PathBuf::from(p));
        }
    }
    None
}

pub fn fixtures_dir() -> PathBuf {
    let d = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/fixtures/vp8-tests");
    std::fs::create_dir_all(&d).expect("create fixtures dir");
    d
}

/// Unique suffix for temporary files: tests run on several threads and may generate the same
/// fixture concurrently.
pub fn unique() -> String {
    static N: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    format!("{}-{n}", std::process::id())
}

pub fn run(cmd: &mut Command) -> bool {
    match cmd.output() {
        Ok(o) if o.status.success() => true,
        Ok(o) => {
            eprintln!("command failed: {cmd:?}\n{}", String::from_utf8_lossy(&o.stderr));
            false
        }
        Err(e) => {
            eprintln!("failed to run {cmd:?}: {e}");
            false
        }
    }
}

/// Generate (if needed) the fixture stream and its reference decode by libvpx through ffmpeg.
/// `None` (the test skips) without ffmpeg or its libvpx encoder.
pub fn ensure(f: &Fixture) -> Option<(PathBuf, PathBuf)> {
    let Some(ff) = ffmpeg() else {
        eprintln!("SKIP {}: ffmpeg not found", f.name);
        return None;
    };
    let dir = fixtures_dir();
    // one generation at a time, across test processes too: tests share fixtures, and realtime
    // libvpx encodes differ from run to run, so a stream and its reference decode must come from
    // the same encode
    // a lock failure is a broken environment, not a missing tool: fail rather than skip
    let lock = std::fs::File::create(dir.join("vp8.generate.lock")).expect("create the fixture lock file");
    lock.lock().expect("lock the fixture lock file");
    let ivf = dir.join(format!("{}.ivf", f.name));
    let yuv = dir.join(format!("{}.yuv", f.name));
    if !ivf.exists() {
        let tmp = dir.join(format!("{}.tmp.{}.ivf", f.name, unique()));
        // the source at an even size, scaled to the fixture's (possibly odd) size
        let src =
            format!("{}=size={}x{}:rate=25,scale={}:{},format=yuv420p", f.source, f.width.next_multiple_of(2), f.height.next_multiple_of(2), f.width, f.height);
        let two_pass = f.args.first() == Some(&"2pass");
        let args: Vec<&str> = f.args.iter().copied().filter(|a| *a != "2pass").collect();
        let log = dir.join(format!("{}.{}.passlog", f.name, unique()));
        let enc = |pass: Option<u32>| {
            let mut c = Command::new(&ff);
            c.args(["-hide_banner", "-loglevel", "error", "-y", "-f", "lavfi", "-i", &src]);
            c.args(["-frames:v", &f.frames.to_string(), "-c:v", "libvpx"]);
            c.args(&args);
            if let Some(p) = pass {
                c.args(["-pass", &p.to_string(), "-passlogfile"]).arg(&log);
            }
            if pass == Some(1) {
                c.args(["-f", "null", "-"]);
            } else {
                c.args(["-f", "ivf"]).arg(&tmp);
            }
            c
        };
        if two_pass && !run(&mut enc(Some(1))) {
            eprintln!("SKIP {}: encode failed (ffmpeg without libvpx?)", f.name);
            return None;
        }
        if !run(&mut enc(two_pass.then_some(2))) {
            eprintln!("SKIP {}: encode failed (ffmpeg without libvpx?)", f.name);
            return None;
        }
        if std::fs::rename(&tmp, &ivf).is_err() && !ivf.exists() {
            return None;
        }
    }
    if !yuv.exists() {
        let tmp = dir.join(format!("{}.tmp.{}.yuv", f.name, unique()));
        let mut c = Command::new(&ff);
        c.args(["-hide_banner", "-loglevel", "error", "-y", "-c:v", "libvpx", "-i"]).arg(&ivf);
        c.args(["-fps_mode", "passthrough", "-f", "rawvideo", "-pix_fmt", "yuv420p"]).arg(&tmp);
        if !run(&mut c) {
            eprintln!("SKIP {}: reference decode failed", f.name);
            return None;
        }
        if std::fs::rename(&tmp, &yuv).is_err() && !yuv.exists() {
            return None;
        }
    }
    Some((ivf, yuv))
}

/// Frames of an IVF file.
pub fn ivf_frames(data: &[u8]) -> Vec<&[u8]> {
    let mut out = Vec::new();
    let Some(h) = data.get(6..8) else { return out };
    let mut pos = usize::from(u16::from_le_bytes([h[0], h[1]]));
    while pos + 12 <= data.len() {
        let size = u32::from_le_bytes([data[pos], data[pos + 1], data[pos + 2], data[pos + 3]]) as usize;
        pos += 12;
        let end = (pos + size).min(data.len());
        out.push(&data[pos..end]);
        pos = end;
    }
    out
}

/// Decode every frame; the pictures in display order.
pub fn decode_all(frames: &[&[u8]]) -> Vec<Picture> {
    let mut dec = Decoder::new();
    frames.iter().enumerate().filter_map(|(i, f)| dec.decode(f, i as i64).expect("decode")).collect()
}

/// A picture as I420 bytes.
pub fn i420(p: &Picture) -> Vec<u8> {
    [p.y.as_slice(), &p.u, &p.v].concat()
}
