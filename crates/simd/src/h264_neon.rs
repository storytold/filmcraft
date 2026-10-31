//! NEON kernels for H.264 8-bit inter prediction (8.4.2.2, 8.4.2.3) and deblocking (8.7.2).
//!
//! Prediction reads the block's reference window and writes the block straight into the picture:
//! put, the rounding average of bi-prediction (`vrhadd`), or weighted prediction, each a kernel
//! of its own (the mode is a const parameter). Luma blocks 16 wide take 16 lanes
//! (6-tap sums in two vectors of 16-bit lanes), 8- and 4-wide ones 8 lanes; the sums fit 16 bits
//! (-2550..=10710), the centre position's second pass takes 32-bit lanes, and the rounding
//! narrowing shifts `vqrshrun` are exactly Clip1((v + 16) >> 5) and Clip1((v + 512) >> 10).

use core::arch::aarch64::*;

use crate::H264Out;

/// Output modes, the const parameter of the kernels.
const PUT: u8 = 0;
const AVG: u8 = 1;
const WEIGHT: u8 = 2;
const WEIGHT_BI: u8 = 3;

/// The weights of an output mode (unused ones zero).
#[derive(Clone, Copy)]
struct Wt {
    w0: i16,
    w1: i16,
    o: i16,
    log_wd: i16,
}

/// The kernel parameter and weights of `out`, or None when the weights are outside 8-bit H.264's
/// range (-128..=127, log2 denominator up to 7), for which the single weighting fits 16-bit lanes.
fn mode(out: H264Out) -> Option<(u8, Wt)> {
    let r = |v: i32| (-128..=127).contains(&v);
    let z = Wt { w0: 0, w1: 0, o: 0, log_wd: 0 };
    match out {
        H264Out::Put => Some((PUT, z)),
        H264Out::Avg => Some((AVG, z)),
        H264Out::Weight { w, o, log_wd } if r(w) && r(o) && log_wd <= 7 => Some((WEIGHT, Wt { w0: w as i16, o: o as i16, log_wd: log_wd as i16, ..z })),
        H264Out::WeightBi { w0, w1, o, log_wd } if r(w0) && r(w1) && r(o) && log_wd <= 7 => {
            Some((WEIGHT_BI, Wt { w0: w0 as i16, w1: w1 as i16, o: o as i16, log_wd: log_wd as i16 }))
        }
        _ => None,
    }
}

/// Whether the w x h block fits `dst` (rows `ds` apart).
fn dst_fits(dst: &[u8], ds: usize, w: usize, h: usize) -> bool {
    h == 0 || (h - 1).checked_mul(ds).and_then(|n| n.checked_add(w)).is_some_and(|n| n <= dst.len())
}

/// See [`crate::h264_luma`]; false when the window is too small for the vector loads (16
/// samples, 8 for blocks up to 8 wide, from up to 5 past each row's start), `dst` too small, or
/// the weights out of range: the scalar version then runs.
pub(crate) fn luma((win, ws): (&[u8], usize), (fx, fy): (u32, u32), (w, h): (usize, usize), (dst, ds): (&mut [u8], usize), out: H264Out) -> bool {
    let lanes = if w == 16 { 16 } else { 8 };
    let Some((m, wt)) = mode(out) else {
        return false;
    };
    let fits = matches!(w, 4 | 8 | 16)
        && (1..=16).contains(&h)
        && (h + 4).checked_mul(ws).and_then(|n| n.checked_add(5 + lanes)).is_some_and(|n| n <= win.len())
        && dst_fits(dst, ds, w, h);
    if !fits {
        return false;
    }
    // SAFETY: NEON is part of the aarch64 baseline: every core this build runs on has it (every arm).
    unsafe {
        match (w == 16, m) {
            (true, PUT) => luma16::<PUT>((win, ws), (fx, fy), h, (dst, ds), wt),
            (true, AVG) => luma16::<AVG>((win, ws), (fx, fy), h, (dst, ds), wt),
            (true, WEIGHT) => luma16::<WEIGHT>((win, ws), (fx, fy), h, (dst, ds), wt),
            (true, _) => luma16::<WEIGHT_BI>((win, ws), (fx, fy), h, (dst, ds), wt),
            (false, PUT) => luma8::<PUT>((win, ws), (fx, fy), w, h, (dst, ds), wt),
            (false, AVG) => luma8::<AVG>((win, ws), (fx, fy), w, h, (dst, ds), wt),
            (false, WEIGHT) => luma8::<WEIGHT>((win, ws), (fx, fy), w, h, (dst, ds), wt),
            (false, _) => luma8::<WEIGHT_BI>((win, ws), (fx, fy), w, h, (dst, ds), wt),
        }
    }
    true
}

