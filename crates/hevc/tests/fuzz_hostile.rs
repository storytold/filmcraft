//! Hostile-input robustness: header-region sweeps (SPS/PPS/first slice headers), truncation and
//! random mutation of small libx265 streams must never panic or run away. Skips without ffmpeg.

#[path = "it/common/mod.rs"]
mod common;

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::process::{Command, Stdio};

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

/// (name, encoder, extra encoder args, pix_fmt)
const SEEDS: &[(&str, &str, &str, &str)] = &[
    ("hevc_fz_main_b", "libx265", "-x265-params log-level=error:bframes=2:keyint=8:min-keyint=8:sao=1:amp=1", "yuv420p"),
    ("hevc_fz_main10", "libx265", "-x265-params log-level=error:bframes=1:keyint=6:min-keyint=6", "yuv420p10le"),
    ("hevc_fz_wpp_slices", "libx265", "-x265-params log-level=error:keyint=6:min-keyint=6:wpp=1:slices=3", "yuv420p"),
    // x265 cannot encode tiles; kvazaar can, at 256 x 64 luma per tile at least (Main profile)
    ("hevc_fz_tiles_kvazaar", "libkvazaar", "-s 512x128 -kvazaar-params tiles=2x2,gop=0,period=6,qp=40", "yuv420p"),
    ("hevc_fz_422_10", "libx265", "-x265-params log-level=error:keyint=6:min-keyint=6", "yuv422p10le"),
];

fn make(ff: &std::path::Path, name: &str, enc: &str, args: &str, pix: &str) -> Option<Vec<u8>> {
    let out = filmcraft_testkit::fixtures_dir("hevc").join(format!("{name}.hevc"));
    let p = filmcraft_testkit::fixtures::generate(&out, |tmp| {
        let a: Vec<&str> = args.split_whitespace().collect();
        matches!(
            Command::new(ff)
                .args(["-hide_banner", "-loglevel", "error", "-y", "-f", "lavfi", "-i", "testsrc2=size=64x48:rate=25", "-frames:v", "7", "-pix_fmt", pix, "-c:v", enc])
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
    filmcraft_testkit::set_progress(label);
    count_panics();
    let before = PANICS.load(std::sync::atomic::Ordering::SeqCst);
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
}

/// Seconds the whole sweep may take before it counts as a hang (override: FILMCRAFT_HOSTILE_TIMEOUT_SECS).
const DEADLINE_SECS: u64 = 600;

/// CI runs a rotating share of every part of the sweep; `FILMCRAFT_FUZZ_ITERS=400` runs all of it (see [`Depth`]).
#[test]
fn hostile_streams_never_panic() {
    filmcraft_testkit::with_deadline(DEADLINE_SECS, sweep);
}

fn sweep() {
    let Some(ff) = filmcraft_testkit::ffmpeg_or_skip("hevc fuzz") else { return };
    let depth = Depth::from_env();
    for &(name, enc, args, pix) in SEEDS {
        if !filmcraft_testkit::ffmpeg_has_encoder(&ff, enc) {
            filmcraft_testkit::skip("hevc fuzz", &format!("this ffmpeg has no {enc} encoder (seed {name})"));
            continue;
        }
        let Some(seed) = make(&ff, name, enc, args, pix) else {
            panic!("ffmpeg has {enc} but could not encode the {name} seed");
        };
        for n in depth.truncations(seed.len()) {
            run_all(&seed[..n], &format!("{name} truncate {n}"));
        }
        // SPS/PPS/first slice header region: every byte to a few values, every bit
        for p in 0..seed.len().min(120) {
            for (i, v) in VALUES.into_iter().enumerate() {
                if !depth.value(p, i) {
                    continue;
                }
                let mut d = seed.clone();
                d[p] = v;
                run_all(&d, &format!("{name} byte {p}={v:#x}"));
            }
            for b in 0..8 {
                if !depth.bit(p, b) {
                    continue;
                }
                let mut d = seed.clone();
                d[p] ^= 1 << b;
                run_all(&d, &format!("{name} flip {p}.{b}"));
            }
        }
        let mut rng = Rng(0x9e3779b97f4a7c15 ^ name.len() as u64);
        // Skipped rounds still draw their mutations, so round i matches round i of a full run.
        for i in 0..depth.rounds {
            let mut d = seed.clone();
            for _ in 0..1 + rng.below(6) {
                let p = rng.below(d.len());
                if rng.below(2) == 0 {
                    d[p] ^= 1 << rng.below(8);
                } else {
                    d[p] = rng.next() as u8;
                }
            }
            if depth.round(i) {
                run_all(&d, &format!("{name} random {i}"));
            }
        }
    }
}

const VALUES: [u8; 5] = [0, 0xff, 0x80, 0x01, 0x7f];

/// How much of the sweep runs. `FILMCRAFT_FUZZ_ITERS=N` runs all of it (about 300 truncation
/// points spread over each seed, every header byte, overwrite value and bit flip) plus N random
/// rounds per seed: 400 is the full depth, more hunts longer. Unset, CI runs a third of the
/// truncation points and of the full depth's random rounds and, for each header byte, one value
/// and two bits. A rotation offset shifts which share runs (`FILMCRAFT_FUZZ_SEED`, else the UTC
/// day number), so every case of the full sweep runs within a few days, and a random round's
/// mutations match the same round of a full run.
struct Depth {
    full: bool,
    truncations: usize,
    rounds: usize,
    rotation: usize,
}

impl Depth {
    fn from_env() -> Self {
        match std::env::var("FILMCRAFT_FUZZ_ITERS").ok().and_then(|s| s.parse().ok()) {
            Some(rounds) => Depth { full: true, truncations: 300, rounds, rotation: 0 },
            None => {
                let rotation = rotation();
                eprintln!("fuzz subset rotation {rotation} (FILMCRAFT_FUZZ_SEED={rotation} replays it; FILMCRAFT_FUZZ_ITERS=400 runs everything)");
                Depth { full: false, truncations: 300, rounds: 400, rotation }
            }
        }
    }
    /// Truncation lengths for a `len`-byte input: every point of the full sweep, or every third one.
    fn truncations(&self, len: usize) -> std::iter::StepBy<std::ops::Range<usize>> {
        let stride = (len / self.truncations).max(1);
        let (start, step) = if self.full { (0, stride) } else { ((self.rotation % 3) * stride, 3 * stride) };
        (start..len).step_by(step)
    }
    fn value(&self, p: usize, i: usize) -> bool {
        self.full || i == (p + self.rotation % VALUES.len()) % VALUES.len()
    }
    fn bit(&self, p: usize, b: usize) -> bool {
        self.full || (b + p + self.rotation % 4).is_multiple_of(4)
    }
    fn round(&self, i: usize) -> bool {
        self.full || i % 3 == self.rotation % 3
    }
}

/// The default subset's rotation offset: `FILMCRAFT_FUZZ_SEED`, else days since the Unix epoch (UTC).
fn rotation() -> usize {
    std::env::var("FILMCRAFT_FUZZ_SEED").ok().and_then(|s| s.parse().ok()).unwrap_or_else(|| {
        let secs = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs());
        usize::try_from(secs / 86_400).unwrap_or(0)
    })
}
