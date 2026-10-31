//! NEON kernel for VP8 inter prediction (18.3): the six-tap (or bilinear, as six taps) sub-pixel
//! filters of a 16, 8 or 4 wide block, read in place from the reference and written straight
//! into the picture.
//!
//! Each pass is (sum + 64) >> 7 clipped to 0..=255, so the first pass's output is 8-bit again.
//! The sums of 8-bit pixels with taps whose positive ones add up to at most 192 and negative ones
//! to at least -64 ([`crate::vp8_wraps`]) lie in [-16256, 49024] with the rounding term, so they
//! are kept modulo 2^16, 8 lanes per vector: the non-negative ones stay below 0xC000 and the
//! negative ones (which clip to 0) land at or above it. Blocks go down strips of 8 columns (one of
//! 4 for 4-wide blocks); a two-dimensional filter keeps the last six horizontally filtered rows in
//! registers.

use core::arch::aarch64::*;

/// See [`crate::vp8_predict`].
pub(crate) fn predict(
    (src, ss): (&[u8], usize),
    (fh, fv): (Option<&[i16; 6]>, Option<&[i16; 6]>),
    (w, h): (usize, usize),
    (dst, ds): (&mut [u8], usize),
) -> bool {
    if !matches!(w, 4 | 8 | 16) || !(1..=16).contains(&h) {
        return false;
    }
    // Every load is 16 pixels from a strip's first column (h + 5 rows, `ss` apart).
    let last = if w == 16 { 8 } else { 0 };
    let src_ok = (h + 4).checked_mul(ss).and_then(|n| n.checked_add(last + 16)).is_some_and(|n| n <= src.len());
    let dst_ok = (h - 1).checked_mul(ds).and_then(|n| n.checked_add(w)).is_some_and(|n| n <= dst.len());
    if !src_ok || !dst_ok {
        return false;
    }
    // SAFETY: NEON is part of the aarch64 baseline: every core this build runs on has it.
    unsafe {
        let mut x = 0;
        while x < w {
            if w == 4 {
                strip::<4>((src, ss), (fh, fv), h, x, (dst, ds));
            } else {
                strip::<8>((src, ss), (fh, fv), h, x, (dst, ds));
            }
            x += 8;
        }
    }
    true
}

/// 16 pixels of `s` from `o`; `predict` checked that every row's loads are in `s`.
#[target_feature(enable = "neon")]
fn ld16(s: &[u8], o: usize) -> uint8x16_t {
    debug_assert!(o + 16 <= s.len());
    // SAFETY: o + 16 <= s.len() (checked by `predict`), the 16 bytes `vld1q_u8` reads.
    unsafe { vld1q_u8(s.as_ptr().add(o)) }
}

/// 8 pixels of `s` from `o`, o + 8 <= s.len().
#[target_feature(enable = "neon")]
fn ld8(s: &[u8], o: usize) -> uint8x8_t {
    debug_assert!(o + 8 <= s.len());
    // SAFETY: o + 8 <= s.len(): `row` reads from 2 past a strip's first column, whose 16 samples
    // `predict` checked are in `s`.
    unsafe { vld1_u8(s.as_ptr().add(o)) }
}

/// The taps as u16 lanes (two's complement: the sums are kept modulo 2^16).
#[target_feature(enable = "neon")]
fn taps(f: &[i16; 6]) -> [uint16x8_t; 6] {
    f.map(|c| vdupq_n_u16(c as u16))
}

/// (sum + 64) >> 7 clipped to 0..=255, from the sum + 64 modulo 2^16 (see the module).
#[target_feature(enable = "neon")]
fn narrow(acc: uint16x8_t) -> uint16x8_t {
    let ok = vcltq_u16(acc, vdupq_n_u16(0xC000));
    vminq_u16(vshrq_n_u16::<7>(vandq_u16(acc, ok)), vdupq_n_u16(255))
}

/// The horizontal filter of 8 outputs of the row from `o` (pixels o .. o + 13).
#[target_feature(enable = "neon")]
fn hfir(s: &[u8], o: usize, k: &[uint16x8_t; 6]) -> uint16x8_t {
    let v = ld16(s, o);
    let (lo, hi) = (vmovl_u8(vget_low_u8(v)), vmovl_high_u8(v));
    let x = [lo, vextq_u16::<1>(lo, hi), vextq_u16::<2>(lo, hi), vextq_u16::<3>(lo, hi), vextq_u16::<4>(lo, hi), vextq_u16::<5>(lo, hi)];
    let mut acc = vdupq_n_u16(64);
    for t in 0..6 {
        acc = vmlaq_u16(acc, x[t], k[t]);
    }
    narrow(acc)
}

/// Row `r` of the window after the horizontal pass (the pixels themselves without a horizontal
/// filter), for the strip at column `x`.
#[target_feature(enable = "neon")]
fn row(s: &[u8], ss: usize, r: usize, x: usize, kh: Option<&[uint16x8_t; 6]>) -> uint16x8_t {
    match kh {
        Some(k) => hfir(s, r * ss + x, k),
        // 8 pixels from column 2: inside the 16 `predict` checked from the strip's first column
        None => vmovl_u8(ld8(s, r * ss + x + 2)),
    }
}

/// Store the first N (8 or 4) of the 8 pixels `v` at `d[o..]`.
#[target_feature(enable = "neon")]
fn store<const N: usize>(d: &mut [u8], o: usize, v: uint16x8_t) {
    let v = vmovn_u16(v);
    if N == 8 {
        debug_assert!(o + 8 <= d.len());
        // SAFETY: `predict` checked (h - 1) * ds + w <= d.len(); o + 8 <= r * ds + w.
        unsafe { vst1_u8(d.as_mut_ptr().add(o), v) };
    } else {
        let t: [u8; 8] = bytemuck::must_cast(v);
        if let Some(d) = d.get_mut(o..o + 4) {
            d.copy_from_slice(&t[..4]);
        }
    }
}

/// The columns `x..x + N` of the block (8 lanes computed, the first N written).
#[target_feature(enable = "neon")]
fn strip<const N: usize>((s, ss): (&[u8], usize), (fh, fv): (Option<&[i16; 6]>, Option<&[i16; 6]>), h: usize, x: usize, (d, ds): (&mut [u8], usize)) {
    let kh = fh.map(|f| taps(f));
    let kh = kh.as_ref();
    match fv {
        None => {
            for r in 0..h {
                store::<N>(d, r * ds + x, row(s, ss, r + 2, x, kh));
            }
        }
        Some(f) => {
            let kv = taps(f);
            let mut win = [vdupq_n_u16(0); 6];
            for t in 0..5 {
                win[t + 1] = row(s, ss, t, x, kh);
            }
            for r in 0..h {
                for t in 0..5 {
                    win[t] = win[t + 1];
                }
                win[5] = row(s, ss, r + 5, x, kh);
                let mut acc = vdupq_n_u16(64);
                for t in 0..6 {
                    acc = vmlaq_u16(acc, win[t], kv[t]);
                }
                store::<N>(d, r * ds + x, narrow(acc));
            }
        }
    }
}
