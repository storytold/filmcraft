//! Hostile configs and sample values must give errors or frames, never a panic.

use filmcraft_flac::{Encoder, EncoderConfig};
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
fn hostile_configs_and_samples_never_panic() {
    let mut rng = Rng(0x1234_5678_9abc_def1);
    let rates = [0u32, 1, 8000, 44100, 1_048_575, 1_048_576, u32::MAX];
    let blocks = [0u16, 1, 15, 16, 17, 255, 4096, 65535];
    let extremes = [0i32, 1, -1, i32::MAX, i32::MIN, 127, -128, 32767, -32768, 0x7f_ffff, -0x80_0000];
    for i in 0..4000 {
        let cfg = EncoderConfig {
            sample_rate: rates[rng.below(rates.len())],
            channels: rng.below(11) as u8,
            bits_per_sample: [0u8, 7, 8, 12, 16, 20, 24, 32, 33, 255][rng.below(10)],
            block_size: blocks[rng.below(blocks.len())],
            level: rng.below(12) as u8,
        };
        let r = catch_unwind(AssertUnwindSafe(|| {
            let Ok(mut enc) = Encoder::new(cfg.clone()) else { return };
            let _ = enc.header();
            for _ in 0..1 + rng.below(3) {
                let n = rng.below(9000);
                let chans = usize::from(cfg.channels) + usize::from(i % 7 == 0);
                let data: Vec<Vec<i32>> = (0..chans)
                    .map(|_| {
                        let mode = rng.below(4);
                        (0..n)
                            .map(|j| match mode {
                                0 => extremes[rng.below(extremes.len())],
                                1 => rng.next() as i32,
                                2 => 0,
                                _ => {
                                    if j % 2 == 0 {
                                        i32::MAX
                                    } else {
                                        i32::MIN
                                    }
                                }
                            })
                            .collect()
                    })
                    .collect();
                let refs: Vec<&[i32]> = data.iter().map(|v| v.as_slice()).collect();
                let _ = enc.encode_frames(&refs);
            }
            let _ = enc.finish();
            let _ = enc.streaminfo();
        }));
        assert!(r.is_ok(), "encoder panicked for {cfg:?}");
    }
}
