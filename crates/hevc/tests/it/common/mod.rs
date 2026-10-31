//! Test fixture generation (ffmpeg + libx265 / VideoToolbox as external oracle and encoder) and
//! comparison helpers.
#![allow(dead_code)]

use filmcraft_hevc::{ChromaFormat, Decoder, Picture};
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
    /// Extra video filter appended after the source (e.g. noise, fade).
    pub filter: &'static str,
    /// 10 (yuv420p10le) or 8.
    pub bit_depth: u32,
    /// 4:2:0 or 4:2:2.
    pub chroma: ChromaFormat,
    /// Encoder arguments (libx265 unless `-c:v` is given).
    pub args: &'static [&'static str],
}

const NOISE: &str = "noise=alls=12:allf=t+u";

macro_rules! fx {
    ($name:expr, $src:expr, $w:expr, $h:expr, $n:expr, $filter:expr, $bd:expr, [$($a:expr),* $(,)?]) => {
        Fixture { name: $name, source: $src, width: $w, height: $h, frames: $n, filter: $filter, bit_depth: $bd, chroma: ChromaFormat::C420, args: &[$($a),*] }
    };
    ($name:expr, $src:expr, $w:expr, $h:expr, $n:expr, $filter:expr, $bd:expr, 422, [$($a:expr),* $(,)?]) => {
        Fixture { name: $name, source: $src, width: $w, height: $h, frames: $n, filter: $filter, bit_depth: $bd, chroma: ChromaFormat::C422, args: &[$($a),*] }
    };
}

