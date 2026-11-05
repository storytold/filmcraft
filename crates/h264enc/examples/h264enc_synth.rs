//! Development tool: encode synthetic (or lavfi) content and verify against ffmpeg.
//!
//! `cargo run --release -p filmcraft-h264enc --example h264enc_synth -- W H FRAMES [profile] [preset] [bframes] [rate] [slices] [src]`
//! profile: baseline|main|high, preset: speed|balanced|quality, rate: qpN | crfN | cbrN | vbrN
#[path = "../tests/it/common/mod.rs"]
mod common;

use filmcraft_h264enc::{Encoder, EncoderConfig, Preset, Profile, RateControl};
use std::time::Instant;

fn main() {
    let a: Vec<String> = std::env::args().collect();
    let arg = |i: usize, d: &str| a.get(i).cloned().unwrap_or_else(|| d.to_string());
    let w: usize = arg(1, "320").parse().unwrap();
    let h: usize = arg(2, "240").parse().unwrap();
    let n: usize = arg(3, "10").parse().unwrap();
    let mut cfg = EncoderConfig::new(w as u32, h as u32, 30, 1);
    cfg.profile = match arg(4, "high").as_str() {
        "baseline" => Profile::Baseline,
        "main" => Profile::Main,
        _ => Profile::High,
    };
    cfg.preset = match arg(5, "balanced").as_str() {
        "speed" => Preset::Speed,
        "quality" => Preset::Quality,
        _ => Preset::Balanced,
    };
    cfg.bframes = arg(6, "0").parse().unwrap();
    let rate = arg(7, "qp26");
    cfg.rate = if let Some(q) = rate.strip_prefix("qp") {
        RateControl::Qp(q.parse().unwrap())
    } else if let Some(q) = rate.strip_prefix("crf") {
        RateControl::Crf(q.parse().unwrap())
    } else if let Some(q) = rate.strip_prefix("cbr") {
        RateControl::Cbr { kbps: q.parse().unwrap() }
    } else {
        let k: u32 = rate.strip_prefix("vbr").unwrap().parse().unwrap();
        RateControl::Vbr { target_kbps: k, max_kbps: k * 2 }
    };
    cfg.slices = arg(8, "0").parse().unwrap();
    let src_kind = arg(9, "synth");
    if let Ok(t) = std::env::var("THREADS") {
        cfg.threads = t.parse().unwrap();
    }
    if let Ok(k) = std::env::var("KEYINT") {
        cfg.keyint = k.parse().unwrap();
    }
    if let Ok(k) = std::env::var("AQ") {
        cfg.aq_strength = k.parse().unwrap();
    }
    // PERF=k: generate only k distinct pictures and cycle them (for long speed runs without verification).
    let distinct = std::env::var("PERF").ok().map_or(n, |k| k.parse::<usize>().unwrap().min(n));
    let base: Vec<common::Yuv> =
        if src_kind == "synth" { (0..distinct).map(|t| common::synth(w, h, t)).collect() } else { common::ffmpeg_source(&src_kind, w, h, distinct) };
    let perf = distinct < n;
    let frames: Vec<&common::Yuv> = (0..n).map(|i| &base[i % distinct]).collect();
    let dir = common::out_dir("dev");
    let mut enc = Encoder::new(cfg).unwrap();
    enc.set_recon_capture(true);
    let mut out = Vec::new();
    let mut pk = Vec::new();
    let t0 = Instant::now();
    if perf {
        enc.set_recon_capture(false);
    }
    for (i, f) in frames.iter().enumerate() {
        pk.extend(enc.encode(&f.frame(), i as i64).unwrap());
    }
    pk.extend(enc.flush());
    let el = t0.elapsed().as_secs_f64();
    for p in &pk {
        out.extend_from_slice(&p.data);
    }
    let file = dir.join("out.h264");
    std::fs::write(&file, &out).unwrap();
    let mut recon = enc.take_recon();
    recon.sort_by_key(|r| r.pts);
    let kbps = out.len() as f64 * 8.0 / (n as f64 / 30.0) / 1000.0;
    println!("encoded {n} frames in {el:.3}s = {:.1} fps, {} bytes, {kbps:.1} kbps", n as f64 / el, out.len());
    for p in pk.iter().take(12) {
        print!("{:?}(pts{} dts{} qp{} {}B) ", p.frame_type, p.pts, p.dts, p.qp, p.data.len());
    }
    println!();
    if common::ffmpeg().is_none() || perf {
        return;
    }
    let (err, dec) = common::ffmpeg_decode(&file);
    if !err.is_empty() {
        println!("ffmpeg stderr: {err}");
    }
    let fs = w * h * 3 / 2;
    println!("ffmpeg decoded {} frames (expected {n})", dec.len() / fs);
    let mut src_all = Vec::new();
    let mut psnr_sum = 0.0;
    for (i, r) in recon.iter().enumerate() {
        let mut rb = r.y.clone();
        rb.extend_from_slice(&r.u);
        rb.extend_from_slice(&r.v);
        let s = frames[i].bytes();
        src_all.extend_from_slice(&s);
        let p = common::psnr(&frames[i].y, &r.y);
        psnr_sum += p;
        if dec.len() >= (i + 1) * fs {
            let d = &dec[i * fs..(i + 1) * fs];
            if d != rb.as_slice() {
                let pos = d.iter().zip(&rb).position(|(a, b)| a != b).unwrap();
                let (plane, off) = if pos < w * h {
                    ("Y", pos)
                } else if pos < w * h * 5 / 4 {
                    ("U", pos - w * h)
                } else {
                    ("V", pos - w * h * 5 / 4)
                };
                let pw = if plane == "Y" { w } else { w / 2 };
                let (x, y) = (off % pw, off / pw);
                let mbsz = if plane == "Y" { 16 } else { 8 };
                let nd = d.iter().zip(&rb).filter(|(a, b)| a != b).count();
                println!(
                    "frame {i} (pts {}): MISMATCH first at {plane} ({x},{y}) mb ({},{}) dec={} rec={} ({nd} samples differ)",
                    r.pts,
                    x / mbsz,
                    y / mbsz,
                    d[pos],
                    rb[pos]
                );
                if std::env::var("MAP").is_ok() {
                    let (mbw, mbh) = (w.div_ceil(16), h.div_ceil(16));
                    for my in 0..mbh {
                        let mut line = String::new();
                        for mx in 0..mbw {
                            let mut n = 0;
                            for yy in my * 16..(my * 16 + 13).min(h) {
                                for xx in mx * 16..(mx * 16 + 13).min(w) {
                                    n += (d[yy * w + xx] != rb[yy * w + xx]) as usize;
                                }
                            }
                            line.push(if n == 0 {
                                '.'
                            } else if n < 16 {
                                'e'
                            } else {
                                'X'
                            });
                        }
                        println!("  {line}");
                    }
                    break;
                }
            }
        }
    }
    println!("mean luma PSNR vs source: {:.2} dB", psnr_sum / recon.len() as f64);
    std::fs::write(dir.join("src.yuv"), &src_all).unwrap();
}
