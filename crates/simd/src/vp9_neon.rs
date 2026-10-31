//! NEON kernels for VP9 unscaled inter prediction (8.5.2.4), on 16-bit samples.
//!
//! Each block is filtered and written straight into the picture: put, or the rounding average of
//! compound prediction (`vrhadd`). 8-bit sums are kept modulo 2^16, 8 lanes per vector, and
//! narrowed as the scalar version's `narrow8` does; deeper samples take 32-bit sums, whose
//! rounding narrowing shift `vqrshrun` is exactly Round2(sum, 7) clipped below at 0. Blocks 4
//! wide take 4 lanes, wider ones vectors of 8. Two-pass blocks go down strips of 8 (or 4)
//! columns with the last 8 horizontally filtered rows in registers, so nothing is stored between
//! the passes.

use core::arch::aarch64::*;

use crate::Vp9Out;

/// Whether `rows` rows of `cols` samples, `stride` apart, fit in `len` samples.
fn fits(len: usize, stride: usize, cols: usize, rows: usize) -> bool {
    rows == 0 || (rows - 1).checked_mul(stride).and_then(|n| n.checked_add(cols)).is_some_and(|n| n <= len)
}

/// See [`crate::vp9_predict`]; false when the window is too small for the vector loads (one
/// sample past each row's w + 7) or `dst` too small: the scalar version then runs.
pub(crate) fn predict(
    (src, ss): (&[u16], usize),
    (fh, fv): (Option<&[i16; 8]>, Option<&[i16; 8]>),
    (w, h): (usize, usize),
    max: u16,
    wrap: bool,
    (dst, ds): (&mut [u16], usize),
    out: Vp9Out,
) -> bool {
    if !matches!(w, 4 | 8 | 16 | 32 | 64) || !(1..=64).contains(&h) || !fits(src.len(), ss, w + 8, h + 7) || !fits(dst.len(), ds, w, h) {
        return false;
    }
    let k = Taps::new(fh, fv);
    // SAFETY: NEON is part of the aarch64 baseline: every core this build runs on has it (every arm).
    unsafe {
        match (w, out == Vp9Out::Avg, wrap) {
            (4, false, true) => block4::<false, true>(src, ss, &k, h, max, dst, ds),
            (4, false, false) => block4::<false, false>(src, ss, &k, h, max, dst, ds),
            (4, true, true) => block4::<true, true>(src, ss, &k, h, max, dst, ds),
            (4, true, false) => block4::<true, false>(src, ss, &k, h, max, dst, ds),
            (8, false, true) => block::<8, false, true>(src, ss, &k, h, max, dst, ds),
            (8, false, false) => block::<8, false, false>(src, ss, &k, h, max, dst, ds),
            (8, true, true) => block::<8, true, true>(src, ss, &k, h, max, dst, ds),
            (8, true, false) => block::<8, true, false>(src, ss, &k, h, max, dst, ds),
            (16, false, true) => block::<16, false, true>(src, ss, &k, h, max, dst, ds),
            (16, false, false) => block::<16, false, false>(src, ss, &k, h, max, dst, ds),
            (16, true, true) => block::<16, true, true>(src, ss, &k, h, max, dst, ds),
            (16, true, false) => block::<16, true, false>(src, ss, &k, h, max, dst, ds),
            (32, false, true) => block::<32, false, true>(src, ss, &k, h, max, dst, ds),
            (32, false, false) => block::<32, false, false>(src, ss, &k, h, max, dst, ds),
            (32, true, true) => block::<32, true, true>(src, ss, &k, h, max, dst, ds),
            (32, true, false) => block::<32, true, false>(src, ss, &k, h, max, dst, ds),
            (_, false, true) => block::<64, false, true>(src, ss, &k, h, max, dst, ds),
            (_, false, false) => block::<64, false, false>(src, ss, &k, h, max, dst, ds),
            (_, true, true) => block::<64, true, true>(src, ss, &k, h, max, dst, ds),
            (_, true, false) => block::<64, true, false>(src, ss, &k, h, max, dst, ds),
        }
    }
    true
}

/// The two filters, as scalars for the wrapping 16-bit sums and as vectors for the 32-bit ones.
struct Taps {
    h: Option<([u16; 8], [i16; 8])>,
    v: Option<([u16; 8], [i16; 8])>,
}

impl Taps {
    fn new(fh: Option<&[i16; 8]>, fv: Option<&[i16; 8]>) -> Self {
        let split = |f: &[i16; 8]| (f.map(|c| c as u16), *f);
        Taps { h: fh.map(split), v: fv.map(split) }
    }
}

/// 8 samples from `o`: `predict` checked `s` holds every offset the kernels load from plus 8.
#[target_feature(enable = "neon")]
fn ld8(s: &[u16], o: usize) -> uint16x8_t {
    debug_assert!(o + 8 <= s.len());
    // SAFETY: `o + 8 <= s.len()` (checked by `predict`), so the 8 samples `vld1q_u16` reads are in `s`.
    unsafe { vld1q_u16(s.as_ptr().add(o)) }
}

