//! Decode/encode throughput. Run with
//! `cargo test --release -p filmcraft-prores --test it perf:: -- --ignored --nocapture`.

use crate::common;
use filmcraft_prores::*;
use std::time::Instant;

/// A 1080p frame whose HQ encoding uses the full nominal rate (detailed, noisy content).
fn busy_1080p(chroma: ChromaFormat) -> Frame {
    let mut f = Frame::new(1920, 1080, chroma, 10, false);
    let mut s = 0x1234_5678u32;
    let mut rnd = move || {
        s ^= s << 13;
        s ^= s >> 17;
        s ^= s << 5;
        s
    };
    let cw = f.chroma_width() as usize;
    for y in 0..1080usize {
        for x in 0..1920usize {
            let base = 200 + ((x * 3 + y * 2) % 600) as u32;
            f.y[y * 1920 + x] = (base + rnd() % 48) as u16;
        }
        for x in 0..cw {
            f.cb[y * cw + x] = (480 + (x + y) as u32 % 64 + rnd() % 16) as u16;
            f.cr[y * cw + x] = (500 + (x * 7) as u32 % 50 + rnd() % 16) as u16;
        }
    }
    f
}

fn fps(frame: &[u8], threads: bool, secs: f64) -> f64 {
    let opts = DecodeOptions { bit_depth: None, threads };
    // Reuse the output planes across frames, as a player would.
    let mut out = decode_frame_with(frame, &opts).unwrap();
    let t = Instant::now();
    let mut n = 0;
    while t.elapsed().as_secs_f64() < secs {
        decode_frame_into(frame, &opts, &mut out).unwrap();
        std::hint::black_box(&out);
        n += 1;
    }
    n as f64 / t.elapsed().as_secs_f64()
}

#[test]
#[ignore]
fn decode_1080p_fps() {
    let mut streams: Vec<(String, Vec<u8>)> = Vec::new();
    for p in [Profile::Hq, Profile::P4444] {
        let src = busy_1080p(p.chroma());
        let mut enc = Encoder::new(p, 1920, 1080);
        let t = Instant::now();
        let coded = enc.encode(&src).unwrap();
        println!("encode {p:?} 1080p: {:.1} ms, {} bytes", t.elapsed().as_secs_f64() * 1e3, coded.len());
        streams.push((format!("ours {p:?} (nominal rate)"), coded));
    }
    if let Some(ff) = common::ffmpeg() {
        let spec = common::Spec::new("perf_ks_hq_mandel_1080", "mandelbrot=s=1920x1080:r=25,format=yuv422p10le", 1, &["-c:v", "prores_ks", "-profile:v", "3"]);
        let mov = common::make(&ff, &spec);
        streams.push(("prores_ks HQ mandelbrot".into(), common::packets(&ff, &mov).remove(0)));
    }
    for (name, s) in &streams {
        let mt = fps(s, true, 2.0);
        let st = fps(s, false, 2.0);
        println!("{name:<32} {:>8} bytes: {mt:7.1} fps threaded, {st:6.1} fps single-threaded", s.len());
    }
}