pub const FIXTURES: &[Fixture] = &[
    // intra only
    fx!("intra_main", "testsrc2", 352, 288, 3, NOISE, 8, ["-x265-params", "keyint=1:log-level=error"]),
    fx!("intra_main10", "testsrc2", 352, 288, 3, NOISE, 10, ["-x265-params", "keyint=1:log-level=error"]),
    fx!("intra_nosao_nodbk", "testsrc2", 352, 288, 2, NOISE, 8, ["-x265-params", "keyint=1:no-sao=1:no-deblock=1:log-level=error"]),
    // presets (WPP is on by default in x265)
    fx!("ultrafast", "testsrc2", 352, 288, 20, NOISE, 8, ["-preset", "ultrafast", "-x265-params", "log-level=error"]),
    fx!("veryfast", "testsrc2", 352, 288, 20, NOISE, 8, ["-preset", "veryfast", "-x265-params", "log-level=error"]),
    fx!("medium", "testsrc2", 352, 288, 20, NOISE, 8, ["-preset", "medium", "-x265-params", "log-level=error"]),
    fx!("slow", "mandelbrot", 352, 288, 16, NOISE, 8, ["-preset", "slow", "-x265-params", "log-level=error"]),
    fx!("medium_main10", "testsrc2", 352, 288, 20, NOISE, 10, ["-preset", "medium", "-x265-params", "log-level=error"]),
    fx!("slow_main10", "mandelbrot", 352, 288, 12, NOISE, 10, ["-preset", "slow", "-x265-params", "log-level=error"]),
    // coding tools
    fx!("p_only", "testsrc2", 352, 288, 20, NOISE, 8, ["-x265-params", "bframes=0:log-level=error"]),
    fx!("bframes", "testsrc2", 352, 288, 30, NOISE, 8, ["-x265-params", "bframes=8:b-pyramid=1:ref=4:log-level=error"]),
    fx!("no_sao", "testsrc2", 352, 288, 16, NOISE, 8, ["-x265-params", "no-sao=1:log-level=error"]),
    fx!("no_deblock", "testsrc2", 352, 288, 16, NOISE, 8, ["-x265-params", "no-deblock=1:log-level=error"]),
    fx!("deblock_m3_3", "testsrc2", 352, 288, 12, NOISE, 8, ["-x265-params", "deblock=-3,3:log-level=error"]),
    fx!("no_wpp", "testsrc2", 352, 288, 12, NOISE, 8, ["-x265-params", "no-wpp=1:log-level=error"]),
    fx!("rect_amp", "mandelbrot", 352, 288, 16, NOISE, 8, ["-x265-params", "rect=1:amp=1:log-level=error"]),
    fx!("tskip", "testsrc2", 352, 288, 12, NOISE, 8, ["-x265-params", "tskip=1:log-level=error"]),
    fx!("weightp", "testsrc2", 352, 288, 30, "fade=in:0:25", 8, ["-x265-params", "weightp=1:weightb=1:bframes=3:log-level=error"]),
    fx!("weightp_main10", "testsrc2", 352, 288, 20, "fade=in:0:18", 10, ["-x265-params", "weightp=1:weightb=1:log-level=error"]),
    fx!("crf0", "mandelbrot", 176, 144, 6, NOISE, 8, ["-crf", "0", "-x265-params", "log-level=error"]),
    fx!("crf51", "testsrc2", 352, 288, 12, NOISE, 8, ["-crf", "51", "-x265-params", "log-level=error"]),
    fx!("qp1_main10", "mandelbrot", 176, 144, 4, NOISE, 10, ["-x265-params", "qp=1:log-level=error"]),
    fx!("lossless", "testsrc2", 176, 144, 6, NOISE, 8, ["-x265-params", "lossless=1:log-level=error"]),
    fx!("cu_lossless", "testsrc2", 176, 144, 6, NOISE, 8, ["-x265-params", "cu-lossless=1:log-level=error"]),
    fx!("slices4", "testsrc2", 352, 288, 12, NOISE, 8, ["-x265-params", "slices=4:log-level=error"]),
    fx!("scaling_list", "testsrc2", 352, 288, 12, NOISE, 8, ["-x265-params", "scaling-list=default:log-level=error"]),
    fx!("ctu16", "testsrc2", 352, 288, 12, NOISE, 8, ["-x265-params", "ctu=16:log-level=error"]),
    fx!("ctu32_tu4", "testsrc2", 352, 288, 12, NOISE, 8, ["-x265-params", "ctu=32:tu-intra-depth=4:tu-inter-depth=4:max-tu-size=16:log-level=error"]),
    fx!("min_cu16", "testsrc2", 352, 288, 12, NOISE, 8, ["-x265-params", "min-cu-size=16:rect=1:amp=1:log-level=error"]),
    fx!("no_signhide", "testsrc2", 352, 288, 12, NOISE, 8, ["-x265-params", "no-signhide=1:log-level=error"]),
    fx!("constrained_intra", "testsrc2", 352, 288, 12, NOISE, 8, ["-x265-params", "constrained-intra=1:log-level=error"]),
    fx!("no_tmvp", "testsrc2", 352, 288, 12, NOISE, 8, ["-x265-params", "no-temporal-mvp=1:log-level=error"]),
    fx!("no_strong_intra", "testsrc2", 352, 288, 6, NOISE, 8, ["-x265-params", "no-strong-intra-smoothing=1:keyint=1:log-level=error"]),
    fx!("max_merge1", "testsrc2", 352, 288, 12, NOISE, 8, ["-x265-params", "max-merge=1:log-level=error"]),
    fx!("open_gop", "testsrc2", 352, 288, 40, NOISE, 8, ["-x265-params", "keyint=12:min-keyint=12:open-gop=1:bframes=3:log-level=error"]),
    fx!("keyint5_idr", "testsrc2", 352, 288, 20, NOISE, 8, ["-x265-params", "keyint=5:no-open-gop=1:log-level=error"]),
    fx!("odd_1918x1078", "testsrc2", 1918, 1078, 3, NOISE, 8, ["-preset", "veryfast", "-x265-params", "log-level=error"]),
    fx!("hd_1080p", "testsrc2", 1920, 1080, 3, "", 8, ["-preset", "fast", "-x265-params", "log-level=error"]),
    fx!("uhd_4k_main10", "testsrc2", 3840, 2160, 2, "", 10, ["-preset", "ultrafast", "-x265-params", "log-level=error"]),
    // 4:2:2 (Main 4:2:2 10 / RExt profile without range-extension tools: libx265 with yuv422p / yuv422p10le)
    fx!("intra_422", "testsrc2", 352, 288, 3, NOISE, 8, 422, ["-x265-params", "keyint=1:log-level=error"]),
    fx!("intra_422_10", "testsrc2", 352, 288, 3, NOISE, 10, 422, ["-x265-params", "keyint=1:log-level=error"]),
    fx!("intra_422_10_nosao_nodbk", "testsrc2", 352, 288, 2, NOISE, 10, 422, ["-x265-params", "keyint=1:no-sao=1:no-deblock=1:log-level=error"]),
    fx!("medium_422_10", "testsrc2", 352, 288, 20, NOISE, 10, 422, ["-preset", "medium", "-x265-params", "log-level=error"]),
    fx!("slow_422", "mandelbrot", 352, 288, 12, NOISE, 8, 422, ["-preset", "slow", "-x265-params", "log-level=error"]),
    fx!("p_only_422_10", "testsrc2", 352, 288, 20, NOISE, 10, 422, ["-x265-params", "bframes=0:log-level=error"]),
    fx!("bframes_422_10", "testsrc2", 352, 288, 30, NOISE, 10, 422, ["-x265-params", "bframes=8:b-pyramid=1:ref=4:log-level=error"]),
    fx!("no_sao_422_10", "testsrc2", 352, 288, 12, NOISE, 10, 422, ["-x265-params", "no-sao=1:log-level=error"]),
    fx!("deblock_m3_3_422_10", "testsrc2", 352, 288, 12, NOISE, 10, 422, ["-x265-params", "deblock=-3,3:log-level=error"]),
    fx!("rect_amp_422_10", "mandelbrot", 352, 288, 12, NOISE, 10, 422, ["-x265-params", "rect=1:amp=1:log-level=error"]),
    fx!("tskip_422_10", "testsrc2", 352, 288, 12, NOISE, 10, 422, ["-x265-params", "tskip=1:log-level=error"]),
    fx!("weightp_422_10", "testsrc2", 352, 288, 20, "fade=in:0:18", 10, 422, ["-x265-params", "weightp=1:weightb=1:log-level=error"]),
    fx!("qp_422_10", "mandelbrot", 352, 288, 8, NOISE, 10, 422, ["-x265-params", "aq-mode=3:cutree=1:log-level=error"]),
    fx!("crf51_422_10", "testsrc2", 352, 288, 8, NOISE, 10, 422, ["-crf", "51", "-x265-params", "log-level=error"]),
    fx!("lossless_422_10", "testsrc2", 176, 144, 4, NOISE, 10, 422, ["-x265-params", "lossless=1:log-level=error"]),
    fx!(
        "ctu32_tu4_422_10",
        "testsrc2",
        352,
        288,
        12,
        NOISE,
        10,
        422,
        ["-x265-params", "ctu=32:tu-intra-depth=4:tu-inter-depth=4:max-tu-size=16:log-level=error"]
    ),
    fx!("ctu16_422_10", "testsrc2", 352, 288, 8, NOISE, 10, 422, ["-x265-params", "ctu=16:log-level=error"]),
    fx!("scaling_list_422_10", "testsrc2", 352, 288, 8, NOISE, 10, 422, ["-x265-params", "scaling-list=default:log-level=error"]),
    fx!("constrained_intra_422_10", "testsrc2", 352, 288, 8, NOISE, 10, 422, ["-x265-params", "constrained-intra=1:log-level=error"]),
    fx!("slices4_422_10", "testsrc2", 352, 288, 8, NOISE, 10, 422, ["-x265-params", "slices=4:log-level=error"]),
    fx!("odd_1918x1078_422_10", "testsrc2", 1918, 1078, 3, NOISE, 10, 422, ["-preset", "veryfast", "-x265-params", "log-level=error"]),
    fx!("uhd_4k_422_10", "testsrc2", 3840, 2160, 2, "", 10, 422, ["-preset", "ultrafast", "-x265-params", "log-level=error"]),
    // performance reference streams (tests/it/perf.rs)
    fx!("bench_1080p", "testsrc2", 1920, 1080, 60, "noise=alls=3:allf=t", 8, ["-preset", "medium", "-crf", "24", "-x265-params", "log-level=error"]),
    fx!("bench_4k", "testsrc2", 3840, 2160, 20, "noise=alls=3:allf=t", 10, ["-preset", "fast", "-crf", "26", "-x265-params", "log-level=error"]),
    // Apple VideoToolbox (hardware encoder, macOS only)
    fx!("vt_main", "testsrc2", 640, 360, 20, NOISE, 8, ["-c:v", "hevc_videotoolbox", "-b:v", "2M"]),
    fx!("vt_main10", "mandelbrot", 640, 360, 20, NOISE, 10, ["-c:v", "hevc_videotoolbox", "-profile:v", "main10", "-b:v", "2M"]),
];

