//! Inverse transforms (spec 7.13), implemented step by step as specified.

use crate::spec_tables::{
    ADST_ADST, ADST_DCT, ADST_FLIPADST, COS128_LOOKUP, DCT_ADST, DCT_DCT, DCT_FLIPADST, FLIPADST_ADST, FLIPADST_DCT, FLIPADST_FLIPADST, H_ADST, H_DCT,
    H_FLIPADST, TRANSFORM_ROW_SHIFT, TX_HEIGHT_LOG2, TX_WIDTH_LOG2, V_ADST, V_DCT, V_FLIPADST,
};

#[inline(always)]
fn round2_64(x: i64, n: u32) -> i32 {
    if n == 0 { x as i32 } else { ((x + (1i64 << (n - 1))) >> n) as i32 }
}

#[inline(always)]
pub(crate) fn round2(x: i32, n: u32) -> i32 {
    if n == 0 { x } else { (x + (1 << (n - 1))) >> n }
}

/// cos128( angle ) for every `angle & 255`.
static COS128: [i32; 256] = {
    let mut t = [0; 256];
    let mut a = 0;
    while a < 256 {
        t[a] = match a {
            0..=64 => COS128_LOOKUP[a] as i32,
            65..=128 => -(COS128_LOOKUP[128 - a] as i32),
            129..=192 => -(COS128_LOOKUP[a - 128] as i32),
            _ => COS128_LOOKUP[256 - a] as i32,
        };
        a += 1;
    }
    t
};

#[inline(always)]
fn cos128(angle: i32) -> i64 {
    COS128[(angle & 255) as usize] as i64
}

#[inline(always)]
fn sin128(angle: i32) -> i64 {
    cos128(angle - 64)
}

/// One value of the 1D transforms, or `L` independent columns / rows of them processed
/// together (lane-wise, so the steps vectorise).
pub(crate) trait Lane: Copy {
    const ZERO: Self;
    /// (Round2( a * cos - b * sin, 12 ), Round2( a * sin + b * cos, 12 ))
    fn rotate(a: Self, b: Self, cos: i32, sin: i32) -> (Self, Self);
    /// (Clamp( x + y ), Clamp( x - y )) to [lo, hi]
    fn hadamard(x: Self, y: Self, lo: i32, hi: i32) -> (Self, Self);
    fn neg(self) -> Self;
    /// Round2( v * m, 12 )
    fn mul_round12(self, m: i32) -> Self;
    fn mul(self, k: i32) -> Self;
    fn adst4(t: &mut [Self]);
}

impl Lane for i32 {
    const ZERO: i32 = 0;
    #[inline(always)]
    fn rotate(a: i32, b: i32, cos: i32, sin: i32) -> (i32, i32) {
        let (a, b, c, s) = (a as i64, b as i64, cos as i64, sin as i64);
        (round2_64(a * c - b * s, 12), round2_64(a * s + b * c, 12))
    }
    #[inline(always)]
    fn hadamard(x: i32, y: i32, lo: i32, hi: i32) -> (i32, i32) {
        ((x + y).clamp(lo, hi), (x - y).clamp(lo, hi))
    }
    #[inline(always)]
    fn neg(self) -> i32 {
        -self
    }
    #[inline(always)]
    fn mul_round12(self, m: i32) -> i32 {
        round2_64(self as i64 * m as i64, 12)
    }
    #[inline(always)]
    fn mul(self, k: i32) -> i32 {
        self * k
    }
    fn adst4(t: &mut [i32]) {
        inverse_adst4_scalar(t);
    }
}

