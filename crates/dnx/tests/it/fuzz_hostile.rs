//! Hostile-input robustness: every header byte set to extreme values, every bit flipped,
//! truncation at (nearly) every length and random mutation of encoder-made frames must never
//! panic or run away (errors are fine). No ffmpeg needed.

use std::panic::{AssertUnwindSafe, catch_unwind};

use filmcraft_dnx::{ChromaFormat, DecodeOptions, Encoder, EncoderConfig, Frame, Profile, decode_frame_with, probe};

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

fn seeds() -> Vec<Vec<u8>> {
    let mut v = Vec::new();
    for (profile, depth, w, h) in
        [(Profile::Lb, 8, 64u32, 48u32), (Profile::Hq, 8, 48, 32), (Profile::Hqx, 10, 64, 32), (Profile::Hqx, 12, 48, 32), (Profile::R444, 10, 48, 32)]
    {
        let mut cfg = EncoderConfig::new(profile, w, h);
        cfg.bit_depth = depth;
        let chroma = cfg.chroma();
        let mut f = Frame::new(w, h, chroma, depth, false);
        for (i, s) in f.y.iter_mut().enumerate() {
            *s = ((16 + (i * 37) % 200) as u16) << (depth - 8);
        }
        if chroma != ChromaFormat::Yuv444 {
            f.cb.fill(128 << (depth - 8));
            f.cr.fill(100 << (depth - 8));
        }
        if let Ok(mut enc) = Encoder::with_config(cfg)
            && let Ok(d) = enc.encode(&f)
        {
            v.push(d);
        }
    }
    v
}

fn run(d: &[u8], label: &str) {
    filmcraft_testkit::set_progress(label);
    let res = catch_unwind(AssertUnwindSafe(|| {
        let _ = probe(d);
        for opts in [DecodeOptions::default(), DecodeOptions { threads: true, spec_dequant: true }] {
            let _ = decode_frame_with(d, &opts);
        }
    }));
    assert!(res.is_ok(), "panic: {label}");
}

/// Seconds the whole sweep may take before it counts as a hang (override: FILMCRAFT_HOSTILE_TIMEOUT_SECS).
const DEADLINE_SECS: u64 = 600;

/// CI runs a rotating share of every part of the sweep; `FILMCRAFT_FUZZ_ITERS=1500` runs all of it (see [`Depth`]).
#[test]
fn hostile_frames_never_panic() {
    filmcraft_testkit::with_deadline(DEADLINE_SECS, sweep);
}

fn sweep() {
    let depth = Depth::from_env();
    let seeds = seeds();
    assert!(!seeds.is_empty());
    for (si, seed) in seeds.iter().enumerate() {
        for n in depth.truncations(seed.len()) {
            run(&seed[..n], &format!("seed {si} truncate {n}"));
        }
        // The header (and the start of the scan-line index): every byte to extreme values, every bit.
        for p in 0..seed.len().min(0x300) {
            for (i, v) in VALUES.into_iter().enumerate() {
                if !depth.value(p, i) {
                    continue;
                }
                let mut d = seed.clone();
                d[p] = v;
                run(&d, &format!("seed {si} byte {p:#x}={v:#x}"));
            }
            for b in 0..8 {
                if !depth.bit(p, b) {
                    continue;
                }
                let mut d = seed.clone();
                d[p] ^= 1 << b;
                run(&d, &format!("seed {si} flip {p:#x}.{b}"));
            }
        }
        // Big dimension fields with the rest intact (lines 0x18, width 0x1A, macroblock rows 0x16C).
        for (off, v) in [(0x18usize, 0xffffu16), (0x1a, 0xffff), (0x18, 0x4000), (0x1a, 0x4000), (0x16c, 0xffff)] {
            let mut d = seed.clone();
            if let Some(s) = d.get_mut(off..off + 2) {
                s.copy_from_slice(&v.to_be_bytes());
            }
            run(&d, &format!("seed {si} u16 {off:#x}={v:#x}"));
        }
        let mut rng = Rng(0x9e3779b97f4a7c15 ^ si as u64);
        // Skipped rounds still draw their mutations, so round i matches round i of a full run.
        for i in 0..depth.rounds {
            let mut d = seed.clone();
            for _ in 0..1 + rng.below(8) {
                let p = rng.below(d.len());
                if rng.below(2) == 0 {
                    d[p] ^= 1 << rng.below(8);
                } else {
                    d[p] = rng.next() as u8;
                }
            }
            if depth.round(i) {
                run(&d, &format!("seed {si} random {i}"));
            }
        }
    }
}

const VALUES: [u8; 5] = [0, 0xff, 0x80, 0x01, 0x7f];

/// How much of the sweep runs. `FILMCRAFT_FUZZ_ITERS=N` runs all of it (about 400 truncation
/// points spread over each seed, every header byte, overwrite value and bit flip) plus N random
/// rounds per seed: 1500 is the full depth, more hunts longer. Unset, CI runs a third of the
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
            Some(rounds) => Depth { full: true, truncations: 400, rounds, rotation: 0 },
            None => {
                let rotation = rotation();
                eprintln!("fuzz subset rotation {rotation} (FILMCRAFT_FUZZ_SEED={rotation} replays it; FILMCRAFT_FUZZ_ITERS=1500 runs everything)");
                Depth { full: false, truncations: 400, rounds: 1500, rotation }
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
