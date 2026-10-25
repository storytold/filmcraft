//! Hostile-input robustness: every header byte set to extreme values, every bit flipped,
//! truncation at (nearly) every length must never panic or run away (errors are fine).

use std::panic::{AssertUnwindSafe, catch_unwind};

use filmcraft_prores::*;

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
    for (p, w, h) in [(Profile::Proxy, 96u32, 48u32), (Profile::Hq, 70, 38), (Profile::P4444, 64, 32), (Profile::P4444Xq, 48, 20)] {
        let chroma = p.chroma();
        let mut f = Frame::new(w, h, chroma, 10, chroma == ChromaFormat::Yuv444);
        for (i, s) in f.y.iter_mut().enumerate() {
            *s = (64 + (i * 37) % 800) as u16;
        }
        if let Some(a) = f.alpha.as_mut() {
            for (i, s) in a.iter_mut().enumerate() {
                *s = if i % 5 == 0 { 1023 } else { (i % 1024) as u16 };
            }
        }
        let mut cfg = EncoderConfig::new(p, w, h);
        cfg.log2_slice_mbs = (w % 4) as u8;
        if let Ok(d) = Encoder::with_config(cfg).encode(&f) {
            v.push(d);
        }
    }
    v
}

/// Decodes `d` at every output depth, probes and checks it, failing the test if any of them panics.
fn run(d: &[u8], label: &str) {
    filmcraft_testkit::set_progress(label);
    let res = catch_unwind(AssertUnwindSafe(|| {
        for depth in [None, Some(8), Some(16)] {
            let _ = decode_frame_with(d, &DecodeOptions { bit_depth: depth, threads: depth.is_none() });
        }
        let _ = probe(d);
        let _ = check_frame(d);
    }));
    assert!(res.is_ok(), "panic: {label}");
}

/// Seconds the whole sweep may take before it counts as a hang (override: FILMCRAFT_HOSTILE_TIMEOUT_SECS).
const DEADLINE_SECS: u64 = 600;

#[test]
fn hostile_frames_never_panic() {
    filmcraft_testkit::with_deadline(DEADLINE_SECS, sweep);
}

/// `check_frame` accepts every seed frame, with the header `probe` reads, and refuses it one byte
/// short, one byte longer, with a frame size past the sample, or with bytes no slice covers inside
/// its picture or after it (sizes grown to match), which still decodes to the same frame.
#[test]
fn check_frame_takes_whole_frames_only() {
    for (si, seed) in seeds().iter().enumerate() {
        let h = check_frame(seed).unwrap_or_else(|e| panic!("seed {si}: {e}"));
        assert_eq!(h, probe(seed).unwrap());
        assert!(check_frame(&seed[..seed.len() - 1]).is_err(), "seed {si}: one byte short");
        let mut longer = seed.clone();
        longer.push(0);
        assert!(check_frame(&longer).is_err(), "seed {si}: one byte appended");
        // the frame size in the frame header grows past the sample
        let mut lying = seed.clone();
        lying[0..4].copy_from_slice(&(seed.len() as u32 + 1).to_be_bytes());
        assert!(check_frame(&lying).is_err(), "seed {si}: frame size past the sample");
        // padding after the picture's slices (picture and frame size grown), or after the picture
        // (frame size grown): consistent sizes, still decodable, but not what the frame codes
        let off = 8 + usize::from(u16::from_be_bytes([seed[8], seed[9]]));
        let pic = u32::from_be_bytes([seed[off + 1], seed[off + 2], seed[off + 3], seed[off + 4]]) as usize;
        let mut inside = seed.clone();
        inside.splice(off + pic..off + pic, [0u8; 1000]);
        inside[off + 1..off + 5].copy_from_slice(&((pic + 1000) as u32).to_be_bytes());
        let mut after = seed.clone();
        after.extend([0u8; 1000]);
        for (name, mut padded) in [("inside the picture", inside), ("after the picture", after)] {
            let size = padded.len() as u32;
            padded[0..4].copy_from_slice(&size.to_be_bytes());
            assert!(check_frame(&padded).is_err(), "seed {si}: padding {name}");
            assert_eq!(decode_frame(&padded).unwrap(), decode_frame(seed).unwrap(), "seed {si}: padding {name} decodes differently");
        }
    }
}

fn sweep() {
    let seeds = seeds();
    assert!(!seeds.is_empty());
    for (si, seed) in seeds.iter().enumerate() {
        let stride = (seed.len() / 400).max(1);
        for n in (0..seed.len()).step_by(stride) {
            run(&seed[..n], &format!("seed {si} truncate {n}"));
        }
        for p in 0..seed.len().min(256) {
            for v in [0u8, 0xff, 0x80, 0x01, 0x7f] {
                let mut d = seed.clone();
                d[p] = v;
                run(&d, &format!("seed {si} byte {p}={v:#x}"));
            }
            for b in 0..8 {
                let mut d = seed.clone();
                d[p] ^= 1 << b;
                run(&d, &format!("seed {si} flip {p}.{b}"));
            }
        }
        // Whole big-endian 32-bit words of the header set to 0 / huge values (sizes, counts, dimensions).
        for p in 0..seed.len().min(128) {
            for v in [0u32, 0xffff_ffff, 0x0001_0000, 0x7fff_ffff] {
                let mut d = seed.clone();
                if let Some(s) = d.get_mut(p..p + 4) {
                    s.copy_from_slice(&v.to_be_bytes());
                }
                run(&d, &format!("seed {si} u32 {p}={v:#x}"));
            }
        }
        let mut rng = Rng(0x9e3779b97f4a7c15 ^ si as u64);
        let iters = std::env::var("FILMCRAFT_FUZZ_ITERS").ok().and_then(|s| s.parse().ok()).unwrap_or(1000);
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