/// Lanes in 32-bit arithmetic: exact for conforming streams up to 10 bits per sample (the
/// spec requires every intermediate value to fit in 8 + BitDepth <= 18 bits, so products with
/// the 12-bit cosines stay below 2^31). Wrapping operations keep corrupt streams from
/// panicking.
impl<const L: usize> Lane for [i32; L] {
    const ZERO: Self = [0; L];
    #[inline(always)]
    fn rotate(a: Self, b: Self, cos: i32, sin: i32) -> (Self, Self) {
        let mut x = [0; L];
        let mut y = [0; L];
        for l in 0..L {
            let p = a[l].wrapping_mul(cos).wrapping_sub(b[l].wrapping_mul(sin));
            let q = a[l].wrapping_mul(sin).wrapping_add(b[l].wrapping_mul(cos));
            x[l] = p.wrapping_add(1 << 11) >> 12;
            y[l] = q.wrapping_add(1 << 11) >> 12;
        }
        (x, y)
    }
    #[inline(always)]
    fn hadamard(x: Self, y: Self, lo: i32, hi: i32) -> (Self, Self) {
        let mut a = [0; L];
        let mut b = [0; L];
        for l in 0..L {
            a[l] = x[l].wrapping_add(y[l]).max(lo).min(hi);
            b[l] = x[l].wrapping_sub(y[l]).max(lo).min(hi);
        }
        (a, b)
    }
    #[inline(always)]
    fn neg(self) -> Self {
        self.map(|v| v.wrapping_neg())
    }
    #[inline(always)]
    fn mul_round12(self, m: i32) -> Self {
        self.map(|v| v.wrapping_mul(m).wrapping_add(1 << 11) >> 12)
    }
    #[inline(always)]
    fn mul(self, k: i32) -> Self {
        self.map(|v| v.wrapping_mul(k))
    }
    fn adst4(t: &mut [Self]) {
        for l in 0..L {
            let mut v = [t[0][l], t[1][l], t[2][l], t[3][l]];
            inverse_adst4_scalar(&mut v);
            for k in 0..4 {
                t[k][l] = v[k];
            }
        }
    }
}

const fn brev(num_bits: u32, x: usize) -> usize {
    let mut t = 0;
    let mut i = 0;
    while i < num_bits {
        let bit = (x >> i) & 1;
        t += bit << (num_bits - 1 - i);
        i += 1;
    }
    t
}

/// BREV[n][x] = brev( n, x ) for n <= 6.
const BREV: [[u8; 64]; 7] = {
    let mut t = [[0; 64]; 7];
    let mut n = 0;
    while n < 7 {
        let mut x = 0;
        while x < 1 << n {
            t[n][x] = brev(n as u32, x) as u8;
            x += 1;
        }
        n += 1;
    }
    t
};

/// Butterfly rotation B( a, b, angle, flip ).
#[inline(always)]
fn bf<T: Lane>(t: &mut [T], a: usize, b: usize, angle: i32, flip: bool) {
    let (x, y) = T::rotate(t[a], t[b], cos128(angle) as i32, sin128(angle) as i32);
    t[a] = x;
    t[b] = y;
    if flip {
        t.swap(a, b);
    }
}

/// Hadamard rotation H( a, b, flip, r ).
#[inline(always)]
fn hd<T: Lane>(t: &mut [T], a: usize, b: usize, flip: bool, r: u32) {
    let (a, b) = if flip { (b, a) } else { (a, b) };
    let lo = -(1i32 << (r - 1));
    let hi = (1i32 << (r - 1)) - 1;
    let (x, y) = T::hadamard(t[a], t[b], lo, hi);
    t[a] = x;
    t[b] = y;
}

