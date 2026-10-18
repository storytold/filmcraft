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
    // Cache detection once per process; only this kernel gets a modern CPU variant.
    // Pulp's safe token checks CPU/OS support before crossing the target-feature boundary.
    #[cfg(target_arch = "x86_64")]
    {
        static SIMD: std::sync::OnceLock<Option<pulp::x86::V3>> = std::sync::OnceLock::new();
        if let Some(simd) = SIMD.get_or_init(pulp::x86::V3::try_new) {
            return simd.vectorize(|| inverse_transform_impl(c, n, dst, max_x, max_y, bd_shift));
        }
    }
    inverse_transform_impl(c, n, dst, max_x, max_y, bd_shift);
}

#[inline(always)]
fn inverse_transform_impl(c: &mut [i32], n: usize, dst: bool, max_x: usize, max_y: usize, bd_shift: u32) {
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
        // Sparse/dense blocks, extreme coefficients, every size, DST and 8/10/12-bit shifts.
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
                for bd_shift in [12u32, 10, 8] {
                    let (mut a, mut b) = (c.clone(), c.clone());
                    inverse_transform(&mut a, n, dst, max_x, max_y, bd_shift);
                    reference(&mut b, n, dst, max_x, max_y, bd_shift);
                    assert_eq!(a, b, "n {n} dst {dst} max ({max_x}, {max_y}) shift {bd_shift}");
                    let mut baseline = c.clone();
                    inverse_transform_impl(&mut baseline, n, dst, max_x, max_y, bd_shift);
                    assert_eq!(a, baseline, "runtime CPU path vs baseline, n {n} dst {dst}");
                }
            }
        }
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
