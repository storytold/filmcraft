//! Test fixture generation (ffmpeg/libx264 as an external oracle) and comparison helpers.
#![allow(dead_code)]

use filmcraft_h264::{Decoder, Picture};
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
    /// Encoder arguments.
    pub args: &'static [&'static str],
}

const NOISE: &str = "noise=alls=12:allf=t+u";

pub const FIXTURES: &[Fixture] = &[
    // Apple VideoToolbox (hardware) encoder streams: different encoder design, generated on macOS only
    Fixture {
        name: "vt_high",
        source: "testsrc2",
        width: 640,
        height: 360,
        frames: 30,
        filter: NOISE,
        args: &["-c:v", "h264_videotoolbox", "-profile:v", "high", "-b:v", "3M"],
    },
    Fixture {
        name: "vt_main",
        source: "mandelbrot",
        width: 640,
        height: 360,
        frames: 30,
        filter: NOISE,
        args: &["-c:v", "h264_videotoolbox", "-profile:v", "main", "-b:v", "2M"],
    },
    Fixture {
        name: "vt_baseline",
        source: "testsrc2",
        width: 320,
        height: 240,
        frames: 30,
        filter: NOISE,
        args: &["-c:v", "h264_videotoolbox", "-profile:v", "baseline", "-b:v", "1M"],
    },
    Fixture {
        name: "vt_high_1080p",
        source: "testsrc2",
        width: 1920,
        height: 1080,
        frames: 10,
        filter: "",
        args: &["-c:v", "h264_videotoolbox", "-profile:v", "high", "-b:v", "8M"],
    },
    // performance reference stream (see tests/perf.rs)
    Fixture {
        name: "bench_1080p",
        source: "testsrc2",
        width: 1920,
        height: 1080,
        frames: 120,
        filter: "noise=alls=3:allf=t",
        args: &["-preset", "medium", "-crf", "20"],
    },
    Fixture {
        name: "intra_cavlc",
        source: "testsrc2",
        width: 176,
        height: 144,
        frames: 3,
        filter: "",
        args: &["-profile:v", "baseline", "-x264-params", "keyint=1:no-deblock=1"],
    },
    Fixture {
        name: "intra_cavlc_noise",
        source: "mandelbrot",
        width: 352,
        height: 288,
        frames: 3,
        filter: NOISE,
        args: &["-profile:v", "baseline", "-x264-params", "keyint=1:no-deblock=1"],
    },
    Fixture {
        name: "intra_cavlc_8x8",
        source: "testsrc2",
        width: 352,
        height: 288,
        frames: 3,
        filter: NOISE,
        args: &["-profile:v", "high", "-x264-params", "keyint=1:no-deblock=1:cabac=0"],
    },
    Fixture {
        name: "intra_cavlc_cqm",
        source: "testsrc2",
        width: 352,
        height: 288,
        frames: 3,
        filter: NOISE,
        args: &["-profile:v", "high", "-x264-params", "keyint=1:no-deblock=1:cabac=0:cqm=jvt"],
    },
    Fixture {
        name: "p_cavlc_nodeblock",
        source: "testsrc2",
        width: 352,
        height: 288,
        frames: 20,
        filter: NOISE,
        args: &["-profile:v", "baseline", "-x264-params", "no-deblock=1:ref=3"],
    },
    Fixture {
        name: "baseline_qcif",
        source: "testsrc2",
        width: 176,
        height: 144,
        frames: 30,
        filter: "",
        args: &["-profile:v", "baseline", "-x264-params", "keyint=15"],
    },
    Fixture { name: "baseline_cif_noise", source: "mandelbrot", width: 352, height: 288, frames: 25, filter: NOISE, args: &["-profile:v", "baseline"] },
    Fixture {
        name: "cavlc_b",
        source: "testsrc2",
        width: 352,
        height: 288,
        frames: 30,
        filter: NOISE,
        args: &["-profile:v", "high", "-x264-params", "cabac=0:bframes=3"],
    },
    Fixture {
        name: "cavlc_b_temporal",
        source: "testsrc2",
        width: 352,
        height: 288,
        frames: 30,
        filter: NOISE,
        args: &["-profile:v", "main", "-x264-params", "cabac=0:bframes=3:direct=temporal:weightb=1"],
    },
    Fixture {
        name: "cavlc_weightp",
        source: "testsrc2",
        width: 352,
        height: 288,
        frames: 30,
        filter: "fade=in:0:25",
        args: &["-profile:v", "main", "-x264-params", "cabac=0:weightp=2:bframes=2:weightb=1"],
    },
    Fixture { name: "main_cabac_b", source: "testsrc2", width: 352, height: 288, frames: 30, filter: NOISE, args: &["-profile:v", "main"] },
    Fixture { name: "high_720p", source: "smptehdbars", width: 1280, height: 720, frames: 10, filter: NOISE, args: &["-profile:v", "high"] },
    Fixture { name: "high_1080p", source: "testsrc2", width: 1920, height: 1080, frames: 6, filter: "", args: &["-profile:v", "high"] },
    Fixture { name: "crop_1918x1078", source: "testsrc2", width: 1918, height: 1078, frames: 5, filter: NOISE, args: &["-profile:v", "high"] },
    Fixture {
        name: "bpyramid",
        source: "testsrc2",
        width: 352,
        height: 288,
        frames: 30,
        filter: NOISE,
        args: &["-profile:v", "high", "-x264-params", "bframes=3:b-pyramid=normal"],
    },
    Fixture {
        name: "weightp2",
        source: "testsrc2",
        width: 352,
        height: 288,
        frames: 30,
        filter: "fade=in:0:25",
        args: &["-profile:v", "high", "-x264-params", "weightp=2"],
    },
    Fixture {
        name: "weightb",
        source: "testsrc2",
        width: 352,
        height: 288,
        frames: 30,
        filter: "fade=in:0:25",
        args: &["-profile:v", "high", "-x264-params", "weightb=1:bframes=3"],
    },
    Fixture {
        name: "direct_temporal",
        source: "testsrc2",
        width: 352,
        height: 288,
        frames: 30,
        filter: NOISE,
        args: &["-profile:v", "high", "-x264-params", "direct=temporal:bframes=3"],
    },
    Fixture {
        name: "direct_spatial",
        source: "mandelbrot",
        width: 352,
        height: 288,
        frames: 30,
        filter: NOISE,
        args: &["-profile:v", "high", "-x264-params", "direct=spatial:bframes=3"],
    },
    Fixture {
        name: "ref4",
        source: "testsrc2",
        width: 352,
        height: 288,
        frames: 30,
        filter: NOISE,
        args: &["-profile:v", "high", "-x264-params", "ref=4:bframes=2"],
    },
    Fixture {
        name: "no_deblock",
        source: "testsrc2",
        width: 352,
        height: 288,
        frames: 20,
        filter: NOISE,
        args: &["-profile:v", "high", "-x264-params", "no-deblock=1"],
    },
    Fixture {
        name: "deblock_m2_2",
        source: "testsrc2",
        width: 352,
        height: 288,
        frames: 20,
        filter: NOISE,
        args: &["-profile:v", "high", "-x264-params", "deblock=-2,2"],
    },
    Fixture {
        name: "cqm_jvt",
        source: "testsrc2",
        width: 352,
        height: 288,
        frames: 20,
        filter: NOISE,
        args: &["-profile:v", "high", "-x264-params", "cqm=jvt"],
    },
    Fixture {
        name: "slices4",
        source: "testsrc2",
        width: 352,
        height: 288,
        frames: 20,
        filter: NOISE,
        args: &["-profile:v", "high", "-x264-params", "slices=4"],
    },
    Fixture {
        name: "slices4_cavlc",
        source: "testsrc2",
        width: 352,
        height: 288,
        frames: 20,
        filter: NOISE,
        args: &["-profile:v", "baseline", "-x264-params", "slices=4"],
    },
    Fixture {
        name: "keyint10",
        source: "testsrc2",
        width: 352,
        height: 288,
        frames: 30,
        filter: NOISE,
        args: &["-profile:v", "high", "-x264-params", "keyint=10"],
    },
    Fixture {
        name: "open_gop",
        source: "testsrc2",
        width: 352,
        height: 288,
        frames: 40,
        filter: NOISE,
        args: &["-profile:v", "high", "-x264-params", "open-gop=1:keyint=12:bframes=3"],
    },
    Fixture {
        name: "constrained_intra",
        source: "testsrc2",
        width: 352,
        height: 288,
        frames: 20,
        filter: NOISE,
        args: &["-profile:v", "high", "-x264-params", "constrained-intra=1"],
    },
    Fixture {
        name: "no8x8dct",
        source: "testsrc2",
        width: 352,
        height: 288,
        frames: 20,
        filter: NOISE,
        args: &["-profile:v", "high", "-x264-params", "8x8dct=0"],
    },
    Fixture { name: "qp50", source: "testsrc2", width: 352, height: 288, frames: 20, filter: NOISE, args: &["-profile:v", "high", "-qp", "50"] },
    Fixture { name: "qp1", source: "mandelbrot", width: 352, height: 288, frames: 10, filter: NOISE, args: &["-profile:v", "high", "-qp", "1"] },
    Fixture {
        name: "qp1_cavlc",
        source: "mandelbrot",
        width: 176,
        height: 144,
        frames: 10,
        filter: NOISE,
        args: &["-profile:v", "main", "-qp", "1", "-x264-params", "cabac=0"],
    },
    // High 10 (10-bit 4:2:0)
    Fixture {
        name: "high10_cabac",
        source: "testsrc2",
        width: 352,
        height: 288,
        frames: 12,
        filter: NOISE,
        args: &["-pix_fmt", "yuv420p10le", "-preset", "medium", "-crf", "22"],
    },
    Fixture {
        name: "high10_cavlc_8x8",
        source: "mandelbrot",
        width: 256,
        height: 144,
        frames: 8,
        filter: NOISE,
        args: &["-pix_fmt", "yuv420p10le", "-x264-params", "cabac=0:keyint=3"],
    },
    Fixture {
        name: "high10_weightb",
        source: "testsrc2",
        width: 256,
        height: 144,
        frames: 12,
        filter: NOISE,
        args: &["-pix_fmt", "yuv420p10le", "-x264-params", "weightp=2:weightb=1:bframes=3"],
    },
    // Clean (unfiltered) 10-bit: x264's rate control (mb-tree + AQ) walks `mb_qp_delta` over most of
    // its legal range, which a shallow `|delta| <= 26` range check rejects. The `NOISE` fixtures
    // above keep the deltas small, so these two are the regression for that.
    Fixture {
        name: "high10_clean",
        source: "testsrc2",
        width: 352,
        height: 288,
        frames: 12,
        filter: "",
        args: &["-pix_fmt", "yuv420p10le", "-preset", "medium", "-crf", "22"],
    },
    Fixture {
        name: "high422_clean",
        source: "testsrc2",
        width: 352,
        height: 288,
        frames: 12,
        filter: "",
        args: &["-pix_fmt", "yuv422p10le", "-preset", "medium", "-crf", "22"],
    },
    // High 4:2:2 (the Panasonic Lumix MOV mode: H264_422_LongGOP is High 4:2:2 10-bit CABAC)
    Fixture {
        name: "high422_cabac",
        source: "testsrc2",
        width: 352,
        height: 288,
        frames: 12,
        filter: NOISE,
        args: &["-pix_fmt", "yuv422p10le", "-preset", "medium", "-crf", "22"],
    },
    Fixture {
        name: "high422_cavlc",
        source: "testsrc2",
        width: 352,
        height: 288,
        frames: 10,
        filter: NOISE,
        args: &["-pix_fmt", "yuv422p", "-x264-params", "cabac=0:keyint=3"],
    },
    Fixture {
        name: "high422_intra",
        source: "mandelbrot",
        width: 256,
        height: 144,
        frames: 4,
        filter: NOISE,
        args: &["-pix_fmt", "yuv422p10le", "-x264-params", "keyint=1"],
    },
    Fixture {
        name: "high422_weightb",
        source: "testsrc2",
        width: 256,
        height: 144,
        frames: 12,
        filter: NOISE,
        args: &["-pix_fmt", "yuv422p10le", "-x264-params", "weightp=2:weightb=1:bframes=3"],
    },
];