/// Rotations and Hadamards that build the odd half of the larger transforms (first part of 7.13.2.3).
#[inline(always)]
fn dct_stage_outer_rotations<T: Lane, const N: u32>(t: &mut [T], r: u32) {
    if N == 6 {
        for i in 0..16 {
            bf(t, 32 + i, 63 - i, 63 - 4 * BREV[4][i] as i32, false);
        }
    }
    if N >= 5 {
        for i in 0..8 {
            bf(t, 16 + i, 31 - i, 6 + ((BREV[3][7 - i] as i32) << 3), false);
        }
    }
    if N == 6 {
        for i in 0..16 {
            hd(t, 32 + i * 2, 33 + i * 2, i & 1 == 1, r);
        }
    }
    if N >= 4 {
        for i in 0..4 {
            bf(t, 8 + i, 15 - i, 12 + ((BREV[2][3 - i] as i32) << 4), false);
        }
    }
    if N >= 5 {
        for i in 0..8 {
            hd(t, 16 + 2 * i, 17 + 2 * i, i & 1 == 1, r);
        }
    }
    if N == 6 {
        for i in 0..4 {
            for j in 0..2 {
                bf(t, 62 - i * 4 - j, 33 + i * 4 + j, 60 - 16 * BREV[2][i] as i32 + 64 * j as i32, true);
            }
        }
    }
    if N >= 3 {
        for i in 0..2 {
            bf(t, 4 + i, 7 - i, 56 - 32 * i as i32, false);
        }
    }
    if N >= 4 {
        for i in 0..4 {
            hd(t, 8 + 2 * i, 9 + 2 * i, i & 1 == 1, r);
        }
    }
    if N >= 5 {
        for i in 0..2 {
            for j in 0..2 {
                bf(t, 30 - 4 * i - j, 17 + 4 * i + j, 24 + ((j as i32) << 6) + (((1 - i) as i32) << 5), true);
            }
        }
    }
    if N == 6 {
        for i in 0..8 {
            for j in 0..2 {
                hd(t, 32 + i * 4 + j, 35 + i * 4 - j, i & 1 == 1, r);
            }
        }
    }
}

/// Butterflies of the smallest sub-transforms (second part of 7.13.2.3).
#[inline(always)]
fn dct_stage_even_half<T: Lane, const N: u32>(t: &mut [T], r: u32) {
    for i in 0..2 {
        bf(t, 2 * i, 2 * i + 1, 32 + 16 * i as i32, i == 0);
    }
    if N >= 3 {
        for i in 0..2 {
            hd(t, 4 + 2 * i, 5 + 2 * i, i == 1, r);
        }
    }
    if N >= 4 {
        for i in 0..2 {
            bf(t, 14 - i, 9 + i, 48 + 64 * i as i32, true);
        }
    }
    if N >= 5 {
        for i in 0..4 {
            for j in 0..2 {
                hd(t, 16 + 4 * i + j, 19 + 4 * i - j, i & 1 == 1, r);
            }
        }
    }
    if N == 6 {
        for i in 0..2 {
            for j in 0..4 {
                bf(t, 61 - i * 8 - j, 34 + i * 8 + j, 56 - i as i32 * 32 + (j as i32 >> 1) * 64, true);
            }
        }
    }
}

/// Merges the quarter-size sub-transforms (third part of 7.13.2.3).
#[inline(always)]
fn dct_stage_merge_quarters<T: Lane, const N: u32>(t: &mut [T], r: u32) {
    for i in 0..2 {
        hd(t, i, 3 - i, false, r);
    }
    if N >= 3 {
        bf(t, 6, 5, 32, true);
    }
    if N >= 4 {
        for i in 0..2 {
            for j in 0..2 {
                hd(t, 8 + 4 * i + j, 11 + 4 * i - j, i == 1, r);
            }
        }
    }
    if N >= 5 {
        for i in 0..4 {
            bf(t, 29 - i, 18 + i, 48 + (i as i32 >> 1) * 64, true);
        }
    }
    if N == 6 {
        for i in 0..4 {
            for j in 0..4 {
                hd(t, 32 + 8 * i + j, 39 + 8 * i - j, i & 1 == 1, r);
            }
        }
    }
    if N >= 3 {
        for i in 0..4 {
            hd(t, i, 7 - i, false, r);
        }
    }
    if N >= 4 {
        for i in 0..2 {
            bf(t, 13 - i, 10 + i, 32, true);
        }
    }
    if N >= 5 {
        for i in 0..2 {
            for j in 0..4 {
                hd(t, 16 + i * 8 + j, 23 + i * 8 - j, i == 1, r);
            }
        }
    }
    if N == 6 {
        for i in 0..8 {
            bf(t, 59 - i, 36 + i, if i < 4 { 48 } else { 112 }, true);
        }
    }
}