pub fn fixture(name: &str) -> &'static Fixture {
    FIXTURES.iter().find(|f| f.name == name).unwrap_or_else(|| panic!("unknown fixture {name}"))
}

/// ffmpeg (see `filmcraft_testkit::oracle`).
pub fn ffmpeg() -> Option<PathBuf> {
    filmcraft_testkit::ffmpeg()
}

pub fn fixtures_dir() -> PathBuf {
    filmcraft_testkit::fixtures_dir("hevc")
}

fn run(cmd: &mut Command) -> bool {
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

pub fn pix_fmt(bd: u32, chroma: ChromaFormat) -> &'static str {
    match (chroma, bd > 8) {
        (ChromaFormat::C420, false) => "yuv420p",
        (ChromaFormat::C420, true) => "yuv420p10le",
        (ChromaFormat::C422, false) => "yuv422p",
        (ChromaFormat::C422, true) => "yuv422p10le",
    }
}

/// Bytes of one raw frame of the fixture as ffmpeg writes it.
pub fn frame_size(f: &Fixture) -> usize {
    let (w, h) = (f.width as usize, f.height as usize);
    let bps = if f.bit_depth > 8 { 2 } else { 1 };
    let (cw, ch) = (w.div_ceil(f.chroma.sub_width()), h.div_ceil(f.chroma.sub_height()));
    (w * h + 2 * cw * ch) * bps
}

