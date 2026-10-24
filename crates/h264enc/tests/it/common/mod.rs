//! Shared helpers for the encoder tests: a synthetic content generator and ffmpeg/ffprobe oracle wrappers.
#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::process::Command;

pub struct Yuv {
    pub w: usize,
    pub h: usize,
    pub y: Vec<u8>,
    pub u: Vec<u8>,
    pub v: Vec<u8>,
}

impl Yuv {
    pub fn frame(&self) -> filmcraft_h264enc::YuvFrame<'_> {
        filmcraft_h264enc::YuvFrame { y: &self.y, u: &self.u, v: &self.v, y_stride: self.w, uv_stride: self.w / 2 }
    }
    pub fn bytes(&self) -> Vec<u8> {
        let mut v = self.y.clone();
        v.extend_from_slice(&self.u);
        v.extend_from_slice(&self.v);
        v
    }
}

fn hash(mut x: u32) -> u32 {
    x ^= x >> 16;
    x = x.wrapping_mul(0x7feb352d);
    x ^= x >> 15;
    x = x.wrapping_mul(0x846ca68b);
    x ^= x >> 16;
    x
}

/// Synthetic test picture `t`: four regions with a moving gradient, film-grain-like noise,
/// text-like hard edges and a sub-pixel panning texture.
pub fn synth(w: usize, h: usize, t: usize) -> Yuv {
    let mut y = vec![0u8; w * h];
    let mut u = vec![0u8; w * h / 4];
    let mut v = vec![0u8; w * h / 4];
    let tf = t as f32;
    for yy in 0..h {
        for xx in 0..w {
            let region = (xx * 2 / w) + 2 * (yy * 2 / h);
            let val: f32 = match region {
                0 => {
                    // moving smooth gradient
                    let fx = xx as f32 / w as f32;
                    let fy = yy as f32 / h as f32;
                    128.0 + 90.0 * ((fx * 6.0 + tf * 0.07).sin() * (fy * 4.0 - tf * 0.05).cos())
                }
                1 => {
                    // mild noise over a slow ramp (like film grain)
                    let n = (hash((xx as u32).wrapping_mul(7919) ^ (yy as u32).wrapping_mul(104729) ^ (t as u32).wrapping_mul(1_000_003)) & 31) as f32;
                    60.0 + (xx as f32 * 0.1 + yy as f32 * 0.05) % 120.0 + n
                }
                2 => {
                    // text-like strokes: glyph grid scrolling slowly upwards
                    let sy = yy + t;
                    let gx = xx / 12;
                    let gy = sy / 16;
                    let lx = xx % 12;
                    let ly = sy % 16;
                    let bits = hash((gx as u32) * 31 + gy as u32 * 977);
                    let inr = |v: usize, a: usize, b: usize| (a..=b).contains(&v);
                    let bar = inr(lx, 2, 9);
                    let stroke = (inr(lx, 2, 3) && bits & 1 != 0)
                        || (inr(lx, 8, 9) && bits & 2 != 0)
                        || (inr(ly, 3, 4) && bar && bits & 4 != 0)
                        || (inr(ly, 8, 9) && bar && bits & 8 != 0)
                        || (inr(ly, 12, 13) && bar && bits & 16 != 0);
                    if ly < 14 && lx < 11 && stroke { 235.0 } else { 20.0 }
                }
                _ => {
                    // panning texture with sub-pixel motion
                    let px = xx as f32 + tf * 1.75;
                    let py = yy as f32 + tf * 0.5;
                    let c = ((px / 9.0).sin() + (py / 7.0).cos() + ((px + py) / 23.0).sin()) * 40.0;
                    let checker = if ((px / 32.0).floor() as i64 + (py / 32.0).floor() as i64) % 2 == 0 { 30.0 } else { -30.0 };
                    128.0 + c + checker
                }
            };
            y[yy * w + xx] = val.clamp(0.0, 255.0) as u8;
        }
    }
    for yy in 0..h / 2 {
        for xx in 0..w / 2 {
            let fx = xx as f32 / (w / 2) as f32;
            let fy = yy as f32 / (h / 2) as f32;
            u[yy * w / 2 + xx] = (128.0 + 60.0 * (fx * 3.0 + tf * 0.03).sin()) as u8;
            v[yy * w / 2 + xx] = (128.0 + 50.0 * (fy * 5.0 - tf * 0.04).cos() * fx) as u8;
        }
    }
    Yuv { w, h, y, u, v }
}

