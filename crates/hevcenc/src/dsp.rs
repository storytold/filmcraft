//! Distortion metrics for HEVC motion search, lookahead and AQ: SAD, SATD (4x4 Hadamard), SSD, variance.
//!
//! Functions take strided byte planes; callers pass buffers holding `h` rows of `w` pixels at the given strides
//! (short buffers are a caller bug). Loops use plain indexing with no inner branches so they auto-vectorise.

/// Sum of absolute differences over a `w` x `h` block; the basic motion-search cost.
#[inline]
pub fn sad(a: &[u8], sa: usize, b: &[u8], sb: usize, w: usize, h: usize) -> u32 {
    let mut s = 0u32;
    for r in 0..h {
        let base_a = r * sa;
        let base_b = r * sb;
        for c in 0..w {
            s += a[base_a + c].abs_diff(b[base_b + c]) as u32;
        }
    }
    s
}

/// SATD of a 4-row strip `N` wide (N/4 side-by-side 4x4 blocks), lane-wise so the compiler can vectorise it.
/// Uses |p+q| + |p-q| = 2 max(|p|, |q|) for the last butterfly, which also absorbs the /2.
#[inline(always)]
fn satd_strip<const N: usize, const H: usize>(a: &[u8], sa: usize, b: &[u8], sb: usize) -> u32 {
    let mut d = [[0i16; N]; 4];
    for r in 0..4 {
        let base_a = r * sa;
        let base_b = r * sb;
        for i in 0..N {
            d[r][i] = a[base_a + i] as i16 - b[base_b + i] as i16;
        }
    }
    let mut v = [[0i16; N]; 4];
    for i in 0..N {
        let s01 = d[0][i] + d[1][i];
        let d01 = d[0][i] - d[1][i];
        let s23 = d[2][i] + d[3][i];
        let d23 = d[2][i] - d[3][i];
        v[0][i] = s01 + s23;
        v[1][i] = s01 - s23;
        v[2][i] = d01 + d23;
        v[3][i] = d01 - d23;
    }
    let mut total = 0u32;
    for row in &v {
        let mut hs = [0i16; H];
        let mut hd = [0i16; H];
        for k in 0..H {
            hs[k] = row[2 * k] + row[2 * k + 1];
            hd[k] = row[2 * k] - row[2 * k + 1];
        }
        for k in 0..H / 2 {
            total += hs[2 * k].unsigned_abs().max(hs[2 * k + 1].unsigned_abs()) as u32;
            total += hd[2 * k].unsigned_abs().max(hd[2 * k + 1].unsigned_abs()) as u32;
        }
    }
    total
}

/// Sum of absolute 4x4 Hadamard-transformed differences (halved) for blocks with `w` and `h` in {4, 8, 16, 32}.
/// Any other size returns the SAD, so callers can use one entry point for every candidate size.
#[inline]
pub fn satd(a: &[u8], sa: usize, b: &[u8], sb: usize, w: usize, h: usize) -> u32 {
    if !matches!(w, 4 | 8 | 16 | 32) || !matches!(h, 4 | 8 | 16 | 32) {
        return sad(a, sa, b, sb, w, h);
    }
    let mut total = 0u32;
    for by in (0..h).step_by(4) {
        let (ao, bo) = (by * sa, by * sb);
        match w {
            32 => total += satd_strip::<32, 16>(&a[ao..], sa, &b[bo..], sb),
            16 => total += satd_strip::<16, 8>(&a[ao..], sa, &b[bo..], sb),
            8 => total += satd_strip::<8, 4>(&a[ao..], sa, &b[bo..], sb),
            _ => total += satd_strip::<4, 2>(&a[ao..], sa, &b[bo..], sb),
        }
    }
    total
}

/// Sum of squared differences over a `w` x `h` block; the RD-optimal cost for mode decisions.
#[inline]
pub fn ssd(a: &[u8], sa: usize, b: &[u8], sb: usize, w: usize, h: usize) -> u64 {
    let mut s = 0u64;
    for r in 0..h {
        let base_a = r * sa;
        let base_b = r * sb;
        for c in 0..w {
            let d = a[base_a + c] as i32 - b[base_b + c] as i32;
            s += (d * d) as u64;
        }
    }
    s
}

/// Variance times N (sum of squared deviations from the mean); drives AQ strength per block.
#[inline]
pub fn block_var(a: &[u8], sa: usize, w: usize, h: usize) -> u32 {
    let mut sum = 0u64;
    let mut sq = 0u64;
    for r in 0..h {
        let base = r.saturating_mul(sa);
        for c in 0..w {
            let v = u64::from(*a.get(base.saturating_add(c)).unwrap_or(&0));
            sum += v;
            sq += v * v;
        }
    }
    // Mean uses integer division; the truncation only lowers the result slightly, which AQ tolerates.
    let n = w as u64 * h as u64;
    (sq - sum * sum / n) as u32
}

