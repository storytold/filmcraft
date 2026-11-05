//! 2D inverse transforms and reconstruction (8.6.2, 8.7.2). The 1D DCT / ADST butterflies are
//! generated from the specification's step lists (`transform_gen.rs`); ADST4 (8.7.1.6) and the
//! Walsh-Hadamard transform (8.7.1.10) are written out here.

use crate::tables::{ADST_ADST, ADST_DCT, DCT_ADST, DCT_DCT};
use crate::transform_gen::{narrow, wide};

const SINPI_1_9: i64 = 5283;
const SINPI_2_9: i64 = 9929;
const SINPI_3_9: i64 = 13377;
const SINPI_4_9: i64 = 15212;

macro_rules! adst4 {
    ($name:ident, $t:ty) => {
        #[inline(always)]
        fn $name(x: [$t; 4]) -> [$t; 4] {
            let (s1_9, s2_9, s3_9, s4_9) = (SINPI_1_9 as $t, SINPI_2_9 as $t, SINPI_3_9 as $t, SINPI_4_9 as $t);
            let s0 = s1_9.wrapping_mul(x[0]);
            let s1 = s2_9.wrapping_mul(x[0]);
            let s2 = s3_9.wrapping_mul(x[1]);
            let s3 = s4_9.wrapping_mul(x[2]);
            let s4 = s1_9.wrapping_mul(x[2]);
            let s5 = s2_9.wrapping_mul(x[3]);
            let s6 = s4_9.wrapping_mul(x[3]);
            let v = x[0].wrapping_sub(x[2]).wrapping_add(x[3]);
            let s7 = s3_9.wrapping_mul(v);
            let x0 = s0.wrapping_add(s3).wrapping_add(s5);
            let x1 = s1.wrapping_sub(s4).wrapping_sub(s6);
            let x2 = s7;
            let x3 = s2;
            let r = |v: $t| v.wrapping_add(1 << 13) >> 14;
            [r(x0.wrapping_add(x3)), r(x1.wrapping_add(x3)), r(x2), r(x0.wrapping_add(x1).wrapping_sub(x3))]
        }
    };
}
adst4!(iadst4_narrow, i32);
adst4!(iadst4_wide, i64);

/// Inverse Walsh-Hadamard transform (8.7.1.10).
fn iwht4(x: [i32; 4], shift: u32) -> [i32; 4] {
    let mut a = x[0] >> shift;
    let mut c = x[1] >> shift;
    let mut d = x[2] >> shift;
    let mut b = x[3] >> shift;
    a = a.wrapping_add(c);
    d = d.wrapping_sub(b);
    let e = a.wrapping_sub(d) >> 1;
    b = e.wrapping_sub(b);
    c = e.wrapping_sub(c);
    a = a.wrapping_sub(b);
    d = d.wrapping_add(c);
    [a, b, c, d]
}

/// Apply a 1D transform of length `n` (4, 8, 16, 32) in place.
#[inline(always)]
fn tx1d_narrow(buf: &mut [i32], n: usize, adst: bool) {
    match (n, adst) {
        (4, false) => {
            if let Some(a) = buf.first_chunk_mut::<4>() {
                *a = narrow::idct4(*a);
            }
        }
        (4, true) => {
            if let Some(a) = buf.first_chunk_mut::<4>() {
                *a = iadst4_narrow(*a);
            }
        }
        (8, false) => {
            if let Some(a) = buf.first_chunk_mut::<8>() {
                *a = narrow::idct8(*a);
            }
        }
        (8, true) => {
            if let Some(a) = buf.first_chunk_mut::<8>() {
                *a = narrow::iadst8(*a);
            }
        }
        (16, false) => {
            if let Some(a) = buf.first_chunk_mut::<16>() {
                *a = narrow::idct16(*a);
            }
        }
        (16, true) => {
            if let Some(a) = buf.first_chunk_mut::<16>() {
                *a = narrow::iadst16(*a);
            }
        }
        _ => {
            if let Some(a) = buf.first_chunk_mut::<32>() {
                *a = narrow::idct32(*a);
            }
        }
    }
}

