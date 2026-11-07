//! Shared helpers for the MPEG-4 Part 2 oracle tests. ffmpeg is used only as an external fixture
//! generator and reference decoder (never linked).
#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use filmcraft_mpeg4v::{Decoder, Picture, start_codes};

/// An elementary-stream fixture: a lavfi source encoded by ffmpeg's MPEG-4 encoder.
pub struct Spec {
    pub name: &'static str,
    pub lavfi: &'static str,
    pub frames: u32,
    pub enc: &'static [&'static str],
}

pub fn path(spec: &Spec) -> PathBuf {
    filmcraft_testkit::fixtures_dir("mpeg4v").join(format!("{}.m4v", spec.name))
}

/// Generate (or reuse) the fixture.
pub fn make(ff: &Path, spec: &Spec) -> Option<PathBuf> {
    let out = path(spec);
    filmcraft_testkit::fixtures::generate(&out, |tmp| {
        let frames = spec.frames.to_string();
        let st = Command::new(ff)
            .args(["-hide_banner", "-loglevel", "error", "-y", "-f", "lavfi", "-i", spec.lavfi, "-frames:v", &frames, "-c:v", "mpeg4"])
            .args(spec.enc)
            .args(["-f", "m4v"])
            .arg(tmp)
            .stdin(Stdio::null())
            .status();
        matches!(st, Ok(s) if s.success())
    })
}

/// ffmpeg's decode of `file` as raw 4:2:0 frames.
pub fn ffmpeg_frames(ff: &Path, file: &Path) -> Vec<u8> {
    let o = Command::new(ff)
        .args(["-hide_banner", "-loglevel", "error", "-i"])
        .arg(file)
        .args(["-fps_mode", "passthrough", "-f", "rawvideo", "-pix_fmt", "yuv420p", "-"])
        .stdin(Stdio::null())
        .output()
        .expect("run ffmpeg");
    assert!(o.status.success(), "ffmpeg: {}", String::from_utf8_lossy(&o.stderr));
    o.stdout
}

/// An elementary stream cut into chunks of one VOP each, headers with the VOP after them.
pub fn chunks(es: &[u8]) -> Vec<&[u8]> {
    let mut out = Vec::new();
    let mut start = 0;
    let vops: Vec<usize> = start_codes(es).iter().filter(|c| c.1 == 0xB6).map(|c| c.0).collect();
    for w in vops.iter().skip(1) {
        // what stands between two VOPs (a VOL, a GOV) goes with the later one
        let end = start_codes(&es[..*w]).iter().rev().take_while(|c| c.1 != 0xB6).last().map_or(*w, |c| c.0);
        out.push(&es[start..end]);
        start = end;
    }
    out.push(&es[start..]);
    out
}

pub fn decode_all(chunks: &[&[u8]]) -> Vec<Picture> {
    let mut d = Decoder::new();
    let mut out = Vec::new();
    for (i, c) in chunks.iter().enumerate() {
        out.extend(d.decode(c, i as i64).expect("decode"));
    }
    out.extend(d.flush());
    out
}

/// PSNR and the worst sample difference of one frame against raw planes.
pub fn psnr(p: &Picture, raw: &[u8]) -> (f64, u8) {
    let mut sse = 0u64;
    let mut worst = 0u8;
    let n = p.y.len() + p.cb.len() + p.cr.len();
    assert_eq!(raw.len(), n);
    for (a, b) in p.y.iter().chain(&p.cb).chain(&p.cr).zip(raw) {
        let d = a.abs_diff(*b);
        sse += u64::from(d) * u64::from(d);
        worst = worst.max(d);
    }
    if sse == 0 {
        return (99.0, 0);
    }
    (10.0 * (255.0f64 * 255.0 * n as f64 / sse as f64).log10(), worst)
}

/// Frame-by-frame comparison against ffmpeg's frames.
#[derive(Clone, Copy, Debug)]
pub struct Compared {
    pub frames: usize,
    pub min_psnr: f64,
    pub mean_psnr: f64,
    pub last_psnr: f64,
    pub worst: u8,
    pub first_diff: Option<usize>,
}

pub fn compare(pics: &[Picture], raw: &[u8]) -> Compared {
    let size = pics.first().map_or(1, |p| p.y.len() + p.cb.len() + p.cr.len());
    assert_eq!(raw.len() / size, pics.len(), "frame count: ffmpeg {} ours {}", raw.len() / size, pics.len());
    let (mut min_psnr, mut sum, mut worst, mut last) = (f64::MAX, 0.0, 0, 0.0);
    let mut first_diff = None;
    for (k, (p, r)) in pics.iter().zip(raw.chunks_exact(size)).enumerate() {
        let (db, w) = psnr(p, r);
        if w > 0 && first_diff.is_none() {
            first_diff = Some(k);
        }
        min_psnr = min_psnr.min(db);
        sum += db;
        last = db;
        worst = worst.max(w);
    }
    Compared { frames: pics.len(), min_psnr, mean_psnr: sum / pics.len().max(1) as f64, last_psnr: last, worst, first_diff }
}
