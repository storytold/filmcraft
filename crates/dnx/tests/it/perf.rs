//! Throughput: `cargo test --release -p filmcraft-dnx --test it perf:: -- --ignored --nocapture`.

use filmcraft_dnx::{ChromaFormat, DecodeOptions, Encoder, EncoderConfig, Frame, Profile, decode_frame_with};
use std::time::Instant;

fn busy(w: u32, h: u32, chroma: ChromaFormat, depth: u8) -> Frame {
    let mut f = Frame::new(w, h, chroma, depth, false);
    let max = (1u32 << depth) - 1;
    let mut s = 1u32;
    for v in f.y.iter_mut().chain(f.cb.iter_mut()).chain(f.cr.iter_mut()) {
        s ^= s << 13;
        s ^= s >> 17;
        s ^= s << 5;
        *v = (((*v as u32) + (s % (24 << (depth - 8)))).min(max)) as u16;
    }
    for y in 0..h {
        for x in 0..w {
            let i = (y * w + x) as usize;
            f.y[i] = (f.y[i] as u32 + (((x ^ y) % 160) << (depth - 8))).min(max) as u16;
        }
    }
    f
}

#[test]
#[ignore]
fn throughput() {
    for (profile, depth, w, h) in [(Profile::Hq, 8, 1920, 1080), (Profile::Hqx, 10, 1920, 1080), (Profile::Sq, 8, 3840, 2160), (Profile::Hqx, 10, 3840, 2160)] {
        let mut cfg = EncoderConfig::new(profile, w, h);
        cfg.bit_depth = depth;
        let src = busy(w, h, cfg.chroma(), depth);
        let mut enc = Encoder::with_config(cfg).unwrap();
        let data = enc.encode(&src).unwrap();
        let n = if w > 1920 { 5 } else { 20 };
        let t = Instant::now();
        for _ in 0..n {
            enc.encode(&src).unwrap();
        }
        let enc_fps = n as f64 / t.elapsed().as_secs_f64();
        let mut fps = [0f64; 2];
        for (k, threads) in [true, false].into_iter().enumerate() {
            let opts = DecodeOptions { threads, ..Default::default() };
            let m = if threads { n * 3 } else { n / 2 };
            let t = Instant::now();
            for _ in 0..m {
                decode_frame_with(&data, &opts).unwrap();
            }
            fps[k] = m as f64 / t.elapsed().as_secs_f64();
        }
        println!(
            "{} {depth}-bit {w}x{h} ({} KB/frame): decode {:.0} fps threaded, {:.1} fps single-thread; encode {:.1} fps",
            profile.name(),
            data.len() / 1024,
            fps[0],
            fps[1],
            enc_fps
        );
    }
}
