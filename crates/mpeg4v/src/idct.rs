//! 8×8 inverse DCT (ISO/IEC 14496-2 Annex A): the separable 2-D IDCT evaluated in `f32` with even/odd
//! symmetry, rounded to the nearest integer and saturated to [-256, 255]. Its error is far inside
//! the IEEE Std 1180-1990 accuracy limits the standard requires (checked by the test procedure of
//! IEEE 1180 itself, below).

use std::sync::OnceLock;

/// `M[x][u] = C(u)/2 · cos((2x+1)uπ/16)`, `C(0) = 1/√2`, `C(u>0) = 1`.
fn basis() -> &'static [[f32; 8]; 8] {
    static B: OnceLock<[[f32; 8]; 8]> = OnceLock::new();
    B.get_or_init(|| {
        let mut m = [[0f32; 8]; 8];
        for (x, row) in m.iter_mut().enumerate() {
            for (u, v) in row.iter_mut().enumerate() {
                let c = if u == 0 { std::f64::consts::FRAC_1_SQRT_2 } else { 1.0 };
                *v = (c / 2.0 * (((2 * x + 1) * u) as f64 * std::f64::consts::PI / 16.0).cos()) as f32;
            }
        }
        m
    })
}

/// In-place IDCT of a block of dequantised coefficients (raster order). `dc_only`: only `blk[0]`
/// may be non-zero.
#[inline]
pub(crate) fn idct(blk: &mut [i32; 64], dc_only: bool) {
    if dc_only {
        let v = ((blk[0] + 4) >> 3).clamp(-256, 255);
        blk.fill(v);
        return;
    }
    let m = basis();
    // The DC term F[0][0]/8 is added exactly (so ties round as in the DC-only path).
    let dc = blk[0] as f32 * 0.125;
    blk[0] = 0;
    // rows: tmp[v][x] = Σ_u M[x][u] F[v][u]
    let mut tmp = [[0f32; 8]; 8];
    for v in 0..8 {
        let r = &blk[v * 8..v * 8 + 8];
        if r.iter().all(|&c| c == 0) {
            continue;
        }
        let f: [f32; 8] = std::array::from_fn(|u| r[u] as f32);
        for x in 0..4 {
            let mx = &m[x];
            let e = mx[0] * f[0] + mx[2] * f[2] + mx[4] * f[4] + mx[6] * f[6];
            let o = mx[1] * f[1] + mx[3] * f[3] + mx[5] * f[5] + mx[7] * f[7];
            tmp[v][x] = e + o;
            tmp[v][7 - x] = e - o;
        }
    }
    // columns, eight at a time: out[y][x] = Σ_v M[y][v] tmp[v][x]
    for y in 0..4 {
        let my = &m[y];
        let mut e = [0f32; 8];
        let mut o = [0f32; 8];
        for x in 0..8 {
            e[x] = my[0] * tmp[0][x] + my[2] * tmp[2][x] + my[4] * tmp[4][x] + my[6] * tmp[6][x];
            o[x] = my[1] * tmp[1][x] + my[3] * tmp[3][x] + my[5] * tmp[5][x] + my[7] * tmp[7][x];
        }
        for x in 0..8 {
            blk[y * 8 + x] = ((dc + (e[x] + o[x]) + 0.5).floor() as i32).clamp(-256, 255);
            blk[(7 - y) * 8 + x] = ((dc + (e[x] - o[x]) + 0.5).floor() as i32).clamp(-256, 255);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The pseudo-random generator of IEEE Std 1180-1990 (values in [-l, h]).
    struct Rand(i64);
    impl Rand {
        fn next(&mut self, l: i64, h: i64) -> i64 {
            self.0 = (self.0.wrapping_mul(1_103_515_245).wrapping_add(12_345)) & 0xFFFF_FFFF;
            let i = self.0 & 0x7FFF_FFFE;
            let x = i as f64 / 0x7FFF_FFFF as f64 * (l + h + 1) as f64;
            x as i64 - l
        }
    }

    fn c(u: usize) -> f64 {
        if u == 0 { std::f64::consts::FRAC_1_SQRT_2 } else { 1.0 }
    }

    fn cosv(a: usize, b: usize) -> f64 {
        static T: OnceLock<[[f64; 8]; 8]> = OnceLock::new();
        T.get_or_init(|| std::array::from_fn(|a| std::array::from_fn(|b| (((2 * a + 1) * b) as f64 * std::f64::consts::PI / 16.0).cos())))[a][b]
    }

    /// Separable double-precision 1-D passes (exact up to f64 rounding).
    fn fdct(p: &[i64; 64]) -> [i64; 64] {
        let mut t = [0f64; 64];
        for y in 0..8 {
            for u in 0..8 {
                t[y * 8 + u] = (0..8).map(|x| p[y * 8 + x] as f64 * cosv(x, u)).sum::<f64>() * c(u) / 2.0;
            }
        }
        let mut out = [0i64; 64];
        for v in 0..8 {
            for u in 0..8 {
                let s = (0..8).map(|y| t[y * 8 + u] * cosv(y, v)).sum::<f64>() * c(v) / 2.0;
                out[v * 8 + u] = (s.round() as i64).clamp(-2048, 2047);
            }
        }
        out
    }

    fn reference_idct(f: &[i64; 64]) -> [i64; 64] {
        let mut t = [0f64; 64];
        for v in 0..8 {
            for x in 0..8 {
                t[v * 8 + x] = (0..8).map(|u| c(u) * f[v * 8 + u] as f64 * cosv(x, u)).sum::<f64>() / 2.0;
            }
        }
        let mut out = [0i64; 64];
        for y in 0..8 {
            for x in 0..8 {
                let s = (0..8).map(|v| c(v) * t[v * 8 + x] * cosv(y, v)).sum::<f64>() / 2.0;
                out[y * 8 + x] = (s.round() as i64).clamp(-256, 255);
            }
        }
        out
    }

    /// IEEE 1180-1990 §3: 10 000 blocks for each range and sign; peak error ≤ 1, per-pixel mean
    /// square error ≤ 0.06, overall ≤ 0.02, per-pixel mean error ≤ 0.015, overall ≤ 0.0015.
    #[test]
    fn ieee_1180_accuracy() {
        let blocks = if cfg!(debug_assertions) { 2_000 } else { 10_000 };
        for (l, h) in [(256i64, 255i64), (5, 5), (300, 300)] {
            for sign in [1i64, -1] {
                let mut rng = Rand(1);
                let mut sum_err = [0i64; 64];
                let mut sum_sq = [0i64; 64];
                for _ in 0..blocks {
                    let mut p = [0i64; 64];
                    for v in p.iter_mut() {
                        *v = rng.next(l, h) * sign;
                    }
                    let f = fdct(&p);
                    let r = reference_idct(&f);
                    let mut blk: [i32; 64] = std::array::from_fn(|i| f[i] as i32);
                    idct(&mut blk, false);
                    for i in 0..64 {
                        let e = blk[i] as i64 - r[i];
                        assert!(e.abs() <= 1, "peak error {e} (range {l}/{h} sign {sign})");
                        sum_err[i] += e;
                        sum_sq[i] += e * e;
                    }
                }
                let n = blocks as f64;
                for i in 0..64 {
                    assert!(sum_sq[i] as f64 / n <= 0.06);
                    assert!((sum_err[i] as f64 / n).abs() <= 0.015);
                }
                assert!(sum_sq.iter().sum::<i64>() as f64 / (64.0 * n) <= 0.02);
                assert!((sum_err.iter().sum::<i64>() as f64 / (64.0 * n)).abs() <= 0.0015);
            }
        }
        // all-zero input gives all-zero output
        let mut z = [0i32; 64];
        idct(&mut z, false);
        assert!(z.iter().all(|&v| v == 0));
    }

    #[test]
    fn dc_only_matches_the_full_transform() {
        for dc in -2048..=2047 {
            let mut a = [0i32; 64];
            a[0] = dc;
            let mut b = a;
            idct(&mut a, true);
            idct(&mut b, false);
            assert_eq!(a, b, "dc {dc}");
        }
    }
}