pub fn fixture(name: &str) -> &'static Fixture {
    FIXTURES.iter().find(|f| f.name == name).unwrap_or_else(|| panic!("unknown fixture {name}"))
}

/// ffmpeg (see `filmcraft_testkit::oracle`).
pub fn ffmpeg() -> Option<PathBuf> {
    filmcraft_testkit::ffmpeg()
}

pub fn fixtures_dir() -> PathBuf {
    filmcraft_testkit::fixtures_dir("h264")
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

/// The fixture's input / reference pixel format (`-pix_fmt` in its args, yuv420p when absent).
fn pix_fmt_of(f: &Fixture) -> &'static str {
    f.args.iter().position(|a| *a == "-pix_fmt").and_then(|i| f.args.get(i + 1)).copied().unwrap_or("yuv420p")
}

/// Generate (if needed) the fixture stream and its ffmpeg reference decode.
/// Returns None (with a message) when ffmpeg is unavailable.
pub fn ensure(f: &Fixture) -> Option<(PathBuf, PathBuf)> {
    let ff = filmcraft_testkit::ffmpeg_or_skip(f.name)?;
    let dir = fixtures_dir();
    let h264 = dir.join(format!("{}.h264", f.name));
    let yuv = dir.join(format!("{}.yuv", f.name));
    let pix = pix_fmt_of(f);
    if !h264.exists() {
        let tmp = filmcraft_testkit::temp_path(&h264);
        let mut vf = format!("{}=size={}x{}:rate=25,format={}", f.source, f.width, f.height, pix);
        if !f.filter.is_empty() {
            vf.push(',');
            vf.push_str(f.filter);
        }
        let mut c = Command::new(&ff);
        c.args(["-hide_banner", "-loglevel", "error", "-y", "-f", "lavfi", "-i", &vf]);
        c.args(["-frames:v", &f.frames.to_string()]);
        if !f.args.contains(&"-c:v") {
            c.args(["-c:v", "libx264"]);
        }
        c.args(f.args);
        c.args(["-f", "h264"]).arg(&tmp);
        if !run(&mut c) {
            // Platform encoders (VideoToolbox) are optional; libx264 fixtures must generate.
            assert!(f.args.contains(&"-c:v"), "fixture generation failed for {}", f.name);
            eprintln!("SKIPPED ({}): encoder unavailable", f.name);
            return None;
        }
        std::fs::rename(&tmp, &h264).unwrap();
    }
    if !yuv.exists() {
        let tmp = filmcraft_testkit::temp_path(&yuv);
        let mut c = Command::new(&ff);
        c.args(["-hide_banner", "-loglevel", "error", "-y", "-i"]).arg(&h264);
        c.args(["-fps_mode", "passthrough", "-f", "rawvideo", "-pix_fmt", pix]).arg(&tmp);
        assert!(run(&mut c), "reference decode failed for {}", f.name);
        std::fs::rename(&tmp, &yuv).unwrap();
    }
    Some((h264, yuv))
}

