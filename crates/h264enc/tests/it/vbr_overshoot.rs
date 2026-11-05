//! Single-pass VBR must land near its target bitrate even on a short, high-motion clip.

use crate::common::{self, Yuv};
use filmcraft_h264enc::*;
use std::io::Read;
use std::process::{Command, Stdio};

/// Encode two seconds of a zooming fractal at 1080p, streaming the frames from ffmpeg into the encoder, and return
/// the resulting bitrate; `None` when there is no ffmpeg.
fn encode_clip(fps: usize) -> Option<f64> {
    let (w, h) = (1920usize, 1080usize);
    let ffmpeg = common::ffmpeg()?;
    let mut child = Command::new(ffmpeg)
        .args(["-v", "error", "-f", "lavfi", "-i"])
        .arg(format!("mandelbrot=size={w}x{h}:rate={fps}"))
        .args(["-frames:v", &(2 * fps).to_string(), "-f", "rawvideo", "-pix_fmt", "yuv420p", "-"])
        .stdout(Stdio::piped())
        .spawn()
        .expect("start ffmpeg");
    let mut stdout = child.stdout.take().expect("ffmpeg stdout");
    // the export defaults: High, Balanced, VBR 20 Mbit/s with a 30 Mbit/s peak, two B-frames, a slice per four MB rows
    let mut c = EncoderConfig::new(w as u32, h as u32, fps as u32, 1);
    c.profile = Profile::High;
    c.preset = Preset::Balanced;
    c.bframes = 2;
    c.slices = 17;
    c.rate = RateControl::Vbr { target_kbps: 20_000, max_kbps: 30_000 };
    let mut enc = Encoder::new(c).unwrap();
    let mut buf = vec![0u8; w * h * 3 / 2];
    let (mut bytes, mut n) = (0usize, 0usize);
    while stdout.read_exact(&mut buf).is_ok() {
        let f = Yuv { w, h, y: buf[..w * h].to_vec(), u: buf[w * h..w * h * 5 / 4].to_vec(), v: buf[w * h * 5 / 4..].to_vec() };
        bytes += enc.encode(&f.frame(), n as i64).unwrap().iter().map(|p| p.data.len()).sum::<usize>();
        n += 1;
    }
    let status = child.wait().expect("wait for ffmpeg");
    assert!(status.success(), "ffmpeg exited with {status}");
    assert_eq!(n, 2 * fps, "ffmpeg produced {n} frames");
    bytes += enc.flush().iter().map(|p| p.data.len()).sum::<usize>();
    Some(bytes as f64 * 8.0 / (n as f64 / fps as f64) / 1000.0)
}

#[test]
fn default_vbr_lands_near_its_target_on_a_short_high_motion_clip() {
    for fps in [30usize, 60] {
        let Some(kbps) = encode_clip(fps) else { return };
        // +9.8% (30 fps) and +13.5% (60 fps) before the rate controller's correction horizon was shortened
        assert!((kbps / 20_000.0 - 1.0).abs() <= 0.06, "{fps} fps: {kbps:.0} kbit/s against a 20000 kbit/s target");
    }
}

/// Noisy blocks whose detail changes from frame to frame, so each frame costs a different number of bits.
fn busy_frame(w: usize, h: usize, i: usize) -> Yuv {
    let detail = 1 + (i * 7) % 5;
    let px = |x: usize, y: usize| ((x * 31 + y * 17 + i * 13) ^ (x * y / detail)) as u8;
    let y: Vec<u8> = (0..w * h).map(|k| px(k % w, k / w)).collect();
    Yuv { w, h, y, u: vec![128; w * h / 4], v: vec![128; w * h / 4] }
}

#[test]
fn one_fps_vbr_does_not_swing_the_qp_from_frame_to_frame() {
    let (w, h) = (320usize, 240usize);
    let mut c = EncoderConfig::new(w as u32, h as u32, 1, 1);
    c.bframes = 0;
    c.rate = RateControl::Vbr { target_kbps: 400, max_kbps: 600 };
    let mut enc = Encoder::new(c).unwrap();
    let mut qps = Vec::new();
    for i in 0..24 {
        let f = busy_frame(w, h, i);
        qps.extend(enc.encode(&f.frame(), i as i64).unwrap().iter().map(|p| i32::from(p.qp)));
    }
    qps.extend(enc.flush().iter().map(|p| i32::from(p.qp)));
    assert_eq!(qps.len(), 24);
    let swing = qps.windows(2).skip(1).map(|p| (p[1] - p[0]).abs()).max().unwrap_or(0);
    assert!(swing <= 4, "QP swings by {swing} between frames: {qps:?}");
}
