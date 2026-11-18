//! Shared helpers for the codecs oracle tests (MXF, Ogg). ffmpeg/ffprobe are external fixture
//! generators and oracles only (never linked).
#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;

use filmcraft_frame::{PixelData, VideoFrame};

pub fn dir() -> PathBuf {
    filmcraft_testkit::fixtures_dir("codecs")
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

pub fn bytes(p: &Path) -> Arc<[u8]> {
    std::fs::read(p).unwrap().into()
}

/// Run ffmpeg and capture stdout.
pub fn ffmpeg_out(ff: &Path, args: &[&str]) -> Vec<u8> {
    let o = Command::new(ff).args(["-v", "error"]).args(args).stdin(Stdio::null()).output().expect("run ffmpeg");
    assert!(o.status.success(), "ffmpeg {args:?}: {}", String::from_utf8_lossy(&o.stderr));
    o.stdout
}

/// Decoded frames of the first video stream as raw planes in `pix_fmt`.
pub fn ffmpeg_frames(ff: &Path, file: &Path, pix_fmt: &str) -> Vec<u8> {
    ffmpeg_out(ff, &["-i", file.to_str().unwrap(), "-map", "0:v:0", "-fps_mode", "passthrough", "-f", "rawvideo", "-pix_fmt", pix_fmt, "-"])
}

/// Interleaved f32 samples of the first audio stream (all channels).
pub fn ffmpeg_audio_f32(ff: &Path, file: &Path, extra: &[&str]) -> Vec<f32> {
    let mut args = vec!["-i", file.to_str().unwrap()];
    args.extend_from_slice(extra);
    args.extend_from_slice(&["-map", "0:a:0", "-f", "f32le", "-"]);
    ffmpeg_out(ff, &args).as_chunks::<4>().0.iter().map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]])).collect()
}

/// Our frame's three planes widened to u16 (and their sizes).
pub fn planes(f: &VideoFrame) -> Vec<Vec<u16>> {
    match &f.data {
        PixelData::Yuv8 { planes, .. } => planes.iter().map(|p| p.iter().map(|&v| v as u16).collect()).collect(),
        PixelData::Yuv16 { planes, .. } => planes.iter().map(|p| p.to_vec()).collect(),
        _ => panic!("expected a YUV frame"),
    }
}

/// Split one raw frame of `ffmpeg_frames` into planes (`bps` bytes per sample, chroma `cw`×`ch`).
pub fn raw_planes(raw: &[u8], w: usize, h: usize, cw: usize, ch: usize, bps: usize) -> Vec<Vec<u16>> {
    let get = |b: &[u8]| -> Vec<u16> {
        if bps == 1 { b.iter().map(|&v| v as u16).collect() } else { b.as_chunks::<2>().0.iter().map(|c| u16::from_le_bytes([c[0], c[1]])).collect() }
    };
    let y = w * h * bps;
    let c = cw * ch * bps;
    vec![get(&raw[..y]), get(&raw[y..y + c]), get(&raw[y + c..y + 2 * c])]
}

/// Largest absolute difference between plane sets.
pub fn max_diff(a: &[Vec<u16>], b: &[Vec<u16>]) -> u16 {
    a.iter()
        .zip(b)
        .map(|(x, y)| {
            assert_eq!(x.len(), y.len(), "plane size");
            x.iter().zip(y).map(|(p, q)| p.abs_diff(*q)).max().unwrap_or(0)
        })
        .max()
        .unwrap_or(0)
}

/// Deterministic pseudo-random numbers (xorshift).
pub struct Rng(pub u64);
impl Rng {
    pub fn below(&mut self, n: u64) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0 % n.max(1)
    }
}
