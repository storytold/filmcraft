//! Decode + develop speed of a synthetic 4096 × 2160 12-bit lossless-JPEG CinemaDNG frame, the
//! current pipeline against the reference one of PR #1068, interleaved in one run at 1, 4 and 16
//! threads (rayon pools). `cargo run --release -p filmcraft-dng --features writer --example bench`
//! (`-- --reps N`, `-- --quick` for a 1920 × 1080 frame, `-- --target 1920x1080` for the reduced
//! path). Times are the best of the repetitions, in ms.

use std::time::Instant;

use filmcraft_dng::tiff::tags;
use filmcraft_dng::writer::{Compression, Layout, Options, Value, write};
use filmcraft_dng::{Dng, ProcessOptions};

fn frame(w: usize, h: usize, layout: Layout) -> Vec<u8> {
    // a smooth scene with sensor-like noise, black 256, white 4095
    let mut s = 0x1234_5678u32;
    let px: Vec<u16> = (0..w * h)
        .map(|i| {
            s ^= s << 13;
            s ^= s >> 17;
            s ^= s << 5;
            let (x, y) = ((i % w) as f32 / w as f32, (i / w) as f32 / h as f32);
            let c = (i % w) % 2 + (i / w) % 2;
            let v = 0.15 + 0.6 * (x * 3.0 + y * 2.0 + c as f32).sin().abs() * y;
            (256.0 + v * 3500.0 + (s % 48) as f32).min(4095.0) as u16
        })
        .collect();
    let cm = [0.9f64, -0.3, -0.05, -0.4, 1.2, 0.2, -0.05, 0.15, 0.6];
    let o = Options {
        width: w,
        height: h,
        bits: 12,
        layout,
        compression: Compression::LosslessJpeg { predictor: 1, components: 2, restart_lines: 0, pt: 0 },
        main_tags: vec![
            (tags::COLOR_MATRIX_1, Value::srationals(&cm)),
            (tags::CALIBRATION_ILLUMINANT_1, Value::Short(vec![21])),
            (tags::AS_SHOT_NEUTRAL, Value::rationals(&[0.55, 1.0, 0.7])),
            (tags::FRAME_RATE, Value::SRational(vec![(24, 1)])),
        ],
        raw_tags: vec![(tags::BLACK_LEVEL, Value::Short(vec![256])), (tags::WHITE_LEVEL, Value::Short(vec![4095]))],
        ..Default::default()
    };
    write(&px, &o)
}

/// Best of the repetitions: the least disturbed by other work on a shared machine.
fn best(v: Vec<f64>) -> f64 {
    v.into_iter().fold(f64::INFINITY, f64::min)
}

fn ms(t: Instant) -> f64 {
    t.elapsed().as_secs_f64() * 1000.0
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let quick = args.iter().any(|a| a == "--quick");
    // profiling: skip the reference
    let only_new = args.iter().any(|a| a == "--only-new");
    let reps: usize = args.iter().position(|a| a == "--reps").and_then(|i| args.get(i + 1)).and_then(|v| v.parse().ok()).unwrap_or(7);
    let (w, h) = if quick { (1920, 1080) } else { (4096, 2160) };
    let target = args.iter().position(|a| a == "--target").and_then(|i| args.get(i + 1)).and_then(|v| {
        let (a, b) = v.split_once('x')?;
        Some((a.parse().ok()?, b.parse().ok()?))
    });
    let opts = ProcessOptions { target_size: target, ..Default::default() };
    for (name, layout) in [("tiles 256x256", Layout::Tiles { width: 256, height: 256 }), ("one strip", Layout::Strips { rows: h })] {
        let f = frame(w, h, layout);
        println!("\n{w}x{h} 12-bit LJ92, {name}, {} MB, options {opts:?}", f.len() / 1_000_000);
        println!("loadavg: {}", std::fs::read_to_string("/proc/loadavg").unwrap_or_default().trim());
        println!(
            "{:>7} | {:>9} {:>9} {:>9} | {:>9} {:>9} {:>9} | {:>7}",
            "threads", "ref raw", "ref dev", "ref all", "new raw", "new dev", "new all", "speedup"
        );
        for threads in [1, 4, 16] {
            let pool = rayon::ThreadPoolBuilder::new().num_threads(threads).build().unwrap();
            let (mut rr, mut ra, mut nr, mut na) = (vec![], vec![], vec![], vec![]);
            pool.install(|| {
                for _ in 0..reps {
                    let d = Dng::parse(&f).unwrap();
                    if !only_new {
                        let t = Instant::now();
                        std::hint::black_box(d.reference_raw().unwrap());
                        rr.push(ms(t));
                        let t = Instant::now();
                        std::hint::black_box(d.reference_develop(&opts).unwrap());
                        ra.push(ms(t));
                    }
                    let t = Instant::now();
                    std::hint::black_box(d.raw().unwrap());
                    nr.push(ms(t));
                    let t = Instant::now();
                    std::hint::black_box(d.develop_with(&opts).unwrap());
                    na.push(ms(t));
                }
            });
            let (rr, ra, nr, na) = (best(rr), best(ra), best(nr), best(na));
            println!("{threads:>7} | {rr:>9.1} {:>9.1} {ra:>9.1} | {nr:>9.1} {:>9.1} {na:>9.1} | {:>6.2}x", ra - rr, na - nr, ra / na);
        }
        println!("loadavg: {}", std::fs::read_to_string("/proc/loadavg").unwrap_or_default().trim());
    }
}