/// 4 samples from `o` (checked as for [`ld8`]).
#[target_feature(enable = "neon")]
fn ld4(s: &[u16], o: usize) -> uint16x4_t {
    debug_assert!(o + 4 <= s.len());
    // SAFETY: `o + 4 <= s.len()` (checked as for `ld8`), so the 4 samples `vld1_u16` reads are in `s`.
    unsafe { vld1_u16(s.as_ptr().add(o)) }
}

#[target_feature(enable = "neon")]
fn taps_s16(f: &[i16; 8]) -> int16x8_t {
    bytemuck::must_cast(*f)
}

/// The 8-tap filter of 8 lanes, `x[t]` the samples under tap t. `WRAP`: sums modulo 2^16 (see
/// the module); otherwise in 32-bit lanes, clipped to 0..=max.
#[target_feature(enable = "neon")]
fn fir8<const WRAP: bool>(x: [uint16x8_t; 8], f: &([u16; 8], [i16; 8]), max: u16) -> uint16x8_t {
    if WRAP {
        let c = &f.0;
        let mut acc = vdupq_n_u16(64);
        for t in 0..8 {
            acc = vmlaq_n_u16(acc, x[t], c[t]);
        }
        let ok = vcltq_u16(acc, vdupq_n_u16(0xC000));
        return vandq_u16(vminq_u16(vshrq_n_u16::<7>(acc), vdupq_n_u16(255)), ok);
    }
    let k = taps_s16(&f.1);
    let s = |v: uint16x8_t| vreinterpretq_s16_u16(v);
    let mut lo = vmull_laneq_s16::<0>(vget_low_s16(s(x[0])), k);
    let mut hi = vmull_high_laneq_s16::<0>(s(x[0]), k);
    macro_rules! tap {
        ($t:literal) => {
            lo = vmlal_laneq_s16::<$t>(lo, vget_low_s16(s(x[$t])), k);
            hi = vmlal_high_laneq_s16::<$t>(hi, s(x[$t]), k);
        };
    }
    tap!(1);
    tap!(2);
    tap!(3);
    tap!(4);
    tap!(5);
    tap!(6);
    tap!(7);
    vminq_u16(vcombine_u16(vqrshrun_n_s32::<7>(lo), vqrshrun_n_s32::<7>(hi)), vdupq_n_u16(max))
}

/// [`fir8`] of 4 lanes.
#[target_feature(enable = "neon")]
fn fir4<const WRAP: bool>(x: [uint16x4_t; 8], f: &([u16; 8], [i16; 8]), max: u16) -> uint16x4_t {
    if WRAP {
        let c = &f.0;
        let mut acc = vdup_n_u16(64);
        for t in 0..8 {
            acc = vmla_n_u16(acc, x[t], c[t]);
        }
        let ok = vclt_u16(acc, vdup_n_u16(0xC000));
        return vand_u16(vmin_u16(vshr_n_u16::<7>(acc), vdup_n_u16(255)), ok);
    }
    let k = taps_s16(&f.1);
    let s = |v: uint16x4_t| vreinterpret_s16_u16(v);
    let mut acc = vmull_laneq_s16::<0>(s(x[0]), k);
    macro_rules! tap {
        ($t:literal) => {
            acc = vmlal_laneq_s16::<$t>(acc, s(x[$t]), k);
        };
    }
    tap!(1);
    tap!(2);
    tap!(3);
    tap!(4);
    tap!(5);
    tap!(6);
    tap!(7);
    vmin_u16(vqrshrun_n_s32::<7>(acc), vdup_n_u16(max))
}

/// The horizontal filter of 8 outputs from `s[o..o + 15]` (loads `s[o..o + 16]`).
#[target_feature(enable = "neon")]
fn h8<const WRAP: bool>(s: &[u16], o: usize, f: &([u16; 8], [i16; 8]), max: u16) -> uint16x8_t {
    let (a, b) = (ld8(s, o), ld8(s, o + 8));
    let x = [
        a,
        vextq_u16::<1>(a, b),
        vextq_u16::<2>(a, b),
        vextq_u16::<3>(a, b),
        vextq_u16::<4>(a, b),
        vextq_u16::<5>(a, b),
        vextq_u16::<6>(a, b),
        vextq_u16::<7>(a, b),
    ];
    fir8::<WRAP>(x, f, max)
}

/// The horizontal filter of 4 outputs from `s[o..o + 11]` (loads `s[o..o + 12]`).
#[target_feature(enable = "neon")]
fn h4<const WRAP: bool>(s: &[u16], o: usize, f: &([u16; 8], [i16; 8]), max: u16) -> uint16x4_t {
    let (a, b, c) = (ld4(s, o), ld4(s, o + 4), ld4(s, o + 8));
    let x = [a, vext_u16::<1>(a, b), vext_u16::<2>(a, b), vext_u16::<3>(a, b), b, vext_u16::<1>(b, c), vext_u16::<2>(b, c), vext_u16::<3>(b, c)];
    fir4::<WRAP>(x, f, max)
}