/// Final merges of the half-size sub-transforms (last part of 7.13.2.3).
#[inline(always)]
fn dct_stage_merge_halves<T: Lane, const N: u32>(t: &mut [T], r: u32) {
    if N >= 4 {
        for i in 0..8 {
            hd(t, i, 15 - i, false, r);
        }
    }
    if N >= 5 {
        for i in 0..4 {
            bf(t, 27 - i, 20 + i, 32, true);
        }
    }
    if N == 6 {
        for i in 0..8 {
            hd(t, 32 + i, 47 - i, false, r);
            hd(t, 48 + i, 63 - i, true, r);
        }
    }
    if N >= 5 {
        for i in 0..16 {
            hd(t, i, 31 - i, false, r);
        }
    }
    if N == 6 {
        for i in 0..8 {
            bf(t, 55 - i, 40 + i, 32, true);
        }
    }
    if N == 6 {
        for i in 0..32 {
            hd(t, i, 63 - i, false, r);
        }
    }
}

/// Inverse DCT of length 2^n (7.13.2.3), in place.
pub(crate) fn inverse_dct<T: Lane>(t: &mut [T], n: u32, r: u32) {
    match n {
        2 => inverse_dct_n::<T, 2>(t, r),
        3 => inverse_dct_n::<T, 3>(t, r),
        4 => inverse_dct_n::<T, 4>(t, r),
        5 => inverse_dct_n::<T, 5>(t, r),
        _ => inverse_dct_n::<T, 6>(t, r),
    }
}

fn inverse_dct_n<T: Lane, const N: u32>(t: &mut [T], r: u32) {
    // 7.13.2.2 permutation
    let len = 1usize << N;
    let mut copy = [T::ZERO; 64];
    copy[..len].copy_from_slice(&t[..len]);
    for i in 0..len {
        t[i] = copy[BREV[N as usize][i] as usize];
    }
    dct_stage_outer_rotations::<T, N>(t, r);
    dct_stage_even_half::<T, N>(t, r);
    dct_stage_merge_quarters::<T, N>(t, r);
    dct_stage_merge_halves::<T, N>(t, r);
}

const SINPI_1_9: i64 = 1321;
const SINPI_2_9: i64 = 2482;
const SINPI_3_9: i64 = 3344;
const SINPI_4_9: i64 = 3803;

fn inverse_adst4_scalar(t: &mut [i32]) {
    let mut s = [0i64; 7];
    let mut x = [0i64; 4];
    let t0 = t[0] as i64;
    let t1 = t[1] as i64;
    let t2 = t[2] as i64;
    let t3 = t[3] as i64;
    s[0] = SINPI_1_9 * t0;
    s[1] = SINPI_2_9 * t0;
    s[2] = SINPI_3_9 * t1;
    s[3] = SINPI_4_9 * t2;
    s[4] = SINPI_1_9 * t2;
    s[5] = SINPI_2_9 * t3;
    s[6] = SINPI_4_9 * t3;
    let a7 = t0 - t2;
    let b7 = a7 + t3;
    s[0] += s[3];
    s[1] -= s[4];
    s[3] = s[2];
    s[2] = SINPI_3_9 * b7;
    s[0] += s[5];
    s[1] -= s[6];
    x[0] = s[0] + s[3];
    x[1] = s[1] + s[3];
    x[2] = s[2];
    x[3] = s[0] + s[1];
    x[3] -= s[3];
    for i in 0..4 {
        t[i] = round2_64(x[i], 12);
    }
}

fn adst_in_permute<T: Lane>(t: &mut [T], n: u32) {
    let n0 = 1usize << n;
    let mut copy = [T::ZERO; 16];
    copy[..n0].copy_from_slice(&t[..n0]);
    for i in 0..n0 {
        let idx = if i & 1 == 1 { i - 1 } else { n0 - i - 1 };
        t[i] = copy[idx];
    }
}