/// 16 samples of the window from `o`: `luma` checked `win` holds every offset the kernel
/// loads from (at most window row h + 4, column 5) plus 16.
#[target_feature(enable = "neon")]
fn ld16(win: &[u8], o: usize) -> uint8x16_t {
    debug_assert!(o + 16 <= win.len());
    // SAFETY: `o + 16 <= win.len()` (see above), so the 16 bytes `vld1q_u8` reads are in `win`.
    unsafe { vld1q_u8(win.as_ptr().add(o)) }
}

/// 8 samples of the window from `o` (checked as for [`ld16`], for 8 lanes).
#[target_feature(enable = "neon")]
fn ld8(win: &[u8], o: usize) -> uint8x8_t {
    debug_assert!(o + 8 <= win.len());
    // SAFETY: `o + 8 <= win.len()` (checked by `luma` / `chroma` for every offset they load).
    unsafe { vld1_u8(win.as_ptr().add(o)) }
}

/// a - 5b + 20c + 20d - 5e + f, unclipped, in 16-bit lanes (wrapping u16 arithmetic gives the
/// exact signed result, which fits).
#[target_feature(enable = "neon")]
fn tap6_8(v: [uint8x8_t; 6]) -> int16x8_t {
    let [a, b, c, d, e, f] = v;
    vreinterpretq_s16_u16(vmlsq_n_u16(vmlaq_n_u16(vaddl_u8(a, f), vaddl_u8(c, d), 20), vaddl_u8(b, e), 5))
}

#[target_feature(enable = "neon")]
fn tap6_16(v: [uint8x16_t; 6]) -> [int16x8_t; 2] {
    let [a, b, c, d, e, f] = v;
    let lo = tap6_8([a, b, c, d, e, f].map(|x| vget_low_u8(x)));
    let hi = vmlsq_n_u16(vmlaq_n_u16(vaddl_high_u8(a, f), vaddl_high_u8(c, d), 20), vaddl_high_u8(b, e), 5);
    [lo, vreinterpretq_s16_u16(hi)]
}

/// Clip1((t + 16) >> 5).
#[target_feature(enable = "neon")]
fn half8(t: int16x8_t) -> uint8x8_t {
    vqrshrun_n_s16::<5>(t)
}

#[target_feature(enable = "neon")]
fn half16(t: [int16x8_t; 2]) -> uint8x16_t {
    vcombine_u8(half8(t[0]), half8(t[1]))
}

/// The centre sample j of 8 lanes: Clip1((6-tap of the unclipped horizontal half samples of
/// rows k[0..6] + 512) >> 10), in 32-bit lanes.
#[target_feature(enable = "neon")]
fn centre8(k: [int16x8_t; 6]) -> uint8x8_t {
    let [t0, t1, t2, t3, t4, t5] = k;
    let lo = vmlsl_n_s16(
        vmlsl_n_s16(vmlal_n_s16(vmlal_n_s16(vaddl_s16(vget_low_s16(t0), vget_low_s16(t5)), vget_low_s16(t2), 20), vget_low_s16(t3), 20), vget_low_s16(t1), 5),
        vget_low_s16(t4),
        5,
    );
    let hi = vmlsl_high_n_s16(vmlsl_high_n_s16(vmlal_high_n_s16(vmlal_high_n_s16(vaddl_high_s16(t0, t5), t2, 20), t3, 20), t1, 5), t4, 5);
    vqmovn_u16(vcombine_u16(vqrshrun_n_s32::<10>(lo), vqrshrun_n_s32::<10>(hi)))
}

#[target_feature(enable = "neon")]
fn centre16(k: [[int16x8_t; 2]; 6]) -> uint8x16_t {
    vcombine_u8(centre8(k.map(|v| v[0])), centre8(k.map(|v| v[1])))
}

/// `p` combined with `d` (the first prediction, for AVG and WEIGHT_BI) by output mode `M`.
#[target_feature(enable = "neon")]
fn combine8<const M: u8>(d: uint8x8_t, p: uint8x8_t, wt: Wt) -> uint8x8_t {
    match M {
        PUT => p,
        AVG => vrhadd_u8(d, p),
        WEIGHT => {
            // |p * w| <= 32640, plus the rounding (at most 64): 16-bit lanes; the offset is added
            // saturating, which clips the same as Clip1 would.
            let x = vreinterpretq_s16_u16(vmovl_u8(p));
            let rnd = if wt.log_wd >= 1 { 1i16 << (wt.log_wd - 1) } else { 0 };
            let v = vshlq_s16(vaddq_s16(vmulq_n_s16(x, wt.w0), vdupq_n_s16(rnd)), vdupq_n_s16(-wt.log_wd));
            vqmovun_s16(vqaddq_s16(v, vdupq_n_s16(wt.o)))
        }
        _ => {
            let x = vreinterpretq_s16_u16(vmovl_u8(d));
            let y = vreinterpretq_s16_u16(vmovl_u8(p));
            let (rnd, sh) = (vdupq_n_s32(1 << wt.log_wd), vdupq_n_s32(-(wt.log_wd as i32 + 1)));
            let lo = vshlq_s32(vaddq_s32(vmlal_n_s16(vmull_n_s16(vget_low_s16(x), wt.w0), vget_low_s16(y), wt.w1), rnd), sh);
            let hi = vshlq_s32(vaddq_s32(vmlal_high_n_s16(vmull_high_n_s16(x, wt.w0), y, wt.w1), rnd), sh);
            vqmovun_s16(vqaddq_s16(vcombine_s16(vqmovn_s32(lo), vqmovn_s32(hi)), vdupq_n_s16(wt.o)))
        }
    }
}

