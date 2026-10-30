//! Shared helpers for the MPEG video oracle tests. ffmpeg is used only as an external fixture
//! generator and reference decoder (never linked).
#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use filmcraft_mpeg2v::{ChromaFormat, Decoder, Picture};

pub fn fixture_dir() -> PathBuf {
    filmcraft_testkit::fixtures_dir("mpeg2v")
}

/// An elementary-stream fixture: a lavfi source encoded by ffmpeg's MPEG-1/2 video encoder.
pub struct Spec {
    pub name: &'static str,
    pub lavfi: &'static str,
    pub frames: u32,
    pub enc: &'static [&'static str],
    /// ffmpeg rawvideo pixel format of the decoded frames.
    pub pix_fmt: &'static str,
}

/// 1080i 4:2:0 (decode benchmark; Main@High).
pub const HD1080I: Spec = Spec {
    name: "m2v_1080i",
    lavfi: "testsrc2=size=1920x1080:rate=50,tinterlace=mode=interleave_top,setfield=tff",
    frames: 25,
    enc: &["-c:v", "mpeg2video", "-flags", "+ilme+ildct", "-bf", "2", "-g", "12", "-b:v", "25M", "-maxrate", "25M", "-bufsize", "9M"],
    pix_fmt: "yuv420p",
};

pub fn path(spec: &Spec) -> PathBuf {
    fixture_dir().join(format!("{}.{}", spec.name, if spec.enc.contains(&"mpeg1video") { "m1v" } else { "m2v" }))
}

/// Generate (or reuse) the fixture.
pub fn make(ff: &Path, spec: &Spec) -> Option<PathBuf> {
    let out = path(spec);
    filmcraft_testkit::fixtures::generate(&out, |tmp| {
        let frames = spec.frames.to_string();
        let fmt = if spec.enc.contains(&"mpeg1video") { "mpeg1video" } else { "mpeg2video" };
        let st = Command::new(ff)
            .args(["-hide_banner", "-loglevel", "error", "-y", "-f", "lavfi", "-i", spec.lavfi, "-frames:v", &frames])
            .args(spec.enc)
            .args(["-f", fmt])
            .arg(tmp)
            .stdin(Stdio::null())
            .status();
        matches!(st, Ok(s) if s.success())
    })
}

/// ffmpeg's decode of `file` as raw frames.
pub fn ffmpeg_frames(ff: &Path, file: &Path, pix_fmt: &str) -> Vec<u8> {
    let o = Command::new(ff)
        .args(["-hide_banner", "-loglevel", "error", "-i"])
        .arg(file)
        .args(["-fps_mode", "passthrough", "-f", "rawvideo", "-pix_fmt", pix_fmt, "-"])
        .stdin(Stdio::null())
        .output()
        .expect("run ffmpeg");
    assert!(o.status.success(), "ffmpeg: {}", String::from_utf8_lossy(&o.stderr));
    o.stdout
}

/// Decode an elementary stream access unit by access unit (pts = index), flushing at the end.
pub fn decode_es(es: &[u8], threads: bool) -> (Vec<Picture>, Decoder) {
    let mut d = Decoder::new();
    d.set_threads(threads);
    let mut out = Vec::new();
    for (i, r) in filmcraft_mpeg2v::access_units(es).into_iter().enumerate() {
        out.extend(d.decode(&es[r], i as i64).expect("decode"));
    }
    out.extend(d.flush());
    (out, d)
}

/// Per-frame comparison statistics against raw planes.
#[derive(Default, Debug, Clone, Copy)]
pub struct Stats {
    pub max: u8,
    pub sse: f64,
    pub n: u64,
    /// Samples differing at all.
    pub diff: u64,
}

impl Stats {
    pub fn psnr(&self) -> f64 {
        if self.sse == 0.0 { f64::INFINITY } else { 10.0 * (255.0 * 255.0 * self.n as f64 / self.sse).log10() }
    }
    pub fn add(&mut self, a: &[u8], b: &[u8]) {
        assert_eq!(a.len(), b.len(), "plane size");
        for (x, y) in a.iter().zip(b) {
            let d = x.abs_diff(*y);
            self.max = self.max.max(d);
            self.sse += (d as f64) * (d as f64);
            self.diff += (d != 0) as u64;
        }
        self.n += a.len() as u64;
    }
    pub fn merge(&mut self, o: &Stats) {
        self.max = self.max.max(o.max);
        self.sse += o.sse;
        self.n += o.n;
        self.diff += o.diff;
    }
}

/// Compare decoded pictures with ffmpeg's raw frames; returns per-frame stats.
pub fn compare(pics: &[Picture], raw: &[u8]) -> Vec<Stats> {
    let p0 = &pics[0];
    let (w, h) = (p0.width as usize, p0.height as usize);
    let (cw, ch) = (p0.chroma_width() as usize, p0.chroma_height() as usize);
    let fsize = w * h + 2 * cw * ch;
    assert_eq!(raw.len() % fsize, 0, "raw size");
    assert_eq!(raw.len() / fsize, pics.len(), "frame count = ffmpeg's");
    pics.iter()
        .enumerate()
        .map(|(i, p)| {
            let f = &raw[i * fsize..(i + 1) * fsize];
            let mut s = Stats::default();
            s.add(&p.y, &f[..w * h]);
            s.add(&p.cb, &f[w * h..w * h + cw * ch]);
            s.add(&p.cr, &f[w * h + cw * ch..]);
            s
        })
        .collect()
}

pub fn pix_fmt_of(c: ChromaFormat) -> &'static str {
    match c {
        ChromaFormat::Yuv420 => "yuv420p",
        ChromaFormat::Yuv422 => "yuv422p",
        ChromaFormat::Yuv444 => "yuv444p",
    }
}
