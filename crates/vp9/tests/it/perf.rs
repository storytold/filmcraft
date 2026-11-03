//! Decode speed (ignored by default; run with
//! `cargo test --release -p filmcraft-vp9 --test it perf:: -- --ignored --nocapture`).

use crate::common;

use filmcraft_vp9::Decoder;
use std::time::Instant;

fn bench(frames: &[&[u8]], threads: usize, iters: usize) -> (usize, f64) {
    let mut best = f64::MAX;
    let mut n = 0;
    for _ in 0..iters {
        let mut dec = Decoder::with_threads(threads);
        let t0 = Instant::now();
        n = 0;
        for f in frames {
            n += dec.decode(f, 0).expect("decode").len();
        }
        n += dec.flush().len();
        best = best.min(t0.elapsed().as_secs_f64());
    }
    (n, best)
}

fn run(name: &str, label: &str) {
    let f = common::fixture(name);
    let Some((ivf, _)) = common::ensure(f) else { return };
    common::check_fixture(name).unwrap();
    let data = std::fs::read(ivf).unwrap();
    let frames = common::ivf_frames(&data);
    let cores = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1);
    for threads in [1, cores.min(16)] {
        let (n, secs) = bench(&frames, threads, 3);
        println!("{label} ({} KB): {threads} thread(s): {n} frames in {secs:.3}s = {:.1} fps", data.len() / 1024, n as f64 / secs);
    }
}

#[test]
#[ignore]
fn perf_1080p() {
    run("bench_1080p", "1080p 8-bit, 4 tile columns");
}

#[test]
#[ignore]
fn perf_4k() {
    run("bench_4k", "2160p 8-bit, 8 tile columns");
}