fn adst_out_permute<T: Lane>(t: &mut [T], n: u32) {
    let n0 = 1usize << n;
    let mut copy = [T::ZERO; 16];
    copy[..n0].copy_from_slice(&t[..n0]);
    for i in 0..n0 {
        let a = (i >> 3) & 1;
        let b = ((i >> 2) & 1) ^ ((i >> 3) & 1);
        let c = ((i >> 1) & 1) ^ ((i >> 2) & 1);
        let d = (i & 1) ^ ((i >> 1) & 1);
        let idx = ((d << 3) | (c << 2) | (b << 1) | a) >> (4 - n);
        t[i] = if i & 1 == 1 { copy[idx].neg() } else { copy[idx] };
    }
}

fn inverse_adst8<T: Lane>(t: &mut [T], r: u32) {
    adst_in_permute(t, 3);
    for i in 0..4 {
        bf(t, 2 * i, 2 * i + 1, 60 - 16 * i as i32, true);
    }
    for i in 0..4 {
        hd(t, i, 4 + i, false, r);
    }
    for i in 0..2 {
        bf(t, 4 + 3 * i, 5 + i, 48 - 32 * i as i32, true);
    }
    for i in 0..2 {
        for j in 0..2 {
            hd(t, 4 * j + i, 2 + 4 * j + i, false, r);
        }
    }
    for i in 0..2 {
        bf(t, 2 + 4 * i, 3 + 4 * i, 32, true);
    }
    adst_out_permute(t, 3);
}

fn inverse_adst16<T: Lane>(t: &mut [T], r: u32) {
    adst_in_permute(t, 4);
    for i in 0..8 {
        bf(t, 2 * i, 2 * i + 1, 62 - 8 * i as i32, true);
    }
    for i in 0..8 {
        hd(t, i, 8 + i, false, r);
    }
    for i in 0..2 {
        bf(t, 8 + 2 * i, 9 + 2 * i, 56 - 32 * i as i32, true);
        bf(t, 13 + 2 * i, 12 + 2 * i, 8 + 32 * i as i32, true);
    }
    for i in 0..4 {
        for j in 0..2 {
            hd(t, 8 * j + i, 4 + 8 * j + i, false, r);
        }
    }
    for i in 0..2 {
        for j in 0..2 {
            bf(t, 4 + 8 * j + 3 * i, 5 + 8 * j + i, 48 - 32 * i as i32, true);
        }
    }
    for i in 0..2 {
        for j in 0..4 {
            hd(t, 4 * j + i, 2 + 4 * j + i, false, r);
        }
    }
    for i in 0..4 {
        bf(t, 2 + 4 * i, 3 + 4 * i, 32, true);
    }
    adst_out_permute(t, 4);
}

fn inverse_adst<T: Lane>(t: &mut [T], n: u32, r: u32) {
    match n {
        2 => T::adst4(t),
        3 => inverse_adst8(t, r),
        _ => inverse_adst16(t, r),
    }
}

fn inverse_identity<T: Lane>(t: &mut [T], n: u32) {
    match n {
        2 => {
            for v in t[..4].iter_mut() {
                *v = v.mul_round12(5793);
            }
        }
        3 => {
            for v in t[..8].iter_mut() {
                *v = v.mul(2);
            }
        }
        4 => {
            for v in t[..16].iter_mut() {
                *v = v.mul_round12(11586);
            }
        }
        _ => {
            for v in t[..32].iter_mut() {
                *v = v.mul(4);
            }
        }
    }
}

