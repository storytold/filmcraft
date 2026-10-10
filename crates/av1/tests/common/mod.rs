//! Shared helpers for the AV1 oracle tests. ffmpeg (libsvtav1 to encode fixtures, libdav1d to
//! decode the reference) is used only as an external process.
#![allow(dead_code)]

use filmcraft_av1::{Decoder, Picture};
use std::path::{Path, PathBuf};
use std::process::Command;

/// ffmpeg with libsvtav1 (fixtures) and libdav1d (reference), or `None` after reporting a skip.
pub fn ffmpeg() -> Option<PathBuf> {
    ffmpeg_with(&[("-encoders", "libsvtav1"), ("-decoders", "libdav1d")])
}

/// ffmpeg with libdav1d, for checks that decode existing files only.
pub fn ffmpeg_dav1d() -> Option<PathBuf> {
    ffmpeg_with(&[("-decoders", "libdav1d")])
}

/// Builds such as the Windows "essentials" ones lack these libraries; skip rather than fail.
fn ffmpeg_with(needs: &[(&str, &str)]) -> Option<PathBuf> {
    let ff = filmcraft_testkit::ffmpeg_or_skip("av1 oracle")?;
    for &(list, lib) in needs {
        let out = Command::new(&ff).args(["-hide_banner", list]).output().map(|o| String::from_utf8_lossy(&o.stdout).into_owned()).unwrap_or_default();
        if !out.contains(lib) {
            filmcraft_testkit::skip("av1 oracle", &format!("ffmpeg has no {lib} (see `ffmpeg {list}`)"));
            return None;
        }
    }
    Some(ff)
}

pub fn fixture_dir() -> PathBuf {
    filmcraft_testkit::fixtures_dir("av1")
}

pub fn run(ff: &Path, args: &[&str]) {
    let out = Command::new(ff).args(["-hide_banner", "-loglevel", "error", "-y"]).args(args).output().expect("run ffmpeg");
    assert!(out.status.success(), "ffmpeg {:?} failed: {}", args, String::from_utf8_lossy(&out.stderr));
}

/// An AV1 fixture: a lavfi source encoded by ffmpeg's libsvtav1 into IVF.
pub struct Spec {
    pub name: &'static str,
    pub lavfi: String,
    pub frames: u32,
    pub pix_fmt: &'static str,
    /// Extra encoder arguments.
    pub enc: Vec<String>,
}

impl Spec {
    pub fn new(name: &'static str, lavfi: impl Into<String>, frames: u32, pix_fmt: &'static str, enc: &[&str]) -> Spec {
        Spec { name, lavfi: lavfi.into(), frames, pix_fmt, enc: enc.iter().map(|s| s.to_string()).collect() }
    }
}

pub fn make(ff: &Path, spec: &Spec) -> PathBuf {
    let path = fixture_dir().join(format!("{}.ivf", spec.name));
    if !path.exists() {
        let tmp = filmcraft_testkit::temp_path(&path);
        let frames = spec.frames.to_string();
        let t = tmp.to_str().unwrap().to_string();
        let mut args: Vec<&str> = vec!["-f", "lavfi", "-i", spec.lavfi.as_str(), "-frames:v", frames.as_str(), "-pix_fmt", spec.pix_fmt, "-c:v", "libsvtav1"];
        args.extend(spec.enc.iter().map(String::as_str));
        args.extend(["-f", "ivf", t.as_str()]);
        run(ff, &args);
        std::fs::rename(&tmp, &path).unwrap();
    }
    path
}

/// Frames of an IVF file.
pub fn ivf_frames(path: &Path) -> Vec<Vec<u8>> {
    let d = std::fs::read(path).unwrap();
    assert_eq!(&d[0..4], b"DKIF");
    let hdr = u16::from_le_bytes([d[6], d[7]]) as usize;
    let mut pos = hdr;
    let mut out = Vec::new();
    while pos + 12 <= d.len() {
        let n = u32::from_le_bytes([d[pos], d[pos + 1], d[pos + 2], d[pos + 3]]) as usize;
        pos += 12;
        out.push(d[pos..pos + n].to_vec());
        pos += n;
    }
    out
}

/// Reference decode with ffmpeg's libdav1d (8-bit formats widened to u16).
pub fn reference(ff: &Path, path: &Path, pix_fmt: &str) -> Vec<u16> {
    let out = Command::new(ff)
        .args(["-hide_banner", "-loglevel", "error", "-c:v", "libdav1d", "-i"])
        .arg(path)
        .args(["-f", "rawvideo", "-pix_fmt", pix_fmt, "-"])
        .output()
        .expect("run ffmpeg");
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    if pix_fmt.ends_with("le") {
        out.stdout.as_chunks::<2>().0.iter().map(|c| u16::from_le_bytes([c[0], c[1]])).collect()
    } else {
        out.stdout.iter().map(|&b| b as u16).collect()
    }
}

/// Decode every frame of an IVF file with our decoder (all cores).
pub fn decode_all(path: &Path) -> Result<Vec<Picture>, String> {
    decode_all_threads(path, std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1))
}

/// Decode every frame of an IVF file with a decoder using `threads` threads.
pub fn decode_all_threads(path: &Path, threads: usize) -> Result<Vec<Picture>, String> {
    decode_all_opts(path, threads, false)
}