/// One output row of `w` (2, 4 or 8) samples from `o`, written by mode `M`. The kernels' callers
/// checked that `dst` holds every row (`dst_fits`: o + w <= dst.len()); rows narrower than 8 are
/// read and written as one integer.
#[target_feature(enable = "neon")]
fn out8<const M: u8>(dst: &mut [u8], o: usize, w: usize, p: uint8x8_t, wt: Wt) {
    match w {
        8 => {
            debug_assert!(o + 8 <= dst.len());
            // SAFETY: o + 8 <= dst.len() (`dst_fits` for 8-wide blocks), the 8 bytes `vld1_u8`
            // reads and `vst1_u8` writes.
            unsafe {
                let d = dst.as_mut_ptr().add(o);
                let old = if M == AVG || M == WEIGHT_BI { vld1_u8(d) } else { p };
                vst1_u8(d, combine8::<M>(old, p, wt));
            }
        }
        4 => {
            let Some(d) = dst.get_mut(o..).and_then(|d| d.first_chunk_mut::<4>()) else {
                return;
            };
            let old = vcreate_u8(u32::from_le_bytes(*d) as u64);
            *d = vget_lane_u32::<0>(vreinterpret_u32_u8(combine8::<M>(old, p, wt))).to_le_bytes();
        }
        _ => {
            let Some(d) = dst.get_mut(o..).and_then(|d| d.first_chunk_mut::<2>()) else {
                return;
            };
            let old = vcreate_u8(u16::from_le_bytes(*d) as u64);
            *d = vget_lane_u16::<0>(vreinterpret_u16_u8(combine8::<M>(old, p, wt))).to_le_bytes();
        }
    }
}

/// One output row of 16 samples from `o` (o + 16 <= dst.len(), checked by `luma` with
/// `dst_fits`), written by mode `M`.
#[target_feature(enable = "neon")]
fn out16<const M: u8>(dst: &mut [u8], o: usize, p: uint8x16_t, wt: Wt) {
    debug_assert!(o + 16 <= dst.len());
    // SAFETY: o + 16 <= dst.len() (see above).
    let d = unsafe { dst.as_mut_ptr().add(o) };
    if M == PUT {
        // SAFETY: `d` points to the 16 bytes of `dst` that `vst1q_u8` writes.
        unsafe { vst1q_u8(d, p) };
        return;
    }
    // SAFETY: `d` points to the 16 bytes of `dst` that `vld1q_u8` reads and `vst1q_u8` writes.
    unsafe {
        let old = vld1q_u8(d);
        let v = if M == AVG {
            vrhaddq_u8(old, p)
        } else {
            vcombine_u8(combine8::<M>(vget_low_u8(old), vget_low_u8(p), wt), combine8::<M>(vget_high_u8(old), vget_high_u8(p), wt))
        };
        vst1q_u8(d, v);
    }
}

