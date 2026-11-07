//! Development tool: encode a raw yuv420p file (30 fps) with export-like settings and report the speed.
//!
//! `cargo run --release -p filmcraft-h264enc --example h264enc_bench -- IN.yuv W H FRAMES OUT.h264 [preset] [rate] [slices] [bframes] [threads]`
//! preset: speed|balanced|quality, rate: qpN | crfN | cbrN | vbrN (max = 1.5 x target), slices: -1 = export rule
//! (one per four macroblock rows), threads: 0 = all cores. Defaults match an export: balanced vbr20000 -1 2 0.
use filmcraft_h264enc::{Encoder, EncoderConfig, PacketFormat, Preset, RateControl, YuvFrame};
use std::time::Instant;

type Res<T> = Result<T, String>;

fn num<T: std::str::FromStr>(v: &str, what: &str) -> Res<T>
where
    T::Err: std::fmt::Display,
{
    v.parse().map_err(|e| format!("invalid {what} {v:?}: {e}"))
}

fn run(a: &[String]) -> Res<()> {
    let arg = |i: usize, d: &str| a.get(i).cloned().unwrap_or_else(|| d.to_string());
    if a.len() < 6 {
        return Err("usage: h264enc_bench IN.yuv W H FRAMES OUT.h264 [preset] [rate] [slices] [bframes] [threads]".into());
    }
    let w: usize = num(&arg(2, ""), "width")?;
    let h: usize = num(&arg(3, ""), "height")?;
    let frames: usize = num(&arg(4, ""), "frame count")?;
    // Same limits the encoder enforces, checked before any size arithmetic.
    let (w32, h32) = match (u32::try_from(w), u32::try_from(h)) {
        (Ok(w), Ok(h)) if w != 0 && h != 0 && w.is_multiple_of(2) && h.is_multiple_of(2) && w <= 16384 && h <= 16384 => (w, h),
        _ => return Err("dimensions must be non-zero, even and at most 16384".into()),
    };
    let luma = w.checked_mul(h).ok_or("frame size overflows")?;
    let fs = luma.checked_mul(3).ok_or("frame size overflows")? / 2;
    let raw = std::fs::read(arg(1, "")).map_err(|e| format!("read input: {e}"))?;
    let n = frames.min(raw.len() / fs);
    if n == 0 {
        return Err("input holds no complete frame".into());
    }
    let mut cfg = EncoderConfig::new(w32, h32, 30, 1);
    cfg.preset = match arg(6, "balanced").as_str() {
        "speed" => Preset::Speed,
        "quality" => Preset::Quality,
        _ => Preset::Balanced,
    };
    let rate = arg(7, "vbr20000");
    cfg.rate = if let Some(q) = rate.strip_prefix("qp") {
        RateControl::Qp(num(q, "qp")?)
    } else if let Some(q) = rate.strip_prefix("crf") {
        RateControl::Crf(num(q, "crf")?)
    } else if let Some(k) = rate.strip_prefix("cbr") {
        RateControl::Cbr { kbps: num(k, "kbps")? }
    } else {
        let k: u32 = num(rate.strip_prefix("vbr").unwrap_or(&rate), "kbps")?;
        let max_kbps = k.checked_mul(3).ok_or("vbr target too large")? / 2;
        RateControl::Vbr { target_kbps: k, max_kbps }
    };
    let slices: i64 = num(&arg(8, "-1"), "slices")?;
    cfg.slices = usize::try_from(slices).unwrap_or_else(|_| h.div_ceil(16).div_ceil(4).max(1));
    cfg.bframes = num(&arg(9, "2"), "bframes")?;
    cfg.threads = num(&arg(10, "0"), "threads")?;
    cfg.keyint = 60;
    cfg.format = PacketFormat::AnnexB;

    let mut enc = Encoder::new(cfg.clone()).map_err(|e| format!("encoder: {e}"))?;
    let mut out = Vec::new();
    let t0 = Instant::now();
    for (i, f) in raw.chunks_exact(fs).take(n).enumerate() {
        let (y, rest) = f.split_at(luma);
        let (u, v) = rest.split_at(luma / 4);
        let fr = YuvFrame { y, u, v, y_stride: w, uv_stride: w / 2 };
        for p in enc.encode(&fr, i as i64).map_err(|e| format!("encode: {e}"))? {
            out.extend_from_slice(&p.data);
        }
    }
    for p in enc.flush() {
        out.extend_from_slice(&p.data);
    }
    let el = t0.elapsed().as_secs_f64();
    std::fs::write(arg(5, ""), &out).map_err(|e| format!("write output: {e}"))?;
    let kbps = out.len() as f64 * 8.0 / (n as f64 / 30.0) / 1000.0;
    println!("frames={n} wall={el:.3}s fps={:.2} bytes={} kbps={kbps:.0} slices={} threads={}", n as f64 / el, out.len(), cfg.slices, cfg.threads);
    Ok(())
}

fn main() {
    let a: Vec<String> = std::env::args().collect();
    if let Err(e) = run(&a) {
        eprintln!("h264enc_bench: {e}");
        std::process::exit(2);
    }
}

#[cfg(test)]
mod tests {
    use super::run;

    fn args(v: &[&str]) -> Vec<String> {
        std::iter::once("h264enc_bench").chain(v.iter().copied()).map(String::from).collect()
    }

    #[test]
    fn bad_dimensions_are_clean_errors() {
        // The dimension check runs before the input is read, so the missing file is never reached.
        for (w, h) in [("0", "16"), ("16", "0"), ("15", "16"), ("16", "15"), ("16386", "16"), ("16", "16386"), ("99999999999", "16")] {
            let r = run(&args(&["/nonexistent.yuv", w, h, "1", "/nonexistent.h264"]));
            assert_eq!(r.unwrap_err(), "dimensions must be non-zero, even and at most 16384", "{w}x{h}");
        }
        let r = run(&args(&["/nonexistent.yuv", "abc", "16", "1", "/nonexistent.h264"]));
        assert!(r.unwrap_err().starts_with("invalid width"));
        // Valid dimensions get past the check and fail on the missing file instead.
        let r = run(&args(&["/nonexistent.yuv", "16384", "16", "1", "/nonexistent.h264"]));
        assert!(r.unwrap_err().starts_with("read input"));
    }

    #[test]
    fn oversized_vbr_target_is_rejected() {
        let p = std::env::temp_dir().join(format!("h264enc_bench_{}.yuv", std::process::id()));
        std::fs::write(&p, vec![0u8; 16 * 16 * 3 / 2]).unwrap();
        let r = run(&args(&[p.to_str().unwrap(), "16", "16", "1", "/nonexistent/out.h264", "speed", "vbr4294967295"]));
        let _ = std::fs::remove_file(&p);
        assert_eq!(r.unwrap_err(), "vbr target too large");
    }
}
