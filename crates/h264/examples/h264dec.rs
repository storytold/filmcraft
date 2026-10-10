//! Decode an Annex-B H.264 file to raw planar YUV (8-bit bytes or 16-bit little-endian samples):
//! `h264dec in.h264 [out.yuv]`.
//! Prints timing information to stderr. `H264_BENCH_ITERS=n` (with `H264_THREADS=t`, and
//! `H264_DRAFT=1` for draft mode) turns it into a benchmark.

use filmcraft_h264::{Decoder, Plane};
use std::io::Write;

/// Append one output plane to the raw stream (u8 samples as bytes, u16 little-endian).
fn put_plane(o: &mut std::io::BufWriter<std::fs::File>, pl: &Plane) {
    match pl {
        Plane::U8(v) => {
            o.write_all(v).unwrap();
        }
        Plane::U16(v) => {
            for &s in v {
                o.write_all(&s.to_le_bytes()).unwrap();
            }
        }
    }
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 2 {
        eprintln!("usage: h264dec in.h264 [out.yuv]");
        std::process::exit(2);
    }
    let data = std::fs::read(&args[1]).expect("read input");
    // H264_BENCH_ITERS=n: decode n times without output and report the best run
    if let Some(n) = std::env::var("H264_BENCH_ITERS").ok().and_then(|v| v.parse::<usize>().ok()) {
        let mut best = f64::MAX;
        let mut frames = 0;
        let threads = std::env::var("H264_THREADS").ok().and_then(|v| v.parse::<usize>().ok());
        for _ in 0..n {
            let mut dec = threads.map(Decoder::with_threads).unwrap_or_default();
            dec.set_draft(std::env::var_os("H264_DRAFT").is_some());
            let t0 = std::time::Instant::now();
            frames = dec.decode(&data, 0).expect("decode").len() + dec.flush().len();
            best = best.min(t0.elapsed().as_secs_f64());
        }
        eprintln!("best of {n}: {frames} frames in {best:.3}s ({:.1} fps)", frames as f64 / best);
        return;
    }
    let mut out = args.get(2).map(|p| std::io::BufWriter::new(std::fs::File::create(p).expect("create output")));
    let mut dec = Decoder::new();
    let t0 = std::time::Instant::now();
    let mut n = 0usize;
    let mut write = |pics: Vec<filmcraft_h264::Picture>| {
        for p in pics {
            n += 1;
            if let Some(o) = out.as_mut() {
                put_plane(o, &p.y);
                put_plane(o, &p.u);
                put_plane(o, &p.v);
            }
        }
    };
    match dec.decode(&data, 0) {
        Ok(p) => write(p),
        Err(e) => eprintln!("decode error: {e}"),
    }
    write(dec.flush());
    let dt = t0.elapsed().as_secs_f64();
    if let Some(e) = dec.take_error() {
        eprintln!("decode error: {e}");
    }
    if std::env::var("H264_STATS").is_ok() {
        eprintln!("{:#?}", dec.stats());
    }
    eprintln!("{n} frames in {dt:.3}s ({:.1} fps)", n as f64 / dt);
}