/// Split an Annex-B stream into access units (new AU at AUD/SPS/PPS/SEI after a slice, or at a slice with
/// first_mb_in_slice == 0).
pub fn split_access_units(data: &[u8]) -> Vec<&[u8]> {
    // find start code positions
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
    let mut seen_slice = false;
    for &(sc, payload) in &starts {
        if payload >= data.len() {
            continue;
        }
        let t = data[payload] & 0x1f;
        let is_slice = t == 1 || t == 5;
        let first_mb_zero = is_slice && payload + 1 < data.len() && data[payload + 1] & 0x80 != 0;
        let boundary = seen_slice && (matches!(t, 6..=9) || first_mb_zero);
        if boundary {
            aus.push(&data[au_start..sc]);
            au_start = sc;
            seen_slice = false;
        }
        if is_slice {
            seen_slice = true;
        }
    }
    if au_start < data.len() {
        aus.push(&data[au_start..]);
    }
    aus
}

/// Decode a whole Annex-B file access unit by access unit (pts = AU index).
/// Failed decode: (access unit index, error, pictures output before the error).
pub type DecodeFailure = (usize, filmcraft_h264::Error, Vec<Picture>);

pub fn decode_file(path: &Path) -> Result<Vec<Picture>, DecodeFailure> {
    decode_file_threads(path, 0)
}