/// The vertical filter of 8 outputs from the rows `stride` apart from `o`.
#[target_feature(enable = "neon")]
fn v8<const WRAP: bool>(s: &[u16], o: usize, stride: usize, f: &([u16; 8], [i16; 8]), max: u16) -> uint16x8_t {
    let x = core::array::from_fn(|t| ld8(s, o + t * stride));
    fir8::<WRAP>(x, f, max)
}

#[target_feature(enable = "neon")]
fn v4<const WRAP: bool>(s: &[u16], o: usize, stride: usize, f: &([u16; 8], [i16; 8]), max: u16) -> uint16x4_t {
    let x = core::array::from_fn(|t| ld4(s, o + t * stride));
    fir4::<WRAP>(x, f, max)
}

/// Write 8 samples at `dst[o..]` (`AVG`: averaged with what is there).
#[target_feature(enable = "neon")]
fn st8<const AVG: bool>(dst: &mut [u16], o: usize, p: uint16x8_t) {
    let Some(d) = dst.get_mut(o..).and_then(|d| d.first_chunk_mut::<8>()) else {
        return;
    };
    // SAFETY: `d` is 8 samples, the 8 `vld1q_u16` reads and `vst1q_u16` writes.
    unsafe {
        let v = if AVG { vrhaddq_u16(vld1q_u16(d.as_ptr()), p) } else { p };
        vst1q_u16(d.as_mut_ptr(), v);
    }
}

#[target_feature(enable = "neon")]
fn st4<const AVG: bool>(dst: &mut [u16], o: usize, p: uint16x4_t) {
    let Some(d) = dst.get_mut(o..).and_then(|d| d.first_chunk_mut::<4>()) else {
        return;
    };
    // SAFETY: `d` is 4 samples, the 4 `vld1_u16` reads and `vst1_u16` writes.
    unsafe {
        let v = if AVG { vrhadd_u16(vld1_u16(d.as_ptr()), p) } else { p };
        vst1_u16(d.as_mut_ptr(), v);
    }
}

/// A block `W` (8..=64) wide.
#[target_feature(enable = "neon")]
fn block<const W: usize, const AVG: bool, const WRAP: bool>(src: &[u16], ss: usize, k: &Taps, h: usize, max: u16, dst: &mut [u16], ds: usize) {
    match (&k.h, &k.v) {
        (None, None) => {
            for i in 0..h {
                for c in (0..W).step_by(8) {
                    st8::<AVG>(dst, i * ds + c, ld8(src, (i + 3) * ss + 3 + c));
                }
            }
        }
        (Some(f), None) => {
            for i in 0..h {
                for c in (0..W).step_by(8) {
                    st8::<AVG>(dst, i * ds + c, h8::<WRAP>(src, (i + 3) * ss + c, f, max));
                }
            }
        }
        (None, Some(g)) => {
            for i in 0..h {
                for c in (0..W).step_by(8) {
                    st8::<AVG>(dst, i * ds + c, v8::<WRAP>(src, i * ss + 3 + c, ss, g, max));
                }
            }
        }
        (Some(f), Some(g)) => {
            // Down each strip of 8 columns, with the last 8 horizontally filtered rows in registers.
            for c in (0..W).step_by(8) {
                let mut x: [uint16x8_t; 8] = core::array::from_fn(|t| h8::<WRAP>(src, t * ss + c, f, max));
                for i in 0..h {
                    st8::<AVG>(dst, i * ds + c, fir8::<WRAP>(x, g, max));
                    if i + 1 < h {
                        let next = h8::<WRAP>(src, (i + 8) * ss + c, f, max);
                        x = core::array::from_fn(|t| if t < 7 { x[t + 1] } else { next });
                    }
                }
            }
        }
    }
}

/// A block 4 wide.
#[target_feature(enable = "neon")]
fn block4<const AVG: bool, const WRAP: bool>(src: &[u16], ss: usize, k: &Taps, h: usize, max: u16, dst: &mut [u16], ds: usize) {
    match (&k.h, &k.v) {
        (None, None) => {
            for i in 0..h {
                st4::<AVG>(dst, i * ds, ld4(src, (i + 3) * ss + 3));
            }
        }
        (Some(f), None) => {
            for i in 0..h {
                st4::<AVG>(dst, i * ds, h4::<WRAP>(src, (i + 3) * ss, f, max));
            }
        }
        (None, Some(g)) => {
            for i in 0..h {
                st4::<AVG>(dst, i * ds, v4::<WRAP>(src, i * ss + 3, ss, g, max));
            }
        }
        (Some(f), Some(g)) => {
            let mut x: [uint16x4_t; 8] = core::array::from_fn(|t| h4::<WRAP>(src, t * ss, f, max));
            for i in 0..h {
                st4::<AVG>(dst, i * ds, fir4::<WRAP>(x, g, max));
                if i + 1 < h {
                    let next = h4::<WRAP>(src, (i + 8) * ss, f, max);
                    x = core::array::from_fn(|t| if t < 7 { x[t + 1] } else { next });
                }
            }
        }
    }
}
