//! Test fixture generation (ffmpeg + libvpx-vp9 as external encoder and oracle), a tiny IVF
//! reader, and comparison helpers.
#![allow(dead_code)]

use filmcraft_vp9::{Decoder, Picture};
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
    /// Extra video filter appended after the source.
    pub filter: &'static str,
    /// Pixel format of the encode (and of the reference decode).
    pub pix_fmt: &'static str,
    /// libvpx-vp9 arguments.
    pub args: &'static [&'static str],
}

const NOISE: &str = "noise=alls=10:allf=t+u";

macro_rules! fx {
    ($name:expr, $src:expr, $w:expr, $h:expr, $n:expr, $filter:expr, $pf:expr, [$($a:expr),* $(,)?]) => {
        Fixture { name: $name, source: $src, width: $w, height: $h, frames: $n, filter: $filter, pix_fmt: $pf, args: &[$($a),*] }
    };
}

const P420: &str = "yuv420p";
const P420_10: &str = "yuv420p10le";

pub const FIXTURES: &[Fixture] = &[
    fx!("intra_only_keyframes", "testsrc2", 352, 288, 3, NOISE, P420, ["-g", "1", "-b:v", "1M"]),
    fx!("default_good", "testsrc2", 352, 288, 20, NOISE, P420, ["-b:v", "800k", "-frame-parallel", "0"]),
    fx!("profile2_10bit", "testsrc2", 352, 288, 12, NOISE, P420_10, ["-b:v", "800k", "-profile:v", "2", "-frame-parallel", "0"]),
    fx!("profile2_12bit", "mandelbrot", 176, 144, 8, NOISE, "yuv420p12le", ["-b:v", "500k", "-profile:v", "2", "-frame-parallel", "0"]),
    fx!("profile1_444", "testsrc2", 176, 144, 8, NOISE, "yuv444p", ["-b:v", "500k", "-profile:v", "1", "-frame-parallel", "0"]),
    fx!("profile1_422", "testsrc2", 176, 144, 8, NOISE, "yuv422p", ["-b:v", "500k", "-profile:v", "1", "-frame-parallel", "0"]),
    fx!("profile1_440", "testsrc2", 176, 144, 8, NOISE, "yuv440p", ["-b:v", "500k", "-profile:v", "1", "-frame-parallel", "0"]),
    fx!("profile3_444_10bit", "testsrc2", 176, 144, 6, NOISE, "yuv444p10le", ["-b:v", "500k", "-profile:v", "3", "-frame-parallel", "0"]),
    fx!("profile3_422_12bit", "mandelbrot", 176, 144, 6, NOISE, "yuv422p12le", ["-b:v", "500k", "-profile:v", "3", "-frame-parallel", "0"]),
    fx!("lossless", "testsrc2", 176, 144, 5, NOISE, P420, ["-lossless", "1", "-frame-parallel", "0"]),
    fx!("lossless_10bit", "mandelbrot", 176, 144, 4, NOISE, P420_10, ["-lossless", "1", "-profile:v", "2", "-frame-parallel", "0"]),
    fx!("tiles_cols4", "testsrc2", 1280, 720, 6, NOISE, P420, ["-b:v", "2M", "-tile-columns", "2", "-speed", "4", "-frame-parallel", "0"]),
    fx!(
        "tiles_rows_cols",
        "testsrc2",
        1280,
        720,
        6,
        NOISE,
        P420,
        ["-b:v", "2M", "-tile-columns", "1", "-tile-rows", "2", "-speed", "4", "-frame-parallel", "0"]
    ),
    fx!("tiles_rows4", "testsrc2", 640, 480, 5, NOISE, P420, ["-b:v", "1M", "-tile-columns", "0", "-tile-rows", "2", "-frame-parallel", "0"]),
    fx!(
        "altref_hidden",
        "testsrc2",
        352,
        288,
        30,
        NOISE,
        P420,
        ["2pass", "-b:v", "600k", "-auto-alt-ref", "1", "-lag-in-frames", "25", "-frame-parallel", "0"]
    ),
    fx!(
        "altref_10bit",
        "mandelbrot",
        352,
        288,
        24,
        NOISE,
        P420_10,
        ["2pass", "-b:v", "600k", "-profile:v", "2", "-auto-alt-ref", "1", "-lag-in-frames", "16", "-frame-parallel", "0"]
    ),
    fx!(
        "altref_tiles",
        "testsrc2",
        1280,
        720,
        12,
        NOISE,
        P420,
        ["2pass", "-b:v", "2M", "-auto-alt-ref", "1", "-lag-in-frames", "10", "-tile-columns", "2", "-speed", "2", "-frame-parallel", "0"]
    ),
    fx!("frame_parallel", "testsrc2", 352, 288, 16, NOISE, P420, ["-b:v", "600k", "-frame-parallel", "1"]),
    fx!("error_resilient", "testsrc2", 352, 288, 16, NOISE, P420, ["-b:v", "600k", "-error-resilient", "1"]),
    fx!("odd_size", "testsrc2", 347, 251, 10, NOISE, P420, ["-b:v", "600k", "-frame-parallel", "0"]),
    fx!("tiny_odd", "testsrc2", 33, 17, 8, NOISE, P420, ["-b:v", "200k", "-frame-parallel", "0"]),
    // Two superblock columns and twelve rows: the loop filter wavefront waits for whole rows.
    fx!("narrow_tall", "testsrc2", 120, 720, 6, NOISE, P420, ["-b:v", "400k", "-frame-parallel", "0"]),
    fx!("row_mt", "testsrc2", 640, 360, 10, NOISE, P420, ["-b:v", "1M", "-row-mt", "1", "-tile-columns", "1", "-frame-parallel", "0"]),
    fx!("speed0", "mandelbrot", 176, 144, 8, NOISE, P420, ["2pass", "-b:v", "300k", "-speed", "0", "-frame-parallel", "0"]),
    fx!("speed1", "testsrc2", 352, 288, 10, NOISE, P420, ["-b:v", "600k", "-speed", "1", "-frame-parallel", "0"]),
    fx!("speed2", "testsrc2", 352, 288, 10, NOISE, P420, ["-b:v", "600k", "-speed", "2", "-frame-parallel", "0"]),
    fx!("speed3", "testsrc2", 352, 288, 10, NOISE, P420, ["-b:v", "600k", "-speed", "3", "-frame-parallel", "0"]),
    fx!("speed4", "testsrc2", 352, 288, 10, NOISE, P420, ["-b:v", "600k", "-speed", "4", "-frame-parallel", "0"]),
    fx!("speed5", "testsrc2", 352, 288, 10, NOISE, P420, ["-b:v", "600k", "-speed", "5", "-deadline", "realtime", "-frame-parallel", "0"]),
    fx!("speed6", "testsrc2", 352, 288, 10, NOISE, P420, ["-b:v", "600k", "-speed", "6", "-deadline", "realtime", "-frame-parallel", "0"]),
    fx!("speed7", "testsrc2", 352, 288, 10, NOISE, P420, ["-b:v", "600k", "-speed", "7", "-deadline", "realtime", "-frame-parallel", "0"]),
    fx!("speed8", "testsrc2", 352, 288, 10, NOISE, P420, ["-b:v", "600k", "-speed", "8", "-deadline", "realtime", "-frame-parallel", "0"]),
    fx!("cq_low", "testsrc2", 352, 288, 8, NOISE, P420, ["-crf", "0", "-b:v", "0", "-frame-parallel", "0"]),
    fx!("cq_q4", "testsrc2", 352, 288, 8, NOISE, P420, ["-crf", "4", "-b:v", "0", "-frame-parallel", "0"]),
    fx!("cq_high", "testsrc2", 352, 288, 12, NOISE, P420, ["-crf", "63", "-b:v", "0", "-frame-parallel", "0"]),
    fx!("cq_10bit_low", "testsrc2", 176, 144, 6, NOISE, P420_10, ["-crf", "2", "-b:v", "0", "-profile:v", "2", "-frame-parallel", "0"]),
    fx!("cq_12bit_high", "testsrc2", 176, 144, 6, NOISE, "yuv420p12le", ["-crf", "60", "-b:v", "0", "-profile:v", "2", "-frame-parallel", "0"]),
    fx!("aq_segmentation", "testsrc2", 352, 288, 12, NOISE, P420, ["-b:v", "600k", "-aq-mode", "3", "-frame-parallel", "0"]),
    fx!("aq_variance", "mandelbrot", 352, 288, 12, NOISE, P420, ["-b:v", "600k", "-aq-mode", "1", "-frame-parallel", "0"]),
    fx!("aq_complexity", "testsrc2", 352, 288, 12, NOISE, P420, ["2pass", "-b:v", "600k", "-aq-mode", "2", "-frame-parallel", "0"]),
    fx!("sharpness", "testsrc2", 352, 288, 8, NOISE, P420, ["-b:v", "400k", "-sharpness", "5", "-frame-parallel", "0"]),
    fx!("fade_intra_heavy", "testsrc2", 352, 288, 20, "fade=in:0:15", P420, ["-b:v", "400k", "-g", "8", "-frame-parallel", "0"]),
    fx!("hd_1080p", "testsrc2", 1920, 1080, 3, NOISE, P420, ["-b:v", "4M", "-speed", "5", "-deadline", "realtime"]),
    // Two temporal layers: every other frame refreshes no reference slot (draft mode test).
    fx!(
        "temporal_layers",
        "testsrc2",
        352,
        288,
        20,
        NOISE,
        P420,
        [
            "-b:v",
            "800k",
            "-speed",
            "8",
            "-deadline",
            "realtime",
            "-ts-parameters",
            "ts_number_layers=2:ts_target_bitrate=400,800:ts_rate_decimator=2,1:ts_periodicity=2:ts_layer_id=0,1:ts_layering_mode=2"
        ]
    ),
    fx!(
        "bench_1080p",
        "testsrc2",
        1920,
        1080,
        60,
        "noise=alls=3:allf=t",
        P420,
        ["-b:v", "4M", "-speed", "4", "-tile-columns", "2", "-row-mt", "1", "-frame-parallel", "0"]
    ),
    fx!(
        "bench_4k",
        "testsrc2",
        3840,
        2160,
        20,
        "noise=alls=3:allf=t",
        P420,
        ["-b:v", "12M", "-speed", "6", "-tile-columns", "3", "-row-mt", "1", "-frame-parallel", "0"]
    ),
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
    let d = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/fixtures/vp9");
    std::fs::create_dir_all(&d).expect("create fixtures dir");
    d
}

