//! Hostile-input robustness: truncation at every length, byte/bit mutation and header-field
//! sweeps over a small ffmpeg-generated stream must never panic or run away (errors are fine).
//! Skips when ffmpeg is missing.

mod common;

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::time::{Duration, Instant};

use filmcraft_mpeg2v::{Decoder, access_units, probe, scan_access_unit};

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n.max(1) as u64) as usize
    }
}

const SPECS: [common::Spec; 3] = [
    common::Spec {
        name: "m2v_fuzz_seed",
        lavfi: "testsrc2=size=64x48:rate=25",
        frames: 8,
        enc: &["-c:v", "mpeg2video", "-bf", "2", "-g", "6", "-b:v", "500k"],
        pix_fmt: "yuv420p",
    },
    common::Spec {
        name: "m2v_fuzz_seed_interlaced",
        lavfi: "testsrc2=size=96x64:rate=25,tinterlace=mode=interleave_top,setfield=tff",
        frames: 6,
        enc: &["-c:v", "mpeg2video", "-flags", "+ilme+ildct", "-bf", "1", "-g", "6", "-b:v", "800k"],
        pix_fmt: "yuv420p",
    },
    common::Spec {
        name: "m2v_fuzz_seed_422",
        lavfi: "testsrc2=size=64x48:rate=25",
        frames: 4,
        enc: &["-c:v", "mpeg2video", "-pix_fmt", "yuv422p", "-profile:v", "0", "-b:v", "1M"],
        pix_fmt: "yuv422p",
    },
];

static PANICS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// Decoder worker threads catch panics and turn them into errors; count them so they still fail the test.
fn count_panics() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        std::panic::set_hook(Box::new(|info| {
            PANICS.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            eprintln!("PANIC: {info}");
        }));
    });
}

fn run_all(d: &[u8], label: &str) {
    count_panics();
    let before = PANICS.load(std::sync::atomic::Ordering::SeqCst);
    let t = Instant::now();
    let res = catch_unwind(AssertUnwindSafe(|| {
        let _ = probe(d);
        let _ = scan_access_unit(d);
        for threads in [false, true] {
            let mut dec = Decoder::new();
            dec.set_threads(threads);
            for (i, r) in access_units(d).into_iter().enumerate() {
                let _ = dec.decode(&d[r], i as i64);
            }
            // also the whole buffer as one unit
            let _ = dec.decode(d, 99);
            let _ = dec.flush();
        }
    }));
    assert!(res.is_ok() && PANICS.load(std::sync::atomic::Ordering::SeqCst) == before, "panic: {label}");
    assert!(t.elapsed() < Duration::from_secs(20), "too slow ({:?}): {label}", t.elapsed());
}

#[test]
fn hostile_streams_never_panic() {
    let Some(ff) = filmcraft_testkit::ffmpeg_or_skip("mpeg2v fuzz") else { return };
    for spec in &SPECS {
        let Some(path) = common::make(&ff, spec) else { continue };
        fuzz(&std::fs::read(path).unwrap());
    }
}

fn fuzz(seed: &[u8]) {
    // every truncation length (stride keeps it quick on larger seeds)
    let stride = (seed.len() / 600).max(1);
    for n in (0..seed.len()).step_by(stride) {
        run_all(&seed[..n], &format!("truncate {n}"));
    }
    // header region: every byte to 0 / 0xff / 0x80 / each single bit
    for p in 0..seed.len().min(96) {
        for v in [0u8, 0xff, 0x80, 0x01] {
            let mut d = seed.to_vec();
            d[p] = v;
            run_all(&d, &format!("byte {p}={v:#x}"));
        }
        for b in 0..8 {
            let mut d = seed.to_vec();
            d[p] ^= 1 << b;
            run_all(&d, &format!("flip {p}.{b}"));
        }
    }
    // random multi-mutations
    let mut rng = Rng(0x9e3779b97f4a7c15);
    for i in 0..iters() {
        let mut d = seed.to_vec();
        for _ in 0..1 + rng.below(6) {
            let p = rng.below(d.len());
            if rng.below(2) == 0 {
                d[p] ^= 1 << rng.below(8);
            } else {
                d[p] = rng.next() as u8;
            }
        }
        run_all(&d, &format!("random {i}"));
    }
}

/// Random-mutation rounds per seed (raise with FILMCRAFT_FUZZ_ITERS for a long hunt).
fn iters() -> usize {
    std::env::var("FILMCRAFT_FUZZ_ITERS").ok().and_then(|s| s.parse().ok()).unwrap_or(2500)
}
