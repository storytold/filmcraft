//! Shared helpers for oracle tests. ffmpeg is used only as an external fixture generator and
//! reference decoder (never linked).
#![allow(dead_code)]

use filmcraft_dnx::{Frame, probe};
use std::path::{Path, PathBuf};
use std::process::Command;

/// ffmpeg, or `None` after printing `SKIPPED` (see `filmcraft_testkit::oracle`).
pub fn ffmpeg() -> Option<PathBuf> {
    filmcraft_testkit::ffmpeg_or_skip("dnx oracle")
}

pub fn fixture_dir() -> PathBuf {
    filmcraft_testkit::fixtures_dir("dnx")
}

pub fn run(ff: &Path, args: &[&str]) {
    let out = Command::new(ff).args(["-hide_banner", "-loglevel", "error", "-y"]).args(args).output().expect("run ffmpeg");
    assert!(out.status.success(), "ffmpeg {:?} failed: {}", args, String::from_utf8_lossy(&out.stderr));
}

/// A DNxHD/DNxHR fixture: a lavfi source encoded by ffmpeg's VC-3 encoder into a MOV.
pub struct Spec {
    pub name: &'static str,
    pub lavfi: String,
    pub frames: u32,
    /// Encoder arguments (bitrate / profile, pixel format, flags).
    pub enc: Vec<&'static str>,
    /// Raw pixel format of the reference decode.
    pub pix_fmt: &'static str,
}

impl Spec {
    pub fn new(name: &'static str, lavfi: impl Into<String>, frames: u32, enc: &[&'static str], pix_fmt: &'static str) -> Spec {
        Spec { name, lavfi: lavfi.into(), frames, enc: enc.to_vec(), pix_fmt }
    }
}

/// Generate (or reuse) the fixture MOV.
pub fn make(ff: &Path, spec: &Spec) -> PathBuf {
    let path = fixture_dir().join(format!("{}.mov", spec.name));
    if !path.exists() {
        let tmp = filmcraft_testkit::temp_path(&path);
        let frames = spec.frames.to_string();
        let mut args = vec!["-f", "lavfi", "-i", spec.lavfi.as_str(), "-frames:v", frames.as_str(), "-c:v", "dnxhd"];
        args.extend(spec.enc.iter().copied());
        args.extend(["-f", "mov"]);
        let t = tmp.to_str().unwrap().to_string();
        args.push(&t);
        run(ff, &args);
        std::fs::rename(&tmp, &path).unwrap();
    }
    path
}

/// The coded frames of a MOV (ffmpeg's `data` muxer concatenates packets; split them with the
/// coding-unit sizes from the headers).
pub fn packets(ff: &Path, mov: &Path) -> Vec<Vec<u8>> {
    let out = Command::new(ff)
        .args(["-hide_banner", "-loglevel", "error", "-i"])
        .arg(mov)
        .args(["-map", "0:v", "-c", "copy", "-f", "data", "-"])
        .output()
        .expect("run ffmpeg");
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    split_frames(&out.stdout)
}

pub fn split_frames(mut d: &[u8]) -> Vec<Vec<u8>> {
    let mut v = Vec::new();
    while d.len() >= 0x280 {
        let h = probe(d).expect("header");
        let mut n = h.coding_unit_size().expect("CBR") as usize;
        if h.interlaced && !h.frame_encoding {
            n += probe(&d[n..]).expect("second field").coding_unit_size().unwrap() as usize;
        }
        v.push(d[..n].to_vec());
        d = &d[n..];
    }
    v
}

