//! Hostile-input robustness: header-region sweeps (SPS/PPS/first slice headers), truncation and
//! random mutation of small libx265 streams must never panic or run away. Skips without ffmpeg.

mod common;

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use filmcraft_hevc::Decoder;

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

/// (name, extra x264 args, pix_fmt)
const SEEDS: &[(&str, &str, &str)] = &[
    ("hevc_fz_main_b", "-x265-params log-level=error:bframes=2:keyint=8:min-keyint=8:sao=1:amp=1", "yuv420p"),
    ("hevc_fz_main10", "-x265-params log-level=error:bframes=1:keyint=6:min-keyint=6", "yuv420p10le"),
    ("hevc_fz_wpp_slices", "-x265-params log-level=error:keyint=6:min-keyint=6:wpp=1:slices=3", "yuv420p"),
    ("hevc_fz_tiles", "-x265-params log-level=error:keyint=6:min-keyint=6:tiles=2x2", "yuv420p"),
    ("hevc_fz_422_10", "-x265-params log-level=error:keyint=6:min-keyint=6", "yuv422p10le"),
];

fn make(ff: &std::path::Path, name: &str, args: &str, pix: &str) -> Option<Vec<u8>> {
    let out = filmcraft_testkit::fixtures_dir("hevc").join(format!("{name}.hevc"));
    let p = filmcraft_testkit::fixtures::generate(&out, |tmp| {
        let a: Vec<&str> = args.split_whitespace().collect();
        matches!(
            Command::new(ff)
                .args(["-hide_banner", "-loglevel", "error", "-y", "-f", "lavfi", "-i", "testsrc2=size=64x48:rate=25", "-frames:v", "7", "-pix_fmt", pix, "-c:v", "libx265"])
                .args(a)
                .args(["-f", "hevc"])
                .arg(tmp)
                .stdin(Stdio::null())
                .status(),
            Ok(s) if s.success()
        )
    })?;
    std::fs::read(p).ok()
}

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
        for threads in [1, 3] {
            let mut dec = Decoder::with_threads(threads);
            for au in common::split_access_units(d) {
                let _ = dec.decode(au, 0);
            }
            let _ = dec.decode(d, 1);
            let _ = dec.flush();
            let _ = dec.take_error();
        }
    }));
    assert!(res.is_ok() && PANICS.load(std::sync::atomic::Ordering::SeqCst) == before, "panic: {label}");
    assert!(t.elapsed() < Duration::from_secs(20), "too slow ({:?}): {label}", t.elapsed());
}

#[test]
fn hostile_streams_never_panic() {
    let Some(ff) = filmcraft_testkit::ffmpeg_or_skip("hevc fuzz") else { return };
    for &(name, args, pix) in SEEDS {
        let Some(seed) = make(&ff, name, args, pix) else { continue };
        let stride = (seed.len() / 300).max(1);
        for n in (0..seed.len()).step_by(stride) {
            run_all(&seed[..n], &format!("{name} truncate {n}"));
        }
        // SPS/PPS/first slice header region: every byte to a few values, every bit
        for p in 0..seed.len().min(120) {
            for v in [0u8, 0xff, 0x80, 0x01, 0x7f] {
                let mut d = seed.clone();
                d[p] = v;
                run_all(&d, &format!("{name} byte {p}={v:#x}"));
            }
            for b in 0..8 {
                let mut d = seed.clone();
                d[p] ^= 1 << b;
                run_all(&d, &format!("{name} flip {p}.{b}"));
            }
        }
        let mut rng = Rng(0x9e3779b97f4a7c15 ^ name.len() as u64);
        for i in 0..iters() {
            let mut d = seed.clone();
            for _ in 0..1 + rng.below(6) {
                let p = rng.below(d.len());
                if rng.below(2) == 0 {
                    d[p] ^= 1 << rng.below(8);
                } else {
                    d[p] = rng.next() as u8;
                }
            }
            run_all(&d, &format!("{name} random {i}"));
        }
    }
}

/// Random-mutation rounds per seed (raise with FILMCRAFT_FUZZ_ITERS for a long hunt).
fn iters() -> usize {
    std::env::var("FILMCRAFT_FUZZ_ITERS").ok().and_then(|s| s.parse().ok()).unwrap_or(400)
}