/// The pixel format ffprobe reports for the video stream in `path`.
fn stream_pix_fmt(ffprobe: &Path, path: &Path) -> Option<String> {
    let mut c = Command::new(ffprobe);
    c.args(["-v", "error", "-select_streams", "v:0", "-show_entries", "stream=pix_fmt", "-of", "csv=p=0"]).arg(path);
    let o = c.output().ok().filter(|o| o.status.success())?;
    Some(String::from_utf8_lossy(&o.stdout).trim().to_owned())
}

/// Generate (if needed) the fixture stream and its ffmpeg reference decode.
/// Returns None (with a message) when ffmpeg or the encoder is unavailable.
pub fn ensure(f: &Fixture) -> Option<(PathBuf, PathBuf)> {
    let ff = filmcraft_testkit::ffmpeg_or_skip(f.name)?;
    let dir = fixtures_dir();
    let hevc = dir.join(format!("{}.hevc", f.name));
    let yuv = dir.join(format!("{}.yuv", f.name));
    if !hevc.exists() {
        let tmp = filmcraft_testkit::temp_path(&hevc);
        let mut vf = format!("{}=size={}x{}:rate=25,format={}", f.source, f.width, f.height, pix_fmt(f.bit_depth, f.chroma));
        if !f.filter.is_empty() {
            vf.push(',');
            vf.push_str(f.filter);
        }
        let mut c = Command::new(&ff);
        c.args(["-hide_banner", "-loglevel", "error", "-y", "-f", "lavfi", "-i", &vf]);
        c.args(["-frames:v", &f.frames.to_string()]);
        if !f.args.contains(&"-c:v") {
            c.args(["-c:v", "libx265"]);
        }
        c.args(["-pix_fmt", pix_fmt(f.bit_depth, f.chroma)]);
        c.args(f.args);
        c.args(["-f", "hevc"]).arg(&tmp);
        if !run(&mut c) {
            // Platform encoders (VideoToolbox) are optional; libx265 fixtures must generate.
            assert!(f.args.contains(&"-c:v"), "fixture generation failed for {}", f.name);
            let _ = std::fs::remove_file(&tmp);
            eprintln!("SKIPPED ({}): encoder unavailable", f.name);
            return None;
        }
        filmcraft_testkit::fixtures::publish(&tmp, &hevc).unwrap();
    }
    if f.bit_depth > 8 {
        // An 8-bit-only libx265 (Arch Linux ARM's) encodes 8-bit without an error when asked for
        // 10, and the reference decode scales that up to 10 bits, so no sample would match.
        let probe = filmcraft_testkit::ffprobe_or_skip(f.name)?;
        let got = stream_pix_fmt(&probe, &hevc);
        if got.as_deref() != Some(pix_fmt(f.bit_depth, f.chroma)) {
            let _ = std::fs::remove_file(&hevc);
            let _ = std::fs::remove_file(&yuv);
            let reason = format!("the encoder made {} instead of {}", got.as_deref().unwrap_or("no stream"), pix_fmt(f.bit_depth, f.chroma));
            filmcraft_testkit::skip(f.name, &format!("{reason} (libx265 built for 8 bits only?)"));
            return None;
        }
    }
    if !yuv.exists() {
        let tmp = filmcraft_testkit::temp_path(&yuv);
        let mut c = Command::new(&ff);
        c.args(["-hide_banner", "-loglevel", "error", "-y", "-i"]).arg(&hevc);
        c.args(filmcraft_testkit::fps_passthrough_args(&ff));
        c.args(["-f", "rawvideo", "-pix_fmt", pix_fmt(f.bit_depth, f.chroma)]).arg(&tmp);
        assert!(run(&mut c), "reference decode failed for {}", f.name);
        filmcraft_testkit::fixtures::publish(&tmp, &yuv).unwrap();
    }
    Some((hevc, yuv))
}

