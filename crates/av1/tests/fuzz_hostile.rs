//! Hostile-input robustness: truncation, header-region sweeps and random mutation of small
//! ffmpeg-generated AV1 streams (frame by frame) must never panic or run away. Skips without ffmpeg.

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};

use filmcraft_av1::Decoder;

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

struct Seed {
    name: &'static str,
    encoder: &'static str,
    /// Encoder arguments after `-c:v <encoder>`.
    args: &'static str,
    lavfi: &'static str,
    frames: u32,
    pix: &'static str,
}

const SMALL: &str = "testsrc2=size=64x48:rate=25";
const AOM: &str = "-cpu-used 8 -g 3";

const SEEDS: &[Seed] = &[
    Seed { name: "av1_fz_default", encoder: "libsvtav1", args: "-g 4", lavfi: SMALL, frames: 6, pix: "yuv420p" },
    Seed { name: "av1_fz_10bit", encoder: "libsvtav1", args: "-g 6", lavfi: SMALL, frames: 6, pix: "yuv420p10le" },
    Seed { name: "av1_fz_444", encoder: "libaom-av1", args: AOM, lavfi: SMALL, frames: 3, pix: "yuv444p" },
    Seed { name: "av1_fz_422_10bit", encoder: "libaom-av1", args: AOM, lavfi: SMALL, frames: 3, pix: "yuv422p10le" },
    Seed { name: "av1_fz_12bit", encoder: "libaom-av1", args: AOM, lavfi: SMALL, frames: 3, pix: "yuv420p12le" },
    Seed { name: "av1_fz_mono", encoder: "libaom-av1", args: AOM, lavfi: SMALL, frames: 3, pix: "gray" },
    Seed { name: "av1_fz_screen", encoder: "libaom-av1", args: "-cpu-used 8 -g 3 -aom-params tune-content=screen", lavfi: SMALL, frames: 3, pix: "yuv420p" },
    Seed {
        name: "av1_fz_tiles",
        encoder: "libaom-av1",
        args: "-cpu-used 8 -g 3 -tile-columns 2 -tile-rows 1 -aom-params sb-size=64",
        lavfi: "testsrc2=size=256x128:rate=25",
        frames: 3,
        pix: "yuv420p",
    },
    Seed {
        name: "av1_fz_superres",
        encoder: "libsvtav1",
        args: "-g 3 -svtav1-params superres-mode=1:superres-denom=12:superres-kf-denom=13",
        lavfi: "testsrc2=size=128x64:rate=25",
        frames: 3,
        pix: "yuv420p",
    },
    Seed {
        name: "av1_fz_grain",
        encoder: "libsvtav1",
        args: "-g 3 -svtav1-params film-grain=10",
        lavfi: "testsrc2=size=64x48:rate=25,noise=alls=20:allf=t",
        frames: 3,
        pix: "yuv420p",
    },
];

fn ivf_frames(data: &[u8]) -> Vec<Vec<u8>> {
    let mut out = Vec::new();
    let Some(h) = data.get(6..8) else { return out };
    let mut pos = usize::from(u16::from_le_bytes([h[0], h[1]]));
    while let Some(hd) = data.get(pos..pos + 12) {
        let size = u32::from_le_bytes([hd[0], hd[1], hd[2], hd[3]]) as usize;
        pos += 12;
        let end = pos.saturating_add(size).min(data.len());
        out.push(data[pos..end].to_vec());
        pos = end;
    }
    out
}

fn make(ff: &std::path::Path, seed: &Seed) -> Option<Vec<Vec<u8>>> {
    let out = filmcraft_testkit::fixtures_dir("av1").join(format!("{}.ivf", seed.name));
    let p = filmcraft_testkit::fixtures::generate(&out, |tmp| {
        let frames = seed.frames.to_string();
        matches!(
            Command::new(ff)
                .args(["-hide_banner", "-loglevel", "error", "-y", "-f", "lavfi", "-i", seed.lavfi, "-frames:v", &frames, "-pix_fmt", seed.pix, "-c:v", seed.encoder])
                .args(seed.args.split_whitespace())
                .args(["-f", "ivf"])
                .arg(tmp)
                .stdin(Stdio::null())
                .status(),
            Ok(s) if s.success()
        )
    })?;
    Some(ivf_frames(&std::fs::read(p).ok()?))
}

static PANICS: AtomicUsize = AtomicUsize::new(0);