#[inline(always)]
fn tx1d_wide(buf: &mut [i64], n: usize, adst: bool) {
    match (n, adst) {
        (4, false) => {
            if let Some(a) = buf.first_chunk_mut::<4>() {
                *a = wide::idct4(*a);
            }
        }
        (4, true) => {
            if let Some(a) = buf.first_chunk_mut::<4>() {
                *a = iadst4_wide(*a);
            }
        }
        (8, false) => {
            if let Some(a) = buf.first_chunk_mut::<8>() {
                *a = wide::idct8(*a);
            }
        }
        (8, true) => {
            if let Some(a) = buf.first_chunk_mut::<8>() {
                *a = wide::iadst8(*a);
            }
        }
        (16, false) => {
            if let Some(a) = buf.first_chunk_mut::<16>() {
                *a = wide::idct16(*a);
            }
        }
        (16, true) => {
            if let Some(a) = buf.first_chunk_mut::<16>() {
                *a = wide::iadst16(*a);
            }
        }
        _ => {
            if let Some(a) = buf.first_chunk_mut::<32>() {
                *a = wide::idct32(*a);
            }
        }
    }
}

/// Clamp coefficients so that garbage streams cannot overflow the 32-bit arithmetic (conformant
/// streams keep far smaller values, 8 + BitDepth bits).
#[inline]
fn sat(v: i32, lim: i32) -> i32 {
    v.clamp(-lim, lim)
}

/// Block description for [`inverse_transform_add`].
#[derive(Clone, Copy, Debug)]
pub struct InverseTx {
    pub tx_size: u8,
    pub tx_type: u8,
    pub lossless: bool,
    /// End-of-block position (number of coded coefficients).
    pub eob: usize,
    /// Number of leading rows that can hold non-zero values.
    pub rows: usize,
    pub bit_depth: u8,
}

/// Inverse transform `coefs` (row-major n x n dequantized coefficients, `rows` = number of leading
/// rows that can hold non-zero values) and add the residual to `dst` (8.6.2 step 4).
pub fn inverse_transform_add(coefs: &[i32], tx: &InverseTx, dst: &mut [u16], stride: usize) {
    let InverseTx { tx_size, tx_type, lossless, eob, rows, bit_depth } = *tx;
    let n = 4usize << tx_size;
    let max = (1i32 << bit_depth) - 1;
    if lossless {
        let mut t = [0i32; 16];
        for i in 0..4 {
            let c = coefs.get(i * 4..).and_then(|c| c.first_chunk::<4>()).copied().unwrap_or_default();
            let r = iwht4(c, 2);
            t[i * 4..i * 4 + 4].copy_from_slice(&r);
        }
        for j in 0..4 {
            let r = iwht4([t[j], t[4 + j], t[8 + j], t[12 + j]], 0);
            for i in 0..4 {
                let p = &mut dst[i * stride + j];
                *p = (*p as i32).saturating_add(r[i]).clamp(0, max) as u16;
            }
        }
        return;
    }
    let shift = (tx_size as u32 + 4).min(6);
    let round = 1i32 << (shift - 1);
    if eob == 1 && tx_type == DCT_DCT {
        // Only the DC coefficient is present: every output of both passes is equal (8.7.2 with a
        // single non-zero input).
        let dc = coefs[0] as i64;
        let a = (dc * 11585 + (1 << 13)) >> 14;
        let b = (a * 11585 + (1 << 13)) >> 14;
        let v = ((b + round as i64) >> shift).clamp(-(1 << 20), 1 << 20) as i32;
        for i in 0..n {
            for p in dst[i * stride..i * stride + n].iter_mut() {
                *p = (*p as i32 + v).clamp(0, max) as u16;
            }
        }
        return;
    }
    let row_adst = matches!(tx_type, DCT_ADST | ADST_ADST);
    let col_adst = matches!(tx_type, ADST_DCT | ADST_ADST);
    let p = TxParams { row_adst, col_adst, rows, shift, round, max };
    match (tx_size, bit_depth == 8) {
        (0, true) => itx_narrow::<4>(coefs, &p, dst, stride),
        (1, true) => itx_narrow::<8>(coefs, &p, dst, stride),
        (2, true) => itx_narrow::<16>(coefs, &p, dst, stride),
        (_, true) => itx_narrow::<32>(coefs, &p, dst, stride),
        (0, false) => itx_wide::<4>(coefs, &p, dst, stride),
        (1, false) => itx_wide::<8>(coefs, &p, dst, stride),
        (2, false) => itx_wide::<16>(coefs, &p, dst, stride),
        (_, false) => itx_wide::<32>(coefs, &p, dst, stride),
    }
}

