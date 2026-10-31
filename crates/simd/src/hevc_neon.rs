//! NEON kernels for HEVC inter prediction of 8-bit samples (8.5.3.3.3): the fractional sample
//! interpolation of luma (8 taps) and chroma (4 taps), and the default weighted sample
//! prediction (8-262, 8-263) fused into the store.
//!
//! The reference is read in place, one slice per window row. With 8-bit samples the first pass
//! needs no shift (shift1 = 0) and its sums fit 16-bit lanes exactly (the taps of every filter
//! add up to at most 88 positive and -24 negative), so it runs 8 lanes per vector; the second
//! pass of a two-dimensional filter sums those 16-bit values in 32-bit lanes and shifts by 6
//! (shift2), as the specification does. Blocks go down strips of 8 columns (and one of 4 for
//! widths 4, 12, ...), the last T horizontally filtered rows kept in registers.
//!
//! Output: the 14-bit predictions themselves; Clip3(0, 255, (p + 32) >> 6) for uni-prediction,
//! which is `vqrshrun` by 6; and Clip3(0, 255, (p0 + p1 + 64) >> 7) for bi-prediction, which is
//! `vqrshrun` by 6 of the halving add `(p0 + p1) >> 1`: dropping the low bit of an odd sum
//! cannot change the result, since an odd sum plus 64 is never a multiple of 128.

use core::arch::aarch64::*;

use crate::HevcDst;

/// See [`crate::hevc_luma8`]: false (nothing written) for a block shape the vector version does
/// not cover, rows too short for its loads, or a destination too small.
pub(crate) fn predict<const T: usize>(rows: &[&[u8]], fh: Option<&[i8; T]>, fv: Option<&[i8; T]>, w: usize, h: usize, dst: HevcDst) -> bool {
    if !w.is_multiple_of(4) || !(4..=64).contains(&w) || !(1..=64).contains(&h) {
        return false;
    }
    let Some(rows) = rows.get(..h + T - 1) else { return false };
    // Every load is 16 samples from a strip's first column or 8 from its first column + m.
    let last = if w.is_multiple_of(8) { w - 8 } else { w - 4 };
    if rows.iter().map(|r| r.len()).min().is_none_or(|n| n < last + 16) {
        return false;
    }
    let fits = |len: usize, stride: usize| (h - 1).checked_mul(stride).and_then(|n| n.checked_add(w)).is_some_and(|n| n <= len);
    match dst {
        // SAFETY: NEON is part of the aarch64 baseline: every core this build runs on has it.
        HevcDst::Pred(p) if p.len() >= w * h => unsafe { block::<T, _>(rows, fh, fv, w, h, Pred(p, w)) },
        // SAFETY: as above.
        HevcDst::Put(d, ds) if fits(d.len(), ds) => unsafe { block::<T, _>(rows, fh, fv, w, h, Put(d, ds)) },
        // SAFETY: as above.
        HevcDst::Bi(p0, d, ds) if p0.len() >= w * h && fits(d.len(), ds) => unsafe { block::<T, _>(rows, fh, fv, w, h, Bi(p0, w, d, ds)) },
        _ => return false,
    }
    true
}

/// 16 samples of `row` from `o`; `predict` checked that every row holds `o + 16` of them.
#[target_feature(enable = "neon")]
fn ld16(row: &[u8], o: usize) -> uint8x16_t {
    debug_assert!(o + 16 <= row.len());
    // SAFETY: `o + 16 <= row.len()` (checked by `predict`), the 16 bytes `vld1q_u8` reads.
    unsafe { vld1q_u8(row.as_ptr().add(o)) }
}

/// 8 samples of `row` from `o` (o + 8 <= o' + 16 for the strip's first column o' and m <= 3).
#[target_feature(enable = "neon")]
fn ld8(row: &[u8], o: usize) -> int16x8_t {
    debug_assert!(o + 8 <= row.len());
    // SAFETY: `o + 8 <= row.len()` (checked by `predict` as for `ld16`), the 8 bytes `vld1_u8` reads.
    vreinterpretq_s16_u16(vmovl_u8(unsafe { vld1_u8(row.as_ptr().add(o)) }))
}

