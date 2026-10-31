//! NEON kernels for the VP8 loop filters (15.2 simple, 15.3 normal: macroblock and subblock
//! edges): 16 lines per vector of 8-bit lanes, a 16-line luma edge or the same 8-line edge of
//! both chroma planes (U in lanes 0..8, V in 8..16) per call.
//!
//! The filters work on pixels minus 128 with `vp8_signed_char_clamp` after each step, which the
//! saturating 8-bit operations are. The one sum of more than two terms, clamp(outer +
//! 3 * (q0 - p0)), is three saturating additions of the saturated q0 - p0: they have one sign, so
//! a clamped partial sum stays clamped, and a q0 - p0 beyond 8 bits takes the sum beyond 8 bits
//! either way. Only the macroblock filter's 27 / 18 / 9 taps take 16-bit lanes. A vertical edge's
//! rows of p3..q3 are transposed in registers (8x8 bytes, two side by side) and back.

use core::arch::aarch64::*;

use crate::{Vp8Filter, Vp8Limits};

/// Whether p3..q3 of `lines` lines from q0 at `start` lie in `len` samples: 4 steps before
/// `start`, 3 after the last line's.
fn fits(len: usize, start: usize, stride: usize, vertical: bool, lines: usize) -> bool {
    let (step, along) = if vertical { (1, stride) } else { (stride, 1) };
    4usize.checked_mul(step).is_some_and(|b| b <= start)
        && (lines - 1).checked_mul(along).and_then(|a| a.checked_add(start)).and_then(|e| e.checked_add(3usize.checked_mul(step)?)).is_some_and(|e| e < len)
}

/// The filter as the kernels' const parameter.
const fn kind(f: Vp8Filter) -> u8 {
    match f {
        Vp8Filter::Simple => 0,
        Vp8Filter::Subblock => 1,
        Vp8Filter::Macroblock => 2,
    }
}

/// See [`crate::vp8_loop_filter`].
pub(crate) fn luma(d: &mut [u8], start: usize, stride: usize, vertical: bool, f: Vp8Filter, l: &Vp8Limits) -> bool {
    if !fits(d.len(), start, stride, vertical, 16) {
        return false;
    }
    match kind(f) {
        // SAFETY: NEON is part of the aarch64 baseline: every core this build runs on has it.
        0 => unsafe { luma_k::<0>(d, start, stride, vertical, l) },
        // SAFETY: as above.
        1 => unsafe { luma_k::<1>(d, start, stride, vertical, l) },
        // SAFETY: as above.
        _ => unsafe { luma_k::<2>(d, start, stride, vertical, l) },
    }
    true
}

/// [`luma`] with the filter [`kind`] `K`: each filter its own loads, transposes and stores
/// around it, so that it is inlined.
#[target_feature(enable = "neon")]
fn luma_k<const K: u8>(d: &mut [u8], start: usize, stride: usize, vertical: bool, l: &Vp8Limits) {
    {
        if vertical {
            let x = transpose(std::array::from_fn(|r| ld8(d, start - 4 + r * stride)));
            if let Some(y) = filter::<K>(x, l) {
                for (r, v) in transpose_back(y).into_iter().enumerate() {
                    st8(d, start - 4 + r * stride, v);
                }
            }
        } else {
            let x = std::array::from_fn(|k| ld16(d, start + k * stride - 4 * stride));
            if let Some(y) = filter::<K>(x, l) {
                for (k, &v) in y.iter().enumerate().take(7).skip(1) {
                    st16(d, start + k * stride - 4 * stride, v);
                }
            }
        }
    }
}

/// See [`crate::vp8_loop_filter_uv`].
pub(crate) fn chroma(u: &mut [u8], v: &mut [u8], start: usize, stride: usize, vertical: bool, f: Vp8Filter, l: &Vp8Limits) -> bool {
    if !fits(u.len(), start, stride, vertical, 8) || !fits(v.len(), start, stride, vertical, 8) {
        return false;
    }
    match kind(f) {
        // SAFETY: NEON is part of the aarch64 baseline: every core this build runs on has it.
        0 => unsafe { chroma_k::<0>(u, v, start, stride, vertical, l) },
        // SAFETY: as above.
        1 => unsafe { chroma_k::<1>(u, v, start, stride, vertical, l) },
        // SAFETY: as above.
        _ => unsafe { chroma_k::<2>(u, v, start, stride, vertical, l) },
    }
    true
}

