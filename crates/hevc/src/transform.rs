//! Scaling (8.6.3), transform skip and inverse transforms (8.6.4): DCT 4..32 and DST 4x4.

use crate::tables::{DST_MATRIX, TRANS_MATRIX};

/// Inverse transform of an n x n block of scaled coefficients `c` (row-major, `c[y * n + x]`) in place,
/// producing residuals after the final `bd_shift` rounding (8-299). `max_x` / `max_y` bound the
/// non-zero coefficients (inclusive). `dst`: DST-VII (intra 4x4 luma). Coefficients are in the
/// 16-bit range the scaling process clips them to (8.6.2).
///
/// The first stage is evaluated as sums of basis rows scaled by the non-zero coefficients (zero
/// coefficients, which are most of them, cost nothing), with the size a compile-time constant so
/// the inner loops over contiguous basis rows vectorise. The second stage of the 8..32-point DCT
/// uses the even/odd butterfly, skipping all-zero columns. Integer sums are exact (|sum| < 2^27),
/// so the result equals the direct matrix product of 8.6.4.2 in any summation order.
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
    let c = &mut c[..N * N];
    let step = 32 / N;
    // basis function of coefficient index j (row j of the transform matrix, N samples)
    let basis = |j: usize| -> &[i8] { if dst { &DST_MATRIX[j][..N] } else { &TRANS_MATRIX[j * step][..N] } };
    // 1. columns (8-297 ... first stage): e[x][y] = sum_j M[j][y] * d[x][j], stored transposed
    //    (col[x][y]) so each coefficient adds one contiguous basis row.
    let mut col = [[0i16; N]; N];
    let mut nz = 0u32; // columns x with a non-zero coefficient
    for x in 0..=max_x.min(N - 1) {
        let mut acc = [0i32; N];
        for j in 0..=max_y.min(N - 1) {
            let d = c[j * N + x] as i16;
            if d != 0 {
                nz |= 1 << x;
                for (a, &m) in acc.iter_mut().zip(basis(j)) {
                    *a += m as i16 as i32 * d as i32;
                }
            }
        }
        for (g, a) in col[x].iter_mut().zip(acc) {
            *g = ((a + 64) >> 7).clamp(-32768, 32767) as i16;
        }
    }
    // 2. rows: r[x][y] = sum_j M[j][x] * g[j][y], g[j][y] = col[j][y]
    let round = 1 << (bd_shift - 1);
    if N >= 8 {
        let mut e = [[0i32; N]; N];
        rows_butterfly::<N>(&col, nz, &mut e);
        for (y, r) in c.as_chunks_mut::<N>().0.iter_mut().enumerate() {
            for (r, e) in r.iter_mut().zip(&e) {
                *r = (e[y] + round) >> bd_shift;
            }
        }
        return;
    }
    for y in 0..N {
        let mut out = [round; N];
        for (j, g) in col.iter().enumerate().take(max_x.min(N - 1) + 1) {
            let g = g[y] as i32;
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

/// Second stage of the N-point DCT (N >= 8) by even/odd decomposition, kept transposed like `col`:
/// `e[x][y] = sum_j M[j][x] * col[j][y]` over the rows of `col` set in `nz` (the others are zero).
/// Level `m` (2, 4, .. N) adds the odd basis rows of the m-point DCT (coefficients j = s, 3s, 5s,
/// .., s = N / m), which are antisymmetric, to the symmetric m/2-point result:
/// `e[k] += o[k]`, `e[m-1-k] = e[k] - o[k]` for k < m/2.
#[inline(never)]
fn rows_butterfly<const N: usize>(col: &[[i16; N]; N], nz: u32, e: &mut [[i32; N]; N]) {
    let step = 32 / N;
    for (e, &g) in e[0].iter_mut().zip(&col[0]) {
        *e = 64 * g as i32;
    }
    let mut m = 2;
    while m <= N {
        let s = N / m;
        let odd = (s..N).step_by(2 * s).fold(0, |b, j| b | 1 << j) & nz;
        let (lo, hi) = e.split_at_mut(m / 2);
        for (k, ek) in lo.iter_mut().enumerate() {
            let em = &mut hi[m / 2 - 1 - k];
            if odd == 0 {
                *em = *ek;
                continue;
            }
            let mut o = [0; N];
            let mut bits = odd;
            while bits != 0 {
                let j = bits.trailing_zeros() as usize;
                mac(&mut o, TRANS_MATRIX[j * step][k] as i16, &col[j]);
                bits &= bits - 1;
            }
            *em = *ek;
            add(ek, &o);
            sub(em, &o);
        }
        m *= 2;
    }
}

#[inline(always)]
fn add<const N: usize>(a: &mut [i32; N], b: &[i32; N]) {
    for (a, &b) in a.iter_mut().zip(b) {
        *a += b;
    }
}

#[inline(always)]
fn sub<const N: usize>(a: &mut [i32; N], b: &[i32; N]) {
    for (a, &b) in a.iter_mut().zip(b) {
        *a -= b;
    }
}

#[inline(always)]
fn mac<const N: usize>(o: &mut [i32; N], w: i16, g: &[i16; N]) {
    for (o, &g) in o.iter_mut().zip(g) {
        *o += w as i32 * g as i32;
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