struct TxParams {
    row_adst: bool,
    col_adst: bool,
    rows: usize,
    shift: u32,
    round: i32,
    max: i32,
}

/// 2D inverse transform of size N with 32-bit intermediates (8-bit streams).
fn itx_narrow<const N: usize>(coefs: &[i32], p: &TxParams, dst: &mut [u16], stride: usize) {
    let lim = 1 << 24;
    let mut t = [[0i32; N]; N];
    for (i, row) in t.iter_mut().enumerate().take(p.rows.min(N)) {
        for (d, s) in row.iter_mut().zip(&coefs[i * N..i * N + N]) {
            *d = sat(*s, lim);
        }
        tx1d_narrow(row, N, p.row_adst);
        for v in row.iter_mut() {
            *v = sat(*v, lim);
        }
    }
    // Column pass across 4 / 8 columns at once (the same butterflies on lane vectors), then
    // the rounding, residual add and clipping per row of lanes.
    if N == 4 {
        let x: [Cols<4>; N] = std::array::from_fn(|i| Cols(t[i].first_chunk::<4>().copied().unwrap_or_default()));
        let y = col_tx4(x, p.col_adst);
        add_cols(&y, 0, p, dst, stride);
    } else {
        for c0 in (0..N).step_by(8) {
            let x: [Cols<8>; N] = std::array::from_fn(|i| Cols(t[i].get(c0..).and_then(|r| r.first_chunk::<8>()).copied().unwrap_or_default()));
            let y = col_tx8(x, p.col_adst);
            add_cols(&y, c0, p, dst, stride);
        }
    }
}

/// Round the column pass output `y` (columns c0..c0 + W of every row) and add it to `dst`.
#[inline(always)]
fn add_cols<const N: usize, const W: usize>(y: &[Cols<W>; N], c0: usize, p: &TxParams, dst: &mut [u16], stride: usize) {
    for (i, v) in y.iter().enumerate() {
        let Some(d) = dst.get_mut(i * stride + c0..).and_then(|d| d.first_chunk_mut::<W>()) else {
            return;
        };
        for j in 0..W {
            let r = (v.0[j].wrapping_add(p.round) >> p.shift).clamp(-(1 << 20), 1 << 20);
            d[j] = (d[j] as i32 + r).max(0).min(p.max) as u16;
        }
    }
}

/// `W` columns of 32-bit values processed together: the generated butterflies
/// (`transform_gen::cols4` / `cols8`) run on these lane vectors with the scalar arithmetic of
/// `narrow` applied per lane, so the column pass vectorises.
#[derive(Clone, Copy)]
pub struct Cols<const W: usize>(pub [i32; W]);

impl<const W: usize> From<i32> for Cols<W> {
    #[inline(always)]
    fn from(v: i32) -> Self {
        Cols([v; W])
    }
}