/// [`chroma`] with the filter [`kind`] `K`.
#[target_feature(enable = "neon")]
fn chroma_k<const K: u8>(u: &mut [u8], v: &mut [u8], start: usize, stride: usize, vertical: bool, l: &Vp8Limits) {
    {
        if vertical {
            let x = transpose(std::array::from_fn(|r| if r < 8 { ld8(u, start - 4 + r * stride) } else { ld8(v, start - 4 + (r - 8) * stride) }));
            if let Some(y) = filter::<K>(x, l) {
                for (r, w) in transpose_back(y).into_iter().enumerate() {
                    if r < 8 {
                        st8(u, start - 4 + r * stride, w);
                    } else {
                        st8(v, start - 4 + (r - 8) * stride, w);
                    }
                }
            }
        } else {
            let x = std::array::from_fn(|k| {
                let o = start + k * stride - 4 * stride;
                vcombine_u8(ld8(u, o), ld8(v, o))
            });
            if let Some(y) = filter::<K>(x, l) {
                for (k, &w) in y.iter().enumerate().take(7).skip(1) {
                    let o = start + k * stride - 4 * stride;
                    st8(u, o, vget_low_u8(w));
                    st8(v, o, vget_high_u8(w));
                }
            }
        }
    }
}

/// 8 bytes of `d` from `o`: the callers checked with `fits` that every row they touch is in `d`.
#[target_feature(enable = "neon")]
fn ld8(d: &[u8], o: usize) -> uint8x8_t {
    debug_assert!(o + 8 <= d.len());
    // SAFETY: o + 8 <= d.len() (p3..q3 of a vertical edge's line, or 8 of a horizontal edge's
    // lines, are in `d` by `fits`), the 8 bytes `vld1_u8` reads.
    unsafe { vld1_u8(d.as_ptr().add(o)) }
}

#[target_feature(enable = "neon")]
fn st8(d: &mut [u8], o: usize, v: uint8x8_t) {
    debug_assert!(o + 8 <= d.len());
    // SAFETY: as for `ld8`.
    unsafe { vst1_u8(d.as_mut_ptr().add(o), v) }
}

#[target_feature(enable = "neon")]
fn ld16(d: &[u8], o: usize) -> uint8x16_t {
    debug_assert!(o + 16 <= d.len());
    // SAFETY: o + 16 <= d.len() (the 16 lines of a horizontal luma edge are in `d` by `fits`),
    // the 16 bytes `vld1q_u8` reads.
    unsafe { vld1q_u8(d.as_ptr().add(o)) }
}

#[target_feature(enable = "neon")]
fn st16(d: &mut [u8], o: usize, v: uint8x16_t) {
    debug_assert!(o + 16 <= d.len());
    // SAFETY: as for `ld16`.
    unsafe { vst1q_u8(d.as_mut_ptr().add(o), v) }
}

/// 16 rows of 8 bytes to 8 columns of 16 (rows 0..8 in the low halves, 8..16 in the high ones):
/// two 8x8 transposes side by side.
#[target_feature(enable = "neon")]
fn transpose(r: [uint8x8_t; 16]) -> [uint8x16_t; 8] {
    let q: [uint8x16_t; 8] = std::array::from_fn(|i| vcombine_u8(r[i], r[i + 8]));
    let b = [vtrnq_u8(q[0], q[1]), vtrnq_u8(q[2], q[3]), vtrnq_u8(q[4], q[5]), vtrnq_u8(q[6], q[7])];
    let h = vreinterpretq_u16_u8;
    // Rows 0..4 and 4..8 (of each half) of columns (0, 4), (2, 6), (1, 5) and (3, 7).
    let c = [vtrnq_u16(h(b[0].0), h(b[1].0)), vtrnq_u16(h(b[0].1), h(b[1].1)), vtrnq_u16(h(b[2].0), h(b[3].0)), vtrnq_u16(h(b[2].1), h(b[3].1))];
    let w = vreinterpretq_u32_u16;
    let d = [vtrnq_u32(w(c[0].0), w(c[2].0)), vtrnq_u32(w(c[1].0), w(c[3].0)), vtrnq_u32(w(c[0].1), w(c[2].1)), vtrnq_u32(w(c[1].1), w(c[3].1))];
    let u = vreinterpretq_u8_u32;
    [u(d[0].0), u(d[1].0), u(d[2].0), u(d[3].0), u(d[0].1), u(d[1].1), u(d[2].1), u(d[3].1)]
}

/// The inverse of [`transpose`].
#[target_feature(enable = "neon")]
fn transpose_back(x: [uint8x16_t; 8]) -> [uint8x8_t; 16] {
    // The transpose of the 8 columns (as 8 rows of two 8x8 blocks) gives the rows.
    let r = transpose(std::array::from_fn(|i| if i < 8 { vget_low_u8(x[i]) } else { vget_high_u8(x[i - 8]) }));
    std::array::from_fn(|i| if i < 8 { vget_low_u8(r[i]) } else { vget_high_u8(r[i - 8]) })
}

