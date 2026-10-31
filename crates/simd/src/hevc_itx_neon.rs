//! NEON kernels for the HEVC inverse transforms (8.6.4.2: DCT 4..32, DST-VII 4x4) of 8-bit
//! pictures, with the reconstruction (8.6.7) fused into the last pass.
//!
//! The first (vertical) pass runs down strips of 8 coefficient columns (4 for 4x4 blocks), one
//! column per lane: each output row is the sum of the basis entries times the non-zero coefficient
//! rows, in 32-bit lanes, narrowed with `vqrshrn` by 7, which is the specification's
//! Clip3(coeffMin, coeffMax, (x + 64) >> 7) into 16-bit lanes. The second (horizontal) pass sums
//! each row's basis rows scaled by that row's values, narrows with `vqrshrn` by 12 (bdShift for
//! 8-bit samples), adds the prediction with a saturating add and stores with `vqmovun`: residuals
//! beyond 16 bits saturate there, which cannot change a sample, since anything past +-32767
//! clips it to 0 or 255 either way. Coefficient columns past the last non-zero one and rows past
//! the last non-zero one cost nothing.

use core::arch::aarch64::*;

use crate::hevc_itx::{DCT, DST};

/// See [`crate::hevc_idct_add8`]: false (nothing written) for an unsupported size or arguments
/// that do not fit.
pub(crate) fn idct_add(coefs: &[i32], n: usize, dst_type: bool, max_x: usize, max_y: usize, dst: &mut [u8], ds: usize) -> bool {
    if !matches!(n, 4 | 8 | 16 | 32) || max_x >= n || max_y >= n || (dst_type && n != 4) || coefs.len() < n * n {
        return false;
    }
    if !(n - 1).checked_mul(ds).and_then(|v| v.checked_add(n)).is_some_and(|v| v <= dst.len()) {
        return false;
    }
    // SAFETY: NEON is part of the aarch64 baseline: every core this build runs on has it.
    unsafe {
        match (n, dst_type) {
            (4, true) => block::<4, true>(coefs, max_x, max_y, dst, ds),
            (4, false) => block::<4, false>(coefs, max_x, max_y, dst, ds),
            (8, _) => block::<8, false>(coefs, max_x, max_y, dst, ds),
            (16, _) => block::<16, false>(coefs, max_x, max_y, dst, ds),
            _ => block::<32, false>(coefs, max_x, max_y, dst, ds),
        }
    }
    true
}

/// See [`crate::hevc_dc_add8`].
pub(crate) fn dc_add(r: i32, n: usize, dst: &mut [u8], ds: usize) -> bool {
    if !matches!(n, 4 | 8 | 16 | 32) || !(n - 1).checked_mul(ds).and_then(|v| v.checked_add(n)).is_some_and(|v| v <= dst.len()) {
        return false;
    }
    // SAFETY: NEON is part of the aarch64 baseline: every core this build runs on has it.
    unsafe { dc(r.clamp(-32768, 32767) as i16, n, dst, ds) };
    true
}

#[target_feature(enable = "neon")]
fn dc(r: i16, n: usize, dst: &mut [u8], ds: usize) {
    let v = vdupq_n_s16(r);
    for y in 0..n {
        let o = y * ds;
        let mut x = 0;
        while x < n {
            if n == 4 {
                add_store4(dst, o, v);
            } else {
                add_store8(dst, o + x, v);
            }
            x += 8;
        }
    }
}

/// The basis value of coefficient row `j` at sample `k` of an N-point transform.
#[inline(always)]
fn basis<const N: usize, const DT: bool>(j: usize, k: usize) -> i16 {
    if DT { DST[j][k] } else { DCT[j * (32 / N)][k] }
}

/// 8 (or 4, for N = 4) consecutive basis values of row `j` from sample `k`.
#[target_feature(enable = "neon")]
fn basis_row<const N: usize, const DT: bool>(j: usize, k: usize) -> int16x8_t {
    let row: &[i16] = if DT { &DST[j][..] } else { &DCT[j * (32 / N)][..] };
    debug_assert!(k + N.min(8) <= row.len());
    // SAFETY: the rows hold 32 (DCT) or 4 (DST) values and k + 8 <= N <= 32 (k + 4 <= 4 for
    // the 4-point ones, of which 4 are read).
    unsafe { if N == 4 { vcombine_s16(vld1_s16(row.as_ptr().add(k)), vdup_n_s16(0)) } else { vld1q_s16(row.as_ptr().add(k)) } }
}

