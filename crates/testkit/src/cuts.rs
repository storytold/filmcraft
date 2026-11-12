//! Read-boundary independence: a source must give the same samples however a range is cut into
//! requests (the mixer documents that its output does not depend on the cuts).

/// Cut patterns (request lengths, repeated) that put request starts at many fractional positions.
pub const CUT_PATTERNS: &[&[usize]] = &[&[333], &[1, 63, 1000], &[480, 512, 1024]];

/// `n` full-scale pseudo-random samples (white noise). Resampling weights only show in the output
/// when neighbouring samples differ a lot, so a smooth tone cannot expose a one-ulp weight change.
pub fn noise(n: usize, seed: u64) -> Vec<f32> {
    let mut x = seed | 1;
    (0..n)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            ((x >> 40) as f32 / (1u64 << 23) as f32) - 1.0
        })
        .collect()
}

/// `n` samples of +1.0 or -1.0 at random. Neighbouring samples then differ by 0 or 2, so any
/// change of an interpolation weight, however small, changes the interpolated sample.
pub fn signs(n: usize, seed: u64) -> Vec<f32> {
    noise(n, seed).into_iter().map(|v| if v < 0.0 { -1.0 } else { 1.0 }).collect()
}

/// Output rate and frame count for a read that exposes weight rounding: a large ratio and many
/// frames put source positions in the millions, where one f64 rounding error moves an f32 weight.
pub const RATE: u32 = 1_999;
pub const FRAMES: usize = 60_000;
/// Source samples such a read needs from a 48 kHz or 44.1 kHz source, with room to spare.
pub const SOURCE_SAMPLES: usize = 1_600_000;

/// Reads `frames` samples from `start` in one request and in every [`CUT_PATTERNS`] cut, through
/// `read(start, frames) -> planar channels`, and asserts every cut is bit-identical to the single
/// read. The single read must carry a signal, so silence cannot pass by accident.
pub fn assert_cut_independent(what: &str, start: i64, frames: usize, mut read: impl FnMut(i64, usize) -> Vec<Vec<f32>>) {
    let whole = read(start, frames);
    assert!(whole.iter().flatten().any(|v| *v != 0.0), "{what}: the single read is silent");
    assert!(whole.iter().all(|c| c.len() == frames), "{what}: the single read is not {frames} samples per channel");
    for pattern in CUT_PATTERNS {
        let mut cut: Vec<Vec<f32>> = vec![Vec::with_capacity(frames); whole.len()];
        let mut done = 0;
        for &len in pattern.iter().cycle() {
            if done >= frames {
                break;
            }
            let n = len.min(frames - done);
            let part = read(start + done as i64, n);
            assert_eq!(part.len(), whole.len(), "{what}: channel count of a cut read");
            for (c, p) in cut.iter_mut().zip(part) {
                assert_eq!(p.len(), n, "{what}: length of a cut read");
                c.extend(p);
            }
            done += n;
        }
        for (c, (w, g)) in whole.iter().zip(&cut).enumerate() {
            let differ = w.iter().zip(g).filter(|(a, b)| a.to_bits() != b.to_bits()).count();
            let first = w.iter().zip(g).position(|(a, b)| a.to_bits() != b.to_bits());
            assert_eq!(differ, 0, "{what}: cuts {pattern:?} change {differ} samples of channel {c} (first at {first:?})");
        }
    }
}
