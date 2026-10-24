// to_rgba8 YUV->RGBA display-conversion microbenchmark (filmcraft-frame).
use filmcraft_frame::{Chroma, PixelData, VideoFrame};
use filmcraft_color::ColorInfo;
use filmcraft_time::Tick;
use std::sync::Arc;
use std::time::Instant;
fn main() {
    let a: Vec<String> = std::env::args().collect();
    let (w, h, iters) = (1920usize, 1080usize, a.get(1).and_then(|s| s.parse().ok()).unwrap_or(30));
    for (name, chroma, sx, sy) in [("420", Chroma::C420, 1, 1), ("444", Chroma::C444, 0, 0)] {
        let (cw, ch) = (w.div_ceil(1 << sx), h.div_ceil(1 << sy));
        let mut s: u32 = 0x1234_5678;
        let mut nx = || { s = s.wrapping_mul(1_664_525).wrapping_add(1_013_904_223); (s >> 24) as u8 };
        let y: Vec<u8> = (0..w * h).map(|_| nx()).collect();
        let u: Vec<u8> = (0..cw * ch).map(|_| nx()).collect();
        let v: Vec<u8> = (0..cw * ch).map(|_| nx()).collect();
        let f = VideoFrame { width: w as u32, height: h as u32,
            data: PixelData::Yuv8 { planes: [Arc::new(y), Arc::new(u), Arc::new(v)], chroma, alpha: None },
            color: ColorInfo::REC709, par: (1, 1), pts: Tick::ZERO };
        let mut sum = 0u64;
        for _ in 0..3 { sum = sum.wrapping_add(f.to_rgba8().iter().map(|&b| b as u64).sum::<u64>()); }
        let t = Instant::now();
        for _ in 0..iters { let o = f.to_rgba8(); sum = sum.wrapping_add(o[o.len() / 2] as u64); }
        let ms = t.elapsed().as_secs_f64() * 1000.0 / iters as f64;
        let ck: u64 = f.to_rgba8().iter().enumerate().map(|(i, &b)| (i as u64 % 251 + 1) * b as u64).sum();
        println!("{name} ms_per_frame={ms:.3} checksum={ck} ({sum})");
    }
}
