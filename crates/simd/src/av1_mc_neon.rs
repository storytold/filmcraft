//! NEON kernel for AV1 block inter prediction without reference scaling (7.11.3.4): both subpel
//! passes of a block in one call, the intermediate rows kept in registers, the result written
//! straight into the picture (a single prediction) or into the compound buffer.
//!
//! The block goes down strips of 8 columns (one of 4 for 4-wide blocks). The horizontal pass of a
//! row is two loads and `vext`s for the taps; the last eight of its rows are the vertical pass's
//! window, which slides down the strip one row per output row. For 8-bit samples with
//! InterRound0 = 3 and taps whose positive ones add up to at most 192 and negative ones to at least
//! -64 (all of AV1's filters), the horizontal sums lie in [-16320, 48960]: they are kept modulo
//! 2^16 plus 16388 (the rounding term and an offset that keeps them in 0..65536 and is a multiple
//! of 8), so the pass is one 16-bit multiply-accumulate per tap; otherwise it accumulates in 32
//! bits. A phase-0 (identity) filter skips its pass: Round2(128 * x, n) is x << (7 - n) for n <= 7
//! and the vertical pass's Round2 of 128 times the row otherwise. The vertical sums need 32 bits.

use core::arch::aarch64::*;

use crate::Av1McOut;

/// How a horizontal row is made.
#[derive(Clone, Copy, PartialEq, Eq)]
enum HPass {
    /// 8-bit samples, InterRound0 3, taps that keep the sums modulo 2^16 exact.
    Wrap,
    /// 32-bit sums.
    Wide,
    /// The identity filter: x << (7 - InterRound0).
    Copy,
}

/// See [`crate::av1_mc_at`]; `wrap` says the 16-bit horizontal pass is exact.
pub(crate) fn mc(
    (src, ss): (&[u16], usize),
    (hf, vf): (&[i16; 8], &[i16; 8]),
    (w, h): (usize, usize),
    wrap: bool,
    (r0, r1): (u32, u32),
    out: Av1McOut,
) -> bool {
    if !matches!(w, 4 | 8 | 16 | 32 | 64 | 128) || !(1..=128).contains(&h) || r0 > 7 || !(1..=16).contains(&r1) {
        return false;
    }
    // Every row load is 16 samples from a strip's first column, h + 7 rows `ss` apart.
    let last = if w == 4 { 0 } else { w - 8 };
    let src_ok = (h + 6).checked_mul(ss).and_then(|n| n.checked_add(last + 16)).is_some_and(|n| n <= src.len());
    let fits = |len: usize, stride: usize| stride >= w && (h - 1).checked_mul(stride).and_then(|n| n.checked_add(w)).is_some_and(|n| n <= len);
    let dst_ok = match &out {
        Av1McOut::Put { dst, ds, .. } => fits(dst.len(), *ds),
        Av1McOut::Pred { out, os } => fits(out.len(), *os),
        Av1McOut::Avg { first, fs, dst, ds, shift, .. } => fits(first.len(), *fs) && fits(dst.len(), *ds) && (1..=16).contains(shift),
    };
    if !src_ok || !dst_ok {
        return false;
    }
    let identity = |f: &[i16; 8]| *f == [0, 0, 0, 128, 0, 0, 0, 0];
    let hp = if identity(hf) {
        HPass::Copy
    } else if wrap {
        HPass::Wrap
    } else {
        HPass::Wide
    };
    let v_copy = identity(vf);
    // SAFETY: NEON is part of the aarch64 baseline: every core this build runs on has it.
    unsafe { block((src, ss), (hf, vf), (w, h), hp, v_copy, (r0, r1), out) };
    true
}

/// 8 samples of `s` from `o`; `mc` checked that every load of the block is in `s`.
#[inline]
#[target_feature(enable = "neon")]
fn ld(s: &[u16], o: usize) -> uint16x8_t {
    debug_assert!(o + 8 <= s.len());
    // SAFETY: o + 8 <= s.len() (checked by `mc`: each row's loads end at most 16 samples past
    // the last strip's first column), the 8 samples `vld1q_u16` reads.
    unsafe { vld1q_u16(s.as_ptr().add(o)) }
}

/// The taps of `f` broadcast, as u16 (two's complement) and as i16.
#[inline]
#[target_feature(enable = "neon")]
fn taps(f: &[i16; 8]) -> [int16x8_t; 8] {
    f.map(|c| vdupq_n_s16(c))
}