/// The luma prediction of one block (all 16 sample positions, as the scalar version), over
/// vectors of `$ld` / `$tap6` / `$half` / `$centre`, written by `$out`.
macro_rules! luma_kernel {
    ($name:ident, $ld:ident, $tap6:ident, $half:ident, $centre:ident, $avg:ident, $zero:expr, ($($w:ident: $wt:ty),*), $out:ident $(, $outw:ident)?) => {
        #[target_feature(enable = "neon")]
        fn $name<const M: u8>((win, ws): (&[u8], usize), (fx, fy): (u32, u32), $($w: $wt,)* h: usize, (dst, ds): (&mut [u8], usize), wt: Wt) {
            let ld = |r: usize, c: usize| $ld(win, r * ws + c);
            let tap_h = |r: usize| $tap6([ld(r, 0), ld(r, 1), ld(r, 2), ld(r, 3), ld(r, 4), ld(r, 5)]);
            let tap_v = |r: usize, c: usize| $tap6([ld(r, c), ld(r + 1, c), ld(r + 2, c), ld(r + 3, c), ld(r + 4, c), ld(r + 5, c)]);
            // Sample G of block row r is window row r + 2, column 2.
            let g = |r: usize, dr: usize, dc: usize| ld(r + 2 + dr, 2 + dc);
            let mut put = |r: usize, v| $out::<M>(dst, r * ds, $($outw,)? v, wt);
            match (fx, fy) {
                (0, 0) => {
                    for r in 0..h {
                        put(r, g(r, 0, 0));
                    }
                }
                (_, 0) => {
                    for r in 0..h {
                        let b = $half(tap_h(r + 2));
                        let v = match fx {
                            2 => b,
                            1 => $avg(g(r, 0, 0), b),
                            _ => $avg(g(r, 0, 1), b),
                        };
                        put(r, v);
                    }
                }
                (0, _) => {
                    for r in 0..h {
                        let hv = $half(tap_v(r, 2));
                        let v = match fy {
                            2 => hv,
                            1 => $avg(g(r, 0, 0), hv),
                            _ => $avg(g(r, 1, 0), hv),
                        };
                        put(r, v);
                    }
                }
                (2, _) | (_, 2) => {
                    // Unclipped horizontal half samples of window rows 0..h + 5, once per block.
                    let mut b1 = [$zero; 21];
                    for (k, b) in b1.iter_mut().enumerate().take(h + 5) {
                        *b = tap_h(k);
                    }
                    for r in 0..h {
                        let k = std::array::from_fn(|i| b1[(r + i).min(20)]);
                        let j = $centre(k);
                        let v = match (fx, fy) {
                            (2, 2) => j,
                            (2, 1) => $avg($half(k[2]), j),
                            (2, _) => $avg($half(k[3]), j),
                            _ => $avg($half(tap_v(r, if fx == 1 { 2 } else { 3 })), j),
                        };
                        put(r, v);
                    }
                }
                _ => {
                    // e, g, p, r: a horizontal half sample of row r + 2 or r + 3 and a vertical
                    // one of column c + 2 or c + 3.
                    let hoff = if fy == 1 { 2 } else { 3 };
                    let voff = if fx == 1 { 2 } else { 3 };
                    for r in 0..h {
                        put(r, $avg($half(tap_h(r + hoff)), $half(tap_v(r, voff))));
                    }
                }
            }
        }
    };
}

luma_kernel!(luma16, ld16, tap6_16, half16, centre16, vrhaddq_u8, [vdupq_n_s16(0); 2], (), out16);
luma_kernel!(luma8, ld8, tap6_8, half8, centre8, vrhadd_u8, vdupq_n_s16(0), (w: usize), out8, w);

/// See [`crate::h264_chroma`]; false when the window is too small (9 samples are loaded from
/// each row's start), `dst` too small or the weights out of range.
pub(crate) fn chroma((win, ws): (&[u8], usize), (fx, fy): (u32, u32), (w, h): (usize, usize), (dst, ds): (&mut [u8], usize), out: H264Out) -> bool {
    let Some((m, wt)) = mode(out) else {
        return false;
    };
    let fits = matches!(w, 2 | 4 | 8)
        && (1..=8).contains(&h)
        && fx < 8
        && fy < 8
        && h.checked_mul(ws).and_then(|n| n.checked_add(9)).is_some_and(|n| n <= win.len())
        && dst_fits(dst, ds, w, h);
    if !fits {
        return false;
    }
    // SAFETY: NEON is part of the aarch64 baseline: every core this build runs on has it (every arm).
    unsafe {
        match m {
            PUT => chroma_neon::<PUT>((win, ws), (fx, fy), (w, h), (dst, ds), wt),
            AVG => chroma_neon::<AVG>((win, ws), (fx, fy), (w, h), (dst, ds), wt),
            WEIGHT => chroma_neon::<WEIGHT>((win, ws), (fx, fy), (w, h), (dst, ds), wt),
            _ => chroma_neon::<WEIGHT_BI>((win, ws), (fx, fy), (w, h), (dst, ds), wt),
        }
    }
    true
}

#[target_feature(enable = "neon")]
fn chroma_neon<const M: u8>((win, ws): (&[u8], usize), (fx, fy): (u32, u32), (w, h): (usize, usize), (dst, ds): (&mut [u8], usize), wt: Wt) {
    // Weights at most 64: the sums stay below 2^14.
    let [a, b, c, d] = [(8 - fx) * (8 - fy), fx * (8 - fy), (8 - fx) * fy, fx * fy].map(|v| vdup_n_u8(v as u8));
    for r in 0..h {
        let (o0, o1) = (r * ws, (r + 1) * ws);
        let acc = vmlal_u8(vmlal_u8(vmlal_u8(vmull_u8(ld8(win, o0), a), ld8(win, o0 + 1), b), ld8(win, o1), c), ld8(win, o1 + 1), d);
        out8::<M>(dst, r * ds, w, vrshrn_n_u16::<6>(acc), wt);
    }
}

