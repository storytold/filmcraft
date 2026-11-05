//! Robustness: mutated, truncated, spliced, and random bitstreams must never panic (errors are fine).

use std::panic::{AssertUnwindSafe, catch_unwind};

use filmcraft_apv::*;

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
    for (p, w, h) in
        [(Profile::P422_10, 64u32, 48u32), (Profile::P422_12, 70, 38), (Profile::P444_10, 48, 32), (Profile::P4444_10, 64, 32), (Profile::P400_10, 32, 32)]
    {
        let chroma = p.default_chroma();
        let depth = p.default_bit_depth();
        let mut f = Frame::new(w, h, chroma, depth, chroma == ChromaFormat::Yuv4444);
        for (i, s) in f.y.iter_mut().enumerate() {
            *s = (64 + (i * 37) % 800) as u16;
        }
        if let Some(a) = f.alpha.as_mut() {
            for (i, s) in a.iter_mut().enumerate() {
                *s = ((i * 19) % 1024) as u16;
            }
        }
        let mut cfg = EncoderConfig::new(p, w, h);
        cfg.qp = Some(16);
        let mut enc = Encoder::with_config(cfg).expect("seed encoder");
        v.push(enc.encode(&f).expect("seed encode"));
        v.push(enc.encode_raw_au(&f).expect("seed encode_raw_au"));
    }
    v
}

#[test]
fn mutated_streams_never_panic() {
    let seeds = seeds();
    for s in &seeds {
        decode_frame(s).expect("seed decodes");
    }
    let mut rng = Rng(0x9e3779b97f4a7c15);
    let mut decoded_ok = 0usize;
    let iters = 3000usize;

    for i in 0..iters {
        let mut d = seeds[i % seeds.len()].clone();
        match rng.below(6) {
            0 => {
                // Bit flips
                for _ in 0..1 + rng.below(8) {
                    let p = rng.below(d.len());
                    d[p] ^= 1 << rng.below(8);
                }
            }
            1 => {
                // Random byte overwrites
                for _ in 0..1 + rng.below(4) {
                    let p = rng.below(d.len());
                    d[p] = rng.next() as u8;
                }
            }
            2 => {
                // Truncation
                let n = rng.below(d.len());
                d.truncate(n);
            }
            3 => {
                // Corrupt header fields
                if d.len() > 8 {
                    let p = 4 + rng.below(24.min(d.len() - 4));
                    d[p] = rng.next() as u8;
                }
            }
            4 => {
                // Corrupt tile payload region
                let start = 24.min(d.len().saturating_sub(1));
                let off = start + rng.below(d.len().saturating_sub(start).max(1));
                for b in d.iter_mut().skip(off).take(1 + rng.below(48)) {
                    *b = rng.next() as u8;
                }
            }
            _ => {
                // Splice random bytes
                let p = rng.below(d.len());
                let extra: Vec<u8> = (0..1 + rng.below(16)).map(|_| rng.next() as u8).collect();
                d.splice(p..p, extra);
            }
        }

        let res = catch_unwind(AssertUnwindSafe(|| {
            for depth in [None, Some(8), Some(16)] {
                let opts = DecodeOptions { bit_depth: depth, threads: i % 2 == 0 };
                if let Ok(f) = decode_frame_with(&d, &opts) {
                    decoded_ok += 1;
                    assert_eq!(f.y.len(), (f.width as usize) * (f.height as usize));
                }
            }
            let _ = probe(&d);
            let _ = split_raw_bitstream(&d);
        }));
        assert!(res.is_ok(), "panic on mutated stream iteration {i}");
    }
    println!("{decoded_ok} of {} mutated decodes succeeded", iters * 3);
}

#[test]
fn random_bytes_never_panic() {
    let mut rng = Rng(123456789);
    for i in 0..2000 {
        let n = rng.below(400);
        let mut d: Vec<u8> = (0..n).map(|_| rng.next() as u8).collect();
        if d.len() >= 8 && rng.below(2) == 0 {
            d[..4].copy_from_slice(b"aPv1");
        } else if d.len() >= 12 && rng.below(2) == 0 {
            let au_sz = (n - 4) as u32;
            d[..4].copy_from_slice(&au_sz.to_be_bytes());
            d[4..8].copy_from_slice(b"aPv1");
        }
        let res = catch_unwind(AssertUnwindSafe(|| {
            let _ = decode_frame(&d);
            let _ = probe(&d);
            let _ = split_raw_bitstream(&d);
        }));
        assert!(res.is_ok(), "panic on random bytes iteration {i}");
    }
}
