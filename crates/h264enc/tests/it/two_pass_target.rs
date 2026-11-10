//! Two-pass VBR must land near its target bitrate on slow moves over a detailed still, as a photo or a
//! drawing with Motion keyframes and holds gives.

use crate::common::Yuv;
use filmcraft_h264enc::*;

fn hash(x: i64, y: i64, octave: u64) -> f32 {
    let mut h = (x as u64).wrapping_mul(0x9e37_79b9_7f4a_7c15) ^ (y as u64).wrapping_mul(0xc2b2_ae3d_27d4_eb4f) ^ octave.wrapping_mul(0x1656_67b1_9e37_79f9);
    h ^= h >> 31;
    h = h.wrapping_mul(0xd6e8_feb8_6659_fd93);
    h ^= h >> 32;
    (h & 0xffff) as f32 / 65535.0
}

fn value_noise(x: f32, y: f32, octave: u64) -> f32 {
    let (xi, yi) = (x.floor() as i64, y.floor() as i64);
    let (fx, fy) = (x - x.floor(), y - y.floor());
    let (sx, sy) = (fx * fx * (3.0 - 2.0 * fx), fy * fy * (3.0 - 2.0 * fy));
    let (a, b) = (hash(xi, yi, octave), hash(xi + 1, yi, octave));
    let (c, d) = (hash(xi, yi + 1, octave), hash(xi + 1, yi + 1, octave));
    a + (b - a) * sx + (c - a) * sy + (a - b - c + d) * sx * sy
}

/// A still with detail at every scale down to the pixel: value noise summed over eight octaves.
fn detailed_still(w: usize, h: usize) -> Vec<u8> {
    let mut v = vec![0f32; w * h];
    for (k, p) in v.iter_mut().enumerate() {
        let (x, y) = ((k % w) as f32, (k / w) as f32);
        let (mut amp, mut freq, mut norm) = (1.0f32, 1.0 / 256.0, 0.0);
        for octave in 0..8 {
            *p += amp * value_noise(x * freq, y * freq, octave);
            norm += amp;
            amp *= 0.72;
            freq *= 2.0;
        }
        *p /= norm;
    }
    let (lo, hi) = v.iter().fold((f32::MAX, f32::MIN), |(lo, hi), &p| (lo.min(p), hi.max(p)));
    v.iter().map(|&p| ((p - lo) / (hi - lo).max(1e-6) * 235.0 + 10.0) as u8).collect()
}

fn ease(t: f64) -> f64 {
    let t = t.clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// Picture `t` of `n`: the still (`sw` x `sh`) pushed in from 100% to 108% with a slow pan, in two eased moves,
/// each followed by a hold, resampled bilinearly. The chroma follows the luma.
fn picture(still: &[u8], sw: usize, sh: usize, w: usize, h: usize, t: usize, n: usize) -> Yuv {
    let p = t as f64 / n as f64;
    let k = match p {
        p if p < 0.3 => 0.5 * ease(p / 0.3),
        p if p < 0.5 => 0.5,
        p if p < 0.8 => 0.5 + 0.5 * ease((p - 0.5) / 0.3),
        _ => 1.0,
    };
    let scale = 1.0 + 0.08 * k;
    let (cx, cy) = (sw as f64 / 2.0 + 40.0 * k, sh as f64 / 2.0 - 25.0 * k);
    let at = |x: usize, y: usize| f64::from(still[y.min(sh - 1) * sw + x.min(sw - 1)]);
    let mut y = vec![0u8; w * h];
    for (i, px) in y.iter_mut().enumerate() {
        let sx = cx + ((i % w) as f64 - w as f64 / 2.0) / scale;
        let sy = cy + ((i / w) as f64 - h as f64 / 2.0) / scale;
        let (x0, y0, fx, fy) = (sx as usize, sy as usize, sx.fract(), sy.fract());
        let top = at(x0, y0) * (1.0 - fx) + at(x0 + 1, y0) * fx;
        let bottom = at(x0, y0 + 1) * (1.0 - fx) + at(x0 + 1, y0 + 1) * fx;
        *px = (top * (1.0 - fy) + bottom * fy).round() as u8;
    }
    let luma = |i: usize| f64::from(y[(i / (w / 2)) * 2 * w + (i % (w / 2)) * 2]) - 128.0;
    let u = (0..w * h / 4).map(|i| (128.0 + 0.15 * luma(i)) as u8).collect();
    let v = (0..w * h / 4).map(|i| (128.0 - 0.10 * luma(i)) as u8).collect();
    Yuv { w, h, y, u, v }
}

/// Encode `frames` with `c` and return the bytes of the stream, and the first-pass statistics.
fn encode(c: EncoderConfig, frames: &[Yuv]) -> (usize, Option<PassStats>) {
    let mut enc = Encoder::new(c).unwrap();
    let mut bytes = 0;
    for (i, f) in frames.iter().enumerate() {
        bytes += enc.encode(&f.frame(), i as i64).unwrap().iter().map(|p| p.data.len()).sum::<usize>();
    }
    bytes += enc.flush().iter().map(|p| p.data.len()).sum::<usize>();
    (bytes, enc.pass_stats())
}

#[test]
fn two_pass_vbr_lands_near_its_target_on_slow_moves_over_a_detailed_still() {
    let (w, h, fps, n, kbps) = (640usize, 360usize, 24u32, 96usize, 400u32);
    let (sw, sh) = (w * 5 / 4, h * 5 / 4);
    let still = detailed_still(sw, sh);
    let frames: Vec<Yuv> = (0..n).map(|t| picture(&still, sw, sh, w, h, t, n)).collect();
    // as an export sets it up: High, Balanced, two B-frames, a keyframe every two seconds, a slice per four
    // macroblock rows, a peak of 1.5 times the target
    let mut c = EncoderConfig::new(w as u32, h as u32, fps, 1);
    c.keyint = 2 * fps;
    c.slices = 6;
    c.threads = 4;
    c.rate = RateControl::Vbr { target_kbps: kbps, max_kbps: kbps * 3 / 2 };
    c.pass = Pass::First;
    let (_, stats) = encode(c.clone(), &frames);
    c.pass = Pass::Second(stats.unwrap());
    let (bytes, _) = encode(c, &frames);
    let got = bytes as f64 * 8.0 / (n as f64 / f64::from(fps)) / 1000.0;
    eprintln!("2-pass: {got:.1} kbit/s (target {kbps})");
    assert!((got / f64::from(kbps) - 1.0).abs() <= 0.05, "{got:.1} kbit/s against a {kbps} kbit/s target");
}