/// ffmpeg (see `filmcraft_testkit::oracle`); `None` after printing `SKIPPED`.
pub fn ffmpeg() -> Option<PathBuf> {
    filmcraft_testkit::ffmpeg_or_skip("h264enc oracle")
}

/// ffprobe (see `filmcraft_testkit::oracle`); `None` after printing `SKIPPED`.
pub fn ffprobe() -> Option<PathBuf> {
    filmcraft_testkit::ffprobe_or_skip("h264enc oracle")
}

/// Per-test output directory under `<workspace>/target/fixtures/h264enc/`.
pub fn out_dir(name: &str) -> PathBuf {
    filmcraft_testkit::fixtures_dir(&format!("h264enc/{name}"))
}

/// Strict decode of an Annex-B file with ffmpeg; returns (diagnostics, raw yuv420p bytes). Any diagnostic
/// text is a failure for the caller.
///
/// - `-v warning`: ffmpeg reports a picture with a damaged, missing or concealed slice as
///   `corrupt decoded frame`, which is a warning. At `-v error` a stream with a whole slice missing
///   decodes silently (#74).
/// - Error concealment stays at ffmpeg's default. With `-ec 0`, ffmpeg (7.1, 8.1) flags every picture
///   that has more than one slice as corrupt, libx264's multi-slice streams included, so that mode
///   can't tell a real defect from a clean multi-slice picture. Concealment can't hide a defect here:
///   a concealed picture is still reported, and the caller compares every decoded pixel with the
///   encoder's reconstruction.
/// - `-err_detect`: also report the non-conformances ffmpeg tolerates by default.
pub fn ffmpeg_decode(file: &Path) -> (String, Vec<u8>) {
    let out = Command::new(ffmpeg().unwrap())
        .args(["-nostdin", "-hide_banner", "-v", "warning", "-err_detect", "+crccheck+bitstream+buffer+explode", "-i"])
        .arg(file)
        .args(["-f", "rawvideo", "-pix_fmt", "yuv420p", "-"])
        .output()
        .expect("run ffmpeg");
    let mut diag = String::from_utf8_lossy(&out.stderr).into_owned();
    if !out.status.success() {
        diag.push_str(&format!("ffmpeg exited with {}", out.status));
    }
    (diag, out.stdout)
}

/// Annex-B NAL units of `stream` as byte ranges (start code included).
pub fn annexb_nals(stream: &[u8]) -> Vec<std::ops::Range<usize>> {
    let starts: Vec<usize> = (0..stream.len().saturating_sub(3)).filter(|&i| stream[i..i + 3] == [0, 0, 1]).collect();
    starts.iter().enumerate().map(|(k, &s)| s..starts.get(k + 1).copied().unwrap_or(stream.len())).collect()
}

/// Raw frames produced by ffmpeg from a lavfi source.
pub fn ffmpeg_source(src: &str, w: usize, h: usize, frames: usize) -> Vec<Yuv> {
    let out = Command::new(ffmpeg().unwrap())
        .args(["-v", "error", "-f", "lavfi", "-i"])
        .arg(format!("{src}=size={w}x{h}:rate=30"))
        .args(["-frames:v", &frames.to_string(), "-f", "rawvideo", "-pix_fmt", "yuv420p", "-"])
        .output()
        .expect("run ffmpeg");
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let fs = w * h * 3 / 2;
    out.stdout.chunks_exact(fs).map(|c| Yuv { w, h, y: c[..w * h].to_vec(), u: c[w * h..w * h * 5 / 4].to_vec(), v: c[w * h * 5 / 4..].to_vec() }).collect()
}

pub fn psnr(a: &[u8], b: &[u8]) -> f64 {
    assert_eq!(a.len(), b.len());
    let se: f64 = a.iter().zip(b).map(|(&x, &y)| (x as f64 - y as f64).powi(2)).sum();
    if se == 0.0 {
        return 99.0;
    }
    10.0 * (255.0f64 * 255.0 * a.len() as f64 / se).log10()
}
