//! NEON kernels for HEVC sample adaptive offset (8.7.3) of 8-bit samples, 16 per vector.
//!
//! Band offset looks the offset up by band (sample >> 3) in a 32-entry table (`vqtbl2q_s8`).
//! Edge offset takes the sign of the sample minus each neighbour from two comparisons (-1, 0 or
//! 1 per lane), and their sum + 2 indexes the five offsets (`vqtbl1q_s8`, edgeIdx 0 adding 0).
//! Both add with `vsqaddq_u8`, the unsigned saturating add of a signed value, which is Clip1
//! for 8-bit samples. A run that is not a multiple of 16 ends with a vector overlapping the
//! previous one: the output is a function of the inputs alone, so rewriting lanes is harmless.

use core::arch::aarch64::*;

/// See [`crate::hevc_sao_band8`].
pub(crate) fn band(src: &[u8], dst: &mut [u8], table: &[i8; 32]) -> bool {
    let n = src.len();
    if n < 16 || dst.len() < n {
        return false;
    }
    // SAFETY: NEON is part of the aarch64 baseline: every core this build runs on has it.
    unsafe { band_v(src, dst, n, table) };
    true
}

/// See [`crate::hevc_sao_edge8`].
pub(crate) fn edge(cur: &[u8], a: &[u8], b: &[u8], dst: &mut [u8], offs: &[i8; 5]) -> bool {
    let n = cur.len();
    if n < 16 || a.len() < n || b.len() < n || dst.len() < n {
        return false;
    }
    // SAFETY: NEON is part of the aarch64 baseline: every core this build runs on has it.
    unsafe { edge_v(cur, a, b, dst, n, offs) };
    true
}

/// The vector starts of a run of `n` >= 16 samples: every 16, the last one at n - 16.
fn starts(n: usize) -> impl Iterator<Item = usize> {
    (0..n.div_ceil(16)).map(move |k| (16 * k).min(n - 16))
}

/// 16 samples of `s` from `o` (o + 16 <= n <= s.len(), checked by the callers).
#[target_feature(enable = "neon")]
fn ld(s: &[u8], o: usize) -> uint8x16_t {
    debug_assert!(o + 16 <= s.len());
    // SAFETY: o + 16 <= n <= s.len() (checked by `band` / `edge`), the 16 bytes `vld1q_u8` reads.
    unsafe { vld1q_u8(s.as_ptr().add(o)) }
}

#[target_feature(enable = "neon")]
fn st(s: &mut [u8], o: usize, v: uint8x16_t) {
    debug_assert!(o + 16 <= s.len());
    // SAFETY: as for `ld`.
    unsafe { vst1q_u8(s.as_mut_ptr().add(o), v) }
}

#[target_feature(enable = "neon")]
fn band_v(src: &[u8], dst: &mut [u8], n: usize, table: &[i8; 32]) {
    let t: int8x16x2_t = bytemuck::must_cast(*table);
    for o in starts(n) {
        let v = ld(src, o);
        st(dst, o, vsqaddq_u8(v, vqtbl2q_s8(t, vshrq_n_u8::<3>(v))));
    }
}

/// sign(v - a) per lane: -1, 0 or 1.
#[target_feature(enable = "neon")]
fn sign(v: uint8x16_t, a: uint8x16_t) -> int8x16_t {
    vsubq_s8(vreinterpretq_s8_u8(vcltq_u8(v, a)), vreinterpretq_s8_u8(vcgtq_u8(v, a)))
}

#[target_feature(enable = "neon")]
fn edge_v(cur: &[u8], a: &[u8], b: &[u8], dst: &mut [u8], n: usize, offs: &[i8; 5]) {
    let mut t = [0i8; 16];
    t[..5].copy_from_slice(offs);
    let t: int8x16_t = bytemuck::must_cast(t);
    let two = vdupq_n_s8(2);
    for o in starts(n) {
        let v = ld(cur, o);
        let k = vaddq_s8(vaddq_s8(sign(v, ld(a, o)), sign(v, ld(b, o))), two);
        st(dst, o, vsqaddq_u8(v, vqtbl1q_s8(t, vreinterpretq_u8_s8(k))));
    }
}
