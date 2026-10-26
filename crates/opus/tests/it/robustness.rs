//! Garbage, truncated and bit-flipped packets must never panic; concealment must produce
//! well-formed output.

use std::path::PathBuf;

use filmcraft_opus::{Decoder, OpusHead, StreamDecoder};

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u32 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        (self.0 >> 16) as u32
    }
}

fn check_finite(out: &[f32]) {
    assert!(out.iter().all(|v| v.is_finite()), "non-finite output");
}

#[test]
fn random_garbage_never_panics() {
    let mut rng = Rng(0x1234_5678_9abc_def0);
    for &rate in &[48000u32, 24000, 16000, 12000, 8000] {
        for channels in 1..=2 {
            let mut dec = StreamDecoder::new(rate, channels).unwrap();
            let mut out = Vec::new();
            for i in 0..3000 {
                let len = (rng.next() % 400) as usize;
                let mut pkt: Vec<u8> = (0..len).map(|_| rng.next() as u8).collect();
                if let Some(t) = pkt.first_mut() {
                    // Cycle through every TOC configuration.
                    *t = ((i % 32) as u8) << 3 | (*t & 7);
                }
                out.clear();
                let _ = dec.decode_interleaved(Some(&pkt), &mut out);
                check_finite(&out);
                if i % 17 == 0 {
                    out.clear();
                    let _ = dec.decode_interleaved(None, &mut out);
                    check_finite(&out);
                }
            }
        }
    }
}

#[test]
fn multistream_garbage_never_panics() {
    let mut h = b"OpusHead".to_vec();
    h.extend([1, 6, 0x38, 0x01, 0x80, 0xBB, 0, 0, 0, 0, 1, 4, 2, 0, 4, 1, 2, 3, 5]);
    let mut dec = Decoder::new(&h).unwrap();
    let mut rng = Rng(42);
    for _ in 0..2000 {
        let len = (rng.next() % 600) as usize;
        let pkt: Vec<u8> = (0..len).map(|_| rng.next() as u8).collect();
        if let Ok(out) = dec.decode(Some(&pkt)) {
            assert_eq!(out.len(), 6);
            for c in &out {
                check_finite(c);
            }
        }
    }
    let out = dec.decode(None).unwrap();
    assert_eq!(out.len(), 6);
}

#[test]
fn concealment_before_and_after_packets() {
    let mut dec = Decoder::from_head(OpusHead::simple(2).unwrap(), 48000).unwrap();
    // Loss before any packet: silence of a default duration.
    let out = dec.decode(None).unwrap();
    assert_eq!(out.len(), 2);
    assert!(out[0].iter().all(|&v| v == 0.0));
}

fn vectors_dir() -> Option<PathBuf> {
    let d = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/fixtures/opus/vectors");
    d.join("testvector01.bit").exists().then_some(d)
}

/// Real packets (from the conformance vectors) truncated and bit-flipped, interleaved with losses.
#[test]
fn corrupted_real_packets() {
    let Some(dir) = vectors_dir() else {
        return;
    };
    let mut rng = Rng(7);
    for v in [1, 2, 5, 12] {
        let data = std::fs::read(dir.join(format!("testvector{v:02}.bit"))).unwrap();
        let mut dec = StreamDecoder::new(48000, 2).unwrap();
        let mut out = Vec::new();
        let mut p = 0;
        let mut n = 0;
        while p + 8 <= data.len() && n < 1500 {
            let len = u32::from_be_bytes(data[p..p + 4].try_into().unwrap()) as usize;
            p += 8;
            let mut pkt = data[p..(p + len).min(data.len())].to_vec();
            p += len;
            n += 1;
            match rng.next() % 4 {
                0 => {
                    let keep = (rng.next() as usize) % (pkt.len() + 1);
                    pkt.truncate(keep);
                }
                1 if !pkt.is_empty() => {
                    for _ in 0..3 {
                        let i = rng.next() as usize % pkt.len();
                        pkt[i] ^= 1 << (rng.next() % 8);
                    }
                }
                2 => pkt.clear(),
                _ => {}
            }
            out.clear();
            let _ = dec.decode_interleaved(if pkt.is_empty() { None } else { Some(&pkt) }, &mut out);
            check_finite(&out);
        }
    }
}