/// The taps as vectors of 8 equal lanes.
#[target_feature(enable = "neon")]
fn splat<const T: usize>(f: &[i8; T]) -> [int16x8_t; T] {
    f.map(|c| vdupq_n_s16(c as i16))
}

/// The horizontal filter of 8 outputs from column `o` of `row` (16-bit lanes, exact).
#[target_feature(enable = "neon")]
fn hfir<const T: usize>(row: &[u8], o: usize, k: &[int16x8_t; T]) -> int16x8_t {
    let v = ld16(row, o);
    let lo = vreinterpretq_s16_u16(vmovl_u8(vget_low_u8(v)));
    let hi = vreinterpretq_s16_u16(vmovl_high_u8(v));
    let x = [
        lo,
        vextq_s16::<1>(lo, hi),
        vextq_s16::<2>(lo, hi),
        vextq_s16::<3>(lo, hi),
        vextq_s16::<4>(lo, hi),
        vextq_s16::<5>(lo, hi),
        vextq_s16::<6>(lo, hi),
        vextq_s16::<7>(lo, hi),
    ];
    let mut acc = vmulq_s16(x[0], k[0]);
    for t in 1..T {
        acc = vmlaq_s16(acc, x[t], k[t]);
    }
    acc
}

/// The vertical filter of 8 lanes over the 16-bit values `x[t]`: (sum) >> 6 in 32-bit lanes.
#[target_feature(enable = "neon")]
fn vfir32<const T: usize>(x: &[int16x8_t; T], k: &[int16x8_t; T]) -> int16x8_t {
    let mut lo = vmull_s16(vget_low_s16(x[0]), vget_low_s16(k[0]));
    let mut hi = vmull_high_s16(x[0], k[0]);
    for t in 1..T {
        lo = vmlal_s16(lo, vget_low_s16(x[t]), vget_low_s16(k[t]));
        hi = vmlal_high_s16(hi, x[t], k[t]);
    }
    vcombine_s16(vshrn_n_s32::<6>(lo), vshrn_n_s32::<6>(hi))
}

/// Where a block's prediction rows go (see [`HevcDst`]), as a type so that each output gets its
/// own loops.
trait Out {
    /// The predictions `p` of row `r`, columns `x..x + N` (N = 8 or 4).
    fn put<const N: usize>(&mut self, r: usize, x: usize, p: int16x8_t);
}

/// [`HevcDst::Pred`]: rows `w` apart.
struct Pred<'a>(&'a mut [i16], usize);
/// [`HevcDst::Put`].
struct Put<'a>(&'a mut [u8], usize);
/// [`HevcDst::Bi`], with the block width (the first prediction's row stride).
struct Bi<'a>(&'a [i16], usize, &'a mut [u8], usize);

impl Out for Pred<'_> {
    #[inline(always)]
    fn put<const N: usize>(&mut self, r: usize, x: usize, p: int16x8_t) {
        let o = r * self.1 + x;
        debug_assert!(o + N <= self.0.len());
        // SAFETY: `predict` checked buf.len() >= w * h, and o + N <= (r + 1) * w <= w * h.
        unsafe {
            if N == 8 {
                vst1q_s16(self.0.as_mut_ptr().add(o), p);
            } else {
                vst1_s16(self.0.as_mut_ptr().add(o), vget_low_s16(p));
            }
        }
    }
}

impl Out for Put<'_> {
    #[inline(always)]
    fn put<const N: usize>(&mut self, r: usize, x: usize, p: int16x8_t) {
        // SAFETY: NEON is part of the aarch64 baseline (and the caller runs with it).
        unsafe { store8::<N>(self.0, r * self.1 + x, vqrshrun_n_s16::<6>(p)) }
    }
}

impl Out for Bi<'_> {
    #[inline(always)]
    fn put<const N: usize>(&mut self, r: usize, x: usize, p: int16x8_t) {
        let o = r * self.1 + x;
        debug_assert!(o + N <= self.0.len());
        // SAFETY: `predict` checked p0.len() >= w * h, and o + N <= w * h; a 4-sample strip
        // reads 4 of them.
        unsafe {
            let a = if N == 8 { vld1q_s16(self.0.as_ptr().add(o)) } else { vcombine_s16(vld1_s16(self.0.as_ptr().add(o)), vdup_n_s16(0)) };
            store8::<N>(self.2, r * self.3 + x, vqrshrun_n_s16::<6>(vhaddq_s16(a, p)));
        }
    }
}

