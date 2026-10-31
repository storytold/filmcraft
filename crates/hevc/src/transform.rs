//! Scaling (8.6.3), transform skip and inverse transforms (8.6.4): DCT 4..32 and DST 4x4.

use crate::tables::{DST_MATRIX, TRANS_MATRIX};

/// Inverse transform of an n x n block of scaled coefficients `c` (row-major, `c[y * n + x]`) in place,
/// producing residuals after the final `bd_shift` rounding (8-299). `max_x` / `max_y` bound the
/// non-zero coefficients (inclusive). `dst`: DST-VII (intra 4x4 luma).
///
/// Evaluated as sums of basis rows scaled by the non-zero coefficients (zero coefficients, which
/// are most of them, cost nothing), with the size a compile-time constant so the inner loops over
/// contiguous basis rows vectorise. Integer sums are exact (|sum| < 2^27), so the result equals the
/// direct matrix product of 8.6.4.2 in any summation order.
pub fn inverse_transform(c: &mut [i32], n: usize, dst: bool, max_x: usize, max_y: usize, bd_shift: u32) {
    match n {
        4 => inverse::<4>(c, dst, max_x, max_y, bd_shift),
        8 => inverse::<8>(c, false, max_x, max_y, bd_shift),
        16 => inverse::<16>(c, false, max_x, max_y, bd_shift),
        _ => inverse::<32>(c, false, max_x, max_y, bd_shift),
    }
}

#[inline(always)]
fn inverse<const N: usize>(c: &mut [i32], dst: bool, max_x: usize, max_y: usize, bd_shift: u32) {
    let step = 32 / N;
    // basis function of coefficient index j (row j of the transform matrix, N samples)
    let basis = |j: usize| -> &[i8] { if dst { &DST_MATRIX[j][..N] } else { &TRANS_MATRIX[j * step][..N] } };
    // 1. columns (8-297 ... first stage): e[x][y] = sum_j M[j][y] * d[x][j], stored transposed
    //    (col[x][y]) so each coefficient adds one contiguous basis row.
    let mut col = [[0i32; N]; N];
    for x in 0..=max_x.min(N - 1) {
        let acc = &mut col[x];
        for j in 0..=max_y.min(N - 1) {
            let d = c[j * N + x];
            if d != 0 {
                for (a, &m) in acc.iter_mut().zip(basis(j)) {
                    *a += m as i32 * d;
                }
            }
        }
        for a in acc.iter_mut() {
            *a = ((*a + 64) >> 7).clamp(-32768, 32767);
        }
    }
    // 2. rows: r[x][y] = sum_j M[j][x] * g[j][y], g[j][y] = col[j][y]
    let round = 1 << (bd_shift - 1);
    for y in 0..N {
        let mut out = [round; N];
        for (j, g) in col.iter().enumerate().take(max_x.min(N - 1) + 1) {
            let g = g[y];
            if g != 0 {
                for (o, &m) in out.iter_mut().zip(basis(j)) {
                    *o += m as i32 * g;
                }
            }
        }
        for (r, o) in c[y * N..y * N + N].iter_mut().zip(out) {
            *r = o >> bd_shift;
        }
    }
}

/// DC-only inverse DCT: every residual sample gets the same value.
pub fn inverse_dc(dc: i32, bd_shift: u32) -> i32 {
    let e = ((64 * dc + 64) >> 7).clamp(-32768, 32767);
    (64 * e + (1 << (bd_shift - 1))) >> bd_shift
}

