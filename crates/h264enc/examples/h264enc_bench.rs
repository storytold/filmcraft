//! Development tool: encode a raw yuv420p file (30 fps) with export-like settings and report the speed.
//!
//! `cargo run --release -p filmcraft-h264enc --example h264enc_bench -- IN.yuv W H FRAMES OUT.h264 [preset] [rate] [slices] [bframes] [threads]`
//! preset: speed|balanced|quality, rate: qpN | crfN | cbrN | vbrN (max = 1.5 x target), slices: -1 = export rule
//! (one per four macroblock rows), threads: 0 = all cores. Defaults match an export: balanced vbr20000 -1 2 0.
use filmcraft_h264enc::{Encoder, EncoderConfig, PacketFormat, Preset, RateControl, YuvFrame};
use std::time::Instant;

fn main() {
    let a: Vec<String> = std::env::args().collect();
    let arg = |i: usize, d: &str| a.get(i).cloned().unwrap_or_else(|| d.to_string());
    if a.len() < 6 {
        eprintln!("usage: h264enc_bench IN.yuv W H FRAMES OUT.h264 [preset] [rate] [slices] [bframes] [threads]");
        std::process::exit(2);
    }
    let w: usize = arg(2, "").parse().expect("width");
    let h: usize = arg(3, "").parse().expect("height");
    let frames: usize = arg(4, "").parse().expect("frame count");
    let raw = std::fs::read(arg(1, "")).expect("read input");
    let fs = w * h * 3 / 2;
    let n = frames.min(raw.len() / fs);
    let mut cfg = EncoderConfig::new(w as u32, h as u32, 30, 1);
    cfg.preset = match arg(6, "balanced").as_str() {
        "speed" => Preset::Speed,
        "quality" => Preset::Quality,
        _ => Preset::Balanced,
    };
    let rate = arg(7, "vbr20000");
    cfg.rate = if let Some(q) = rate.strip_prefix("qp") {
        RateControl::Qp(q.parse().expect("qp"))
    } else if let Some(q) = rate.strip_prefix("crf") {
        RateControl::Crf(q.parse().expect("crf"))
    } else if let Some(k) = rate.strip_prefix("cbr") {
        RateControl::Cbr { kbps: k.parse().expect("kbps") }
    } else {
        let k: u32 = rate.strip_prefix("vbr").unwrap_or(&rate).parse().expect("kbps");
        RateControl::Vbr { target_kbps: k, max_kbps: k * 3 / 2 }
    };
    let slices: i64 = arg(8, "-1").parse().expect("slices");
    cfg.slices = usize::try_from(slices).unwrap_or_else(|_| h.div_ceil(16).div_ceil(4).max(1));
    cfg.bframes = arg(9, "2").parse().expect("bframes");
    cfg.threads = arg(10, "0").parse().expect("threads");
    cfg.keyint = 60;
    cfg.format = PacketFormat::AnnexB;

    let mut enc = Encoder::new(cfg.clone()).expect("encoder");
    let mut out = Vec::new();
    let t0 = Instant::now();
    for (i, f) in raw.chunks_exact(fs).take(n).enumerate() {
        let (y, rest) = f.split_at(w * h);
        let (u, v) = rest.split_at(w * h / 4);
        let fr = YuvFrame { y, u, v, y_stride: w, uv_stride: w / 2 };
        for p in enc.encode(&fr, i as i64).expect("encode") {
            out.extend_from_slice(&p.data);
        }
    }
    for p in enc.flush() {
        out.extend_from_slice(&p.data);
    }
    let el = t0.elapsed().as_secs_f64();
    std::fs::write(arg(5, ""), &out).expect("write output");
    let kbps = out.len() as f64 * 8.0 / (n as f64 / 30.0) / 1000.0;
    println!("frames={n} wall={el:.3}s fps={:.2} bytes={} kbps={kbps:.0} slices={} threads={}", n as f64 / el, out.len(), cfg.slices, cfg.threads);
}
