//! Decode speed (ignored; run with `cargo test --release -p filmcraft-mpeg2v --test it perf:: --
//! --ignored --nocapture`). Uses the 1080i fixture of `tests/it/oracle.rs` (ffmpeg-encoded, 25 Mb/s).

use crate::common;

use common::*;

#[test]
#[ignore]
fn perf_1080i_decode_fps() {
    let ff = filmcraft_testkit::require_ffmpeg!();
    let f = make(&ff, &HD1080I).expect("fixture");
    let es = std::fs::read(&f).unwrap();
    let aus = filmcraft_mpeg2v::access_units(&es);
    for threads in [false, true] {
        let mut best = f64::MAX;
        let mut frames = 0;
        for _ in 0..5 {
            let mut d = filmcraft_mpeg2v::Decoder::new();
            d.set_threads(threads);
            let t0 = std::time::Instant::now();
            let mut n = 0;
            for (i, r) in aus.iter().enumerate() {
                n += d.decode(&es[r.clone()], i as i64).unwrap().len();
            }
            n += d.flush().len();
            best = best.min(t0.elapsed().as_secs_f64());
            frames = n;
        }
        println!(
            "1080i25 MPEG-2 (4:2:0, 25 Mb/s, IBBP): {frames} frames in {:.1} ms = {:.1} fps ({})",
            best * 1e3,
            frames as f64 / best,
            if threads { "slice rows on rayon" } else { "single thread" }
        );
    }
}