/// [`decode_all_threads`] with draft mode on or off.
pub fn decode_all_opts(path: &Path, threads: usize, draft: bool) -> Result<Vec<Picture>, String> {
    let mut dec = Decoder::with_threads(threads);
    dec.set_draft(draft);
    let mut pics = Vec::new();
    for (i, f) in ivf_frames(path).iter().enumerate() {
        let p = dec.decode_pts(f, i as i64).map_err(|e| format!("frame {i}: {e}"))?;
        pics.extend(p);
    }
    pics.extend(dec.flush_result().map_err(|e| format!("flush: {e}"))?);
    Ok(pics)
}

pub fn picture_samples(p: &Picture) -> usize {
    let mut n = (p.width * p.height) as usize;
    if !p.mono_chrome {
        n += 2 * (p.plane_width(1) * p.plane_height(1)) as usize;
    }
    n
}

/// Compare a decoded picture with the raw reference; returns the first mismatch as
/// (plane, x, y, ours, reference) and the mismatch count.
pub fn compare(p: &Picture, raw: &[u16]) -> (Option<(usize, u32, u32, u16, u16)>, usize) {
    let mut first = None;
    let mut count = 0;
    let mut off = 0;
    for plane in 0..if p.mono_chrome { 1 } else { 3 } {
        let w = p.plane_width(plane);
        let h = p.plane_height(plane);
        for y in 0..h {
            for x in 0..w {
                let a = p.planes[plane][(y * w + x) as usize];
                let b = raw[off + (y * w + x) as usize];
                if a != b {
                    count += 1;
                    if first.is_none() {
                        first = Some((plane, x, y, a, b));
                    }
                }
            }
        }
        off += (w * h) as usize;
    }
    (first, count)
}

/// The raw pixel format matching a decoded picture.
pub fn pix_fmt_for(p: &Picture) -> &'static str {
    let hi = p.bit_depth > 8;
    if p.mono_chrome {
        return match p.bit_depth {
            8 => "gray",
            10 => "gray10le",
            _ => "gray12le",
        };
    }
    match (p.subsampling_x, p.subsampling_y, p.bit_depth) {
        (1, 1, 8) => "yuv420p",
        (1, 1, 10) => "yuv420p10le",
        (1, 1, _) => "yuv420p12le",
        (1, 0, 8) => "yuv422p",
        (1, 0, 10) => "yuv422p10le",
        (1, 0, _) => "yuv422p12le",
        (_, _, 8) => "yuv444p",
        (_, _, 10) => "yuv444p10le",
        _ if hi => "yuv444p12le",
        _ => "yuv444p",
    }
}

/// Decode a fixture and compare every frame with libdav1d; panics with details on mismatch.
pub fn check_bit_exact(ff: &Path, spec: &Spec) {
    let path = make(ff, spec);
    check_file(ff, spec.name, &path);
}

/// Decode an IVF file and compare every frame with libdav1d; panics with details on mismatch.
pub fn check_file(ff: &Path, name: &str, path: &Path) {
    let pics = decode_all_threads(path, 1).unwrap_or_else(|e| panic!("{name}: {e}"));
    // Tile, post-filter and frame threading must give the same pictures.
    let threads = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1).max(4);
    let mt = decode_all_threads(path, threads).unwrap_or_else(|e| panic!("{name} ({threads} threads): {e}"));
    assert_eq!(mt.len(), pics.len(), "{name}: frame count with {threads} threads");
    for (i, (a, b)) in pics.iter().zip(&mt).enumerate() {
        assert!(a == b, "{name}: frame {i} differs with {threads} threads");
    }
    assert!(!pics.is_empty(), "{name}: no frames");
    assert!(pics.iter().all(|p| !p.draft), "{name}: picture flagged draft without draft mode");
    let raw = reference(ff, path, pix_fmt_for(&pics[0]));
    let per = picture_samples(&pics[0]);
    assert_eq!(raw.len(), per * pics.len(), "{name}: frame count/size ({} pictures)", pics.len());
    for (i, p) in pics.iter().enumerate() {
        let (first, count) = compare(p, &raw[i * per..(i + 1) * per]);
        if let Some((plane, x, y, a, b)) = first {
            panic!("{name}: frame {i} differs in {count} samples; first at plane {plane} ({x},{y}): ours {a}, reference {b}");
        }
    }
    println!("{:<34} {:>3} frames {}x{} {}-bit bit-exact", name, pics.len(), pics[0].width, pics[0].height, pics[0].bit_depth);
}

/// An AV1 conformance test vector (libaom test data), downloaded on first use with curl into
/// the fixture directory. Returns None (after printing why) when it can't be fetched.
pub fn test_vector(name: &str) -> Option<PathBuf> {
    let dir = fixture_dir().join("aom-test-data");
    std::fs::create_dir_all(&dir).ok()?;
    let path = dir.join(name);
    if path.exists() {
        return Some(path);
    }
    let tmp = filmcraft_testkit::temp_path(&path);
    let url = format!("https://storage.googleapis.com/aom-test-data/{name}");
    let ok = Command::new("curl").args(["-sfL", "--max-time", "120", "-o"]).arg(&tmp).arg(&url).status().map(|s| s.success()).unwrap_or(false);
    if !ok {
        let _ = std::fs::remove_file(&tmp);
        filmcraft_testkit::skip("av1 conformance", &format!("could not download {url}"));
        return None;
    }
    std::fs::rename(&tmp, &path).ok()?;
    Some(path)
}