/// 8 (or 4) coefficients of row `j` from column `x`, saturated to 16 bits.
#[target_feature(enable = "neon")]
fn coef_row<const N: usize>(c: &[i32], j: usize, x: usize) -> int16x8_t {
    let o = j * N + x;
    debug_assert!(o + N.min(8) <= c.len());
    // SAFETY: `idct_add` checked c.len() >= N * N; j < N and x + 8 <= N (x + 4 <= 4 for N = 4).
    unsafe {
        let lo = vqmovn_s32(vld1q_s32(c.as_ptr().add(o)));
        if N == 4 { vcombine_s16(lo, vdup_n_s16(0)) } else { vcombine_s16(lo, vqmovn_s32(vld1q_s32(c.as_ptr().add(o + 4)))) }
    }
}

/// `dst[o..o + 8] = sat8(dst + v)`.
#[target_feature(enable = "neon")]
fn add_store8(dst: &mut [u8], o: usize, v: int16x8_t) {
    debug_assert!(o + 8 <= dst.len());
    // SAFETY: the callers checked (N - 1) * ds + N <= dst.len(); o + 8 <= y * ds + N.
    unsafe {
        let p = vreinterpretq_s16_u16(vmovl_u8(vld1_u8(dst.as_ptr().add(o))));
        vst1_u8(dst.as_mut_ptr().add(o), vqmovun_s16(vqaddq_s16(p, v)));
    }
}

/// [`add_store8`] of the first 4 lanes.
#[target_feature(enable = "neon")]
fn add_store4(dst: &mut [u8], o: usize, v: int16x8_t) {
    let Some(d) = dst.get_mut(o..o + 4) else { return };
    let mut t = [0u8; 8];
    t[..4].copy_from_slice(d);
    // SAFETY: `t` is the 8 bytes `vld1_u8` reads and `vst1_u8` writes.
    unsafe {
        let p = vreinterpretq_s16_u16(vmovl_u8(vld1_u8(t.as_ptr())));
        vst1_u8(t.as_mut_ptr(), vqmovun_s16(vqaddq_s16(p, v)));
    }
    d.copy_from_slice(&t[..4]);
}

/// The two 32-bit halves of `acc + b * s`.
#[target_feature(enable = "neon")]
fn mla(acc: (int32x4_t, int32x4_t), b: int16x8_t, s: i16) -> (int32x4_t, int32x4_t) {
    (vmlal_n_s16(acc.0, vget_low_s16(b), s), vmlal_high_n_s16(acc.1, b, s))
}

#[target_feature(enable = "neon")]
fn block<const N: usize, const DT: bool>(c: &[i32], max_x: usize, max_y: usize, dst: &mut [u8], ds: usize) {
    // Lanes per vector: 4 for 4-point transforms, else 8.
    let l = N.min(8);
    // First pass into t[y][x] (columns past the last non-zero one stay zero).
    let mut t = [[0i16; N]; N];
    let zero = (vdupq_n_s32(0), vdupq_n_s32(0));
    let cols = (max_x + 1).next_multiple_of(l);
    let mut rows = [vdupq_n_s16(0); 32];
    let mut x = 0;
    while x < cols {
        for (j, r) in rows[..=max_y].iter_mut().enumerate() {
            *r = coef_row::<N>(c, j, x);
        }
        for y in 0..N {
            let mut acc = zero;
            for (j, &r) in rows[..=max_y].iter().enumerate() {
                acc = mla(acc, r, basis::<N, DT>(j, y));
            }
            let v = vcombine_s16(vqrshrn_n_s32::<7>(acc.0), vqrshrn_n_s32::<7>(acc.1));
            // SAFETY: x + l <= cols <= N = t[y].len(); a 4-point row stores its first 4 lanes.
            unsafe {
                if N == 4 {
                    vst1_s16(t[y].as_mut_ptr().add(x), vget_low_s16(v));
                } else {
                    vst1q_s16(t[y].as_mut_ptr().add(x), v);
                }
            }
        }
        x += l;
    }
    // Second pass: each row of t times the basis rows, added to the prediction.
    for y in 0..N {
        let g = &t[y];
        let mut x = 0;
        while x < N {
            let mut acc = zero;
            for (j, &s) in g[..=max_x].iter().enumerate() {
                acc = mla(acc, basis_row::<N, DT>(j, x), s);
            }
            let v = vcombine_s16(vqrshrn_n_s32::<12>(acc.0), vqrshrn_n_s32::<12>(acc.1));
            if N == 4 {
                add_store4(dst, y * ds, v);
            } else {
                add_store8(dst, y * ds + x, v);
            }
            x += l;
        }
    }
}