/// The horizontal pass of the 8 samples from `o` of a source row (16 samples loaded).
#[inline]
#[target_feature(enable = "neon")]
fn hrow<const S: usize, const E: usize>(s: &[u16], o: usize, k: &[int16x8_t; 8], f: &[i16; 8], hp: HPass, sh0: int32x4_t, up: int16x8_t) -> int16x8_t {
    let (a, b) = (ld(s, o), ld(s, o + 8));
    if hp == HPass::Copy {
        return vshlq_s16(vreinterpretq_s16_u16(vextq_u16::<3>(a, b)), up);
    }
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
    if hp == HPass::Wrap {
        let mut acc = vdupq_n_u16(16388);
        for t in S..E {
            acc = vmlaq_u16(acc, x[t], vreinterpretq_u16_s16(k[t]));
        }
        return vsubq_s16(vreinterpretq_s16_u16(vshrq_n_u16::<3>(acc)), vdupq_n_s16(2048));
    }
    let (mut lo, mut hi) = (vdupq_n_s32(0), vdupq_n_s32(0));
    for t in S..E {
        let v = vreinterpretq_s16_u16(x[t]);
        lo = vmlal_n_s16(lo, vget_low_s16(v), f[t]);
        hi = vmlal_high_n_s16(hi, v, f[t]);
    }
    vcombine_s16(vmovn_s32(vrshlq_s32(lo, sh0)), vmovn_s32(vrshlq_s32(hi, sh0)))
}

/// Writes the first N (8 or 4) of the 8 vertical sums `lo`, `hi` (already rounded) of output row
/// `r`, strip column `j`.
#[inline]
#[target_feature(enable = "neon")]
fn put<const N: usize>(out: &mut Av1McOut, r: usize, j: usize, lo: int32x4_t, hi: int32x4_t) {
    match out {
        Av1McOut::Put { dst, ds, max } => {
            let v = vminq_u16(vcombine_u16(vqmovun_s32(lo), vqmovun_s32(hi)), vdupq_n_u16(*max));
            let o = r * *ds + j;
            debug_assert!(o + N <= dst.len());
            // SAFETY: `mc` checked (h - 1) * ds + w <= dst.len(), and j + N <= w.
            unsafe {
                if N == 8 {
                    vst1q_u16(dst.as_mut_ptr().add(o), v);
                } else {
                    vst1_u16(dst.as_mut_ptr().add(o), vget_low_u16(v));
                }
            }
        }
        Av1McOut::Pred { out, os } => {
            let o = r * *os + j;
            debug_assert!(o + N <= out.len());
            // SAFETY: as for `Put`, with `os`.
            unsafe {
                vst1q_s32(out.as_mut_ptr().add(o), lo);
                if N == 8 {
                    vst1q_s32(out.as_mut_ptr().add(o + 4), hi);
                }
            }
        }
        Av1McOut::Avg { first, fs, dst, ds, max, shift } => {
            let f = r * *fs + j;
            debug_assert!(f + N <= first.len());
            // SAFETY: `mc` checked (h - 1) * fs + w <= first.len(), and j + N <= w.
            let (a, b) = unsafe { (vld1q_s32(first.as_ptr().add(f)), if N == 8 { vld1q_s32(first.as_ptr().add(f + 4)) } else { vdupq_n_s32(0) }) };
            let sh = vdupq_n_s32(-(*shift as i32));
            let (lo, hi) = (vrshlq_s32(vaddq_s32(lo, a), sh), vrshlq_s32(vaddq_s32(hi, b), sh));
            let v = vminq_u16(vcombine_u16(vqmovun_s32(lo), vqmovun_s32(hi)), vdupq_n_u16(*max));
            let o = r * *ds + j;
            debug_assert!(o + N <= dst.len());
            // SAFETY: as for `Put`.
            unsafe {
                if N == 8 {
                    vst1q_u16(dst.as_mut_ptr().add(o), v);
                } else {
                    vst1_u16(dst.as_mut_ptr().add(o), vget_low_u16(v));
                }
            }
        }
    }
}

/// The taps of `f` that can be nonzero: 0..8 (AV1's sharp filters), 1..7 (its regular and
/// smooth ones) or 2..6 (its 4-tap and bilinear ones, and the identity), as (start, end).
fn span(f: &[i16; 8]) -> (usize, usize) {
    if f[0] != 0 || f[7] != 0 {
        (0, 8)
    } else if f[1] != 0 || f[6] != 0 {
        (1, 7)
    } else {
        (2, 6)
    }
}