fn inverse_wht(t: &mut [i32], shift: u32) {
    let mut a = t[0] >> shift;
    let mut c = t[1] >> shift;
    let mut d = t[2] >> shift;
    let mut b = t[3] >> shift;
    a += c;
    d -= b;
    let e = (a - d) >> 1;
    b = e - b;
    c = e - c;
    a -= b;
    d += c;
    t[0] = a;
    t[1] = b;
    t[2] = c;
    t[3] = d;
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind1d {
    Dct,
    Adst,
    Identity,
}

fn row_kind(tx_type: usize) -> Kind1d {
    match tx_type {
        DCT_DCT | ADST_DCT | FLIPADST_DCT | H_DCT => Kind1d::Dct,
        DCT_ADST | ADST_ADST | DCT_FLIPADST | FLIPADST_FLIPADST | ADST_FLIPADST | FLIPADST_ADST | H_ADST | H_FLIPADST => Kind1d::Adst,
        _ => Kind1d::Identity,
    }
}

fn col_kind(tx_type: usize) -> Kind1d {
    match tx_type {
        DCT_DCT | DCT_ADST | DCT_FLIPADST | V_DCT => Kind1d::Dct,
        ADST_DCT | ADST_ADST | FLIPADST_DCT | FLIPADST_FLIPADST | ADST_FLIPADST | FLIPADST_ADST | V_ADST | V_FLIPADST => Kind1d::Adst,
        _ => Kind1d::Identity,
    }
}

/// 2D inverse transform (7.13.3). `dequant` is 64x64 (row stride 64, only the top-left 32x32
/// may be non-zero; with `dc_only` only the first value); the residual is written to
/// `residual` with row stride `w`.
pub(crate) fn inverse_transform_2d(dequant: &[i32], residual: &mut [i32], tx_sz: usize, tx_type: usize, lossless: bool, bit_depth: u32, dc_only: bool) {
    let log2w = TX_WIDTH_LOG2[tx_sz] as u32;
    let log2h = TX_HEIGHT_LOG2[tx_sz] as u32;
    let w = 1usize << log2w;
    let h = 1usize << log2h;
    let row_shift = if lossless { 0 } else { TRANSFORM_ROW_SHIFT[tx_sz] as u32 };
    let col_shift = if lossless { 0 } else { 4 };
    let row_clamp = bit_depth + 8;
    let col_clamp = (bit_depth + 6).max(16);
    let rk = row_kind(tx_type);
    let ck = col_kind(tx_type);
    let mut t = [0i32; 64];
    let rect2 = log2w.abs_diff(log2h) == 1;
    if !lossless && bit_depth <= 10 {
        let p = Tx2d { w, h, log2w, log2h, row_shift, col_shift, row_clamp, col_clamp, rk, ck, rect2 };
        if dc_only && rk == Kind1d::Dct && ck == Kind1d::Dct && transform_dc_only(dequant[0], residual, &p) {
            return;
        }
        if w >= 8 && h >= 8 {
            transform_lanes::<8>(dequant, residual, &p);
        } else {
            transform_lanes::<4>(dequant, residual, &p);
        }
        return;
    }
    let in_w = w.min(32);
    for i in 0..h {
        // Every 1D transform maps an all-zero row to zeros (each step is linear with
        // Round2( 0 ) = 0), so rows without coefficients (including rows 32..63) are zero.
        if i >= 32 || dequant[i * 64..i * 64 + in_w].iter().all(|&v| v == 0) {
            residual[i * w..i * w + w].fill(0);
            continue;
        }
        t[..in_w].copy_from_slice(&dequant[i * 64..i * 64 + in_w]);
        t[in_w..w].fill(0);
        if rect2 {
            for v in t[..w].iter_mut() {
                *v = round2_64(*v as i64 * 2896, 12);
            }
        }
        if lossless {
            inverse_wht(&mut t, 2);
        } else {
            match rk {
                Kind1d::Dct => inverse_dct(&mut t, log2w, row_clamp),
                Kind1d::Adst => inverse_adst(&mut t, log2w, row_clamp),
                Kind1d::Identity => inverse_identity(&mut t, log2w),
            }
        }
        let lo = -(1i32 << (col_clamp - 1));
        let hi = (1i32 << (col_clamp - 1)) - 1;
        for j in 0..w {
            residual[i * w + j] = round2(t[j], row_shift).clamp(lo, hi);
        }
    }
    for j in 0..w {
        for i in 0..h {
            t[i] = residual[i * w + j];
        }
        if lossless {
            inverse_wht(&mut t, 0);
        } else {
            match ck {
                Kind1d::Dct => inverse_dct(&mut t, log2h, col_clamp),
                Kind1d::Adst => inverse_adst(&mut t, log2h, col_clamp),
                Kind1d::Identity => inverse_identity(&mut t, log2h),
            }
        }
        for i in 0..h {
            residual[i * w + j] = round2(t[i], col_shift);
        }
    }
}

struct Tx2d {
    w: usize,
    h: usize,
    log2w: u32,
    log2h: u32,
    row_shift: u32,
    col_shift: u32,
    row_clamp: u32,
    col_clamp: u32,
    rk: Kind1d,
    ck: Kind1d,
    rect2: bool,
}

#[inline(always)]
fn transform_1d<T: Lane>(t: &mut [T], kind: Kind1d, n: u32, r: u32) {
    match kind {
        Kind1d::Dct => inverse_dct(t, n, r),
        Kind1d::Adst => inverse_adst(t, n, r),
        Kind1d::Identity => inverse_identity(t, n),
    }
}

/// [`transform_lanes`] for a DCT_DCT block whose only coefficient is DC: the other rows are
/// zero and every column gets the same input, so one row and one column are transformed and
/// the column is replicated. Returns false (nothing written) if the row output is not flat.
fn transform_dc_only(dc: i32, residual: &mut [i32], p: &Tx2d) -> bool {
    let (w, h) = (p.w, p.h);
    let lo = -(1i32 << (p.col_clamp - 1));
    let hi = (1i32 << (p.col_clamp - 1)) - 1;
    let mut t = [[0i32; 1]; 64];
    t[0] = [dc];
    if p.rect2 {
        t[0] = t[0].mul_round12(2896);
    }
    inverse_dct(&mut t[..], p.log2w, p.row_clamp);
    let v = round2(t[0][0], p.row_shift).clamp(lo, hi);
    if t[1..w].iter().any(|x| round2(x[0], p.row_shift).clamp(lo, hi) != v) {
        return false;
    }
    let mut c = [[0i32; 1]; 64];
    c[0] = [v];
    inverse_dct(&mut c[..], p.log2h, p.col_clamp);
    for i in 0..h {
        residual[i * w..i * w + w].fill(round2(c[i][0], p.col_shift));
    }
    true
}

/// The 2D inverse transform with `L` rows (row pass) or columns (column pass) at a time;
/// bit-identical to the one-at-a-time process for bit depths up to 10.
fn transform_lanes<const L: usize>(dequant: &[i32], residual: &mut [i32], p: &Tx2d) {
    let (w, h) = (p.w, p.h);
    let in_w = w.min(32);
    let lo = -(1i32 << (p.col_clamp - 1));
    let hi = (1i32 << (p.col_clamp - 1)) - 1;
    let mut t = [[0i32; L]; 64];
    let mut i0 = 0;
    while i0 < h {
        // Rows without coefficients (and rows 32..63) transform to zeros.
        if i0 >= 32 || (i0..i0 + L).all(|i| dequant[i * 64..i * 64 + in_w].iter().all(|&v| v == 0)) {
            residual[i0 * w..(i0 + L) * w].fill(0);
            i0 += L;
            continue;
        }
        for (j, tj) in t[..w].iter_mut().enumerate() {
            for l in 0..L {
                tj[l] = if j < in_w { dequant[(i0 + l) * 64 + j] } else { 0 };
            }
        }
        if p.rect2 {
            for v in t[..w].iter_mut() {
                *v = v.mul_round12(2896);
            }
        }
        transform_1d(&mut t[..], p.rk, p.log2w, p.row_clamp);
        for l in 0..L {
            let out = &mut residual[(i0 + l) * w..(i0 + l + 1) * w];
            for j in 0..w {
                out[j] = round2(t[j][l], p.row_shift).clamp(lo, hi);
            }
        }
        i0 += L;
    }
    let mut j0 = 0;
    while j0 < w {
        for i in 0..h {
            t[i].copy_from_slice(&residual[i * w + j0..i * w + j0 + L]);
        }
        transform_1d(&mut t[..], p.ck, p.log2h, p.col_clamp);
        for i in 0..h {
            let out = &mut residual[i * w + j0..i * w + j0 + L];
            for l in 0..L {
                out[l] = round2(t[i][l], p.col_shift);
            }
        }
        j0 += L;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::spec_tables::{TX_HEIGHT, TX_SIZES_ALL, TX_WIDTH};

    /// The DCT step list must implement an orthogonal transform: a DC-only input gives a flat
    /// output equal to DC * cos(pi/4) (in the 2^12 fixed-point scale).
    #[test]
    fn dct_dc_is_flat() {
        for n in 2..=6 {
            let len = 1 << n;
            let mut t = [0i32; 64];
            t[0] = 4096;
            inverse_dct(&mut t, n, 24);
            for v in &t[..len] {
                assert_eq!(*v, 2896, "n={n}: {:?}", &t[..len]);
            }
        }
    }

    /// The DC-only shortcut gives the same residual as the full 2D transform.
    #[test]
    fn dc_only_matches_full_transform() {
        for tx_sz in 0..TX_SIZES_ALL {
            let (w, h) = (TX_WIDTH[tx_sz] as usize, TX_HEIGHT[tx_sz] as usize);
            for bd in [8, 10] {
                let lim = 1i32 << (7 + bd);
                for dc in [1, -1, 37, -1000, 4096, lim - 1, -lim] {
                    let mut dequant = vec![0; 64 * 64];
                    dequant[0] = dc;
                    let mut full = vec![0; 64 * 64];
                    let mut fast = vec![1; 64 * 64];
                    inverse_transform_2d(&dequant, &mut full, tx_sz, DCT_DCT, false, bd, false);
                    inverse_transform_2d(&dequant, &mut fast, tx_sz, DCT_DCT, false, bd, true);
                    assert_eq!(full[..w * h], fast[..w * h], "tx_sz {tx_sz} bd {bd} dc {dc}");
                }
            }
        }
    }

    /// Compare each inverse DCT basis vector with the real-valued DCT-III.
    #[test]
    fn dct_matches_float() {
        for n in 2..=6u32 {
            let len = 1usize << n;
            for k in 0..len {
                let mut t = [0i32; 64];
                t[k] = 1 << 12;
                inverse_dct(&mut t, n, 24);
                for x in 0..len {
                    let c = if k == 0 { std::f64::consts::FRAC_1_SQRT_2 } else { 1.0 };
                    let want = 4096.0 * c * ((2 * x + 1) as f64 * k as f64 * std::f64::consts::PI / (2 * len) as f64).cos();
                    assert!((t[x] as f64 - want).abs() < 4.0 + 0.5 * n as f64, "n={n} k={k} x={x}: {} vs {want}", t[x]);
                }
            }
        }
    }

    /// ADST8/16 basis vectors follow sin(pi (2i+1)(2k+1) / 4N) (scaled like the DCT).
    #[test]
    fn adst_basis_matches_sine() {
        for n in 3..=4u32 {
            let len = 1usize << n;
            for k in 0..len {
                let mut t = [0i32; 64];
                t[k] = 1 << 12;
                inverse_adst(&mut t, n, 24);
                for i in 0..len {
                    let want = 4096.0 * ((std::f64::consts::PI * (2 * i + 1) as f64 * (2 * k + 1) as f64) / (4 * len) as f64).sin();
                    assert!((t[i] as f64 - want).abs() < 8.0, "n={n} k={k} i={i}: {} vs {want:.1}", t[i]);
                }
            }
        }
    }

    #[test]
    fn adst_is_close_to_sine_transform() {
        for n in 3..=4u32 {
            let len = 1usize << n;
            for k in 0..len {
                let mut t = [0i32; 64];
                t[k] = 1 << 12;
                inverse_adst(&mut t, n, 24);
                let energy: f64 = t[..len].iter().map(|&v| (v as f64 / 4096.0).powi(2)).sum();
                // like the DCT, the AV1 ADST is scaled by sqrt(N / 2)
                assert!((energy - len as f64 / 2.0).abs() < 0.02, "n={n} k={k} energy {energy}");
            }
        }
    }
}