/// Unique suffix for temporary files: tests run on several threads of one process and may
/// generate the same fixture concurrently.
pub fn unique() -> String {
    static N: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    format!("{}-{n}", std::process::id())
}

pub fn run(cmd: &mut Command) -> bool {
    match cmd.output() {
        Ok(o) if o.status.success() => true,
        Ok(o) => {
            eprintln!("command failed: {:?}\n{}", cmd, String::from_utf8_lossy(&o.stderr));
            false
        }
        Err(e) => {
            eprintln!("failed to run {:?}: {e}", cmd);
            false
        }
    }
}

/// Bytes per sample and chroma subsampling of a pixel format.
pub fn format_info(pix_fmt: &str) -> (usize, u32, u32) {
    let bps = if pix_fmt.contains("p10") || pix_fmt.contains("p12") { 2 } else { 1 };
    let (sx, sy) = if pix_fmt.starts_with("yuv420") {
        (1, 1)
    } else if pix_fmt.starts_with("yuv422") {
        (1, 0)
    } else if pix_fmt.starts_with("yuv440") {
        (0, 1)
    } else {
        (0, 0)
    };
    (bps, sx, sy)
}

/// Reference decode of `ivf` with ffmpeg (native vp9 decoder, every frame, no fps conversion).
pub fn reference_decode(ivf: &Path, yuv: &Path, pix_fmt: &str) -> bool {
    let Some(ff) = ffmpeg() else { return false };
    let tmp = yuv.with_extension(format!("tmp.{}.yuv", unique()));
    let mut c = Command::new(&ff);
    c.args(["-hide_banner", "-loglevel", "error", "-y", "-c:v", "vp9", "-i"]).arg(ivf);
    c.args(["-fps_mode", "passthrough", "-noautoscale", "-f", "rawvideo", "-pix_fmt", pix_fmt]).arg(&tmp);
    if !run(&mut c) {
        return false;
    }
    std::fs::rename(&tmp, yuv).is_ok() || yuv.exists()
}

