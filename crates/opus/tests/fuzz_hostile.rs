//! Hostile headers, constructor arguments and concealment lengths must give errors, not panics
//! or giant allocations.

use filmcraft_opus::{Decoder, OpusHead, Packet, StreamDecoder};
use std::panic::{AssertUnwindSafe, catch_unwind};

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

fn valid_head(channels: u8) -> Vec<u8> {
    let mut h = b"OpusHead".to_vec();
    h.extend([1, channels, 0x38, 0x01, 0x80, 0xBB, 0, 0, 0, 0, 0, 0]);
    h
}

#[test]
fn hostile_headers_never_panic() {
    let mut rng = Rng(0x0fed_cba9_8765_4321);
    let mut seeds = vec![valid_head(1), valid_head(2)];
    let mut ms = b"OpusHead".to_vec();
    ms.extend([1, 6, 0x38, 0x01, 0x80, 0xBB, 0, 0, 0, 0, 1, 4, 2, 0, 4, 1, 2, 3, 5]);
    seeds.push(ms);
    for i in 0..20_000 {
        let mut h = seeds[i % seeds.len()].clone();
        for _ in 0..1 + rng.below(4) {
            match rng.below(4) {
                0 => {
                    let p = rng.below(h.len().max(1));
                    if let Some(b) = h.get_mut(p) {
                        *b = rng.next() as u8;
                    }
                }
                1 => h.truncate(rng.below(h.len() + 1)),
                2 => {
                    let p = rng.below(h.len().max(1));
                    if let Some(b) = h.get_mut(p) {
                        *b = [0u8, 255, 1, 2][rng.below(4)];
                    }
                }
                _ => h.extend((0..rng.below(8)).map(|_| rng.next() as u8)),
            }
        }
        let r = catch_unwind(AssertUnwindSafe(|| {
            let _ = OpusHead::parse(&h);
            let _ = OpusHead::from_dops(&h);
            if let Ok(mut d) = Decoder::new(&h) {
                let pkt: Vec<u8> = (0..rng.below(100)).map(|_| rng.next() as u8).collect();
                let _ = d.decode(Some(&pkt));
                let _ = d.decode(None);
            }
        }));
        assert!(r.is_ok(), "header {h:02x?}");
    }
}

#[test]
fn hostile_constructor_and_lengths_never_panic() {
    for rate in [0u32, 1, 7999, 8000, 12000, 16000, 24000, 48000, 96000, u32::MAX] {
        for ch in [0usize, 1, 2, 3, 255, usize::MAX] {
            let r = catch_unwind(AssertUnwindSafe(|| {
                if let Ok(mut d) = StreamDecoder::new(rate, ch) {
                    let mut out = Vec::new();
                    for n in [0usize, 1, 480, 5760, 1 << 20] {
                        out.clear();
                        let _ = d.conceal(n, &mut out);
                        out.clear();
                        let _ = d.decode_fec(&[0xFC, 1, 2, 3], n, &mut out);
                        out.clear();
                        let _ = d.decode_fec(&[], n, &mut out);
                    }
                    d.set_gain_q8(i16::MIN);
                    d.set_gain_q8(i16::MAX);
                }
            }));
            assert!(r.is_ok(), "rate {rate} channels {ch}");
        }
    }
    let mut rng = Rng(7);
    for _ in 0..5000 {
        let p: Vec<u8> = (0..rng.below(50)).map(|_| rng.next() as u8).collect();
        let _ = Packet::parse(&p);
        let _ = Packet::parse_self_delimited(&p);
    }
}