fn is_vcl(t: u8) -> bool {
    t < 32
}

/// Split an Annex-B stream into access units: a new AU starts at an AUD / VPS / SPS / PPS / prefix SEI
/// NAL after a VCL NAL, or at a VCL NAL with first_slice_segment_in_pic_flag = 1.
pub fn split_access_units(data: &[u8]) -> Vec<&[u8]> {
    let mut starts = Vec::new();
    let mut i = 0;
    while i + 3 <= data.len() {
        if data[i] == 0 && data[i + 1] == 0 && data[i + 2] == 1 {
            let s = if i > 0 && data[i - 1] == 0 { i - 1 } else { i };
            starts.push((s, i + 3));
            i += 3;
        } else {
            i += 1;
        }
    }
    let mut aus = Vec::new();
    let mut au_start = 0usize;
    let mut seen_vcl = false;
    for &(sc, payload) in &starts {
        if payload + 2 >= data.len() {
            continue;
        }
        let t = (data[payload] >> 1) & 0x3f;
        let first_slice = is_vcl(t) && data[payload + 2] & 0x80 != 0;
        let boundary = seen_vcl && (matches!(t, 32..=35 | 39) || first_slice);
        if boundary {
            aus.push(&data[au_start..sc]);
            au_start = sc;
            seen_vcl = false;
        }
        if is_vcl(t) {
            seen_vcl = true;
        }
    }
    if au_start < data.len() {
        aus.push(&data[au_start..]);
    }
    aus
}

/// Failed decode: (access unit index, error, pictures output before the error).
pub type DecodeFailure = (usize, filmcraft_hevc::Error, Vec<Picture>);

/// Decode with `threads` worker threads (0 = decoder default), pts = access unit index.
pub fn decode_file_threads(path: &Path, threads: usize) -> Result<Vec<Picture>, DecodeFailure> {
    decode_file_opts(path, threads, false)
}

/// [`decode_file_threads`] with draft mode on or off.
pub fn decode_file_opts(path: &Path, threads: usize, draft: bool) -> Result<Vec<Picture>, DecodeFailure> {
    let data = std::fs::read(path).unwrap();
    let mut dec = if threads == 0 { Decoder::new() } else { Decoder::with_threads(threads) };
    dec.set_draft(draft);
    let mut out = Vec::new();
    for (i, au) in split_access_units(&data).into_iter().enumerate() {
        match dec.decode(au, i as i64) {
            Ok(p) => out.extend(p),
            Err(e) => return Err((i, e, out)),
        }
    }
    out.extend(dec.flush());
    if let Some(e) = dec.take_error() {
        return Err((usize::MAX, e, out));
    }
    Ok(out)
}