/// `strip::<N, HS, HE, VS, VE>` for the spans of the filters.
macro_rules! dispatch {
    ($n:literal, $hs:expr, $vs:expr, $args:tt) => {
        match ($hs, $vs) {
            ((0, _), (0, _)) => dispatch!(@call $n, 0, 8, 0, 8, $args),
            ((0, _), (1, _)) => dispatch!(@call $n, 0, 8, 1, 7, $args),
            ((0, _), _) => dispatch!(@call $n, 0, 8, 2, 6, $args),
            ((1, _), (0, _)) => dispatch!(@call $n, 1, 7, 0, 8, $args),
            ((1, _), (1, _)) => dispatch!(@call $n, 1, 7, 1, 7, $args),
            ((1, _), _) => dispatch!(@call $n, 1, 7, 2, 6, $args),
            (_, (0, _)) => dispatch!(@call $n, 2, 6, 0, 8, $args),
            (_, (1, _)) => dispatch!(@call $n, 2, 6, 1, 7, $args),
            _ => dispatch!(@call $n, 2, 6, 2, 6, $args),
        }
    };
    (@call $n:literal, $a:literal, $b:literal, $c:literal, $d:literal, ($($arg:expr),*)) => {
        strip::<$n, $a, $b, $c, $d>($($arg),*)
    };
}

/// What the strips of a block share: the source `s` (rows `ss` apart), the filters (the
/// horizontal one also broadcast, `kh`), the block height, the passes, and the rounding (`sh0`
/// and `sh1` the shifts by -InterRound0 and -InterRound1, `up` the one by 7 - InterRound0).
#[derive(Clone, Copy)]
struct Kernel<'a> {
    s: &'a [u16],
    ss: usize,
    kh: [int16x8_t; 8],
    hf: &'a [i16; 8],
    vf: &'a [i16; 8],
    h: usize,
    hp: HPass,
    v_copy: bool,
    sh0: int32x4_t,
    sh1: int32x4_t,
    up: int16x8_t,
}

#[target_feature(enable = "neon")]
fn block((s, ss): (&[u16], usize), (hf, vf): (&[i16; 8], &[i16; 8]), (w, h): (usize, usize), hp: HPass, v_copy: bool, (r0, r1): (u32, u32), mut out: Av1McOut) {
    let k =
        Kernel { s, ss, kh: taps(hf), hf, vf, h, hp, v_copy, sh0: vdupq_n_s32(-(r0 as i32)), sh1: vdupq_n_s32(-(r1 as i32)), up: vdupq_n_s16(7 - r0 as i16) };
    // The identity filter's one tap is 3, in every span.
    let (hs, vs) = (if hp == HPass::Copy { (2, 6) } else { span(hf) }, if v_copy { (2, 6) } else { span(vf) });
    let mut j = 0;
    while j < w {
        if w == 4 {
            dispatch!(4, hs, vs, (&k, j, &mut out));
        } else {
            dispatch!(8, hs, vs, (&k, j, &mut out));
        }
        j += 8;
    }
}

/// The columns j..j + N of the block, with the horizontal taps HS..HE and the vertical ones
/// VS..VE (the others are zero): output row r reads intermediate rows r + VS .. r + VE.
#[inline]
#[target_feature(enable = "neon")]
fn strip<const N: usize, const HS: usize, const HE: usize, const VS: usize, const VE: usize>(k: &Kernel, j: usize, out: &mut Av1McOut) {
    let &Kernel { s, ss, ref kh, hf, vf, h, hp, v_copy, sh0, sh1, up } = k;
    let row = |r: usize| hrow::<HS, HE>(s, r * ss + j, kh, hf, hp, sh0, up);
    if v_copy {
        // Only intermediate row r + 3 has a non-zero tap.
        for r in 0..h {
            let t = row(r + 3);
            put::<N>(out, r, j, vrshlq_s32(vshll_n_s16::<7>(vget_low_s16(t)), sh1), vrshlq_s32(vshll_high_n_s16::<7>(t), sh1));
        }
        return;
    }
    let mut win = [vdupq_n_s16(0); 8];
    for t in VS + 1..VE {
        win[t] = row(t - 1);
    }
    for r in 0..h {
        for t in VS..VE - 1 {
            win[t] = win[t + 1];
        }
        win[VE - 1] = row(r + VE - 1);
        let (mut lo, mut hi) = (vdupq_n_s32(0), vdupq_n_s32(0));
        for t in VS..VE {
            lo = vmlal_n_s16(lo, vget_low_s16(win[t]), vf[t]);
            hi = vmlal_high_n_s16(hi, win[t], vf[t]);
        }
        put::<N>(out, r, j, vrshlq_s32(lo, sh1), vrshlq_s32(hi, sh1));
    }
}