/// Generate (if needed) the fixture stream and its ffmpeg reference decode.
pub fn ensure(f: &Fixture) -> Option<(PathBuf, PathBuf)> {
    let Some(ff) = ffmpeg() else {
        eprintln!("SKIP {}: ffmpeg not found", f.name);
        return None;
    };
    let dir = fixtures_dir();
    let ivf = dir.join(format!("{}.ivf", f.name));
    let yuv = dir.join(format!("{}.yuv", f.name));
    if !ivf.exists() {
        let tmp = dir.join(format!("{}.tmp.{}.ivf", f.name, unique()));
        let mut vf = format!("{}=size={}x{}:rate=25,format={}", f.source, f.width, f.height, f.pix_fmt);
        if !f.filter.is_empty() {
            vf.push(',');
            vf.push_str(f.filter);
        }
        let two_pass = f.args.first() == Some(&"2pass");
        let args: Vec<&str> = f.args.iter().copied().filter(|a| *a != "2pass").collect();
        let log = dir.join(format!("{}.{}.passlog", f.name, unique()));
        let enc = |pass: Option<u32>, out: &Path| {
            let mut c = Command::new(&ff);
            c.args(["-hide_banner", "-loglevel", "error", "-y", "-f", "lavfi", "-i", &vf]);
            c.args(["-frames:v", &f.frames.to_string(), "-c:v", "libvpx-vp9", "-pix_fmt", f.pix_fmt]);
            c.args(&args);
            if let Some(p) = pass {
                c.args(["-pass", &p.to_string(), "-passlogfile"]).arg(&log);
            }
            if pass == Some(1) {
                c.args(["-f", "null", "-"]);
            } else {
                c.args(["-f", "ivf"]).arg(out);
            }
            c
        };
        if two_pass && !run(&mut enc(Some(1), &tmp)) {
            eprintln!("SKIP {}: first pass failed", f.name);
            return None;
        }
        let mut c = enc(if two_pass { Some(2) } else { None }, &tmp);
        if !run(&mut c) {
            let _ = std::fs::remove_file(&tmp);
            eprintln!("SKIP {}: libvpx-vp9 encode failed", f.name);
            return None;
        }
        let _ = std::fs::remove_file(format!("{}-0.log", log.display()));
        if std::fs::rename(&tmp, &ivf).is_err() && !ivf.exists() {
            panic!("could not store fixture {}", ivf.display());
        }
    }
    if !yuv.exists() {
        assert!(reference_decode(&ivf, &yuv, f.pix_fmt), "reference decode failed for {}", f.name);
    }
    Some((ivf, yuv))
}