/// Reference-decode a MOV with ffmpeg; samples as u16 (8-bit formats widened).
pub fn reference(ff: &Path, mov: &Path, pix_fmt: &str) -> Vec<u16> {
    let out = Command::new(ff)
        .args(["-hide_banner", "-loglevel", "error", "-i"])
        .arg(mov)
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

#[derive(Default, Clone, Copy, Debug)]
pub struct Stats {
    pub max: u32,
    pub sum: u64,
    pub n: u64,
    pub exact: u64,
    /// Samples off by more than 1.
    pub over1: u64,
}

impl Stats {
    pub fn add(&mut self, a: &[u16], b: &[u16]) {
        assert_eq!(a.len(), b.len());
        for (&x, &y) in a.iter().zip(b) {
            let d = (x as i32 - y as i32).unsigned_abs();
            self.max = self.max.max(d);
            self.sum += d as u64;
            self.n += 1;
            self.exact += (d == 0) as u64;
            self.over1 += (d > 1) as u64;
        }
    }
    pub fn merge(&mut self, o: &Stats) {
        self.max = self.max.max(o.max);
        self.sum += o.sum;
        self.n += o.n;
        self.exact += o.exact;
        self.over1 += o.over1;
    }
    pub fn mean(&self) -> f64 {
        self.sum as f64 / self.n.max(1) as f64
    }
    pub fn exact_pct(&self) -> f64 {
        100.0 * self.exact as f64 / self.n.max(1) as f64
    }
}

/// Planes in the order of ffmpeg's planar raw output.
pub fn planes(f: &Frame, pix_fmt: &str) -> Vec<u16> {
    // RGB frames hold G, B, R in y, cb, cr: the same order as `gbrp`
    let _ = pix_fmt;
    let order: Vec<&Vec<u16>> = vec![&f.y, &f.cb, &f.cr];
    order.into_iter().chain(f.alpha.as_ref()).flat_map(|p| p.iter().copied()).collect()
}

/// Samples per frame of a raw reference image.
pub fn frame_samples(f: &Frame) -> usize {
    let n = f.width as usize * f.height as usize;
    let c = f.chroma_width() as usize * f.chroma_height() as usize;
    n + 2 * c + if f.alpha.is_some() { n } else { 0 }
}

pub fn psnr(a: &[u16], b: &[u16], peak: f64) -> f64 {
    let mse: f64 = a.iter().zip(b).map(|(&x, &y)| (x as f64 - y as f64).powi(2)).sum::<f64>() / a.len() as f64;
    if mse == 0.0 { 99.0 } else { 10.0 * (peak * peak / mse).log10() }
}

fn bx(kind: &[u8; 4], payload: &[u8]) -> Vec<u8> {
    let mut v = Vec::with_capacity(payload.len() + 8);
    v.extend_from_slice(&((payload.len() + 8) as u32).to_be_bytes());
    v.extend_from_slice(kind);
    v.extend_from_slice(payload);
    v
}
const MATRIX: [u32; 9] = [0x10000, 0, 0, 0, 0x10000, 0, 0, 0, 0x4000_0000];
/// Minimal QuickTime writer (test-only): one video track, all samples in one chunk.
pub fn write_mov(path: &Path, fourcc: &[u8; 4], width: u16, height: u16, depth: u16, frames: &[Vec<u8>]) {
    let n = frames.len() as u32;
    let be32 = |v: u32| v.to_be_bytes();
    let ftyp = bx(b"ftyp", &[b"qt  ".as_slice(), &be32(0x200), b"qt  "].concat());
    let mdat_payload: Vec<u8> = frames.concat();
    let data_off = ftyp.len() as u32 + 8;
    let mdat = bx(b"mdat", &mdat_payload);
    let matrix: Vec<u8> = MATRIX.iter().flat_map(|v| v.to_be_bytes()).collect();

    let mut mvhd = vec![0u8; 4 + 8];
    mvhd.extend_from_slice(&be32(25));
    mvhd.extend_from_slice(&be32(n));
    mvhd.extend_from_slice(&be32(0x10000));
    mvhd.extend_from_slice(&[1, 0]);
    mvhd.extend_from_slice(&[0; 10]);
    mvhd.extend_from_slice(&matrix);
    mvhd.extend_from_slice(&[0; 24]);
    mvhd.extend_from_slice(&be32(2));

    let mut tkhd = vec![0, 0, 0, 0xf];
    tkhd.extend_from_slice(&[0; 8]);
    tkhd.extend_from_slice(&be32(1));
    tkhd.extend_from_slice(&[0; 4]);
    tkhd.extend_from_slice(&be32(n));
    tkhd.extend_from_slice(&[0; 8]);
    tkhd.extend_from_slice(&[0; 8]); // layer, alternate group, volume, reserved
    tkhd.extend_from_slice(&matrix);
    tkhd.extend_from_slice(&be32((width as u32) << 16));
    tkhd.extend_from_slice(&be32((height as u32) << 16));

    let mut mdhd = vec![0u8; 4 + 8];
    mdhd.extend_from_slice(&be32(25));
    mdhd.extend_from_slice(&be32(n));
    mdhd.extend_from_slice(&[0; 4]);
    let hdlr = [&[0u8; 4][..], b"mhlr", b"vide", &[0; 12], &[0]].concat();
    let vmhd = [0u8, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0, 0];
    let dref = bx(b"dref", &[&[0u8; 4][..], &be32(1), &bx(b"alis", &[0, 0, 0, 1])].concat());
    let dinf = bx(b"dinf", &dref);

    let mut entry = vec![0u8; 6];
    entry.extend_from_slice(&[0, 1]);
    entry.extend_from_slice(&[0; 4]); // version, revision
    entry.extend_from_slice(b"fmcr");
    entry.extend_from_slice(&be32(0));
    entry.extend_from_slice(&be32(0x400));
    entry.extend_from_slice(&width.to_be_bytes());
    entry.extend_from_slice(&height.to_be_bytes());
    entry.extend_from_slice(&be32(72 << 16));
    entry.extend_from_slice(&be32(72 << 16));
    entry.extend_from_slice(&be32(0));
    entry.extend_from_slice(&[0, 1]);
    entry.extend_from_slice(&[0; 32]);
    entry.extend_from_slice(&depth.to_be_bytes());
    entry.extend_from_slice(&[0xff, 0xff]);
    let stsd = bx(b"stsd", &[&[0u8; 4][..], &be32(1), &bx(fourcc, &entry)].concat());
    let stts = bx(b"stts", &[&[0u8; 4][..], &be32(1), &be32(n), &be32(1)].concat());
    let stsc = bx(b"stsc", &[&[0u8; 4][..], &be32(1), &be32(1), &be32(n), &be32(1)].concat());
    let mut stsz = vec![0u8; 4];
    stsz.extend_from_slice(&be32(0));
    stsz.extend_from_slice(&be32(n));
    for f in frames {
        stsz.extend_from_slice(&be32(f.len() as u32));
    }
    let stsz = bx(b"stsz", &stsz);
    let stco = bx(b"stco", &[&[0u8; 4][..], &be32(1), &be32(data_off)].concat());
    let stbl = bx(b"stbl", &[stsd, stts, stsc, stsz, stco].concat());
    let minf = bx(b"minf", &[bx(b"vmhd", &vmhd), dinf, stbl].concat());
    let mdia = bx(b"mdia", &[bx(b"mdhd", &mdhd), bx(b"hdlr", &hdlr), minf].concat());
    let trak = bx(b"trak", &[bx(b"tkhd", &tkhd), mdia].concat());
    let moov = bx(b"moov", &[bx(b"mvhd", &mvhd), trak].concat());
    std::fs::write(path, [ftyp, mdat, moov].concat()).unwrap();
}

/// Decode a MOV with ffmpeg (failing on any decode error); raw samples as u16.
pub fn ffmpeg_decode(ff: &Path, mov: &Path, pix_fmt: &str) -> Vec<u16> {
    let out = Command::new(ff)
        .args(["-hide_banner", "-loglevel", "error", "-xerror", "-i"])
        .arg(mov)
        .args(["-f", "rawvideo", "-pix_fmt", pix_fmt, "-"])
        .output()
        .expect("run ffmpeg");
    let err = String::from_utf8_lossy(&out.stderr).to_string();
    assert!(out.status.success() && err.trim().is_empty(), "ffmpeg failed: {err}");
    if pix_fmt.ends_with("le") {
        out.stdout.as_chunks::<2>().0.iter().map(|c| u16::from_le_bytes([c[0], c[1]])).collect()
    } else {
        out.stdout.iter().map(|&b| b as u16).collect()
    }
}

/// Render a lavfi source to raw planar frames with ffmpeg and load them as [`Frame`]s.
pub fn source_frames(ff: &Path, name: &str, lavfi: &str, frames: u32, w: u32, h: u32, chroma: filmcraft_dnx::ChromaFormat, depth: u8) -> Vec<Frame> {
    let pix = match (chroma, depth) {
        (filmcraft_dnx::ChromaFormat::Yuv422, 8) => "yuv422p",
        (filmcraft_dnx::ChromaFormat::Yuv422, 10) => "yuv422p10le",
        (filmcraft_dnx::ChromaFormat::Yuv422, _) => "yuv422p12le",
        (_, 10) => "yuv444p10le",
        _ => "yuv444p12le",
    };
    let path = fixture_dir().join(format!("{name}.{pix}.raw"));
    if !path.exists() {
        let tmp = filmcraft_testkit::temp_path(&path);
        let f = frames.to_string();
        run(ff, &["-f", "lavfi", "-i", lavfi, "-frames:v", &f, "-f", "rawvideo", "-pix_fmt", pix, tmp.to_str().unwrap()]);
        std::fs::rename(&tmp, &path).unwrap();
    }
    let bytes = std::fs::read(&path).unwrap();
    let s: Vec<u16> = if depth == 8 {
        bytes.iter().map(|&b| b as u16).collect()
    } else {
        bytes.as_chunks::<2>().0.iter().map(|c| u16::from_le_bytes([c[0], c[1]])).collect()
    };
    let proto = Frame::new(w, h, chroma, depth, false);
    let per = frame_samples(&proto);
    assert_eq!(s.len(), per * frames as usize);
    s.chunks_exact(per)
        .map(|p| {
            let mut f = proto.clone();
            let n = f.y.len();
            let c = f.cb.len();
            f.y.copy_from_slice(&p[..n]);
            f.cb.copy_from_slice(&p[n..n + c]);
            f.cr.copy_from_slice(&p[n + c..n + 2 * c]);
            f
        })
        .collect()
}
