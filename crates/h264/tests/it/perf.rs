//! Decode speed on a 1080p High-profile stream (ignored by default; run with
//! `cargo test --release -p filmcraft-h264 --test it perf:: -- --ignored --nocapture`).

use crate::common;

use filmcraft_h264::Decoder;
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

#[test]
#[ignore]
fn perf_1080p() {
    let f = common::fixture("bench_1080p");
    let Some((h264, _)) = common::ensure(f) else { return };
    // correctness first
    common::check_fixture("bench_1080p").unwrap();
    let data = std::fs::read(h264).unwrap();
    let cores = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1);
    for threads in [1, cores.min(16)] {
        let (frames, secs) = bench(&data, threads, 5);
        println!("1080p High CRF20 ({} KB): {threads} thread(s): {frames} frames in {secs:.3}s = {:.1} fps", data.len() / 1024, frames as f64 / secs);
    }
}
