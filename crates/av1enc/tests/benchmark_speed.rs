//! Performance benchmark for AV1 software encoder.

use filmcraft_av1enc::{Encoder, EncoderConfig, Preset, RateControl, YuvFrame};
use std::time::Instant;

#[test]
fn bench_av1_speed_and_fps() {
    let resolutions = [("720p", 1280u32, 720u32, 50), ("1080p", 1920u32, 1080u32, 30), ("4K", 3840u32, 2160u32, 10)];

    println!("\n=======================================================");
    println!("     FilmCraft Pure-Rust AV1 Encoder Benchmark         ");
    println!("=======================================================");

    for &(name, w, h, frames_count) in &resolutions {
        for preset in [Preset::Speed, Preset::Balanced, Preset::Quality] {
            let mut cfg = EncoderConfig::new(w, h, 30, 1);
            cfg.preset = preset;
            cfg.rate_control = Some(RateControl::Qindex(100));

            let mut enc = Encoder::new(cfg).unwrap();

            // Create realistic synthetic frame with texture and gradients
            let mut y_plane = vec![128u8; (w * h) as usize];
            for y in 0..h {
                for x in 0..w {
                    let val = (((x ^ y) & 0x7F) + (y % 128)) as u8;
                    y_plane[(y * w + x) as usize] = val;
                }
            }
            let uv_plane = vec![128u8; ((w / 2) * (h / 2)) as usize];
            let frame = YuvFrame { y: &y_plane, u: &uv_plane, v: &uv_plane, y_stride: w as usize, uv_stride: (w / 2) as usize };

            // Warm-up
            let _ = enc.encode(&frame, 0).unwrap();

            let start = Instant::now();
            let mut total_bytes = 0usize;
            for i in 1..=frames_count {
                let packet = enc.encode(&frame, i * 33).unwrap();
                total_bytes += packet.data.len();
            }
            let elapsed = start.elapsed();
            let fps = (frames_count as f64) / elapsed.as_secs_f64();
            let ms_per_frame = (elapsed.as_secs_f64() * 1000.0) / (frames_count as f64);
            let avg_kb_per_frame = (total_bytes as f64 / frames_count as f64) / 1024.0;

            println!(
                "[{name:5}] Preset: {:<8} | {:>6.1} FPS | {:>5.2} ms/frame | {:>5.1} KB/frame",
                format!("{preset:?}"),
                fps,
                ms_per_frame,
                avg_kb_per_frame
            );
        }
        println!("-------------------------------------------------------");
    }
}