/// The filter [`kind`] `K` of 16 lines: x[k] is pixel k - 4 across the edge (p3 p2 p1 p0 | q0
/// q1 q2 q3); None when no line is filtered.
#[target_feature(enable = "neon")]
fn filter<const K: u8>(x: [uint8x16_t; 8], l: &Vp8Limits) -> Option<[uint8x16_t; 8]> {
    let f = const {
        match K {
            0 => Vp8Filter::Simple,
            1 => Vp8Filter::Subblock,
            _ => Vp8Filter::Macroblock,
        }
    };
    let [p3, p2, p1, p0, q0, q1, q2, q3] = x;
    // filter masks (15.2, 15.3): the sums saturate at 255, above any limit
    let a0 = vabdq_u8(p0, q0);
    let e = vqaddq_u8(vqaddq_u8(a0, a0), vshrq_n_u8::<1>(vabdq_u8(p1, q1)));
    let mut m = vcleq_u8(e, vdupq_n_u8(l.edge.clamp(0, 255) as u8));
    let mut hev = vdupq_n_u8(0);
    if f != Vp8Filter::Simple {
        let (ap, aq) = (vabdq_u8(p1, p0), vabdq_u8(q1, q0));
        let dmax = vmaxq_u8(vmaxq_u8(vmaxq_u8(vabdq_u8(p3, p2), vabdq_u8(p2, p1)), ap), vmaxq_u8(vmaxq_u8(vabdq_u8(q3, q2), vabdq_u8(q2, q1)), aq));
        m = vandq_u8(m, vcleq_u8(dmax, vdupq_n_u8(l.interior.clamp(0, 255) as u8)));
        hev = vcgtq_u8(vmaxq_u8(ap, aq), vdupq_n_u8(l.hev_threshold.clamp(0, 255) as u8));
    }
    if vmaxvq_u8(m) == 0 {
        return None;
    }
    let s = |v: uint8x16_t| vreinterpretq_s8_u8(veorq_u8(v, vdupq_n_u8(0x80)));
    let u = |v: int8x16_t| veorq_u8(vreinterpretq_u8_s8(v), vdupq_n_u8(0x80));
    let (m, hev) = (vreinterpretq_s8_u8(m), vreinterpretq_s8_u8(hev));
    let (sp2, sp1, sp0, sq0, sq1, sq2) = (s(p2), s(p1), s(p0), s(q0), s(q1), s(q2));
    let d = vqsubq_s8(sq0, sp0);
    let outer = vqsubq_s8(sp1, sq1);
    // clamp(base + 3 * (q0 - p0)) (see the module)
    let three = |base: int8x16_t| vqaddq_s8(vqaddq_s8(vqaddq_s8(base, d), d), d);
    let mut y = x;
    match f {
        Vp8Filter::Simple => {
            let w = vandq_s8(three(outer), m);
            y[4] = u(vqsubq_s8(sq0, vshrq_n_s8::<3>(vqaddq_s8(w, vdupq_n_s8(4)))));
            y[3] = u(vqaddq_s8(sp0, vshrq_n_s8::<3>(vqaddq_s8(w, vdupq_n_s8(3)))));
        }
        Vp8Filter::Subblock => {
            let w = vandq_s8(three(vandq_s8(outer, hev)), m);
            let f1 = vshrq_n_s8::<3>(vqaddq_s8(w, vdupq_n_s8(4)));
            y[4] = u(vqsubq_s8(sq0, f1));
            y[3] = u(vqaddq_s8(sp0, vshrq_n_s8::<3>(vqaddq_s8(w, vdupq_n_s8(3)))));
            // (f1 + 1) >> 1: f1 is within -16..=15
            let a = vbicq_s8(vrshrq_n_s8::<1>(f1), hev);
            y[5] = u(vqsubq_s8(sq1, a));
            y[2] = u(vqaddq_s8(sp1, a));
        }
        Vp8Filter::Macroblock => {
            let w = vandq_s8(three(outer), m);
            let fh = vandq_s8(w, hev);
            let q0a = vqsubq_s8(sq0, vshrq_n_s8::<3>(vqaddq_s8(fh, vdupq_n_s8(4))));
            let p0a = vqaddq_s8(sp0, vshrq_n_s8::<3>(vqaddq_s8(fh, vdupq_n_s8(3))));
            let w = vbicq_s8(w, hev);
            // clamp((mul * w + 63) >> 7) in 16-bit lanes
            let tap = |mul: i8| {
                let lo = vshrq_n_s16::<7>(vaddq_s16(vmull_s8(vget_low_s8(w), vdup_n_s8(mul)), vdupq_n_s16(63)));
                let hi = vshrq_n_s16::<7>(vaddq_s16(vmull_high_s8(w, vdupq_n_s8(mul)), vdupq_n_s16(63)));
                vcombine_s8(vqmovn_s16(lo), vqmovn_s16(hi))
            };
            let (a27, a18, a9) = (tap(27), tap(18), tap(9));
            y[4] = u(vqsubq_s8(q0a, a27));
            y[3] = u(vqaddq_s8(p0a, a27));
            y[5] = u(vqsubq_s8(sq1, a18));
            y[2] = u(vqaddq_s8(sp1, a18));
            y[6] = u(vqsubq_s8(sq2, a9));
            y[1] = u(vqaddq_s8(sp2, a9));
        }
    }
    Some(y)
}
