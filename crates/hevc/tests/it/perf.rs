//! Decode speed (ignored by default; run with
//! `cargo test --release -p filmcraft-hevc --test it perf:: -- --ignored --nocapture`).

use crate::common;

use filmcraft_hevc::Decoder;
use std::time::Instant;

fn bench(data: &[u8], threads: usize, iters: usize) -> (usize, f64) {
    let mut best = f64::MAX;
    let mut frames = 0;
    for _ in 0..iters {
        let mut dec = Decoder::with_threads(threads);
        let t0 = Instant::now();
        frames = dec.decode(data, 0).expect("decode").len() + dec.flush().len();
        best = best.min(t0.elapsed().as_secs_f64());
    }
    (frames, best)
}

fn run(name: &str, label: &str) {
    let f = common::fixture(name);
    let Some((hevc, _)) = common::ensure(f) else { return };
    common::check_fixture(name).unwrap();
    let data = std::fs::read(hevc).unwrap();
    let cores = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1);
    for threads in [1, cores.min(16)] {
        let (frames, secs) = bench(&data, threads, 3);
        println!("{label} ({} KB): {threads} thread(s): {frames} frames in {secs:.3}s = {:.1} fps", data.len() / 1024, frames as f64 / secs);
    }
}

#[test]
#[ignore]
fn perf_1080p() {
    run("bench_1080p", "1080p Main CRF24 medium");
}

#[test]
#[ignore]
fn perf_4k() {
    run("bench_4k", "2160p Main10 CRF26 fast");
}