/// See [`crate::h264_chroma_pair`]; false as [`chroma`] for either plane.
pub(crate) fn chroma_pair(
    (win, ws): ([&[u8]; 2], usize),
    (fx, fy): (u32, u32),
    (w, h): (usize, usize),
    (dst, ds): ([&mut [u8]; 2], usize),
    out: H264Out,
) -> bool {
    let Some((m, wt)) = mode(out) else {
        return false;
    };
    let fits = matches!(w, 2 | 4 | 8)
        && (1..=8).contains(&h)
        && fx < 8
        && fy < 8
        && win.iter().all(|win| h.checked_mul(ws).and_then(|n| n.checked_add(9)).is_some_and(|n| n <= win.len()))
        && dst.iter().all(|d| dst_fits(d, ds, w, h));
    if !fits {
        return false;
    }
    // SAFETY: NEON is part of the aarch64 baseline: every core this build runs on has it (every arm).
    unsafe {
        match m {
            PUT => chroma_pair_neon::<PUT>((win, ws), (fx, fy), (w, h), (dst, ds), wt),
            AVG => chroma_pair_neon::<AVG>((win, ws), (fx, fy), (w, h), (dst, ds), wt),
            WEIGHT => chroma_pair_neon::<WEIGHT>((win, ws), (fx, fy), (w, h), (dst, ds), wt),
            _ => chroma_pair_neon::<WEIGHT_BI>((win, ws), (fx, fy), (w, h), (dst, ds), wt),
        }
    }
    true
}

/// [`chroma_neon`] of Cb (lanes 0..8) and Cr (lanes 8..16) at once; each window row is loaded
/// once, as the bottom row of one output row and the top row of the next.
#[target_feature(enable = "neon")]
fn chroma_pair_neon<const M: u8>((win, ws): ([&[u8]; 2], usize), (fx, fy): (u32, u32), (w, h): (usize, usize), (dst, ds): ([&mut [u8]; 2], usize), wt: Wt) {
    let [a, b, c, d] = [(8 - fx) * (8 - fy), fx * (8 - fy), (8 - fx) * fy, fx * fy].map(|v| vdupq_n_u8(v as u8));
    let [wb, wr] = win;
    let [db, dr] = dst;
    // window row r: its samples from columns 0 and 1, Cb and Cr
    let row = |r: usize| {
        let o = r * ws;
        (vcombine_u8(ld8(wb, o), ld8(wr, o)), vcombine_u8(ld8(wb, o + 1), ld8(wr, o + 1)))
    };
    let (mut t0, mut t1) = row(0);
    for r in 0..h {
        let (b0, b1) = row(r + 1);
        // weights at most 64: the sums stay below 2^14
        let lo = vmlal_u8(
            vmlal_u8(vmlal_u8(vmull_u8(vget_low_u8(t0), vget_low_u8(a)), vget_low_u8(t1), vget_low_u8(b)), vget_low_u8(b0), vget_low_u8(c)),
            vget_low_u8(b1),
            vget_low_u8(d),
        );
        let hi = vmlal_high_u8(vmlal_high_u8(vmlal_high_u8(vmull_high_u8(t0, a), t1, b), b0, c), b1, d);
        out8::<M>(db, r * ds, w, vrshrn_n_u16::<6>(lo), wt);
        out8::<M>(dr, r * ds, w, vrshrn_n_u16::<6>(hi), wt);
        (t0, t1) = (b0, b1);
    }
}

/// See [`crate::h264_deblock`]; false when the edge's samples do not fit `pix`.
pub(crate) fn deblock(pix: &mut [u8], stride: usize, x: usize, y: usize, vertical: bool, chroma: bool, e: &crate::H264Edge) -> bool {
    let n = if chroma { 8 } else { 16 };
    // The last byte read or written: a vertical edge reads lines y .. y + n at x - 4 .. x + 4, a
    // horizontal one rows y - 4 .. y + 4 at x .. x + n.
    let end = if vertical {
        if x < 4 { None } else { (y + n - 1).checked_mul(stride).and_then(|o| o.checked_add(x + 4)) }
    } else if y < 4 {
        None
    } else {
        (y + 3).checked_mul(stride).and_then(|o| o.checked_add(x + n))
    };
    let fits = end.is_some_and(|end| end <= pix.len());
    if fits {
        // SAFETY: NEON is part of the aarch64 baseline: every core this build runs on has it.
        unsafe { deblock_neon(pix, stride, x, y, vertical, chroma, e) };
    }
    fits
}

/// 8 bytes of `pix` from `o`; `deblock` checked every offset `deblock_neon` passes.
#[target_feature(enable = "neon")]
fn px8(pix: &[u8], o: usize) -> uint8x8_t {
    debug_assert!(o + 8 <= pix.len());
    // SAFETY: `o + 8 <= pix.len()` (checked by `deblock` for every row of the edge).
    unsafe { vld1_u8(pix.as_ptr().add(o)) }
}