impl<const W: usize> Cols<W> {
    #[inline(always)]
    pub fn wrapping_add<O: Into<Self>>(self, o: O) -> Self {
        let o = o.into();
        Cols(std::array::from_fn(|i| self.0[i].wrapping_add(o.0[i])))
    }
    #[inline(always)]
    pub fn wrapping_sub(self, o: Self) -> Self {
        Cols(std::array::from_fn(|i| self.0[i].wrapping_sub(o.0[i])))
    }
    #[inline(always)]
    pub fn wrapping_mul(self, c: i32) -> Self {
        Cols(self.0.map(|v| v.wrapping_mul(c)))
    }
    #[inline(always)]
    pub fn wrapping_neg(self) -> Self {
        Cols(self.0.map(|v| v.wrapping_neg()))
    }
}

impl<const W: usize> std::ops::Shr<i32> for Cols<W> {
    type Output = Self;
    #[inline(always)]
    fn shr(self, s: i32) -> Self {
        Cols(self.0.map(|v| v >> s))
    }
}

/// `x` (N rows of 4 columns) through the 1D column transform.
#[inline(always)]
fn col_tx4<const N: usize>(x: [Cols<4>; N], adst: bool) -> [Cols<4>; N] {
    use crate::transform_gen::cols4;
    let r = if adst { iadst4_cols(fixed(&x)) } else { cols4::idct4(fixed(&x)) };
    std::array::from_fn(|i| r[i])
}

/// `x` (N rows of 8 columns) through the 1D column transform.
#[inline(always)]
fn col_tx8<const N: usize>(x: [Cols<8>; N], adst: bool) -> [Cols<8>; N] {
    use crate::transform_gen::cols8;
    let mut out = x;
    match (N, adst) {
        (8, false) => out.copy_from_slice(&cols8::idct8(fixed(&x))),
        (8, true) => out.copy_from_slice(&cols8::iadst8(fixed(&x))),
        (16, false) => out.copy_from_slice(&cols8::idct16(fixed(&x))),
        (16, true) => out.copy_from_slice(&cols8::iadst16(fixed(&x))),
        _ => out.copy_from_slice(&cols8::idct32(fixed(&x))),
    }
    out
}

/// A slice of known length as an array.
#[inline(always)]
fn fixed<T: Copy + From<i32>, const M: usize>(x: &[T]) -> [T; M] {
    x.first_chunk::<M>().copied().unwrap_or([T::from(0); M])
}

/// ADST4 (8.7.1.6) on 4 columns.
#[inline(always)]
fn iadst4_cols(x: [Cols<4>; 4]) -> [Cols<4>; 4] {
    let s0 = x[0].wrapping_mul(SINPI_1_9 as i32);
    let s1 = x[0].wrapping_mul(SINPI_2_9 as i32);
    let s2 = x[1].wrapping_mul(SINPI_3_9 as i32);
    let s3 = x[2].wrapping_mul(SINPI_4_9 as i32);
    let s4 = x[2].wrapping_mul(SINPI_1_9 as i32);
    let s5 = x[3].wrapping_mul(SINPI_2_9 as i32);
    let s6 = x[3].wrapping_mul(SINPI_4_9 as i32);
    let v = x[0].wrapping_sub(x[2]).wrapping_add(x[3]);
    let s7 = v.wrapping_mul(SINPI_3_9 as i32);
    let x0 = s0.wrapping_add(s3).wrapping_add(s5);
    let x1 = s1.wrapping_sub(s4).wrapping_sub(s6);
    let x2 = s7;
    let x3 = s2;
    let r = |v: Cols<4>| v.wrapping_add(1 << 13) >> 14;
    [r(x0.wrapping_add(x3)), r(x1.wrapping_add(x3)), r(x2), r(x0.wrapping_add(x1).wrapping_sub(x3))]
}

