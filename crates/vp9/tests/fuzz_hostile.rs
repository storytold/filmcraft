//! Hostile-input robustness: truncation, header-region sweeps and random mutation of small
//! ffmpeg-generated VP9 streams (frame by frame) must never panic or run away. Skips without ffmpeg.

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};

use filmcraft_vp9::Decoder;

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

/// (name, encoder args, pix_fmt)
const SEEDS: &[(&str, &str, &str)] = &[
    ("vp9_fz_default", "-c:v libvpx-vp9 -g 4 -b:v 300k -lag-in-frames 0", "yuv420p"),
    // libvpx needs 256 px per tile column, so two columns need 512 px
    ("vp9_fz_2tiles_10bit", "-c:v libvpx-vp9 -g 6 -b:v 300k -lag-in-frames 0 -tile-columns 1 -profile:v 2 -s 512x48", "yuv420p10le"),
    ("vp9_fz_444", "-c:v libvpx-vp9 -g 6 -b:v 300k -lag-in-frames 0 -profile:v 1", "yuv444p"),
    ("vp9_fz_altref", "-c:v libvpx-vp9 -g 6 -b:v 300k -auto-alt-ref 1 -lag-in-frames 5", "yuv420p"),
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

fn make(ff: &std::path::Path, name: &str, args: &str, pix: &str) -> Option<Vec<Vec<u8>>> {
    let out = filmcraft_testkit::fixtures_dir("vp9").join(format!("{name}.ivf"));
    let p = filmcraft_testkit::fixtures::generate(&out, |tmp| {
        let a: Vec<&str> = args.split_whitespace().collect();
        matches!(
            Command::new(ff)
                .args(["-hide_banner", "-loglevel", "error", "-y", "-f", "lavfi", "-i", "testsrc2=size=64x48:rate=25", "-frames:v", "6", "-pix_fmt", pix])
                .args(a)
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

/// Decoder worker threads may catch panics and turn them into errors; count them so they still fail the test.
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
                let _ = dec.decode(f, i as i64);
            }
            let _ = dec.flush();
        }
    }));
    assert!(res.is_ok() && PANICS.load(Ordering::SeqCst) == before, "panic: {label}");
}

const VALUES: [u8; 5] = [0, 0xff, 0x80, 0x01, 0x7f];

/// How much of the sweep runs. `FILMCRAFT_FUZZ_ITERS=N` runs all of it (about 200 truncation
/// points spread over each of the first three frames, every header byte, overwrite value and
/// bit flip) plus N random rounds per seed: 300 is the full depth, more hunts longer. Unset, CI
/// runs a third of the truncation points and of the full depth's random rounds and, for each
/// header byte of each frame, one value and one bit, shifted by the frame index. A rotation
/// offset shifts which share runs (`FILMCRAFT_FUZZ_SEED`, else the UTC day number), so every
/// case of the full sweep runs within a few days, and a random round's mutations match the same
/// round of a full run.
struct Depth {
    full: bool,
    truncations: usize,
    rounds: usize,
    rotation: usize,
}

impl Depth {
    fn from_env() -> Self {
        match std::env::var("FILMCRAFT_FUZZ_ITERS").ok().and_then(|s| s.parse().ok()) {
            Some(rounds) => Depth { full: true, truncations: 200, rounds, rotation: 0 },
            None => {
                let rotation = rotation();
                eprintln!("fuzz subset rotation {rotation} (FILMCRAFT_FUZZ_SEED={rotation} replays it; FILMCRAFT_FUZZ_ITERS=300 runs everything)");
                Depth { full: false, truncations: 200, rounds: 300, rotation }
            }
        }
    }
    /// Truncation lengths for a `len`-byte input: every point of the full sweep, or every third one.
    fn truncations(&self, len: usize) -> std::iter::StepBy<std::ops::Range<usize>> {
        let stride = (len / self.truncations).max(1);
        let (start, step) = if self.full { (0, stride) } else { ((self.rotation % 3) * stride, 3 * stride) };
        (start..len).step_by(step)
    }
    fn value(&self, fi: usize, p: usize, i: usize) -> bool {
        self.full || i == (p + fi + self.rotation % VALUES.len()) % VALUES.len()
    }
    fn bit(&self, fi: usize, p: usize, b: usize) -> bool {
        self.full || b == (p + fi + self.rotation % 8) % 8
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

/// Seconds the whole sweep may take before it counts as a hang (override: FILMCRAFT_HOSTILE_TIMEOUT_SECS).
const DEADLINE_SECS: u64 = 600;

/// CI runs a rotating share of every part of the sweep; `FILMCRAFT_FUZZ_ITERS=300` runs all of it (see [`Depth`]).
#[test]
fn hostile_streams_never_panic() {
    filmcraft_testkit::with_deadline(DEADLINE_SECS, sweep);
}

fn sweep() {
    let Some(ff) = filmcraft_testkit::ffmpeg_or_skip("VP9 fuzz") else { return };
    let depth = Depth::from_env();
    for &(name, args, pix) in SEEDS {
        if !filmcraft_testkit::ffmpeg_has_encoder(&ff, "libvpx-vp9") {
            filmcraft_testkit::skip("VP9 fuzz", &format!("this ffmpeg has no libvpx-vp9 encoder (seed {name})"));
            continue;
        }
        let Some(seed) = make(&ff, name, args, pix) else {
            panic!("ffmpeg has libvpx-vp9 but could not encode the {name} seed");
        };
        if seed.is_empty() {
            filmcraft_testkit::skip("VP9 fuzz", &format!("the {name} seed has no frames"));
            continue;
        }
        // truncate each of the first frames at every length
        for fi in 0..seed.len().min(3) {
            let len = seed[fi].len();
            for n in depth.truncations(len) {
                let mut d = seed.clone();
                d[fi].truncate(n);
                run_all(&d, &format!("{name} frame {fi} truncate {n}"));
            }
        }
        // header region of every frame: every byte to a few values, every bit
        for fi in 0..seed.len() {
            for p in 0..seed[fi].len().min(96) {
                for (i, v) in VALUES.into_iter().enumerate() {
                    if !depth.value(fi, p, i) {
                        continue;
                    }
                    let mut d = seed.clone();
                    d[fi][p] = v;
                    run_all(&d, &format!("{name} frame {fi} byte {p}={v:#x}"));
                }
                for b in 0..8 {
                    if !depth.bit(fi, p, b) {
                        continue;
                    }
                    let mut d = seed.clone();
                    d[fi][p] ^= 1 << b;
                    run_all(&d, &format!("{name} frame {fi} flip {p}.{b}"));
                }
            }
        }
        let mut rng = Rng(0x9e3779b97f4a7c15 ^ name.len() as u64);
        // Skipped rounds still draw their mutations, so round i matches round i of a full run.
        for i in 0..depth.rounds {
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
            if depth.round(i) {
                run_all(&d, &format!("{name} random {i}"));
            }
        }
    }
}