/// Frames of an IVF file (32-byte file header, then 12-byte frame headers).
pub fn ivf_frames(data: &[u8]) -> Vec<&[u8]> {
    let mut out = Vec::new();
    if data.len() < 32 || &data[..4] != b"DKIF" {
        return out;
    }
    let hl = u16::from_le_bytes([data[6], data[7]]) as usize;
    let mut p = hl.max(32);
    while p + 12 <= data.len() {
        let sz = u32::from_le_bytes(data[p..p + 4].try_into().unwrap()) as usize;
        p += 12;
        if p + sz > data.len() {
            break;
        }
        out.push(&data[p..p + sz]);
        p += sz;
    }
    out
}

/// Write an IVF file.
pub fn write_ivf(path: &Path, w: u16, h: u16, frames: &[Vec<u8>]) {
    let mut d = Vec::new();
    d.extend(b"DKIF");
    d.extend(0u16.to_le_bytes());
    d.extend(32u16.to_le_bytes());
    d.extend(b"VP90");
    d.extend(w.to_le_bytes());
    d.extend(h.to_le_bytes());
    d.extend(25u32.to_le_bytes());
    d.extend(1u32.to_le_bytes());
    d.extend((frames.len() as u32).to_le_bytes());
    d.extend(0u32.to_le_bytes());
    for (i, f) in frames.iter().enumerate() {
        d.extend((f.len() as u32).to_le_bytes());
        d.extend((i as u64).to_le_bytes());
        d.extend(f);
    }
    std::fs::write(path, d).unwrap();
}

pub type DecodeFailure = (usize, filmcraft_vp9::Error, Vec<Picture>);

/// Decode an IVF file with `threads` workers (0 = default), pts = frame index.
pub fn decode_file_threads(path: &Path, threads: usize) -> Result<Vec<Picture>, DecodeFailure> {
    let data = std::fs::read(path).unwrap();
    let mut dec = if threads == 0 { Decoder::new() } else { Decoder::with_threads(threads) };
    let mut out = Vec::new();
    for (i, f) in ivf_frames(&data).into_iter().enumerate() {
        match dec.decode(f, i as i64) {
            Ok(p) => out.extend(p),
            Err(e) => return Err((i, e, out)),
        }
    }
    out.extend(dec.flush());
    Ok(out)
}

