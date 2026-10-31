//! NEON kernel for the VP8 inverse DCT (14.4) of a 4x4 block with the addition to the
//! prediction (14.5) fused in, written straight into the picture.
//!
//! The block is four 32-bit vectors, one row each: the first (vertical) pass works across them
//! without a transpose; its results are truncated to 16 bits (`vmovn`, wrapping) as the
//! specification's intermediate is; the second pass runs on the transposed block. The products
//! (x * 35468) >> 16 and x + ((x * 20091) >> 16) fit 32 bits for 16-bit x. A block whose AC
//! coefficients are all zero is one broadcast of (dc + 4) >> 3, as the full transform gives.

use core::arch::aarch64::*;

/// See [`crate::vp8_idct_add`].
pub(crate) fn idct_add(coefs: &[i16; 16], dst: &mut [u8], pos: usize, stride: usize) -> bool {
    if !stride.checked_mul(3).and_then(|n| n.checked_add(pos)?.checked_add(4)).is_some_and(|n| n <= dst.len()) {
        return false;
    }
    // The prediction rows, gathered and written back as one vector (4 bytes per row).
    let mut px = [0u8; 16];
    for r in 0..4 {
        px[4 * r..4 * r + 4].copy_from_slice(&dst[pos + r * stride..pos + r * stride + 4]);
    }
    // SAFETY: NEON is part of the aarch64 baseline: every core this build runs on has it.
    unsafe { block(coefs, &mut px) };
    for r in 0..4 {
        dst[pos + r * stride..pos + r * stride + 4].copy_from_slice(&px[4 * r..4 * r + 4]);
    }
    true
}

#[target_feature(enable = "neon")]
fn block(coefs: &[i16; 16], px: &mut [u8; 16]) {
    let ([c01, c23], p): ([int16x8_t; 2], uint8x16_t) = (bytemuck::must_cast(*coefs), bytemuck::must_cast(*px));
    let ac = vorrq_s16(vsetq_lane_s16::<0>(0, c01), c23);
    let res: [int16x8_t; 2] = if vmaxvq_u16(vreinterpretq_u16_s16(vabsq_s16(ac))) == 0 {
        // DC only: every residue sample is (dc + 4) >> 3
        let v = vdupq_n_s16(((i32::from(vgetq_lane_s16::<0>(c01)) + 4) >> 3) as i16);
        [v, v]
    } else {
        let r = [vmovl_s16(vget_low_s16(c01)), vmovl_high_s16(c01), vmovl_s16(vget_low_s16(c23)), vmovl_high_s16(c23)];
        // first pass over the columns (all four at once), truncated to 16 bits
        let t = butterfly(r).map(|v| vmovl_s16(vmovn_s32(v)));
        // second pass over the rows: transposed, the same butterfly, (x + 4) >> 3 to 16 bits
        let o = butterfly(transpose(t));
        let o = transpose(o).map(|v| vmovn_s32(vshrq_n_s32::<3>(vaddq_s32(v, vdupq_n_s32(4)))));
        [vcombine_s16(o[0], o[1]), vcombine_s16(o[2], o[3])]
    };
    // add to the prediction (rows 0, 1 then 2, 3), saturating to 8 bits
    let lo = vqaddq_s16(vreinterpretq_s16_u16(vmovl_u8(vget_low_u8(p))), res[0]);
    let hi = vqaddq_s16(vreinterpretq_s16_u16(vmovl_high_u8(p)), res[1]);
    *px = bytemuck::must_cast(vcombine_u8(vqmovun_s16(lo), vqmovun_s16(hi)));
}

/// One pass of 14.4 over four inputs ip(0..4) (vectors of 4 lanes): the outputs 0..4 in order.
#[target_feature(enable = "neon")]
fn butterfly(ip: [int32x4_t; 4]) -> [int32x4_t; 4] {
    let mul_s = |x: int32x4_t| vshrq_n_s32::<16>(vmulq_n_s32(x, 35468));
    let mul_c = |x: int32x4_t| vaddq_s32(x, vshrq_n_s32::<16>(vmulq_n_s32(x, 20091)));
    let a1 = vaddq_s32(ip[0], ip[2]);
    let b1 = vsubq_s32(ip[0], ip[2]);
    let c1 = vsubq_s32(mul_s(ip[1]), mul_c(ip[3]));
    let d1 = vaddq_s32(mul_c(ip[1]), mul_s(ip[3]));
    [vaddq_s32(a1, d1), vaddq_s32(b1, c1), vsubq_s32(b1, c1), vsubq_s32(a1, d1)]
}

/// A 4x4 block of 32-bit values, rows to columns.
#[target_feature(enable = "neon")]
fn transpose(r: [int32x4_t; 4]) -> [int32x4_t; 4] {
    let (a, b) = (vtrn1q_s32(r[0], r[1]), vtrn2q_s32(r[0], r[1]));
    let (c, d) = (vtrn1q_s32(r[2], r[3]), vtrn2q_s32(r[2], r[3]));
    let (l, s) = (vreinterpretq_s64_s32, vreinterpretq_s32_s64);
    [s(vtrn1q_s64(l(a), l(c))), s(vtrn1q_s64(l(b), l(d))), s(vtrn2q_s64(l(a), l(c))), s(vtrn2q_s64(l(b), l(d)))]
}