/// Transform skip residual (8-298, 8-299): r = (d << tsShift + round) >> bdShift.
pub fn transform_skip(c: &mut [i32], n: usize, bd_shift: u32) {
    let ts_shift = 5 + n.trailing_zeros();
    let round = 1 << (bd_shift - 1);
    for v in c[..n * n].iter_mut() {
        *v = ((*v << ts_shift) + round) >> bd_shift;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The direct matrix product of 8.6.4.2 (the original implementation), as the reference.
    fn reference(c: &mut [i32], n: usize, dst: bool, max_x: usize, max_y: usize, bd_shift: u32) {
        let mut tmp = [0i32; 32 * 32];
        let step = 32 / n;
        for x in 0..=max_x {
            for y in 0..n {
                let mut s = 0i32;
                for j in 0..=max_y {
                    let m = if dst { DST_MATRIX[j][y] as i32 } else { TRANS_MATRIX[j * step][y] as i32 };
                    s += m * c[j * n + x];
                }
                tmp[y * n + x] = ((s + 64) >> 7).clamp(-32768, 32767);
            }
        }
        let round = 1 << (bd_shift - 1);
        for y in 0..n {
            for x in 0..n {
                let mut s = 0i32;
                for j in 0..=max_x {
                    let m = if dst { DST_MATRIX[j][x] as i32 } else { TRANS_MATRIX[j * step][x] as i32 };
                    s += m * tmp[y * n + j];
                }
                c[y * n + x] = (s + round) >> bd_shift;
            }
        }
    }

    #[test]
    fn matches_the_direct_matrix_product() {
        // sparse and dense blocks, extreme coefficients, every size, DST, 8- and 10-bit shifts
        let mut rng = 0x2545_f491_4f6c_dd1du64;
        let mut next = move || {
            rng ^= rng << 13;
            rng ^= rng >> 7;
            rng ^= rng << 17;
            rng
        };
        for &(n, dst) in &[(4usize, true), (4, false), (8, false), (16, false), (32, false)] {
            for case in 0..400 {
                let (max_x, max_y) = ((next() as usize) % n, (next() as usize) % n);
                let density = [2u64, 8, 50, 100][case % 4];
                let mut c = vec![0i32; n * n];
                for y in 0..=max_y {
                    for x in 0..=max_x {
                        if next() % 100 < density {
                            c[y * n + x] = match next() % 8 {
                                0 => 32767,
                                1 => -32768,
                                _ => (next() % 2001) as i32 - 1000,
                            };
                        }
                    }
                }
                for bd_shift in [12u32, 10] {
                    let (mut a, mut b) = (c.clone(), c.clone());
                    inverse_transform(&mut a, n, dst, max_x, max_y, bd_shift);
                    reference(&mut b, n, dst, max_x, max_y, bd_shift);
                    assert_eq!(a, b, "n {n} dst {dst} max ({max_x}, {max_y}) shift {bd_shift}");
                }
            }
        }
    }

    /// The vector kernels (transform and addition to 8-bit predictions in one) give what the
    /// transform and Clip1 addition give: every size, DST, bounded regions, extreme and
    /// out-of-range coefficients (which saturate as the scaling process clips them), extreme
    /// predictions, and DC-only blocks.
    #[test]
    fn vector_transform_add_matches_transform_and_add() {
        let mut rng = 0x9e37_79b9_7f4a_7c15u64;
        let mut next = move || {
            rng ^= rng << 13;
            rng ^= rng >> 7;
            rng ^= rng << 17;
            rng
        };
        let mut ran = 0;
        for &level in filmcraft_simd::levels() {
            for &(n, dst) in &[(4usize, true), (4, false), (8, false), (16, false), (32, false)] {
                for case in 0..300 {
                    let (max_x, max_y) = if case % 5 == 0 { (0, 0) } else { ((next() as usize) % n, (next() as usize) % n) };
                    let density = [2u64, 8, 50, 100][case % 4];
                    let mut c = vec![0i32; n * n];
                    for y in 0..=max_y {
                        for x in 0..=max_x {
                            if next() % 100 < density || (x, y) == (max_x, max_y) {
                                c[y * n + x] = match next() % 10 {
                                    0 => 32767,
                                    1 => -32768,
                                    2 => 40000,
                                    3 => -40000,
                                    _ => (next() % 2001) as i32 - 1000,
                                };
                            }
                        }
                    }
                    let ds = n + 3;
                    let pred: Vec<u8> = (0..ds * n)
                        .map(|_| match next() % 6 {
                            0 => 0,
                            1 => 255,
                            _ => next() as u8,
                        })
                        .collect();
                    // the scalar path: coefficients clipped as scaling leaves them
                    let mut r: Vec<i32> = c.iter().map(|&v| v.clamp(-32768, 32767)).collect();
                    let dc_only = max_x == 0 && max_y == 0 && !dst;
                    if dc_only {
                        let v = inverse_dc(r[0], 12);
                        r.fill(v);
                    } else {
                        inverse_transform(&mut r, n, dst, max_x, max_y, 12);
                    }
                    let mut want = pred.clone();
                    for y in 0..n {
                        for x in 0..n {
                            want[y * ds + x] = (want[y * ds + x] as i32 + r[y * n + x]).clamp(0, 255) as u8;
                        }
                    }
                    let mut got = pred.clone();
                    let done = if dc_only {
                        filmcraft_simd::hevc_dc_add8_at(level, inverse_dc(c[0].clamp(-32768, 32767), 12), n, &mut got, ds)
                    } else {
                        filmcraft_simd::hevc_idct_add8_at(level, &c, n, dst, (max_x, max_y), &mut got, ds)
                    };
                    if !done {
                        assert_eq!(got, pred, "nothing written when declined");
                        continue;
                    }
                    ran += 1;
                    assert_eq!(got, want, "{level:?} n {n} dst {dst} max ({max_x}, {max_y}) case {case}");
                }
            }
        }
        // (only aarch64 has vector versions of these kernels so far)
        assert!(ran > 0 || !cfg!(target_arch = "aarch64"));
    }

    #[test]
    fn dc_only_matches_full_transform() {
        for &n in &[4usize, 8, 16, 32] {
            for dc in [-300, -1, 1, 7, 64, 1000] {
                let mut c = vec![0i32; n * n];
                c[0] = dc;
                inverse_transform(&mut c, n, false, 0, 0, 12);
                let v = inverse_dc(dc, 12);
                assert!(c.iter().all(|&x| x == v), "n {n} dc {dc}");
            }
        }
    }

    #[test]
    fn bounded_region_matches_full() {
        let n = 16;
        let mut c = vec![0i32; n * n];
        c[0] = 50;
        c[1] = -20;
        c[2 * n + 3] = 9;
        let mut a = c.clone();
        inverse_transform(&mut a, n, false, 3, 2, 12);
        let mut b = c.clone();
        inverse_transform(&mut b, n, false, n - 1, n - 1, 12);
        assert_eq!(a, b);
    }
}
