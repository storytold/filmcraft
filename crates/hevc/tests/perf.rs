//! Decode speed (ignored by default; run with
//! `cargo test --release -p filmcraft-hevc --test perf -- --ignored --nocapture`).

mod common;

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

/// Stable JSON records for alternating two builds; validation and hashing are outside the timer.
#[test]
#[ignore]
fn decode_cpu_benchmark() {
    let iterations = std::env::var("FILMCRAFT_DECODE_BENCH_ITERS").ok().and_then(|s| s.parse().ok()).unwrap_or(3usize);
    for name in ["bench_1080p", "bench_4k"] {
        let f = common::fixture(name);
        let (hevc, _) = common::ensure(f).expect("benchmark requires the ffmpeg oracle");
        assert!(common::check_fixture(name).unwrap());
        let data = std::fs::read(hevc).unwrap();
        let mut seconds = Vec::new();
        let mut expected = None;
        let mut count = 0;
        for _ in 0..iterations {
            let mut dec = Decoder::with_threads(1);
            let start = Instant::now();
            let mut pics = dec.decode(&data, 0).unwrap();
            pics.extend(dec.flush());
            let elapsed = start.elapsed().as_secs_f64();
            let checksum = pics
                .iter()
                .flat_map(|p| [&p.y, &p.u, &p.v])
                .flat_map(|p| p.to_le_bytes())
                .fold(0xcbf2_9ce4_8422_2325u64, |h, v| (h ^ u64::from(v)).wrapping_mul(0x100_0000_01b3));
            let value = (pics.len(), checksum);
            assert_eq!(*expected.get_or_insert(value), value);
            count = pics.len();
            seconds.push(elapsed);
        }
        seconds.sort_by(f64::total_cmp);
        println!(
            "DECODE_BENCH {{\"name\":\"{name}\",\"frames\":{count},\"seconds\":{},\"checksum\":{},\"samples\":{seconds:?}}}",
            seconds[seconds.len() / 2],
            expected.unwrap().1
        );
    }
}
