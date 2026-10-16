//! Hostile-input robustness: every header byte set to extreme values, every bit flipped,
//! truncation at (nearly) every length and random mutation of encoder-made frames must never
//! panic or run away (errors are fine). No ffmpeg needed.

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::time::{Duration, Instant};

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
    let t = Instant::now();
    let res = catch_unwind(AssertUnwindSafe(|| {
        let _ = probe(d);
        for opts in [DecodeOptions::default(), DecodeOptions { threads: true, spec_dequant: true }] {
            let _ = decode_frame_with(d, &opts);
        }
    }));
    assert!(res.is_ok(), "panic: {label}");
    assert!(t.elapsed() < Duration::from_secs(10), "too slow ({:?}): {label}", t.elapsed());
}

#[test]
fn hostile_frames_never_panic() {
    let seeds = seeds();
    assert!(!seeds.is_empty());
    for (si, seed) in seeds.iter().enumerate() {
        let stride = (seed.len() / 400).max(1);
        for n in (0..seed.len()).step_by(stride) {
            run(&seed[..n], &format!("seed {si} truncate {n}"));
        }
        // The header (and the start of the scan-line index): every byte to extreme values, every bit.
        for p in 0..seed.len().min(0x300) {
            for v in [0u8, 0xff, 0x80, 0x01, 0x7f] {
                let mut d = seed.clone();
                d[p] = v;
                run(&d, &format!("seed {si} byte {p:#x}={v:#x}"));
            }
            for b in 0..8 {
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
        let iters = std::env::var("FILMCRAFT_FUZZ_ITERS").ok().and_then(|s| s.parse().ok()).unwrap_or(1500);
        for i in 0..iters {
            let mut d = seed.clone();
            for _ in 0..1 + rng.below(8) {
                let p = rng.below(d.len());
                if rng.below(2) == 0 {
                    d[p] ^= 1 << rng.below(8);
                } else {
                    d[p] = rng.next() as u8;
                }
            }
            run(&d, &format!("seed {si} random {i}"));
        }
    }
}