#[cfg(test)]
mod tests {
    use super::*;

    // Deterministic pseudo-pixels; same seed gives the same logical block at any stride.
    fn blot(w: usize, h: usize, stride: usize, seed: u32) -> Vec<u8> {
        let mut v = vec![0xAAu8; stride * h];
        let mut s = seed;
        for r in 0..h {
            for c in 0..w {
                s = s.wrapping_mul(1103515245).wrapping_add(12345);
                v[r * stride + c] = (s >> 16) as u8;
            }
        }
        v
    }

    #[test]
    fn identical_blocks_measure_zero() {
        for (w, h) in [(4, 4), (8, 8), (16, 16), (32, 32), (16, 8), (8, 32)] {
            let a = blot(w, h, w, 7);
            let b = a.clone();
            assert_eq!(sad(&a, w, &b, w, w, h), 0, "{w}x{h}");
            assert_eq!(satd(&a, w, &b, w, w, h), 0, "{w}x{h}");
            assert_eq!(ssd(&a, w, &b, w, w, h), 0, "{w}x{h}");
        }
    }

    #[test]
    fn checkerboard_satd_exceeds_flat_shift_sad() {
        let (w, h) = (8, 8);
        let mut a = vec![0u8; w * h];
        let mut b = vec![0u8; w * h];
        for r in 0..h {
            for c in 0..w {
                a[r * w + c] = if (r + c) % 2 == 0 { 0 } else { 255 };
                b[r * w + c] = if (r + c) % 2 == 0 { 255 } else { 0 };
            }
        }
        let flat_a = vec![128u8; w * h];
        let flat_b = vec![138u8; w * h];
        let checker_satd = satd(&a, w, &b, w, w, h);
        let checker_ssd = ssd(&a, w, &b, w, w, h);
        let flat_sad = sad(&flat_a, w, &flat_b, w, w, h);
        let flat_ssd = ssd(&flat_a, w, &flat_b, w, w, h);
        assert_eq!(flat_sad, 640, "8x8 shift of 10");
        assert_eq!(flat_ssd, 6400, "64 * 10^2");
        assert_eq!(checker_ssd, 4_161_600, "64 * 255^2");
        assert!(checker_satd > flat_sad, "checkerboard SATD {checker_satd} should exceed flat-shift SAD {flat_sad}");
    }

    #[test]
    fn variance_of_flat_is_zero_and_gradient_is_positive() {
        let flat = vec![77u8; 8 * 8];
        assert_eq!(block_var(&flat, 8, 8, 8), 0);
        let mut grad = vec![0u8; 8 * 8];
        for r in 0..8 {
            for c in 0..8 {
                grad[r * 8 + c] = (r * 8 + c) as u8;
            }
        }
        // Values 0..63: sum 2016, sum of squares 85344, mean-square term 63504.
        assert_eq!(block_var(&grad, 8, 8, 8), 21_840);
    }

    #[test]
    fn padded_stride_matches_dense() {
        let (w, h) = (8, 8);
        let dense_a = blot(w, h, w, 7);
        let dense_b = blot(w, h, w, 99);
        let pad_a = blot(w, h, w + 4, 7);
        let pad_b = blot(w, h, w + 4, 99);
        assert_eq!(sad(&dense_a, w, &dense_b, w, w, h), sad(&pad_a, w + 4, &pad_b, w + 4, w, h));
        assert_eq!(satd(&dense_a, w, &dense_b, w, w, h), satd(&pad_a, w + 4, &pad_b, w + 4, w, h));
        assert_eq!(ssd(&dense_a, w, &dense_b, w, w, h), ssd(&pad_a, w + 4, &pad_b, w + 4, w, h));
        assert_eq!(block_var(&dense_a, w, w, h), block_var(&pad_a, w + 4, w, h));
    }

    #[test]
    fn satd_falls_back_to_sad_for_other_sizes() {
        let a = blot(7, 5, 7, 7);
        let b = blot(7, 5, 7, 99);
        assert_eq!(satd(&a, 7, &b, 7, 7, 5), sad(&a, 7, &b, 7, 7, 5));
        let (x, y) = ([10u8], [3u8]);
        assert_eq!(satd(&x, 1, &y, 1, 1, 1), 7);
    }
}
