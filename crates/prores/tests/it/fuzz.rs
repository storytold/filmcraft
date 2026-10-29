//! Robustness: mutated and random inputs must never panic (errors are fine).

use crate::common;
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
        v.push(Encoder::with_config(cfg).encode(&f).unwrap());
    }
    // An interlaced ffmpeg stream, when fixtures from the oracle tests are present.
    if let Some(ff) = common::ffmpeg() {
        let spec = common::Spec::new(
            "fuzz_seed_interlaced",
            "testsrc2=s=64x48:r=25,setfield=tff,format=yuv422p10le",
            1,
            &["-c:v", "prores_ks", "-profile:v", "2", "-flags", "+ildct+ilme"],
        );
        let mov = common::make(&ff, &spec);
        v.extend(common::packets(&ff, &mov));
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
    let mut decoded_ok = 0;
    let iters = 3000;
    for i in 0..iters {
        let mut d = seeds[i % seeds.len()].clone();
        match rng.below(6) {
            0 => {
                for _ in 0..1 + rng.below(8) {
                    let p = rng.below(d.len());
                    d[p] ^= 1 << rng.below(8);
                }
            }
            1 => {
                for _ in 0..1 + rng.below(4) {
                    let p = rng.below(d.len());
                    d[p] = rng.next() as u8;
                }
            }
            2 => {
                let n = rng.below(d.len());
                d.truncate(n);
                if d.len() >= 4 && rng.below(2) == 0 {
                    // keep the frame size field consistent with the truncation
                    let n = d.len() as u32;
                    d[..4].copy_from_slice(&n.to_be_bytes());
                }
            }
            3 => {
                // header fields
                let p = 8 + rng.below(20.min(d.len() - 8));
                d[p] = rng.next() as u8;
            }
            4 => {
                // slice region garbage
                let start = 150.min(d.len() - 1) + rng.below(d.len().saturating_sub(150).max(1));
                for b in d.iter_mut().skip(start).take(1 + rng.below(64)) {
                    *b = rng.next() as u8;
                }
            }
            _ => {
                let p = rng.below(d.len());
                let extra: Vec<u8> = (0..1 + rng.below(16)).map(|_| rng.next() as u8).collect();
                d.splice(p..p, extra);
            }
        }
        for depth in [None, Some(8), Some(16)] {
            let opts = DecodeOptions { bit_depth: depth, threads: i % 2 == 0 };
            if let Ok(f) = decode_frame_with(&d, &opts) {
                decoded_ok += 1;
                assert_eq!(f.y.len(), f.width as usize * f.height as usize);
            }
        }
        let _ = probe(&d);
    }
    println!("{decoded_ok} of {} mutated decodes succeeded", iters * 3);
}

#[test]
fn random_bytes_never_panic() {
    let mut rng = Rng(12345);
    for _ in 0..2000 {
        let n = rng.below(400);
        let mut d: Vec<u8> = (0..n).map(|_| rng.next() as u8).collect();
        if d.len() >= 8 && rng.below(2) == 0 {
            d[..4].copy_from_slice(&(n as u32).to_be_bytes());
            d[4..8].copy_from_slice(b"icpf");
        }
        let _ = decode_frame(&d);
    }
}