/// Frame size in bytes of a raw picture.
pub fn raw_size(w: usize, h: usize, pix_fmt: &str) -> usize {
    let (bps, sx, sy) = format_info(pix_fmt);
    let (cw, ch) = ((w + sx as usize) >> sx, (h + sy as usize) >> sy);
    (w * h + 2 * cw * ch) * bps
}

/// Compare decoded pictures with a raw reference with frames of possibly varying sizes (sizes
/// are taken from the decoded pictures). Reports the first mismatch.
pub fn compare(pics: &[Picture], reference: &[u8], pix_fmt: &str) -> Result<(), String> {
    let (bps, _, _) = format_info(pix_fmt);
    let mut off = 0usize;
    for (i, p) in pics.iter().enumerate() {
        let (w, h) = (p.width as usize, p.height as usize);
        let (cw, ch) = (p.chroma_width as usize, p.chroma_height as usize);
        let fsize = raw_size(w, h, pix_fmt);
        if off + fsize > reference.len() {
            return Err(format!("decoder produced {} frames, reference has fewer (at frame {i})", pics.len()));
        }
        let f = &reference[off..off + fsize];
        off += fsize;
        let sample = |buf: &[u8], k: usize| if bps == 2 { u16::from_le_bytes([buf[2 * k], buf[2 * k + 1]]) } else { buf[k] as u16 };
        let (ys, cs) = (w * h * bps, cw * ch * bps);
        let planes = [("Y", &p.y, &f[..ys], w, h), ("U", &p.u, &f[ys..ys + cs], cw, ch), ("V", &p.v, &f[ys + cs..], cw, ch)];
        for (name, got, exp, pw, ph) in planes {
            let mut first = None;
            let mut count = 0usize;
            let mut maxd = 0i32;
            for y in 0..ph {
                for x in 0..pw {
                    let (a, b) = (got.get(y * pw + x), sample(exp, y * pw + x));
                    if a != b {
                        count += 1;
                        maxd = maxd.max((a as i32 - b as i32).abs());
                        if first.is_none() {
                            first = Some((x, y, a, b));
                        }
                    }
                }
            }
            if let Some((x, y, a, b)) = first {
                return Err(format!(
                    "frame {i} (pts {}, {w}x{h}) plane {name}: first mismatch at ({x},{y}) [SB64 of luma ~({},{})]: got {a} want {b}; {count} samples differ, max diff {maxd}",
                    p.pts,
                    x / 64,
                    y / 64
                ));
            }
        }
    }
    if off != reference.len() {
        return Err(format!("decoder produced {} frames, reference has more data", pics.len()));
    }
    Ok(())
}

/// Per-frame MD5-like digest is not needed: comparison is sample exact. Full check of one
/// fixture: generate, decode (single- and multi-threaded), compare. Ok(false) when skipped.
pub fn check_fixture(name: &str) -> Result<bool, String> {
    let f = fixture(name);
    let Some((ivf, yuv)) = ensure(f) else { return Ok(false) };
    let reference = std::fs::read(&yuv).unwrap();
    check_files(name, &ivf, &reference, f.pix_fmt)?;
    Ok(true)
}

pub fn check_files(name: &str, ivf: &Path, reference: &[u8], pix_fmt: &str) -> Result<(), String> {
    // Single-threaded, frame threads with a small pool, and every core.
    for threads in [1, 3, 0] {
        let pics = match decode_file_threads(ivf, threads) {
            Ok(p) => p,
            Err((fr, e, partial)) => {
                let cmp = compare(&partial, reference, pix_fmt).err().unwrap_or_default();
                return Err(format!("{name} (threads {threads}): decode error at chunk {fr}: {e} (after {} pictures) {cmp}", partial.len()));
            }
        };
        compare(&pics, reference, pix_fmt).map_err(|e| format!("{name} (threads {threads}): {e}"))?;
        if pics.iter().any(|p| p.draft) {
            return Err(format!("{name} (threads {threads}): picture flagged draft without draft mode"));
        }
    }
    Ok(())
}
