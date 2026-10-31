//! NEON transposes between the AV1 inverse transform's row pass and its neighbours: 8 rows of
//! dequantised coefficients into the pass's lanes (one vector per column), and the pass's lanes
//! back into rows, 8x8 blocks at a time in registers (`vtrn` of halfwords, words and
//! doublewords).

use core::arch::aarch64::*;

/// See [`crate::av1_rows_to_lanes`].
pub(crate) fn rows_to_lanes(src: &[i32], stride: usize, cols: usize, out: &mut [[i16; 8]]) -> bool {
    let src_ok = stride >= cols && 7usize.checked_mul(stride).and_then(|n| n.checked_add(cols)).is_some_and(|n| n <= src.len());
    if !cols.is_multiple_of(8) || out.len() < cols || !src_ok {
        return false;
    }
    // SAFETY: NEON is part of the aarch64 baseline: every core this build runs on has it.
    unsafe { rows_to_lanes_v(src, stride, cols, out) };
    true
}

/// See [`crate::av1_lanes_to_rows`].
pub(crate) fn lanes_to_rows(t: &[[i16; 8]], cols: usize, shift: u32, dst: &mut [i32], stride: usize) -> bool {
    let dst_ok = stride >= cols && 7usize.checked_mul(stride).and_then(|n| n.checked_add(cols)).is_some_and(|n| n <= dst.len());
    if !cols.is_multiple_of(8) || t.len() < cols || !dst_ok || shift > 16 {
        return false;
    }
    // SAFETY: NEON is part of the aarch64 baseline: every core this build runs on has it.
    unsafe { lanes_to_rows_v(t, cols, shift, dst, stride) };
    true
}

/// 8 rows of 8 halfwords to 8 columns.
#[inline]
#[target_feature(enable = "neon")]
fn transpose(r: [int16x8_t; 8]) -> [int16x8_t; 8] {
    let a = [vtrn1q_s16(r[0], r[1]), vtrn2q_s16(r[0], r[1]), vtrn1q_s16(r[2], r[3]), vtrn2q_s16(r[2], r[3])];
    let b = [vtrn1q_s16(r[4], r[5]), vtrn2q_s16(r[4], r[5]), vtrn1q_s16(r[6], r[7]), vtrn2q_s16(r[6], r[7])];
    let w = vreinterpretq_s32_s16;
    let c = [vtrn1q_s32(w(a[0]), w(a[2])), vtrn1q_s32(w(a[1]), w(a[3])), vtrn2q_s32(w(a[0]), w(a[2])), vtrn2q_s32(w(a[1]), w(a[3]))];
    let d = [vtrn1q_s32(w(b[0]), w(b[2])), vtrn1q_s32(w(b[1]), w(b[3])), vtrn2q_s32(w(b[0]), w(b[2])), vtrn2q_s32(w(b[1]), w(b[3]))];
    let (q, s) = (vreinterpretq_s64_s32, vreinterpretq_s16_s64);
    let lo = |x: int32x4_t, y: int32x4_t| s(vtrn1q_s64(q(x), q(y)));
    let hi = |x: int32x4_t, y: int32x4_t| s(vtrn2q_s64(q(x), q(y)));
    [lo(c[0], d[0]), lo(c[1], d[1]), lo(c[2], d[2]), lo(c[3], d[3]), hi(c[0], d[0]), hi(c[1], d[1]), hi(c[2], d[2]), hi(c[3], d[3])]
}

#[target_feature(enable = "neon")]
fn rows_to_lanes_v(src: &[i32], stride: usize, cols: usize, out: &mut [[i16; 8]]) {
    for j0 in (0..cols).step_by(8) {
        let rows: [int16x8_t; 8] = std::array::from_fn(|l| {
            let o = l * stride + j0;
            debug_assert!(o + 8 <= src.len());
            // SAFETY: o + 8 <= 7 * stride + cols <= src.len() (checked by `rows_to_lanes`).
            let (a, b) = unsafe { (vld1q_s32(src.as_ptr().add(o)), vld1q_s32(src.as_ptr().add(o + 4))) };
            vcombine_s16(vmovn_s32(a), vmovn_s32(b))
        });
        for (k, v) in transpose(rows).into_iter().enumerate() {
            out[j0 + k] = bytemuck::must_cast(v);
        }
    }
}

#[target_feature(enable = "neon")]
fn lanes_to_rows_v(t: &[[i16; 8]], cols: usize, shift: u32, dst: &mut [i32], stride: usize) {
    let sh = vdupq_n_s32(-(shift as i32));
    for j0 in (0..cols).step_by(8) {
        let lanes: [int16x8_t; 8] = std::array::from_fn(|k| bytemuck::must_cast(t[j0 + k]));
        for (l, v) in transpose(lanes).into_iter().enumerate() {
            let o = l * stride + j0;
            debug_assert!(o + 8 <= dst.len());
            // SAFETY: o + 8 <= 7 * stride + cols <= dst.len() (checked by `lanes_to_rows`).
            unsafe {
                vst1q_s32(dst.as_mut_ptr().add(o), vrshlq_s32(vmovl_s16(vget_low_s16(v)), sh));
                vst1q_s32(dst.as_mut_ptr().add(o + 4), vrshlq_s32(vmovl_high_s16(v), sh));
            }
        }
    }
}