/// 2D inverse transform of size N with 64-bit intermediates (high bit depth).
fn itx_wide<const N: usize>(coefs: &[i32], p: &TxParams, dst: &mut [u16], stride: usize) {
    let lim = 1i64 << 40;
    let mut t = [[0i64; N]; N];
    for (i, row) in t.iter_mut().enumerate().take(p.rows.min(N)) {
        for (d, s) in row.iter_mut().zip(&coefs[i * N..i * N + N]) {
            *d = *s as i64;
        }
        tx1d_wide(row, N, p.row_adst);
        for v in row.iter_mut() {
            *v = (*v).clamp(-lim, lim);
        }
    }
    let mut col = [0i64; N];
    for j in 0..N {
        for i in 0..N {
            col[i] = t[i][j];
        }
        tx1d_wide(&mut col, N, p.col_adst);
        for i in 0..N {
            let d = &mut dst[i * stride + j];
            let r = ((col[i].clamp(-lim, lim) + p.round as i64) >> p.shift).clamp(-(1 << 30), 1 << 30) as i32;
            *d = (*d as i32 + r).clamp(0, p.max) as u16;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Reference inverse DCT (orthonormal, scaled like VP9: DC gain 1/sqrt(2) per pass times
    /// sqrt(2) ... ) — compare basis functions up to rounding.
    fn float_idct(input: &[f64]) -> Vec<f64> {
        let n = input.len();
        (0..n)
            .map(|x| {
                let mut s = input[0] / 2f64.sqrt();
                for (k, &v) in input.iter().enumerate().skip(1) {
                    s += v * ((2 * x + 1) as f64 * k as f64 * std::f64::consts::PI / (2 * n) as f64).cos();
                }
                s
            })
            .collect()
    }

    #[test]
    fn idct_matches_float_reference() {
        for n in [4usize, 8, 16, 32] {
            for k in 0..n {
                let mut buf = vec![0i32; n];
                buf[k] = 1000;
                tx1d_narrow(&mut buf, n, false);
                let mut inp = vec![0f64; n];
                inp[k] = 1000.0;
                let r = float_idct(&inp);
                for (a, b) in buf.iter().zip(&r) {
                    assert!((*a as f64 - b).abs() <= 3.0, "n {n} k {k}: {buf:?} vs {r:?}");
                }
                let mut w: Vec<i64> = vec![0; n];
                w[k] = 1000;
                tx1d_wide(&mut w, n, false);
                assert_eq!(w.iter().map(|&v| v as i32).collect::<Vec<_>>(), buf);
            }
        }
    }

    #[test]
    fn adst_is_sine_basis() {
        // VP9's ADST (for n = 8, 16) is a DST-IV variant: out[x] ~ sum in[k] sin(pi (2x+1)(2k+1) / 4n).
        for n in [8usize, 16] {
            for k in 0..n {
                let mut buf = vec![0i32; n];
                buf[k] = 1000;
                tx1d_narrow(&mut buf, n, true);
                let r: Vec<f64> = (0..n)
                    .map(|x| 1000.0 * ((2 * x + 1) as f64 * (2 * k + 1) as f64 * std::f64::consts::PI / (4 * n) as f64).sin() * 2f64.sqrt() / 2f64.sqrt())
                    .collect();
                for (a, b) in buf.iter().zip(&r) {
                    assert!((*a as f64 - b).abs() <= 4.0, "n {n} k {k}: {buf:?} vs {r:?}");
                }
            }
        }
        // ADST4 is the sine transform sin(pi (x+1)(2k+1) / 9) scaled.
        for k in 0..4 {
            let mut buf = vec![0i32; 4];
            buf[k] = 1000;
            tx1d_narrow(&mut buf, 4, true);
            let r: Vec<f64> =
                (0..4).map(|x| 1000.0 * (std::f64::consts::PI * (x + 1) as f64 * (2 * k + 1) as f64 / 9.0).sin() * 2.0 * 2f64.sqrt() / 3.0).collect();
            for (a, b) in buf.iter().zip(&r) {
                assert!((*a as f64 - b).abs() <= 3.0, "k {k}: {buf:?} vs {r:?}");
            }
        }
    }

    #[test]
    fn dc_only_shortcut_matches_full_transform() {
        for tx in 0..4u8 {
            let n = 4usize << tx;
            for dc in [-5000, -37, -1, 1, 2, 57, 1234, 9999] {
                let mut coefs = vec![0i32; n * n];
                coefs[0] = dc;
                let mut a = vec![128u16; n * n];
                let mut b = vec![128u16; n * n];
                inverse_transform_add(&coefs, &InverseTx { tx_size: tx, tx_type: DCT_DCT, lossless: false, eob: 1, rows: 1, bit_depth: 8 }, &mut a, n);
                inverse_transform_add(&coefs, &InverseTx { tx_size: tx, tx_type: DCT_DCT, lossless: false, eob: 2, rows: n, bit_depth: 8 }, &mut b, n);
                assert_eq!(a, b, "tx {tx} dc {dc}");
            }
        }
    }

    /// The per-column formulation the vectorised column pass replaced.
    fn itx_narrow_ref<const N: usize>(coefs: &[i32], p: &TxParams, dst: &mut [u16], stride: usize) {
        let lim = 1 << 24;
        let mut t = [[0i32; N]; N];
        for (i, row) in t.iter_mut().enumerate().take(p.rows.min(N)) {
            for (d, s) in row.iter_mut().zip(&coefs[i * N..i * N + N]) {
                *d = sat(*s, lim);
            }
            tx1d_narrow(row, N, p.row_adst);
            for v in row.iter_mut() {
                *v = sat(*v, lim);
            }
        }
        let mut col = [0i32; N];
        for j in 0..N {
            for i in 0..N {
                col[i] = t[i][j];
            }
            tx1d_narrow(&mut col, N, p.col_adst);
            for i in 0..N {
                let d = &mut dst[i * stride + j];
                *d = (*d as i32 + (col[i].wrapping_add(p.round) >> p.shift).clamp(-(1 << 20), 1 << 20)).clamp(0, p.max) as u16;
            }
        }
    }

    fn check_cols<const N: usize>(seed: &mut u32) {
        let mut rnd = || {
            *seed ^= *seed << 13;
            *seed ^= *seed >> 17;
            *seed ^= *seed << 5;
            *seed
        };
        let tx_size = N.trailing_zeros() - 2;
        let shift = (tx_size + 4).min(6);
        for case in 0..200 {
            let (row_adst, col_adst) = if N == 32 { (false, false) } else { (case & 1 == 1, case & 2 == 2) };
            let rows = 1 + rnd() as usize % N;
            let big = if case % 7 == 0 { 1 << 22 } else { 2000 };
            let coefs: Vec<i32> = (0..N * N).map(|i| if i / N < rows && rnd() % 3 == 0 { (rnd() % (2 * big)) as i32 - big as i32 } else { 0 }).collect();
            let p = TxParams { row_adst, col_adst, rows, shift, round: 1 << (shift - 1), max: 255 };
            let stride = N + 5;
            let base: Vec<u16> = (0..N * stride).map(|_| (rnd() % 256) as u16).collect();
            let (mut a, mut b) = (base.clone(), base);
            itx_narrow::<N>(&coefs, &p, &mut a, stride);
            itx_narrow_ref::<N>(&coefs, &p, &mut b, stride);
            assert_eq!(a, b, "N {N} case {case}");
        }
    }

    #[test]
    fn column_pass_matches_per_column_transform() {
        let mut seed = 0x9e37_79b9u32;
        check_cols::<4>(&mut seed);
        check_cols::<8>(&mut seed);
        check_cols::<16>(&mut seed);
        check_cols::<32>(&mut seed);
    }

    #[test]
    fn wht_round_trip_identity() {
        // A single DC coefficient of 4 * v spreads v / 4 ... check invertibility on a simple case:
        // input [4, 0, ..] (after the >> 2 pre-scaling a unit impulse).
        let mut coefs = [0i32; 16];
        coefs[0] = 4 * 4;
        let mut dst = [100u16; 16];
        inverse_transform_add(&coefs, &InverseTx { tx_size: 0, tx_type: DCT_DCT, lossless: true, eob: 1, rows: 4, bit_depth: 8 }, &mut dst, 4);
        let sum: i32 = dst.iter().map(|&v| v as i32 - 100).sum();
        assert_eq!(sum, 16);
    }
}
