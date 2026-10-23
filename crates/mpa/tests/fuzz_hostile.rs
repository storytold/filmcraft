//! Hostile input: random frames behind valid-looking headers must never panic or hang.

use filmcraft_mpa::{Decoder, parse_header};
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
fn random_frames_never_panic() {
    let mut rng = Rng(0x9e37_79b9_7f4a_7c15);
    let mut dec = Decoder::new();
    let mut ok = 0;
    for i in 0..std::env::var("FUZZ_ITERS").ok().and_then(|s| s.parse().ok()).unwrap_or(60_000) {
        let n = if i % 3 == 0 { rng.below(8) } else { 4 + rng.below(1500) };
        let mut d: Vec<u8> = (0..n).map(|_| rng.next() as u8).collect();
        if i % 4 != 0 && d.len() >= 4 {
            d[0] = 0xFF;
            d[1] = 0xE0 | (rng.next() as u8 & 0x1F);
        }
        if i % 5 == 0 {
            // zero or saturate the payload
            let fill = if i % 10 == 0 { 0 } else { 0xFF };
            for b in d.iter_mut().skip(4) {
                *b = fill;
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
    }
    eprintln!("{ok} frames decoded");
}
