//! Hostile encoder configurations and mismatched channel buffers: an error or output, no panic.

use filmcraft_aac::{BitrateMode, Decoder, Encoder, EncoderConfig, WindowShape};
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
fn hostile_encoder_configs_never_panic() {
    let mut rng = Rng(0xaac0_aac0_1234_5678);
    let rates = [0u32, 1, 7999, 8000, 16000, 44100, 48000, 96000, 96001, 192_000, u32::MAX];
    let chans = [0usize, 1, 2, 3, 6, 7, 8, 9, 255, usize::MAX];
    let brs = [0u32, 1, 8000, 128_000, 640_000, u32::MAX];
    let vbrs = [0.0f32, 1.0, 3.0, 5.0, -1.0, 99.0, f32::NAN, f32::INFINITY, f32::NEG_INFINITY];
    let bws = [None, Some(0u32), Some(1), Some(20_000), Some(u32::MAX)];
    let mut bad = Vec::new();
    for _ in 0..400 {
        let cfg = EncoderConfig {
            sample_rate: rates[rng.below(rates.len())],
            channels: chans[rng.below(chans.len())],
            mode: if rng.below(2) == 0 { BitrateMode::Cbr(brs[rng.below(brs.len())]) } else { BitrateMode::Vbr(vbrs[rng.below(vbrs.len())]) },
            bandwidth: bws[rng.below(bws.len())],
            tns: rng.below(2) == 0,
            ms_stereo: rng.below(2) == 0,
            window_shape: if rng.below(2) == 0 { WindowShape::Sine } else { WindowShape::Kbd },
            adts: rng.below(2) == 0,
        };
        let r = catch_unwind(AssertUnwindSafe(|| {
            let Ok(mut enc) = Encoder::new(cfg.clone()) else { return };
            let asc = enc.audio_specific_config();
            let n = rng.below(6000);
            let ch = cfg.channels;
            let data: Vec<Vec<f32>> = (0..ch)
                .map(|c| (0..if c == 1 && n > 3 { n / 2 } else { n }).map(|i| ((i as f32) * 0.03).sin() * [1.0, 1e9, f32::NAN][rng.below(3)]).collect())
                .collect();
            let refs: Vec<&[f32]> = data.iter().map(|v| v.as_slice()).collect();
            let mut aus = enc.encode(&refs);
            // wrong channel count
            let _ = enc.encode(&refs[..refs.len().saturating_sub(1)]);
            let _ = enc.encode(&[]);
            aus.extend(enc.flush());
            if !cfg.adts
                && let Ok(mut dec) = Decoder::new(&asc)
            {
                for au in aus.iter().take(20) {
                    let _ = dec.decode(au);
                }
            }
        }));
        if r.is_err() {
            bad.push(format!("{cfg:?}"));
        }
    }
    assert!(bad.is_empty(), "{} panics, e.g. {:?}", bad.len(), bad.first());
}