/// Decode with `threads` worker threads (0 = decoder default).
pub fn decode_file_threads(path: &Path, threads: usize) -> Result<Vec<Picture>, DecodeFailure> {
    decode_file_opts(path, threads, false)
}

/// [`decode_file_threads`] with draft mode ([`Decoder::set_draft`]) on or off.
pub fn decode_file_opts(path: &Path, threads: usize, draft: bool) -> Result<Vec<Picture>, DecodeFailure> {
    let data = std::fs::read(path).unwrap();
    let mut dec = if threads == 0 { Decoder::new() } else { Decoder::with_threads(threads) };
    assert!(!dec.draft(), "draft mode is off by default");
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

/// Compare decoded pictures with a raw planar reference in the fixture's pixel format (8-bit
/// bytes or 16-bit little-endian samples, 4:2:0 or 4:2:2 as the pictures report); returns a
/// diagnostic on the first mismatch.
pub fn compare(pics: &[Picture], reference: &[u8], w: usize, h: usize) -> Result<(), String> {
    let Some(first) = pics.first() else {
        return if reference.is_empty() { Ok(()) } else { Err("decoder produced no frames".into()) };
    };
    let deep = first.bit_depth > 8;
    let (cw, ch) = (first.chroma_width as usize, first.chroma_height as usize);
    let bytes = if deep { 2 } else { 1 };
    let fsize = (w * h + 2 * cw * ch) * bytes;
    let nref = reference.len() / fsize;
    for (i, p) in pics.iter().enumerate() {
        if i >= nref {
            return Err(format!("decoder produced {} frames, reference has {}", pics.len(), nref));
        }
        if p.width as usize != w || p.height as usize != h {
            return Err(format!("frame {i}: size {}x{} != {}x{}", p.width, p.height, w, h));
        }
        let f = &reference[i * fsize..(i + 1) * fsize];
        let raw = |off: usize, len: usize| -> Vec<u16> {
            let s = f.get(off..).unwrap_or_default().get(..len * bytes).unwrap_or_default();
            if deep { s.as_chunks::<2>().0.iter().map(|b| u16::from_le_bytes(*b)).collect() } else { s.iter().map(|&b| b as u16).collect() }
        };
        let (y, u, v) = (p.y.to_u16(), p.u.to_u16(), p.v.to_u16());
        let planes = [
            ("Y", &y[..], raw(0, w * h), w, h, 16),
            ("U", &u[..], raw(w * h * bytes, cw * ch), cw, ch, 8),
            ("V", &v[..], raw((w * h + cw * ch) * bytes, cw * ch), cw, ch, 8),
        ];
        for (name, got, exp, pw, ph, mbs) in planes {
            if got != exp.as_slice() {
                let mut first = None;
                let mut count = 0usize;
                let mut maxd = 0i32;
                for y in 0..ph {
                    for x in 0..pw {
                        let (a, b) = (got[y * pw + x], exp[y * pw + x]);
                        if a != b {
                            count += 1;
                            maxd = maxd.max((a as i32 - b as i32).abs());
                            if first.is_none() {
                                first = Some((x, y, a, b));
                            }
                        }
                    }
                }
                let (x, y, a, b) = first.unwrap();
                let mb_w = pw.div_ceil(mbs);
                return Err(format!(
                    "frame {i} (poc {}, pts {}) plane {name}: first mismatch at ({x},{y}) MB {} (mb_x {}, mb_y {}): got {a} want {b}; {count} samples differ, max diff {maxd}",
                    p.poc,
                    p.pts,
                    (y / mbs) * mb_w + x / mbs,
                    x / mbs,
                    y / mbs
                ));
            }
        }
    }
    if pics.len() != nref {
        return Err(format!("decoder produced {} frames, reference has {}", pics.len(), nref));
    }
    Ok(())
}

/// Full check of one fixture: generate, decode, compare. Returns Ok(false) when skipped.
pub fn check_fixture(name: &str) -> Result<bool, String> {
    let f = fixture(name);
    let Some((h264, yuv)) = ensure(f) else { return Ok(false) };
    let reference = std::fs::read(&yuv).unwrap();
    // single-threaded and frame-threaded decoding (an odd thread count and every core) must all
    // be bit-exact
    for threads in [1, 3, 0] {
        let pics = match decode_file_threads(&h264, threads) {
            Ok(p) => p,
            Err((au, e, partial)) => {
                // still report comparison of what was decoded
                let cmp = compare(&partial, &reference, f.width as usize, f.height as usize).err().unwrap_or_default();
                return Err(format!("{name} (threads {threads}): decode error at access unit {au}: {e} (after {} pictures) {cmp}", partial.len()));
            }
        };
        compare(&pics, &reference, f.width as usize, f.height as usize).map_err(|e| format!("{name} (threads {threads}): {e}"))?;
        if pics.iter().any(|p| p.draft) {
            return Err(format!("{name} (threads {threads}): draft picture without draft mode"));
        }
        check_pts(&pics).map_err(|e| format!("{name} (threads {threads}): {e}"))?;
    }
    Ok(true)
}

/// Output pts values (= access unit index) must be a permutation of 0..n, and follow POC order
/// between IDR pictures.
pub fn check_pts(pics: &[Picture]) -> Result<(), String> {
    let mut seen = vec![false; pics.len()];
    for p in pics {
        let i = p.pts as usize;
        if i >= seen.len() || seen[i] {
            return Err(format!("unexpected pts {} in output", p.pts));
        }
        seen[i] = true;
    }
    for w in pics.windows(2) {
        if !w[1].key && w[1].poc <= w[0].poc {
            return Err(format!("output POC order violated: {} then {}", w[0].poc, w[1].poc));
        }
    }
    Ok(())
}
