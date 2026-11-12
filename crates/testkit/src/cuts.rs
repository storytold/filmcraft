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

/// Reads `frames` samples from `start` in one request and in every [`CUT_PATTERNS`] cut, through
/// `read(start, frames) -> planar channels`, and asserts every cut is bit-identical to the single
/// read. The single read must carry a signal, so silence cannot pass by accident.
pub fn assert_cut_independent(what: &str, start: i64, frames: usize, mut read: impl FnMut(i64, usize) -> Vec<Vec<f32>>) {
    let whole = read(start, frames);
    assert!(whole.iter().flatten().any(|v| *v != 0.0), "{what}: the single read is silent");
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