#[target_feature(enable = "neon")]
fn px16(pix: &[u8], o: usize) -> uint8x16_t {
    debug_assert!(o + 16 <= pix.len());
    // SAFETY: `o + 16 <= pix.len()` (checked by `deblock` for every row of a luma edge).
    unsafe { vld1q_u8(pix.as_ptr().add(o)) }
}

#[target_feature(enable = "neon")]
fn put8(pix: &mut [u8], o: usize, v: uint8x8_t) {
    debug_assert!(o + 8 <= pix.len());
    // SAFETY: as `px8`.
    unsafe { vst1_u8(pix.as_mut_ptr().add(o), v) }
}

#[target_feature(enable = "neon")]
fn put16(pix: &mut [u8], o: usize, v: uint8x16_t) {
    debug_assert!(o + 16 <= pix.len());
    // SAFETY: as `px16`.
    unsafe { vst1q_u8(pix.as_mut_ptr().add(o), v) }
}

/// The 8x8 byte transpose (its own inverse): row i of the result is column i of `a`.
#[target_feature(enable = "neon")]
fn transpose8(a: [uint8x8_t; 8]) -> [uint8x8_t; 8] {
    let b = [
        vtrn1_u8(a[0], a[1]),
        vtrn2_u8(a[0], a[1]),
        vtrn1_u8(a[2], a[3]),
        vtrn2_u8(a[2], a[3]),
        vtrn1_u8(a[4], a[5]),
        vtrn2_u8(a[4], a[5]),
        vtrn1_u8(a[6], a[7]),
        vtrn2_u8(a[6], a[7]),
    ];
    let h = b.map(|v| vreinterpret_u16_u8(v));
    let c = [
        vtrn1_u16(h[0], h[2]),
        vtrn1_u16(h[1], h[3]),
        vtrn2_u16(h[0], h[2]),
        vtrn2_u16(h[1], h[3]),
        vtrn1_u16(h[4], h[6]),
        vtrn1_u16(h[5], h[7]),
        vtrn2_u16(h[4], h[6]),
        vtrn2_u16(h[5], h[7]),
    ];
    let w = c.map(|v| vreinterpret_u32_u16(v));
    let d = [
        vtrn1_u32(w[0], w[4]),
        vtrn1_u32(w[1], w[5]),
        vtrn1_u32(w[2], w[6]),
        vtrn1_u32(w[3], w[7]),
        vtrn2_u32(w[0], w[4]),
        vtrn2_u32(w[1], w[5]),
        vtrn2_u32(w[2], w[6]),
        vtrn2_u32(w[3], w[7]),
    ];
    d.map(|v| vreinterpret_u8_u32(v))
}

/// Round(weighted sum of `t` / 2^SH) of samples, the sums in 16-bit lanes (at most 8 * 255).
#[target_feature(enable = "neon")]
fn wsum<const SH: i32>(t: &[(uint8x16_t, u8)]) -> uint8x16_t {
    let (mut lo, mut hi) = (vdupq_n_u16(0), vdupq_n_u16(0));
    for &(v, w) in t {
        lo = vmlal_u8(lo, vget_low_u8(v), vdup_n_u8(w));
        hi = vmlal_high_u8(hi, v, vdupq_n_u8(w));
    }
    vcombine_u8(vrshrn_n_u16::<SH>(lo), vrshrn_n_u16::<SH>(hi))
}

/// Clip3(-tc, tc, ((q0 - p0) * 4 + (p1 - q1) + 4) >> 3), in 16-bit lanes, narrowed (|tc| < 28).
#[target_feature(enable = "neon")]
fn delta(p1: uint8x16_t, p0: uint8x16_t, q0: uint8x16_t, q1: uint8x16_t, tc: uint8x16_t) -> int8x16_t {
    let half = |qp: uint16x8_t, pq: uint16x8_t, tc: uint16x8_t| {
        let d = vshrq_n_s16::<3>(vaddq_s16(vaddq_s16(vshlq_n_s16::<2>(vreinterpretq_s16_u16(qp)), vreinterpretq_s16_u16(pq)), vdupq_n_s16(4)));
        let tc = vreinterpretq_s16_u16(tc);
        vmovn_s16(vminq_s16(vmaxq_s16(d, vnegq_s16(tc)), tc))
    };
    let lo = half(vsubl_u8(vget_low_u8(q0), vget_low_u8(p0)), vsubl_u8(vget_low_u8(p1), vget_low_u8(q1)), vmovl_u8(vget_low_u8(tc)));
    let hi = half(vsubl_high_u8(q0, p0), vsubl_high_u8(p1, q1), vmovl_high_u8(tc));
    vcombine_s8(lo, hi)
}