/// Frame workers catch panics and turn them into decoding errors; count them so they still fail the test.
fn count_panics() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        std::panic::set_hook(Box::new(|info| {
            PANICS.fetch_add(1, Ordering::SeqCst);
            eprintln!("PANIC: {info}");
        }));
    });
}

fn run_all(frames: &[Vec<u8>], label: &str) {
    filmcraft_testkit::set_progress(label);
    count_panics();
    let before = PANICS.load(Ordering::SeqCst);
    let res = catch_unwind(AssertUnwindSafe(|| {
        for threads in [1, 3] {
            let mut dec = Decoder::with_threads(threads);
            for (i, f) in frames.iter().enumerate() {
                let _ = dec.decode_pts(f, i as i64);
            }
            let _ = dec.flush_result();
        }
    }));
    assert!(res.is_ok() && PANICS.load(Ordering::SeqCst) == before, "panic: {label}");
}

/// The unmutated seed decodes without error, so the sweep starts from a stream that reaches every stage.
fn decodes_cleanly(frames: &[Vec<u8>], name: &str) {
    for threads in [1, 3] {
        let mut dec = Decoder::with_threads(threads);
        let mut shown = 0;
        for (i, f) in frames.iter().enumerate() {
            shown += dec.decode_pts(f, i as i64).unwrap_or_else(|e| panic!("{name} frame {i} ({threads} threads): {e}")).len();
        }
        shown += dec.flush_result().unwrap_or_else(|e| panic!("{name} flush ({threads} threads): {e}")).len();
        assert!(shown > 0, "{name} shows no frame");
    }
}

/// Random-mutation rounds per seed (raise with FILMCRAFT_FUZZ_ITERS for a long hunt).
fn iters() -> usize {
    std::env::var("FILMCRAFT_FUZZ_ITERS").ok().and_then(|s| s.parse().ok()).unwrap_or(300)
}

/// Seconds the whole sweep may take before it counts as a hang (override: FILMCRAFT_HOSTILE_TIMEOUT_SECS).
const DEADLINE_SECS: u64 = 600;

#[test]
fn hostile_streams_never_panic() {
    filmcraft_testkit::with_deadline(DEADLINE_SECS, sweep);
}

fn sweep() {
    let Some(ff) = filmcraft_testkit::ffmpeg_or_skip("AV1 fuzz") else { return };
    for s in SEEDS {
        let name = s.name;
        if !filmcraft_testkit::ffmpeg_has_encoder(&ff, s.encoder) {
            filmcraft_testkit::skip("AV1 fuzz", &format!("this ffmpeg has no {} encoder (seed {name})", s.encoder));
            continue;
        }
        let Some(seed) = make(&ff, s) else {
            panic!("ffmpeg has {} but could not encode the {name} seed", s.encoder);
        };
        if seed.is_empty() {
            filmcraft_testkit::skip("AV1 fuzz", &format!("the {name} seed has no frames"));
            continue;
        }
        decodes_cleanly(&seed, name);
        // truncate each of the first frames at every length
        for fi in 0..seed.len().min(3) {
            let len = seed[fi].len();
            for n in (0..len).step_by((len / 200).max(1)) {
                let mut d = seed.clone();
                d[fi].truncate(n);
                run_all(&d, &format!("{name} frame {fi} truncate {n}"));
            }
        }
        // header region of every frame: every byte to a few values, every bit
        for fi in 0..seed.len() {
            for p in 0..seed[fi].len().min(96) {
                for v in [0u8, 0xff, 0x80, 0x01, 0x7f] {
                    let mut d = seed.clone();
                    d[fi][p] = v;
                    run_all(&d, &format!("{name} frame {fi} byte {p}={v:#x}"));
                }
                for b in 0..8 {
                    let mut d = seed.clone();
                    d[fi][p] ^= 1 << b;
                    run_all(&d, &format!("{name} frame {fi} flip {p}.{b}"));
                }
            }
        }
        let mut rng = Rng(0x9e3779b97f4a7c15 ^ name.len() as u64);
        for i in 0..iters() {
            let mut d = seed.clone();
            for _ in 0..1 + rng.below(6) {
                let fi = rng.below(d.len());
                if d[fi].is_empty() {
                    continue;
                }
                let p = rng.below(d[fi].len());
                if rng.below(2) == 0 {
                    d[fi][p] ^= 1 << rng.below(8);
                } else {
                    d[fi][p] = rng.next() as u8;
                }
            }
            run_all(&d, &format!("{name} random {i}"));
        }
    }
}
