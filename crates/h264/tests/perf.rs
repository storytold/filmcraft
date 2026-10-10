//! Decode speed reference streams (ignored by default; run with
//! `cargo test --release -p filmcraft-h264 --test perf -- --ignored --nocapture`).
//!
//! Two streams, because the 8-bit 4:2:0 one does not predict the cost of deep bit depths or 4:2:2
//! chroma: `bench_1080p` (High, 8-bit 4:2:0) and `bench_1080p_422_10b` (High 4:2:2, 10-bit). Both
//! are verified bit-exact against ffmpeg before being timed, so a speed change can never be bought
//! with a correctness regression.

mod common;

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

/// Decode `name` and report fps for one thread and for all cores. Correctness first: the stream must
/// already be bit-exact against ffmpeg, or the timing is meaningless.
fn report(name: &str, iters: usize) {
    let f = common::fixture(name);
    let Some((h264, _)) = common::ensure(f) else { return };
    common::check_fixture(name).expect("fixture must be bit-exact before it is used as a benchmark");
    let data = match std::fs::read(h264) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("{name}: cannot read fixture: {e}");
            return;
        }
    };
    let cores = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1);
    println!("{name} ({} KB):", data.len() / 1024);
    for threads in [1, cores.min(16)] {
        let (frames, secs) = bench(&data, threads, iters);
        println!("  {threads:2} thread(s): {frames} frames in {secs:.3}s = {:.1} fps", frames as f64 / secs);
    }
}

#[test]
#[ignore]
fn perf_1080p() {
    report("bench_1080p", 5);
}

#[test]
#[ignore]
fn perf_1080p_422_10b() {
    report("bench_1080p_422_10b", 5);
}