/// Compare decoded pictures with a raw planar reference (yuv420p / yuv420p10le / yuv422p /
/// yuv422p10le); returns a diagnostic on the first mismatch (frame, plane, sample position, CTB
/// (64x64) and 8x8 block).
pub fn compare(pics: &[Picture], reference: &[u8], w: usize, h: usize, bd: u32, chroma: ChromaFormat) -> Result<(), String> {
    let bps = if bd > 8 { 2 } else { 1 };
    let (sw, sh) = (chroma.sub_width(), chroma.sub_height());
    let (cw, ch) = (w.div_ceil(sw), h.div_ceil(sh));
    let fsize = (w * h + 2 * cw * ch) * bps;
    let nref = reference.len() / fsize;
    for (i, p) in pics.iter().enumerate() {
        if i >= nref {
            return Err(format!("decoder produced {} frames, reference has {}", pics.len(), nref));
        }
        if p.width as usize != w || p.height as usize != h {
            return Err(format!("frame {i}: size {}x{} != {}x{}", p.width, p.height, w, h));
        }
        if p.chroma != chroma || p.chroma_width as usize != cw || p.chroma_height as usize != ch {
            return Err(format!("frame {i}: chroma {:?} {}x{} != {chroma:?} {cw}x{ch}", p.chroma, p.chroma_width, p.chroma_height));
        }
        let f = &reference[i * fsize..(i + 1) * fsize];
        let sample = |buf: &[u8], k: usize| if bps == 2 { u16::from_le_bytes([buf[2 * k], buf[2 * k + 1]]) } else { buf[k] as u16 };
        let (ys, cs) = (w * h * bps, cw * ch * bps);
        let planes = [("Y", &p.y, &f[..ys], w, h, (1, 1)), ("U", &p.u, &f[ys..ys + cs], cw, ch, (sw, sh)), ("V", &p.v, &f[ys + cs..], cw, ch, (sw, sh))];
        for (name, got, exp, pw, ph, (subx, suby)) in planes {
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
                let (lx, ly) = (x * subx, y * suby);
                return Err(format!(
                    "frame {i} (poc {}, pts {}) plane {name}: first mismatch at ({x},{y}) [luma ({lx},{ly}), CTB64 ({},{}), 8x8 ({},{})]: got {a} want {b}; {count} samples differ, max diff {maxd}",
                    p.poc,
                    p.pts,
                    lx / 64,
                    ly / 64,
                    lx / 8,
                    ly / 8
                ));
            }
        }
    }
    if pics.len() != nref {
        return Err(format!("decoder produced {} frames, reference has {}", pics.len(), nref));
    }
    Ok(())
}

/// Full check of one fixture: generate, decode (single- and multi-threaded), compare. Ok(false) when skipped.
pub fn check_fixture(name: &str) -> Result<bool, String> {
    let f = fixture(name);
    let Some((hevc, yuv)) = ensure(f) else { return Ok(false) };
    let reference = std::fs::read(&yuv).unwrap();
    // Single-threaded, frame threads with a small pool, and every core.
    for threads in [1, 3, 0] {
        let pics = match decode_file_threads(&hevc, threads) {
            Ok(p) => p,
            Err((au, e, partial)) => {
                let cmp = compare(&partial, &reference, f.width as usize, f.height as usize, f.bit_depth, f.chroma).err().unwrap_or_default();
                return Err(format!("{name} (threads {threads}): decode error at access unit {au}: {e} (after {} pictures) {cmp}", partial.len()));
            }
        };
        compare(&pics, &reference, f.width as usize, f.height as usize, f.bit_depth, f.chroma).map_err(|e| format!("{name} (threads {threads}): {e}"))?;
        check_pts(&pics).map_err(|e| format!("{name} (threads {threads}): {e}"))?;
        if pics.iter().any(|p| p.draft) {
            return Err(format!("{name} (threads {threads}): picture flagged draft without draft mode"));
        }
    }
    Ok(true)
}

/// Output pts values must be distinct and follow POC order between IRAP pictures.
pub fn check_pts(pics: &[Picture]) -> Result<(), String> {
    let mut seen = std::collections::HashSet::new();
    for p in pics {
        if !seen.insert(p.pts) {
            return Err(format!("duplicate pts {} in output", p.pts));
        }
    }
    for w in pics.windows(2) {
        if !w[1].key && w[1].poc <= w[0].poc {
            return Err(format!("output POC order violated: {} then {}", w[0].poc, w[1].poc));
        }
    }
    Ok(())
}