/// [`crate::h264::filter_lanes`] on 16 lanes of bytes: 16 luma lines, or 8 chroma lines in the
/// first 8 lanes, or (`pair`) the 8 lines of the Cb edge and then those of the Cr edge.
#[target_feature(enable = "neon")]
fn filter(s: &mut [uint8x16_t; 8], e: &crate::H264Edge, chroma: bool, pair: bool) {
    let [p3, p2, p1, p0, q0, q1, q2, q3] = *s;
    let per = if chroma { 2 } else { 4 };
    let (mut on_b, mut tc_b) = ([0u8; 16], [0u8; 16]);
    for i in 0..16 {
        let seg = if pair { (i & 7) / per } else { i / per };
        if let (Some(&b), Some(&t)) = (e.bs.get(seg), e.tc0.get(seg)) {
            on_b[i] = if b != 0 { 0xff } else { 0 };
            tc_b[i] = t;
        }
    }
    let (bs_on, tc0): (uint8x16_t, uint8x16_t) = (bytemuck::must_cast(on_b), bytemuck::must_cast(tc_b));
    let (alpha, beta) = (vdupq_n_u8(e.alpha), vdupq_n_u8(e.beta));
    let on = vandq_u8(vandq_u8(bs_on, vcltq_u8(vabdq_u8(p0, q0), alpha)), vandq_u8(vcltq_u8(vabdq_u8(p1, p0), beta), vcltq_u8(vabdq_u8(q1, q0), beta)));
    if vmaxvq_u8(on) == 0 {
        return;
    }
    // bS 4 is only ever assigned to all four segments of an edge.
    let strong_edge = e.bs[0] >= 4;
    if chroma {
        let (np0, nq0) = if strong_edge {
            (wsum::<2>(&[(p1, 2), (p0, 1), (q1, 1)]), wsum::<2>(&[(q1, 2), (q0, 1), (p1, 1)]))
        } else {
            let d = delta(p1, p0, q0, q1, vaddq_u8(tc0, vdupq_n_u8(1)));
            (vsqaddq_u8(p0, d), vsqaddq_u8(q0, vnegq_s8(d)))
        };
        s[3] = vbslq_u8(on, np0, p0);
        s[4] = vbslq_u8(on, nq0, q0);
        return;
    }
    let ap = vcltq_u8(vabdq_u8(p2, p0), beta);
    let aq = vcltq_u8(vabdq_u8(q2, q0), beta);
    if strong_edge {
        let strong = vcltq_u8(vabdq_u8(p0, q0), vdupq_n_u8((e.alpha >> 2) + 2));
        let sp = vandq_u8(vandq_u8(on, ap), strong);
        let sq = vandq_u8(vandq_u8(on, aq), strong);
        let wp0 = wsum::<2>(&[(p1, 2), (p0, 1), (q1, 1)]);
        let wq0 = wsum::<2>(&[(q1, 2), (q0, 1), (p1, 1)]);
        s[3] = vbslq_u8(sp, wsum::<3>(&[(p2, 1), (p1, 2), (p0, 2), (q0, 2), (q1, 1)]), vbslq_u8(on, wp0, p0));
        s[2] = vbslq_u8(sp, wsum::<2>(&[(p2, 1), (p1, 1), (p0, 1), (q0, 1)]), p1);
        s[1] = vbslq_u8(sp, wsum::<3>(&[(p3, 2), (p2, 3), (p1, 1), (p0, 1), (q0, 1)]), p2);
        s[4] = vbslq_u8(sq, wsum::<3>(&[(p1, 1), (p0, 2), (q0, 2), (q1, 2), (q2, 1)]), vbslq_u8(on, wq0, q0));
        s[5] = vbslq_u8(sq, wsum::<2>(&[(p0, 1), (q0, 1), (q1, 1), (q2, 1)]), q1);
        s[6] = vbslq_u8(sq, wsum::<3>(&[(q3, 2), (q2, 3), (q1, 1), (q0, 1), (p0, 1)]), q2);
    } else {
        // tc = tC0 + (ap ? 1 : 0) + (aq ? 1 : 0): the masks are 0xff, so subtracting adds 1.
        let tc = vsubq_u8(vsubq_u8(tc0, ap), aq);
        let d = delta(p1, p0, q0, q1, tc);
        let avg = vrhaddq_u8(p0, q0);
        // p1 + Clip3(-tC0, tC0, (p2 + avg - 2 * p1) >> 1) = Clip3(p1 - tC0, p1 + tC0, (p2 + avg) >> 1).
        let np1 = vminq_u8(vmaxq_u8(vhaddq_u8(p2, avg), vqsubq_u8(p1, tc0)), vqaddq_u8(p1, tc0));
        let nq1 = vminq_u8(vmaxq_u8(vhaddq_u8(q2, avg), vqsubq_u8(q1, tc0)), vqaddq_u8(q1, tc0));
        s[3] = vbslq_u8(on, vsqaddq_u8(p0, d), p0);
        s[4] = vbslq_u8(on, vsqaddq_u8(q0, vnegq_s8(d)), q0);
        s[2] = vbslq_u8(vandq_u8(on, ap), np1, p1);
        s[5] = vbslq_u8(vandq_u8(on, aq), nq1, q1);
    }
}

