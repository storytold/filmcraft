//! Hostile input: random syncframes behind valid-looking headers must never panic or hang.

use filmcraft_ac3::{Decoder, parse_header};
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

#[test]
fn random_syncframes_never_panic() {
    let mut rng = Rng(0x9e37_79b9_7f4a_7c15);
    let mut dec = Decoder::new();
    let mut ok = 0;
    let bsids = [4u8, 6, 8, 9, 10, 11, 12, 16, 17, 31];
    for i in 0..std::env::var("FUZZ_ITERS").ok().and_then(|s| s.parse().ok()).unwrap_or(40_000) {
        let n = if i % 7 == 0 { rng.below(10) } else { 7 + rng.below(4000) };
        let mut d: Vec<u8> = (0..n).map(|_| rng.next() as u8).collect();
        if d.len() >= 6 && i % 5 != 0 {
            d[0] = 0x0B;
            d[1] = 0x77;
            if i % 2 == 0 {
                d[5] = (bsids[rng.below(bsids.len())] << 3) | (d[5] & 7);
            }
            if i % 3 == 0 {
                d[4] = (d[4] & 0xC0) | rng.below(38) as u8;
            }
        }
        if i % 11 == 0 {
            for b in d.iter_mut().skip(6) {
                *b = if i % 22 == 0 { 0 } else { 0xFF };
            }
        }
        let r = catch_unwind(AssertUnwindSafe(|| {
            let _ = parse_header(&d);
            dec.decode(&d).is_ok()
        }));
        match r {
            Ok(true) => ok += 1,
            Ok(false) => {}
            Err(_) => panic!("decoder panicked on {:02x?}", &d[..d.len().min(16)]),
        }
        if i % 1000 == 0 {
            dec.reset();
        }
    }
    eprintln!("{ok} frames decoded");
}