/// Store the first N (8 or 4) samples of `v` at `d[o..]`.
#[target_feature(enable = "neon")]
fn store8<const N: usize>(d: &mut [u8], o: usize, v: uint8x8_t) {
    debug_assert!(o + N <= d.len());
    if N == 8 {
        // SAFETY: `predict` checked (h - 1) * ds + w <= d.len(), and o + 8 <= r * ds + w for the
        // strips of row r.
        unsafe { vst1_u8(d.as_mut_ptr().add(o), v) };
    } else {
        let t: [u8; 8] = bytemuck::must_cast(v);
        if let Some(d) = d.get_mut(o..o + 4) {
            d.copy_from_slice(&t[..4]);
        }
    }
}

#[target_feature(enable = "neon")]
fn block<const T: usize, O: Out>(rows: &[&[u8]], fh: Option<&[i8; T]>, fv: Option<&[i8; T]>, w: usize, h: usize, mut out: O) {
    let two_d = fh.is_some() && fv.is_some();
    let mut x = 0;
    while x < w {
        if w - x >= 16 && !two_d {
            strip::<T, 16, O>(rows, fh, fv, h, x, &mut out);
            x += 16;
        } else if w - x >= 8 {
            strip::<T, 8, O>(rows, fh, fv, h, x, &mut out);
            x += 8;
        } else {
            strip::<T, 4, O>(rows, fh, fv, h, x, &mut out);
            x += 4;
        }
    }
}

/// The columns `x..x + N` of the block: N = 16 as two vectors of 8 per row (one-pass filters
/// only), else 8 lanes computed and the first N written.
#[target_feature(enable = "neon")]
fn strip<const T: usize, const N: usize, O: Out>(rows: &[&[u8]], fh: Option<&[i8; T]>, fv: Option<&[i8; T]>, h: usize, x: usize, out: &mut O) {
    // The block's top-left sample is at (m, m) in the window.
    let m = T / 2 - 1;
    // Each row's vectors: the first 8 columns, and the next 8 for a strip of 16.
    let nv = N.div_ceil(8);
    match (fh, fv) {
        (None, None) => {
            for r in 0..h {
                let row = rows[r + m];
                for v in 0..nv {
                    put_n::<N, O>(out, r, x + 8 * v, vshlq_n_s16::<6>(ld8(row, m + x + 8 * v)));
                }
            }
        }
        (Some(f), None) => {
            let k = splat(f);
            for r in 0..h {
                let row = rows[r + m];
                for v in 0..nv {
                    put_n::<N, O>(out, r, x + 8 * v, hfir(row, x + 8 * v, &k));
                }
            }
        }
        (None, Some(f)) => {
            let k = splat(f);
            for r in 0..h {
                for v in 0..nv {
                    let o = m + x + 8 * v;
                    let mut acc = vmulq_s16(ld8(rows[r], o), k[0]);
                    for t in 1..T {
                        acc = vmlaq_s16(acc, ld8(rows[r + t], o), k[t]);
                    }
                    put_n::<N, O>(out, r, x + 8 * v, acc);
                }
            }
        }
        (Some(f), Some(g)) => {
            let (kh, kv) = (splat(f), splat(g));
            // The horizontally filtered rows r..r + T of the window.
            let mut win = [vdupq_n_s16(0); T];
            for t in 0..T - 1 {
                win[t + 1] = hfir(rows[t], x, &kh);
            }
            for r in 0..h {
                for t in 0..T - 1 {
                    win[t] = win[t + 1];
                }
                win[T - 1] = hfir(rows[r + T - 1], x, &kh);
                put_n::<N, O>(out, r, x, vfir32(&win, &kv));
            }
        }
    }
}

/// One vector of a strip `N` wide: all 8 lanes, or the first 4 of a 4-wide strip.
#[inline(always)]
fn put_n<const N: usize, O: Out>(out: &mut O, r: usize, x: usize, p: int16x8_t) {
    if N == 4 {
        out.put::<4>(r, x, p);
    } else {
        out.put::<8>(r, x, p);
    }
}