#[target_feature(enable = "neon")]
fn deblock_neon(pix: &mut [u8], stride: usize, x: usize, y: usize, vertical: bool, chroma: bool, e: &crate::H264Edge) {
    let zero = vdup_n_u8(0);
    let mut s = [vdupq_n_u8(0); 8];
    if vertical {
        // Line i's samples p3 .. q3 are the 8 bytes from x - 4 of row y + i: transposed, vector k
        // holds sample k of every line.
        let rows = |i0: usize| transpose8(std::array::from_fn(|i| px8(pix, (y + i0 + i) * stride + x - 4)));
        let lo = rows(0);
        let hi = if chroma { [zero; 8] } else { rows(8) };
        for k in 0..8 {
            s[k] = vcombine_u8(lo[k], hi[k]);
        }
        filter(&mut s, e, chroma, false);
        let lo = transpose8(s.map(|v| vget_low_u8(v)));
        for (i, v) in lo.into_iter().enumerate() {
            put8(pix, (y + i) * stride + x - 4, v);
        }
        if !chroma {
            let hi = transpose8(s.map(|v| vget_high_u8(v)));
            for (i, v) in hi.into_iter().enumerate() {
                put8(pix, (y + 8 + i) * stride + x - 4, v);
            }
        }
    } else {
        let base = (y - 4) * stride + x;
        for (k, v) in s.iter_mut().enumerate() {
            *v = if chroma { vcombine_u8(px8(pix, base + k * stride), zero) } else { px16(pix, base + k * stride) };
        }
        filter(&mut s, e, chroma, false);
        // Only p2 .. q2 (luma) or p0, q0 (chroma) can change.
        if chroma {
            for k in 3..5 {
                put8(pix, base + k * stride, vget_low_u8(s[k]));
            }
        } else {
            for k in 1..7 {
                put16(pix, base + k * stride, s[k]);
            }
        }
    }
}

/// See [`crate::h264_deblock_chroma_pair`]; false when the edge's samples do not fit both planes.
pub(crate) fn deblock_pair(cb: &mut [u8], cr: &mut [u8], stride: usize, x: usize, y: usize, vertical: bool, e: &crate::H264Edge) -> bool {
    // the last byte read or written, as in `deblock` for 8 chroma lines
    let end = if vertical {
        if x < 4 { None } else { (y + 7).checked_mul(stride).and_then(|o| o.checked_add(x + 4)) }
    } else if y < 4 {
        None
    } else {
        (y + 3).checked_mul(stride).and_then(|o| o.checked_add(x + 8))
    };
    let fits = end.is_some_and(|end| end <= cb.len() && end <= cr.len());
    if fits {
        // SAFETY: NEON is part of the aarch64 baseline: every core this build runs on has it.
        unsafe { deblock_pair_neon(cb, cr, stride, x, y, vertical, e) };
    }
    fits
}

/// The Cb edge in lanes 0..8 and the Cr edge in lanes 8..16 of one [`filter`].
#[target_feature(enable = "neon")]
fn deblock_pair_neon(cb: &mut [u8], cr: &mut [u8], stride: usize, x: usize, y: usize, vertical: bool, e: &crate::H264Edge) {
    let mut s = [vdupq_n_u8(0); 8];
    if vertical {
        let rows = |pix: &[u8]| transpose8(std::array::from_fn(|i| px8(pix, (y + i) * stride + x - 4)));
        let (lo, hi) = (rows(cb), rows(cr));
        for k in 0..8 {
            s[k] = vcombine_u8(lo[k], hi[k]);
        }
        filter(&mut s, e, true, true);
        let (lo, hi) = (transpose8(s.map(|v| vget_low_u8(v))), transpose8(s.map(|v| vget_high_u8(v))));
        for i in 0..8 {
            put8(cb, (y + i) * stride + x - 4, lo[i]);
            put8(cr, (y + i) * stride + x - 4, hi[i]);
        }
    } else {
        let base = (y - 4) * stride + x;
        for (k, v) in s.iter_mut().enumerate() {
            *v = vcombine_u8(px8(cb, base + k * stride), px8(cr, base + k * stride));
        }
        filter(&mut s, e, true, true);
        // only p0 and q0 can change
        for k in 3..5 {
            put8(cb, base + k * stride, vget_low_u8(s[k]));
            put8(cr, base + k * stride, vget_high_u8(s[k]));
        }
    }
}
